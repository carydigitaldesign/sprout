//! Wait queues: block until someone says a condition may have changed.
//!
//! `wait_while` checks the condition, enqueues and blocks with interrupts
//! off, so on one hart a wake-up cannot slip in between the check and the
//! block. SMP will need the check to happen under the queue's lock.

use alloc::vec::Vec;

use super::{Next, ThreadId, current_id, schedule, wake};
use crate::arch;
use crate::sync::SpinLock;

pub struct WaitQueue {
    waiters: SpinLock<Vec<ThreadId>>,
}

impl WaitQueue {
    pub const fn new() -> WaitQueue {
        WaitQueue {
            waiters: SpinLock::new(Vec::new()),
        }
    }

    /// Blocks while `condition` holds, re-checking after every wake-up.
    pub fn wait_while(&self, mut condition: impl FnMut() -> bool) {
        let interrupts_were_enabled = arch::interrupts::disable();
        while condition() {
            self.waiters.lock().push(current_id());
            schedule(Next::Block);
        }
        arch::interrupts::restore(interrupts_were_enabled);
    }

    /// Readies every waiter. They re-check their conditions.
    pub fn wake_all(&self) {
        let ids = core::mem::take(&mut *self.waiters.lock());
        for id in ids {
            wake(id);
        }
    }

    /// Readies the longest-waiting thread, if any. Channels use this at M5.
    #[allow(dead_code)]
    pub fn wake_one(&self) {
        let id = {
            let mut waiters = self.waiters.lock();
            if waiters.is_empty() {
                None
            } else {
                Some(waiters.remove(0))
            }
        };
        if let Some(id) = id {
            wake(id);
        }
    }
}

#[cfg(feature = "ktest")]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::sched::{join, spawn, yield_now};

    static READY: AtomicUsize = AtomicUsize::new(0);
    static QUEUE: WaitQueue = WaitQueue::new();
    static WOKE: AtomicUsize = AtomicUsize::new(0);

    fn waiter(_: usize) {
        QUEUE.wait_while(|| READY.load(Ordering::Relaxed) == 0);
        WOKE.fetch_add(1, Ordering::Relaxed);
    }

    crate::ktest! {
        fn wait_queue_blocks_until_woken() {
            READY.store(0, Ordering::Relaxed);
            WOKE.store(0, Ordering::Relaxed);
            let a = spawn("waiter-a", waiter, 0);
            let b = spawn("waiter-b", waiter, 0);
            for _ in 0..5 {
                yield_now();
            }
            assert_eq!(WOKE.load(Ordering::Relaxed), 0, "waiters ran past the condition");
            READY.store(1, Ordering::Relaxed);
            QUEUE.wake_all();
            join(a);
            join(b);
            assert_eq!(WOKE.load(Ordering::Relaxed), 2);
        }
    }
}
