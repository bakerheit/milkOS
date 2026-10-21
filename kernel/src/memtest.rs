//! Boot-time memory self-test. Panics (loudly, via the kernel panic handler) on
//! any failure; prints one line per stage on success.

use crate::mmu::{self, AddressSpace, MapAttrs, MapError, PAGE_SIZE, PmmTables, phys_to_virt};
use crate::{heap, kprintln, pmm};
use alloc::alloc::{alloc, dealloc};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::{format, vec};
use core::alloc::Layout;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn fill_frame(pa: usize, tag: u64) {
    let p = phys_to_virt(pa) as *mut u64;
    for i in 0..PAGE_SIZE / 8 {
        // SAFETY: caller owns the frame; RAM is mapped.
        unsafe { p.add(i).write_volatile(tag ^ i as u64) };
    }
}

fn check_frame(pa: usize, tag: u64) {
    let p = phys_to_virt(pa) as *const u64;
    for i in 0..PAGE_SIZE / 8 {
        // SAFETY: as above.
        let v = unsafe { p.add(i).read_volatile() };
        assert!(
            v == tag ^ i as u64,
            "selftest: frame {pa:#x} corrupted at word {i}: {v:#x}"
        );
    }
}

fn pmm_consistent() {
    let s = pmm::stats();
    assert!(
        s.free_frames == pmm::count_free_slow(),
        "selftest: pmm free counter {} != bitmap count {}",
        s.free_frames,
        pmm::count_free_slow()
    );
}

fn test_mmu() {
    assert!(mmu::caches_on(), "selftest: MMU/caches are not enabled");
    let (kstart, _) = crate::mem::kernel_range();
    mmu::with_kernel_space(|s| {
        // Linear map (KERNEL_BASE + pa) of device space and RAM; nothing low, nothing
        // non-canonical.
        for pa in [
            0x0900_0000usize,
            0x0a00_0000,
            0x3fff_f000,
            kstart,
            kstart + 0x1234,
        ] {
            assert!(
                s.translate(mmu::phys_to_virt(pa)) == Some(pa),
                "selftest: {pa:#x} not in the linear map"
            );
        }
        for va in [
            0x0900_0000usize,
            kstart,
            1 << 39,
            1 << 48,
            mmu::KERNEL_BASE - 0x1000,
        ] {
            assert!(s.translate(va).is_none(), "selftest: {va:#x} mapped");
        }

        // Map two scattered frames at an unused kernel virtual address and use them
        // through both names.
        let va = mmu::KERNEL_BASE + (1usize << 39);
        let a = pmm::alloc_frame().unwrap();
        let b = pmm::alloc_frame().unwrap();
        s.map(va, a, PAGE_SIZE, MapAttrs::KERNEL_DATA, &mut PmmTables)
            .unwrap();
        s.map(
            va + PAGE_SIZE,
            b,
            PAGE_SIZE,
            MapAttrs::KERNEL_DATA,
            &mut PmmTables,
        )
        .unwrap();
        assert!(s.translate(va + 8) == Some(a + 8));
        assert!(s.translate(va + PAGE_SIZE + 8) == Some(b + 8));
        // SAFETY: both pages are mapped read/write and we own the frames.
        unsafe {
            (va as *mut u64).write_volatile(0xdead_beef_cafe_f00d);
            ((va + PAGE_SIZE) as *mut u64).write_volatile(0x1234_5678_9abc_def0);
            assert!((mmu::phys_to_virt(a) as *const u64).read_volatile() == 0xdead_beef_cafe_f00d);
            assert!((mmu::phys_to_virt(b) as *const u64).read_volatile() == 0x1234_5678_9abc_def0);
        }
        // Overlap checks: same page again, and inside an existing 2 MiB block.
        assert!(
            s.map(va, a, PAGE_SIZE, MapAttrs::KERNEL_DATA, &mut PmmTables)
                == Err(MapError::AlreadyMapped)
        );
        assert!(
            s.map(
                mmu::phys_to_virt(kstart) & !0xfff,
                0,
                PAGE_SIZE,
                MapAttrs::KERNEL_DATA,
                &mut PmmTables
            ) == Err(MapError::AlreadyMapped)
        );
        assert!(
            s.map(va + 1, a, PAGE_SIZE, MapAttrs::KERNEL_DATA, &mut PmmTables)
                == Err(MapError::BadRange)
        );
        assert!(s.unmap_page(va) == Some(a));
        assert!(s.unmap_page(va + PAGE_SIZE) == Some(b));
        assert!(s.translate(va).is_none() && s.unmap_page(va).is_none());
        pmm::free_frame(a);
        pmm::free_frame(b);
    });

    // A throwaway address space: blocks vs pages, then give the tables back.
    let free_before = pmm::stats().free_frames;
    let mut space =
        AddressSpace::new(mmu::Half::Lower, &mut PmmTables).expect("selftest: no table frame");
    let two_mib = 512 * PAGE_SIZE;
    let pa = pmm::alloc_contiguous(1024, 512).expect("selftest: no 4 MiB contiguous");
    assert!(pa.is_multiple_of(two_mib));
    let va = (1usize << 40) + two_mib;
    // 4 MiB + one page: two 2 MiB blocks then a 4 KiB tail.
    space
        .map(
            va,
            pa,
            2 * two_mib + PAGE_SIZE,
            MapAttrs::KERNEL_DATA,
            &mut PmmTables,
        )
        .unwrap();
    for off in [0usize, 0x1000, two_mib - 8, two_mib, 2 * two_mib - 8] {
        assert!(
            space.translate(va + off) == Some(pa + off),
            "selftest: block translate"
        );
    }
    assert!(space.translate(va + 2 * two_mib + 0x10) == Some(pa + 2 * two_mib + 0x10));
    assert!(space.translate(va + 2 * two_mib + PAGE_SIZE).is_none());
    assert!(
        space.unmap_page(va).is_none(),
        "unmap of a block page must be refused"
    );
    // SAFETY: never loaded into a TTBR; tables came from the PMM.
    unsafe { space.destroy() };
    pmm::free_contiguous(pa, 1024);
    assert!(
        pmm::stats().free_frames == free_before,
        "selftest: table frames leaked"
    );
    kprintln!(
        "selftest: mmu ok (caches on, higher-half linear map, 4K/2M mappings, aliasing, destroy)"
    );
}

