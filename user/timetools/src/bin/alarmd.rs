//! alarmd [-q]: rings alarms, finished timers and calendar alerts as desktop notifications,
//! and appends a line per ring to `~/.cache/milk/alarmd.log` (so a missed toast can be found).
//! -q: say nothing at start.
//!
//! Started by init. Once a second it looks at the files Clock and Calendar write:
//! `~/Library/Clock/alarms`, `~/Library/Clock/timers` and `~/Library/Calendar/*.cal`. Nothing is
//! cached, so an edit made by an app counts on the next tick. Time that passed while the VM was
//! paused is caught up (at most ten minutes of it); a clock set backwards rings nothing.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use rt::{println, sys};
use timekit::text::fmt_hm;

rt::main!(main);

/// The longest stretch of missed time (clock jump, paused VM) that is replayed.
const CATCH_UP_SECS: i64 = 600;
const LOG_LIMIT: u64 = 64 * 1024;

/// Show a toast and add a line to the log.
fn ring(paths: &timeio::Paths, kind: &str, title: &str, sub: &str) {
    timeio::notify(title, sub);
    let (day, sod) = timekit::date::split_secs(timeio::now_secs());
    let line = format!(
        "{} {:02}:{:02}:{:02} {kind}: {title} / {sub}\n",
        day.fmt(),
        sod / 3600,
        sod / 60 % 60,
        sod % 60
    );
    let log = format!("{}/alarmd.log", paths.cache_dir());
    timeio::mkdir_p(&paths.cache_dir());
    // Start a fresh file when it gets big, so it never grows without bound.
    let big = sys::stat_path(&log, true).is_ok_and(|st| st.size > LOG_LIMIT);
    let trunc = if big { sys::O_TRUNC } else { 0 };
    if let Ok(fd) = sys::open_mode(
        &log,
        sys::O_WRONLY | sys::O_CREAT | sys::O_APPEND | trunc,
        0o644,
    ) {
        let _ = sys::write_all(fd, line.as_bytes());
        let _ = sys::close(fd);
    }
}

fn main(args: &rt::Args) -> i32 {
    let paths = timeio::Paths::from_args(args);
    if !args.argv.contains(&"-q") {
        println!(
            "alarmd: watching {} {} {}",
            paths.alarms, paths.timers, paths.calendars
        );
    }
    // Create the boot id now, as the user who starts the daemon (init), so apps only ever read it.
    let _ = timeio::boot_id();
    let mut last = timeio::now_secs();
    loop {
        // Wake just after the second changes.
        let ms = timeio::now_ms();
        sys::sleep_ms(1050 - ms % 1000);
        timeio::reap_children();
        let now = timeio::now_secs();
        if now <= last {
            last = now;
            continue;
        }
        let from = last.max(now - CATCH_UP_SECS);
        alarms(&paths, from, now);
        timers(&paths, timeio::mono_ms());
        calendar(&paths, from, now);
        last = now;
    }
}

fn alarms(paths: &timeio::Paths, from: i64, now: i64) {
    let (mut list, _) = timeio::load_alarms(paths);
    let due = list.take_due(from, now);
    if due.is_empty() {
        return;
    }
    for (id, label) in &due {
        let at = list.list.iter().find(|a| a.id == *id).map_or(0, |a| a.at);
        let sub = if label.is_empty() {
            fmt_hm(at)
        } else {
            format!("{} ({})", label, fmt_hm(at))
        };
        ring(paths, "alarm", "Alarm", &sub);
        // The panel plays the alarm sound until it is stopped; without it, one chime.
        if !timeio::open_alarm_panel(label, &fmt_hm(at)) {
            timeio::chime();
        }
    }
    // Switch off the "once" alarms that just rang, on a fresh copy so an edit made while we
    // were ringing is not lost.
    let (mut fresh, _) = timeio::load_alarms(paths);
    let mut changed = false;
    for (id, _) in &due {
        if let Some(a) = fresh.list.iter_mut().find(|a| a.id == *id)
            && a.days == 0
            && a.enabled
        {
            a.enabled = false;
            changed = true;
        }
    }
    if changed {
        let _ = timeio::save_alarms(paths, &fresh);
    }
}

fn timers(paths: &timeio::Paths, mono_ms: u64) {
    let (mut list, _) = timeio::load_timers(paths);
    let done = list.take_due(mono_ms);
    if done.is_empty() {
        return;
    }
    for t in &done {
        let sub = if t.label.is_empty() {
            "Time is up".into()
        } else {
            t.label.clone()
        };
        ring(paths, "timer", "Timer done", &sub);
    }
    timeio::chime();
    // Drop the finished ones from a fresh copy of the file (the user may have added another).
    let (mut fresh, _) = timeio::load_timers(paths);
    for t in &done {
        fresh.remove(t.id);
    }
    let _ = timeio::save_timers(paths, &fresh);
}

fn calendar(paths: &timeio::Paths, from: i64, now: i64) {
    let (set, _) = timeio::load_calendars(paths);
    for (id, day, title) in set.alerts_due(from, now) {
        let at = set
            .get(id)
            .and_then(|(_, e)| e.start)
            .map_or(alloc::string::String::new(), fmt_hm);
        let (today, _) = timekit::date::split_secs(now);
        let when = if day == today {
            format!("Today {at}")
        } else {
            format!("{} {at}", day.fmt())
        };
        ring(paths, "event", &title, when.trim());
    }
}
