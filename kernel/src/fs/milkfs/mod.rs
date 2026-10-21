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
        let key = if sb.flags & format::SB_ENCRYPTED != 0 {
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
    let label = if sb.label.is_empty() || sb.label.contains('/') {
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
fn test_passphrase() -> Option<String> {
    let f = crate::fwcfg::find_file("opt/milkos/passphrase")?;
    let mut buf = alloc::vec![0u8; f.size as usize];
    if !crate::fwcfg::read_file(f, &mut buf) {
        return None;
    }
    let s = String::from_utf8_lossy(&buf)
        .trim_end_matches('\0')
        .trim()
        .into();
    crate::crypto::ct::zeroize(&mut buf);
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
/// primary volume, home, /Applications and its clones are set up as at boot.
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
    let ok = !mounted.is_empty();
    audit_op(
        "milkfs.unlock",
        &format!("dev={} mounted={}", dev.unwrap_or("-"), mounted.join(",")),
        &if ok { Ok(()) } else { Err(last) },
    );
    if ok { Ok(mounted) } else { Err(last) }
}

/// Change a passphrase of the primary (encrypted) volume: `old` (a passphrase or the
/// recovery key) must open a slot; the volume key is rewrapped under `new` (no block is
/// rewritten). Replaces the slot `old` opened, or the first passphrase slot if `old`
/// was the recovery key.
pub fn change_passphrase(old: &str, new: &str) -> Result<()> {
    if new.len() < 8 {
        return Err(Errno::EINVAL);
    }
    let v = primary()?;
    let r = v.state.lock().rewrap(old, new);
    audit_op("milkfs.passwd", "", &r);
    r
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

/// Where packages live (`milk install`).
pub const APPLICATIONS: &str = "/Applications";

/// Persistent applications: bind `<volume>/Applications` onto `/Applications` (created
/// empty the first time), so installed packages survive reboots and are part of the
/// volume's snapshots and clones (`milk try`). Without a volume it stays on tmpfs.
pub fn setup_applications() {
    let Some(vol) = mount_path() else { return };
    let on_disk = format!("{vol}{APPLICATIONS}");
    if crate::fs::ops::stat(&on_disk).is_err()
        && let Err(e) = crate::fs::ops::mkdir_all(&on_disk, 0o755)
    {
        kprintln!("milkfs: can't create {on_disk}: {e}; {APPLICATIONS} stays in RAM");
        return;
    }
    if let Err(e) = crate::fs::path::bind(&on_disk, APPLICATIONS) {
        kprintln!("milkfs: bind {on_disk} -> {APPLICATIONS} failed: {e}");
    }
}

// ---- versions -----------------------------------------------------------------------

pub use versions::Version;

/// The live file at `path` (any VFS path, e.g. `/home/milk/Documents/ideas.md`), as
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
}

/// Write snapshot `snap` of the primary volume as a stream to `out` (a file path),
/// incremental from the older snapshot `from`. The volume isn't locked while the stream
/// is written (it may go onto the volume itself); the snapshots read can't be deleted
/// meanwhile (EBUSY).
pub fn send(snap: &str, from: Option<&str>, out: &str) -> Result<SendReport> {
    use crate::fs::abi::{O_CREAT, O_TRUNC, O_WRONLY};
    use stream::{Encoder, Plan, Record};
    let v = primary()?;
    let t0 = crate::timer::now_ns();
    let plan = {
        let mut st = v.state.lock();
        let p = st.send_plan(snap, from)?;
        st.busy_snaps.extend(p.gens.iter().copied());
        p
    };
    let gens = plan.gens.clone();
    let r = (|| -> Result<SendReport> {
        let f = crate::fs::ops::open(out, O_WRONLY | O_CREAT | O_TRUNC, 0o644)?;
        let mut enc = Encoder::default();
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
            "sent {snap}{} to {out}: {} records, {} data blocks, {} bytes in {} ms\n",
            from.map(|f| format!(" (incremental from {f})"))
                .unwrap_or_default(),
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
        &format!("snapshot={snap} from={} out={out}", from.unwrap_or("-")),
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
pub fn receive(path: &str, target: Option<&str>, force: bool) -> Result<RecvReport> {
    use crate::fs::abi::O_RDONLY;
    use stream::{Decoder, Record};
    let v = match target {
        Some(t) => volume_at(t)?,
        None => primary()?,
    };
    let t0 = crate::timer::now_ns();
    let f = crate::fs::ops::open(path, O_RDONLY, 0)?;
    let mut rd = Decoder::new(|b: &mut [u8]| f.read(b));
    let r = (|| -> Result<RecvReport> {
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
        })
    })();
    let text = match &r {
        Ok(s) => format!(
            "received {}{} from {path}: {} items, {} data blocks, {} bytes in {} ms\n",
            s.snapshot,
            if s.incremental { " (incremental)" } else { "" },
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
    dir.add_control(
        "diff",
        || LAST_DIFF.lock().clone(),
        |b| diff_control(arg(b)?),
    );
    dir.add_control(
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
    dir.add_control("scrub", scrub::status_text, |b| scrub::control(arg(b)?));
    dir.add_control(
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
            crate::crypto::ct::zeroize(&mut raw);
            r
        },
    );
    dir.add_control("passwd", String::new, |b| {
        // `OLD\nNEW` (each a whole line, so they may contain spaces)
        let mut raw = alloc::vec::Vec::from(b);
        let r = (|| {
            let t = core::str::from_utf8(&raw).map_err(|_| Errno::EINVAL)?;
            let (old, new) = t.split_once('\n').ok_or(Errno::EINVAL)?;
            change_passphrase(old, new.trim_end_matches(['\n', '\r']))
        })();
        crate::crypto::ct::zeroize(&mut raw);
        r
    });
    dir.add_control(
        "send",
        || LAST_SEND.lock().clone(),
        |b| {
            // `SNAP [FROM] PATH`
            let w: alloc::vec::Vec<&str> = arg(b)?.split_whitespace().collect();
            match w.as_slice() {
                [s, p] => send(s, None, p).map(|_| ()),
                [s, f, p] => send(s, Some(f), p).map(|_| ()),
                _ => Err(Errno::EINVAL),
            }
        },
    );
    dir.add_control(
        "receive",
        || LAST_RECEIVE.lock().clone(),
        |b| {
            // `PATH [TARGET] [force]`
            let mut w: alloc::vec::Vec<&str> = arg(b)?.split_whitespace().collect();
            let force = w.last() == Some(&"force");
            if force {
                w.pop();
            }
            match w.as_slice() {
                [p] => receive(p, None, force).map(|_| ()),
                [p, t] => receive(p, Some(t), force).map(|_| ()),
                _ => Err(Errno::EINVAL),
            }
        },
    );
    dir.add_control(
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
    dir.add_control(
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
    dir.add_control(
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
    dir.add_control(
        "versions",
        || LAST_VERSIONS.lock().clone(),
        |b| versions_control(arg(b)?),
    );
    dir.add_control("restore", String::new, |b| {
        // `SNAPSHOT PATH` (the path may contain spaces).
        let (snap, path) = arg(b)?.split_once(' ').ok_or(Errno::EINVAL)?;
        restore_version(path.trim(), snap)
    });
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
