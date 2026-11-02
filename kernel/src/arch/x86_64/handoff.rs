//! From the Multiboot information structure to the portable hand-off:
//! the memory map says where RAM is, and the PIT measures how fast the
//! time-stamp counter runs.

use core::sync::atomic::{AtomicUsize, Ordering};

use super::paging::phys_to_virt;
use super::pit;
use crate::handoff::{Handoff, MAX_REGIONS, Region};
use crate::log;

const MULTIBOOT_MAGIC: usize = 0x2BAD_B002;
const FLAG_MEMORY_MAP: u32 = 1 << 6;
const MEMORY_AVAILABLE: u32 = 1;
/// RAM below this is BIOS territory; the kernel is loaded at 1 MiB.
const LOW_MEMORY_END: usize = 0x10_0000;

static MAGIC: AtomicUsize = AtomicUsize::new(0);
static INFO_PA: AtomicUsize = AtomicUsize::new(0);

/// Called by `boot.rs` with `eax` and `ebx` as the loader left them.
#[unsafe(no_mangle)]
pub extern "C" fn x86_entry(magic: usize, info: usize) -> ! {
    MAGIC.store(magic, Ordering::Relaxed);
    INFO_PA.store(info, Ordering::Relaxed);
    crate::kmain()
}

fn u32_at(pa: usize) -> u32 {
    // SAFETY: the boot page table direct-maps the first GiB, where the
    // loader placed its information, and reading it has no side effect.
    unsafe { core::ptr::read_unaligned(phys_to_virt(pa) as *const u32) }
}

fn u64_at(pa: usize) -> u64 {
    // SAFETY: as for `u32_at`.
    unsafe { core::ptr::read_unaligned(phys_to_virt(pa) as *const u64) }
}

/// Reads the memory map and measures the clock. Runs after the console
/// is up and before memory management.
pub fn handoff() -> Handoff {
    let magic = MAGIC.load(Ordering::Relaxed);
    let info = INFO_PA.load(Ordering::Relaxed);
    if magic != MULTIBOOT_MAGIC {
        panic!("not booted by a Multiboot loader (magic {magic:#x})");
    }

    pit::start();
    let timebase_hz = pit::calibrate_tsc();
    log::event("platform")
        .str("boot", "multiboot")
        .hex("info", info)
        .num("tsc_hz", timebase_hz)
        .end();

    let mut handoff = Handoff::empty();
    handoff.timebase_hz = timebase_hz;
    let flags = u32_at(info);
    if flags & FLAG_MEMORY_MAP != 0 {
        let length = u32_at(info + 44) as usize;
        let mut at = u32_at(info + 48) as usize;
        let end = at + length;
        let mut found = 0;
        while at + 24 <= end && found < MAX_REGIONS {
            let entry_size = u32_at(at) as usize;
            let base = u64_at(at + 4) as usize;
            let size = u64_at(at + 12) as usize;
            let kind = u32_at(at + 20);
            if kind == MEMORY_AVAILABLE && base >= LOW_MEMORY_END {
                handoff.ram[found] = Some(Region { base, size });
                found += 1;
            }
            at += entry_size + 4;
        }
    }
    handoff
}
