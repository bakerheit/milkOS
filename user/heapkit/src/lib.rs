//! The user-space heap: the allocator behind every milkOS program's `Vec`, `String` and `Box`
//! (rt installs it as the global allocator). No syscalls of its own: the system side (the brk
//! arena and page commit/decommit) comes in through [`Pages`], so the whole thing runs and is
//! tested on the host (`scripts/heapkit-test.sh`).
//!
//! Layout: one contiguous arena that grows and shrinks at the top (brk on milkOS).
//!
//! * **Small blocks** (up to 2 KiB, alignment up to 16): 24 size classes, 16 B to 2 KiB in
//!   quarter-power-of-two steps (at most 25% rounding). Each class carves blocks out of *runs*
//!   (4 to 16 KiB, aligned to their size, so `ptr & !(run - 1)` finds a block's run). A run
//!   keeps its own free list and a live count; a run that empties goes back to the chunk heap,
//!   unless it is its class's only run.
//! * **Everything else** (and the runs themselves) is a *chunk*: a 16-byte header (size, flags
//!   and the size of the chunk before it) and the payload. Free chunks sit in segregated
//!   lists (exact 16-byte bins below 512 B, four bins per power of two above), are split on
//!   allocation and coalesced with both neighbours on free, so free memory never fragments
//!   into adjacent pieces.
//! * **Giving memory back**: a free chunk at the top of the arena of 256 KiB or more shrinks
//!   the arena (keeping 64 KiB). A free chunk of 256 KiB or more anywhere else has its whole
//!   pages decommitted (munmap) and is marked `HOLES`; when part of it is handed out again,
//!   exactly those pages are committed again (mmap MAP_FIXED). Smaller frees are batched:
//!   after every 256 KiB freed, a sweep decommits the whole pages of every free chunk (those
//!   already fully decommitted are marked `CLEAN` and skipped). So a process's resident size
//!   follows its live data, and the arena's address range is reused (no fresh mmap address
//!   per big block: the kernel never reuses mmap addresses or frees their page tables).
//! * **realloc** grows in place into a free neighbour or at the top of the arena, and shrinks
//!   in place.
//! * **Threads**: one spinlock around everything (it yields to the scheduler while it waits).
//!
//! Not async-signal-safe: a signal handler must not allocate.

#![cfg_attr(not(test), no_std)]

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

/// The system side of the heap: a contiguous arena that can move its end (brk) and give whole
/// pages inside it back and take them again.
pub trait Pages {
    /// Commit/decommit granularity (the kernel's page size).
    const PAGE: usize;
    /// The arena's start (the current break). Called once, before anything else.
    fn start(&mut self) -> usize;
    /// Move the end of the arena to `end`: growing makes `[old, end)` usable, shrinking gives
    /// the pages above `end` back. False if refused (out of memory).
    fn set_end(&mut self, end: usize) -> bool;
    /// Make the page-aligned range usable again after [`decommit`](Self::decommit); its
    /// contents are undefined. False if out of memory.
    fn commit(&mut self, addr: usize, len: usize) -> bool;
    /// Give the page-aligned range back; it must not be touched until committed again.
    fn decommit(&mut self, addr: usize, len: usize);
    /// Called while waiting for the lock (yield the CPU).
    fn relax();
}

// ---- tuning --------------------------------------------------------------------------------

/// Chunk header: size | flags, then the previous chunk's size.
const HDR: usize = 16;
/// Smallest chunk: a header and the two free-list links.
const MIN_CHUNK: usize = 32;
const INUSE: usize = 1;
/// Some whole pages inside this free chunk may be decommitted.
const HOLES: usize = 2;
/// Every whole page inside this free chunk is decommitted (with HOLES).
const CLEAN: usize = 4;
const FLAGS: usize = 15;
/// Grow the arena by at least this much at a time.
const GROW: usize = 64 << 10;
/// A free chunk this big is given back to the system (top: arena shrinks; else decommitted).
const RELEASE_MIN: usize = 256 << 10;
/// What a trimmed top chunk keeps, so a loop around the threshold doesn't brk every time.
const TOP_KEEP: usize = 64 << 10;
/// After this many bytes freed, decommit the whole pages of every free chunk (not just the
/// big ones), so small frees can't pile up resident memory either.
const SWEEP_AFTER: usize = 256 << 10;

