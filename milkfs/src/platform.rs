//! What milkfs needs from the system it runs in, besides a [`BlockDevice`]: the time, random
//! bytes, scrypt's memory, a log, and locks. The kernel implements [`Platform`] once
//! (`kernel/src/fs/milkfs/platform.rs`) and installs it with [`set_platform`] before the
//! first volume is touched; the host tests install theirs (fake clock, seeded RNG).
//!
//! [`BlockDevice`]: crate::blockdev::BlockDevice

use alloc::boxed::Box;
use core::fmt;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::abi::Timespec;

/// A lock's raw operations. Built by [`Platform::new_lock`]; [`crate::sync::Lock`] puts the
/// data and a guard around it.
pub trait RawLock: Send + Sync {
    fn lock(&self);
    /// Release it.
    ///
    /// # Safety
    /// The caller holds the lock (took it with [`RawLock::lock`] and hasn't released it).
    unsafe fn unlock(&self);
}

/// The kind of lock a [`crate::sync::Lock`] needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LockKind {
    /// May be held across disk I/O (which sleeps): the kernel's sleeping mutex.
    Sleeping,
    /// Short critical sections, no I/O: the kernel's spinlock.
    Spin,
}

pub trait Platform: Sync {
    /// Wall-clock time for timestamps (Unix seconds; seconds since boot without an RTC).
    fn now(&self) -> Timespec;
    /// Monotonic nanoseconds (commit batching, timings).
    fn now_ns(&self) -> u64;
    /// A free-running counter (seeds volume UUIDs).
    fn counter(&self) -> u64;
    /// Cryptographically random bytes (volume keys, salts, nonces).
    fn fill_random(&self, buf: &mut [u8]);
    /// scrypt(pass, salt, 2^log_n, r, p) into `out` (`milkcrypto::scrypt`, with its big
    /// array wherever the platform wants it). None: bad parameters or no memory.
    fn scrypt(
        &self,
        pass: &[u8],
        salt: &[u8],
        log_n: u8,
        r: u32,
        p: u32,
        out: &mut [u8],
    ) -> Option<()>;
    /// One line of log (no trailing newline in `args`).
    fn log(&self, args: fmt::Arguments<'_>);
    /// A new, unlocked lock of `kind`.
    fn new_lock(&self, kind: LockKind) -> Box<dyn RawLock>;
}

static PLATFORM: AtomicPtr<&'static dyn Platform> = AtomicPtr::new(core::ptr::null_mut());

/// Install the platform (once, before any volume is touched; installing again replaces it).
pub fn set_platform(p: &'static &'static dyn Platform) {
    PLATFORM.store(p as *const _ as *mut _, Ordering::Release);
}

/// The installed platform. Panics if there is none (a host forgot [`set_platform`]).
pub fn platform() -> &'static dyn Platform {
    let p = PLATFORM.load(Ordering::Acquire);
    assert!(!p.is_null(), "milkfs: no platform installed (set_platform)");
    // SAFETY: only ever set from a `&'static &'static dyn Platform`.
    unsafe { *p }
}

/// Wall-clock time (see [`Platform::now`]).
pub fn now() -> Timespec {
    platform().now()
}

/// Monotonic nanoseconds (see [`Platform::now_ns`]).
pub fn now_ns() -> u64 {
    platform().now_ns()
}

/// Random bytes (see [`Platform::fill_random`]).
pub fn fill_random(buf: &mut [u8]) {
    platform().fill_random(buf)
}

/// Log a line through the platform: `log!("milkfs: {x}")`.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::platform::platform().log(format_args!($($arg)*))
    };
}
