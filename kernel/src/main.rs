//! Sprout kernel entry point.
//!
//! The architecture boot code (`arch/<arch>/boot.rs`) sets up a stack, clears
//! `.bss` and calls [`kmain`]. Everything above `arch` is portable.

#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod fdt;
// The logging API is deliberately wider than any one milestone uses.
#[allow(dead_code)]
mod log;
// Memory management's API is exercised by its tests; the kernel itself
// grows into all of it over M3 to M5.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
mod mm;
mod panic;
mod sync;
mod testing;

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

    #[cfg(feature = "ktest")]
    {
        let code = testing::run_all();
        arch::exit(code)
    }

    #[cfg(not(feature = "ktest"))]
    {
        log::info("boot", "Sprout is up; nothing to schedule yet").end();
        // Nothing can wake us before M3 (no interrupt sources are enabled),
        // so this is where `cargo xtask run` and MCP `boot` sessions sit.
        log::event("idle").end();
        arch::wait_forever()
    }
}
