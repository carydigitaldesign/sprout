//! Supervisor trap entry: every exception and interrupt lands here.
//!
//! `__trap_entry` saves the interrupted context into a [`TrapFrame`] on the
//! current stack, calls [`trap_handler`], restores the frame and returns
//! with `sret`. Until user mode exists (M4) every trap comes from the kernel
//! itself, so the kernel stack is the right place for the frame.
//!
//! Policy at M1: breakpoints (`ebreak`) are counted and skipped, which is
//! how the tests exercise this path without dying; every other exception is
//! logged as a JSON `trap` event with the faulting context's backtrace and
//! then turned into a panic. Interrupts are logged and ignored; none are
//! enabled before M3.

use core::sync::atomic::{AtomicUsize, Ordering};

use super::{backtrace, csr};
use crate::log;

/// Saved context of an interrupted instruction stream. Layout is shared
/// with the assembly below: `regs[n]` is `x<n>` at offset `8*n`.
#[repr(C)]
pub struct TrapFrame {
    /// General registers `x0..x31`. `regs[0]` is never written; `regs[2]`
    /// is the stack pointer before the frame was pushed.
    pub regs: [usize; 32],
    pub sstatus: usize,
    pub sepc: usize,
    pub scause: usize,
    pub stval: usize,
}

const FRAME_SIZE: usize = core::mem::size_of::<TrapFrame>();
const _: () = assert!(
    FRAME_SIZE == 288,
    "assembly below hard-codes the frame size"
);
const _: () = assert!(FRAME_SIZE % 16 == 0, "the stack must stay 16-byte aligned");

core::arch::global_asm!(
    r#"
    .section .text
    .p2align 2
    .globl __trap_entry
    .type __trap_entry, @function
__trap_entry:
    addi sp, sp, -288

    // Save every general register except x0 (always zero) and x2 (sp),
    // which is recorded after the others below.
    .irp n, 1,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    sd x\n, 8*\n(sp)
    .endr
    addi t0, sp, 288
    sd t0, 16(sp)

    csrr t0, sstatus
    sd t0, 256(sp)
    csrr t0, sepc
    sd t0, 264(sp)
    csrr t0, scause
    sd t0, 272(sp)
    csrr t0, stval
    sd t0, 280(sp)

    mv a0, sp
    call trap_handler

    ld t0, 256(sp)
    csrw sstatus, t0
    ld t0, 264(sp)
    csrw sepc, t0

    // Restore in the same order, t0 (x5) last because it is our scratch.
    .irp n, 1,3,4,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    ld x\n, 8*\n(sp)
    .endr
    ld x5, 40(sp)
    addi sp, sp, 288
    sret
    .size __trap_entry, . - __trap_entry
"#
);

unsafe extern "C" {
    fn __trap_entry();
}

/// Index of the stack pointer in [`TrapFrame::regs`].
const REG_SP: usize = 2;
/// Index of the frame pointer (`s0`) in [`TrapFrame::regs`].
const REG_FP: usize = 8;

const EXC_BREAKPOINT: usize = 3;
const INT_SUPERVISOR_TIMER: usize = 5;
const INT_SUPERVISOR_EXTERNAL: usize = 9;

static BREAKPOINTS: AtomicUsize = AtomicUsize::new(0);

/// Address of `__trap_entry`, as `stvec` wants it.
fn trap_entry_address() -> usize {
    __trap_entry as *const () as usize
}

/// Points `stvec` at our entry. Call once per hart, before anything can trap.
pub fn init() {
    let entry = trap_entry_address();
    assert_eq!(
        entry & 0b11,
        0,
        "stvec direct mode needs a 4-byte aligned entry"
    );
    // SAFETY: `__trap_entry` is a complete save/handle/restore sequence that
    // ends in `sret`, and the low bits are clear so the vector is in direct
    // mode. From here on traps are handled rather than ignored.
    unsafe { csr::stvec::write(entry) };
}

/// How many `ebreak` instructions have been caught and skipped.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
pub fn breakpoints() -> usize {
    BREAKPOINTS.load(Ordering::Relaxed)
}