/// Largest small (size-class) block.
pub const SMALL_MAX: usize = 2048;
const NCLASS: usize = 24;
/// Run header after the chunk header: next run, previous run, free-list head, live count.
const RUN_HDR: usize = HDR + 32;

const NBINS: usize = 192;

#[inline]
fn align_up(x: usize, a: usize) -> usize {
    (x + a - 1) & !(a - 1)
}

/// Size class of a small request (1..=SMALL_MAX): 16..128 in steps of 16, then four steps per
/// power of two.
pub fn class_of(size: usize) -> usize {
    if size <= 128 {
        return size.max(1).div_ceil(16) - 1;
    }
    let b = (usize::BITS - 1 - (size - 1).leading_zeros()) as usize; // floor(log2(size - 1))
    8 + (b - 7) * 4 + ((size - 1) >> (b - 2)) - 4
}

/// Block size of class `k`.
pub fn class_size(k: usize) -> usize {
    if k < 8 {
        return (k + 1) * 16;
    }
    let b = (k - 8) / 4 + 7; // power of two below the class
    (1 << b) + ((k - 8) % 4 + 1) * (1 << (b - 2))
}

/// Run size of class `k`: a power of two holding at least about eight blocks.
pub fn run_size(k: usize) -> usize {
    (class_size(k) * 8).next_power_of_two().max(4096)
}

/// Free-list bin of a free chunk of `size` bytes.
fn bin_of(size: usize) -> usize {
    if size < 512 {
        return size / 16;
    }
    let b = (usize::BITS - 1 - size.leading_zeros()) as usize;
    (32 + (b - 9) * 4 + ((size >> (b - 2)) & 3)).min(NBINS - 1)
}

/// Heap numbers for diagnostics.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// The arena's start and end.
    pub start: usize,
    pub end: usize,
    /// Bytes in free chunks (committed or not).
    pub free: usize,
    /// Bytes in use by chunks (including the small-block runs).
    pub used: usize,
    /// The largest the arena has been since the start (or the last `reset_peak`), in bytes:
    /// the process's peak memory use, to within the arena's growth step.
    pub peak: usize,
}

struct State<P> {
    sys: P,
    base: usize,
    /// End of the arena; the fence header sits just below it.
    end: usize,
    /// The highest the arena's end has been.
    peak_end: usize,
    bins: [usize; NBINS],
    bitmap: [u64; NBINS / 64],
    /// Per class: the first run that has free blocks (a doubly linked list).
    runs: [usize; NCLASS],
    free_bytes: usize,
    /// Bytes freed since the last sweep.
    dirty: usize,
}

/// The heap: a lock around the allocator state. `static`-constructible.
pub struct Heap<P: Pages> {
    locked: AtomicBool,
    st: UnsafeCell<State<P>>,
}

// SAFETY: every access to `st` happens with `locked` held.
unsafe impl<P: Pages + Send> Sync for Heap<P> {}

impl<P: Pages> Heap<P> {
    pub const fn new(sys: P) -> Self {
        Heap {
            locked: AtomicBool::new(false),
            st: UnsafeCell::new(State {
                sys,
                base: 0,
                end: 0,
                peak_end: 0,
                bins: [0; NBINS],
                bitmap: [0; NBINS / 64],
                runs: [0; NCLASS],
                free_bytes: 0,
                dirty: 0,
            }),
        }
    }

