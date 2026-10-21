//! timer: countdown timers from the shell. alarmd rings them, even with Clock closed.
//!
//!   timer [list]
//!   timer DURATION [label...]        e.g. timer 5m tea, timer 1h30m, timer 90
//!   timer pause ID | resume ID | reset ID | rm ID

#![no_std]
#![no_main]

extern crate alloc;

use rt::println;
use timekit::clock::{Timer, fmt_countdown, parse_duration};

rt::main!(main);

fn usage() -> i32 {
    println!("usage: timer [list]");
    println!("       timer DURATION [label...]    (5m, 1h30m, 90s; a bare number is seconds)");
    println!("       timer pause ID | resume ID | reset ID | rm ID");
    2
}

fn main(args: &rt::Args) -> i32 {
    let paths = timeio::Paths::from_args(args);
    let (mut list, skipped) = timeio::load_timers(&paths);
    if skipped > 0 {
        println!(
            "timer: skipped {skipped} unreadable line(s) in {}",
            paths.timers
        );
    }
    let now = timeio::now_ms();
    let sub = args.get(1).unwrap_or("list");
    match sub {
        "list" | "ls" => {
            if list.list.is_empty() {
                println!("no timers (timer 5m tea)");
            }
            for t in &list.list {
                println!(
                    "{:>3}  {:>8}  {:<7}  {}",
                    t.id,
                    fmt_countdown(t.remaining_ms(now)),
                    if t.running { "running" } else { "paused" },
                    t.label
                );
            }
            return 0;
        }
        "pause" | "resume" | "reset" | "rm" => {
            let Some(id) = args.get(2).and_then(|s| s.parse::<u32>().ok()) else {
                return usage();
            };
            let found = if sub == "rm" {
                list.remove(id)
            } else {
                list.list.iter_mut().find(|t| t.id == id).is_some_and(|t| {
                    match sub {
                        "pause" => t.pause(now),
                        "resume" => t.resume(now),
                        _ => t.reset(),
                    }
                    true
                })
            };
            if !found {
                println!("timer: no timer {id}");
                return 1;
            }
        }
        d => {
            let Some(ms) = parse_duration(d) else {
                println!("timer: {d:?} is not a time (try 5m, 1h30m or 90s)");
                return usage();
            };
            let label = args.argv[2..].join(" ");
            let id = list.add(Timer::start(0, &label, ms, now));
            println!("timer {id}: {} {}", fmt_countdown(ms), label);
        }
    }
    match timeio::save_timers(&paths, &list) {
        Ok(()) => 0,
        Err(e) => {
            println!("timer: cannot save {}: {e}", paths.timers);
            1
        }
    }
}
