//! Runtime for Sprout user programs.
//!
//! A program is a `#![no_std] #![no_main]` crate that defines
//! `#[unsafe(no_mangle)] pub extern "C" fn main() -> i32` and links this
//! crate. `_start` sets up the global pointer, calls `main`, and exits with
//! its result. Everything else here is a thin wrapper over a syscall.

#![no_std]

pub use sprout_abi::cpio;

use core::fmt::Write;

pub use sprout_abi::{
    self as abi, CHANNEL_MAX_BYTES, CHANNEL_MAX_HANDLES, Error, HANDLE_BOOTSTRAP, HANDLE_KERNEL,
    HANDLE_SELF_PROCESS, Handle, Message, PortPacket, Rights, TIMEOUT_INFINITE, signals, vmo_flags,
};

core::arch::global_asm!(
    r#"
    .section .text.start, "ax"
    .globl _start
    .type _start, @function
_start:
    .option push
    .option norelax
    la gp, __global_pointer$
    .option pop
    mv s0, zero
    call main
    // main's return value is the exit code.
    li a7, {sys_exit}
    ecall
1:  j 1b
    .size _start, . - _start
"#,
    sys_exit = const abi::sys::EXIT,
);

/// Raw syscall. Prefer the typed wrappers below.
#[inline]
pub fn syscall(num: usize, args: [usize; 6]) -> isize {
    let result: isize;
    // SAFETY: `ecall` hands control to the kernel, which follows the ABI in
    // `sprout-abi`: a0..a5 arguments, a7 number, a0 result. The kernel
    // treats every argument as untrusted, so no invariant rests on us here.
    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") args[0] => result,
            in("a1") args[1],
            in("a2") args[2],
            in("a3") args[3],
            in("a4") args[4],
            in("a5") args[5],
            in("a7") num,
            options(nostack),
        );
    }
    result
}

fn call(num: usize, args: [usize; 6]) -> Result<usize, Error> {
    abi::decode(syscall(num, args))
}

/// Writes a message to the kernel log.
pub fn log(msg: &str) {
    let _ = call(
        abi::sys::LOG,
        [msg.as_ptr() as usize, msg.len(), 0, 0, 0, 0],
    );
}

/// Ends the process.
pub fn exit(code: i32) -> ! {
    let _ = syscall(abi::sys::EXIT, [code as usize, 0, 0, 0, 0, 0]);
    loop {
        core::hint::spin_loop();
    }
}

pub fn yield_now() {
    let _ = call(abi::sys::YIELD, [0; 6]);
}

pub fn sleep_ms(ms: u64) {
    let _ = call(abi::sys::SLEEP, [ms as usize, 0, 0, 0, 0, 0]);
}

/// Nanoseconds since boot.
pub fn clock_ns() -> u64 {
    syscall(abi::sys::CLOCK_NS, [0; 6]) as u64
}

/// Fills `buf` with the JSON description of `handle` (or the kernel, for
/// [`HANDLE_KERNEL`]). Returns the number of bytes the full description
/// needs; if that is larger than `buf`, the text was cut short.
pub fn describe(handle: Handle, buf: &mut [u8]) -> Result<usize, Error> {
    call(
        abi::sys::DESCRIBE,
        [
            handle as usize,
            buf.as_mut_ptr() as usize,
            buf.len(),
            0,
            0,
            0,
        ],
    )
}

pub fn handle_close(handle: Handle) -> Result<(), Error> {
    call(abi::sys::HANDLE_CLOSE, [handle as usize, 0, 0, 0, 0, 0]).map(|_| ())
}

/// A second handle to the same object, with at most `rights`.
pub fn handle_duplicate(handle: Handle, rights: Rights) -> Result<Handle, Error> {
    call(
        abi::sys::HANDLE_DUPLICATE,
        [handle as usize, rights.0 as usize, 0, 0, 0, 0],
    )
    .map(|h| h as Handle)
}