/// Length in bytes of the instruction at `pc`: 2 for a compressed one, 4 otherwise.
fn instruction_len(pc: usize) -> usize {
    // SAFETY: `pc` is `sepc`, the address of an instruction the hart just
    // fetched and executed, so it is mapped, readable kernel text.
    let low_half = unsafe { core::ptr::read_volatile(pc as *const u16) };
    if low_half & 0b11 == 0b11 { 4 } else { 2 }
}

fn exception_name(code: usize) -> &'static str {
    match code {
        0 => "instruction address misaligned",
        1 => "instruction access fault",
        2 => "illegal instruction",
        3 => "breakpoint",
        4 => "load address misaligned",
        5 => "load access fault",
        6 => "store address misaligned",
        7 => "store access fault",
        8 => "ecall from user mode",
        9 => "ecall from supervisor mode",
        12 => "instruction page fault",
        13 => "load page fault",
        15 => "store page fault",
        _ => "unknown exception",
    }
}

fn interrupt_name(code: usize) -> &'static str {
    match code {
        1 => "supervisor software",
        5 => "supervisor timer",
        9 => "supervisor external",
        _ => "unknown interrupt",
    }
}

/// Called by `__trap_entry` with the saved context. Changes to `frame`
/// take effect when the assembly restores it.
#[unsafe(no_mangle)]
extern "C" fn trap_handler(frame: &mut TrapFrame) {
    let is_interrupt = frame.scause >> (usize::BITS - 1) == 1;
    let code = frame.scause & (usize::MAX >> 1);

    if is_interrupt {
        match code {
            INT_SUPERVISOR_TIMER => {
                crate::time::on_tick();
                if crate::sched::on_tick(crate::time::ticks()) {
                    crate::sched::yield_now();
                }
            }
            INT_SUPERVISOR_EXTERNAL => super::plic::handle_external(),
            _ => log::warn("trap", "unexpected interrupt")
                .str("cause", interrupt_name(code))
                .num("code", code)
                .end(),
        }
        return;
    }

    if code == EXC_BREAKPOINT {
        BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
        frame.sepc += instruction_len(frame.sepc);
        return;
    }

    let cause = exception_name(code);
    log::event("trap")
        .str("kind", "exception")
        .str("cause", cause)
        .num("code", code)
        .hex("sepc", frame.sepc)
        .hex("stval", frame.stval)
        .hex("sp", frame.regs[REG_SP])
        .hex_list(
            "frames",
            core::iter::once(frame.sepc).chain(backtrace::frames_from(frame.regs[REG_FP]).take(32)),
        )
        .end();
    panic!("unhandled trap: {cause} at {:#x}", frame.sepc);
}

crate::ktest! {
    fn stvec_points_at_trap_entry() {
        assert_eq!(csr::stvec::read(), trap_entry_address());
    }
}

crate::ktest! {
    fn ebreak_is_caught_and_execution_resumes() {
        let before = breakpoints();
        // SAFETY: `ebreak` raises a breakpoint exception, which the trap
        // handler above counts and steps over; execution resumes here.
        unsafe { core::arch::asm!("ebreak", options(nomem, nostack)) };
        assert_eq!(breakpoints(), before + 1);
    }
}

crate::ktest! {
    fn trap_entry_preserves_caller_saved_registers() {
        let t0: usize;
        let a7: usize;
        // SAFETY: as above, the breakpoint is handled and skipped. The
        // registers named as outputs are the only state the block changes.
        unsafe {
            core::arch::asm!(
                "li t0, 0x1234",
                "li a7, 0x5678",
                "ebreak",
                "mv {0}, t0",
                "mv {1}, a7",
                out(reg) t0,
                out(reg) a7,
                out("t0") _,
                out("a7") _,
                options(nomem, nostack),
            )
        };
        assert_eq!(t0, 0x1234, "t0 was clobbered across the trap");
        assert_eq!(a7, 0x5678, "a7 was clobbered across the trap");
    }
}
