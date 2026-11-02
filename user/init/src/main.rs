//! `init`: the first thing that runs in user mode.
//!
//! It exercises every M4 syscall once and logs what the kernel says about
//! itself and about init's own process handle. Its exit code is the
//! kernel test's verdict: 0 means every check passed.

#![no_std]
#![no_main]

use sprout_rt as rt;

/// Logs a description, which is JSON the kernel produced.
fn show(what: &str, handle: rt::Handle) -> bool {
    let mut buf = [0u8; 4096];
    match rt::describe(handle, &mut buf) {
        Ok(needed) if needed <= buf.len() => {
            let text = core::str::from_utf8(&buf[..needed]).unwrap_or("<not utf-8>");
            rt::log!("{what}: {text}");
            true
        }
        Ok(needed) => {
            rt::log!(
                "{what}: description needs {needed} bytes, buffer has {}",
                buf.len()
            );
            false
        }
        Err(e) => {
            rt::log!("{what}: describe failed: {}", e.name());
            false
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    rt::log("hello from userspace");

    let mut ok = true;
    ok &= show("kernel", rt::HANDLE_KERNEL);
    ok &= show("self", rt::HANDLE_SELF_PROCESS);

    // A VMO through a handle: create, write, read back, close.
    match rt::vmo_create(8192) {
        Ok(vmo) => {
            let mut back = [0u8; 6];
            let round_trip = rt::vmo_write(vmo, 4090, b"sprout").is_ok()
                && rt::vmo_read(vmo, 4090, &mut back).is_ok()
                && &back == b"sprout";
            rt::log!("vmo round trip across a page boundary: {round_trip}");
            ok &= round_trip;
            ok &= show("vmo", vmo);
            // Out of range must be refused, not silently truncated.
            let refused = rt::vmo_read(vmo, 8190, &mut back) == Err(rt::Error::OutOfRange);
            rt::log!("vmo out-of-range read refused: {refused}");
            ok &= refused;
            ok &= rt::handle_close(vmo).is_ok();
            let closed = rt::vmo_read(vmo, 0, &mut back) == Err(rt::Error::InvalidHandle);
            rt::log!("closed handle refused: {closed}");
            ok &= closed;
        }
        Err(e) => {
            rt::log!("vmo_create failed: {}", e.name());
            ok = false;
        }
    }

    // The kernel must not let us touch its memory through a syscall.
    let kernel_ptr = 0xffff_ffc0_8020_0000usize as *mut u8;
    // SAFETY: the pointer is never dereferenced here; it is handed to the
    // kernel, which must reject it.
    let bad = rt::syscall(rt::abi::sys::LOG, [kernel_ptr as usize, 16, 0, 0, 0, 0]);
    let rejected = rt::abi::decode(bad) == Err(rt::Error::BadAddress);
    rt::log!("kernel address rejected: {rejected}");
    ok &= rejected;

    rt::yield_now();
    rt::sleep_ms(5);
    rt::log!("init done, ok={ok}");
    if ok { 0 } else { 1 }
}
