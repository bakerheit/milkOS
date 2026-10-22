//! Host tests: the heap runs on a reserved range of the Mac's address space, with brk, commit
//! and decommit done by mprotect, so touching a page the heap gave back faults, and a commit
//! fills its pages with garbage (a commit over live data would show up as corruption).

use super::*;
use std::collections::BTreeMap;
use std::sync::Arc;

const RESERVE: usize = 1 << 30;
const HOST_PAGE: usize = 16 << 10; // Apple silicon's page size: mprotect works in these
const PROT_NONE: i32 = 0;
const PROT_RW: i32 = 3;
const MAP_PRIVATE: i32 = 2;
#[cfg(target_os = "macos")]
const MAP_ANON: i32 = 0x1000;
#[cfg(not(target_os = "macos"))]
const MAP_ANON: i32 = 0x20;

unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn mprotect(addr: *mut u8, len: usize, prot: i32) -> i32;
    fn munmap(addr: *mut u8, len: usize) -> i32;
}

/// brk on a reserved range, counting committed pages.
struct Host {
    base: usize,
    brk: usize,
    limit: usize,
    on: Vec<bool>,
    resident: usize,
    syscalls: usize,
}

impl Host {
    fn new() -> Host {
        Host::with_limit(RESERVE)
    }
    fn with_limit(limit: usize) -> Host {
        // SAFETY: a fresh PROT_NONE reservation.
        let p = unsafe {
            mmap(
                core::ptr::null_mut(),
                RESERVE,
                PROT_NONE,
                MAP_PRIVATE | MAP_ANON,
                -1,
                0,
            )
        };
        assert!(p as isize != -1, "mmap reserve failed");
        let base = p as usize;
        Host {
            base,
            brk: base,
            limit: base + limit.min(RESERVE),
            on: vec![false; RESERVE / HOST_PAGE],
            resident: 0,
            syscalls: 0,
        }
    }
    fn set(&mut self, a: usize, len: usize, on: bool) {
        assert!(
            a.is_multiple_of(HOST_PAGE) && len.is_multiple_of(HOST_PAGE),
            "unaligned page range"
        );
        assert!(
            a >= self.base && a + len <= self.base + RESERVE,
            "page range outside the arena"
        );
        self.syscalls += 1;
        // SAFETY: inside our reservation.
        let r = unsafe { mprotect(a as *mut u8, len, if on { PROT_RW } else { PROT_NONE }) };
        assert_eq!(r, 0, "mprotect");
        if on {
            // SAFETY: just made writable. Garbage, so nothing relies on old or zero contents.
            unsafe { core::ptr::write_bytes(a as *mut u8, 0xA5, len) };
        }
        for i in (a - self.base) / HOST_PAGE..(a + len - self.base) / HOST_PAGE {
            if self.on[i] != on {
                self.on[i] = on;
                if on {
                    self.resident += HOST_PAGE;
                } else {
                    self.resident -= HOST_PAGE;
                }
            }
        }
    }
    fn up(x: usize) -> usize {
        align_up(x, HOST_PAGE)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // SAFETY: our own reservation.
        unsafe { munmap(self.base as *mut u8, RESERVE) };
    }
}

impl Pages for Host {
    const PAGE: usize = HOST_PAGE;
    fn start(&mut self) -> usize {
        self.brk
    }
    fn set_end(&mut self, end: usize) -> bool {
        if end < self.base || end > self.limit {
            return false;
        }
        let (old, new) = (Host::up(self.brk), Host::up(end));
        if new > old {
            self.set(old, new - old, true);
        } else if new < old {
            self.set(new, old - new, false);
        }
        self.brk = end;
        true
    }
    fn commit(&mut self, a: usize, len: usize) -> bool {
        assert!(a + len <= Host::up(self.brk), "commit above the break");
        self.set(a, len, true);
        true
    }
    fn decommit(&mut self, a: usize, len: usize) {
        assert!(a + len <= Host::up(self.brk), "decommit above the break");
        self.set(a, len, false);
    }
    fn relax() {
        std::thread::yield_now();
    }
}

