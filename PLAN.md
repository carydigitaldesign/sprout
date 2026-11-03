# Sprout: an AI-native kernel

Sprout is a from-scratch operating system kernel. Its goals, in order:

1. **AI drives, human navigates.** The kernel is designed to be written,
   tested, debugged and operated by an AI agent, with a human setting direction
   and reviewing. The roles can swap at any time: every tool the AI uses is a
   plain command a human can run, and every decision the human makes is
   written down where the AI can read it.
2. **Unique to the market: the OS whose control plane is MCP.** The kernel's
   interface is a self-describing tool registry, so an agent speaking the Model
   Context Protocol can inspect and drive the running OS the same way it drives
   any other MCP server. No existing kernel is built this way.
3. **Simple.** A kernel one person plus an AI pair can hold in their head. No
   legacy compatibility, no POSIX, few dependencies, and line counts that are
   watched rather than enforced. Lite is kept in mind, not chased.

"Like Linux" in ambition (a real kernel: memory, scheduling, isolation,
drivers, networking). Unlike Linux in shape: capability microkernel, about 30
syscalls, drivers in userspace, message passing everywhere.

## 0. AI as driver, human as navigator

What this means in practice, so the roles are real and not a slogan:

| | Driver (usually the AI) | Navigator (usually the human) |
|---|---|---|
| Owns | Writing code, tests and docs. Building, booting, debugging. Proposing designs. | Direction, priorities, taste. Approving ADRs. Saying "that is not what I meant". |
| Works through | The MCP dev loop (`tools/mcp-dev`), `cargo xtask`, the repo. | `PLAN.md`, `docs/adr/`, milestone pages, review of diffs and boot logs. |
| Blocks on | An ADR awaiting a decision. A milestone definition that is ambiguous. | Nothing: the driver keeps going on anything already decided. |
| Can swap by | Writing an ADR or design page and asking the human to implement it. | Running the same `cargo xtask` commands and editing code directly. |

Rules that make the swap cheap:

- **Everything the AI knows is in the repo.** No tribal knowledge in chat
  history. `CLAUDE.md`, `PLAN.md`, the ADRs and the milestone pages are the
  whole context an agent needs to pick up the project cold.
- **Every tool is a command.** The MCP server is a thin wrapper over
  `cargo xtask`; a human at a terminal gets identical behaviour.
- **Every decision is a file.** The navigator decides in `docs/adr/`; the driver
  reads it there. A decision made only in conversation does not exist.
- **Machine-readable results.** Logs, test output and panics are JSON lines.
  The driver reads them without help; the navigator reads them with a glance.

---

## 1. Decisions (reasoning lives in `docs/adr/`)

| # | Decision | Choice | Alternative kept in reserve |
|---|----------|--------|-----------------------------|
| 1 | Language | **Rust**, `no_std`, stable toolchain | C via the Zig toolchain |
| 2 | First target | **riscv64** on QEMU `virt` (OpenSBI loads our ELF directly) | x86_64 via Limine, as port #2 |
| 3 | Kernel shape | **Capability microkernel-lite**: objects, handles, channels. Drivers, filesystem and network stack live in userspace | Small monolith |
| 4 | Process model | **Spawn, not fork.** Static ELF, no POSIX | POSIX shim as a userspace library, later |
| 5 | Boot | OpenSBI (bundled in QEMU) on riscv64; Limine on x86_64 | `bootloader` crate |
| 6 | Dev host | Native Windows: rustup and QEMU via winget. No WSL required. Project folder moved out of OneDrive (ADR-0005) | WSL2 if cross tooling bites |
| 7 | Wire format | Compact binary messages on kernel channels, with a schema the kernel can export (`sys_describe`). JSON only at the MCP bridge | Postcard or Cap'n Proto |
| 8 | Test strategy | Kernel-side test harness, JSON lines over serial, QEMU exit code is the result. Every change boots | Host-side unit tests only |

Why riscv64 first, in short: zero boot tooling (QEMU loads the kernel ELF
itself), no GDT/IDT/APIC/legacy-mode code, Sv39 paging fits on one page of the
spec, and `xv6-riscv` is a clean public reference an AI model reasons about
well. The `arch/` boundary is drawn so x86_64 is a port, not a rewrite. If
booting on your own laptop early matters more than code volume, flip ADR-0002
and start with x86_64 plus Limine. The rest of this plan is unchanged.

---

## 2. Architecture

