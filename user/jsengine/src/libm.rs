//! The transcendental functions, ported from fdlibm (as musl carries them): exp, log,
//! log2, log10, log1p, expm1, pow, sin, cos, tan, asin, acos, atan, atan2, the hyperbolic
//! functions, cbrt and hypot. No std in the guest, so no system libm.

#![allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    clippy::many_single_char_names
)]

use crate::math::{floor, sqrt};

#[inline]
fn hi(x: f64) -> u32 {
    (x.to_bits() >> 32) as u32
}
#[inline]
fn lo(x: f64) -> u32 {
    x.to_bits() as u32
}
#[inline]
fn words(h: u32, l: u32) -> f64 {
    f64::from_bits(((h as u64) << 32) | l as u64)
}
#[inline]
fn with_lo(x: f64, l: u32) -> f64 {
    words(hi(x), l)
}

pub(crate) fn scalbn(x: f64, mut n: i32) -> f64 {
    let x1p1023 = f64::from_bits(0x7fe0000000000000);
    let x1p53 = f64::from_bits(0x4340000000000000);
    let x1p_1022 = f64::from_bits(0x0010000000000000);
    let mut y = x;
    if n > 1023 {
        y *= x1p1023;
        n -= 1023;
        if n > 1023 {
            y *= x1p1023;
            n -= 1023;
            if n > 1023 {
                n = 1023;
            }
        }
    } else if n < -1022 {
        y *= x1p_1022 * x1p53;
        n += 1022 - 53;
        if n < -1022 {
            y *= x1p_1022 * x1p53;
            n += 1022 - 53;
            if n < -1022 {
                n = -1022;
            }
        }
    }
    y * f64::from_bits(((0x3ff + n) as u64) << 52)
}

// ---- exp ----

pub(crate) fn exp(x: f64) -> f64 {
    const HALF: [f64; 2] = [0.5, -0.5];
    const LN2HI: f64 = 6.93147180369123816490e-01;
    const LN2LO: f64 = 1.90821492927058770002e-10;
    const INVLN2: f64 = 1.44269504088896338700e+00;
    const P1: f64 = 1.66666666666666019037e-01;
    const P2: f64 = -2.77777777770155933842e-03;
    const P3: f64 = 6.61375632143793436117e-05;
    const P4: f64 = -1.65339022054652515390e-06;
    const P5: f64 = 4.13813679705723846039e-08;
    let mut x = x;
    let mut hx = hi(x);
    let sign = (hx >> 31) as usize;
    hx &= 0x7fffffff;
    if hx >= 0x4086232b {
        if x.is_nan() {
            return x;
        }
        if x > 709.782712893383973096e+00 {
            return f64::INFINITY;
        }
        if x < -708.39641853226410622 && x < -745.13321910194110842 {
            return 0.0;
        }
    }
    let k: i32;
    let (hi_, lo_);
    if hx > 0x3fd62e42 {
        if hx >= 0x3ff0a2b2 {
            k = (INVLN2 * x + HALF[sign]) as i32;
        } else {
            k = 1 - sign as i32 - sign as i32;
        }
        hi_ = x - k as f64 * LN2HI;
        lo_ = k as f64 * LN2LO;
        x = hi_ - lo_;
    } else if hx > 0x3e300000 {
        k = 0;
        hi_ = x;
        lo_ = 0.0;
    } else {
        return 1.0 + x;
    }
    let xx = x * x;
    let c = x - xx * (P1 + xx * (P2 + xx * (P3 + xx * (P4 + xx * P5))));
    let y = 1.0 + (x * c / (2.0 - c) - lo_ + hi_);
    if k == 0 { y } else { scalbn(y, k) }
}

// ---- log family ----

const LG1: f64 = 6.666666666666735130e-01;
const LG2: f64 = 3.999999999940941908e-01;
const LG3: f64 = 2.857142874366239149e-01;
const LG4: f64 = 2.222219843214978396e-01;
const LG5: f64 = 1.818357216161805012e-01;
const LG6: f64 = 1.531383769920937332e-01;
const LG7: f64 = 1.479819860511658591e-01;

/// Shared reduction: x = 2^k (1 + f), with special cases returned early.
fn log_reduce(x: f64) -> Result<(i32, f64), f64> {
    let mut x = x;
    let mut ix = x.to_bits();
    let mut hx = (ix >> 32) as u32;
    let mut k: i32 = 0;
    if hx < 0x00100000 || (hx >> 31) != 0 {
        if ix << 1 == 0 {
            return Err(f64::NEG_INFINITY);
        }
        if hx >> 31 != 0 {
            return Err(f64::NAN);
        }
        k -= 54;
        x *= f64::from_bits(0x4350000000000000); // 2^54
        ix = x.to_bits();
        hx = (ix >> 32) as u32;
    } else if hx >= 0x7ff00000 {
        return Err(x);
    } else if hx == 0x3ff00000 && ix << 32 == 0 {
        return Err(0.0);
    }
    hx += 0x3ff00000 - 0x3fe6a09e;
    k += (hx >> 20) as i32 - 0x3ff;
    hx = (hx & 0x000fffff) + 0x3fe6a09e;
    ix = ((hx as u64) << 32) | (ix & 0xffffffff);
    Ok((k, f64::from_bits(ix) - 1.0))
}

pub(crate) fn log(x: f64) -> f64 {
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    let (k, f) = match log_reduce(x) {
        Ok(v) => v,
        Err(v) => return v,
    };
    let hfsq = 0.5 * f * f;
    let s = f / (2.0 + f);
    let z = s * s;
    let w = z * z;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    let r = t2 + t1;
    let dk = k as f64;
    s * (hfsq + r) + dk * LN2_LO - hfsq + f + dk * LN2_HI
}

fn log_hi_lo(f: f64) -> (f64, f64) {
    let hfsq = 0.5 * f * f;
    let s = f / (2.0 + f);
    let z = s * s;
    let w = z * z;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    let r = t2 + t1;
    let mut hi_ = f - hfsq;
    hi_ = f64::from_bits(hi_.to_bits() & 0xffffffff00000000);
    let lo_ = f - hi_ - hfsq + s * (hfsq + r);
    (hi_, lo_)
}

pub(crate) fn log10(x: f64) -> f64 {
    const IVLN10HI: f64 = 4.34294481878168880939e-01;
    const IVLN10LO: f64 = 2.50829467116452752298e-11;
    const LOG10_2HI: f64 = 3.01029995663611771306e-01;
    const LOG10_2LO: f64 = 3.69423907715893078616e-13;
    let (k, f) = match log_reduce(x) {
        Ok(v) => v,
        Err(v) => return if v == 0.0 { 0.0 } else { v },
    };
    let (hi_, lo_) = log_hi_lo(f);
    let mut val_hi = hi_ * IVLN10HI;
    let dk = k as f64;
    let y = dk * LOG10_2HI;
    let mut val_lo = dk * LOG10_2LO + (lo_ + hi_) * IVLN10LO + lo_ * IVLN10HI;
    let w = y + val_hi;
    val_lo += (y - w) + val_hi;
    val_hi = w;
    let r = val_lo + val_hi;
    // Exact for exact powers of ten (as V8's fdlibm port is).
    let rr = crate::math::js_round(r);
    if (r - rr).abs() < 1e-14 && rr.abs() <= 22.0 && pow10(rr as i32) == x {
        return rr;
    }
    r
}

