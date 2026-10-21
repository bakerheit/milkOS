//! milkfs v0: a copy-on-write file system with instant snapshots. See
//! `docs/filesystem.md` ("milkfs") for the format and the design.
//!
//! - `format`: on-disk structures (superblock, nodes, dead lists, items), xxh64.
//! - `store`: block allocation (free map rebuilt at mount), birth-time release rules.
//! - `btree`: the CoW B+tree.
//! - `volume`: transaction / commit, mount (mark + check + orphan reclaim), mkfs.
//! - `items`: inodes, directory entries, extents, file data on top of the tree.
//! - `snap`: snapshot create / delete / rollback.
//! - `inode`: the VFS objects (`Volume`, `MilkInode`, `.snapshots`).
//!
//! Kernel API: [`mkfs`], [`probe`], [`mount`], [`automount`], [`snapshot`], [`snapshots`],
//! [`delete_snapshot`], [`rollback`], [`sync`], [`info`]. Shell API: `/sys/fs/milkfs/*`.

pub mod btree;
pub mod crashtest;
pub mod format;
pub mod inode;
pub mod items;
pub mod snap;
pub mod store;
pub mod volume;

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::fmt::Write;

use crate::fs::blockdev::{self, BlockRef};
use crate::fs::errno::{Errno, Result};
use crate::fs::synth::SynthDir;
use crate::kprintln;
use crate::sync::SpinLock;
pub use format::Superblock;
pub use inode::Volume;
pub use snap::SnapInfo;
pub use volume::MountReport;

/// The volume the snapshot API and `/sys/fs/milkfs` act on (the first one mounted).
static PRIMARY: SpinLock<Option<Arc<Volume>>> = SpinLock::new(None);

/// The superblock of `dev` if it holds milkfs.
pub fn probe(dev: &BlockRef) -> Option<Superblock> {
    volume::probe(dev)
}

/// Format `dev` as an empty milkfs. Refuses (EEXIST) if it already holds milkfs unless
/// `force`, and always (EBUSY) if it is the mounted volume's device.
pub fn mkfs(dev: &BlockRef, label: &str, force: bool) -> Result<()> {
    if label.is_empty() || label.len() > format::LABEL_MAX || label.contains('/') {
        return Err(Errno::EINVAL);
    }
    if PRIMARY
        .lock()
        .as_ref()
        .is_some_and(|v| v.device_name == dev.name())
    {
        return Err(Errno::EBUSY);
    }
    if probe(dev).is_some() && !force {
        return Err(Errno::EEXIST);
    }
    volume::mkfs(dev, label)
}

/// Open the milkfs on `dev` and mount it at `path` (created if missing).
pub fn mount(dev: BlockRef, path: &str) -> Result<(Arc<Volume>, MountReport)> {
    let name = String::from(dev.name());
    let (state, report) = volume::State::open(dev)?;
    let vol = Volume::new(state, &name);
    crate::fs::ops::mkdir_all(path, 0o755)?;
    crate::fs::path::mount(path, vol.clone())?;
    let mut p = PRIMARY.lock();
    if p.is_none() {
        *p = Some(vol.clone());
        *PRIMARY_PATH.lock() = String::from(path);
    }
    Ok((vol, report))
}

/// Mount the first disk that holds milkfs at `/Volumes/<label>`. Never formats anything.
pub fn automount() {
    for dev in blockdev::devices() {
        let Some(sb) = probe(&dev) else { continue };
        let label = if sb.label.is_empty() || sb.label.contains('/') {
            String::from("Milk")
        } else {
            sb.label.clone()
        };
        let path = format!("/Volumes/{label}");
        match mount(crashtest::wrap(dev.clone()), &path) {
            Ok((vol, r)) => {
                let st = vol.state.lock();
                kprintln!(
                    "milkfs: {} mounted at {path}: generation {}, {} inodes, {}/{} blocks used, {} snapshot(s), check {}{}",
                    dev.name(),
                    r.generation,
                    r.inodes,
                    r.used_blocks,
                    st.store.total(),
                    st.snaps.len(),
                    if r.problems == 0 {
                        "clean"
                    } else {
                        "FOUND PROBLEMS"
                    },
                    if r.orphans > 0 {
                        format!(", {} orphan(s) reclaimed", r.orphans)
                    } else {
                        String::new()
                    }
                );
            }
            Err(e) => kprintln!("milkfs: mounting {} failed: {e}", dev.name()),
        }
        return;
    }
}

/// Where the primary volume is mounted (`/Volumes/<label>`).
static PRIMARY_PATH: SpinLock<String> = SpinLock::new(String::new());

pub fn mount_path() -> Option<String> {
    let p = PRIMARY_PATH.lock().clone();
    (!p.is_empty()).then_some(p)
}

/// The user's home directory on the primary volume.
pub const HOME: &str = "/home/milk";

