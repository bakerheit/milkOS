//! "Time Machine": automatic snapshots of the primary volume from a kernel thread.
//!
//! On boot, then every `interval`, the `timemachine` thread snapshots the volume as
//! `auto-YYYY-MM-DDTHH:MM:SS` (RTC time), unless nothing changed since the newest snapshot
//! (the committed root pointer is the same). Retention, auto snapshots only (manual ones
//! are never touched): keep the newest `keep`, plus the newest of each day for the last
//! `daily` days. If the 30-entry snapshot table is full, the oldest auto snapshot goes first.
//!
//! Settings live in the superblock (so they survive reboots) and are changed through
//! `/System/Kernel/fs/milkfs/auto`: write `on`, `off`, `interval=30s|15m|2h`, `keep=K`, `daily=D`,
//! `now` (space separated). A snapshot costs one commit plus a superblock write: no data
//! is copied.
//!
//! This is the policy and the snapshotting (pure, host-tested); the thread, the
//! `/System/Kernel/fs/milkfs/auto` file and the last-run time are the kernel's
//! (`kernel/src/fs/milkfs/timemachine.rs`).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::snap::SnapInfo;
use super::volume::State;
use crate::errno::{Errno, Result};

pub const PREFIX: &str = "auto-";
const DEFAULT_INTERVAL_S: u64 = 15 * 60;
const DEFAULT_KEEP: usize = 8;
const DEFAULT_DAILY: usize = 14;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub enabled: bool,
    pub interval_s: u64,
    pub keep: usize,
    pub daily: usize,
}

impl Config {
    pub fn from_raw(a: [u32; 4]) -> Config {
        let or = |v: u32, d: u64| if v == 0 { d } else { v as u64 };
        Config {
            enabled: a[0] != 2,
            interval_s: or(a[1], DEFAULT_INTERVAL_S),
            keep: or(a[2], DEFAULT_KEEP as u64) as usize,
            daily: or(a[3], DEFAULT_DAILY as u64) as usize,
        }
    }

    fn to_raw(self) -> [u32; 4] {
        [
            if self.enabled { 1 } else { 2 },
            self.interval_s.min(u32::MAX as u64) as u32,
            self.keep as u32,
            self.daily as u32,
        ]
    }
}

/// Unix seconds -> (year, month, day, hour, minute, second), proleptic Gregorian.
pub fn civil(t: u64) -> (u64, u64, u64, u64, u64, u64) {
    let (days, secs) = (t / 86400, t % 86400);
    // Howard Hinnant's civil_from_days.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (
        y as u64,
        m as u64,
        d as u64,
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
    )
}

pub fn stamp(t: u64) -> String {
    let (y, mo, d, h, mi, s) = civil(t);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
}

/// Which auto snapshots to delete. `snaps` are auto snapshots in any order.
pub fn prune_plan(snaps: &[SnapInfo], keep: usize, daily: usize) -> Vec<String> {
    let mut s: Vec<&SnapInfo> = snaps.iter().collect();
    s.sort_by_key(|x| core::cmp::Reverse(x.generation)); // newest first
    let mut keepers: Vec<&str> = s.iter().take(keep).map(|x| x.name.as_str()).collect();
    let mut days: Vec<u64> = Vec::new();
    for x in &s {
        let day = x.created / 86400;
        if !days.contains(&day) {
            if days.len() >= daily {
                break;
            }
            days.push(day);
            keepers.push(&x.name); // newest of its day
        }
    }
    s.iter()
        .filter(|x| !keepers.contains(&x.name.as_str()))
        .map(|x| x.name.clone())
        .collect()
}

/// The volume's automatic snapshots.
pub fn autos(st: &State) -> Vec<SnapInfo> {
    st.snap_list()
        .into_iter()
        .filter(|s| s.name.starts_with(PREFIX))
        .collect()
}

impl State {
    pub fn auto_config(&self) -> Config {
        Config::from_raw(self.auto)
    }

    pub fn set_auto_config(&mut self, c: Config) -> Result<()> {
        self.auto = c.to_raw();
        self.meta_dirty = true;
        self.commit()
    }

    /// Nothing changed since the newest snapshot?
    fn unchanged_since_snapshot(&self) -> bool {
        !self.dirty()
            && self
                .snaps
                .iter()
                .max_by_key(|s| s.generation)
                .is_some_and(|s| s.root == self.sb.root)
    }

    /// Take an automatic snapshot now (unless nothing changed), then apply retention.
    pub fn auto_snapshot(&mut self, now: u64) -> Result<Option<SnapInfo>> {
        if self.unchanged_since_snapshot() {
            return Ok(None);
        }
        let cfg = self.auto_config();
        let base = format!("{PREFIX}{}", stamp(now));
        let mut name = base.clone();
        let mut n = 2;
        while self.snaps.iter().any(|s| s.name == name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        if self.records_full() {
            let mut a: Vec<SnapInfo> = autos(self)
                .into_iter()
                .filter(|s| !self.is_origin(s.generation))
                .collect();
            a.sort_by_key(|s| s.generation);
            match a.first() {
                Some(oldest) => {
                    let o = oldest.name.clone();
                    self.snap_delete(&o)?;
                }
                None => return Err(Errno::ENOSPC), // all manual
            }
        }
        let info = self.snap_create(&name)?;
        for victim in prune_plan(&autos(self), cfg.keep, cfg.daily) {
            match self.snap_delete(&victim) {
                Ok(()) | Err(Errno::EBUSY) => {} // EBUSY: a clone's origin stays
                Err(e) => return Err(e),
            }
        }
        Ok(Some(info))
    }
}

pub fn interval_text(s: u64) -> String {
    if s % 3600 == 0 {
        format!("{}h", s / 3600)
    } else if s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// An interval as `/System/Kernel/fs/milkfs/auto` takes it: `30s`, `15m`, `2h`, or a bare number
/// of minutes.
pub fn parse_interval(v: &str) -> Option<u64> {
    let (num, mult) = match v.as_bytes().last()? {
        b's' => (&v[..v.len() - 1], 1),
        b'm' => (&v[..v.len() - 1], 60),
        b'h' => (&v[..v.len() - 1], 3600),
        _ => (v, 60), // bare number: minutes
    };
    let n: u64 = num.parse().ok()?;
    (n > 0).then_some(n * mult)
}
