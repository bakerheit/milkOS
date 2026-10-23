//! AES-128 (FIPS 197), encryption only (GCM and CTR never decrypt a block), bitsliced and
//! constant time: no table is indexed by key or data. Four blocks go through the cipher at
//! once as eight 64-bit words (bit `i` of every byte of the four blocks in word `i`), the
//! S-box is the Boyar-Peralta circuit (XOR / AND / NOT only) and the key schedule runs its
//! S-boxes through the same circuit. This is the layout of BearSSL's `aes_ct64`.
//!
//! [`selftest`]: FIPS 197 Appendix B and C.1, one block and four at once.

use core::sync::atomic::{Ordering, compiler_fence};

/// The S-box as a circuit over bit planes (q[0] = least significant bit of every byte).
fn sbox(q: &mut [u64; 8]) {
    let x0 = q[7];
    let x1 = q[6];
    let x2 = q[5];
    let x3 = q[4];
    let x4 = q[3];
    let x5 = q[2];
    let x6 = q[1];
    let x7 = q[0];

    // Top linear transformation.
    let y14 = x3 ^ x5;
    let y13 = x0 ^ x6;
    let y9 = x0 ^ x3;
    let y8 = x0 ^ x5;
    let t0 = x1 ^ x2;
    let y1 = t0 ^ x7;
    let y4 = y1 ^ x3;
    let y12 = y13 ^ y14;
    let y2 = y1 ^ x0;
    let y5 = y1 ^ x6;
    let y3 = y5 ^ y8;
    let t1 = x4 ^ y12;
    let y15 = t1 ^ x5;
    let y20 = t1 ^ x1;
    let y6 = y15 ^ x7;
    let y10 = y15 ^ t0;
    let y11 = y20 ^ y9;
    let y7 = x7 ^ y11;
    let y17 = y10 ^ y11;
    let y19 = y10 ^ y8;
    let y16 = t0 ^ y11;
    let y21 = y13 ^ y16;
    let y18 = x0 ^ y16;

    // Non-linear section (inversion in GF(2^8) over a tower field).
    let t2 = y12 & y15;
    let t3 = y3 & y6;
    let t4 = t3 ^ t2;
    let t5 = y4 & x7;
    let t6 = t5 ^ t2;
    let t7 = y13 & y16;
    let t8 = y5 & y1;
    let t9 = t8 ^ t7;
    let t10 = y2 & y7;
    let t11 = t10 ^ t7;
    let t12 = y9 & y11;
    let t13 = y14 & y17;
    let t14 = t13 ^ t12;
    let t15 = y8 & y10;
    let t16 = t15 ^ t12;
    let t17 = t4 ^ t14;
    let t18 = t6 ^ t16;
    let t19 = t9 ^ t14;
    let t20 = t11 ^ t16;
    let t21 = t17 ^ y20;
    let t22 = t18 ^ y19;
    let t23 = t19 ^ y21;
    let t24 = t20 ^ y18;

    let t25 = t21 ^ t22;
    let t26 = t21 & t23;
    let t27 = t24 ^ t26;
    let t28 = t25 & t27;
    let t29 = t28 ^ t22;
    let t30 = t23 ^ t24;
    let t31 = t22 ^ t26;
    let t32 = t31 & t30;
    let t33 = t32 ^ t24;
    let t34 = t23 ^ t33;
    let t35 = t27 ^ t33;
    let t36 = t24 & t35;
    let t37 = t36 ^ t34;
    let t38 = t27 ^ t36;
    let t39 = t29 & t38;
    let t40 = t25 ^ t39;

    let t41 = t40 ^ t37;
    let t42 = t29 ^ t33;
    let t43 = t29 ^ t40;
    let t44 = t33 ^ t37;
    let t45 = t42 ^ t41;
    let z0 = t44 & y15;
    let z1 = t37 & y6;
    let z2 = t33 & x7;
    let z3 = t43 & y16;
    let z4 = t40 & y1;
    let z5 = t29 & y7;
    let z6 = t42 & y11;
    let z7 = t45 & y17;
    let z8 = t41 & y10;
    let z9 = t44 & y12;
    let z10 = t37 & y3;
    let z11 = t33 & y4;
    let z12 = t43 & y13;
    let z13 = t40 & y5;
    let z14 = t29 & y2;
    let z15 = t42 & y9;
    let z16 = t45 & y14;
    let z17 = t41 & y8;

    // Bottom linear transformation.
    let t46 = z15 ^ z16;
    let t47 = z10 ^ z11;
    let t48 = z5 ^ z13;
    let t49 = z9 ^ z10;
    let t50 = z2 ^ z12;
    let t51 = z2 ^ z5;
    let t52 = z7 ^ z8;
    let t53 = z0 ^ z3;
    let t54 = z6 ^ z7;
    let t55 = z16 ^ z17;
    let t56 = z12 ^ t48;
    let t57 = t50 ^ t53;
    let t58 = z4 ^ t46;
    let t59 = z3 ^ t54;
    let t60 = t46 ^ t57;
    let t61 = z14 ^ t57;
    let t62 = t52 ^ t58;
    let t63 = t49 ^ t58;
    let t64 = z4 ^ t59;
    let t65 = t61 ^ t62;
    let t66 = z1 ^ t63;
    let s0 = t59 ^ t63;
    let s6 = t56 ^ !t62;
    let s7 = t48 ^ !t60;
    let t67 = t64 ^ t65;
    let s3 = t53 ^ t66;
    let s4 = t51 ^ t66;
    let s5 = t47 ^ t65;
    let s1 = t64 ^ !s3;
    let s2 = t55 ^ !t67;

    q[7] = s0;
    q[6] = s1;
    q[5] = s2;
    q[4] = s3;
    q[3] = s4;
    q[2] = s5;
    q[1] = s6;
    q[0] = s7;
}

