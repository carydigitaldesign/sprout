//! Just enough JSON for JSON-RPC without a heap: a growable fixed buffer
//! that writes escaped strings, and a scanner that pulls named fields out
//! of a request. The scanner finds the first occurrence of a key, which
//! is right for the MCP messages this server handles.

/// A fixed-capacity byte buffer.
pub struct Buf<const N: usize> {
    data: [u8; N],
    len: usize,
}

impl<const N: usize> Buf<N> {
    pub const fn new() -> Self {
        Buf {
            data: [0; N],
            len: 0,
        }
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.data[..self.len]
    }

    /// Appends bytes; returns false (appending nothing) if they do not fit.
    pub fn push(&mut self, bytes: &[u8]) -> bool {
        if self.len + bytes.len() > N {
            return false;
        }
        self.data[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        true
    }

    pub fn push_str(&mut self, s: &str) -> bool {
        self.push(s.as_bytes())
    }

    pub fn push_num(&mut self, mut n: u64) -> bool {
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        loop {
            i -= 1;
            digits[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        self.push(&digits[i..])
    }

    /// Appends `s` as the inside of a JSON string, escaped.
    pub fn push_escaped(&mut self, s: &[u8]) -> bool {
        for &b in s {
            let ok = match b {
                b'"' => self.push(b"\\\""),
                b'\\' => self.push(b"\\\\"),
                b'\n' => self.push(b"\\n"),
                b'\r' => self.push(b"\\r"),
                b'\t' => self.push(b"\\t"),
                b if b < 0x20 => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    self.push(b"\\u00")
                        && self.push(&[HEX[(b >> 4) as usize], HEX[(b & 15) as usize]])
                }
                b => self.push(&[b]),
            };
            if !ok {
                return false;
            }
        }
        true
    }
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// Position just after `"key":` (and any spaces) for the first occurrence
/// of the key.
fn value_start(json: &[u8], key: &str) -> Option<usize> {
    let mut from = 0;
    loop {
        let at = find(json, b"\"", from)?;
        let end = at + 1 + key.len();
        if json.get(at + 1..end) == Some(key.as_bytes()) && json.get(end) == Some(&b'"') {
            let mut p = end + 1;
            while json.get(p).is_some_and(|b| b.is_ascii_whitespace()) {
                p += 1;
            }
            if json.get(p) == Some(&b':') {
                p += 1;
                while json.get(p).is_some_and(|b| b.is_ascii_whitespace()) {
                    p += 1;
                }
                return Some(p);
            }
        }
        from = at + 1;
    }
}

/// The raw bytes of a string field, between its quotes, still escaped.
pub fn str_field<'a>(json: &'a [u8], key: &str) -> Option<&'a [u8]> {
    let start = value_start(json, key)?;
    if json.get(start) != Some(&b'"') {
        return None;
    }
    let mut p = start + 1;
    while p < json.len() {
        match json[p] {
            b'\\' => p += 2,
            b'"' => return Some(&json[start + 1..p]),
            _ => p += 1,
        }
    }
    None
}

/// The raw JSON of any field: a string with its quotes, a number, a
/// literal, or a balanced object or array.
pub fn raw_field<'a>(json: &'a [u8], key: &str) -> Option<&'a [u8]> {
    let start = value_start(json, key)?;
    let first = *json.get(start)?;
    let end = match first {
        b'"' => {
            let inner = str_field(json, key)?;
            start + inner.len() + 2
        }
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut in_string = false;
            let mut p = start;
            loop {
                let b = *json.get(p)?;
                if in_string {
                    match b {
                        b'\\' => p += 1,
                        b'"' => in_string = false,
                        _ => {}
                    }
                } else {
                    match b {
                        b'"' => in_string = true,
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                break p + 1;
                            }
                        }
                        _ => {}
                    }
                }
                p += 1;
            }
        }
        _ => {
            let mut p = start;
            while json
                .get(p)
                .is_some_and(|b| !matches!(b, b',' | b'}' | b']') && !b.is_ascii_whitespace())
            {
                p += 1;
            }
            p
        }
    };
    Some(&json[start..end])
}

/// Decodes JSON string escapes into `out`; returns the decoded length.
/// `\uXXXX` becomes `?`.
pub fn unescape(raw: &[u8], out: &mut [u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < raw.len() && n < out.len() {
        let b = raw[i];
        if b == b'\\' && i + 1 < raw.len() {
            let (c, skip) = match raw[i + 1] {
                b'n' => (b'\n', 2),
                b't' => (b'\t', 2),
                b'r' => (b'\r', 2),
                b'u' => (b'?', 6),
                other => (other, 2),
            };
            out[n] = c;
            n += 1;
            i += skip;
        } else {
            out[n] = b;
            n += 1;
            i += 1;
        }
    }
    n
}
