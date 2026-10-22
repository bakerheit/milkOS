//! `snap diff`: what changed between two snapshots, or a snapshot and the live volume.
//!
//! Only changed tree nodes are read. Walking the newer tree, any subtree whose pointer was
//! born at or before the older snapshot is skipped: copy-on-write means it is still the
//! very same subtree in the older tree (in linear history a block that leaves a tree never
//! comes back). Those skipped pointers are then skipped when walking the older tree too.
//! What remains are the items of the leaves that differ; comparing them is exact, because
//! a key can't be both in a shared and an unshared leaf of the same tree. On a branched
//! volume (after a promote) birth times can't be trusted, so the shared set is the older
//! tree's full block set instead (pointer equality).
//!
//! Item changes become file changes: directory entries added / removed (a removed and an
//! added entry with the same inode = a rename), content (extents, symlink target, size,
//! mode) changed = modified. Directories whose only change is their own metadata are not
//! listed.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::btree;
use super::format::*;
use super::items::LIVE;
use super::volume::State;
use crate::fs::errno::{Errno, Result};
use crate::fs::node::{S_IFDIR, S_IFMT};

/// Items by key.
pub type ItemMap = BTreeMap<Key, Vec<u8>>;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    Added(String),
    Removed(String),
    Modified(String),
    Renamed(String, String),
}

impl Change {
    pub fn path(&self) -> &str {
        match self {
            Change::Added(p) | Change::Removed(p) | Change::Modified(p) => p,
            Change::Renamed(_, n) => n,
        }
    }

