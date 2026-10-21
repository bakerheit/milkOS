//! Quick add: "Call mum tomorrow at 5pm", "Water plants every 2 weeks", "Rent +3d 09:00".
//!
//! The title comes first; whatever follows it must be made only of words this module knows (a day,
//! a time, a repeat), otherwise it all stays in the title: nothing is guessed. The caller shows
//! [`Quick::preview`] so the user sees how the sentence was understood before it is added.

use crate::cal::Repeat;
use crate::date::{Date, MONTHS, WEEKDAYS, parse_when};
use crate::text::{fmt_hm, parse_hm};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quick {
    pub title: String,
    pub due: Option<Date>,
    pub time: Option<u16>,
    pub repeat: Repeat,
    pub every: u16,
}

impl Quick {
    /// "Call mum" ... "due Thu 2026-10-23 17:00, repeats weekly".
    pub fn preview(&self, today: Date, list: &str) -> String {
        let mut s = format!("\"{}\"", self.title);
        match self.due {
            Some(d) => {
                let day = if d == today {
                    String::from("today")
                } else if d == today.add_days(1) {
                    String::from("tomorrow")
                } else {
                    format!("{} {}", &WEEKDAYS[d.weekday() as usize][..3], d.fmt())
                };
                s.push_str(&format!(", due {day}"));
                if let Some(t) = self.time {
                    s.push_str(&format!(" {}", fmt_hm(t)));
                }
            }
            None => s.push_str(", no date"),
        }
        if self.repeat != Repeat::None {
            let (one, many) = match self.repeat {
                Repeat::Daily => ("daily", "days"),
                Repeat::Weekly => ("weekly", "weeks"),
                Repeat::Monthly => ("monthly", "months"),
                _ => ("yearly", "years"),
            };
            if self.every > 1 {
                s.push_str(&format!(", every {} {many}", self.every));
            } else {
                s.push_str(&format!(", {one}"));
            }
        }
        s.push_str(&format!(", in {list}"));
        s
    }
}

/// "5pm", "5:30pm", "12am", "17:00", "noon", "midnight".
pub fn parse_time(tok: &str) -> Option<u16> {
    let t = tok.to_ascii_lowercase();
    match t.as_str() {
        "noon" => return Some(12 * 60),
        "midnight" => return Some(0),
        _ => {}
    }
    let (body, pm) = if let Some(b) = t.strip_suffix("pm") {
        (b, Some(true))
    } else if let Some(b) = t.strip_suffix("am") {
        (b, Some(false))
    } else {
        (t.as_str(), None)
    };
    match pm {
        None => body.contains(':').then(|| parse_hm(body)).flatten(),
        Some(pm) => {
            let (h, m) = match body.split_once(':') {
                Some((h, m)) => (h.parse::<u16>().ok()?, m.parse::<u16>().ok()?),
                None => (body.parse::<u16>().ok()?, 0),
            };
            if !(1..=12).contains(&h) || m > 59 {
                return None;
            }
            Some((h % 12 + if pm { 12 } else { 0 }) * 60 + m)
        }
    }
}

fn unit(w: &str) -> Option<Repeat> {
    Some(match w.to_ascii_lowercase().trim_end_matches('s') {
        "day" => Repeat::Daily,
        "week" => Repeat::Weekly,
        "month" => Repeat::Monthly,
        "year" => Repeat::Yearly,
        _ => return None,
    })
}

fn weekday_of(w: &str) -> Option<u8> {
    let l = w.to_ascii_lowercase();
    if l.len() < 3 {
        return None;
    }
    WEEKDAYS
        .iter()
        .position(|d| d.to_ascii_lowercase().starts_with(&l))
        .map(|i| i as u8)
}

#[derive(Default)]
struct Tail {
    due: Option<Date>,
    time: Option<u16>,
    repeat: Option<(Repeat, u16)>,
    phrases: u32,
}

/// Can the words from here to the end be read entirely as a day, a time and a repeat?
fn read_tail(words: &[&str], today: Date) -> Option<Tail> {
    let mut t = Tail::default();
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let lw = w.to_ascii_lowercase();
        match lw.as_str() {
            "at" => {
                t.time = Some(parse_time(words.get(i + 1)?)?);
                i += 2;
            }
            "on" => {
                // "on" only introduces a day.
                let d = parse_when(words.get(i + 1)?, today)?;
                if t.due.replace(d).is_some() {
                    return None;
                }
                i += 2;
            }
            "every" => {
                let next = *words.get(i + 1)?;
                if let Ok(n) = next.parse::<u16>() {
                    // every 2 weeks
                    let r = unit(words.get(i + 2)?)?;
                    if n == 0 || t.repeat.replace((r, n)).is_some() {
                        return None;
                    }
                    i += 3;
                } else if let Some(r) = unit(next) {
                    if t.repeat.replace((r, 1)).is_some() {
                        return None;
                    }
                    i += 2;
                } else {
                    // every monday: weekly, starting on the next one
                    let wd = weekday_of(next)?;
                    let d = today
                        .add_days(((wd as i64 - today.weekday() as i64 - 1).rem_euclid(7)) + 1);
                    if t.repeat.replace((Repeat::Weekly, 1)).is_some() || t.due.replace(d).is_some()
                    {
                        return None;
                    }
                    i += 2;
                }
            }
            "daily" | "weekly" | "monthly" | "yearly" => {
                let r = match lw.as_str() {
                    "daily" => Repeat::Daily,
                    "weekly" => Repeat::Weekly,
                    "monthly" => Repeat::Monthly,
                    _ => Repeat::Yearly,
                };
                if t.repeat.replace((r, 1)).is_some() {
                    return None;
                }
                i += 1;
            }
            _ => {
                if let Some(tm) = parse_time(w) {
                    if t.time.replace(tm).is_some() {
                        return None;
                    }
                } else {
                    let d = parse_when(w, today)?;
                    if t.due.replace(d).is_some() {
                        return None;
                    }
                }
                i += 1;
            }
        }
        t.phrases += 1;
    }
    (t.phrases > 0).then_some(t)
}

