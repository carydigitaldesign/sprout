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

use core::fmt::{self, Write};

use crate::arch;

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

/// Wraps a sink and escapes everything written for the inside of a JSON string.
struct Escaper<'a, W: Write>(&'a mut W);

impl<W: Write> Write for Escaper<'_, W> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            match c {
                '"' => self.0.write_str("\\\"")?,
                '\\' => self.0.write_str("\\\\")?,
                '\n' => self.0.write_str("\\n")?,
                '\r' => self.0.write_str("\\r")?,
                '\t' => self.0.write_str("\\t")?,
                c if (c as u32) < 0x20 => write!(self.0, "\\u{:04x}", c as u32)?,
                c => self.0.write_char(c)?,
            }
        }
        Ok(())
    }
}

/// A JSON object being written to the console. Must be finished with `end()`.
#[must_use = "a log line is only printed in full by calling .end()"]
pub struct Line {
    first: bool,
}

/// Integer types that print as bare JSON numbers.
pub trait Num: fmt::Display {}
macro_rules! num {
    ($($t:ty)*) => { $(impl Num for $t {})* };
}
num!(u8 u16 u32 u64 usize i8 i16 i32 i64 isize);

/// Starts an empty object.
pub fn line() -> Line {
    let _ = Console.write_str("{");
    Line { first: true }
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
    fn key(&mut self, key: &str) {
        if !self.first {
            let _ = Console.write_str(",");
        }
        self.first = false;
        let _ = Console.write_str("\"");
        let _ = Escaper(&mut Console).write_str(key);
        let _ = Console.write_str("\":");
    }

    /// Adds a string field.
    pub fn str(self, key: &str, value: &str) -> Self {
        self.fmt(key, format_args!("{value}"))
    }

    /// Adds a string field built from format arguments.
    pub fn fmt(mut self, key: &str, args: fmt::Arguments) -> Self {
        self.key(key);
        let _ = Console.write_str("\"");
        let _ = Escaper(&mut Console).write_fmt(args);
        let _ = Console.write_str("\"");
        self
    }

    /// Adds an integer field.
    pub fn num<N: Num>(mut self, key: &str, value: N) -> Self {
        self.key(key);
        let _ = write!(Console, "{value}");
        self
    }

    /// Adds a boolean field.
    pub fn bool(mut self, key: &str, value: bool) -> Self {
        self.key(key);
        let _ = Console.write_str(if value { "true" } else { "false" });
        self
    }

    /// Adds an address or bit pattern as a `"0x..."` string.
    pub fn hex(mut self, key: &str, value: usize) -> Self {
        self.key(key);
        let _ = write!(Console, "\"{value:#x}\"");
        self
    }

    /// Adds an array of addresses as `"0x..."` strings.
    pub fn hex_list(mut self, key: &str, values: impl IntoIterator<Item = usize>) -> Self {
        self.key(key);
        let _ = Console.write_str("[");
        for (i, value) in values.into_iter().enumerate() {
            if i > 0 {
                let _ = Console.write_str(",");
            }
            let _ = write!(Console, "\"{value:#x}\"");
        }
        let _ = Console.write_str("]");
        self
    }

    /// Closes the object and ends the line.
    pub fn end(self) {
        let _ = Console.write_str("}\n");
    }
}

crate::ktest! {
    fn escaper_produces_valid_json_strings() {
        struct Buf {
            data: [u8; 64],
            len: usize,
        }
        impl Write for Buf {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                for b in s.bytes() {
                    if self.len == self.data.len() {
                        return Err(fmt::Error);
                    }
                    self.data[self.len] = b;
                    self.len += 1;
                }
                Ok(())
            }
        }

        let mut buf = Buf { data: [0; 64], len: 0 };
        Escaper(&mut buf).write_str("a\"b\\c\nd\te\u{1}f").unwrap();
        let got = core::str::from_utf8(&buf.data[..buf.len]).unwrap();
        assert_eq!(got, "a\\\"b\\\\c\\nd\\te\\u0001f");
    }
}
