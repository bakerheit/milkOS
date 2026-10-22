//! Namespace and file operations on the view writes go to (`State::cur`): create, link,
//! unlink, rmdir, rename, write, truncate, and dedupe. The kernel's VFS objects
//! (`kernel/src/fs/milkfs/inode.rs`) run these inside their transaction (`Volume::txn`,
//! which picks the view and commits when the batch is due) after the VFS-level checks
//! (names, read-only views, `.snapshots`); the host tests use them as their file API.
//!
//! Inodes are found by number; a caller that keeps inodes alive after their last link is
//! gone (open files) says so with `is_open(view, ino)`: those are put on the orphan list
//! instead of being deleted (see `drop_link`).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use super::format::*;
use super::volume::State;
use crate::abi::{InodeType, S_IFDIR, S_IFMT};
use crate::errno::{Errno, Result};

fn now() -> (i64, u32) {
    let t = crate::platform::now();
    (t.sec, t.nsec as u32)
}

fn kind_of(mode: u32) -> InodeType {
    InodeType::from_mode(mode).unwrap_or(InodeType::File)
}

/// What [`State::dedupe`] did.
#[derive(Clone, Debug, Default)]
pub struct DedupeReport {
    /// Regular files with data blocks looked at.
    pub files: usize,
    /// Groups of two or more files with the same size and content hash.
    pub groups: usize,
    /// Files now sharing the blocks of the first file of their group.
    pub deduped: usize,
    /// Files that already shared them.
    pub already: usize,
    /// Same hash but different bytes (left alone).
    pub differ: usize,
    /// Data blocks the deduped files now share instead of owning.
    pub blocks: u64,
    /// Net change of free blocks once committed (blocks a snapshot still has stay used
    /// until that snapshot is deleted).
    pub freed: i64,
}