```
+------------------------------------------------------------------+
| userspace (each box is its own address space, talks via channels)|
|                                                                  |
|  init -> shell      mcpd (MCP bridge)      fsd     netd (smoltcp)|
|                        ^ MCP over HTTP / serial                  |
|  virtio-blk drv   virtio-net drv   virtio-console drv  agent host|
+------------------------------------------------------------------+
| kernel: sprout-kernel, no_std Rust, budget <= 15k lines          |
|                                                                  |
|  syscalls -> tool registry (self-describing via sys_describe)    |
|  objects: AddressSpace . Thread . Channel . Vmo . Interrupt . Port|
|  handles/capabilities . scheduler . mm (phys + Sv39) . traps     |
|  log (JSON lines) . test harness . arch/{riscv64, x86_64}        |
+------------------------------------------------------------------+
| OpenSBI (riscv64)   /   Limine (x86_64)                          |
+------------------------------------------------------------------+
        ^ serial / gdbstub / qemu exit device
+-------+----------------------------------------------------------+
| tools/mcp-dev: Python MCP server for the dev loop                |
|  build . boot . serial_read . test . gdb_inspect . image         |
|  registered in .mcp.json so Claude Code drives the kernel        |
+------------------------------------------------------------------+
```

### 2.1 Kernel objects (the whole API surface)

Everything a program can touch is an object reached through a **handle**: an
index into the process's capability table, with rights bits. No global
namespaces inside the kernel: no guessable PIDs, no file paths, no signals.

| Object | Purpose | Operations (sketch) |
|--------|---------|---------------------|
| `AddressSpace` | A page table | `map(vmo, addr, rights)`, `unmap`, `protect` |
| `Vmo` | Virtual memory object: a sized bag of physical pages | `create(size)`, `read`, `write`, `resize` |
| `Thread` | A schedulable unit inside an address space | `create`, `start(entry, sp, arg)`, `exit`, `yield`, `sleep(deadline)` |
| `Channel` | Bidirectional message pipe; messages carry bytes and handles | `create` (pair), `send`, `recv`, `call` (send and wait for reply) |
| `Port` | Wait on many things at once: channels, interrupts, timers | `create`, `attach(obj, key)`, `wait(deadline)` |
| `Interrupt` | A hardware IRQ line handed to a userspace driver | `create(irq)`, `wait`, `ack` |
| `Process` | Address space plus handle table plus threads, created by spawn | `spawn(elf_vmo, bootstrap_channel)`, `kill` |

Target: **at most 32 syscalls.** Every syscall takes a handle and a small
struct. There is no `ioctl`-style escape hatch.

### 2.2 The MCP-native part

Three layers, cheapest first:

1. **MCP dev loop (M0).** `tools/mcp-dev/` is a Python MCP server exposing
   `build`, `boot`, `serial_read`, `test`, `gdb_inspect` and `image`. Claude
   Code picks it up through `.mcp.json`. From the first commit, the AI can
   build the kernel, boot it, read the serial log and inspect registers
   without a human relaying terminal output.
2. **Self-describing kernel (M4).** `sys_describe(handle)` returns the
   object's type, its operations, their argument schemas and the caller's
   rights, in the same compact wire format channels use. The kernel is a tool
   registry. Capabilities map one-to-one onto MCP tool permissions.
3. **MCP bridge (M7).** `mcpd` is an ordinary userspace program holding
   handles. It serves MCP (streamable HTTP over the in-OS network stack, or
   JSON-RPC over the serial console before networking exists), listing every
   object it holds as a tool, generated from `sys_describe`. An external agent
   can then list processes, read memory stats, spawn a program, open a channel
   to it and read its log. The OS becomes something an agent operates.

Design rules that fall out of this:

- **Structured everything.** Kernel log lines are JSON, one object per line.
  Test results are JSON. Panics are JSON. An AI reads these without parsing
  prose.
- **Schema in one place.** `lib/sprout-abi` holds syscall numbers, message
  layouts and object schemas. Kernel and userspace both depend on it. The MCP
  bridge generates its tool definitions from it.
- **Capabilities are the security model.** No users, no uid 0. A program can
  only reach what it was handed at spawn. This is what makes it safe to let an
  agent operate the OS: give `mcpd` exactly the handles you want exposed.

### 2.3 Keeping it simple (guidance, not a gate)

`cargo xtask loc` reports line counts per subsystem against the targets
below. CI prints the report and warns when a target is passed; it does not
fail. A target being passed is a prompt to ask "should this be in userspace?",
not a reason to stop. The figures are what a clean implementation should land
near.

