//! Lengths of time for `pause`: a number, then an optional unit.
//!
//! `5` and `5s` are seconds, `2m` minutes, `1h` hours, `500ms` milliseconds. Fractions are
//! fine (`0.5`, `1.5m`), up to nanoseconds.

use alloc::format;
use alloc::string::String;

const NS: [(&str, u64); 4] = [
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("m", 60_000_000_000),
    ("h", 3_600_000_000_000),
];

/// "5s" -> 5_000_000_000. Err: what to tell the user.
pub fn parse(s: &str) -> Result<u64, String> {
    let bad = || format!("{s} isn't a length of time (try 5, 5s, 2m or 500ms)");
    let digits = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(digits);
    let scale = if unit.is_empty() {
        1_000_000_000
    } else {
        NS.iter().find(|(u, _)| *u == unit).ok_or_else(bad)?.1
    };
    let (int, frac) = num.split_once('.').unwrap_or((num, ""));
    if (int.is_empty() && frac.is_empty()) || frac.contains('.') {
        return Err(bad());
    }
    let whole: u64 = if int.is_empty() {
        0
    } else {
        int.parse().map_err(|_| bad())?
    };
    let mut ns = whole
        .checked_mul(scale)
        .ok_or_else(|| format!("{s} is too long"))?;
    // The fraction, digit by digit, so 0.1 is exactly 100 ms.
    let mut part = scale;
    for c in frac.chars() {
        part /= 10;
        if part == 0 {
            break;
        }
        ns += c.to_digit(10).ok_or_else(bad)? as u64 * part;
    }
    Ok(ns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units() {
        assert_eq!(parse("5"), Ok(5_000_000_000));
        assert_eq!(parse("5s"), Ok(5_000_000_000));
        assert_eq!(parse("2m"), Ok(120_000_000_000));
        assert_eq!(parse("1h"), Ok(3_600_000_000_000));
        assert_eq!(parse("500ms"), Ok(500_000_000));
        assert_eq!(parse("0"), Ok(0));
    }

    #[test]
    fn fractions() {
        assert_eq!(parse("0.5"), Ok(500_000_000));
        assert_eq!(parse(".25"), Ok(250_000_000));
        assert_eq!(parse("1.5m"), Ok(90_000_000_000));
        assert_eq!(parse("0.1s"), Ok(100_000_000));
        assert_eq!(parse("2.5ms"), Ok(2_500_000));
        assert_eq!(parse("1.0000000001"), Ok(1_000_000_000)); // past nanoseconds: dropped
    }

    #[test]
    fn errors() {
        let e = "isn't a length of time (try 5, 5s, 2m or 500ms)";
        assert_eq!(parse("abc"), Err(format!("abc {e}")));
        assert_eq!(parse("5x"), Err(format!("5x {e}")));
        assert_eq!(parse(""), Err(format!(" {e}")));
        assert_eq!(parse("s"), Err(format!("s {e}")));
        assert_eq!(parse("1.2.3"), Err(format!("1.2.3 {e}")));
        assert_eq!(parse("-1"), Err(format!("-1 {e}")));
        assert_eq!(parse("5 s"), Err(format!("5 s {e}")));
        assert!(parse("99999999999999h").unwrap_err().contains("too long"));
    }
}
