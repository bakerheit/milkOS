//! Shared bits for the tools.
#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;

/// "Oct 20 23:01" from Unix seconds (UTC).
pub fn fmt_time(t: i64) -> String {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    // civil_from_days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let _y = yoe + era * 400 + i64::from(m <= 2);
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{} {:>2} {:02}:{:02}",
        MON[(m - 1) as usize],
        d,
        secs / 3600,
        secs / 60 % 60
    )
}

/// "rwxr-xr-x" from permission bits.
/// `rwxr-xr-x`, with set-user-ID / set-group-ID shown as `s` (`S` without x) in the
/// owner / group execute column and the sticky bit as `t` (`T`) in the other one.
pub fn fmt_perm(mode: u32) -> String {
    let mut s = String::new();
    for (shift, special, ch) in [(6, 0o4000, 's'), (3, 0o2000, 's'), (0, 0o1000, 't')] {
        let b = (mode >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(match (b & 1 != 0, mode & special != 0) {
            (true, true) => ch,
            (false, true) => ch.to_ascii_uppercase(),
            (true, false) => 'x',
            (false, false) => '-',
        });
    }
    s
}

/// 1536 -> "1.5K", like `-h`.
pub fn human(n: u64) -> String {
    let units = ["", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n}")
    } else if v < 10.0 {
        format!("{:.1}{}", v, units[u])
    } else {
        format!("{:.0}{}", v, units[u])
    }
}

/// Last path component.
pub fn basename(p: &str) -> &str {
    p.trim_end_matches('/').rsplit('/').next().unwrap_or(p)
}

/// `dir` + "/" + `name` without doubling slashes.
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}