/// Transpose between "one word per pair of blocks" and "one word per bit plane".
/// It is its own inverse.
fn ortho(q: &mut [u64; 8]) {
    fn swap(q: &mut [u64; 8], a: usize, b: usize, cl: u64, ch: u64, s: u32) {
        let (x, y) = (q[a], q[b]);
        q[a] = (x & cl) | ((y & cl) << s);
        q[b] = ((x & ch) >> s) | (y & ch);
    }
    const C2: (u64, u64) = (0x5555_5555_5555_5555, 0xAAAA_AAAA_AAAA_AAAA);
    const C4: (u64, u64) = (0x3333_3333_3333_3333, 0xCCCC_CCCC_CCCC_CCCC);
    const C8: (u64, u64) = (0x0F0F_0F0F_0F0F_0F0F, 0xF0F0_F0F0_F0F0_F0F0);
    for (a, b) in [(0, 1), (2, 3), (4, 5), (6, 7)] {
        swap(q, a, b, C2.0, C2.1, 1);
    }
    for (a, b) in [(0, 2), (1, 3), (4, 6), (5, 7)] {
        swap(q, a, b, C4.0, C4.1, 2);
    }
    for (a, b) in [(0, 4), (1, 5), (2, 6), (3, 7)] {
        swap(q, a, b, C8.0, C8.1, 4);
    }
}

/// Spread one block (four little-endian words) over two 64-bit words.
fn interleave_in(w: &[u32]) -> (u64, u64) {
    let mut x = [w[0] as u64, w[1] as u64, w[2] as u64, w[3] as u64];
    for v in x.iter_mut() {
        *v |= *v << 16;
        *v &= 0x0000_FFFF_0000_FFFF;
        *v |= *v << 8;
        *v &= 0x00FF_00FF_00FF_00FF;
    }
    (x[0] | (x[2] << 8), x[1] | (x[3] << 8))
}

/// The inverse of [`interleave_in`].
fn interleave_out(w: &mut [u32], q0: u64, q1: u64) {
    let mut x = [
        q0 & 0x00FF_00FF_00FF_00FF,
        q1 & 0x00FF_00FF_00FF_00FF,
        (q0 >> 8) & 0x00FF_00FF_00FF_00FF,
        (q1 >> 8) & 0x00FF_00FF_00FF_00FF,
    ];
    for (i, v) in x.iter_mut().enumerate() {
        *v |= *v >> 8;
        *v &= 0x0000_FFFF_0000_FFFF;
        w[i] = (*v as u32) | ((*v >> 16) as u32);
    }
}