| Area | Target (lines of Rust, excluding tests) |
|------|------|
| `arch/riscv64` (boot, traps, timer, PLIC, SBI, paging glue) | 1,500 |
| `arch/x86_64` (Multiboot, GDT/IDT, PIC/PIT/COM1, 4-level paging) | 1,500 |
| `mm` (physical allocator, Sv39, kernel heap) | 1,500 |
| `sched` and threads | 1,000 |
| objects, handles, channels, ports | 2,500 |
| syscall layer and `describe` | 1,000 |
| ELF loader and spawn | 500 |
| log, test harness, panic | 500 |
| **kernel total** | **around 10,000 at M7; well under 20,000 is the aim** |
| `sprout-abi` | 1,500 |
| `sprout-rt` (userspace runtime) | 1,000 |

Kernel dependencies start at `spin`, `bitflags` and `linked_list_allocator`
(or our own slab). Adding one is fine; it gets a short ADR so the reason is on
record. Rule of thumb: if it is under 200 lines, write it. Written code is
something the AI can read; a crate is a black box. Userspace may use heavier
crates (`smoltcp`, `virtio-drivers`) because userspace is not the kernel.

---

## 3. Roadmap

Each milestone ends with: boots in QEMU, kernel tests pass, CI green, and a
one-page `docs/design/Mx.md` written by whoever did the work, human or AI.

Progress: **M0 to M8 done** (2026-11-02, see `docs/design/M0.md` through
`M8.md`). The kernel is self-describing, processes talk over channels,
drivers run in userspace from an initramfs, `mcpd` serves MCP over HTTP
from inside the OS (`claude mcp add --transport http sprout-os
http://127.0.0.1:8080/mcp` against a running kernel), and the same kernel
and programs run on riscv64 and x86_64 (`cargo xtask test --arch x86_64`).
M9 is in progress (`docs/design/M9.md`): slice 1 added a VGA console and a
CI-built bootable ISO for x86_64. Next: virtio-pci so x86_64 gets the user
drivers.

| M | Name | Done when | Estimate |
|---|------|-----------|----------|
| **M0** | Toolchain and MCP dev loop | `cargo xtask run` boots QEMU and prints `{"event":"boot"}` over serial. `tools/mcp-dev` works from Claude Code. GitHub Actions boots the kernel. | 1 to 2 days |
| **M1** | Bare metal basics | SBI console, structured logging, panic handler with backtrace, kernel test harness, clean QEMU exit with a status code. | 2 to 3 days |
| **M2** | Memory | Physical frame allocator (bitmap, then buddy), Sv39 page tables, higher-half kernel, kernel heap, `Vmo` object. Tests for every allocator. | 1 week |
| **M3** | Time and threads | Trap handling, timer via SBI, PLIC, kernel threads, round-robin scheduler, sleep and wake, preemption. | 1 week |
| **M4** | User mode | Handles and capability table, syscall entry, ELF loader, `spawn`, `sys_describe`, first userspace program (`init`) that says hello through a syscall. The kernel is now self-describing. | 1 to 2 weeks |
| **M5** | IPC | `Channel` with bytes and handle passing, `Port`, `call` semantics, shared memory via `Vmo`. `init` spawns a `shell` and talks to it over a channel. | 1 to 2 weeks |
| **M6** | Userspace drivers | `Interrupt` object, MMIO mapping, virtio-mmio discovery from the device tree. `virtio-console` and `virtio-blk` drivers as userspace programs. initramfs (cpio inside a `Vmo`) and a tiny `fsd`. | 2 weeks |
| **M7** | Network and MCP bridge | `virtio-net` driver, `netd` on `smoltcp`, `mcpd` serving MCP over HTTP. Claude can call the running OS as an MCP server. | 2 to 3 weeks |
| **M8** | Second architecture | `arch/x86_64` booted by Multiboot (ADR-0007): every kernel test and `init`'s checks pass on both architectures; `xtask --arch`; CI matrix. SMP and real-hardware boot moved to M9+. | 2 to 3 weeks |
| **M9+** | Grow | virtio-pci for x86_64 devices, SMP on both architectures, UEFI boot, on-disk filesystem, agent runtime (`sprout-agent` hosting MCP servers as sandboxed processes), signed images, hardening pass, fuzzing the syscall layer. | ongoing |

Estimates assume the AI does most of the typing and a human reviews design, a
few hours a day. They are optimistic in the way all kernel estimates are.

