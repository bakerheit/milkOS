//! The host test harness: the platform (a fake clock and a seeded RNG per test thread),
//! disks (in memory, a file, and one that loses power), and a small file API over a
//! volume's `State` that does what the kernel's VFS objects do.

use std::cell::{Cell, RefCell};
use std::string::{String, ToString};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::vec::Vec;

use crate::abi::{InodeType, S_IFDIR, S_IFLNK, S_IFMT, S_IFREG, Timespec};
use crate::blockdev::{BlockDevice, BlockRef};
use crate::errno::{Errno, Result};
use crate::format::*;
use crate::items::LIVE;
use crate::ops::DedupeReport;
use crate::platform::{LockKind, Platform, RawLock};
use crate::volume::{MountReport, State, StateAccess};

// ---- platform -------------------------------------------------------------------------

/// Where every test's clock starts: 2026-10-22T00:00:00Z.
pub const EPOCH: i64 = 1_792_627_200;

thread_local! {
    static WALL: Cell<(i64, i64)> = const { Cell::new((EPOCH, 0)) };
    static MONO: Cell<u64> = const { Cell::new(0) };
    static RNG: Cell<u64> = const { Cell::new(1) };
    static COUNTER: Cell<Option<u64>> = const { Cell::new(None) };
    static REASON: RefCell<String> = const { RefCell::new(String::new()) };
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

struct Host;

/// splitmix64: the test RNG (deterministic per seed).
fn next_random() -> u64 {
    RNG.with(|r| {
        let mut z = r.get().wrapping_add(0x9e37_79b9_7f4a_7c15);
        r.set(z);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    })
}

struct HostLock(AtomicBool);

impl RawLock for HostLock {
    fn lock(&self) {
        while self
            .0
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::thread::yield_now();
        }
    }
    unsafe fn unlock(&self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Platform for Host {
    fn now(&self) -> Timespec {
        let (sec, nsec) = WALL.with(|w| w.get());
        Timespec { sec, nsec }
    }
    fn now_ns(&self) -> u64 {
        MONO.with(|m| m.get())
    }
    fn counter(&self) -> u64 {
        COUNTER.with(|c| c.get()).unwrap_or_else(next_random)
    }
    fn fill_random(&self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let r = next_random().to_le_bytes();
            chunk.copy_from_slice(&r[..chunk.len()]);
        }
    }
    fn scrypt(
        &self,
        pass: &[u8],
        salt: &[u8],
        log_n: u8,
        r: u32,
        p: u32,
        out: &mut [u8],
    ) -> Option<()> {
        milkcrypto::scrypt::scrypt(pass, salt, log_n, r, p, out)
    }
    fn log(&self, args: core::fmt::Arguments<'_>) {
        let line = args.to_string();
        if std::env::var_os("MILKFS_TEST_LOG").is_some() {
            std::eprintln!("{line}");
        }
        LOG.with(|l| l.borrow_mut().push(line));
    }
    fn set_reason(&self, why: &str) {
        REASON.with(|r| *r.borrow_mut() = why.to_string());
    }
    fn new_lock(&self, _kind: LockKind) -> Box<dyn RawLock> {
        Box::new(HostLock(AtomicBool::new(false)))
    }
}

static HOST: &dyn Platform = &Host;

/// Install the host platform and reset this thread's clock, RNG (to `seed`), log and
/// error reason. Every test calls it first.
pub fn setup(seed: u64) {
    crate::platform::set_platform(&HOST);
    WALL.with(|w| w.set((EPOCH, 0)));
    MONO.with(|m| m.set(0));
    RNG.with(|r| r.set(seed));
    COUNTER.with(|c| c.set(None));
    REASON.with(|r| r.borrow_mut().clear());
    LOG.with(|l| l.borrow_mut().clear());
}

/// Move both clocks forward.
pub fn advance(secs: u64) {
    WALL.with(|w| {
        let (s, n) = w.get();
        w.set((s + secs as i64, n));
    });
    MONO.with(|m| m.set(m.get() + secs * 1_000_000_000));
}

/// Move both clocks forward by `ns` nanoseconds.
pub fn advance_ns(ns: u64) {
    WALL.with(|w| {
        let (s, n) = w.get();
        let t = n as u64 + ns;
        w.set((s + (t / 1_000_000_000) as i64, (t % 1_000_000_000) as i64));
    });
    MONO.with(|m| m.set(m.get() + ns));
}

/// Set the wall clock exactly (seconds, nanoseconds).
pub fn set_wall(sec: i64, nsec: i64) {
    WALL.with(|w| w.set((sec, nsec)));
}

/// Make `Platform::counter` return `v` (what seeds a new volume's UUID).
pub fn set_counter(v: u64) {
    COUNTER.with(|c| c.set(Some(v)));
}

/// The reason the last `fail` gave (and forget it).
pub fn take_reason() -> String {
    REASON.with(|r| core::mem::take(&mut *r.borrow_mut()))
}

/// Log lines so far (and forget them).
pub fn take_log() -> Vec<String> {
    LOG.with(|l| core::mem::take(&mut *l.borrow_mut()))
}

/// A random number from the test RNG.
pub fn rand() -> u64 {
    next_random()
}

// ---- disks ----------------------------------------------------------------------------

pub const SECTOR: usize = 512;

/// A disk in memory, 512-byte sectors (like virtio-blk).
pub struct MemDisk {
    name: String,
    data: Mutex<Vec<u8>>,
    pub writes: AtomicU64,
    pub flushes: AtomicU64,
}

impl MemDisk {
    pub fn new(bytes: usize) -> Arc<MemDisk> {
        Self::from_image(std::vec![0u8; bytes])
    }

    pub fn from_image(img: Vec<u8>) -> Arc<MemDisk> {
        assert!(img.len().is_multiple_of(SECTOR));
        Arc::new(MemDisk {
            name: String::from("vda"),
            data: Mutex::new(img),
            writes: AtomicU64::new(0),
            flushes: AtomicU64::new(0),
        })
    }

    pub fn image(&self) -> Vec<u8> {
        self.data.lock().unwrap().clone()
    }

    /// Overwrite bytes at `off` (damage on purpose).
    pub fn poke(&self, off: usize, bytes: &[u8]) {
        self.data.lock().unwrap()[off..off + bytes.len()].copy_from_slice(bytes);
    }

    pub fn peek(&self, off: usize, len: usize) -> Vec<u8> {
        self.data.lock().unwrap()[off..off + len].to_vec()
    }
}

impl BlockDevice for MemDisk {
    fn name(&self) -> &str {
        &self.name
    }
    fn block_size(&self) -> usize {
        SECTOR
    }
    fn num_blocks(&self) -> u64 {
        (self.data.lock().unwrap().len() / SECTOR) as u64
    }
    fn read_only(&self) -> bool {
        false
    }
    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        let d = self.data.lock().unwrap();
        let at = lba as usize * SECTOR;
        if at + buf.len() > d.len() {
            return Err(Errno::EIO);
        }
        buf.copy_from_slice(&d[at..at + buf.len()]);
        Ok(())
    }
    fn write_blocks(&self, lba: u64, buf: &[u8]) -> Result<()> {
        let mut d = self.data.lock().unwrap();
        let at = lba as usize * SECTOR;
        if at + buf.len() > d.len() {
            return Err(Errno::EIO);
        }
        d[at..at + buf.len()].copy_from_slice(buf);
        self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn flush(&self) -> Result<()> {
        self.flushes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// How a [`CrashDisk`] loses power.
#[derive(Clone, Copy, Debug)]
pub enum Cut {
    /// The machine is killed after `n` more write requests, all of which landed whole
    /// (qemu SIGKILLed: what it wrote is in the host's file).
    Writes(u64),
    /// Writes land in order, straight on the disk; after `sectors` more sectors the power
    /// goes: the request crossing that point is torn (its first part lands), nothing after
    /// it does (the kernel's `powercut=N`).
    Sectors(u64),
    /// The `k`-th write to the superblock area is cut after `keep` of its 8 sectors
    /// (8: it landed, but the commit never returned) (the kernel's `powercut-sb=K:T`).
    Superblock(u64, u64),
    /// A drive with a volatile write cache: writes reach the platter only at a flush, in
    /// any order. After `sectors` more sectors the power goes, and of the writes since
    /// the last flush a random subset (by `seed`) lands, in a random order, each one
    /// possibly torn.
    Cache { sectors: u64, seed: u64 },
}

/// A disk that loses power at a chosen moment (see [`Cut`]). Afterwards every request
/// fails (EIO) and [`CrashDisk::image`] is what the platter holds.
pub struct CrashDisk {
    disk: Mutex<Vec<u8>>,
    cache: Mutex<Vec<(u64, Vec<u8>)>>,
    cut: Cut,
    armed: AtomicBool,
    dead: AtomicBool,
    written: AtomicU64,
    sb_writes: AtomicU64,
}

impl CrashDisk {
    pub fn new(img: Vec<u8>, cut: Cut) -> Arc<CrashDisk> {
        Arc::new(CrashDisk {
            disk: Mutex::new(img),
            cache: Mutex::new(Vec::new()),
            cut,
            armed: AtomicBool::new(false),
            dead: AtomicBool::new(false),
            written: AtomicU64::new(0),
            sb_writes: AtomicU64::new(0),
        })
    }

    /// Start counting (the workload begins; mount and setup don't count).
    pub fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    pub fn dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// Sectors (or for `Cut::Writes`, requests) written since `arm`.
    pub fn sectors_written(&self) -> u64 {
        self.written.load(Ordering::SeqCst)
    }

    /// What the platter holds (after a cut: what survived it).
    pub fn image(&self) -> Vec<u8> {
        self.disk.lock().unwrap().clone()
    }

    fn put(disk: &mut [u8], lba: u64, buf: &[u8]) {
        let at = lba as usize * SECTOR;
        disk[at..at + buf.len()].copy_from_slice(buf);
    }

    /// Power off: with a write cache, some of what it held lands.
    fn die(&self, disk: &mut [u8]) {
        if let Cut::Cache { seed, .. } = self.cut {
            let mut pending = core::mem::take(&mut *self.cache.lock().unwrap());
            let mut x = seed | 1;
            let mut r = || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            // A random order (Fisher-Yates), then each write lands whole, torn, or not.
            for i in (1..pending.len()).rev() {
                let j = (r() % (i as u64 + 1)) as usize;
                pending.swap(i, j);
            }
            for (lba, data) in pending {
                let sectors = data.len() / SECTOR;
                match r() % 3 {
                    0 => {}
                    1 => Self::put(disk, lba, &data),
                    _ => {
                        let keep = (r() % sectors as u64) as usize;
                        Self::put(disk, lba, &data[..keep * SECTOR]);
                    }
                }
            }
        }
        self.dead.store(true, Ordering::SeqCst);
    }
}

impl BlockDevice for CrashDisk {
    fn name(&self) -> &str {
        "vda"
    }
    fn block_size(&self) -> usize {
        SECTOR
    }
    fn num_blocks(&self) -> u64 {
        (self.disk.lock().unwrap().len() / SECTOR) as u64
    }
    fn read_only(&self) -> bool {
        false
    }
    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        if self.dead() {
            return Err(Errno::EIO);
        }
        let d = self.disk.lock().unwrap();
        let at = lba as usize * SECTOR;
        buf.copy_from_slice(&d[at..at + buf.len()]);
        // The drive answers reads from its cache too.
        for (l, data) in self.cache.lock().unwrap().iter() {
            let (a0, a1) = (*l as usize * SECTOR, *l as usize * SECTOR + data.len());
            let (b0, b1) = (at, at + buf.len());
            let (lo, hi) = (a0.max(b0), a1.min(b1));
            if lo < hi {
                buf[lo - b0..hi - b0].copy_from_slice(&data[lo - a0..hi - a0]);
            }
        }
        Ok(())
    }
    fn write_blocks(&self, lba: u64, buf: &[u8]) -> Result<()> {
        if self.dead() {
            return Err(Errno::EIO);
        }
        let mut d = self.disk.lock().unwrap();
        let n = (buf.len() / SECTOR) as u64;
        if !self.armed.load(Ordering::SeqCst) {
            Self::put(&mut d, lba, buf);
            return Ok(());
        }
        match self.cut {
            Cut::Writes(n) => {
                if self.written.fetch_add(1, Ordering::SeqCst) >= n {
                    self.die(&mut d);
                    return Err(Errno::EIO);
                }
                Self::put(&mut d, lba, buf);
                Ok(())
            }
            Cut::Superblock(k, keep) => {
                let sb_sectors = (2 * BLOCK / SECTOR) as u64;
                if lba < sb_sectors && self.sb_writes.fetch_add(1, Ordering::SeqCst) + 1 == k {
                    let keep = keep.min(n) as usize;
                    Self::put(&mut d, lba, &buf[..keep * SECTOR]);
                    self.die(&mut d);
                    return Err(Errno::EIO);
                }
                Self::put(&mut d, lba, buf);
                Ok(())
            }
            Cut::Sectors(budget)
            | Cut::Cache {
                sectors: budget, ..
            } => {
                let done = self.written.load(Ordering::SeqCst);
                let left = budget.saturating_sub(done);
                let cached = matches!(self.cut, Cut::Cache { .. });
                if n <= left {
                    self.written.fetch_add(n, Ordering::SeqCst);
                    if cached {
                        self.cache.lock().unwrap().push((lba, buf.to_vec()));
                    } else {
                        Self::put(&mut d, lba, buf);
                    }
                    return Ok(());
                }
                // Tear: only the first `left` sectors make it (into the cache, if any).
                if left > 0 {
                    let part = &buf[..left as usize * SECTOR];
                    if cached {
                        self.cache.lock().unwrap().push((lba, part.to_vec()));
                    } else {
                        Self::put(&mut d, lba, part);
                    }
                }
                self.die(&mut d);
                Err(Errno::EIO)
            }
        }
    }
    fn flush(&self) -> Result<()> {
        if self.dead() {
            return Err(Errno::EIO);
        }
        let mut d = self.disk.lock().unwrap();
        for (lba, data) in core::mem::take(&mut *self.cache.lock().unwrap()) {
            Self::put(&mut d, lba, &data);
        }
        Ok(())
    }
}

// ---- a volume and its files -----------------------------------------------------------

/// One mounted volume, with the file operations the kernel's VFS objects do (each a
/// transaction joined to the open batch, committed when it is due, as in `Volume::txn`).
pub struct Fs {
    pub st: RefCell<State>,
    pub report: MountReport,
}

impl StateAccess for Fs {
    fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.st.borrow_mut())
    }
}

