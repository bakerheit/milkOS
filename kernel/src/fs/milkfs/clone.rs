//! Clones: writable copies of a snapshot, for try-before-install.
//!
//! A clone starts as the snapshot's tree (O(1), nothing copied) and gets its own tree from
//! the first write. Space rules inside a clone: a block born after the origin snapshot was
//! written by the clone and is the clone's alone (clones can't be snapshotted in v0), so
//! dropping it frees it; an older block belongs to the origin snapshot, which can't be
//! deleted while the clone exists (EBUSY), so the clone never frees it.
//!
//! - **discard**: free the clone's own blocks (born after the origin), drop the record.
//! - **promote**: the clone becomes the live volume. The old live state is kept as a
//!   snapshot first (so promoting can be undone with a rollback). After a promote the
//!   history is branched (blocks can leave the live tree and come back), so the volume
//!   switches from dead lists to exact reachability sweeps (`SB_BRANCHED`).

use alloc::string::String;
use alloc::vec::Vec;

use super::btree::{self, Child};
use super::format::*;
use super::snap::check_snap_name;
use super::volume::State;
use crate::fs::errno::{Errno, Result};

#[derive(Clone, Debug)]
pub struct CloneInfo {
    pub name: String,
    pub id: u32,
    /// Name of the origin snapshot.
    pub origin: String,
    pub origin_generation: u64,
    pub created: u64,
}

impl State {
    /// Is the snapshot at `generation` the origin of some clone?
    pub fn is_origin(&self, generation: u64) -> bool {
        self.clones.iter().any(|c| c.generation == generation)
    }

    pub fn name_taken(&self, name: &str) -> bool {
        self.snaps
            .iter()
            .chain(self.clones.iter())
            .any(|s| s.name == name)
    }

    pub fn records_full(&self) -> bool {
        self.snaps.len() + self.clones.len() >= MAX_SNAPS
    }

    pub fn clone_list(&self) -> Vec<CloneInfo> {
        self.clones
            .iter()
            .map(|c| CloneInfo {
                name: c.name.clone(),
                id: c.id,
                origin: self
                    .snaps
                    .iter()
                    .find(|s| s.generation == c.generation)
                    .map(|s| s.name.clone())
                    .unwrap_or_default(),
                origin_generation: c.generation,
                created: c.created,
            })
            .collect()
    }

    /// A writable clone of snapshot `snap`. Returns its id.
    pub fn clone_create(&mut self, snap: &str, name: &str) -> Result<u32> {
        check_snap_name(name)?;
        if self.name_taken(name) {
            return Err(Errno::EEXIST);
        }
        if self.records_full() {
            return Err(Errno::ENOSPC);
        }
        self.commit()?;
        let s = self
            .snaps
            .iter()
            .find(|s| s.name == snap)
            .cloned()
            .ok_or(Errno::ENOENT)?;
        let id = self.clones.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        self.clones.push(SnapRecord {
            name: String::from(name),
            generation: s.generation,
            created: crate::fs::clock::now().sec as u64,
            root: s.root,
            deadlist: BlockPtr::NONE,
            next_ino: self.next_ino,
            id,
        });
        self.meta_dirty = true;
        self.commit()?;
        Ok(id)
    }

    /// ENOENT unless clone `name` exists.
    pub fn clone_index_check(&self, name: &str) -> Result<()> {
        self.clone_index(name).map(|_| ())
    }

    fn clone_index(&self, name: &str) -> Result<usize> {
        self.clones
            .iter()
            .position(|c| c.name == name)
            .ok_or(Errno::ENOENT)
    }

    /// Throw clone `name` away.
    pub fn clone_discard(&mut self, name: &str) -> Result<()> {
        self.commit()?;
        let k = self.clone_index(name)?;
        let c = self.clones[k].clone();
        // Free what the clone wrote: blocks born after its origin (subtrees born at or
        // before it are entirely the origin's, CoW).
        let mut free = Vec::new();
        {
            let mut node = |p: &BlockPtr| {
                if p.birth > c.generation {
                    free.push((p.block, p.birth));
                    true
                } else {
                    false
                }
            };
            let mut data = Vec::new();
            let mut item = |k: &Key, v: &[u8]| -> Result<()> {
                for p in data_ptrs(k, v) {
                    if p.birth > c.generation {
                        data.push((p.block, p.birth));
                    }
                }
                Ok(())
            };
            btree::walk(&self.store, c.root, &mut node, &mut item)?;
            free.extend(data);
        }
        for (b, birth) in free {
            self.store.free_after_commit(b, birth);
        }
        self.clones.remove(k);
        self.clone_trees.remove(&c.id);
        self.meta_dirty = true;
        self.commit()
    }

    /// Make clone `name` the live volume; the current live state is kept as snapshot
    /// `keep_as`.
    pub fn clone_promote(&mut self, name: &str, keep_as: &str) -> Result<()> {
        self.commit()?;
        let k = self.clone_index(name)?;
        let c = self.clones[k].clone();
        // 1. Keep the current live tree as a snapshot (moves the head's dead list onto it).
        self.snap_create(keep_as)?;
        // 2. Switch the live tree.
        self.tree = Child::Disk(c.root);
        self.clones.remove(k);
        self.clone_trees.remove(&c.id);
        // 3. History is branched now: blocks can leave the live tree and come back, so
        //    birth times no longer say who shares what. Drop the dead lists and reclaim
        //    space by exact reachability from here on (until no snapshots are left).
        self.set_branched();
        self.sweep().map(|_| ())
    }
}
