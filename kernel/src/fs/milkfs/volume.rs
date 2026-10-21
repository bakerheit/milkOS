//! One milkfs volume's state: the live tree, the store, the snapshot table, and the
//! transaction machinery (commit = write dirty nodes, flush, write the superblock, flush).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::btree::{self, Child};
use super::format::*;
use super::store::Store;
use crate::fs::bcache::BCache;
use crate::fs::blockdev::BlockRef;
use crate::fs::errno::{Errno, Result};
use crate::fs::node::{S_IFDIR, S_IFMT};
use crate::kprintln;

pub struct State {
    pub store: Store,
    /// Live tree.
    pub tree: Child,
    /// Last committed superblock.
    pub sb: Superblock,
    pub snaps: Vec<SnapRecord>,
    /// Writable clones (records with `REC_CLONE`); their trees, once touched, are in
    /// `clone_trees` (by clone id) until the next commit writes them.
    pub clones: Vec<SnapRecord>,
    pub clone_trees: BTreeMap<u32, Child>,
    /// The view writes go to: `items::LIVE` or a clone view (`items::clone_view(id)`).
    pub cur: u64,
    /// Live head's dead list (committed part; `store.dead_new` is prepended at commit).
    pub deadlist: BlockPtr,
    pub next_ino: u64,
    /// Automatic snapshot settings (superblock `auto`), see `timemachine`.
    pub auto: [u32; 4],
    /// Superblock flags (`SB_BRANCHED`).
    pub flags: u32,
    /// Format version the next commit writes (`BASE_VERSION`, or `REFLINK_VERSION` once
    /// blocks are shared).
    pub version: u32,
    /// Superblock-only fields changed since the last commit.
    pub meta_dirty: bool,
    /// A commit failed: the in-memory state can't be trusted, refuse further writes.
    pub broken: bool,
    pub commits: u64,
    /// Batching: when the open transaction got its first change, and how many
    /// operations it holds.
    pub dirty_since: Option<u64>,
    pub batched_ops: u32,
}

/// A transaction is committed once it is this old ...
pub const COMMIT_DELAY_NS: u64 = 1_000_000_000;
/// ... or holds this many operations (bounds the dirty tree kept in memory).
pub const MAX_BATCH_OPS: u32 = 512;

/// What mount found.
#[derive(Clone, Debug, Default)]
pub struct MountReport {
    pub generation: u64,
    pub inodes: usize,
    pub used_blocks: u64,
    pub problems: usize,
    pub orphans: usize,
    /// How long `open` took.
    pub mount_ns: u64,
    /// The space map was current and loaded: no tree scan, no namespace check (`inodes`
    /// is unknown then; `check` does the full check on request).
    pub fast: bool,
}

/// Read both superblock slots straight from the device; the newest valid one wins.
pub fn probe(dev: &BlockRef) -> Option<Superblock> {
    let per = (BLOCK / dev.block_size()) as u64;
    let mut best: Option<Superblock> = None;
    for slot in 0..2u64 {
        let mut b = [0u8; BLOCK];
        if dev.read_blocks(slot * per, &mut b).is_err() {
            continue;
        }
        if let Some(sb) = Superblock::decode(&b)
            && best.as_ref().is_none_or(|x| sb.generation > x.generation)
        {
            best = Some(sb);
        }
    }
    best
}

fn now() -> (i64, u32) {
    let t = crate::fs::clock::now();
    (t.sec, t.nsec as u32)
}

pub fn new_uuid() -> [u8; 16] {
    let mut x = crate::timer::counter() ^ 0x6d69_6c6b_6673_0001;
    let mut u = [0u8; 16];
    for chunk in u.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        chunk.copy_from_slice(&x.to_le_bytes());
    }
    u
}

