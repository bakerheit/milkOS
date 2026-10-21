//! Futexes (fast userspace mutexes): the kernel side of `futex(2)`, enough for a
//! pthread-style mutex and condition variable in `user/rt`.
//!
//! Supported: `FUTEX_WAIT` / `FUTEX_WAKE` and their `_PRIVATE` variants (the private flag
//! only hints that the futex isn't shared between processes; we handle both the same way).
//! A waiter is keyed by the **physical address** the user virtual address maps to, so a
//! futex in memory two threads share — or two processes share, via a shared mapping — lines
//! up regardless of each side's virtual address.
//!
//! Wait is race free the same way the wait queues are: the waiter takes the futex lock,
//! re-reads `*uaddr`, enqueues itself, drops the lock, then blocks; a waker takes the lock,
//! pops waiters and calls `sched::wake`, which on a thread that hasn't blocked yet records
//! a pending wake so its block returns at once.

use alloc::vec::Vec;

use super::errno::*;
use crate::sched::{self, Tid};
use crate::sync::SpinLock;

pub const FUTEX_WAIT: u32 = 0;
pub const FUTEX_WAKE: u32 = 1;
pub const FUTEX_PRIVATE_FLAG: u32 = 128;
pub const FUTEX_CLOCK_REALTIME: u32 = 256;
const CMD_MASK: u32 = !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME);
/// Mask to test whether an op is FUTEX_WAIT (so the syscall layer reads the timeout).
pub const CMD_WAIT_MASK: u32 = CMD_MASK;

struct Waiter {
    key: u64,
    tid: Tid,
    /// Cleared by a waker; the waiter wakes and stops re-blocking.
    waiting: bool,
}

static WAITERS: SpinLock<Vec<Waiter>> = SpinLock::new(Vec::new());

/// Physical address `uaddr` maps to in the current process (its futex key).
fn key_of(uaddr: usize) -> KResult<u64> {
    if uaddr & 3 != 0 {
        return Err(EINVAL); // futex words are 4-byte aligned
    }
    super::current()
        .inner
        .lock()
        .space
        .as_ref()
        .ok_or(ESRCH)?
        .translate(uaddr)
        .map(|pa| pa as u64)
        .ok_or(EFAULT)
}

fn read_u32(uaddr: usize) -> KResult<u32> {
    let mut b = [0u8; 4];
    super::current()
        .inner
        .lock()
        .space
        .as_ref()
        .ok_or(ESRCH)?
        .copy_in(uaddr, &mut b)?;
    Ok(u32::from_le_bytes(b))
}

/// `futex(uaddr, op, val, timeout_or_val2, ...)`. Returns the number woken for WAKE, 0 for
/// a completed WAIT.
pub fn futex(uaddr: usize, op: u32, val: u32, timeout_ns: Option<u64>) -> KResult<usize> {
    match op & CMD_MASK {
        FUTEX_WAIT => wait(uaddr, val, timeout_ns),
        FUTEX_WAKE => Ok(wake(uaddr, val as usize)?),
        _ => Err(ENOSYS),
    }
}

fn wait(uaddr: usize, val: u32, timeout_ns: Option<u64>) -> KResult<usize> {
    let key = key_of(uaddr)?;
    let me = sched::current_tid();
    // Enqueue, then check *uaddr under the lock, so a wake that changes the word and wakes
    // can't slip between the check and the enqueue.
    {
        let mut w = WAITERS.lock();
        if read_u32(uaddr)? != val {
            return Err(EAGAIN);
        }
        w.push(Waiter {
            key,
            tid: me,
            waiting: true,
        });
    }
    let deadline = timeout_ns.map(|ns| crate::timer::now_ns().saturating_add(ns));
    let mut timed_out = false;
    loop {
        let (still, interrupted) = (
            WAITERS.lock().iter().any(|x| x.tid == me && x.waiting),
            super::signal::interrupted(),
        );
        if !still {
            break; // woken by FUTEX_WAKE
        }
        if interrupted {
            timed_out = false;
            break;
        }
        if let Some(d) = deadline
            && crate::timer::now_ns() >= d
        {
            timed_out = true;
            break;
        }
        let _g = crate::sync::IrqGuard::new();
        // Re-check under IRQ-off so a wake between the checks above and the block isn't
        // lost (sched::wake records a pending wake on a running thread).
        if WAITERS.lock().iter().any(|x| x.tid == me && x.waiting) {
            match deadline {
                Some(d) => sched::block_current_until(d),
                None => sched::block_current(),
            }
        }
    }
    WAITERS.lock().retain(|x| x.tid != me);
    if super::signal::interrupted() && !timed_out {
        return Err(EINTR);
    }
    if timed_out {
        return Err(Errno(110)); // ETIMEDOUT
    }
    Ok(0)
}

fn wake(uaddr: usize, n: usize) -> KResult<usize> {
    let key = key_of(uaddr)?;
    let mut woken = Vec::new();
    {
        let mut w = WAITERS.lock();
        for waiter in w.iter_mut().filter(|x| x.key == key && x.waiting).take(n) {
            waiter.waiting = false;
            woken.push(waiter.tid);
        }
    }
    for tid in &woken {
        sched::wake(*tid);
    }
    Ok(woken.len())
}

/// A thread exiting: wake anything waiting on it (CLONE_CHILD_CLEARTID / clear_child_tid).
pub fn wake_addr(uaddr: usize) {
    let _ = wake(uaddr, i32::MAX as usize);
}
