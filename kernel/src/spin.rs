#![allow(dead_code)]
//! A tiny interrupt-safe spinlock.
//!
//! Locking masks IRQs on the local core (so an interrupt handler can never
//! deadlock against code that holds the lock) and then spins on an atomic flag
//! (so it is also correct once other cores come up).
//!
//! Do NOT use this before the MMU is on: exclusive/atomic instructions on
//! Device memory (which is what everything is with the MMU off) are
//! implementation-defined and may fault. `mem::init` turns the MMU on before
//! anything takes a lock.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: access to `data` is serialised by `locked`.
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
    saved_daif: u64,
}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> SpinGuard<'_, T> {
        let saved_daif = irq_save();
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        SpinGuard {
            lock: self,
            saved_daif,
        }
    }
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: we hold the lock.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: we hold the lock.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        irq_restore(self.saved_daif);
    }
}

/// Mask IRQs and FIQs on this core, returning the previous DAIF value.
pub fn irq_save() -> u64 {
    let daif: u64;
    // SAFETY: only touches the DAIF mask bits.
    unsafe {
        core::arch::asm!("mrs {0}, daif", "msr daifset, #3", out(reg) daif, options(nomem, nostack));
    }
    daif
}

/// Restore a DAIF value returned by [`irq_save`].
pub fn irq_restore(daif: u64) {
    // SAFETY: restores the previously-saved mask bits.
    unsafe {
        core::arch::asm!("msr daif, {0}", in(reg) daif, options(nomem, nostack));
    }
}
