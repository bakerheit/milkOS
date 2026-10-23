//! Date: time values in ms since the epoch, a fixed local offset from the embedder (no DST
//! rules), ISO / RFC 2822 / common free-form parsing, and en-US style formatting.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::math::{floor, fmod, trunc};
use crate::names::N;
use crate::object::{C, ObjKind, PropKey};
use crate::realm::{Realm, Res};
use crate::value::Val;

use super::arg;

const MS_PER_DAY: f64 = 86_400_000.0;
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTH_NAMES: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

fn day(t: f64) -> f64 {
    floor(t / MS_PER_DAY)
}

fn time_within_day(t: f64) -> f64 {
    let r = fmod(t, MS_PER_DAY);
    if r < 0.0 { r + MS_PER_DAY } else { r }
}

/// Days from 1970-01-01 to y-m-d (m 1..12), proleptic Gregorian.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// (year, month 0..11, day 1..31) of a day number.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m - 1, d)
}

/// MakeDay(year, month, date).
fn make_day(y: f64, m: f64, d: f64) -> f64 {
    if !y.is_finite() || !m.is_finite() || !d.is_finite() {
        return f64::NAN;
    }
    let y = trunc(y);
    let m = trunc(m);
    let d = trunc(d);
    let ym = y + floor(m / 12.0);
    if ym.abs() > 400_000.0 {
        return f64::NAN;
    }
    let mn = fmod(m, 12.0);
    let mn = if mn < 0.0 { mn + 12.0 } else { mn };
    let days = days_from_civil(ym as i64, mn as i64 + 1, 1);
    days as f64 + d - 1.0
}

fn make_time(h: f64, m: f64, s: f64, ms: f64) -> f64 {
    if !h.is_finite() || !m.is_finite() || !s.is_finite() || !ms.is_finite() {
        return f64::NAN;
    }
    trunc(h) * 3_600_000.0 + trunc(m) * 60_000.0 + trunc(s) * 1000.0 + trunc(ms)
}

fn make_date(day: f64, time: f64) -> f64 {
    if !day.is_finite() || !time.is_finite() {
        return f64::NAN;
    }
    let tv = day * MS_PER_DAY + time;
    if !tv.is_finite() { f64::NAN } else { tv }
}

fn time_clip(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > 8.64e15 {
        return f64::NAN;
    }
    trunc(t) + 0.0
}

/// The parts of a time value: (year, month, date, hours, minutes, seconds, ms, weekday).
fn parts(t: f64) -> (i64, i64, i64, i64, i64, i64, i64, i64) {
    let d = day(t);
    let (y, m, dd) = civil_from_days(d as i64);
    let tw = time_within_day(t) as i64;
    let wd = ((d as i64 % 7) + 11) % 7; // 1970-01-01 was a Thursday
    (
        y,
        m,
        dd,
        tw / 3_600_000,
        (tw / 60_000) % 60,
        (tw / 1000) % 60,
        tw % 1000,
        wd,
    )
}

impl Realm {
    fn now_ms(&self) -> f64 {
        match &self.clock {
            Some(f) => trunc(f()),
            None => 0.0,
        }
    }

    fn tz_ms(&self) -> f64 {
        self.tz_offset_min as f64 * 60_000.0
    }

    fn local(&self, t: f64) -> f64 {
        t + self.tz_ms()
    }

    fn utc(&self, t: f64) -> f64 {
        t - self.tz_ms()
    }

    fn this_time(&mut self, this: Val, name: &str) -> Res<f64> {
        if let Val::Obj(o) = this
            && let ObjKind::Date(t) = self.o(o).kind
        {
            return Ok(t);
        }
        Err(self.type_error(&alloc::format!(
            "this is not a Date object. (Date.prototype.{name})"
        )))
    }

    fn set_time_value(&mut self, this: Val, t: f64) -> f64 {
        let t = time_clip(t);
        if let Val::Obj(o) = this
            && let ObjKind::Date(v) = &mut self.om(o).kind
        {
            *v = t;
        }
        t
    }

    fn tz_string(&self) -> String {
        let off = self.tz_offset_min;
        let sign = if off < 0 { '-' } else { '+' };
        let a = off.unsigned_abs();
        alloc::format!("GMT{sign}{:02}{:02}", a / 60, a % 60)
    }