fn shift_rows(q: &mut [u64; 8]) {
    for x in q.iter_mut() {
        let v = *x;
        *x = (v & 0x0000_0000_0000_FFFF)
            | ((v & 0x0000_0000_FFF0_0000) >> 4)
            | ((v & 0x0000_0000_000F_0000) << 12)
            | ((v & 0x0000_FF00_0000_0000) >> 8)
            | ((v & 0x0000_00FF_0000_0000) << 8)
            | ((v & 0xF000_0000_0000_0000) >> 12)
            | ((v & 0x0FFF_0000_0000_0000) << 4);
    }
}

fn mix_columns(q: &mut [u64; 8]) {
    let [q0, q1, q2, q3, q4, q5, q6, q7] = *q;
    let r = |x: u64| x.rotate_right(16);
    let rot32 = |x: u64| x.rotate_right(32);
    let (r0, r1, r2, r3, r4, r5, r6, r7) = (r(q0), r(q1), r(q2), r(q3), r(q4), r(q5), r(q6), r(q7));
    q[0] = q7 ^ r7 ^ r0 ^ rot32(q0 ^ r0);
    q[1] = q0 ^ r0 ^ q7 ^ r7 ^ r1 ^ rot32(q1 ^ r1);
    q[2] = q1 ^ r1 ^ r2 ^ rot32(q2 ^ r2);
    q[3] = q2 ^ r2 ^ q7 ^ r7 ^ r3 ^ rot32(q3 ^ r3);
    q[4] = q3 ^ r3 ^ q7 ^ r7 ^ r4 ^ rot32(q4 ^ r4);
    q[5] = q4 ^ r4 ^ r5 ^ rot32(q5 ^ r5);
    q[6] = q5 ^ r5 ^ r6 ^ rot32(q6 ^ r6);
    q[7] = q6 ^ r6 ^ r7 ^ rot32(q7 ^ r7);
}

fn add_round_key(q: &mut [u64; 8], k: &[u64]) {
    for (x, y) in q.iter_mut().zip(k) {
        *x ^= y;
    }
}

/// Zero words so the stores are not optimised away (the slice escapes through `black_box`).
pub(crate) fn wipe_u64(w: &mut [u64]) {
    for x in w.iter_mut() {
        *x = 0;
    }
    core::hint::black_box(&mut *w);
    compiler_fence(Ordering::SeqCst);
}

pub(crate) fn wipe_u32(w: &mut [u32]) {
    for x in w.iter_mut() {
        *x = 0;
    }
    core::hint::black_box(&mut *w);
    compiler_fence(Ordering::SeqCst);
}

const ROUNDS: usize = 10;

/// An expanded AES-128 key: the 11 round keys, bitsliced (eight words each, the same key
/// in all four block lanes). Wiped on drop.
pub struct Aes128 {
    rk: [u64; 8 * (ROUNDS + 1)],
}

impl Drop for Aes128 {
    fn drop(&mut self) {
        wipe_u64(&mut self.rk);
    }
}

/// SubWord of the key schedule, through the bitsliced S-box.
fn sub_word(x: u32) -> u32 {
    let mut q = [0u64; 8];
    q[0] = x as u64;
    ortho(&mut q);
    sbox(&mut q);
    ortho(&mut q);
    let r = q[0] as u32;
    wipe_u64(&mut q);
    r
}

