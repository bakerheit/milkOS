//! scrypt with its big array in page frames: taken from the frame allocator for the
//! call and handed back (wiped) after, instead of the kernel heap, which never shrinks
//! (a passphrase slot's N = 2^16, r = 8 is 64 MiB). The algorithm is
//! `milkcrypto::scrypt`.

use alloc::vec::Vec;

use crate::pmm;

struct Frames {
    pas: Vec<usize>,
    words: usize,
    per_frame: usize,
}

impl Frames {
    fn new(n: usize, words: usize) -> Option<Frames> {
        let per_frame = (4096 / 4) / words; // r <= 32: a block fits a frame
        let mut pas = Vec::with_capacity(n.div_ceil(per_frame));
        for _ in 0..n.div_ceil(per_frame) {
            match pmm::alloc_frame() {
                Some(pa) => pas.push(pa),
                None => {
                    for pa in pas {
                        pmm::free_frame(pa);
                    }
                    return None;
                }
            }
        }
        Some(Frames {
            pas,
            words,
            per_frame,
        })
    }
}

impl milkcrypto::scrypt::Scratch for Frames {
    fn block(&mut self, i: usize) -> &mut [u32] {
        let pa = self.pas[i / self.per_frame];
        let off = (i % self.per_frame) * self.words;
        // SAFETY: a frame we own, in the kernel's linear map; one borrow at a time.
        unsafe {
            core::slice::from_raw_parts_mut(
                (crate::mmu::phys_to_virt(pa) as *mut u32).add(off),
                self.words,
            )
        }
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        for &pa in &self.pas {
            // SAFETY: our frame; wiped before it goes back.
            unsafe { core::ptr::write_bytes(crate::mmu::phys_to_virt(pa) as *mut u8, 0, 4096) };
            pmm::free_frame(pa);
        }
    }
}

/// scrypt(pass, salt, 2^log_n, r, p) into `out`. None: bad parameters or no memory.
pub fn scrypt(pass: &[u8], salt: &[u8], log_n: u8, r: u32, p: u32, out: &mut [u8]) -> Option<()> {
    milkcrypto::scrypt::scrypt_with(pass, salt, log_n, r, p, out, Frames::new)
}