    fn tz_name(&self) -> &'static str {
        match self.tz_offset_min {
            0 => "Coordinated Universal Time",
            60 => "Central European Standard Time",
            120 => "Eastern European Standard Time",
            -300 => "Eastern Standard Time",
            -360 => "Central Standard Time",
            -420 => "Mountain Standard Time",
            -480 => "Pacific Standard Time",
            540 => "Japan Standard Time",
            480 => "China Standard Time",
            600 => "Australian Eastern Standard Time",
            780 => "New Zealand Daylight Time",
            720 => "New Zealand Standard Time",
            _ => "Local Time",
        }
    }

    pub(crate) fn date_to_string(&self, t: f64) -> String {
        if t.is_nan() {
            return String::from("Invalid Date");
        }
        let (y, m, d, h, mi, s, _, wd) = parts(self.local(t));
        alloc::format!(
            "{} {} {:02} {} {:02}:{:02}:{:02} {} ({})",
            DAYS[wd as usize],
            MONTHS[m as usize],
            d,
            year_str(y),
            h,
            mi,
            s,
            self.tz_string(),
            self.tz_name()
        )
    }
}

fn year_str(y: i64) -> String {
    if y < 0 {
        alloc::format!("-{:06}", -y)
    } else {
        alloc::format!("{:04}", y)
    }
}

fn iso_string(t: f64) -> String {
    let (y, m, d, h, mi, s, ms, _) = parts(t);
    let ys = if (0..=9999).contains(&y) {
        alloc::format!("{:04}", y)
    } else if y < 0 {
        alloc::format!("-{:06}", -y)
    } else {
        alloc::format!("+{:06}", y)
    };
    alloc::format!(
        "{ys}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        m + 1,
        d,
        h,
        mi,
        s,
        ms
    )
}

// ---- parsing ----

fn parse_iso(s: &str) -> Option<(f64, bool)> {
    // Returns (time value treating the fields as UTC, whether it had an explicit offset/UTC
    // or was date-only (both mean "UTC")).
    let b = s.as_bytes();
    let mut i = 0;
    let num = |i: &mut usize, n: usize| -> Option<i64> {
        if *i + n > b.len() {
            return None;
        }
        let mut v = 0i64;
        for k in 0..n {
            let c = b[*i + k];
            if !c.is_ascii_digit() {
                return None;
            }
            v = v * 10 + (c - b'0') as i64;
        }
        *i += n;
        Some(v)
    };
    let year = match b.first() {
        Some(b'+') | Some(b'-') => {
            let neg = b[0] == b'-';
            i = 1;
            let y = num(&mut i, 6)?;
            if neg && y == 0 {
                return None;
            }
            if neg { -y } else { y }
        }
        _ => num(&mut i, 4)?,
    };
    let mut month = 1;
    let mut dayn = 1;
    if i < b.len() && b[i] == b'-' {
        i += 1;
        month = num(&mut i, 2)?;
        if i < b.len() && b[i] == b'-' {
            i += 1;
            dayn = num(&mut i, 2)?;
        }
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&dayn) {
        return None;
    }
    let mut h = 0;
    let mut mi = 0;
    let mut sec = 0;
    let mut ms = 0.0;
    let mut utc = true;
    let mut has_time = false;
    if i < b.len() && (b[i] == b'T' || b[i] == b't' || b[i] == b' ') {
        i += 1;
        has_time = true;
        h = num(&mut i, 2)?;
        if i >= b.len() || b[i] != b':' {
            return None;
        }
        i += 1;
        mi = num(&mut i, 2)?;
        if i < b.len() && b[i] == b':' {
            i += 1;
            sec = num(&mut i, 2)?;
            if i < b.len() && (b[i] == b'.' || b[i] == b',') {
                i += 1;
                let start = i;
                let mut frac = 0.0;
                let mut scale = 100.0;
                while i < b.len() && b[i].is_ascii_digit() {
                    frac += (b[i] - b'0') as f64 * scale;
                    scale /= 10.0;
                    i += 1;
                }
                if i == start {
                    return None;
                }
                ms = trunc(frac);
            }
        }
        if h > 24 || mi > 59 || sec > 59 || (h == 24 && (mi > 0 || sec > 0 || ms > 0.0)) {
            return None;
        }
        utc = false;
    }
    let mut offset = 0i64;
    if i < b.len() {
        match b[i] {
            b'Z' | b'z' => {
                utc = true;
                i += 1;
            }
            b'+' | b'-' if has_time => {
                let neg = b[i] == b'-';
                i += 1;
                let oh = num(&mut i, 2)?;
                if i < b.len() && b[i] == b':' {
                    i += 1;
                }
                let om = num(&mut i, 2)?;
                offset = (oh * 60 + om) * if neg { -1 } else { 1 };
                utc = true;
            }
            _ => return None,
        }
    }
    if i != b.len() {
        return None;
    }
    let dv = make_day(year as f64, (month - 1) as f64, dayn as f64);
    let tv =
        make_date(dv, make_time(h as f64, mi as f64, sec as f64, ms)) - (offset * 60_000) as f64;
    Some((tv, utc))
}

