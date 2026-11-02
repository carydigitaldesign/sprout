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

/// The initramfs archive format, read by the kernel and by userspace.
pub mod cpio;

/// Index into a process's handle table. Zero is never a real handle; passed
/// to `describe` it means "the kernel itself".
pub type Handle = u32;

/// The pseudo-handle naming the kernel, for `describe`.
pub const HANDLE_KERNEL: Handle = 0;

/// Every process starts with its own process object at this handle.
pub const HANDLE_SELF_PROCESS: Handle = 1;

/// The handle a spawner passes along (normally a channel endpoint). For
/// `init`, which nobody spawns, the kernel puts the `shell` program image
/// here instead.
pub const HANDLE_BOOTSTRAP: Handle = 2;

/// Lowest address a program is linked at (see `lib/sprout-rt/user.ld`).
pub const USER_IMAGE_BASE: usize = 0x1_0000;
/// `vmo_map` places mappings from here upwards.
pub const USER_MMAP_BASE: usize = 0x1000_0000;
/// User stacks grow down from here.
pub const USER_STACK_TOP: usize = 0x4000_0000;
pub const USER_STACK_SIZE: usize = 64 * 1024;
/// User addresses are below this; the kernel's half starts far above.
pub const USER_ADDRESS_LIMIT: usize = 0x40_0000_0000;

/// Most bytes one channel message carries.
pub const CHANNEL_MAX_BYTES: usize = 4096;
/// Most handles one channel message carries.
pub const CHANNEL_MAX_HANDLES: usize = 8;
/// A timeout meaning "wait forever".
pub const TIMEOUT_INFINITE: u64 = u64::MAX;

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
    pub const HANDLE_DUPLICATE: usize = 9;
    pub const CHANNEL_CREATE: usize = 10;
    pub const CHANNEL_SEND: usize = 11;
    pub const CHANNEL_RECV: usize = 12;
    pub const PORT_CREATE: usize = 13;
    pub const PORT_ATTACH: usize = 14;
    pub const PORT_WAIT: usize = 15;
    pub const PROCESS_SPAWN: usize = 16;
    pub const PROCESS_WAIT: usize = 17;
    pub const VMO_MAP: usize = 18;
    pub const VMO_UNMAP: usize = 19;
    pub const DEVICE_MAP: usize = 20;
    pub const DEVICE_IRQ_WAIT: usize = 21;
    pub const DEVICE_IRQ_ACK: usize = 22;
    pub const VMO_PHYSICAL: usize = 23;
    pub const CLOCK_NS: usize = 24;
}

/// Flags for `vmo_create`.
pub mod vmo_flags {
    /// Back the VMO with physically contiguous pages, so a device can be
    /// handed its physical address (`vmo_physical`).
    pub const CONTIGUOUS: usize = 1 << 0;
}

/// Signals an object can raise, as seen through a port.
pub mod signals {
    /// A channel endpoint has a message waiting.
    pub const READABLE: u32 = 1 << 0;
    /// The other end of a channel is gone.
    pub const PEER_CLOSED: u32 = 1 << 1;
}

/// A channel message as `channel_send` reads it and `channel_recv` fills
/// it. For send, `bytes_len` and `handles_len` say how much to take; for
/// recv they are set to how much arrived, and `*_cap` bound the buffers.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Message {
    pub bytes: *mut u8,
    pub bytes_cap: usize,
    pub bytes_len: usize,
    pub handles: *mut Handle,
    pub handles_cap: usize,
    pub handles_len: usize,
}

/// What `port_wait` returns: the key given at `port_attach` and the
/// signals currently asserted on that object.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PortPacket {
    pub key: u64,
    pub signals: u32,
    pub _reserved: u32,
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
    PeerClosed = 10,
    TimedOut = 11,
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
            10 => Some(Error::PeerClosed),
            11 => Some(Error::TimedOut),
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
            Error::PeerClosed => "peer closed",
            Error::TimedOut => "timed out",
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

    pub const fn intersection(self, other: Rights) -> Rights {
        Rights(self.0 & other.0)
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
    Channel,
    Port,
    /// A hardware device: a register window and an interrupt line.
    Device,
}

