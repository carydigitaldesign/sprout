//! Supervisor interrupt enable bits.
//!
//! `sstatus.SIE` is the global switch; `sie` selects sources (timer,
//! external, software). Traps clear `SIE` on entry and `sret` restores it,
//! so handlers run with interrupts off.

use super::csr;

const SSTATUS_SIE: usize = 1 << 1;
const SIE_STIE: usize = 1 << 5;
const SIE_SEIE: usize = 1 << 9;

/// Whether this hart currently takes interrupts.
#[inline]
pub fn enabled() -> bool {
    csr::sstatus::read() & SSTATUS_SIE != 0
}

/// Turns interrupts on.
#[inline]
pub fn enable() {
    // SAFETY: setting SIE only lets already-configured sources deliver
    // interrupts to the trap vector installed at boot.
    unsafe { core::arch::asm!("csrsi sstatus, 2", options(nomem, nostack)) };
}

/// Turns interrupts off and reports whether they were on, for [`restore`].
#[inline]
pub fn disable() -> bool {
    let was = enabled();
    // SAFETY: clearing SIE defers interrupts; nothing is lost, only delayed.
    unsafe { core::arch::asm!("csrci sstatus, 2", options(nomem, nostack)) };
    was
}

/// Puts interrupts back the way [`disable`] found them.
#[inline]
pub fn restore(was_enabled: bool) {
    if was_enabled {
        enable();
    }
}

/// Lets the timer raise supervisor timer interrupts.
pub fn enable_timer_source() {
    // SAFETY: the trap handler dispatches timer interrupts to `time::on_tick`.
    unsafe { csr::sie::write(csr::sie::read() | SIE_STIE) };
}

/// Lets the PLIC raise supervisor external interrupts.
pub fn enable_external_source() {
    // SAFETY: the trap handler dispatches external interrupts to the PLIC.
    unsafe { csr::sie::write(csr::sie::read() | SIE_SEIE) };
}

/// Sleeps the hart until the next interrupt. With interrupts disabled this
/// still returns when one becomes pending, which is how an idle loop that
/// checks state between sleeps stays correct.
#[inline]
pub fn wait_for_interrupt() {
    // SAFETY: `wfi` only pauses the hart; it has no other effect.
    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
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
