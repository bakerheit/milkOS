//! scrypt (RFC 7914), the memory-hard password KDF: Salsa20/8, BlockMix, ROMix over
//! PBKDF2-HMAC-SHA256. The big array (N blocks of 128 r bytes) comes from a [`Scratch`],
//! so a caller can put it where it likes (the kernel: page frames, handed back after;
//! [`VecScratch`] otherwise). [`selftest`]: RFC 7914 section 12, both vectors.

use alloc::vec::Vec;

use crate::hmac::pbkdf2_sha256;

/// Where ROMix keeps its N blocks of `words` u32s.
pub trait Scratch {
    fn block(&mut self, i: usize) -> &mut [u32];
}

/// The array on the heap.
pub struct VecScratch {
    v: Vec<u32>,
    words: usize,
}

impl VecScratch {
    pub fn new(n: usize, words: usize) -> Option<VecScratch> {
        let len = n.checked_mul(words)?;
        Some(VecScratch {
            v: alloc::vec![0u32; len],
            words,
        })
    }
}

impl Scratch for VecScratch {
    fn block(&mut self, i: usize) -> &mut [u32] {
        &mut self.v[i * self.words..(i + 1) * self.words]
    }
}

impl Drop for VecScratch {
    fn drop(&mut self) {
        for w in self.v.iter_mut() {
            // SAFETY: a plain write through a valid &mut.
            unsafe { core::ptr::write_volatile(w, 0) };
        }
    }
}

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
        let dst = if i % 2 == 0 { i / 2 } else { r + i / 2 };
        y[dst * 16..dst * 16 + 16].copy_from_slice(&x);
    }
}

fn romix(b: &mut [u32], v: &mut dyn Scratch, n: usize, r: usize) {
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

/// Sane limits: N = 2^1..2^24, r <= 32 (a block fits a 4 KiB page), p <= 64.
pub fn params_ok(log_n: u8, r: u32, p: u32) -> bool {
    (1..=24).contains(&log_n) && (1..=32).contains(&r) && (1..=64).contains(&p)
}

/// scrypt(pass, salt, N = 2^log_n, r, p) into `out`, with the array from `scratch(N,
/// words per block)`. None: bad parameters or no memory.
pub fn scrypt_with<S: Scratch>(
    pass: &[u8],
    salt: &[u8],
    log_n: u8,
    r: u32,
    p: u32,
    out: &mut [u8],
    scratch: impl FnOnce(usize, usize) -> Option<S>,
) -> Option<()> {
    if !params_ok(log_n, r, p) {
        return None;
    }
    let (n, r, p) = (1usize << log_n, r as usize, p as usize);
    let blen = 128 * r;
    let mut v = scratch(n, blen / 4)?;
    let mut b = alloc::vec![0u8; p * blen];
    pbkdf2_sha256(pass, salt, 1, &mut b);
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
    crate::wipe(&mut b);
    Some(())
}

/// scrypt with the array on the heap.
pub fn scrypt(pass: &[u8], salt: &[u8], log_n: u8, r: u32, p: u32, out: &mut [u8]) -> Option<()> {
    scrypt_with(pass, salt, log_n, r, p, out, VecScratch::new)
}

pub fn selftest() -> (usize, usize) {
    let mut ok = 0;
    let mut out = [0u8; 64];
    ok += (scrypt(b"", b"", 4, 1, 1, &mut out).is_some()
        && out[..]
            == crate::unhex_vec(
                "77d6576238657b203b19ca42c18a0497f16b4844e3074ae8dfdffa3fede21442fcd0069ded0948f8326a753a0fc81f17e8d3e0fb2e0d3628cf35e20c38d18906",
            )[..]) as usize;
    ok += (scrypt(b"password", b"NaCl", 10, 8, 16, &mut out).is_some()
        && out[..]
            == crate::unhex_vec(
                "fdbabe1c9d3472007856e7190d01e9fe7c6ad7cbc8237830e77376634b3731622eaf30d92e22a3886ff109279d9830dac727afb94a83ee6d8360cbdfa2cc0640",
            )[..]) as usize;
    (ok, 2)
}