fn test_pmm() {
    pmm_consistent();
    let before = pmm::stats();
    let (kstart, kend) = crate::mem::kernel_range();
    let dtb = crate::mem::boot_dtb();

    // Many single frames: aligned, in RAM, outside kernel/bitmap, unique,
    // pairwise non-overlapping (checked by patterns).
    const N: usize = 3000;
    let mut frames = Vec::with_capacity(N);
    for i in 0..N {
        let pa = pmm::alloc_frame().expect("selftest: pmm exhausted");
        assert!(pa.is_multiple_of(PAGE_SIZE) && pa >= pmm::reserved_end());
        assert!(
            !(kstart..kend).contains(&pa),
            "selftest: pmm handed out kernel memory"
        );
        if dtb != 0 {
            assert!(
                !(dtb..dtb + 0x10_0000).contains(&pa),
                "selftest: pmm handed out the DTB"
            );
        }
        fill_frame(pa, (i as u64) << 32 | 0xa5a5);
        frames.push(pa);
    }
    for (i, &pa) in frames.iter().enumerate() {
        check_frame(pa, (i as u64) << 32 | 0xa5a5);
    }
    let mut sorted = frames.clone();
    sorted.sort_unstable();
    assert!(
        sorted.windows(2).all(|w| w[0] != w[1]),
        "selftest: duplicate frame"
    );
    assert!(pmm::stats().free_frames == before.free_frames - N);
    pmm_consistent();

    // Free every other frame, then ask for runs while the rest stay put.
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut kept = Vec::new();
    for (i, &pa) in frames.iter().enumerate() {
        if i % 2 == 0 {
            pmm::free_frame(pa);
        } else {
            kept.push((i, pa));
        }
    }
    let mut runs = Vec::new();
    for k in 0..40u64 {
        let count = 1 + rng.below(40) as usize;
        let align = 1usize << rng.below(6); // 1..32 frames
        let pa = pmm::alloc_contiguous(count, align).expect("selftest: contiguous alloc failed");
        assert!(
            pa.is_multiple_of(align * PAGE_SIZE),
            "selftest: contiguous alignment"
        );
        for f in 0..count {
            fill_frame(pa + f * PAGE_SIZE, 0xc0de_0000 + k * 100 + f as u64);
        }
        runs.push((k, pa, count));
    }
    for &(i, pa) in &kept {
        check_frame(pa, (i as u64) << 32 | 0xa5a5); // untouched by the runs
    }
    for &(k, pa, count) in &runs {
        for f in 0..count {
            check_frame(pa + f * PAGE_SIZE, 0xc0de_0000 + k * 100 + f as u64);
        }
    }
    // Big aligned run (2 MiB, as a DMA ring or a block mapping would want).
    let big = pmm::alloc_contiguous_zeroed(512, 512).expect("selftest: 2 MiB run");
    assert!(big.is_multiple_of(512 * PAGE_SIZE));
    // SAFETY: zeroed frames we own.
    unsafe {
        assert!(
            core::slice::from_raw_parts(phys_to_virt(big) as *const u8, 512 * PAGE_SIZE)
                .iter()
                .all(|&b| b == 0)
        );
    }
    pmm::free_contiguous(big, 512);
    // Impossible request must fail cleanly.
    assert!(pmm::alloc_contiguous(before.total_frames, 1).is_none());

    // Free the rest in scrambled order.
    let mut rest: Vec<usize> = kept.iter().map(|&(_, pa)| pa).collect();
    for i in (1..rest.len()).rev() {
        rest.swap(i, rng.below(i as u64 + 1) as usize);
    }
    for pa in rest {
        pmm::free_frame(pa);
    }
    for &(_, pa, count) in &runs {
        pmm::free_contiguous(pa, count);
    }
    drop(frames);
    drop(sorted);
    drop(kept);
    drop(runs);
    pmm_consistent();
    kprintln!(
        "selftest: pmm ok ({} singles, 40 aligned runs, 2 MiB run, scrambled frees)",
        N
    );
}