fn pow10(n: i32) -> f64 {
    let mut v = 1.0;
    if n >= 0 {
        for _ in 0..n {
            v *= 10.0;
        }
    } else {
        for _ in 0..(-n) {
            v /= 10.0;
        }
    }
    v
}

pub(crate) fn log2(x: f64) -> f64 {
    const IVLN2HI: f64 = 1.44269504072144627571e+00;
    const IVLN2LO: f64 = 1.67517131648865118353e-10;
    let (k, f) = match log_reduce(x) {
        Ok(v) => v,
        Err(v) => return if v == 0.0 { 0.0 } else { v },
    };
    let (hi_, lo_) = log_hi_lo(f);
    let mut val_hi = hi_ * IVLN2HI;
    let mut val_lo = (lo_ + hi_) * IVLN2LO + lo_ * IVLN2HI;
    let y = k as f64;
    let w = y + val_hi;
    val_lo += (y - w) + val_hi;
    val_hi = w;
    val_lo + val_hi
}

pub(crate) fn log1p(x: f64) -> f64 {
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    let mut ui = x.to_bits();
    let hx = (ui >> 32) as u32;
    let mut k = 1i32;
    let mut c = 0.0;
    let mut f = 0.0;
    if hx < 0x3fda827a || (hx >> 31) != 0 {
        if hx >= 0xbff00000 {
            if x == -1.0 {
                return f64::NEG_INFINITY;
            }
            return f64::NAN;
        }
        if hx << 1 < 0x3ca00000 << 1 {
            return x;
        }
        if hx <= 0xbfd2bec4 {
            k = 0;
            c = 0.0;
            f = x;
        }
    } else if hx >= 0x7ff00000 {
        return x;
    }
    if k != 0 {
        ui = (1.0 + x).to_bits();
        let mut hu = (ui >> 32) as u32;
        hu += 0x3ff00000 - 0x3fe6a09e;
        k = (hu >> 20) as i32 - 0x3ff;
        if k < 54 {
            let uf = f64::from_bits(ui);
            c = if k >= 2 {
                1.0 - (uf - x)
            } else {
                x - (uf - 1.0)
            };
            c /= uf;
        } else {
            c = 0.0;
        }
        hu = (hu & 0x000fffff) + 0x3fe6a09e;
        ui = ((hu as u64) << 32) | (ui & 0xffffffff);
        f = f64::from_bits(ui) - 1.0;
    }
    let hfsq = 0.5 * f * f;
    let s = f / (2.0 + f);
    let z = s * s;
    let w = z * z;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    let r = t2 + t1;
    let dk = k as f64;
    s * (hfsq + r) + (dk * LN2_LO + c) - hfsq + f + dk * LN2_HI
}

pub(crate) fn expm1(x: f64) -> f64 {
    const O_THRESHOLD: f64 = 7.09782712893383973096e+02;
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    const INVLN2: f64 = 1.44269504088896338700e+00;
    const Q1: f64 = -3.33333333333331316428e-02;
    const Q2: f64 = 1.58730158725481460165e-03;
    const Q3: f64 = -7.93650757867487942473e-05;
    const Q4: f64 = 4.00821782732936239552e-06;
    const Q5: f64 = -2.01099218183624371326e-07;
    let mut x = x;
    let hx = hi(x) & 0x7fffffff;
    let sign = hi(x) >> 31 != 0;
    if hx >= 0x4043687A {
        if x.is_nan() {
            return x;
        }
        if sign {
            return -1.0;
        }
        if x > O_THRESHOLD {
            return f64::INFINITY;
        }
    }
    let k: i32;
    let mut c = 0.0;
    if hx > 0x3fd62e42 {
        let (hi_, lo_);
        if hx < 0x3FF0A2B2 {
            if !sign {
                hi_ = x - LN2_HI;
                lo_ = LN2_LO;
                k = 1;
            } else {
                hi_ = x + LN2_HI;
                lo_ = -LN2_LO;
                k = -1;
            }
        } else {
            k = (INVLN2 * x + if sign { -0.5 } else { 0.5 }) as i32;
            let t = k as f64;
            hi_ = x - t * LN2_HI;
            lo_ = t * LN2_LO;
        }
        x = hi_ - lo_;
        c = (hi_ - x) - lo_;
    } else if hx < 0x3c900000 {
        return x;
    } else {
        k = 0;
    }
    let hfx = 0.5 * x;
    let hxs = x * hfx;
    let r1 = 1.0 + hxs * (Q1 + hxs * (Q2 + hxs * (Q3 + hxs * (Q4 + hxs * Q5))));
    let t = 3.0 - r1 * hfx;
    let mut e = hxs * ((r1 - t) / (6.0 - x * t));
    if k == 0 {
        return x - (x * e - hxs);
    }
    e = x * (e - c) - c;
    e -= hxs;
    if k == -1 {
        return 0.5 * (x - e) - 0.5;
    }
    if k == 1 {
        if x < -0.25 {
            return -2.0 * (e - (x + 0.5));
        }
        return 1.0 + 2.0 * (x - e);
    }
    let twopk = f64::from_bits(((0x3ff + k) as u64) << 52);
    if !(0..=56).contains(&k) {
        let mut y = x - e + 1.0;
        if k == 1024 {
            y = y * 2.0 * f64::from_bits(0x7fe0000000000000);
        } else {
            y *= twopk;
        }
        return y - 1.0;
    }
    let uf = f64::from_bits(((0x3ff - k) as u64) << 52);
    let y = if k < 20 {
        (x - e + (1.0 - uf)) * twopk
    } else {
        (x - (e + uf) + 1.0) * twopk
    };
    y
}

// ---- pow ----

