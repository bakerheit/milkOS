//! Calendar dates (proleptic Gregorian) and the month grid.

use alloc::format;
use alloc::string::String;

pub const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
pub const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

pub fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn days_in_month(y: i32, m: u8) -> u8 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        _ => 28,
    }
}

/// Days since 1970-01-01 for a civil date.
pub fn days_from_civil(y: i32, m: u8, d: u8) -> i64 {
    let (m, d) = (m as i64, d as i64);
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The civil date for days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i32, u8, u8) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((if m <= 2 { y + 1 } else { y }) as i32, m as u8, d as u8)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Date {
    pub y: i32,
    pub m: u8,
    pub d: u8,
}

impl Date {
    pub fn new(y: i32, m: u8, d: u8) -> Option<Date> {
        ((1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m)).then_some(Date { y, m, d })
    }

    pub fn days(self) -> i64 {
        days_from_civil(self.y, self.m, self.d)
    }

    pub fn from_days(z: i64) -> Date {
        let (y, m, d) = civil_from_days(z);
        Date { y, m, d }
    }

    /// 0 = Monday ... 6 = Sunday.
    pub fn weekday(self) -> u8 {
        (self.days() + 3).rem_euclid(7) as u8
    }

    pub fn add_days(self, n: i64) -> Date {
        Date::from_days(self.days() + n)
    }

    /// Whole months later (or earlier); the day is clamped to the length of the target month
    /// (31 January + 1 month = 28 or 29 February).
    pub fn add_months(self, n: i32) -> Date {
        let idx = self.y * 12 + (self.m as i32 - 1) + n;
        let (y, m) = (idx.div_euclid(12), (idx.rem_euclid(12) + 1) as u8);
        Date {
            y,
            m,
            d: self.d.min(days_in_month(y, m)),
        }
    }

    /// "YYYY-MM-DD".
    pub fn fmt(self) -> String {
        format!("{:04}-{:02}-{:02}", self.y, self.m, self.d)
    }

    pub fn parse(s: &str) -> Option<Date> {
        let mut it = s.trim().splitn(3, '-');
        let y: i32 = it.next()?.parse().ok()?;
        let m: u8 = it.next()?.parse().ok()?;
        let d: u8 = it.next()?.parse().ok()?;
        Date::new(y, m, d)
    }
}

/// A day as typed on a command line: `today`, `tomorrow`, `+3d`, a weekday name (the next one,
/// after today) or `YYYY-MM-DD`.
pub fn parse_when(s: &str, today: Date) -> Option<Date> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "today" => return Some(today),
        "tomorrow" => return Some(today.add_days(1)),
        _ => {}
    }
    if let Some(n) = lower.strip_prefix('+').and_then(|r| r.strip_suffix('d')) {
        return n.parse::<u16>().ok().map(|n| today.add_days(n as i64));
    }
    if lower.len() >= 3
        && let Some(wd) = WEEKDAYS
            .iter()
            .position(|w| w.to_ascii_lowercase().starts_with(&lower))
    {
        let ahead = (wd as i64 - today.weekday() as i64 - 1).rem_euclid(7) + 1;
        return Some(today.add_days(ahead));
    }
    Date::parse(s)
}

/// Split local seconds since the epoch into a date and the second of the day.
pub fn split_secs(secs: i64) -> (Date, u32) {
    (
        Date::from_days(secs.div_euclid(86400)),
        secs.rem_euclid(86400) as u32,
    )
}

pub fn join_secs(d: Date, sec_of_day: u32) -> i64 {
    d.days() * 86400 + sec_of_day as i64
}

/// The first whole minute that is at least `dur_ms` after `now_secs`: its date and minutes since
/// midnight. For "in 5 minutes" style times, because alarms and events have minute resolution.
pub fn minute_after(now_secs: i64, dur_ms: u64) -> (Date, u16) {
    let target = now_secs + dur_ms.div_ceil(1000) as i64;
    let (d, sod) = split_secs((target + 59).div_euclid(60) * 60);
    (d, (sod / 60) as u16)
}