impl Aes128 {
    pub fn new(key: &[u8; 16]) -> Aes128 {
        const RCON: [u32; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1B, 0x36];
        // The 44 words of the FIPS 197 expansion, little-endian (so RotWord is a right
        // rotation and Rcon goes in the low byte).
        let mut w = [0u32; 44];
        for i in 0..4 {
            w[i] = u32::from_le_bytes([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
        }
        for i in 4..44 {
            let mut t = w[i - 1];
            if i % 4 == 0 {
                t = sub_word(t.rotate_right(8)) ^ RCON[i / 4 - 1];
            }
            w[i] = t ^ w[i - 4];
        }
        let mut rk = [0u64; 8 * (ROUNDS + 1)];
        for r in 0..=ROUNDS {
            let (a, b) = interleave_in(&w[4 * r..4 * r + 4]);
            let mut q = [a, a, a, a, b, b, b, b];
            ortho(&mut q);
            rk[8 * r..8 * r + 8].copy_from_slice(&q);
            wipe_u64(&mut q);
        }
        wipe_u32(&mut w);
        Aes128 { rk }
    }

    /// The cipher on bit planes (four blocks).
    fn rounds(&self, q: &mut [u64; 8]) {
        add_round_key(q, &self.rk[..8]);
        for r in 1..ROUNDS {
            sbox(q);
            shift_rows(q);
            mix_columns(q);
            add_round_key(q, &self.rk[8 * r..8 * r + 8]);
        }
        sbox(q);
        shift_rows(q);
        add_round_key(q, &self.rk[8 * ROUNDS..]);
    }

    /// Encrypt four blocks given as sixteen little-endian words (block `i` in words
    /// `4i..4i+4`), in place.
    pub(crate) fn encrypt_words(&self, w: &mut [u32; 16]) {
        let mut q = [0u64; 8];
        for i in 0..4 {
            let (a, b) = interleave_in(&w[4 * i..4 * i + 4]);
            q[i] = a;
            q[i + 4] = b;
        }
        ortho(&mut q);
        self.rounds(&mut q);
        ortho(&mut q);
        for i in 0..4 {
            interleave_out(&mut w[4 * i..4 * i + 4], q[i], q[i + 4]);
        }
        wipe_u64(&mut q);
    }

    /// Encrypt four blocks in place (the batch the bitsliced cipher does in one pass).
    pub fn encrypt4(&self, blocks: &mut [[u8; 16]; 4]) {
        let mut w = [0u32; 16];
        for (i, b) in blocks.iter().enumerate() {
            for j in 0..4 {
                w[4 * i + j] =
                    u32::from_le_bytes([b[4 * j], b[4 * j + 1], b[4 * j + 2], b[4 * j + 3]]);
            }
        }
        self.encrypt_words(&mut w);
        for (i, b) in blocks.iter_mut().enumerate() {
            for j in 0..4 {
                b[4 * j..4 * j + 4].copy_from_slice(&w[4 * i + j].to_le_bytes());
            }
        }
        wipe_u32(&mut w);
    }

    /// Encrypt one block in place (the other three lanes carry zeros).
    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        let mut b = [*block, [0; 16], [0; 16], [0; 16]];
        self.encrypt4(&mut b);
        *block = b[0];
        for x in b.iter_mut() {
            crate::wipe(x);
        }
    }
}

/// FIPS 197 Appendix B and C.1, as single blocks and as one four-block batch.
pub fn selftest() -> (usize, usize) {
    let cases: [(&str, &str, &str); 2] = [
        (
            "2b7e151628aed2a6abf7158809cf4f3c",
            "3243f6a8885a308d313198a2e0370734",
            "3925841d02dc09fbdc118597196a0b32",
        ),
        (
            "000102030405060708090a0b0c0d0e0f",
            "00112233445566778899aabbccddeeff",
            "69c4e0d86a7b0430d8cdb78070b4c55a",
        ),
    ];
    let mut ok = 0;
    let mut total = 0;
    for (k, p, c) in cases {
        let (Some(k), Some(p)) = (crate::unhex::<16>(k), crate::unhex::<16>(p)) else {
            total += 2;
            continue;
        };
        let aes = Aes128::new(&k);
        let mut b = p;
        aes.encrypt_block(&mut b);
        total += 1;
        ok += (crate::hex(&b) == c) as usize;
        // The same block in every lane, and in lane 2 between other data.
        let mut four = [p, [0x5a; 16], p, p];
        aes.encrypt4(&mut four);
        let mut z = [0x5a; 16];
        aes.encrypt_block(&mut z);
        total += 1;
        ok += (crate::hex(&four[0]) == c
            && crate::hex(&four[2]) == c
            && crate::hex(&four[3]) == c
            && four[1] == z) as usize;
    }
    (ok, total)
}

#[cfg(test)]
#[allow(clippy::needless_range_loop)] // test-only reference code, written like the standards
pub(crate) mod tests {
    use super::*;

    // A straightforward table AES-128, for tests only: the S-box computed from its
    // definition (inverse in GF(2^8), then the affine map).
    fn gmul(mut a: u8, mut b: u8) -> u8 {
        let mut p = 0u8;
        while b != 0 {
            if b & 1 != 0 {
                p ^= a;
            }
            let hi = a & 0x80;
            a <<= 1;
            if hi != 0 {
                a ^= 0x1b;
            }
            b >>= 1;
        }
        p
    }

