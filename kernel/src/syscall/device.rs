//! Device and DMA syscalls: what a user driver needs and nothing more.

use alloc::sync::Arc;

use sprout_abi::{Error, Handle, Rights};

use super::{deadline, lookup};
use crate::mm::MapFlags;
use crate::obj::{Device, Object, Process};

fn device(process: &Process, handle: Handle, needs: Rights) -> Result<Arc<Device>, Error> {
    match lookup(process, handle, needs)?.object {
        Object::Device(device) => Ok(device),
        _ => Err(Error::WrongType),
    }
}

pub fn map(process: &Process, handle: Handle) -> Result<usize, Error> {
    let device = device(process, handle, Rights::MAP)?;
    process.map_physical(device.base, device.size, MapFlags::READ | MapFlags::WRITE)
}

pub fn irq_wait(process: &Process, handle: Handle, timeout_ms: u64) -> Result<usize, Error> {
    let device = device(process, handle, Rights::READ)?;
    device.wait_irq(deadline(timeout_ms))
}

pub fn irq_ack(process: &Process, handle: Handle) -> Result<usize, Error> {
    let device = device(process, handle, Rights::WRITE)?;
    device.ack_irq();
    Ok(0)
}

pub fn vmo_physical(process: &Process, handle: Handle, offset: usize) -> Result<usize, Error> {
    let Object::Vmo(vmo) = lookup(process, handle, Rights::MAP)?.object else {
        return Err(Error::WrongType);
    };
    vmo.lock().physical(offset).ok_or(Error::InvalidArgument)
}
