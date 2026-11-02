//! Stack walking through frame pointers.
//!
//! The kernel is compiled with `-C force-frame-pointers=yes`, so every
//! function keeps `rbp` pointing at its frame: the caller's `rbp` sits at
//! `[rbp]` and the return address at `[rbp + 8]`. Frames are validated
//! against the running thread's stack, which the scheduler announces with
//! [`set_current_stack`] on every switch; on x86_64 that call also tells
//! the TSS which stack traps from user mode should land on.

use core::sync::atomic::{AtomicUsize, Ordering};

use super::gdt;

unsafe extern "C" {
    static __stack_bottom: u8;
    static __stack_top: u8;
    static __kernel_start: u8;
    static __kernel_end: u8;
}

static STACK_BOTTOM: AtomicUsize = AtomicUsize::new(0);
static STACK_TOP: AtomicUsize = AtomicUsize::new(0);

/// The boot CPU's stack from the linker script: `(bottom, top)`.
pub fn boot_stack() -> (usize, usize) {
    (
        core::ptr::addr_of!(__stack_bottom) as usize,
        core::ptr::addr_of!(__stack_top) as usize,
    )
}

/// Tells the walker, and the CPU, which stack the current thread runs on.
pub fn set_current_stack(bottom: usize, top: usize) {
    STACK_BOTTOM.store(bottom, Ordering::Relaxed);
    STACK_TOP.store(top, Ordering::Relaxed);
    gdt::set_rsp0(top);
}

/// Starts out on the boot stack.
pub fn init() {
    let (bottom, top) = boot_stack();
    set_current_stack(bottom, top);
}

/// The current function's frame pointer.
#[inline(always)]
pub fn frame_pointer() -> usize {
    let fp: usize;
    // SAFETY: reading `rbp` has no side effects.
    unsafe { core::arch::asm!("mov {}, rbp", out(reg) fp, options(nomem, nostack)) };
    fp
}

/// Return addresses up the stack from `fp`, innermost first.
pub fn frames_from(fp: usize) -> Frames {
    Frames { fp }
}

/// Iterator behind [`frames_from`]. Stops at the first frame that does not
/// look like one: outside the current stack, misaligned, or a return
/// address outside the kernel image.
pub struct Frames {
    fp: usize,
}

impl Iterator for Frames {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        let stack_bottom = STACK_BOTTOM.load(Ordering::Relaxed);
        let stack_top = STACK_TOP.load(Ordering::Relaxed);
        let kernel_start = core::ptr::addr_of!(__kernel_start) as usize;
        let kernel_end = core::ptr::addr_of!(__kernel_end) as usize;

        let fp = self.fp;
        if fp < stack_bottom || fp.checked_add(16).is_none_or(|end| end > stack_top) || fp % 8 != 0
        {
            return None;
        }
        // SAFETY: `fp` lies inside the current stack with room for two
        // words above it (checked above), so both reads are of mapped,
        // initialised stack memory.
        let (caller_fp, ra) = unsafe {
            (
                core::ptr::read(fp as *const usize),
                core::ptr::read((fp + 8) as *const usize),
            )
        };
        if !(kernel_start..kernel_end).contains(&ra) {
            return None;
        }
        if caller_fp != 0 && caller_fp <= fp {
            return None;
        }
        self.fp = caller_fp;
        Some(ra)
    }
}

crate::ktest! {
    fn backtrace_walks_through_nested_calls() {
        #[inline(never)]
        fn inner() -> usize {
            frames_from(frame_pointer()).take(32).count()
        }
        #[inline(never)]
        fn outer() -> usize {
            core::hint::black_box(inner()) + 0
        }
        let frames = core::hint::black_box(outer());
        assert!(frames >= 2, "only {frames} frame(s) found");
    }
}

crate::ktest! {
    fn backtrace_stops_on_garbage_frame_pointer() {
        assert_eq!(frames_from(0).count(), 0);
        assert_eq!(frames_from(usize::MAX - 7).count(), 0);
        assert_eq!(frames_from(frame_pointer() + 4).count(), 0, "misaligned fp");
    }
}
