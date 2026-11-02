//! Platform-Level Interrupt Controller, as found on QEMU `virt` (the SiFive
//! layout). Routes device interrupts to this hart's supervisor context.

use super::interrupts;
use super::paging::phys_to_virt;
use crate::log;
use crate::sync::SpinLock;

const PLIC_PA: usize = 0x0c00_0000;
/// Interrupt sources we keep handlers for. `virt` defines fewer than this.
const MAX_SOURCES: usize = 64;
/// Contexts alternate machine, supervisor per hart: hart 0's S-mode is 1.
const CONTEXT: usize = 1;

const PRIORITY_BASE: usize = 0x0;
const ENABLE_BASE: usize = 0x2000;
const ENABLE_STRIDE: usize = 0x80;
const CONTEXT_BASE: usize = 0x20_0000;
const CONTEXT_STRIDE: usize = 0x1000;
const THRESHOLD_OFFSET: usize = 0x0;
const CLAIM_OFFSET: usize = 0x4;

/// Called with the source number when its interrupt fires.
pub type Handler = fn(u32);

static HANDLERS: SpinLock<[Option<Handler>; MAX_SOURCES]> = SpinLock::new([None; MAX_SOURCES]);

fn read(offset: usize) -> u32 {
    // SAFETY: the PLIC's registers sit in the device GiB mapped read-write
    // in the direct map; reading one has no side effect except for claim,
    // which is the intended one.
    unsafe { core::ptr::read_volatile(phys_to_virt(PLIC_PA + offset) as *const u32) }
}

fn write(offset: usize, value: u32) {
    // SAFETY: as for `read`; writes configure routing or complete a claim.
    unsafe { core::ptr::write_volatile(phys_to_virt(PLIC_PA + offset) as *mut u32, value) };
}

fn context_reg(offset: usize) -> usize {
    CONTEXT_BASE + CONTEXT * CONTEXT_STRIDE + offset
}

/// Lets every enabled source through and turns on external interrupts.
pub fn init() {
    write(context_reg(THRESHOLD_OFFSET), 0);
    interrupts::enable_external_source();
}

/// Masks or unmasks `source` for this context.
pub fn set_source_enabled(source: u32, enabled: bool) {
    let index = source as usize;
    let reg = ENABLE_BASE + CONTEXT * ENABLE_STRIDE + 4 * (index / 32);
    let bit = 1u32 << (index % 32);
    let current = read(reg);
    write(
        reg,
        if enabled {
            current | bit
        } else {
            current & !bit
        },
    );
}

/// Routes `source` to `handler` at `priority` (1 is lowest that passes).
pub fn enable(source: u32, priority: u32, handler: Handler) {
    let index = source as usize;
    assert!(
        index != 0 && index < MAX_SOURCES,
        "PLIC source {source} out of range"
    );
    HANDLERS.lock()[index] = Some(handler);
    write(PRIORITY_BASE + 4 * index, priority);
    set_source_enabled(source, true);
}

/// Priority currently programmed for `source`.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
pub fn priority(source: u32) -> u32 {
    read(PRIORITY_BASE + 4 * source as usize)
}

/// The highest-priority pending source, now owned by us until `complete`.
pub fn claim() -> Option<u32> {
    let source = read(context_reg(CLAIM_OFFSET));
    (source != 0).then_some(source)
}

/// Tells the PLIC we are done with `source`.
pub fn complete(source: u32) {
    write(context_reg(CLAIM_OFFSET), source);
}

/// Supervisor external interrupt: drain every pending source.
pub fn handle_external() {
    while let Some(source) = claim() {
        let handler = HANDLERS.lock().get(source as usize).copied().flatten();
        match handler {
            Some(handler) => handler(source),
            None => log::warn("plic", "interrupt from a source with no handler")
                .num("source", source)
                .end(),
        }
        complete(source);
    }
}

crate::ktest! {
    fn nothing_pending_claims_nothing() {
        assert_eq!(claim(), None);
    }
}

crate::ktest! {
    fn priority_register_reads_back() {
        // Source 63 exists on virt but nothing drives it.
        let before = priority(63);
        write(PRIORITY_BASE + 4 * 63, 3);
        assert_eq!(priority(63), 3);
        write(PRIORITY_BASE + 4 * 63, before);
    }
}