    /// Run `f` on the state with the lock held.
    fn locked<R>(&self, f: impl FnOnce(&mut State<P>) -> R) -> R {
        let mut spins = 0u32;
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                spins += 1;
                if spins < 100 {
                    core::hint::spin_loop();
                } else {
                    P::relax();
                }
            }
        }
        // Let go on the way out, even if `f` panics (in a host test).
        struct Unlock<'a>(&'a AtomicBool);
        impl Drop for Unlock<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _unlock = Unlock(&self.locked);
        // SAFETY: we hold the lock, so this is the only reference until we let go.
        f(unsafe { &mut *self.st.get() })
    }

    pub fn stats(&self) -> Stats {
        self.locked(|s| {
            if s.base == 0 {
                return Stats::default();
            }
            Stats {
                start: s.base,
                end: s.end,
                free: s.free_bytes,
                used: s.end - s.base - s.free_bytes,
                peak: s.peak_end.max(s.end) - s.base,
            }
        })
    }

    /// Forgets the peak so far: the next `stats().peak` counts from the arena as it is now.
    pub fn reset_peak(&self) {
        self.locked(|s| s.peak_end = s.end);
    }

    /// Run `f` on the system side, under the lock (tests read their page counts this way).
    pub fn with_pages<R>(&self, f: impl FnOnce(&P) -> R) -> R {
        self.locked(|s| f(&s.sys))
    }

    /// Walk every chunk, free list and run and check they agree. For tests.
    pub fn check(&self) -> Result<(), &'static str> {
        self.locked(|s| s.check())
    }
}

unsafe impl<P: Pages> GlobalAlloc for Heap<P> {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        self.locked(|s| s.alloc(l.size(), l.align())) as *mut u8
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        self.locked(|s| s.dealloc(p as usize, l.size(), l.align()));
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        self.locked(|s| s.realloc(p as usize, l.size(), l.align(), new)) as *mut u8
    }
}

// ---- raw words ---------------------------------------------------------------------------

#[inline]
fn rd(a: usize) -> usize {
    // SAFETY: only called on heap metadata words, which are always committed.
    unsafe { (a as *const usize).read() }
}
#[inline]
fn wr(a: usize, v: usize) {
    // SAFETY: as rd.
    unsafe { (a as *mut usize).write(v) }
}

// Chunk fields.
fn size(c: usize) -> usize {
    rd(c) & !FLAGS
}
fn chunk(c: usize) -> usize {
    size(c)
}
fn flags(c: usize) -> usize {
    rd(c) & FLAGS
}
fn inuse(c: usize) -> bool {
    rd(c) & INUSE != 0
}
fn set_head(c: usize, size: usize, flags: usize) {
    wr(c, size | flags);
}
fn prev_size(c: usize) -> usize {
    rd(c + 8)
}
fn set_prev_size(c: usize, v: usize) {
    wr(c + 8, v)
}
// Free chunk links.
fn next_free(c: usize) -> usize {
    rd(c + 16)
}
fn prev_free(c: usize) -> usize {
    rd(c + 24)
}
// Run fields (after the run chunk's header).
const R_NEXT: usize = HDR;
const R_PREV: usize = HDR + 8;
const R_FREE: usize = HDR + 16;
const R_LIVE: usize = HDR + 24;

/// Small or chunk, from the request alone (dealloc and realloc get the same layout).
fn small(size: usize, align: usize) -> bool {
    size <= SMALL_MAX && align <= 16
}

/// Chunk size for a payload of `size` bytes; None if absurd.
fn chunk_for(size: usize) -> Option<usize> {
    let s = size.checked_add(HDR + 15)? & !15;
    (s <= isize::MAX as usize / 2).then_some(s.max(MIN_CHUNK))
}

impl<P: Pages> State<P> {
    fn page_down(x: usize) -> usize {
        x & !(P::PAGE - 1)
    }
    fn page_up(x: usize) -> usize {
        align_up(x, P::PAGE)
    }

    fn fence(&self) -> usize {
        self.end - HDR
    }

    fn init(&mut self) -> bool {
        let base = align_up(self.sys.start(), 16);
        let end = Self::page_up(base + GROW);
        if !self.sys.set_end(end) {
            return false;
        }
        self.base = base;
        self.end = end;
        self.peak_end = end;
        // A permanent in-use sentinel at the start and the fence at the end, so coalescing
        // never looks past the arena.
        set_head(base, HDR, INUSE);
        set_prev_size(base, 0);
        let first = base + HDR;
        let fsize = self.fence() - first;
        set_head(first, fsize, 0);
        set_prev_size(first, HDR);
        set_head(self.fence(), HDR, INUSE);
        set_prev_size(self.fence(), fsize);
        self.insert(first);
        true
    }

