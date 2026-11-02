//! `cargo xtask`: the one place build, boot and test logic lives.
//!
//! Humans run these from a terminal, `tools/mcp-dev` runs them for the AI,
//! and CI runs them on Linux. No shell scripts, no Unix-only tools (ADR-0005).

use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The architectures Sprout builds for. Chosen with `--arch` or the
/// `SPROUT_ARCH` environment variable; riscv64 is the default.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arch {
    Riscv64,
    X86_64,
}

impl Arch {
    fn parse(name: &str) -> Result<Arch, String> {
        match name {
            "riscv64" => Ok(Arch::Riscv64),
            "x86_64" => Ok(Arch::X86_64),
            other => Err(format!(
                "unknown architecture {other:?}; use riscv64 or x86_64"
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Arch::Riscv64 => "riscv64",
            Arch::X86_64 => "x86_64",
        }
    }

    fn target(self) -> &'static str {
        match self {
            Arch::Riscv64 => "riscv64gc-unknown-none-elf",
            Arch::X86_64 => "x86_64-unknown-none",
        }
    }

    fn qemu(self) -> &'static str {
        match self {
            Arch::Riscv64 => "qemu-system-riscv64",
            Arch::X86_64 => "qemu-system-x86_64",
        }
    }
}

static ARCH: std::sync::OnceLock<Arch> = std::sync::OnceLock::new();

fn arch() -> Arch {
    *ARCH.get().expect("architecture chosen in main")
}

fn target() -> &'static str {
    arch().target()
}

fn qemu_name() -> &'static str {
    arch().qemu()
}
const KERNEL_PACKAGE: &str = "sprout-kernel";
const KERNEL_BINARY: &str = "sprout-kernel";

/// Exit code we report when QEMU had to be killed on timeout.
const EXIT_TIMEOUT: u8 = 124;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");
    let flags = &args[args.len().min(1)..];

    let chosen = flag_value(flags, "--arch")
        .map(str::to_string)
        .or_else(|| env::var("SPROUT_ARCH").ok())
        .unwrap_or_else(|| "riscv64".to_string());
    match Arch::parse(&chosen) {
        Ok(a) => {
            let _ = ARCH.set(a);
        }
        Err(e) => {
            eprintln!("xtask: {e}");
            return ExitCode::from(2);
        }
    }

    let result = match command {
        "build" => build(&BuildOptions::from_flags(flags)).map(|path| {
            println!("{}", path.display());
            0
        }),
        "run" => run(flags),
        "test" => test(flags),
        "qemu-args" => qemu_args_command(flags),
        "mcp-check" => mcp_check(flags),
        "symbolize" => symbolize_command(flags),
        "loc" => Ok(loc()),
        "lint" => Ok(lint()),
        _ => {
            print_help();
            Ok(0)
        }
    };

    match result {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!(
        "cargo xtask <command> [flags]        (every command takes --arch riscv64|x86_64; default riscv64)

  build [--ktest] [--release]            build the kernel; prints the ELF path
  run   [--ktest] [--release] [--gdb] [--timeout SECS]
                                         boot in QEMU, serial on stdout; an idling kernel
                                         ends at the timeout (default 60s) with exit 0
  test  [--release] [--timeout SECS]     boot the test kernel; exit 0 only if every test passed
  qemu-args [--ktest] [--release] [--gdb] build, then print the QEMU argv as a JSON array
  symbolize [--ktest] [--release] ADDR... function, file and line for kernel addresses
  mcp-check [--timeout SECS]             boot, then talk to the in-OS MCP server from the host
  loc                                    line counts per subsystem vs. PLAN.md targets
  lint                                   every `unsafe` block needs a // SAFETY: comment"
    );
}

// ---------------------------------------------------------------- helpers

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("tools/xtask sits two levels below the repository root")
        .to_path_buf()
}

fn cargo() -> String {
    env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

fn has_flag(flags: &[String], name: &str) -> bool {
    flags.iter().any(|f| f == name)
}

fn flag_value<'a>(flags: &'a [String], name: &str) -> Option<&'a str> {
    flags
        .iter()
        .position(|f| f == name)
        .and_then(|i| flags.get(i + 1))
        .map(String::as_str)
}

