"""Sprout dev-loop MCP server (ADR-0004).

A thin wrapper over `cargo xtask` plus QEMU session management, so an AI agent
can build, boot, read the serial console, run the kernel tests and query the
debugger without a human relaying terminal output.

Claude Code starts it through `.mcp.json`. By hand:

    python tools/mcp-dev/server.py

Kernel output is JSON lines; anything else on the console comes from OpenSBI
or QEMU and is returned as {"raw": "..."}.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import threading
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path

try:
    # mcp >= 2.0
    from mcp.server.mcpserver import MCPServer as Server
except ImportError:  # mcp 1.x
    from mcp.server.fastmcp import FastMCP as Server

ROOT = Path(__file__).resolve().parents[2]
CARGO = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo")
GDB_CANDIDATES = ["riscv-none-elf-gdb", "riscv64-unknown-elf-gdb", "gdb-multiarch", "gdb"]

mcp = Server(
    "sprout-dev",
    instructions=(
        "Build, boot, test and debug the Sprout kernel. Use `test` as the gate "
        "for any change. Use `boot` + `serial_read` to watch a kernel run, and "
        "`boot(gdb=True)` + `gdb_inspect` to look at registers and memory."
    ),
)


# ------------------------------------------------------------------ helpers


def _parse(line: str) -> dict:
    if line.startswith("{"):
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            pass
    return {"raw": line}


def _tail(text: str, n: int = 80) -> list[str]:
    return text.rstrip().splitlines()[-n:]


def _arch_flag(arch: str) -> list[str]:
    if arch not in ("riscv64", "x86_64"):
        raise ValueError(f"arch must be riscv64 or x86_64, not {arch!r}")
    return ["--arch", arch]


def _xtask(args: list[str], timeout: float = 600) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [CARGO, "xtask", *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=timeout,
    )


@dataclass
class Session:
    proc: subprocess.Popen
    kernel: str
    lines: list[str] = field(default_factory=list)
    cursor: int = 0
    lock: threading.Lock = field(default_factory=threading.Lock)

    def pump(self) -> None:
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            with self.lock:
                self.lines.append(line.rstrip("\r\n"))

    def read(self, until: str | None, timeout_s: float) -> dict:
        deadline = time.monotonic() + timeout_s
        while True:
            with self.lock:
                new = self.lines[self.cursor :]
                hit = next((i for i, l in enumerate(new) if until and until in l), None)
                exited = self.proc.poll()
                if hit is not None:
                    new = new[: hit + 1]
                if hit is not None or exited is not None or time.monotonic() >= deadline:
                    self.cursor += len(new)
                    return {
                        "lines": [_parse(l) for l in new],
                        "exited": exited,
                        "matched": hit is not None,
                    }
            time.sleep(0.05)


SESSIONS: dict[str, Session] = {}


def _session(session: str) -> Session:
    if session not in SESSIONS:
        raise ValueError(f"unknown session {session!r}; call boot() first")
    return SESSIONS[session]


# -------------------------------------------------------------------- tools


@mcp.tool()
def build(ktest: bool = False, release: bool = False, arch: str = "riscv64") -> dict:
    """Build the kernel (or the test kernel with ktest=True) for arch (riscv64 or x86_64).
    Returns the ELF path and compiler output."""
    args = ["build", *_arch_flag(arch)] + (["--ktest"] if ktest else []) + (["--release"] if release else [])
    p = _xtask(args)
    out = p.stdout.strip().splitlines()
    return {
        "ok": p.returncode == 0,
        "kernel": out[-1] if p.returncode == 0 and out else None,
        "stderr": _tail(p.stderr),
    }


@mcp.tool()
def boot(ktest: bool = False, release: bool = False, gdb: bool = False, settle_s: float = 2.0, arch: str = "riscv64") -> dict:
    """Build and boot the kernel in QEMU. Returns a session id for serial_read / serial_write / stop,
    plus whatever the console printed in the first settle_s seconds. The kernel parks after boot and
    prints {"event":"idle"}; serial_read(session, until='"idle"') waits for that. With gdb=True QEMU
    starts halted with a gdbstub on localhost:1234; use gdb_inspect, then serial_read. Call stop when done."""
    flags = _arch_flag(arch) + (["--ktest"] if ktest else []) + (["--release"] if release else []) + (["--gdb"] if gdb else [])
    p = _xtask(["qemu-args", *flags])
    if p.returncode != 0:
        return {"ok": False, "stderr": _tail(p.stderr)}
    argv = json.loads(p.stdout.strip().splitlines()[-1])
    proc = subprocess.Popen(
        argv,
        cwd=ROOT,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    session = Session(proc=proc, kernel=argv[argv.index("-kernel") + 1])
    threading.Thread(target=session.pump, daemon=True).start()
    sid = uuid.uuid4().hex[:8]
    SESSIONS[sid] = session
    first = session.read(until=None, timeout_s=settle_s)
    return {"ok": True, "session": sid, "argv": argv, **first}


@mcp.tool()
def serial_read(session: str, until: str | None = None, timeout_s: float = 5.0) -> dict:
    """Read new console lines from a booted session. Waits until a line containing `until` appears,
    QEMU exits, or timeout_s passes. JSON lines are returned parsed."""
    return _session(session).read(until, timeout_s)


@mcp.tool()
def serial_write(session: str, text: str) -> dict:
    """Type text into the guest console (no newline is added). Bytes are paced in small chunks:
    QEMU's Windows stdio backend drops input once the emulated UART FIFO reports no room, and the
    guest can only drain it as fast as its CPU gets to run."""
    s = _session(session)
    if s.proc.stdin is None or s.proc.poll() is not None:
        return {"ok": False, "reason": "session has exited"}
    chunk = 8
    for at in range(0, len(text), chunk):
        s.proc.stdin.write(text[at : at + chunk])
        s.proc.stdin.flush()
        time.sleep(0.02)
    return {"ok": True, "bytes": len(text)}


@mcp.tool()
def stop(session: str) -> dict:
    """Kill a booted session's QEMU. Returns its exit code if it had already exited."""
    s = SESSIONS.pop(session, None)
    if s is None:
        return {"ok": False, "reason": "unknown session"}
    code = s.proc.poll()
    if code is None:
        s.proc.kill()
        s.proc.wait()
    return {"ok": True, "exited": code}


