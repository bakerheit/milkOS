//! Byte ring buffer for socket send/receive buffers.
//!
//! Each ring owns physically contiguous frames from the PMM (not the kernel heap, which
//! is small): [`Ring::alloc`] takes them, dropping the ring gives them back, and
//! [`Ring::grow`] moves the contents to a bigger block (TCP buffer auto-sizing). All
//! socket buffers together are capped at [`BUDGET`] bytes; past that, allocation and
//! growth fail (a socket then keeps its current size). Used under the stack lock only.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::mmu::{PAGE_SIZE, PhysAddr, phys_to_virt};
use crate::pmm;

/// Upper limit for the memory of all socket buffers together.
pub const BUDGET: usize = 64 << 20;
static IN_USE: AtomicUsize = AtomicUsize::new(0);

/// Bytes currently held by socket buffers (for /proc/net/sockstat-style reporting).
pub fn in_use() -> usize {
    IN_USE.load(Ordering::Relaxed)
}

pub struct Ring {
    buf: *mut u8,
    cap: usize,
    pa: PhysAddr,
    /// Offset of the first byte.
    head: usize,
    len: usize,
}

// SAFETY: the memory is owned by this ring; access is serialised by the stack lock.
unsafe impl Send for Ring {}

fn take(cap: usize) -> Option<(PhysAddr, *mut u8)> {
    let pages = cap.div_ceil(PAGE_SIZE);
    let bytes = pages * PAGE_SIZE;
    if IN_USE.fetch_add(bytes, Ordering::AcqRel) + bytes > BUDGET {
        IN_USE.fetch_sub(bytes, Ordering::AcqRel);
        return None;
    }
    let Some(pa) = pmm::alloc_contiguous(pages, 1) else {
        IN_USE.fetch_sub(bytes, Ordering::AcqRel);
        return None;
    };
    Some((pa, phys_to_virt(pa) as *mut u8))
}

fn give_back(pa: PhysAddr, cap: usize) {
    let pages = cap.div_ceil(PAGE_SIZE);
    pmm::free_contiguous(pa, pages);
    IN_USE.fetch_sub(pages * PAGE_SIZE, Ordering::AcqRel);
}

impl Ring {
    /// A ring of `cap` bytes (rounded up to whole pages), or None (out of memory/budget).
    pub fn alloc(cap: usize) -> Option<Ring> {
        let cap = cap.div_ceil(PAGE_SIZE).max(1) * PAGE_SIZE;
        let (pa, buf) = take(cap)?;
        Some(Ring {
            buf,
            cap,
            pa,
            head: 0,
            len: 0,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn cap(&self) -> usize {
        self.cap
    }
    pub fn free(&self) -> usize {
        self.cap - self.len
    }
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// Move to a block of `new_cap` bytes (bigger), keeping every byte: the readable ones
    /// and anything written past the end (TCP out-of-order data), at the same offsets
    /// from the head. False if there is no memory (the ring is unchanged).
    pub fn grow(&mut self, new_cap: usize) -> bool {
        let new_cap = new_cap.div_ceil(PAGE_SIZE) * PAGE_SIZE;
        if new_cap <= self.cap {
            return true;
        }
        let Some((pa, buf)) = take(new_cap) else {
            return false;
        };
        // SAFETY: both blocks are ours; the old one is cap bytes, the new one bigger.
        unsafe {
            let first = self.cap - self.head;
            core::ptr::copy_nonoverlapping(self.buf.add(self.head), buf, first);
            core::ptr::copy_nonoverlapping(self.buf, buf.add(first), self.head);
        }
        give_back(self.pa, self.cap);
        self.buf = buf;
        self.pa = pa;
        self.cap = new_cap;
        self.head = 0;
        true
    }

    fn mem(&self) -> &[u8] {
        // SAFETY: `buf..buf+cap` is our block.
        unsafe { core::slice::from_raw_parts(self.buf, self.cap) }
    }
    fn mem_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above.
        unsafe { core::slice::from_raw_parts_mut(self.buf, self.cap) }
    }

    /// Append as much of `data` as fits; returns bytes taken.
    pub fn push(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(self.free());
        let start = (self.head + self.len) % self.cap;
        let first = n.min(self.cap - start);
        let m = self.mem_mut();
        m[start..start + first].copy_from_slice(&data[..first]);
        m[..n - first].copy_from_slice(&data[first..n]);
        self.len += n;
        n
    }

    /// Write `data` into free space `off` bytes past the end, without making it readable
    /// (TCP out-of-order data). Returns how much fit. [`commit`](Self::commit) later.
    pub fn write_at(&mut self, off: usize, data: &[u8]) -> usize {
        if off >= self.free() {
            return 0;
        }
        let n = data.len().min(self.free() - off);
        let start = (self.head + self.len + off) % self.cap;
        let first = n.min(self.cap - start);
        let m = self.mem_mut();
        m[start..start + first].copy_from_slice(&data[..first]);
        m[..n - first].copy_from_slice(&data[first..n]);
        n
    }

    /// Make `n` bytes already written past the end (by `write_at`) readable.
    pub fn commit(&mut self, n: usize) {
        self.len += n.min(self.free());
    }

    /// Copy bytes `[off, off + out.len())` (relative to the head) without consuming them.
    /// Returns how many were available.
    pub fn peek_at(&self, off: usize, out: &mut [u8]) -> usize {
        if off >= self.len {
            return 0;
        }
        let n = out.len().min(self.len - off);
        let start = (self.head + off) % self.cap;
        let first = n.min(self.cap - start);
        let m = self.mem();
        out[..first].copy_from_slice(&m[start..start + first]);
        out[first..n].copy_from_slice(&m[..n - first]);
        n
    }

    /// Drop `n` bytes from the head.
    pub fn consume(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = (self.head + n) % self.cap;
        self.len -= n;
        // Don't re-home `head` to 0 when empty: out-of-order bytes may already sit past
        // the end (`write_at`), at positions relative to the current head.
    }

    /// Copy out and consume up to `out.len()` bytes.
    pub fn pop(&mut self, out: &mut [u8]) -> usize {
        let n = self.peek_at(0, out);
        self.consume(n);
        n
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        give_back(self.pa, self.cap);
    }
}