/// A lenient parser for the forms pages use: "Tue, 21 Oct 2026 14:03:00 GMT",
/// "Oct 21, 2026", "October 21, 2026 2:03 PM", "2026/10/21 14:03", "10/21/2026".
fn parse_loose(s: &str, tz_min: i32) -> Option<f64> {
    let mut year: Option<i64> = None;
    let mut month: Option<i64> = None;
    let mut dayn: Option<i64> = None;
    let mut h = 0i64;
    let mut mi = 0i64;
    let mut sec = 0i64;
    let mut pm: Option<bool> = None;
    let mut offset: Option<i64> = None;
    let mut nums: Vec<i64> = Vec::new();
    // Drop parenthesized comments ("(Coordinated Universal Time)").
    let mut lower = alloc::string::String::new();
    let mut depth = 0;
    for c in s.to_ascii_lowercase().chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            c if depth == 0 => lower.push(c),
            _ => {}
        }
    }
    let toks: Vec<&str> = lower
        .split([' ', ',', '\t'])
        .filter(|t| !t.is_empty())
        .collect();
    for tok in toks {
        let t = tok.trim_matches(|c| c == '(' || c == ')');
        if t.contains(':') {
            let ps: Vec<&str> = t.split(':').collect();
            h = ps.first()?.parse().ok()?;
            mi = ps.get(1).map_or(Some(0), |x| x.parse().ok())?;
            sec = ps
                .get(2)
                .map_or(Some(0), |x| x.split('.').next().unwrap_or("0").parse().ok())?;
            continue;
        }
        if t == "am" || t == "pm" {
            pm = Some(t == "pm");
            continue;
        }
        if t == "gmt" || t == "utc" || t == "z" || t == "ut" {
            offset.get_or_insert(0);
            continue;
        }
        if let Some(rest) = t.strip_prefix("gmt").or_else(|| t.strip_prefix("utc"))
            && (rest.starts_with('+') || rest.starts_with('-'))
        {
            let neg = rest.starts_with('-');
            let d: String = rest[1..].chars().filter(|c| c.is_ascii_digit()).collect();
            let v: i64 = d.parse().ok()?;
            let (oh, om) = if d.len() <= 2 {
                (v, 0)
            } else {
                (v / 100, v % 100)
            };
            offset = Some((oh * 60 + om) * if neg { -1 } else { 1 });
            continue;
        }
        if (t.starts_with('+') || t.starts_with('-'))
            && t.len() >= 3
            && t[1..].chars().all(|c| c.is_ascii_digit())
        {
            let neg = t.starts_with('-');
            let v: i64 = t[1..].parse().ok()?;
            let (oh, om) = if t.len() <= 3 {
                (v, 0)
            } else {
                (v / 100, v % 100)
            };
            offset = Some((oh * 60 + om) * if neg { -1 } else { 1 });
            continue;
        }
        if t.contains('/')
            || (t.contains('-') && t.chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            let sep = if t.contains('/') { '/' } else { '-' };
            let ps: Vec<i64> = t.split(sep).filter_map(|x| x.parse().ok()).collect();
            if ps.len() == 3 {
                if ps[0] > 31 {
                    year = Some(ps[0]);
                    month = Some(ps[1]);
                    dayn = Some(ps[2]);
                } else {
                    month = Some(ps[0]);
                    dayn = Some(ps[1]);
                    year = Some(ps[2]);
                }
                continue;
            }
            return None;
        }
        if let Some(mi_) = MONTH_NAMES
            .iter()
            .position(|m| t.len() >= 3 && m.starts_with(t))
        {
            month = Some(mi_ as i64 + 1);
            continue;
        }
        if DAYS.iter().any(|d| t.starts_with(&d.to_ascii_lowercase())) {
            continue;
        }
        if let Ok(n) = t
            .trim_end_matches(|c| {
                c == '.' || c == 's' || c == 't' || c == 'h' || c == 'n' || c == 'd' || c == 'r'
            })
            .parse::<i64>()
        {
            nums.push(n);
            continue;
        }
        if matches!(
            t,
            "est" | "edt" | "cst" | "cdt" | "mst" | "mdt" | "pst" | "pdt"
        ) {
            offset = Some(match t {
                "est" => -300,
                "edt" => -240,
                "cst" => -360,
                "cdt" => -300,
                "mst" => -420,
                "mdt" => -360,
                "pst" => -480,
                _ => -420,
            });
            continue;
        }
        return None;
    }
    for n in nums {
        if dayn.is_none() && (1..=31).contains(&n) && month.is_some() {
            dayn = Some(n);
        } else if year.is_none() {
            year = Some(n);
        } else if dayn.is_none() && (1..=31).contains(&n) {
            dayn = Some(n);
        } else {
            return None;
        }
    }
    let mut y = year?;
    if (0..50).contains(&y) {
        y += 2000;
    } else if (50..100).contains(&y) {
        y += 1900;
    }
    let m = month?;
    let d = dayn.unwrap_or(1);
    if let Some(p) = pm {
        if h == 12 {
            h = 0;
        }
        if p {
            h += 12;
        }
    }
    let dv = make_day(y as f64, (m - 1) as f64, d as f64);
    let tv = make_date(dv, make_time(h as f64, mi as f64, sec as f64, 0.0));
    let off = offset.unwrap_or(tz_min as i64);
    Some(tv - (off * 60_000) as f64)
}