fn heap() -> Heap<Host> {
    Heap::new(Host::new())
}
fn resident(h: &Heap<Host>) -> usize {
    h.with_pages(|p| p.resident)
}
fn syscalls(h: &Heap<Host>) -> usize {
    h.with_pages(|p| p.syscalls)
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }
    /// Mostly small, some medium, a few large: what programs do.
    fn size(&mut self) -> usize {
        match self.below(100) {
            0..60 => self.range(1, 128),
            60..80 => self.range(129, 2048),
            80..93 => self.range(2049, 64 << 10),
            93..99 => self.range(64 << 10, 1 << 20),
            _ => self.range(1 << 20, 4 << 20),
        }
    }
    fn align(&mut self) -> usize {
        if self.below(20) == 0 {
            1 << self.range(5, 16)
        } else {
            1 << self.below(5)
        }
    }
}

/// The byte a block tagged `tag` holds at offset `i`.
fn pat(tag: u32, i: usize) -> u8 {
    (tag as usize)
        .wrapping_mul(31)
        .wrapping_add(i)
        .wrapping_mul(0x9e37_79b9) as u8
        ^ (i >> 8) as u8
}

/// Offsets checked in a block: all of a small one, the ends and a stride of a big one.
fn spots(size: usize) -> impl Iterator<Item = usize> {
    let (head, tail) = if size <= 4096 {
        (size, size)
    } else {
        (512, size - 512)
    };
    (0..head)
        .chain((head..tail).step_by(1021))
        .chain(tail..size)
}

fn fill(p: usize, size: usize, tag: u32) {
    for i in spots(size) {
        // SAFETY: inside a live block.
        unsafe { *((p + i) as *mut u8) = pat(tag, i) };
    }
}

fn holds(p: usize, size: usize, tag: u32, upto: usize) -> bool {
    spots(size).filter(|&i| i < upto).all(|i| {
        // SAFETY: inside a live block.
        unsafe { *((p + i) as *const u8) == pat(tag, i) }
    })
}

/// What the test thinks is allocated: start -> (end, size, align, tag).
#[derive(Default)]
struct Model {
    live: BTreeMap<usize, (usize, usize, usize, u32)>,
    bytes: usize,
}

impl Model {
    fn add(&mut self, p: usize, size: usize, align: usize, tag: u32) {
        assert!(p != 0, "out of memory for {size} bytes");
        assert_eq!(
            p % align,
            0,
            "{size} bytes at {p:#x} not aligned to {align}"
        );
        let end = p + size.max(1);
        if let Some((_, &(e, ..))) = self.live.range(..=p).next_back() {
            assert!(e <= p, "block at {p:#x} overlaps the one before");
        }
        if let Some((&s, _)) = self.live.range(p..).next() {
            assert!(s >= end, "block at {p:#x} overlaps the one after");
        }
        self.live.insert(p, (end, size, align, tag));
        self.bytes += size;
        fill(p, size, tag);
    }
    fn remove(&mut self, p: usize) -> (usize, usize, u32) {
        let (_, size, align, tag) = self.live.remove(&p).unwrap();
        assert!(
            holds(p, size, tag, size),
            "block at {p:#x} ({size} bytes) was overwritten"
        );
        self.bytes -= size;
        (size, align, tag)
    }
    fn nth(&self, i: usize) -> usize {
        *self.live.keys().nth(i).unwrap()
    }
}

fn alloc(h: &Heap<Host>, size: usize, align: usize) -> usize {
    // SAFETY: valid layout.
    unsafe { h.alloc(Layout::from_size_align(size, align).unwrap()) as usize }
}
fn free(h: &Heap<Host>, p: usize, size: usize, align: usize) {
    // SAFETY: allocated with this layout.
    unsafe { h.dealloc(p as *mut u8, Layout::from_size_align(size, align).unwrap()) }
}
fn realloc(h: &Heap<Host>, p: usize, size: usize, align: usize, new: usize) -> usize {
    // SAFETY: allocated with this layout.
    unsafe {
        h.realloc(
            p as *mut u8,
            Layout::from_size_align(size, align).unwrap(),
            new,
        ) as usize
    }
}