pub fn vmo_create(size: usize) -> Result<Handle, Error> {
    call(abi::sys::VMO_CREATE, [size, 0, 0, 0, 0, 0]).map(|h| h as Handle)
}

/// A VMO whose pages are physically contiguous, for device buffers.
pub fn vmo_create_contiguous(size: usize) -> Result<Handle, Error> {
    call(
        abi::sys::VMO_CREATE,
        [size, vmo_flags::CONTIGUOUS, 0, 0, 0, 0],
    )
    .map(|h| h as Handle)
}

/// Physical address of a byte in a contiguous VMO, to give to a device.
pub fn vmo_physical(handle: Handle, offset: usize) -> Result<usize, Error> {
    call(
        abi::sys::VMO_PHYSICAL,
        [handle as usize, offset, 0, 0, 0, 0],
    )
}

/// Maps a device's registers and returns their address.
pub fn device_map(device: Handle) -> Result<usize, Error> {
    call(abi::sys::DEVICE_MAP, [device as usize, 0, 0, 0, 0, 0])
}

/// Waits for the device's interrupt; returns how many fired.
pub fn device_irq_wait(device: Handle, timeout_ms: u64) -> Result<usize, Error> {
    call(
        abi::sys::DEVICE_IRQ_WAIT,
        [device as usize, timeout_ms as usize, 0, 0, 0, 0],
    )
}

/// Unmasks the device's interrupt line after handling one.
pub fn device_irq_ack(device: Handle) -> Result<(), Error> {
    call(abi::sys::DEVICE_IRQ_ACK, [device as usize, 0, 0, 0, 0, 0]).map(|_| ())
}

pub fn vmo_read(handle: Handle, offset: usize, buf: &mut [u8]) -> Result<(), Error> {
    call(
        abi::sys::VMO_READ,
        [
            handle as usize,
            offset,
            buf.as_mut_ptr() as usize,
            buf.len(),
            0,
            0,
        ],
    )
    .map(|_| ())
}

pub fn vmo_write(handle: Handle, offset: usize, buf: &[u8]) -> Result<(), Error> {
    call(
        abi::sys::VMO_WRITE,
        [
            handle as usize,
            offset,
            buf.as_ptr() as usize,
            buf.len(),
            0,
            0,
        ],
    )
    .map(|_| ())
}

/// Size of a VMO in bytes, read from its description.
pub fn vmo_size(handle: Handle) -> Result<usize, Error> {
    let mut buf = [0u8; 256];
    let n = describe(handle, &mut buf)?.min(buf.len());
    let text = core::str::from_utf8(&buf[..n]).map_err(|_| Error::WrongType)?;
    let at = text.find("\"size\":").ok_or(Error::WrongType)? + 7;
    let digits: usize = text[at..].bytes().take_while(u8::is_ascii_digit).count();
    text[at..at + digits].parse().map_err(|_| Error::WrongType)
}

/// Maps the whole VMO and returns its address.
pub fn vmo_map(handle: Handle) -> Result<usize, Error> {
    call(abi::sys::VMO_MAP, [handle as usize, 0, 0, 0, 0, 0])
}

pub fn vmo_unmap(addr: usize) -> Result<(), Error> {
    call(abi::sys::VMO_UNMAP, [addr, 0, 0, 0, 0, 0]).map(|_| ())
}

/// A channel: two endpoints, each with all rights.
pub fn channel_create() -> Result<(Handle, Handle), Error> {
    let mut out = [0 as Handle; 2];
    call(
        abi::sys::CHANNEL_CREATE,
        [out.as_mut_ptr() as usize, 0, 0, 0, 0, 0],
    )?;
    Ok((out[0], out[1]))
}

