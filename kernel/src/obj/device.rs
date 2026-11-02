//! Devices: a register window and an interrupt line, handed to a user
//! driver as a capability. The kernel never drives the device itself.
//!
//! Interrupt policy: when the line fires, the kernel masks it at the PLIC,
//! counts it and wakes the driver. The driver handles the device and
//! unmasks with `ack`. That is what makes level-triggered lines safe to
//! hand to code that may be slow to respond.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use sprout_abi::Error;

use crate::sched::wait::WaitQueue;
use crate::sync::SpinLock;
use crate::{arch, log};

pub struct Device {
    /// Bus or transport, for descriptions: `virtio-mmio`.
    pub kind: &'static str,
    pub base: usize,
    pub size: usize,
    pub irq: u32,
    /// virtio device id (2 = block), when the transport is virtio.
    pub virtio_id: u32,
    pending: AtomicUsize,
    fired: WaitQueue,
}

static DEVICES: SpinLock<Vec<Arc<Device>>> = SpinLock::new(Vec::new());

impl Device {
    /// Registers a device and routes its interrupt to it.
    pub fn register(
        kind: &'static str,
        base: usize,
        size: usize,
        irq: u32,
        virtio_id: u32,
    ) -> Arc<Device> {
        let device = Arc::new(Device {
            kind,
            base,
            size,
            irq,
            virtio_id,
            pending: AtomicUsize::new(0),
            fired: WaitQueue::new(),
        });
        DEVICES.lock().push(device.clone());
        arch::irq_register(irq, on_interrupt);
        log::event("device")
            .str("kind", kind)
            .hex("base", base)
            .num("size", size)
            .num("irq", irq)
            .num("virtio_id", virtio_id)
            .str("virtio_device", virtio_device_name(virtio_id))
            .end();
        device
    }

    /// Waits for an interrupt; returns how many fired since the last wait.
    pub fn wait_irq(&self, deadline: Option<u64>) -> Result<usize, Error> {
        if !self
            .fired
            .wait_while_until(|| self.pending.load(Ordering::Acquire) == 0, deadline)
        {
            return Err(Error::TimedOut);
        }
        Ok(self.pending.swap(0, Ordering::AcqRel))
    }

    /// Unmasks the line after the driver has handled the interrupt.
    pub fn ack_irq(&self) {
        arch::irq_unmask(self.irq);
    }

    /// Interrupts counted but not yet collected by `wait_irq`.
    pub fn pending_irqs(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }
}

/// Runs from the external interrupt handler.
fn on_interrupt(source: u32) {
    let device = DEVICES.lock().iter().find(|d| d.irq == source).cloned();
    if let Some(device) = device {
        arch::irq_mask(source);
        device.pending.fetch_add(1, Ordering::AcqRel);
        device.fired.wake_all();
    }
}

/// Every registered device.
pub fn all() -> Vec<Arc<Device>> {
    DEVICES.lock().clone()
}

/// Human name for a virtio device id.
pub fn virtio_device_name(id: u32) -> &'static str {
    match id {
        1 => "network",
        2 => "block",
        3 => "console",
        4 => "entropy",
        16 => "gpu",
        18 => "input",
        _ => "unknown",
    }
}
