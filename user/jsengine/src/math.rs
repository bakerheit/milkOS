//! The float math core doesn't have without std (no libm in no_std): rounding, fmod, sqrt.
//! The transcendental functions live in `libm.rs`.

#[inline]
pub(crate) fn trunc(x: f64) -> f64 {
    let bits = x.to_bits();
    let e = ((bits >> 52) & 0x7ff) as i32 - 1023;
    if e >= 52 {
        return x; // integral, inf or nan
    }
    if e < 0 {
        return f64::from_bits(bits & (1u64 << 63)); // ±0
    }
    let m = (1u64 << (52 - e)) - 1;
    f64::from_bits(bits & !m)
}

#[inline]
pub(crate) fn floor(x: f64) -> f64 {
    let t = trunc(x);
    if t > x { t - 1.0 } else { t }
}

#[inline]
pub(crate) fn ceil(x: f64) -> f64 {
    let t = trunc(x);
    if t < x { t + 1.0 } else { t }
}

/// Math.round: the nearest integer, ties toward +∞; keeps -0 for -0.5 <= x < 0.
pub(crate) fn js_round(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    if x > 0.0 && x < 0.5 {
        return 0.0;
    }
    if (-0.5..0.0).contains(&x) {
        return -0.0;
    }
    let f = floor(x);
    if x - f >= 0.5 { f + 1.0 } else { f }
}

pub(crate) fn abs(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & !(1u64 << 63))
}

/// fmod, exact (the musl algorithm): the result has the sign of x.
pub(crate) fn fmod(x: f64, y: f64) -> f64 {
    let mut uxi = x.to_bits();
    let mut uyi = y.to_bits();
    let mut ex = ((uxi >> 52) & 0x7ff) as i64;
    let mut ey = ((uyi >> 52) & 0x7ff) as i64;
    let sx = uxi >> 63;
    if uyi << 1 == 0 || y.is_nan() || ex == 0x7ff {
        return f64::NAN;
    }
    if uxi << 1 <= uyi << 1 {
        if uxi << 1 == uyi << 1 {
            return 0.0 * x;
        }
        return x;
    }
    // normalize x and y
    if ex == 0 {
        let mut i = uxi << 12;
        while i >> 63 == 0 {
            ex -= 1;
            i <<= 1;
        }
        uxi <<= -ex + 1;
    } else {
        uxi &= u64::MAX >> 12;
        uxi |= 1 << 52;
    }
    if ey == 0 {
        let mut i = uyi << 12;
        while i >> 63 == 0 {
            ey -= 1;
            i <<= 1;
        }
        uyi <<= -ey + 1;
    } else {
        uyi &= u64::MAX >> 12;
        uyi |= 1 << 52;
    }
    // x mod y
    while ex > ey {
        let i = uxi.wrapping_sub(uyi);
        if i >> 63 == 0 {
            if i == 0 {
                return 0.0 * x;
            }
            uxi = i;
        }
        uxi <<= 1;
        ex -= 1;
    }
    let i = uxi.wrapping_sub(uyi);
    if i >> 63 == 0 {
        if i == 0 {
            return 0.0 * x;
        }
        uxi = i;
    }
    while uxi >> 52 == 0 {
        uxi <<= 1;
        ex -= 1;
    }
    // scale result
    if ex > 0 {
        uxi -= 1 << 52;
        uxi |= (ex as u64) << 52;
    } else {
        uxi >>= -ex + 1;
    }
    uxi |= sx << 63;
    f64::from_bits(uxi)
}

/// Correctly rounded square root (bit-by-bit, as in fdlibm's e_sqrt).
pub(crate) fn sqrt(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return if x == 0.0 { x } else { f64::NAN };
    }
    if x == 0.0 || x.is_infinite() {
        return x;
    }
    let bits = x.to_bits();
    let mut e = ((bits >> 52) & 0x7ff) as i64;
    let mut m = bits & ((1u64 << 52) - 1);
    if e == 0 {
        // subnormal: normalize
        while m & (1u64 << 52) == 0 {
            m <<= 1;
            e -= 1;
        }
        e += 1;
        m &= (1u64 << 52) - 1;
    }
    m |= 1u64 << 52;
    let mut exp = e - 1023;
    if exp & 1 != 0 {
        m <<= 1;
        exp -= 1;
    }
    // m in [2^52, 2^54); sqrt has 53 bits + guard. Work in u128.
    let n: u128 = (m as u128) << 52; // value = n * 2^(exp-104)... sqrt(n) ~ 2^52..2^53
    // integer sqrt of n with remainder
    let mut r: u128 = 0;
    let mut rem: u128 = n;
    let mut bit: u128 = 1u128 << 106;
    while bit > n {
        bit >>= 2;
    }
    while bit != 0 {
        if rem >= r + bit {
            rem -= r + bit;
            r = (r >> 1) + bit;
        } else {
            r >>= 1;
        }
        bit >>= 2;
    }
    // r = floor(sqrt(n)), 53 bits. Round to nearest (ties can't happen exactly for sqrt).
    if rem > r {
        r += 1;
    }
    let mut mant = r as u64;
    let mut rexp = exp / 2;
    if mant >= (1u64 << 53) {
        mant >>= 1;
        rexp += 1;
    }
    let be = (rexp + 1023) as u64;
    f64::from_bits((be << 52) | (mant & ((1u64 << 52) - 1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basics() {
        assert_eq!(floor(-1.5), -2.0);
        assert_eq!(ceil(1.2), 2.0);
        assert_eq!(trunc(-2.7), -2.0);
        assert!(trunc(-0.5).is_sign_negative());
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert!(js_round(-0.4).is_sign_negative());
        assert_eq!(fmod(7.5, 2.0), 1.5);
        assert_eq!(fmod(-7.5, 2.0), -1.5);
        assert_eq!(fmod(5.0, 0.3), 5.0 % 0.3);
        assert_eq!(sqrt(2.0), 2.0f64.sqrt());
        assert_eq!(sqrt(1e-310), 1e-310f64.sqrt());
        for i in 1..2000 {
            let x = i as f64 * 0.731;
            assert_eq!(sqrt(x), x.sqrt(), "{x}");
            assert_eq!(fmod(x, 1.7), x % 1.7);
        }
    }
}
