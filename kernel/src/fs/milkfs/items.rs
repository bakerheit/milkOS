//! Typed access to the items of a volume's trees: inodes, directory entries, extents,
//! symlink targets, and file data on top of extents (one extent item per 4 KiB block).
//!
//! Reads take a *view*: 0 is the live tree, any other value is the generation of a
//! snapshot (read-only). Writes always go to the live tree and leave a transaction open;
//! the caller commits.

use alloc::string::String;
use alloc::vec::Vec;

use super::btree::{self, Child};
use super::format::*;
use super::store::Store;
use super::volume::State;
use crate::fs::errno::{Errno, Result};

pub const LIVE: u64 = 0;
/// Views with this bit are clones (low 32 bits = clone id); other non-zero views are
/// snapshots (by generation, always far below 2^63).
pub const CLONE_VIEW: u64 = 1 << 63;

pub const fn clone_view(id: u32) -> u64 {
    CLONE_VIEW | id as u64
}

pub const fn is_clone_view(view: u64) -> bool {
    view & CLONE_VIEW != 0
}

/// A view that can be written: the live tree or a clone.
pub const fn is_writable_view(view: u64) -> bool {
    view == LIVE || is_clone_view(view)
}

/// Largest file milkfs v0 accepts.
pub const MAX_FILE_SIZE: u64 = 1 << 32;

fn inode_key(ino: u64) -> Key {
    Key::new(ino, K_INODE, 0)
}

impl State {
    /// Run `f` on the root of `view`.
    pub fn view<R>(&self, view: u64, f: impl FnOnce(&Child, &Store) -> Result<R>) -> Result<R> {
        if view == LIVE {
            return f(&self.tree, &self.store);
        }
        if is_clone_view(view) {
            let id = view as u32;
            if let Some(t) = self.clone_trees.get(&id) {
                return f(t, &self.store);
            }
            let c = self
                .clones
                .iter()
                .find(|c| c.id == id)
                .ok_or(Errno::ENOENT)?;
            return f(&Child::Disk(c.root), &self.store);
        }
        let s = self
            .snaps
            .iter()
            .find(|s| s.generation == view)
            .ok_or(Errno::ENOENT)?;
        f(&Child::Disk(s.root), &self.store)
    }

    /// Direct writes at `view` (live or a clone), with the matching release rules.
    pub fn set_cur(&mut self, view: u64) -> Result<()> {
        if view == LIVE {
            self.cur = LIVE;
            self.store.clone_origin = None;
            return Ok(());
        }
        if !is_clone_view(view) {
            return Err(Errno::EROFS);
        }
        let c = self
            .clones
            .iter()
            .find(|c| c.id == view as u32)
            .ok_or(Errno::ENOENT)?;
        self.store.clone_origin = Some(c.generation);
        self.cur = view;
        Ok(())
    }

    /// The tree writes go to, plus the store (split borrow).
    fn wtree(&mut self) -> Result<(&mut Child, &mut Store)> {
        self.check_writable()?;
        if self.cur != LIVE {
            let id = self.cur as u32;
            if !self.clone_trees.contains_key(&id) {
                let root = self
                    .clones
                    .iter()
                    .find(|c| c.id == id)
                    .ok_or(Errno::ENOENT)?
                    .root;
                self.clone_trees.insert(id, Child::Disk(root));
            }
        }
        let State {
            tree,
            clone_trees,
            store,
            cur,
            ..
        } = self;
        let t = if *cur == LIVE {
            tree
        } else {
            clone_trees.get_mut(&(*cur as u32)).ok_or(Errno::ENOENT)?
        };
        Ok((t, store))
    }

    fn put(&mut self, key: Key, val: Vec<u8>) -> Result<()> {
        let (t, st) = self.wtree()?;
        btree::insert(t, st, key, val)
    }

    fn del(&mut self, key: &Key) -> Result<bool> {
        let (t, st) = self.wtree()?;
        btree::remove(t, st, key)
    }

    pub fn alloc_ino(&mut self) -> u64 {
        let i = self.next_ino;
        self.next_ino += 1;
        i
    }

    // ---- inodes ------------------------------------------------------------------

    pub fn inode(&self, view: u64, ino: u64) -> Result<InodeItem> {
        let v = self
            .view(view, |r, s| btree::get(r, s, &inode_key(ino)))?
            .ok_or(Errno::ENOENT)?;
        InodeItem::decode(&v)
    }