impl State {
    /// Create `name` in directory `dir`: a file, directory or symlink by `mode` (`target`
    /// for a symlink). The name must be free. Returns the new inode number.
    pub fn create_node(
        &mut self,
        dir: u64,
        name: &str,
        mode: u32,
        target: Option<&str>,
    ) -> Result<u64> {
        let kind = kind_of(mode);
        let st = self;
        let mut d = st.inode(st.cur, dir)?;
        if kind_of(d.mode) != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        if st.lookup(st.cur, dir, name)?.is_some() {
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
                // Files record their (first) directory too: diff and versions use it
                // to find a path without scanning.
                parent: dir,
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
    }

    /// Drop one link to `ino` (its entry is already gone); delete it if that was the last
    /// and it isn't open, else put it on the orphan list.
    pub fn drop_link(&mut self, ino: u64, is_open: &dyn Fn(u64, u64) -> bool) -> Result<()> {
        let st = self;
        let mut i = st.inode(st.cur, ino)?;
        let is_dir = i.mode & S_IFMT == S_IFDIR;
        i.nlink = if is_dir { 0 } else { i.nlink.saturating_sub(1) };
        i.ctime = now();
        if i.nlink == 0 && !is_open(st.cur, ino) {
            st.delete_inode(ino)
        } else {
            if i.nlink == 0 {
                st.add_orphan(ino)?;
            }
            st.put_inode(ino, &i)
        }
    }

    /// A new hard link `name` in `dir` to `ino` (not a directory; `kind` is its type).
    pub fn link_node(&mut self, dir: u64, name: &str, ino: u64, kind: InodeType) -> Result<()> {
        let st = self;
        if st.lookup(st.cur, dir, name)?.is_some() {
            return Err(Errno::EEXIST);
        }
        let mut i = st.inode(st.cur, ino)?;
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
        let mut d = st.inode(st.cur, dir)?;
        d.mtime = now();
        d.ctime = d.mtime;
        st.put_inode(dir, &d)
    }

    /// Remove entry `name` (not a directory) from `dir` and drop its link.
    pub fn unlink_node(
        &mut self,
        dir: u64,
        name: &str,
        is_open: &dyn Fn(u64, u64) -> bool,
    ) -> Result<()> {
        let st = self;
        let d = st.lookup(st.cur, dir, name)?.ok_or(Errno::ENOENT)?;
        if d.dtype == InodeType::Dir.dirent_type() {
            return Err(Errno::EISDIR);
        }
        st.remove_dirent(dir, name)?;
        st.drop_link(d.ino, is_open)?;
        let mut di = st.inode(st.cur, dir)?;
        di.mtime = now();
        di.ctime = di.mtime;
        st.put_inode(dir, &di)
    }

    /// Remove the empty directory `name` from `dir`.
    pub fn rmdir_node(&mut self, dir: u64, name: &str) -> Result<()> {
        let st = self;
        let d = st.lookup(st.cur, dir, name)?.ok_or(Errno::ENOENT)?;
        if d.dtype != InodeType::Dir.dirent_type() {
            return Err(Errno::ENOTDIR);
        }
        if !st.list(st.cur, d.ino)?.is_empty() {
            return Err(Errno::ENOTEMPTY);
        }
        st.remove_dirent(dir, name)?;
        st.delete_inode(d.ino)?;
        let mut di = st.inode(st.cur, dir)?;
        di.nlink -= 1;
        di.mtime = now();
        di.ctime = di.mtime;
        st.put_inode(dir, &di)
    }

    /// Rename `old` in `od` to `new` in `ndi`, replacing what is there (a file, or an
    /// empty directory when moving a directory).
    pub fn rename_node(
        &mut self,
        od: u64,
        old: &str,
        ndi: u64,
        new: &str,
        is_open: &dyn Fn(u64, u64) -> bool,
    ) -> Result<()> {
        let st = self;
        let src = st.lookup(st.cur, od, old)?.ok_or(Errno::ENOENT)?;
        let dst = st.lookup(st.cur, ndi, new)?;
        let dir_t = InodeType::Dir.dirent_type();
        let moving_dir = src.dtype == dir_t;
        if let Some(dst) = &dst {
            if dst.ino == src.ino {
                return Ok(());
            }
            match (moving_dir, dst.dtype == dir_t) {
                (true, false) => return Err(Errno::ENOTDIR),
                (false, true) => return Err(Errno::EISDIR),
                (true, true) if !st.list(st.cur, dst.ino)?.is_empty() => {
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
                cur = st.inode(st.cur, cur)?.parent;
            }
        }
        let t = now();
        st.remove_dirent(od, old)?;
        if let Some(dst) = &dst {
            st.remove_dirent(ndi, new)?;
            if dst.dtype == dir_t {
                st.delete_inode(dst.ino)?;
                let mut n = st.inode(st.cur, ndi)?;
                n.nlink -= 1;
                st.put_inode(ndi, &n)?;
            } else {
                st.drop_link(dst.ino, is_open)?;
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
        let mut s = st.inode(st.cur, src.ino)?;
        s.ctime = t;
        s.parent = ndi;
        if moving_dir && od != ndi {
            let mut o = st.inode(st.cur, od)?;
            o.nlink -= 1;
            st.put_inode(od, &o)?;
            let mut n = st.inode(st.cur, ndi)?;
            n.nlink += 1;
            st.put_inode(ndi, &n)?;
        }
        st.put_inode(src.ino, &s)?;
        for d in [od, ndi] {
            let mut i = st.inode(st.cur, d)?;
            i.mtime = t;
            i.ctime = t;
            st.put_inode(d, &i)?;
        }
        Ok(())
    }

    /// Write `buf` at `off` into file `ino` (size and times updated). Returns `buf.len()`.
    pub fn write_file(&mut self, ino: u64, off: u64, buf: &[u8]) -> Result<usize> {
        let st = self;
        let mut i = st.inode(st.cur, ino)?;
        i.size = st.write_data(ino, i.size, off, buf)?;
        let t = now();
        i.mtime = t;
        i.ctime = t;
        st.put_inode(ino, &i)?;
        Ok(buf.len())
    }

    /// Set file `ino`'s size to `len` (dropping or zero-extending), times updated.
    pub fn truncate_file(&mut self, ino: u64, len: u64) -> Result<()> {
        let st = self;
        let mut i = st.inode(st.cur, ino)?;
        st.truncate_data(ino, i.size, len)?;
        i.size = len;
        let t = now();
        i.mtime = t;
        i.ctime = t;
        st.put_inode(ino, &i)
    }

    /// `dst`'s contents become a copy-on-write copy of `src`'s (`FICLONE`); both regular
    /// files in the current view.
    pub fn reflink_file(&mut self, src: u64, dst: u64) -> Result<()> {
        let st = self;
        let size = st.reflink(src, dst)?;
        let mut i = st.inode(st.cur, dst)?;
        i.size = size;
        let t = now();
        i.mtime = t;
        i.ctime = t;
        st.put_inode(dst, &i)
    }

    /// Find regular files under directory `top` (recursively, the current view) with
    /// identical contents and make all but the first of each group share its blocks
    /// (reflink). Files are grouped by size and content hash (from the block checksums, no
    /// data read), then compared byte by byte before anything changes. Inodes (times,
    /// owner) are left as they are. `dry_run`: only report (nothing is written). `freed`
    /// is left to the caller (it needs a commit).
    pub fn dedupe(&mut self, top: u64, dry_run: bool, rep: &mut DedupeReport) -> Result<()> {
        let st = self;
        let v = st.cur;
        // Every regular file under `top` that has data blocks, by (size, hash).
        let mut groups: BTreeMap<(u64, u64), Vec<u64>> = BTreeMap::new();
        let mut dirs = alloc::vec![top];
        while let Some(dir) = dirs.pop() {
            for e in st.list(v, dir)? {
                if e.dtype == InodeType::Dir.dirent_type() {
                    dirs.push(e.ino);
                } else if e.dtype == InodeType::File.dirent_type() && st.block_count(v, e.ino)? > 0
                {
                    let (h, i) = st.content_hash(v, e.ino)?;
                    groups.entry((i.size, h)).or_default().push(e.ino);
                    rep.files += 1;
                }
            }
        }
        let mut a = alloc::vec![0u8; 1 << 20];
        let mut b = alloc::vec![0u8; 1 << 20];
        for ((size, _), mut inos) in groups {
            inos.sort_unstable();
            inos.dedup(); // hard links
            if inos.len() < 2 {
                continue;
            }
            rep.groups += 1;
            let keep = inos[0];
            for &other in &inos[1..] {
                if st.same_blocks(keep, other)? {
                    rep.already += 1;
                    continue;
                }
                let mut off = 0;
                let mut equal = true;
                while equal && off < size {
                    let n = st.read_data(v, keep, size, off, &mut a)?;
                    st.read_data(v, other, size, off, &mut b[..n])?;
                    equal = a[..n] == b[..n];
                    off += n as u64;
                }
                if !equal {
                    rep.differ += 1;
                    continue;
                }
                rep.deduped += 1;
                rep.blocks += st.block_count(v, other)?;
                if !dry_run {
                    st.reflink(keep, other)?;
                }
            }
        }
        Ok(())
    }
}
