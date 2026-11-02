# ADR-0004: The development loop is an MCP server from M0

Status: accepted (implemented in M0, 2026-11-02)
Date: 2026-11-02

## Context

Kernel development is a loop: build, boot in an emulator, read the serial
output, attach a debugger, repeat. Today that loop runs through a human
copying terminal output into chat. For a kernel that an AI will mostly write,
the loop itself should be a tool the AI holds.

## Decision

`tools/mcp-dev/` is a small Python MCP server (Python 3.13 and `pip` are
already on the machine) registered in `.mcp.json` at the repo root, so Claude
Code loads it automatically. Tools:

| Tool | Does |
|------|------|
| `build(profile)` | `cargo build` for the kernel and userspace. Returns `cargo`'s JSON diagnostics, not scraped text. |
| `boot(timeout_s, extra_args)` | Starts QEMU headless with serial on a pipe. Returns a session id. |
| `serial_read(session, until, timeout_s)` | Reads serial output until a marker, a timeout, or QEMU exit. Parses JSON log lines into objects. |
| `serial_write(session, bytes)` | Types into the guest console. |
| `test(filter)` | Builds the test kernel, boots it, collects `{"test": ..., "ok": ...}` lines, returns a structured pass/fail list and the QEMU exit code. |
| `gdb_inspect(session, command)` | Attaches to QEMU's gdbstub and runs one command: registers, memory, backtrace, symbol lookup. |
| `image()` | Builds the bootable image (needed for x86_64 later; a no-op on riscv64). |
| `loc()` | Line counts per subsystem against the budget in `PLAN.md`. |

The Python server shells out to `cargo xtask`, which holds the real logic, so
a human at a terminal and the AI use the same commands.

## Kernel-side conventions the dev server depends on

- The kernel logs **one JSON object per line** on the serial console:
  `{"lvl":"info","mod":"mm","msg":"frame allocator ready","free":16384}`.
- The test harness emits `{"test":"mm::bitmap_alloc","ok":true}` per test and
  exits QEMU through the SiFive test device (riscv64) or `isa-debug-exit`
  (x86_64) with a status code: 0 pass, 1 fail, 2 panic.
- Panics emit `{"panic":true,"msg":...,"file":...,"line":...}` then exit with 2.

## Reasons

- Removes the human from the inner loop. The agent builds, boots, reads and
  debugs by itself and shows the human the conclusion.
- Structured output means no regex scraping of boot logs.
- Same tooling in CI: GitHub Actions calls `cargo xtask test`, so the AI's
  local loop and the merge gate are one thing.

## Alternatives

- **Plain `cargo run` and copy-paste.** Works, but every iteration costs a human
  round trip. Rejected for the stated goal.
- **Write the MCP server in Rust.** Possible later. Python gets it done in an
  afternoon and the server is not performance-sensitive.

## Consequences

- M0 includes this server. It is a deliverable, not a nice-to-have.
- The kernel's log format is frozen early because tooling depends on it.
