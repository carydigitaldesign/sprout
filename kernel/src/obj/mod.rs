//! Kernel objects and the handles that reach them (PLAN.md 2.1, ADR-0003).
//!
//! Everything a process can touch is an [`Object`] behind a handle in its
//! [`handle::HandleTable`], with [`Rights`] that say what it may do. There
//! are no global names: a process reaches exactly what it was handed, and
//! handles move between processes only through channels.

pub mod channel;
pub mod device;
pub mod handle;
pub mod port;
pub mod process;

use alloc::sync::Arc;

use sprout_abi::ObjectType;

use crate::mm::vmo::Vmo;
use crate::sync::SpinLock;
pub use channel::Endpoint;
pub use device::Device;
pub use port::Port;
pub use process::Process;

/// A reference to a kernel object. Cloning shares the object.
#[derive(Clone)]
pub enum Object {
    Process(Arc<Process>),
    Vmo(Arc<SpinLock<Vmo>>),
    Channel(Arc<Endpoint>),
    Port(Arc<Port>),
    Device(Arc<Device>),
}

impl Object {
    pub fn kind(&self) -> ObjectType {
        match self {
            Object::Process(_) => ObjectType::Process,
            Object::Vmo(_) => ObjectType::Vmo,
            Object::Channel(_) => ObjectType::Channel,
            Object::Port(_) => ObjectType::Port,
            Object::Device(_) => ObjectType::Device,
        }
    }
}
