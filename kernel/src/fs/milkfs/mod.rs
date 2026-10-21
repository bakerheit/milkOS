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
pub mod clone;
pub mod crashtest;
pub mod format;
pub mod inode;
pub mod items;
pub mod snap;
pub mod store;
pub mod timemachine;
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
pub use clone::CloneInfo;
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
        || TEST_OVERRIDE
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

/// Open the milkfs on `dev` and mount it at `path` (created if missing). The first
/// volume mounted with this becomes the primary one (snapshot API, `/sys/fs/milkfs`).
pub fn mount(dev: BlockRef, path: &str) -> Result<(Arc<Volume>, MountReport)> {
    let r = mount_secondary(dev, path)?;
    let mut p = PRIMARY.lock();
    if p.is_none() {
        *p = Some(r.0.clone());
        *PRIMARY_PATH.lock() = String::from(path);
    }
    Ok(r)
}

/// Mount without ever becoming the primary volume (self-test volumes).
pub fn mount_secondary(dev: BlockRef, path: &str) -> Result<(Arc<Volume>, MountReport)> {
    let name = String::from(dev.name());
    let (state, report) = volume::State::open(dev)?;
    let vol = Volume::new(state, &name);
    crate::fs::ops::mkdir_all(path, 0o755)?;
    crate::fs::path::mount(path, vol.clone())?;
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
        // Clones made on an earlier boot.
        if let Some(v) = user_volume() {
            let list = v.state.lock().clone_list();
            for c in list {
                match mount_clone(&v, &c.name, c.id) {
                    Ok(p) => kprintln!("milkfs: clone {} (of {}) mounted at {p}", c.name, c.origin),
                    Err(e) => kprintln!("milkfs: clone {}: {e}", c.name),
                }
            }
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

// ---- clones -------------------------------------------------------------------------

/// Where each clone of the primary volume is mounted: (name, path).
static CLONE_MOUNTS: SpinLock<alloc::vec::Vec<(String, String)>> =
    SpinLock::new(alloc::vec::Vec::new());

/// `/Volumes/<label>-<clone>`.
fn clone_path(v: &Volume, name: &str) -> String {
    format!("/Volumes/{}-{name}", v.label)
}

fn mount_clone(v: &Arc<Volume>, name: &str, id: u32) -> Result<String> {
    let path = clone_path(v, name);
    crate::fs::ops::mkdir_all(&path, 0o755)?;
    crate::fs::path::mount(
        &path,
        Arc::new(inode::ViewFs {
            vol: v.clone(),
            view: items::clone_view(id),
        }),
    )?;
    CLONE_MOUNTS.lock().push((String::from(name), path.clone()));
    Ok(path)
}

fn unmount_clone(name: &str) {
    let path = {
        let mut m = CLONE_MOUNTS.lock();
        let Some(i) = m.iter().position(|(n, _)| n == name) else {
            return;
        };
        m.remove(i).1
    };
    let _ = crate::fs::path::unmount(&path);
    let _ = crate::fs::ops::rmdir(&path);
}

/// A writable clone of snapshot `snap`, mounted at `/Volumes/<label>-<name>`. Returns
/// the mount path. Install into the clone, try it, then [`promote_clone`] or
/// [`discard_clone`].
pub fn clone(snap: &str, name: &str) -> Result<String> {
    let v = primary()?;
    let id = v.state.lock().clone_create(snap, name)?;
    let r = mount_clone(&v, name, id);
    audit_op(
        "milkfs.clone",
        &alloc::format!("snap={snap} name={name}"),
        &r,
    );
    r
}

pub fn clones() -> Result<alloc::vec::Vec<CloneInfo>> {
    Ok(primary()?.state.lock().clone_list())
}

/// Throw clone `name` away (unmounts it; files open in it see ENOENT).
pub fn discard_clone(name: &str) -> Result<()> {
    let v = primary()?;
    v.state.lock().clone_index_check(name)?;
    unmount_clone(name);
    let r = v.state.lock().clone_discard(name);
    audit_op("milkfs.clone.discard", &alloc::format!("name={name}"), &r);
    r
}

/// Make clone `name` the live volume. The live state it replaces is kept as snapshot
/// `before-<name>`, so `rollback before-<name>` undoes it. Returns that snapshot's name.
pub fn promote_clone(name: &str) -> Result<String> {
    let v = primary()?;
    v.state.lock().clone_index_check(name)?;
    let mut keep = format!("before-{name}");
    keep.truncate(format::SNAP_NAME_MAX);
    unmount_clone(name);
    let r = v.state.lock().clone_promote(name, &keep);
    audit_op(
        "milkfs.clone.promote",
        &alloc::format!("name={name} kept_as={keep}"),
        &r,
    );
    r?;
    if user_op() {
        crate::audit::volume_rewound();
    }
    Ok(keep)
}

/// Background work, once the scheduler runs: the flusher (commits the open transaction
/// once it is a second old) and the Time Machine thread.
pub fn start_threads() {
    if volume().is_none() {
        return;
    }
    drop(crate::sched::spawn("milkfs-flush", || {
        loop {
            crate::sched::sleep_ms(250);
            if let Some(v) = volume() {
                let _ = v.state.lock().flush_if_due();
            }
        }
    }));
    timemachine::start();
}

/// Self-test only (before the scheduler runs, so nothing else can notice): point the
/// primary-volume API and `/sys/fs/milkfs` at a throwaway test volume, so destructive
/// checks never reach the user's disk. `None` restores the real primary volume.
static TEST_OVERRIDE: SpinLock<Option<Arc<Volume>>> = SpinLock::new(None);

pub fn set_test_override(v: Option<Arc<Volume>>) {
    *TEST_OVERRIDE.lock() = v;
}

pub fn volume() -> Option<Arc<Volume>> {
    if let Some(v) = TEST_OVERRIDE.lock().clone() {
        return Some(v);
    }
    PRIMARY.lock().clone()
}

/// The user's volume, ignoring any self-test override.
pub fn user_volume() -> Option<Arc<Volume>> {
    PRIMARY.lock().clone()
}

fn primary() -> Result<Arc<Volume>> {
    volume().ok_or(Errno::ENODEV)
}

/// Take a snapshot of the primary volume.
pub fn snapshot(name: &str) -> Result<SnapInfo> {
    let r = primary()?.state.lock().snap_create(name);
    audit_op("milkfs.snapshot", &alloc::format!("name={name}"), &r);
    r
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
    let r = v.reclaim(&zombies);
    audit_op("milkfs.rollback", &alloc::format!("to={name}"), &r);
    if user_op() {
        crate::audit::volume_rewound();
    }
    r
}

/// Is the public API working on the user's volume (not a self-test volume)?
fn user_op() -> bool {
    TEST_OVERRIDE.lock().is_none()
}

/// Audit a user-volume operation (self-test volumes are not audited).
fn audit_op<T>(event: &str, detail: &str, r: &Result<T>) {
    if user_op() {
        let d = match r {
            Ok(_) => alloc::format!("{detail} result=ok"),
            Err(e) => alloc::format!("{detail} result={e:?}"),
        };
        crate::audit::record(event, &d);
    }
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
        "open txn:    {} op(s){}",
        st.batched_ops,
        match st.dirty_since {
            Some(t) => format!(
                ", dirty for {} ms",
                (crate::timer::now_ns() - t) / 1_000_000
            ),
            None => String::new(),
        }
    );
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

fn clones_text() -> String {
    let mut s = String::new();
    let mounts = CLONE_MOUNTS.lock().clone();
    if let Ok(list) = clones() {
        for c in list {
            let at = mounts
                .iter()
                .find(|(n, _)| *n == c.name)
                .map(|(_, p)| p.clone())
                .unwrap_or_else(|| String::from("-"));
            let _ = writeln!(s, "{} of={} at={at}", c.name, c.origin);
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
    dir.add_control("auto", timemachine::status, |b| {
        timemachine::control(arg(b)?)
    });
    dir.add_file("clones", clones_text);
    dir.add_control("clone", clones_text, |b| {
        let mut w = arg(b)?.split_whitespace();
        let (Some(snap), Some(name), None) = (w.next(), w.next(), w.next()) else {
            return Err(Errno::EINVAL);
        };
        clone(snap, name).map(|_| ())
    });
    dir.add_control("discard", clones_text, |b| discard_clone(arg(b)?));
    dir.add_control("promote", clones_text, |b| {
        promote_clone(arg(b)?).map(|_| ())
    });
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
