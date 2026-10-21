//! Number formatting for the tables and headlines.

use alloc::format;
use alloc::string::String;

/// kilobytes as "152.0 MB"-style text: megabytes with one decimal.
pub fn mb(kb: u64) -> String {
    format!("{}.{} MB", kb / 1024, (kb % 1024) * 10 / 1024)
}

/// kilobytes as a whole number of megabytes, "124 MB".
pub fn mb0(kb: u64) -> String {
    format!("{} MB", (kb + 512) / 1024)
}

/// Tenths of a percent as "12.3".
pub fn pct10(t: u32) -> String {
    format!("{}.{}", t / 10, t % 10)
}

/// Bytes per second as "12 KB/s" or "3.4 MB/s".
pub fn rate(bytes: u64) -> String {
    if bytes >= 1 << 20 {
        format!(
            "{}.{} MB/s",
            bytes >> 20,
            ((bytes & ((1 << 20) - 1)) * 10) >> 20
        )
    } else {
        format!("{} KB/s", bytes >> 10)
    }
}

/// "HH:MM:SS" for wall-clock nanoseconds since 1970 (the RTC is local time).
pub fn clock(ns: u64) -> String {
    let s = ns / 1_000_000_000 % 86400;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// How long ago, from two nanosecond times: "5 s", "3 min", "2 h", "4 d".
pub fn ago(now_ns: u64, then_ns: u64) -> String {
    let s = now_ns.saturating_sub(then_ns) / 1_000_000_000;
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", s / 60),
        3600..=86399 => format!("{} h", s / 3600),
        _ => format!("{} d", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_rates() {
        assert_eq!(mb(152), "0.1 MB");
        assert_eq!(mb(1024), "1.0 MB");
        assert_eq!(mb(1536), "1.5 MB");
        assert_eq!(mb0(126_976), "124 MB");
        assert_eq!(pct10(0), "0.0");
        assert_eq!(pct10(1234), "123.4");
        assert_eq!(rate(2048), "2 KB/s");
        assert_eq!(rate(3 * 1024 * 1024 + 512 * 1024), "3.5 MB/s");
        assert_eq!(rate(0), "0 KB/s");
    }

    #[test]
    fn times() {
        assert_eq!(
            clock(13 * 3_600_000_000_000 + 5 * 60_000_000_000 + 9_000_000_000),
            "13:05:09"
        );
        assert_eq!(clock(86_400_000_000_000 + 1_000_000_000), "00:00:01");
        let s = 1_000_000_000u64;
        assert_eq!(ago(100 * s, 95 * s), "5 s");
        assert_eq!(ago(1000 * s, 1000 * s - 200 * s), "3 min");
        assert_eq!(ago(10_000 * s, 0), "2 h");
        assert_eq!(ago(500_000 * s, 0), "5 d");
        assert_eq!(ago(5, 10), "0 s");
    }
}
