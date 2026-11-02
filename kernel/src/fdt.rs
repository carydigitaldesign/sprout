//! A minimal flattened device tree reader: enough to find RAM now and
//! virtio devices later (M6). Spec: <https://devicetree-specification.readthedocs.io>.
//!
//! The blob is read in place; nothing is copied or allocated.

use core::fmt;

const MAGIC: u32 = 0xd00d_feed;

const TOKEN_BEGIN_NODE: u32 = 1;
const TOKEN_END_NODE: u32 = 2;
const TOKEN_PROP: u32 = 3;
const TOKEN_NOP: u32 = 4;
const TOKEN_END: u32 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdtError {
    BadMagic(u32),
    Truncated,
}

impl fmt::Display for FdtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic(m) => write!(f, "bad device tree magic {m:#x}"),
            Self::Truncated => f.write_str("device tree is truncated"),
        }
    }
}

/// A physical address range reported by the device tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub base: usize,
    pub size: usize,
}

/// A parsed device tree header over a blob in memory.
pub struct Fdt<'a> {
    structs: &'a [u8],
    strings: &'a [u8],
    total_size: usize,
}

fn be32(bytes: &[u8], offset: usize) -> Option<u32> {
    let b = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be64(bytes: &[u8], offset: usize) -> Option<u64> {
    let hi = be32(bytes, offset)? as u64;
    let lo = be32(bytes, offset + 4)? as u64;
    Some((hi << 32) | lo)
}

fn cstr(bytes: &[u8], offset: usize) -> &str {
    let rest = bytes.get(offset..).unwrap_or(&[]);
    let len = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    core::str::from_utf8(&rest[..len]).unwrap_or("")
}

impl<'a> Fdt<'a> {
    /// Reads the header of the blob at `ptr`.
    ///
    /// # Safety
    /// `ptr` must point at a complete device tree blob that stays mapped and
    /// unmodified for `'a`.
    pub unsafe fn from_ptr(ptr: *const u8) -> Result<Fdt<'a>, FdtError> {
        // SAFETY: the caller promises a complete blob, and a blob starts
        // with a 40-byte header.
        let header = unsafe { core::slice::from_raw_parts(ptr, 40) };
        let magic = be32(header, 0).ok_or(FdtError::Truncated)?;
        if magic != MAGIC {
            return Err(FdtError::BadMagic(magic));
        }
        let total_size = be32(header, 4).ok_or(FdtError::Truncated)? as usize;
        let off_struct = be32(header, 8).ok_or(FdtError::Truncated)? as usize;
        let off_strings = be32(header, 12).ok_or(FdtError::Truncated)? as usize;
        let size_strings = be32(header, 32).ok_or(FdtError::Truncated)? as usize;
        let size_struct = be32(header, 36).ok_or(FdtError::Truncated)? as usize;

        // SAFETY: `total_size` is the blob's own declared length, which the
        // caller promised is all present.
        let blob = unsafe { core::slice::from_raw_parts(ptr, total_size) };
        let structs = blob
            .get(off_struct..off_struct + size_struct)
            .ok_or(FdtError::Truncated)?;
        let strings = blob
            .get(off_strings..off_strings + size_strings)
            .ok_or(FdtError::Truncated)?;
        Ok(Fdt {
            structs,
            strings,
            total_size,
        })
    }

    /// Size of the whole blob in bytes, for reserving its memory.
    pub fn total_size(&self) -> usize {
        self.total_size
    }

    /// Walks the structure block token by token.
    pub fn tokens(&self) -> Tokens<'a> {
        Tokens {
            structs: self.structs,
            strings: self.strings,
            pos: 0,
        }
    }

    /// Every `reg` range of every `memory` node, in blob order, up to `N`.
    pub fn memory_regions<const N: usize>(&self) -> [Option<Region>; N] {
        let mut regions = [None; N];
        let mut found = 0;
        let mut depth = 0u32;
        let mut in_memory = false;
        let mut address_cells = 2u32;
        let mut size_cells = 2u32;

        for token in self.tokens() {
            match token {
                Token::BeginNode(name) => {
                    depth += 1;
                    in_memory = depth == 2 && (name == "memory" || name.starts_with("memory@"));
                }
                Token::EndNode => {
                    depth = depth.saturating_sub(1);
                    in_memory = false;
                }
                Token::Prop { name, value } if depth == 1 => match name {
                    "#address-cells" => address_cells = be32(value, 0).unwrap_or(2),
                    "#size-cells" => size_cells = be32(value, 0).unwrap_or(2),
                    _ => {}
                },
                Token::Prop { name: "reg", value } if in_memory => {
                    let cell = |cells: u32, at: usize| -> Option<usize> {
                        match cells {
                            1 => be32(value, at).map(|v| v as usize),
                            2 => be64(value, at).map(|v| v as usize),
                            _ => None,
                        }
                    };
                    let stride = ((address_cells + size_cells) * 4) as usize;
                    let mut at = 0;
                    while at + stride <= value.len() && found < N {
                        if let (Some(base), Some(size)) = (
                            cell(address_cells, at),
                            cell(size_cells, at + address_cells as usize * 4),
                        ) {
                            regions[found] = Some(Region { base, size });
                            found += 1;
                        }
                        at += stride;
                    }
                }
                Token::Prop { .. } => {}
                Token::End => break,
            }
        }
        regions
    }
}