    // ---- free lists ----

    fn insert(&mut self, c: usize) {
        let b = bin_of(size(c));
        let head = self.bins[b];
        wr(c + 16, head);
        wr(c + 24, 0);
        if head != 0 {
            wr(head + 24, c);
        }
        self.bins[b] = c;
        self.bitmap[b / 64] |= 1 << (b % 64);
        self.free_bytes += size(c);
    }

    fn unlink(&mut self, c: usize) {
        let b = bin_of(size(c));
        let (n, p) = (next_free(c), prev_free(c));
        if p != 0 {
            wr(p + 16, n);
        } else {
            self.bins[b] = n;
            if n == 0 {
                self.bitmap[b / 64] &= !(1 << (b % 64));
            }
        }
        if n != 0 {
            wr(n + 24, p);
        }
        self.free_bytes -= size(c);
    }

    /// The first non-empty bin at or above `b`.
    fn next_bin(&self, b: usize) -> Option<usize> {
        let mut w = b / 64;
        let mut bits = self.bitmap[w] & (!0u64 << (b % 64));
        loop {
            if bits != 0 {
                return Some(w * 64 + bits.trailing_zeros() as usize);
            }
            w += 1;
            if w == self.bitmap.len() {
                return None;
            }
            bits = self.bitmap[w];
        }
    }

    // ---- pages ----

    /// Commit whatever of `[lo, hi)` may be decommitted inside free chunk `c`.
    fn ensure(&mut self, c: usize, lo: usize, hi: usize) -> bool {
        if flags(c) & HOLES == 0 {
            return true;
        }
        let a = Self::page_down(lo).max(Self::page_up(c + MIN_CHUNK));
        let b = Self::page_up(hi).min(Self::page_down(c + size(c)));
        a >= b || self.sys.commit(a, b - a)
    }

    /// Give back the whole pages inside free chunk `c` (its header and links stay).
    fn decommit(&mut self, c: usize) {
        let a = Self::page_up(c + MIN_CHUNK);
        let b = Self::page_down(c + size(c));
        if a < b {
            self.sys.decommit(a, b - a);
            wr(c, rd(c) | HOLES | CLEAN);
        }
    }

    /// Decommit the whole pages of every free chunk that may have some committed.
    fn sweep(&mut self) {
        self.dirty = 0;
        let mut b = bin_of(P::PAGE);
        while let Some(bin) = self.next_bin(b) {
            let mut c = self.bins[bin];
            while c != 0 {
                if flags(c) & CLEAN == 0 {
                    self.decommit(c);
                }
                c = next_free(c);
            }
            b = bin + 1;
            if b == NBINS {
                break;
            }
        }
    }

    // ---- chunks ----

    /// Where a chunk of `size` with `(start + off) % align == 0` fits in free chunk `c`.
    fn fit(c: usize, size: usize, align: usize, off: usize) -> Option<usize> {
        let mut t = c;
        if !(t + off).is_multiple_of(align) {
            t = align_up(c + off, align) - off;
            if t - c < MIN_CHUNK {
                t += align; // the gap in front must be a chunk of its own
            }
        }
        (t + size <= c + chunk(c)).then_some(t)
    }