/// Format `dev`: an empty root directory at generation 1.
pub fn mkfs(dev: &BlockRef, label: &str) -> Result<()> {
    let cache = BCache::new(dev.clone(), 8)?;
    let total = cache.num_blocks();
    if total < 16 {
        return Err(Errno::ENOSPC);
    }
    let t = now();
    let root_inode = InodeItem {
        mode: S_IFDIR | 0o755,
        nlink: 2,
        parent: ROOT_INO,
        atime: t,
        mtime: t,
        ctime: t,
        ..InodeItem::default()
    };
    let mut leaf = [0u8; BLOCK];
    encode_leaf(
        &[(Key::new(ROOT_INO, K_INODE, 0), root_inode.encode())],
        1,
        &mut leaf,
    );
    cache.write(2, &leaf)?;
    let sb = Superblock {
        total_blocks: total,
        generation: 1,
        root: BlockPtr {
            block: 2,
            birth: 1,
            csum: xxh64(&leaf),
        },
        deadlist: BlockPtr::NONE,
        next_ino: ROOT_INO + 1,
        created: t.0 as u64,
        uuid: new_uuid(),
        label: String::from(label),
        snaps: Vec::new(),
        clones: Vec::new(),
        auto: [0; 4],
        flags: 0,
        version: BASE_VERSION,
        spacemap: BlockPtr::NONE,
    };
    cache.write(0, &[0u8; BLOCK])?; // invalidate slot 0 (an older fs's superblock)
    cache.flush()?;
    cache.write_through(1, &sb.encode())?;
    cache.flush()
}

