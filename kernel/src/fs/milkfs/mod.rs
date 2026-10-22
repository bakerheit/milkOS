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
pub mod convert;
pub mod crashtest;
pub mod crypt;
pub mod diff;
pub mod format;
pub mod fsck;
pub mod inode;
pub mod items;
pub mod scrub;
pub mod snap;
pub mod store;
pub mod stream;
pub mod timemachine;
pub mod versions;
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
    mkfs_check(dev, label, force)?;
    let r = volume::mkfs(dev, label);
    audit_op(
        "milkfs.mkfs",
        &format!("dev={} label={label} encrypted=no", dev.name()),
        &r,
    );
    r
}

/// What `mkfs` refuses: a bad label (EINVAL), a mounted volume's device (EBUSY), a disk
/// holding milkfs without `force` (EEXIST).
fn mkfs_check(dev: &BlockRef, label: &str, force: bool) -> Result<()> {
    if !label_ok(label) {
        return fail(
            Errno::EINVAL,
            format!(
                "labels are 1-{} bytes of UTF-8, without '/' or control characters, not starting with '.'",
                format::LABEL_MAX
            ),
        );
    }
    let mounted = |v: &Volume| v.device_name == dev.name();
    if PRIMARY.lock().as_deref().is_some_and(mounted)
        || TEST_OVERRIDE.lock().as_deref().is_some_and(mounted)
        || all_volumes().iter().any(|(v, _)| mounted(v))
    {
        return fail(
            Errno::EBUSY,
            format!("{} is mounted: lock or unmount it first", dev.name()),
        );
    }
    if probe(dev).is_some() && !force {
        return Err(Errno::EEXIST);
    }
    Ok(())
}

/// A volume label (it becomes `/Volumes/<label>`): 1 to `LABEL_MAX` bytes of UTF-8 (so a
/// multi-byte character counts for more than one), no `/`, no control characters, not
/// starting with `.` (that would be a hidden folder, and rules out `.` and `..`). Spaces
/// and other punctuation are fine.
pub fn label_ok(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= format::LABEL_MAX
        && !label.starts_with('.')
        && !label.chars().any(|c| c == '/' || c.is_control())
}

/// `mkfs` an encrypted volume on `dev`, under `passphrase` (8 characters or more). With
/// `recovery_out`, also a recovery slot, whose key is written (once, mode 0600) to that
/// file; without, there is no recovery key. The new volume is left locked: `unlock`
/// mounts it. The passphrase is never logged.
pub fn mkfs_encrypted(
    dev: &BlockRef,
    label: &str,
    force: bool,
    passphrase: &str,
    recovery_out: Option<&str>,
) -> Result<()> {
    use crate::fs::abi::{O_CREAT, O_EXCL, O_WRONLY};
    if passphrase.len() < 8 {
        return fail(Errno::EINVAL, "the passphrase needs 8 characters or more");
    }
    mkfs_check(dev, label, force)?;
    // The recovery file first: if it can't be made, nothing is formatted.
    let out = match recovery_out {
        Some(p) => match crate::fs::ops::open(p, O_WRONLY | O_CREAT | O_EXCL, 0o600) {
            Ok(f) => Some(f),
            Err(e) => {
                return fail(e, format!("can't create the recovery key file {p}: {e}"));
            }
        },
        None => None,
    };
    let r = (|| {
        let key = volume::mkfs_encrypted(dev, label, passphrase, out.is_some())?;
        if let (Some(f), Some(k)) = (&out, key) {
            let mut text = format!("{k}\n").into_bytes();
            let w = f.write(&text).and_then(|_| f.fsync());
            crate::crypto::wipe(&mut text);
            w?;
        }
        LOCKED.lock().retain(|d| d.name() != dev.name());
        LOCKED.lock().push(dev.clone());
        Ok(())
    })();
    if r.is_err()
        && let Some(p) = recovery_out
    {
        let _ = crate::fs::ops::unlink(p);
    }
    audit_op(
        "milkfs.mkfs",
        &format!(
            "dev={} label={label} encrypted=yes recovery={}",
            dev.name(),
            recovery_out.is_some()
        ),
        &r,
    );
    r
}

/// Open the milkfs on `dev` and mount it at `path` (created if missing). The first
/// volume mounted with this becomes the primary one (snapshot API, `/sys/fs/milkfs`).
pub fn mount(dev: BlockRef, path: &str) -> Result<(Arc<Volume>, MountReport)> {
    mount_keyed(dev, path, None, true)
}

/// Mount, with the volume key for an encrypted volume; `primary`: become the primary
/// volume if there is none yet.
fn mount_keyed(
    dev: BlockRef,
    path: &str,
    key: Option<Arc<crypt::VolumeKey>>,
    primary: bool,
) -> Result<(Arc<Volume>, MountReport)> {
    let r = mount_secondary_with(dev, path, key)?;
    if !primary {
        return Ok(r);
    }
    let mut p = PRIMARY.lock();
    if p.is_none() {
        *p = Some(r.0.clone());
        *PRIMARY_PATH.lock() = String::from(path);
    }
    Ok(r)
}

/// Mount without ever becoming the primary volume (self-test volumes, backup disks).
pub fn mount_secondary(dev: BlockRef, path: &str) -> Result<(Arc<Volume>, MountReport)> {
    mount_secondary_with(dev, path, None)
}

fn mount_secondary_with(
    dev: BlockRef,
    path: &str,
    key: Option<Arc<crypt::VolumeKey>>,
) -> Result<(Arc<Volume>, MountReport)> {
    let name = String::from(dev.name());
    let (state, report) = volume::State::open_with(dev, key)?;
    let vol = Volume::new(state, &name);
    crate::fs::ops::mkdir_all(path, 0o755)?;
    crate::fs::path::mount(path, vol.clone())?;
    let mut all = VOLUMES.lock();
    all.retain(|(w, _)| w.strong_count() > 0);
    all.push((Arc::downgrade(&vol), String::from(path)));
    Ok((vol, report))
}

/// Every mounted milkfs volume and where (for the flusher and the scrub).
#[allow(clippy::type_complexity)]
static VOLUMES: SpinLock<alloc::vec::Vec<(alloc::sync::Weak<Volume>, String)>> =
    SpinLock::new(alloc::vec::Vec::new());

pub(crate) fn all_volumes() -> alloc::vec::Vec<(Arc<Volume>, String)> {
    VOLUMES
        .lock()
        .iter()
        .filter_map(|(w, p)| w.upgrade().map(|v| (v, p.clone())))
        .collect()
}

/// Mount every disk that holds milkfs at `/Volumes/<label>` (`/Volumes/<label>-<dev>`
/// if that is taken). The primary volume (home, Time Machine, the snapshot API) is the
/// one labelled `Milk`, or the first found if none is (qemu's disk order isn't the
/// command line's). Encrypted volumes stay unmounted ("locked") until `unlock`; if the
/// primary one is locked, no other disk takes its place. Never formats anything.
pub fn automount() {
    let mut found: alloc::vec::Vec<(BlockRef, format::Superblock)> = blockdev::devices()
        .into_iter()
        .filter_map(|d| probe(&d).map(|sb| (d, sb)))
        .collect();
    if let Some(i) = found.iter().position(|(_, sb)| sb.label == "Milk") {
        let p = found.remove(i);
        found.insert(0, p);
    }
    let test_pass = test_passphrase();
    let mut primary_done = false;
    for (dev, sb) in found {
        let key = if format::needs_key(sb.flags) {
            match test_pass.as_deref().map(|p| unlock_key(&dev, &sb, p)) {
                Some(Ok(k)) => Some(k),
                other => {
                    if let Some(Err(e)) = other {
                        kprintln!(
                            "milkfs: {}: the test passphrase didn't unlock it: {e}",
                            dev.name()
                        );
                    }
                    kprintln!(
                        "milkfs: {} ({}) is encrypted and locked: `snap unlock` (or write the passphrase to /sys/fs/milkfs/unlock)",
                        dev.name(),
                        sb.label
                    );
                    LOCKED.lock().push(dev);
                    primary_done = true; // keep the primary role for it
                    continue;
                }
            }
        } else {
            None
        };
        let primary = !primary_done;
        primary_done = true;
        mount_found(dev, &sb, key, primary);
    }
}

/// Mount a volume found on `dev` (boot or unlock), logging the result; for the primary
/// volume also its earlier clones. Returns the mount path.
fn mount_found(
    dev: BlockRef,
    sb: &format::Superblock,
    key: Option<Arc<crypt::VolumeKey>>,
    primary: bool,
) -> Option<String> {
    let label = if !label_ok(&sb.label) {
        String::from("Milk")
    } else {
        sb.label.clone()
    };
    let mut path = format!("/Volumes/{label}");
    if !primary {
        if crate::fs::ops::stat(&path).is_ok() {
            path = format!("/Volumes/{label}-{}", dev.name());
        }
        return match mount_keyed(dev.clone(), &path, key, false) {
            Ok((_, r)) => {
                kprintln!(
                    "milkfs: {} mounted at {path} in {}.{:02} ms: generation {}, {} blocks used",
                    dev.name(),
                    r.mount_ns / 1_000_000,
                    r.mount_ns / 10_000 % 100,
                    r.generation,
                    r.used_blocks
                );
                Some(path)
            }
            Err(e) => {
                kprintln!("milkfs: mounting {} failed: {e}", dev.name());
                None
            }
        };
    }
    match mount_keyed(crashtest::wrap(dev.clone()), &path, key, true) {
        Ok((vol, r)) => {
            let st = vol.state.lock();
            kprintln!(
                "milkfs: {} mounted at {path} in {}.{:02} ms: generation {}, {}, {}/{} blocks used, {} snapshot(s), check {}{}{}",
                dev.name(),
                r.mount_ns / 1_000_000,
                r.mount_ns / 10_000 % 100,
                r.generation,
                if r.fast {
                    String::from("space map current")
                } else {
                    format!("{} inodes", r.inodes)
                },
                r.used_blocks,
                st.store.total(),
                st.snaps.len(),
                if r.fast {
                    "skipped (snap check)"
                } else if r.problems == 0 {
                    "clean"
                } else {
                    "FOUND PROBLEMS"
                },
                if r.orphans > 0 {
                    format!(", {} orphan(s) reclaimed", r.orphans)
                } else {
                    String::new()
                },
                if st.store.encrypted() {
                    ", encrypted"
                } else {
                    ""
                }
            );
        }
        Err(e) => {
            kprintln!("milkfs: mounting {} failed: {e}", dev.name());
            return None;
        }
    }
    // Clones made on an earlier boot (not the hidden ones of a receive).
    if let Some(v) = user_volume() {
        let list = v.state.lock().clone_list();
        for c in list.into_iter().filter(|c| !c.name.starts_with('.')) {
            match mount_clone(&v, &c.name, c.id) {
                Ok(p) => kprintln!("milkfs: clone {} (of {}) mounted at {p}", c.name, c.origin),
                Err(e) => kprintln!("milkfs: clone {}: {e}", c.name),
            }
        }
    }
    Some(path)
}

