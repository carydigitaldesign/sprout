//! From the firmware's registers to the portable hand-off: OpenSBI gives
//! us a hart id and a device tree; the tree says where RAM is and how fast
//! the timer runs.

use core::sync::atomic::{AtomicUsize, Ordering};

use super::paging::phys_to_virt;
use crate::fdt::Fdt;
use crate::handoff::{Handoff, MAX_REGIONS, Region};
use crate::log;

static HARTID: AtomicUsize = AtomicUsize::new(0);
static DTB_PA: AtomicUsize = AtomicUsize::new(0);

/// Called by `boot.rs` with `a0` and `a1` as OpenSBI left them.
#[unsafe(no_mangle)]
pub extern "C" fn riscv_entry(hartid: usize, dtb: usize) -> ! {
    HARTID.store(hartid, Ordering::Relaxed);
    DTB_PA.store(dtb, Ordering::Relaxed);
    crate::kmain()
}

/// Reads the device tree. Runs after the console is up and before memory
/// management, through the boot page table's direct map.
pub fn handoff() -> Handoff {
    let dtb = DTB_PA.load(Ordering::Relaxed);
    log::event("platform")
        .num("hart", HARTID.load(Ordering::Relaxed))
        .hex("dtb", dtb)
        .end();
    // SAFETY: the firmware placed a complete device tree at `dtb`, and it
    // is reserved below so nothing overwrites it.
    let fdt = unsafe { Fdt::from_ptr(phys_to_virt(dtb) as *const u8) }
        .unwrap_or_else(|e| panic!("device tree at {dtb:#x}: {e}"));

    let mut handoff = Handoff::empty();
    handoff.ram = fdt.memory_regions::<MAX_REGIONS>();
    handoff.reserved[0] = Some(Region {
        base: dtb,
        size: fdt.total_size(),
    });
    handoff.timebase_hz = fdt.timebase_frequency().unwrap_or_else(|| {
        log::warn(
            "platform",
            "device tree has no timebase-frequency; assuming 10 MHz",
        )
        .end();
        10_000_000
    });
    handoff.device_tree = Some(dtb);
    handoff
}