pub(crate) fn pow(x: f64, y: f64) -> f64 {
    const BP: [f64; 2] = [1.0, 1.5];
    const DP_H: [f64; 2] = [0.0, 5.84962487220764160156e-01];
    const DP_L: [f64; 2] = [0.0, 1.35003920212974897128e-08];
    const TWO53: f64 = 9007199254740992.0;
    const HUGE: f64 = 1.0e300;
    const TINY: f64 = 1.0e-300;
    const L1: f64 = 5.99999999999994648725e-01;
    const L2: f64 = 4.28571428578550184252e-01;
    const L3: f64 = 3.33333329818377432918e-01;
    const L4: f64 = 2.72728123808534006489e-01;
    const L5: f64 = 2.30660745775561754067e-01;
    const L6: f64 = 2.06975017800338417784e-01;
    const P1: f64 = 1.66666666666666019037e-01;
    const P2: f64 = -2.77777777770155933842e-03;
    const P3: f64 = 6.61375632143793436117e-05;
    const P4: f64 = -1.65339022054652515390e-06;
    const P5: f64 = 4.13813679705723846039e-08;
    const LG2: f64 = 6.93147180559945286227e-01;
    const LG2_H: f64 = 6.93147182464599609375e-01;
    const LG2_L: f64 = -1.90465429995776804525e-09;
    const OVT: f64 = 8.0085662595372944372e-017;
    const CP: f64 = 9.61796693925975554329e-01;
    const CP_H: f64 = 9.61796700954437255859e-01;
    const CP_L: f64 = -7.02846165095275826516e-09;
    const IVLN2: f64 = 1.44269504088896338700e+00;
    const IVLN2_H: f64 = 1.44269502162933349609e+00;
    const IVLN2_L: f64 = 1.92596299112661746887e-08;

    let hx = hi(x) as i32;
    let lx = lo(x);
    let hy = hi(y) as i32;
    let ly = lo(y);
    let mut ix = hx & 0x7fffffff;
    let iy = hy & 0x7fffffff;

    if (iy as u32 | ly) == 0 {
        return 1.0;
    }
    if hx == 0x3ff00000 && lx == 0 {
        return 1.0;
    }
    if ix > 0x7ff00000
        || (ix == 0x7ff00000 && lx != 0)
        || iy > 0x7ff00000
        || (iy == 0x7ff00000 && ly != 0)
    {
        return x + y;
    }
    // yisint: 0 = not an integer, 1 = odd, 2 = even
    let mut yisint = 0;
    if hx < 0 {
        if iy >= 0x43400000 {
            yisint = 2;
        } else if iy >= 0x3ff00000 {
            let k = (iy >> 20) - 0x3ff;
            if k > 20 {
                let j = ly >> (52 - k);
                if (j << (52 - k)) == ly {
                    yisint = 2 - (j & 1) as i32;
                }
            } else if ly == 0 {
                let j = iy >> (20 - k);
                if (j << (20 - k)) == iy {
                    yisint = 2 - (j & 1);
                }
            }
        }
    }
    if ly == 0 {
        if iy == 0x7ff00000 {
            if ((ix - 0x3ff00000) as u32 | lx) == 0 {
                return 1.0;
            } else if ix >= 0x3ff00000 {
                return if hy >= 0 { y } else { 0.0 };
            } else {
                return if hy >= 0 { 0.0 } else { -y };
            }
        }
        if iy == 0x3ff00000 {
            return if hy >= 0 { x } else { 1.0 / x };
        }
        if hy == 0x40000000 {
            return x * x;
        }
        if hy == 0x3fe00000 && hx >= 0 {
            return sqrt(x);
        }
    }
    let mut ax = x.abs();
    if lx == 0 && (ix == 0x7ff00000 || ix == 0 || ix == 0x3ff00000) {
        let mut z = ax;
        if hy < 0 {
            z = 1.0 / z;
        }
        if hx < 0 {
            if ((ix - 0x3ff00000) | yisint) == 0 {
                z = (z - z) / (z - z);
            } else if yisint == 1 {
                z = -z;
            }
        }
        return z;
    }
    let mut s = 1.0;
    if hx < 0 {
        if yisint == 0 {
            return (x - x) / (x - x);
        }
        if yisint == 1 {
            s = -1.0;
        }
    }
    let (t1, t2);
    if iy > 0x41e00000 {
        if iy > 0x43f00000 {
            if ix <= 0x3fefffff {
                return if hy < 0 { HUGE * HUGE } else { TINY * TINY };
            }
            if ix >= 0x3ff00000 {
                return if hy > 0 { HUGE * HUGE } else { TINY * TINY };
            }
        }
        if ix < 0x3fefffff {
            return if hy < 0 {
                s * HUGE * HUGE
            } else {
                s * TINY * TINY
            };
        }
        if ix > 0x3ff00000 {
            return if hy > 0 {
                s * HUGE * HUGE
            } else {
                s * TINY * TINY
            };
        }
        let t = ax - 1.0;
        let w = (t * t) * (0.5 - t * (0.3333333333333333333333 - t * 0.25));
        let u = IVLN2_H * t;
        let v = t * IVLN2_L - w * IVLN2;
        let tt = u + v;
        t1 = with_lo(tt, 0);
        t2 = v - (t1 - u);
    } else {
        let mut n = 0i32;
        if ix < 0x00100000 {
            ax *= TWO53;
            n -= 53;
            ix = hi(ax) as i32;
        }
        n += ((ix) >> 20) - 0x3ff;
        let j = ix & 0x000fffff;
        ix = j | 0x3ff00000;
        let k: usize;
        if j <= 0x3988E {
            k = 0;
        } else if j < 0xBB67A {
            k = 1;
        } else {
            k = 0;
            n += 1;
            ix -= 0x00100000;
        }
        ax = words(ix as u32, lo(ax));
        let u = ax - BP[k];
        let v = 1.0 / (ax + BP[k]);
        let ss = u * v;
        let s_h = with_lo(ss, 0);
        let t_h = words(
            (((ix >> 1) | 0x20000000) + 0x00080000 + ((k as i32) << 18)) as u32,
            0,
        );
        let t_l = ax - (t_h - BP[k]);
        let s_l = v * ((u - s_h * t_h) - s_h * t_l);
        let s2 = ss * ss;
        let mut r = s2 * s2 * (L1 + s2 * (L2 + s2 * (L3 + s2 * (L4 + s2 * (L5 + s2 * L6)))));
        r += s_l * (s_h + ss);
        let s2b = s_h * s_h;
        let t_h2 = with_lo(3.0 + s2b + r, 0);
        let t_l2 = r - ((t_h2 - 3.0) - s2b);
        let u2 = s_h * t_h2;
        let v2 = s_l * t_h2 + t_l2 * ss;
        let p_h = with_lo(u2 + v2, 0);
        let p_l = v2 - (p_h - u2);
        let z_h = CP_H * p_h;
        let z_l = CP_L * p_h + p_l * CP + DP_L[k];
        let t = n as f64;
        let tt = with_lo(((z_h + z_l) + DP_H[k]) + t, 0);
        t1 = tt;
        t2 = z_l - (((tt - t) - DP_H[k]) - z_h);
    }
    let y1 = with_lo(y, 0);
    let p_l = (y - y1) * t1 + y * t2;
    let mut p_h = y1 * t1;
    let mut z = p_l + p_h;
    let j = hi(z) as i32;
    let i = lo(z) as i32;
    if j >= 0x40900000 {
        if ((j - 0x40900000) | i) != 0 {
            return s * HUGE * HUGE;
        }
        if p_l + OVT > z - p_h {
            return s * HUGE * HUGE;
        }
    } else if (j & 0x7fffffff) >= 0x4090cc00 {
        if ((j as u32).wrapping_sub(0xc090cc00) as i32 | i) != 0 {
            return s * TINY * TINY;
        }
        if p_l <= z - p_h {
            return s * TINY * TINY;
        }
    }
    let ii = j & 0x7fffffff;
    let mut k = (ii >> 20) - 0x3ff;
    let mut n = 0i32;
    if ii > 0x3fe00000 {
        n = j + (0x00100000 >> (k + 1));
        k = ((n & 0x7fffffff) >> 20) - 0x3ff;
        let t = words((n & !(0x000fffff >> k)) as u32, 0);
        n = ((n & 0x000fffff) | 0x00100000) >> (20 - k);
        if j < 0 {
            n = -n;
        }
        p_h -= t;
    }
    let t = with_lo(p_l + p_h, 0);
    let u = t * LG2_H;
    let v = (p_l - (t - p_h)) * LG2 + t * LG2_L;
    z = u + v;
    let w = v - (z - u);
    let t = z * z;
    let t1b = z - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    let r = (z * t1b) / (t1b - 2.0) - (w + z * w);
    z = 1.0 - (r - z);
    let mut jj = hi(z) as i32;
    jj += n << 20;
    if (jj >> 20) <= 0 {
        z = scalbn(z, n);
    } else {
        z = words(jj as u32, lo(z));
    }
    s * z
}

