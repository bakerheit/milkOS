//! milkfs through the VFS: [`Volume`] (a mounted volume, the `FileSystem`), [`MilkInode`]
//! (a file or directory in the live tree or in a snapshot) and [`SnapsDir`]
//! (`<volume>/.snapshots`).
//!
//! Inode objects hold no file data or metadata, only `(view, ino)`: every call reads the
//! tree. So a rollback needs no cache invalidation, and inode numbers are never reused
//! (`next_ino` only grows) so a stale object can only ever see ENOENT.
//!
//! Every mutating call is its own transaction and commits before returning (v0 is
//! synchronous: an `Ok` means it is on disk).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use super::format::*;
use super::items::LIVE;
use super::volume::State;
use crate::fs::clock;
use crate::fs::errno::{Errno, Result};
use crate::fs::node::{
    DirEntry, FileSystem, Inode, InodeRef, InodeType, SetAttr, Stat, Timespec, alloc_dev,
    check_name, downcast,
};
use crate::sched::Mutex;
use crate::sync::SpinLock;

/// Name of the snapshot directory in the volume root.
pub const SNAPS_NAME: &str = ".snapshots";
/// Inode number of `.snapshots` (never allocated to a real inode).
pub const SNAPS_INO: u64 = u64::MAX - 1;

pub struct Volume {
    /// Held across disk I/O (commits sleep on virtio-blk), so a sleeping mutex.
    pub state: Mutex<State>,
    /// Device number of the live view; snapshots get their own (`snap_dev`).
    pub dev: u64,
    pub label: String,
    pub device_name: String,
    inodes: SpinLock<BTreeMap<(u64, u64), Weak<MilkInode>>>,
    snap_devs: SpinLock<BTreeMap<u64, u64>>,
    /// Inodes whose last in-memory reference went away (unlinked-while-open cleanup).
    reap: SpinLock<Vec<u64>>,
    me: Weak<Volume>,
}

fn now() -> (i64, u32) {
    let t = clock::now();
    (t.sec, t.nsec as u32)
}

fn ts(t: (i64, u32)) -> Timespec {
    Timespec {
        sec: t.0,
        nsec: t.1 as i64,
    }
}

fn kind_of(mode: u32) -> InodeType {
    InodeType::from_mode(mode).unwrap_or(InodeType::File)
}

impl Volume {
    pub fn new(state: State, device_name: &str) -> Arc<Volume> {
        let label = String::from(state.label());
        Arc::new_cyclic(|me| Volume {
            state: Mutex::new(state),
            dev: alloc_dev(),
            label,
            device_name: String::from(device_name),
            inodes: SpinLock::new(BTreeMap::new()),
            snap_devs: SpinLock::new(BTreeMap::new()),
            reap: SpinLock::new(Vec::new()),
            me: me.clone(),
        })
    }

    fn arc(&self) -> Arc<Volume> {
        self.me.upgrade().expect("milkfs: volume gone")
    }

    pub fn view_dev(&self, view: u64) -> u64 {
        if view == LIVE {
            return self.dev;
        }
        *self.snap_devs.lock().entry(view).or_insert_with(alloc_dev)
    }

    /// The one object for `(view, ino)`.
    pub fn inode_obj(&self, view: u64, ino: u64, kind: InodeType) -> Arc<MilkInode> {
        let mut map = self.inodes.lock();
        if let Some(i) = map.get(&(view, ino)).and_then(|w| w.upgrade()) {
            return i;
        }
        if map.len() > 1024 {
            map.retain(|_, w| w.strong_count() > 0);
        }
        let i = Arc::new_cyclic(|me| MilkInode {
            vol: self.arc(),
            view,
            ino,
            kind,
            dev: self.view_dev(view),
            me: me.clone(),
        });
        map.insert((view, ino), Arc::downgrade(&i));
        i
    }

    fn is_open(&self, ino: u64) -> bool {
        self.inodes
            .lock()
            .get(&(LIVE, ino))
            .is_some_and(|w| w.strong_count() > 0)
    }

    pub fn root_obj(&self, view: u64) -> Arc<MilkInode> {
        self.inode_obj(view, ROOT_INO, InodeType::Dir)
    }

