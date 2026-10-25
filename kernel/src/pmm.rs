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
//!
//! Every allocatable frame also has a one-byte [`Use`] tag (an array next to the bitmap),
//! so `/proc/meminfo` can say what the memory is for. Allocations start as
//! [`Use::Other`]; callers that matter say what theirs are with the `*_as` allocators or
//! [`set_use`]. A free resets the tag, so the counts can't drift when an owner forgets.

use crate::mmu::{PAGE_SIZE, PhysAddr, phys_to_virt};
use crate::sync::SpinLock;

const BITS: usize = 64;

/// What an allocated frame is for (`/proc/meminfo`, docs/memory.md "Where the memory goes").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Use {
    Free = 0,
    /// Allocated by someone who didn't say what for.
    Other,
    /// Reserved after boot (the device tree blob).
    Boot,
    /// Chunks of the kernel heap.
    Heap,
    /// Page-table pages (user address spaces, kernel stacks' mappings).
    PageTables,
    KernelStacks,
    /// User processes' private pages (anonymous memory, stacks, loaded programs).
    AppPages,
    /// tmpfs file data (the root file system, /tmp).
    Files,
    PageCache,
    /// Framebuffers (virtio-gpu backing, ramfb) and the compositor's back buffer.
    Display,
    /// The desktop picture and its blurred (glass) copies.
    Wallpaper,
    /// Window surfaces, and userland windows' shared and front buffers.
    WindowBuffers,
    /// Cached panels (menu bar, dock, toasts), Spaces thumbnails, the lock screen.
    PanelCaches,
    /// Device rings and DMA buffers, RAM disks.
    Devices,
}

impl Use {
    pub const COUNT: usize = 14;
    pub const ALL: [Use; Use::COUNT] = [
        Use::Free,
        Use::Other,
        Use::Boot,
        Use::Heap,
        Use::PageTables,
        Use::KernelStacks,
        Use::AppPages,
        Use::Files,
        Use::PageCache,
        Use::Display,
        Use::Wallpaper,
        Use::WindowBuffers,
        Use::PanelCaches,
        Use::Devices,
    ];
}

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
    /// One [`Use`] byte per frame.
    tags: *mut u8,
    /// Frames per [`Use`] (index = the tag).
    uses: [usize; Use::COUNT],
    /// The lowest `free` has been (frames), allocations that found no memory, and how many
    /// times `free` fell below the low and critical marks (`/proc/mempressure`).
    min_free: usize,
    failed: u64,
    low_events: u64,
    critical_events: u64,
    in_low: bool,
    in_critical: bool,
}

/// Memory is "low" under this part of RAM, "critical" under half of it (`/proc/mempressure`).
pub const LOW_DIV: usize = 8;
pub const CRITICAL_DIV: usize = 16;

// SAFETY: the raw pointer refers to RAM owned exclusively by the Pmm.
unsafe impl Send for Pmm {}

#[derive(Clone, Copy, Debug)]
pub struct PmmStats {
    pub base: PhysAddr,
    pub total_frames: usize,
    pub reserved_frames: usize,
    pub free_frames: usize,
    /// Allocated frames per [`Use`] (index = `Use as usize`; `Free` is always 0 here).
    pub uses: [usize; Use::COUNT],
    /// Below this many free frames memory is low, below `critical_mark` critical.
    pub low_mark: usize,
    pub critical_mark: usize,
    /// The fewest free frames there have been since boot.
    pub min_free: usize,
    /// Allocations that found no memory, since boot.
    pub failed: u64,
    /// Times free memory fell below the low / critical mark (from above it), since boot.
    pub low_events: u64,
    pub critical_events: u64,
}

/// How much trouble memory is in (`/proc/mempressure`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pressure {
    Normal,
    Low,
    Critical,
}

impl Pressure {
    pub fn name(self) -> &'static str {
        match self {
            Pressure::Normal => "normal",
            Pressure::Low => "low",
            Pressure::Critical => "critical",
        }
    }
}

impl PmmStats {
    /// The level free memory is at now.
    pub fn pressure(&self) -> Pressure {
        let free = self.free_frames;
        if free < self.critical_mark {
            Pressure::Critical
        } else if free < self.low_mark {
            Pressure::Low
        } else {
            Pressure::Normal
        }
    }
}

impl PmmStats {
    /// Frames tagged `u`.
    pub fn of(&self, u: Use) -> usize {
        self.uses[u as usize]
    }
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

