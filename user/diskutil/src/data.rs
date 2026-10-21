//! Reading /sys/fs/milkfs and /proc/mounts, and writing the control files.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use diskkit::fsinfo::{self, Locked, VolumeInfo};
use diskkit::mounts::{self, Volume};
use diskkit::scrub::{self, Status};
use diskkit::snaps::{self, AutoSettings, Clone_, Snap};
use rt::sys;

const DIR: &str = "/sys/fs/milkfs";

pub fn text(p: &str) -> String {
    sys::read_file(p)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// Write `text` to a control file. `Err` carries the errno.
pub fn ctl(op: &str, text: &str) -> Result<(), i32> {
    let fd = sys::open(&format!("{DIR}/{op}"), sys::O_WRONLY).map_err(|e| e.0)?;
    let r = sys::write(fd, text.as_bytes());
    let _ = sys::close(fd);
    r.map(|_| ()).map_err(|e| e.0)
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
    pub volumes: Vec<Volume>,
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
        self.volumes = mounts::volumes(&text("/proc/mounts"));
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

    /// Volumes that are not clones, for the sidebar.
    pub fn other_volumes(&self) -> Vec<&Volume> {
        let mounts: Vec<Option<String>> = self.clones.iter().map(|c| c.mount.clone()).collect();
        self.volumes
            .iter()
            .filter(|v| v.kind != mounts::Kind::Milk && !mounts::is_clone_mount(v, &mounts))
            .collect()
    }

    pub fn snap_has_clone(&self, name: &str) -> bool {
        !snaps::clones_of(&self.clones, name).is_empty()
    }

    pub fn records_used(&self) -> usize {
        self.snaps.len() + self.clones.len()
    }
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
