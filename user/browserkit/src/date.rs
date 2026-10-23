//! Dates: the cookie-date parser of RFC 6265 section 5.1.1 (it reads every format servers
//! really send), and Unix seconds to a calendar date and back.

use alloc::format;
use alloc::string::String;

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month 1-12, day 1-31) for days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// "2026-10-23" for Unix seconds.
pub fn ymd(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// "23 Oct 2026" for Unix seconds.
pub fn day_month_year(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let mut mon = String::from(MONTHS[(m - 1) as usize]);
    if let Some(f) = mon.get_mut(0..1) {
        f.make_ascii_uppercase();
    }
    format!("{d} {mon} {y}")
}

fn is_delim(c: u8) -> bool {
    c == 0x09
        || (0x20..=0x2F).contains(&c)
        || (0x3B..=0x40).contains(&c)
        || (0x5B..=0x60).contains(&c)
        || (0x7B..=0x7E).contains(&c)
}

/// Leading digits of `t` (between `min` and `max` of them), as a number, if `t` starts so.
fn digits(t: &[u8], min: usize, max: usize) -> Option<(u32, usize)> {
    let n = t.iter().take_while(|c| c.is_ascii_digit()).count();
    if n < min || n > max {
        return None;
    }
    let mut v = 0u32;
    for &c in &t[..n] {
        v = v * 10 + (c - b'0') as u32;
    }
    Some((v, n))
}

/// RFC 6265 5.1.1: a cookie's Expires date to Unix seconds (None if it isn't one).
pub fn parse_cookie_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    for tok in b.split(|&c| is_delim(c)).filter(|t| !t.is_empty()) {
        if time.is_none() {
            // hms-time: 1*2DIGIT ":" 1*2DIGIT ":" 1*2DIGIT ( non-digit *OCTET )
            let mut at = 0;
            let mut parts = [0u32; 3];
            let mut ok = true;
            for (i, p) in parts.iter_mut().enumerate() {
                match digits(&tok[at..], 1, 2) {
                    Some((v, n)) => {
                        *p = v;
                        at += n;
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
                if i < 2 {
                    if tok.get(at) != Some(&b':') {
                        ok = false;
                        break;
                    }
                    at += 1;
                }
            }
            if ok && tok.get(at).is_none_or(|c| !c.is_ascii_digit()) {
                time = Some(parts);
                continue;
            }
        }
        if day.is_none()
            && let Some((v, n)) = digits(tok, 1, 2)
            && tok.get(n).is_none_or(|c| !c.is_ascii_digit())
        {
            day = Some(v);
            continue;
        }
        if month.is_none() && tok.len() >= 3 {
            let m = tok[..3].to_ascii_lowercase();
            if let Some(i) = MONTHS.iter().position(|x| x.as_bytes() == m.as_slice()) {
                month = Some(i as u32 + 1);
                continue;
            }
        }
        if year.is_none()
            && let Some((v, n)) = digits(tok, 2, 4)
            && tok.get(n).is_none_or(|c| !c.is_ascii_digit())
        {
            year = Some(v);
            continue;
        }
    }
    let (t, d, m, mut y) = (time?, day?, month?, year?);
    if (70..=99).contains(&y) {
        y += 1900;
    } else if y <= 69 {
        y += 2000;
    }
    if !(1..=31).contains(&d) || y < 1601 || t[0] > 23 || t[1] > 59 || t[2] > 59 {
        return None;
    }
    // A day past the month's end (31 Feb) is no date.
    let days = days_from_civil(y as i64, m, d);
    if civil_from_days(days) != (y as i64, m, d) {
        return None;
    }
    Some(days * 86_400 + t[0] as i64 * 3600 + t[1] as i64 * 60 + t[2] as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        for z in [-800_000i64, -1, 0, 1, 59, 60, 10_957, 20_000, 2_000_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(ymd(1_792_713_600), "2026-10-23");
        assert_eq!(day_month_year(0), "1 Jan 1970");
    }

    #[test]
    fn cookie_dates() {
        let want = days_from_civil(2015, 10, 21) * 86_400 + 7 * 3600 + 28 * 60;
        for s in [
            "Wed, 21 Oct 2015 07:28:00 GMT",
            "Wednesday, 21-Oct-15 07:28:00 GMT",
            "Wed Oct 21 07:28:00 2015",
            "21 oct 2015 7:28:0",
            "Wed, 21-October-2015 07:28:00 UTC",
        ] {
            assert_eq!(parse_cookie_date(s), Some(want), "{s}");
        }
        assert_eq!(parse_cookie_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        for bad in [
            "",
            "yesterday",
            "Wed, 31 Feb 2015 07:28:00 GMT",
            "Wed, 21 Oct 2015 25:28:00 GMT",
            "Wed, 21 Foo 2015 07:28:00 GMT",
            "Wed, 21 Oct 1500 07:28:00 GMT",
            "21 Oct 2015",
            "\u{0}\u{1}::::::",
        ] {
            assert_eq!(parse_cookie_date(bad), None, "{bad:?}");
        }
        // Two-digit years.
        assert_eq!(
            parse_cookie_date("01 Jan 70 00:00:00").map(|s| s / 86_400),
            Some(0)
        );
        assert_eq!(
            parse_cookie_date("01 Jan 69 00:00:00"),
            Some(days_from_civil(2069, 1, 1) * 86_400)
        );
    }
}