    /// Retag `count` frames from `first` (all allocated) as `u`.
    fn tag(&mut self, first: usize, count: usize, u: Use) {
        for f in first..first + count {
            // SAFETY: f < self.frames (callers check), the tag array has `frames` bytes.
            let t = unsafe { &mut *self.tags.add(f) };
            self.uses[*t as usize] -= 1;
            *t = u as u8;
            self.uses[u as usize] += 1;
        }
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

    /// The marks, in frames: low under 1/8 of RAM, critical under 1/16.
    fn marks(&self) -> (usize, usize) {
        (self.frames / LOW_DIV, self.frames / CRITICAL_DIV)
    }

    /// Free memory changed: keep the low-water mark and count the crossings of the marks (a
    /// mark is crossed again only after free memory has come back a little above it).
    fn note(&mut self) {
        self.min_free = self.min_free.min(self.free);
        let (low, critical) = self.marks();
        if self.free < low {
            if !self.in_low {
                self.in_low = true;
                self.low_events += 1;
            }
        } else if self.free >= low + low / 8 {
            self.in_low = false;
        }
        if self.free < critical {
            if !self.in_critical {
                self.in_critical = true;
                self.critical_events += 1;
            }
        } else if self.free >= critical + critical / 8 {
            self.in_critical = false;
        }
    }

    fn alloc(&mut self, u: Use) -> Option<PhysAddr> {
        let Some(frame) = self.find_free(self.hint.max(self.reserved)) else {
            self.failed += 1;
            return None;
        };
        self.set_used(frame);
        self.free -= 1;
        self.hint = frame + 1;
        self.tag(frame, 1, u);
        self.note();
        Some(self.frame_addr(frame))
    }

    /// Find `count` consecutive free frames whose first frame's address is a
    /// multiple of `align_frames` frames (a power of two).
    fn alloc_contiguous(&mut self, count: usize, align_frames: usize, u: Use) -> Option<PhysAddr> {
        self.alloc_contiguous_between(count, align_frames, u, 0, usize::MAX)
    }

    /// [`alloc_contiguous`](Self::alloc_contiguous) restricted to frames wholly inside the
    /// physical range `[lo, hi)` (for devices that cannot address all of RAM).
    fn alloc_contiguous_between(
        &mut self,
        count: usize,
        align_frames: usize,
        u: Use,
        lo: PhysAddr,
        hi: PhysAddr,
    ) -> Option<PhysAddr> {
        if count == 0 || !align_frames.is_power_of_two() {
            return None;
        }
        if count > self.free {
            self.failed += 1;
            return None;
        }
        // Alignment is relative to physical address 0, not to `base`.
        let base_frame = self.base / PAGE_SIZE;
        let mut start = self
            .reserved
            .max(lo.saturating_sub(self.base).div_ceil(PAGE_SIZE));
        let end = self.frames.min(hi.saturating_sub(self.base) / PAGE_SIZE);
        'outer: while start + count <= end {
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
            self.tag(start, count, u);
            self.note();
            return Some(self.frame_addr(start));
        }
        // (No run long enough although there are frames: fragmentation, not exhaustion; the
        // caller has a way out and this is not counted.)
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
        self.tag(first, count, Use::Free);
        self.free += count;
        self.hint = self.hint.min(first);
        self.note();
    }
}

static PMM: SpinLock<Option<Pmm>> = SpinLock::new(None);

/// Is the allocator set up (and not locked by whoever is crashing)? For the Pixel 2's crash screen,
/// which must not allocate when it cannot.
#[cfg(feature = "msm8998")]
pub fn is_ready() -> bool {
    PMM.try_lock().is_some_and(|g| g.is_some())
}

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
    let bitmap_pa = reserved_end.next_multiple_of(PAGE_SIZE);
    let bitmap_end = (bitmap_pa + metadata_bytes(size)).next_multiple_of(PAGE_SIZE);
    assert!(bitmap_end <= base + size, "pmm: RAM too small");
    let reserved = (bitmap_end - base) / PAGE_SIZE;

    let bitmap = phys_to_virt(bitmap_pa) as *mut u64;
    let words = frames.div_ceil(BITS);
    let tags = bitmap.wrapping_add(words).cast::<u8>();
    // SAFETY: the bitmap and tag regions are inside RAM and unused.
    unsafe {
        // All used, then punch out the allocatable range.
        for i in 0..words {
            bitmap.add(i).write(!0);
        }
        // Tags: the reserved low region is nobody's (not counted), the rest free.
        core::ptr::write_bytes(tags, Use::Free as u8, frames);
    }
    let mut pmm = Pmm {
        base,
        frames,
        free: 0,
        hint: reserved,
        reserved,
        bitmap,
        tags,
        uses: [0; Use::COUNT],
        min_free: usize::MAX,
        failed: 0,
        low_events: 0,
        critical_events: 0,
        in_low: false,
        in_critical: false,
    };
    for f in reserved..frames {
        pmm.set_free(f);
    }
    pmm.free = frames - reserved;
    pmm.uses[Use::Free as usize] = frames - reserved;
    pmm.min_free = pmm.free;
    *PMM.lock() = Some(pmm);
}

/// Bytes of allocator metadata (bitmap + tags) for `size` bytes of RAM; it sits right
/// after the kernel image.
pub const fn metadata_bytes(size: usize) -> usize {
    let frames = size / PAGE_SIZE;
    frames.div_ceil(BITS) * 8 + frames
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
                if f >= p.reserved {
                    p.tag(f, 1, Use::Boot);
                }
            }
        }
    });
}

/// Allocate one 4 KiB frame (contents unspecified), counted as [`Use::Other`].
pub fn alloc_frame() -> Option<PhysAddr> {
    alloc_frame_as(Use::Other)
}

