//! Ports: wait on several objects at once. Attached objects wake the port
//! whenever they change; `wait` reports the first one with a signal up.
//! Level-triggered: a signal stays reported until its cause is gone.

use alloc::sync::Arc;
use alloc::vec::Vec;

use sprout_abi::{Error, PortPacket};

use super::Object;
use crate::sched::wait::WaitQueue;
use crate::sync::SpinLock;

pub struct Port {
    attachments: SpinLock<Vec<(Object, u64)>>,
    changed: WaitQueue,
}

impl Port {
    pub fn new() -> Arc<Port> {
        Arc::new(Port {
            attachments: SpinLock::new(Vec::new()),
            changed: WaitQueue::new(),
        })
    }

    /// Watches `object`; packets about it carry `key`.
    pub fn attach(self: &Arc<Self>, object: Object, key: u64) -> Result<(), Error> {
        match &object {
            Object::Channel(endpoint) => endpoint.observe(Arc::downgrade(self)),
            _ => return Err(Error::WrongType),
        }
        self.attachments.lock().push((object, key));
        Ok(())
    }

    /// Called by attached objects when their signals may have changed.
    pub fn notify(&self) {
        self.changed.wake_all();
    }

    fn poll(&self) -> Option<PortPacket> {
        for (object, key) in self.attachments.lock().iter() {
            let bits = match object {
                Object::Channel(endpoint) => endpoint.signals(),
                _ => 0,
            };
            if bits != 0 {
                return Some(PortPacket {
                    key: *key,
                    signals: bits,
                    _reserved: 0,
                });
            }
        }
        None
    }

    /// Waits until some attachment has a signal asserted, or `deadline`
    /// (a tick count; `None` waits forever) passes.
    pub fn wait(&self, deadline: Option<u64>) -> Result<PortPacket, Error> {
        loop {
            if let Some(packet) = self.poll() {
                return Ok(packet);
            }
            if !self
                .changed
                .wait_while_until(|| self.poll().is_none(), deadline)
            {
                return Err(Error::TimedOut);
            }
        }
    }

    pub fn attachment_count(&self) -> usize {
        self.attachments.lock().len()
    }
}

#[cfg(feature = "ktest")]
mod tests {
    use super::*;
    use crate::obj::channel::{Endpoint, Message};
    use crate::{sched, time};
    use sprout_abi::signals;

    static LATE_SENDER: SpinLock<Option<Arc<Endpoint>>> = SpinLock::new(None);

    fn send_after_a_while(_: usize) {
        time::sleep_ms(20);
        if let Some(ep) = LATE_SENDER.lock().take() {
            ep.send(Message {
                bytes: b"wake".to_vec(),
                handles: Vec::new(),
            })
            .expect("send");
        }
    }

    crate::ktest! {
        fn port_reports_a_readable_endpoint_and_times_out_otherwise() {
            let (a, b) = Endpoint::pair();
            let port = Port::new();
            port.attach(Object::Channel(b.clone()), 42).expect("attach");
            assert_eq!(port.attachment_count(), 1);
            assert_eq!(port.wait(Some(time::ticks())).err(), Some(Error::TimedOut));

            *LATE_SENDER.lock() = Some(a);
            let id = sched::spawn("port-sender", send_after_a_while, 0);
            let packet = port.wait(Some(time::ticks() + 200)).expect("woken");
            assert_eq!(packet.key, 42);
            // The sender may already have dropped its end by the time we
            // look, so peer-closed can come along with readable.
            assert_ne!(packet.signals & signals::READABLE, 0);
            sched::join(id);

            // Draining clears the signal; the dropped sender raises another.
            b.try_recv(64, 0).expect("recv").expect("queued");
            let packet = port.wait(Some(time::ticks() + 5)).expect("peer closed");
            assert_eq!(packet.signals, signals::PEER_CLOSED);
        }
    }

    crate::ktest! {
        fn only_channels_can_be_attached() {
            use crate::mm::vmo::Vmo;
            let port = Port::new();
            let vmo = Object::Vmo(Arc::new(SpinLock::new(Vmo::new(1).expect("vmo"))));
            assert_eq!(port.attach(vmo, 1), Err(Error::WrongType));
        }
    }
}