    /// Run a mutating operation on the live tree as one transaction.
    pub fn txn<R>(&self, f: impl FnOnce(&mut State) -> Result<R>) -> Result<R> {
        let reap = core::mem::take(&mut *self.reap.lock());
        let mut st = self.state.lock();
        st.check_writable()?;
        for ino in reap {
            if !self.is_open(ino)
                && let Ok(i) = st.inode(LIVE, ino)
                && i.nlink == 0
            {
                st.delete_inode(ino)?;
            }
        }
        let r = f(&mut st);
        // Commit whatever happened, even on error (a failed op may have changed nothing,
        // or a consistent prefix of the change: each tree update is a complete step).
        let c = st.commit();
        let r = r?;
        c?;
        Ok(r)
    }

    /// Delete the given nlink-0 inodes that nothing has open.
    pub fn reclaim(&self, inos: &[u64]) -> Result<()> {
        if inos.is_empty() {
            return Ok(());
        }
        self.txn(|st| {
            for &ino in inos {
                if !self.is_open(ino) && st.inode(LIVE, ino).is_ok_and(|i| i.nlink == 0) {
                    st.delete_inode(ino)?;
                }
            }
            Ok(())
        })
    }

    /// Drop one link to `ino` (its entry is already gone); delete it if that was the last
    /// and nothing has it open.
    fn drop_link(&self, st: &mut State, ino: u64) -> Result<()> {
        let mut i = st.inode(LIVE, ino)?;
        let is_dir = i.mode & S_IFMT_ == crate::fs::node::S_IFDIR;
        i.nlink = if is_dir { 0 } else { i.nlink.saturating_sub(1) };
        i.ctime = now();
        if i.nlink == 0 && !self.is_open(ino) {
            st.delete_inode(ino)
        } else {
            st.put_inode(ino, &i)
        }
    }
}

const S_IFMT_: u32 = crate::fs::node::S_IFMT;

impl FileSystem for Volume {
    fn name(&self) -> &'static str {
        "milkfs"
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn root(&self) -> InodeRef {
        self.root_obj(LIVE)
    }
}

/// A file, directory or symlink in the live tree (`view == 0`) or a snapshot.
pub struct MilkInode {
    vol: Arc<Volume>,
    view: u64,
    ino: u64,
    kind: InodeType,
    dev: u64,
    me: Weak<MilkInode>,
}

impl Drop for MilkInode {
    fn drop(&mut self) {
        if self.view == LIVE && self.kind != InodeType::Dir {
            self.vol.reap.lock().push(self.ino);
        }
    }
}

impl MilkInode {
    fn ro(&self) -> Result<()> {
        if self.view != LIVE {
            return Err(Errno::EROFS);
        }
        Ok(())
    }

    fn is_live_root(&self) -> bool {
        self.view == LIVE && self.ino == ROOT_INO
    }

    /// Check `name` can be created here.
    fn new_name(&self, name: &str) -> Result<()> {
        check_name(name)?;
        if name.len() > 255 {
            return Err(Errno::ENAMETOOLONG);
        }
        if self.is_live_root() && name == SNAPS_NAME {
            return Err(Errno::EEXIST);
        }
        Ok(())
    }

    fn add(&self, name: &str, mode: u32, target: Option<&str>) -> Result<InodeRef> {
        self.ro()?;
        self.new_name(name)?;
        let kind = kind_of(mode);
        let dir = self.ino;
        let ino = self.vol.txn(|st| {
            let mut d = st.inode(LIVE, dir)?;
            if kind_of(d.mode) != InodeType::Dir {
                return Err(Errno::ENOTDIR);
            }
            if st.lookup(LIVE, dir, name)?.is_some() {
                return Err(Errno::EEXIST);
            }
            let ino = st.alloc_ino();
            let t = now();
            let is_dir = kind == InodeType::Dir;
            let mut size = 0;
            if let Some(target) = target {
                st.set_symlink(ino, target)?;
                size = target.len() as u64;
            }
            st.put_inode(
                ino,
                &InodeItem {
                    mode,
                    nlink: if is_dir { 2 } else { 1 },
                    uid: d.uid,
                    gid: d.gid,
                    size,
                    parent: if is_dir { dir } else { 0 },
                    atime: t,
                    mtime: t,
                    ctime: t,
                },
            )?;
            st.add_dirent(
                dir,
                Dirent {
                    ino,
                    dtype: kind.dirent_type(),
                    name: String::from(name),
                },
            )?;
            if is_dir {
                d.nlink += 1;
            }
            d.mtime = t;
            d.ctime = t;
            st.put_inode(dir, &d)?;
            Ok(ino)
        })?;
        Ok(self.vol.inode_obj(LIVE, ino, kind))
    }
}

