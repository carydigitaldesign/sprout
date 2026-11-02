//! Runtime for Sprout user programs.
//!
//! A program is a `#![no_std] #![no_main]` crate that defines
//! `#[unsafe(no_mangle)] pub extern "C" fn main() -> i32` and links this
//! crate. `_start` sets up the global pointer, calls `main`, and exits with
//! its result. Everything else here is a thin wrapper over a syscall.

#![no_std]

use core::fmt::Write;

pub use sprout_abi::{self as abi, Error, HANDLE_KERNEL, HANDLE_SELF_PROCESS, Handle, Rights};

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

pub fn vmo_create(size: usize) -> Result<Handle, Error> {
    call(abi::sys::VMO_CREATE, [size, 0, 0, 0, 0, 0]).map(|h| h as Handle)
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
