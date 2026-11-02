//! Just enough ELF64 to load a static, non-PIE riscv64 program: the header
//! and its `PT_LOAD` segments. Nothing is copied; segment data is sliced
//! out of the image on demand.

use core::fmt;

const MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const CLASS_64: u8 = 2;
const TYPE_EXEC: u16 = 2;
/// `e_machine` for the architecture this kernel runs on; a program built
/// for another one is refused.
#[cfg(target_arch = "riscv64")]
const MACHINE: u16 = 243;
#[cfg(target_arch = "x86_64")]
const MACHINE: u16 = 62;
const PT_LOAD: u32 = 1;
const PROGRAM_HEADER_SIZE: usize = 56;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    TooShort,
    BadMagic,
    Not64Bit,
    NotExecutable,
    WrongMachine,
    BadProgramHeaders,
    SegmentOutsideImage,
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ElfError::TooShort => "file too short for an ELF header",
            ElfError::BadMagic => "not an ELF file",
            ElfError::Not64Bit => "not a 64-bit ELF",
            ElfError::NotExecutable => "not a static executable (ET_EXEC)",
            ElfError::WrongMachine => "built for another architecture",
            ElfError::BadProgramHeaders => "program header table is malformed",
            ElfError::SegmentOutsideImage => "a segment points outside the file",
        })
    }
}

/// A loadable segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub vaddr: usize,
    pub memsz: usize,
    pub filesz: usize,
    pub offset: usize,
    pub read: bool,
    pub write: bool,
    pub exec: bool,
}

pub struct Elf<'a> {
    bytes: &'a [u8],
    entry: usize,
    phoff: usize,
    phentsize: usize,
    phnum: usize,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|x| u16::from_le_bytes([x[0], x[1]]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8)
        .map(|x| u64::from_le_bytes([x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7]]))
}

impl<'a> Elf<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Elf<'a>, ElfError> {
        if bytes.len() < 64 {
            return Err(ElfError::TooShort);
        }
        if bytes[..4] != MAGIC {
            return Err(ElfError::BadMagic);
        }
        if bytes[4] != CLASS_64 {
            return Err(ElfError::Not64Bit);
        }
        if u16_at(bytes, 16) != Some(TYPE_EXEC) {
            return Err(ElfError::NotExecutable);
        }
        if u16_at(bytes, 18) != Some(MACHINE) {
            return Err(ElfError::WrongMachine);
        }
        let entry = u64_at(bytes, 24).ok_or(ElfError::TooShort)? as usize;
        let phoff = u64_at(bytes, 32).ok_or(ElfError::TooShort)? as usize;
        let phentsize = u16_at(bytes, 54).ok_or(ElfError::TooShort)? as usize;
        let phnum = u16_at(bytes, 56).ok_or(ElfError::TooShort)? as usize;
        if phentsize < PROGRAM_HEADER_SIZE
            || phoff
                .checked_add(phentsize * phnum)
                .is_none_or(|end| end > bytes.len())
        {
            return Err(ElfError::BadProgramHeaders);
        }
        Ok(Elf {
            bytes,
            entry,
            phoff,
            phentsize,
            phnum,
        })
    }

    /// Address of the first instruction.
    pub fn entry(&self) -> usize {
        self.entry
    }

    /// The `PT_LOAD` segments, in table order.
    pub fn segments(&self) -> impl Iterator<Item = Result<Segment, ElfError>> + '_ {
        (0..self.phnum).filter_map(move |i| {
            let at = self.phoff + i * self.phentsize;
            let header = &self.bytes[at..at + PROGRAM_HEADER_SIZE];
            if u32_at(header, 0) != Some(PT_LOAD) {
                return None;
            }
            let flags = u32_at(header, 4).unwrap_or(0);
            let offset = u64_at(header, 8).unwrap_or(0) as usize;
            let vaddr = u64_at(header, 16).unwrap_or(0) as usize;
            let filesz = u64_at(header, 32).unwrap_or(0) as usize;
            let memsz = u64_at(header, 40).unwrap_or(0) as usize;
            if filesz > memsz
                || offset
                    .checked_add(filesz)
                    .is_none_or(|end| end > self.bytes.len())
            {
                return Some(Err(ElfError::SegmentOutsideImage));
            }
            Some(Ok(Segment {
                vaddr,
                memsz,
                filesz,
                offset,
                read: flags & 0x4 != 0,
                write: flags & 0x2 != 0,
                exec: flags & 0x1 != 0,
            }))
        })
    }

    /// The bytes a segment loads from the file (its `filesz` part).
    pub fn segment_data(&self, segment: &Segment) -> &'a [u8] {
        &self.bytes[segment.offset..segment.offset + segment.filesz]
    }
}

crate::ktest! {
    fn parses_the_embedded_init_program() {
        let image = sprout_abi::cpio::find(crate::user_image::INITRAMFS, "init").expect("init in initramfs");
        let elf = Elf::parse(image).expect("init is a valid ELF");
        assert_eq!(elf.entry(), sprout_abi::USER_IMAGE_BASE, "_start is linked first");
        let segments: alloc::vec::Vec<Segment> = elf.segments().map(|s| s.expect("segment")).collect();
        assert!(!segments.is_empty());
        assert!(segments.iter().any(|s| s.exec), "a text segment");
        assert!(segments.iter().all(|s| s.vaddr >= sprout_abi::USER_IMAGE_BASE));
    }
}

crate::ktest! {
    fn rejects_garbage() {
        assert_eq!(Elf::parse(&[0; 10]).err(), Some(ElfError::TooShort));
        assert_eq!(Elf::parse(&[0; 64]).err(), Some(ElfError::BadMagic));
        let mut bytes = [0u8; 64];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[4] = CLASS_64;
        assert_eq!(Elf::parse(&bytes).err(), Some(ElfError::NotExecutable));
    }
}
