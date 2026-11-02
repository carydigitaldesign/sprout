# Sprout kernel: working notes for AI pairs

Read `PLAN.md` first. The architecture, budgets and roadmap are there. The
decision records in `docs/adr/` explain why.

## What this is

Sprout: a from-scratch capability microkernel in `no_std` Rust. First target
riscv64 on QEMU `virt`. The kernel is self-describing so an MCP agent can
drive it, and the dev loop is itself an MCP server (`tools/mcp-dev`, loaded
via `.mcp.json`).

## Your role

You are usually the **driver**: you write the code, tests and docs, build,
boot, debug, and propose designs. The human is usually the **navigator**: they
set direction, approve ADRs and review. Roles can swap at any time, so leave
the repo in a state where a human can pick up the wheel: commands documented,
decisions in files, nothing important living only in chat.

Do not wait on anything already decided in `PLAN.md` or an accepted ADR. Stop
and ask only when an ADR is open or a milestone definition is ambiguous.

Development is on native Windows (see ADR-0005). Never shell out to a
Unix-only tool from `xtask`; CI on Ubuntu will catch it, but do not make it.

## Commands

```
cargo xtask build [--ktest]       # cross-compile the kernel; prints the ELF path
cargo xtask run [--timeout N]     # boot in QEMU, serial on stdout; the kernel idles,
                                  # so the run ends at the timeout (default 60s) with 0
cargo xtask run --gdb             # boot halted with a gdbstub on :1234
cargo xtask test                  # boot the test kernel; exit 0 only if all pass
cargo xtask symbolize ADDR...     # function/file/line for kernel addresses (--ktest
                                  # for addresses from the test kernel)
cargo xtask lint                  # every unsafe block needs a // SAFETY: comment
cargo xtask loc                   # line counts vs. the targets in PLAN.md
cargo xtask qemu-args             # the QEMU argv as JSON (used by the MCP server)
```

Tests are declared with `crate::ktest! { fn name() { ... } }` next to the
code they cover and run inside QEMU. A failing test stops the run (M0
limitation), so read the first `"ok":false` line. Panics, traps and failed
tests carry a `frames` array; `xtask test` symbolizes them into the summary
under `symbols`, and `xtask run` prints a `{"event":"symbols"}` line.

Prefer the MCP tools (`build`, `boot`, `serial_read`, `test`, `gdb_inspect`)
over running these by hand; they return structured results.

## Rules

- Every change must boot and pass `cargo xtask test` before it is done.
- Every `unsafe` block carries a `// SAFETY:` comment stating the invariant.
- Architecture-specific code lives only under `kernel/src/arch/<arch>/`.
- The kernel runs in the higher half. Reach physical memory through
  `arch::phys_to_virt`; frames come from `mm::frame` and go back there.
  Every allocator test checks the free count before and after.
- Interrupts are on from `kmain` onwards. `SpinLock` masks them while held,
  so it is safe from interrupt handlers; never hold one across
  `sched::yield_now`, `sleep` or a wait. A log `Line` holds the console
  lock until `.end()`, so keep lines short and never log mid-lock-dance.
- Kernel threads: `sched::spawn(name, fn, arg)`, `yield_now`, `join`,
  `time::sleep_ms`, `sched::wait::WaitQueue`. Preemption is on; shared state
  needs atomics or a lock.
- Anything that can be a userspace program is a userspace program.
- New kernel dependency, new syscall, new object type, or budget change: write
  an ADR in `docs/adr/` in the same change.
- Kernel log lines are JSON, one object per line. Never `println!` prose from
  the kernel.
- Keep files under about 600 lines. Split by concern, not by size.
- Line targets in `PLAN.md` 2.3 are guidance. Passing one means "ask whether
  this belongs in userspace", not "stop".
- Reference `xv6-riscv`, `rCore`, Zircon docs and the RISC-V privileged spec.
  Do not paste from them. Write it, understand it, test it.
- Finish a milestone with a one-page `docs/design/Mx.md`.

## Userspace

- The syscall table in `lib/sprout-abi/src/lib.rs` is the spec. Adding a
  syscall means: a number and a `Syscall` entry there, a wrapper in
  `lib/sprout-rt`, the implementation in `kernel/src/syscall/mod.rs`, and a
  check in `user/init` (plus an ADR, per the rules).
- User pointers are untrusted. Go through `syscall::user::{copy_from_user,
  copy_to_user, check_range}`; never dereference one directly.
- Objects live in `kernel/src/obj`; a new object type is an `Object` variant,
  an `ObjectType` in the ABI, and a branch in `describe`.
- User programs are `no_std` crates under `user/` linking `sprout-rt`, with a
  five-line `build.rs` that points at `lib/sprout-rt/user.ld`. `cargo xtask
  build` builds them first; the kernel embeds `init`.
- A user fault kills the process (exit code 128 + cause) and logs a
  `user_fault` event; a kernel fault is still a panic.

## Layout

See `PLAN.md` section 4. Short version: `kernel/` (the kernel), `lib/sprout-abi`
(shared syscall and wire definitions), `lib/sprout-rt` (userspace runtime),
`user/` (one crate per program), `tools/xtask` and `tools/mcp-dev`.
