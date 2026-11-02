//! Stopping the machine with a status code the host can read.

use crate::log;

/// QEMU `virt` "SiFive test finisher": a write here ends the emulator with an
/// exit status. Not a real device; harmless to poke elsewhere.
const SIFIVE_TEST_PA: usize = 0x0010_0000;
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
    // SAFETY: the device region is mapped read-write in the direct map by
    // both the boot page table and the kernel table, so this is the test
    // finisher's register on the QEMU virt machine. The write touches no
    // kernel memory; it asks the emulator to stop. If the machine is not
    // QEMU the write lands on nothing and we fall through to the firmware
    // shutdown below.
    unsafe { core::ptr::write_volatile(super::phys_to_virt(SIFIVE_TEST_PA) as *mut u32, word) };

    super::sbi::shutdown();
    super::wait_forever()
}
