//! Seconds since 1970 (milkOS time: the RTC's local time, no zones) as dates people read.

use alloc::format;
use alloc::string::String;

pub const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// (year, month 1..=12, day 1..=31, hour, minute, second).
pub fn civil(t: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    // civil_from_days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        y,
        m as u32,
        d as u32,
        (secs / 3600) as u32,
        (secs / 60 % 60) as u32,
        (secs % 60) as u32,
    )
}

/// "Oct 20 2026, 23:01".
pub fn when(t: i64) -> String {
    let (y, m, d, h, mi, _) = civil(t);
    format!("{} {d} {y}, {h:02}:{mi:02}", MONTHS[m as usize - 1])
}

/// Short, relative to `now`: "today 23:01", "yesterday 08:10", "Oct 20", "Oct 20 2025".
pub fn when_short(t: i64, now: i64) -> String {
    let (y, m, d, h, mi, _) = civil(t);
    let day = t.div_euclid(86_400);
    let today = now.div_euclid(86_400);
    if day == today {
        format!("today {h:02}:{mi:02}")
    } else if day + 1 == today {
        format!("yesterday {h:02}:{mi:02}")
    } else if y == civil(now).0 {
        format!("{} {d}", MONTHS[m as usize - 1])
    } else {
        format!("{} {d} {y}", MONTHS[m as usize - 1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        // 2026-10-20 23:01:05
        let t = 1_792_537_265;
        assert_eq!(civil(t), (2026, 10, 20, 23, 1, 5));
        assert_eq!(when(t), "Oct 20 2026, 23:01");
        assert_eq!(when(951_782_400), "Feb 29 2000, 00:00");
        assert_eq!(when_short(t, t + 60), "today 23:01");
        assert_eq!(when_short(t, t + 3600), "yesterday 23:01");
        assert_eq!(when_short(t, t + 10 * 86_400), "Oct 20");
        assert_eq!(when_short(t, t + 100 * 86_400), "Oct 20 2026");
    }
}
