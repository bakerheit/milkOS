//! Memory a device reads and writes by DMA: zeroed pages from the frame allocator that a device
//! can reach (below the board's DMA limit), and the cache maintenance that keeps the CPU and the
//! device looking at the same bytes.
//!
//! On qemu the devices see the CPU's caches; on the Raspberry Pi 4 the PCIe controller's DMA does
//! not. The rule that is right for both: after the CPU writes memory a device will read, clean it
//! ([`sync`]) before telling the device; before the CPU reads memory a device wrote, invalidate
//! it ([`sync`] again: clean + invalidate; the lines are clean, because everything the CPU wrote
//! was cleaned right after, so nothing stale is written back over the device's data). A freshly
//! allocated buffer is zeroed and synced here.

use core::arch::asm;

use crate::mmu::{PAGE_SIZE, phys_to_virt};
use crate::pmm;

/// A run of physically contiguous, zeroed, DMA-reachable pages.
pub struct Buffer {
    pa: usize,
    pages: usize,
}

impl Buffer {
    /// `pages` pages aligned to `align_pages` pages (a power of two), or `None` without memory.
    pub fn new(pages: usize, align_pages: usize) -> Option<Buffer> {
        let pa = pmm::alloc_dma_zeroed_as(pmm::Use::Devices, pages, align_pages)?;
        let b = Buffer { pa, pages };
        sync(b.va(), pages * PAGE_SIZE);
        Some(b)
    }

    /// The address a device uses.
    pub fn pa(&self) -> u64 {
        self.pa as u64
    }

    pub fn va(&self) -> usize {
        phys_to_virt(self.pa)
    }

    pub fn len(&self) -> usize {
        self.pages * PAGE_SIZE
    }

    /// Push the CPU's writes to `[off, off+len)` out to memory (before the device reads it) or drop
    /// the CPU's copy (before it reads what the device wrote).
    pub fn sync(&self, off: usize, len: usize) {
        if off < self.len() {
            sync(self.va() + off, len.min(self.len() - off));
        }
    }

    pub fn write32(&self, off: usize, v: u32) {
        if off + 4 <= self.len() {
            // SAFETY: inside our own pages, 4-byte aligned by the callers' layouts.
            unsafe { ((self.va() + off) as *mut u32).write_volatile(v) }
        }
    }

    pub fn read32(&self, off: usize) -> u32 {
        if off + 4 <= self.len() {
            // SAFETY: as above.
            unsafe { ((self.va() + off) as *const u32).read_volatile() }
        } else {
            0
        }
    }

    pub fn write64(&self, off: usize, v: u64) {
        self.write32(off, v as u32);
        self.write32(off + 4, (v >> 32) as u32);
    }

    pub fn read_bytes(&self, off: usize, out: &mut [u8]) {
        let n = out.len().min(self.len().saturating_sub(off));
        for (i, o) in out.iter_mut().take(n).enumerate() {
            // SAFETY: inside our pages.
            *o = unsafe { ((self.va() + off + i) as *const u8).read_volatile() };
        }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        pmm::free_contiguous(self.pa, self.pages);
    }
}

/// Clean and invalidate the data cache over `[va, va+len)` (to the point of coherency).
pub fn sync(va: usize, len: usize) {
    let ctr: u64;
    // SAFETY: register read.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    let line = 4usize << ((ctr >> 16) & 0xf);
    let mut a = va & !(line - 1);
    while a < va + len {
        // SAFETY: cache maintenance on mapped memory.
        unsafe { asm!("dc civac, {}", in(reg) a, options(nostack)) };
        a += line;
    }
    // SAFETY: barrier.
    unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
}
