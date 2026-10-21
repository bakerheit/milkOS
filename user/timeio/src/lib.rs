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
use timekit::reminders::ReminderSet;

pub const DEFAULT_HOME: &str = "/home/milk";

/// Where the data files live.
#[derive(Clone, Debug)]
pub struct Paths {
    /// `~/Library/Calendar`: one `<Name>.cal` file per calendar.
    pub calendars: String,
    /// `~/Library/Reminders`: one `<Name>.rem` file per list.
    pub reminders: String,
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
            reminders: format!("{home}/Library/Reminders"),
            alarms: format!("{home}/Library/Clock/alarms"),
            timers: format!("{home}/Library/Clock/timers"),
            recent: format!("{home}/Library/Clock/recent"),
            home: String::from(home),
        }
    }

    /// The paths for the user this process runs as. Apps started from the dock get no useful
    /// HOME (empty or "/"), so those fall back to the OS user's home.
    pub fn from_args(args: &rt::Args) -> Paths {
        let home = args
            .env("HOME")
            .filter(|h| h.len() > 1 && h.starts_with('/'))
            .unwrap_or(DEFAULT_HOME);
        Paths::for_home(home.trim_end_matches('/'))
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

/// When root (alarmd, a root shell) creates something in a user's folder, give it to that
/// folder's owner, so the user's own apps can still change it.
fn inherit_owner(path: &str) {
    if sys::geteuid() != 0 {
        return;
    }
    let parent = path
        .rfind('/')
        .map_or("/", |i| if i == 0 { "/" } else { &path[..i] });
    if let Ok(st) = sys::stat_path(parent, true)
        && st.uid != 0
    {
        let _ = sys::chown(path, st.uid, st.gid);
    }
}

/// `mkdir -p`; each folder it makes belongs to its parent's owner.
pub fn mkdir_p(dir: &str) {
    for (i, _) in dir
        .match_indices('/')
        .chain(core::iter::once((dir.len(), "/")))
    {
        if i > 0 && sys::mkdir(&dir[..i], 0o755).is_ok() {
            inherit_owner(&dir[..i]);
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
    inherit_owner(&tmp);
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

/// Read every `*.rem` file in the reminders folder; none gives the default "Reminders" list.
pub fn load_reminders(p: &Paths) -> (ReminderSet, usize) {
    let mut files: Vec<(String, String)> = Vec::new();
    for (name, _) in sys::read_dir(&p.reminders).unwrap_or_default() {
        if let Some(stem) = name.strip_suffix(".rem") {
            files.push((
                String::from(stem),
                read_text(&format!("{}/{name}", p.reminders)),
            ));
        }
    }
    files.sort();
    ReminderSet::from_files(&files)
}

/// Write each list to its file and remove the files of lists that are gone.
pub fn save_reminders(p: &Paths, set: &ReminderSet) -> sys::Result<()> {
    let keep: Vec<String> = set.lists.iter().map(ReminderSet::file_name).collect();
    for (l, f) in set.lists.iter().zip(&keep) {
        write_atomic(&format!("{}/{f}", p.reminders), &l.to_text())?;
    }
    for (name, _) in sys::read_dir(&p.reminders).unwrap_or_default() {
        if name.ends_with(".rem") && !keep.contains(&name) {
            let _ = sys::unlink(&format!("{}/{name}", p.reminders));
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
        // Another process created it first, or this user may not create it (alarmd, started at
        // boot, normally has): fall back to a number every process in this boot agrees on.
        Err(_) => read().unwrap_or_else(stable_boot_id),
    }
}

/// The wall clock minus the monotonic clock is the same for every process until the clock is set,
/// and different after a reboot; four-second buckets absorb the jitter between two reads.
fn stable_boot_id() -> u64 {
    let since_boot =
        sys::clock_ns(sys::CLOCK_REALTIME).saturating_sub(sys::clock_ns(sys::CLOCK_MONOTONIC));
    (since_boot / 4_000_000_000) | 1
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

/// Show a toast with up to two buttons (/dev/wm `notify`, docs/processes.md). A button is a label and
/// the command the desktop runs when it is pressed (an absolute path or a /bin program with its
/// arguments, or `open <what>`; no shell), so a windowless daemon can offer Snooze and Done.
pub fn notify_with(app: &str, title: &str, sub: &str, id: u32, actions: &[(&str, &str)]) -> bool {
    let Ok(fd) = sys::open("/dev/wm", sys::O_WRONLY | sys::O_CLOEXEC) else {
        return false;
    };
    let mut line = format!(
        "notify\t{}\t{}\tapp={}\tid={id}",
        one_line(title),
        one_line(sub),
        one_line(app)
    );
    for (label, command) in actions.iter().take(2) {
        line.push_str(&format!(
            "\taction={}:{}",
            one_line(label).replace(':', " "),
            one_line(command)
        ));
    }
    line.push('\n');
    let ok = sys::write_all(fd, line.as_bytes()).is_ok();
    let _ = sys::close(fd);
    ok
}

/// Show (or with empty `text`, clear) this app's countdown in the menu bar; it goes by itself 5
/// seconds after the last call, so call it every second while it runs.
pub fn menu_timer(app: &str, text: &str, open: &str) -> bool {
    let Ok(fd) = sys::open("/dev/wm", sys::O_WRONLY | sys::O_CLOEXEC) else {
        return false;
    };
    let line = format!(
        "timer\t{}\tapp={}\topen={}\n",
        one_line(text),
        one_line(app),
        one_line(open)
    );
    let ok = sys::write_all(fd, line.as_bytes()).is_ok();
    let _ = sys::close(fd);
    ok
}

/// Play the system chime without waiting for it (reap the child with [`reap_children`]).
pub fn chime() {
    // No sound device (a headless VM): `play` would only complain on the console.
    if sys::stat_path("/dev/audio", true).is_err() {
        return;
    }
    let _ = sys::spawn(
        "/bin/play",
        &["play", "-n", "system", "chime"],
        &["PATH=/bin"],
    );
}

/// Open the ringing panel (Snooze / Stop with the alarm sound) for an alarm. False if Clock
/// could not be started.
pub fn open_alarm_panel(label: &str, hhmm: &str) -> bool {
    sys::spawn(
        "/bin/clock",
        &["clock", "ring", label, hhmm],
        &["PATH=/bin", "HOME=/home/milk"],
    )
    .is_ok()
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
