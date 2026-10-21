#![allow(dead_code)]
//! Kernel heap: an address-ordered free list with coalescing, behind a
//! `GlobalAlloc`.
//!
//! * Every block and every request is rounded to a multiple of 16 bytes and
//!   16-byte aligned, so any leftover from splitting is either 0 or at least
//!   one free-list node (16 bytes).
//! * Free blocks hold a `Node { size, next }` in their own first 16 bytes.
//! * The heap starts with [`INITIAL_FRAMES`] contiguous frames from the PMM and
//!   grows by whole contiguous chunks when a request does not fit. Chunks are
//!   never returned to the PMM; adjacent chunks merge into one free block.
//!
//! The heap lock masks IRQs, but allocating in an interrupt handler is still a
//! bad idea (it can fail and it is slow). Don't.

use crate::mmu::{PAGE_SIZE, PhysAddr, phys_to_virt};
use crate::pmm;
use crate::sync::SpinLock;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

const MIN_ALIGN: usize = 16;
/// 4 MiB to start with.
const INITIAL_FRAMES: usize = 1024;
/// Smallest growth step: 1 MiB.
const GROW_FRAMES: usize = 256;

#[repr(C)]
struct Node {
    size: usize,
    next: *mut Node,
}

struct Heap {
    head: *mut Node,
    /// Bytes handed to the heap by the PMM, in total.
    total: usize,
    /// Bytes currently allocated (rounded sizes).
    used: usize,
    /// Number of times we grew after the initial chunk.
    grows: usize,
}

// SAFETY: the raw pointers refer to heap memory owned by the Heap.
unsafe impl Send for Heap {}

#[derive(Clone, Copy, Debug)]
pub struct HeapStats {
    pub total: usize,
    pub used: usize,
    pub free: usize,
    /// Number of separate free blocks (1 = fully coalesced).
    pub free_blocks: usize,
    pub largest_free: usize,
    pub grows: usize,
}

fn norm(layout: Layout) -> (usize, usize) {
    let size = layout.size().max(1).next_multiple_of(MIN_ALIGN);
    (size, layout.align().max(MIN_ALIGN))
}

impl Heap {
    const fn new() -> Heap {
        Heap {
            head: ptr::null_mut(),
            total: 0,
            used: 0,
            grows: 0,
        }
    }

    /// Insert `[addr, addr+size)` into the free list, merging with neighbours.
    ///
    /// # Safety
    /// The range must be unused heap memory, 16-byte aligned, size a multiple of 16.
    unsafe fn insert(&mut self, addr: usize, size: usize) {
        debug_assert!(
            addr.is_multiple_of(MIN_ALIGN) && size.is_multiple_of(MIN_ALIGN) && size >= MIN_ALIGN
        );
        // SAFETY: list nodes are valid free blocks by construction.
        unsafe {
            let mut prev: *mut Node = ptr::null_mut();
            let mut cur = self.head;
            while !cur.is_null() && (cur as usize) < addr {
                prev = cur;
                cur = (*cur).next;
            }
            debug_assert!(prev.is_null() || (prev as usize) + (*prev).size <= addr);
            debug_assert!(cur.is_null() || addr + size <= cur as usize);

            let node = addr as *mut Node;
            let mut node_size = size;
            let mut next = cur;
            // Merge with the following block.
            if !cur.is_null() && addr + size == cur as usize {
                node_size += (*cur).size;
                next = (*cur).next;
            }
            // Merge with the preceding block.
            if !prev.is_null() && (prev as usize) + (*prev).size == addr {
                (*prev).size += node_size;
                (*prev).next = next;
                return;
            }
            node.write(Node {
                size: node_size,
                next,
            });
            if prev.is_null() {
                self.head = node;
            } else {
                (*prev).next = node;
            }
        }
    }

    fn alloc(&mut self, size: usize, align: usize) -> *mut u8 {
        // SAFETY: list nodes are valid free blocks by construction.
        unsafe {
            let mut link: *mut *mut Node = &raw mut self.head;
            while !(*link).is_null() {
                let blk = *link;
                let start = blk as usize;
                let end = start + (*blk).size;
                let aligned = start.next_multiple_of(align);
                if aligned + size <= end {
                    let next = (*blk).next;
                    let front = aligned - start;
                    let back = end - (aligned + size);
                    // Unlink, then put back the leftover pieces (front first so
                    // the list stays address-ordered).
                    let mut after = next;
                    if back > 0 {
                        let b = (aligned + size) as *mut Node;
                        b.write(Node {
                            size: back,
                            next: after,
                        });
                        after = b;
                    }
                    if front > 0 {
                        let f = start as *mut Node;
                        f.write(Node {
                            size: front,
                            next: after,
                        });
                        after = f;
                    }
                    *link = after;
                    self.used += size;
                    return aligned as *mut u8;
                }
                link = &raw mut (*blk).next;
            }
        }
        ptr::null_mut()
    }

    fn dealloc(&mut self, p: *mut u8, size: usize) {
        // SAFETY: `p` came from `alloc` with the same normalised size.
        unsafe { self.insert(p as usize, size) };
        self.used -= size;
    }

    /// Add a fresh chunk of `frames` contiguous frames at `pa`.
    fn add_chunk(&mut self, pa: PhysAddr, frames: usize) {
        let bytes = frames * PAGE_SIZE;
        // SAFETY: freshly allocated, RAM is mapped, page aligned.
        unsafe { self.insert(phys_to_virt(pa), bytes) };
        self.total += bytes;
    }

    fn grow_for(&mut self, size: usize, align: usize) -> bool {
        // Worst case the block start is `align` short of aligned.
        let need = size + if align > PAGE_SIZE { align } else { 0 };
        let frames = need.div_ceil(PAGE_SIZE).max(GROW_FRAMES);
        let Some(pa) = pmm::alloc_contiguous(frames, 1) else {
            return false;
        };
        self.add_chunk(pa, frames);
        self.grows += 1;
        true
    }

    fn stats(&self) -> HeapStats {
        let mut free = 0;
        let mut blocks = 0;
        let mut largest = 0;
        let mut cur = self.head;
        // SAFETY: walking our own list.
        unsafe {
            while !cur.is_null() {
                free += (*cur).size;
                blocks += 1;
                largest = largest.max((*cur).size);
                cur = (*cur).next;
            }
        }
        HeapStats {
            total: self.total,
            used: self.used,
            free,
            free_blocks: blocks,
            largest_free: largest,
            grows: self.grows,
        }
    }
}

struct KernelHeap(SpinLock<Heap>);

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let (size, align) = norm(layout);
        let mut heap = self.0.lock();
        let p = heap.alloc(size, align);
        if !p.is_null() {
            return p;
        }
        if heap.grow_for(size, align) {
            return heap.alloc(size, align);
        }
        ptr::null_mut()
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        let (size, _) = norm(layout);
        self.0.lock().dealloc(p, size);
    }
}

#[global_allocator]
static HEAP: KernelHeap = KernelHeap(SpinLock::new(Heap::new()));

/// Give the heap its first chunk. Call after `pmm::init`.
pub fn init() {
    let pa = pmm::alloc_contiguous(INITIAL_FRAMES, 1).expect("heap: no memory for initial chunk");
    HEAP.0.lock().add_chunk(pa, INITIAL_FRAMES);
}

pub fn stats() -> HeapStats {
    HEAP.0.lock().stats()
}
