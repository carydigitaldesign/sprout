//! Time: a fixed-rate tick from the platform timer, and nanoseconds from
//! the cycle counter for anything finer.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::{arch, sched};

/// Timer interrupts per second. Also the scheduler's preemption rate.
pub const TICK_HZ: u64 = 100;

static FREQUENCY_HZ: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Starts the tick. `frequency_hz` is the counter's rate from the device tree.
pub fn init(frequency_hz: u64) {
    FREQUENCY_HZ.store(frequency_hz, Ordering::Relaxed);
    arm_next_tick();
    arch::interrupts::enable_timer_source();
}

fn arm_next_tick() {
    let cycles_per_tick = FREQUENCY_HZ.load(Ordering::Relaxed) / TICK_HZ;
    arch::set_timer(arch::timer_cycles() + cycles_per_tick);
}

/// The timer interrupt. Counts the tick and programs the next one.
pub fn on_tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    arm_next_tick();
}

/// Ticks since boot.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Nanoseconds since the counter started, which on QEMU is boot.
pub fn now_ns() -> u64 {
    let cycles = arch::timer_cycles() as u128;
    let hz = FREQUENCY_HZ.load(Ordering::Relaxed).max(1) as u128;
    (cycles * 1_000_000_000 / hz) as u64
}

/// Ticks that cover at least `ms` milliseconds.
pub fn ms_to_ticks(ms: u64) -> u64 {
    (ms * TICK_HZ).div_ceil(1000)
}

/// Blocks the current thread for at least `ms` milliseconds, as measured
/// by [`now_ns`].
pub fn sleep_ms(ms: u64) {
    let start = now_ns();
    let wanted = ms * 1_000_000;
    // One extra tick: the current tick is already partly spent. The tick
    // and the fine clock can be different oscillators (on x86 the PIT and
    // the TSC), so the promise is checked on the fine clock and topped up
    // a tick at a time.
    let mut until = ticks() + ms_to_ticks(ms) + 1;
    loop {
        sched::sleep_until(until);
        if now_ns().wrapping_sub(start) >= wanted {
            return;
        }
        until = ticks() + 1;
    }
}

crate::ktest! {
    fn ticks_advance_while_waiting() {
        arch::interrupts::enable();
        let start_ticks = ticks();
        let start = now_ns();
        while now_ns() - start < 35_000_000 {
            core::hint::spin_loop();
        }
        assert!(ticks() > start_ticks, "no timer interrupt in 35 ms");
    }
}

crate::ktest! {
    fn sleep_ms_waits_at_least_that_long() {
        let start = now_ns();
        sleep_ms(20);
        let elapsed = now_ns() - start;
        assert!(elapsed >= 20_000_000, "slept only {elapsed} ns");
        assert!(elapsed < 200_000_000, "slept {elapsed} ns, far too long");
    }
}
