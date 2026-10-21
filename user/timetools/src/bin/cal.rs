//! cal: the Calendar from the shell.
//!
//!   cal                          this month; days with events are marked *
//!   cal MONTH YEAR               e.g. cal 3 2026
//!   cal list [DAYS]              what is coming up (default 14 days)
//!   cal calendars                the calendars and how many events each has
//!   cal add WHEN [HH:MM[-HH:MM]] TITLE... [--calendar NAME] [--repeat daily|weekly|monthly|yearly[:N]]
//!           [--until DATE] [--alert MINUTES] [--notes TEXT...]
//!   cal add in 5m TITLE...       (starts at the first whole minute 5 minutes from now)
//!   cal rm ID
//!
//! WHEN is today, tomorrow, +3d, a weekday name or YYYY-MM-DD. The files are
//! ~/Library/Calendar/<Name>.cal, shared with the Calendar app and alarmd.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use rt::{print, println};
use timekit::cal::{CalendarSet, Event, Repeat};
use timekit::clock::parse_duration;
use timekit::date::{Date, MONTHS, WEEKDAYS, minute_after, parse_when, split_secs};
use timekit::text::{fmt_hm, parse_hm};

rt::main!(main);

fn usage() -> i32 {
    println!("usage: cal [MONTH YEAR]");
    println!("       cal list [DAYS] | calendars | rm ID");
    println!(
        "       cal add WHEN [HH:MM[-HH:MM]] TITLE... [--calendar NAME] [--repeat daily|weekly|monthly|yearly[:N]]"
    );
    println!("               [--until DATE] [--alert MINUTES] [--notes TEXT...]");
    println!("       cal add in 5m TITLE...");
    println!("WHEN: today, tomorrow, +3d, a weekday name or YYYY-MM-DD");
    2
}

fn when(e: &Event) -> String {
    match (e.start, e.end) {
        (Some(s), Some(en)) => alloc::format!("{}-{}", fmt_hm(s), fmt_hm(en)),
        (Some(s), None) => fmt_hm(s),
        _ => String::from("all day"),
    }
}

fn month_view(set: &CalendarSet, y: i32, m: u8, today: Date) {
    let title = alloc::format!("{} {y}", MONTHS[m as usize - 1]);
    println!("{:^28}", title);
    println!(" Mo  Tu  We  Th  Fr  Sa  Su");
    let counts = set.month_counts(y, m);
    for row in timekit::date::month_grid(y, m, true) {
        for d in row {
            if d == 0 {
                print!("    ");
                continue;
            }
            // The number, then * if the day has events; > marks today.
            let mark = if counts[d as usize - 1] > 0 { '*' } else { ' ' };
            let me = if (Date { y, m, d }) == today {
                '>'
            } else {
                ' '
            };
            print!("{me}{d:>2}{mark}");
        }
        println!();
    }
}

fn main(args: &rt::Args) -> i32 {
    let paths = timeio::Paths::from_args(args);
    let (mut set, skipped) = timeio::load_calendars(&paths);
    if skipped > 0 {
        println!(
            "cal: skipped {skipped} unreadable line(s) in {}",
            paths.calendars
        );
    }
    let (today, _) = split_secs(timeio::now_secs());
    match args.get(1) {
        None => month_view(&set, today.y, today.m, today),
        Some(m) if m.parse::<u8>().is_ok() => {
            let (Ok(m), Some(y)) = (
                m.parse::<u8>(),
                args.get(2).and_then(|y| y.parse::<i32>().ok()),
            ) else {
                return usage();
            };
            if !(1..=12).contains(&m) {
                return usage();
            }
            month_view(&set, y, m, today);
        }
        Some("list") | Some("ls") => {
            let n = args
                .get(2)
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(14)
                .clamp(1, 366);
            let items = set.between(today, today.add_days(n - 1));
            if items.is_empty() {
                println!("nothing in the next {n} days");
            }
            let mut last = None;
            for (d, ci, e) in items {
                if last != Some(d) {
                    println!("{} {}", &WEEKDAYS[d.weekday() as usize][..3], d.fmt());
                    last = Some(d);
                }
                println!(
                    "  {:>3}  {:<11}  {}  [{}]",
                    e.id,
                    when(e),
                    e.title,
                    set.cals[ci].name
                );
            }
            return 0;
        }
        Some("calendars") => {
            for c in &set.cals {
                println!(
                    "{:<12} {:<7} {} event(s){}",
                    c.name,
                    c.colour,
                    c.events.len(),
                    if c.hidden { "  (hidden)" } else { "" }
                );
            }
            return 0;
        }
        Some("rm") => {
            let Some(id) = args.get(2).and_then(|s| s.parse::<u32>().ok()) else {
                return usage();
            };
            if !set.remove(id) {
                println!("cal: no event {id}");
                return 1;
            }
            return save(&paths, &set);
        }
        Some("add") => return add(args, &paths, &mut set, today),
        Some(_) => return usage(),
    }
    0
}

