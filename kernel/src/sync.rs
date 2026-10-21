//! Interrupt masking and locking for the (currently) single-core kernel.
//!
//! Everything here is written so SMP is a drop-in later: [`SpinLock`] already has the
//! final API (`lock()` returns a guard that derefs to the data and unlocks on drop). Today
//! the "spin" part is a single-core deadlock check; the one place to change for SMP is
//! [`SpinLock::acquire`] / [`SpinLock::release`].
//!
//! Note on atomics: until the MMU is on, RAM is Device memory, where exclusive
//! (ldxr/stxr, LSE) accesses are not guaranteed to work. So code in this kernel that has to
//! run before the MMU uses only plain atomic loads/stores (never `fetch_*`/`swap`/`cas`).
//! That is fine for single-writer state; real multi-writer atomics come with SMP + MMU.

// Public API surface for later subsystems (scheduler, drivers); not all of it is used yet.
#![allow(dead_code)]

use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::{size_of, transmute_copy};
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
/// IRQs masked on this core.
pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// SAFETY: access to `data` is serialised by `locked` (plus IRQ masking on one core).
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
    _irq: IrqGuard,
}

impl<T> SpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
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

    /// Try to take the lock without blocking.
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        let irq = IrqGuard::new();
        if self.locked.load(Ordering::Acquire) {
            return None; // `irq` drops and restores the mask.
        }
        self.locked.store(true, Ordering::Relaxed);
        Some(SpinLockGuard {
            lock: self,
            _irq: irq,
        })
    }

    // --- the SMP seam -----------------------------------------------------------------
    // Single core, IRQs masked: if the lock is already held, the holder is us further up the
    // stack, so spinning would hang forever. Fail loudly instead.
    // For SMP, replace with:
    //   while self.locked.compare_exchange_weak(false, true, Acquire, Relaxed).is_err() {
    //       core::hint::spin_loop();
    //   }
    // (needs the MMU on with cacheable memory so exclusives work).
    #[inline]
    fn acquire(&self) {
        if self.locked.load(Ordering::Acquire) {
            panic!("SpinLock deadlock: lock already held on this core");
        }
        self.locked.store(true, Ordering::Relaxed);
    }

    #[inline]
    fn release(&self) {
        self.locked.store(false, Ordering::Release);
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