impl State {
    /// Load the newest committed state from `dev`: the free map from the space map when it
    /// is current (and orphans from their list), otherwise rebuilt from the trees with a
    /// namespace check and orphan reclaim (inodes no directory entry points at).
    pub fn open(dev: BlockRef) -> Result<(State, MountReport)> {
        let t0 = crate::timer::now_ns();
        let name = String::from(dev.name());
        let sb = probe(&dev).ok_or(Errno::EINVAL)?;
        let cache = BCache::new(dev, 512)?; // 2 MiB
        if sb.total_blocks > cache.num_blocks() || sb.total_blocks < 3 {
            return Err(Errno::EINVAL);
        }
        let snap_gen = sb.snaps.iter().map(|s| s.generation).max().unwrap_or(0);
        if sb.version < BASE_VERSION {
            kprintln!(
                "milkfs: format v{} volume: it is upgraded to v{BASE_VERSION} (multi-block extents, inline data) by its first write; kernels older than that won't mount it then",
                sb.version
            );
        }
        // What's on disk now is the last committed generation (until this boot commits).
        super::crashtest::LAST_COMMIT.store(sb.generation, core::sync::atomic::Ordering::Relaxed);
        let mut store = Store::new(cache, sb.total_blocks, sb.generation + 1, snap_gen);
        store.branched = sb.flags & SB_BRANCHED != 0;

        let loaded = if sb.spacemap.is_none() {
            Err("absent (last written by the Mac tool or an older kernel)")
        } else {
            let mut known = alloc::vec![sb.root.block];
            for s in sb.snaps.iter().chain(sb.clones.iter()) {
                known.push(s.root.block);
                if !s.deadlist.is_none() {
                    known.push(s.deadlist.block);
                }
            }
            if !sb.deadlist.is_none() {
                known.push(sb.deadlist.block);
            }
            store.load_spacemap(sb.spacemap, sb.generation, &known)
        };
        match loaded {
            Ok(()) => return Self::open_fast(store, sb, t0),
            // (Generation 1 = just made by mkfs: nothing to say.)
            Err(why) if sb.generation > 1 => {
                kprintln!("milkfs: {name}: space map {why}: rebuilding it from the trees")
            }
            Err(_) => {}
        }

        // Mark everything reachable. Shared subtrees are visited once.
        let mut inodes: BTreeMap<u64, InodeItem> = BTreeMap::new();
        let mut refs: BTreeMap<u64, u32> = BTreeMap::new();
        let mut subdirs: BTreeMap<u64, u32> = BTreeMap::new();
        let mut problems = 0usize;
        let mut dirents: Vec<(u64, Dirent)> = Vec::new();
        let mut orphan_items: Vec<u64> = Vec::new();
        let used = reachable(&store, &sb, &mut |k: &Key, v: &[u8]| -> Result<()> {
            if k.obj == VOL_OBJ {
                if k.kind == K_ORPHAN {
                    orphan_items.push(k.off);
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
                _ => {}
            }
            Ok(())
        })?;
        for b in used {
            store.mark(b);
        }

        // Check the live namespace.
        for (dir, d) in &dirents {
            *refs.entry(d.ino).or_default() += 1;
            match inodes.get(&d.ino) {
                None => {
                    problems += 1;
                    kprintln!("milkfs: check: {dir}/{} -> missing inode {}", d.name, d.ino);
                }
                Some(i) if i.mode & S_IFMT == S_IFDIR => {
                    *subdirs.entry(*dir).or_default() += 1;
                    if i.parent != *dir {
                        problems += 1;
                        kprintln!("milkfs: check: dir {} parent {} != {dir}", d.ino, i.parent);
                    }
                }
                _ => {}
            }
        }
        let mut orphans = Vec::new();
        for (&ino, i) in &inodes {
            let r = refs.get(&ino).copied().unwrap_or(0);
            if ino != ROOT_INO && r == 0 {
                if i.nlink == 0 {
                    // Unlinked while open when the machine went down: a deferred delete,
                    // finished here (like Linux's orphan list). Not a repair.
                    orphans.push(ino);
                } else {
                    // Links but no entry: damage. Mount never repairs; `check --repair` does.
                    problems += 1;
                    kprintln!(
                        "milkfs: check: inode {ino} has {} link(s) but no entry",
                        i.nlink
                    );
                }
                continue;
            }
            let want = if i.mode & S_IFMT == S_IFDIR {
                2 + subdirs.get(&ino).copied().unwrap_or(0)
            } else {
                r
            };
            if i.nlink != want {
                problems += 1;
                kprintln!("milkfs: check: inode {ino} nlink {} != {want}", i.nlink);
            }
        }
        if !inodes.contains_key(&ROOT_INO) {
            kprintln!("milkfs: check: no root directory");
            return Err(Errno::EIO);
        }

        let mut report = MountReport {
            generation: sb.generation,
            inodes: inodes.len() - orphans.len(),
            used_blocks: store.total() - store.free_blocks(),
            problems,
            orphans: orphans.len(),
            mount_ns: 0,
            fast: false,
        };
        let mut st = Self::from_sb(store, sb);
        // Orphan-list entries of inodes that are gone or linked again (an older kernel
        // finished the delete) are dropped.
        let stale: Vec<u64> = orphan_items
            .into_iter()
            .filter(|i| !orphans.contains(i))
            .collect();
        if !orphans.is_empty() || !stale.is_empty() {
            for ino in orphans {
                st.delete_inode(ino)?;
            }
            for ino in stale {
                st.del_orphan(ino)?;
            }
            st.commit()?;
        }
        report.mount_ns = crate::timer::now_ns() - t0;
        Ok((st, report))
    }

    /// Mount with a current space map: nothing to scan. Finish the deferred deletes on
    /// the orphan list.
    fn open_fast(store: Store, sb: Superblock, t0: u64) -> Result<(State, MountReport)> {
        let mut items = Vec::new();
        btree::range(
            &Child::Disk(sb.root),
            &store,
            &Key::new(VOL_OBJ, K_ORPHAN, 0),
            &Key::new(VOL_OBJ, K_ORPHAN + 1, 0),
            &mut items,
        )?;
        let mut report = MountReport {
            generation: sb.generation,
            used_blocks: store.total() - store.free_blocks(),
            fast: true,
            ..MountReport::default()
        };
        let mut st = Self::from_sb(store, sb);
        if !items.is_empty() {
            for (k, _) in items {
                if st
                    .inode(super::items::LIVE, k.off)
                    .is_ok_and(|i| i.nlink == 0)
                {
                    st.delete_inode(k.off)?;
                    report.orphans += 1;
                } else {
                    st.del_orphan(k.off)?;
                }
            }
            st.commit()?;
        }
        report.mount_ns = crate::timer::now_ns() - t0;
        Ok((st, report))
    }

    fn from_sb(store: Store, sb: Superblock) -> State {
        State {
            store,
            tree: Child::Disk(sb.root),
            snaps: sb.snaps.clone(),
            clones: sb.clones.clone(),
            clone_trees: BTreeMap::new(),
            cur: super::items::LIVE,
            deadlist: sb.deadlist,
            next_ino: sb.next_ino,
            auto: sb.auto,
            flags: sb.flags,
            version: sb.version.max(BASE_VERSION),
            sb,
            meta_dirty: false,
            broken: false,
            commits: 0,
            dirty_since: None,
            batched_ops: 0,
        }
    }

    pub fn dirty(&self) -> bool {
        self.tree.is_dirty()
            || !self.store.dead_new.is_empty()
            || self.meta_dirty
            || self.clone_trees.values().any(|t| t.is_dirty())
    }

    pub fn check_writable(&self) -> Result<()> {
        if self.broken {
            return Err(Errno::EIO);
        }
        Ok(())
    }

    /// Make everything since the last commit durable. The superblock write is the commit
    /// point: until it lands, the previous generation is what a reboot sees.
    pub fn commit(&mut self) -> Result<()> {
        self.check_writable()?;
        if !self.dirty() {
            return Ok(());
        }
        let r = self.commit_inner();
        match r {
            Ok(sb) => {
                super::crashtest::LAST_COMMIT
                    .store(sb.generation, core::sync::atomic::Ordering::Relaxed);
                self.sb = sb;
                self.store.committed();
                self.meta_dirty = false;
                self.commits += 1;
                self.dirty_since = None;
                self.batched_ops = 0;
                Ok(())
            }
            Err(e) => {
                kprintln!(
                    "milkfs: commit of generation {} failed: {e}",
                    self.store.txn
                );
                self.broken = true;
                self.store.abandon();
                Err(e)
            }
        }
    }

    fn commit_inner(&mut self) -> Result<Superblock> {
        let root = btree::write(&mut self.tree, &mut self.store)?;
        for (id, tree) in self.clone_trees.iter_mut() {
            let r = btree::write(tree, &mut self.store)?;
            if let Some(c) = self.clones.iter_mut().find(|c| c.id == *id) {
                c.root = r;
            }
        }
        if !self.store.dead_new.is_empty() {
            let entries = core::mem::take(&mut self.store.dead_new);
            self.deadlist = self.store.write_deadlist(&entries, self.deadlist)?;
        }
        let generation = self.store.txn;
        // Last: the space map (every other allocation of this commit is done). On a
        // branched volume it also counts blocks waiting for the next sweep, like the
        // in-memory map does.
        let spacemap = self.store.write_spacemap(generation)?;
        let sb = Superblock {
            total_blocks: self.sb.total_blocks,
            generation,
            root,
            deadlist: self.deadlist,
            next_ino: self.next_ino,
            created: self.sb.created,
            uuid: self.sb.uuid,
            label: self.sb.label.clone(),
            snaps: self.snaps.clone(),
            clones: self.clones.clone(),
            auto: self.auto,
            flags: self.flags,
            version: self.version,
            spacemap,
        };
        let cache = &self.store.cache;
        cache.flush()?; // 1. every new block is durable
        cache.write_through(generation % 2, &sb.encode())?; // 2. the commit point
        cache.device().flush()?; // 3. and it is durable
        Ok(sb)
    }

    /// An operation finished: commit if the batch is old or big enough, or the block cache
    /// is filling up with dirty buffers. Otherwise the transaction stays open.
    pub fn op_done(&mut self) -> Result<()> {
        let now = crate::timer::now_ns();
        if !self.dirty() {
            return Ok(());
        }
        let since = *self.dirty_since.get_or_insert(now);
        self.batched_ops += 1;
        let (_, cap, dirty) = self.store.cache.usage();
        if self.batched_ops >= MAX_BATCH_OPS
            || now - since >= COMMIT_DELAY_NS
            || dirty * 4 >= cap * 3
        {
            return self.commit();
        }
        Ok(())
    }

    /// For the flusher thread: commit if the open transaction is older than the delay.
    pub fn flush_if_due(&mut self) -> Result<()> {
        match self.dirty_since {
            Some(t) if crate::timer::now_ns() - t >= COMMIT_DELAY_NS => self.commit(),
            _ => Ok(()),
        }
    }

    pub fn label(&self) -> &str {
        &self.sb.label
    }

    pub fn cache(&self) -> &Arc<BCache> {
        &self.store.cache
    }
}

/// Every block the committed state `sb` can reach: trees (live and snapshots, shared
/// subtrees once), their data blocks, dead-list chains. `live_item` sees every item of
/// the live tree.
pub fn reachable(
    store: &Store,
    sb: &Superblock,
    live_item: &mut dyn FnMut(&Key, &[u8]) -> Result<()>,
) -> Result<BTreeSet<u64>> {
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    let mut data: Vec<u64> = Vec::new();
    {
        let mut node = |p: &BlockPtr| seen.insert(p.block);
        let mut item = |k: &Key, v: &[u8]| -> Result<()> {
            data.extend(data_ptrs(k, v).iter().map(|p| p.block));
            live_item(k, v)
        };
        btree::walk(store, sb.root, &mut node, &mut item)?;
    }
    for s in sb.snaps.iter().chain(sb.clones.iter()) {
        let mut node = |p: &BlockPtr| seen.insert(p.block);
        let mut item = |k: &Key, v: &[u8]| -> Result<()> {
            data.extend(data_ptrs(k, v).iter().map(|p| p.block));
            Ok(())
        };
        btree::walk(store, s.root, &mut node, &mut item)?;
    }
    for head in core::iter::once(sb.deadlist).chain(sb.snaps.iter().map(|s| s.deadlist)) {
        let (_, chain) = store.read_deadlist(head)?;
        seen.extend(chain.iter().map(|p| p.block));
    }
    seen.extend(data);
    // The space map of the last commit (none right after a rebuilding mount).
    seen.extend(store.map_blocks().iter().map(|p| p.block));
    seen.insert(0);
    seen.insert(1);
    Ok(seen)
}

/// Space accounting check (for the self-test): after a commit, the blocks the allocator
/// considers used must be exactly the reachable ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceCheck {
    pub reachable: u64,
    pub allocated: u64,
    /// Reachable but marked free: would be overwritten (corruption).
    pub free_but_used: u64,
    /// Marked used but unreachable: a leak.
    pub leaked: u64,
}

