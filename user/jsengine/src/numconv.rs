//! Number ↔ string conversions: Number::toString (the shortest round-tripping digits, from
//! core's float formatting), radix conversions, toFixed / toExponential / toPrecision, and
//! StringToNumber / parseInt / parseFloat scanning.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// The shortest digits and exponent of a finite positive x: x = 0.d1d2... × 10^n.
/// Returns (digits, n).
pub(crate) fn shortest_digits(x: f64) -> (Vec<u8>, i32) {
    let mut s = String::new();
    let _ = write!(s, "{:e}", x);
    let (mant, exp) = match s.split_once('e') {
        Some(p) => p,
        None => (s.as_str(), "0"),
    };
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: Vec<u8> = mant.bytes().filter(|b| b.is_ascii_digit()).collect();
    let mut d = digits;
    while d.len() > 1 && *d.last().unwrap_or(&b'1') == b'0' {
        d.pop();
    }
    (d, exp + 1)
}

/// Number::toString(10).
pub(crate) fn number_to_string(x: f64) -> String {
    if x.is_nan() {
        return String::from("NaN");
    }
    if x == 0.0 {
        return String::from("0");
    }
    if x.is_infinite() {
        return String::from(if x > 0.0 { "Infinity" } else { "-Infinity" });
    }
    let mut out = String::new();
    let mut x = x;
    if x < 0.0 {
        out.push('-');
        x = -x;
    }
    // Integers below 2^53 print exactly and fast.
    if x < 9007199254740992.0 && (x as u64) as f64 == x {
        let _ = write!(out, "{}", x as u64);
        return out;
    }
    let (d, n) = shortest_digits(x);
    let k = d.len() as i32;
    let ds = core::str::from_utf8(&d).unwrap_or("0");
    if k <= n && n <= 21 {
        out.push_str(ds);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        out.push_str(&ds[..n as usize]);
        out.push('.');
        out.push_str(&ds[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(ds);
    } else {
        out.push_str(&ds[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&ds[1..]);
        }
        out.push('e');
        let e = n - 1;
        if e >= 0 {
            out.push('+');
        }
        let _ = write!(out, "{}", e);
    }
    out
}

pub(crate) fn u32_to_units(i: u32) -> Vec<u16> {
    let mut buf = [0u16; 10];
    let mut n = i;
    let mut p = 10;
    loop {
        p -= 1;
        buf[p] = b'0' as u16 + (n % 10) as u16;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    buf[p..].to_vec()
}

pub(crate) fn i32_to_string(i: i32) -> String {
    let mut s = String::new();
    let _ = write!(s, "{}", i);
    s
}

/// Number::prototype.toString(radix) for radix 2..=36 (not 10).
pub(crate) fn number_to_radix(x: f64, radix: u32) -> String {
    if x.is_nan() {
        return String::from("NaN");
    }
    if x == 0.0 {
        return String::from("0");
    }
    if x.is_infinite() {
        return String::from(if x > 0.0 { "Infinity" } else { "-Infinity" });
    }
    let neg = x < 0.0;
    let x = if neg { -x } else { x };
    let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let r = radix as f64;
    let mut int = crate::math::floor(x);
    let mut frac = x - int;
    let mut ip: Vec<u8> = Vec::new();
    if int == 0.0 {
        ip.push(b'0');
    }
    let mut guard = 0;
    while int >= 1.0 && guard < 1100 {
        let q = crate::math::floor(int / r);
        let d = (int - q * r) as usize;
        ip.push(digits[d.min(35)]);
        int = q;
        guard += 1;
    }
    ip.reverse();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(core::str::from_utf8(&ip).unwrap_or("0"));
    if frac > 0.0 {
        out.push('.');
        // Enough digits to distinguish the value (like V8: stop when the remaining fraction is
        // below half an ulp of the original).
        let mut delta = 0.5 * (next_up(x) - x);
        delta = delta.max(next_up(0.0));
        if frac >= delta {
            let mut fd: Vec<u8> = Vec::new();
            loop {
                frac *= r;
                delta *= r;
                let d = crate::math::floor(frac) as usize;
                fd.push(d.min(35) as u8);
                frac -= d as f64;
                if frac > 0.5 || (frac == 0.5 && (d & 1) == 1) {
                    if frac + delta > 1.0 {
                        // Round up, propagating carries.
                        loop {
                            match fd.last_mut() {
                                None => break,
                                Some(l) => {
                                    if (*l as u32) + 1 < radix {
                                        *l += 1;
                                        break;
                                    }
                                    fd.pop();
                                }
                            }
                        }
                        break;
                    }
                }
                if frac < delta || fd.len() > 1100 {
                    break;
                }
            }
            for d in fd {
                out.push(digits[d as usize] as char);
            }
        }
        if out.ends_with('.') {
            out.pop();
        }
    }
    out
}

fn next_up(x: f64) -> f64 {
    if x.is_nan() || x == f64::INFINITY {
        return x;
    }
    if x == 0.0 {
        return f64::from_bits(1);
    }
    let b = x.to_bits();
    if x > 0.0 {
        f64::from_bits(b + 1)
    } else {
        f64::from_bits(b - 1)
    }
}

/// Exact decimal digits of a finite non-negative double, as (integer digits, fraction digits)
/// with no rounding: doubles are dyadic, so the expansion terminates.
fn exact_decimal(x: f64) -> (Vec<u8>, Vec<u8>) {
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let mut mant = bits & ((1u64 << 52) - 1);
    let e2;
    if exp == 0 {
        e2 = -1074;
    } else {
        mant |= 1 << 52;
        e2 = exp - 1075;
    }
    // value = mant * 2^e2. Big decimal arithmetic in base 1e9 limbs.
    let mut int_part: Vec<u32>; // little-endian base 1e9
    let mut frac_digits: Vec<u8> = Vec::new();
    if e2 >= 0 {
        int_part = from_u64(mant);
        for _ in 0..e2 {
            mul_small(&mut int_part, 2);
        }
    } else {
        let sh = (-e2) as u32;
        let ip = if sh >= 64 { 0 } else { mant >> sh };
        int_part = from_u64(ip);
        // fraction = (mant mod 2^sh) / 2^sh: multiply by 10 repeatedly.
        let mut frac: Vec<u32> = if sh >= 64 {
            from_u64_bin(mant)
        } else {
            from_u64_bin(mant & ((1u64 << sh) - 1))
        };
        // frac is a binary big number (base 2^32 limbs) with sh fractional bits.
        let mut guard = 0;
        while !is_zero(&frac) && guard < 1100 {
            // frac *= 10; digit = frac >> sh; frac &= (1<<sh)-1
            mul_small_bin(&mut frac, 10);
            let d = shift_out(&mut frac, sh);
            frac_digits.push(d as u8);
            guard += 1;
        }
    }
    let int_digits = to_dec_digits(&int_part);
    (int_digits, frac_digits)
}

fn from_u64(mut v: u64) -> Vec<u32> {
    let mut out = Vec::new();
    while v > 0 {
        out.push((v % 1_000_000_000) as u32);
        v /= 1_000_000_000;
    }
    out
}

fn mul_small(a: &mut Vec<u32>, m: u32) {
    let mut carry: u64 = 0;
    for l in a.iter_mut() {
        let t = *l as u64 * m as u64 + carry;
        *l = (t % 1_000_000_000) as u32;
        carry = t / 1_000_000_000;
    }
    while carry > 0 {
        a.push((carry % 1_000_000_000) as u32);
        carry /= 1_000_000_000;
    }
}

fn to_dec_digits(a: &[u32]) -> Vec<u8> {
    if a.is_empty() {
        return alloc::vec![0];
    }
    let mut s = String::new();
    let _ = write!(s, "{}", a[a.len() - 1]);
    for l in a[..a.len() - 1].iter().rev() {
        let _ = write!(s, "{:09}", l);
    }
    s.bytes().map(|b| b - b'0').collect()
}

fn from_u64_bin(v: u64) -> Vec<u32> {
    alloc::vec![v as u32, (v >> 32) as u32]
}

fn is_zero(a: &[u32]) -> bool {
    a.iter().all(|&l| l == 0)
}

fn mul_small_bin(a: &mut Vec<u32>, m: u32) {
    let mut carry: u64 = 0;
    for l in a.iter_mut() {
        let t = *l as u64 * m as u64 + carry;
        *l = t as u32;
        carry = t >> 32;
    }
    if carry > 0 {
        a.push(carry as u32);
    }
}

/// Removes and returns the bits at and above `sh` (the integer part).
fn shift_out(a: &mut [u32], sh: u32) -> u32 {
    let limb = (sh / 32) as usize;
    let bit = sh % 32;
    let mut val: u64 = 0;
    // The integer part after *10 is < 10, so it fits in the next few bits.
    for i in limb..a.len() {
        let mut part = a[i] as u64;
        if i == limb {
            part >>= bit;
        }
        let shift = if i == limb {
            0
        } else {
            (i - limb) as u32 * 32 - bit
        };
        if shift < 64 {
            val |= part << shift;
        }
        // Clear the bits.
        if i == limb {
            if bit == 0 {
                a[i] = 0;
            } else {
                a[i] &= (1u32 << bit) - 1;
            }
        } else {
            a[i] = 0;
        }
    }
    val as u32
}

/// Rounds a digit string at position `keep` (digits after it dropped), half up (the spec's
/// "if there are two such n, pick the larger"). Returns true if it carried out of the top.
fn round_digits(d: &mut Vec<u8>, keep: usize) -> bool {
    if keep >= d.len() {
        d.resize(keep, 0);
        return false;
    }
    let up = d[keep] >= 5;
    d.truncate(keep);
    if up {
        let mut i = keep;
        loop {
            if i == 0 {
                d.insert(0, 1);
                return true;
            }
            i -= 1;
            if d[i] == 9 {
                d[i] = 0;
            } else {
                d[i] += 1;
                break;
            }
        }
    }
    false
}

/// Number.prototype.toFixed(f), 0 <= f <= 100, |x| < 1e21 (the caller handles the rest).
pub(crate) fn to_fixed(x: f64, f: usize) -> String {
    let neg = x < 0.0;
    let ax = if neg { -x } else { x };
    let (ip, fp) = exact_decimal(ax);
    let mut all: Vec<u8> = ip.clone();
    all.extend_from_slice(&fp);
    let int_len = ip.len();
    let carried = round_digits(&mut all, int_len + f);
    let int_len = int_len + carried as usize;
    let mut out = String::new();
    // -0.0.toFixed is "0"; a negative that rounds to zero keeps its sign ("-0.00").
    if neg && ax != 0.0 {
        out.push('-');
    }
    let ints = &all[..int_len];
    // Strip leading zeros of the integer part (keep one).
    let mut start = 0;
    while start + 1 < ints.len() && ints[start] == 0 {
        start += 1;
    }
    for &d in &ints[start..] {
        out.push((b'0' + d) as char);
    }
    if f > 0 {
        out.push('.');
        for &d in &all[int_len..int_len + f] {
            out.push((b'0' + d) as char);
        }
    }
    out
}

/// The first `p` significant digits of x > 0, rounded half up, and the decimal exponent e
/// such that x ≈ d.ddd × 10^e.
fn sig_digits(x: f64, p: usize) -> (Vec<u8>, i32) {
    let (ip, fp) = exact_decimal(x);
    let mut all = ip.clone();
    all.extend_from_slice(&fp);
    // Position of the first non-zero digit.
    let first = all.iter().position(|&d| d != 0).unwrap_or(0);
    let mut e = ip.len() as i32 - 1 - first as i32;
    let mut d: Vec<u8> = all[first..].to_vec();
    if round_digits(&mut d, p) {
        e += 1;
        d.truncate(p);
    }
    (d, e)
}

/// Number.prototype.toExponential(f); `f` None means "as many digits as needed".
pub(crate) fn to_exponential(x: f64, f: Option<usize>) -> String {
    let neg = x < 0.0;
    let ax = if neg { -x } else { x };
    let (d, e) = if ax == 0.0 {
        (alloc::vec![0u8; f.unwrap_or(0) + 1], 0)
    } else {
        match f {
            Some(f) => sig_digits(ax, f + 1),
            None => {
                let (d, n) = shortest_digits(ax);
                (d.iter().map(|c| c - b'0').collect(), n - 1)
            }
        }
    };
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push((b'0' + d[0]) as char);
    if d.len() > 1 {
        out.push('.');
        for &c in &d[1..] {
            out.push((b'0' + c) as char);
        }
    }
    out.push('e');
    out.push(if e >= 0 { '+' } else { '-' });
    let _ = write!(out, "{}", e.unsigned_abs());
    out
}

/// Number.prototype.toPrecision(p), 1 <= p <= 100.
pub(crate) fn to_precision(x: f64, p: usize) -> String {
    let neg = x < 0.0;
    let ax = if neg { -x } else { x };
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    let (d, e) = if ax == 0.0 {
        (alloc::vec![0u8; p], 0)
    } else {
        sig_digits(ax, p)
    };
    if e < -6 || e >= p as i32 {
        out.push((b'0' + d[0]) as char);
        if p > 1 {
            out.push('.');
            for &c in &d[1..] {
                out.push((b'0' + c) as char);
            }
        }
        out.push('e');
        out.push(if e >= 0 { '+' } else { '-' });
        let _ = write!(out, "{}", e.unsigned_abs());
        return out;
    }
    if e >= 0 {
        let e = e as usize;
        for &c in &d[..=e] {
            out.push((b'0' + c) as char);
        }
        if p > e + 1 {
            out.push('.');
            for &c in &d[e + 1..] {
                out.push((b'0' + c) as char);
            }
        }
    } else {
        out.push_str("0.");
        for _ in 0..(-e - 1) {
            out.push('0');
        }
        for &c in &d {
            out.push((b'0' + c) as char);
        }
    }
    out
}

// ---- string → number ----

fn digit_val(c: u16) -> Option<u32> {
    let c = c as u32;
    match c {
        0x30..=0x39 => Some(c - 0x30),
        0x61..=0x7a => Some(c - 0x61 + 10),
        0x41..=0x5a => Some(c - 0x41 + 10),
        _ => None,
    }
}

/// Parses an unsigned decimal literal prefix (`123`, `1.5e3`, `.5`), returning the value and
/// how many units it used; None if there is no number.
pub(crate) fn scan_decimal(u: &[u16], allow_exp: bool) -> Option<(f64, usize)> {
    let mut i = 0;
    let mut s = String::new();
    let mut digits = 0;
    while i < u.len() && crate::string::is_ascii_digit(u[i]) {
        s.push(u[i] as u8 as char);
        i += 1;
        digits += 1;
    }
    if i < u.len() && u[i] == b'.' as u16 {
        let mut j = i + 1;
        let mut fd = 0;
        let mut frac = String::from(".");
        while j < u.len() && crate::string::is_ascii_digit(u[j]) {
            frac.push(u[j] as u8 as char);
            j += 1;
            fd += 1;
        }
        if digits > 0 || fd > 0 {
            s.push_str(&frac);
            i = j;
            digits += fd;
        }
    }
    if digits == 0 {
        return None;
    }
    if allow_exp && i < u.len() && (u[i] == b'e' as u16 || u[i] == b'E' as u16) {
        let mut j = i + 1;
        let mut e = String::from("e");
        if j < u.len() && (u[j] == b'+' as u16 || u[j] == b'-' as u16) {
            e.push(u[j] as u8 as char);
            j += 1;
        }
        let mut ed = 0;
        while j < u.len() && crate::string::is_ascii_digit(u[j]) {
            if ed < 12 {
                e.push(u[j] as u8 as char);
            }
            j += 1;
            ed += 1;
        }
        if ed > 0 {
            s.push_str(&e);
            i = j;
        }
    }
    if s.starts_with('.') {
        s.insert(0, '0');
    }
    if s.ends_with('.') {
        s.push('0');
    }
    let v: f64 = s.parse().unwrap_or(f64::NAN);
    Some((v, i))
}

/// StringToNumber (what `Number(str)` and `+str` do).
pub(crate) fn string_to_number(u: &[u16]) -> f64 {
    let t = crate::string::trim(u, true, true);
    if t.is_empty() {
        return 0.0;
    }
    if t.len() > 2 && t[0] == b'0' as u16 {
        let radix = match t[1] {
            0x78 | 0x58 => 16,
            0x6f | 0x4f => 8,
            0x62 | 0x42 => 2,
            _ => 0,
        };
        if radix != 0 {
            let mut v = 0.0f64;
            for &c in &t[2..] {
                match digit_val(c) {
                    Some(d) if d < radix => v = v * radix as f64 + d as f64,
                    _ => return f64::NAN,
                }
            }
            return v;
        }
    }
    let (neg, body) = match t[0] {
        0x2d => (true, &t[1..]),
        0x2b => (false, &t[1..]),
        _ => (false, t),
    };
    if crate::string::eq_str(body, "Infinity") {
        return if neg {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    match scan_decimal(body, true) {
        Some((v, n)) if n == body.len() => {
            if neg {
                -v
            } else {
                v
            }
        }
        _ => f64::NAN,
    }
}

/// parseFloat.
pub(crate) fn parse_float(u: &[u16]) -> f64 {
    let t = crate::string::trim(u, true, false);
    let (neg, body) = match t.first() {
        Some(0x2d) => (true, &t[1..]),
        Some(0x2b) => (false, &t[1..]),
        _ => (false, t),
    };
    let r = if body.len() >= 8 && crate::string::eq_str(&body[..8], "Infinity") {
        f64::INFINITY
    } else {
        match scan_decimal(body, true) {
            Some((v, _)) => v,
            None => return f64::NAN,
        }
    };
    if neg { -r } else { r }
}

/// parseInt(string, radix); radix 0 means "decide".
pub(crate) fn parse_int(u: &[u16], radix: i32) -> f64 {
    let t = crate::string::trim(u, true, false);
    let (neg, mut body) = match t.first() {
        Some(0x2d) => (true, &t[1..]),
        Some(0x2b) => (false, &t[1..]),
        _ => (false, t),
    };
    let mut r = radix;
    let mut strip = true;
    if r != 0 {
        if !(2..=36).contains(&r) {
            return f64::NAN;
        }
        if r != 16 {
            strip = false;
        }
    } else {
        r = 10;
    }
    if strip
        && body.len() >= 2
        && body[0] == b'0' as u16
        && (body[1] == b'x' as u16 || body[1] == b'X' as u16)
    {
        body = &body[2..];
        r = 16;
    }
    let mut end = 0;
    while end < body.len() && digit_val(body[end]).is_some_and(|d| d < r as u32) {
        end += 1;
    }
    if end == 0 {
        return f64::NAN;
    }
    let digits = &body[..end];
    let v = if r == 10 && end > 15 {
        // Many digits: let the decimal parser round correctly.
        let s: String = digits.iter().map(|&c| c as u8 as char).collect();
        s.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        let mut v = 0.0f64;
        for &c in digits {
            v = v * r as f64 + digit_val(c).unwrap_or(0) as f64;
        }
        v
    };
    if neg { -v } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }
    #[test]
    fn to_string() {
        assert_eq!(number_to_string(0.1), "0.1");
        assert_eq!(number_to_string(1e21), "1e+21");
        assert_eq!(number_to_string(1e-7), "1e-7");
        assert_eq!(number_to_string(123.456), "123.456");
        assert_eq!(number_to_string(-1.5), "-1.5");
        assert_eq!(number_to_string(0.000001), "0.000001");
        assert_eq!(
            number_to_string(1.7976931348623157e308),
            "1.7976931348623157e+308"
        );
        assert_eq!(number_to_string(5e-324), "5e-324");
        assert_eq!(number_to_string(100.0), "100");
        assert_eq!(number_to_string(1e20), "100000000000000000000");
        assert_eq!(number_to_string(0.1 + 0.2), "0.30000000000000004");
    }
    #[test]
    fn fixed_exp_prec() {
        assert_eq!(to_fixed(1.005, 2), "1.00");
        assert_eq!(to_fixed(2.5, 0), "3");
        assert_eq!(to_fixed(1.45, 1), "1.4");
        assert_eq!(to_fixed(0.0, 2), "0.00");
        assert_eq!(to_fixed(123.456, 1), "123.5");
        assert_eq!(to_fixed(-1.5, 0), "-2");
        assert_eq!(to_exponential(123456.0, Some(2)), "1.23e+5");
        assert_eq!(to_exponential(0.00015, None), "1.5e-4");
        assert_eq!(to_precision(123.456, 4), "123.5");
        assert_eq!(to_precision(0.000123, 2), "0.00012");
        assert_eq!(to_precision(123456.0, 2), "1.2e+5");
    }
    #[test]
    fn radix() {
        assert_eq!(number_to_radix(255.0, 16), "ff");
        assert_eq!(number_to_radix(-8.0, 2), "-1000");
        assert_eq!(number_to_radix(0.5, 2), "0.1");
        assert_eq!(number_to_radix(3.25, 2), "11.01");
    }
    #[test]
    fn parse() {
        assert_eq!(string_to_number(&u("  12.5  ")), 12.5);
        assert_eq!(string_to_number(&u("0x1F")), 31.0);
        assert!(string_to_number(&u("12px")).is_nan());
        assert_eq!(string_to_number(&u("")), 0.0);
        assert_eq!(string_to_number(&u("-Infinity")), f64::NEG_INFINITY);
        assert_eq!(string_to_number(&u(".5e1")), 5.0);
        assert!(string_to_number(&u("infinity")).is_nan());
        assert_eq!(parse_int(&u("12px"), 0), 12.0);
        assert_eq!(parse_int(&u("0x10"), 0), 16.0);
        assert_eq!(parse_int(&u("-077"), 8), -63.0);
        assert!(parse_int(&u("z"), 10).is_nan());
        assert_eq!(parse_float(&u("3.14abc")), 3.14);
        assert_eq!(parse_float(&u("-.5")), -0.5);
    }
}
