# ADR-0003: Capability microkernel-lite, spawn not fork, no POSIX

Status: accepted (direction confirmed 2026-11-02; first objects land at M4)
Date: 2026-11-02

## Context

The kernel must be small and must present a control plane an MCP agent can
reason about. MCP is "a registry of typed tools, each callable with explicit
permission". The kernel's own interface should have the same shape.

## Decision

A **capability-based microkernel-lite** in the spirit of Zircon and seL4, much
simpler than either:

- The kernel owns: address spaces, threads and scheduling, physical memory,
  channels (message passing with handle transfer), ports (multiplexed wait),
  interrupt delivery to userspace, and process spawn from an ELF.
- Userspace owns: all device drivers (virtio first), the filesystem, the
  network stack, the shell, and the MCP bridge.
- **Handles are the only way to reach anything.** A process's capability table
  is populated by whoever spawned it. No global namespaces, no users, no
  ambient authority.
- **Spawn, not fork.** `spawn(elf_vmo, bootstrap_channel)` creates a fresh
  process. Fork drags in copy-on-write, signal semantics and a dozen POSIX
  corners. Spawn plus channels does everything we need.
- **No POSIX.** Not now. If ever, it is a userspace library over channels.
- **At most 32 syscalls.** Each takes a handle and a small struct. No `ioctl`.
- **The kernel is self-describing.** `sys_describe(handle)` returns the type,
  operations, argument schemas and the caller's rights for that object. This is
  what makes a direct kernel-to-MCP bridge possible.

## Reasons

- **Smallest coherent kernel.** Every subsystem that can be a userspace program
  is a userspace program, and a userspace program is allowed to crash.
- **Capabilities are MCP permissions.** When `mcpd` exposes the OS to an agent,
  the agent's reach is exactly the handles `mcpd` was given. Same model top
  to bottom.
- **AI-reviewable.** A handful of object types with a few operations each is a
  surface an agent can read in full before touching it.

## Alternatives

- **Small monolith** (xv6-style, drivers in-kernel). Faster to first useful
  system, but every driver bug is a kernel bug and the interface grows an
  `ioctl` zoo. Rejected for the MCP goal.
- **Unikernel / single address space.** Lightest of all, but gives up isolation,
  which is the whole point of hosting third-party agents.

## Consequences

- IPC performance matters early. Channels must be cheap: fixed-size message
  header, bytes plus up to a small number of handles, zero-copy for bulk data
  through shared `Vmo`s.
- Driver authors (that is, the AI) need an `Interrupt` object and MMIO mapping
  by M6. That is the price of userspace drivers and it is on the roadmap.
