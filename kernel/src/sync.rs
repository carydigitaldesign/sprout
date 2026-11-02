//! A spinlock. The only lock the kernel has until M3 brings threads.
//!
//! Not yet interrupt-safe: interrupts are off for the whole of M2, and M3
//! will add interrupt masking inside `lock` when it turns them on. A lock
//! that spins for too long panics rather than hanging silently, which on a
//! single hart means "someone locked it twice".

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// Spins this many times before deciding the lock is never coming back.
const STUCK_AFTER: u32 = 100_000_000;

/// Mutual exclusion by spinning.
pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: the lock hands out at most one `Guard` at a time, so `&SpinLock<T>`
// can be shared across harts as long as `T` itself can be sent between them.
unsafe impl<T: Send> Sync for SpinLock<T> {}

/// Exclusive access to the data; releases the lock when dropped.
pub struct Guard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> SpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    /// Acquires the lock, spinning until it is free.
    pub fn lock(&self) -> Guard<'_, T> {
        let mut spins = 0u32;
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spins += 1;
            if spins == STUCK_AFTER {
                panic!("spinlock held for too long (double lock?)");
            }
            core::hint::spin_loop();
        }
        Guard { lock: self }
    }
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: holding the guard means holding the lock, so no other
        // reference to the data exists.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as above, and `&mut self` makes this the only access
        // through this guard.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

crate::ktest! {
    fn spinlock_guards_and_releases() {
        let lock = SpinLock::new(1u32);
        {
            let mut guard = lock.lock();
            *guard += 1;
        }
        assert_eq!(*lock.lock(), 2);
    }
}