pub(crate) fn parse_date(s: &str, tz_min: i32) -> f64 {
    let s = s.trim();
    if let Some((tv, utc)) = parse_iso(s) {
        let tv = if utc {
            tv
        } else {
            tv - tz_min as f64 * 60_000.0
        };
        return time_clip(tv);
    }
    match parse_loose(s, tz_min) {
        Some(tv) => time_clip(tv),
        None => f64::NAN,
    }
}

// ---- the constructor and statics ----

fn date_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let nt = r.cur_new_target();
    if nt.is_undef() {
        let s = r.date_to_string(r.now_ms());
        return Ok(r.str_val(&s));
    }
    let tv = match args.len() {
        0 => r.now_ms(),
        1 => {
            let v = args[0];
            if let Val::Obj(o) = v
                && let ObjKind::Date(t) = r.o(o).kind
            {
                t
            } else {
                let p = r.to_primitive(v, crate::conv::Hint::Default)?;
                match p {
                    Val::Str(s) => {
                        let st = r.rust_string(s);
                        parse_date(&st, r.tz_offset_min)
                    }
                    _ => r.to_num(p)?,
                }
            }
        }
        _ => {
            let mut f = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
            for (i, a) in args.iter().take(7).enumerate() {
                f[i] = r.to_num(*a)?;
            }
            let mut y = f[0];
            if !y.is_nan() {
                let yi = trunc(y);
                if (0.0..=99.0).contains(&yi) {
                    y = 1900.0 + yi;
                }
            }
            let d = make_date(make_day(y, f[1], f[2]), make_time(f[3], f[4], f[5], f[6]));
            r.utc(d)
        }
    };
    let proto = r.proto_from_ctor(nt, |r| r.intr.date_proto)?;
    Ok(Val::Obj(r.alloc(Some(proto), ObjKind::Date(time_clip(tv)))))
}

fn now(r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    Ok(Val::num(r.now_ms()))
}

fn parse(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let s = r.to_rust_string(arg(args, 0))?;
    Ok(Val::num(parse_date(&s, r.tz_offset_min)))
}

fn utc(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let mut f = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    for (i, a) in args.iter().take(7).enumerate() {
        f[i] = r.to_num(*a)?;
    }
    let mut y = f[0];
    if !y.is_nan() {
        let yi = trunc(y);
        if (0.0..=99.0).contains(&yi) {
            y = 1900.0 + yi;
        }
    }
    Ok(Val::num(time_clip(make_date(
        make_day(y, f[1], f[2]),
        make_time(f[3], f[4], f[5], f[6]),
    ))))
}

// ---- getters ----

#[derive(Clone, Copy)]
enum Field {
    Year,
    Month,
    Date,
    Day,
    Hours,
    Minutes,
    Seconds,
    Ms,
}

