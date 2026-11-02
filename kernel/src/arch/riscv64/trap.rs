//! Supervisor trap entry: every exception and interrupt lands here.
//!
//! `__trap_entry` saves the interrupted context into a [`TrapFrame`], calls
//! [`trap_handler`], restores the frame and returns with `sret`.
//!
//! Traps from the kernel use the current stack. Traps from user mode must
//! not: `sscratch` holds the thread's kernel stack top while user code
//! runs (set by `enter_user` and again on every return to user) and zero
//! while the kernel runs, so one `csrrw` tells the two apart.
//!
//! Policy: `ecall` from user mode is a syscall. Breakpoints (`ebreak`) are
//! counted and skipped, which is how the tests exercise this path. Any
//! other exception from user mode kills the process; from the kernel it
//! is logged with the faulting context's backtrace and turned into a
//! panic. Timer and external interrupts go to their handlers.

use core::sync::atomic::{AtomicUsize, Ordering};

use super::{backtrace, csr};
use crate::log;

/// Saved context of an interrupted instruction stream. Layout is shared
/// with the assembly below: `regs[n]` is `x<n>` at offset `8*n`.
#[repr(C)]
pub struct TrapFrame {
    /// General registers `x0..x31`. `regs[0]` is never written; `regs[2]`
    /// is the stack pointer at the time of the trap (user or kernel).
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
    // From user mode sscratch holds our kernel stack and sp the user's:
    // swap. From the kernel sscratch is zero: swap back.
    csrrw sp, sscratch, sp
    bnez sp, 1f
    csrrw sp, sscratch, sp
1:
    addi sp, sp, -288

    // Save every general register except x0 (always zero) and x2 (sp),
    // which is recorded after the others below.
    .irp n, 1,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    sd x\n, 8*\n(sp)
    .endr

    // The interrupted stack pointer: from user it is in sscratch, from
    // the kernel it is sp + 288. SPP says which. Once saved, sscratch
    // goes to zero so a nested trap knows it came from the kernel.
    csrr t0, sstatus
    andi t0, t0, 0x100
    bnez t0, 2f
    csrr t1, sscratch
    csrw sscratch, zero
    j 3f
2:  addi t1, sp, 288
3:  sd t1, 16(sp)

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
    ld t1, 264(sp)
    csrw sepc, t1

    // Returning to user mode? The next trap must find this kernel stack,
    // which is where sp will be once the frame is gone.
    andi t0, t0, 0x100
    bnez t0, 4f
    addi t0, sp, 288
    csrw sscratch, t0
4:
    // Restore in the same order; t0 (x5) and sp (x2) last.
    .irp n, 1,3,4,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    ld x\n, 8*\n(sp)
    .endr
    ld x5, 40(sp)
    ld sp, 16(sp)
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
/// `a0`: first argument and the return value.
const REG_A0: usize = 10;
/// `a7`: the syscall number.
const REG_A7: usize = 17;

const SSTATUS_SPP: usize = 1 << 8;

const EXC_BREAKPOINT: usize = 3;
const EXC_USER_ECALL: usize = 8;
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
    // mode. `sscratch` is zero while the kernel runs. From here on traps
    // are handled rather than ignored.
    unsafe {
        csr::sscratch::write(0);
        csr::stvec::write(entry);
    }
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
    let from_user = frame.sstatus & SSTATUS_SPP == 0;

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

    if from_user && code == EXC_USER_ECALL {
        // Resume after the ecall, then run the syscall with interrupts on:
        // it may sleep or block like any kernel code.
        frame.sepc += 4;
        let args = [
            frame.regs[REG_A0],
            frame.regs[REG_A0 + 1],
            frame.regs[REG_A0 + 2],
            frame.regs[REG_A0 + 3],
            frame.regs[REG_A0 + 4],
            frame.regs[REG_A0 + 5],
        ];
        super::interrupts::enable();
        let result = crate::syscall::dispatch(frame.regs[REG_A7], args);
        super::interrupts::disable();
        frame.regs[REG_A0] = result as usize;
        return;
    }

    if from_user {
        super::interrupts::enable();
        crate::syscall::user_fault(exception_name(code), code, frame.sepc, frame.stval);
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

crate::ktest! {
    fn sscratch_is_zero_while_the_kernel_runs() {
        assert_eq!(csr::sscratch::read(), 0);
    }
}
