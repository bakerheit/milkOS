#![allow(dead_code)]
//! Physical memory manager: a bitmap of 4 KiB frames.
//!
//! One bit per frame (1 = used). The bitmap lives in RAM right after the kernel
//! image. Allocation is next-fit from a moving hint, which keeps repeated
//! single-frame allocation O(1) in the common case; contiguous allocation is a
//! linear scan for a long enough run of clear bits.
//!
//! Frames from the start of RAM to the end of the bitmap (kernel image, boot
//! stack, the bitmap itself) are marked used at init and never handed out;
//! other ranges (the device tree blob) are excluded with [`reserve`].

use crate::mmu::{PAGE_SIZE, PhysAddr, phys_to_virt};
use crate::sync::SpinLock;

const BITS: usize = 64;

pub struct Pmm {
    /// Physical address of frame 0 (start of RAM).
    base: PhysAddr,
    /// Total frames covered by the bitmap.
    frames: usize,
    free: usize,
    /// Frame index where the next search starts.
    hint: usize,
    /// Frames below this index are firmware/kernel and permanently used.
    reserved: usize,
    bitmap: *mut u64,
}

// SAFETY: the raw pointer refers to RAM owned exclusively by the Pmm.
unsafe impl Send for Pmm {}

#[derive(Clone, Copy, Debug)]
pub struct PmmStats {
    pub base: PhysAddr,
    pub total_frames: usize,
    pub reserved_frames: usize,
    pub free_frames: usize,
}

impl Pmm {
    fn word(&self, i: usize) -> u64 {
        // SAFETY: i < ceil(frames/64), checked by callers.
        unsafe { self.bitmap.add(i / BITS).read() }
    }

    fn is_used(&self, frame: usize) -> bool {
        self.word(frame) >> (frame % BITS) & 1 != 0
    }

    fn set_used(&mut self, frame: usize) {
        // SAFETY: frame < self.frames.
        unsafe {
            let w = self.bitmap.add(frame / BITS);
            w.write(w.read() | 1 << (frame % BITS));
        }
    }

    fn set_free(&mut self, frame: usize) {
        // SAFETY: frame < self.frames.
        unsafe {
            let w = self.bitmap.add(frame / BITS);
            w.write(w.read() & !(1 << (frame % BITS)));
        }
    }

    fn frame_addr(&self, frame: usize) -> PhysAddr {
        self.base + frame * PAGE_SIZE
    }

    /// First free frame at or after `from` (wrapping once), as an index.
    fn find_free(&self, from: usize) -> Option<usize> {
        let words = self.frames.div_ceil(BITS);
        let start_word = from / BITS;
        for n in 0..=words {
            let w = (start_word + n) % words;
            let mut bits = !self.word(w * BITS);
            if n == 0 {
                bits &= !0u64 << (from % BITS); // ignore bits before `from`
            }
            if bits != 0 {
                let frame = w * BITS + bits.trailing_zeros() as usize;
                if frame < self.frames && frame >= self.reserved {
                    return Some(frame);
                }
            }
        }
        None
    }

    fn alloc(&mut self) -> Option<PhysAddr> {
        let frame = self.find_free(self.hint.max(self.reserved))?;
        self.set_used(frame);
        self.free -= 1;
        self.hint = frame + 1;
        Some(self.frame_addr(frame))
    }

    /// Find `count` consecutive free frames whose first frame's address is a
    /// multiple of `align_frames` frames (a power of two).
    fn alloc_contiguous(&mut self, count: usize, align_frames: usize) -> Option<PhysAddr> {
        if count == 0 || count > self.free || !align_frames.is_power_of_two() {
            return None;
        }
        // Alignment is relative to physical address 0, not to `base`.
        let base_frame = self.base / PAGE_SIZE;
        let mut start = self.reserved;
        'outer: while start + count <= self.frames {
            let misalign = (base_frame + start) % align_frames;
            if misalign != 0 {
                start += align_frames - misalign;
                continue;
            }
            for i in 0..count {
                if self.is_used(start + i) {
                    // Skip past the used frame and retry.
                    start += i + 1;
                    continue 'outer;
                }
            }
            for i in 0..count {
                self.set_used(start + i);
            }
            self.free -= count;
            self.hint = start + count;
            return Some(self.frame_addr(start));
        }
        None
    }

    fn free_frames(&mut self, pa: PhysAddr, count: usize) {
        assert!(
            pa.is_multiple_of(PAGE_SIZE) && pa >= self.base,
            "pmm: bad free address {pa:#x}"
        );
        let first = (pa - self.base) / PAGE_SIZE;
        assert!(
            first >= self.reserved && first + count <= self.frames,
            "pmm: free of {pa:#x} x{count} outside allocatable RAM"
        );
        for f in first..first + count {
            assert!(
                self.is_used(f),
                "pmm: double free of frame {:#x}",
                self.frame_addr(f)
            );
            self.set_free(f);
        }
        self.free += count;
        self.hint = self.hint.min(first);
    }
}

