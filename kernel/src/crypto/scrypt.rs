//! scrypt (RFC 7914): a slow, memory-hard password KDF. The big array (N blocks of 128 r
//! bytes) lives in page frames taken from the frame allocator and given back at the end,
//! not on the kernel heap (which never shrinks).

use alloc::vec::Vec;

use super::hmac::pbkdf2_sha256;
use crate::pmm;

fn salsa8(b: &mut [u32; 16]) {
    let mut x = *b;
    macro_rules! r {
        ($a:expr, $b:expr, $c:expr, $k:expr) => {
            x[$a] ^= x[$b].wrapping_add(x[$c]).rotate_left($k)
        };
    }
    for _ in 0..4 {
        r!(4, 0, 12, 7);
        r!(8, 4, 0, 9);
        r!(12, 8, 4, 13);
        r!(0, 12, 8, 18);
        r!(9, 5, 1, 7);
        r!(13, 9, 5, 9);
        r!(1, 13, 9, 13);
        r!(5, 1, 13, 18);
        r!(14, 10, 6, 7);
        r!(2, 14, 10, 9);
        r!(6, 2, 14, 13);
        r!(10, 6, 2, 18);
        r!(3, 15, 11, 7);
        r!(7, 3, 15, 9);
        r!(11, 7, 3, 13);
        r!(15, 11, 7, 18);
        r!(1, 0, 3, 7);
        r!(2, 1, 0, 9);
        r!(3, 2, 1, 13);
        r!(0, 3, 2, 18);
        r!(6, 5, 4, 7);
        r!(7, 6, 5, 9);
        r!(4, 7, 6, 13);
        r!(5, 4, 7, 18);
        r!(11, 10, 9, 7);
        r!(8, 11, 10, 9);
        r!(9, 8, 11, 13);
        r!(10, 9, 8, 18);
        r!(12, 15, 14, 7);
        r!(13, 12, 15, 9);
        r!(14, 13, 12, 13);
        r!(15, 14, 13, 18);
    }
    for i in 0..16 {
        b[i] = b[i].wrapping_add(x[i]);
    }
}

/// BlockMix: `b` (2r 64-byte blocks as words) -> `y`.
fn blockmix(b: &[u32], y: &mut [u32], r: usize) {
    let mut x: [u32; 16] = b[(2 * r - 1) * 16..].try_into().unwrap();
    for i in 0..2 * r {
        for k in 0..16 {
            x[k] ^= b[i * 16 + k];
        }
        salsa8(&mut x);
        // Even blocks first, then odd.
        let dst = if i % 2 == 0 { i / 2 } else { r + i / 2 };
        y[dst * 16..dst * 16 + 16].copy_from_slice(&x);
    }
}

/// The big array, in frames.
struct Frames {
    pas: Vec<usize>,
    words_per_block: usize,
    blocks_per_frame: usize,
}

impl Frames {
    fn new(n: usize, words_per_block: usize) -> Option<Frames> {
        let bpf = (4096 / 4) / words_per_block;
        let count = n.div_ceil(bpf);
        let mut pas = Vec::with_capacity(count);
        for _ in 0..count {
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
            words_per_block,
            blocks_per_frame: bpf,
        })
    }

    fn block(&mut self, i: usize) -> &mut [u32] {
        let pa = self.pas[i / self.blocks_per_frame];
        let off = (i % self.blocks_per_frame) * self.words_per_block;
        // SAFETY: a frame we own, mapped in the kernel's linear map; one exclusive borrow.
        unsafe {
            core::slice::from_raw_parts_mut(
                (crate::mmu::phys_to_virt(pa) as *mut u32).add(off),
                self.words_per_block,
            )
        }
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        for &pa in &self.pas {
            // SAFETY: our frame; wipe it before giving it back.
            unsafe { core::ptr::write_bytes(crate::mmu::phys_to_virt(pa) as *mut u8, 0, 4096) };
            pmm::free_frame(pa);
        }
    }
}

fn romix(b: &mut [u32], v: &mut Frames, n: usize, r: usize) {
    let w = 32 * r;
    let mut x = b.to_vec();
    let mut y = alloc::vec![0u32; w];
    for i in 0..n {
        v.block(i).copy_from_slice(&x);
        blockmix(&x, &mut y, r);
        core::mem::swap(&mut x, &mut y);
    }
    for _ in 0..n {
        let j = (x[(2 * r - 1) * 16] as usize) & (n - 1);
        let vj = v.block(j);
        for k in 0..w {
            x[k] ^= vj[k];
        }
        blockmix(&x, &mut y, r);
        core::mem::swap(&mut x, &mut y);
    }
    b.copy_from_slice(&x);
}

/// scrypt(pass, salt, N = 2^log_n, r, p) into `out`. None: bad parameters or no memory.
pub fn scrypt(pass: &[u8], salt: &[u8], log_n: u8, r: u32, p: u32, out: &mut [u8]) -> Option<()> {
    let (r, p) = (r as usize, p as usize);
    if log_n == 0 || log_n > 24 || r == 0 || r > 32 || p == 0 || p > 64 {
        return None;
    }
    let n = 1usize << log_n;
    let blen = 128 * r;
    let mut b = alloc::vec![0u8; p * blen];
    pbkdf2_sha256(pass, salt, 1, &mut b);
    let mut v = Frames::new(n, blen / 4)?;
    let mut words = alloc::vec![0u32; blen / 4];
    for i in 0..p {
        let chunk = &mut b[i * blen..(i + 1) * blen];
        for (k, w) in words.iter_mut().enumerate() {
            *w = u32::from_le_bytes(chunk[4 * k..4 * k + 4].try_into().unwrap());
        }
        romix(&mut words, &mut v, n, r);
        for (k, w) in words.iter().enumerate() {
            chunk[4 * k..4 * k + 4].copy_from_slice(&w.to_le_bytes());
        }
    }
    pbkdf2_sha256(pass, &b, 1, out);
    super::ct::zeroize(&mut b);
    Some(())
}