impl<'a> Fdt<'a> {
    /// The raw value of `property` on the first top-level node called
    /// `node` (exact name or `node@...`).
    pub fn node_property(&self, node: &str, property: &str) -> Option<&'a [u8]> {
        let mut depth = 0u32;
        let mut inside = false;
        for token in self.tokens() {
            match token {
                Token::BeginNode(name) => {
                    depth += 1;
                    inside = depth == 2
                        && (name == node
                            || name
                                .strip_prefix(node)
                                .is_some_and(|rest| rest.starts_with('@')));
                }
                Token::EndNode => {
                    depth = depth.saturating_sub(1);
                    inside = false;
                }
                Token::Prop { name, value } if inside && name == property => return Some(value),
                Token::Prop { .. } => {}
                Token::End => break,
            }
        }
        None
    }

    /// Rate of the platform timer from `/cpus/timebase-frequency`.
    pub fn timebase_frequency(&self) -> Option<u64> {
        let value = self.node_property("cpus", "timebase-frequency")?;
        match value.len() {
            4 => be32(value, 0).map(u64::from),
            8 => be64(value, 0),
            _ => None,
        }
    }
}

/// One structure-block token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token<'a> {
    BeginNode(&'a str),
    EndNode,
    Prop { name: &'a str, value: &'a [u8] },
    End,
}

/// Iterator behind [`Fdt::tokens`]. Ends after [`Token::End`] or on a
/// malformed blob.
pub struct Tokens<'a> {
    structs: &'a [u8],
    strings: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        loop {
            let token = be32(self.structs, self.pos)?;
            self.pos += 4;
            match token {
                TOKEN_BEGIN_NODE => {
                    let name = cstr(self.structs, self.pos);
                    self.pos = (self.pos + name.len() + 1 + 3) & !3;
                    return Some(Token::BeginNode(name));
                }
                TOKEN_END_NODE => return Some(Token::EndNode),
                TOKEN_PROP => {
                    let len = be32(self.structs, self.pos)? as usize;
                    let name_off = be32(self.structs, self.pos + 4)? as usize;
                    let value = self.structs.get(self.pos + 8..self.pos + 8 + len)?;
                    self.pos = (self.pos + 8 + len + 3) & !3;
                    return Some(Token::Prop {
                        name: cstr(self.strings, name_off),
                        value,
                    });
                }
                TOKEN_NOP => continue,
                TOKEN_END => {
                    self.pos = self.structs.len();
                    return Some(Token::End);
                }
                _ => return None,
            }
        }
    }
}

crate::ktest! {
    fn parses_a_hand_built_blob() {
        // Root with #address-cells=2, #size-cells=2, one child
        // "memory@80000000" with reg = <0x0 0x80000000 0x0 0x08000000>.
        const STRINGS: &[u8] = b"#address-cells\0#size-cells\0reg\0";
        let mut blob = [0u8; 256];
        let mut p = 40;
        let mut put32 = |blob: &mut [u8], v: u32| {
            blob[p..p + 4].copy_from_slice(&v.to_be_bytes());
            p += 4;
        };
        put32(&mut blob, TOKEN_BEGIN_NODE);
        put32(&mut blob, 0); // root name "" padded
        put32(&mut blob, TOKEN_PROP);
        put32(&mut blob, 4);
        put32(&mut blob, 0); // "#address-cells"
        put32(&mut blob, 2);
        put32(&mut blob, TOKEN_PROP);
        put32(&mut blob, 4);
        put32(&mut blob, 15); // "#size-cells"
        put32(&mut blob, 2);
        put32(&mut blob, TOKEN_BEGIN_NODE);
        for chunk in b"memory@80000000\0".chunks(4) {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            put32(&mut blob, u32::from_be_bytes(word));
        }
        put32(&mut blob, TOKEN_PROP);
        put32(&mut blob, 16);
        put32(&mut blob, 27); // "reg"
        put32(&mut blob, 0);
        put32(&mut blob, 0x8000_0000);
        put32(&mut blob, 0);
        put32(&mut blob, 0x0800_0000);
        put32(&mut blob, TOKEN_END_NODE);
        put32(&mut blob, TOKEN_END_NODE);
        put32(&mut blob, TOKEN_END);
        let struct_end = p;
        blob[p..p + STRINGS.len()].copy_from_slice(STRINGS);
        let strings_end = p + STRINGS.len();

        // Header.
        blob[0..4].copy_from_slice(&MAGIC.to_be_bytes());
        blob[4..8].copy_from_slice(&(strings_end as u32).to_be_bytes());
        blob[8..12].copy_from_slice(&40u32.to_be_bytes());
        blob[12..16].copy_from_slice(&(struct_end as u32).to_be_bytes());
        blob[32..36].copy_from_slice(&(STRINGS.len() as u32).to_be_bytes());
        blob[36..40].copy_from_slice(&((struct_end - 40) as u32).to_be_bytes());

        // SAFETY: `blob` is a complete device tree that outlives `fdt`.
        let fdt = unsafe { Fdt::from_ptr(blob.as_ptr()) }.expect("valid blob");
        assert_eq!(fdt.total_size(), strings_end);
        let regions = fdt.memory_regions::<2>();
        assert_eq!(
            regions[0],
            Some(Region {
                base: 0x8000_0000,
                size: 0x0800_0000
            })
        );
        assert_eq!(regions[1], None);
    }
}

crate::ktest! {
    fn rejects_bad_magic() {
        let blob = [0u8; 64];
        // SAFETY: 64 zero bytes are a complete (if invalid) header-sized blob.
        let err = unsafe { Fdt::from_ptr(blob.as_ptr()) }.err();
        assert_eq!(err, Some(FdtError::BadMagic(0)));
    }
}