/// `n` random alloc / free / realloc steps against the model, checking the heap as it goes.
fn churn(
    h: &Heap<Host>,
    m: &mut Model,
    rng: &mut Rng,
    n: usize,
    max_live: usize,
    tag0: u32,
    check_every: usize,
) {
    let mut tag = tag0;
    for step in 0..n {
        tag = tag.wrapping_add(1);
        // About as many allocations as frees, around a working set of up to `max_live` blocks.
        let r = rng.below(100);
        if m.live.is_empty() || (r < 45 && m.live.len() < max_live) {
            let (size, align) = (rng.size(), rng.align());
            let p = alloc(h, size, align);
            m.add(p, size, align, tag);
        } else if r < 85 {
            let p = m.nth(rng.below(m.live.len()));
            let (size, align, _) = m.remove(p);
            free(h, p, size, align);
        } else {
            let p = m.nth(rng.below(m.live.len()));
            let &(_, size, align, old_tag) = m.live.get(&p).unwrap();
            let new = if rng.below(2) == 0 {
                rng.size()
            } else {
                (size * 3 / 2).max(1)
            };
            let q = realloc(h, p, size, align, new);
            assert!(q != 0);
            assert!(
                holds(q, size, old_tag, new.min(size)),
                "realloc {size} -> {new} lost the contents"
            );
            m.live.remove(&p);
            m.bytes -= size;
            m.add(q, new, align, tag);
        }
        if check_every != 0 && step % check_every == 0 {
            h.check().unwrap();
            // Resident memory follows the live data: what's free can't pile up.
            let res = resident(h);
            assert!(
                res <= m.bytes * 2 + (4 << 20),
                "{res} resident for {} live bytes",
                m.bytes
            );
        }
    }
}

fn free_all(h: &Heap<Host>, m: &mut Model) {
    while let Some((&p, _)) = m.live.iter().next() {
        let (size, align, _) = m.remove(p);
        free(h, p, size, align);
    }
}

// ---- size classes and bins ----

#[test]
fn classes_cover_every_small_size() {
    let mut last = 0;
    for s in 1..=SMALL_MAX {
        let k = class_of(s);
        assert!(k < NCLASS, "{s}");
        let cs = class_size(k);
        assert!(cs >= s && cs % 16 == 0, "{s} -> class {k} of {cs}");
        assert!(
            k == 0 || class_size(k - 1) < s,
            "{s} is not in the smallest class that fits"
        );
        assert!(
            s <= 128 || cs * 4 <= s * 5,
            "{s} rounds up by more than 25% to {cs}"
        );
        assert!(k >= last);
        last = k;
        let rs = run_size(k);
        assert!(
            rs.is_power_of_two() && (rs - RUN_HDR) / cs >= 7,
            "class {k}: run {rs}"
        );
    }
    assert_eq!(class_of(SMALL_MAX), NCLASS - 1);
    assert_eq!(class_size(NCLASS - 1), SMALL_MAX);
}

#[test]
fn bins_are_in_size_order() {
    let mut last = 0;
    let mut s = MIN_CHUNK;
    while s < 1 << 40 {
        let b = bin_of(s);
        assert!(b >= last && b < NBINS, "{s}");
        last = b;
        s += 16 + s / 37 / 16 * 16;
    }
}

// ---- correctness against a model ----

#[test]
fn random_ops_match_a_model() {
    let h = heap();
    let mut m = Model::default();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    churn(&h, &mut m, &mut rng, 200_000, 2000, 0, 97);
    h.check().unwrap();
    free_all(&h, &mut m);
    h.check().unwrap();
    // Everything freed: only the kept runs and the top's slack stay resident.
    assert!(
        resident(&h) <= 1 << 20,
        "{} bytes still resident with nothing allocated",
        resident(&h)
    );
}

