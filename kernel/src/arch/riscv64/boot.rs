//! The first instructions: OpenSBI jumps here in supervisor mode, paging
//! off, at the physical load address.
//!
//! On entry `a0` holds the hart id and `a1` the device tree address. OpenSBI
//! starts only one hart; the others stay parked in the firmware until asked
//! for, so there is no secondary-hart handling here yet.
//!
//! The kernel is linked in the higher half (see `linker.ld`), so the very
//! first job is to turn on paging with a page table built at compile time
//! and jump to the high alias of the next instruction. From then on every
//! address the code computes is the one it was linked at.

use super::paging::PHYS_MAP_BASE;

/// A single Sv39 top-level table, each entry covering 1 GiB.
#[repr(C, align(4096))]
pub struct BootPageTable([u64; 512]);

const PTE_V: u64 = 1 << 0;
const PTE_R: u64 = 1 << 1;
const PTE_W: u64 = 1 << 2;
const PTE_X: u64 = 1 << 3;
const PTE_A: u64 = 1 << 6;
const PTE_D: u64 = 1 << 7;
const RW: u64 = PTE_V | PTE_R | PTE_W | PTE_A | PTE_D;
const RWX: u64 = RW | PTE_X;

const fn leaf_1g(pa: u64, flags: u64) -> u64 {
    ((pa >> 12) << 10) | flags
}

const fn vpn2(va: usize) -> usize {
    (va >> 30) & 0x1ff
}

/// Identity-maps the first 4 GiB (so the jump below works and devices stay
/// reachable for a moment) and direct-maps the same 4 GiB at
/// `PHYS_MAP_BASE`. Replaced by a proper table in `mm::init`.
#[unsafe(no_mangle)]
#[used]
static BOOT_PAGE_TABLE: BootPageTable = {
    let mut table = [0u64; 512];
    let mut gib = 0;
    while gib < 4 {
        let pa = (gib as u64) << 30;
        let flags = if gib == 0 { RW } else { RWX };
        table[vpn2(pa as usize)] = leaf_1g(pa, flags);
        table[vpn2(PHYS_MAP_BASE + pa as usize)] = leaf_1g(pa, flags);
        gib += 1;
    }
    BootPageTable(table)
};

core::arch::global_asm!(
    r#"
    .section .text.boot, "ax"
    .globl _start
    .type _start, @function
_start:
    // Sv39 with the boot page table. `lla` is PC-relative, so the address
    // it produces is physical: we are running where we were loaded.
    lla t0, BOOT_PAGE_TABLE
    srli t0, t0, 12
    li t1, 8
    slli t1, t1, 60
    or t0, t0, t1
    csrw satp, t0
    sfence.vma

    // Continue at the higher-half alias of the next instruction.
    lla t0, 1f
    li t1, {phys_map_base}
    add t0, t0, t1
    jr t0

1:
    // Global pointer for gp-relative data access. Relaxation must be off for
    // this one instruction or the assembler turns it into `mv gp, gp`.
    .option push
    .option norelax
    la gp, __global_pointer$
    .option pop

    la sp, __stack_top

    // Zero .bss so Rust statics begin in their declared state.
    la t0, __bss_start
    la t1, __bss_end
2:  bgeu t0, t1, 3f
    sd zero, 0(t0)
    addi t0, t0, 8
    j 2b
3:
    // A zero frame pointer ends every backtrace walk here.
    mv s0, zero

    // a0 = hart id, a1 = dtb: passed through unchanged to kmain(hartid, dtb).
    call kmain

    // kmain never returns; park if it somehow does.
4:  wfi
    j 4b
    .size _start, . - _start
"#,
    phys_map_base = const PHYS_MAP_BASE,
);
