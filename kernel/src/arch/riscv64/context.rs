//! Switching between kernel threads.
//!
//! Only callee-saved state needs saving: the RISC-V calling convention lets
//! `switch` clobber everything else, because to the caller it is just a
//! function call that happens to return much later on another stack.

/// Registers preserved across a switch, in the order the assembly saves them.
#[repr(C)]
pub struct Context {
    ra: usize,
    sp: usize,
    s: [usize; 12],
}

impl Context {
    /// A slot for the first switch away from a thread that already runs.
    pub const fn empty() -> Context {
        Context {
            ra: 0,
            sp: 0,
            s: [0; 12],
        }
    }

    /// A context that, when switched to, starts `entry` on `stack_top` with
    /// a zero frame pointer so backtraces end there.
    pub fn new(entry: extern "C" fn() -> !, stack_top: usize) -> Context {
        Context {
            ra: entry as *const () as usize,
            sp: stack_top & !0xf,
            s: [0; 12],
        }
    }
}

core::arch::global_asm!(
    r#"
    .section .text
    .globl __switch
    .type __switch, @function
__switch:
    sd ra, 0(a0)
    sd sp, 8(a0)
    .irp n, 0,1,2,3,4,5,6,7,8,9,10,11
    sd s\n, 16+8*\n(a0)
    .endr

    ld ra, 0(a1)
    ld sp, 8(a1)
    .irp n, 0,1,2,3,4,5,6,7,8,9,10,11
    ld s\n, 16+8*\n(a1)
    .endr
    ret
    .size __switch, . - __switch
"#
);

unsafe extern "C" {
    fn __switch(prev: *mut Context, next: *const Context);
}

/// Saves the current thread into `prev` and resumes `next`. Returns when
/// something switches back to `prev`.
///
/// # Safety
/// `prev` must be the running thread's own slot. `next` must have been
/// filled by an earlier `switch` or by [`Context::new`], and its stack must
/// still be allocated. Interrupts should be off, and no spinlock may be
/// held across the call.
pub unsafe fn switch(prev: *mut Context, next: *const Context) {
    // SAFETY: forwarded from the caller's contract.
    unsafe { __switch(prev, next) }
}
