//! Panics become one JSON line with a backtrace, then a machine stop. Inside
//! the test kernel a panic fails the running test instead.

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{arch, log};

/// Exit code for a kernel panic outside of tests.
const EXIT_PANIC: u32 = 2;
/// Most frames a panic line will carry.
const MAX_FRAMES: usize = 32;

static PANICKING: AtomicBool = AtomicBool::new(false);

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Nothing may preempt the report, and nothing else will run again.
    arch::interrupts::disable();

    // A panic while reporting a panic (console broken, bad frame pointer)
    // would recurse forever. Stop instead; the first line already went out.
    if PANICKING.swap(true, Ordering::AcqRel) {
        arch::exit(EXIT_PANIC);
    }

    // SAFETY: interrupts are off and this context never returns, so if the
    // console lock is held it is held by us (a panic mid-line) and no other
    // holder can be running.
    unsafe { log::force_unlock_console() };

    let frames = arch::stack_frames(arch::frame_pointer()).take(MAX_FRAMES);

    #[cfg(feature = "ktest")]
    if let Some(test) = crate::testing::current() {
        /// Exit code when a kernel test fails.
        const EXIT_TEST_FAILED: u32 = 1;
        let mut line = log::line()
            .str("test", test)
            .bool("ok", false)
            .fmt("panic", format_args!("{}", info.message()));
        if let Some(loc) = info.location() {
            line = line.str("file", loc.file()).num("line", loc.line());
        }
        line.hex_list("frames", frames).end();
        arch::exit(EXIT_TEST_FAILED);
    }

    let mut line = log::event("panic").fmt("msg", format_args!("{}", info.message()));
    if let Some(loc) = info.location() {
        line = line.str("file", loc.file()).num("line", loc.line());
    }
    line.hex_list("frames", frames).end();
    arch::exit(EXIT_PANIC)
}