impl ObjectType {
    pub const ALL: &[ObjectType] = &[
        ObjectType::Process,
        ObjectType::Thread,
        ObjectType::Vmo,
        ObjectType::AddressSpace,
        ObjectType::Channel,
        ObjectType::Port,
        ObjectType::Device,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            ObjectType::Process => "process",
            ObjectType::Thread => "thread",
            ObjectType::Vmo => "vmo",
            ObjectType::AddressSpace => "address_space",
            ObjectType::Channel => "channel",
            ObjectType::Port => "port",
            ObjectType::Device => "device",
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
        args: &[arg("size", "usize"), arg("flags", "vmo_flags")],
        returns: "a new handle with all rights",
        object: None,
        rights: Rights::NONE,
        doc: "Create a zero-filled virtual memory object of at least size bytes. With \
              CONTIGUOUS its pages are physically contiguous, for device buffers.",
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
    Syscall {
        num: sys::HANDLE_DUPLICATE,
        name: "handle_duplicate",
        args: &[arg("handle", "Handle"), arg("rights", "Rights")],
        returns: "a new handle to the same object",
        object: None,
        rights: Rights::DUPLICATE,
        doc: "Make a second handle to the same object with a subset of the rights \
              (the intersection of the mask with the handle's rights).",
    },
    Syscall {
        num: sys::CHANNEL_CREATE,
        name: "channel_create",
        args: &[arg("out", "*mut [Handle; 2]")],
        returns: "0; out holds the two endpoints, each with all rights",
        object: None,
        rights: Rights::NONE,
        doc: "Create a channel: two endpoints, what one sends the other receives.",
    },
    Syscall {
        num: sys::CHANNEL_SEND,
        name: "channel_send",
        args: &[arg("handle", "Handle"), arg("msg", "*const Message")],
        returns: "0",
        object: Some(ObjectType::Channel),
        rights: Rights::WRITE,
        doc: "Queue a message of up to CHANNEL_MAX_BYTES bytes and CHANNEL_MAX_HANDLES \
              handles for the peer. Handles must carry the transfer right; they leave \
              this process and arrive in the receiver's table.",
    },
    Syscall {
        num: sys::CHANNEL_RECV,
        name: "channel_recv",
        args: &[
            arg("handle", "Handle"),
            arg("msg", "*mut Message"),
            arg("timeout_ms", "u64"),
        ],
        returns: "0; msg.bytes_len and msg.handles_len say what arrived",
        object: Some(ObjectType::Channel),
        rights: Rights::READ,
        doc: "Take the oldest queued message, waiting up to timeout_ms (0 polls, \
              TIMEOUT_INFINITE waits forever). Buffer too small leaves it queued; \
              peer closed with nothing queued is an error.",
    },
    Syscall {
        num: sys::PORT_CREATE,
        name: "port_create",
        args: &[],
        returns: "a new port handle with all rights",
        object: None,
        rights: Rights::NONE,
        doc: "Create a port, something to wait on several objects with.",
    },
    Syscall {
        num: sys::PORT_ATTACH,
        name: "port_attach",
        args: &[
            arg("port", "Handle"),
            arg("object", "Handle"),
            arg("key", "u64"),
        ],
        returns: "0",
        object: Some(ObjectType::Port),
        rights: Rights::WRITE,
        doc: "Watch an object through the port; packets for it carry key. The object \
              handle needs the read right.",
    },
    Syscall {
        num: sys::PORT_WAIT,
        name: "port_wait",
        args: &[
            arg("port", "Handle"),
            arg("packet", "*mut PortPacket"),
            arg("timeout_ms", "u64"),
        ],
        returns: "0; packet names an attached object with signals asserted",
        object: Some(ObjectType::Port),
        rights: Rights::READ,
        doc: "Wait until an attached object has a signal asserted (readable, peer \
              closed) or the timeout passes.",
    },
    Syscall {
        num: sys::PROCESS_SPAWN,
        name: "process_spawn",
        args: &[
            arg("image", "Handle"),
            arg("bootstrap", "Handle"),
            arg("name", "*const u8"),
            arg("name_len", "usize"),
        ],
        returns: "a handle to the new process",
        object: Some(ObjectType::Vmo),
        rights: Rights::READ,
        doc: "Start a process from the ELF in a VMO. The bootstrap handle (transfer \
              right; 0 for none) moves into the new process as HANDLE_BOOTSTRAP.",
    },
    Syscall {
        num: sys::PROCESS_WAIT,
        name: "process_wait",
        args: &[arg("process", "Handle")],
        returns: "the exit code",
        object: Some(ObjectType::Process),
        rights: Rights::READ,
        doc: "Block until the process exits.",
    },
    Syscall {
        num: sys::VMO_MAP,
        name: "vmo_map",
        args: &[arg("handle", "Handle")],
        returns: "the address the VMO is mapped at",
        object: Some(ObjectType::Vmo),
        rights: Rights::MAP,
        doc: "Map the whole VMO into the calling process, readable and, with the \
              write right, writable. Two processes mapping one VMO share memory.",
    },
    Syscall {
        num: sys::VMO_UNMAP,
        name: "vmo_unmap",
        args: &[arg("addr", "usize")],
        returns: "0",
        object: None,
        rights: Rights::NONE,
        doc: "Remove a mapping made by vmo_map.",
    },
    Syscall {
        num: sys::DEVICE_MAP,
        name: "device_map",
        args: &[arg("device", "Handle")],
        returns: "the address the device's registers are mapped at",
        object: Some(ObjectType::Device),
        rights: Rights::MAP,
        doc: "Map a device's register window into the calling process, read-write.",
    },
    Syscall {
        num: sys::DEVICE_IRQ_WAIT,
        name: "device_irq_wait",
        args: &[arg("device", "Handle"), arg("timeout_ms", "u64")],
        returns: "interrupts taken since the last wait (at least 1)",
        object: Some(ObjectType::Device),
        rights: Rights::READ,
        doc: "Wait for the device's interrupt. The kernel masks the line when it fires; \
              the driver must call device_irq_ack after handling it.",
    },
    Syscall {
        num: sys::DEVICE_IRQ_ACK,
        name: "device_irq_ack",
        args: &[arg("device", "Handle")],
        returns: "0",
        object: Some(ObjectType::Device),
        rights: Rights::WRITE,
        doc: "Unmask the device's interrupt line after handling an interrupt.",
    },
    Syscall {
        num: sys::VMO_PHYSICAL,
        name: "vmo_physical",
        args: &[arg("handle", "Handle"), arg("offset", "usize")],
        returns: "the physical address of the byte at offset",
        object: Some(ObjectType::Vmo),
        rights: Rights::MAP,
        doc: "The physical address behind a byte of a CONTIGUOUS VMO, to hand to a \
              device. This is the DMA capability: give it only to drivers.",
    },
    Syscall {
        num: sys::CLOCK_NS,
        name: "clock_ns",
        args: &[],
        returns: "nanoseconds since boot",
        object: None,
        rights: Rights::NONE,
        doc: "A monotonic clock, for timeouts and protocol timers.",
    },
];

/// Looks a syscall up by number.
pub fn syscall(num: usize) -> Option<&'static Syscall> {
    SYSCALLS.iter().find(|s| s.num == num)
}
