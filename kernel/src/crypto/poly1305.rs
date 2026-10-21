//! Poly1305 (RFC 8439), 26-bit limbs ("donna-32"): constant time, including the final
//! reduction (masks, not branches).

const M26: u32 = 0x3ff_ffff;

pub struct Poly1305 {
    r: [u32; 5],
    h: [u32; 5],
    pad: [u32; 4],
    buf: [u8; 16],
    n: usize,
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

impl Poly1305 {
    pub fn new(key: &[u8; 32]) -> Poly1305 {
        Poly1305 {
            r: [
                le32(&key[0..]) & 0x3ff_ffff,
                (le32(&key[3..]) >> 2) & 0x3ff_ff03,
                (le32(&key[6..]) >> 4) & 0x3ff_c0ff,
                (le32(&key[9..]) >> 6) & 0x3f0_3fff,
                (le32(&key[12..]) >> 8) & 0x00f_ffff,
            ],
            h: [0; 5],
            pad: [
                le32(&key[16..]),
                le32(&key[20..]),
                le32(&key[24..]),
                le32(&key[28..]),
            ],
            buf: [0; 16],
            n: 0,
        }
    }

    fn block(&mut self, m: &[u8; 16], hibit: u32) {
        let [r0, r1, r2, r3, r4] = self.r.map(|x| x as u64);
        let (s1, s2, s3, s4) = (r1 * 5, r2 * 5, r3 * 5, r4 * 5);
        let h = &mut self.h;
        h[0] += le32(&m[0..]) & M26;
        h[1] += (le32(&m[3..]) >> 2) & M26;
        h[2] += (le32(&m[6..]) >> 4) & M26;
        h[3] += (le32(&m[9..]) >> 6) & M26;
        h[4] += (le32(&m[12..]) >> 8) | hibit;
        let [h0, h1, h2, h3, h4] = h.map(|x| x as u64);
        let d0 = h0 * r0 + h1 * s4 + h2 * s3 + h3 * s2 + h4 * s1;
        let mut d1 = h0 * r1 + h1 * r0 + h2 * s4 + h3 * s3 + h4 * s2;
        let mut d2 = h0 * r2 + h1 * r1 + h2 * r0 + h3 * s4 + h4 * s3;
        let mut d3 = h0 * r3 + h1 * r2 + h2 * r1 + h3 * r0 + h4 * s4;
        let mut d4 = h0 * r4 + h1 * r3 + h2 * r2 + h3 * r1 + h4 * r0;
        let mut c = d0 >> 26;
        h[0] = (d0 as u32) & M26;
        d1 += c;
        c = d1 >> 26;
        h[1] = (d1 as u32) & M26;
        d2 += c;
        c = d2 >> 26;
        h[2] = (d2 as u32) & M26;
        d3 += c;
        c = d3 >> 26;
        h[3] = (d3 as u32) & M26;
        d4 += c;
        c = d4 >> 26;
        h[4] = (d4 as u32) & M26;
        let t = (h[0] as u64) + c * 5;
        h[0] = (t as u32) & M26;
        h[1] += (t >> 26) as u32;
    }

    pub fn update(&mut self, mut data: &[u8]) {
        if self.n > 0 {
            let k = (16 - self.n).min(data.len());
            self.buf[self.n..self.n + k].copy_from_slice(&data[..k]);
            self.n += k;
            data = &data[k..];
            if self.n < 16 {
                return;
            }
            let b = self.buf;
            self.block(&b, 1 << 24);
            self.n = 0;
        }
        while data.len() >= 16 {
            self.block(data[..16].try_into().unwrap(), 1 << 24);
            data = &data[16..];
        }
        self.buf[..data.len()].copy_from_slice(data);
        self.n = data.len();
    }

    pub fn finish(mut self) -> [u8; 16] {
        if self.n > 0 {
            let mut b = [0u8; 16];
            b[..self.n].copy_from_slice(&self.buf[..self.n]);
            b[self.n] = 1;
            self.block(&b, 0);
        }
        let h = &mut self.h;
        let mut c = h[1] >> 26;
        h[1] &= M26;
        for x in h.iter_mut().skip(2) {
            *x += c;
            c = *x >> 26;
            *x &= M26;
        }
        h[0] += c * 5;
        c = h[0] >> 26;
        h[0] &= M26;
        h[1] += c;
        // g = h + 5 - 2^130; use g if h >= p.
        let mut g = [0u32; 5];
        g[0] = h[0].wrapping_add(5);
        c = g[0] >> 26;
        g[0] &= M26;
        for i in 1..4 {
            g[i] = h[i].wrapping_add(c);
            c = g[i] >> 26;
            g[i] &= M26;
        }
        g[4] = h[4].wrapping_add(c).wrapping_sub(1 << 26);
        let mask = (g[4] >> 31).wrapping_sub(1); // all ones if g >= 0
        for i in 0..5 {
            h[i] = (h[i] & !mask) | (g[i] & mask);
        }
        let w0 = h[0] | (h[1] << 26);
        let w1 = (h[1] >> 6) | (h[2] << 20);
        let w2 = (h[2] >> 12) | (h[3] << 14);
        let w3 = (h[3] >> 18) | (h[4] << 8);
        let mut out = [0u8; 16];
        let mut f: u64 = 0;
        for (i, w) in [w0, w1, w2, w3].into_iter().enumerate() {
            f = (w as u64) + (self.pad[i] as u64) + (f >> 32);
            out[4 * i..4 * i + 4].copy_from_slice(&(f as u32).to_le_bytes());
        }
        out
    }
}

/// One-shot Poly1305.
pub fn mac(key: &[u8; 32], data: &[u8]) -> [u8; 16] {
    let mut p = Poly1305::new(key);
    p.update(data);
    p.finish()
}
