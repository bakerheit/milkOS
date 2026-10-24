//! A counting allocator for the host tests: current and peak live bytes, so the large-script
//! tests can say how much memory parsing and compiling took. Only meaningful when one test runs
//! (`--test-threads=1`); the counters are global.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};

pub(crate) struct Counting;

static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { std::alloc::System.alloc(l) };
        if !p.is_null() {
            let c = CUR.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(c, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { std::alloc::System.dealloc(p, l) };
        CUR.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { std::alloc::System.realloc(p, l, n) };
        if !q.is_null() {
            if n >= l.size() {
                let c = CUR.fetch_add(n - l.size(), Relaxed) + (n - l.size());
                PEAK.fetch_max(c, Relaxed);
            } else {
                CUR.fetch_sub(l.size() - n, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

pub(crate) fn current() -> usize {
    CUR.load(Relaxed)
}

/// Peak since the last `reset_peak`, in bytes.
pub(crate) fn peak() -> usize {
    PEAK.load(Relaxed)
}

pub(crate) fn reset_peak() {
    PEAK.store(CUR.load(Relaxed), Relaxed);
}