---

## 4. Repository layout

```
sprout/
+-- CLAUDE.md                 how an AI works in this repo: build, test, rules
+-- PLAN.md                   this file
+-- README.md
+-- rust-toolchain.toml       pinned stable plus targets
+-- Cargo.toml                workspace
+-- .mcp.json                 registers tools/mcp-dev with Claude Code
+-- .github/workflows/ci.yml  build, boot, test, line budget
+-- docs/
|   +-- adr/                  architecture decision records, numbered
|   +-- design/               one page per milestone or subsystem
+-- kernel/                   crate sprout-kernel (bin, no_std)
|   +-- src/
|       +-- arch/{riscv64,x86_64}/
|       +-- mm/  sched/  obj/  ipc/  syscall/  log/  test/
|       +-- main.rs
+-- lib/
|   +-- sprout-abi/           syscall numbers, wire format, schemas (kernel and user)
|   +-- sprout-rt/            userspace runtime: _start, syscalls, allocator, channel helpers
+-- user/                     one crate per program: init, shell, fsd, netd, mcpd, drivers/*
+-- tools/
    +-- xtask/                cargo xtask: build, image, run, test, gdb, loc
    +-- mcp-dev/              Python MCP server wrapping xtask, QEMU and gdb
```

---

## 5. Working agreement (human and AI)

- **Every change boots.** `cargo xtask test` is the gate. CI runs it under QEMU.
- **Every `unsafe` block has a `// SAFETY:` comment** stating the invariant.
  CI greps for naked `unsafe`.
- **One ADR per irreversible choice.** New dependency, new syscall, new object
  type, change of direction. Short is fine; the point is that it is a file.
- **The driver does not wait.** Anything already decided gets built. Only an
  open ADR or an ambiguous milestone is a reason to stop and ask.
- **Docs are written by whoever did the work**, in the same change, one page
  maximum.
- **Reference, do not copy.** `xv6-riscv`, `rCore`, the Zircon docs, the
  RISC-V privileged spec and Phil Opp's blog are references. Code is written
  fresh and understood, not pasted.
- **Small modules.** A file over about 600 lines is a smell. The AI should be
  able to read a subsystem in one sitting.
- **No heroics in the kernel.** If it can live in userspace, it lives in
  userspace.

---

## 6. M0 setup on this machine (native Windows)

Windows is a fine host for this project, and the riscv64 and Rust choices are
most of why. Details and the caveats are in ADR-0005. Steps:

1. **Move the project out of OneDrive.** The folder is currently under
   `OneDrive\Desktop`. A Rust `target/` directory holds tens of thousands of
   files that change on every build; OneDrive will sync them, lock them
   mid-build and occasionally corrupt an incremental build. Put the repo at
   something like `C:\dev\sprout` and let git be the backup.
2. Install the toolchain:

```powershell
winget install Rustlang.Rustup
winget install SoftwareFreedomConservancy.QEMU   # accept the elevation prompt
rustup set default-host x86_64-pc-windows-gnu    # no Visual Studio needed (ADR-0005)
pip install mcp
```

   The pinned toolchain, its components and the riscv64 target install
   themselves the first time `cargo` runs inside the repo, from
   `rust-toolchain.toml`.

3. Add a Windows Defender exclusion for the repo's `target/` folder (builds
   are noticeably faster) and enable long paths in git:
   `git config --global core.longpaths true`.
4. Debugger: QEMU exposes a gdbstub; the client on Windows is either the
   xPack `riscv-none-elf-gdb` build or LLDB from `winget install LLVM.LLVM`.
   `tools/mcp-dev` wraps whichever is installed.

Then `cargo xtask run` boots QEMU with roughly:

```
qemu-system-riscv64 -machine virt -bios default -kernel target/riscv64gc-unknown-none-elf/debug/sprout-kernel -nographic
```

and `cargo xtask test` exits 0.

---

## 7. Open questions to settle before M1

1. **riscv64 first, or x86_64 first?** ADR-0002 recommends riscv64.
2. **Git hosting.** GitHub, so Actions can run the QEMU boot test? CI is what
   makes "every change boots" real.
3. **License.** MIT or Apache-2.0 for a kernel that may host third-party agents?
4. **Project location.** Move out of OneDrive before M0 (ADR-0005).

Settled: the project is named **Sprout**. The kernel crate is `sprout-kernel`,
the shared ABI crate `sprout-abi`, the userspace runtime `sprout-rt`.
