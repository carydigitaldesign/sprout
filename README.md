# Sprout

A from-scratch operating system kernel built for AI to drive and humans to
navigate. The kernel is operated by AI agents over the Model Context Protocol,
and it is built by AI agents through the same protocol.

- Capability microkernel in `no_std` Rust, about 30 syscalls, drivers in
  userspace.
- Self-describing: every kernel object can report its operations and schema,
  so the running OS is itself an MCP tool registry. No other kernel works
  this way.
- The development loop (build, boot, serial, debugger) is an MCP server in
  the repo, so an agent iterates without a human relaying terminal output.
- Simple by design: no POSIX, no legacy, few dependencies. Lite is kept in
  mind, not chased.
- Runs on riscv64 and x86_64 under QEMU. Developed natively on Windows.

Status: M0 to M8 done. It boots on riscv64 and x86_64 under QEMU, runs
user programs from an initramfs with userspace virtio drivers (riscv64 for
now), and serves MCP over HTTP from inside the OS (`cargo xtask run`, then
`claude mcp add --transport http sprout-os http://127.0.0.1:8080/mcp`).
See [PLAN.md](PLAN.md), [docs/design/](docs/design/) and [docs/adr/](docs/adr/).
