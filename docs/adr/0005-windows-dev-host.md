# ADR-0005: Develop natively on Windows, outside OneDrive

Status: accepted (2026-11-02)
Date: 2026-11-02

## Context

The navigator's machine is Windows 11 with git and Python installed, winget
available, no WSL distribution, and the project folder under
`OneDrive\Desktop`. The question was whether to build a kernel here or on
Linux.

## Decision

Build natively on Windows. Do not require WSL or a Linux VM. Move the
repository out of the OneDrive-synced tree before the first build.

## Why Windows is fine for this project

Most "you need Linux for OS dev" advice comes from toolchains this plan does
not use:

- **No cross-compiler to build.** Rust ships `riscv64gc-unknown-none-elf` and
  `x86_64-unknown-none` as prebuilt targets, and links with its bundled
  `rust-lld`. No GCC cross toolchain, no `binutils` build.
- **No image tooling.** On riscv64, QEMU loads the kernel ELF directly, so
  there is no `xorriso`, `mkfs`, `dd` or `mtools` step. When x86_64 arrives at
  M8, QEMU's `-drive file=fat:rw:<dir>` with OVMF boots Limine from a plain
  folder, still with no Linux-only tools.
- **No `make`.** All build logic lives in `tools/xtask`, a Rust program, so it
  behaves identically on Windows, the Linux CI runner and a future Mac.
- **QEMU speed is identical.** A riscv64 guest is software-emulated (TCG) on
  every host, so Windows loses nothing to Linux here. (For an x86_64 guest,
  Linux KVM would be faster than Windows WHPX, but that is M8 and the test
  kernel is small.)
- **CI runs on Linux anyway.** GitHub Actions on Ubuntu is a second, free host
  that catches any Windows-only assumption the moment it is introduced.

## The caveats, and what we do about them

| Problem | Effect | Mitigation |
|---------|--------|------------|
| Repo under OneDrive | `target/` has tens of thousands of files rewritten each build. OneDrive syncs them, holds locks, and can corrupt incremental builds. | Move the repo to a non-synced path such as `C:\dev\sprout`. Git is the backup. Failing that, set `CARGO_TARGET_DIR` outside OneDrive. |
| Windows Defender real-time scan | Every build is slower than it should be. | Exclude the repo's `target/` directory. |
| Path length limit | Deep `target/` paths hit 260 characters. | `git config --global core.longpaths true`; enable long paths in Windows. |
| Line endings | CRLF in linker scripts and assembly files confuses tools. | `.gitattributes` forcing LF on `*.rs`, `*.ld`, `*.S`, `*.toml`, `*.py`. |
| No `gdb-multiarch` package | Need a debugger client that speaks the QEMU gdbstub for riscv64. | xPack `riscv-none-elf-gdb` (Windows binaries) or LLDB via `winget install LLVM.LLVM`. The MCP dev server detects whichever exists. |
| Two shells (PowerShell, Git Bash) | Scripts written for one break in the other. | No shell scripts. Build logic is `cargo xtask`; the MCP server is Python. |

## Alternatives

- **WSL2.** Would work and gives KVM-accelerated x86_64 QEMU. Rejected for now
  because it adds a second filesystem, a second toolchain and cross-boundary
  path confusion for no gain on riscv64. Easy to adopt later if M8 wants
  faster x86_64 emulation.
- **A Linux box or cloud VM.** More moving parts between the navigator and the
  code. Rejected while a single machine suffices.

## What M0 actually hit (2026-11-02)

- **No Visual Studio.** Rust's default Windows host toolchain (`-msvc`) links
  with Visual Studio's `link.exe`, which is not installed, and Git Bash puts a
  GNU coreutils `link` on PATH that rustc then tried to use. Installing the
  Build Tools is several gigabytes and needs admin. Fix: use the `-gnu` host
  toolchain, which ships its own self-contained MinGW linker:
  `rustup set default-host x86_64-pc-windows-gnu`. This only affects host
  tooling (`xtask`); the kernel always links with `rust-lld`.
- **QEMU needs one UAC click.** The winget package runs an installer that
  asks for elevation. A silent install from a background shell is cancelled;
  run it where the prompt can be accepted. Installed to
  `C:\Program Files\qemu` (QEMU 11.1.0).
- **New installs are not on an existing shell's PATH.** `xtask` looks for
  `qemu-system-riscv64` on PATH, then in `C:\Program Files\qemu`, and honours
  a `QEMU` environment variable.
- **Everything else just worked.** The riscv64 kernel linked on the first
  try, the SiFive test finisher is reachable from S-mode under OpenSBI, and the
  SBI v2.0 debug console is available.

## Consequences

- M0 begins with moving the folder, then the winget installs listed in
  `PLAN.md` section 6.
- `tools/xtask` must never shell out to a Unix-only tool. CI on Ubuntu plus
  local Windows keeps both honest.
