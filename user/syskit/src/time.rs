//! Uptimes and clock times. Seconds since 1970 are milkOS time (the RTC's local time).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// "1:02:03" (hours can pass 24).
pub fn clock(secs: u64) -> String {
    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// "less than a minute", "1 minute", "3 hours 5 minutes", "2 days 4 hours": the two biggest
/// parts that aren't zero.
pub fn words(secs: u64) -> String {
    if secs < 60 {
        return String::from("less than a minute");
    }
    let parts = [
        (secs / 86_400, "day"),
        (secs / 3600 % 24, "hour"),
        (secs / 60 % 60, "minute"),
    ];
    let first = parts.iter().position(|p| p.0 > 0).unwrap_or(2);
    let v: Vec<String> = parts[first..(first + 2).min(3)]
        .iter()
        .filter(|p| p.0 > 0)
        .map(|&(n, w)| format!("{n} {w}{}", if n == 1 { "" } else { "s" }))
        .collect();
    v.join(" ")
}

/// "Oct 22 13:05:09" for seconds since 1970.
pub fn stamp(t: i64) -> String {
    let (_, m, d, h, mi, s) = filekit::date::civil(t);
    format!(
        "{} {d:>2} {h:02}:{mi:02}:{s:02}",
        filekit::date::MONTHS[m as usize - 1]
    )
}

/// "Oct 22 13:05" for seconds since 1970.
pub fn since(t: i64) -> String {
    let s = stamp(t);
    String::from(&s[..s.len() - 3])
}

/// The first number in /proc/uptime ("123.45 0.00"), in whole seconds.
pub fn parse_uptime(text: &str) -> u64 {
    text.split_whitespace()
        .next()
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// The three load averages from /proc/loadavg ("0.02 0.01 0.00 1/19 15"), as text.
pub fn parse_load(text: &str) -> Option<[String; 3]> {
    let v: Vec<&str> = text.split_whitespace().take(3).collect();
    (v.len() == 3).then(|| [String::from(v[0]), String::from(v[1]), String::from(v[2])])
}

/// `system uptime`: "up 2 hours 3 minutes (since Oct 22 11:02), 12 processes,
/// load 0.02 0.01 0.00 (1, 5, 15 minutes)". `now` is the wall clock in seconds.
pub fn uptime_line(up: u64, now: i64, procs: usize, load: Option<&[String; 3]>) -> String {
    let mut s = format!(
        "up {} (since {}), {} {}",
        words(up),
        since(now - up as i64),
        procs,
        if procs == 1 { "process" } else { "processes" }
    );
    if let Some(l) = load {
        s += &format!(", load {} {} {} (1, 5, 15 minutes)", l[0], l[1], l[2]);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(clock(0), "0:00:00");
        assert_eq!(clock(3723), "1:02:03");
        assert_eq!(clock(90_000), "25:00:00");
        assert_eq!(words(0), "less than a minute");
        assert_eq!(words(59), "less than a minute");
        assert_eq!(words(60), "1 minute");
        assert_eq!(words(125), "2 minutes");
        assert_eq!(words(3600), "1 hour");
        assert_eq!(words(3600 + 5 * 60), "1 hour 5 minutes");
        assert_eq!(words(2 * 86_400 + 4 * 3600 + 59), "2 days 4 hours");
        assert_eq!(words(86_400 + 7 * 60), "1 day");
    }

    #[test]
    fn stamps_and_proc_files() {
        // 2026-10-22 13:05:09
        let t = 1_792_674_309;
        assert_eq!(stamp(t), "Oct 22 13:05:09");
        assert_eq!(since(t), "Oct 22 13:05");
        assert_eq!(stamp(0), "Jan  1 00:00:00");
        assert_eq!(parse_uptime("123.45 0.00\n"), 123);
        assert_eq!(parse_uptime(""), 0);
        let l = parse_load("0.02 0.01 0.00 1/19 15\n").unwrap();
        assert_eq!(l, ["0.02", "0.01", "0.00"]);
        assert!(parse_load("0.02").is_none());
        assert_eq!(
            uptime_line(7380, t, 12, Some(&l)),
            "up 2 hours 3 minutes (since Oct 22 11:02), 12 processes, load 0.02 0.01 0.00 (1, 5, 15 minutes)"
        );
        assert_eq!(
            uptime_line(30, t, 1, None),
            "up less than a minute (since Oct 22 13:04), 1 process"
        );
    }
}
