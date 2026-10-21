//! Files, clock and notifications for the time apps. The logic itself is in `timekit`.
//!
//! The RTC holds local time, so "now" here is local time too.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use rt::sys;
use timekit::cal::CalendarSet;
use timekit::clock::{Alarms, Timers};

pub const DEFAULT_HOME: &str = "/home/milk";

/// Where the data files live.
#[derive(Clone, Debug)]
pub struct Paths {
    /// `~/Library/Calendar`: one `<Name>.cal` file per calendar.
    pub calendars: String,
    pub alarms: String,
    pub timers: String,
    /// The custom timer durations Clock remembers.
    pub recent: String,
    home: String,
}

impl Paths {
    pub fn for_home(home: &str) -> Paths {
        Paths {
            calendars: format!("{home}/Library/Calendar"),
            alarms: format!("{home}/Library/Clock/alarms"),
            timers: format!("{home}/Library/Clock/timers"),
            recent: format!("{home}/Library/Clock/recent"),
            home: String::from(home),
        }
    }

    pub fn from_args(args: &rt::Args) -> Paths {
        Paths::for_home(args.env("HOME").unwrap_or(DEFAULT_HOME))
    }

    /// `~/.cache/milk`: where alarmd keeps its log.
    pub fn cache_dir(&self) -> String {
        format!("{}/.cache/milk", self.home)
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

/// Read every `*.cal` file in the calendar folder. No folder or no files gives the default
/// calendars (Personal, Home, Work). Returns the set and how many lines were skipped.
pub fn load_calendars(p: &Paths) -> (CalendarSet, usize) {
    let mut files: Vec<(String, String)> = Vec::new();
    for (name, _) in sys::read_dir(&p.calendars).unwrap_or_default() {
        if let Some(stem) = name.strip_suffix(".cal") {
            files.push((
                String::from(stem),
                read_text(&format!("{}/{name}", p.calendars)),
            ));
        }
    }
    files.sort();
    CalendarSet::from_files(&files)
}

/// Write each calendar to its file, and remove the `.cal` files of calendars that are gone
/// (renamed or deleted in the app).
pub fn save_calendars(p: &Paths, set: &CalendarSet) -> sys::Result<()> {
    let keep: Vec<String> = set.cals.iter().map(CalendarSet::file_name).collect();
    for (c, f) in set.cals.iter().zip(&keep) {
        write_atomic(&format!("{}/{f}", p.calendars), &c.to_text())?;
    }
    for (name, _) in sys::read_dir(&p.calendars).unwrap_or_default() {
        if name.ends_with(".cal") && !keep.contains(&name) {
            let _ = sys::unlink(&format!("{}/{name}", p.calendars));
        }
    }
    Ok(())
}

/// A number that is the same for every process in this boot and different in the next one:
/// timers store it so a running one from an earlier boot is not mistaken for a live one.
pub fn boot_id() -> u64 {
    const FILE: &str = "/run/boot-id";
    let read = || {
        sys::read_file(FILE).ok().and_then(|b| {
            core::str::from_utf8(&b)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
        })
    };
    if let Some(id) = read() {
        return id;
    }
    let id = (sys::clock_ns(sys::CLOCK_REALTIME)
        ^ sys::clock_ns(sys::CLOCK_MONOTONIC).rotate_left(21)
        ^ (sys::getpid() as u64) << 40)
        | 1;
    mkdir_p("/run");
    match sys::open_mode(FILE, sys::O_WRONLY | sys::O_CREAT | sys::O_EXCL, 0o644) {
        Ok(fd) => {
            let _ = sys::write_all(fd, format!("{id}\n").as_bytes());
            let _ = sys::close(fd);
            id
        }
        Err(_) => read().unwrap_or(id), // another process created it first
    }
}

pub fn load_alarms(p: &Paths) -> (Alarms, usize) {
    Alarms::parse(&read_text(&p.alarms))
}

pub fn save_alarms(p: &Paths, a: &Alarms) -> sys::Result<()> {
    write_atomic(&p.alarms, &a.to_text())
}

/// The timers, belonging to this boot (see [`boot_id`]). Running timers written in an earlier
/// boot are dropped.
pub fn load_timers(p: &Paths) -> (Timers, usize) {
    let (mut t, skipped) = Timers::parse(&read_text(&p.timers));
    t.for_boot(boot_id());
    (t, skipped)
}

pub fn save_timers(p: &Paths, t: &Timers) -> sys::Result<()> {
    let mut t = t.clone();
    t.boot = boot_id();
    write_atomic(&p.timers, &t.to_text())
}

pub fn load_recent(p: &Paths) -> Vec<u32> {
    timekit::clock::parse_recent(&read_text(&p.recent))
}

pub fn save_recent(p: &Paths, r: &[u32]) -> sys::Result<()> {
    let line: Vec<String> = r.iter().map(|m| format!("{m}")).collect();
    write_atomic(&p.recent, &(line.join(" ") + "\n"))
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
