//! Supervisor Binary Interface client: the few calls the kernel makes into
//! OpenSBI. Spec: <https://github.com/riscv-non-isa/riscv-sbi-doc>.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};

/// Base extension: probe which other extensions the firmware implements.
const EID_BASE: usize = 0x10;
const FID_BASE_PROBE_EXTENSION: usize = 3;

/// Legacy console putchar. Deprecated but universally present; our fallback.
const EID_LEGACY_CONSOLE_PUTCHAR: usize = 0x01;

/// Debug Console extension ("DBCN"), SBI v2.0.
const EID_DEBUG_CONSOLE: usize = 0x4442_434E;
const FID_DEBUG_CONSOLE_WRITE_BYTE: usize = 2;

/// System Reset extension ("SRST").
const EID_SYSTEM_RESET: usize = 0x5352_5354;
const FID_SYSTEM_RESET: usize = 0;
const RESET_TYPE_SHUTDOWN: usize = 0;
const RESET_REASON_NONE: usize = 0;

static HAS_DEBUG_CONSOLE: AtomicBool = AtomicBool::new(false);

/// What every SBI call returns: an error code in `a0`, a value in `a1`.
struct SbiRet {
    error: isize,
    value: usize,
}

fn call(eid: usize, fid: usize, arg0: usize, arg1: usize, arg2: usize) -> SbiRet {
    let error: usize;
    let value: usize;
    // SAFETY: `ecall` traps into the SBI firmware, which follows the SBI
    // calling convention: a0..a2 carry arguments, a6 the function id, a7 the
    // extension id, and the firmware returns error/value in a0/a1 leaving
    // every other register intact. No kernel memory is read or written.
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") arg0 => error,
            inlateout("a1") arg1 => value,
            in("a2") arg2,
            in("a6") fid,
            in("a7") eid,
            options(nostack),
        );
    }
    SbiRet {
        error: error as isize,
        value,
    }
}

/// Discovers optional extensions. Must run before the first console write.
pub fn init() {
    let probe = call(EID_BASE, FID_BASE_PROBE_EXTENSION, EID_DEBUG_CONSOLE, 0, 0);
    HAS_DEBUG_CONSOLE.store(probe.error == 0 && probe.value != 0, Ordering::Relaxed);
}

/// Whether the SBI v2.0 debug console is available.
pub fn has_debug_console() -> bool {
    HAS_DEBUG_CONSOLE.load(Ordering::Relaxed)
}

/// Writes one byte to the firmware console.
pub fn console_putc(byte: u8) {
    if has_debug_console() {
        call(
            EID_DEBUG_CONSOLE,
            FID_DEBUG_CONSOLE_WRITE_BYTE,
            byte as usize,
            0,
            0,
        );
    } else {
        call(EID_LEGACY_CONSOLE_PUTCHAR, 0, byte as usize, 0, 0);
    }
}

/// Asks the firmware to power the machine off. Returns only if it cannot.
pub fn shutdown() {
    call(
        EID_SYSTEM_RESET,
        FID_SYSTEM_RESET,
        RESET_TYPE_SHUTDOWN,
        RESET_REASON_NONE,
        0,
    );
}

crate::ktest! {
    fn base_extension_answers_probe() {
        // Probing the base extension itself must succeed on any SBI.
        let ret = call(EID_BASE, FID_BASE_PROBE_EXTENSION, EID_BASE, 0, 0);
        assert_eq!(ret.error, 0);
        assert_ne!(ret.value, 0);
    }
}