impl State {
    /// Exact space reclaim: commit, then free every allocated block the committed state
    /// can't reach. Used once history is branched (dead lists can't be trusted then).
    pub fn sweep(&mut self) -> Result<u64> {
        self.commit()?;
        let r = reachable(&self.store, &self.sb, &mut |_, _| Ok(()))?;
        let mut n = 0;
        for b in 2..self.store.total() {
            if self.store.is_used(b) && !r.contains(&b) {
                self.store.sweep_free(b);
                n += 1;
            }
        }
        if n > 0 {
            // The map just committed still counts them: write one without.
            self.meta_dirty = true;
            self.commit()?;
        }
        Ok(n)
    }

    /// Mark every reachable block used (repair of a free map that lost some). Returns
    /// how many were marked free.
    pub fn mark_reachable(&mut self) -> Result<u64> {
        // No commit first: it could allocate one of those blocks. What the open
        // transaction dropped is still marked (pending) or on a dead list.
        let r = reachable(&self.store, &self.sb, &mut |_, _| Ok(()))?;
        let n = r.into_iter().filter(|&b| self.store.mark(b)).count() as u64;
        if n > 0 {
            self.meta_dirty = true;
            self.commit()?;
        }
        Ok(n)
    }

    pub fn branched(&self) -> bool {
        self.flags & SB_BRANCHED != 0
    }

