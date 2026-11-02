//! Console input: bytes the UART receive interrupt collected, waiting for
//! whoever wants them (a shell, from M5).

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sync::SpinLock;

const CAPACITY: usize = 256;

struct Ring {
    buf: [u8; CAPACITY],
    head: usize,
    len: usize,
}

static INPUT: SpinLock<Ring> = SpinLock::new(Ring {
    buf: [0; CAPACITY],
    head: 0,
    len: 0,
});

/// Bytes ever received, including any dropped because the ring was full.
static RECEIVED: AtomicUsize = AtomicUsize::new(0);

/// Called from the receive interrupt. Drops the byte if the ring is full.
pub fn push(byte: u8) {
    RECEIVED.fetch_add(1, Ordering::Relaxed);
    let mut ring = INPUT.lock();
    if ring.len == CAPACITY {
        return;
    }
    let at = (ring.head + ring.len) % CAPACITY;
    ring.buf[at] = byte;
    ring.len += 1;
}

/// The oldest unread byte, if any.
pub fn pop() -> Option<u8> {
    let mut ring = INPUT.lock();
    if ring.len == 0 {
        return None;
    }
    let byte = ring.buf[ring.head];
    ring.head = (ring.head + 1) % CAPACITY;
    ring.len -= 1;
    Some(byte)
}

/// Total bytes received since boot.
pub fn received() -> usize {
    RECEIVED.load(Ordering::Relaxed)
}

crate::ktest! {
    fn ring_is_fifo_and_bounded() {
        while pop().is_some() {}
        let before = received();
        for b in 0..=255u8 {
            push(b);
        }
        push(1);
        push(2);
        assert_eq!(received(), before + 258);
        for b in 0..=255u8 {
            assert_eq!(pop(), Some(b));
        }
        assert_eq!(pop(), None, "overflow bytes are dropped, not wrapped");
    }
}