    fn sbox_table() -> [u8; 256] {
        static T: std::sync::OnceLock<[u8; 256]> = std::sync::OnceLock::new();
        *T.get_or_init(sbox_table_slow)
    }

    fn sbox_table_slow() -> [u8; 256] {
        let mut t = [0u8; 256];
        for x in 0..256usize {
            let inv = if x == 0 {
                0
            } else {
                (1..=255u8).find(|&y| gmul(x as u8, y) == 1).unwrap()
            };
            let mut s = inv;
            for i in 1..5 {
                s ^= inv.rotate_left(i);
            }
            t[x] = s ^ 0x63;
        }
        t
    }

    pub(crate) fn reference(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
        let sb = sbox_table();
        let mut rk = [[0u8; 16]; 11];
        rk[0] = *key;
        let mut rcon = 1u8;
        for r in 1..11 {
            let p = rk[r - 1];
            let mut t = [
                sb[p[13] as usize],
                sb[p[14] as usize],
                sb[p[15] as usize],
                sb[p[12] as usize],
            ];
            t[0] ^= rcon;
            rcon = gmul(rcon, 2);
            for i in 0..16 {
                let v = if i < 4 { t[i] } else { rk[r][i - 4] };
                rk[r][i] = p[i] ^ v;
            }
        }
        let mut s = *block;
        for i in 0..16 {
            s[i] ^= rk[0][i];
        }
        for r in 1..11 {
            for x in s.iter_mut() {
                *x = sb[*x as usize];
            }
            let t = s;
            for c in 0..4 {
                for row in 0..4 {
                    s[4 * c + row] = t[4 * ((c + row) % 4) + row];
                }
            }
            if r != 10 {
                for c in 0..4 {
                    let a = [s[4 * c], s[4 * c + 1], s[4 * c + 2], s[4 * c + 3]];
                    for row in 0..4 {
                        s[4 * c + row] = gmul(a[row], 2)
                            ^ gmul(a[(row + 1) % 4], 3)
                            ^ a[(row + 2) % 4]
                            ^ a[(row + 3) % 4];
                    }
                }
            }
            for i in 0..16 {
                s[i] ^= rk[r][i];
            }
        }
        s
    }

    #[test]
    fn fips197() {
        let (p, t) = selftest();
        assert_eq!((p, t), (4, 4));
    }

    #[test]
    fn sbox_circuit_matches_table() {
        let sb = sbox_table();
        // All 256 inputs through the circuit, 64 lanes per pass.
        for base in (0..256).step_by(64) {
            let mut q = [0u64; 8];
            for lane in 0..64 {
                let x = (base + lane) as u8;
                for bit in 0..8 {
                    q[bit] |= (((x >> bit) & 1) as u64) << lane;
                }
            }
            sbox(&mut q);
            for lane in 0..64 {
                let mut y = 0u8;
                for bit in 0..8 {
                    y |= (((q[bit] >> lane) & 1) as u8) << bit;
                }
                assert_eq!(y, sb[base + lane], "S({})", base + lane);
            }
        }
    }

    #[test]
    fn random_against_reference() {
        let mut s = 0x1234_5678_9abc_def0u64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for _ in 0..200 {
            let mut key = [0u8; 16];
            let mut blocks = [[0u8; 16]; 4];
            for b in key.iter_mut() {
                *b = next() as u8;
            }
            for bl in blocks.iter_mut() {
                for b in bl.iter_mut() {
                    *b = next() as u8;
                }
            }
            let aes = Aes128::new(&key);
            let want: [[u8; 16]; 4] = core::array::from_fn(|i| reference(&key, &blocks[i]));
            let mut got = blocks;
            aes.encrypt4(&mut got);
            assert_eq!(got, want);
            let mut one = blocks[1];
            aes.encrypt_block(&mut one);
            assert_eq!(one, want[1]);
        }
    }

    #[test]
    fn fips197_reference_itself() {
        // The test-only reference agrees with FIPS 197 C.1, so comparing against it means something.
        let k = crate::unhex::<16>("000102030405060708090a0b0c0d0e0f").unwrap();
        let p = crate::unhex::<16>("00112233445566778899aabbccddeeff").unwrap();
        assert_eq!(
            crate::hex(&reference(&k, &p)),
            "69c4e0d86a7b0430d8cdb78070b4c55a"
        );
    }
}