fn timeout_from(flags: &[String], default_secs: u64) -> Result<Duration, String> {
    match flag_value(flags, "--timeout") {
        None => Ok(Duration::from_secs(default_secs)),
        Some(v) => v
            .parse()
            .map(Duration::from_secs)
            .map_err(|_| format!("--timeout expects seconds, got {v:?}")),
    }
}

// ------------------------------------------------------------------ build

struct BuildOptions {
    ktest: bool,
    release: bool,
}

impl BuildOptions {
    fn from_flags(flags: &[String]) -> Self {
        Self {
            ktest: has_flag(flags, "--ktest"),
            release: has_flag(flags, "--release"),
        }
    }
}

/// Where cargo puts the kernel for these options. The test kernel builds into
/// its own target directory so switching between the two does not rebuild
/// everything each time.
fn target_dir(opts: &BuildOptions) -> PathBuf {
    let dir = root().join("target");
    if opts.ktest { dir.join("ktest") } else { dir }
}

fn kernel_path(opts: &BuildOptions) -> PathBuf {
    let profile = if opts.release { "release" } else { "debug" };
    target_dir(opts)
        .join(target())
        .join(profile)
        .join(KERNEL_BINARY)
}

/// User programs, built before the kernel so it can embed them.
const USER_PROGRAMS: &[&str] = &["init", "shell", "fsd", "blkd", "mcpd"];

/// Host TCP port forwarded to the guest's port 80, where mcpd listens.
const HOST_MCP_PORT: u16 = 8080;

/// Builds the user programs into `target/user`, packs them into an
/// initramfs (newc cpio) and returns the archive's path for the kernel
/// build script to embed.
fn build_user(release: bool) -> Result<PathBuf, String> {
    let dir = root().join("target").join("user");
    let mut cmd = Command::new(cargo());
    // The kernel embeds these ELFs whole and copies them when spawning;
    // debug info would triple their size for nothing the loader uses.
    cmd.current_dir(root())
        .env("CARGO_PROFILE_DEV_STRIP", "debuginfo")
        .env("CARGO_PROFILE_RELEASE_STRIP", "debuginfo")
        .arg("build")
        .arg("--target")
        .arg(target());
    for program in USER_PROGRAMS {
        cmd.args(["--package", program]);
    }
    cmd.arg("--target-dir").arg(&dir);
    if release {
        cmd.arg("--release");
    }
    let status = cmd
        .status()
        .map_err(|e| format!("could not run cargo: {e}"))?;
    if !status.success() {
        return Err("user program build failed".to_string());
    }
    let profile = if release { "release" } else { "debug" };
    let out = dir.join(target()).join(profile);
    let mut files = Vec::new();
    for program in USER_PROGRAMS {
        let bytes = fs::read(out.join(program))
            .map_err(|e| format!("could not read built program {program}: {e}"))?;
        files.push((program.to_string(), bytes));
    }
    let archive = out.join("initramfs.cpio");
    fs::write(&archive, cpio_newc(&files))
        .map_err(|e| format!("could not write initramfs: {e}"))?;
    Ok(archive)
}