#[test]
fn other_seeds_match_a_model() {
    for seed in [1u64, 0xdead_beef, 0x0123_4567_89ab_cdef, 77, 9_999_999] {
        let h = heap();
        let mut m = Model::default();
        churn(&h, &mut m, &mut Rng(seed), 50_000, 1000, 0, 251);
        h.check().unwrap();
        free_all(&h, &mut m);
        h.check().unwrap();
    }
}

#[test]
fn every_alignment_is_honoured() {
    let h = heap();
    let mut m = Model::default();
    let mut tag = 0;
    for shift in 0..=16 {
        for size in [1, 7, 16, 100, 2048, 2049, 5000, 70_000] {
            tag += 1;
            let p = alloc(&h, size, 1 << shift);
            m.add(p, size, 1 << shift, tag);
        }
    }
    h.check().unwrap();
    free_all(&h, &mut m);
    h.check().unwrap();
}

#[test]
fn realloc_keeps_contents_across_every_route() {
    let h = heap();
    let sizes = [
        1,
        15,
        16,
        17,
        100,
        2048,
        2049,
        4000,
        100_000,
        300_000,
        3 << 20,
    ];
    for &a in &sizes {
        for &b in &sizes {
            let p = alloc(&h, a, 8);
            fill(p, a, 7);
            let q = realloc(&h, p, a, 8, b);
            assert!(holds(q, a, 7, a.min(b)), "{a} -> {b}");
            free(&h, q, b, 8);
        }
    }
    h.check().unwrap();
}

// ---- memory is reused and returned ----

#[test]
fn a_long_alloc_free_loop_does_not_grow_the_heap() {
    // A program's main loop: format some strings, build a Vec, drop it all.
    let h = heap();
    let mut keep = Vec::new();
    for i in 0..50 {
        let p = alloc(&h, 48, 8);
        keep.push(p);
        let _ = i;
    }
    let mut after_warmup = (0, 0);
    for round in 0..100_000 {
        let mut s = alloc(&h, 8, 1);
        let mut cap = 8;
        while cap < 300 {
            s = realloc(&h, s, cap, 1, cap * 2);
            cap *= 2;
        }
        let v = alloc(&h, 8000, 8);
        let b = alloc(&h, 64, 16);
        let big = if round % 10 == 0 {
            alloc(&h, 200_000, 8)
        } else {
            0
        };
        free(&h, b, 64, 16);
        free(&h, s, cap, 1);
        free(&h, v, 8000, 8);
        if big != 0 {
            free(&h, big, 200_000, 8);
        }
        if round == 1000 {
            after_warmup = (h.stats().end, resident(&h));
        }
    }
    let end = (h.stats().end, resident(&h));
    assert_eq!(
        end, after_warmup,
        "the heap grew in a steady loop (end, resident)"
    );
    for p in keep {
        free(&h, p, 48, 8);
    }
    h.check().unwrap();
}

#[test]
fn fragmentation_stays_bounded_under_churn() {
    // A steady working set of mixed sizes, one replaced at random every step.
    let h = heap();
    let mut rng = Rng(42);
    let mut slots: Vec<(usize, usize)> = Vec::new();
    let mut live = 0;
    let size = |rng: &mut Rng| match rng.below(10) {
        0..6 => rng.range(8, 512),
        6..9 => rng.range(513, 16 << 10),
        _ => rng.range(16 << 10, 200 << 10),
    };
    for _ in 0..3000 {
        let s = size(&mut rng);
        slots.push((alloc(&h, s, 8), s));
        live += s;
    }
    let mut peak_live = live;
    let mut worst = 0usize;
    let mut res_mid = 0;
    for step in 0..300_000 {
        let i = rng.below(slots.len());
        let (p, s) = slots[i];
        free(&h, p, s, 8);
        live -= s;
        let s = size(&mut rng);
        slots[i] = (alloc(&h, s, 8), s);
        live += s;
        peak_live = peak_live.max(live);
        worst = worst.max(resident(&h));
        if step == 100_000 {
            res_mid = h.stats().end;
        }
    }
    h.check().unwrap();
    let st = h.stats();
    println!(
        "churn: live {live}, peak live {peak_live}, arena {} (at 100k steps {}), worst resident {worst}",
        st.end - st.start,
        res_mid - st.start
    );
    assert!(
        worst <= peak_live / 4 * 5 + (1 << 20),
        "resident {worst} for at most {peak_live} live bytes"
    );
    assert!(
        st.end <= res_mid + (res_mid - st.start) / 8,
        "the arena kept growing under churn"
    );
    for (p, s) in slots {
        free(&h, p, s, 8);
    }
    assert!(resident(&h) <= 1 << 20);
}