/// Sends bytes and handles. The handles leave this process.
pub fn channel_send(handle: Handle, bytes: &[u8], handles: &[Handle]) -> Result<(), Error> {
    let msg = Message {
        bytes: bytes.as_ptr() as *mut u8,
        bytes_cap: bytes.len(),
        bytes_len: bytes.len(),
        handles: handles.as_ptr() as *mut Handle,
        handles_cap: handles.len(),
        handles_len: handles.len(),
    };
    call(
        abi::sys::CHANNEL_SEND,
        [handle as usize, &msg as *const Message as usize, 0, 0, 0, 0],
    )
    .map(|_| ())
}

/// Receives the oldest message into `bytes` and `handles`; returns how
/// much of each arrived.
pub fn channel_recv(
    handle: Handle,
    bytes: &mut [u8],
    handles: &mut [Handle],
    timeout_ms: u64,
) -> Result<(usize, usize), Error> {
    let mut msg = Message {
        bytes: bytes.as_mut_ptr(),
        bytes_cap: bytes.len(),
        bytes_len: 0,
        handles: handles.as_mut_ptr(),
        handles_cap: handles.len(),
        handles_len: 0,
    };
    call(
        abi::sys::CHANNEL_RECV,
        [
            handle as usize,
            &mut msg as *mut Message as usize,
            timeout_ms as usize,
            0,
            0,
            0,
        ],
    )?;
    Ok((msg.bytes_len, msg.handles_len))
}

pub fn port_create() -> Result<Handle, Error> {
    call(abi::sys::PORT_CREATE, [0; 6]).map(|h| h as Handle)
}

pub fn port_attach(port: Handle, object: Handle, key: u64) -> Result<(), Error> {
    call(
        abi::sys::PORT_ATTACH,
        [port as usize, object as usize, key as usize, 0, 0, 0],
    )
    .map(|_| ())
}

pub fn port_wait(port: Handle, timeout_ms: u64) -> Result<PortPacket, Error> {
    let mut packet = PortPacket::default();
    call(
        abi::sys::PORT_WAIT,
        [
            port as usize,
            &mut packet as *mut PortPacket as usize,
            timeout_ms as usize,
            0,
            0,
            0,
        ],
    )?;
    Ok(packet)
}

/// Starts a process from the ELF in `image`; `bootstrap` (or 0) moves into
/// it as [`HANDLE_BOOTSTRAP`].
pub fn process_spawn(image: Handle, bootstrap: Handle, name: &str) -> Result<Handle, Error> {
    call(
        abi::sys::PROCESS_SPAWN,
        [
            image as usize,
            bootstrap as usize,
            name.as_ptr() as usize,
            name.len(),
            0,
            0,
        ],
    )
    .map(|h| h as Handle)
}

/// Waits for a process to exit and returns its code.
pub fn process_wait(process: Handle) -> Result<i32, Error> {
    call(abi::sys::PROCESS_WAIT, [process as usize, 0, 0, 0, 0, 0]).map(|c| c as i32)
}

/// A fixed buffer that implements `fmt::Write`, for formatting without a heap.
pub struct StackString<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackString<N> {
    pub const fn new() -> Self {
        StackString {
            buf: [0; N],
            len: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> Default for StackString<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Write for StackString<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let room = N - self.len;
        let take = s.len().min(room);
        // Only cut on a char boundary so `as_str` stays valid UTF-8.
        let take = (0..=take)
            .rev()
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(0);
        self.buf[self.len..self.len + take].copy_from_slice(s[..take].as_bytes());
        self.len += take;
        if take < s.len() {
            Err(core::fmt::Error)
        } else {
            Ok(())
        }
    }
}

/// Logs a formatted message (up to 512 bytes).
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        let mut s = $crate::StackString::<512>::new();
        let _ = ::core::fmt::Write::write_fmt(&mut s, format_args!($($arg)*));
        $crate::log(s.as_str());
    }};
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let mut s = StackString::<512>::new();
    let _ = write!(s, "panic: {}", info.message());
    if let Some(loc) = info.location() {
        let _ = write!(s, " at {}:{}", loc.file(), loc.line());
    }
    log(s.as_str());
    exit(101)
}
