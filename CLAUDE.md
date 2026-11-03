# Sprout kernel: working notes for AI pairs

Read `PLAN.md` first. The architecture, budgets and roadmap are there. The
decision records in `docs/adr/` explain why.

## What this is

Sprout: a from-scratch capability microkernel in `no_std` Rust. Runs on
riscv64 (QEMU `virt`, the default) and x86_64 (QEMU `pc`, Multiboot). The kernel is self-describing so an MCP agent can
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
                                  # (every command takes --arch riscv64|x86_64,
                                  #  or SPROUT_ARCH; default riscv64)
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

- Every change must boot and pass `cargo xtask test` before it is done; a
  change under `arch/`, in the boot path or in `mm` passes it on both
  architectures (`--arch x86_64` too).
- Every `unsafe` block carries a `// SAFETY:` comment stating the invariant.
- Architecture-specific code lives only under `kernel/src/arch/<arch>/`.
  Both ports export the same surface (listed in `kernel/src/arch/mod.rs`)
  and hand the portable kernel a `Handoff` (`kernel/src/handoff.rs`).
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
- Handle numbers are reused as soon as a slot is closed or transferred.
  Never reason about a number you no longer hold; `describe` it if in doubt.
- Processes talk over channels (`channel_send`/`channel_recv`, handles
  included) and wait on several with a port. New programs are spawned from
  a VMO holding their ELF with a channel as the bootstrap handle.
- Programs live in the initramfs (`cargo xtask build` packs `user/` into a
  cpio; add a crate name to `USER_PROGRAMS` in xtask and `PROGRAMS` in the
  kernel's `user_image` test). `init` gets the archive plus a handle per
  discovered device; `fsd` serves files from it.
- Drivers are user programs holding a `Device` handle: `device_map` for the
  registers, `device_irq_wait` / `device_irq_ack` for the line (the kernel
  masks it until the ack), `vmo_create` with `CONTIGUOUS` plus
  `vmo_physical` for DMA memory. `lib/sprout-virtio` has the transport.
- QEMU runs with `virtio-mmio.force-legacy=false` and a `virtio-blk-device`
  backed by `target/disk.img`; writes persist, delete the file to reset.
- QEMU also has a `virtio-net-device` on user networking (guest 10.0.2.15),
  host port 8080 forwarded to guest port 80. `user/mcpd` serves MCP there;
  `cargo xtask mcp-check` (also in CI) talks to it from the host. `.mcp.json`
  lists the running kernel as the `sprout-os` server, so a Claude Code
  session started in this folder gets its tools (the `claude` CLI
  equivalent: `claude mcp add --transport http sprout-os
  http://127.0.0.1:8080/mcp`). Only one QEMU can hold the port, so never
  run `test`/`run`/`mcp-check` concurrently.
- x86_64 has no virtio devices yet (it needs virtio-pci), so `blkd` and
  `mcpd` do not run there and `init` reports those checks as skipped;
  `mcp-check` is riscv64-only. x86_64 logs to COM1 and the VGA text screen;
  `tools/mkiso.sh` (Linux or WSL; CI's `iso` job) wraps the kernel in a
  GRUB ISO that boots on BIOS machines and VirtualBox or Hyper-V Gen 1.
- The kernel's description carries `"mode":"test"` in the test kernel;
  `init` uses it to shut services down so the run can end.

## Layout

See `PLAN.md` section 4. Short version: `kernel/` (the kernel), `lib/sprout-abi`
(shared syscall and wire definitions), `lib/sprout-rt` (userspace runtime),
`user/` (one crate per program), `tools/xtask` and `tools/mcp-dev`.
