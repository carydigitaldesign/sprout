//! Panics become one JSON line and a machine stop. Inside the test kernel a
//! panic fails the running test instead of the whole run's reporting.

use core::panic::PanicInfo;

use crate::{arch, log};

/// Exit code for a kernel panic outside of tests.
const EXIT_PANIC: u32 = 2;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
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
        line.end();
        arch::exit(EXIT_TEST_FAILED);
    }

    let mut line = log::event("panic").fmt("msg", format_args!("{}", info.message()));
    if let Some(loc) = info.location() {
        line = line.str("file", loc.file()).num("line", loc.line());
    }
    line.end();
    arch::exit(EXIT_PANIC)
}
