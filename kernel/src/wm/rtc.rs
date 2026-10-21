//! Wall-clock time for the menu bar: the PL031 RTC on qemu virt (0x0901_0000) gives seconds since
//! the Unix epoch at boot; we add uptime to it. qemu's RTC follows `-rtc base=` (UTC unless the
//! scripts pass `base=localtime`, which they do), so this is shown as local time with no zone maths.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::timer;

const PL031_DR: usize = 0x0901_0000;

/// Epoch seconds at uptime 0 (0 = no RTC; then we count from 2026-01-01 00:00).
static BASE: AtomicU64 = AtomicU64::new(0);

pub fn init() {
    // SAFETY: PL031 data register on qemu virt; device memory is identity-mapped.
    let now = unsafe { core::ptr::read_volatile(PL031_DR as *const u32) } as u64;
    let up = timer::now_ns() / 1_000_000_000;
    let base = if now > 1_600_000_000 {
        now - up
    } else {
        1_767_225_600 // 2026-01-01
    };
    BASE.store(base, Ordering::Relaxed);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: i32,
    /// 1..=12
    pub month: u32,
    /// 1..=31
    pub day: u32,
    /// 0 = Monday .. 6 = Sunday
    pub weekday: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

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
pub const DAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

pub fn now() -> DateTime {
    from_epoch(BASE.load(Ordering::Relaxed) + timer::now_ns() / 1_000_000_000)
}

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((y + (m <= 2) as i64) as i32, m as u32, d as u32)
}

pub fn from_epoch(secs: u64) -> DateTime {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil(days);
    DateTime {
        year,
        month,
        day,
        weekday: ((days + 3).rem_euclid(7)) as u32, // 1970-01-01 was a Thursday
        hour: (rem / 3600) as u32,
        minute: (rem / 60 % 60) as u32,
        second: (rem % 60) as u32,
    }
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Weekday (0 = Monday) of the first of the month.
pub fn first_weekday(dt: &DateTime) -> u32 {
    (dt.weekday + 35 - (dt.day - 1) % 7) % 7
}