    /// A chunk of exactly `size` bytes (multiple of 16) whose start + `off` is aligned to
    /// `align`, marked in use. 0 if out of memory.
    fn alloc_chunk(&mut self, size: usize, align: usize, off: usize) -> usize {
        if self.base == 0 && !self.init() {
            return 0;
        }
        for attempt in 0..2 {
            let mut b = bin_of(size);
            while let Some(bin) = self.next_bin(b) {
                let mut c = self.bins[bin];
                while c != 0 {
                    if let Some(t) = Self::fit(c, size, align, off) {
                        return self.take(c, t, size);
                    }
                    c = next_free(c);
                }
                b = bin + 1;
                if b == NBINS {
                    break;
                }
            }
            if attempt == 1 {
                break;
            }
            // Nothing fits: grow the arena so the top chunk is big enough for any alignment.
            let want = if align <= 16 {
                size
            } else {
                match size.checked_add(align + MIN_CHUNK) {
                    Some(w) => w,
                    None => return 0,
                }
            };
            if !self.extend(want) {
                return 0;
            }
        }
        0
    }

    /// Carve `[t, t+size)` out of free chunk `c`; the gap before and the rest after stay free.
    fn take(&mut self, c: usize, t: usize, mut len: usize) -> usize {
        let csize = size(c);
        let end = c + csize;
        // The pages of the gap and of the rest are left alone, so they keep both flags.
        let holes = flags(c) & (HOLES | CLEAN);
        // The new chunk and the header of the rest after it must be in committed pages.
        if !self.ensure(c, t, (t + len + MIN_CHUNK).min(end)) {
            return 0;
        }
        self.unlink(c);
        if t > c {
            set_head(c, t - c, holes);
            self.insert(c);
            set_prev_size(t, t - c);
        }
        let rest = end - (t + len);
        if rest >= MIN_CHUNK {
            let r = t + len;
            set_head(r, rest, holes);
            set_prev_size(r, len);
            set_prev_size(end, rest);
            self.insert(r);
        } else {
            len += rest;
            set_prev_size(end, len);
        }
        set_head(t, len, INUSE);
        t
    }

    /// Free chunk `c`: coalesce with free neighbours, then give memory back if `give_back`
    /// and it's big enough.
    fn release(&mut self, mut c: usize, give_back: bool) {
        let mut s = size(c);
        if give_back {
            self.dirty += s;
        }
        // The merged chunk has committed pages (ours), so it's never CLEAN.
        let mut holes = 0;
        let p = c - prev_size(c);
        if !inuse(p) {
            holes |= flags(p) & HOLES;
            self.unlink(p);
            s += size(p);
            c = p;
        }
        let n = c + s;
        if !inuse(n) {
            holes |= flags(n) & HOLES;
            self.unlink(n);
            s += size(n);
        }
        set_head(c, s, holes);
        set_prev_size(c + s, s);
        if give_back && s >= RELEASE_MIN {
            if c + s == self.fence() {
                self.trim(c);
            } else {
                self.decommit(c);
            }
        }
        self.insert(c);
        if give_back && self.dirty >= SWEEP_AFTER {
            self.sweep();
        }
    }

    /// Shrink the arena so the free top chunk `c` (not in a list) keeps about TOP_KEEP.
    fn trim(&mut self, c: usize) {
        let new_end = Self::page_up(c + TOP_KEEP + HDR);
        if new_end >= self.end {
            return;
        }
        // The new fence goes inside `c`, maybe on a decommitted page.
        if !self.ensure(c, new_end - HDR, new_end) {
            return;
        }
        let keep = new_end - HDR - c;
        set_head(new_end - HDR, HDR, INUSE);
        set_prev_size(new_end - HDR, keep);
        if !self.sys.set_end(new_end) {
            // Refused: the old fence still stands, the header just written is free space.
            return;
        }
        self.end = new_end;
        set_head(c, keep, flags(c));
    }

    /// Grow the arena so the free chunk at the top is at least `want` bytes.
    fn extend(&mut self, want: usize) -> bool {
        let f = self.fence();
        let last = f - prev_size(f);
        let have = if inuse(last) { 0 } else { size(last) };
        let Some(add) = want
            .saturating_sub(have)
            .checked_add(P::PAGE)
            .map(|a| Self::page_up(a).max(GROW))
        else {
            return false;
        };
        let Some(new_end) = self.end.checked_add(add) else {
            return false;
        };
        if !self.sys.set_end(new_end) {
            return false;
        }
        // The old fence becomes the header of the new space.
        set_head(f, add, INUSE);
        self.end = new_end;
        self.peak_end = self.peak_end.max(new_end);
        set_head(self.fence(), HDR, INUSE);
        set_prev_size(self.fence(), add);
        self.release(f, false);
        true
    }