    /// `A path`, `D path`, `M path`, `R old -> new`.
    pub fn line(&self) -> String {
        match self {
            Change::Added(p) => format!("A {p}"),
            Change::Removed(p) => format!("D {p}"),
            Change::Modified(p) => format!("M {p}"),
            Change::Renamed(o, n) => format!("R {o} -> {n}"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DiffResult {
    pub changes: Vec<Change>,
    /// Tree nodes read on each side, and subtrees skipped as unchanged.
    pub nodes_read: u64,
    pub subtrees_skipped: u64,
}

/// One side of a diff: a view id (for path lookups) and its committed root.
#[derive(Clone, Copy)]
struct Side {
    view: u64,
    root: BlockPtr,
    generation: u64,
}

impl State {
    fn side(&self, name: Option<&str>) -> Result<Side> {
        match name {
            None | Some("live") => Ok(Side {
                view: LIVE,
                root: self.sb.root,
                generation: self.sb.generation,
            }),
            Some(n) => {
                let s = self
                    .snaps
                    .iter()
                    .find(|s| s.name == n)
                    .ok_or(Errno::ENOENT)?;
                Ok(Side {
                    view: s.generation,
                    root: s.root,
                    generation: s.generation,
                })
            }
        }
    }

    /// Path of `ino` in `view`, from the volume root (`/Users/milk/a.txt`).
    pub fn path_in(&self, view: u64, ino: u64) -> Option<String> {
        let mut names: Vec<String> = Vec::new();
        let mut cur = ino;
        for _ in 0..256 {
            if cur == ROOT_INO {
                let mut p = String::new();
                for n in names.iter().rev() {
                    p.push('/');
                    p.push_str(n.as_str());
                }
                return Some(if p.is_empty() { String::from("/") } else { p });
            }
            let i = self.inode(view, cur).ok()?;
            let parent = if i.parent != 0 {
                i.parent
            } else {
                self.find_parent(view, cur)?
            };
            let name = self
                .list(view, parent)
                .ok()?
                .into_iter()
                .find(|d| d.ino == cur)
                .map(|d| d.name)
                .or_else(|| {
                    // Recorded parent is stale (e.g. the hard link it was made under is gone).
                    let p = self.find_parent(view, cur)?;
                    self.list(view, p)
                        .ok()?
                        .into_iter()
                        .find(|d| d.ino == cur)
                        .map(|d| d.name)
                })?;
            names.push(name);
            cur = parent;
        }
        None
    }

    /// Slow path: scan every directory entry of `view` for one naming `ino`.
    fn find_parent(&self, view: u64, ino: u64) -> Option<u64> {
        let found = core::cell::Cell::new(None);
        let root = self.side_root(view)?;
        let mut node = |_: &BlockPtr| found.get().is_none();
        let mut item = |k: &Key, v: &[u8]| -> Result<()> {
            if k.kind == K_DIRENT
                && found.get().is_none()
                && decode_dirents(v)?.iter().any(|d| d.ino == ino)
            {
                found.set(Some(k.obj));
            }
            Ok(())
        };
        btree::walk(&self.store, root, &mut node, &mut item).ok()?;
        found.get()
    }

    fn side_root(&self, view: u64) -> Option<BlockPtr> {
        if view == LIVE {
            Some(self.sb.root)
        } else {
            self.snaps
                .iter()
                .find(|s| s.generation == view)
                .map(|s| s.root)
        }
    }

    /// Items of the leaves of `root` that are not under a pointer `shared` accepts.
    fn unshared_items(
        &self,
        root: BlockPtr,
        shared: &mut dyn FnMut(&BlockPtr) -> bool,
        read: &mut u64,
        skipped: &mut u64,
    ) -> Result<BTreeMap<Key, Vec<u8>>> {
        let mut out = BTreeMap::new();
        let mut node = |p: &BlockPtr| {
            if shared(p) {
                *skipped += 1;
                false
            } else {
                *read += 1;
                true
            }
        };
        let mut item = |k: &Key, v: &[u8]| -> Result<()> {
            out.insert(*k, v.to_vec());
            Ok(())
        };
        btree::walk(&self.store, root, &mut node, &mut item)?;
        Ok(out)
    }

    /// The items of the leaves that differ between an older tree (`old_root`, committed at
    /// `old_gen`) and a newer one: `(old side, new side)`. Every key whose value differs, or
    /// that exists on one side only, is in one of them (with its value); keys in neither
    /// are unchanged. Only changed nodes are read.
    pub fn tree_delta(
        &self,
        old_root: BlockPtr,
        old_gen: u64,
        new_root: BlockPtr,
        nodes_read: &mut u64,
        skipped: &mut u64,
    ) -> Result<(ItemMap, ItemMap)> {
        if old_root == new_root {
            return Ok((BTreeMap::new(), BTreeMap::new()));
        }
        // Pointers of the newer tree that are shared with the older one.
        let mut shared_set: BTreeSet<u64> = BTreeSet::new();
        let new_items = if self.branched() {
            let mut all = BTreeSet::new();
            let mut node = |p: &BlockPtr| all.insert(p.block);
            btree::walk(&self.store, old_root, &mut node, &mut |_, _| Ok(()))?;
            self.unshared_items(
                new_root,
                &mut |p| {
                    let s = all.contains(&p.block);
                    if s {
                        shared_set.insert(p.block);
                    }
                    s
                },
                nodes_read,
                skipped,
            )?
        } else {
            self.unshared_items(
                new_root,
                &mut |p| {
                    let s = p.birth <= old_gen;
                    if s {
                        shared_set.insert(p.block);
                    }
                    s
                },
                nodes_read,
                skipped,
            )?
        };
        let mut dummy = 0;
        let old_items = self.unshared_items(
            old_root,
            &mut |p| shared_set.contains(&p.block),
            nodes_read,
            &mut dummy,
        )?;
        Ok((old_items, new_items))
    }

    /// What changed from `a` to `b` (snapshot names; `None`/`"live"` = the live volume).
    pub fn diff(&mut self, a: &str, b: Option<&str>) -> Result<DiffResult> {
        self.commit()?; // diff committed state
        let (mut old, mut new) = (self.side(Some(a))?, self.side(b)?);
        if old.generation > new.generation {
            core::mem::swap(&mut old, &mut new);
        }
        let mut res = DiffResult::default();
        if old.root == new.root {
            return Ok(res);
        }
        let (old_items, new_items) = self.tree_delta(
            old.root,
            old.generation,
            new.root,
            &mut res.nodes_read,
            &mut res.subtrees_skipped,
        )?;

        // Directory entries, by (dir, name).
        let entries = |m: &BTreeMap<Key, Vec<u8>>| -> BTreeMap<(u64, String), (u64, u8)> {
            let mut e = BTreeMap::new();
            for (k, v) in m {
                if k.kind == K_DIRENT
                    && let Ok(list) = decode_dirents(v)
                {
                    for d in list {
                        e.insert((k.obj, d.name), (d.ino, d.dtype));
                    }
                }
            }
            e
        };
        let (eo, en) = (entries(&old_items), entries(&new_items));
        // The same dirent bucket can be in both maps with other names unchanged.
        let gone: Vec<((u64, String), u64)> = eo
            .iter()
            .filter(|(k, v)| en.get(k).map(|n| n.0) != Some(v.0))
            .map(|(k, v)| (k.clone(), v.0))
            .collect();
        let came: Vec<((u64, String), u64)> = en
            .iter()
            .filter(|(k, v)| eo.get(k).map(|o| o.0) != Some(v.0))
            .map(|(k, v)| (k.clone(), v.0))
            .collect();
        let mut reported: BTreeSet<u64> = BTreeSet::new();
        let path = |st: &State, view: u64, (dir, name): &(u64, String)| -> String {
            match st.path_in(view, *dir) {
                Some(d) if d == "/" => format!("/{name}"),
                Some(d) => format!("{d}/{name}"),
                None => format!("?/{name}"),
            }
        };
        let gone_by_ino: BTreeMap<u64, (u64, String)> =
            gone.iter().map(|(k, ino)| (*ino, k.clone())).collect();
        for (k, ino) in &came {
            if let Some(ok) = gone_by_ino.get(ino) {
                res.changes.push(Change::Renamed(
                    path(self, old.view, ok),
                    path(self, new.view, k),
                ));
            } else {
                res.changes.push(Change::Added(path(self, new.view, k)));
            }
            reported.insert(*ino);
        }
        let came_inos: BTreeSet<u64> = came.iter().map(|(_, i)| *i).collect();
        for (k, ino) in &gone {
            if !came_inos.contains(ino) {
                res.changes.push(Change::Removed(path(self, old.view, k)));
                reported.insert(*ino);
            }
        }
        // Content / metadata changes of files that kept their name.
        let mut touched: BTreeSet<u64> = BTreeSet::new();
        for k in old_items.keys().chain(new_items.keys()) {
            if matches!(k.kind, K_INODE | K_EXTENT | K_EXT | K_INLINE | K_SYMLINK)
                && old_items.get(k) != new_items.get(k)
            {
                touched.insert(k.obj);
            }
        }
        for ino in touched {
            if reported.contains(&ino) {
                continue;
            }
            let Ok(i) = self.inode(new.view, ino) else {
                continue;
            };
            if i.mode & S_IFMT == S_IFDIR {
                continue; // a directory's own mtime/nlink change: its entries say it all
            }
            // Only report real changes, not just ctime/atime.
            let ko = Key::new(ino, K_INODE, 0);
            let meta_only = match (old_items.get(&ko), new_items.get(&ko)) {
                (Some(a), Some(b)) => {
                    let (x, y) = (InodeItem::decode(a)?, InodeItem::decode(b)?);
                    x.size == y.size && x.mode == y.mode && x.mtime == y.mtime
                }
                _ => false,
            } && !old_items
                .keys()
                .chain(new_items.keys())
                .any(|k| k.obj == ino && k.kind != K_INODE);
            if meta_only {
                continue;
            }
            if let Some(p) = self.path_in(new.view, ino) {
                res.changes.push(Change::Modified(p));
            }
        }
        res.changes.sort_by(|a, b| a.path().cmp(b.path()));
        Ok(res)
    }
}