/// `**` / Math.pow with JavaScript's special cases (1 ** NaN is NaN; ±1 ** ±∞ is NaN).
pub(crate) fn js_pow(x: f64, y: f64) -> f64 {
    if y.is_nan() {
        return f64::NAN;
    }
    if y == 0.0 {
        return 1.0;
    }
    if (x == 1.0 || x == -1.0) && y.is_infinite() {
        return f64::NAN;
    }
    // Exact small integer powers (common in pages: 2 ** 10, 10 ** 3).
    if y.abs() <= 64.0 && crate::math::trunc(y) == y && x.is_finite() && x != 0.0 {
        let mut n = y.abs() as u32;
        let mut b = x;
        let mut acc = 1.0;
        let exact_int = crate::math::trunc(x) == x && x.abs() < 1e7;
        if exact_int {
            while n > 0 {
                if n & 1 == 1 {
                    acc *= b;
                }
                b *= b;
                n >>= 1;
            }
            if acc.abs() < 9007199254740992.0 {
                return if y < 0.0 { 1.0 / acc } else { acc };
            }
        }
    }
    pow(x, y)
}

// ---- trigonometry ----

const PIO2_1: f64 = 1.57079632673412561417e+00;
const PIO2_1T: f64 = 6.07710050650619224932e-11;
const PIO2_2: f64 = 6.07710050630396597660e-11;
const PIO2_2T: f64 = 2.02226624879595063154e-21;
const PIO2_3: f64 = 2.02226624871116645580e-21;
const PIO2_3T: f64 = 8.47842766036889956997e-32;
const INVPIO2: f64 = 6.36619772367581382433e-01;

fn k_sin(x: f64, y: f64, iy: i32) -> f64 {
    const S1: f64 = -1.66666666666666324348e-01;
    const S2: f64 = 8.33333333332248946124e-03;
    const S3: f64 = -1.98412698298579493134e-04;
    const S4: f64 = 2.75573137070700676789e-06;
    const S5: f64 = -2.50507602534068634195e-08;
    const S6: f64 = 1.58969099521155010221e-10;
    let z = x * x;
    let w = z * z;
    let r = S2 + z * (S3 + z * S4) + z * w * (S5 + z * S6);
    let v = z * x;
    if iy == 0 {
        x + v * (S1 + z * r)
    } else {
        x - ((z * (0.5 * y - v * r) - y) - v * S1)
    }
}

fn k_cos(x: f64, y: f64) -> f64 {
    const C1: f64 = 4.16666666666666019037e-02;
    const C2: f64 = -1.38888888888741095749e-03;
    const C3: f64 = 2.48015872894767294178e-05;
    const C4: f64 = -2.75573143513906633035e-07;
    const C5: f64 = 2.08757232129817482790e-09;
    const C6: f64 = -1.13596475577881948265e-11;
    let z = x * x;
    let w = z * z;
    let r = z * (C1 + z * (C2 + z * C3)) + w * w * (C4 + z * (C5 + z * C6));
    let hz = 0.5 * z;
    let w = 1.0 - hz;
    w + (((1.0 - w) - hz) + (z * r - x * y))
}

fn k_tan(x: f64, y: f64, odd: bool) -> f64 {
    const T: [f64; 13] = [
        3.33333333333334091986e-01,
        1.33333333333201242699e-01,
        5.39682539762260521377e-02,
        2.18694882948595424599e-02,
        8.86323982359930005737e-03,
        3.59207910759131235356e-03,
        1.45620945432529025516e-03,
        5.88041240820264096874e-04,
        2.46463134818469906812e-04,
        7.81794442939557092300e-05,
        7.14072491382608190305e-05,
        -1.85586374855275456654e-05,
        2.59073051863633712884e-05,
    ];
    const PIO4: f64 = 7.85398163397448278999e-01;
    const PIO4LO: f64 = 3.06161699786838301793e-17;
    let mut x = x;
    let mut y = y;
    let hx = hi(x);
    let big = (hx & 0x7fffffff) >= 0x3FE59428;
    let mut sign = false;
    if big {
        sign = hx >> 31 != 0;
        if sign {
            x = -x;
            y = -y;
        }
        x = (PIO4 - x) + (PIO4LO - y);
        y = 0.0;
    }
    let z = x * x;
    let w = z * z;
    let r = T[1] + w * (T[3] + w * (T[5] + w * (T[7] + w * (T[9] + w * T[11]))));
    let v = z * (T[2] + w * (T[4] + w * (T[6] + w * (T[8] + w * (T[10] + w * T[12])))));
    let s = z * x;
    let r = y + z * (s * (r + v) + y) + s * T[0];
    let w = x + r;
    if big {
        let s = if odd { -1.0 } else { 1.0 };
        let v = s - 2.0 * (x + (r - w * w / (w + s)));
        return if sign { -v } else { v };
    }
    if !odd {
        return w;
    }
    let w0 = with_lo(w, 0);
    let v = r - (w0 - x);
    let a = -1.0 / w;
    let a0 = with_lo(a, 0);
    a0 + a * (1.0 + a0 * w0 + a0 * v)
}

/// x = n·π/2 + (y0 + y1), for any finite x.
fn rem_pio2(x: f64) -> (i32, f64, f64) {
    let ix = hi(x) & 0x7fffffff;
    let sign = hi(x) >> 31 != 0;
    if ix <= 0x400f6a7a {
        if (ix & 0xfffff) == 0x921fb {
            return medium(x, ix);
        }
        if ix <= 0x4002d97c {
            if !sign {
                let z = x - PIO2_1;
                let y0 = z - PIO2_1T;
                return (1, y0, (z - y0) - PIO2_1T);
            } else {
                let z = x + PIO2_1;
                let y0 = z + PIO2_1T;
                return (-1, y0, (z - y0) + PIO2_1T);
            }
        } else if !sign {
            let z = x - 2.0 * PIO2_1;
            let y0 = z - 2.0 * PIO2_1T;
            return (2, y0, (z - y0) - 2.0 * PIO2_1T);
        } else {
            let z = x + 2.0 * PIO2_1;
            let y0 = z + 2.0 * PIO2_1T;
            return (-2, y0, (z - y0) + 2.0 * PIO2_1T);
        }
    }
    if ix <= 0x401c463b {
        if ix <= 0x4015fdbc {
            if ix == 0x4012d97c {
                return medium(x, ix);
            }
            if !sign {
                let z = x - 3.0 * PIO2_1;
                let y0 = z - 3.0 * PIO2_1T;
                return (3, y0, (z - y0) - 3.0 * PIO2_1T);
            } else {
                let z = x + 3.0 * PIO2_1;
                let y0 = z + 3.0 * PIO2_1T;
                return (-3, y0, (z - y0) + 3.0 * PIO2_1T);
            }
        } else {
            if ix == 0x401921fb {
                return medium(x, ix);
            }
            if !sign {
                let z = x - 4.0 * PIO2_1;
                let y0 = z - 4.0 * PIO2_1T;
                return (4, y0, (z - y0) - 4.0 * PIO2_1T);
            } else {
                let z = x + 4.0 * PIO2_1;
                let y0 = z + 4.0 * PIO2_1T;
                return (-4, y0, (z - y0) + 4.0 * PIO2_1T);
            }
        }
    }
    if ix < 0x413921fb {
        return medium(x, ix);
    }
    large(x)
}

