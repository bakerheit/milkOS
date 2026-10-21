//! Split virtqueues (virtio 1.x, 2.7): descriptor table, available ring, used ring.
//!
//! Memory: each queue lives in one [`QueueMem`] (4 KiB aligned, one page for
//! [`MAX_QUEUE_SIZE`] = 64). Drivers own that memory as a static for now.
//! TODO(mem): allocate `QueueMem` from `pmm` (one contiguous, zeroed frame per queue) instead
//! of statics. The device is handed physical addresses: [`Virtqueue::addresses`] translates
//! with `mmu::virt_to_phys`. Keep this memory in the kernel's linear map (or the image) and
//! Normal cacheable: virtio-mmio on qemu virt is `dma-coherent`, so no cache maintenance is
//! needed.
//!
//! Every ring access is a volatile, naturally aligned load/store: with the MMU off RAM is
//! Device memory, where unaligned or exclusive accesses fault or hang. No atomics RMW here.
//!
//! Not thread safe by itself: a `Virtqueue` must sit behind the owning driver's lock (or be
//! touched only with IRQs masked).

// Public API surface for later drivers (block, net, gpu); not all of it is used yet.
#![allow(dead_code)]

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use super::mmio;

/// Biggest queue a [`QueueMem`] holds.
pub const MAX_QUEUE_SIZE: usize = 64;

/// Descriptor flags.
pub const DESC_F_NEXT: u16 = 1;
/// Buffer is device-writable (device -> driver).
pub const DESC_F_WRITE: u16 = 2;

/// Used ring flag: the device does not want to be notified.
const USED_F_NO_NOTIFY: u16 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
struct Avail {
    flags: u16,
    idx: u16,
    ring: [u16; MAX_QUEUE_SIZE],
    used_event: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UsedElem {
    id: u32,
    len: u32,
}

#[repr(C)]
struct Used {
    flags: u16,
    idx: u16,
    ring: [UsedElem; MAX_QUEUE_SIZE],
    avail_event: u16,
}

/// Backing memory of one virtqueue: 1024 B descriptors, 134 B available ring, 518 B used
/// ring, all suitably aligned (16 / 2 / 4) inside one page. Must start zeroed.
#[repr(C, align(4096))]
pub struct QueueMem {
    desc: [Desc; MAX_QUEUE_SIZE],
    avail: Avail,
    used: Used,
}

impl QueueMem {
    pub const fn new() -> Self {
        const D: Desc = Desc {
            addr: 0,
            len: 0,
            flags: 0,
            next: 0,
        };
        const U: UsedElem = UsedElem { id: 0, len: 0 };
        QueueMem {
            desc: [D; MAX_QUEUE_SIZE],
            avail: Avail {
                flags: 0,
                idx: 0,
                ring: [0; MAX_QUEUE_SIZE],
                used_event: 0,
            },
            used: Used {
                flags: 0,
                idx: 0,
                ring: [U; MAX_QUEUE_SIZE],
                avail_event: 0,
            },
        }
    }
}

impl Default for QueueMem {
    fn default() -> Self {
        Self::new()
    }
}

const _: () = assert!(core::mem::size_of::<QueueMem>() <= 4096);

/// A split virtqueue over a [`QueueMem`].
pub struct Virtqueue {
    mem: *mut QueueMem,
    size: u16,
    /// Head of the free descriptor list (chained through `next`).
    free_head: u16,
    num_free: u16,
    /// Our shadow of `avail.idx` (we are its only writer).
    avail_idx: u16,
    /// Next used-ring slot we have not consumed yet.
    last_used: u16,
}

// SAFETY: the raw pointer is to memory owned by this queue; access is serialised by the
// owning driver (see the module docs).
unsafe impl Send for Virtqueue {}

impl Virtqueue {
    /// Build an empty queue of `size` entries (a power of two, 1..=MAX_QUEUE_SIZE) over
    /// `mem`, which it zeroes.
    ///
    /// # Safety
    /// `mem` must be valid, a kernel (linear-map) address, and used by nothing else for as long as the
    /// device may touch it (i.e. until the device is reset).
    pub unsafe fn new(mem: *mut QueueMem, size: u16) -> Self {
        assert!(size.is_power_of_two() && size as usize <= MAX_QUEUE_SIZE);
        // Zero with volatile word stores (no memset on Device memory surprises).
        let words = mem as *mut u64;
        for i in 0..core::mem::size_of::<QueueMem>() / 8 {
            unsafe { write_volatile(words.add(i), 0) };
        }
        let mut q = Virtqueue {
            mem,
            size,
            free_head: 0,
            num_free: size,
            avail_idx: 0,
            last_used: 0,
        };
        for i in 0..size {
            q.write_desc(i, 0, 0, 0, (i + 1) % size);
        }
        q
    }

    pub fn size(&self) -> u16 {
        self.size
    }

