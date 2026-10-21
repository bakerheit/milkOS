//! Alarms, countdown timers and the stopwatch.
//!
//! Alarms and timers live in files so `alarmd` can ring them with the Clock window closed:
//!
//! ```text
//! alarm  <id>  <HH:MM>  <days>  <on|off>  <label>
//! timer  <id>  <running|paused>  <deadline_ms | remaining_ms>  <total_ms>  <label>
//! ```
//!
//! `days` is a weekday mask (bit 0 = Monday ... bit 6 = Sunday); 0 means "once": the next time
//! the clock reaches HH:MM, after which the alarm switches itself off. A running timer stores the
//! local clock time (ms) it ends at; a paused one stores what is left.

use crate::date::{Date, join_secs, split_secs};
use crate::text::{esc, fmt_hm, parse_hm, unesc};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub const MON_FRI: u8 = 0b001_1111;
pub const WEEKEND: u8 = 0b110_0000;
pub const EVERY_DAY: u8 = 0b111_1111;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Alarm {
    pub id: u32,
    pub label: String,
    /// Minutes since midnight.
    pub at: u16,
    pub days: u8,
    pub enabled: bool,
}

impl Alarm {
    pub fn new(id: u32, label: &str, at: u16, days: u8) -> Alarm {
        Alarm {
            id,
            label: label.to_string(),
            at,
            days: days & EVERY_DAY,
            enabled: true,
        }
    }

    fn on_day(&self, d: Date) -> bool {
        self.days == 0 || self.days & (1 << d.weekday()) != 0
    }

    /// The next ring strictly after `after` (local seconds), or `None` if the alarm is off.
    pub fn next_fire(&self, after: i64) -> Option<i64> {
        if !self.enabled {
            return None;
        }
        let (today, _) = split_secs(after);
        (0..=7).find_map(|k| {
            let d = today.add_days(k);
            let at = join_secs(d, self.at as u32 * 60);
            (at > after && self.on_day(d)).then_some(at)
        })
    }

    /// Does the alarm ring in (`last`, `now`]?
    fn rings_between(&self, last: i64, now: i64) -> bool {
        self.enabled && self.next_fire(last).is_some_and(|t| t <= now)
    }

    fn line(&self) -> String {
        format!(
            "alarm\t{}\t{}\t{}\t{}\t{}",
            self.id,
            fmt_hm(self.at),
            self.days,
            if self.enabled { "on" } else { "off" },
            esc(&self.label)
        )
    }

