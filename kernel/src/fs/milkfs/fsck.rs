//! `check [--repair]`: consistency check of a mounted milkfs volume, and the repairs that
//! are safe. Only on request (`/sys/fs/milkfs/check`, `snap check`): mount never repairs
//! beyond reclaiming inodes with no links left (deferred deletes of files that were open).
//!
//! Checked: both superblock slots; every live tree node and data block checksum; the
//! namespace (entries point at inodes, link counts, directory parents, unreferenced
//! inodes); space accounting. Repaired with `repair`:
//! - a damaged superblock slot -> rewritten from the good one (a new commit lands there);
//! - an entry pointing at a missing inode -> removed;
//! - an inode with links but no entry -> linked into `/lost+found` as `#<ino>`;
//! - an inode with no links and no entry -> deleted;
//! - a wrong link count or directory parent -> fixed;
//! - an orphan-list entry for an inode that is gone or has links -> dropped;
//! - blocks allocated but unreachable -> swept; reachable blocks marked free -> marked
//!   (either way the next commit writes a corrected space map).
//!
//! Not repairable (reported): a data block or tree node whose checksum fails. Recover
//! those from a snapshot (`versions restore`, `snap rollback`).

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::btree;
use super::format::*;
use super::items::LIVE;
use super::volume::State;
use crate::fs::errno::Result;
use crate::fs::node::{InodeType, S_IFDIR, S_IFMT};

pub const LOST_FOUND: &str = "lost+found";

#[derive(Clone, Debug, Default)]
pub struct CheckReport {
    /// What is wrong (before repair).
    pub problems: Vec<String>,
    /// What was fixed (with `repair`).
    pub repaired: Vec<String>,
    /// Damage that can't be repaired safely.
    pub unrepairable: Vec<String>,
    /// Not damage: e.g. a torn superblock slot left by a commit interrupted mid-write
    /// (the next commit overwrites it). `repair` still tidies these up.
    pub notes: Vec<String>,
    pub inodes: usize,
    pub blocks_checked: u64,
}

impl CheckReport {
    pub fn clean(&self) -> bool {
        self.problems.is_empty() && self.unrepairable.is_empty()
    }

    pub fn text(&self) -> String {
        let mut s = format!(
            "{} inodes, {} blocks checksummed: {}\n",
            self.inodes,
            self.blocks_checked,
            if self.clean() {
                "clean"
            } else if self.unrepairable.is_empty() && self.problems.len() == self.repaired.len() {
                "repaired"
            } else {
                "PROBLEMS"
            }
        );
        for n in &self.notes {
            s.push_str(&format!("note: {n}\n"));
        }
        for p in &self.problems {
            s.push_str(&format!("problem: {p}\n"));
        }
        for r in &self.repaired {
            s.push_str(&format!("repaired: {r}\n"));
        }
        for u in &self.unrepairable {
            s.push_str(&format!("cannot repair: {u}\n"));
        }
        s
    }
}

