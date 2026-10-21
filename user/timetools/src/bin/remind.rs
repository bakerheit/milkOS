//! remind: Reminders from the shell. alarmd rings them when they fall due.
//!
//!   remind [list [NAME]]             open reminders, by list (all lists, or one)
//!   remind lists                     the lists and how many are open
//!   remind add [--list NAME] [WHEN [HH:MM]] TITLE... [--repeat daily|weekly|monthly|yearly[:N]]
//!             [--until DATE] [--no-alert] [--notes TEXT...]
//!   remind add TITLE with the day, time and repeat at the end, e.g. "Call mum tomorrow at 5pm",
//!             "Water plants every 2 weeks" (the same words as the Reminders window's quick add)
//!   remind add in 5m TITLE...       (due at the first whole minute 5 minutes from now)
//!   remind done ID | undone ID | rm ID
//!
//! WHEN is today, tomorrow, +3d, a weekday name or YYYY-MM-DD; without a time a reminder rings at
//! 09:00. Completing a repeating reminder moves it to its next day. Files: ~/Library/Reminders.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use rt::println;
use timekit::cal::Repeat;
use timekit::clock::parse_duration;
use timekit::date::{minute_after, parse_when, split_secs};
use timekit::reminders::{Reminder, ReminderSet};
use timekit::text::{fmt_hm, parse_hm};

rt::main!(main);

fn usage() -> i32 {
    println!("usage: remind [list [NAME]] | lists");
    println!(
        "       remind add [--list NAME] [WHEN [HH:MM]] TITLE... [--repeat daily|weekly|monthly|yearly[:N]]"
    );
    println!("                  [--until DATE] [--no-alert] [--notes TEXT...]");
    println!("       remind add TITLE [tomorrow] [at 5pm] [every 2 weeks]    (sentence form)");
    println!("       remind add in 5m TITLE...");
    println!("       remind done ID | undone ID | rm ID");
    println!("WHEN: today, tomorrow, +3d, a weekday name or YYYY-MM-DD");
    2
}

fn show(r: &Reminder, today: timekit::Date) -> String {
    let when = match (r.due, r.time) {
        (Some(d), Some(t)) => alloc::format!("{} {}", d.fmt(), fmt_hm(t)),
        (Some(d), None) => d.fmt(),
        _ => String::from("no date"),
    };
    alloc::format!(
        "{:>3}  {}  {:<16} {}",
        r.id,
        if r.overdue(today) { '*' } else { ' ' },
        when,
        r.title
    )
}

fn main(args: &rt::Args) -> i32 {
    let paths = timeio::Paths::from_args(args);
    let (mut set, skipped) = timeio::load_reminders(&paths);
    if skipped > 0 {
        println!(
            "remind: skipped {skipped} unreadable line(s) in {}",
            paths.reminders
        );
    }
    let (today, _) = split_secs(timeio::now_secs());
    let id_arg = || args.get(2).and_then(|s| s.parse::<u32>().ok());
    match args.get(1).unwrap_or("list") {
        "list" | "ls" => {
            let only = args.get(2);
            let mut shown = 0;
            for (li, l) in set.lists.iter().enumerate() {
                if only.is_some_and(|n| n != l.name) {
                    continue;
                }
                let open = set.open_in(li);
                println!("{} ({} open)", l.name, open.len());
                for r in open {
                    println!("{}", show(r, today));
                    shown += 1;
                }
            }
            if shown == 0 {
                println!("nothing to do (* = overdue)");
            }
            return 0;
        }
        "lists" => {
            for (li, l) in set.lists.iter().enumerate() {
                println!(
                    "{:<14} {:<7} {} open",
                    l.name,
                    l.colour,
                    set.open_in(li).len()
                );
            }
            return 0;
        }
        "add" => return add(args, &paths, &mut set, today),
        sub @ ("done" | "undone" | "rm") => {
            let Some(id) = id_arg() else {
                return usage();
            };
            let ok = match sub {
                "rm" => set.remove(id),
                _ => set.get_mut(id).is_some_and(|r| {
                    match sub {
                        "done" => {
                            let stayed = r.complete(today);
                            if stayed {
                                println!("next due {}", r.due.map_or(String::new(), |d| d.fmt()));
                            }
                        }
                        _ => r.reopen(),
                    }
                    true
                }),
            };
            if !ok {
                println!("remind: no reminder {id}");
                return 1;
            }
        }
        _ => return usage(),
    }
    save(&paths, &set)
}

fn add(args: &rt::Args, paths: &timeio::Paths, set: &mut ReminderSet, today: timekit::Date) -> i32 {
    let mut i = 2;
    let mut list = 0;
    if args.get(i) == Some("--list") {
        let Some(name) = args.get(i + 1) else {
            return usage();
        };
        match set.lists.iter().position(|l| l.name == name) {
            Some(li) => list = li,
            None => {
                println!("remind: no list called {name} (remind lists)");
                return 1;
            }
        }
        i += 2;
    }
    let mut r = Reminder::new(0, "");
    if args.get(i) == Some("in") {
        let Some(ms) = args.get(i + 1).and_then(parse_duration) else {
            println!("remind: \"in\" needs a time like 5m, 90s or 1h30m");
            return usage();
        };
        let (d, at) = minute_after(timeio::now_secs(), ms);
        r.due = Some(d);
        r.time = Some(at);
        i += 2;
    } else if let Some(d) = args.get(i).and_then(|w| parse_when(w, today)) {
        r.due = Some(d);
        i += 1;
        if let Some(t) = args.get(i).and_then(parse_hm) {
            r.time = Some(t);
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
    r.title = title.join(" ");
    if r.title.is_empty() {
        println!("remind: the reminder needs a title");
        return usage();
    }
    if r.due.is_none() {
        // The sentence form: the day, time and repeat at the end of the title.
        let q = timekit::quickadd::parse(&r.title, today);
        r.title = q.title;
        r.due = q.due;
        r.time = q.time;
        r.repeat = q.repeat;
        r.every = q.every;
    }
    while let Some(flag) = args.get(i) {
        i += 1;
        match (flag, args.get(i)) {
            ("--no-alert", _) => {
                r.alert = false;
                continue;
            }
            ("--repeat", Some(v)) => {
                let (name, n) = v.split_once(':').unwrap_or((v, "1"));
                r.repeat = match name {
                    "daily" => Repeat::Daily,
                    "weekly" => Repeat::Weekly,
                    "monthly" => Repeat::Monthly,
                    "yearly" => Repeat::Yearly,
                    _ => return usage(),
                };
                r.every = match n.parse::<u16>() {
                    Ok(n) if n >= 1 => n,
                    _ => return usage(),
                };
                if r.due.is_none() {
                    println!("remind: a repeating reminder needs a day");
                    return 1;
                }
            }
            ("--until", Some(v)) => match parse_when(v, today) {
                Some(d) => r.until = Some(d),
                None => return usage(),
            },
            ("--notes", Some(_)) => {
                r.notes = args.argv[i..].join(" ");
                break;
            }
            _ => return usage(),
        }
        i += 1;
    }
    let shown = r.clone();
    let id = set.add(list, r);
    println!("{}", show(&Reminder { id, ..shown }, today));
    save(paths, set)
}

fn save(paths: &timeio::Paths, set: &ReminderSet) -> i32 {
    match timeio::save_reminders(paths, set) {
        Ok(()) => 0,
        Err(e) => {
            println!("remind: cannot save in {}: {e}", paths.reminders);
            1
        }
    }
}