fn medium(x: f64, ix: u32) -> (i32, f64, f64) {
    let toint = 1.5 / f64::EPSILON;
    let f_n = (x * INVPIO2 + toint) - toint;
    let n = f_n as i32;
    let mut r = x - f_n * PIO2_1;
    let mut w = f_n * PIO2_1T;
    let mut y0 = r - w;
    let ey = ((hi(y0) >> 20) & 0x7ff) as i32;
    let ex = (ix >> 20) as i32;
    if ex - ey > 16 {
        let t = r;
        w = f_n * PIO2_2;
        r = t - w;
        w = f_n * PIO2_2T - ((t - r) - w);
        y0 = r - w;
        let ey = ((hi(y0) >> 20) & 0x7ff) as i32;
        if ex - ey > 49 {
            let t = r;
            w = f_n * PIO2_3;
            r = t - w;
            w = f_n * PIO2_3T - ((t - r) - w);
            y0 = r - w;
        }
    }
    let y1 = (r - y0) - w;
    (n, y0, y1)
}

/// Payne-Hanek reduction for huge arguments (fdlibm's __kernel_rem_pio2, two-output form).
fn large(x: f64) -> (i32, f64, f64) {
    const IPIO2: [i32; 66] = [
        0xA2F983, 0x6E4E44, 0x1529FC, 0x2757D1, 0xF534DD, 0xC0DB62, 0x95993C, 0x439041, 0xFE5163,
        0xABDEBB, 0xC561B7, 0x246E3A, 0x424DD2, 0xE00649, 0x2EEA09, 0xD1921C, 0xFE1DEB, 0x1CB129,
        0xA73EE8, 0x8235F5, 0x2EBB44, 0x84E99C, 0x7026B4, 0x5F7E41, 0x3991D6, 0x398353, 0x39F49C,
        0x845F8B, 0xBDF928, 0x3B1FF8, 0x97FFDE, 0x05980F, 0xEF2F11, 0x8B5A0A, 0x6D1F6D, 0x367ECF,
        0x27CB09, 0xB74F46, 0x3F669E, 0x5FEA2D, 0x7527BA, 0xC7EBE5, 0xF17B3D, 0x0739F7, 0x8A5292,
        0xEA6BFB, 0x5FB11F, 0x8D5D08, 0x560330, 0x46FC7B, 0x6BABF0, 0xCFBC20, 0x9AF436, 0x1DA9E3,
        0x91615E, 0xE61B08, 0x659985, 0x5F14A0, 0x68408D, 0xFFD880, 0x4D7327, 0x310606, 0x1556CA,
        0x73A8C9, 0x60E27B, 0xC08C6B,
    ];
    const PIO2: [f64; 8] = [
        1.57079625129699707031e+00,
        7.54978941586159635335e-08,
        5.39030252995776476554e-15,
        3.28200341580791294123e-22,
        1.27065575308067607349e-29,
        1.22933308981111328932e-36,
        2.73370053816464559624e-44,
        2.16741683877804819444e-51,
    ];
    let sign = hi(x) >> 31 != 0;
    // Split |x| into three 24-bit chunks scaled by 2^(-e0).
    let ax = x.abs();
    let e0 = ((hi(ax) >> 20) as i32) - 1046;
    // z = |x| scaled into [2^23, 2^24): keep the mantissa, set the exponent.
    let mut z = f64::from_bits((ax.to_bits() & ((1u64 << 52) - 1)) | ((0x3ffu64 + 23) << 52));
    let mut tx = [0.0f64; 3];
    for t in tx.iter_mut().take(2) {
        *t = (z as i32) as f64;
        z = (z - *t) * 16777216.0;
    }
    tx[2] = z;
    let mut nx = 3;
    while nx > 1 && tx[nx - 1] == 0.0 {
        nx -= 1;
    }
    let (n, y) = kernel_rem_pio2(&tx[..nx], e0, 1, &IPIO2, &PIO2);
    if sign {
        (-n, -y[0], -y[1])
    } else {
        (n, y[0], y[1])
    }
}

