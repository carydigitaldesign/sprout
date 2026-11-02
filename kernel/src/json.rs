//! A small JSON writer over any `fmt::Write`: log lines go to the console,
//! `describe` results go to a `String`. Nesting is tracked so commas land
//! in the right places; strings are escaped so the output always parses.

use core::fmt::{self, Write};

const MAX_DEPTH: usize = 16;

/// Integer types that print as bare JSON numbers.
pub trait Num: fmt::Display {}
macro_rules! num {
    ($($t:ty)*) => { $(impl Num for $t {})* };
}
num!(u8 u16 u32 u64 usize i8 i16 i32 i64 isize);

/// Wraps a sink and escapes everything written for the inside of a JSON string.
pub struct Escaper<'a, W: Write>(pub &'a mut W);

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

/// Builds JSON incrementally. Methods with a `key` add a member to the
/// enclosing object; `push_*` methods add an element to the enclosing array.
pub struct Json<W: Write> {
    out: W,
    depth: usize,
    first: [bool; MAX_DEPTH],
    closers: [char; MAX_DEPTH],
}

impl<W: Write> Json<W> {
    pub fn new(out: W) -> Self {
        Json {
            out,
            depth: 0,
            first: [true; MAX_DEPTH],
            closers: ['}'; MAX_DEPTH],
        }
    }

    /// The sink, once every container is closed.
    pub fn into_inner(self) -> W {
        self.out
    }

    fn separator(&mut self) {
        if self.depth > 0 {
            if !self.first[self.depth - 1] {
                let _ = self.out.write_char(',');
            }
            self.first[self.depth - 1] = false;
        }
    }

    fn key(&mut self, key: &str) {
        self.separator();
        self.quoted(format_args!("{key}"));
        let _ = self.out.write_char(':');
    }

    fn quoted(&mut self, args: fmt::Arguments) {
        let _ = self.out.write_char('"');
        let _ = Escaper(&mut self.out).write_fmt(args);
        let _ = self.out.write_char('"');
    }

    fn open(&mut self, opener: char, closer: char) {
        assert!(self.depth < MAX_DEPTH, "JSON nested too deep");
        let _ = self.out.write_char(opener);
        self.first[self.depth] = true;
        self.closers[self.depth] = closer;
        self.depth += 1;
    }

    /// Opens an object as a member.
    pub fn object(&mut self, key: &str) -> &mut Self {
        self.key(key);
        self.open('{', '}');
        self
    }

    /// Opens an object as an array element, or at the top level.
    pub fn push_object(&mut self) -> &mut Self {
        self.separator();
        self.open('{', '}');
        self
    }

    /// Opens an array as a member.
    pub fn array(&mut self, key: &str) -> &mut Self {
        self.key(key);
        self.open('[', ']');
        self
    }

    /// Opens an array as an array element.
    pub fn push_array(&mut self) -> &mut Self {
        self.separator();
        self.open('[', ']');
        self
    }

    /// Closes the innermost object or array.
    pub fn end(&mut self) -> &mut Self {
        assert!(self.depth > 0, "JSON end without open");
        self.depth -= 1;
        let _ = self.out.write_char(self.closers[self.depth]);
        self
    }

    pub fn str(&mut self, key: &str, value: &str) -> &mut Self {
        self.fmt(key, format_args!("{value}"))
    }

    pub fn fmt(&mut self, key: &str, args: fmt::Arguments) -> &mut Self {
        self.key(key);
        self.quoted(args);
        self
    }

    pub fn num<N: Num>(&mut self, key: &str, value: N) -> &mut Self {
        self.key(key);
        let _ = write!(self.out, "{value}");
        self
    }

    pub fn bool(&mut self, key: &str, value: bool) -> &mut Self {
        self.key(key);
        let _ = self.out.write_str(if value { "true" } else { "false" });
        self
    }

    /// An address or bit pattern as a `"0x..."` string.
    pub fn hex(&mut self, key: &str, value: usize) -> &mut Self {
        self.key(key);
        let _ = write!(self.out, "\"{value:#x}\"");
        self
    }

    pub fn push_str(&mut self, value: &str) -> &mut Self {
        self.separator();
        self.quoted(format_args!("{value}"));
        self
    }

    pub fn push_fmt(&mut self, args: fmt::Arguments) -> &mut Self {
        self.separator();
        self.quoted(args);
        self
    }

    pub fn push_num<N: Num>(&mut self, value: N) -> &mut Self {
        self.separator();
        let _ = write!(self.out, "{value}");
        self
    }

    pub fn push_bool(&mut self, value: bool) -> &mut Self {
        self.separator();
        let _ = self.out.write_str(if value { "true" } else { "false" });
        self
    }

    pub fn push_hex(&mut self, value: usize) -> &mut Self {
        self.separator();
        let _ = write!(self.out, "\"{value:#x}\"");
        self
    }
}

crate::ktest! {
    fn escaper_produces_valid_json_strings() {
        let mut out = alloc::string::String::new();
        Escaper(&mut out).write_str("a\"b\\c\nd\te\u{1}f").unwrap();
        assert_eq!(out, "a\\\"b\\\\c\\nd\\te\\u0001f");
    }
}

crate::ktest! {
    fn nested_objects_and_arrays_get_their_commas() {
        let mut j = Json::new(alloc::string::String::new());
        j.push_object();
        j.str("name", "sprout").num("n", 3usize).bool("ok", true);
        j.array("list").push_num(1u8).push_str("two").push_object().hex("a", 255).end().end();
        j.object("inner").end();
        j.end();
        assert_eq!(
            j.into_inner(),
            r#"{"name":"sprout","n":3,"ok":true,"list":[1,"two",{"a":"0xff"}],"inner":{}}"#
        );
    }
}