/// The weeks of a month: 6 rows of 7 day numbers, 0 where the cell belongs to another month.
pub fn month_grid(y: i32, m: u8, monday_first: bool) -> [[u8; 7]; 6] {
    let first = Date { y, m, d: 1 }.weekday();
    let lead = if monday_first { first } else { (first + 1) % 7 } as usize;
    let mut g = [[0u8; 7]; 6];
    for d in 1..=days_in_month(y, m) {
        let cell = lead + d as usize - 1;
        g[cell / 7][cell % 7] = d;
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_known_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(Date::new(1970, 1, 1).unwrap().weekday(), 3); // Thursday
        assert_eq!(Date::new(2024, 2, 29).unwrap().weekday(), 3); // Thursday
        assert_eq!(Date::new(2000, 1, 1).unwrap().weekday(), 5); // Saturday
    }

    #[test]
    fn civil_roundtrip_across_centuries() {
        for z in (-800_000..800_000).step_by(37) {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z, "{y}-{m}-{d}");
            assert!(Date::new(y, m, d).is_some());
        }
    }

    #[test]
    fn leap_rules() {
        assert!(is_leap(2000) && is_leap(2024) && !is_leap(1900) && !is_leap(2023));
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2023, 2), 28);
        assert_eq!(days_in_month(2023, 4), 30);
        assert!(Date::new(2023, 2, 29).is_none());
        assert!(Date::new(2023, 13, 1).is_none());
        assert!(Date::new(2023, 0, 1).is_none());
        assert!(Date::new(2023, 4, 31).is_none());
    }

    #[test]
    fn add_months_clamps() {
        let d = Date::new(2024, 1, 31).unwrap();
        assert_eq!(d.add_months(1), Date::new(2024, 2, 29).unwrap());
        assert_eq!(d.add_months(13), Date::new(2025, 2, 28).unwrap());
        assert_eq!(d.add_months(-2), Date::new(2023, 11, 30).unwrap());
        assert_eq!(d.add_months(12), Date::new(2025, 1, 31).unwrap());
        assert_eq!(
            Date::new(2024, 1, 15).unwrap().add_months(-1),
            Date::new(2023, 12, 15).unwrap()
        );
    }

    #[test]
    fn parse_and_format() {
        let d = Date::parse("2025-03-09").unwrap();
        assert_eq!(d.fmt(), "2025-03-09");
        assert!(Date::parse("2025-02-30").is_none());
        assert!(Date::parse("garbage").is_none());
        assert!(Date::parse("2025-3").is_none());
    }

    #[test]
    fn when_words() {
        let thu = Date::new(2025, 3, 6).unwrap();
        assert_eq!(thu.weekday(), 3);
        assert_eq!(parse_when("today", thu), Some(thu));
        assert_eq!(parse_when("Tomorrow", thu), Some(thu.add_days(1)));
        assert_eq!(parse_when("+10d", thu), Some(thu.add_days(10)));
        assert_eq!(parse_when("fri", thu), Some(thu.add_days(1)));
        assert_eq!(parse_when("thu", thu), Some(thu.add_days(7))); // strictly after today
        assert_eq!(parse_when("monday", thu), Some(thu.add_days(4)));
        assert_eq!(parse_when("2025-12-25", thu), Date::new(2025, 12, 25));
        assert_eq!(parse_when("2025-02-30", thu), None);
        assert_eq!(parse_when("someday", thu), None);
        assert_eq!(parse_when("+xd", thu), None);
        assert_eq!(parse_when("", thu), None);
    }

    #[test]
    fn minute_after_rounds_up_and_rolls_over() {
        let day = Date::new(2025, 3, 6).unwrap();
        let t = |h: u32, m: u32, s: u32| join_secs(day, h * 3600 + m * 60 + s);
        assert_eq!(minute_after(t(10, 0, 0), 5_000), (day, 10 * 60 + 1));
        assert_eq!(minute_after(t(10, 0, 55), 5_000), (day, 10 * 60 + 1)); // exactly on the minute
        assert_eq!(minute_after(t(10, 0, 56), 5_000), (day, 10 * 60 + 2));
        assert_eq!(minute_after(t(10, 0, 0), 5 * 60_000), (day, 10 * 60 + 5));
        assert_eq!(minute_after(t(10, 0, 0), 1), (day, 10 * 60 + 1));
        let (d, m) = minute_after(t(23, 59, 30), 60_000);
        assert_eq!((d, m), (day.add_days(1), 1));
    }

    #[test]
    fn secs_split_join() {
        let (d, s) = split_secs(86400 * 365 + 3661);
        assert_eq!(d, Date::new(1971, 1, 1).unwrap());
        assert_eq!(s, 3661);
        assert_eq!(join_secs(d, s), 86400 * 365 + 3661);
        let (d, s) = split_secs(-1);
        assert_eq!(d, Date::new(1969, 12, 31).unwrap());
        assert_eq!(s, 86399);
    }

    #[test]
    fn grid() {
        // February 2024 starts on a Thursday and has 29 days.
        let g = month_grid(2024, 2, true);
        assert_eq!(g[0], [0, 0, 0, 1, 2, 3, 4]);
        assert_eq!(g[4], [26, 27, 28, 29, 0, 0, 0]);
        let g = month_grid(2024, 2, false); // Sunday first
        assert_eq!(g[0], [0, 0, 0, 0, 1, 2, 3]);
        // The longest layout: a 31-day month starting on the last cell column needs 6 rows.
        let g = month_grid(2023, 7, false); // July 2023 starts on a Saturday
        assert_eq!(g[0][6], 1);
        assert_eq!(g[5][1], 31);
        let total: u32 = g.iter().flatten().map(|&d| d as u32).sum();
        assert_eq!(total, (1..=31).sum());
    }
}
