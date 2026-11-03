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

## Try it

Under QEMU, from a checkout (Rust via `rustup`, QEMU installed; see
`CLAUDE.md` for the commands):

```
cargo xtask run                      # riscv64: the full system, MCP on :8080
cargo xtask run --arch x86_64        # x86_64: kernel and user programs, no devices yet
```

Without a checkout: every green push on `main` attaches a BIOS-bootable
`sprout-x86_64-iso` artifact to its CI run (Actions tab, sign-in needed to
download). It boots in VirtualBox, Hyper-V Generation 1, QEMU
(`-cdrom sprout-x86_64.iso`) and on a BIOS PC, and shows the boot log and
`init`'s checks on the screen. `tools/mkiso.sh` builds the same ISO on
Linux or WSL.

## License

MIT or Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).