fn get_field(r: &mut Realm, this: Val, f: Field, local: bool) -> Res<Val> {
    let t = r.this_time(this, "get")?;
    if t.is_nan() {
        return Ok(Val::Num(f64::NAN));
    }
    let t = if local { r.local(t) } else { t };
    let (y, m, d, h, mi, s, ms, wd) = parts(t);
    Ok(Val::num(match f {
        Field::Year => y,
        Field::Month => m,
        Field::Date => d,
        Field::Day => wd,
        Field::Hours => h,
        Field::Minutes => mi,
        Field::Seconds => s,
        Field::Ms => ms,
    } as f64))
}

macro_rules! getter {
    ($name:ident, $f:expr, $local:expr) => {
        fn $name(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
            get_field(r, this, $f, $local)
        }
    };
}

getter!(get_full_year, Field::Year, true);
getter!(get_month, Field::Month, true);
getter!(get_date, Field::Date, true);
getter!(get_day, Field::Day, true);
getter!(get_hours, Field::Hours, true);
getter!(get_minutes, Field::Minutes, true);
getter!(get_seconds, Field::Seconds, true);
getter!(get_ms, Field::Ms, true);
getter!(get_utc_full_year, Field::Year, false);
getter!(get_utc_month, Field::Month, false);
getter!(get_utc_date, Field::Date, false);
getter!(get_utc_day, Field::Day, false);
getter!(get_utc_hours, Field::Hours, false);
getter!(get_utc_minutes, Field::Minutes, false);
getter!(get_utc_seconds, Field::Seconds, false);
getter!(get_utc_ms, Field::Ms, false);

fn get_year(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let v = get_full_year(r, this, args)?;
    Ok(Val::num(v.as_f64().unwrap_or(f64::NAN) - 1900.0))
}

fn get_time(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    Ok(Val::num(r.this_time(this, "getTime")?))
}

fn get_tz_offset(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let t = r.this_time(this, "getTimezoneOffset")?;
    if t.is_nan() {
        return Ok(Val::Num(f64::NAN));
    }
    Ok(Val::num(0.0 - r.tz_offset_min as f64))
}

// ---- setters ----

/// Sets fields starting at `first` from the arguments (as many as the method takes).
fn set_fields(
    r: &mut Realm,
    this: Val,
    args: &[Val],
    first: Field,
    max: usize,
    local: bool,
) -> Res<Val> {
    let t = r.this_time(this, "set")?;
    let mut vals = Vec::new();
    for a in args.iter().take(max) {
        vals.push(r.to_num(*a)?);
    }
    if vals.is_empty() {
        vals.push(f64::NAN);
    }
    let base = if t.is_nan() {
        if matches!(first, Field::Year) {
            0.0
        } else {
            return Ok(Val::Num(f64::NAN));
        }
    } else if local {
        r.local(t)
    } else {
        t
    };
    let (y, m, d, h, mi, s, ms, _) = parts(base);
    let mut f = [
        y as f64, m as f64, d as f64, h as f64, mi as f64, s as f64, ms as f64,
    ];
    let start = match first {
        Field::Year => 0,
        Field::Month => 1,
        Field::Date => 2,
        Field::Hours => 3,
        Field::Minutes => 4,
        Field::Seconds => 5,
        Field::Ms => 6,
        Field::Day => 2,
    };
    for (i, v) in vals.iter().enumerate() {
        if start + i < 7 {
            f[start + i] = *v;
        }
    }
    let nt = make_date(
        make_day(f[0], f[1], f[2]),
        make_time(f[3], f[4], f[5], f[6]),
    );
    let nt = if local { r.utc(nt) } else { nt };
    Ok(Val::num(r.set_time_value(this, nt)))
}

macro_rules! setter {
    ($name:ident, $f:expr, $max:expr, $local:expr) => {
        fn $name(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
            set_fields(r, this, args, $f, $max, $local)
        }
    };
}

setter!(set_full_year, Field::Year, 3, true);
setter!(set_month, Field::Month, 2, true);
setter!(set_date, Field::Date, 1, true);
setter!(set_hours, Field::Hours, 4, true);
setter!(set_minutes, Field::Minutes, 3, true);
setter!(set_seconds, Field::Seconds, 2, true);
setter!(set_ms, Field::Ms, 1, true);
setter!(set_utc_full_year, Field::Year, 3, false);
setter!(set_utc_month, Field::Month, 2, false);
setter!(set_utc_date, Field::Date, 1, false);
setter!(set_utc_hours, Field::Hours, 4, false);
setter!(set_utc_minutes, Field::Minutes, 3, false);
setter!(set_utc_seconds, Field::Seconds, 2, false);
setter!(set_utc_ms, Field::Ms, 1, false);