/// Packs files into a `newc` cpio archive, the initramfs format the kernel
/// and `sprout-abi::cpio` read.
fn cpio_newc(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    fn entry(out: &mut Vec<u8>, ino: usize, name: &str, data: &[u8]) {
        let mode = if name == "TRAILER!!!" { 0 } else { 0o100644 };
        let header = format!(
            "070701{ino:08x}{mode:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
            0,
            0,
            1,
            0,
            data.len(),
            0,
            0,
            0,
            0,
            name.len() + 1,
            0
        );
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    let mut out = Vec::new();
    for (i, (name, data)) in files.iter().enumerate() {
        entry(&mut out, i + 1, name, data);
    }
    entry(&mut out, 0, "TRAILER!!!", &[]);
    out
}

/// The disk image `blkd` drives: 1 MiB, sector 0 carrying a magic string.
/// Created on first use; writes by the guest persist across runs.
fn disk_image() -> Result<PathBuf, String> {
    let path = root().join("target").join("disk.img");
    if !path.exists() {
        let mut image = vec![0u8; 1 << 20];
        image[..15].copy_from_slice(
            b"SPROUT-DISK v1
",
        );
        fs::write(&path, image).map_err(|e| format!("could not create disk image: {e}"))?;
    }
    Ok(path)
}

/// Cross-compiles the user programs, then the kernel; returns the kernel ELF.
fn build(opts: &BuildOptions) -> Result<PathBuf, String> {
    let initramfs = build_user(opts.release)?;
    let mut cmd = Command::new(cargo());
    cmd.current_dir(root())
        .env("SPROUT_INITRAMFS", &initramfs)
        .args(["build", "--package", KERNEL_PACKAGE, "--target", target()]);
    if opts.ktest {
        cmd.args(["--features", "ktest"]);
        cmd.arg("--target-dir").arg(target_dir(opts));
    }
    if opts.release {
        cmd.arg("--release");
    }

    let status = cmd
        .status()
        .map_err(|e| format!("could not run cargo: {e}"))?;
    if !status.success() {
        return Err("kernel build failed".to_string());
    }
    Ok(kernel_path(opts))
}

// -------------------------------------------------------------- symbolize

/// Makes a source path from DWARF relative to the repository and
/// forward-slashed, so it reads the same on every host.
fn repo_relative(path: &str) -> String {
    let path = path.replace('\\', "/");
    let root = root().display().to_string().replace('\\', "/");
    path.strip_prefix(&root)
        .map(|rest| rest.trim_start_matches('/').to_string())
        .unwrap_or(path)
}

/// Resolves kernel addresses to `{"addr", "frames": [{"fn", "file", "line"}]}`
/// from the ELF's DWARF, in-process. Inlined call chains give several frames
/// per address, innermost first; a return address is backed up by one byte so
/// a frame reports the call, not whatever follows it.
fn symbolize(elf: &Path, addrs: &[String]) -> Result<Vec<Value>, String> {
    if addrs.is_empty() {
        return Ok(Vec::new());
    }
    let loader = addr2line::Loader::new(elf)
        .map_err(|e| format!("could not load {}: {e}", elf.display()))?;

    let mut results = Vec::with_capacity(addrs.len());
    for addr in addrs {
        let value = u64::from_str_radix(addr.trim_start_matches("0x"), 16)
            .map_err(|_| format!("not a hex address: {addr}"))?;
        let probe = value.saturating_sub(1);
        let mut frames = Vec::new();
        if let Ok(mut iter) = loader.find_frames(probe) {
            while let Ok(Some(frame)) = iter.next() {
                let function = frame
                    .function
                    .as_ref()
                    .and_then(|f| f.demangle().ok().map(|s| s.into_owned()))
                    .unwrap_or_else(|| "??".to_string());
                let (file, line) = match &frame.location {
                    Some(loc) => (loc.file.map(repo_relative), loc.line),
                    None => (None, None),
                };
                frames.push(json!({ "fn": function, "file": file, "line": line }));
            }
        }
        if frames.is_empty() {
            // No line info: fall back to the symbol table.
            let name = loader
                .find_symbol(probe)
                .map(|s| addr2line::demangle_auto(s.into(), None).into_owned())
                .unwrap_or_else(|| "??".to_string());
            frames.push(json!({ "fn": name, "file": null, "line": null }));
        }
        results.push(json!({ "addr": addr, "frames": frames }));
    }
    Ok(results)
}

fn symbolize_command(flags: &[String]) -> Result<u8, String> {
    let opts = BuildOptions::from_flags(flags);
    let addrs: Vec<String> = flags
        .iter()
        .filter(|f| !f.starts_with("--"))
        .cloned()
        .collect();
    let elf = kernel_path(&opts);
    if !elf.exists() {
        return Err(format!("{} does not exist; build first", elf.display()));
    }
    for entry in symbolize(&elf, &addrs)? {
        println!("{entry}");
    }
    Ok(0)
}

/// Collects every address from `"frames"` arrays seen in kernel JSON lines, so
/// a run can be symbolized once it ends.
#[derive(Default)]
struct FrameCollector {
    addrs: Vec<String>,
}

impl FrameCollector {
    fn observe(&mut self, v: &Value) {
        let Some(frames) = v.get("frames").and_then(Value::as_array) else {
            return;
        };
        for addr in frames.iter().filter_map(Value::as_str) {
            if !self.addrs.iter().any(|a| a == addr) {
                self.addrs.push(addr.to_string());
            }
        }
    }

    /// Prints a `{"event":"symbols", ...}` line and returns the map.
    fn report(&self, elf: &Path) -> Value {
        if self.addrs.is_empty() {
            return Value::Null;
        }
        match symbolize(elf, &self.addrs) {
            Ok(entries) => {
                let line = json!({ "event": "symbols", "addrs": entries });
                println!("{line}");
                Value::Array(entries)
            }
            Err(e) => {
                eprintln!("xtask: could not symbolize backtrace: {e}");
                Value::Null
            }
        }
    }
}

// ------------------------------------------------------------------- qemu

/// The QEMU binary: `$QEMU` if set, else PATH, else the default Windows
/// install location (the winget package does not update an open shell's PATH).
fn qemu_binary() -> String {
    if let Ok(q) = env::var("QEMU") {
        return q;
    }
    let on_path = env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|dir| {
            dir.join(qemu_name()).exists() || dir.join(format!("{}.exe", qemu_name())).exists()
        })
    });
    if on_path {
        return qemu_name().to_string();
    }
    let windows_default = Path::new(r"C:\Program Files\qemu").join(format!("{}.exe", qemu_name()));
    if windows_default.exists() {
        return windows_default.display().to_string();
    }
    qemu_name().to_string()
}

