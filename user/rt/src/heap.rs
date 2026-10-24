//! The global allocator: heapkit's heap on the brk arena. Freed memory is reused, big free
//! blocks are given back to the kernel (the break shrinks, or munmap inside the arena and
//! mmap MAP_FIXED to take the pages again), and a lock makes it safe with rt::thread.
//! The allocator itself is in user/heapkit (host-tested).

use crate::sys;

pub use heapkit::Stats;

/// brk(2) and page commit/decommit for the heap.
pub struct Brk;

impl heapkit::Pages for Brk {
    const PAGE: usize = 4096;

    fn start(&mut self) -> usize {
        sys::brk(0)
    }

    fn set_end(&mut self, end: usize) -> bool {
        sys::brk(end) == end
    }

    fn commit(&mut self, addr: usize, len: usize) -> bool {
        sys::mmap_fixed(addr, len).is_ok()
    }

    fn decommit(&mut self, addr: usize, len: usize) {
        let _ = sys::munmap(addr as *mut u8, len);
    }

    fn relax() {
        sys::sched_yield();
    }
}

#[global_allocator]
static HEAP: heapkit::Heap<Brk> = heapkit::Heap::new(Brk);

/// The heap's arena and how much of it is free, for diagnostics.
pub fn stats() -> Stats {
    HEAP.stats()
}

/// Forgets the peak arena size so far (see [`Stats::peak`]).
pub fn reset_peak() {
    HEAP.reset_peak();
}
