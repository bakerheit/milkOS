//! alarm: the Clock's alarms from the shell. alarmd rings them.
//!
//!   alarm [list]
//!   alarm add HH:MM [once|daily|weekdays|weekends|mon,wed,fri] [label...]
//!   alarm add in 5m [label...]       (the first whole minute 5 minutes from now, once)
//!   alarm rm ID | on ID | off ID
//!   alarm snooze ID [MINUTES]       (a one-off alarm with the same label, default 9 minutes: what the toast button runs)

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use rt::println;
use timekit::clock::{Alarm, fmt_days, fmt_in, parse_days, parse_duration};
use timekit::date::minute_after;
use timekit::text::{fmt_hm, parse_hm};

rt::main!(main);

fn usage() -> i32 {
    println!("usage: alarm [list]");
    println!("       alarm add HH:MM [once|daily|weekdays|weekends|mon,wed,fri] [label...]");
    println!("       alarm add in 5m [label...]");
    println!("       alarm rm ID | on ID | off ID | snooze ID [MINUTES]");
    2
}

fn main(args: &rt::Args) -> i32 {
    let paths = timeio::Paths::from_args(args);
    let (mut list, skipped) = timeio::load_alarms(&paths);
    if skipped > 0 {
        println!(
            "alarm: skipped {skipped} unreadable line(s) in {}",
            paths.alarms
        );
    }
    let sub = args.get(1).unwrap_or("list");
    let id_arg = || args.get(2).and_then(|s| s.parse::<u32>().ok());
    match sub {
        "list" | "ls" => {
            let now = timeio::now_secs();
            if list.list.is_empty() {
                println!("no alarms (alarm add HH:MM ...)");
            }
            for a in &list.list {
                let next = match a.next_fire(now) {
                    Some(t) => fmt_in(t - now),
                    None => String::from("off"),
                };
                println!(
                    "{:>3}  {}  {:<9}  {:<17}  {}",
                    a.id,
                    fmt_hm(a.at),
                    fmt_days(a.days),
                    next,
                    a.label
                );
            }
            return 0;
        }
        "add" => {
            let (at, days, label_from) = if args.get(2) == Some("in") {
                let Some(ms) = args.get(3).and_then(parse_duration) else {
                    println!("alarm: \"in\" needs a time like 5m, 90s or 1h30m");
                    return usage();
                };
                let (_, at) = minute_after(timeio::now_secs(), ms);
                (at, 0, 4)
            } else {
                let Some(at) = args.get(2).and_then(parse_hm) else {
                    println!("alarm: add needs a time like 07:30, or \"in 5m\"");
                    return usage();
                };
                match args.get(3).and_then(parse_days) {
                    Some(d) => (at, d, 4),
                    None => (at, 0, 3),
                }
            };
            let label = args.argv[label_from.min(args.argv.len())..].join(" ");
            let id = list.add(Alarm::new(0, &label, at, days));
            println!("alarm {id}: {} {}", fmt_hm(at), fmt_days(days));
        }
        "snooze" => {
            let Some(id) = id_arg() else {
                return usage();
            };
            let mins = args
                .get(3)
                .and_then(|m| m.parse::<u32>().ok())
                .unwrap_or(9)
                .clamp(1, 1440);
            let Some(label) = list
                .list
                .iter()
                .find(|a| a.id == id)
                .map(|a| a.label.clone())
            else {
                println!("alarm: no alarm {id}");
                return 1;
            };
            let new = list.snooze(&label, timeio::now_secs(), mins);
            println!("alarm {new}: snoozed {mins} min");
        }
        "rm" | "on" | "off" => {
            let Some(id) = id_arg() else {
                return usage();
            };
            let found = match sub {
                "rm" => list.remove(id),
                s => list.list.iter_mut().find(|a| a.id == id).is_some_and(|a| {
                    a.enabled = s == "on";
                    true
                }),
            };
            if !found {
                println!("alarm: no alarm {id}");
                return 1;
            }
        }
        _ => return usage(),
    }
    match timeio::save_alarms(&paths, &list) {
        Ok(()) => 0,
        Err(e) => {
            println!("alarm: cannot save {}: {e}", paths.alarms);
            1
        }
    }
}
