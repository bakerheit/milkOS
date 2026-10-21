//! Calendar events: repeat rules, the file format and "what happens on this day".
//!
//! One event per line, tab-separated, so a text editor (or `grep`) works on the file:
//!
//! ```text
//! event  <id>  <date>  <start|->  <end|->  <repeat[:every]>  <until|->  <alert|->  <title>  <notes>
//! ```
//!
//! `start`/`end` are `HH:MM` (`-` = all day), `repeat` is `none`, `daily`, `weekly`, `monthly`
//! or `yearly`, `alert` is minutes before the start. Lines that do not parse are skipped and
//! counted, never fatal, so a hand-edited file cannot lock the app.

use crate::date::{Date, days_in_month, join_secs, split_secs};
use crate::text::{esc, fmt_hm, parse_hm, unesc};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Repeat {
    None,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Repeat {
    fn name(self) -> &'static str {
        match self {
            Repeat::None => "none",
            Repeat::Daily => "daily",
            Repeat::Weekly => "weekly",
            Repeat::Monthly => "monthly",
            Repeat::Yearly => "yearly",
        }
    }

    fn from_name(s: &str) -> Option<Repeat> {
        Some(match s {
            "none" => Repeat::None,
            "daily" => Repeat::Daily,
            "weekly" => Repeat::Weekly,
            "monthly" => Repeat::Monthly,
            "yearly" => Repeat::Yearly,
            _ => return None,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Event {
    pub id: u32,
    pub title: String,
    pub notes: String,
    pub date: Date,
    /// Minutes since midnight; `None` = all day.
    pub start: Option<u16>,
    pub end: Option<u16>,
    pub repeat: Repeat,
    /// Repeat every N days/weeks/months/years (at least 1).
    pub every: u16,
    /// Last day the event may occur (inclusive).
    pub until: Option<Date>,
    /// Minutes before `start` to notify.
    pub alert: Option<u16>,
}

impl Event {
    pub fn new(id: u32, title: &str, date: Date) -> Event {
        Event {
            id,
            title: title.to_string(),
            notes: String::new(),
            date,
            start: None,
            end: None,
            repeat: Repeat::None,
            every: 1,
            until: None,
            alert: None,
        }
    }

    pub fn occurs_on(&self, d: Date) -> bool {
        if d < self.date || self.until.is_some_and(|u| d > u) {
            return false;
        }
        let n = self.every.max(1) as i64;
        let day_gap = d.days() - self.date.days();
        match self.repeat {
            Repeat::None => d == self.date,
            Repeat::Daily => day_gap % n == 0,
            Repeat::Weekly => day_gap % (7 * n) == 0,
            Repeat::Monthly => {
                let gap =
                    (d.y as i64 * 12 + d.m as i64) - (self.date.y as i64 * 12 + self.date.m as i64);
                gap % n == 0 && d.d == self.date.d.min(days_in_month(d.y, d.m))
            }
            Repeat::Yearly => {
                (d.y as i64 - self.date.y as i64) % n == 0
                    && d.m == self.date.m
                    && d.d == self.date.d.min(days_in_month(d.y, d.m))
            }
        }
    }

    fn line(&self) -> String {
        let hm = |v: Option<u16>| v.map_or("-".to_string(), fmt_hm);
        let rep = if self.every > 1 {
            format!("{}:{}", self.repeat.name(), self.every)
        } else {
            self.repeat.name().to_string()
        };
        format!(
            "event\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.id,
            self.date.fmt(),
            hm(self.start),
            hm(self.end),
            rep,
            self.until.map_or("-".to_string(), |d| d.fmt()),
            self.alert.map_or("-".to_string(), |a| a.to_string()),
            esc(&self.title),
            esc(&self.notes),
        )
    }

    fn from_line(line: &str) -> Option<Event> {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 9 || f[0] != "event" {
            return None;
        }
        let opt_hm = |s: &str| {
            if s == "-" {
                Some(None)
            } else {
                parse_hm(s).map(Some)
            }
        };
        let (rep, every) = match f[5].split_once(':') {
            Some((r, n)) => (r, n.parse::<u16>().ok().filter(|&n| n >= 1)?),
            None => (f[5], 1),
        };
        let start = opt_hm(f[3])?;
        let end = opt_hm(f[4])?;
        if let (Some(s), Some(e)) = (start, end)
            && e < s
        {
            return None;
        }
        Some(Event {
            id: f[1].parse().ok()?,
            date: Date::parse(f[2])?,
            start,
            end,
            repeat: Repeat::from_name(rep)?,
            every,
            until: if f[6] == "-" {
                None
            } else {
                Some(Date::parse(f[6])?)
            },
            alert: if f[7] == "-" {
                None
            } else {
                Some(f[7].parse().ok()?)
            },
            title: unesc(f[8]),
            notes: f.get(9).map_or(String::new(), |n| unesc(n)),
        })
    }

    /// The sort key within a day: all-day events first, then by start time, then title.
    fn day_key(&self) -> (u16, &str) {
        (self.start.map_or(0, |s| s + 1), &self.title)
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Calendar {
    pub events: Vec<Event>,
}

/// Most days a range query scans, so a bad date cannot spin the app.
pub const MAX_RANGE_DAYS: i64 = 3700;

impl Calendar {
    /// Parse a file. Returns the calendar and how many lines were skipped.
    pub fn parse(text: &str) -> (Calendar, usize) {
        let mut c = Calendar::default();
        let mut skipped = 0;
        for l in text
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        {
            match Event::from_line(l) {
                Some(e) if !c.events.iter().any(|x| x.id == e.id) => c.events.push(e),
                _ => skipped += 1,
            }
        }
        (c, skipped)
    }

    pub fn to_text(&self) -> String {
        let mut o = String::from(
            "# milkOS calendar: one event per line, tab-separated (see timekit/src/cal.rs)\n",
        );
        for e in &self.events {
            o.push_str(&e.line());
            o.push('\n');
        }
        o
    }

    pub fn next_id(&self) -> u32 {
        self.events.iter().map(|e| e.id).max().map_or(1, |m| m + 1)
    }

    /// Add an event, giving it a fresh id. Returns the id.
    pub fn add(&mut self, mut e: Event) -> u32 {
        e.id = self.next_id();
        let id = e.id;
        self.events.push(e);
        id
    }

    pub fn remove(&mut self, id: u32) -> bool {
        let n = self.events.len();
        self.events.retain(|e| e.id != id);
        self.events.len() != n
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Event> {
        self.events.iter_mut().find(|e| e.id == id)
    }

    /// The events that happen on `d`, all-day first, then by time.
    pub fn on(&self, d: Date) -> Vec<&Event> {
        let mut v: Vec<&Event> = self.events.iter().filter(|e| e.occurs_on(d)).collect();
        v.sort_by(|a, b| a.day_key().cmp(&b.day_key()));
        v
    }

    /// How many events happen on each day of a month (index 0 is the 1st): for the dots on the grid.
    pub fn month_counts(&self, y: i32, m: u8) -> Vec<u8> {
        (1..=days_in_month(y, m))
            .map(|d| {
                let day = Date { y, m, d };
                self.events
                    .iter()
                    .filter(|e| e.occurs_on(day))
                    .count()
                    .min(255) as u8
            })
            .collect()
    }

    /// Everything from `from` to `to` inclusive, in date order.
    pub fn between(&self, from: Date, to: Date) -> Vec<(Date, &Event)> {
        let mut out = Vec::new();
        let (a, b) = (from.days(), to.days().min(from.days() + MAX_RANGE_DAYS));
        for z in a..=b {
            let d = Date::from_days(z);
            for e in self.on(d) {
                out.push((d, e));
            }
        }
        out
    }

    /// Alerts whose time falls in (`last`, `now`] (local seconds). One entry per (event, day).
    pub fn alerts_due(&self, last: i64, now: i64) -> Vec<(u32, Date, String)> {
        let mut out = Vec::new();
        if now <= last {
            return out;
        }
        let (first, _) = split_secs(last);
        let (lastday, _) = split_secs(now);
        // An alert can fire the day before its event, so look one day ahead.
        for z in first.days()..=lastday.days().min(first.days() + MAX_RANGE_DAYS) + 1 {
            let d = Date::from_days(z);
            for e in self.events.iter().filter(|e| e.occurs_on(d)) {
                let (Some(start), Some(alert)) = (e.start, e.alert) else {
                    continue;
                };
                let at = join_secs(d, 0) + start as i64 * 60 - alert as i64 * 60;
                if at > last && at <= now {
                    out.push((e.id, d, e.title.clone()));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u8, dd: u8) -> Date {
        Date::new(y, m, dd).unwrap()
    }

    #[test]
    fn single_event() {
        let e = Event::new(1, "Dentist", d(2025, 5, 6));
        assert!(e.occurs_on(d(2025, 5, 6)));
        assert!(!e.occurs_on(d(2025, 5, 7)));
        assert!(!e.occurs_on(d(2025, 5, 5)));
    }

    #[test]
    fn daily_and_weekly_every_n() {
        let mut e = Event::new(1, "x", d(2025, 1, 1));
        e.repeat = Repeat::Daily;
        e.every = 3;
        assert!(e.occurs_on(d(2025, 1, 4)) && e.occurs_on(d(2025, 1, 7)));
        assert!(!e.occurs_on(d(2025, 1, 5)));
        assert!(!e.occurs_on(d(2024, 12, 29))); // before the start, even on the cadence
        e.repeat = Repeat::Weekly;
        e.every = 2;
        assert!(e.occurs_on(d(2025, 1, 15)));
        assert!(!e.occurs_on(d(2025, 1, 8)));
    }

    #[test]
    fn monthly_clamps_to_short_months() {
        let mut e = Event::new(1, "rent", d(2025, 1, 31));
        e.repeat = Repeat::Monthly;
        assert!(e.occurs_on(d(2025, 2, 28)));
        assert!(!e.occurs_on(d(2025, 2, 27)));
        assert!(e.occurs_on(d(2025, 3, 31)));
        assert!(e.occurs_on(d(2025, 4, 30)));
        assert!(e.occurs_on(d(2028, 2, 29))); // leap year
        e.every = 2;
        assert!(e.occurs_on(d(2025, 3, 31)));
        assert!(!e.occurs_on(d(2025, 2, 28)));
        assert!(!e.occurs_on(d(2025, 4, 30)));
    }

    #[test]
    fn yearly_leap_day() {
        let mut e = Event::new(1, "leap", d(2024, 2, 29));
        e.repeat = Repeat::Yearly;
        assert!(e.occurs_on(d(2025, 2, 28)));
        assert!(e.occurs_on(d(2028, 2, 29)));
        assert!(!e.occurs_on(d(2025, 3, 1)));
        e.every = 4;
        assert!(e.occurs_on(d(2028, 2, 29)));
        assert!(!e.occurs_on(d(2025, 2, 28)));
    }

    #[test]
    fn until_is_inclusive() {
        let mut e = Event::new(1, "x", d(2025, 1, 1));
        e.repeat = Repeat::Daily;
        e.until = Some(d(2025, 1, 10));
        assert!(e.occurs_on(d(2025, 1, 10)));
        assert!(!e.occurs_on(d(2025, 1, 11)));
    }

    #[test]
    fn file_roundtrip_with_odd_text() {
        let mut c = Calendar::default();
        let mut e = Event::new(0, "Lunch\twith Sam", d(2025, 6, 1));
        e.notes = "Bring the book\nSecond line \\ backslash".into();
        e.start = Some(12 * 60 + 30);
        e.end = Some(13 * 60 + 30);
        e.repeat = Repeat::Weekly;
        e.every = 2;
        e.until = Some(d(2025, 12, 31));
        e.alert = Some(15);
        c.add(e);
        c.add(Event::new(0, "Holiday", d(2025, 7, 4)));
        let text = c.to_text();
        let (back, skipped) = Calendar::parse(&text);
        assert_eq!(skipped, 0);
        assert_eq!(back, c);
        assert_eq!(text.lines().count(), 3); // header + two events, notes did not break lines
    }

    #[test]
    fn bad_lines_are_skipped_not_fatal() {
        let text = "# header\n\
            event\t1\t2025-01-01\t-\t-\tnone\t-\t-\tok\t\n\
            event\t2\t2025-02-30\t-\t-\tnone\t-\t-\tbad date\t\n\
            event\t3\t2025-01-01\t10:00\t09:00\tnone\t-\t-\tends before it starts\t\n\
            event\t4\t2025-01-01\t-\t-\tsometimes\t-\t-\tbad repeat\t\n\
            event\t1\t2025-01-02\t-\t-\tnone\t-\t-\tduplicate id\t\n\
            event\t5\t2025-01-01\t-\t-\tdaily:0\t-\t-\tzero interval\t\n\
            garbage line\n\
            \n\
            event\t6\t2025-01-01\t-\t-\tnone\t-\t-\tno notes field\n";
        let (c, skipped) = Calendar::parse(text);
        assert_eq!(c.events.len(), 2);
        assert_eq!(skipped, 6);
        assert_eq!(c.events[1].title, "no notes field");
    }

    #[test]
    fn day_view_sorts_all_day_first() {
        let mut c = Calendar::default();
        let day = d(2025, 3, 3);
        let mut a = Event::new(0, "Standup", day);
        a.start = Some(9 * 60);
        let mut b = Event::new(0, "Late", day);
        b.start = Some(17 * 60);
        let all = Event::new(0, "Birthday", day);
        c.add(b);
        c.add(a);
        c.add(all);
        let titles: Vec<&str> = c.on(day).iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["Birthday", "Standup", "Late"]);
        assert!(c.on(d(2025, 3, 4)).is_empty());
    }

    #[test]
    fn ids_and_removal() {
        let mut c = Calendar::default();
        let a = c.add(Event::new(0, "a", d(2025, 1, 1)));
        let b = c.add(Event::new(0, "b", d(2025, 1, 1)));
        assert_eq!((a, b), (1, 2));
        assert!(c.remove(a));
        assert!(!c.remove(a));
        assert_eq!(c.add(Event::new(0, "c", d(2025, 1, 1))), 3); // ids are not reused downwards
        c.get_mut(b).unwrap().title = "B".into();
        assert_eq!(c.on(d(2025, 1, 1))[0].title, "B");
    }

    #[test]
    fn month_counts_and_ranges() {
        let mut c = Calendar::default();
        let mut e = Event::new(0, "gym", d(2025, 2, 3));
        e.repeat = Repeat::Weekly;
        c.add(e);
        c.add(Event::new(0, "once", d(2025, 2, 10)));
        let counts = c.month_counts(2025, 2);
        assert_eq!(counts.len(), 28);
        assert_eq!(counts[2], 1); // 3 Feb
        assert_eq!(counts[9], 2); // 10 Feb: gym + once
        assert_eq!(counts[3], 0);
        let r = c.between(d(2025, 2, 1), d(2025, 2, 14));
        assert_eq!(r.len(), 3); // gym on the 3rd and 10th, "once" on the 10th
        assert_eq!(r[0].0, d(2025, 2, 3));
    }

    #[test]
    fn alerts_fire_once_in_their_window() {
        let mut c = Calendar::default();
        let mut e = Event::new(0, "Meeting", d(2025, 1, 10));
        e.start = Some(9 * 60);
        e.alert = Some(30);
        c.add(e);
        let fire = join_secs(d(2025, 1, 10), 8 * 3600 + 30 * 60);
        assert_eq!(c.alerts_due(fire - 5, fire).len(), 1);
        assert_eq!(c.alerts_due(fire, fire + 5).len(), 0); // window is (last, now]
        assert_eq!(c.alerts_due(fire - 10, fire - 1).len(), 0);
        assert_eq!(c.alerts_due(fire - 100, fire + 100).len(), 1);
    }

    #[test]
    fn alert_across_midnight() {
        let mut c = Calendar::default();
        let mut e = Event::new(0, "Early flight", d(2025, 1, 10));
        e.start = Some(30); // 00:30
        e.alert = Some(60); // fires at 23:30 the day before
        c.add(e);
        let fire = join_secs(d(2025, 1, 9), 23 * 3600 + 30 * 60);
        let hits = c.alerts_due(fire - 1, fire);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, d(2025, 1, 10));
    }

    #[test]
    fn alert_needs_a_start_time() {
        let mut c = Calendar::default();
        let mut e = Event::new(0, "All day", d(2025, 1, 10));
        e.alert = Some(10);
        c.add(e);
        let day = join_secs(d(2025, 1, 10), 0);
        assert!(c.alerts_due(day - 86400, day + 86400).is_empty());
    }
}
