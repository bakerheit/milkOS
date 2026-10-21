//! Cross-CPU TLB shootdown for multi-threaded processes.
//!
//! All TLB *invalidation* already uses the inner-shareable broadcast form (`tlbi …is`),
//! which reaches every CPU, so a page's stale translations are gone everywhere after the
//! `dsb ish` that follows. What a broadcast can't do by itself is tell us that no other CPU
//! is still *mid page-table walk* through a table we're about to free. For a single-threaded
//! process only one CPU ever walks its TTBR0, so nothing is needed. Once a process has more
//! than one thread, before we free any page-table structure we send every other CPU an IPI
//! (SGI 1) and wait for it to acknowledge: by the time it does, it has taken an exception
//! and so finished any walk that was in flight.
//!
//! Leaf frames freed after `unmap_page` (which does `tlbi vaae1is; dsb ish`) need no IPI:
//! the `dsb ish` already waits for in-flight walks that used the now-removed entry, and
//! two threads' syscalls into the same address space are serialised by the process lock.
//! The IPI is for freeing the tables themselves (address-space teardown).

use core::sync::atomic::{AtomicU64, Ordering};

use crate::percpu;

pub const IPI_SYNC: u32 = 1;

static GENERATION: AtomicU64 = AtomicU64::new(0);
static ACKS: AtomicU64 = AtomicU64::new(0);

/// The SGI handler: acknowledge the current shootdown. A `dsb ish` + the exception entry
/// itself guarantee any page-table walk this CPU was doing has completed.
pub fn on_sync_ipi(_irq: u32, _frame: &mut crate::exceptions::TrapFrame) {
    // SAFETY: barrier.
    unsafe { core::arch::asm!("dsb ish", "isb", options(nostack)) };
    ACKS.fetch_add(1, Ordering::Release);
}

/// Make sure every other online CPU has taken an exception (and so is not mid-walk) before
/// the caller frees page-table memory. No-op when this is the only CPU online.
pub fn shootdown() {
    // SAFETY: barrier.
    unsafe { core::arch::asm!("dsb ish", options(nostack)) };
    let others: alloc::vec::Vec<usize> = percpu::online().filter(|&c| c != percpu::id()).collect();
    if others.is_empty() {
        return;
    }
    let _g = crate::sync::IrqGuard::new(); // don't get preempted mid-shootdown
    GENERATION.fetch_add(1, Ordering::Relaxed);
    ACKS.store(0, Ordering::Release);
    for &c in &others {
        crate::gic::send_sgi(IPI_SYNC, 1 << c);
    }
    let want = others.len() as u64;
    let start = crate::timer::counter();
    let limit = crate::timer::frequency(); // ~1 s
    while ACKS.load(Ordering::Acquire) < want {
        core::hint::spin_loop();
        if crate::timer::counter().wrapping_sub(start) > limit {
            break; // a CPU with IRQs masked for a long time; give up rather than hang
        }
    }
}
