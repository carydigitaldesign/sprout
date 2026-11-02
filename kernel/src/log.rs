//! Structured logging: every line the kernel prints is one JSON object.
//!
//! ```text
//! {"event":"boot","kernel":"0.1.0","hart":0}
//! {"lvl":"info","mod":"mm","msg":"frame allocator ready","free":16384}
//! ```
//!
//! Build a line with [`event`], [`info`], [`warn`] or [`error`], add fields,
//! and finish with [`Line::end`]. Values are escaped so the output is always
//! valid JSON, which is what `tools/mcp-dev` and `cargo xtask test` parse.
//!
//! A [`Line`] holds the console lock from start to `end`, so lines from
//! different threads and interrupt handlers never interleave.

use core::fmt::{self, Write};

use crate::arch;
use crate::json::{Json, Num};
use crate::sync::{Guard, SpinLock};

static CONSOLE: SpinLock<()> = SpinLock::new(());

/// The boot console as a `fmt::Write` sink.
struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            arch::console_putc(byte);
        }
        Ok(())
    }
}

/// A JSON object being written to the console. Must be finished with `end()`.
#[must_use = "a log line is only printed in full by calling .end()"]
pub struct Line {
    json: Json<Console>,
    _console: Guard<'static, ()>,
}

/// Starts an empty object. Takes the console lock until `end()`.
pub fn line() -> Line {
    let console = CONSOLE.lock();
    let mut json = Json::new(Console);
    json.push_object();
    Line {
        json,
        _console: console,
    }
}

/// Releases the console lock without a guard, so a panic that happens
/// while a line is being built can still be reported.
///
/// # Safety
/// Only from the panic handler, after interrupts are off: the holder is
/// this very context or one that will never run again.
pub unsafe fn force_unlock_console() {
    // SAFETY: forwarded from the caller's contract.
    unsafe { CONSOLE.force_unlock() };
}

/// Starts a line describing a lifecycle event: `boot`, `exit`, `panic`, ...
pub fn event(name: &str) -> Line {
    line().str("event", name)
}

fn leveled(level: &str, module: &str, msg: &str) -> Line {
    line().str("lvl", level).str("mod", module).str("msg", msg)
}

/// Starts an informational line from `module`.
pub fn info(module: &str, msg: &str) -> Line {
    leveled("info", module, msg)
}

/// Starts a warning line from `module`.
pub fn warn(module: &str, msg: &str) -> Line {
    leveled("warn", module, msg)
}

/// Starts an error line from `module`.
pub fn error(module: &str, msg: &str) -> Line {
    leveled("error", module, msg)
}

impl Line {
    /// Adds a string field.
    pub fn str(mut self, key: &str, value: &str) -> Self {
        self.json.str(key, value);
        self
    }

    /// Adds a string field built from format arguments.
    pub fn fmt(mut self, key: &str, args: fmt::Arguments) -> Self {
        self.json.fmt(key, args);
        self
    }

    /// Adds an integer field.
    pub fn num<N: Num>(mut self, key: &str, value: N) -> Self {
        self.json.num(key, value);
        self
    }

    /// Adds a boolean field.
    pub fn bool(mut self, key: &str, value: bool) -> Self {
        self.json.bool(key, value);
        self
    }

    /// Adds an address or bit pattern as a `"0x..."` string.
    pub fn hex(mut self, key: &str, value: usize) -> Self {
        self.json.hex(key, value);
        self
    }

    /// Adds an array of addresses as `"0x..."` strings.
    pub fn hex_list(mut self, key: &str, values: impl IntoIterator<Item = usize>) -> Self {
        self.json.array(key);
        for value in values {
            self.json.push_hex(value);
        }
        self.json.end();
        self
    }

    /// Closes the object and ends the line, releasing the console.
    pub fn end(mut self) {
        self.json.end();
        let _ = Console.write_str("\n");
    }
}
