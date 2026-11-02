//! Syscall dispatch: the kernel's entire interface to userspace. The table
//! in `sprout-abi` is the spec; this file is the implementation, and
//! [`describe`] renders the spec so userspace can read it at runtime.

pub mod describe;
mod user;

use alloc::sync::Arc;

use sprout_abi::{Error, Handle, Rights, sys};

use crate::mm::vmo::Vmo;
use crate::obj::handle::Entry;
use crate::obj::{Object, Process};
use crate::sync::SpinLock;
use crate::{log, sched, time};

/// Longest message `log` accepts.
const MAX_LOG_BYTES: usize = 512;
/// Largest VMO a process may create, until there is a budget per process.
const MAX_VMO_BYTES: usize = 64 << 20;
/// Exit code for a process killed by a fault: 128 plus the exception cause.
const FAULT_EXIT_BASE: i32 = 128;

/// Handles `ecall` from user mode. Returns the value for `a0`, or exits
/// the calling thread for `exit`.
pub fn dispatch(num: usize, args: [usize; 6]) -> isize {
    let process = current();
    if num == sys::EXIT {
        process.exit(args[0] as i32);
        // The thread never returns, so nothing on this stack is dropped:
        // release the process reference by hand or its memory leaks.
        drop(process);
        sched::exit();
    }
    let result = match num {
        sys::LOG => sys_log(&process, args[0], args[1]),
        sys::YIELD => {
            sched::yield_now();
            Ok(0)
        }
        sys::SLEEP => {
            time::sleep_ms(args[0] as u64);
            Ok(0)
        }
        sys::DESCRIBE => sys_describe(&process, args[0] as Handle, args[1], args[2]),
        sys::HANDLE_CLOSE => process.handles.lock().remove(args[0] as Handle).map(|_| 0),
        sys::VMO_CREATE => sys_vmo_create(&process, args[0]),
        sys::VMO_READ => sys_vmo_rw(&process, args, false),
        sys::VMO_WRITE => sys_vmo_rw(&process, args, true),
        _ => Err(Error::NoSuchSyscall),
    };
    match result {
        Ok(value) => value as isize,
        Err(e) => e.code(),
    }
}

/// A fault in user mode: log it, kill the process, leave the thread.
pub fn user_fault(cause: &str, code: usize, pc: usize, address: usize) -> ! {
    let process = current();
    log::event("user_fault")
        .num("pid", process.pid)
        .str("name", &process.name)
        .str("cause", cause)
        .num("code", code)
        .hex("pc", pc)
        .hex("address", address)
        .end();
    process.exit(FAULT_EXIT_BASE + code as i32);
    drop(process);
    sched::exit()
}

fn current() -> Arc<Process> {
    sched::current_process().expect("syscall from a thread without a process")
}

/// Looks a handle up and checks the rights the operation needs.
fn lookup(process: &Process, handle: Handle, needs: Rights) -> Result<Entry, Error> {
    let entry = process.handles.lock().get(handle)?.clone();
    if !entry.rights.contains(needs) {
        return Err(Error::AccessDenied);
    }
    Ok(entry)
}

fn sys_log(process: &Process, ptr: usize, len: usize) -> Result<usize, Error> {
    if len > MAX_LOG_BYTES {
        return Err(Error::InvalidArgument);
    }
    let mut buf = [0u8; MAX_LOG_BYTES];
    user::copy_from_user(process, &mut buf[..len], ptr)?;
    let msg = core::str::from_utf8(&buf[..len]).map_err(|_| Error::InvalidArgument)?;
    log::info("user", msg).num("pid", process.pid).end();
    Ok(0)
}

fn sys_describe(process: &Process, handle: Handle, buf: usize, len: usize) -> Result<usize, Error> {
    let text = if handle == sprout_abi::HANDLE_KERNEL {
        describe::kernel()
    } else {
        let entry = lookup(process, handle, Rights::DESCRIBE)?;
        describe::object(handle, &entry)
    };
    let bytes = text.as_bytes();
    let n = bytes.len().min(len);
    user::copy_to_user(process, buf, &bytes[..n])?;
    Ok(bytes.len())
}

fn sys_vmo_create(process: &Process, size: usize) -> Result<usize, Error> {
    if size == 0 || size > MAX_VMO_BYTES {
        return Err(Error::InvalidArgument);
    }
    let vmo = Vmo::new(size).map_err(|_| Error::OutOfMemory)?;
    let handle = process
        .handles
        .lock()
        .insert(Object::Vmo(Arc::new(SpinLock::new(vmo))), Rights::ALL);
    Ok(handle as usize)
}

/// `vmo_read` and `vmo_write` through a bounded kernel buffer, so a large
/// transfer never pins a large kernel allocation.
fn sys_vmo_rw(process: &Process, args: [usize; 6], write: bool) -> Result<usize, Error> {
    let (handle, offset, ptr, len) = (args[0] as Handle, args[1], args[2], args[3]);
    let needs = if write { Rights::WRITE } else { Rights::READ };
    let entry = lookup(process, handle, needs)?;
    let Object::Vmo(vmo) = entry.object else {
        return Err(Error::WrongType);
    };
    if offset
        .checked_add(len)
        .is_none_or(|end| end > vmo.lock().size())
    {
        return Err(Error::OutOfRange);
    }
    // Validate the whole user range up front so a failure cannot leave a
    // half-written VMO behind a successful-looking partial copy.
    user::check_range(process, ptr, len, !write)?;

    let mut chunk = [0u8; 256];
    let mut done = 0;
    while done < len {
        let n = chunk.len().min(len - done);
        if write {
            user::copy_from_user(process, &mut chunk[..n], ptr + done)?;
            vmo.lock()
                .write(offset + done, &chunk[..n])
                .map_err(|_| Error::OutOfRange)?;
        } else {
            vmo.lock()
                .read(offset + done, &mut chunk[..n])
                .map_err(|_| Error::OutOfRange)?;
            user::copy_to_user(process, ptr + done, &chunk[..n])?;
        }
        done += n;
    }
    Ok(0)
}
