//! The Sprout ABI: everything the kernel and userspace agree on, in one
//! place (PLAN.md 2.2, "schema in one place").
//!
//! The kernel implements [`SYSCALLS`]; `sprout-rt` wraps them; the kernel's
//! `describe` syscall renders this same table as JSON, which is what makes
//! the kernel self-describing and what the MCP bridge turns into tools.
//!
//! Calling convention (riscv64): syscall number in `a7`, up to six
//! arguments in `a0`..`a5`, result in `a0`. A non-negative result is the
//! value; a negative one is `-(Error as isize)`.

#![no_std]

/// Index into a process's handle table. Zero is never a real handle; passed
/// to `describe` it means "the kernel itself".
pub type Handle = u32;

/// The pseudo-handle naming the kernel, for `describe`.
pub const HANDLE_KERNEL: Handle = 0;

/// Every process starts with its own process object at this handle.
pub const HANDLE_SELF_PROCESS: Handle = 1;

/// Lowest address a program is linked at (see `lib/sprout-rt/user.ld`).
pub const USER_IMAGE_BASE: usize = 0x1_0000;
/// User stacks grow down from here.
pub const USER_STACK_TOP: usize = 0x4000_0000;
pub const USER_STACK_SIZE: usize = 64 * 1024;
/// User addresses are below this; the kernel's half starts far above.
pub const USER_ADDRESS_LIMIT: usize = 0x40_0000_0000;

/// Syscall numbers.
pub mod sys {
    pub const LOG: usize = 0;
    pub const EXIT: usize = 1;
    pub const YIELD: usize = 2;
    pub const SLEEP: usize = 3;
    pub const DESCRIBE: usize = 4;
    pub const HANDLE_CLOSE: usize = 5;
    pub const VMO_CREATE: usize = 6;
    pub const VMO_READ: usize = 7;
    pub const VMO_WRITE: usize = 8;
}

/// Why a syscall failed. Returned as `-(code)` in `a0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(isize)]
pub enum Error {
    NoSuchSyscall = 1,
    InvalidHandle = 2,
    WrongType = 3,
    AccessDenied = 4,
    InvalidArgument = 5,
    BadAddress = 6,
    OutOfMemory = 7,
    OutOfRange = 8,
    BufferTooSmall = 9,
}

impl Error {
    /// The raw syscall return value for this error.
    pub const fn code(self) -> isize {
        -(self as isize)
    }

    /// Decodes a raw syscall return value.
    pub const fn from_code(code: isize) -> Option<Error> {
        match -code {
            1 => Some(Error::NoSuchSyscall),
            2 => Some(Error::InvalidHandle),
            3 => Some(Error::WrongType),
            4 => Some(Error::AccessDenied),
            5 => Some(Error::InvalidArgument),
            6 => Some(Error::BadAddress),
            7 => Some(Error::OutOfMemory),
            8 => Some(Error::OutOfRange),
            9 => Some(Error::BufferTooSmall),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Error::NoSuchSyscall => "no such syscall",
            Error::InvalidHandle => "invalid handle",
            Error::WrongType => "wrong object type",
            Error::AccessDenied => "access denied",
            Error::InvalidArgument => "invalid argument",
            Error::BadAddress => "bad address",
            Error::OutOfMemory => "out of memory",
            Error::OutOfRange => "out of range",
            Error::BufferTooSmall => "buffer too small",
        }
    }
}

/// Turns a raw return value into a `Result`.
pub const fn decode(raw: isize) -> Result<usize, Error> {
    if raw >= 0 {
        Ok(raw as usize)
    } else {
        match Error::from_code(raw) {
            Some(e) => Err(e),
            None => Err(Error::InvalidArgument),
        }
    }
}

/// What a handle lets its holder do. Capabilities are the security model:
/// a process can only reach what it was handed, and only in these ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights(pub u32);

impl Rights {
    pub const NONE: Rights = Rights(0);
    pub const READ: Rights = Rights(1 << 0);
    pub const WRITE: Rights = Rights(1 << 1);
    pub const MAP: Rights = Rights(1 << 2);
    pub const DUPLICATE: Rights = Rights(1 << 3);
    pub const TRANSFER: Rights = Rights(1 << 4);
    pub const DESCRIBE: Rights = Rights(1 << 5);
    pub const ALL: Rights = Rights((1 << 6) - 1);

    const NAMES: &[(Rights, &str)] = &[
        (Rights::READ, "read"),
        (Rights::WRITE, "write"),
        (Rights::MAP, "map"),
        (Rights::DUPLICATE, "duplicate"),
        (Rights::TRANSFER, "transfer"),
        (Rights::DESCRIBE, "describe"),
    ];

