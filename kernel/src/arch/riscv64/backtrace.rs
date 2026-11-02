//! Stack walking through frame pointers.
//!
//! The kernel is compiled with `-C force-frame-pointers=yes` (see
//! `.cargo/config.toml`), so every function keeps `s0` pointing at its frame
//! and the RISC-V ABI fixes the layout: the return address sits at `s0 - 8`
//! and the caller's frame pointer at `s0 - 16`. Precompiled `core` does not
//! maintain `s0`, but as a callee-saved register it passes through those
//! functions untouched, so the chain skips them rather than breaking.
//!
//! Frames are validated against the running thread's stack, which the
//! scheduler announces with [`set_current_stack`] on every switch.
//! Addresses are raw; `cargo xtask symbolize` turns them into names.

use core::sync::atomic::{AtomicUsize, Ordering};

unsafe extern "C" {
    static __stack_bottom: u8;
    static __stack_top: u8;
    static __kernel_start: u8;
    static __kernel_end: u8;
}

static STACK_BOTTOM: AtomicUsize = AtomicUsize::new(0);
static STACK_TOP: AtomicUsize = AtomicUsize::new(0);

/// The boot hart's stack from the linker script: `(bottom, top)`.
pub fn boot_stack() -> (usize, usize) {
    (
        core::ptr::addr_of!(__stack_bottom) as usize,
        core::ptr::addr_of!(__stack_top) as usize,
    )
}

/// Tells the walker which stack the current thread runs on.
pub fn set_current_stack(bottom: usize, top: usize) {
    STACK_BOTTOM.store(bottom, Ordering::Relaxed);
    STACK_TOP.store(top, Ordering::Relaxed);
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
    // SAFETY: reading `s0` has no side effects.
    unsafe { core::arch::asm!("mv {}, s0", out(reg) fp, options(nomem, nostack)) };
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
        if fp < stack_bottom + 16 || fp > stack_top || fp % 16 != 0 {
            return None;
        }
        // SAFETY: `fp` lies inside the current stack with room for two
        // words below it (checked above), so both reads are of mapped,
        // initialised stack memory.
        let (ra, caller_fp) = unsafe {
            (
                core::ptr::read((fp - 8) as *const usize),
                core::ptr::read((fp - 16) as *const usize),
            )
        };
        if !(kernel_start..kernel_end).contains(&ra) {
            return None;
        }
        // Stacks grow down, so a genuine caller frame is above ours. Zero
        // is the sentinel the boot code and new threads put in `s0`; it
        // fails the bounds check on the next call and ends the walk.
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
        // At least: inner's return into outer, outer's return into the test.
        assert!(frames >= 2, "only {frames} frame(s) found");
    }
}

crate::ktest! {
    fn backtrace_stops_on_garbage_frame_pointer() {
        assert_eq!(frames_from(0).count(), 0);
        assert_eq!(frames_from(usize::MAX - 15).count(), 0);
        assert_eq!(frames_from(frame_pointer() + 8).count(), 0, "misaligned fp");
    }
}
