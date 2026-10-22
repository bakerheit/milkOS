//! Reading /System/Kernel/fs/milkfs and /proc/mounts, and writing the control files.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use diskkit::disks::{self, DiskEntry};
use diskkit::fsinfo::{self, ControlError, LockEntry, Locked, VolumeInfo};
use diskkit::mounts::{self, Volume};
use diskkit::scrub::{self, Status};
use diskkit::snaps::{self, AutoSettings, Clone_, Snap};
use rt::sys;

const DIR: &str = "/System/Kernel/fs/milkfs";

pub fn text(p: &str) -> String {
    sys::read_file(p)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// Write `text` to a control file. `Err` carries the errno. A destructive control
/// refused with EPERM needs an admin ticket (docs/security.md, "Authorisation tickets"):
/// ask the window system's trusted password panel once, then retry.
pub fn ctl(op: &str, text: &str) -> Result<(), i32> {
    let write = || -> Result<(), i32> {
        let fd = sys::open(&format!("{DIR}/{op}"), sys::O_WRONLY).map_err(|e| e.0)?;
        let r = sys::write(fd, text.as_bytes());
        let _ = sys::close(fd);
        r.map(|_| ()).map_err(|e| e.0)
    };
    match write() {
        Err(1) if sys::authorize(&format!("Disk Utility wants to {op}")).is_ok() => write(),
        r => r,
    }
}

/// What the volume looks like right now.
#[derive(Default)]
pub struct Disk {
    pub info: Option<VolumeInfo>,
    pub snaps: Vec<Snap>,
    pub clones: Vec<Clone_>,
    pub auto: Option<AutoSettings>,
    pub scrub: Option<Status>,
    pub locked: Vec<Locked>,
    /// Encrypted volumes and whether they can be locked (`/System/Kernel/fs/milkfs/lock`).
    pub lock: Vec<LockEntry>,
    /// Every block device (`/System/Kernel/fs/milkfs/disks`), on a system that lists them. Its presence also
    /// means backups of an encrypted volume can be encrypted.
    pub disks: Option<Vec<DiskEntry>>,
    pub volumes: Vec<Volume>,
    /// Size and free space by mount path, from statfs.
    pub usage: Vec<(String, Usage)>,
    /// Can the Mac folder be written? `None` until checked.
    pub share_rw: Option<bool>,
    pub root_used_kb: u64,
    pub total_kb: u64,
}

impl Disk {
    pub fn refresh(&mut self) {
        self.info = fsinfo::parse_info(&text(&format!("{DIR}/info")));
        self.snaps = snaps::parse_snapshots(&text(&format!("{DIR}/snapshots")));
        self.clones = snaps::parse_clones(&text(&format!("{DIR}/clones")));
        self.auto = snaps::parse_auto(&text(&format!("{DIR}/auto")));
        self.scrub = Some(scrub::parse(&text(&format!("{DIR}/scrub"))));
        self.locked = fsinfo::parse_locked(&text(&format!("{DIR}/unlock")));
        self.disks = sys::stat_path(&format!("{DIR}/disks"), true)
            .ok()
            .map(|_| disks::parse(&text(&format!("{DIR}/disks"))));
        self.lock = fsinfo::parse_lock(&text(&format!("{DIR}/lock")));
        self.volumes = mounts::volumes(&text("/proc/mounts"));
        self.usage = self
            .volumes
            .iter()
            .map(|v| v.path.clone())
            .chain(core::iter::once(String::from("/")))
            .filter_map(|p| usage_of(&p).map(|u| (p, u)))
            .collect();
        let mi = text("/proc/meminfo");
        let kb = |key: &str| {
            mi.lines()
                .find_map(|l| l.strip_prefix(key))
                .and_then(|v| {
                    v.trim_start_matches(':')
                        .split_whitespace()
                        .next()?
                        .parse()
                        .ok()
                })
                .unwrap_or(0)
        };
        self.root_used_kb = kb("Shmem");
        self.total_kb = kb("MemTotal");
    }

    /// Volumes for the sidebar that are neither the primary one nor a clone of it.
    pub fn other_volumes(&self) -> Vec<&Volume> {
        let mounts: Vec<Option<String>> = self.clones.iter().map(|c| c.mount.clone()).collect();
        let primary = self.info.as_ref().map(|i| format!("/Volumes/{}", i.label));
        self.volumes
            .iter()
            .filter(|v| !mounts::is_clone_mount(v, &mounts) && Some(&v.path) != primary.as_ref())
            .collect()
    }

    pub fn usage_of(&self, path: &str) -> Option<&Usage> {
        self.usage.iter().find(|(p, _)| p == path).map(|(_, u)| u)
    }

    /// The lock entry for a mount path (an encrypted volume), if there is one.
    pub fn lock_entry(&self, path: &str) -> Option<&LockEntry> {
        self.lock.iter().find(|l| l.at.as_deref() == Some(path))
    }

    pub fn snap_has_clone(&self, name: &str) -> bool {
        !snaps::clones_of(&self.clones, name).is_empty()
    }

    pub fn records_used(&self) -> usize {
        self.snaps.len() + self.clones.len()
    }
}

/// A volume's size and free space, from statfs.
#[derive(Clone, Copy, Debug)]
pub struct Usage {
    pub total_kb: u64,
    pub free_kb: u64,
    pub read_only: bool,
}

impl Usage {
    pub fn used_pct10(&self) -> u32 {
        (self.total_kb.saturating_sub(self.free_kb) * 1000)
            .checked_div(self.total_kb)
            .unwrap_or(0) as u32
    }
}

pub fn usage_of(path: &str) -> Option<Usage> {
    let st = sys::statfs(path).ok()?;
    if st.blocks == 0 {
        return None;
    }
    Some(Usage {
        total_kb: st.blocks * st.bsize / 1024,
        free_kb: st.bfree * st.bsize / 1024,
        read_only: st.read_only(),
    })
}

/// Why the last control write failed (`/System/Kernel/fs/milkfs/error`); read it right after the write.
pub fn last_error() -> Option<ControlError> {
    fsinfo::parse_error(&text(&format!("{DIR}/error")))
}

/// Is the Mac folder writable? Creates and removes a small file to find out.
pub fn probe_share() -> bool {
    let p = "/Volumes/Mac/.milk-write-test";
    match sys::open_mode(p, sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC, 0o644) {
        Ok(fd) => {
            let _ = sys::close(fd);
            let _ = sys::unlink(p);
            true
        }
        Err(_) => false,
    }
}

/// The stream files in the backup folder.
pub fn backup_files() -> Vec<String> {
    let names: Vec<String> = sys::read_dir(diskkit::backup::DIR)
        .unwrap_or_default()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    diskkit::backup::streams(&names)
        .into_iter()
        .cloned()
        .collect()
}

/// A background job: a forked child that does one blocking control-file write and exits with the
/// errno (0 on success), so the window stays alive.
pub struct Job {
    pub pid: u32,
    pub what: String,
    pub started_ms: u64,
    /// The control file whose report we read afterwards ("send", "receive", "check").
    pub report_from: &'static str,
}

pub fn spawn_write(what: &str, op: &'static str, text: &str, started_ms: u64) -> Option<Job> {
    match sys::fork() {
        Ok(0) => {
            let code = match ctl(op, text) {
                Ok(()) => 0,
                Err(e) => e.clamp(1, 120),
            };
            sys::exit(code);
        }
        Ok(pid) => Some(Job {
            pid,
            what: String::from(what),
            started_ms,
            report_from: op,
        }),
        Err(_) => None,
    }
}

/// `Some(errno)` once the job has finished (0 = fine).
pub fn poll_job(j: &Job) -> Option<i32> {
    match sys::wait4(j.pid as i32, 1) {
        Ok((0, _)) => None,
        Ok((_, status)) => Some((status >> 8) & 0xff),
        Err(_) => Some(0),
    }
}
