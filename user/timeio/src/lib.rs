//! Files, clock and notifications for the time apps. The logic itself is in `timekit`.
//!
//! The RTC holds local time, so "now" here is local time too.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use rt::sys;
use timekit::cal::Calendar;
use timekit::clock::{Alarms, Timers};

pub const DEFAULT_HOME: &str = "/home/milk";

/// Where the data files live.
#[derive(Clone, Debug)]
pub struct Paths {
    pub alarms: String,
    pub timers: String,
    pub calendar: String,
    home: String,
}

impl Paths {
    pub fn for_home(home: &str) -> Paths {
        Paths {
            alarms: format!("{home}/.config/milk/alarms"),
            timers: format!("{home}/.config/milk/timers"),
            calendar: format!("{home}/Library/Calendar/events"),
            home: String::from(home),
        }
    }

    /// `~/.cache/milk`: where alarmd keeps its log.
    pub fn cache_dir(&self) -> String {
        format!("{}/.cache/milk", self.home)
    }

    pub fn from_args(args: &rt::Args) -> Paths {
        Paths::for_home(args.env("HOME").unwrap_or(DEFAULT_HOME))
    }
}

/// Local seconds since 1970-01-01 00:00.
pub fn now_secs() -> i64 {
    (sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000_000) as i64
}

/// Local milliseconds since 1970-01-01 00:00.
pub fn now_ms() -> u64 {
    sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000
}

/// Milliseconds since boot (does not jump when the clock is set).
pub fn mono_ms() -> u64 {
    sys::clock_ns(sys::CLOCK_MONOTONIC) / 1_000_000
}

/// The file as text; empty if it is missing or unreadable.
pub fn read_text(path: &str) -> String {
    sys::read_file(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// `mkdir -p`.
pub fn mkdir_p(dir: &str) {
    for (i, _) in dir
        .match_indices('/')
        .chain(core::iter::once((dir.len(), "/")))
    {
        if i > 0 {
            let _ = sys::mkdir(&dir[..i], 0o755);
        }
    }
}

/// Write a file so a reader never sees half of it: write a sibling, then rename over the target.
pub fn write_atomic(path: &str, text: &str) -> sys::Result<()> {
    if let Some(i) = path.rfind('/') {
        mkdir_p(&path[..i]);
    }
    let tmp = format!("{path}.tmp");
    let fd = sys::open_mode(&tmp, sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC, 0o644)?;
    let r = sys::write_all(fd, text.as_bytes());
    let _ = sys::close(fd);
    r?;
    sys::rename(&tmp, path)
}

pub fn load_calendar(p: &Paths) -> (Calendar, usize) {
    Calendar::parse(&read_text(&p.calendar))
}

pub fn save_calendar(p: &Paths, c: &Calendar) -> sys::Result<()> {
    write_atomic(&p.calendar, &c.to_text())
}

pub fn load_alarms(p: &Paths) -> (Alarms, usize) {
    Alarms::parse(&read_text(&p.alarms))
}

pub fn save_alarms(p: &Paths, a: &Alarms) -> sys::Result<()> {
    write_atomic(&p.alarms, &a.to_text())
}

pub fn load_timers(p: &Paths) -> (Timers, usize) {
    Timers::parse(&read_text(&p.timers))
}

pub fn save_timers(p: &Paths, t: &Timers) -> sys::Result<()> {
    write_atomic(&p.timers, &t.to_text())
}

fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Show a toast on the desktop (the `notify` command of /dev/wm). Fails quietly with no desktop.
pub fn notify(title: &str, subtitle: &str) -> bool {
    let Ok(fd) = sys::open("/dev/wm", sys::O_WRONLY | sys::O_CLOEXEC) else {
        return false;
    };
    let line = format!("notify\t{}\t{}\n", one_line(title), one_line(subtitle));
    let ok = sys::write_all(fd, line.as_bytes()).is_ok();
    let _ = sys::close(fd);
    ok
}

/// Play the system chime without waiting for it (reap the child with [`reap_children`]).
pub fn chime() {
    let _ = sys::spawn(
        "/bin/play",
        &["play", "-n", "system", "chime"],
        &["PATH=/bin"],
    );
}

/// Collect any children that have exited, so they do not pile up as zombies.
pub fn reap_children() {
    const WNOHANG: usize = 1;
    while let Ok((pid, _)) = sys::wait4(-1, WNOHANG) {
        if pid == 0 {
            break;
        }
    }
}