fn qemu_argv(kernel: &Path, gdb: bool) -> Vec<String> {
    let mut argv: Vec<String> = vec![qemu_binary()];
    let common = [
        "-m",
        "128M",
        "-smp",
        "1",
        "-display",
        "none",
        "-serial",
        "stdio",
        "-monitor",
        "none",
        "-no-reboot",
    ];
    match arch() {
        Arch::Riscv64 => {
            argv.extend(
                ["-machine", "virt", "-bios", "default"]
                    .iter()
                    .map(|s| s.to_string()),
            );
            argv.extend(common.iter().map(|s| s.to_string()));
            // Modern virtio-mmio (version 2), which sprout-virtio speaks.
            argv.extend(
                ["-global", "virtio-mmio.force-legacy=false"]
                    .iter()
                    .map(|s| s.to_string()),
            );
            argv.push("-kernel".to_string());
            argv.push(kernel.display().to_string());
            if let Ok(disk) = disk_image() {
                argv.push("-drive".to_string());
                argv.push(format!(
                    "file={},if=none,format=raw,id=disk0",
                    disk.display()
                ));
                argv.push("-device".to_string());
                argv.push("virtio-blk-device,drive=disk0".to_string());
            }
            // User-mode networking: the guest is 10.0.2.15; its port 80
            // (mcpd) is reachable from the host at 127.0.0.1:HOST_MCP_PORT.
            argv.push("-netdev".to_string());
            argv.push(format!(
                "user,id=net0,hostfwd=tcp:127.0.0.1:{HOST_MCP_PORT}-:80"
            ));
            argv.push("-device".to_string());
            argv.push("virtio-net-device,netdev=net0".to_string());
        }
        Arch::X86_64 => {
            // The default `pc` machine, booted through QEMU's Multiboot
            // loader. isa-debug-exit lets the kernel end the run. No virtio
            // devices yet: x86 uses virtio-pci, which Sprout cannot drive.
            argv.extend(common.iter().map(|s| s.to_string()));
            argv.push("-kernel".to_string());
            argv.push(kernel.display().to_string());
            argv.extend(
                ["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        }
    }
    if gdb {
        // Halt at reset with a gdbstub on localhost:1234.
        argv.extend(["-s", "-S"].iter().map(|s| s.to_string()));
    }
    argv
}

fn qemu_args_command(flags: &[String]) -> Result<u8, String> {
    let kernel = build(&BuildOptions::from_flags(flags))?;
    let argv = qemu_argv(&kernel, has_flag(flags, "--gdb"));
    println!(
        "{}",
        Value::Array(argv.into_iter().map(Value::String).collect())
    );
    Ok(0)
}

fn spawn_qemu(argv: &[String], stdin: Stdio) -> Result<Child, String> {
    Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(root())
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| {
            format!(
                "could not start {}: {e} (is QEMU installed and on PATH?)",
                qemu_name()
            )
        })
}

/// Streams QEMU's serial output line by line to `on_line` until it exits or
/// `timeout` passes, in which case QEMU is killed. Returns the exit code, or
/// `None` on timeout.
fn stream(mut child: Child, timeout: Duration, mut on_line: impl FnMut(&str)) -> Option<i32> {
    let stdout = child.stdout.take().expect("stdout was piped");
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) => on_line(&line),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                eprintln!("xtask: timeout after {}s, killing QEMU", timeout.as_secs());
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    child.wait().ok().and_then(|s| s.code())
}

// -------------------------------------------------------------------- run

/// Boots the kernel and streams its console. A kernel that reached
/// `{"event":"idle"}` has nothing more to say, so hitting the timeout after
/// that counts as a normal end.
fn run(flags: &[String]) -> Result<u8, String> {
    let kernel = build(&BuildOptions::from_flags(flags))?;
    let timeout = timeout_from(flags, 60)?;
    let argv = qemu_argv(&kernel, has_flag(flags, "--gdb"));
    let child = spawn_qemu(&argv, Stdio::inherit())?;

    let mut idle = false;
    let mut frames = FrameCollector::default();
    let exit = stream(child, timeout, |line| {
        println!("{line}");
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            frames.observe(&v);
            if v.get("event").and_then(Value::as_str) == Some("idle") {
                idle = true;
            }
        }
    });
    frames.report(&kernel);
    match exit {
        Some(code) => Ok(code.clamp(0, 255) as u8),
        None if idle => Ok(0),
        None => Ok(EXIT_TIMEOUT),
    }
}