fn add(args: &rt::Args, paths: &timeio::Paths, set: &mut CalendarSet, today: Date) -> i32 {
    let mut e;
    let mut i;
    if args.get(2) == Some("in") {
        let Some(ms) = args.get(3).and_then(parse_duration) else {
            println!("cal: \"in\" needs a time like 5m, 90s or 1h30m");
            return usage();
        };
        let (d, at) = minute_after(timeio::now_secs(), ms);
        e = Event::new(0, "", d);
        e.start = Some(at);
        i = 4;
    } else {
        let Some(d) = args.get(2).and_then(|w| parse_when(w, today)) else {
            println!("cal: add needs a day (today, tomorrow, +3d, mon, 2026-03-09) or \"in 5m\"");
            return usage();
        };
        e = Event::new(0, "", d);
        i = 3;
        if let Some((s, en)) = args.get(i).and_then(|t| {
            let (a, b) = t.split_once('-').unwrap_or((t, ""));
            Some((
                parse_hm(a)?,
                if b.is_empty() {
                    None
                } else {
                    Some(parse_hm(b)?)
                },
            ))
        }) {
            e.start = Some(s);
            e.end = en;
            i += 1;
        }
    }
    let mut title: Vec<&str> = Vec::new();
    while let Some(w) = args.get(i) {
        if w.starts_with("--") {
            break;
        }
        title.push(w);
        i += 1;
    }
    e.title = title.join(" ");
    if e.title.is_empty() {
        println!("cal: the event needs a title");
        return usage();
    }
    let mut cal = 0;
    while let Some(flag) = args.get(i) {
        i += 1;
        match (flag, args.get(i)) {
            ("--calendar", Some(name)) => match set.cals.iter().position(|c| c.name == name) {
                Some(ci) => cal = ci,
                None => {
                    println!("cal: no calendar called {name} (cal calendars)");
                    return 1;
                }
            },
            ("--repeat", Some(r)) => {
                let (name, n) = r.split_once(':').unwrap_or((r, "1"));
                e.repeat = match name {
                    "daily" => Repeat::Daily,
                    "weekly" => Repeat::Weekly,
                    "monthly" => Repeat::Monthly,
                    "yearly" => Repeat::Yearly,
                    _ => return usage(),
                };
                e.every = match n.parse::<u16>() {
                    Ok(n) if n >= 1 => n,
                    _ => return usage(),
                };
            }
            ("--until", Some(d)) => match parse_when(d, today) {
                Some(d) => e.until = Some(d),
                None => return usage(),
            },
            ("--alert", Some(m)) => match m.parse::<u16>() {
                Ok(m) => e.alert = Some(m),
                _ => return usage(),
            },
            ("--notes", Some(_)) => {
                e.notes = args.argv[i..].join(" ");
                break;
            }
            _ => return usage(),
        }
        i += 1;
    }
    let date = e.date;
    let shown = e.clone();
    let id = set.add(cal, e);
    println!(
        "event {id}: {} {} {} [{}]",
        date.fmt(),
        when(&shown),
        shown.title,
        set.cals[cal].name
    );
    save(paths, set)
}

fn save(paths: &timeio::Paths, set: &CalendarSet) -> i32 {
    match timeio::save_calendars(paths, set) {
        Ok(()) => 0,
        Err(e) => {
            println!("cal: cannot save in {}: {e}", paths.calendars);
            1
        }
    }
}