    pub fn put_inode(&mut self, ino: u64, i: &InodeItem) -> Result<()> {
        self.put(inode_key(ino), i.encode())
    }

    /// Remove an inode and everything it owns (extents, symlink target).
    pub fn delete_inode(&mut self, ino: u64) -> Result<()> {
        for (idx, p) in self.extents(self.cur, ino, 0)? {
            self.del(&Key::new(ino, K_EXTENT, idx))?;
            self.store.release(p);
        }
        self.del(&Key::new(ino, K_SYMLINK, 0))?;
        self.del(&inode_key(ino))?;
        Ok(())
    }

    // ---- directories -------------------------------------------------------------

    fn dirent_bucket(&self, view: u64, dir: u64, name: &str) -> Result<(Key, Vec<Dirent>)> {
        let key = Key::new(dir, K_DIRENT, name_hash(name.as_bytes()));
        let list = match self.view(view, |r, s| btree::get(r, s, &key))? {
            Some(v) => decode_dirents(&v)?,
            None => Vec::new(),
        };
        Ok((key, list))
    }

    pub fn lookup(&self, view: u64, dir: u64, name: &str) -> Result<Option<Dirent>> {
        let (_, list) = self.dirent_bucket(view, dir, name)?;
        Ok(list.into_iter().find(|d| d.name == name))
    }

    /// Entries of `dir`, in hash order.
    pub fn list(&self, view: u64, dir: u64) -> Result<Vec<Dirent>> {
        let mut items = Vec::new();
        self.view(view, |r, s| {
            btree::range(
                r,
                s,
                &Key::new(dir, K_DIRENT, 0),
                &Key::new(dir, K_DIRENT + 1, 0),
                &mut items,
            )
        })?;
        let mut out = Vec::new();
        for (_, v) in items {
            out.extend(decode_dirents(&v)?);
        }
        Ok(out)
    }

    pub fn add_dirent(&mut self, dir: u64, d: Dirent) -> Result<()> {
        let (key, mut list) = self.dirent_bucket(self.cur, dir, &d.name)?;
        if list.iter().any(|x| x.name == d.name) {
            return Err(Errno::EEXIST);
        }
        list.push(d);
        self.put(key, encode_dirents(&list))
    }

    pub fn remove_dirent(&mut self, dir: u64, name: &str) -> Result<()> {
        let (key, mut list) = self.dirent_bucket(self.cur, dir, name)?;
        let before = list.len();
        list.retain(|x| x.name != name);
        if list.len() == before {
            return Err(Errno::ENOENT);
        }
        if list.is_empty() {
            self.del(&key)?;
            Ok(())
        } else {
            self.put(key, encode_dirents(&list))
        }
    }

    // ---- symlinks ----------------------------------------------------------------

    pub fn symlink_target(&self, view: u64, ino: u64) -> Result<String> {
        let v = self
            .view(view, |r, s| btree::get(r, s, &Key::new(ino, K_SYMLINK, 0)))?
            .ok_or(Errno::EINVAL)?;
        Ok(String::from_utf8_lossy(&v).into_owned())
    }

    pub fn set_symlink(&mut self, ino: u64, target: &str) -> Result<()> {
        if target.len() > SYMLINK_MAX {
            return Err(Errno::ENAMETOOLONG);
        }
        self.put(Key::new(ino, K_SYMLINK, 0), target.as_bytes().to_vec())
    }

    // ---- extents and data --------------------------------------------------------

    /// `(block index, pointer)` for every extent of `ino` at index >= `from`.
    pub fn extents(&self, view: u64, ino: u64, from: u64) -> Result<Vec<(u64, BlockPtr)>> {
        let mut items = Vec::new();
        self.view(view, |r, s| {
            btree::range(
                r,
                s,
                &Key::new(ino, K_EXTENT, from),
                &Key::new(ino, K_EXTENT + 1, 0),
                &mut items,
            )
        })?;
        Ok(items
            .iter()
            .filter_map(|(k, v)| btree::extent_ptr(k, v).map(|p| (k.off, p)))
            .collect())
    }

