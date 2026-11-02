//! Port I/O, the x86 way to reach the legacy devices.

/// Reads a byte from `port`.
///
/// # Safety
/// Reading some ports has side effects; the caller knows the device.
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: forwarded from the caller's contract.
    unsafe {
        core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack))
    };
    value
}

/// Writes a byte to `port`.
///
/// # Safety
/// The caller knows the device behind the port.
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    // SAFETY: forwarded from the caller's contract.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack))
    };
}

/// Writes a 32-bit word to `port`.
///
/// # Safety
/// The caller knows the device behind the port.
#[inline]
pub unsafe fn outl(port: u16, value: u32) {
    // SAFETY: forwarded from the caller's contract.
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack))
    };
}