fn kernel_rem_pio2(
    x: &[f64],
    e0: i32,
    prec: usize,
    ipio2: &[i32],
    pio2: &[f64],
) -> (i32, [f64; 3]) {
    const INIT_JK: [usize; 4] = [3, 4, 4, 6];
    let x1p24 = 16777216.0;
    let x1p_24 = 1.0 / 16777216.0;
    let nx = x.len();
    let jk = INIT_JK[prec];
    let jp = jk;
    let jx = nx - 1;
    let mut jv = (e0 - 3) / 24;
    if jv < 0 {
        jv = 0;
    }
    let mut q0 = e0 - 24 * (jv + 1);
    let jv = jv as usize;
    let mut f = [0.0f64; 20];
    let mut q = [0.0f64; 20];
    let mut fq = [0.0f64; 20];
    let mut iq = [0i32; 20];
    let m = jx + jk;
    let mut j = jv as i32 - jx as i32;
    for i in 0..=m {
        f[i] = if j < 0 {
            0.0
        } else {
            ipio2.get(j as usize).copied().unwrap_or(0) as f64
        };
        j += 1;
    }
    for i in 0..=jk {
        let mut fw = 0.0;
        for j in 0..=jx {
            fw += x[j] * f[jx + i - j];
        }
        q[i] = fw;
    }
    let mut jz = jk;
    let mut n: i32;
    let mut ih: i32;
    let mut z: f64;
    loop {
        // distill q[] into iq[] reversingly
        let mut i = 0;
        let mut jj = jz;
        z = q[jz];
        while jj > 0 {
            let fw = ((x1p_24 * z) as i32) as f64;
            iq[i] = (z - x1p24 * fw) as i32;
            z = q[jj - 1] + fw;
            i += 1;
            jj -= 1;
        }
        z = scalbn(z, q0);
        z -= 8.0 * floor(z * 0.125);
        n = z as i32;
        z -= n as f64;
        ih = 0;
        if q0 > 0 {
            let i = iq[jz - 1] >> (24 - q0);
            n += i;
            iq[jz - 1] -= i << (24 - q0);
            ih = iq[jz - 1] >> (23 - q0);
        } else if q0 == 0 {
            ih = iq[jz - 1] >> 23;
        } else if z >= 0.5 {
            ih = 2;
        }
        if ih > 0 {
            n += 1;
            let mut carry = 0;
            for i in 0..jz {
                let j = iq[i];
                if carry == 0 {
                    if j != 0 {
                        carry = 1;
                        iq[i] = 0x1000000 - j;
                    }
                } else {
                    iq[i] = 0xffffff - j;
                }
            }
            if q0 > 0 {
                match q0 {
                    1 => iq[jz - 1] &= 0x7fffff,
                    2 => iq[jz - 1] &= 0x3fffff,
                    _ => {}
                }
            }
            if ih == 2 {
                z = 1.0 - z;
                if carry != 0 {
                    z -= scalbn(1.0, q0);
                }
            }
        }
        if z == 0.0 {
            let mut j = 0;
            let mut i = jz - 1;
            while i >= jk {
                j |= iq[i];
                if i == 0 {
                    break;
                }
                i -= 1;
            }
            if j == 0 {
                let mut k = 1;
                while iq[jk - k] == 0 {
                    k += 1;
                }
                for i in jz + 1..=jz + k {
                    f[jx + i] = ipio2.get(jv + i).copied().unwrap_or(0) as f64;
                    let mut fw = 0.0;
                    for j in 0..=jx {
                        fw += x[j] * f[jx + i - j];
                    }
                    q[i] = fw;
                }
                jz += k;
                continue;
            }
        }
        break;
    }
    if z == 0.0 {
        jz -= 1;
        q0 -= 24;
        while iq[jz] == 0 {
            jz -= 1;
            q0 -= 24;
        }
    } else {
        z = scalbn(z, -q0);
        if z >= x1p24 {
            let fw = ((x1p_24 * z) as i32) as f64;
            iq[jz] = (z - x1p24 * fw) as i32;
            jz += 1;
            q0 += 24;
            iq[jz] = fw as i32;
        } else {
            iq[jz] = z as i32;
        }
    }
    let mut fw = scalbn(1.0, q0);
    let mut i = jz as i32;
    while i >= 0 {
        q[i as usize] = fw * iq[i as usize] as f64;
        fw *= x1p_24;
        i -= 1;
    }
    let mut i = jz as i32;
    while i >= 0 {
        let mut fw = 0.0;
        let mut k = 0;
        while k <= jp && k <= jz - i as usize {
            fw += pio2[k] * q[i as usize + k];
            k += 1;
        }
        fq[jz - i as usize] = fw;
        i -= 1;
    }
    let mut y = [0.0f64; 3];
    // prec == 1: two outputs
    let mut fw = 0.0;
    let mut i = jz as i32;
    while i >= 0 {
        fw += fq[i as usize];
        i -= 1;
    }
    let fw2 = fw;
    y[0] = if ih == 0 { fw2 } else { -fw2 };
    let mut fw = fq[0] - fw2;
    for i in 1..=jz {
        fw += fq[i];
    }
    y[1] = if ih == 0 { fw } else { -fw };
    (n & 7, y)
}

pub(crate) fn sin(x: f64) -> f64 {
    let ix = hi(x) & 0x7fffffff;
    if ix <= 0x3fe921fb {
        if ix < 0x3e500000 {
            return x;
        }
        return k_sin(x, 0.0, 0);
    }
    if ix >= 0x7ff00000 {
        return f64::NAN;
    }
    let (n, y0, y1) = rem_pio2(x);
    match n & 3 {
        0 => k_sin(y0, y1, 1),
        1 => k_cos(y0, y1),
        2 => -k_sin(y0, y1, 1),
        _ => -k_cos(y0, y1),
    }
}

pub(crate) fn cos(x: f64) -> f64 {
    let ix = hi(x) & 0x7fffffff;
    if ix <= 0x3fe921fb {
        if ix < 0x3e46a09e {
            return 1.0;
        }
        return k_cos(x, 0.0);
    }
    if ix >= 0x7ff00000 {
        return f64::NAN;
    }
    let (n, y0, y1) = rem_pio2(x);
    match n & 3 {
        0 => k_cos(y0, y1),
        1 => -k_sin(y0, y1, 1),
        2 => -k_cos(y0, y1),
        _ => k_sin(y0, y1, 1),
    }
}

pub(crate) fn tan(x: f64) -> f64 {
    let ix = hi(x) & 0x7fffffff;
    if ix <= 0x3fe921fb {
        if ix < 0x3e400000 {
            return x;
        }
        return k_tan(x, 0.0, false);
    }
    if ix >= 0x7ff00000 {
        return f64::NAN;
    }
    let (n, y0, y1) = rem_pio2(x);
    k_tan(y0, y1, n & 1 != 0)
}

// ---- inverse trigonometry ----

const PIO2_HI: f64 = 1.57079632679489655800e+00;
const PIO2_LO: f64 = 6.12323399573676603587e-17;

fn asin_r(z: f64) -> f64 {
    const PS0: f64 = 1.66666666666666657415e-01;
    const PS1: f64 = -3.25565818622400915405e-01;
    const PS2: f64 = 2.01212532134862925881e-01;
    const PS3: f64 = -4.00555345006794114027e-02;
    const PS4: f64 = 7.91534994289814532176e-04;
    const PS5: f64 = 3.47933107596021167570e-05;
    const QS1: f64 = -2.40339491173441421878e+00;
    const QS2: f64 = 2.02094576023350569471e+00;
    const QS3: f64 = -6.88283971605453293030e-01;
    const QS4: f64 = 7.70381505559019352791e-02;
    let p = z * (PS0 + z * (PS1 + z * (PS2 + z * (PS3 + z * (PS4 + z * PS5)))));
    let q = 1.0 + z * (QS1 + z * (QS2 + z * (QS3 + z * QS4)));
    p / q
}

pub(crate) fn asin(x: f64) -> f64 {
    let hx = hi(x);
    let ix = hx & 0x7fffffff;
    if ix >= 0x3ff00000 {
        if ((ix - 0x3ff00000) | lo(x)) == 0 {
            return x * PIO2_HI + f64::from_bits(0x3870000000000000);
        }
        return f64::NAN;
    }
    if ix < 0x3fe00000 {
        if (0x00100000..0x3e500000).contains(&ix) {
            return x;
        }
        return x + x * asin_r(x * x);
    }
    let z = (1.0 - x.abs()) * 0.5;
    let s = sqrt(z);
    let r = asin_r(z);
    let res = if ix >= 0x3fef3333 {
        PIO2_HI - (2.0 * (s + s * r) - PIO2_LO)
    } else {
        let f = with_lo(s, 0);
        let c = (z - f * f) / (s + f);
        0.5 * PIO2_HI - (2.0 * s * r - (PIO2_LO - 2.0 * c) - (0.5 * PIO2_HI - 2.0 * f))
    };
    if hx >> 31 != 0 { -res } else { res }
}