    pub const fn contains(self, other: Rights) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Rights) -> Rights {
        Rights(self.0 | other.0)
    }

    /// The names of the rights set, for descriptions.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        Self::NAMES
            .iter()
            .filter(move |(r, _)| self.contains(*r))
            .map(|(_, n)| *n)
    }
}

impl core::ops::BitOr for Rights {
    type Output = Rights;
    fn bitor(self, rhs: Rights) -> Rights {
        self.union(rhs)
    }
}

/// Kinds of kernel object a handle can refer to (PLAN.md 2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Process,
    Thread,
    Vmo,
    AddressSpace,
}

impl ObjectType {
    pub const ALL: &[ObjectType] = &[
        ObjectType::Process,
        ObjectType::Thread,
        ObjectType::Vmo,
        ObjectType::AddressSpace,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            ObjectType::Process => "process",
            ObjectType::Thread => "thread",
            ObjectType::Vmo => "vmo",
            ObjectType::AddressSpace => "address_space",
        }
    }
}

/// One argument of a syscall.
#[derive(Debug, Clone, Copy)]
pub struct Arg {
    pub name: &'static str,
    /// A type name as a human and an agent would read it.
    pub ty: &'static str,
}

const fn arg(name: &'static str, ty: &'static str) -> Arg {
    Arg { name, ty }
}

/// One syscall, as the kernel describes it.
#[derive(Debug, Clone, Copy)]
pub struct Syscall {
    pub num: usize,
    pub name: &'static str,
    pub args: &'static [Arg],
    /// What a non-negative result means.
    pub returns: &'static str,
    /// The object type the first argument's handle must have, if any.
    pub object: Option<ObjectType>,
    /// Rights that handle needs.
    pub rights: Rights,
    pub doc: &'static str,
}

/// Every syscall the kernel implements. Order is by number.
pub const SYSCALLS: &[Syscall] = &[
    Syscall {
        num: sys::LOG,
        name: "log",
        args: &[arg("ptr", "*const u8"), arg("len", "usize")],
        returns: "0",
        object: None,
        rights: Rights::NONE,
        doc: "Write a UTF-8 message to the kernel log as a JSON line.",
    },
    Syscall {
        num: sys::EXIT,
        name: "exit",
        args: &[arg("code", "i32")],
        returns: "never",
        object: None,
        rights: Rights::NONE,
        doc: "End the calling process with an exit code.",
    },
    Syscall {
        num: sys::YIELD,
        name: "yield",
        args: &[],
        returns: "0",
        object: None,
        rights: Rights::NONE,
        doc: "Let other threads run.",
    },
    Syscall {
        num: sys::SLEEP,
        name: "sleep",
        args: &[arg("ms", "u64")],
        returns: "0",
        object: None,
        rights: Rights::NONE,
        doc: "Block for at least this many milliseconds.",
    },
    Syscall {
        num: sys::DESCRIBE,
        name: "describe",
        args: &[
            arg("handle", "Handle"),
            arg("buf", "*mut u8"),
            arg("len", "usize"),
        ],
        returns: "bytes the full description needs; buf holds min(len, that)",
        object: None,
        rights: Rights::DESCRIBE,
        doc: "Write a JSON description of a handle's object, or of the kernel for handle 0: \
              its type, the caller's rights, and the operations available.",
    },
    Syscall {
        num: sys::HANDLE_CLOSE,
        name: "handle_close",
        args: &[arg("handle", "Handle")],
        returns: "0",
        object: None,
        rights: Rights::NONE,
        doc: "Drop a handle. The object lives on while anything else refers to it.",
    },
    Syscall {
        num: sys::VMO_CREATE,
        name: "vmo_create",
        args: &[arg("size", "usize")],
        returns: "a new handle with all rights",
        object: None,
        rights: Rights::NONE,
        doc: "Create a zero-filled virtual memory object of at least size bytes.",
    },
    Syscall {
        num: sys::VMO_READ,
        name: "vmo_read",
        args: &[
            arg("handle", "Handle"),
            arg("offset", "usize"),
            arg("buf", "*mut u8"),
            arg("len", "usize"),
        ],
        returns: "0",
        object: Some(ObjectType::Vmo),
        rights: Rights::READ,
        doc: "Copy len bytes out of the VMO starting at offset.",
    },
    Syscall {
        num: sys::VMO_WRITE,
        name: "vmo_write",
        args: &[
            arg("handle", "Handle"),
            arg("offset", "usize"),
            arg("buf", "*const u8"),
            arg("len", "usize"),
        ],
        returns: "0",
        object: Some(ObjectType::Vmo),
        rights: Rights::WRITE,
        doc: "Copy len bytes into the VMO starting at offset.",
    },
];

/// Looks a syscall up by number.
pub fn syscall(num: usize) -> Option<&'static Syscall> {
    SYSCALLS.iter().find(|s| s.num == num)
}
