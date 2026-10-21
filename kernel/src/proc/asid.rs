//! Address-space IDs. 8-bit ASIDs (TCR_EL1.AS = 0), so up to 255 live user address spaces;
//! ASID 0 is the kernel's (kernel-only threads, no user mappings). An ASID's TLB entries
//! are flushed when it is freed, so a reused ASID never sees stale translations and a
//! context switch never needs a flush.

use crate::sync::SpinLock;

const COUNT: usize = 256;

static USED: SpinLock<[u64; COUNT / 64]> = SpinLock::new([1, 0, 0, 0]); // ASID 0 reserved

pub struct Asid(u16);

impl Asid {
    pub fn alloc() -> Option<Asid> {
        let mut used = USED.lock();
        for (w, word) in used.iter_mut().enumerate() {
            if *word != !0 {
                let bit = (!*word).trailing_zeros() as usize;
                *word |= 1 << bit;
                return Some(Asid((w * 64 + bit) as u16));
            }
        }
        None
    }

    pub fn get(&self) -> u16 {
        self.0
    }

    /// Value to OR into TTBR0_EL1.
    pub fn ttbr_bits(&self) -> u64 {
        (self.0 as u64) << 48
    }
}

impl Drop for Asid {
    fn drop(&mut self) {
        // SAFETY: TLB maintenance for this ASID only.
        unsafe {
            core::arch::asm!(
                "dsb ishst",
                "tlbi aside1is, {0}",
                "dsb ish",
                "isb",
                in(reg) self.ttbr_bits(),
                options(nostack)
            );
        }
        let (w, b) = (self.0 as usize / 64, self.0 as usize % 64);
        USED.lock()[w] &= !(1 << b);
    }
}

pub fn in_use() -> usize {
    USED.lock()
        .iter()
        .map(|w| w.count_ones() as usize)
        .sum::<usize>()
        - 1
}
