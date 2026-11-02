//! In-kernel test harness, on stable Rust.
//!
//! Tests are declared next to the code they cover:
//!
//! ```ignore
//! crate::ktest! {
//!     fn frame_allocator_hands_out_distinct_frames() {
//!         assert_ne!(alloc(), alloc());
//!     }
//! }
//! ```
//!
//! The macro places a [`TestCase`] in the `.ktests` linker section, which
//! the linker script gathers between `__ktests_start` and `__ktests_end`.
//! The test kernel (`--features ktest`) walks that table, prints one JSON
//! line per test and exits QEMU with 0 (all passed) or 1 (a test panicked).
//! Outside the test kernel the macro expands to nothing.

/// One registered test.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
pub struct TestCase {
    /// Full module path of the test function.
    pub name: &'static str,
    /// The test body. Panicking fails the test.
    pub run: fn(),
}

/// Declares a kernel test. See the module docs.
#[macro_export]
macro_rules! ktest {
    ($(#[$meta:meta])* fn $name:ident() $body:block) => {
        #[cfg(feature = "ktest")]
        $(#[$meta])*
        fn $name() $body

        #[cfg(feature = "ktest")]
        mod $name {
            #[used]
            #[unsafe(link_section = ".ktests")]
            static ENTRY: $crate::testing::TestCase = $crate::testing::TestCase {
                name: module_path!(),
                run: super::$name,
            };
        }
    };
}

#[cfg(feature = "ktest")]
mod harness {
    use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

    use super::TestCase;
    use crate::log;

    unsafe extern "C" {
        static __ktests_start: u8;
        static __ktests_end: u8;
    }

    static CURRENT_NAME: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
    static CURRENT_LEN: AtomicUsize = AtomicUsize::new(0);

    fn registered() -> &'static [TestCase] {
        let start = core::ptr::addr_of!(__ktests_start) as usize;
        let end = core::ptr::addr_of!(__ktests_end) as usize;
        let count = (end - start) / core::mem::size_of::<TestCase>();
        // SAFETY: the linker script lays out `.ktests` as a packed array of
        // `TestCase` values between these two symbols, each placed there by
        // the `ktest!` macro, and the section is immutable for the kernel's
        // lifetime.
        unsafe { core::slice::from_raw_parts(start as *const TestCase, count) }
    }

    /// The test currently running, if any. Read by the panic handler.
    pub fn current() -> Option<&'static str> {
        let ptr = CURRENT_NAME.load(Ordering::Acquire);
        if ptr.is_null() {
            return None;
        }
        let len = CURRENT_LEN.load(Ordering::Acquire);
        // SAFETY: the pointer and length were taken from a `&'static str`
        // in `run_all` and are only cleared, never changed to anything else.
        Some(unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(ptr, len)) })
    }

    /// Runs every registered test. Returns the exit code: 0 if all passed.
    /// A failing test panics, so control reaches the end only on success.
    pub fn run_all() -> u32 {
        let tests = registered();
        log::event("tests").num("count", tests.len()).end();
        for test in tests {
            CURRENT_LEN.store(test.name.len(), Ordering::Release);
            CURRENT_NAME.store(test.name.as_ptr().cast_mut(), Ordering::Release);
            (test.run)();
            CURRENT_NAME.store(core::ptr::null_mut(), Ordering::Release);
            log::line().str("test", test.name).bool("ok", true).end();
        }
        log::event("tests_done")
            .num("passed", tests.len())
            .num("failed", 0usize)
            .end();
        0
    }
}

#[cfg(feature = "ktest")]
pub use harness::{current, run_all};

crate::ktest! {
    fn harness_smoke() {
        assert_eq!(2 + 2, 4);
    }
}
