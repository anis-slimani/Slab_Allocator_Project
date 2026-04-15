//! Simple spinlock — no external dependencies.
//!
//! Needed because `GlobalAlloc::alloc` takes `&self` but our allocator
//! needs `&mut self`. The `Mutex` here provides safe interior mutability.

use core::{
    cell::UnsafeCell,
    hint,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

/// A spinlock-based mutual-exclusion primitive.
///
/// Spins until the lock is free, then acquires it atomically.
/// The lock is released when the [`MutexGuard`] is dropped.
///
/// # Examples
///
/// ```
/// use slab_allocator::spinlock::Mutex;
///
/// let m = Mutex::new(0u32);
/// *m.lock() += 1;
/// assert_eq!(*m.lock(), 1);
/// ```
pub struct Mutex<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: access to T is serialised by the spinlock.
unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    /// Create a new unlocked `Mutex`. `const fn` so it works in statics.
    ///
    /// # Examples
    ///
    /// ```
    /// use slab_allocator::spinlock::Mutex;
    ///
    /// static COUNTER: Mutex<u64> = Mutex::new(0);
    /// ```
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    /// Acquire the lock, spinning until it becomes available.
    ///
    /// # Examples
    ///
    /// ```
    /// use slab_allocator::spinlock::Mutex;
    ///
    /// let m = Mutex::new(42u32);
    /// {
    ///     let mut guard = m.lock();
    ///     *guard = 100;
    /// } // lock released here
    /// assert_eq!(*m.lock(), 100);
    /// ```
    pub fn lock(&self) -> MutexGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            hint::spin_loop();
        }
        MutexGuard { mutex: self }
    }
}

/// RAII guard — releases the lock on drop.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: we hold the lock.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: we hold the lock.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.locked.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn lock_and_mutate() {
        let m = Mutex::new(0u32);
        *m.lock() = 42;
        assert_eq!(*m.lock(), 42);
    }

    #[test]
    fn const_in_static() {
        static M: Mutex<u64> = Mutex::new(0);
        *M.lock() += 1;
        assert_eq!(*M.lock(), 1);
    }

    #[test]
    fn guard_releases_on_drop() {
        let m = Mutex::new(false);
        {
            let mut g = m.lock();
            *g = true;
        }
        assert!(*m.lock());
    }
}