/// Understand a sentence. The title is everything before the longest tail of known words.
pub fn parse(text: &str, today: Date) -> Quick {
    let words: Vec<&str> = text.split_whitespace().collect();
    // The earliest word from which everything after is understood (leaving a title of at least one word).
    for i in 1..words.len() {
        if let Some(t) = read_tail(&words[i..], today) {
            let (repeat, every) = t.repeat.unwrap_or((Repeat::None, 1));
            // A time or a repeat with no day means today (a repeat needs a day to count from).
            let due = t
                .due
                .or_else(|| (t.time.is_some() || repeat != Repeat::None).then_some(today));
            return Quick {
                title: words[..i].join(" "),
                due,
                time: t.time.filter(|_| due.is_some()),
                repeat,
                every,
            };
        }
    }
    Quick {
        title: words.join(" "),
        due: None,
        time: None,
        repeat: Repeat::None,
        every: 1,
    }
}

/// Month name for a preview that wants one: "Oct".
pub fn month_short(d: Date) -> &'static str {
    &MONTHS[d.m as usize - 1][..3]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> Date {
        Date::new(2025, 3, 6).unwrap() // a Thursday
    }

    fn q(s: &str) -> Quick {
        parse(s, today())
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("5pm"), Some(17 * 60));
        assert_eq!(parse_time("5:30pm"), Some(17 * 60 + 30));
        assert_eq!(parse_time("12am"), Some(0));
        assert_eq!(parse_time("12pm"), Some(12 * 60));
        assert_eq!(parse_time("17:00"), Some(17 * 60));
        assert_eq!(parse_time("noon"), Some(720));
        assert_eq!(parse_time("13pm"), None);
        assert_eq!(parse_time("17"), None);
        assert_eq!(parse_time("5:61pm"), None);
        assert_eq!(parse_time("soon"), None);
    }

    #[test]
    fn a_plain_title_stays_whole() {
        let r = q("Buy oat milk");
        assert_eq!(r.title, "Buy oat milk");
        assert_eq!((r.due, r.time, r.repeat), (None, None, Repeat::None));
        assert_eq!(q("").title, "");
        assert_eq!(q("tomorrow").title, "tomorrow"); // a title is always left
    }

    #[test]
    fn days_and_times() {
        let r = q("Call mum tomorrow at 5pm");
        assert_eq!(
            (r.title.as_str(), r.due, r.time),
            ("Call mum", Some(today().add_days(1)), Some(17 * 60))
        );
        let r = q("Rent +3d 09:00");
        assert_eq!(
            (r.title.as_str(), r.due, r.time),
            ("Rent", Some(today().add_days(3)), Some(540))
        );
        let r = q("Dentist friday");
        assert_eq!((r.due, r.time), (Some(today().add_days(1)), None));
        let r = q("Pay bill on 2025-04-01 at noon");
        assert_eq!(
            (r.title.as_str(), r.due, r.time),
            ("Pay bill", Date::new(2025, 4, 1), Some(720))
        );
    }

    #[test]
    fn repeats() {
        let r = q("Water plants every 2 weeks");
        assert_eq!(
            (r.repeat, r.every, r.due),
            (Repeat::Weekly, 2, Some(today()))
        );
        let r = q("Stretch every day 8am");
        assert_eq!((r.repeat, r.every, r.time), (Repeat::Daily, 1, Some(480)));
        let r = q("Bins every monday");
        assert_eq!(
            (r.repeat, r.due),
            (Repeat::Weekly, Some(today().add_days(4)))
        );
        let r = q("Review monthly tomorrow");
        assert_eq!(
            (r.repeat, r.due),
            (Repeat::Monthly, Some(today().add_days(1)))
        );
        let r = q("Taxes every year");
        assert_eq!((r.repeat, r.every), (Repeat::Yearly, 1));
    }

    #[test]
    fn nothing_is_guessed() {
        // A word the grammar does not know keeps the whole sentence as the title.
        assert_eq!(
            q("Call mum tomorrow evening").title,
            "Call mum tomorrow evening"
        );
        assert_eq!(q("Call mum tomorrow evening").due, None);
        // Two days in one sentence: only the last is read as a day; the first stays in the title.
        let r = q("Trip friday saturday");
        assert_eq!(
            (r.title.as_str(), r.due),
            ("Trip friday", Some(today().add_days(2)))
        );
        assert_eq!(q("x every 0 weeks").due, None);
        // A time alone means today.
        let r = q("Take pills 8pm");
        assert_eq!((r.due, r.time), (Some(today()), Some(20 * 60)));
        // The words belong to the title when something known comes before more title.
        let r = q("Read sat in the park");
        assert_eq!(r.title, "Read sat in the park");
    }

    #[test]
    fn previews() {
        let r = q("Call mum tomorrow at 5pm");
        assert_eq!(
            r.preview(today(), "Personal"),
            "\"Call mum\", due tomorrow 17:00, in Personal"
        );
        let r = q("Water plants every 2 weeks");
        assert_eq!(
            r.preview(today(), "Home"),
            "\"Water plants\", due today, every 2 weeks, in Home"
        );
        assert_eq!(
            q("Milk").preview(today(), "Home"),
            "\"Milk\", no date, in Home"
        );
        assert_eq!(month_short(today()), "Mar");
    }
}
