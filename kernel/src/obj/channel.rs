//! Channels: two endpoints; what one sends the other receives, bytes and
//! handles alike. Handles travel as `Entry` values, so the receiver gets
//! exactly the rights the sender gave up.

use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use sprout_abi::{Error, signals};

use super::handle::Entry;
use super::port::Port;
use crate::sched::wait::WaitQueue;
use crate::sync::SpinLock;

/// One queued message.
pub struct Message {
    pub bytes: Vec<u8>,
    pub handles: Vec<Entry>,
}

struct State {
    queue: VecDeque<Message>,
    peer_closed: bool,
    /// Ports watching this endpoint; woken on every change.
    observers: Vec<Weak<Port>>,
}

/// One end of a channel.
pub struct Endpoint {
    peer: SpinLock<Weak<Endpoint>>,
    state: SpinLock<State>,
    readable: WaitQueue,
}

impl Endpoint {
    fn new() -> Endpoint {
        Endpoint {
            peer: SpinLock::new(Weak::new()),
            state: SpinLock::new(State {
                queue: VecDeque::new(),
                peer_closed: false,
                observers: Vec::new(),
            }),
            readable: WaitQueue::new(),
        }
    }

    /// A connected pair.
    pub fn pair() -> (Arc<Endpoint>, Arc<Endpoint>) {
        let a = Arc::new(Endpoint::new());
        let b = Arc::new(Endpoint::new());
        *a.peer.lock() = Arc::downgrade(&b);
        *b.peer.lock() = Arc::downgrade(&a);
        (a, b)
    }

    /// Queues a message for the peer.
    pub fn send(&self, message: Message) -> Result<(), Error> {
        let peer = self.peer.lock().upgrade().ok_or(Error::PeerClosed)?;
        peer.state.lock().queue.push_back(message);
        peer.notify();
        Ok(())
    }

    /// Takes the oldest message if one is queued and fits. `Ok(None)` means
    /// nothing is queued yet; a closed peer with an empty queue is an error.
    pub fn try_recv(&self, max_bytes: usize, max_handles: usize) -> Result<Option<Message>, Error> {
        let mut state = self.state.lock();
        match state.queue.front() {
            Some(m) if m.bytes.len() > max_bytes || m.handles.len() > max_handles => {
                Err(Error::BufferTooSmall)
            }
            Some(_) => Ok(state.queue.pop_front()),
            None if state.peer_closed => Err(Error::PeerClosed),
            None => Ok(None),
        }
    }

    /// Like [`try_recv`](Self::try_recv), waiting until `deadline` (a tick
    /// count; `None` waits forever) for a message to arrive.
    pub fn recv(
        &self,
        max_bytes: usize,
        max_handles: usize,
        deadline: Option<u64>,
    ) -> Result<Message, Error> {
        loop {
            if let Some(m) = self.try_recv(max_bytes, max_handles)? {
                return Ok(m);
            }
            let arrived = self.readable.wait_while_until(
                || {
                    let s = self.state.lock();
                    s.queue.is_empty() && !s.peer_closed
                },
                deadline,
            );
            if !arrived {
                return Err(Error::TimedOut);
            }
        }
    }

    /// Signals a port would report.
    pub fn signals(&self) -> u32 {
        let s = self.state.lock();
        let mut bits = 0;
        if !s.queue.is_empty() {
            bits |= signals::READABLE;
        }
        if s.peer_closed {
            bits |= signals::PEER_CLOSED;
        }
        bits
    }

    pub fn pending(&self) -> usize {
        self.state.lock().queue.len()
    }

    pub fn peer_closed(&self) -> bool {
        self.state.lock().peer_closed
    }

    /// Registers a port to be woken whenever this endpoint changes.
    pub fn observe(&self, port: Weak<Port>) {
        self.state.lock().observers.push(port);
    }

    fn notify(&self) {
        self.readable.wake_all();
        let observers: Vec<Weak<Port>> = self.state.lock().observers.clone();
        for o in observers {
            if let Some(port) = o.upgrade() {
                port.notify();
            }
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if let Some(peer) = self.peer.lock().upgrade() {
            peer.state.lock().peer_closed = true;
            peer.notify();
        }
    }
}

#[cfg(feature = "ktest")]
mod tests {
    use super::*;
    use crate::{sched, time};

    fn text(s: &str) -> Message {
        Message {
            bytes: s.as_bytes().to_vec(),
            handles: Vec::new(),
        }
    }

    crate::ktest! {
        fn messages_arrive_in_order_and_closing_is_visible() {
            let (a, b) = Endpoint::pair();
            a.send(text("one")).expect("send");
            a.send(text("two")).expect("send");
            assert_eq!(b.signals(), signals::READABLE);
            assert_eq!(b.pending(), 2);
            assert!(
                matches!(b.try_recv(1, 0), Err(Error::BufferTooSmall)),
                "too small leaves it queued"
            );
            let one = b.try_recv(64, 0).expect("recv").expect("queued");
            assert_eq!(one.bytes, b"one");
            let two = b.recv(64, 0, Some(time::ticks())).expect("recv");
            assert_eq!(two.bytes, b"two");
            assert!(matches!(b.try_recv(64, 0), Ok(None)));
            drop(a);
            assert_eq!(b.signals(), signals::PEER_CLOSED);
            assert!(matches!(b.try_recv(64, 0), Err(Error::PeerClosed)));
            assert_eq!(b.send(text("x")).err(), Some(Error::PeerClosed));
        }
    }

    crate::ktest! {
        fn recv_times_out_when_nothing_arrives() {
            let (_a, b) = Endpoint::pair();
            let start = time::now_ns();
            let deadline = time::ticks() + 3;
            assert_eq!(b.recv(64, 0, Some(deadline)).err(), Some(Error::TimedOut));
            assert!(time::now_ns() - start >= 20_000_000, "waited at least two ticks");
        }
    }

    static LATE_SENDER: SpinLock<Option<Arc<Endpoint>>> = SpinLock::new(None);

    fn send_after_a_while(_: usize) {
        time::sleep_ms(20);
        if let Some(ep) = LATE_SENDER.lock().take() {
            ep.send(text("late")).expect("send");
        }
    }

    crate::ktest! {
        fn blocking_recv_is_woken_by_a_later_send() {
            let (a, b) = Endpoint::pair();
            *LATE_SENDER.lock() = Some(a);
            let id = sched::spawn("late-sender", send_after_a_while, 0);
            let got = b.recv(64, 0, None).expect("recv");
            assert_eq!(got.bytes, b"late");
            sched::join(id);
        }
    }
}