@mcp.tool()
def sessions() -> list[dict]:
    """List booted sessions and whether each is still running."""
    return [{"session": k, "running": v.proc.poll() is None, "kernel": v.kernel} for k, v in SESSIONS.items()]


@mcp.tool()
def test(release: bool = False, timeout_s: float = 180, arch: str = "riscv64") -> dict:
    """Build the test kernel, boot it, run every ktest. The gate for any change: ok must be true,
    on both architectures (arch = riscv64 or x86_64) when arch code or the boot path changed."""
    args = ["test", *_arch_flag(arch), "--timeout", str(int(timeout_s))] + (["--release"] if release else [])
    try:
        p = _xtask(args, timeout=timeout_s + 600)
    except subprocess.TimeoutExpired:
        return {"ok": False, "reason": "xtask test did not return in time"}
    parsed = [_parse(l) for l in p.stdout.splitlines()]
    summary = next((d["summary"] for d in parsed if "summary" in d), None)
    tests = [d for d in parsed if "test" in d]
    return {
        "ok": p.returncode == 0,
        "summary": summary,
        "tests": tests,
        "console_tail": [d for d in parsed[-40:] if "test" not in d and "summary" not in d],
        "stderr": _tail(p.stderr, 40),
    }


@mcp.tool()
def gdb_inspect(session: str, commands: list[str]) -> dict:
    """Run gdb commands against a session started with boot(gdb=True), e.g.
    ["info registers", "x/8gx $sp", "bt", "continue"]. Each command's output is returned."""
    s = _session(session)
    gdb = next((g for g in GDB_CANDIDATES if shutil.which(g)), None)
    if gdb is None:
        return {
            "ok": False,
            "reason": "no riscv-capable gdb on PATH",
            "install": "xPack riscv-none-elf-gcc (includes gdb) or `winget install LLVM.LLVM` for lldb",
        }
    argv = [gdb, "-q", "-batch", "-ex", "set pagination off", "-ex", "target remote localhost:1234"]
    for c in commands:
        argv += ["-ex", c]
    argv.append(s.kernel)
    p = subprocess.run(argv, cwd=ROOT, capture_output=True, text=True, timeout=60)
    return {"ok": p.returncode == 0, "gdb": gdb, "stdout": _tail(p.stdout, 200), "stderr": _tail(p.stderr, 40)}


@mcp.tool()
def mcp_check(timeout_s: float = 240) -> dict:
    """Boot the kernel and talk to the MCP server running INSIDE Sprout from the host:
    initialize, tools/list, and a shell ping through its HTTP endpoint on localhost:8080.
    While a `boot` session is running you can also add that server to Claude Code yourself:
    claude mcp add --transport http sprout-os http://127.0.0.1:8080/mcp"""
    try:
        p = _xtask(["mcp-check"], timeout=timeout_s)
    except subprocess.TimeoutExpired:
        return {"ok": False, "reason": "mcp-check did not return in time"}
    parsed = [_parse(l) for l in p.stdout.splitlines()]
    result = next((d["mcp_check"] for d in parsed if "mcp_check" in d), None)
    return {"ok": p.returncode == 0, "result": result, "console_tail": parsed[-20:], "stderr": _tail(p.stderr, 40)}


@mcp.tool()
def symbolize(addrs: list[str], ktest: bool = False, release: bool = False, arch: str = "riscv64") -> dict:
    """Resolve kernel addresses (e.g. the "frames" of a panic or trap line) to function, file and line.
    Use ktest=True for addresses from the test kernel. `test` already does this for failures."""
    flags = _arch_flag(arch) + (["--ktest"] if ktest else []) + (["--release"] if release else [])
    p = _xtask(["symbolize", *flags, *addrs], timeout=60)
    if p.returncode != 0:
        return {"ok": False, "stderr": _tail(p.stderr)}
    return {"ok": True, "addrs": [_parse(l) for l in p.stdout.splitlines() if l.strip()]}


@mcp.tool()
def loc() -> str:
    """Line counts per kernel subsystem against the PLAN.md targets."""
    return _xtask(["loc"]).stdout


@mcp.tool()
def lint(arch: str = "riscv64") -> dict:
    """Check that every unsafe block has a // SAFETY: comment."""
    p = _xtask(["lint", *_arch_flag(arch)])
    return {"ok": p.returncode == 0, "output": _tail(p.stdout, 100)}


if __name__ == "__main__":
    mcp.run()
