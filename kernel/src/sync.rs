//! Interrupt masking and locking (SMP).
//!
//! [`SpinLock`] masks IRQs on the local CPU and spins on an atomic compare-exchange
//! against other CPUs. The `atomic-locks` cargo feature is gone: this is the only
//! implementation now (the MMU is on everywhere before any lock is taken).

// Public API surface for later subsystems (scheduler, drivers); not all of it is used yet.
#![allow(dead_code)]

use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::{size_of, transmute_copy};
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// PSTATE.I (IRQ mask) as it appears in DAIF.
const DAIF_I: u64 = 1 << 7;

/// Unmask IRQs on this core.
#[inline]
pub fn irq_enable() {
    unsafe { asm!("msr daifclr, #2", options(nomem, nostack)) };
}

/// Mask IRQs on this core.
#[inline]
pub fn irq_disable() {
    unsafe { asm!("msr daifset, #2", options(nomem, nostack)) };
}

/// Are IRQs currently unmasked?
#[inline]
pub fn irqs_enabled() -> bool {
    daif() & DAIF_I == 0
}

#[inline]
fn daif() -> u64 {
    let v: u64;
    unsafe { asm!("mrs {}, daif", out(reg) v, options(nomem, nostack)) };
    v
}

/// Mask IRQs and return the previous DAIF so it can be put back with [`irq_restore`].
#[inline]
pub fn irq_save() -> u64 {
    let flags = daif();
    irq_disable();
    flags
}

/// Restore the IRQ mask state returned by [`irq_save`]. Only touches the I bit.
#[inline]
pub fn irq_restore(flags: u64) {
    if flags & DAIF_I == 0 {
        irq_enable();
    }
}

/// RAII critical section: IRQs are masked while this lives, and put back to whatever they
/// were (so guards nest correctly) when it drops.
pub struct IrqGuard {
    flags: u64,
}

impl IrqGuard {
    #[inline]
    pub fn new() -> Self {
        Self { flags: irq_save() }
    }
}

impl Default for IrqGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for IrqGuard {
    #[inline]
    fn drop(&mut self) {
        irq_restore(self.flags);
    }
}

/// Run `f` with IRQs masked.
#[inline]
pub fn without_irqs<R>(f: impl FnOnce() -> R) -> R {
    let _g = IrqGuard::new();
    f()
}

/// A lock that is safe to take from both thread context and IRQ handlers: holding it keeps
/// IRQs masked on this CPU (so a holder is never preempted or interrupted), and other CPUs
/// spin on an atomic compare-exchange. The word holds the owner's CPU index + 1, so taking
/// a lock this CPU already holds is reported as a deadlock panic instead of spinning
/// forever. Must not be used before the MMU is on (exclusives hang on Device memory);
/// nothing is: `mem::init` turns the MMU on before the first lock.
pub struct SpinLock<T> {
    owner: AtomicU32,
    data: UnsafeCell<T>,
}

// SAFETY: access to `data` is serialised by `owner`.
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
    _irq: IrqGuard,
}

impl<T> SpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            owner: AtomicU32::new(0),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        let irq = IrqGuard::new(); // IRQs off first, then take the lock.
        self.acquire();
        SpinLockGuard {
            lock: self,
            _irq: irq,
        }
    }

    /// Take the lock without a guard, IRQs masked first as in [`SpinLock::lock`], for a raw
    /// lock interface (milkfs's `RawLock`). Returns the IRQ state for [`SpinLock::raw_unlock`].
    pub fn raw_lock(&self) -> u64 {
        let flags = irq_save();
        self.acquire();
        flags
    }

    /// Release a lock taken with [`SpinLock::raw_lock`] and put the IRQ mask back.
    ///
    /// # Safety
    /// The caller holds the lock (from `raw_lock`) and `flags` is what that returned.
    pub unsafe fn raw_unlock(&self, flags: u64) {
        self.release();
        irq_restore(flags);
    }

    /// Try to take the lock without blocking.
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        let irq = IrqGuard::new();
        if !self.try_acquire() {
            return None; // `irq` drops and restores the mask.
        }
        Some(SpinLockGuard {
            lock: self,
            _irq: irq,
        })
    }

    #[inline]
    fn me() -> u32 {
        crate::percpu::id() as u32 + 1
    }

    #[inline]
    fn try_acquire(&self) -> bool {
        self.owner
            .compare_exchange(0, Self::me(), Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    #[inline]
    fn acquire(&self) {
        let me = Self::me();
        loop {
            match self
                .owner
                .compare_exchange_weak(0, me, Ordering::Acquire, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(o) if o == me => panic!("SpinLock deadlock: already held by this CPU"),
                Err(_) => {}
            }
            // Spin on a plain load so we don't hammer the line with exclusives.
            while self.owner.load(Ordering::Relaxed) != 0 {
                core::hint::spin_loop();
            }
        }
    }

    #[inline]
    fn release(&self) {
        self.owner.store(0, Ordering::Release);
    }

    #[cfg(feature = "raspi")]
    /// The protected data without taking the lock, for a crash path that must read it even if the
    /// crashed context holds the lock (the boot log ring: bytes in an array, a racing writer can at worst
    /// tear a line).
    ///
    /// # Safety
    /// The caller must only read plain data that stays valid under a racing writer, and never use the
    /// pointer for anything that needs the lock's exclusion.
    pub unsafe fn data_unchecked(&self) -> *const T {
        self.data.get()
    }

    /// Is the lock held (by anyone)? For crash dumps and assertions.
    pub fn is_locked(&self) -> bool {
        self.owner.load(Ordering::Relaxed) != 0
    }
}

impl<T> Deref for SpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.release();
        // `_irq` drops after this, restoring the IRQ mask.
    }
}

/// An atomically swappable function pointer (or any pointer-sized `Copy` handle) slot.
/// Empty until `set`. Plain load/store only, so it works before the MMU is on.
pub struct FnSlot(AtomicUsize);

impl FnSlot {
    pub const fn new() -> Self {
        Self(AtomicUsize::new(0))
    }

    /// Store `f`. `F` must be a pointer-sized, non-null `fn` pointer type.
    pub fn set<F: Copy>(&self, f: F) {
        assert!(size_of::<F>() == size_of::<usize>());
        // SAFETY: size checked above; fn pointers are plain addresses.
        let raw: usize = unsafe { transmute_copy(&f) };
        self.0.store(raw, Ordering::Release);
    }

    pub fn clear(&self) {
        self.0.store(0, Ordering::Release);
    }

    pub fn get<F: Copy>(&self) -> Option<F> {
        assert!(size_of::<F>() == size_of::<usize>());
        let raw = self.0.load(Ordering::Acquire);
        if raw == 0 {
            None
        } else {
            // SAFETY: only ever filled by `set::<F>` with the same F by convention.
            Some(unsafe { transmute_copy(&raw) })
        }
    }
}

impl Default for FnSlot {
    fn default() -> Self {
        Self::new()
    }
}