fn test_heap() {
    let pmm_before = pmm::stats().free_frames;
    let before = heap::stats();

    // Randomised alloc/free with alignment, content checks and a live set.
    struct Live {
        ptr: *mut u8,
        layout: Layout,
        tag: u8,
    }
    let mut rng = Rng(0xdead_beef_1234_5678);
    let mut live: Vec<Live> = Vec::new();
    for round in 0..20_000u32 {
        if live.len() < 300 && (live.is_empty() || rng.below(100) < 55) {
            let size = match rng.below(100) {
                0..=69 => 1 + rng.below(200),
                70..=94 => 200 + rng.below(5000),
                _ => 5000 + rng.below(100_000),
            } as usize;
            let align = 1usize << rng.below(13); // 1..4096
            let layout = Layout::from_size_align(size, align).unwrap();
            // SAFETY: non-zero size.
            let ptr = unsafe { alloc(layout) };
            assert!(!ptr.is_null(), "selftest: heap alloc failed ({layout:?})");
            assert!(
                (ptr as usize).is_multiple_of(align),
                "selftest: heap alignment"
            );
            let tag = round as u8 | 1;
            // SAFETY: freshly allocated, size bytes.
            unsafe { core::ptr::write_bytes(ptr, tag, size) };
            live.push(Live { ptr, layout, tag });
        } else {
            let i = rng.below(live.len() as u64) as usize;
            let l = live.swap_remove(i);
            // SAFETY: allocated above with this layout.
            unsafe {
                let s = core::slice::from_raw_parts(l.ptr, l.layout.size());
                assert!(
                    s.iter().all(|&b| b == l.tag),
                    "selftest: heap block corrupted (overlap?)"
                );
                dealloc(l.ptr, l.layout);
            }
        }
    }
    for l in live.drain(..) {
        // SAFETY: as above.
        unsafe {
            let s = core::slice::from_raw_parts(l.ptr, l.layout.size());
            assert!(
                s.iter().all(|&b| b == l.tag),
                "selftest: heap block corrupted (overlap?)"
            );
            dealloc(l.ptr, l.layout);
        }
    }
    drop(live);

    // The standard collections.
    let mut v: Vec<u64> = Vec::new();
    for i in 0..100_000u64 {
        v.push(i * i);
    }
    assert!(
        v.iter()
            .enumerate()
            .all(|(i, &x)| x == (i as u64) * (i as u64))
    );
    let b = Box::new([7u32; 1000]);
    assert!(b.iter().all(|&x| x == 7));
    let mut s = String::new();
    for i in 0..200 {
        s.push_str(&format!("{i},"));
    }
    assert!(s.len() == 690 && s.starts_with("0,1,2,") && s.ends_with("198,199,"));
    let mut m = BTreeMap::new();
    for i in 0..2000u32 {
        m.insert(format!("key{i:05}"), i);
    }
    assert!(m.len() == 2000 && m["key01234"] == 1234);
    let boxed: Vec<Box<[u8]>> = (0..100)
        .map(|i| vec![i as u8; 100 + i].into_boxed_slice())
        .collect();
    assert!(
        boxed
            .iter()
            .enumerate()
            .all(|(i, b)| b.len() == 100 + i && b.iter().all(|&x| x == i as u8))
    );
    drop((v, b, s, m, boxed));

    // Force the heap to grow: 6 MiB > the 4 MiB initial chunk.
    let grows = heap::stats().grows;
    let mut big = vec![0x5au8; 6 << 20];
    big[(6 << 20) - 1] = 1;
    assert!(big.iter().take((6 << 20) - 1).all(|&x| x == 0x5a));
    assert!(heap::stats().grows > grows, "selftest: heap did not grow");
    drop(big);

    // Nothing leaked, everything coalesced, and PMM frames are all accounted
    // for (frames the heap grew by are held by the heap).
    let after = heap::stats();
    assert!(
        after.used == before.used,
        "selftest: heap leak: {} -> {} bytes",
        before.used,
        after.used
    );
    assert!(after.free == before.free + (after.total - before.total));
    // Each growth chunk may legitimately be its own block (not adjacent to the
    // previous one); beyond that everything must have coalesced.
    assert!(
        after.free_blocks <= before.free_blocks + (after.grows - before.grows),
        "selftest: heap fragmented: {} blocks -> {}",
        before.free_blocks,
        after.free_blocks
    );
    assert!(
        pmm::stats().free_frames + after.total / PAGE_SIZE == pmm_before + before.total / PAGE_SIZE,
        "selftest: frames lost between pmm and heap"
    );
    pmm_consistent();
    kprintln!(
        "selftest: heap ok (20k random allocs w/ alignment, Vec/Box/String/BTreeMap, grew {}x, {} KiB total, {} free block(s))",
        after.grows,
        after.total >> 10,
        after.free_blocks
    );
}