impl Inode for MilkInode {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn kind(&self) -> InodeType {
        self.kind
    }

    fn stat(&self) -> Stat {
        let st = self.vol.state.lock();
        let i = st.inode(self.view, self.ino).unwrap_or_default();
        let blocks = if self.kind == InodeType::File {
            st.block_count(self.view, self.ino).unwrap_or(0)
        } else {
            0
        };
        drop(st);
        let mut s = Stat::new(self.dev, self.ino, self.kind, i.mode & 0o7777);
        s.nlink = i.nlink;
        s.uid = i.uid;
        s.gid = i.gid;
        s.size = if self.kind == InodeType::Dir {
            BLOCK as u64
        } else {
            i.size
        };
        s.blocks = blocks * (BLOCK as u64 / 512);
        s.atime = ts(i.atime);
        s.mtime = ts(i.mtime);
        s.ctime = ts(i.ctime);
        s
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize> {
        if self.kind != InodeType::File {
            return Err(if self.kind == InodeType::Dir {
                Errno::EISDIR
            } else {
                Errno::EINVAL
            });
        }
        let st = self.vol.state.lock();
        let i = st.inode(self.view, self.ino)?;
        st.read_data(self.view, self.ino, i.size, off, buf)
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> Result<usize> {
        if self.kind != InodeType::File {
            return Err(if self.kind == InodeType::Dir {
                Errno::EISDIR
            } else {
                Errno::EINVAL
            });
        }
        self.ro()?;
        let ino = self.ino;
        self.vol.txn(|st| {
            let mut i = st.inode(LIVE, ino)?;
            i.size = st.write_data(ino, i.size, off, buf)?;
            let t = now();
            i.mtime = t;
            i.ctime = t;
            st.put_inode(ino, &i)?;
            Ok(buf.len())
        })
    }

    fn truncate(&self, len: u64) -> Result<()> {
        if self.kind != InodeType::File {
            return Err(if self.kind == InodeType::Dir {
                Errno::EISDIR
            } else {
                Errno::EINVAL
            });
        }
        self.ro()?;
        if len > super::items::MAX_FILE_SIZE {
            return Err(Errno::EFBIG);
        }
        let ino = self.ino;
        self.vol.txn(|st| {
            let mut i = st.inode(LIVE, ino)?;
            st.truncate_data(ino, i.size, len)?;
            i.size = len;
            let t = now();
            i.mtime = t;
            i.ctime = t;
            st.put_inode(ino, &i)
        })
    }

    fn lookup(&self, name: &str) -> Result<InodeRef> {
        if self.kind != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        if name == ".." {
            if self.view != LIVE && self.ino == ROOT_INO {
                return Ok(Arc::new(SnapsDir::new(self.vol.clone())));
            }
            let parent = self.vol.state.lock().inode(self.view, self.ino)?.parent;
            return Ok(self.vol.inode_obj(self.view, parent, InodeType::Dir));
        }
        if self.is_live_root() && name == SNAPS_NAME {
            return Ok(Arc::new(SnapsDir::new(self.vol.clone())));
        }
        let d = self
            .vol
            .state
            .lock()
            .lookup(self.view, self.ino, name)?
            .ok_or(Errno::ENOENT)?;
        let kind = self
            .vol
            .state
            .lock()
            .inode(self.view, d.ino)
            .map(|i| kind_of(i.mode))?;
        Ok(self.vol.inode_obj(self.view, d.ino, kind))
    }

    fn readdir(&self) -> Result<Vec<DirEntry>> {
        if self.kind != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        let st = self.vol.state.lock();
        st.inode(self.view, self.ino)?; // ENOENT if it went away (rollback)
        let mut out: Vec<DirEntry> = st
            .list(self.view, self.ino)?
            .into_iter()
            .map(|d| DirEntry {
                kind: InodeType::from_mode(match d.dtype {
                    4 => crate::fs::node::S_IFDIR,
                    10 => crate::fs::node::S_IFLNK,
                    2 => crate::fs::node::S_IFCHR,
                    _ => crate::fs::node::S_IFREG,
                })
                .unwrap_or(InodeType::File),
                ino: d.ino,
                name: d.name,
            })
            .collect();
        if self.is_live_root() {
            out.push(DirEntry {
                name: String::from(SNAPS_NAME),
                ino: SNAPS_INO,
                kind: InodeType::Dir,
            });
        }
        Ok(out)
    }

    fn create(&self, name: &str, perm: u32) -> Result<InodeRef> {
        self.add(name, crate::fs::node::S_IFREG | (perm & 0o7777), None)
    }

    fn mkdir(&self, name: &str, perm: u32) -> Result<InodeRef> {
        self.add(name, crate::fs::node::S_IFDIR | (perm & 0o7777), None)
    }

    fn symlink(&self, name: &str, target: &str) -> Result<InodeRef> {
        if target.len() > SYMLINK_MAX {
            return Err(Errno::ENAMETOOLONG);
        }
        self.add(name, crate::fs::node::S_IFLNK | 0o777, Some(target))
    }

    fn link(&self, name: &str, target: &InodeRef) -> Result<()> {
        self.ro()?;
        self.new_name(name)?;
        let t = downcast::<MilkInode>(target).ok_or(Errno::EXDEV)?;
        if !Arc::ptr_eq(&t.vol, &self.vol) || t.view != LIVE {
            return Err(Errno::EXDEV);
        }
        if t.kind == InodeType::Dir {
            return Err(Errno::EPERM);
        }
        let (dir, ino, kind) = (self.ino, t.ino, t.kind);
        self.vol.txn(|st| {
            if st.lookup(LIVE, dir, name)?.is_some() {
                return Err(Errno::EEXIST);
            }
            let mut i = st.inode(LIVE, ino)?;
            i.nlink += 1;
            i.ctime = now();
            st.put_inode(ino, &i)?;
            st.add_dirent(
                dir,
                Dirent {
                    ino,
                    dtype: kind.dirent_type(),
                    name: String::from(name),
                },
            )?;
            let mut d = st.inode(LIVE, dir)?;
            d.mtime = now();
            d.ctime = d.mtime;
            st.put_inode(dir, &d)
        })
    }

    fn unlink(&self, name: &str) -> Result<()> {
        self.ro()?;
        if self.is_live_root() && name == SNAPS_NAME {
            return Err(Errno::EISDIR);
        }
        let dir = self.ino;
        let vol = self.vol.clone();
        self.vol.txn(|st| {
            let d = st.lookup(LIVE, dir, name)?.ok_or(Errno::ENOENT)?;
            if d.dtype == InodeType::Dir.dirent_type() {
                return Err(Errno::EISDIR);
            }
            st.remove_dirent(dir, name)?;
            vol.drop_link(st, d.ino)?;
            let mut di = st.inode(LIVE, dir)?;
            di.mtime = now();
            di.ctime = di.mtime;
            st.put_inode(dir, &di)
        })
    }

    fn rmdir(&self, name: &str) -> Result<()> {
        self.ro()?;
        if self.is_live_root() && name == SNAPS_NAME {
            return Err(Errno::EBUSY);
        }
        let dir = self.ino;
        self.vol.txn(|st| {
            let d = st.lookup(LIVE, dir, name)?.ok_or(Errno::ENOENT)?;
            if d.dtype != InodeType::Dir.dirent_type() {
                return Err(Errno::ENOTDIR);
            }
            if !st.list(LIVE, d.ino)?.is_empty() {
                return Err(Errno::ENOTEMPTY);
            }
            st.remove_dirent(dir, name)?;
            st.delete_inode(d.ino)?;
            let mut di = st.inode(LIVE, dir)?;
            di.nlink -= 1;
            di.mtime = now();
            di.ctime = di.mtime;
            st.put_inode(dir, &di)
        })
    }

    fn rename(&self, old: &str, new_dir: &InodeRef, new: &str) -> Result<()> {
        self.ro()?;
        let nd = downcast::<MilkInode>(new_dir).ok_or(Errno::EXDEV)?;
        if !Arc::ptr_eq(&nd.vol, &self.vol) || nd.view != LIVE {
            return Err(Errno::EXDEV);
        }
        nd.new_name(new)?;
        if self.is_live_root() && old == SNAPS_NAME {
            return Err(Errno::EBUSY);
        }
        let (od, ndi) = (self.ino, nd.ino);
        let vol = self.vol.clone();
        self.vol.txn(|st| {
            let src = st.lookup(LIVE, od, old)?.ok_or(Errno::ENOENT)?;
            let dst = st.lookup(LIVE, ndi, new)?;
            let dir_t = InodeType::Dir.dirent_type();
            let moving_dir = src.dtype == dir_t;
            if let Some(dst) = &dst {
                if dst.ino == src.ino {
                    return Ok(());
                }
                match (moving_dir, dst.dtype == dir_t) {
                    (true, false) => return Err(Errno::ENOTDIR),
                    (false, true) => return Err(Errno::EISDIR),
                    (true, true) if !st.list(LIVE, dst.ino)?.is_empty() => {
                        return Err(Errno::ENOTEMPTY);
                    }
                    _ => {}
                }
            }
            if moving_dir {
                // Not into its own subtree: walk up from the target directory.
                let mut cur = ndi;
                loop {
                    if cur == src.ino {
                        return Err(Errno::EINVAL);
                    }
                    if cur == ROOT_INO {
                        break;
                    }
                    cur = st.inode(LIVE, cur)?.parent;
                }
            }
            let t = now();
            st.remove_dirent(od, old)?;
            if let Some(dst) = &dst {
                st.remove_dirent(ndi, new)?;
                if dst.dtype == dir_t {
                    st.delete_inode(dst.ino)?;
                    let mut n = st.inode(LIVE, ndi)?;
                    n.nlink -= 1;
                    st.put_inode(ndi, &n)?;
                } else {
                    vol.drop_link(st, dst.ino)?;
                }
            }
            st.add_dirent(
                ndi,
                Dirent {
                    ino: src.ino,
                    dtype: src.dtype,
                    name: String::from(new),
                },
            )?;
            let mut s = st.inode(LIVE, src.ino)?;
            s.ctime = t;
            if moving_dir && od != ndi {
                s.parent = ndi;
                let mut o = st.inode(LIVE, od)?;
                o.nlink -= 1;
                st.put_inode(od, &o)?;
                let mut n = st.inode(LIVE, ndi)?;
                n.nlink += 1;
                st.put_inode(ndi, &n)?;
            }
            st.put_inode(src.ino, &s)?;
            for d in [od, ndi] {
                let mut i = st.inode(LIVE, d)?;
                i.mtime = t;
                i.ctime = t;
                st.put_inode(d, &i)?;
            }
            Ok(())
        })
    }

    fn readlink(&self) -> Result<String> {
        if self.kind != InodeType::Symlink {
            return Err(Errno::EINVAL);
        }
        self.vol.state.lock().symlink_target(self.view, self.ino)
    }

    fn setattr(&self, a: &SetAttr) -> Result<()> {
        self.ro()?;
        let ino = self.ino;
        let a = *a;
        self.vol.txn(|st| {
            let mut i = st.inode(LIVE, ino)?;
            if let Some(p) = a.perm {
                i.mode = (i.mode & S_IFMT_) | (p & 0o7777);
            }
            if let Some(u) = a.uid {
                i.uid = u;
            }
            if let Some(g) = a.gid {
                i.gid = g;
            }
            if let Some(t) = a.atime {
                i.atime = (t.sec, t.nsec as u32);
            }
            if let Some(t) = a.mtime {
                i.mtime = (t.sec, t.nsec as u32);
            }
            i.ctime = now();
            st.put_inode(ino, &i)
        })
    }
}

/// `<volume>/.snapshots`: one read-only directory per snapshot.
pub struct SnapsDir {
    vol: Arc<Volume>,
}

impl SnapsDir {
    pub fn new(vol: Arc<Volume>) -> SnapsDir {
        SnapsDir { vol }
    }
}

impl Inode for SnapsDir {
    fn ino(&self) -> u64 {
        SNAPS_INO
    }
    fn dev(&self) -> u64 {
        self.vol.dev
    }
    fn kind(&self) -> InodeType {
        InodeType::Dir
    }
    fn stat(&self) -> Stat {
        let mut s = Stat::new(self.vol.dev, SNAPS_INO, InodeType::Dir, 0o555);
        let n = self.vol.state.lock().snaps.len() as u32;
        s.nlink = 2 + n;
        s.size = BLOCK as u64;
        s
    }
    fn lookup(&self, name: &str) -> Result<InodeRef> {
        if name == ".." {
            return Ok(self.vol.root_obj(LIVE));
        }
        let generation = self
            .vol
            .state
            .lock()
            .snaps
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.generation)
            .ok_or(Errno::ENOENT)?;
        Ok(self.vol.root_obj(generation))
    }
    fn readdir(&self) -> Result<Vec<DirEntry>> {
        Ok(self
            .vol
            .state
            .lock()
            .snaps
            .iter()
            .map(|s| DirEntry {
                name: s.name.clone(),
                ino: ROOT_INO,
                kind: InodeType::Dir,
            })
            .collect())
    }
}
