//! riscv64 on the QEMU `virt` machine, booted by OpenSBI.

mod backtrace;
mod boot;
// Every CSR gets read and write; not every milestone uses both.
#[allow(dead_code)]
mod csr;
mod exit;
mod sbi;
mod trap;

pub use backtrace::{frame_pointer, frames_from as stack_frames};
pub use exit::exit;

/// Architecture name for logs.
pub const NAME: &str = "riscv64";

/// Runs before the first log line: find the SBI console, install the trap
/// vector so a fault from here on is reported rather than silent.
pub fn init_early() {
    sbi::init();
    trap::init();
}

/// Writes one byte to the firmware console.
pub fn console_putc(byte: u8) {
    sbi::console_putc(byte);
}

/// Which console mechanism `console_putc` uses.
pub fn console_backend() -> &'static str {
    if sbi::has_debug_console() {
        "sbi-dbcn"
    } else {
        "sbi-legacy"
    }
}

/// Parks this hart until an interrupt arrives, forever.
pub fn wait_forever() -> ! {
    loop {
        // SAFETY: `wfi` only pauses the hart until an interrupt; it reads and
        // writes no memory and changes no architectural state we rely on.
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}

crate::ktest! {
    fn boot_stack_is_16_byte_aligned() {
        let sp: usize;
        // SAFETY: reading the stack pointer register has no side effects.
        unsafe { core::arch::asm!("mv {}, sp", out(reg) sp, options(nomem, nostack)) };
        assert_eq!(sp % 16, 0, "sp = {sp:#x}");
    }
}