/// DFSC of a data/instruction abort ESR: 0b0011LL = permission fault at level LL.
fn perm_fault(esr: u64) -> bool {
    esr & 0x3c == 0x0c
}

fn esr_desc(r: Result<(), u64>) -> String {
    match r {
        Ok(()) => String::from("no fault"),
        Err(e) => format!(
            "EC {:#x} {} level {}",
            e >> 26,
            if perm_fault(e) {
                "permission fault"
            } else {
                "other fault"
            },
            e & 3
        ),
    }
}

/// Kernel W^X: text is not writable, rodata not writable, heap and stacks not executable,
/// and the heap is still writable (the probes work). Faults are caught by
/// `exceptions::probe_*`.
fn test_wx() {
    use crate::exceptions::{probe_exec, probe_write};
    static RODATA: [u8; 8] = *b"readonly";
    let text = test_wx as *const () as usize;
    let rodata = RODATA.as_ptr() as usize;
    // A heap buffer holding `ret`, made coherent for instruction fetch in case it ran.
    let code = Box::new([0xd65f_03c0u32; 4]);
    let heap = code.as_ptr() as usize;
    // SAFETY: cache maintenance on our own buffer.
    unsafe {
        core::arch::asm!("dc cvau, {0}", "dsb ish", "ic ivau, {0}", "dsb ish", "isb",
            in(reg) heap, options(nostack));
    }
    let stack_code = [0xd65f_03c0u32; 4];
    let stack = stack_code.as_ptr() as usize;
    let w_text = probe_write(text, 0);
    let w_ro = probe_write(rodata, b'R');
    let x_heap = probe_exec(heap);
    let x_stack = probe_exec(stack);
    let w_heap = probe_write(heap + 8, 0);
    let ok = |r: &Result<(), u64>| r.is_err_and(perm_fault);
    assert!(
        ok(&w_text),
        "W^X: write to kernel text: {}",
        esr_desc(w_text)
    );
    assert!(ok(&w_ro), "W^X: write to kernel rodata: {}", esr_desc(w_ro));
    assert!(ok(&x_heap), "W^X: jump into the heap: {}", esr_desc(x_heap));
    assert!(
        ok(&x_stack),
        "W^X: jump into a kernel stack: {}",
        esr_desc(x_stack)
    );
    assert!(w_heap.is_ok(), "W^X: heap not writable?");
    assert!(RODATA == *b"readonly");
    let sctlr: u64;
    // SAFETY: register read.
    unsafe { core::arch::asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack)) };
    assert!(sctlr & (1 << 19) != 0, "W^X: SCTLR.WXN is off");
    kprintln!(
        "selftest: W^X ok: write to text ({}), to rodata ({}), jump into heap ({}) and \
         stack ({}) all fault; heap writes work; SCTLR.WXN on",
        esr_desc(w_text),
        esr_desc(w_ro),
        esr_desc(x_heap),
        esr_desc(x_stack)
    );
    drop(core::hint::black_box(code));
}

pub fn run() {
    kprintln!("selftest: memory...");
    test_mmu();
    test_pmm();
    test_heap();
    test_wx();
    let s = pmm::stats();
    let h = heap::stats();
    kprintln!(
        "selftest: PASS. pmm {} MiB free, heap {} KiB used of {} KiB",
        (s.free_frames * PAGE_SIZE) >> 20,
        h.used >> 10,
        h.total >> 10
    );
}