/// Persistent home: bind `<volume>/home/milk` onto `/home/milk` (seeding it from the
/// initramfs copy the first time). Without a volume, `/home/milk` stays on tmpfs. Runs
/// in `fs::init`, before the desktop creates its folders in the home directory.
pub fn setup_home() {
    let Some(vol) = mount_path() else { return };
    let on_disk = format!("{vol}{HOME}");
    let fresh = crate::fs::ops::stat(&on_disk).is_err();
    if fresh && let Err(e) = crate::fs::ops::copy_tree(HOME, &on_disk) {
        kprintln!("milkfs: can't create {on_disk}: {e}; home stays in RAM");
        return;
    }
    match crate::fs::path::bind(&on_disk, HOME) {
        Ok(()) => kprintln!(
            "milkfs: {HOME} is {on_disk}{} (persistent)",
            if fresh { ", new" } else { "" }
        ),
        Err(e) => kprintln!("milkfs: bind {on_disk} -> {HOME} failed: {e}; home stays in RAM"),
    }
}

pub fn volume() -> Option<Arc<Volume>> {
    PRIMARY.lock().clone()
}

fn primary() -> Result<Arc<Volume>> {
    volume().ok_or(Errno::ENODEV)
}

/// Take a snapshot of the primary volume.
pub fn snapshot(name: &str) -> Result<SnapInfo> {
    primary()?.state.lock().snap_create(name)
}

pub fn snapshots() -> Result<alloc::vec::Vec<SnapInfo>> {
    Ok(primary()?.state.lock().snap_list())
}

pub fn delete_snapshot(name: &str) -> Result<()> {
    primary()?.state.lock().snap_delete(name)
}

/// Roll the primary volume back to snapshot `name` (newer snapshots are deleted).
pub fn rollback(name: &str) -> Result<()> {
    let v = primary()?;
    let zombies = {
        let mut st = v.state.lock();
        st.snap_rollback(name)?;
        st.unlinked_inodes()?
    };
    v.reclaim(&zombies)
}

/// Commit anything pending (v0 commits every operation already, so this is cheap).
pub fn sync() -> Result<()> {
    primary()?.state.lock().commit()
}

/// Human-readable status of the primary volume (`/sys/fs/milkfs/info`).
pub fn info() -> String {
    let Some(v) = volume() else {
        return String::from("no milkfs volume mounted\n");
    };
    let st = v.state.lock();
    let c = st.cache().stats();
    let (bufs, cap, dirty) = st.cache().usage();
    let mut s = String::new();
    let _ = writeln!(s, "label:       {}", st.label());
    let _ = writeln!(s, "device:      {}", v.device_name);
    let _ = writeln!(s, "generation:  {}", st.sb.generation);
    let _ = writeln!(
        s,
        "blocks:      {} total, {} free (4 KiB)",
        st.store.total(),
        st.store.free_blocks()
    );
    let _ = writeln!(s, "snapshots:   {}", st.snaps.len());
    let _ = writeln!(s, "next inode:  {}", st.next_ino);
    let _ = writeln!(s, "commits:     {} this boot", st.commits);
    let _ = writeln!(
        s,
        "state:       {}",
        if st.broken {
            "BROKEN (read-only)"
        } else {
            "ok"
        }
    );
    let _ = writeln!(
        s,
        "bcache:      {bufs}/{cap} buffers, {dirty} dirty, {} hits, {} misses, {} writebacks",
        c.hits, c.misses, c.writebacks
    );
    s
}

fn snapshots_text() -> String {
    let mut s = String::new();
    if let Ok(list) = snapshots() {
        for i in list {
            let _ = writeln!(s, "{} gen={} created={}", i.name, i.generation, i.created);
        }
    }
    s
}

fn arg(buf: &[u8]) -> Result<&str> {
    let s = core::str::from_utf8(buf).map_err(|_| Errno::EINVAL)?.trim();
    if s.is_empty() {
        return Err(Errno::EINVAL);
    }
    Ok(s)
}

/// `/sys/fs/milkfs/`: `info`, `snapshots` (read); `snapshot`, `delete`, `rollback`,
/// `sync`, `mkfs` (write a name / `vdX [label] [force]`).
pub fn sysfs(dir: &Arc<SynthDir>) {
    dir.add_file("info", info);
    dir.add_file("snapshots", snapshots_text);
    dir.add_control("snapshot", snapshots_text, |b| {
        snapshot(arg(b)?).map(|_| ())
    });
    dir.add_control("delete", snapshots_text, |b| delete_snapshot(arg(b)?));
    dir.add_control("rollback", snapshots_text, |b| rollback(arg(b)?));
    dir.add_control("sync", String::new, |_| sync());
    dir.add_control(
        "mkfs",
        || String::from("write: <device> [label] [force]\n"),
        |b| {
            let mut words = arg(b)?.split_whitespace();
            let dev = words.next().ok_or(Errno::EINVAL)?;
            let (mut label, mut force) = ("Milk", false);
            for w in words {
                if w == "force" {
                    force = true;
                } else {
                    label = w;
                }
            }
            let d = blockdev::get(dev).ok_or(Errno::ENODEV)?;
            mkfs(&d, label, force)
        },
    );
}