pub(crate) fn acos(x: f64) -> f64 {
    let hx = hi(x);
    let ix = hx & 0x7fffffff;
    if ix >= 0x3ff00000 {
        if ((ix - 0x3ff00000) | lo(x)) == 0 {
            if hx >> 31 != 0 {
                return 2.0 * PIO2_HI + f64::from_bits(0x3870000000000000);
            }
            return 0.0;
        }
        return f64::NAN;
    }
    if ix < 0x3fe00000 {
        if ix <= 0x3c600000 {
            return PIO2_HI + f64::from_bits(0x3870000000000000);
        }
        return PIO2_HI - (x - (PIO2_LO - x * asin_r(x * x)));
    }
    if hx >> 31 != 0 {
        let z = (1.0 + x) * 0.5;
        let s = sqrt(z);
        let w = asin_r(z) * s - PIO2_LO;
        return 2.0 * (PIO2_HI - (s + w));
    }
    let z = (1.0 - x) * 0.5;
    let s = sqrt(z);
    let df = with_lo(s, 0);
    let c = (z - df * df) / (s + df);
    let w = asin_r(z) * s + c;
    2.0 * (df + w)
}

pub(crate) fn atan(x: f64) -> f64 {
    const ATANHI: [f64; 4] = [
        4.63647609000806093515e-01,
        7.85398163397448278999e-01,
        9.82793723247329054082e-01,
        1.57079632679489655800e+00,
    ];
    const ATANLO: [f64; 4] = [
        2.26987774529616870924e-17,
        3.06161699786838301793e-17,
        1.39033110312309984516e-17,
        6.12323399573676603587e-17,
    ];
    const AT: [f64; 11] = [
        3.33333333333329318027e-01,
        -1.99999999998764832476e-01,
        1.42857142725034663711e-01,
        -1.11111104054623557880e-01,
        9.09088713343650656196e-02,
        -7.69187620504482999495e-02,
        6.66107313738753120669e-02,
        -5.83357013379057348645e-02,
        4.97687799461593236017e-02,
        -3.65315727442169155270e-02,
        1.62858201153657823623e-02,
    ];
    let hx = hi(x);
    let sign = hx >> 31 != 0;
    let ix = hx & 0x7fffffff;
    let mut x = x;
    if ix >= 0x44100000 {
        if x.is_nan() {
            return x;
        }
        let z = ATANHI[3] + f64::from_bits(0x0380000000000000);
        return if sign { -z } else { z };
    }
    let id: i32;
    if ix < 0x3fdc0000 {
        if ix < 0x3e400000 {
            return x;
        }
        id = -1;
    } else {
        x = x.abs();
        if ix < 0x3ff30000 {
            if ix < 0x3fe60000 {
                id = 0;
                x = (2.0 * x - 1.0) / (2.0 + x);
            } else {
                id = 1;
                x = (x - 1.0) / (x + 1.0);
            }
        } else if ix < 0x40038000 {
            id = 2;
            x = (x - 1.5) / (1.0 + 1.5 * x);
        } else {
            id = 3;
            x = -1.0 / x;
        }
    }
    let z = x * x;
    let w = z * z;
    let s1 = z * (AT[0] + w * (AT[2] + w * (AT[4] + w * (AT[6] + w * (AT[8] + w * AT[10])))));
    let s2 = w * (AT[1] + w * (AT[3] + w * (AT[5] + w * (AT[7] + w * AT[9]))));
    if id < 0 {
        return x - x * (s1 + s2);
    }
    let id = id as usize;
    let z = ATANHI[id] - (x * (s1 + s2) - ATANLO[id] - x);
    if sign { -z } else { z }
}

pub(crate) fn atan2(y: f64, x: f64) -> f64 {
    const PI: f64 = 3.1415926535897931160E+00;
    const PI_LO: f64 = 1.2246467991473531772E-16;
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    let ix = hi(x);
    let lx = lo(x);
    let iy = hi(y);
    let ly = lo(y);
    if ((ix.wrapping_sub(0x3ff00000)) | lx) == 0 {
        return atan(y);
    }
    let m = ((iy >> 31) & 1) | ((ix >> 30) & 2);
    let ix = ix & 0x7fffffff;
    let iy = iy & 0x7fffffff;
    if (iy | ly) == 0 {
        return match m {
            0 | 1 => y,
            2 => PI,
            _ => -PI,
        };
    }
    if (ix | lx) == 0 {
        return if m & 1 != 0 { -PI / 2.0 } else { PI / 2.0 };
    }
    if ix == 0x7ff00000 {
        if iy == 0x7ff00000 {
            return match m {
                0 => PI / 4.0,
                1 => -PI / 4.0,
                2 => 3.0 * PI / 4.0,
                _ => -3.0 * PI / 4.0,
            };
        } else {
            return match m {
                0 => 0.0,
                1 => -0.0,
                2 => PI,
                _ => -PI,
            };
        }
    }
    if ix + (64 << 20) < iy || iy == 0x7ff00000 {
        return if m & 1 != 0 { -PI / 2.0 } else { PI / 2.0 };
    }
    let z = if (m & 2) != 0 && iy + (64 << 20) < ix {
        0.0
    } else {
        atan((y / x).abs())
    };
    match m {
        0 => z,
        1 => -z,
        2 => PI - (z - PI_LO),
        _ => (z - PI_LO) - PI,
    }
}

// ---- hyperbolic and friends ----

pub(crate) fn sinh(x: f64) -> f64 {
    let h = if x < 0.0 { -0.5 } else { 0.5 };
    let ax = x.abs();
    let w = hi(ax);
    if w < 0x40862e42 {
        let t = expm1(ax);
        if w < 0x3ff00000 {
            if w < 0x3ff00000 - (26 << 20) {
                return x;
            }
            return h * (2.0 * t - t * t / (t + 1.0));
        }
        return h * (t + t / (t + 1.0));
    }
    // large: |x| >= log(DBL_MAX)
    let t = exp(0.5 * ax);
    h * 2.0 * t * t
}

pub(crate) fn cosh(x: f64) -> f64 {
    let ax = x.abs();
    let w = hi(ax);
    if w < 0x3fe62e42 {
        if w < 0x3ff00000 - (26 << 20) {
            return 1.0;
        }
        let t = expm1(ax);
        return 1.0 + t * t / (2.0 * (1.0 + t));
    }
    if w < 0x40862e42 {
        let t = exp(ax);
        return 0.5 * (t + 1.0 / t);
    }
    let t = exp(0.5 * ax);
    0.5 * t * t
}

pub(crate) fn tanh(x: f64) -> f64 {
    let sign = x.is_sign_negative();
    let ax = x.abs();
    let w = hi(ax);
    let t;
    if w > 0x3fe193ea {
        if w > 0x40340000 {
            t = 1.0 - 0.0 / ax;
        } else {
            let e = expm1(2.0 * ax);
            t = 1.0 - 2.0 / (e + 2.0);
        }
    } else if w > 0x3fd058ae {
        let e = expm1(2.0 * ax);
        t = e / (e + 2.0);
    } else if w >= 0x00100000 {
        let e = expm1(-2.0 * ax);
        t = -e / (e + 2.0);
    } else {
        t = ax;
    }
    if sign { -t } else { t }
}