static PMM: SpinLock<Option<Pmm>> = SpinLock::new(None);

fn with<R>(f: impl FnOnce(&mut Pmm) -> R) -> R {
    f(PMM.lock().as_mut().expect("pmm not initialised"))
}

/// Set up the allocator for RAM `[base, base+size)`. Everything below
/// `reserved_end` (kernel, boot stack) is excluded, and the bitmap is carved
/// from the start of the remainder.
///
/// # Safety
/// Call once, after the MMU is on, with `[base, base+size)` mapped read/write
/// and nothing else using RAM above `reserved_end`.
pub unsafe fn init(base: PhysAddr, size: usize, reserved_end: PhysAddr) {
    assert!(base.is_multiple_of(PAGE_SIZE));
    let frames = size / PAGE_SIZE;
    let bitmap_bytes = frames.div_ceil(BITS) * 8;
    let bitmap_pa = reserved_end.next_multiple_of(PAGE_SIZE);
    let bitmap_end = (bitmap_pa + bitmap_bytes).next_multiple_of(PAGE_SIZE);
    assert!(bitmap_end <= base + size, "pmm: RAM too small");
    let reserved = (bitmap_end - base) / PAGE_SIZE;

    let bitmap = phys_to_virt(bitmap_pa) as *mut u64;
    let words = frames.div_ceil(BITS);
    // SAFETY: the bitmap region is inside RAM and unused.
    unsafe {
        // All used, then punch out the allocatable range.
        for i in 0..words {
            bitmap.add(i).write(!0);
        }
    }
    let mut pmm = Pmm {
        base,
        frames,
        free: 0,
        hint: reserved,
        reserved,
        bitmap,
    };
    for f in reserved..frames {
        pmm.set_free(f);
    }
    pmm.free = frames - reserved;
    *PMM.lock() = Some(pmm);
}

/// Mark `[pa, pa+len)` (rounded out to whole frames) as in use, e.g. the device
/// tree blob or an initrd. Frames already in use, or outside RAM, are skipped.
pub fn reserve(pa: PhysAddr, len: usize) {
    with(|p| {
        let start = pa & !(PAGE_SIZE - 1);
        let end = (pa + len).next_multiple_of(PAGE_SIZE);
        for addr in (start..end).step_by(PAGE_SIZE) {
            if addr < p.base {
                continue;
            }
            let f = (addr - p.base) / PAGE_SIZE;
            if f < p.frames && !p.is_used(f) {
                p.set_used(f);
                p.free -= 1;
            }
        }
    });
}

/// Allocate one 4 KiB frame (contents unspecified).
pub fn alloc_frame() -> Option<PhysAddr> {
    with(|p| p.alloc())
}

/// Allocate one 4 KiB frame filled with zeroes.
pub fn alloc_frame_zeroed() -> Option<PhysAddr> {
    let pa = alloc_frame()?;
    // SAFETY: we own the frame and RAM is mapped.
    unsafe { core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, PAGE_SIZE) };
    Some(pa)
}

/// Allocate `count` physically contiguous frames, the first aligned to
/// `align_frames` frames (power of two, 1 for no constraint). Use this for DMA
/// buffers and virtqueues. Contents unspecified.
pub fn alloc_contiguous(count: usize, align_frames: usize) -> Option<PhysAddr> {
    with(|p| p.alloc_contiguous(count, align_frames))
}

/// Like [`alloc_contiguous`] but zeroed.
pub fn alloc_contiguous_zeroed(count: usize, align_frames: usize) -> Option<PhysAddr> {
    let pa = alloc_contiguous(count, align_frames)?;
    // SAFETY: we own the frames and RAM is mapped.
    unsafe { core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, count * PAGE_SIZE) };
    Some(pa)
}

/// Free one frame. Panics on double free or a foreign address.
pub fn free_frame(pa: PhysAddr) {
    with(|p| p.free_frames(pa, 1));
}

/// Free `count` frames starting at `pa` (as returned by [`alloc_contiguous`]).
pub fn free_contiguous(pa: PhysAddr, count: usize) {
    with(|p| p.free_frames(pa, count));
}

pub fn stats() -> PmmStats {
    with(|p| PmmStats {
        base: p.base,
        total_frames: p.frames,
        reserved_frames: p.reserved,
        free_frames: p.free,
    })
}

/// Number of frames actually clear in the bitmap, counted from scratch. Used by
/// the self-test to check the `free` counter against the bitmap.
pub fn count_free_slow() -> usize {
    with(|p| (0..p.frames).filter(|&f| !p.is_used(f)).count())
}

/// End of the permanently reserved low region (kernel image, boot stack and the
/// bitmap itself). Individual `reserve`d ranges above it are not reflected here.
pub fn reserved_end() -> PhysAddr {
    with(|p| p.base + p.reserved * PAGE_SIZE)
}
