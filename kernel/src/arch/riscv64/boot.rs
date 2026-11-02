//! The first instructions: OpenSBI jumps here in supervisor mode.
//!
//! On entry `a0` holds the hart id and `a1` the device tree address. OpenSBI
//! starts only one hart; the others stay parked in the firmware until asked
//! for, so there is no secondary-hart handling here yet.

core::arch::global_asm!(
    r#"
    .section .text.boot, "ax"
    .globl _start
    .type _start, @function
_start:
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
1:  bgeu t0, t1, 2f
    sd zero, 0(t0)
    addi t0, t0, 8
    j 1b
2:
    // A zero frame pointer ends every backtrace walk here.
    mv s0, zero

    // a0 = hart id, a1 = dtb: passed through unchanged to kmain(hartid, dtb).
    call kmain

    // kmain never returns; park if it somehow does.
3:  wfi
    j 3b
    .size _start, . - _start
"#
);
