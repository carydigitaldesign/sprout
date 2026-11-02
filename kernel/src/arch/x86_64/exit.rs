//! Stopping the machine with a status code the host can read.

use super::ports::outl;
use crate::log;

/// QEMU's `isa-debug-exit` device, attached by xtask at this port. A write
/// of `v` makes QEMU exit with status `(v << 1) | 1`, so the status is
/// never the code itself; `xtask test` judges by the JSON `exit` event.
const DEBUG_EXIT_PORT: u16 = 0xf4;

/// Logs an `exit` event, then stops the machine. Under QEMU the process
/// exits; elsewhere the CPU halts and the code is only in the log.
pub fn exit(code: u32) -> ! {
    log::event("exit").num("code", code).end();
    // SAFETY: the debug-exit device only ends the emulator; on a machine
    // without it the write lands on nothing.
    unsafe { outl(DEBUG_EXIT_PORT, code) };
    super::interrupts::disable();
    loop {
        // SAFETY: with interrupts off `hlt` is a permanent stop.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}
