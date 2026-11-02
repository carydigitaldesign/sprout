//! Sprout kernel entry point.
//!
//! The architecture boot code (`arch/<arch>/boot.rs`) sets up a stack, clears
//! `.bss`, stashes what the firmware told it and calls [`kmain`]. Everything
//! above `arch` is portable.

#![no_std]
#![no_main]

extern crate alloc;

mod arch;
// Console input is only consumed by tests until a shell reads it.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
mod console;
mod devices;
mod elf;
// The device tree reader is only driven on platforms that have one.
#[cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
mod fdt;
mod handoff;
// The JSON writer is a complete little API; not every method has a caller yet.
#[allow(dead_code)]
mod json;
// The logging API is deliberately wider than any one milestone uses.
#[allow(dead_code)]
mod log;
// Memory management's API is exercised by its tests; the kernel itself
// grows into all of it over time.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
mod mm;
mod obj;
mod panic;
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
mod sched;
mod sync;
mod syscall;
mod testing;
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
mod time;
mod user_image;

/// Kernel version, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// First portable code to run, on the boot CPU, called by the architecture
/// once it has a stack and has recorded the firmware's hand-off.
pub fn kmain() -> ! {
    arch::init_early();

    log::event("boot")
        .str("kernel", VERSION)
        .str("arch", arch::NAME)
        .str("console", arch::console_backend())
        .end();

    let handoff = arch::handoff();
    mm::init(&handoff);
    time::init(handoff.timebase_hz);
    arch::init_devices();
    sched::init();
    devices::init();
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
        let init = obj::process::spawn_init().unwrap_or_else(|e| panic!("cannot start init: {e}"));
        drop(init);

        // `init` and whatever it starts (mcpd keeps serving) run on their
        // own; `main` leaves. `idle` keeps the CPU halted whenever nothing
        // is ready. This is where `cargo xtask run` and MCP `boot`
        // sessions sit; typed bytes arrive as `console_rx` events.
        log::event("idle")
            .num("threads", sched::live_threads())
            .end();
        sched::exit()
    }
}