/// No file is held open by anyone in these tests.
pub fn none_open(_view: u64, _ino: u64) -> bool {
    false
}

impl Fs {
    pub fn mount(dev: BlockRef) -> Result<Fs> {
        let (st, report) = State::open(dev)?;
        Ok(Fs {
            st: RefCell::new(st),
            report,
        })
    }

    pub fn mount_key(dev: BlockRef, key: Arc<crate::crypt::VolumeKey>) -> Result<Fs> {
        let (st, report) = State::open_with(dev, Some(key))?;
        Ok(Fs {
            st: RefCell::new(st),
            report,
        })
    }

    /// A fresh volume on a memory disk of `mib` MiB, mounted.
    pub fn fresh(mib: usize) -> (Arc<MemDisk>, Fs) {
        let d = MemDisk::new(mib << 20);
        let dev: BlockRef = d.clone();
        crate::volume::mkfs(&dev, "Milk").unwrap();
        (d, Fs::mount(dev).unwrap())
    }

    pub fn st(&self) -> std::cell::RefMut<'_, State> {
        self.st.borrow_mut()
    }

    /// Run `f` as one operation on `view` (live or a clone), like `Volume::txn`.
    pub fn txn<R>(&self, view: u64, f: impl FnOnce(&mut State) -> Result<R>) -> Result<R> {
        let mut st = self.st.borrow_mut();
        st.check_writable()?;
        let r = match st.set_cur(view) {
            Ok(()) => f(&mut st),
            Err(e) => Err(e),
        };
        let _ = st.set_cur(LIVE);
        let c = st.op_done();
        let r = r?;
        c?;
        Ok(r)
    }

    pub fn commit(&self) -> Result<()> {
        self.st.borrow_mut().commit()
    }

    /// The inode at `path` (absolute, `/`-separated) in `view`.
    pub fn resolve(&self, view: u64, path: &str) -> Result<u64> {
        let st = self.st.borrow();
        let mut ino = ROOT_INO;
        for c in path.split('/').filter(|c| !c.is_empty()) {
            ino = st.lookup(view, ino, c)?.ok_or(Errno::ENOENT)?.ino;
        }
        Ok(ino)
    }

    fn split(path: &str) -> (&str, &str) {
        match path.rsplit_once('/') {
            Some((d, n)) => (if d.is_empty() { "/" } else { d }, n),
            None => ("/", path),
        }
    }

    pub fn mkdir_in(&self, view: u64, path: &str) -> Result<u64> {
        let (dir, name) = Self::split(path);
        let d = self.resolve(view, dir)?;
        self.txn(view, |st| st.create_node(d, name, S_IFDIR | 0o755, None))
    }

    pub fn mkdir(&self, path: &str) -> Result<u64> {
        self.mkdir_in(LIVE, path)
    }

    /// `mkdir -p` in `view`.
    pub fn mkdir_all_in(&self, view: u64, path: &str) -> Result<u64> {
        let mut at = String::new();
        let mut ino = ROOT_INO;
        for c in path.split('/').filter(|c| !c.is_empty()) {
            at.push('/');
            at.push_str(c);
            ino = match self.resolve(view, &at) {
                Ok(i) => i,
                Err(Errno::ENOENT) => self.mkdir_in(view, &at)?,
                Err(e) => return Err(e),
            };
        }
        Ok(ino)
    }

    pub fn mkdir_all(&self, path: &str) -> Result<u64> {
        self.mkdir_all_in(LIVE, path)
    }

    pub fn create_in(&self, view: u64, path: &str, mode: u32) -> Result<u64> {
        let (dir, name) = Self::split(path);
        let d = self.resolve(view, dir)?;
        self.txn(view, |st| st.create_node(d, name, mode, None))
    }

    pub fn symlink(&self, path: &str, target: &str) -> Result<u64> {
        let (dir, name) = Self::split(path);
        let d = self.resolve(LIVE, dir)?;
        self.txn(LIVE, |st| {
            st.create_node(d, name, S_IFLNK | 0o777, Some(target))
        })
    }

    /// Like `ops::write_file`: open with O_CREAT | O_TRUNC, one write of everything (the
    /// VFS splits nothing here), so three operations.
    pub fn write_in(&self, view: u64, path: &str, data: &[u8]) -> Result<u64> {
        let ino = match self.resolve(view, path) {
            Ok(i) => i,
            Err(Errno::ENOENT) => self.create_in(view, path, S_IFREG | 0o644)?,
            Err(e) => return Err(e),
        };
        self.txn(view, |st| st.truncate_file(ino, 0))?;
        if !data.is_empty() {
            self.txn(view, |st| st.write_file(ino, 0, data))?;
        }
        Ok(ino)
    }

    pub fn write(&self, path: &str, data: &[u8]) -> Result<u64> {
        self.write_in(LIVE, path, data)
    }

    /// Write `data` at `off` of an existing file.
    pub fn pwrite(&self, path: &str, off: u64, data: &[u8]) -> Result<()> {
        let ino = self.resolve(LIVE, path)?;
        self.txn(LIVE, |st| st.write_file(ino, off, data))
            .map(|_| ())
    }

    pub fn append(&self, path: &str, data: &[u8]) -> Result<()> {
        let ino = self.resolve(LIVE, path)?;
        let size = self.st.borrow().inode(LIVE, ino)?.size;
        self.txn(LIVE, |st| st.write_file(ino, size, data))
            .map(|_| ())
    }

    pub fn truncate(&self, path: &str, len: u64) -> Result<()> {
        let ino = self.resolve(LIVE, path)?;
        self.txn(LIVE, |st| st.truncate_file(ino, len))
    }

    pub fn read_ino(&self, view: u64, ino: u64) -> Result<Vec<u8>> {
        let st = self.st.borrow();
        let i = st.inode(view, ino)?;
        if i.mode & S_IFMT != S_IFREG {
            return Err(Errno::EISDIR);
        }
        let mut out = std::vec![0u8; i.size as usize];
        let mut off = 0;
        while off < i.size {
            let n = st.read_data(view, ino, i.size, off, &mut out[off as usize..])?;
            off += n as u64;
        }
        Ok(out)
    }

    pub fn read_in(&self, view: u64, path: &str) -> Result<Vec<u8>> {
        let ino = self.resolve(view, path)?;
        self.read_ino(view, ino)
    }

    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        self.read_in(LIVE, path)
    }

    pub fn unlink_in(&self, view: u64, path: &str) -> Result<()> {
        let (dir, name) = Self::split(path);
        let d = self.resolve(view, dir)?;
        self.txn(view, |st| st.unlink_node(d, name, &none_open))
    }

    pub fn unlink(&self, path: &str) -> Result<()> {
        self.unlink_in(LIVE, path)
    }

    pub fn rmdir(&self, path: &str) -> Result<()> {
        let (dir, name) = Self::split(path);
        let d = self.resolve(LIVE, dir)?;
        self.txn(LIVE, |st| st.rmdir_node(d, name))
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        let (fd, fname) = Self::split(from);
        let (td, tname) = Self::split(to);
        let (a, b) = (self.resolve(LIVE, fd)?, self.resolve(LIVE, td)?);
        self.txn(LIVE, |st| st.rename_node(a, fname, b, tname, &none_open))
    }

    pub fn link(&self, existing: &str, new: &str) -> Result<()> {
        let ino = self.resolve(LIVE, existing)?;
        let (dir, name) = Self::split(new);
        let d = self.resolve(LIVE, dir)?;
        self.txn(LIVE, |st| st.link_node(d, name, ino, InodeType::File))
    }

    /// `cp --reflink`: `dst` (created if missing) shares `src`'s blocks.
    pub fn reflink(&self, src: &str, dst: &str) -> Result<()> {
        let s = self.resolve(LIVE, src)?;
        let d = match self.resolve(LIVE, dst) {
            Ok(i) => i,
            Err(Errno::ENOENT) => self.create_in(LIVE, dst, S_IFREG | 0o644)?,
            Err(e) => return Err(e),
        };
        self.txn(LIVE, |st| st.reflink_file(s, d))
    }

    /// `dedupe [-n] DIR` (what the kernel's `inode::dedupe` does around `State::dedupe`).
    pub fn dedupe(&self, dir: &str, dry_run: bool) -> Result<DedupeReport> {
        let top = self.resolve(LIVE, dir)?;
        let mut rep = DedupeReport::default();
        let free0 = self.st().store.free_blocks();
        if dry_run {
            let mut st = self.st();
            st.set_cur(LIVE)?;
            let r = st.dedupe(top, true, &mut rep);
            let _ = st.set_cur(LIVE);
            r?;
        } else {
            self.st().not_converting()?;
            self.txn(LIVE, |st| st.dedupe(top, false, &mut rep))?;
            let mut st = self.st();
            st.commit()?;
            rep.freed = st.store.free_blocks() as i64 - free0 as i64;
        }
        Ok(rep)
    }

    /// Names in directory `path` of `view`, sorted.
    pub fn ls_in(&self, view: u64, path: &str) -> Result<Vec<String>> {
        let d = self.resolve(view, path)?;
        let mut v: Vec<String> = self
            .st
            .borrow()
            .list(view, d)?
            .into_iter()
            .map(|e| e.name)
            .collect();
        v.sort();
        Ok(v)
    }

    pub fn ls(&self, path: &str) -> Result<Vec<String>> {
        self.ls_in(LIVE, path)
    }

    /// Every path in `view` with its type and content hash, sorted (to compare trees, like
    /// `mkmilkfs.py tree`).
    pub fn tree(&self, view: u64) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut stack = std::vec![(ROOT_INO, String::new())];
        while let Some((dir, prefix)) = stack.pop() {
            let entries = self.st.borrow().list(view, dir)?;
            for e in entries {
                let p = std::format!("{prefix}/{}", e.name);
                let i = self.st.borrow().inode(view, e.ino)?;
                match i.mode & S_IFMT {
                    S_IFDIR => {
                        out.push(std::format!("d {:o} {p}", i.mode & 0o7777));
                        stack.push((e.ino, p));
                    }
                    S_IFLNK => {
                        let t = self.st.borrow().symlink_target(view, e.ino)?;
                        out.push(std::format!("l {p} -> {t}"));
                    }
                    _ => {
                        let data = self.read_ino(view, e.ino)?;
                        out.push(std::format!(
                            "f {:o} {} {:016x} {p}",
                            i.mode & 0o7777,
                            data.len(),
                            xxh64(&data)
                        ));
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// The full check, which must be clean.
    pub fn check_clean(&self) {
        let r = self.st().check(false).unwrap();
        assert!(r.clean(), "check: {}", r.text());
    }

    /// Space accounting after a commit must be exact (no leak, nothing reachable free).
    pub fn space_exact(&self) {
        let c = self.st().verify_space().unwrap();
        assert!(
            c.leaked == 0 && c.free_but_used == 0,
            "space accounting: {c:?}"
        );
    }

    pub fn generation(&self) -> u64 {
        self.st.borrow().sb.generation
    }
}

// ---- self-describing contents (the kernel's crash workload) ----------------------------

/// File contents for iteration `i`: a header line, then a body derived from `i`.
pub fn content(i: u64) -> Vec<u8> {
    let len = 600 + (i.wrapping_mul(7919) % 120_000) as usize;
    let mut v = std::format!("milkcrash i={i} len={len}\n").into_bytes();
    let mut j = v.len();
    while j < len {
        v.push((i.wrapping_mul(31) as usize ^ j.wrapping_mul(7) ^ (j >> 8)) as u8);
        j += 1;
    }
    v.truncate(len);
    v
}

/// Does `data` match its own header? Empty is reported as `None`.
pub fn verify(data: &[u8]) -> Option<bool> {
    if data.is_empty() {
        return None;
    }
    let nl = data.iter().position(|&b| b == b'\n')?;
    let head = core::str::from_utf8(&data[..nl]).ok()?;
    let mut it = head.split(' ');
    if it.next() != Some("milkcrash") {
        return Some(false);
    }
    let i: u64 = it.next()?.strip_prefix("i=")?.parse().ok()?;
    Some(content(i) == data)
}

/// Random bytes from the test RNG.
pub fn noise(len: usize) -> Vec<u8> {
    let mut v = std::vec![0u8; len];
    crate::platform::fill_random(&mut v);
    v
}
