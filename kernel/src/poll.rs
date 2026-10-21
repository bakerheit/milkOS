//! The readiness core under poll(2), select(2) and epoll(7).
//!
//! Every object that can block a reader or writer answers `Inode::poll()` (Linux `POLL*`
//! bits) and, when its readiness may have changed, calls [`notify`]. That bumps one global
//! generation counter and wakes [`wait`]ers, which then re-check their fds. Objects that
//! don't notify (a few devices) are still seen: waiters re-check at least every
//! [`RECHECK_NS`].
//!
//! `Inode::poll_seq()` is a per-object event counter for edge-triggered epoll: a level
//! that stays "readable" but got new data changes the counter, so EPOLLET reports it again
//! (Linux reports every new arrival). The default is the global generation (never misses
//! an edge, can report extra ones).

use core::sync::atomic::{AtomicU64, Ordering};

use crate::sched::WaitQueue;

pub const POLLIN: u32 = 0x001;
#[allow(dead_code)]
pub const POLLPRI: u32 = 0x002;
pub const POLLOUT: u32 = 0x004;
pub const POLLERR: u32 = 0x008;
pub const POLLHUP: u32 = 0x010;
#[allow(dead_code)]
pub const POLLNVAL: u32 = 0x020;
pub const POLLRDNORM: u32 = 0x040;
pub const POLLWRNORM: u32 = 0x100;
pub const POLLRDHUP: u32 = 0x2000;

/// Waiters re-check every fd at least this often (for objects that never call `notify`).
pub const RECHECK_NS: u64 = 20_000_000;

static GEN: AtomicU64 = AtomicU64::new(1);
static WQ: WaitQueue = WaitQueue::new();

/// Something's readiness may have changed: wake everyone waiting in poll/select/epoll.
/// Cheap and IRQ-safe.
pub fn notify() {
    GEN.fetch_add(1, Ordering::AcqRel);
    WQ.wake_all();
}

/// The global readiness generation (changes on every [`notify`]).
pub fn generation() -> u64 {
    GEN.load(Ordering::Acquire)
}

/// Sleep until the generation moves past `seen`, `deadline_ns` (now_ns time) passes, or
/// [`RECHECK_NS`] elapses, or a signal is pending. The caller re-checks its fds.
pub fn wait(seen: u64, deadline_ns: u64) {
    let now = crate::timer::now_ns();
    if now >= deadline_ns {
        return;
    }
    let t = (deadline_ns - now).min(RECHECK_NS);
    WQ.wait_until_timeout(
        || generation() != seen || crate::proc::signal::interrupted(),
        t,
    );
}
