//! The 8254 programmable interval timer: a 100 Hz tick on IRQ 0, and a
//! known-rate counter to measure the time-stamp counter against.

use super::ports::{inb, outb};

const CHANNEL0: u16 = 0x40;
const COMMAND: u16 = 0x43;
/// The PIT's input clock.
const INPUT_HZ: u64 = 1_193_182;
/// Reload value for 100 Hz, rounded up so a tick is never shorter than
/// 10 ms: `time::sleep_ms` promises at least the asked-for time in
/// ticks.
const DIVISOR: u16 = INPUT_HZ.div_ceil(crate::time::TICK_HZ) as u16;

/// Starts channel 0 as a 100 Hz rate generator. Interrupts stay masked
/// until the PIC is set up and `time::init` asks for them.
pub fn start() {
    // SAFETY: standard PIT programming on its own ports.
    unsafe {
        outb(COMMAND, 0b0011_0100); // channel 0, lo/hi byte, mode 2
        outb(CHANNEL0, DIVISOR as u8);
        outb(CHANNEL0, (DIVISOR >> 8) as u8);
    }
}

fn read_counter() -> u16 {
    // SAFETY: the latch command then two reads is the documented sequence.
    unsafe {
        outb(COMMAND, 0);
        let low = inb(CHANNEL0) as u16;
        let high = inb(CHANNEL0) as u16;
        (high << 8) | low
    }
}

/// Measures the TSC rate over a few PIT periods. Runs with interrupts off
/// during boot; takes about 50 ms.
pub fn calibrate_tsc() -> u64 {
    const PERIODS: u64 = 5;
    let mut last = read_counter();
    // Align to the start of a period.
    loop {
        let now = read_counter();
        if now > last {
            break;
        }
        last = now;
    }
    let start = super::timer_cycles();
    let mut wraps = 0;
    last = read_counter();
    while wraps < PERIODS {
        let now = read_counter();
        if now > last {
            wraps += 1;
        }
        last = now;
    }
    let elapsed = super::timer_cycles() - start;
    // The measured span is PERIODS reloads of DIVISOR input cycles.
    elapsed * INPUT_HZ / (PERIODS * DIVISOR as u64)
}
