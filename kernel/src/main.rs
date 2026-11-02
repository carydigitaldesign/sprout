//! Sprout kernel entry point.
//!
//! The architecture boot code (`arch/<arch>/boot.rs`) sets up a stack, clears
//! `.bss` and calls [`kmain`]. Everything above `arch` is portable.

#![no_std]
#![no_main]

extern crate alloc;

mod arch;
// Console input is only consumed by tests until a shell exists (M5).
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
mod console;
mod fdt;
// The logging API is deliberately wider than any one milestone uses.
#[allow(dead_code)]
mod log;
// Memory management's API is exercised by its tests; the kernel itself
// grows into all of it over M3 to M5.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
mod mm;
mod panic;
// Likewise for the scheduler and timer: nothing in the kernel sleeps or
// joins until there is userspace to wait for.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
mod sched;
mod sync;
mod testing;
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
mod time;

/// Kernel version, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// First Rust code to run, on the boot hart.
///
/// `hartid` is the hardware thread we were started on and `dtb` the physical
/// address of the flattened device tree handed over by the firmware.
#[unsafe(no_mangle)]
pub extern "C" fn kmain(hartid: usize, dtb: usize) -> ! {
    arch::init_early();

    log::event("boot")
        .str("kernel", VERSION)
        .str("arch", arch::NAME)
        .num("hart", hartid)
        .hex("dtb", dtb)
        .str("console", arch::console_backend())
        .end();

    mm::init(dtb);
    time::init(mm::boot_info().timebase_hz);
    arch::init_devices();
    sched::init();
    arch::interrupts::enable();
    log::info("boot", "timer and scheduler running")
        .num("tick_hz", time::TICK_HZ)
        .num("threads", sched::live_threads())
        .end();

    #[cfg(feature = "ktest")]
    {
        let code = testing::run_all();
        arch::exit(code)
    }

    #[cfg(not(feature = "ktest"))]
    {
        // Nothing to schedule yet, so `main` leaves and `idle` keeps the
        // hart in `wfi`. This is where `cargo xtask run` and MCP `boot`
        // sessions sit; typed bytes arrive as `console_rx` events.
        log::event("idle")
            .num("threads", sched::live_threads())
            .end();
        sched::exit()
    }
}
