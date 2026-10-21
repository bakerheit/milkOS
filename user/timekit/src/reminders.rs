//! Reminders: lists of things to do, with an optional due date and time and a repeat rule.
//!
//! One file per list in `~/Library/Reminders/<Name>.rem`, tab-separated like the calendar:
//!
//! ```text
//! list  <name>  <colour>
//! reminder  <id>  <due|->  <HH:MM|->  <repeat[:every]>  <until|->  <done: - or the day>  <alert 0|1>  <title>  <notes>
//! ```
//!
//! Completing a repeating reminder moves its due date to the next one instead of closing it. A
//! reminder with a due day but no time rings at 09:00 that day. Lines that do not parse are
//! skipped and counted, never fatal.

use crate::cal::{COLOURS, Event, Repeat};
use crate::date::{Date, join_secs};
use crate::text::{esc, fmt_hm, parse_hm, unesc};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// When a reminder with a day but no time rings (minutes since midnight).
pub const DEFAULT_RING_AT: u16 = 9 * 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reminder {
    pub id: u32,
    pub title: String,
    pub notes: String,
    pub due: Option<Date>,
    /// Minutes since midnight; only meaningful with a due day.
    pub time: Option<u16>,
    pub repeat: Repeat,
    pub every: u16,
    pub until: Option<Date>,
    /// The day it was completed, if it is done.
    pub done: Option<Date>,
    /// Ring when it falls due (on by default).
    pub alert: bool,
    /// A snooze: ring again at this day and minute (the due day does not change).
    pub snooze: Option<(Date, u16)>,
}

impl Reminder {
    pub fn new(id: u32, title: &str) -> Reminder {
        Reminder {
            id,
            title: title.to_string(),
            notes: String::new(),
            due: None,
            time: None,
            repeat: Repeat::None,
            every: 1,
            until: None,
            done: None,
            alert: true,
            snooze: None,
        }
    }

    pub fn is_done(&self) -> bool {
        self.done.is_some()
    }

    /// The moment it rings, in local seconds, if it has a due day.
    pub fn ring_secs(&self) -> Option<i64> {
        let d = self.due?;
        Some(join_secs(
            d,
            self.time.unwrap_or(DEFAULT_RING_AT) as u32 * 60,
        ))
    }

    /// When the snooze rings, in local seconds.
    pub fn snooze_secs(&self) -> Option<i64> {
        let (d, m) = self.snooze?;
        Some(join_secs(d, m as u32 * 60))
    }

    /// Past its due day and still open.
    pub fn overdue(&self, today: Date) -> bool {
        !self.is_done() && self.due.is_some_and(|d| d < today)
    }

    /// The repeat rule as an event on the due day, so the calendar's date logic can be reused.
    fn as_event(&self, due: Date) -> Event {
        let mut e = Event::new(self.id, "", due);
        e.repeat = self.repeat;
        e.every = self.every;
        e.until = self.until;
        e
    }

    /// Mark done. A repeating reminder with a later occurrence moves to it and stays open;
    /// anything else is closed on `today`. Returns true if it stayed open.
    pub fn complete(&mut self, today: Date) -> bool {
        if self.is_done() {
            return false;
        }
        self.snooze = None;
        if self.repeat != Repeat::None
            && let Some(due) = self.due
            && let Some(next) = self.as_event(due).next_after(due.max(today))
        {
            self.due = Some(next);
            return true;
        }
        self.done = Some(today);
        false
    }

    /// Open it again.
    pub fn reopen(&mut self) {
        self.done = None;
    }

    fn line(&self) -> String {
        let rep = if self.every > 1 {
            format!("{}:{}", repeat_name(self.repeat), self.every)
        } else {
            repeat_name(self.repeat).to_string()
        };
        format!(
            "reminder\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.id,
            self.due.map_or("-".to_string(), |d| d.fmt()),
            self.time.map_or("-".to_string(), fmt_hm),
            rep,
            self.until.map_or("-".to_string(), |d| d.fmt()),
            self.done.map_or("-".to_string(), |d| d.fmt()),
            u8::from(self.alert),
            esc(&self.title),
            esc(&self.notes),
            self.snooze.map_or("-".to_string(), |(d, m)| format!(
                "{}@{}",
                d.fmt(),
                fmt_hm(m)
            )),
        )
    }

