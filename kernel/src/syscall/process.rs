//! Process and mapping syscalls.

use alloc::vec::Vec;

use sprout_abi::{Error, Handle, Rights};

use super::{lookup, user};
use crate::mm::MapFlags;
use crate::obj::{self, Object, Process};

/// Longest process name a spawner may give.
const MAX_NAME: usize = 32;

pub fn spawn(
    process: &Process,
    image: Handle,
    bootstrap: Handle,
    name_ptr: usize,
    name_len: usize,
) -> Result<usize, Error> {
    if name_len > MAX_NAME {
        return Err(Error::InvalidArgument);
    }
    let mut name_buf = [0u8; MAX_NAME];
    user::copy_from_user(process, &mut name_buf[..name_len], name_ptr)?;
    let name = core::str::from_utf8(&name_buf[..name_len]).map_err(|_| Error::InvalidArgument)?;

    let Object::Vmo(vmo) = lookup(process, image, Rights::READ)?.object else {
        return Err(Error::WrongType);
    };
    let elf = {
        let vmo = vmo.lock();
        let mut bytes = alloc::vec![0u8; vmo.size()];
        vmo.read(0, &mut bytes).map_err(|_| Error::OutOfRange)?;
        bytes
    };

    // The bootstrap handle leaves this process; put it back if the spawn
    // fails (its number may change).
    let mut extra = Vec::new();
    if bootstrap != 0 {
        lookup(process, bootstrap, Rights::TRANSFER)?;
        let entry = process.handles.lock().remove(bootstrap)?;
        extra.push((entry.object, entry.rights));
    }
    match obj::process::spawn(name, &elf, extra.clone()) {
        Ok(child) => {
            let handle = process.handles.lock().insert(
                Object::Process(child),
                Rights::READ | Rights::DESCRIBE | Rights::DUPLICATE | Rights::TRANSFER,
            );
            Ok(handle as usize)
        }
        Err(e) => {
            for (object, rights) in extra {
                process.handles.lock().insert(object, rights);
            }
            crate::log::warn("syscall", "process_spawn failed")
                .fmt("error", format_args!("{e}"))
                .end();
            Err(Error::InvalidArgument)
        }
    }
}

pub fn wait(process: &Process, handle: Handle) -> Result<usize, Error> {
    let Object::Process(child) = lookup(process, handle, Rights::READ)?.object else {
        return Err(Error::WrongType);
    };
    Ok(child.wait() as u32 as usize)
}

pub fn vmo_map(process: &Process, handle: Handle) -> Result<usize, Error> {
    let entry = lookup(process, handle, Rights::MAP | Rights::READ)?;
    let Object::Vmo(vmo) = entry.object else {
        return Err(Error::WrongType);
    };
    let mut flags = MapFlags::READ;
    if entry.rights.contains(Rights::WRITE) {
        flags = flags | MapFlags::WRITE;
    }
    process.map_vmo(vmo, flags)
}