#[test]
fn big_blocks_go_back_to_the_system() {
    let h = heap();
    let small = alloc(&h, 100, 8);
    let r0 = resident(&h);
    // At the top: the arena shrinks.
    let p = alloc(&h, 8 << 20, 8);
    fill(p, 8 << 20, 1);
    assert!(resident(&h) >= r0 + (8 << 20) - GROW);
    free(&h, p, 8 << 20, 8);
    assert!(
        resident(&h) <= r0 + TOP_KEEP + 2 * HOST_PAGE,
        "top not trimmed: {}",
        resident(&h)
    );
    // In the middle: its pages are decommitted, and come back when the space is reused.
    let a = alloc(&h, 8 << 20, 8);
    let b = alloc(&h, 5000, 8);
    fill(b, 5000, 2);
    let r1 = resident(&h);
    free(&h, a, 8 << 20, 8);
    assert!(
        resident(&h) + (8 << 20) - 2 * HOST_PAGE <= r1,
        "middle block not decommitted"
    );
    let a2 = alloc(&h, 4 << 20, 8);
    assert_eq!(a2, a, "the freed space is reused");
    fill(a2, 4 << 20, 3);
    assert!(holds(b, 5000, 2, 5000));
    let a3 = alloc(&h, 3 << 20, 8);
    fill(a3, 3 << 20, 4);
    assert!(holds(a2, 4 << 20, 3, 4 << 20));
    for (p, s) in [(a2, 4 << 20), (a3, 3 << 20), (b, 5000), (small, 100)] {
        free(&h, p, s, 8);
    }
    h.check().unwrap();
    assert!(resident(&h) <= r0 + TOP_KEEP + 2 * HOST_PAGE);
}

#[test]
fn freed_small_blocks_give_their_runs_back() {
    let h = heap();
    let r0 = {
        let p = alloc(&h, 24, 8);
        free(&h, p, 24, 8);
        resident(&h)
    };
    let mut rng = Rng(5);
    let mut ps: Vec<usize> = (0..200_000).map(|_| alloc(&h, 24, 8)).collect();
    assert!(resident(&h) > 4 << 20);
    for i in (1..ps.len()).rev() {
        ps.swap(i, rng.below(i + 1));
    }
    for p in ps {
        free(&h, p, 24, 8);
    }
    h.check().unwrap();
    assert!(
        resident(&h) <= r0 + TOP_KEEP + 2 * HOST_PAGE,
        "{} resident after freeing",
        resident(&h)
    );
}

// ---- realloc in place ----

#[test]
fn realloc_grows_in_place_at_the_top_and_into_free_space() {
    let h = heap();
    // At the top: a growing buffer never moves.
    let mut p = alloc(&h, 64 << 10, 8);
    let first = p;
    fill(p, 64 << 10, 9);
    let mut size = 64 << 10;
    while size < 16 << 20 {
        p = realloc(&h, p, size, 8, size * 2);
        assert_eq!(p, first, "moved when growing at the top to {}", size * 2);
        size *= 2;
    }
    assert!(holds(p, 64 << 10, 9, 64 << 10));
    free(&h, p, size, 8);
    // Into a freed neighbour.
    let a = alloc(&h, 10_000, 8);
    let b = alloc(&h, 10_000, 8);
    let c = alloc(&h, 10_000, 8);
    free(&h, b, 10_000, 8);
    fill(a, 10_000, 3);
    assert_eq!(realloc(&h, a, 10_000, 8, 18_000), a);
    assert!(holds(a, 10_000, 3, 10_000));
    // Shrinking stays put and frees the tail.
    let free0 = h.stats().free;
    assert_eq!(realloc(&h, a, 18_000, 8, 3000), a);
    assert!(h.stats().free >= free0 + 14_000);
    free(&h, a, 3000, 8);
    free(&h, c, 10_000, 8);
    h.check().unwrap();
}

