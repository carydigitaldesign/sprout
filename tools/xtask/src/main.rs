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

const TARGET: &str = "riscv64gc-unknown-none-elf";
const QEMU: &str = "qemu-system-riscv64";
const KERNEL_PACKAGE: &str = "sprout-kernel";
const KERNEL_BINARY: &str = "sprout-kernel";

/// Exit code we report when QEMU had to be killed on timeout.
const EXIT_TIMEOUT: u8 = 124;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");
    let flags = &args[args.len().min(1)..];

    let result = match command {
        "build" => build(&BuildOptions::from_flags(flags)).map(|path| {
            println!("{}", path.display());
            0
        }),
        "run" => run(flags),
        "test" => test(flags),
        "qemu-args" => qemu_args_command(flags),
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
        "cargo xtask <command> [flags]

  build [--ktest] [--release]            build the kernel; prints the ELF path
  run   [--ktest] [--release] [--gdb] [--timeout SECS]
                                         boot in QEMU, serial on stdout; an idling kernel
                                         ends at the timeout (default 60s) with exit 0
  test  [--release] [--timeout SECS]     boot the test kernel; exit 0 only if every test passed
  qemu-args [--ktest] [--release] [--gdb] build, then print the QEMU argv as a JSON array
  symbolize [--ktest] [--release] ADDR... function, file and line for kernel addresses
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
        .join(TARGET)
        .join(profile)
        .join(KERNEL_BINARY)
}

/// Cross-compiles the kernel and returns the path of the ELF.
fn build(opts: &BuildOptions) -> Result<PathBuf, String> {
    let mut cmd = Command::new(cargo());
    cmd.current_dir(root())
        .args(["build", "--package", KERNEL_PACKAGE, "--target", TARGET]);
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
        env::split_paths(&paths)
            .any(|dir| dir.join(QEMU).exists() || dir.join(format!("{QEMU}.exe")).exists())
    });
    if on_path {
        return QEMU.to_string();
    }
    let windows_default = Path::new(r"C:\Program Files\qemu").join(format!("{QEMU}.exe"));
    if windows_default.exists() {
        return windows_default.display().to_string();
    }
    QEMU.to_string()
}

fn qemu_argv(kernel: &Path, gdb: bool) -> Vec<String> {
    let mut argv: Vec<String> = vec![qemu_binary()];
    argv.extend(
        [
            "-machine",
            "virt",
            "-bios",
            "default",
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
            "-kernel",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    argv.push(kernel.display().to_string());
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
        .map_err(|e| format!("could not start {QEMU}: {e} (is QEMU installed and on PATH?)"))
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
    ("arch", 1_500),
    ("mm", 1_500),
    ("sched", 1_000),
    ("obj", 1_500),
    ("ipc", 1_000),
    ("syscall", 1_000),
    ("elf", 500),
    ("log", 250),
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
        let area = match rel.components().next() {
            Some(c) if rel.components().count() > 1 => c.as_os_str().to_string_lossy().into_owned(),
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

    println!("{:<12} {:>7} {:>7}  note", "area", "lines", "target");
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
        println!("{area:<12} {n:>7} {t:>7}  {note}");
    }
    let note = if total > LOC_KERNEL_TARGET {
        "over target"
    } else {
        ""
    };
    println!("{:<12} {total:>7} {LOC_KERNEL_TARGET:>7}  {note}", "kernel");
    0
}

// ------------------------------------------------------------------- lint

/// Every `unsafe {` block and `unsafe impl` must have a `// SAFETY:` comment
/// on the same line or within the four lines above it.
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
            let window = &lines[i.saturating_sub(4)..=i];
            if !window.iter().any(|l| l.contains("SAFETY:")) {
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
