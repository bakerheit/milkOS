//! [`Lock`]: data behind one of the platform's locks ([`Platform::new_lock`]), with a guard.
//! In the kernel a [`LockKind::Sleeping`] lock is the scheduler's sleeping mutex and a
//! [`LockKind::Spin`] one its IRQ-masking spinlock, exactly what this code used before it
//! moved out of the kernel.
//!
//! [`Platform::new_lock`]: crate::platform::Platform::new_lock

use alloc::boxed::Box;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};

use crate::platform::{LockKind, RawLock, platform};

pub struct Lock<T> {
    raw: Box<dyn RawLock>,
    data: UnsafeCell<T>,
}

// SAFETY: access to `data` is serialised by `raw`.
unsafe impl<T: Send> Sync for Lock<T> {}
unsafe impl<T: Send> Send for Lock<T> {}

pub struct LockGuard<'a, T> {
    lock: &'a Lock<T>,
}

impl<T> Lock<T> {
    pub fn new(kind: LockKind, data: T) -> Lock<T> {
        Lock {
            raw: platform().new_lock(kind),
            data: UnsafeCell::new(data),
        }
    }

    /// A lock that may be held across disk I/O.
    pub fn sleeping(data: T) -> Lock<T> {
        Self::new(LockKind::Sleeping, data)
    }

    /// A lock for short critical sections without I/O.
    pub fn spin(data: T) -> Lock<T> {
        Self::new(LockKind::Spin, data)
    }

    pub fn lock(&self) -> LockGuard<'_, T> {
        self.raw.lock();
        LockGuard { lock: self }
    }
}

impl<T> Deref for LockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: we hold the lock.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for LockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: we hold the lock.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for LockGuard<'_, T> {
    fn drop(&mut self) {
        // SAFETY: this guard is the proof we hold it.
        unsafe { self.lock.raw.unlock() }
    }
}