// ------------------------------------------------------------------- test

/// Boots the test kernel and judges the result from its JSON lines.
///
/// Success needs both: no `{"test":..,"ok":false}` line, and an
/// `{"event":"exit","code":0}` line. QEMU's own exit status is reported but
/// not trusted, because the finisher device is firmware-dependent.
fn test(flags: &[String]) -> Result<u8, String> {
    let kernel = build(&BuildOptions {
        ktest: true,
        release: has_flag(flags, "--release"),
    })?;
    let timeout = timeout_from(flags, 120)?;
    let argv = qemu_argv(&kernel, false);
    let child = spawn_qemu(&argv, Stdio::null())?;

    let mut passed = 0u32;
    let mut failed: Vec<Value> = Vec::new();
    let mut exit_code: Option<i64> = None;
    let mut panic: Option<Value> = None;
    let mut frames = FrameCollector::default();

    let qemu_exit = stream(child, timeout, |line| {
        println!("{line}");
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        frames.observe(&v);
        if v.get("test").is_some() {
            match v.get("ok").and_then(Value::as_bool) {
                Some(true) => passed += 1,
                _ => failed.push(v.clone()),
            }
        }
        match v.get("event").and_then(Value::as_str) {
            Some("exit") => exit_code = v.get("code").and_then(Value::as_i64),
            Some("panic") => panic = Some(v.clone()),
            _ => {}
        }
    });

    let symbols = frames.report(&kernel);
    let ok = failed.is_empty() && exit_code == Some(0) && panic.is_none();
    let summary = json!({
        "summary": {
            "ok": ok,
            "passed": passed,
            "failed": failed.len(),
            "failures": failed,
            "panic": panic,
            "symbols": symbols,
            "exit_event": exit_code,
            "qemu_exit": qemu_exit,
            "timed_out": qemu_exit.is_none(),
        }
    });
    println!("{summary}");
    Ok(if ok { 0 } else { 1 })
}