/// [`alloc_frame`] counted as `u`.
pub fn alloc_frame_as(u: Use) -> Option<PhysAddr> {
    with(|p| p.alloc(u))
}

/// Allocate one 4 KiB frame filled with zeroes.
pub fn alloc_frame_zeroed() -> Option<PhysAddr> {
    alloc_frame_zeroed_as(Use::Other)
}

/// [`alloc_frame_zeroed`] counted as `u`.
pub fn alloc_frame_zeroed_as(u: Use) -> Option<PhysAddr> {
    let pa = alloc_frame_as(u)?;
    // SAFETY: we own the frame and RAM is mapped.
    unsafe { core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, PAGE_SIZE) };
    Some(pa)
}

/// Allocate `count` physically contiguous frames, the first aligned to
/// `align_frames` frames (power of two, 1 for no constraint). Use this for DMA
/// buffers and virtqueues. Contents unspecified.
pub fn alloc_contiguous(count: usize, align_frames: usize) -> Option<PhysAddr> {
    alloc_contiguous_as(Use::Other, count, align_frames)
}

/// [`alloc_contiguous`] counted as `u`.
pub fn alloc_contiguous_as(u: Use, count: usize, align_frames: usize) -> Option<PhysAddr> {
    with(|p| p.alloc_contiguous(count, align_frames, u))
}

/// Allocate `count` frames wholly inside `[lo, hi)` (physical), zeroed: for a device that can
/// only address part of RAM (the Pi's mailbox sees the first GiB; its DMA engines everything
/// below `platform::CURRENT.dma_limit`). `None` if there is no such run.
pub fn alloc_contiguous_zeroed_between(
    u: Use,
    count: usize,
    align_frames: usize,
    lo: PhysAddr,
    hi: PhysAddr,
) -> Option<PhysAddr> {
    let pa = with(|p| p.alloc_contiguous_between(count, align_frames, u, lo, hi))?;
    // SAFETY: we own the frames and RAM is mapped.
    unsafe { core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, count * PAGE_SIZE) };
    Some(pa)
}

/// A zeroed run a DMA device can reach: below the board's DMA limit (everything on qemu virt).
pub fn alloc_dma_zeroed_as(u: Use, count: usize, align_frames: usize) -> Option<PhysAddr> {
    alloc_contiguous_zeroed_between(
        u,
        count,
        align_frames,
        0,
        crate::platform::CURRENT.dma_limit,
    )
}

/// Like [`alloc_contiguous`] but zeroed.
pub fn alloc_contiguous_zeroed(count: usize, align_frames: usize) -> Option<PhysAddr> {
    alloc_contiguous_zeroed_as(Use::Other, count, align_frames)
}

/// [`alloc_contiguous_zeroed`] counted as `u`.
pub fn alloc_contiguous_zeroed_as(u: Use, count: usize, align_frames: usize) -> Option<PhysAddr> {
    let pa = alloc_contiguous_as(u, count, align_frames)?;
    // SAFETY: we own the frames and RAM is mapped.
    unsafe { core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, count * PAGE_SIZE) };
    Some(pa)
}

/// Count `count` allocated frames from `pa` as `u` from now on (e.g. a buffer that changed
/// owner). Panics if any of them is free or outside allocatable RAM.
pub fn set_use(pa: PhysAddr, count: usize, u: Use) {
    with(|p| {
        let first = (pa - p.base) / PAGE_SIZE;
        assert!(
            first >= p.reserved && first + count <= p.frames,
            "pmm: set_use outside allocatable RAM"
        );
        assert!(
            (first..first + count).all(|f| p.is_used(f)),
            "pmm: set_use on a free frame"
        );
        p.tag(first, count, u);
    });
}

/// Free one frame. Panics on double free or a foreign address.
pub fn free_frame(pa: PhysAddr) {
    with(|p| p.free_frames(pa, 1));
}

/// Free `count` frames starting at `pa` (as returned by [`alloc_contiguous`]).
pub fn free_contiguous(pa: PhysAddr, count: usize) {
    with(|p| p.free_frames(pa, count));
}

/// Forget the low-water mark, the failures and the crossings counted so far: the boot
/// self-test asks for an impossible run on purpose, and `/proc/mempressure` should say what
/// happened once the system was up.
pub fn reset_counters() {
    with(|p| {
        p.min_free = p.free;
        p.failed = 0;
        p.low_events = 0;
        p.critical_events = 0;
        p.in_low = false;
        p.in_critical = false;
        p.note();
    });
}

pub fn stats() -> PmmStats {
    with(|p| PmmStats {
        base: p.base,
        total_frames: p.frames,
        reserved_frames: p.reserved,
        free_frames: p.free,
        uses: {
            let mut u = p.uses;
            u[Use::Free as usize] = 0;
            u
        },
        low_mark: p.marks().0,
        critical_mark: p.marks().1,
        min_free: p.min_free,
        failed: p.failed,
        low_events: p.low_events,
        critical_events: p.critical_events,
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