#[test]
fn steady_growth_of_one_string_is_cheap() {
    // Pushing a byte at a time (Vec doubles) does few system calls.
    let h = heap();
    let mut p = alloc(&h, 8, 1);
    let mut cap = 8;
    let calls0 = syscalls(&h);
    while cap < 32 << 20 {
        p = realloc(&h, p, cap, 1, cap * 2);
        cap *= 2;
    }
    assert!(
        syscalls(&h) - calls0 < 40,
        "{} system calls",
        syscalls(&h) - calls0
    );
    free(&h, p, cap, 1);
}

// ---- running out ----

#[test]
fn out_of_memory_is_a_null_not_a_crash() {
    let h = Heap::new(Host::with_limit(8 << 20));
    let ok = alloc(&h, 1 << 20, 8);
    assert!(ok != 0);
    assert_eq!(alloc(&h, 16 << 20, 8), 0);
    assert_eq!(alloc(&h, isize::MAX as usize / 2, 8), 0);
    assert_eq!(alloc(&h, 1 << 20, 1 << 23), 0);
    assert_eq!(realloc(&h, ok, 1 << 20, 8, 64 << 20), 0);
    h.check().unwrap();
    // The old block is untouched after a failed realloc.
    fill(ok, 1 << 20, 5);
    let mut small = Vec::new();
    loop {
        let p = alloc(&h, 1000, 8);
        if p == 0 {
            break;
        }
        small.push(p);
    }
    assert!(small.len() > 4000);
    h.check().unwrap();
    assert!(holds(ok, 1 << 20, 5, 1 << 20));
    for p in small {
        free(&h, p, 1000, 8);
    }
    free(&h, ok, 1 << 20, 8);
    h.check().unwrap();
    assert!(
        alloc(&h, 6 << 20, 8) != 0,
        "memory came back after running out"
    );
}

// ---- threads ----

#[test]
fn threads_hammering_one_heap() {
    let h = Arc::new(heap());
    let threads: Vec<_> = (0..8u32)
        .map(|t| {
            let h = h.clone();
            std::thread::spawn(move || {
                let mut m = Model::default();
                let mut rng = Rng(0x5eed + t as u64 * 7919);
                // Each thread's tags are its own, so a block handed to two threads shows up.
                churn(&h, &mut m, &mut rng, 50_000, 200, t << 24, 0);
                free_all(&h, &mut m);
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    h.check().unwrap();
    assert!(
        resident(&h) <= 2 << 20,
        "{} resident after all threads freed everything",
        resident(&h)
    );
}

#[test]
fn threads_passing_blocks_between_them() {
    // Producer/consumer: blocks are freed on a different thread than the one that made them.
    let h = Arc::new(heap());
    let (tx, rx) = std::sync::mpsc::sync_channel::<(usize, usize, u32)>(256);
    let hp = h.clone();
    let prod = std::thread::spawn(move || {
        let mut rng = Rng(99);
        for tag in 0..100_000u32 {
            let s = rng.size().min(100_000);
            let p = alloc(&hp, s, 8);
            assert!(p != 0);
            fill(p, s, tag);
            tx.send((p, s, tag)).unwrap();
        }
    });
    let hc = h.clone();
    let cons = std::thread::spawn(move || {
        for (p, s, tag) in rx {
            assert!(holds(p, s, tag, s));
            free(&hc, p, s, 8);
        }
    });
    prod.join().unwrap();
    cons.join().unwrap();
    h.check().unwrap();
}
