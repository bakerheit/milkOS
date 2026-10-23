//! The float helpers core doesn't have without std (no libm here): rounding and fmod, exact
//! for the ranges layout uses (|x| < 2^31).

pub fn trunc(x: f32) -> f32 {
    if !(-2.0e9..=2.0e9).contains(&x) {
        return x;
    }
    x as i64 as f32
}

pub fn floor(x: f32) -> f32 {
    let t = trunc(x);
    if t > x { t - 1.0 } else { t }
}

pub fn ceil(x: f32) -> f32 {
    let t = trunc(x);
    if t < x { t + 1.0 } else { t }
}

/// Round half away from zero, to i32 (saturating).
pub fn round_i32(x: f32) -> i32 {
    let r = if x >= 0.0 { x + 0.5 } else { x - 0.5 };
    if r >= i32::MAX as f32 {
        i32::MAX
    } else if r <= i32::MIN as f32 {
        i32::MIN
    } else {
        r as i32
    }
}

pub fn fmod(x: f32, y: f32) -> f32 {
    if y == 0.0 {
        return 0.0;
    }
    x - trunc(x / y) * y
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rounding() {
        assert_eq!(floor(-1.5), -2.0);
        assert_eq!(ceil(1.2), 2.0);
        assert_eq!(round_i32(2.5), 3);
        assert_eq!(round_i32(-2.5), -3);
        assert_eq!(fmod(370.0, 360.0), 10.0);
    }
}
