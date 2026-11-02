//! The interrupt flag.

use super::pic;

const RFLAGS_IF: usize = 1 << 9;

/// Whether this CPU currently takes interrupts.
#[inline]
pub fn enabled() -> bool {
    let rflags: usize;
    // SAFETY: pushing and popping RFLAGS has no other effect.
    unsafe { core::arch::asm!("pushfq", "pop {}", out(reg) rflags, options(nomem)) };
    rflags & RFLAGS_IF != 0
}

/// Turns interrupts on.
#[inline]
pub fn enable() {
    // SAFETY: the IDT and PICs are set up at boot before anyone calls this.
    unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
}

/// Turns interrupts off and reports whether they were on, for [`restore`].
#[inline]
pub fn disable() -> bool {
    let was = enabled();
    // SAFETY: clearing IF defers interrupts; nothing is lost, only delayed.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
    was
}

/// Puts interrupts back the way [`disable`] found them.
#[inline]
pub fn restore(was_enabled: bool) {
    if was_enabled {
        enable();
    }
}

/// Lets the timer raise interrupts (IRQ 0 on the master PIC).
pub fn enable_timer_source() {
    pic::set_masked(0, false);
}

/// Sleeps the CPU until the next interrupt. If interrupts are off, they
/// are enabled for the duration of the halt only, so a caller that
/// checks state between sleeps stays correct.
#[inline]
pub fn wait_for_interrupt() {
    if enabled() {
        // SAFETY: `hlt` waits for an interrupt and resumes.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    } else {
        // SAFETY: `sti; hlt` is atomic with respect to interrupts, which
        // are turned off again before anything else runs.
        unsafe { core::arch::asm!("sti", "hlt", "cli", options(nomem, nostack)) };
    }
}

crate::ktest! {
    fn disable_restore_round_trips() {
        let outer = enabled();
        let was = disable();
        assert_eq!(was, outer);
        assert!(!enabled());
        let inner = disable();
        assert!(!inner);
        restore(inner);
        assert!(!enabled());
        restore(was);
        assert_eq!(enabled(), outer);
    }
}