    fn extent(&self, view: u64, ino: u64, idx: u64) -> Result<Option<BlockPtr>> {
        let k = Key::new(ino, K_EXTENT, idx);
        Ok(self
            .view(view, |r, s| btree::get(r, s, &k))?
            .and_then(|v| btree::extent_ptr(&k, &v)))
    }

    fn set_extent(&mut self, ino: u64, idx: u64, p: BlockPtr) -> Result<()> {
        let mut v = alloc::vec![0u8; PTR_SIZE];
        p.put(&mut v);
        self.put(Key::new(ino, K_EXTENT, idx), v)
    }

    /// Read file data; `size` is the inode's size. Holes read as zeros.
    pub fn read_data(
        &self,
        view: u64,
        ino: u64,
        size: u64,
        off: u64,
        buf: &mut [u8],
    ) -> Result<usize> {
        if off >= size {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(size - off) as usize;
        let mut block = [0u8; BLOCK];
        let mut done = 0;
        while done < n {
            let pos = off + done as u64;
            let (idx, in_blk) = (pos / BLOCK as u64, (pos % BLOCK as u64) as usize);
            let chunk = (BLOCK - in_blk).min(n - done);
            match self.extent(view, ino, idx)? {
                Some(p) => {
                    self.store.read(p, &mut block)?;
                    buf[done..done + chunk].copy_from_slice(&block[in_blk..in_blk + chunk]);
                }
                None => buf[done..done + chunk].fill(0),
            }
            done += chunk;
        }
        Ok(n)
    }

    /// Copy-on-write file data: every touched block is written to a new block. Returns the
    /// new size. Leaves the transaction open.
    pub fn write_data(&mut self, ino: u64, size: u64, off: u64, buf: &[u8]) -> Result<u64> {
        let end = off.checked_add(buf.len() as u64).ok_or(Errno::EFBIG)?;
        if end > MAX_FILE_SIZE {
            return Err(Errno::EFBIG);
        }
        let mut block = [0u8; BLOCK];
        let mut done = 0;
        while done < buf.len() {
            let pos = off + done as u64;
            let (idx, in_blk) = (pos / BLOCK as u64, (pos % BLOCK as u64) as usize);
            let chunk = (BLOCK - in_blk).min(buf.len() - done);
            let old = self.extent(self.cur, ino, idx)?;
            if chunk < BLOCK {
                // Partial block: start from the old contents (or zeros, within EOF only).
                match old {
                    Some(p) if idx * (BLOCK as u64) < size => self.store.read(p, &mut block)?,
                    _ => block.fill(0),
                }
                // Bytes past the old EOF must read as zeros.
                let eof_in = size.saturating_sub(idx * BLOCK as u64);
                if eof_in < BLOCK as u64 {
                    block[eof_in as usize..].fill(0);
                }
            }
            block[in_blk..in_blk + chunk].copy_from_slice(&buf[done..done + chunk]);
            let p = self.store.write_new(&block)?;
            self.set_extent(ino, idx, p)?;
            if let Some(o) = old {
                self.store.release(o);
            }
            done += chunk;
        }
        Ok(size.max(end))
    }

    /// Drop data past `len` (and zero the tail of the last kept block).
    pub fn truncate_data(&mut self, ino: u64, size: u64, len: u64) -> Result<()> {
        if len >= size {
            return Ok(()); // growing: the new range is a hole
        }
        let keep = len.div_ceil(BLOCK as u64);
        for (idx, p) in self.extents(self.cur, ino, keep)? {
            self.del(&Key::new(ino, K_EXTENT, idx))?;
            self.store.release(p);
        }
        let tail = (len % BLOCK as u64) as usize;
        if tail != 0
            && let Some(p) = self.extent(self.cur, ino, len / BLOCK as u64)?
        {
            let mut block = [0u8; BLOCK];
            self.store.read(p, &mut block)?;
            block[tail..].fill(0);
            let np = self.store.write_new(&block)?;
            self.set_extent(ino, len / BLOCK as u64, np)?;
            self.store.release(p);
        }
        Ok(())
    }

    /// A small file's data stored inline in its inode's leaf (None: data is in extents).
    pub fn inline_bytes(&self, _view: u64, _ino: u64) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Number of data blocks `ino` has (for `st_blocks`).
    pub fn block_count(&self, view: u64, ino: u64) -> Result<u64> {
        Ok(self.extents(view, ino, 0)?.len() as u64)
    }
}