    /// Enter branched mode: drop every dead list (a sweep reclaims their blocks).
    pub fn set_branched(&mut self) {
        self.flags |= SB_BRANCHED;
        self.store.branched = true;
        self.store.dead_new.clear();
        self.deadlist = BlockPtr::NONE;
        for s in self.snaps.iter_mut() {
            s.deadlist = BlockPtr::NONE;
        }
        self.meta_dirty = true;
    }

    /// No snapshots left: history is linear again.
    pub fn maybe_unbranch(&mut self) {
        if self.branched() && self.snaps.is_empty() {
            self.flags &= !SB_BRANCHED;
            self.store.branched = false;
            self.meta_dirty = true;
        }
    }

    pub fn verify_space(&mut self) -> Result<SpaceCheck> {
        self.commit()?;
        let r = reachable(&self.store, &self.sb, &mut |_, _| Ok(()))?;
        let total = self.store.total();
        let free_but_used = r.iter().filter(|&&b| !self.store.is_used(b)).count() as u64;
        let allocated = total - self.store.free_blocks();
        let leaked = (0..total)
            .filter(|b| self.store.is_used(*b) && !r.contains(b))
            .count() as u64;
        Ok(SpaceCheck {
            reachable: r.len() as u64,
            allocated,
            free_but_used,
            leaked,
        })
    }
}