// ---- encrypted volumes: unlock, change passphrase ---------------------------------

/// Encrypted volumes found at boot and not unlocked yet.
static LOCKED: SpinLock<alloc::vec::Vec<BlockRef>> = SpinLock::new(alloc::vec::Vec::new());

/// TEST ONLY: a passphrase handed in by qemu (`-fw_cfg name=opt/milkos/passphrase,...`),
/// so test scripts (crashtest.sh on an encrypted volume) can boot unattended. Anyone
/// who can start qemu can read it; real use goes through `snap unlock`.
pub(crate) fn test_passphrase() -> Option<String> {
    let f = crate::fwcfg::find_file("opt/milkos/passphrase")?;
    let mut buf = alloc::vec![0u8; f.size as usize];
    if !crate::fwcfg::read_file(f, &mut buf) {
        return None;
    }
    let s = String::from_utf8_lossy(&buf)
        .trim_end_matches('\0')
        .trim()
        .into();
    crate::crypto::wipe(&mut buf);
    Some(s)
}

/// The keyring block of `sb` on `dev`, read straight from the disk (before any State).
fn read_keyring(dev: &BlockRef, sb: &format::Superblock) -> Result<format::Keyring> {
    let per = (format::BLOCK / dev.block_size()) as u64;
    let mut b = [0u8; format::BLOCK];
    dev.read_blocks(sb.keyring.block * per, &mut b)?;
    if sb.keyring.is_none() || format::xxh64(&b) != sb.keyring.csum {
        return Err(Errno::EIO);
    }
    format::Keyring::decode(&b).ok_or(Errno::EIO)
}

/// The volume key of the encrypted volume on `dev`, from a passphrase or the recovery
/// key. EACCES if `secret` opens no slot.
fn unlock_key(
    dev: &BlockRef,
    sb: &format::Superblock,
    secret: &str,
) -> Result<Arc<crypt::VolumeKey>> {
    let ring = read_keyring(dev, sb)?;
    if ring.uuid != sb.uuid {
        return Err(Errno::EIO);
    }
    let t0 = crate::timer::now_ns();
    let r = crypt::unlock(&ring, secret);
    kprintln!(
        "milkfs: {}: unlock {} ({} ms)",
        dev.name(),
        if r.is_ok() { "ok" } else { "refused" },
        (crate::timer::now_ns() - t0) / 1_000_000
    );
    r.map(|(k, _)| Arc::new(k))
}

/// Self-test: mount the encrypted volume on `dev` at `path`, unlocked with `secret`
/// (the recovery key is fast: no scrypt), never as the primary volume.
pub(crate) fn mount_encrypted_secondary(
    dev: BlockRef,
    path: &str,
    secret: &str,
) -> Result<(Arc<Volume>, MountReport)> {
    let sb = probe(&dev).ok_or(Errno::EINVAL)?;
    let key = unlock_key(&dev, &sb, secret)?;
    mount_secondary_with(dev, path, Some(key))
}

/// Encrypted volumes still locked: (device, label).
pub fn locked() -> alloc::vec::Vec<(String, String)> {
    LOCKED
        .lock()
        .iter()
        .map(|d| {
            (
                String::from(d.name()),
                probe(d).map(|sb| sb.label).unwrap_or_default(),
            )
        })
        .collect()
}

/// Unlock and mount locked encrypted volumes with `secret` (a passphrase or the
/// recovery key): the one on `dev` or every one it opens. Returns where they were
/// mounted. EACCES if it opens none, ENODEV if nothing is locked. If it was the
/// primary volume, home, /Apps and its clones are set up as at boot.
pub fn unlock(secret: &str, dev: Option<&str>) -> Result<alloc::vec::Vec<String>> {
    let cands: alloc::vec::Vec<BlockRef> = LOCKED
        .lock()
        .iter()
        .filter(|d| dev.is_none_or(|n| d.name() == n))
        .cloned()
        .collect();
    if cands.is_empty() {
        return Err(Errno::ENODEV);
    }
    let mut mounted = alloc::vec::Vec::new();
    let mut last = Errno::EACCES;
    for d in cands {
        let Some(sb) = probe(&d) else { continue };
        let key = match unlock_key(&d, &sb, secret) {
            Ok(k) => k,
            Err(e) => {
                last = e;
                continue;
            }
        };
        LOCKED.lock().retain(|x| x.name() != d.name());
        let primary = user_volume().is_none();
        if let Some(p) = mount_found(d.clone(), &sb, Some(key), primary) {
            if primary && user_volume().is_some() {
                setup_home();
                setup_applications();
                crate::fs::notify::rescan(0); // home moved under the indexer's feet
            }
            mounted.push(p);
        }
    }
    convert_kick();
    let ok = !mounted.is_empty();
    audit_op(
        "milkfs.unlock",
        &format!("dev={} mounted={}", dev.unwrap_or("-"), mounted.join(",")),
        &if ok { Ok(()) } else { Err(last) },
    );
    if ok { Ok(mounted) } else { Err(last) }
}

/// Processes holding something on one of `devs` (open files, working or root
/// directory), e.g. `pid 12 (cat): 1 open file`.
fn holders(devs: &[u64]) -> alloc::vec::Vec<String> {
    let mut out = alloc::vec::Vec::new();
    for p in crate::proc::all() {
        let inner = p.inner.lock();
        let files = inner
            .fds
            .list()
            .iter()
            .filter(|(_, f)| devs.contains(&f.inode().dev()))
            .count();
        let mut what = alloc::vec::Vec::new();
        if files > 0 {
            what.push(format!(
                "{files} open file{}",
                if files == 1 { "" } else { "s" }
            ));
        }
        if devs.contains(&inner.cwd.dev()) {
            what.push(String::from("working directory"));
        }
        if inner
            .root_dir
            .as_ref()
            .is_some_and(|r| devs.contains(&r.dev()))
        {
            what.push(String::from("root directory"));
        }
        if !what.is_empty() {
            out.push(format!(
                "pid {} ({}): {}",
                p.pid,
                inner.name,
                what.join(", ")
            ));
        }
    }
    out
}

/// `statfs` for a device number of a milkfs view that isn't mounted itself (a snapshot
/// under `.snapshots`).
pub fn statfs_dev(dev: u64) -> Option<crate::fs::node::StatFs> {
    all_volumes()
        .into_iter()
        .find_map(|(v, _)| v.view_of_dev(dev).map(|view| v.statfs_view(view)))
}

/// Is `t` (a device name, mount path or label) this volume?
fn names_volume(v: &Volume, path: &str, t: &str) -> bool {
    v.device_name == t || path == t || v.label == t
}

