//! Stopping the machine with a status code the host can read.

use crate::log;

/// QEMU `virt` "SiFive test finisher": a write here ends the emulator with an
/// exit status. Not a real device; harmless to poke elsewhere.
const SIFIVE_TEST: *mut u32 = 0x0010_0000 as *mut u32;
const FINISHER_PASS: u32 = 0x5555;
const FINISHER_FAIL: u32 = 0x3333;

/// Logs an `exit` event, then stops the machine. Under QEMU the process exits
/// with `code`. Elsewhere the SBI shutdown runs and the code is only in the log.
pub fn exit(code: u32) -> ! {
    log::event("exit").num("code", code).end();

    let word = if code == 0 {
        FINISHER_PASS
    } else {
        (code << 16) | FINISHER_FAIL
    };
    // SAFETY: with paging off, 0x100000 is the test finisher's register on the
    // QEMU virt machine. The write touches no kernel memory; it asks the
    // emulator to stop. If the machine is not QEMU the write lands on nothing
    // and we fall through to the firmware shutdown below.
    unsafe { core::ptr::write_volatile(SIFIVE_TEST, word) };

    super::sbi::shutdown();
    super::wait_forever()
}
