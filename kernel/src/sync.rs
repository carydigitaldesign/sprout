//! A spinlock that masks interrupts while held.
//!
//! Masking is what makes it safe to take the same lock from a thread and
//! from an interrupt handler on the same hart: the handler cannot run while
//! the thread holds the lock. A lock that spins for too long panics rather
//! than hanging silently, which on one hart means "someone locked it twice".

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::interrupts;

/// Spins this many times before deciding the lock is never coming back.
const STUCK_AFTER: u32 = 100_000_000;

/// Mutual exclusion by spinning, with interrupts off while held.
pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: the lock hands out at most one `Guard` at a time, so `&SpinLock<T>`
// can be shared across harts as long as `T` itself can be sent between them.
unsafe impl<T: Send> Sync for SpinLock<T> {}

/// Exclusive access to the data; releases the lock and restores the
/// interrupt state when dropped.
pub struct Guard<'a, T> {
    lock: &'a SpinLock<T>,
    interrupts_were_enabled: bool,
}

impl<T> SpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    /// Acquires the lock, spinning until it is free. Interrupts stay off
    /// until the guard is dropped.
    pub fn lock(&self) -> Guard<'_, T> {
        let interrupts_were_enabled = interrupts::disable();
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
        Guard {
            lock: self,
            interrupts_were_enabled,
        }
    }

    /// Releases the lock without a guard. For the panic path only.
    ///
    /// # Safety
    /// Any live guard becomes a lie. Call this only when the holder can
    /// never run again, such as from the panic handler on its way out.
    pub unsafe fn force_unlock(&self) {
        self.locked.store(false, Ordering::Release);
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
        interrupts::restore(self.interrupts_were_enabled);
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

crate::ktest! {
    fn spinlock_masks_interrupts_while_held() {
        let outer = SpinLock::new(());
        let inner = SpinLock::new(());
        interrupts::enable();
        {
            let _a = outer.lock();
            assert!(!interrupts::enabled(), "interrupts must be off inside the lock");
            {
                let _b = inner.lock();
            }
            assert!(!interrupts::enabled(), "a nested lock must not re-enable them");
        }
        assert!(interrupts::enabled(), "dropping the guard restores them");
    }
}
