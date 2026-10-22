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
use crate::errno::{Errno, Result};

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

    pub(super) fn put(&mut self, key: Key, val: Vec<u8>) -> Result<()> {
        let (t, st) = self.wtree()?;
        btree::insert(t, st, key, val)
    }

    pub(super) fn del(&mut self, key: &Key) -> Result<bool> {
        let (t, st) = self.wtree()?;
        btree::remove(t, st, key)
    }

    /// Raw item access for the self-test's B+tree model test (live tree).
    pub fn test_put(&mut self, key: Key, val: Vec<u8>) -> Result<()> {
        self.put(key, val)
    }

    pub fn test_del(&mut self, key: &Key) -> Result<bool> {
        self.del(key)
    }

    pub fn test_items(&self, lo: Key, hi: Key) -> Result<Vec<(Key, Vec<u8>)>> {
        let mut out = Vec::new();
        self.view(LIVE, |r, s| btree::range(r, s, &lo, &hi, &mut out))?;
        Ok(out)
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
        for e in self.exts(self.cur, ino, 0, u64::MAX)? {
            self.del(&e.key(ino))?;
            self.release_ext(&e, 0, e.csums.len())?;
        }
        self.del(&Key::new(ino, K_INLINE, 0))?;
        self.del(&Key::new(ino, K_SYMLINK, 0))?;
        self.del(&inode_key(ino))?;
        self.del_orphan(ino)?;
        Ok(())
    }

    /// Put `ino` (no links left, still open) on the orphan list: mount finishes deleting
    /// it if the machine goes down before the last close does.
    pub fn add_orphan(&mut self, ino: u64) -> Result<()> {
        self.put(Key::new(VOL_OBJ, K_ORPHAN, ino), Vec::new())
    }

    pub fn del_orphan(&mut self, ino: u64) -> Result<()> {
        self.del(&Key::new(VOL_OBJ, K_ORPHAN, ino)).map(|_| ())
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
    //
    // A file's data is either inline (`K_INLINE`, files up to INLINE_MAX bytes that never
    // grew past it) or in extents: `K_EXT` runs of up to MAX_RUN consecutive blocks, plus,
    // on volumes from format v0, one-block `K_EXTENT` items. Writes are copy-on-write:
    // the touched block range is written to newly allocated (as contiguous as possible)
    // blocks, and the old extents overlapping it are trimmed around it.

    /// Every extent of `ino` overlapping file blocks `from..to`, in file order.
    pub fn exts(&self, view: u64, ino: u64, from: u64, to: u64) -> Result<Vec<Ext>> {
        let mut items = Vec::new();
        self.view(view, |r, s| {
            btree::range(
                r,
                s,
                &Key::new(ino, K_EXTENT, from),
                &Key::new(ino, K_EXTENT, to),
                &mut items,
            )?;
            btree::range(
                r,
                s,
                &Key::new(ino, K_EXT, from.saturating_sub(MAX_RUN - 1)),
                &Key::new(ino, K_EXT, to),
                &mut items,
            )
        })?;
        let mut out: Vec<Ext> = items
            .iter()
            .filter_map(|(k, v)| Ext::decode(k, v))
            .filter(|e| e.end() > from && e.start < to)
            .collect();
        out.sort_by_key(|e| e.start);
        Ok(out)
    }

    /// `(file block, pointer)` for every data block of `ino` at block >= `from`.
    pub fn extents(&self, view: u64, ino: u64, from: u64) -> Result<Vec<(u64, BlockPtr)>> {
        let mut v = Vec::new();
        for e in self.exts(view, ino, from, u64::MAX)? {
            for i in 0..e.csums.len() {
                let idx = e.start + i as u64;
                if idx >= from {
                    v.push((idx, e.ptr(i)));
                }
            }
        }
        Ok(v)
    }

    /// A small file's data stored inline (None: its data, if any, is in extents).
    pub fn inline_bytes(&self, view: u64, ino: u64) -> Result<Option<Vec<u8>>> {
        self.view(view, |r, s| btree::get(r, s, &Key::new(ino, K_INLINE, 0)))
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
        let out = &mut buf[..n];
        out.fill(0);
        if let Some(d) = self.inline_bytes(view, ino)? {
            if (off as usize) < d.len() {
                let m = (d.len() - off as usize).min(n);
                out[..m].copy_from_slice(&d[off as usize..off as usize + m]);
            }
            return Ok(n);
        }
        let b = BLOCK as u64;
        let (b0, b1) = (off / b, (off + n as u64).div_ceil(b));
        let mut tmp = Vec::new();
        for e in self.exts(view, ino, b0, b1)? {
            // The part of this run we need, read in one go.
            let (s, t) = (e.start.max(b0), e.end().min(b1));
            let first = (s - e.start) as usize;
            let count = (t - s) as usize;
            tmp.resize(count * BLOCK, 0);
            self.store.read_run(
                e.block + first as u64,
                e.birth,
                &e.csums[first..first + count],
                &mut tmp,
            )?;
            // Copy the overlap of [s*B, t*B) and [off, off+n).
            let lo = (s * b).max(off);
            let hi = (t * b).min(off + n as u64);
            out[(lo - off) as usize..(hi - off) as usize]
                .copy_from_slice(&tmp[(lo - s * b) as usize..(hi - s * b) as usize]);
        }
        Ok(n)
    }

    /// Copy-on-write file data. Returns the new size. Leaves the transaction open.
    pub fn write_data(&mut self, ino: u64, size: u64, off: u64, buf: &[u8]) -> Result<u64> {
        let end = off.checked_add(buf.len() as u64).ok_or(Errno::EFBIG)?;
        if end > MAX_FILE_SIZE {
            return Err(Errno::EFBIG);
        }
        let new_size = size.max(end);
        let inline = self.inline_bytes(self.cur, ino)?;
        let empty = size == 0 && self.exts(self.cur, ino, 0, 1)?.is_empty();
        if new_size as usize <= INLINE_MAX && (inline.is_some() || empty) {
            let mut d = inline.unwrap_or_default();
            d.resize(new_size as usize, 0);
            d[off as usize..end as usize].copy_from_slice(buf);
            self.put(Key::new(ino, K_INLINE, 0), d)?;
            return Ok(new_size);
        }
        if let Some(d) = inline {
            // Outgrowing inline: move the data to extents first.
            self.del(&Key::new(ino, K_INLINE, 0))?;
            if !d.is_empty() {
                self.write_extents(ino, d.len() as u64, 0, &d)?;
            }
        }
        self.write_extents(ino, size, off, buf)?;
        Ok(new_size)
    }

    fn write_extents(&mut self, ino: u64, size: u64, off: u64, buf: &[u8]) -> Result<()> {
        let b = BLOCK as u64;
        let end = off + buf.len() as u64;
        let (b0, b1) = (off / b, end.div_ceil(b));
        let n = (b1 - b0) as usize;
        let mut data = alloc::vec![0u8; n * BLOCK];
        // Partial edge blocks start from their old contents (zeros past the old EOF).
        if !off.is_multiple_of(b) {
            self.read_data(self.cur, ino, size, b0 * b, &mut data[..BLOCK])?;
        }
        if !end.is_multiple_of(b) && (n > 1 || off.is_multiple_of(b)) {
            self.read_data(
                self.cur,
                ino,
                size,
                (b1 - 1) * b,
                &mut data[(n - 1) * BLOCK..],
            )?;
        }
        data[(off - b0 * b) as usize..(end - b0 * b) as usize].copy_from_slice(buf);
        self.drop_range(ino, b0, b1)?;
        let mut i = 0;
        while i < n {
            let (start, got) = self.store.alloc_run((n - i).min(MAX_RUN as usize))?;
            let mut csums = Vec::with_capacity(got);
            for j in 0..got {
                let chunk = &mut data[(i + j) * BLOCK..(i + j + 1) * BLOCK];
                let b = start + j as u64;
                csums.push(self.store.seal(b, self.store.txn, chunk));
                self.store.cache.write(b, (&*chunk).try_into().unwrap())?;
            }
            let e = Ext {
                start: b0 + i as u64,
                block: start,
                birth: self.store.txn,
                csums,
                legacy: false,
            };
            self.put(e.key(ino), e.encode())?;
            i += got;
        }
        Ok(())
    }

    /// Remove file blocks `from..to` from `ino`'s extents (releasing those blocks),
    /// keeping the parts of runs outside the range.
    pub fn drop_range(&mut self, ino: u64, from: u64, to: u64) -> Result<()> {
        for e in self.exts(self.cur, ino, from, to)? {
            self.del(&e.key(ino))?;
            let i0 = from.saturating_sub(e.start) as usize;
            let i1 = (to.min(e.end()) - e.start) as usize;
            self.release_ext(&e, i0, i1)?;
            if e.start < from {
                let k = (from - e.start) as usize;
                let left = Ext {
                    csums: e.csums[..k].to_vec(),
                    legacy: false,
                    ..e.clone()
                };
                self.put(left.key(ino), left.encode())?;
            }
            if e.end() > to {
                let k = (to - e.start) as usize;
                let right = Ext {
                    start: to,
                    block: e.block + k as u64,
                    csums: e.csums[k..].to_vec(),
                    legacy: false,
                    ..e.clone()
                };
                self.put(right.key(ino), right.encode())?;
            }
        }
        Ok(())
    }

    /// Set the file's data to `len` bytes (dropping or zero-extending).
    pub fn truncate_data(&mut self, ino: u64, size: u64, len: u64) -> Result<()> {
        if let Some(mut d) = self.inline_bytes(self.cur, ino)? {
            if len as usize <= INLINE_MAX {
                d.resize(len as usize, 0);
                return self.put(Key::new(ino, K_INLINE, 0), d);
            }
            self.del(&Key::new(ino, K_INLINE, 0))?;
            if !d.is_empty() {
                self.write_extents(ino, d.len() as u64, 0, &d)?;
            }
            return Ok(()); // growing: the rest is a hole
        }
        if len >= size {
            return Ok(());
        }
        let b = BLOCK as u64;
        let keep = len.div_ceil(b);
        self.drop_range(ino, keep, u64::MAX)?;
        let tail = (len % b) as usize;
        if tail != 0 && !self.exts(self.cur, ino, keep - 1, keep)?.is_empty() {
            // Zero what's past the new EOF in the last block (copy-on-write).
            let mut block = alloc::vec![0u8; BLOCK];
            self.read_data(self.cur, ino, size, (keep - 1) * b, &mut block)?;
            block[tail..].fill(0);
            self.write_extents(ino, size, (keep - 1) * b, &block)?;
        }
        Ok(())
    }

    // ---- shared blocks (reflink copies) ---------------------------------------------
    //
    // A tree may reference a data block from more than one file. `K_BREF` entries count
    // the extra references per range of disk blocks (in the same tree, so snapshots and
    // clones carry their own counts). Dropping a reference to a shared block lowers its
    // count; only the last reference goes through the usual release rules (birth times,
    // dead lists), which are about which *trees* still have a block.

    /// Shared-block entries of `view` overlapping disk blocks `a..b`: (start, len, extra).
    pub fn bref_entries(&self, view: u64, a: u64, b: u64) -> Result<Vec<(u64, u64, u32)>> {
        let mut items = Vec::new();
        self.view(view, |r, s| {
            btree::range(
                r,
                s,
                &Key::new(VOL_OBJ, K_BREF, a.saturating_sub(MAX_RUN - 1)),
                &Key::new(VOL_OBJ, K_BREF, b),
                &mut items,
            )
        })?;
        Ok(items
            .into_iter()
            .filter_map(|(k, v)| {
                let (len, extra) = decode_bref(&v)?;
                (k.off + len > a && k.off < b).then_some((k.off, len, extra))
            })
            .collect())
    }

    pub fn put_bref(&mut self, start: u64, len: u64, extra: u32) -> Result<()> {
        self.put(Key::new(VOL_OBJ, K_BREF, start), encode_bref(len, extra))
    }

    pub fn del_bref(&mut self, start: u64) -> Result<()> {
        self.del(&Key::new(VOL_OBJ, K_BREF, start)).map(|_| ())
    }

    /// One more reference to disk blocks `a..a + len` (at most one run).
    fn bref_add(&mut self, a: u64, len: u64) -> Result<()> {
        let b = a + len;
        let mut pos = a;
        for (s, l, x) in self.bref_entries(self.cur, a, b)? {
            let (lo, hi) = (s.max(a), (s + l).min(b));
            self.del_bref(s)?;
            if s < a {
                self.put_bref(s, a - s, x)?;
            }
            if pos < lo {
                self.put_bref(pos, lo - pos, 1)?;
            }
            self.put_bref(lo, hi - lo, x + 1)?;
            if s + l > b {
                self.put_bref(b, s + l - b, x)?;
            }
            pos = hi;
        }
        if pos < b {
            self.put_bref(pos, b - pos, 1)?;
        }
        Ok(())
    }

    /// Disk blocks `a..a + len` moved to `to..to + len` (an in-place conversion copied
    /// them): move their shared-block counts in the current view along.
    pub fn bref_move(&mut self, a: u64, len: u64, to: u64) -> Result<()> {
        if self.version < REFLINK_VERSION {
            return Ok(());
        }
        let b = a + len;
        for (s, l, x) in self.bref_entries(self.cur, a, b)? {
            let (lo, hi) = (s.max(a), (s + l).min(b));
            self.del_bref(s)?;
            if s < a {
                self.put_bref(s, a - s, x)?;
            }
            self.put_bref(to + (lo - a), hi - lo, x)?;
            if s + l > b {
                self.put_bref(b, s + l - b, x)?;
            }
        }
        Ok(())
    }

    /// Is any of disk blocks `a..b` shared within the current view?
    pub fn bref_any(&self, a: u64, b: u64) -> Result<bool> {
        Ok(self.version >= REFLINK_VERSION && !self.bref_entries(self.cur, a, b)?.is_empty())
    }

    /// Drop one reference to blocks `i0..i1` of run `e`: shared ones lose a count, the
    /// others are released (birth rules).
    fn release_ext(&mut self, e: &Ext, i0: usize, i1: usize) -> Result<()> {
        if i0 >= i1 {
            return Ok(());
        }
        if self.version < REFLINK_VERSION {
            // Never had a reflink: nothing is shared.
            for i in i0..i1 {
                self.store.release(e.ptr(i));
            }
            return Ok(());
        }
        let (a, b) = (e.block + i0 as u64, e.block + i1 as u64);
        let mut pos = a;
        for (s, l, x) in self.bref_entries(self.cur, a, b)? {
            let (lo, hi) = (s.max(a), (s + l).min(b));
            for blk in pos..lo {
                self.store.release(e.ptr((blk - e.block) as usize));
            }
            self.del_bref(s)?;
            if s < a {
                self.put_bref(s, a - s, x)?;
            }
            if x > 1 {
                self.put_bref(lo, hi - lo, x - 1)?;
            }
            if s + l > b {
                self.put_bref(b, s + l - b, x)?;
            }
            pos = hi;
        }
        for blk in pos..b {
            self.store.release(e.ptr((blk - e.block) as usize));
        }
        Ok(())
    }

    /// Make `dst`'s data a copy-on-write copy of `src`'s, in the current view: `dst`'s old
    /// data goes, then it shares every block of `src` (small inline data is just copied).
    /// Returns the size `dst` must get. The caller updates the inode.
    pub fn reflink(&mut self, src: u64, dst: u64) -> Result<u64> {
        use crate::abi::{S_IFDIR, S_IFMT, S_IFREG};
        self.not_converting()?;
        let si = self.inode(self.cur, src)?;
        let di = self.inode(self.cur, dst)?;
        if si.mode & S_IFMT == S_IFDIR || di.mode & S_IFMT == S_IFDIR {
            return Err(Errno::EISDIR);
        }
        if si.mode & S_IFMT != S_IFREG || di.mode & S_IFMT != S_IFREG || src == dst {
            return Err(Errno::EINVAL);
        }
        self.drop_range(dst, 0, u64::MAX)?;
        self.del(&Key::new(dst, K_INLINE, 0))?;
        if let Some(d) = self.inline_bytes(self.cur, src)? {
            self.put(Key::new(dst, K_INLINE, 0), d)?;
            return Ok(si.size);
        }
        for e in self.exts(self.cur, src, 0, u64::MAX)? {
            self.version = self.version.max(REFLINK_VERSION);
            let copy = Ext {
                legacy: false,
                ..e.clone()
            };
            self.put(copy.key(dst), copy.encode())?;
            self.bref_add(e.block, e.len())?;
        }
        Ok(si.size)
    }

    /// Recount the current view's shared blocks from its references and rewrite the
    /// table to match (after a receive built the tree from a stream). Commits first.
    /// Returns the number of shared blocks.
    pub fn rebuild_brefs(&mut self) -> Result<usize> {
        self.commit()?;
        let root = if self.cur == LIVE {
            self.sb.root
        } else {
            self.clones
                .iter()
                .find(|c| c.id == self.cur as u32)
                .ok_or(Errno::ENOENT)?
                .root
        };
        let mut refs: alloc::collections::BTreeMap<u64, u32> = Default::default();
        let mut old: Vec<u64> = Vec::new();
        btree::walk(&self.store, root, &mut |_| true, &mut |k, v| {
            if k.obj == VOL_OBJ && k.kind == K_BREF {
                old.push(k.off);
            }
            for p in data_ptrs(k, v) {
                *refs.entry(p.block).or_default() += 1;
            }
            Ok(())
        })?;
        for s in old {
            self.del_bref(s)?;
        }
        let mut run: Option<(u64, u64, u32)> = None;
        let mut shared = 0;
        for (&b, &n) in refs.iter().filter(|(_, n)| **n > 1) {
            shared += 1;
            let x = n - 1;
            match run {
                Some((s, l, rx)) if s + l == b && rx == x && l < MAX_RUN => {
                    run = Some((s, l + 1, rx))
                }
                _ => {
                    if let Some((s, l, rx)) = run {
                        self.put_bref(s, l, rx)?;
                    }
                    run = Some((b, 1, x));
                }
            }
        }
        if let Some((s, l, rx)) = run {
            self.put_bref(s, l, rx)?;
        }
        if shared > 0 {
            self.version = self.version.max(REFLINK_VERSION);
        }
        Ok(shared)
    }

    /// Do `a` and `b` (current view) use exactly the same blocks already?
    pub fn same_blocks(&self, a: u64, b: u64) -> Result<bool> {
        let (x, y) = (self.extents(self.cur, a, 0)?, self.extents(self.cur, b, 0)?);
        Ok(!x.is_empty()
            && x.len() == y.len()
            && x.iter()
                .zip(&y)
                .all(|(p, q)| p.0 == q.0 && p.1.block == q.1.block))
    }

    /// Number of data blocks `ino` has (for `st_blocks`).
    pub fn block_count(&self, view: u64, ino: u64) -> Result<u64> {
        Ok(self
            .exts(view, ino, 0, u64::MAX)?
            .iter()
            .map(|e| e.len())
            .sum())
    }
}