    pub fn num_free(&self) -> u16 {
        self.num_free
    }

    /// Physical addresses of (descriptor table, available ring, used ring).
    pub fn addresses(&self) -> (u64, u64, u64) {
        let pa = |va: usize| crate::mmu::virt_to_phys(va) as u64;
        // SAFETY: only computes field addresses.
        unsafe {
            (
                pa(addr_of!((*self.mem).desc) as usize),
                pa(addr_of!((*self.mem).avail) as usize),
                pa(addr_of!((*self.mem).used) as usize),
            )
        }
    }

    fn write_desc(&mut self, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
        // SAFETY: i < size <= MAX_QUEUE_SIZE; fields are naturally aligned.
        unsafe {
            let d = addr_of_mut!((*self.mem).desc[i as usize]);
            write_volatile(addr_of_mut!((*d).addr), addr);
            write_volatile(addr_of_mut!((*d).len), len);
            write_volatile(addr_of_mut!((*d).flags), flags);
            write_volatile(addr_of_mut!((*d).next), next);
        }
    }

    fn desc_flags_next(&self, i: u16) -> (u16, u16) {
        // SAFETY: as in write_desc.
        unsafe {
            let d = addr_of!((*self.mem).desc[i as usize]);
            (
                read_volatile(addr_of!((*d).flags)),
                read_volatile(addr_of!((*d).next)),
            )
        }
    }

    /// Put a chain of buffers on the queue: `out` (device reads) first, then `inb` (device
    /// writes), each as (physical address, length). Returns the head descriptor index, which
    /// [`pop_used`](Self::pop_used) hands back once the device is done. Does not notify; call
    /// [`should_notify`](Self::should_notify) / `Transport::notify` after a batch.
    pub fn add(&mut self, out: &[(u64, u32)], inb: &[(u64, u32)]) -> Option<u16> {
        let n = out.len() + inb.len();
        if n == 0 || n > self.num_free as usize {
            return None;
        }
        let head = self.free_head;
        let mut cur = head;
        for (k, &(addr, len)) in out.iter().chain(inb.iter()).enumerate() {
            let (_, next) = self.desc_flags_next(cur);
            let mut flags = if k >= out.len() { DESC_F_WRITE } else { 0 };
            if k + 1 < n {
                flags |= DESC_F_NEXT;
            }
            self.write_desc(cur, addr, len, flags, next);
            if k + 1 < n {
                cur = next;
            } else {
                self.free_head = next;
            }
        }
        self.num_free -= n as u16;
        self.publish(head);
        Some(head)
    }

    /// Re-offer a chain the device returned (same descriptors, same buffers) without
    /// freeing and re-adding it. For receive-style queues (input events, rx packets).
    pub fn requeue(&mut self, head: u16) {
        self.publish(head);
    }

    fn publish(&mut self, head: u16) {
        let slot = (self.avail_idx % self.size) as usize;
        // SAFETY: slot < size; ring entries are u16-aligned.
        unsafe {
            write_volatile(addr_of_mut!((*self.mem).avail.ring[slot]), head);
        }
        self.avail_idx = self.avail_idx.wrapping_add(1);
        mmio::mb(); // ring entry before index
        unsafe { write_volatile(addr_of_mut!((*self.mem).avail.idx), self.avail_idx) };
    }

    /// Does the device want a QueueNotify after we published buffers?
    pub fn should_notify(&self) -> bool {
        mmio::mb(); // our avail.idx store before reading the device's flags
        let flags = unsafe { read_volatile(addr_of!((*self.mem).used.flags)) };
        flags & USED_F_NO_NOTIFY == 0
    }

    /// Next completed chain as (head descriptor, bytes written by the device), if any.
    /// The chain stays allocated: hand it to [`requeue`](Self::requeue) or
    /// [`free`](Self::free).
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        let idx = unsafe { read_volatile(addr_of!((*self.mem).used.idx)) };
        if idx == self.last_used {
            return None;
        }
        mmio::mb(); // index before the entry it covers
        let slot = (self.last_used % self.size) as usize;
        let e = unsafe { addr_of!((*self.mem).used.ring[slot]) };
        let id = unsafe { read_volatile(addr_of!((*e).id)) };
        let len = unsafe { read_volatile(addr_of!((*e).len)) };
        self.last_used = self.last_used.wrapping_add(1);
        Some((id as u16, len))
    }

    /// Return a chain to the free list.
    pub fn free(&mut self, head: u16) {
        let mut cur = head;
        let mut n = 1;
        loop {
            let (flags, next) = self.desc_flags_next(cur);
            if flags & DESC_F_NEXT == 0 {
                // Splice the chain in front of the free list.
                self.write_desc(cur, 0, 0, 0, self.free_head);
                break;
            }
            cur = next;
            n += 1;
        }
        self.free_head = head;
        self.num_free += n;
    }
}