fn set_time(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    r.this_time(this, "setTime")?;
    let t = r.to_num(arg(args, 0))?;
    Ok(Val::num(r.set_time_value(this, t)))
}

fn set_year(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let y = r.to_num(arg(args, 0))?;
    let y = if !y.is_nan() && (0.0..=99.0).contains(&trunc(y)) {
        trunc(y) + 1900.0
    } else {
        y
    };
    set_fields(r, this, &[Val::num(y)], Field::Year, 1, true)
}

// ---- formatting ----

fn to_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let t = r.this_time(this, "toString")?;
    let s = r.date_to_string(t);
    Ok(r.str_val(&s))
}

fn to_date_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let t = r.this_time(this, "toDateString")?;
    if t.is_nan() {
        return Ok(r.str_val("Invalid Date"));
    }
    let (y, m, d, _, _, _, _, wd) = parts(r.local(t));
    let s = alloc::format!(
        "{} {} {:02} {}",
        DAYS[wd as usize],
        MONTHS[m as usize],
        d,
        year_str(y)
    );
    Ok(r.str_val(&s))
}

fn to_time_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let t = r.this_time(this, "toTimeString")?;
    if t.is_nan() {
        return Ok(r.str_val("Invalid Date"));
    }
    let (_, _, _, h, mi, s, _, _) = parts(r.local(t));
    let st = alloc::format!(
        "{:02}:{:02}:{:02} {} ({})",
        h,
        mi,
        s,
        r.tz_string(),
        r.tz_name()
    );
    Ok(r.str_val(&st))
}

fn to_iso_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let t = r.this_time(this, "toISOString")?;
    if t.is_nan() {
        return Err(r.range_error("Invalid time value"));
    }
    let s = iso_string(t);
    Ok(r.str_val(&s))
}

fn to_utc_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let t = r.this_time(this, "toUTCString")?;
    if t.is_nan() {
        return Ok(r.str_val("Invalid Date"));
    }
    let (y, m, d, h, mi, s, _, wd) = parts(t);
    let st = alloc::format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[wd as usize],
        d,
        MONTHS[m as usize],
        year_str(y),
        h,
        mi,
        s
    );
    Ok(r.str_val(&st))
}

fn to_json(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.to_object(this)?;
    let tv = r.to_primitive(Val::Obj(o), crate::conv::Hint::Number)?;
    if let Some(n) = tv.as_f64()
        && !n.is_finite()
    {
        return Ok(Val::Null);
    }
    let k = r.key(N::toISOString);
    let f = r.get_p(o, k, Val::Obj(o))?;
    r.call_v(f, Val::Obj(o), &[])
}

fn locale_date(r: &Realm, t: f64) -> String {
    let (y, m, d, ..) = parts(r.local(t));
    alloc::format!("{}/{}/{}", m + 1, d, y)
}

fn locale_time(r: &Realm, t: f64) -> String {
    let (_, _, _, h, mi, s, _, _) = parts(r.local(t));
    let (h12, ap) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    let mut out = String::new();
    let _ = write!(out, "{}:{:02}:{:02} {}", h12, mi, s, ap);
    out
}

fn to_locale_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let t = r.this_time(this, "toLocaleString")?;
    if t.is_nan() {
        return Ok(r.str_val("Invalid Date"));
    }
    let s = alloc::format!("{}, {}", locale_date(r, t), locale_time(r, t));
    Ok(r.str_val(&s))
}

fn to_locale_date_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let t = r.this_time(this, "toLocaleDateString")?;
    if t.is_nan() {
        return Ok(r.str_val("Invalid Date"));
    }
    let s = locale_date(r, t);
    Ok(r.str_val(&s))
}

fn to_locale_time_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let t = r.this_time(this, "toLocaleTimeString")?;
    if t.is_nan() {
        return Ok(r.str_val("Invalid Date"));
    }
    let s = locale_time(r, t);
    Ok(r.str_val(&s))
}

fn value_of(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    Ok(Val::num(r.this_time(this, "valueOf")?))
}