    // ---- small blocks ----

    fn alloc_small(&mut self, k: usize) -> usize {
        let mut r = self.runs[k];
        if r == 0 {
            r = self.new_run(k);
            if r == 0 {
                return 0;
            }
        }
        let b = rd(r + R_FREE);
        wr(r + R_FREE, rd(b));
        wr(r + R_LIVE, rd(r + R_LIVE) + 1);
        if rd(r + R_FREE) == 0 {
            self.unlink_run(k, r); // full
        }
        b
    }

    fn free_small(&mut self, p: usize, k: usize) {
        let r = p & !(run_size(k) - 1);
        let was_full = rd(r + R_FREE) == 0;
        wr(p, rd(r + R_FREE));
        wr(r + R_FREE, p);
        let live = rd(r + R_LIVE) - 1;
        wr(r + R_LIVE, live);
        if was_full {
            self.link_run(k, r);
        }
        // An empty run goes back, unless it's the only one with room in its class.
        if live == 0 && !(self.runs[k] == r && rd(r + R_NEXT) == 0) {
            self.unlink_run(k, r);
            self.release(r, true);
        }
    }

    fn new_run(&mut self, k: usize) -> usize {
        let rs = run_size(k);
        let r = self.alloc_chunk(rs, rs, 0);
        if r == 0 {
            return 0;
        }
        let bs = class_size(k);
        let n = (rs - RUN_HDR) / bs;
        // Free list in address order.
        let first = r + RUN_HDR;
        for i in 0..n {
            let b = first + i * bs;
            wr(b, if i + 1 < n { b + bs } else { 0 });
        }
        wr(r + R_FREE, first);
        wr(r + R_LIVE, 0);
        self.link_run(k, r);
        r
    }

    fn link_run(&mut self, k: usize, r: usize) {
        let head = self.runs[k];
        wr(r + R_NEXT, head);
        wr(r + R_PREV, 0);
        if head != 0 {
            wr(head + R_PREV, r);
        }
        self.runs[k] = r;
    }

    fn unlink_run(&mut self, k: usize, r: usize) {
        let (n, p) = (rd(r + R_NEXT), rd(r + R_PREV));
        if p != 0 {
            wr(p + R_NEXT, n);
        } else {
            self.runs[k] = n;
        }
        if n != 0 {
            wr(n + R_PREV, p);
        }
    }

    // ---- the GlobalAlloc entry points ----

    fn alloc(&mut self, size: usize, align: usize) -> usize {
        if small(size, align) {
            if self.base == 0 && !self.init() {
                return 0;
            }
            return self.alloc_small(class_of(size));
        }
        let Some(cs) = chunk_for(size) else { return 0 };
        let c = self.alloc_chunk(cs, align.max(16), HDR);
        if c == 0 { 0 } else { c + HDR }
    }

    fn dealloc(&mut self, p: usize, size: usize, align: usize) {
        if small(size, align) {
            self.free_small(p, class_of(size));
        } else {
            self.release(p - HDR, true);
        }
    }

    fn realloc(&mut self, p: usize, old: usize, align: usize, new: usize) -> usize {
        match (small(old, align), small(new, align)) {
            (true, true) if class_of(old) == class_of(new) => return p,
            (false, false) => {
                if self.resize_in_place(p - HDR, new) {
                    return p;
                }
            }
            _ => {}
        }
        let q = self.alloc(new, align);
        if q != 0 {
            // SAFETY: both blocks are live and distinct.
            unsafe { core::ptr::copy_nonoverlapping(p as *const u8, q as *mut u8, old.min(new)) };
            self.dealloc(p, old, align);
        }
        q
    }

