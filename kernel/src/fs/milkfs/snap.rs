//! Snapshots: create (O(1): save the committed root), delete (dead-list algorithm), and
//! rollback. Each is its own commit.
//!
//! Dead lists, as in ZFS: the live head's list holds blocks it dropped that the newest
//! snapshot still has (born at or before it). Taking a snapshot moves that list onto the
//! snapshot record and starts the head with an empty one. So snapshot `S`'s list = blocks
//! born before the previous snapshot `P` that died between `P` and `S`.
//!
//! Deleting `S` (previous `P`, next `N` = next snapshot or the head): every entry of `N`'s
//! list born after `P` was only ever visible to `S` -> free it. The rest stay, and `S`'s own
//! list moves onto `N` (those blocks now died between `P` and `N`).

use alloc::string::String;
use alloc::vec::Vec;

use super::btree;
use super::format::*;
use super::volume::State;
use crate::fs::errno::{Errno, Result};

/// Public view of one snapshot.
#[derive(Clone, Debug)]
pub struct SnapInfo {
    pub name: String,
    pub generation: u64,
    pub created: u64,
}

pub fn check_snap_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(Errno::EINVAL);
    }
    if name.len() > SNAP_NAME_MAX {
        return Err(Errno::ENAMETOOLONG);
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
    {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

impl State {
    fn update_snap_gen(&mut self) {
        self.store.snap_gen = self.snaps.iter().map(|s| s.generation).max().unwrap_or(0);
    }

    pub fn snap_list(&self) -> Vec<SnapInfo> {
        self.snaps
            .iter()
            .map(|s| SnapInfo {
                name: s.name.clone(),
                generation: s.generation,
                created: s.created,
            })
            .collect()
    }

    /// Snapshot the live tree as it is now (after committing anything pending).
    pub fn snap_create(&mut self, name: &str) -> Result<SnapInfo> {
        self.snap_create_at(name, None)
    }

    /// Same, with a given creation time (a received snapshot keeps the sender's, which is
    /// also how an incremental stream finds its base).
    pub fn snap_create_at(&mut self, name: &str, created: Option<u64>) -> Result<SnapInfo> {
        check_snap_name(name)?;
        if self.name_taken(name) {
            return Err(Errno::EEXIST);
        }
        if self.records_full() {
            return Err(Errno::ENOSPC);
        }
        self.commit()?;
        // A snapshot needs its own generation: if the last commit is already some other
        // snapshot's generation, commit an otherwise empty transaction first.
        if self
            .snaps
            .iter()
            .any(|s| s.generation == self.sb.generation)
        {
            self.meta_dirty = true;
            self.commit()?;
        }
        let rec = SnapRecord {
            name: String::from(name),
            generation: self.sb.generation,
            created: created.unwrap_or(crate::fs::clock::now().sec as u64),
            root: self.sb.root,
            deadlist: self.deadlist,
            next_ino: self.next_ino,
            id: 0,
        };
        let info = SnapInfo {
            name: rec.name.clone(),
            generation: rec.generation,
            created: rec.created,
        };
        self.snaps.push(rec);
        self.deadlist = BlockPtr::NONE;
        self.update_snap_gen();
        self.meta_dirty = true;
        self.commit()?;
        Ok(info)
    }

    /// Delete snapshot `name`, freeing what only it kept.
    pub fn snap_delete(&mut self, name: &str) -> Result<()> {
        self.commit()?;
        let k = self
            .snaps
            .iter()
            .position(|s| s.name == name)
            .ok_or(Errno::ENOENT)?;
        if self.is_origin(self.snaps[k].generation)
            || self.busy_snaps.contains(&self.snaps[k].generation)
        {
            return Err(Errno::EBUSY); // a clone is built on it, or a send is reading it
        }
        if self.branched() {
            // No dead lists after a promote: drop the record, then sweep exactly.
            self.snaps.remove(k);
            self.update_snap_gen();
            self.maybe_unbranch();
            self.meta_dirty = true;
            return self.sweep().map(|_| ());
        }
        let prev_gen = if k == 0 {
            0
        } else {
            self.snaps[k - 1].generation
        };
        let next_head = if k + 1 < self.snaps.len() {
            self.snaps[k + 1].deadlist
        } else {
            self.deadlist
        };
        let (next_entries, next_chain) = self.store.read_deadlist(next_head)?;
        let (own_entries, own_chain) = self.store.read_deadlist(self.snaps[k].deadlist)?;
        let mut keep = Vec::new();
        for e in next_entries {
            if e.birth > prev_gen {
                self.store.free_after_commit(e.block, e.birth);
            } else {
                keep.push(e);
            }
        }
        keep.extend(own_entries);
        for p in next_chain.iter().chain(own_chain.iter()) {
            self.store.free_after_commit(p.block, p.birth);
        }
        let new_head = self.store.write_deadlist(&keep, BlockPtr::NONE)?;
        if k + 1 < self.snaps.len() {
            self.snaps[k + 1].deadlist = new_head;
        } else {
            self.deadlist = new_head;
        }
        self.snaps.remove(k);
        self.update_snap_gen();
        self.meta_dirty = true;
        self.commit()
    }

    /// Make the live tree what snapshot `name` captured. Newer snapshots are deleted first
    /// (like `zfs rollback -r`); `name` itself is kept.
    pub fn snap_rollback(&mut self, name: &str) -> Result<()> {
        self.commit()?;
        let k = self
            .snaps
            .iter()
            .position(|s| s.name == name)
            .ok_or(Errno::ENOENT)?;
        if self.snaps[k + 1..]
            .iter()
            .any(|s| self.is_origin(s.generation))
        {
            return Err(Errno::EBUSY); // a newer snapshot that would go has a clone
        }
        while self.snaps.len() > k + 1 {
            let newest = self.snaps.last().unwrap().name.clone();
            self.snap_delete(&newest)?;
        }
        let s = self.snaps[k].clone();
        if self.branched() {
            // Birth times don't say what the snapshot shares after a promote: sweep.
            self.tree = btree::Child::Disk(s.root);
            self.meta_dirty = true;
            return self.sweep().map(|_| ());
        }
        // Free everything the live tree has that was born after the snapshot. CoW means a
        // node born at or before it has an entirely old subtree, so skip those.
        let mut free = Vec::new();
        {
            let mut node = |p: &BlockPtr| {
                if p.birth > s.generation {
                    free.push((p.block, p.birth));
                    true
                } else {
                    false
                }
            };
            let mut data = Vec::new();
            let mut item = |k: &Key, v: &[u8]| -> Result<()> {
                for p in data_ptrs(k, v) {
                    if p.birth > s.generation {
                        data.push((p.block, p.birth));
                    }
                }
                Ok(())
            };
            btree::walk(&self.store, self.sb.root, &mut node, &mut item)?;
            free.extend(data);
        }
        for (b, birth) in free {
            self.store.free_after_commit(b, birth);
        }
        // The head's dead list only held blocks the snapshot has: they are live again.
        let (_, chain) = self.store.read_deadlist(self.deadlist)?;
        for p in chain {
            self.store.free_after_commit(p.block, p.birth);
        }
        self.deadlist = BlockPtr::NONE;
        self.tree = btree::Child::Disk(s.root);
        self.meta_dirty = true;
        self.commit()
    }

    /// Inodes in the live tree with no links left (unlinked while open when a snapshot was
    /// taken; a rollback brings them back).
    pub fn unlinked_inodes(&self) -> Result<Vec<u64>> {
        let mut out = Vec::new();
        let mut node = |_: &BlockPtr| true;
        let mut item = |k: &Key, v: &[u8]| -> Result<()> {
            if k.kind == K_INODE && InodeItem::decode(v)?.nlink == 0 {
                out.push(k.obj);
            }
            Ok(())
        };
        btree::walk(&self.store, self.sb.root, &mut node, &mut item)?;
        Ok(out)
    }
}
