//! Blocking primitives on top of the scheduler: wait queues, a sleeping mutex, a counting
//! semaphore.
//!
//! No lost wakeups, on any number of CPUs: a waiter first puts itself on the queue, then
//! checks its condition, then blocks. A waker changes the condition, then pops a waiter
//! and calls `sched::wake`. If that wake lands before the waiter has actually blocked, the
//! scheduler records it (`wake_pending`) and the block returns at once. Wakeups can be
//! spurious; `cond` decides.

use alloc::collections::VecDeque;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

use super::Tid;
use crate::sync::{self, SpinLock};

/// Threads waiting for something.
pub struct WaitQueue {
    q: SpinLock<VecDeque<Tid>>,
}

impl WaitQueue {
    pub const fn new() -> Self {
        Self {
            q: SpinLock::new(VecDeque::new()),
        }
    }

    /// Block until `cond()` is true. `cond` runs with IRQs masked: it may take spinlocks,
    /// it must not block. Must be called from thread context (not an IRQ handler).
    pub fn wait_until(&self, mut cond: impl FnMut() -> bool) {
        let flags = sync::irq_save();
        while !cond() {
            if !super::started() {
                // Before the scheduler there is nobody to wait for but IRQs.
                sync::irq_restore(flags);
                // SAFETY: wait for an interrupt.
                unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
                sync::irq_disable();
                continue;
            }
            let me = self.enqueue();
            if cond() {
                self.remove(me);
                break;
            }
            super::block_current();
        }
        sync::irq_restore(flags);
    }

    /// [`wait_until`](Self::wait_until) with a timeout: returns true if `cond()` became
    /// true, false if `timeout_ns` passed first. Wakeups can be spurious; `cond` decides.
    pub fn wait_until_timeout(&self, mut cond: impl FnMut() -> bool, timeout_ns: u64) -> bool {
        let deadline = crate::timer::now_ns().saturating_add(timeout_ns);
        let flags = sync::irq_save();
        let ok = loop {
            if cond() {
                break true;
            }
            if crate::timer::now_ns() >= deadline {
                if super::started() {
                    self.remove(super::current_tid()); // no stale entry left behind
                }
                break false;
            }
            if !super::started() {
                sync::irq_restore(flags);
                // SAFETY: wait for an interrupt.
                unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
                sync::irq_disable();
                continue;
            }
            let me = self.enqueue();
            if cond() {
                self.remove(me);
                break true;
            }
            super::block_current_until(deadline);
        };
        sync::irq_restore(flags);
        ok
    }

    /// Put the calling thread on the queue (once).
    fn enqueue(&self) -> super::Tid {
        let me = super::current_tid();
        let mut q = self.q.lock();
        if !q.contains(&me) {
            q.push_back(me);
        }
        me
    }

    fn remove(&self, tid: super::Tid) {
        self.q.lock().retain(|&t| t != tid);
    }

    /// Wake the longest waiter. Returns false if nobody was waiting. IRQ-safe.
    pub fn wake_one(&self) -> bool {
        loop {
            let Some(t) = self.q.lock().pop_front() else {
                return false;
            };
            if super::wake(t) {
                return true;
            }
        }
    }

    /// Wake every waiter. IRQ-safe.
    pub fn wake_all(&self) {
        while self.wake_one() {}
    }

    pub fn has_waiters(&self) -> bool {
        !self.q.lock().is_empty()
    }
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// A mutex that puts contending threads to sleep. Thread context only (not in IRQs, and
/// not while holding a SpinLock).
pub struct Mutex<T: ?Sized> {
    locked: AtomicBool,
    wq: WaitQueue,
    data: UnsafeCell<T>,
}

// SAFETY: access to `data` is serialised by `locked`.
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}

pub struct MutexGuard<'a, T: ?Sized> {
    m: &'a Mutex<T>,
}

impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            wq: WaitQueue::new(),
            data: UnsafeCell::new(data),
        }
    }

    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }
}

impl<T: ?Sized> Mutex<T> {
    fn try_acquire(&self) -> bool {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        if !self.try_acquire() {
            self.wq.wait_until(|| self.try_acquire());
        }
        MutexGuard { m: self }
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.try_acquire().then_some(MutexGuard { m: self })
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: we hold the lock.
        unsafe { &*self.m.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: we hold the lock.
        unsafe { &mut *self.m.data.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.m.locked.store(false, Ordering::Release);
        self.m.wq.wake_one();
        super::preempt_point();
    }
}

/// Counting semaphore.
pub struct Semaphore {
    count: SpinLock<usize>,
    wq: WaitQueue,
}

impl Semaphore {
    pub const fn new(count: usize) -> Self {
        Self {
            count: SpinLock::new(count),
            wq: WaitQueue::new(),
        }
    }

    /// P / down: take one unit, blocking until one is available.
    pub fn acquire(&self) {
        self.wq.wait_until(|| {
            let mut c = self.count.lock();
            if *c > 0 {
                *c -= 1;
                true
            } else {
                false
            }
        });
    }

    pub fn try_acquire(&self) -> bool {
        let mut c = self.count.lock();
        if *c > 0 {
            *c -= 1;
            true
        } else {
            false
        }
    }

    /// V / up: return one unit. IRQ-safe.
    pub fn release(&self) {
        *self.count.lock() += 1;
        self.wq.wake_one();
    }

    pub fn count(&self) -> usize {
        *self.count.lock()
    }
}