impl State {
    /// Check the volume; with `repair`, fix what can be fixed safely (one transaction).
    pub fn check(&mut self, repair: bool) -> Result<CheckReport> {
        self.commit()?;
        let mut rep = CheckReport::default();

        // 1. Superblock slots: the one not holding the current generation should hold an
        //    older valid one (except right after mkfs, when it is still empty).
        let dev = self.store.cache.device().clone();
        let per = (BLOCK / dev.block_size()) as u64;
        let other = (self.sb.generation + 1) % 2;
        let mut b = [0u8; BLOCK];
        dev.read_blocks(other * per, &mut b)?;
        let bad_slot = self.sb.generation > 1
            && Superblock::decode(&b).is_none_or(|o| o.generation >= self.sb.generation);
        if bad_slot {
            rep.notes.push(format!(
                "superblock slot {other} is torn (an interrupted commit; the next commit rewrites it)"
            ));
        }

        // 2. Walk the live tree: every node checksum (load verifies), every data block.
        let mut inodes: BTreeMap<u64, InodeItem> = BTreeMap::new();
        let mut dirents: Vec<(u64, Dirent)> = Vec::new();
        let mut extents: Vec<(u64, u64, BlockPtr)> = Vec::new();
        let mut orphan_list: Vec<u64> = Vec::new();
        let mut nodes = 0u64;
        let walked = {
            let mut node = |_: &BlockPtr| {
                nodes += 1;
                true
            };
            let mut item = |k: &Key, v: &[u8]| -> Result<()> {
                if k.obj == VOL_OBJ {
                    if k.kind == K_ORPHAN {
                        orphan_list.push(k.off);
                    }
                    return Ok(());
                }
                match k.kind {
                    K_INODE => {
                        inodes.insert(k.obj, InodeItem::decode(v)?);
                    }
                    K_DIRENT => {
                        for d in decode_dirents(v)? {
                            dirents.push((k.obj, d));
                        }
                    }
                    K_EXTENT | K_EXT => {
                        for (i, p) in data_ptrs(k, v).into_iter().enumerate() {
                            extents.push((k.obj, k.off + i as u64, p));
                        }
                    }
                    _ => {}
                }
                Ok(())
            };
            btree::walk(&self.store, self.sb.root, &mut node, &mut item)
        };
        if let Err(e) = walked {
            rep.unrepairable.push(format!(
                "live tree unreadable ({e}): a tree node is damaged"
            ));
            rep.blocks_checked = nodes;
            return Ok(rep);
        }
        let mut buf = [0u8; BLOCK];
        for (ino, idx, p) in &extents {
            if self.store.read(*p, &mut buf).is_err() {
                let path = self
                    .path_in(LIVE, *ino)
                    .unwrap_or_else(|| format!("inode {ino}"));
                rep.unrepairable.push(format!(
                    "{path}: data at offset {} fails its checksum",
                    idx * BLOCK as u64
                ));
            }
        }
        rep.blocks_checked = nodes + extents.len() as u64;
        rep.inodes = inodes.len();

        // 3. Namespace.
        let is_dir = |i: &InodeItem| i.mode & S_IFMT == S_IFDIR;
        let mut refs: BTreeMap<u64, Vec<u64>> = BTreeMap::new(); // ino -> dirs naming it
        let mut subdirs: BTreeMap<u64, u32> = BTreeMap::new();
        let mut dangling: Vec<(u64, String, u64)> = Vec::new();
        for (dir, d) in &dirents {
            match inodes.get(&d.ino) {
                None => dangling.push((*dir, d.name.clone(), d.ino)),
                Some(i) => {
                    refs.entry(d.ino).or_default().push(*dir);
                    if is_dir(i) {
                        *subdirs.entry(*dir).or_default() += 1;
                    }
                }
            }
        }
        for (dir, name, ino) in &dangling {
            let at = self.path_in(LIVE, *dir).unwrap_or_default();
            rep.problems
                .push(format!("{at}/{name} points at missing inode {ino}"));
        }
        let mut lost: Vec<u64> = Vec::new();
        let mut reclaim: Vec<u64> = Vec::new();
        let mut fix_nlink: Vec<(u64, u32)> = Vec::new();
        let mut fix_parent: Vec<(u64, u64)> = Vec::new();
        for (&ino, i) in &inodes {
            if ino == ROOT_INO {
                continue;
            }
            match refs.get(&ino) {
                None if i.nlink == 0 => {
                    rep.problems
                        .push(format!("inode {ino} has no links and no entry (orphan)"));
                    reclaim.push(ino);
                }
                None => {
                    rep.problems.push(format!(
                        "inode {ino} ({} links) is in no directory",
                        i.nlink
                    ));
                    lost.push(ino);
                }
                Some(dirs) => {
                    let want = if is_dir(i) {
                        2 + subdirs.get(&ino).copied().unwrap_or(0)
                    } else {
                        dirs.len() as u32
                    };
                    if i.nlink != want {
                        rep.problems
                            .push(format!("inode {ino}: {} links, should be {want}", i.nlink));
                        fix_nlink.push((ino, want));
                    }
                    if is_dir(i) && i.parent != dirs[0] {
                        rep.problems.push(format!(
                            "directory {ino}: parent {} should be {}",
                            i.parent, dirs[0]
                        ));
                        fix_parent.push((ino, dirs[0]));
                    }
                }
            }
        }
        let mut stale_orphans: Vec<u64> = Vec::new();
        for &ino in &orphan_list {
            match inodes.get(&ino) {
                Some(i) if i.nlink == 0 => {}
                _ => {
                    rep.problems.push(format!(
                        "orphan list names inode {ino}, which is gone or has links"
                    ));
                    stale_orphans.push(ino);
                }
            }
        }
        if let Some(r) = inodes.get(&ROOT_INO) {
            let want = 2 + subdirs.get(&ROOT_INO).copied().unwrap_or(0);
            if r.nlink != want {
                rep.problems
                    .push(format!("root: {} links, should be {want}", r.nlink));
                fix_nlink.push((ROOT_INO, want));
            }
        } else {
            rep.unrepairable.push(String::from("no root directory"));
            return Ok(rep);
        }

        // 4. Space accounting (the in-memory allocator vs what is reachable).
        let space = self.verify_space()?;
        if space.free_but_used != 0 || (space.leaked != 0 && !self.branched()) {
            rep.problems.push(format!(
                "space: {} blocks leaked, {} reachable blocks marked free",
                space.leaked, space.free_but_used
            ));
        } else if space.leaked != 0 {
            rep.notes.push(format!(
                "{} blocks wait for the next sweep (branched history)",
                space.leaked
            ));
        }

        if !repair || (rep.problems.is_empty() && !bad_slot && space.leaked == 0) {
            return Ok(rep);
        }

        // ---- repairs ----
        // First, before anything allocates: blocks in use that the free map lost.
        if space.free_but_used != 0 {
            let n = self.mark_reachable()?;
            rep.repaired
                .push(format!("marked {n} reachable blocks used again"));
        }
        self.set_cur(LIVE)?;
        for (dir, name, ino) in dangling {
            self.remove_dirent(dir, &name)?;
            rep.repaired
                .push(format!("removed entry {name} (missing inode {ino})"));
        }
        for ino in reclaim {
            self.delete_inode(ino)?;
            rep.repaired.push(format!("deleted orphan inode {ino}"));
        }
        for ino in stale_orphans {
            self.del_orphan(ino)?;
            rep.repaired
                .push(format!("dropped orphan-list entry for inode {ino}"));
        }
        for (ino, n) in fix_nlink {
            if let Ok(mut i) = self.inode(LIVE, ino) {
                i.nlink = n;
                self.put_inode(ino, &i)?;
                rep.repaired
                    .push(format!("inode {ino}: link count set to {n}"));
            }
        }
        for (ino, p) in fix_parent {
            let mut i = self.inode(LIVE, ino)?;
            i.parent = p;
            self.put_inode(ino, &i)?;
            rep.repaired
                .push(format!("directory {ino}: parent set to {p}"));
        }
        if !lost.is_empty() {
            let lf = self.lost_found()?;
            for ino in lost {
                let mut i = self.inode(LIVE, ino)?;
                let kind = InodeType::from_mode(i.mode).unwrap_or(InodeType::File);
                let name = format!("#{ino}");
                self.add_dirent(
                    lf,
                    Dirent {
                        ino,
                        dtype: kind.dirent_type(),
                        name: name.clone(),
                    },
                )?;
                if kind == InodeType::Dir {
                    i.parent = lf;
                    let mut l = self.inode(LIVE, lf)?;
                    l.nlink += 1;
                    self.put_inode(lf, &l)?;
                } else {
                    i.nlink = 1;
                }
                self.put_inode(ino, &i)?;
                rep.repaired
                    .push(format!("moved inode {ino} to /{LOST_FOUND}/{name}"));
            }
        }
        if bad_slot {
            self.meta_dirty = true; // the next commit lands in the damaged slot
        }
        self.commit()?;
        if bad_slot {
            rep.repaired.push(format!(
                "superblock slot {other} rewritten (generation {})",
                self.sb.generation
            ));
        }
        if space.leaked != 0 {
            let n = self.sweep()?;
            rep.repaired.push(format!("swept {n} unreachable blocks"));
        }
        Ok(rep)
    }

    /// The inode of `/lost+found`, created if missing.
    fn lost_found(&mut self) -> Result<u64> {
        if let Some(d) = self.lookup(LIVE, ROOT_INO, LOST_FOUND)? {
            return Ok(d.ino);
        }
        let ino = self.alloc_ino();
        let t = crate::fs::clock::now();
        let t = (t.sec, t.nsec as u32);
        self.put_inode(
            ino,
            &InodeItem {
                mode: S_IFDIR | 0o700,
                nlink: 2,
                parent: ROOT_INO,
                atime: t,
                mtime: t,
                ctime: t,
                ..InodeItem::default()
            },
        )?;
        self.add_dirent(
            ROOT_INO,
            Dirent {
                ino,
                dtype: InodeType::Dir.dirent_type(),
                name: String::from(LOST_FOUND),
            },
        )?;
        let mut r = self.inode(LIVE, ROOT_INO)?;
        r.nlink += 1;
        self.put_inode(ROOT_INO, &r)?;
        Ok(ino)
    }
}