// -------------------------------------------------------------------- loc

/// Line-count targets per kernel subsystem, from PLAN.md section 2.3.
/// Guidance, not a gate: `loc` warns and always exits 0.
const LOC_TARGETS: &[(&str, usize)] = &[
    ("arch", 100),
    ("arch/riscv64", 1_500),
    ("arch/x86_64", 1_500),
    ("mm", 1_500),
    ("sched", 1_000),
    ("obj", 1_500),
    ("ipc", 1_000),
    ("syscall", 1_000),
    ("elf", 500),
    ("log", 250),
    ("fdt", 300),
    ("sync", 200),
    ("time", 200),
    ("console", 200),
    ("json", 300),
    ("elf", 300),
    ("obj", 1500),
    ("syscall", 1000),
    ("testing", 250),
    ("panic", 100),
];
const LOC_KERNEL_TARGET: usize = 10_000;

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn count_lines(path: &Path) -> usize {
    fs::read_to_string(path)
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

fn loc() -> u8 {
    let src = root().join("kernel").join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);

    let mut per_area: Vec<(String, usize)> = Vec::new();
    let mut total = 0;
    for file in &files {
        let rel = file.strip_prefix(&src).unwrap_or(file);
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let area = match parts.as_slice() {
            // Each architecture is its own area, measured against one target.
            [arch, port, _, ..] if arch == "arch" => format!("arch/{port}"),
            [dir, _, ..] => dir.clone(),
            _ => rel
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        let n = count_lines(file);
        total += n;
        match per_area.iter_mut().find(|(a, _)| *a == area) {
            Some((_, c)) => *c += n,
            None => per_area.push((area, n)),
        }
    }
    per_area.sort();

    println!("{:<14} {:>7} {:>7}  note", "area", "lines", "target");
    for (area, n) in &per_area {
        let target = LOC_TARGETS.iter().find(|(a, _)| a == area).map(|(_, t)| *t);
        let (t, note) = match target {
            Some(t) if *n > t => (
                t.to_string(),
                "over target: should some of this be userspace?",
            ),
            Some(t) => (t.to_string(), ""),
            None => ("-".to_string(), "no target yet"),
        };
        println!("{area:<14} {n:>7} {t:>7}  {note}");
    }
    let note = if total > LOC_KERNEL_TARGET {
        "over target"
    } else {
        ""
    };
    println!("{:<14} {total:>7} {LOC_KERNEL_TARGET:>7}  {note}", "kernel");
    0
}

// ------------------------------------------------------------------- lint

/// Every `unsafe {` block and `unsafe impl` must have a `// SAFETY:` comment
/// on the same line or in the comment block directly above it.
fn lint() -> u8 {
    let mut files = Vec::new();
    rust_files(&root().join("kernel"), &mut files);
    rust_files(&root().join("lib"), &mut files);
    rust_files(&root().join("user"), &mut files);

    let mut problems = 0;
    for file in &files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            let needs_comment = line.contains("unsafe {") || line.contains("unsafe impl");
            if !needs_comment {
                continue;
            }
            // The comment may be on the same line, on an earlier line of the
            // same statement, or in the block of `//` lines directly above
            // the statement, however long that block is.
            let mut documented = line.contains("SAFETY:");
            let mut j = i;
            while !documented && j > 0 {
                j -= 1;
                let above = lines[j].trim();
                if above.starts_with("//") {
                    documented = above.contains("SAFETY:");
                    continue;
                }
                if above.is_empty()
                    || above.ends_with(';')
                    || above.ends_with('{')
                    || above.ends_with('}')
                {
                    break;
                }
            }
            if !documented {
                problems += 1;
                let rel = file.strip_prefix(root()).unwrap_or(file);
                println!(
                    "{}:{}: unsafe without a // SAFETY: comment",
                    rel.display(),
                    i + 1
                );
            }
        }
    }
    if problems == 0 {
        println!("lint: ok ({} files)", files.len());
        0
    } else {
        println!("lint: {problems} problem(s)");
        1
    }
}