pub(crate) fn asinh(x: f64) -> f64 {
    let sign = x.is_sign_negative();
    let ax = x.abs();
    let e = (hi(ax) >> 20) & 0x7ff;
    let r = if e >= 0x3ff + 26 {
        log(ax) + 0.693147180559945309417232121458176568
    } else if e >= 0x3ff + 1 {
        log(2.0 * ax + 1.0 / (sqrt(ax * ax + 1.0) + ax))
    } else if e >= 0x3ff - 26 {
        log1p(ax + ax * ax / (sqrt(ax * ax + 1.0) + 1.0))
    } else {
        ax
    };
    if sign { -r } else { r }
}

pub(crate) fn acosh(x: f64) -> f64 {
    let e = (hi(x) >> 20) & 0x7ff;
    if x < 1.0 || x.is_nan() {
        return f64::NAN;
    }
    if e < 0x3ff + 1 {
        return log1p(x - 1.0 + sqrt((x - 1.0) * (x - 1.0) + 2.0 * (x - 1.0)));
    }
    if e < 0x3ff + 26 {
        return log(2.0 * x - 1.0 / (x + sqrt(x * x - 1.0)));
    }
    log(x) + 0.693147180559945309417232121458176568
}

pub(crate) fn atanh(x: f64) -> f64 {
    let sign = x.is_sign_negative();
    let y = x.abs();
    let e = (hi(y) >> 20) & 0x7ff;
    let r = if e < 0x3ff - 1 {
        if e < 0x3ff - 32 {
            y
        } else {
            0.5 * log1p(2.0 * y + 2.0 * y * y / (1.0 - y))
        }
    } else {
        0.5 * log1p(2.0 * (y / (1.0 - y)))
    };
    if sign { -r } else { r }
}

pub(crate) fn cbrt(x: f64) -> f64 {
    const B1: u32 = 715094163;
    const B2: u32 = 696219795;
    const P0: f64 = 1.87595182427177009643;
    const P1: f64 = -1.88497979543377169875;
    const P2: f64 = 1.621429720105354466140;
    const P3: f64 = -0.758397934778766047437;
    const P4: f64 = 0.145996192886612446982;
    let mut ui = x.to_bits();
    let mut hx = (ui >> 32) as u32 & 0x7fffffff;
    if hx >= 0x7ff00000 {
        return x + x;
    }
    if hx < 0x00100000 {
        ui = (x * f64::from_bits(0x4350000000000000)).to_bits();
        hx = (ui >> 32) as u32 & 0x7fffffff;
        if hx == 0 {
            return x;
        }
        hx = hx / 3 + B2;
    } else {
        hx = hx / 3 + B1;
    }
    ui &= 1u64 << 63;
    ui |= (hx as u64) << 32;
    let mut t = f64::from_bits(ui);
    let r = (t * t) * (t / x);
    t *= (P0 + r * (P1 + r * P2)) + ((r * r) * r) * (P3 + r * P4);
    ui = t.to_bits();
    t = f64::from_bits((ui + 0x80000000) & 0xffffffffc0000000);
    let s = t * t;
    let r = x / s;
    let w = t + t;
    let r = (r - t) / (w + r);
    t + t * r
}

pub(crate) fn hypot(args: &[f64]) -> f64 {
    let mut max = 0.0f64;
    let mut nan = false;
    for &a in args {
        if a.is_infinite() {
            return f64::INFINITY;
        }
        if a.is_nan() {
            nan = true;
        }
        max = max.max(a.abs());
    }
    if nan {
        return f64::NAN;
    }
    if max == 0.0 {
        return 0.0;
    }
    // Kahan-compensated sum of squares of scaled values.
    let mut sum = 0.0;
    let mut comp = 0.0;
    for &a in args {
        let v = a / max;
        let y = v * v - comp;
        let t = sum + y;
        comp = (t - sum) - y;
        sum = t;
    }
    sqrt(sum) * max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ulps(a: f64, b: f64) -> u64 {
        if a == b || (a.is_nan() && b.is_nan()) {
            return 0;
        }
        let ia = a.to_bits() as i64;
        let ib = b.to_bits() as i64;
        (ia - ib).unsigned_abs()
    }

    #[test]
    fn against_host_libm() {
        let mut xs: alloc::vec::Vec<f64> = alloc::vec![
            0.0, -0.0, 0.5, 1.0, -1.0, 2.0, 10.0, 100.0, 1e-10, 1e10, 3.0, 0.1, 0.7, 1e300, 1e-300
        ];
        for i in 1..400 {
            xs.push(i as f64 * 0.0731 - 7.0);
            xs.push((i as f64).powi(3) * 1.7);
        }
        for &x in &xs {
            assert!(
                ulps(exp(x), x.exp()) <= 1,
                "exp {x}: {} vs {}",
                exp(x),
                x.exp()
            );
            assert!(ulps(sin(x), x.sin()) <= 1, "sin {x}");
            assert!(ulps(cos(x), x.cos()) <= 1, "cos {x}");
            assert!(ulps(tan(x), x.tan()) <= 2, "tan {x}");
            assert!(ulps(atan(x), x.atan()) <= 1, "atan {x}");
            assert!(ulps(sinh(x), x.sinh()) <= 2, "sinh {x}");
            assert!(ulps(tanh(x), x.tanh()) <= 2, "tanh {x}");
            assert!(ulps(cbrt(x), x.cbrt()) <= 1, "cbrt {x}");
            assert!(ulps(expm1(x), x.exp_m1()) <= 1, "expm1 {x}");
            assert!(ulps(asinh(x), x.asinh()) <= 2, "asinh {x}");
            if x > 0.0 {
                assert!(ulps(log(x), x.ln()) <= 1, "log {x}");
                assert!(ulps(log2(x), x.log2()) <= 1, "log2 {x}");
                assert!(ulps(log10(x), x.log10()) <= 1, "log10 {x}");
                assert!(ulps(cosh(x), x.cosh()) <= 2, "cosh {x}");
            }
            if x > -1.0 {
                assert!(ulps(log1p(x), x.ln_1p()) <= 1, "log1p {x}");
            }
            if x.abs() <= 1.0 {
                assert!(ulps(asin(x), x.asin()) <= 1, "asin {x}");
                assert!(ulps(acos(x), x.acos()) <= 1, "acos {x}");
            }
            for &y in &[0.5, 2.0, -1.5, 3.0, 0.1, 10.0, -0.25] {
                assert!(
                    ulps(pow(x, y), x.powf(y)) <= 1,
                    "pow {x} {y}: {} vs {}",
                    pow(x, y),
                    x.powf(y)
                );
                assert!(ulps(atan2(x, y), x.atan2(y)) <= 1, "atan2 {x} {y}");
            }
        }
        for &x in &[1e22, 1e100, 1e300, -1e22, 5.0e15, 123456789.0] {
            assert!(
                ulps(sin(x), f64::sin(x)) <= 1,
                "sin big {x}: {} vs {}",
                sin(x),
                f64::sin(x)
            );
            assert!(ulps(cos(x), f64::cos(x)) <= 1, "cos big {x}");
        }
        assert_eq!(log10(1000.0), 3.0);
        assert_eq!(log2(8.0), 3.0);
        assert_eq!(js_pow(2.0, 10.0), 1024.0);
        assert!(js_pow(1.0, f64::INFINITY).is_nan());
        assert_eq!(hypot(&[3.0, 4.0]), 5.0);
    }
}
