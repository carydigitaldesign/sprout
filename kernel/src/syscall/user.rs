//! Moving bytes across the user/kernel boundary. Every user pointer is
//! untrusted: the range must lie below the user limit and every page must
//! be mapped for user access (and writable, for copies into user memory).
//! The copies run with `sstatus.SUM` set, which `enter_user` turned on.

use sprout_abi::{Error, USER_ADDRESS_LIMIT};

use crate::arch::PAGE_SIZE;
use crate::mm::MapFlags;
use crate::obj::Process;

/// Checks that `[ptr, ptr + len)` is user memory the process can read, and
/// write if `writable` is asked for.
pub fn check_range(process: &Process, ptr: usize, len: usize, writable: bool) -> Result<(), Error> {
    if len == 0 {
        return Ok(());
    }
    let end = ptr.checked_add(len).ok_or(Error::BadAddress)?;
    if end > USER_ADDRESS_LIMIT {
        return Err(Error::BadAddress);
    }
    let needs = if writable {
        MapFlags::USER | MapFlags::READ | MapFlags::WRITE
    } else {
        MapFlags::USER | MapFlags::READ
    };
    let table = process.table.lock();
    let mut page = ptr & !(PAGE_SIZE - 1);
    while page < end {
        match table.translate(page) {
            Some((_, flags)) if flags.contains(needs) => {}
            _ => return Err(Error::BadAddress),
        }
        page += PAGE_SIZE;
    }
    Ok(())
}

/// Copies `dst.len()` bytes from user address `src`.
pub fn copy_from_user(process: &Process, dst: &mut [u8], src: usize) -> Result<(), Error> {
    check_range(process, src, dst.len(), false)?;
    // SAFETY: the range was just checked to be mapped, user-accessible and
    // readable in the current address space, SUM is set, and `dst` is a
    // kernel buffer of exactly that length.
    unsafe { core::ptr::copy_nonoverlapping(src as *const u8, dst.as_mut_ptr(), dst.len()) };
    Ok(())
}

/// Copies `src` to user address `dst`.
pub fn copy_to_user(process: &Process, dst: usize, src: &[u8]) -> Result<(), Error> {
    check_range(process, dst, src.len(), true)?;
    // SAFETY: as above, with the range checked writable.
    unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u8, src.len()) };
    Ok(())
}