    fn from_line(line: &str) -> Option<Reminder> {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 9 || f[0] != "reminder" {
            return None;
        }
        let opt_date = |s: &str| {
            if s == "-" {
                Some(None)
            } else {
                Date::parse(s).map(Some)
            }
        };
        let (rep, every) = match f[4].split_once(':') {
            Some((r, n)) => (r, n.parse::<u16>().ok().filter(|&n| n >= 1)?),
            None => (f[4], 1),
        };
        let due = opt_date(f[2])?;
        let time = if f[3] == "-" {
            None
        } else {
            Some(parse_hm(f[3])?)
        };
        if time.is_some() && due.is_none() {
            return None;
        }
        Some(Reminder {
            id: f[1].parse().ok()?,
            due,
            time,
            repeat: repeat_from(rep)?,
            every,
            until: opt_date(f[5])?,
            done: opt_date(f[6])?,
            alert: match f[7] {
                "0" => false,
                "1" => true,
                _ => return None,
            },
            title: unesc(f[8]),
            notes: f.get(9).map_or(String::new(), |n| unesc(n)),
            snooze: match f.get(10) {
                None | Some(&"-") => None,
                Some(s) => {
                    let (d, t) = s.split_once('@')?;
                    Some((Date::parse(d)?, parse_hm(t)?))
                }
            },
        })
    }
}

fn repeat_name(r: Repeat) -> &'static str {
    match r {
        Repeat::None => "none",
        Repeat::Daily => "daily",
        Repeat::Weekly => "weekly",
        Repeat::Monthly => "monthly",
        Repeat::Yearly => "yearly",
    }
}

