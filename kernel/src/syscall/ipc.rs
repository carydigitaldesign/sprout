//! Channel and port syscalls.

use alloc::vec::Vec;

use sprout_abi::{
    CHANNEL_MAX_BYTES, CHANNEL_MAX_HANDLES, Error, Handle, Message, PortPacket, Rights,
};

use super::{deadline, lookup, user};
use crate::obj::channel::{self, Endpoint};
use crate::obj::handle::Entry;
use crate::obj::{Object, Port, Process};

fn endpoint(
    process: &Process,
    handle: Handle,
    needs: Rights,
) -> Result<alloc::sync::Arc<Endpoint>, Error> {
    match lookup(process, handle, needs)?.object {
        Object::Channel(endpoint) => Ok(endpoint),
        _ => Err(Error::WrongType),
    }
}

fn port(process: &Process, handle: Handle, needs: Rights) -> Result<alloc::sync::Arc<Port>, Error> {
    match lookup(process, handle, needs)?.object {
        Object::Port(port) => Ok(port),
        _ => Err(Error::WrongType),
    }
}

pub fn channel_create(process: &Process, out: usize) -> Result<usize, Error> {
    user::check_range(process, out, 2 * size_of::<Handle>(), true)?;
    let (a, b) = Endpoint::pair();
    let handles = {
        let mut table = process.handles.lock();
        [
            table.insert(Object::Channel(a), Rights::ALL),
            table.insert(Object::Channel(b), Rights::ALL),
        ]
    };
    user::write_user_slice(process, out, &handles)?;
    Ok(0)
}

pub fn channel_send(process: &Process, handle: Handle, msg_ptr: usize) -> Result<usize, Error> {
    let endpoint = endpoint(process, handle, Rights::WRITE)?;
    let msg: Message = user::read_user(process, msg_ptr)?;
    if msg.bytes_len > CHANNEL_MAX_BYTES || msg.handles_len > CHANNEL_MAX_HANDLES {
        return Err(Error::InvalidArgument);
    }

    let mut bytes = alloc::vec![0u8; msg.bytes_len];
    user::copy_from_user(process, &mut bytes, msg.bytes as usize)?;
    let mut ids = [0 as Handle; CHANNEL_MAX_HANDLES];
    let ids = &mut ids[..msg.handles_len];
    user::read_user_slice(process, msg.handles as usize, ids)?;

    // Every handle must be transferable and none may be the endpoint
    // itself; only then do they leave this process, all at once.
    let mut table = process.handles.lock();
    for &id in ids.iter() {
        if id == handle {
            return Err(Error::InvalidArgument);
        }
        if !table.get(id)?.rights.contains(Rights::TRANSFER) {
            return Err(Error::AccessDenied);
        }
    }
    if endpoint.peer_closed() {
        return Err(Error::PeerClosed);
    }
    let handles: Vec<Entry> = ids
        .iter()
        .map(|&id| table.remove(id).expect("validated above"))
        .collect();
    drop(table);

    match endpoint.send(channel::Message { bytes, handles }) {
        Ok(()) => Ok(0),
        Err(e) => Err(e),
    }
}

pub fn channel_recv(
    process: &Process,
    handle: Handle,
    msg_ptr: usize,
    timeout_ms: u64,
) -> Result<usize, Error> {
    let endpoint = endpoint(process, handle, Rights::READ)?;
    let mut msg: Message = user::read_user(process, msg_ptr)?;
    user::check_range(process, msg.bytes as usize, msg.bytes_cap, true)?;
    user::check_range(
        process,
        msg.handles as usize,
        msg.handles_cap * size_of::<Handle>(),
        true,
    )?;

    let received = endpoint.recv(msg.bytes_cap, msg.handles_cap, deadline(timeout_ms))?;
    user::copy_to_user(process, msg.bytes as usize, &received.bytes)?;
    let ids: Vec<Handle> = {
        let mut table = process.handles.lock();
        received
            .handles
            .into_iter()
            .map(|entry| table.insert(entry.object, entry.rights))
            .collect()
    };
    user::write_user_slice(process, msg.handles as usize, &ids)?;
    msg.bytes_len = received.bytes.len();
    msg.handles_len = ids.len();
    user::write_user(process, msg_ptr, &msg)?;
    Ok(0)
}

pub fn port_create(process: &Process) -> Result<usize, Error> {
    let handle = process
        .handles
        .lock()
        .insert(Object::Port(Port::new()), Rights::ALL);
    Ok(handle as usize)
}

pub fn port_attach(
    process: &Process,
    port_handle: Handle,
    object: Handle,
    key: u64,
) -> Result<usize, Error> {
    let port = port(process, port_handle, Rights::WRITE)?;
    let watched = lookup(process, object, Rights::READ)?;
    port.attach(watched.object, key)?;
    Ok(0)
}

pub fn port_wait(
    process: &Process,
    port_handle: Handle,
    packet_ptr: usize,
    timeout_ms: u64,
) -> Result<usize, Error> {
    let port = port(process, port_handle, Rights::READ)?;
    user::check_range(process, packet_ptr, size_of::<PortPacket>(), true)?;
    let packet = port.wait(deadline(timeout_ms))?;
    user::write_user(process, packet_ptr, &packet)?;
    Ok(0)
}