fn to_primitive(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let Val::Obj(o) = this else {
        return Err(r.type_error("Date.prototype[Symbol.toPrimitive] called on non-object"));
    };
    let hint = r.to_rust_string(arg(args, 0))?;
    let order = match hint.as_str() {
        "string" | "default" => [N::toString, N::valueOf],
        "number" => [N::valueOf, N::toString],
        _ => return Err(r.type_error("Invalid hint")),
    };
    for n in order {
        let k = r.key(n);
        let f = r.get_p(o, k, this)?;
        if r.callable(f) {
            let v = r.call_v(f, this, &[])?;
            if !matches!(v, Val::Obj(_)) {
                return Ok(v);
            }
        }
    }
    Err(r.type_error("Cannot convert object to primitive value"))
}

pub(crate) fn init(r: &mut Realm) {
    let dp = r.intr.date_proto;
    let c = r.ctor("Date", 7, date_ctor, dp);
    r.method(c, "now", 0, now);
    r.method(c, "parse", 1, parse);
    r.method(c, "UTC", 7, utc);
    for (n, a, f) in [
        ("getFullYear", 0, get_full_year as crate::object::BuiltinFn),
        ("getMonth", 0, get_month),
        ("getDate", 0, get_date),
        ("getDay", 0, get_day),
        ("getHours", 0, get_hours),
        ("getMinutes", 0, get_minutes),
        ("getSeconds", 0, get_seconds),
        ("getMilliseconds", 0, get_ms),
        ("getUTCFullYear", 0, get_utc_full_year),
        ("getUTCMonth", 0, get_utc_month),
        ("getUTCDate", 0, get_utc_date),
        ("getUTCDay", 0, get_utc_day),
        ("getUTCHours", 0, get_utc_hours),
        ("getUTCMinutes", 0, get_utc_minutes),
        ("getUTCSeconds", 0, get_utc_seconds),
        ("getUTCMilliseconds", 0, get_utc_ms),
        ("getYear", 0, get_year),
        ("getTime", 0, get_time),
        ("getTimezoneOffset", 0, get_tz_offset),
        ("setFullYear", 3, set_full_year),
        ("setMonth", 2, set_month),
        ("setDate", 1, set_date),
        ("setHours", 4, set_hours),
        ("setMinutes", 3, set_minutes),
        ("setSeconds", 2, set_seconds),
        ("setMilliseconds", 1, set_ms),
        ("setUTCFullYear", 3, set_utc_full_year),
        ("setUTCMonth", 2, set_utc_month),
        ("setUTCDate", 1, set_utc_date),
        ("setUTCHours", 4, set_utc_hours),
        ("setUTCMinutes", 3, set_utc_minutes),
        ("setUTCSeconds", 2, set_utc_seconds),
        ("setUTCMilliseconds", 1, set_utc_ms),
        ("setTime", 1, set_time),
        ("setYear", 1, set_year),
        ("toString", 0, to_string),
        ("toDateString", 0, to_date_string),
        ("toTimeString", 0, to_time_string),
        ("toISOString", 0, to_iso_string),
        ("toUTCString", 0, to_utc_string),
        ("toGMTString", 0, to_utc_string),
        ("toJSON", 1, to_json),
        ("toLocaleString", 0, to_locale_string),
        ("toLocaleDateString", 0, to_locale_date_string),
        ("toLocaleTimeString", 0, to_locale_time_string),
        ("valueOf", 0, value_of),
    ] {
        r.method(dp, n, a, f);
    }
    let tp = r.sym.to_primitive;
    let f = r.method_sym(dp, tp, "[Symbol.toPrimitive]", 1, to_primitive);
    r.put_prop(dp, PropKey::Sym(tp), Val::Obj(f), C);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn civil() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 0, 1));
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 1, 29));
        assert_eq!(civil_from_days(-1), (1969, 11, 31));
        assert_eq!(iso_string(0.0), "1970-01-01T00:00:00.000Z");
        assert_eq!(parse_date("2026-10-21T14:03:00Z", 0), 1792591380000.0);
        assert_eq!(
            parse_date("2026-10-21", 600),
            parse_date("2026-10-21T00:00:00Z", 0)
        );
        assert_eq!(
            parse_date("Wed, 21 Oct 2026 14:03:00 GMT", 0),
            1792591380000.0
        );
        assert_eq!(parse_date("Oct 21, 2026 14:03:00 GMT", 0), 1792591380000.0);
        assert!(parse_date("nonsense", 0).is_nan());
    }
}