    fn from_line(line: &str) -> Option<Alarm> {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 || f[0] != "alarm" {
            return None;
        }
        Some(Alarm {
            id: f[1].parse().ok()?,
            at: parse_hm(f[2])?,
            days: f[3].parse::<u8>().ok().filter(|&d| d <= EVERY_DAY)?,
            enabled: match f[4] {
                "on" => true,
                "off" => false,
                _ => return None,
            },
            label: f.get(5).map_or(String::new(), |l| unesc(l)),
        })
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Alarms {
    pub list: Vec<Alarm>,
}

impl Alarms {
    /// Parse the file; returns the alarms and how many lines were skipped.
    pub fn parse(text: &str) -> (Alarms, usize) {
        let mut a = Alarms::default();
        let mut skipped = 0;
        for l in text
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        {
            match Alarm::from_line(l) {
                Some(x) if !a.list.iter().any(|y| y.id == x.id) => a.list.push(x),
                _ => skipped += 1,
            }
        }
        (a, skipped)
    }

    pub fn to_text(&self) -> String {
        let mut o = String::from(
            "# milkOS alarms: one per line, tab-separated (see timekit/src/clock.rs)\n",
        );
        for a in &self.list {
            o.push_str(&a.line());
            o.push('\n');
        }
        o
    }

    pub fn add(&mut self, mut a: Alarm) -> u32 {
        a.id = self.list.iter().map(|x| x.id).max().map_or(1, |m| m + 1);
        let id = a.id;
        self.list.push(a);
        id
    }

    pub fn remove(&mut self, id: u32) -> bool {
        let n = self.list.len();
        self.list.retain(|a| a.id != id);
        self.list.len() != n
    }

    /// Ring the alarms due in (`last`, `now`] (local seconds): returns (id, label) for each and
    /// switches off the "once" ones. Returns an empty list if `now` is not after `last`.
    pub fn take_due(&mut self, last: i64, now: i64) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        if now <= last {
            return out;
        }
        for a in &mut self.list {
            if a.rings_between(last, now) {
                out.push((a.id, a.label.clone()));
                if a.days == 0 {
                    a.enabled = false;
                }
            }
        }
        out
    }

    /// The alarm that rings soonest after `after`: (id, time).
    pub fn next(&self, after: i64) -> Option<(u32, i64)> {
        self.list
            .iter()
            .filter_map(|a| a.next_fire(after).map(|t| (a.id, t)))
            .min_by_key(|&(_, t)| t)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Timer {
    pub id: u32,
    pub label: String,
    pub total_ms: u64,
    pub running: bool,
    /// Running: local clock time (ms) it ends. Paused: milliseconds left.
    pub mark_ms: u64,
}

impl Timer {
    pub fn start(id: u32, label: &str, total_ms: u64, now_ms: u64) -> Timer {
        Timer {
            id,
            label: label.to_string(),
            total_ms,
            running: true,
            mark_ms: now_ms + total_ms,
        }
    }

    pub fn remaining_ms(&self, now_ms: u64) -> u64 {
        if self.running {
            self.mark_ms.saturating_sub(now_ms)
        } else {
            self.mark_ms
        }
    }

    pub fn pause(&mut self, now_ms: u64) {
        if self.running {
            self.mark_ms = self.remaining_ms(now_ms);
            self.running = false;
        }
    }

    pub fn resume(&mut self, now_ms: u64) {
        if !self.running {
            self.mark_ms += now_ms;
            self.running = true;
        }
    }

    /// Back to the full time, paused.
    pub fn reset(&mut self) {
        self.running = false;
        self.mark_ms = self.total_ms;
    }

    fn line(&self) -> String {
        format!(
            "timer\t{}\t{}\t{}\t{}\t{}",
            self.id,
            if self.running { "running" } else { "paused" },
            self.mark_ms,
            self.total_ms,
            esc(&self.label)
        )
    }

    fn from_line(line: &str) -> Option<Timer> {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 || f[0] != "timer" {
            return None;
        }
        Some(Timer {
            id: f[1].parse().ok()?,
            running: match f[2] {
                "running" => true,
                "paused" => false,
                _ => return None,
            },
            mark_ms: f[3].parse().ok()?,
            total_ms: f[4].parse().ok()?,
            label: f.get(5).map_or(String::new(), |l| unesc(l)),
        })
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Timers {
    pub list: Vec<Timer>,
}

impl Timers {
    pub fn parse(text: &str) -> (Timers, usize) {
        let mut t = Timers::default();
        let mut skipped = 0;
        for l in text
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        {
            match Timer::from_line(l) {
                Some(x) if !t.list.iter().any(|y| y.id == x.id) => t.list.push(x),
                _ => skipped += 1,
            }
        }
        (t, skipped)
    }

    pub fn to_text(&self) -> String {
        let mut o = String::from(
            "# milkOS timers: one per line, tab-separated (see timekit/src/clock.rs)\n",
        );
        for t in &self.list {
            o.push_str(&t.line());
            o.push('\n');
        }
        o
    }

    pub fn add(&mut self, mut t: Timer) -> u32 {
        t.id = self.list.iter().map(|x| x.id).max().map_or(1, |m| m + 1);
        let id = t.id;
        self.list.push(t);
        id
    }

    pub fn remove(&mut self, id: u32) -> bool {
        let n = self.list.len();
        self.list.retain(|t| t.id != id);
        self.list.len() != n
    }

    /// Remove and return the running timers that have reached zero.
    pub fn take_due(&mut self, now_ms: u64) -> Vec<Timer> {
        let (done, keep): (Vec<Timer>, Vec<Timer>) = core::mem::take(&mut self.list)
            .into_iter()
            .partition(|t| t.running && t.mark_ms <= now_ms);
        self.list = keep;
        done
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Stopwatch {
    /// Time banked from earlier runs.
    pub banked_ms: u64,
    /// When the current run started (monotonic ms); `None` = stopped.
    pub since: Option<u64>,
    /// Elapsed time at each lap press.
    pub laps: Vec<u64>,
}

impl Stopwatch {
    pub fn elapsed_ms(&self, now_ms: u64) -> u64 {
        self.banked_ms + self.since.map_or(0, |s| now_ms.saturating_sub(s))
    }

    pub fn running(&self) -> bool {
        self.since.is_some()
    }

    pub fn start(&mut self, now_ms: u64) {
        if self.since.is_none() {
            self.since = Some(now_ms);
        }
    }

    pub fn stop(&mut self, now_ms: u64) {
        if self.since.is_some() {
            self.banked_ms = self.elapsed_ms(now_ms);
            self.since = None;
        }
    }

    /// Record a lap (only while running).
    pub fn lap(&mut self, now_ms: u64) {
        if self.running() {
            let e = self.elapsed_ms(now_ms);
            self.laps.push(e);
        }
    }

    pub fn reset(&mut self) {
        *self = Stopwatch::default();
    }

    /// The length of each lap (the gap between lap marks), oldest first.
    pub fn lap_times(&self) -> Vec<u64> {
        let mut prev = 0;
        self.laps
            .iter()
            .map(|&t| {
                let d = t - prev;
                prev = t;
                d
            })
            .collect()
    }
}

/// "MM:SS.cc" (hundredths), or "H:MM:SS.cc" once past an hour.
pub fn fmt_stopwatch(ms: u64) -> String {
    let cs = ms / 10 % 100;
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}.{:02}", s / 3600, s / 60 % 60, s % 60, cs)
    } else {
        format!("{:02}:{:02}.{:02}", s / 60, s % 60, cs)
    }
}

/// A countdown as "MM:SS", or "H:MM:SS" from an hour up. Rounds up, so "00:01" shows until it hits zero.
pub fn fmt_countdown(ms: u64) -> String {
    let s = ms.div_ceil(1000);
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// "in 7 h 20 min", "in 5 min", "in under a minute": for the "next alarm" line.
pub fn fmt_in(secs: i64) -> String {
    let m = (secs.max(0) + 59) / 60;
    match (m / 60, m % 60) {
        (0, 0) => "in under a minute".to_string(),
        (0, m) => format!("in {m} min"),
        (h, 0) => format!("in {h} h"),
        (h, m) => format!("in {h} h {m} min"),
    }
}

/// Days as typed on a command line: `once`, `daily`, `weekdays`, `weekends`, or a list such as
/// `mon,wed,fri` (three-letter names, commas or spaces).
pub fn parse_days(s: &str) -> Option<u8> {
    let s = s.trim().to_ascii_lowercase();
    match s.as_str() {
        "once" => return Some(0),
        "daily" | "everyday" | "every" => return Some(EVERY_DAY),
        "weekdays" => return Some(MON_FRI),
        "weekends" => return Some(WEEKEND),
        _ => {}
    }
    const NAMES: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let mut mask = 0u8;
    for part in s.split([',', ' ']).filter(|p| !p.is_empty()) {
        let i = NAMES
            .iter()
            .position(|n| part.len() >= 3 && part.starts_with(n))?;
        mask |= 1 << i;
    }
    (mask != 0).then_some(mask)
}

/// A length of time as typed on a command line: `90`, `90s`, `5m`, `1h`, `1h30m`, `2m30s`.
/// A bare number is seconds. Returns milliseconds; zero is not a duration.
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    let (mut total, mut num, mut any) = (0u64, None::<u64>, false);
    for c in s.chars() {
        if let Some(d) = c.to_digit(10) {
            num = Some(num.unwrap_or(0).checked_mul(10)?.checked_add(d as u64)?);
            continue;
        }
        let unit = match c {
            'h' => 3_600_000,
            'm' => 60_000,
            's' => 1_000,
            _ => return None,
        };
        total = total.checked_add(num.take()?.checked_mul(unit)?)?;
        any = true;
    }
    if let Some(n) = num {
        if any {
            return None; // "1h30": a trailing number without a unit
        }
        total = n.checked_mul(1_000)?;
    }
    (total > 0).then_some(total)
}

/// Weekday mask as text: "Every day", "Weekdays", "Weekends", "Once", or "Mon Wed Fri".
pub fn fmt_days(mask: u8) -> String {
    const SHORT: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    match mask & EVERY_DAY {
        0 => "Once".to_string(),
        EVERY_DAY => "Every day".to_string(),
        MON_FRI => "Weekdays".to_string(),
        WEEKEND => "Weekends".to_string(),
        m => (0..7)
            .filter(|i| m & (1 << i) != 0)
            .map(|i| SHORT[i])
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u8, d: u8, hh: u32, mm: u32) -> i64 {
        join_secs(Date::new(y, m, d).unwrap(), hh * 3600 + mm * 60)
    }

    #[test]
    fn next_fire_once_rolls_to_tomorrow() {
        let a = Alarm::new(1, "wake", 7 * 60, 0);
        assert_eq!(
            a.next_fire(at(2025, 3, 3, 6, 0)),
            Some(at(2025, 3, 3, 7, 0))
        );
        assert_eq!(
            a.next_fire(at(2025, 3, 3, 7, 0)),
            Some(at(2025, 3, 4, 7, 0))
        ); // strictly after
        assert_eq!(
            a.next_fire(at(2025, 3, 3, 23, 59)),
            Some(at(2025, 3, 4, 7, 0))
        );
    }

    #[test]
    fn next_fire_respects_weekdays() {
        // 2025-03-07 is a Friday. A weekdays-only alarm after the Friday ring goes to Monday.
        let a = Alarm::new(1, "work", 8 * 60, MON_FRI);
        assert_eq!(
            a.next_fire(at(2025, 3, 7, 9, 0)),
            Some(at(2025, 3, 10, 8, 0))
        );
        let sat = Alarm::new(2, "sleep in", 10 * 60, 1 << 5);
        assert_eq!(
            sat.next_fire(at(2025, 3, 7, 9, 0)),
            Some(at(2025, 3, 8, 10, 0))
        );
        // A single weekday, asked just after it rang: a full week later.
        assert_eq!(
            sat.next_fire(at(2025, 3, 8, 10, 0)),
            Some(at(2025, 3, 15, 10, 0))
        );
    }

    #[test]
    fn disabled_never_fires() {
        let mut a = Alarm::new(1, "x", 60, EVERY_DAY);
        a.enabled = false;
        assert_eq!(a.next_fire(0), None);
        let mut list = Alarms {
            list: alloc::vec![a],
        };
        assert!(list.take_due(0, 10 * 86400).is_empty());
    }

    #[test]
    fn take_due_rings_once_and_switches_off_once_alarms() {
        let mut l = Alarms::default();
        l.add(Alarm::new(0, "once", 7 * 60, 0));
        l.add(Alarm::new(0, "daily", 7 * 60, EVERY_DAY));
        let ring = at(2025, 3, 3, 7, 0);
        let hits = l.take_due(ring - 1, ring);
        assert_eq!(hits.len(), 2);
        assert!(!l.list[0].enabled, "a once alarm switches itself off");
        assert!(l.list[1].enabled);
        // The same window replayed: only the daily alarm is still on.
        assert_eq!(l.take_due(ring - 1, ring).len(), 1);
        assert!(l.take_due(ring, ring + 60).is_empty()); // nothing is due right after
        assert_eq!(l.take_due(ring, ring + 86400).len(), 1); // the daily one, tomorrow
    }

    #[test]
    fn take_due_catches_a_late_tick_and_ignores_backwards_time() {
        let mut l = Alarms::default();
        l.add(Alarm::new(0, "x", 12 * 60, EVERY_DAY));
        let ring = at(2025, 3, 3, 12, 0);
        assert_eq!(l.take_due(ring - 30, ring + 30).len(), 1); // the daemon slept through it
        assert!(l.take_due(ring + 30, ring - 30).is_empty()); // clock stepped back
        assert!(l.take_due(ring, ring).is_empty());
    }

    #[test]
    fn ring_at_midnight_boundary() {
        let mut l = Alarms::default();
        l.add(Alarm::new(0, "midnight", 0, EVERY_DAY));
        let ring = at(2025, 3, 4, 0, 0);
        assert_eq!(l.take_due(ring - 1, ring).len(), 1);
        assert!(l.take_due(ring - 3600, ring - 1).is_empty());
    }

    #[test]
    fn next_alarm_picks_soonest() {
        let mut l = Alarms::default();
        l.add(Alarm::new(0, "late", 20 * 60, EVERY_DAY));
        l.add(Alarm::new(0, "early", 6 * 60, EVERY_DAY));
        let now = at(2025, 3, 3, 5, 0);
        assert_eq!(l.next(now), Some((2, at(2025, 3, 3, 6, 0))));
        assert_eq!(
            l.next(at(2025, 3, 3, 21, 0)),
            Some((2, at(2025, 3, 4, 6, 0)))
        );
        assert_eq!(Alarms::default().next(now), None);
    }

    #[test]
    fn alarm_file_roundtrip_and_junk() {
        let mut l = Alarms::default();
        l.add(Alarm::new(0, "Take\tpills\nnow", 8 * 60 + 15, MON_FRI));
        let mut off = Alarm::new(0, "", 23 * 60 + 59, 0);
        off.enabled = false;
        l.add(off);
        let (back, skipped) = Alarms::parse(&l.to_text());
        assert_eq!((back, skipped), (l.clone(), 0));
        let junk = "alarm\t1\t25:00\t0\ton\tbad time\nalarm\t2\t07:00\t200\ton\tbad mask\nalarm\t3\t07:00\t0\tmaybe\tbad state\nnope\nalarm\t4\t07:00\t0\ton\n";
        let (j, skipped) = Alarms::parse(junk);
        assert_eq!(skipped, 4);
        assert_eq!(j.list.len(), 1);
        assert_eq!(j.list[0].label, "");
    }

    #[test]
    fn timer_lifecycle() {
        let mut t = Timer::start(1, "tea", 180_000, 1_000_000);
        assert_eq!(t.remaining_ms(1_000_000), 180_000);
        assert_eq!(t.remaining_ms(1_060_000), 120_000);
        t.pause(1_060_000);
        assert_eq!(t.remaining_ms(9_999_999), 120_000); // paused: time does not pass
        t.resume(2_000_000);
        assert_eq!(t.remaining_ms(2_000_000), 120_000);
        assert_eq!(t.remaining_ms(5_000_000), 0);
        t.reset();
        assert!(!t.running);
        assert_eq!(t.remaining_ms(0), 180_000);
        t.pause(0); // pausing a paused timer is a no-op
        t.resume(10);
        assert_eq!(t.mark_ms, 180_010);
    }

    #[test]
    fn timers_take_due_and_roundtrip() {
        let mut l = Timers::default();
        l.add(Timer::start(0, "short", 1_000, 0));
        l.add(Timer::start(0, "long", 60_000, 0));
        let mut paused = Timer::start(0, "paused", 500, 0);
        paused.pause(100);
        l.add(paused);
        let (back, skipped) = Timers::parse(&l.to_text());
        assert_eq!((back, skipped), (l.clone(), 0));
        let due = l.take_due(1_000);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].label, "short");
        assert_eq!(l.list.len(), 2);
        assert!(l.take_due(1_000).is_empty()); // already taken
        let later = l.take_due(1_000_000); // paused never rings
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].label, "long");
        assert_eq!(l.list.len(), 1);
    }

    #[test]
    fn timer_file_junk() {
        let (t, skipped) = Timers::parse(
            "timer\t1\tsoon\t5\t5\tx\ntimer\tx\trunning\t5\t5\tx\ntimer\t2\trunning\t5\t5\nhello\n",
        );
        assert_eq!((t.list.len(), skipped), (1, 3));
    }

    #[test]
    fn stopwatch_laps() {
        let mut s = Stopwatch::default();
        s.lap(500); // not running: ignored
        assert!(s.laps.is_empty());
        s.start(1_000);
        s.start(9_999); // starting twice keeps the first start
        s.lap(3_000);
        s.stop(4_000);
        assert_eq!(s.elapsed_ms(99_999), 3_000); // stopped: frozen
        s.start(10_000);
        s.lap(10_500);
        assert_eq!(s.laps, [2_000, 3_500]);
        assert_eq!(s.lap_times(), [2_000, 1_500]);
        assert_eq!(s.elapsed_ms(11_000), 4_000);
        s.reset();
        assert_eq!(s, Stopwatch::default());
    }

    #[test]
    fn days_words() {
        assert_eq!(parse_days("once"), Some(0));
        assert_eq!(parse_days("Daily"), Some(EVERY_DAY));
        assert_eq!(parse_days("weekdays"), Some(MON_FRI));
        assert_eq!(parse_days("weekends"), Some(WEEKEND));
        assert_eq!(parse_days("mon,wed,fri"), Some(0b001_0101));
        assert_eq!(parse_days("Sat Sun"), Some(WEEKEND));
        assert_eq!(parse_days("monday,thursday"), Some(0b000_1001));
        assert_eq!(parse_days("mon,blah"), None);
        assert_eq!(parse_days(""), None);
        assert_eq!(parse_days("tomorrow"), None);
    }

    #[test]
    fn duration_words() {
        assert_eq!(parse_duration("90"), Some(90_000));
        assert_eq!(parse_duration("90s"), Some(90_000));
        assert_eq!(parse_duration("5m"), Some(300_000));
        assert_eq!(parse_duration("1h"), Some(3_600_000));
        assert_eq!(parse_duration("1h30m"), Some(5_400_000));
        assert_eq!(parse_duration("2m30s"), Some(150_000));
        assert_eq!(parse_duration("0"), None);
        assert_eq!(parse_duration("0s"), None);
        assert_eq!(parse_duration("1h30"), None);
        assert_eq!(parse_duration("m5"), None);
        assert_eq!(parse_duration("5x"), None);
        assert_eq!(parse_duration("soon"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("99999999999999999999h"), None); // overflow, not a wrap
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_stopwatch(0), "00:00.00");
        assert_eq!(fmt_stopwatch(61_230), "01:01.23");
        assert_eq!(fmt_stopwatch(3_723_450), "1:02:03.45");
        assert_eq!(fmt_countdown(0), "00:00");
        assert_eq!(fmt_countdown(1), "00:01"); // rounds up
        assert_eq!(fmt_countdown(59_001), "01:00");
        assert_eq!(fmt_countdown(3_600_000), "1:00:00");
        assert_eq!(fmt_in(0), "in under a minute");
        assert_eq!(fmt_in(61), "in 2 min");
        assert_eq!(fmt_in(3600), "in 1 h");
        assert_eq!(fmt_in(7 * 3600 + 20 * 60), "in 7 h 20 min");
        assert_eq!(fmt_days(0), "Once");
        assert_eq!(fmt_days(EVERY_DAY), "Every day");
        assert_eq!(fmt_days(MON_FRI), "Weekdays");
        assert_eq!(fmt_days(WEEKEND), "Weekends");
        assert_eq!(fmt_days(0b001_0101), "Mon Wed Fri");
    }
}
