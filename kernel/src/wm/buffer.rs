//! Owned pixel buffers: window backing stores, the back buffer, wallpapers, panel caches.
//!
//! Small buffers come from the kernel heap. Anything of [`FRAMES_FROM`] bytes or more is taken as
//! physically contiguous frames straight from the PMM (and given back on drop), so big windows
//! don't fragment the heap, and so a buffer can later be mapped into a userland client's address
//! space as shared memory without copying. Those frames are counted in `/proc/meminfo`
//! under the buffer's [`Use`] (window surfaces, wallpaper, panel caches, the back buffer).

use alloc::vec::Vec;

use crate::gfx::{Color, DrawTarget};
use crate::mmu::{PAGE_SIZE, phys_to_virt};
use crate::pmm::{self, Use};

/// Buffers at least this big (bytes) use contiguous PMM frames.
const FRAMES_FROM: usize = 128 * 1024;

enum Backing {
    /// Kept only to own (and on drop free) the allocation.
    Heap(#[allow(dead_code)] Vec<u32>),
    Frames {
        pa: usize,
        pages: usize,
    },
}

/// A `width` x `height` XRGB pixel buffer with `stride` pixels per row. The capacity can be
/// larger than the visible size, so a window being resized does not reallocate on every step.
pub struct Buffer {
    ptr: *mut u32,
    width: usize,
    height: usize,
    stride: usize,
    /// Rows the allocation can hold at this stride.
    cap_rows: usize,
    backing: Backing,
    /// What its frames count as in `/proc/meminfo`.
    kind: Use,
}

// SAFETY: the buffer exclusively owns its memory.
unsafe impl Send for Buffer {}

impl Buffer {
    /// A buffer of exactly `w` x `h` filled with `fill`, counted as `kind`. `None` if out of
    /// memory.
    pub fn new(kind: Use, w: usize, h: usize, fill: Color) -> Option<Buffer> {
        Self::with_capacity(kind, w, h, w, h, fill)
    }

    /// A `w` x `h` buffer that can grow to `cap_w` x `cap_h` without reallocating.
    pub fn with_capacity(
        kind: Use,
        w: usize,
        h: usize,
        cap_w: usize,
        cap_h: usize,
        fill: Color,
    ) -> Option<Buffer> {
        let stride = cap_w.max(w).max(1).next_multiple_of(4);
        let rows = cap_h.max(h).max(1);
        let n = stride * rows;
        let bytes = n * 4;
        let (ptr, backing) = if bytes >= FRAMES_FROM {
            let pages = bytes.div_ceil(PAGE_SIZE);
            let pa = pmm::alloc_contiguous_as(kind, pages, 1)?;
            (phys_to_virt(pa) as *mut u32, Backing::Frames { pa, pages })
        } else {
            let mut v: Vec<u32> = Vec::new();
            v.try_reserve_exact(n).ok()?;
            v.resize(n, fill);
            (v.as_mut_ptr(), Backing::Heap(v))
        };
        let mut b = Buffer {
            ptr,
            width: w,
            height: h,
            stride,
            cap_rows: rows,
            backing,
            kind,
        };
        b.clear(fill);
        Some(b)
    }

    /// Change the visible size to exactly `w` x `h`: the allocation is kept only if it is
    /// that size already, so no headroom is left over (a one-shot change: zoom, snap, a tab
    /// strip). Contents are unspecified afterwards. False if out of memory (nothing changed).
    pub fn resize_exact(&mut self, w: usize, h: usize) -> bool {
        if self.stride == w.max(1).next_multiple_of(4) && self.cap_rows == h.max(1) {
            self.width = w;
            self.height = h;
            return true;
        }
        match Buffer::new(self.kind, w, h, 0) {
            Some(b) => {
                *self = b;
                true
            }
            None => false,
        }
    }

    /// Change the visible size while it keeps changing (a resize drag). Reuses the allocation
    /// if it is big enough, otherwise reallocates with some headroom; [`resize_exact`]
    /// (Self::resize_exact) gives the headroom back when the drag ends. Contents are
    /// unspecified afterwards. False if out of memory.
    pub fn resize(&mut self, w: usize, h: usize) -> bool {
        if w <= self.stride && h <= self.cap_rows {
            self.width = w;
            self.height = h;
            return true;
        }
        let (cw, ch) = (w + w / 4, h + h / 4);
        let (sw, sh) = (
            super::theme::screen_w() as usize,
            super::theme::screen_h() as usize,
        );
        match Buffer::with_capacity(self.kind, w, h, cw.min(sw), ch.min(sh), 0) {
            Some(b) => {
                *self = b;
                true
            }
            None => false,
        }
    }

    /// Is the allocation bigger than the visible size needs?
    pub fn has_headroom(&self) -> bool {
        self.stride != self.width.max(1).next_multiple_of(4) || self.cap_rows != self.height.max(1)
    }

    pub fn w(&self) -> i32 {
        self.width as i32
    }

    pub fn h(&self) -> i32 {
        self.height as i32
    }

    /// Row `y` (visible part only).
    #[inline]
    pub fn row(&self, y: usize) -> &[u32] {
        &self.pixels()[y * self.stride..y * self.stride + self.width]
    }
}

impl DrawTarget for Buffer {
    fn width(&self) -> usize {
        self.width
    }
    fn height(&self) -> usize {
        self.height
    }
    fn stride(&self) -> usize {
        self.stride
    }
    fn pixels(&self) -> &[u32] {
        // SAFETY: ptr covers stride * cap_rows pixels, and height <= cap_rows.
        unsafe { core::slice::from_raw_parts(self.ptr, self.stride * self.height) }
    }
    fn pixels_mut(&mut self) -> &mut [u32] {
        // SAFETY: as above; &mut self gives exclusive access.
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.stride * self.height) }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        if let Backing::Frames { pa, pages } = self.backing {
            pmm::free_contiguous(pa, pages);
        }
    }
}
