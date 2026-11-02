//! Reading a `newc` cpio archive in memory: the initramfs format. Each
//! entry is a 110-byte ASCII header, the name (NUL-terminated, padded to
//! 4), and the data (padded to 4). The archive ends at `TRAILER!!!`.

const MAGIC: &[u8; 6] = b"070701";
const HEADER_LEN: usize = 110;
const TRAILER: &[u8] = b"TRAILER!!!";

/// One file in the archive.
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
}

fn hex_field(header: &[u8], index: usize) -> Option<usize> {
    let at = 6 + index * 8;
    let field = header.get(at..at + 8)?;
    let mut value = 0usize;
    for &b in field {
        value = value
            .checked_mul(16)?
            .checked_add((b as char).to_digit(16)? as usize)?;
    }
    Some(value)
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// Iterates the entries of `archive`. Stops at the trailer or at the
/// first malformed header.
pub fn entries(archive: &[u8]) -> impl Iterator<Item = Entry<'_>> {
    let mut at = 0usize;
    core::iter::from_fn(move || {
        let header = archive.get(at..at + HEADER_LEN)?;
        if &header[..6] != MAGIC {
            return None;
        }
        let filesize = hex_field(header, 6)?;
        let namesize = hex_field(header, 11)?;
        let name_start = at + HEADER_LEN;
        let name_bytes = archive.get(name_start..name_start + namesize)?;
        let name_len = name_bytes.iter().position(|&b| b == 0).unwrap_or(namesize);
        let name = core::str::from_utf8(&name_bytes[..name_len]).ok()?;
        let data_start = align4(name_start + namesize);
        let data = archive.get(data_start..data_start + filesize)?;
        at = align4(data_start + filesize);
        if name.as_bytes() == TRAILER {
            return None;
        }
        Some(Entry { name, data })
    })
}

/// The data of the entry called `name`, if present.
pub fn find<'a>(archive: &'a [u8], name: &str) -> Option<&'a [u8]> {
    entries(archive).find(|e| e.name == name).map(|e| e.data)
}
