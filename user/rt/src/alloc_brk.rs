//! Global allocator: a bump allocator on top of brk(2). Grows the break in 64 KiB steps;
//! freeing the most recent allocation gives it back (enough for the usual temporary
//! String/Vec), anything else is leaked. Single-threaded processes only.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

use crate::sys;

struct Bump {
    /// (next free, end of the break). 0 = not initialised.
    st: UnsafeCell<(usize, usize)>,
}

// SAFETY: milkOS processes have one thread.
unsafe impl Sync for Bump {}

const STEP: usize = 64 << 10;

unsafe impl GlobalAlloc for Bump {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: single thread.
        let st = unsafe { &mut *self.st.get() };
        if st.1 == 0 {
            let b = sys::brk(0);
            *st = (b, b);
        }
        let start = st.0.next_multiple_of(l.align());
        let end = start + l.size();
        if end > st.1 {
            let want = end.next_multiple_of(STEP).max(st.1 + STEP);
            if sys::brk(want) != want {
                return core::ptr::null_mut();
            }
            st.1 = want;
        }
        st.0 = end;
        start as *mut u8
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: single thread.
        let st = unsafe { &mut *self.st.get() };
        if p as usize + l.size() == st.0 {
            st.0 = p as usize;
        }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // SAFETY: single thread.
        let st = unsafe { &mut *self.st.get() };
        // Growing the last block in place is the common case (pushing to a String).
        if p as usize + l.size() == st.0 && p as usize + new <= st.1 {
            st.0 = p as usize + new;
            return p;
        }
        // SAFETY: GlobalAlloc contract, forwarded.
        unsafe {
            let n = self.alloc(Layout::from_size_align_unchecked(new, l.align()));
            if !n.is_null() {
                core::ptr::copy_nonoverlapping(p, n, l.size().min(new));
                self.dealloc(p, l);
            }
            n
        }
    }
}

#[global_allocator]
static HEAP: Bump = Bump {
    st: UnsafeCell::new((0, 0)),
};

/// (next free address, current break) for diagnostics.
pub fn stats() -> (usize, usize) {
    // SAFETY: single thread, read only.
    unsafe { *HEAP.st.get() }
}