/// Lock unlocked encrypted volumes again: the one `target` names (device, mount path or
/// label) or, with `None`, every one that can be. Commits, unmounts the live tree,
/// drops its cached plaintext and wipes the key; the device goes back on the locked
/// list for `unlock`. Refused:
/// * EBUSY if a process has a file open on it or works in it (the reason names them),
///   something is mounted inside it, or a scrub, send or receive is using it;
/// * EBUSY for the primary volume: `/Users/milk`, `/Apps` and `/System/Settings/milk/drop`
///   are bound from it (you can't lock the disk you're logged into; it locks at
///   shutdown);
/// * EINVAL if it isn't encrypted; ENODEV if no such (unlocked) volume.
///
/// Returns the devices locked.
pub fn lock(target: Option<&str>) -> Result<alloc::vec::Vec<String>> {
    let home = user_volume();
    let vols = all_volumes();
    let picked: alloc::vec::Vec<(Arc<Volume>, String)> = match target {
        Some(t) => {
            let v: alloc::vec::Vec<_> = vols
                .into_iter()
                .filter(|(v, p)| names_volume(v, p, t))
                .collect();
            if v.is_empty() {
                if LOCKED.lock().iter().any(|d| d.name() == t) {
                    return fail(Errno::EINVAL, format!("{t} is already locked"));
                }
                return fail(Errno::ENODEV, format!("no mounted milkfs volume {t}"));
            }
            v
        }
        None => {
            let v: alloc::vec::Vec<_> = vols
                .into_iter()
                .filter(|(v, _)| {
                    v.state.lock().store.encrypted()
                        && !home.as_ref().is_some_and(|h| Arc::ptr_eq(h, v))
                })
                .collect();
            if v.is_empty() {
                if home
                    .as_ref()
                    .is_some_and(|h| h.state.lock().store.encrypted())
                {
                    return fail(Errno::EBUSY, home_reason(home.as_ref().unwrap()));
                }
                return fail(Errno::ENODEV, "no unlocked encrypted volume");
            }
            v
        }
    };
    let mut done = alloc::vec::Vec::new();
    let mut first_err = None;
    for (v, path) in picked {
        let r = lock_one(&v, &path, home.as_ref());
        audit_op(
            "milkfs.lock",
            &format!("dev={} path={path}", v.device_name),
            &r,
        );
        match r {
            Ok(()) => done.push(v.device_name.clone()),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    match first_err {
        Some(e) if done.is_empty() => Err(e),
        _ => Ok(done),
    }
}

fn home_reason(v: &Volume) -> String {
    format!(
        "{} ({}) holds {HOME}, {APPLICATIONS} and the system's settings: you can't lock the volume you're logged into (it locks at shutdown)",
        v.device_name, v.label
    )
}

fn lock_one(v: &Arc<Volume>, path: &str, home: Option<&Arc<Volume>>) -> Result<()> {
    let dev = v.device_name.clone();
    if home.is_some_and(|h| Arc::ptr_eq(h, v)) {
        return fail(Errno::EBUSY, home_reason(v));
    }
    {
        let mut st = v.state.lock();
        st.not_converting()?;
        if !st.store.encrypted() {
            return fail(
                Errno::EINVAL,
                format!("{dev} ({}) isn't encrypted: nothing to lock", v.label),
            );
        }
        if st.receiving || !st.busy_snaps.is_empty() {
            return fail(
                Errno::EBUSY,
                format!("{dev}: a send or receive is using it; try again when it finishes"),
            );
        }
        if scrub::running_on(path) {
            return fail(
                Errno::EBUSY,
                format!("{dev}: a scrub is reading it (`snap scrub stop`, or wait)"),
            );
        }
        if let Err(e) = st.commit() {
            return fail(e, format!("{dev}: can't write its last changes: {e}"));
        }
    }
    // Unmount first, so nothing new can be opened by path; put it back if it's busy.
    if crate::fs::path::unmount(path).is_err() {
        return fail(
            Errno::EBUSY,
            format!("{dev}: something is mounted inside {path}; unmount it first"),
        );
    }
    let devs = v.all_devs();
    let who = holders(&devs);
    let held = v.held_objects().len();
    if !who.is_empty() || held > 0 {
        if let Err(e) = crate::fs::path::mount(path, v.clone()) {
            kprintln!("milkfs: {dev}: lock refused, but remounting {path} failed: {e}");
        }
        let why = if who.is_empty() {
            format!("{held} file(s) on it are held by the system")
        } else {
            who.join("; ")
        };
        return fail(
            Errno::EBUSY,
            format!("{dev} ({}) is in use: {why}", v.label),
        );
    }
    let bdev = blockdev::get(&dev);
    let wiped = {
        let mut st = v.state.lock();
        let _ = st.commit(); // nothing can have changed, but be sure
        st.forget_key()
    };
    for d in &devs {
        crate::fs::pagecache::invalidate_dev(*d);
    }
    VOLUMES
        .lock()
        .retain(|(w, _)| !core::ptr::eq(w.as_ptr(), Arc::as_ptr(v)));
    let _ = crate::fs::ops::rmdir(path);
    if let Some(b) = bdev {
        LOCKED.lock().push(b);
    }
    kprintln!(
        "milkfs: {dev} ({}) locked, {path} unmounted, key {}",
        v.label,
        if wiped {
            "wiped"
        } else {
            "wiped once its last reader finishes"
        }
    );
    Ok(())
}

/// Change a passphrase of the primary (encrypted) volume: `old` (a passphrase or the
/// recovery key) must open a slot; the volume key is rewrapped under `new` (no block is
/// rewritten). Replaces the slot `old` opened, or the first passphrase slot if `old`
/// was the recovery key.
pub fn change_passphrase(old: &str, new: &str) -> Result<()> {
    if new.len() < 8 {
        return fail(
            Errno::EINVAL,
            "the new passphrase needs 8 characters or more",
        );
    }
    let v = primary()?;
    if !v.state.lock().store.encrypted() {
        return fail(
            Errno::EINVAL,
            format!("{} isn't encrypted: it has no passphrase", v.label),
        );
    }
    let r = v.state.lock().rewrap(old, new);
    audit_op("milkfs.passwd", "", &r);
    r
}

// ---- encrypting / decrypting in place (convert.rs) --------------------------------

/// The mounted volume named by `dev` (a device name, mount path or label), or the
/// primary one.
fn volume_named(dev: Option<&str>) -> Result<Arc<Volume>> {
    match dev {
        None => primary(),
        Some(t) => all_volumes()
            .into_iter()
            .find(|(v, p)| names_volume(v, p, t))
            .map(|(v, _)| v)
            .map_or_else(
                || fail(Errno::ENODEV, format!("no mounted milkfs volume {t}")),
                Ok,
            ),
    }
}

/// Conversion speed limit in MB/s (0: as fast as it goes).
static CONVERT_RATE_MB: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static CONVERT_THREAD: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static THREADS_STARTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// The last conversion that finished or failed (for `/sys/fs/milkfs/encrypt`).
static LAST_CONVERT: SpinLock<String> = SpinLock::new(String::new());

/// Make sure the conversion thread runs if any mounted volume is being converted (after
/// a start, an unlock, or at boot: a conversion cut short by a crash resumes).
pub fn convert_kick() {
    use core::sync::atomic::Ordering;
    if !THREADS_STARTED.load(Ordering::Acquire)
        || crashtest::mode() == Some(crashtest::Mode::Check)
        || !all_volumes()
            .iter()
            .any(|(v, _)| v.state.lock().converting())
        || CONVERT_THREAD.swap(true, Ordering::AcqRel)
    {
        return;
    }
    drop(crate::sched::spawn_prio(
        "milkfs-convert",
        crate::sched::Priority::Low,
        convert_thread,
    ));
}

fn convert_thread() {
    use core::sync::atomic::Ordering;
    const BATCH: u64 = 64;
    loop {
        let vols: alloc::vec::Vec<_> = all_volumes()
            .into_iter()
            .filter(|(v, _)| v.state.lock().converting())
            .collect();
        if vols.is_empty() {
            CONVERT_THREAD.store(false, Ordering::Release);
            // A start that raced with this exit kicks again; if it didn't, we're done.
            if all_volumes()
                .iter()
                .any(|(v, _)| v.state.lock().converting())
                && !CONVERT_THREAD.swap(true, Ordering::AcqRel)
            {
                continue;
            }
            return;
        }
        for (v, path) in vols {
            let t0 = crate::timer::now_ns();
            let r = v.state.lock().convert_step(BATCH);
            match r {
                Ok(convert::Step::More) => {
                    let rate = CONVERT_RATE_MB.load(Ordering::Relaxed) as u64;
                    // BATCH blocks of 4 KiB at `rate` MB/s.
                    if let Some(want_ns) = (BATCH * 4096 * 1000).checked_div(rate) {
                        let spent = crate::timer::now_ns() - t0;
                        if want_ns > spent {
                            crate::sched::sleep_ms((want_ns - spent) / 1_000_000);
                        }
                    } else {
                        crate::sched::yield_now();
                    }
                }
                Ok(convert::Step::Done) => {
                    let enc = v.state.lock().store.encrypted();
                    let text = format!(
                        "{} ({}) at {path}: {} in place, done\n",
                        v.device_name,
                        v.label,
                        if enc { "encrypted" } else { "decrypted" }
                    );
                    kprintln!("milkfs: {}", text.trim_end());
                    *LAST_CONVERT.lock() = text;
                    crate::fs::pagecache::invalidate_dev(v.dev);
                    audit_op(
                        "milkfs.convert.done",
                        &format!("dev={} encrypted={enc}", v.device_name),
                        &Ok(()),
                    );
                }
                Ok(convert::Step::NeedSpace(_)) => crate::sched::sleep_ms(2000),
                Err(e) => {
                    *LAST_CONVERT.lock() = format!(
                        "{} ({}): conversion stopped: {e} (it resumes at the next mount)\n",
                        v.device_name, v.label
                    );
                    kprintln!("milkfs: {}: conversion stopped: {e}", v.device_name);
                    crate::sched::sleep_ms(5000);
                    if v.state.lock().broken {
                        return;
                    }
                }
            }
        }
    }
}

/// Encrypt the volume `dev` names (default the primary one) in place under `passphrase`
/// (8 characters or more), in the background; with `recovery_out` also a recovery key,
/// written there (O_EXCL, 0600). The volume stays usable; from the first commit on it
/// needs the passphrase to mount. Progress: `info`, `/sys/fs/milkfs/encrypt`.
pub fn encrypt_in_place(
    dev: Option<&str>,
    passphrase: &str,
    recovery_out: Option<&str>,
) -> Result<()> {
    use crate::fs::abi::{O_CREAT, O_EXCL, O_WRONLY};
    if passphrase.len() < 8 {
        return fail(Errno::EINVAL, "the passphrase needs 8 characters or more");
    }
    let v = volume_named(dev)?;
    if scrub::running() {
        return fail(
            Errno::EBUSY,
            "a scrub is reading the volume: wait or `snap scrub stop`",
        );
    }
    let uuid = {
        let st = v.state.lock();
        if st.converting() {
            return fail(Errno::EBUSY, "it is being converted already");
        }
        if st.store.crypt.is_some() {
            return fail(Errno::EINVAL, format!("{} is encrypted already", v.label));
        }
        st.sb.uuid
    };
    let out = match recovery_out {
        Some(p) => match crate::fs::ops::open(p, O_WRONLY | O_CREAT | O_EXCL, 0o600) {
            Ok(f) => Some(f),
            Err(e) => return fail(e, format!("can't create the recovery key file {p}: {e}")),
        },
        None => None,
    };
    let r = (|| {
        // The key and its slots first (scrypt, slow), without the volume lock.
        let mut vk = [0u8; 32];
        crate::crypto::rng::fill(&mut vk);
        let key = crypt::VolumeKey::from_master(&vk);
        crate::crypto::wipe(&mut vk);
        let mut ring = format::Keyring {
            uuid,
            slots: alloc::vec![crypt::wrap(
                &uuid,
                &key,
                format::SLOT_PASSPHRASE,
                passphrase.as_bytes()
            )?],
        };
        let mut shown = None;
        if out.is_some() {
            let mut rk = [0u8; 16];
            crate::crypto::rng::fill(&mut rk);
            ring.slots
                .push(crypt::wrap(&uuid, &key, format::SLOT_RECOVERY, &rk)?);
            shown = Some(crypt::format_recovery(&rk));
            crate::crypto::wipe(&mut rk);
        }
        v.state.lock().convert_encrypt_begin(key, &ring)?;
        if let (Some(f), Some(k)) = (&out, shown) {
            let mut text = format!("{k}\n").into_bytes();
            let w = f.write(&text).and_then(|_| f.fsync());
            crate::crypto::wipe(&mut text);
            w?;
        }
        Ok(())
    })();
    if r.is_err()
        && let Some(p) = recovery_out
    {
        let _ = crate::fs::ops::unlink(p);
    }
    audit_op(
        "milkfs.encrypt",
        &format!("dev={} recovery={}", v.device_name, recovery_out.is_some()),
        &r,
    );
    if r.is_ok() {
        LAST_CONVERT.lock().clear();
        kprintln!(
            "milkfs: {} ({}): encrypting in place",
            v.device_name,
            v.label
        );
        convert_kick();
    }
    r
}

/// Decrypt the encrypted volume `dev` names (default the primary one) in place, in the
/// background; `secret` (a passphrase or the recovery key) must open it (EACCES).
pub fn decrypt_in_place(dev: Option<&str>, secret: &str) -> Result<()> {
    let v = volume_named(dev)?;
    if scrub::running() {
        return fail(
            Errno::EBUSY,
            "a scrub is reading the volume: wait or `snap scrub stop`",
        );
    }
    let r = v.state.lock().convert_decrypt_begin(secret);
    audit_op("milkfs.decrypt", &format!("dev={}", v.device_name), &r);
    if r.is_ok() {
        LAST_CONVERT.lock().clear();
        kprintln!(
            "milkfs: {} ({}): decrypting in place",
            v.device_name,
            v.label
        );
        convert_kick();
    }
    r
}

/// `/sys/fs/milkfs/encrypt` (and `decrypt`) read: each converting volume's progress, or
/// the last result.
fn convert_status() -> String {
    let mut s = String::new();
    for (v, p) in all_volumes() {
        if let Some(t) = v.state.lock().convert_text() {
            let _ = writeln!(s, "{} ({}) at {p}: converting: {t}", v.device_name, v.label);
        }
    }
    if s.is_empty() {
        s = LAST_CONVERT.lock().clone();
    }
    let r = CONVERT_RATE_MB.load(core::sync::atomic::Ordering::Relaxed);
    if r > 0 {
        let _ = writeln!(s, "rate limit: {r} MB/s");
    }
    s
}

/// Where the primary volume is mounted (`/Volumes/<label>`).
static PRIMARY_PATH: SpinLock<String> = SpinLock::new(String::new());

pub fn mount_path() -> Option<String> {
    let p = PRIMARY_PATH.lock().clone();
    (!p.is_empty()).then_some(p)
}

/// The user's home directory on the primary volume.
pub const HOME: &str = "/Users/milk";

/// Persistent home: bind `<volume>/Users/milk` onto `/Users/milk` (seeding it from the
/// initramfs copy the first time). Without a volume, `/Users/milk` stays on tmpfs. Runs
/// in `fs::init`, before the desktop creates its folders in the home directory. First,
/// a volume with the old layout (`home/milk`, `Applications`, `etc/milk`) is moved over
/// (`fs::layout::migrate`).
pub fn setup_home() {
    let Some(vol) = mount_path() else { return };
    crate::fs::layout::migrate(&vol);
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

// ---- diff ---------------------------------------------------------------------------

/// What changed from snapshot `a` to snapshot `b` (or the live volume when `b` is `None`
/// or `"live"`). The order of `a` and `b` doesn't matter: older -> newer.
pub fn diff(a: &str, b: Option<&str>) -> Result<diff::DiffResult> {
    primary()?.state.lock().diff(a, b)
}

static LAST_DIFF: SpinLock<String> = SpinLock::new(String::new());

fn diff_control(arg: &str) -> Result<()> {
    let mut w = arg.split_whitespace();
    let a = w.next().ok_or(Errno::EINVAL)?;
    let b = w.next();
    if w.next().is_some() {
        return Err(Errno::EINVAL);
    }
    let r = diff(a, b)?;
    let mut s = format!(
        "diff {a}..{}: {} change(s) ({} tree nodes read, {} unchanged subtrees skipped)\n",
        b.unwrap_or("live"),
        r.changes.len(),
        r.nodes_read,
        r.subtrees_skipped
    );
    for c in &r.changes {
        s.push_str(&c.line());
        s.push('\n');
    }
    *LAST_DIFF.lock() = s;
    Ok(())
}

/// Persistent machine settings under `/System/Settings` (Milk Drop's identity and trusted
/// peers, `/System/Settings/milk/drop`; the hostname, `/System/Settings/milk/net`): bind
/// `<volume><dir>` onto `<dir>`, seeded from the initramfs copy the first time, like the
/// home directory. Without a volume it stays in RAM (a new identity every boot).
pub fn setup_persistent_etc(dir: &str) {
    let Some(vol) = mount_path() else { return };
    let on_disk = format!("{vol}{dir}");
    if crate::fs::ops::stat(&on_disk).is_err() {
        let r = if crate::fs::ops::stat(dir).is_ok() {
            crate::fs::ops::copy_tree(dir, &on_disk)
        } else {
            crate::fs::ops::mkdir_all(&on_disk, 0o700)
        };
        if let Err(e) = r {
            kprintln!("milkfs: can't create {on_disk}: {e}; {dir} stays in RAM");
            return;
        }
    }
    if crate::fs::ops::stat(dir).is_err() {
        let _ = crate::fs::ops::mkdir_all(dir, 0o700);
    }
    if let Err(e) = crate::fs::path::bind(&on_disk, dir) {
        kprintln!("milkfs: bind {on_disk} -> {dir} failed: {e}");
    }
}

/// Where apps live: the built-in ones and the packages `milk install` adds.
pub const APPLICATIONS: &str = crate::fs::layout::APPS;

/// Persistent apps: bind `<volume>/Apps` onto `/Apps` (created the first time), so
/// installed packages survive reboots and are part of the volume's snapshots and clones
/// (`milk try`). The built-in apps are copied in first (`fs::layout::sync_builtin_apps`),
/// so both kinds are in the one folder. Without a volume it stays on tmpfs (built-in apps
/// only, from the initramfs).
pub fn setup_applications() {
    let Some(vol) = mount_path() else { return };
    let on_disk = format!("{vol}{APPLICATIONS}");
    if crate::fs::ops::stat(&on_disk).is_err()
        && let Err(e) = crate::fs::ops::mkdir_all(&on_disk, 0o755)
    {
        kprintln!("milkfs: can't create {on_disk}: {e}; {APPLICATIONS} stays in RAM");
        return;
    }
    crate::fs::layout::sync_builtin_apps(&vol, &on_disk);
    if let Err(e) = crate::fs::path::bind(&on_disk, APPLICATIONS) {
        kprintln!("milkfs: bind {on_disk} -> {APPLICATIONS} failed: {e}");
    }
}

// ---- versions -----------------------------------------------------------------------

pub use versions::Version;

/// The live file at `path` (any VFS path, e.g. `/Users/milk/Documents/ideas.md`), as
/// (volume, inode number).
fn live_file(path: &str) -> Result<(Arc<Volume>, u64)> {
    let v = primary()?;
    let i = crate::fs::ops::lookup(path)?;
    if i.dev() != v.dev {
        return Err(Errno::EXDEV); // not on the (live) milkfs volume
    }
    if i.kind() != crate::fs::node::InodeType::File {
        return Err(Errno::EISDIR);
    }
    Ok((v, i.ino()))
}

/// The distinct versions of the file at `path` across the volume's snapshots, oldest
/// first; the last entry is the current file (`snapshot: None`).
pub fn versions(path: &str) -> Result<alloc::vec::Vec<Version>> {
    let (v, ino) = live_file(path)?;
    let st = v.state.lock();
    st.versions_of(ino)
}

/// Put the content `path` had in `snapshot` back into the live file. If the current
/// content isn't already kept in a snapshot, a `pre-restore-<time>` snapshot is taken
/// first, so restoring never loses anything.
pub fn restore_version(path: &str, snapshot: &str) -> Result<()> {
    let (v, ino) = live_file(path)?;
    let mount = api_volume_path().ok_or(Errno::ENODEV)?;
    let (src, need_safety) = {
        let st = v.state.lock();
        let s = st
            .snaps
            .iter()
            .find(|s| s.name == snapshot)
            .ok_or(Errno::ENOENT)?;
        let in_snap = match st.inode(s.generation, ino) {
            Ok(_) => Some(ino),
            Err(_) => st
                .path_in(items::LIVE, ino)
                .and_then(|p| st.resolve_in(s.generation, &p)),
        }
        .ok_or(Errno::ENOENT)?;
        let rel = st.path_in(s.generation, in_snap).ok_or(Errno::ENOENT)?;
        let (cur, _) = st.content_hash(items::LIVE, ino)?;
        let kept = st.snaps.iter().any(|s| {
            st.inode(s.generation, ino).is_ok()
                && st
                    .content_hash(s.generation, ino)
                    .is_ok_and(|(h, _)| h == cur)
        });
        (format!("{mount}/.snapshots/{snapshot}{rel}"), !kept)
    };
    if need_safety {
        let name = format!(
            "pre-restore-{}",
            timemachine::stamp(crate::fs::clock::now().sec as u64)
        );
        v.state.lock().snap_create(&name)?;
    }
    let data = crate::fs::ops::read_file(&src)?;
    crate::fs::ops::write_file(path, &data)
}

static LAST_VERSIONS: SpinLock<String> = SpinLock::new(String::new());

fn versions_control(path: &str) -> Result<()> {
    let list = versions(path)?;
    let mut s = format!("# {path}\n");
    for x in list {
        let _ = writeln!(
            s,
            "{}\t{}\t{}\t{}\t{:016x}",
            x.snapshot.as_deref().unwrap_or("current"),
            x.taken,
            x.mtime,
            x.size,
            x.hash
        );
    }
    *LAST_VERSIONS.lock() = s;
    Ok(())
}

// ---- check / repair -----------------------------------------------------------------

/// Check the primary volume; with `repair`, fix what is safe to fix (see `fsck`).
pub fn check(repair: bool) -> Result<fsck::CheckReport> {
    let v = primary()?;
    let r = v.state.lock().check(repair);
    if repair {
        crate::fs::pagecache::invalidate_dev(v.dev);
    }
    r
}

static LAST_CHECK: SpinLock<String> = SpinLock::new(String::new());

// ---- benchmark ----------------------------------------------------------------------

static LAST_BENCH: SpinLock<String> = SpinLock::new(String::new());

/// Create `n` files of `size` bytes in `dir` (on the primary volume), sync, read them
/// back, delete them, sync; report the times, commits and blocks the files took.
pub fn bench_create(dir: &str, n: usize, size: usize) -> Result<String> {
    use crate::fs::ops;
    let v = primary()?;
    let _ = ops::mkdir_all(dir, 0o755);
    crate::fs::sync_all()?;
    let data: alloc::vec::Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let stat = |v: &Volume| {
        let st = v.state.lock();
        (st.commits, st.store.free_blocks())
    };
    let ((c0, f0), t0) = (stat(&v), crate::timer::now_ns());
    for i in 0..n {
        ops::write_file(&format!("{dir}/f{i}"), &data)?;
    }
    crate::fs::sync_all()?;
    let ((c1, f1), t1) = (stat(&v), crate::timer::now_ns());
    crate::fs::pagecache::drop_all();
    let mut ok = 0;
    for i in 0..n {
        if ops::read_file(&format!("{dir}/f{i}")).is_ok_and(|d| d == data) {
            ok += 1;
        }
    }
    let t2 = crate::timer::now_ns();
    for i in 0..n {
        ops::unlink(&format!("{dir}/f{i}"))?;
    }
    let _ = ops::rmdir(dir);
    crate::fs::sync_all()?;
    let ((c3, _), t3) = (stat(&v), crate::timer::now_ns());
    let ms = |a: u64, b: u64| format!("{}.{:01} ms", (b - a) / 1_000_000, (b - a) / 100_000 % 10);
    let r = format!(
        "{n} x {size} B: create {} ({} commits, {} blocks), read {} ({ok} ok), delete {} ({} commits)\n",
        ms(t0, t1),
        c1 - c0,
        f0.saturating_sub(f1),
        ms(t1, t2),
        ms(t2, t3),
        c3 - c1
    );
    kprintln!("milkfs bench: {}", r.trim_end());
    *LAST_BENCH.lock() = r.clone();
    Ok(r)
}

// ---- send / receive -----------------------------------------------------------------

static LAST_SEND: SpinLock<String> = SpinLock::new(String::new());
static LAST_RECEIVE: SpinLock<String> = SpinLock::new(String::new());

/// What a send wrote.
#[derive(Clone, Debug, Default)]
pub struct SendReport {
    pub records: u64,
    pub data_blocks: u64,
    pub bytes: u64,
    pub ms: u64,
    pub encrypted: bool,
}

/// How a send protects its stream.
#[derive(Clone, Copy)]
pub enum StreamCrypt<'a> {
    /// Encrypted under this passphrase (8 characters or more).
    Passphrase(&'a str),
    /// Plaintext, even from an encrypted volume (`plaintext`, `snap send --plaintext`).
    Plain,
    /// Plaintext from a plain volume; refused (EINVAL) from an encrypted one, whose
    /// streams are encrypted unless asked otherwise.
    Default,
}

/// A control write's text and the optional secret on its second line: `(first line,
/// secret)`. The secret is everything after the first newline (trailing CR/LF removed).
fn split_secret(t: &str) -> (&str, Option<&str>) {
    match t.split_once('\n') {
        Some((a, b)) => {
            let b = b.trim_end_matches(['\n', '\r']);
            (a, (!b.is_empty()).then_some(b))
        }
        None => (t, None),
    }
}

/// Write snapshot `snap` of the primary volume as a stream to `out` (a file path),
/// incremental from the older snapshot `from`. The volume isn't locked while the stream
/// is written (it may go onto the volume itself); the snapshots read can't be deleted
/// meanwhile (EBUSY).
pub fn send(snap: &str, from: Option<&str>, out: &str, crypt: StreamCrypt) -> Result<SendReport> {
    use crate::fs::abi::{O_CREAT, O_TRUNC, O_WRONLY};
    use stream::{Encoder, Plan, Record};
    let v = primary()?;
    let pass = match crypt {
        StreamCrypt::Passphrase(p) if p.len() < 8 => {
            return fail(
                Errno::EINVAL,
                "a stream passphrase needs 8 characters or more",
            );
        }
        StreamCrypt::Passphrase(p) => Some(p),
        StreamCrypt::Plain => None,
        StreamCrypt::Default if v.state.lock().store.encrypted() => {
            return fail(
                Errno::EINVAL,
                "the volume is encrypted, so its streams are too: give a passphrase (`snap send` asks), or `plaintext`",
            );
        }
        StreamCrypt::Default => None,
    };
    let t0 = crate::timer::now_ns();
    let plan = {
        let mut st = v.state.lock();
        let p = st.send_plan(snap, from)?;
        st.busy_snaps.extend(p.gens.iter().copied());
        p
    };
    let gens = plan.gens.clone();
    let r = (|| -> Result<SendReport> {
        let mut enc = match pass {
            Some(p) => Encoder::sealed(p.as_bytes())?,
            None => Encoder::default(),
        };
        let f = crate::fs::ops::open(out, O_WRONLY | O_CREAT | O_TRUNC, 0o644)?;
        let write = |enc: &mut Encoder, f: &crate::fs::OpenFile| -> Result<()> {
            let mut b = &enc.buf[..];
            while !b.is_empty() {
                let n = f.write(b)?;
                b = &b[n..];
            }
            enc.buf.clear();
            Ok(())
        };
        enc.record(&Record::Header(plan.header.clone()));
        let mut sent = alloc::collections::BTreeSet::new();
        let mut data = alloc::vec::Vec::new();
        for p in plan.recs {
            match p {
                Plan::Item(k, val) => enc.record(&Record::Item(k, val)),
                Plan::Delete(k) => enc.record(&Record::Delete(k)),
                Plan::Drop(ino, from, to) => enc.record(&Record::Drop { ino, from, to }),
                Plan::Extent { ino, old_end, ext } => {
                    data.resize(ext.csums.len() * format::BLOCK, 0);
                    v.state
                        .lock()
                        .store
                        .read_run(ext.block, ext.birth, &ext.csums, &mut data)?;
                    for (i, chunk) in data.chunks(format::BLOCK).enumerate() {
                        let src = ext.block + i as u64;
                        if sent.insert(src) {
                            enc.record(&Record::Data {
                                src,
                                data: chunk.to_vec(),
                            });
                        }
                    }
                    // The stream carries plaintext and its xxh64 (on an encrypted
                    // volume the pointers hold keyed tags, meaningless elsewhere).
                    let csums = data.chunks(format::BLOCK).map(format::xxh64).collect();
                    enc.record(&Record::Extent {
                        ino,
                        start: ext.start,
                        old_end,
                        src: ext.block,
                        csums,
                    });
                }
            }
            if enc.buf.len() >= 1 << 20 {
                write(&mut enc, &f)?;
            }
        }
        enc.record(&Record::End {
            records: 0,
            data: 0,
            chain: 0,
        });
        write(&mut enc, &f)?;
        f.fsync()?;
        Ok(SendReport {
            records: enc.records + 1,
            data_blocks: enc.data_blocks,
            bytes: enc.bytes,
            ms: (crate::timer::now_ns() - t0) / 1_000_000,
            encrypted: enc.encrypted(),
        })
    })();
    {
        let mut st = v.state.lock();
        for g in gens {
            if let Some(i) = st.busy_snaps.iter().position(|&x| x == g) {
                st.busy_snaps.remove(i);
            }
        }
    }
    let text = match &r {
        Ok(s) => format!(
            "sent {snap}{}{} to {out}: {} records, {} data blocks, {} bytes in {} ms\n",
            from.map(|f| format!(" (incremental from {f})"))
                .unwrap_or_default(),
            if s.encrypted { ", encrypted" } else { "" },
            s.records,
            s.data_blocks,
            s.bytes,
            s.ms
        ),
        Err(e) => format!("send {snap} failed: {e}\n"),
    };
    *LAST_SEND.lock() = text;
    audit_op(
        "milkfs.send",
        &format!(
            "snapshot={snap} from={} out={out} encrypted={}",
            from.unwrap_or("-"),
            pass.is_some()
        ),
        &r,
    );
    r
}

/// What a receive did.
#[derive(Clone, Debug, Default)]
pub struct RecvReport {
    pub snapshot: String,
    pub incremental: bool,
    pub items: u64,
    pub data_blocks: u64,
    pub bytes: u64,
    pub ms: u64,
    pub encrypted: bool,
}

/// The milkfs volume a VFS path is on.
pub fn volume_at(path: &str) -> Result<Arc<Volume>> {
    inode::volume_of(&crate::fs::ops::lookup(path)?).ok_or(Errno::EXDEV)
}

/// Recreate the snapshot in stream file `path` on the milkfs volume at `target` (any
/// path on it; default: the primary volume). Full streams need an empty volume;
/// incremental ones need the base snapshot (ENOENT otherwise) and a live tree unchanged
/// since it (EBUSY; `force`: roll back to it first). Atomic: nothing changes unless the
/// whole stream checks out.
/// An encrypted stream needs its `passphrase` (EACCES without or with a wrong one).
pub fn receive(
    path: &str,
    target: Option<&str>,
    force: bool,
    passphrase: Option<&str>,
) -> Result<RecvReport> {
    use crate::fs::abi::O_RDONLY;
    use stream::{Decoder, Record};
    let v = match target {
        Some(t) => volume_at(t)?,
        None => primary()?,
    };
    let t0 = crate::timer::now_ns();
    let f = crate::fs::ops::open(path, O_RDONLY, 0)?;
    let r = (|| -> Result<RecvReport> {
        let mut rd = Decoder::open(|b: &mut [u8]| f.read(b), passphrase.map(str::as_bytes))?;
        let Record::Header(h) = rd.next()? else {
            return Err(Errno::EINVAL);
        };
        let mut ctx = v.state.lock().recv_begin(&h, force)?;
        let applied = (|| -> Result<()> {
            let mut batch = alloc::vec::Vec::new();
            loop {
                let rec = rd.next()?;
                let end = matches!(rec, Record::End { .. });
                let cut = !matches!(rec, Record::Data { .. });
                if !end {
                    batch.push(rec);
                }
                if end || (cut && batch.len() >= 64) {
                    let mut st = v.state.lock();
                    st.recv_apply(&mut ctx, core::mem::take(&mut batch))?;
                    if ctx.unused.is_empty() {
                        st.op_done()?;
                    }
                }
                if end {
                    return Ok(());
                }
            }
        })();
        let mut st = v.state.lock();
        let (items, blocks) = (ctx.items, ctx.blocks);
        match applied {
            Ok(()) => {
                st.recv_finish(&mut ctx, &h)?;
            }
            Err(e) => {
                st.recv_abort(&ctx);
                return Err(e);
            }
        }
        drop(st);
        crate::fs::pagecache::invalidate_dev(v.dev);
        crate::fs::notify::rescan(v.dev);
        Ok(RecvReport {
            snapshot: h.snap.clone(),
            incremental: h.incremental,
            items,
            data_blocks: blocks,
            bytes: rd.bytes,
            ms: (crate::timer::now_ns() - t0) / 1_000_000,
            encrypted: rd.encrypted(),
        })
    })();
    let text = match &r {
        Ok(s) => format!(
            "received {}{}{} from {path}: {} items, {} data blocks, {} bytes in {} ms\n",
            s.snapshot,
            if s.incremental { " (incremental)" } else { "" },
            if s.encrypted { ", encrypted" } else { "" },
            s.items,
            s.data_blocks,
            s.bytes,
            s.ms
        ),
        Err(e) => format!("receive from {path} failed: {e}\n"),
    };
    *LAST_RECEIVE.lock() = text;
    audit_op(
        "milkfs.receive",
        &format!("stream={path} target={}", target.unwrap_or("-")),
        &r,
    );
    r
}

// ---- dedupe -------------------------------------------------------------------------

pub use inode::DedupeReport;

static LAST_DEDUPE: SpinLock<String> = SpinLock::new(String::new());

/// Make identical files under `dir` (any VFS path on a milkfs volume) share their blocks;
/// see [`inode::dedupe`]. `dry_run`: only report.
pub fn dedupe(dir: &str, dry_run: bool) -> Result<DedupeReport> {
    let d = crate::fs::ops::lookup(dir)?;
    let r = inode::dedupe(&d, dry_run)?;
    let mut s = format!(
        "{dir}: {} file(s) with data, {} group(s) of identical files, {} {}, {} already shared, {} same hash but different\n",
        r.files,
        r.groups,
        r.deduped,
        if dry_run { "could share" } else { "now share" },
        r.already,
        r.differ
    );
    let _ = writeln!(
        s,
        "{} block(s) ({} KiB) {}{}",
        r.blocks,
        r.blocks * 4,
        if dry_run { "would be shared" } else { "shared" },
        if dry_run {
            String::new()
        } else {
            format!(
                ", {} block(s) freed now{}",
                r.freed,
                if (r.freed as u64) < r.blocks {
                    " (snapshots keep the rest until they are deleted)"
                } else {
                    ""
                }
            )
        }
    );
    audit_op(
        "fs.dedupe",
        &format!("dir={dir} dry_run={dry_run} deduped={}", r.deduped),
        &Ok::<(), Errno>(()),
    );
    *LAST_DEDUPE.lock() = s;
    Ok(r)
}

// ---- tree shape test --------------------------------------------------------------

static LAST_TREETEST: SpinLock<String> = SpinLock::new(String::new());

/// Shape of the primary volume's live tree (committed first).
pub fn tree_stats() -> Result<btree::TreeStats> {
    let v = primary()?;
    let mut st = v.state.lock();
    st.commit()?;
    btree::stats(&st.store, st.sb.root)
}

fn stats_line(what: &str, s: &btree::TreeStats) -> String {
    format!(
        "{what:<22} height {}, {} nodes ({} leaves), {} items, fill {}%, {} underfull\n",
        s.height,
        s.nodes,
        s.leaves,
        s.items,
        s.fill_pct(),
        s.underfull
    )
}

/// Create `n` empty files in `dir`, delete 99 of every 100, then the rest; the live
/// tree's shape after each step.
pub fn tree_test(dir: &str, n: usize) -> Result<(String, [btree::TreeStats; 4])> {
    use crate::fs::ops;
    let s0 = tree_stats()?;
    ops::mkdir_all(dir, 0o755)?;
    let t0 = crate::timer::now_ns();
    for i in 0..n {
        ops::write_file(&format!("{dir}/f{i}"), b"")?;
    }
    let s1 = tree_stats()?;
    let t1 = crate::timer::now_ns();
    for i in (0..n).filter(|i| i % 100 != 0) {
        ops::unlink(&format!("{dir}/f{i}"))?;
    }
    let s2 = tree_stats()?;
    for i in (0..n).step_by(100) {
        ops::unlink(&format!("{dir}/f{i}"))?;
    }
    ops::rmdir(dir)?;
    let s3 = tree_stats()?;
    let t3 = crate::timer::now_ns();
    let mut r = stats_line("before:", &s0);
    r += &stats_line(&format!("{n} files:"), &s1);
    r += &stats_line(&format!("{} left (99% deleted):", n.div_ceil(100)), &s2);
    r += &stats_line("all deleted:", &s3);
    r += &format!(
        "create {} ms, delete {} ms\n",
        (t1 - t0) / 1_000_000,
        (t3 - t1) / 1_000_000
    );
    *LAST_TREETEST.lock() = r.clone();
    Ok((r, [s0, s1, s2, s3]))
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
    v.state.lock().not_converting()?;
    records_room(&v)?;
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
    let view = v
        .state
        .lock()
        .clone_list()
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.id);
    unmount_clone(name);
    if let Some(id) = view {
        crate::fs::pagecache::invalidate_dev(v.view_dev(items::clone_view(id)));
    }
    let r = v.state.lock().clone_discard(name);
    audit_op("milkfs.clone.discard", &alloc::format!("name={name}"), &r);
    r
}

/// Make clone `name` the live volume. The live state it replaces is kept as snapshot
/// `before-<name>`, so `rollback before-<name>` undoes it. Returns that snapshot's name.
pub fn promote_clone(name: &str) -> Result<String> {
    let v = primary()?;
    v.state.lock().not_converting()?;
    v.state.lock().clone_index_check(name)?;
    let mut keep = format!("before-{name}");
    keep.truncate(format::SNAP_NAME_MAX);
    unmount_clone(name);
    if user_op() {
        crate::audit::before_rewind();
    }
    let r = v.state.lock().clone_promote(name, &keep);
    crate::fs::pagecache::invalidate_dev(v.dev);
    crate::fs::notify::rescan(v.dev);
    if user_op() {
        crate::audit::volume_rewound();
    }
    audit_op(
        "milkfs.clone.promote",
        &alloc::format!("name={name} kept_as={keep}"),
        &r,
    );
    r?;
    Ok(keep)
}

/// Background work, once the scheduler runs: the flusher (commits the open transaction
/// once it is a second old) and the Time Machine thread.
pub fn start_threads() {
    // Always: an encrypted primary volume may only be unlocked later.
    drop(crate::sched::spawn("milkfs-flush", || {
        loop {
            crate::sched::sleep_ms(250);
            for (v, _) in all_volumes() {
                let _ = v.state.lock().flush_if_due();
            }
        }
    }));
    timemachine::start();
    scrub::start_thread();
    THREADS_STARTED.store(true, core::sync::atomic::Ordering::Release);
    convert_kick(); // a conversion a crash cut short resumes
}

/// Self-test only (before the scheduler runs, so nothing else can notice): point the
/// primary-volume API and `/sys/fs/milkfs` at a throwaway test volume, so destructive
/// checks never reach the user's disk. `None` restores the real primary volume.
static TEST_OVERRIDE: SpinLock<Option<Arc<Volume>>> = SpinLock::new(None);

static TEST_OVERRIDE_PATH: SpinLock<String> = SpinLock::new(String::new());

/// `path`: where the test volume is mounted.
pub fn set_test_override(v: Option<(Arc<Volume>, &str)>) {
    match v {
        Some((v, path)) => {
            *TEST_OVERRIDE.lock() = Some(v);
            *TEST_OVERRIDE_PATH.lock() = String::from(path);
        }
        None => {
            *TEST_OVERRIDE.lock() = None;
            TEST_OVERRIDE_PATH.lock().clear();
        }
    }
}

/// Where the volume the API acts on is mounted (the test volume during the self-test).
fn api_volume_path() -> Option<String> {
    let t = TEST_OVERRIDE_PATH.lock().clone();
    if !t.is_empty() {
        return Some(t);
    }
    mount_path()
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
    match volume() {
        Some(v) => Ok(v),
        None if !LOCKED.lock().is_empty() => fail(
            Errno::ENODEV,
            "the volume is encrypted and locked: unlock it first (`snap unlock`)",
        ),
        None => fail(Errno::ENODEV, "no milkfs volume is mounted"),
    }
}

/// ENOSPC with the reason when snapshots + clones are at the limit (before the generic
/// "volume full" ENOSPC can be confused with it).
fn records_room(v: &Volume) -> Result<()> {
    if v.state.lock().records_full() {
        return fail(
            Errno::ENOSPC,
            format!(
                "{} snapshots and clones at most: delete one first",
                format::MAX_SNAPS
            ),
        );
    }
    Ok(())
}

/// Take a snapshot of the primary volume.
pub fn snapshot(name: &str) -> Result<SnapInfo> {
    let v = primary()?;
    records_room(&v)?;
    let r = v.state.lock().snap_create(name);
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
    v.state.lock().not_converting()?;
    if user_op() {
        crate::audit::before_rewind();
    }
    let r = (|| {
        let zombies = {
            let mut st = v.state.lock();
            let r = st.snap_rollback(name);
            crate::fs::pagecache::invalidate_dev(v.dev); // live contents changed underneath
            crate::fs::notify::rescan(v.dev);
            r?;
            st.unlinked_inodes()?
        };
        v.reclaim(&zombies)
    })();
    if user_op() {
        crate::audit::volume_rewound();
    }
    audit_op("milkfs.rollback", &alloc::format!("to={name}"), &r);
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
    let _ = crate::fs::search::flush(); // the search index lives on it
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
    if st.store.encrypted() {
        // Mounted means unlocked (a locked volume isn't mounted: /sys/fs/milkfs/unlock).
        let _ = writeln!(
            s,
            "encryption:  {}",
            if st.converting() { "converting" } else { "on" }
        );
        if let Some(t) = st.convert_text() {
            let _ = writeln!(s, "conversion:  {t}");
        }
        let _ = writeln!(s, "locked:      no");
        let _ = writeln!(
            s,
            "cipher:      xchacha20-siv (per block; tag HMAC-SHA256/64)"
        );
        let _ = writeln!(s, "key wrap:    xchacha20-poly1305");
        match st.read_keyring() {
            Ok(ring) => {
                let pass: alloc::vec::Vec<_> = ring
                    .slots
                    .iter()
                    .filter(|k| k.kind == format::SLOT_PASSPHRASE)
                    .collect();
                let rec = ring
                    .slots
                    .iter()
                    .filter(|k| k.kind == format::SLOT_RECOVERY)
                    .count();
                let kdf = pass.first().map_or_else(
                    || String::from("-"),
                    |k| format!("scrypt N=2^{} r={} p={}", k.log_n, k.r, k.p),
                );
                let _ = writeln!(s, "kdf:         {kdf}");
                let _ = writeln!(s, "key slots:   {} passphrase, {rec} recovery", pass.len());
            }
            Err(e) => {
                let _ = writeln!(s, "kdf:         ? (keyring unreadable: {e})");
            }
        }
    } else {
        let _ = writeln!(s, "encryption:  off");
    }
    s
}

/// `/sys/fs/milkfs/disks`: every block device, one line each:
/// `NAME BYTES rw|ro milkfs plain|encrypted mounted=PATH|locked|unmounted LABEL` (the
/// label last: it may hold spaces), or `NAME BYTES rw|ro other` (no milkfs: FAT, blank,
/// a scratch disk). What `mkfs` can be pointed at.
fn disks_text() -> String {
    let mut s = String::new();
    let vols = all_volumes();
    let locked = LOCKED.lock().clone();
    for d in blockdev::devices() {
        let bytes = d.num_blocks() * d.block_size() as u64;
        let ro = if d.read_only() { "ro" } else { "rw" };
        let _ = write!(s, "{} {bytes} {ro}", d.name());
        match probe(&d) {
            Some(sb) => {
                let enc = if sb.flags & format::SB_CONVERTING != 0 {
                    "converting"
                } else if sb.flags & format::SB_ENCRYPTED != 0 {
                    "encrypted"
                } else {
                    "plain"
                };
                let state = match vols.iter().find(|(v, _)| v.device_name == d.name()) {
                    Some((_, p)) => format!("mounted={p}"),
                    None if locked.iter().any(|l| l.name() == d.name()) => String::from("locked"),
                    None => String::from("unmounted"),
                };
                let _ = write!(s, " milkfs {enc} {state} {}", sb.label);
            }
            None => s.push_str(" other"),
        }
        s.push('\n');
    }
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

// ---- control errors: a readable reason next to the errno --------------------------

/// Why the calling thread's current control write is failing (set by [`fail`]).
static REASON: SpinLock<alloc::vec::Vec<(u32, String)>> = SpinLock::new(alloc::vec::Vec::new());
/// `/sys/fs/milkfs/error`: the last failed control write, `CONTROL: ERRNO: reason`.
static LAST_ERROR: SpinLock<String> = SpinLock::new(String::new());

fn tid() -> u32 {
    crate::sched::current_tid()
}

/// Fail with `e` and say why (in `/sys/fs/milkfs/error` when it came from a control).
pub(crate) fn fail<T>(e: Errno, why: impl Into<String>) -> Result<T> {
    let (t, why) = (tid(), why.into());
    let mut r = REASON.lock();
    r.retain(|(x, _)| *x != t);
    if r.len() >= 64 {
        r.remove(0); // reasons nobody collected (kernel callers, not control writes)
    }
    r.push((t, why));
    Err(e)
}

/// The reason [`fail`] recorded on this thread, if any (taken).
fn take_reason() -> Option<String> {
    let t = tid();
    let mut r = REASON.lock();
    let i = r.iter().position(|(x, _)| *x == t)?;
    Some(r.swap_remove(i).1)
}

/// Run control `name`'s write, keeping `/sys/fs/milkfs/error` current: cleared when a
/// write succeeds, `name: ERRNO: reason` when it fails.
fn run_control(name: &str, f: impl FnOnce() -> Result<()>) -> Result<()> {
    let _ = take_reason();
    let r = f();
    let why = take_reason();
    match r {
        Ok(()) => LAST_ERROR.lock().clear(),
        Err(e) => {
            let why = why.unwrap_or_else(|| String::from(explain(name, e)));
            *LAST_ERROR.lock() = format!("{name}: {}: {why}\n", e.name());
        }
    }
    r
}

/// What an errno from control `name` means, when the code that failed gave no reason of
/// its own (docs/filesystem.md lists them next to each control).
fn explain(name: &str, e: Errno) -> &'static str {
    use Errno as E;
    match (name, e) {
        ("snapshot" | "clone", E::EINVAL) => {
            "names are letters, digits and . _ - : (not . or ..); clone: write `SNAPSHOT NAME`"
        }
        ("snapshot" | "clone", E::ENAMETOOLONG) => "names are 47 characters at most",
        ("snapshot" | "clone", E::EEXIST) => "a snapshot or clone already has that name",
        ("clone" | "delete" | "rollback" | "diff", E::ENOENT) => "no such snapshot",
        ("delete", E::EBUSY) => {
            "a clone is built on that snapshot (discard or promote it first) or a send is reading it"
        }
        ("rollback", E::EBUSY) => {
            "a newer snapshot that would go has a clone: discard or promote it first"
        }
        ("diff", E::EINVAL) => "write `A [B|live]`",
        ("auto", E::EINVAL) => "write on, off, now, interval=15m, keep=N or daily=N",
        ("bench", E::EINVAL) => "write `DIR N SIZE`",
        ("treetest", E::EINVAL) => "write `DIR N`",
        ("scrub", E::EBUSY) => "a scrub is already running",
        ("scrub", E::EINVAL) => "write start, stop, rate=MB/s or auto=off|on|7d|12h",
        ("unlock", E::ENODEV) => "nothing is locked (or no locked volume of that name)",
        ("unlock", E::EACCES) => "wrong passphrase or recovery key",
        ("unlock", E::EINVAL) => "write `[dev=NAME ]SECRET`",
        ("unlock" | "passwd", E::EIO) => "the keyring can't be read (damaged?)",
        ("lock", E::EINVAL) => "write a device, mount path, label or `all`",
        ("passwd", E::EACCES) => "the current passphrase (or recovery key) is wrong",
        ("decrypt", E::EACCES) => "wrong passphrase or recovery key",
        ("encrypt" | "decrypt", E::EBUSY) => {
            "busy: already converting, or a send, receive or scrub is using the volume"
        }
        ("encrypt" | "decrypt", E::EINVAL) => {
            "encrypt: `[dev=NAME] [recovery=FILE] [rate=MB]` and the passphrase (8 or more) on a second line; decrypt: `[dev=NAME]` and the passphrase or recovery key"
        }
        ("passwd", E::EINVAL) => "write `OLD\nNEW`; the new passphrase needs 8 characters or more",
        ("send", E::ENOENT) => "no such snapshot (or the output's folder doesn't exist)",
        ("send", E::EINVAL) => {
            "write `SNAP [FROM] PATH [plaintext]` (and `\\nPASSPHRASE` to encrypt); FROM must be older than SNAP"
        }
        ("send", E::ENOSPC) => "no room for the stream where it's going",
        ("receive", E::ENOENT) => {
            "the stream's base snapshot isn't on the target (or no such stream file)"
        }
        ("receive", E::EBUSY) => {
            "the target changed since the base snapshot (`force` rolls it back), or a receive is already running"
        }
        ("receive", E::ENOTEMPTY) => "a full stream needs an empty volume",
        ("receive", E::EACCES) => "the stream is encrypted: its passphrase is missing or wrong",
        ("receive", E::EEXIST) => "the target already has a snapshot of that name",
        ("receive", E::EINVAL) => {
            "not a milkfs stream, a newer stream format, or bad arguments (`PATH [TARGET] [force]`)"
        }
        ("receive", E::EIO) => "the stream is damaged or cut short (nothing was changed)",
        ("dedupe", E::ENOENT) => "no such folder",
        ("dedupe", E::ENOTDIR) => "not a folder",
        ("dedupe", E::EOPNOTSUPP) => "not on a milkfs volume",
        ("dedupe", E::EINVAL) => "write `DIR` or `DIR dry`",
        ("versions" | "restore", E::EXDEV) => {
            "not on the primary milkfs volume (or a snapshot of it)"
        }
        ("versions" | "restore", E::EISDIR) => "versions are kept for files, not folders",
        ("versions", E::ENOENT) => "no such file",
        ("restore", E::ENOENT) => "no such snapshot, or the file isn't in it",
        ("restore", E::EINVAL) => "write `SNAPSHOT PATH`",
        ("check", E::EINVAL) => "write `check` or `repair`",
        ("discard" | "promote", E::ENOENT) => "no such clone",
        ("promote", E::EEXIST) => {
            "the snapshot `before-NAME` (the live state it keeps) exists already: delete it first"
        }
        ("promote", E::ENOSPC) => {
            "30 snapshots and clones at most (promote keeps one more): delete a snapshot first"
        }
        ("mkfs", E::ENODEV) => "no such device",
        ("mkfs", E::EEXIST) => "the device already holds milkfs: add `force` to erase it",
        ("mkfs", E::EBUSY) => "that device is mounted",
        ("mkfs", E::EINVAL) => {
            "write `DEVICE [LABEL] [force] [encrypt [recovery=FILE]]` (encrypt: the passphrase on a second line); labels are 1-31 bytes of UTF-8, no '/', no control characters, not starting with '.'"
        }
        (_, E::EACCES | E::EPERM) => "not allowed (needs an administrator)",
        (_, E::ENOSPC) => "the volume is full",
        (_, E::ENODEV) => "no milkfs volume is mounted",
        (_, E::EIO) => {
            "disk or checksum error, or the volume went read-only after a failed write (see the kernel log; `snap check`)"
        }
        (_, E::EROFS) => "read-only (a snapshot, or a read-only volume)",
        (_, E::ENOMEM) => "out of memory",
        (_, E::EINVAL) => "bad request",
        (_, E::EBUSY) => "busy",
        (_, E::ENOENT) => "not found",
        _ => "failed",
    }
}

/// Controls whose writes [`authorize`] checks first: they destroy or replace data
/// (`delete`, `rollback`, `restore`, `discard`, `promote`, `receive`, `mkfs`, `check
/// repair`, `auto` (its `keep=` prunes snapshots)), take a volume away (`lock`), change a
/// key (`passwd`), or write and delete files anywhere as the kernel (`send`, `bench`,
/// `treetest`, `dedupe`).
pub const DESTRUCTIVE: &[&str] = &[
    "encrypt", "decrypt", "delete", "rollback", "restore", "discard", "promote", "receive", "mkfs",
    "check", "auto", "lock", "passwd", "send", "bench", "treetest", "dedupe",
];

/// The rule for [`authorize`]: `op` is the control (`check repair` for a repair); Err
/// (EACCES or EPERM, with [`set_reason`] for the text) refuses the write.
pub type Authorizer = fn(op: &str) -> Result<()>;
static AUTHORIZER: crate::sync::FnSlot = crate::sync::FnSlot::new();

/// Install (or with `None` remove) the authorisation rule for destructive controls
/// (proc: an admin ticket). Returns the one it replaces.
pub fn set_authorizer(f: Option<Authorizer>) -> Option<Authorizer> {
    let old = AUTHORIZER.get::<Authorizer>();
    match f {
        Some(f) => AUTHORIZER.set(f),
        None => AUTHORIZER.clear(),
    }
    old
}

/// May the caller run destructive control `op` (with argument `arg`)? The one place
/// that decides, called with the writer's credentials (`fs::perm::who()`) before the
/// control runs as the kernel. Without an installed [`Authorizer`]: yes, the open-time
/// permission check (root, or group admin, docs/security.md) is the rule. `check` is
/// only checked for `repair`.
pub fn authorize(op: &str, arg: &[u8]) -> Result<()> {
    let op = if op == "check" {
        if arg.trim_ascii() != b"repair" {
            return Ok(());
        }
        "check repair"
    } else {
        op
    };
    match AUTHORIZER.get::<Authorizer>() {
        Some(f) => f(op),
        None => Ok(()),
    }
}

/// For an [`Authorizer`] (or anything under a control write): the reason shown in
/// `/sys/fs/milkfs/error` for the error about to be returned.
pub fn set_reason(why: &str) {
    let _ = fail::<()>(Errno::EINVAL, why);
}

/// Register a control whose failures are explained in `error`.
fn control(
    dir: &Arc<SynthDir>,
    name: &'static str,
    read: impl Fn() -> String + Send + Sync + 'static,
    write: impl Fn(&[u8]) -> Result<()> + Send + Sync + 'static,
) {
    dir.add_control(name, read, move |b| run_control(name, || write(b)));
}

/// Same, for a [`DESTRUCTIVE`] control: [`authorize`] runs first, as the writer.
fn guarded(
    dir: &Arc<SynthDir>,
    name: &'static str,
    read: impl Fn() -> String + Send + Sync + 'static,
    write: impl Fn(&[u8]) -> Result<()> + Send + Sync + 'static,
) {
    debug_assert!(DESTRUCTIVE.contains(&name));
    dir.add_guarded_control(
        name,
        read,
        move |b| run_control(name, || authorize(name, b)),
        move |b| run_control(name, || write(b)),
    );
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
    dir.add_file("disks", disks_text);
    control(dir, "snapshot", snapshots_text, |b| {
        snapshot(arg(b)?).map(|_| ())
    });
    guarded(dir, "delete", snapshots_text, |b| delete_snapshot(arg(b)?));
    guarded(dir, "rollback", snapshots_text, |b| rollback(arg(b)?));
    control(dir, "sync", String::new, |_| sync());
    guarded(dir, "auto", timemachine::status, |b| {
        timemachine::control(arg(b)?)
    });
    dir.add_file("clones", clones_text);
    control(
        dir,
        "diff",
        || LAST_DIFF.lock().clone(),
        |b| diff_control(arg(b)?),
    );
    guarded(
        dir,
        "bench",
        || LAST_BENCH.lock().clone(),
        |b| {
            // `DIR N SIZE`
            let mut w = arg(b)?.split_whitespace();
            let (Some(d), Some(n), Some(sz)) = (w.next(), w.next(), w.next()) else {
                return Err(Errno::EINVAL);
            };
            let n = n.parse().map_err(|_| Errno::EINVAL)?;
            let sz = sz.parse().map_err(|_| Errno::EINVAL)?;
            bench_create(d, n, sz).map(|_| ())
        },
    );
    control(dir, "scrub", scrub::status_text, |b| {
        scrub::control(arg(b)?)
    });
    control(
        dir,
        "unlock",
        || {
            let mut s = String::new();
            for (d, l) in locked() {
                let _ = writeln!(s, "{d} {l} locked");
            }
            s
        },
        |b| {
            // `[dev=NAME ]SECRET` (a passphrase or the recovery key; may contain spaces)
            let mut raw = alloc::vec::Vec::from(b);
            let r = (|| {
                let t = core::str::from_utf8(&raw).map_err(|_| Errno::EINVAL)?;
                let t = t.trim_end_matches(['\n', '\r']);
                let (dev, secret) = match t.strip_prefix("dev=") {
                    Some(rest) => {
                        let (d, s) = rest.split_once(' ').ok_or(Errno::EINVAL)?;
                        (Some(d), s)
                    }
                    None => (None, t),
                };
                if secret.is_empty() {
                    return Err(Errno::EINVAL);
                }
                unlock(secret, dev).map(|_| ())
            })();
            crate::crypto::wipe(&mut raw);
            r
        },
    );
    guarded(
        dir,
        "lock",
        || {
            let mut s = String::new();
            let home = user_volume();
            for (v, p) in all_volumes() {
                if !v.state.lock().store.encrypted() {
                    continue;
                }
                let note = if home.as_ref().is_some_and(|h| Arc::ptr_eq(h, &v)) {
                    " (home: can't be locked)"
                } else {
                    ""
                };
                let _ = writeln!(s, "{} {} unlocked at={p}{note}", v.device_name, v.label);
            }
            for (d, l) in locked() {
                let _ = writeln!(s, "{d} {l} locked");
            }
            s
        },
        |b| {
            // `all` (every lockable volume) or `DEV|PATH|LABEL`
            let t = arg(b)?;
            lock((t != "all").then_some(t)).map(|_| ())
        },
    );
    dir.add_file("error", || LAST_ERROR.lock().clone());
    guarded(dir, "encrypt", convert_status, |b| {
        // `[dev=NAME] [recovery=FILE] [rate=MB]` then `\nPASSPHRASE`; or just `rate=MB`
        let mut raw = alloc::vec::Vec::from(b);
        let r = (|| {
            let t = core::str::from_utf8(&raw).map_err(|_| Errno::EINVAL)?;
            let (line, pass) = split_secret(t);
            let (mut dev, mut rec) = (None, None);
            for w in line.split_whitespace() {
                if let Some(d) = w.strip_prefix("dev=") {
                    dev = Some(d);
                } else if let Some(f) = w.strip_prefix("recovery=") {
                    rec = Some(f);
                } else if let Some(r) = w.strip_prefix("rate=") {
                    let r = r.parse().map_err(|_| Errno::EINVAL)?;
                    CONVERT_RATE_MB.store(r, core::sync::atomic::Ordering::Relaxed);
                } else {
                    return Err(Errno::EINVAL);
                }
            }
            match pass {
                Some(p) => encrypt_in_place(dev, p, rec),
                None if dev.is_none() && rec.is_none() => Ok(()), // only a rate change
                None => fail(Errno::EINVAL, "write the new passphrase on the second line"),
            }
        })();
        crate::crypto::wipe(&mut raw);
        r
    });
    guarded(dir, "decrypt", convert_status, |b| {
        // `[dev=NAME]` then `\nSECRET` (a passphrase or the recovery key)
        let mut raw = alloc::vec::Vec::from(b);
        let r = (|| {
            let t = core::str::from_utf8(&raw).map_err(|_| Errno::EINVAL)?;
            let (line, secret) = split_secret(t);
            let mut dev = None;
            for w in line.split_whitespace() {
                dev = Some(w.strip_prefix("dev=").ok_or(Errno::EINVAL)?);
            }
            let Some(sec) = secret else {
                return fail(
                    Errno::EINVAL,
                    "write the passphrase (or recovery key) on the second line",
                );
            };
            decrypt_in_place(dev, sec)
        })();
        crate::crypto::wipe(&mut raw);
        r
    });
    guarded(dir, "passwd", String::new, |b| {
        // `OLD\nNEW` (each a whole line, so they may contain spaces)
        let mut raw = alloc::vec::Vec::from(b);
        let r = (|| {
            let t = core::str::from_utf8(&raw).map_err(|_| Errno::EINVAL)?;
            let (old, new) = t.split_once('\n').ok_or(Errno::EINVAL)?;
            change_passphrase(old, new.trim_end_matches(['\n', '\r']))
        })();
        crate::crypto::wipe(&mut raw);
        r
    });
    guarded(
        dir,
        "send",
        || LAST_SEND.lock().clone(),
        |b| {
            // `SNAP [FROM] PATH [plaintext]`, optionally `\nPASSPHRASE` (encrypted)
            let mut raw = alloc::vec::Vec::from(b);
            let r = (|| {
                let t = core::str::from_utf8(&raw).map_err(|_| Errno::EINVAL)?;
                let (line, pass) = split_secret(t);
                let mut w: alloc::vec::Vec<&str> = line.split_whitespace().collect();
                let plain = w.last() == Some(&"plaintext");
                if plain {
                    w.pop();
                }
                let crypt = match (pass, plain) {
                    (Some(_), true) => return Err(Errno::EINVAL),
                    (Some(p), false) => StreamCrypt::Passphrase(p),
                    (None, true) => StreamCrypt::Plain,
                    (None, false) => StreamCrypt::Default,
                };
                match w.as_slice() {
                    [s, p] => send(s, None, p, crypt).map(|_| ()),
                    [s, f, p] => send(s, Some(f), p, crypt).map(|_| ()),
                    _ => Err(Errno::EINVAL),
                }
            })();
            crate::crypto::wipe(&mut raw);
            r
        },
    );
    guarded(
        dir,
        "receive",
        || LAST_RECEIVE.lock().clone(),
        |b| {
            // `PATH [TARGET] [force]`, optionally `\nPASSPHRASE` (an encrypted stream)
            let mut raw = alloc::vec::Vec::from(b);
            let r = (|| {
                let t = core::str::from_utf8(&raw).map_err(|_| Errno::EINVAL)?;
                let (line, pass) = split_secret(t);
                let mut w: alloc::vec::Vec<&str> = line.split_whitespace().collect();
                let force = w.last() == Some(&"force");
                if force {
                    w.pop();
                }
                match w.as_slice() {
                    [p] => receive(p, None, force, pass).map(|_| ()),
                    [p, t] => receive(p, Some(t), force, pass).map(|_| ()),
                    _ => Err(Errno::EINVAL),
                }
            })();
            crate::crypto::wipe(&mut raw);
            r
        },
    );
    guarded(
        dir,
        "dedupe",
        || LAST_DEDUPE.lock().clone(),
        |b| {
            // `DIR` or `DIR dry`
            let a = arg(b)?;
            let (d, dry) = match a.strip_suffix(" dry") {
                Some(d) => (d.trim(), true),
                None => (a, false),
            };
            dedupe(d, dry).map(|_| ())
        },
    );
    guarded(
        dir,
        "treetest",
        || LAST_TREETEST.lock().clone(),
        |b| {
            // `DIR N`
            let mut w = arg(b)?.split_whitespace();
            let (Some(d), Some(n)) = (w.next(), w.next()) else {
                return Err(Errno::EINVAL);
            };
            let (r, _) = tree_test(d, n.parse().map_err(|_| Errno::EINVAL)?)?;
            for l in r.lines() {
                kprintln!("milkfs treetest: {l}");
            }
            Ok(())
        },
    );
    guarded(
        dir,
        "check",
        || LAST_CHECK.lock().clone(),
        |b| {
            let repair = match arg(b)? {
                "check" => false,
                "repair" => true,
                _ => return Err(Errno::EINVAL),
            };
            let r = check(repair)?;
            *LAST_CHECK.lock() = r.text();
            Ok(())
        },
    );
    control(
        dir,
        "versions",
        || LAST_VERSIONS.lock().clone(),
        |b| versions_control(arg(b)?),
    );
    guarded(dir, "restore", String::new, |b| {
        // `SNAPSHOT PATH` (the path may contain spaces).
        let (snap, path) = arg(b)?.split_once(' ').ok_or(Errno::EINVAL)?;
        restore_version(path.trim(), snap)
    });
    control(dir, "clone", clones_text, |b| {
        let mut w = arg(b)?.split_whitespace();
        let (Some(snap), Some(name), None) = (w.next(), w.next(), w.next()) else {
            return Err(Errno::EINVAL);
        };
        clone(snap, name).map(|_| ())
    });
    guarded(dir, "discard", clones_text, |b| discard_clone(arg(b)?));
    guarded(dir, "promote", clones_text, |b| {
        promote_clone(arg(b)?).map(|_| ())
    });
    guarded(
        dir,
        "mkfs",
        || {
            String::from(
                "write: <device> [label] [force] [encrypt [recovery=FILE]] (encrypt: then a newline and the passphrase)\n",
            )
        },
        |b| {
            let mut raw = alloc::vec::Vec::from(b);
            let r = (|| {
                let t = core::str::from_utf8(&raw).map_err(|_| Errno::EINVAL)?;
                let (line, pass) = split_secret(t);
                let mut words = line.split_whitespace();
                let dev = words.next().ok_or(Errno::EINVAL)?;
                let (mut label, mut force, mut encrypt, mut recovery) =
                    ("Milk", false, false, None);
                for w in words {
                    match w {
                        "force" => force = true,
                        "encrypt" => encrypt = true,
                        _ if w.starts_with("recovery=") => recovery = Some(&w[9..]),
                        _ => label = w,
                    }
                }
                let d = blockdev::get(dev).ok_or(Errno::ENODEV)?;
                match (encrypt, pass) {
                    (false, None) if recovery.is_none() => mkfs(&d, label, force),
                    (true, Some(p)) => mkfs_encrypted(&d, label, force, p, recovery),
                    (true, None) => fail(
                        Errno::EINVAL,
                        "encrypt: write the passphrase on the second line",
                    ),
                    _ => fail(
                        Errno::EINVAL,
                        "a passphrase or recovery= only goes with `encrypt`",
                    ),
                }
            })();
            crate::crypto::wipe(&mut raw);
            r
        },
    );
}
