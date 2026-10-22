//! The kernel as milkfs's [`Platform`]: the RTC clock and the timer, the CSPRNG, scrypt in
//! page frames, `kprintln!`, and the kernel's own locks (a `sched::Mutex` for a lock held
//! across disk I/O, an IRQ-masking `SpinLock` for the rest). [`install`] runs first thing
//! in `fs::init`, before any volume is touched.

use alloc::boxed::Box;
use core::cell::UnsafeCell;
use core::fmt;

use ::milkfs::abi::Timespec;
use ::milkfs::platform::{LockKind, Platform, RawLock};

use crate::sched::Mutex;
use crate::sync::SpinLock;

struct Kernel;

impl Platform for Kernel {
    fn now(&self) -> Timespec {
        crate::fs::clock::now()
    }
    fn now_ns(&self) -> u64 {
        crate::timer::now_ns()
    }
    fn counter(&self) -> u64 {
        crate::timer::counter()
    }
    fn fill_random(&self, buf: &mut [u8]) {
        crate::crypto::rng::fill(buf)
    }
    fn scrypt(
        &self,
        pass: &[u8],
        salt: &[u8],
        log_n: u8,
        r: u32,
        p: u32,
        out: &mut [u8],
    ) -> Option<()> {
        crate::crypto::frames::scrypt(pass, salt, log_n, r, p, out)
    }
    fn log(&self, args: fmt::Arguments<'_>) {
        crate::kprintln!("{args}");
    }
    fn new_lock(&self, kind: LockKind) -> Box<dyn RawLock> {
        match kind {
            LockKind::Sleeping => Box::new(Sleeping(Mutex::new(()))),
            LockKind::Spin => Box::new(Spin {
                lock: SpinLock::new(()),
                flags: UnsafeCell::new(0),
            }),
        }
    }
}

/// `sched::Mutex` without its guard.
struct Sleeping(Mutex<()>);

impl RawLock for Sleeping {
    fn lock(&self) {
        core::mem::forget(self.0.lock());
    }
    unsafe fn unlock(&self) {
        // SAFETY: the caller holds it (its guard was forgotten in `lock`).
        unsafe { self.0.force_unlock() }
    }
}

/// `SpinLock` without its guard; the IRQ state to restore is kept beside it.
struct Spin {
    lock: SpinLock<()>,
    /// Written by the holder only.
    flags: UnsafeCell<u64>,
}

// SAFETY: `flags` is only touched by whoever holds `lock`.
unsafe impl Sync for Spin {}
unsafe impl Send for Spin {}

impl RawLock for Spin {
    fn lock(&self) {
        let f = self.lock.raw_lock();
        // SAFETY: we hold the lock now.
        unsafe { *self.flags.get() = f };
    }
    unsafe fn unlock(&self) {
        // SAFETY: the caller holds the lock, so `flags` is ours to read.
        unsafe {
            let f = *self.flags.get();
            self.lock.raw_unlock(f);
        }
    }
}

static KERNEL: &dyn Platform = &Kernel;

/// Make the kernel milkfs's platform.
pub fn install() {
    ::milkfs::platform::set_platform(&KERNEL);
}
