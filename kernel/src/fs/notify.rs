//! Change notifications ("inotify-lite"): every namespace change and every close of a
//! file that was written is reported to whoever watches, in order.
//!
//! The VFS calls [`emit`] from `ops` (create, mkdir, symlink, link, unlink, rmdir,
//! rename, truncate, setattr) and from `OpenFile` (the last close of a written file, and
//! FICLONE). File systems that change contents wholesale (milkfs rollback, receive,
//! promote) call [`rescan`]. With no watcher, emitting is one atomic load.
//!
//! A watcher ([`watch`]) gets its own bounded queue. If it falls behind, events are
//! dropped and the next [`Watcher::next`] says so ([`Event::Overflow`]): rescan.
//!
//! Events name objects by (device, inode) rather than by path: a path is expensive to
//! build (walk `..`), depends on the caller's root (chroot), and has many spellings with
//! bind mounts; a watcher that cares about paths maps directories to paths itself (the
//! search indexer does).

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::node::{InodeRef, InodeType};
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

/// (device, inode number).
pub type Id = (u64, u64);

pub fn id(i: &InodeRef) -> Id {
    (i.dev(), i.ino())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// `name` appeared in directory `dir` (create, mkdir, symlink, link: `obj` may then
    /// already have other names).
    Created {
        dir: Id,
        name: String,
        obj: Id,
        kind: InodeType,
    },
    /// `name` went from directory `dir` (`obj`: what it named).
    Removed { dir: Id, name: String, obj: Id },
    /// `from_dir/from` is now `to_dir/to` (whatever `to` named before is gone).
    Renamed {
        from_dir: Id,
        from: String,
        to_dir: Id,
        to: String,
        obj: Id,
    },
    /// Contents changed: a written file was closed, truncated or cloned into.
    Modified { obj: Id },
    /// Metadata changed (mode, times).
    Attr { obj: Id },
    /// Everything on device `dev` may have changed (a rollback, a receive).
    Rescan { dev: u64 },
    /// This watcher missed events: rescan everything.
    Overflow,
}

pub struct Watcher {
    q: SpinLock<VecDeque<Event>>,
    cap: usize,
    lost: AtomicBool,
    wq: WaitQueue,
}

impl Watcher {
    /// The next event, waiting up to `timeout_ns` (None: nothing came).
    pub fn next(&self, timeout_ns: u64) -> Option<Event> {
        let mut got = None;
        self.wq.wait_until_timeout(
            || {
                if self.lost.swap(false, Ordering::AcqRel) {
                    self.q.lock().clear();
                    got = Some(Event::Overflow);
                    return true;
                }
                got = self.q.lock().pop_front();
                got.is_some()
            },
            timeout_ns,
        );
        got
    }

    /// Queued events now (without waiting).
    pub fn pending(&self) -> usize {
        self.q.lock().len()
    }
}

static WATCHERS: SpinLock<Vec<Weak<Watcher>>> = SpinLock::new(Vec::new());
static COUNT: AtomicUsize = AtomicUsize::new(0);

/// Start watching every change. The watch ends when the `Arc` is dropped.
pub fn watch(capacity: usize) -> Arc<Watcher> {
    let w = Arc::new(Watcher {
        q: SpinLock::new(VecDeque::new()),
        cap: capacity.max(16),
        lost: AtomicBool::new(false),
        wq: WaitQueue::new(),
    });
    let mut all = WATCHERS.lock();
    all.retain(|x| x.strong_count() > 0);
    all.push(Arc::downgrade(&w));
    COUNT.store(all.len(), Ordering::Release);
    w
}

/// Is anyone watching? (Callers can skip building an event.)
pub fn active() -> bool {
    COUNT.load(Ordering::Acquire) != 0
}

/// Report a change to every watcher.
pub fn emit(e: Event) {
    if !active() {
        return;
    }
    let ws: Vec<Arc<Watcher>> = {
        let mut all = WATCHERS.lock();
        all.retain(|x| x.strong_count() > 0);
        COUNT.store(all.len(), Ordering::Release);
        all.iter().filter_map(|w| w.upgrade()).collect()
    };
    for w in ws {
        {
            let mut q = w.q.lock();
            if q.len() >= w.cap {
                w.lost.store(true, Ordering::Release);
            } else {
                q.push_back(e.clone());
            }
        }
        w.wq.wake_all();
    }
}

/// Everything on `dev` may have changed.
pub fn rescan(dev: u64) {
    emit(Event::Rescan { dev });
}
