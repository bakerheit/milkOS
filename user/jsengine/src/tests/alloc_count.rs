//! A counting allocator for the host tests: current and peak live bytes of the thread that
//! asks, so the large-script tests can say how much memory parsing and compiling took (tests
//! run in parallel, each on its own threads; a block freed by another thread than the one
//! that made it just makes the counts a little off).

use core::alloc::{GlobalAlloc, Layout};
use core::cell::Cell;

pub(crate) struct Counting;

thread_local! {
    static CUR: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn add(n: isize) {
    // (try_with: a thread that is ending has no counters any more)
    let _ = CUR.try_with(|c| {
        let v = c.get() + n;
        c.set(v);
        let _ = PEAK.try_with(|p| {
            if v > p.get() {
                p.set(v);
            }
        });
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { std::alloc::System.alloc(l) };
        if !p.is_null() {
            add(l.size() as isize);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { std::alloc::System.dealloc(p, l) };
        add(-(l.size() as isize));
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { std::alloc::System.realloc(p, l, n) };
        if !q.is_null() {
            add(n as isize - l.size() as isize);
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Live bytes of this thread (made here, not yet freed).
pub(crate) fn current() -> usize {
    CUR.with(|c| c.get().max(0) as usize)
}

/// Peak of `current` since the last `reset_peak`.
pub(crate) fn peak() -> usize {
    PEAK.with(|p| p.get().max(0) as usize)
}

pub(crate) fn reset_peak() {
    CUR.with(|c| PEAK.with(|p| p.set(c.get())));
}