    /// Resize in-use chunk `c` to hold `new` payload bytes without moving it.
    fn resize_in_place(&mut self, c: usize, new: usize) -> bool {
        let Some(need) = chunk_for(new) else {
            return false;
        };
        let cur = size(c);
        if need <= cur {
            if cur - need >= MIN_CHUNK {
                // Split the tail off and free it.
                set_head(c, need, INUSE);
                let r = c + need;
                set_head(r, cur - need, INUSE);
                set_prev_size(r, need);
                set_prev_size(r + cur - need, cur - need);
                self.release(r, true);
            }
            return true;
        }
        // Growing: take from the free chunk after us, growing the arena first if we're at
        // the top.
        let mut n = c + cur;
        let at_top = n == self.fence() || (!inuse(n) && n + size(n) == self.fence());
        let have = if inuse(n) { 0 } else { size(n) };
        if cur + have < need {
            if !at_top || !self.extend(need - cur) {
                return false;
            }
            n = c + cur;
        }
        let ns = size(n);
        let end = n + ns;
        if !self.ensure(n, n, (c + need + MIN_CHUNK).min(end)) {
            return false;
        }
        let holes = flags(n) & (HOLES | CLEAN);
        self.unlink(n);
        let total = cur + ns;
        if total - need >= MIN_CHUNK {
            let r = c + need;
            set_head(r, total - need, holes);
            set_prev_size(r, need);
            set_prev_size(end, total - need);
            self.insert(r);
            set_head(c, need, INUSE);
        } else {
            set_head(c, total, INUSE);
            set_prev_size(end, total);
        }
        true
    }

    // ---- consistency check (tests) ----

    fn check(&self) -> Result<(), &'static str> {
        if self.base == 0 {
            return Ok(());
        }
        let mut c = self.base;
        let mut prev = 0;
        let mut last_free = false;
        let (mut nfree, mut free_bytes) = (0usize, 0usize);
        while c != self.fence() {
            let s = size(c);
            if s < HDR || s % 16 != 0 || c + s > self.fence() {
                return Err("bad chunk size");
            }
            if prev_size(c) != prev {
                return Err("prev_size doesn't match the chunk before");
            }
            let free = !inuse(c);
            if free && last_free {
                return Err("two free chunks next to each other");
            }
            if free {
                nfree += 1;
                free_bytes += s;
            }
            last_free = free;
            prev = s;
            c += s;
        }
        if prev_size(c) != prev || size(c) != HDR || !inuse(c) {
            return Err("bad fence");
        }
        let mut listed = 0;
        for b in 0..NBINS {
            let mut c = self.bins[b];
            if (c != 0) != (self.bitmap[b / 64] & (1 << (b % 64)) != 0) {
                return Err("bitmap disagrees with a bin");
            }
            let mut p = 0;
            while c != 0 {
                if inuse(c) || bin_of(size(c)) != b || prev_free(c) != p {
                    return Err("bad free-list entry");
                }
                listed += 1;
                p = c;
                c = next_free(c);
            }
        }
        if listed != nfree || free_bytes != self.free_bytes {
            return Err("free lists don't match the chunks");
        }
        for (k, &first) in self.runs.iter().enumerate() {
            let (rs, bs) = (run_size(k), class_size(k));
            let n = (rs - RUN_HDR) / bs;
            let mut r = first;
            let mut p = 0;
            while r != 0 {
                if !r.is_multiple_of(rs) || !inuse(r) || size(r) < rs || rd(r + R_PREV) != p {
                    return Err("bad run");
                }
                let mut b = rd(r + R_FREE);
                let mut nf = 0;
                while b != 0 {
                    if b < r + RUN_HDR
                        || !(b - r - RUN_HDR).is_multiple_of(bs)
                        || b >= r + RUN_HDR + n * bs
                    {
                        return Err("bad block in a run's free list");
                    }
                    nf += 1;
                    if nf > n {
                        return Err("loop in a run's free list");
                    }
                    b = rd(b);
                }
                if nf == 0 || nf + rd(r + R_LIVE) != n {
                    return Err("run counts don't add up");
                }
                p = r;
                r = rd(r + R_NEXT);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