// -------------------------------------------------------------- mcp-check

/// One HTTP POST to the in-OS MCP server; returns the response body.
fn mcp_post(port: u16, body: &str) -> Result<String, String> {
    use std::io::{Read, Write as _};
    use std::net::TcpStream;
    let mut stream = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(5),
    )
    .map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| format!("socket: {e}"))?;
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("read: {e}"))?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| format!("no HTTP header in response: {text:?}"))?;
    if !head.starts_with("HTTP/1.1 200") {
        return Err(format!(
            "status line: {}",
            head.lines().next().unwrap_or("")
        ));
    }
    Ok(body.to_string())
}

/// Boots the kernel, waits for mcpd to listen, and speaks MCP to it from
/// the host: initialize, tools/list, and a shell ping. Exit 0 if all three
/// answered as expected.
fn mcp_check(flags: &[String]) -> Result<u8, String> {
    if arch() != Arch::Riscv64 {
        return Err(format!(
            "mcp-check needs a network device, which {} has no driver for yet",
            arch().name()
        ));
    }
    let kernel = build(&BuildOptions::from_flags(flags))?;
    let timeout = timeout_from(flags, 120)?;
    let argv = qemu_argv(&kernel, false);
    let mut child = spawn_qemu(&argv, Stdio::null())?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    // Wait for mcpd to announce itself.
    let deadline = Instant::now() + timeout;
    let mut listening = false;
    while !listening {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                println!("{line}");
                listening = line.contains("mcpd: listening");
            }
            Err(_) => break,
        }
    }
    if !listening {
        let _ = child.kill();
        let _ = child.wait();
        return Err("mcpd never announced it was listening".to_string());
    }
    // Keep draining the console in the background.
    thread::spawn(move || {
        for line in rx {
            println!("{line}");
        }
    });

    let port = HOST_MCP_PORT;
    let init = mcp_post(
        port,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"xtask","version":"0.1"}}}"#,
    );
    let list = mcp_post(port, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let ping = mcp_post(
        port,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"shell","arguments":{"command":"ping"}}}"#,
    );
    let kernel_desc = mcp_post(
        port,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"kernel","arguments":{}}}"#,
    );
    let _ = child.kill();
    let _ = child.wait();

    let init_ok = init
        .as_ref()
        .is_ok_and(|b| b.contains("\"protocolVersion\"") && b.contains("\"sprout\""));
    let list_ok = list
        .as_ref()
        .is_ok_and(|b| b.contains("\"name\":\"shell\"") && b.contains("\"name\":\"kernel\""));
    let ping_ok = ping.as_ref().is_ok_and(|b| b.contains("pong"));
    let kernel_ok = kernel_desc.as_ref().is_ok_and(|b| b.contains("clock_ns"));
    let ok = init_ok && list_ok && ping_ok && kernel_ok;
    let show = |r: Result<String, String>| match r {
        Ok(body) => body,
        Err(e) => format!("ERROR {e}"),
    };
    let summary = json!({
        "mcp_check": {
            "ok": ok,
            "port": port,
            "initialize_ok": init_ok,
            "initialize": show(init),
            "tools_list_ok": list_ok,
            "tools_list_bytes": list.as_ref().map(|b| b.len()).unwrap_or(0),
            "tools_list_error": list.as_ref().err().cloned(),
            "shell_ping_ok": ping_ok,
            "shell_ping": show(ping),
            "kernel_tool_ok": kernel_ok,
            "kernel_tool_bytes": kernel_desc.as_ref().map(|b| b.len()).unwrap_or(0),
            "kernel_tool_error": kernel_desc.as_ref().err().cloned(),
        }
    });
    println!("{summary}");
    Ok(if ok { 0 } else { 1 })
}
