//! Switching between kernel threads.
//!
//! Only callee-saved state needs saving: the System V calling convention
//! lets `switch` clobber everything else, because to the caller it is just
//! a function call that happens to return much later on another stack.
//! A thread's context is its stack pointer; the callee-saved registers
//! and the return address live on the stack itself.

/// A saved thread: where its stack pointer was when it switched away.
#[repr(C)]
pub struct Context {
    rsp: usize,
}

/// Words `__switch` pops, lowest address first: r15, r14, r13, r12, rbp,
/// rbx, then the return address.
const FRAME_WORDS: usize = 7;

impl Context {
    /// A slot for the first switch away from a thread that already runs.
    pub const fn empty() -> Context {
        Context { rsp: 0 }
    }

    /// A context that, when switched to, starts `entry` on `stack_top`
    /// with a zero frame pointer so backtraces end there.
    pub fn new(entry: extern "C" fn() -> !, stack_top: usize) -> Context {
        // After the pops, `ret` must leave the stack as a `call` would:
        // 8 bytes past a 16-byte boundary.
        let top = (stack_top & !0xf) - 16;
        let base = top - FRAME_WORDS * 8;
        let frame = base as *mut usize;
        // SAFETY: the caller hands us a fresh stack of which these words
        // are the top; nothing else uses them yet.
        unsafe {
            for i in 0..FRAME_WORDS - 1 {
                frame.add(i).write(0);
            }
            frame
                .add(FRAME_WORDS - 1)
                .write(entry as *const () as usize);
        }
        Context { rsp: base }
    }
}

core::arch::global_asm!(
    r#"
    .section .text
    .globl __switch
    .type __switch, @function
__switch:
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, [rsi]
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
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
