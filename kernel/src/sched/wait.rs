//! Wait queues: block until someone says a condition may have changed,
//! optionally giving up at a deadline.
//!
//! `wait_while` checks the condition, enqueues and blocks with interrupts
//! off, so on one hart a wake-up cannot slip in between the check and the
//! block. SMP will need the check to happen under the queue's lock.

use alloc::vec::Vec;

use super::{Next, ThreadId, current_id, schedule, wake};
use crate::sync::SpinLock;
use crate::{arch, time};

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
    pub fn wait_while(&self, condition: impl FnMut() -> bool) {
        self.wait_while_until(condition, None);
    }

    /// Blocks while `condition` holds or until the tick count reaches
    /// `deadline`. Returns `false` if it gave up with the condition still
    /// true.
    pub fn wait_while_until(
        &self,
        mut condition: impl FnMut() -> bool,
        deadline: Option<u64>,
    ) -> bool {
        let interrupts_were_enabled = arch::interrupts::disable();
        let me = current_id();
        let mut cleared = true;
        while condition() {
            if deadline.is_some_and(|d| time::ticks() >= d) {
                cleared = false;
                break;
            }
            self.waiters.lock().push(me);
            schedule(Next::Block { until: deadline });
            // Whether woken or timed out, we are no longer waiting here.
            self.waiters.lock().retain(|&id| id != me);
        }
        arch::interrupts::restore(interrupts_were_enabled);
        cleared
    }

    /// Readies every waiter. They re-check their conditions.
    pub fn wake_all(&self) {
        let ids = core::mem::take(&mut *self.waiters.lock());
        for id in ids {
            wake(id);
        }
    }

    /// Readies the longest-waiting thread, if any.
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

    crate::ktest! {
        fn wait_with_deadline_gives_up() {
            let queue = WaitQueue::new();
            let deadline = time::ticks() + 2;
            let cleared = queue.wait_while_until(|| true, Some(deadline));
            assert!(!cleared);
            assert!(time::ticks() >= deadline);
        }
    }
}