fn repeat_from(s: &str) -> Option<Repeat> {
    Some(match s {
        "none" => Repeat::None,
        "daily" => Repeat::Daily,
        "weekly" => Repeat::Weekly,
        "monthly" => Repeat::Monthly,
        "yearly" => Repeat::Yearly,
        _ => return None,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderList {
    pub name: String,
    pub colour: String,
    pub items: Vec<Reminder>,
}

impl ReminderList {
    pub fn new(name: &str, colour: &str) -> ReminderList {
        ReminderList {
            name: name.to_string(),
            colour: colour.to_string(),
            items: Vec::new(),
        }
    }

    pub fn parse(text: &str) -> (ReminderList, usize) {
        let mut l = ReminderList::new("Reminders", "straw");
        let mut skipped = 0;
        for line in text
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        {
            if line.starts_with("list\t") {
                let f: Vec<&str> = line.split('\t').collect();
                if f.len() >= 3 && !f[1].is_empty() && COLOURS.contains(&f[2]) {
                    l.name = unesc(f[1]);
                    l.colour = f[2].to_string();
                } else {
                    skipped += 1;
                }
                continue;
            }
            match Reminder::from_line(line) {
                Some(r) if !l.items.iter().any(|x| x.id == r.id) => l.items.push(r),
                _ => skipped += 1,
            }
        }
        (l, skipped)
    }

    pub fn to_text(&self) -> String {
        let mut o = format!(
            "# milkOS reminders: a header, then one reminder per line, tab-separated (see timekit/src/reminders.rs)\nlist\t{}\t{}\n",
            esc(&self.name),
            self.colour
        );
        for r in &self.items {
            o.push_str(&r.line());
            o.push('\n');
        }
        o
    }
}

fn default_rank(name: &str) -> u8 {
    match name {
        "Personal" => 0,
        "Home" => 1,
        "Work" => 2,
        _ => 3,
    }
}

/// Every list, with reminder ids unique across all of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderSet {
    pub lists: Vec<ReminderList>,
}

impl Default for ReminderSet {
    fn default() -> ReminderSet {
        ReminderSet::defaults()
    }
}

impl ReminderSet {
    /// Personal, Home and Work: what a fresh machine shows.
    pub fn defaults() -> ReminderSet {
        ReminderSet {
            lists: alloc::vec![
                ReminderList::new("Personal", "straw"),
                ReminderList::new("Home", "matcha"),
                ReminderList::new("Work", "blue"),
            ],
        }
    }

    /// From the folder's files, as (file name without `.rem`, text). A file without a header
    /// takes its name from the file; no files gives the default list. Returns the skipped count.
    pub fn from_files(files: &[(String, String)]) -> (ReminderSet, usize) {
        if files.is_empty() {
            return (ReminderSet::defaults(), 0);
        }
        let mut set = ReminderSet { lists: Vec::new() };
        let (mut skipped, mut seen) = (0, Vec::<u32>::new());
        for (stem, text) in files {
            let (mut l, sk) = ReminderList::parse(text);
            skipped += sk;
            if !text.lines().any(|x| x.starts_with("list\t")) {
                l.name = stem.clone();
            }
            let base = l.name.clone();
            let mut n = 2;
            while set.lists.iter().any(|o| o.name == l.name) {
                l.name = format!("{base} {n}");
                n += 1;
            }
            for r in &mut l.items {
                if seen.contains(&r.id) {
                    r.id = seen.iter().max().copied().unwrap_or(0) + 1;
                }
                seen.push(r.id);
            }
            set.lists.push(l);
        }
        // The three default lists first, in their usual order, then the rest by name.
        set.lists
            .sort_by_key(|l| (default_rank(&l.name), l.name.clone()));
        (set, skipped)
    }

    /// The file a list is stored in.
    pub fn file_name(l: &ReminderList) -> String {
        let f: String = l
            .name
            .chars()
            .take(40)
            .map(|c| {
                if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let t = f.trim();
        format!("{}.rem", if t.is_empty() { "reminders" } else { t })
    }

    pub fn next_id(&self) -> u32 {
        self.lists
            .iter()
            .flat_map(|l| l.items.iter().map(|r| r.id))
            .max()
            .map_or(1, |m| m + 1)
    }

    pub fn add(&mut self, list: usize, mut r: Reminder) -> u32 {
        r.id = self.next_id();
        let id = r.id;
        let list = list.min(self.lists.len() - 1);
        self.lists[list].items.push(r);
        id
    }

    pub fn find(&self, id: u32) -> Option<(usize, usize)> {
        self.lists
            .iter()
            .enumerate()
            .find_map(|(li, l)| l.items.iter().position(|r| r.id == id).map(|ri| (li, ri)))
    }

    pub fn get(&self, id: u32) -> Option<(usize, &Reminder)> {
        self.find(id)
            .map(|(li, ri)| (li, &self.lists[li].items[ri]))
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Reminder> {
        let (li, ri) = self.find(id)?;
        Some(&mut self.lists[li].items[ri])
    }

    pub fn remove(&mut self, id: u32) -> bool {
        match self.find(id) {
            Some((li, ri)) => {
                self.lists[li].items.remove(ri);
                true
            }
            None => false,
        }
    }

    pub fn move_to(&mut self, id: u32, to: usize) -> bool {
        let Some((li, ri)) = self.find(id) else {
            return false;
        };
        if to >= self.lists.len() {
            return false;
        }
        let r = self.lists[li].items.remove(ri);
        self.lists[to].items.push(r);
        true
    }

    /// Open reminders of one list, soonest first (undated last).
    pub fn open_in(&self, list: usize) -> Vec<&Reminder> {
        let mut v: Vec<&Reminder> = self.lists[list]
            .items
            .iter()
            .filter(|r| !r.is_done())
            .collect();
        sort_open(&mut v);
        v
    }

    /// Done reminders of one list, newest completion first.
    pub fn done_in(&self, list: usize) -> Vec<&Reminder> {
        let mut v: Vec<&Reminder> = self.lists[list]
            .items
            .iter()
            .filter(|r| r.is_done())
            .collect();
        v.sort_by(|a, b| b.done.cmp(&a.done).then(a.id.cmp(&b.id)));
        v
    }

    /// Open reminders from every list matching `keep`, in due order, with their list index.
    pub fn smart<F: Fn(&Reminder) -> bool>(&self, keep: F) -> Vec<(usize, &Reminder)> {
        let mut v: Vec<(usize, &Reminder)> = self
            .lists
            .iter()
            .enumerate()
            .flat_map(|(li, l)| {
                l.items
                    .iter()
                    .filter(|r| !r.is_done() && keep(r))
                    .map(move |r| (li, r))
            })
            .collect();
        v.sort_by(|a, b| due_key(a.1).cmp(&due_key(b.1)).then(a.1.id.cmp(&b.1.id)));
        v
    }

    /// How many open reminders are due today or overdue: the badge number.
    pub fn due_count(&self, today: Date) -> usize {
        self.smart(|r| r.due.is_some_and(|d| d <= today)).len()
    }

    /// Reminders that ring in (`last`, `now`] (local seconds): (id, title, list name, the time).
    pub fn rings_due(&self, last: i64, now: i64) -> Vec<(u32, String, String, u16)> {
        let mut out = Vec::new();
        if now <= last {
            return out;
        }
        for l in &self.lists {
            for r in l.items.iter().filter(|r| !r.is_done() && r.alert) {
                // The due time, and the snooze time if there is one.
                for (t, at) in [
                    (r.ring_secs(), r.time.unwrap_or(DEFAULT_RING_AT)),
                    (r.snooze_secs(), r.snooze.map_or(0, |(_, m)| m)),
                ] {
                    if let Some(t) = t
                        && t > last
                        && t <= now
                    {
                        out.push((r.id, r.title.clone(), l.name.clone(), at));
                    }
                }
            }
        }
        out
    }
}

fn due_key(r: &Reminder) -> (u8, Option<(Date, u16)>) {
    match r.due {
        Some(d) => (0, Some((d, r.time.unwrap_or(0)))),
        None => (1, None),
    }
}

fn sort_open(v: &mut [&Reminder]) {
    v.sort_by(|a, b| due_key(a).cmp(&due_key(b)).then(a.id.cmp(&b.id)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u8, dd: u8) -> Date {
        Date::new(y, m, dd).unwrap()
    }

    #[test]
    fn complete_a_one_off() {
        let mut r = Reminder::new(1, "Pay rent");
        r.due = Some(d(2025, 3, 1));
        assert!(!r.complete(d(2025, 3, 2)));
        assert_eq!(r.done, Some(d(2025, 3, 2)));
        assert!(!r.complete(d(2025, 3, 3))); // already done: nothing changes
        assert_eq!(r.done, Some(d(2025, 3, 2)));
        r.reopen();
        assert!(!r.is_done());
    }

    #[test]
    fn completing_a_repeating_one_moves_it_on() {
        let mut r = Reminder::new(1, "Water plants");
        r.due = Some(d(2025, 3, 3));
        r.repeat = Repeat::Weekly;
        assert!(r.complete(d(2025, 3, 3)));
        assert_eq!(r.due, Some(d(2025, 3, 10)));
        assert!(!r.is_done());
        // Completing late skips what was missed: the next one after today.
        assert!(r.complete(d(2025, 3, 21)));
        assert_eq!(r.due, Some(d(2025, 3, 24)));
        // The last occurrence closes it.
        let mut last = Reminder::new(2, "Course");
        last.due = Some(d(2025, 3, 3));
        last.repeat = Repeat::Weekly;
        last.until = Some(d(2025, 3, 10));
        assert!(last.complete(d(2025, 3, 3)));
        assert_eq!(last.due, Some(d(2025, 3, 10)));
        assert!(!last.complete(d(2025, 3, 10)));
        assert_eq!(last.done, Some(d(2025, 3, 10)));
    }

    #[test]
    fn undated_repeats_just_close() {
        let mut r = Reminder::new(1, "x");
        r.repeat = Repeat::Daily; // no due day: nothing to advance
        assert!(!r.complete(d(2025, 1, 1)));
        assert!(r.is_done());
    }

    #[test]
    fn ring_time_defaults_to_nine() {
        let mut r = Reminder::new(1, "x");
        assert_eq!(r.ring_secs(), None);
        r.due = Some(d(2025, 3, 3));
        assert_eq!(r.ring_secs(), Some(join_secs(d(2025, 3, 3), 9 * 3600)));
        r.time = Some(17 * 60 + 30);
        assert_eq!(
            r.ring_secs(),
            Some(join_secs(d(2025, 3, 3), 17 * 3600 + 1800))
        );
        assert!(r.overdue(d(2025, 3, 4)) && !r.overdue(d(2025, 3, 3)));
    }

    #[test]
    fn file_roundtrip_and_junk() {
        let mut l = ReminderList::new("Home\tstuff", "matcha");
        let mut r = Reminder::new(3, "Buy milk\nand oats");
        r.notes = "Semi-skimmed \\ one".into();
        r.due = Some(d(2025, 6, 1));
        r.time = Some(8 * 60);
        r.repeat = Repeat::Weekly;
        r.every = 2;
        r.until = Some(d(2025, 12, 1));
        r.alert = false;
        l.items.push(r);
        let mut done = Reminder::new(4, "Done thing");
        done.done = Some(d(2025, 5, 30));
        l.items.push(done);
        let (back, skipped) = ReminderList::parse(&l.to_text());
        assert_eq!((back, skipped), (l, 0));
        let junk = "# c\nlist\tX\tpurple\nreminder\t1\t-\t09:00\tnone\t-\t-\t0\ttime without day\t\nreminder\t2\t2025-02-30\t-\tnone\t-\t-\t0\tbad date\t\nreminder\t3\t-\t-\tsoon\t-\t-\t0\tbad repeat\t\nreminder\t4\t-\t-\tnone\t-\t-\t2\tbad alert\t\nreminder\t5\t-\t-\tnone\t-\t-\t0\tfine\nnope\n";
        let (j, skipped) = ReminderList::parse(junk);
        assert_eq!(j.items.len(), 1);
        assert_eq!(skipped, 6);
        assert_eq!(j.name, "Reminders");
    }

    fn set() -> ReminderSet {
        let mut s = ReminderSet {
            lists: alloc::vec![
                ReminderList::new("Reminders", "straw"),
                ReminderList::new("Work", "blue")
            ],
        };
        let mut a = Reminder::new(0, "later");
        a.due = Some(d(2025, 3, 9));
        let mut b = Reminder::new(0, "sooner");
        b.due = Some(d(2025, 3, 3));
        b.time = Some(10 * 60);
        let c = Reminder::new(0, "someday");
        let e = Reminder::new(0, "also someday");
        s.add(0, a);
        s.add(0, b);
        s.add(0, c);
        s.add(0, e);
        let mut w = Reminder::new(0, "work due today");
        w.due = Some(d(2025, 3, 5));
        s.add(1, w);
        s
    }

    #[test]
    fn ordering_and_smart_lists() {
        let s = set();
        let titles: Vec<&str> = s.open_in(0).iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["sooner", "later", "someday", "also someday"]);
        let today = d(2025, 3, 5);
        let due: Vec<&str> = s
            .smart(|r| r.due.is_some_and(|x| x <= today))
            .iter()
            .map(|(_, r)| r.title.as_str())
            .collect();
        assert_eq!(due, ["sooner", "work due today"]);
        assert_eq!(s.due_count(today), 2);
        assert_eq!(s.due_count(d(2025, 3, 2)), 0);
    }

    #[test]
    fn ids_lookups_moves() {
        let mut s = set();
        let id = s.next_id() - 1;
        assert_eq!(s.get(id).unwrap().0, 1);
        assert!(s.move_to(id, 0));
        assert_eq!(s.get(id).unwrap().0, 0);
        assert!(!s.move_to(id, 7));
        s.get_mut(id).unwrap().title = "renamed".into();
        assert_eq!(s.get(id).unwrap().1.title, "renamed");
        assert!(s.remove(id) && !s.remove(id));
        // Done ones sort newest first and stay out of the open view.
        s.get_mut(1).unwrap().complete(d(2025, 3, 6));
        s.get_mut(2).unwrap().complete(d(2025, 3, 7));
        let done: Vec<u32> = s.done_in(0).iter().map(|r| r.id).collect();
        assert_eq!(done, [2, 1]);
        assert!(s.open_in(0).iter().all(|r| !r.is_done()));
    }

    #[test]
    fn rings_once_in_their_window() {
        let s = set();
        let sooner = join_secs(d(2025, 3, 3), 10 * 3600);
        assert_eq!(s.rings_due(sooner - 1, sooner).len(), 1);
        assert!(s.rings_due(sooner, sooner + 60).is_empty());
        let nine = join_secs(d(2025, 3, 9), 9 * 3600); // "later" has no time
        let hits = s.rings_due(nine - 1, nine);
        assert_eq!(
            (
                hits.len(),
                hits[0].1.as_str(),
                hits[0].2.as_str(),
                hits[0].3
            ),
            (1, "later", "Reminders", 540)
        );
        assert!(s.rings_due(nine, nine - 5).is_empty());
    }

    #[test]
    fn alerts_can_be_off() {
        let mut s = set();
        let id = s.lists[0]
            .items
            .iter()
            .find(|r| r.title == "sooner")
            .unwrap()
            .id;
        s.get_mut(id).unwrap().alert = false;
        let sooner = join_secs(d(2025, 3, 3), 10 * 3600);
        assert!(s.rings_due(sooner - 1, sooner).is_empty());
    }

    #[test]
    fn a_snooze_rings_again_and_survives_the_file() {
        let mut s = set();
        let id = s.lists[0]
            .items
            .iter()
            .find(|r| r.title == "sooner")
            .unwrap()
            .id;
        s.get_mut(id).unwrap().snooze = Some((d(2025, 3, 3), 10 * 60 + 10));
        let (back, _) = ReminderList::parse(&s.lists[0].to_text());
        assert_eq!(
            back.items.iter().find(|r| r.id == id).unwrap().snooze,
            Some((d(2025, 3, 3), 610))
        );
        let at = join_secs(d(2025, 3, 3), 10 * 3600 + 600);
        let hits = s.rings_due(at - 1, at);
        assert_eq!((hits.len(), hits[0].3), (1, 610));
        // Completing it clears the snooze; old files without the column read as no snooze.
        s.get_mut(id).unwrap().complete(d(2025, 3, 3));
        assert!(s.get(id).unwrap().1.snooze.is_none());
        let (old, skipped) = ReminderList::parse("reminder\t1\t-\t-\tnone\t-\t-\t1\told\t\n");
        assert_eq!(
            (old.items.len(), skipped, old.items[0].snooze),
            (1, 0, None)
        );
        let (bad, skipped) = ReminderList::parse("reminder\t1\t-\t-\tnone\t-\t-\t1\tx\t\tsoon\n");
        assert_eq!((bad.items.len(), skipped), (0, 1));
    }

    #[test]
    fn done_ones_never_ring() {
        let mut s = set();
        let id = s.lists[0]
            .items
            .iter()
            .find(|r| r.title == "sooner")
            .unwrap()
            .id;
        s.get_mut(id).unwrap().complete(d(2025, 3, 3));
        let sooner = join_secs(d(2025, 3, 3), 10 * 3600);
        assert!(s.rings_due(sooner - 1, sooner).is_empty());
    }

    #[test]
    fn from_files_and_names() {
        let (s, sk) = ReminderSet::from_files(&[]);
        assert_eq!((s.lists.len(), sk), (3, 0));
        let a = "list\tShopping\tmatcha\nreminder\t1\t-\t-\tnone\t-\t-\t0\tmilk\t\n".to_string();
        let b = "reminder\t1\t-\t-\tnone\t-\t-\t0\tclash\t\n".to_string();
        let (s, _) = ReminderSet::from_files(&[("Shopping".into(), a), ("Garden".into(), b)]);
        assert_eq!(s.lists[0].name, "Garden"); // sorted by name after the defaults
        assert_eq!(s.lists[0].items[0].id, 2);
        let home = "list\tHome\tmatcha\n".to_string();
        let personal = "list\tPersonal\tstraw\n".to_string();
        let (s, _) = ReminderSet::from_files(&[
            ("Home".into(), home),
            ("Personal".into(), personal),
            ("Zed".into(), "list\tZed\tblue\n".to_string()),
        ]);
        let names: Vec<&str> = s.lists.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Personal", "Home", "Zed"]);
        let n = |x: &str| ReminderSet::file_name(&ReminderList::new(x, "straw"));
        assert_eq!(
            (n("Work").as_str(), n("a/b").as_str(), n("").as_str()),
            ("Work.rem", "a_b.rem", "reminders.rem")
        );
    }
}
