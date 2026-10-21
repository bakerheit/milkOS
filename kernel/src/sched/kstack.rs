//! Kernel stacks: 64 KiB, physically contiguous, mapped in the kernel's vmap window with an
//! unmapped guard below.
//!
//! The window ([`mmu::VMAP_BASE`]..) is cut into 128 KiB slots, each aligned to 128 KiB. A
//! stack occupies the *upper* half of its slot; the lower half is never mapped. So:
//! * running off the bottom of a stack hits unmapped memory (a data abort), and
//! * bit [`KSTACK_SHIFT`] of any valid stack pointer is 1, which is what the EL1 exception
//!   vectors test to tell "this abort is a stack overflow" apart from everything else
//!   (see `exceptions.S`). The boot stack is placed the same way by `linker.ld`.
//!
//! The frames are also reachable through the identity map, and [`mmu::virt_to_phys`] knows
//! the window, so a buffer on a kernel stack can still be handed to a DMA device.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::mmu::{self, MapAttrs, PAGE_SIZE, PhysAddr, PmmTables, VMAP_BASE, VMAP_END, VirtAddr};
use crate::pmm;
use crate::sync::SpinLock;

pub const KSTACK_SHIFT: u32 = 16;
pub const KSTACK_SIZE: usize = 1 << KSTACK_SHIFT;
const SLOT_SIZE: usize = 2 * KSTACK_SIZE;
const PAGES: usize = KSTACK_SIZE / PAGE_SIZE;
/// 4096 slots = 512 MiB of window = at most 4096 threads.
const MAX_SLOTS: usize = 4096;

const _: () = assert!(VMAP_BASE.is_multiple_of(SLOT_SIZE));
const _: () = assert!(VMAP_BASE + MAX_SLOTS * SLOT_SIZE <= VMAP_END);

/// Physical base of the stack in each slot; 0 = slot free.
static SLOT_PA: [AtomicUsize; MAX_SLOTS] = [const { AtomicUsize::new(0) }; MAX_SLOTS];
/// Serialises slot allocation. First fit, so slots (and the page tables behind them, which
/// are never freed) get reused instead of creeping through the window.
static ALLOC: SpinLock<()> = SpinLock::new(());

pub struct KStack {
    slot: usize,
    pa: PhysAddr,
}

impl KStack {
    /// Allocate and map a stack. `None` if out of frames or slots.
    pub fn new() -> Option<KStack> {
        let pa = pmm::alloc_contiguous(PAGES, 1)?;
        let slot = {
            let _g = ALLOC.lock();
            let found = (0..MAX_SLOTS).find(|&i| SLOT_PA[i].load(Ordering::Relaxed) == 0);
            match found {
                Some(i) => {
                    SLOT_PA[i].store(pa, Ordering::Relaxed);
                    i
                }
                None => {
                    pmm::free_contiguous(pa, PAGES);
                    return None;
                }
            }
        };
        let stack = KStack { slot, pa };
        let mapped = mmu::with_kernel_space(|s| {
            s.map(
                stack.bottom(),
                pa,
                KSTACK_SIZE,
                MapAttrs::KERNEL_DATA,
                &mut PmmTables,
            )
        });
        if mapped.is_err() {
            SLOT_PA[slot].store(0, Ordering::Relaxed);
            pmm::free_contiguous(pa, PAGES);
            core::mem::forget(stack);
            return None;
        }
        Some(stack)
    }

    /// Lowest usable address.
    pub fn bottom(&self) -> VirtAddr {
        VMAP_BASE + self.slot * SLOT_SIZE + KSTACK_SIZE
    }

    /// Initial stack pointer (one past the highest byte).
    pub fn top(&self) -> VirtAddr {
        self.bottom() + KSTACK_SIZE
    }
}

impl Drop for KStack {
    fn drop(&mut self) {
        let bottom = self.bottom();
        mmu::with_kernel_space(|s| {
            for i in 0..PAGES {
                s.unmap_page(bottom + i * PAGE_SIZE);
            }
        });
        pmm::free_contiguous(self.pa, PAGES);
        SLOT_PA[self.slot].store(0, Ordering::Relaxed);
    }
}

/// Physical address behind a kernel-stack virtual address (for `mmu::virt_to_phys`).
pub fn virt_to_phys(va: VirtAddr) -> PhysAddr {
    let off = va - VMAP_BASE;
    let (slot, within) = (off / SLOT_SIZE, off % SLOT_SIZE);
    let pa = SLOT_PA.get(slot).map_or(0, |s| s.load(Ordering::Relaxed));
    assert!(
        pa != 0 && within >= KSTACK_SIZE,
        "virt_to_phys: {va:#x} is not on a live kernel stack"
    );
    pa + within - KSTACK_SIZE
}
