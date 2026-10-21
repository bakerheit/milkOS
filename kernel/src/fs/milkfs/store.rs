//! Block allocation and checksummed block I/O for one milkfs volume.
//!
//! The free map is written at every commit as part of it (the space map: an index block
//! and one bitmap block per 32768 blocks, fresh blocks each time, pointed at by the
//! superblock), describing exactly the generation being committed. Mount loads it when
//! it is intact and of the superblock's generation; otherwise (no map, damaged, stale)
//! it rebuilds the map by marking every block reachable from the
//! committed superblock (trees, data, dead lists), as before.
//!
//! While a transaction (generation `txn`) is open, a block the live tree drops is handled
//! by its birth generation:
//! - born in `txn`: nothing committed can see it, reuse it right away;
//! - born after the newest snapshot: only the last commit sees it, free it once the next
//!   commit is durable (`pending_free`);
//! - otherwise a snapshot still uses it: it goes on the live dead list (`dead_new`).

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::format::{
    BLOCK, BlockPtr, DEAD_PER_BLOCK, DeadEntry, MAP_BITS, MAP_MAX_CHUNKS, MapIndex, decode_dead,
    encode_dead, xxh64,
};
use crate::fs::bcache::BCache;
use crate::fs::errno::{Errno, Result};

pub struct Store {
    pub cache: Arc<BCache>,
    /// Generation being built (last committed + 1).
    pub txn: u64,
    /// Generation of the newest snapshot (0: none).
    pub snap_gen: u64,
    nblocks: u64,
    bitmap: Vec<u64>,
    free: u64,
    hint: u64,
    pending_free: Vec<u64>,
    pub dead_new: Vec<DeadEntry>,
    /// Writing to a clone of the snapshot at this generation: blocks born after it are the
    /// clone's alone, older ones belong to the origin snapshot (never freed by the clone).
    pub clone_origin: Option<u64>,
    /// History is branched: no dead lists; a block a snapshot may still have is left
    /// for the next reachability sweep instead.
    pub branched: bool,
    /// Decoded tree nodes by (block, checksum): blocks are copy-on-write and only reused
    /// with new contents (a new checksum), so an entry can never be stale.
    nodes: crate::sync::SpinLock<NodeCache>,
    /// Blocks of the space map the last commit wrote (index first): freed by the next.
    map_blocks: Vec<BlockPtr>,
    /// While a receive builds its clone: the origin's blocks the clone drops (the live
    /// tree's dead list once the clone becomes live, see `clone_fast_forward`).
    pub recv_drops: Option<Vec<DeadEntry>>,
    /// While a scrub reads the committed state it started from: blocks freed meanwhile
    /// stay allocated (not reused, so nothing it is about to read is overwritten) but
    /// count as free in the space map; released when the scrub ends.
    held: Option<alloc::collections::BTreeSet<u64>>,
}

#[derive(Default)]
struct NodeCache {
    map: alloc::collections::BTreeMap<(u64, u64), (alloc::sync::Arc<super::format::DiskNode>, u64)>,
    lru: alloc::collections::BTreeMap<u64, (u64, u64)>,
    tick: u64,
}

/// Decoded nodes kept.
const NODE_CACHE: usize = 512;

impl Store {
    /// A store with every block free except the two superblocks. Mount marks the rest.
    pub fn new(cache: Arc<BCache>, nblocks: u64, txn: u64, snap_gen: u64) -> Store {
        let mut s = Store {
            cache,
            txn,
            snap_gen,
            nblocks,
            bitmap: alloc::vec![0; nblocks.div_ceil(64) as usize],
            free: nblocks,
            hint: 2,
            pending_free: Vec::new(),
            dead_new: Vec::new(),
            clone_origin: None,
            branched: false,
            nodes: crate::sync::SpinLock::new(NodeCache::default()),
            map_blocks: Vec::new(),
            recv_drops: None,
            held: None,
        };
        s.mark(0);
        s.mark(1);
        s
    }

    pub fn total(&self) -> u64 {
        self.nblocks
    }

    pub fn free_blocks(&self) -> u64 {
        self.free
    }

    pub fn is_used(&self, b: u64) -> bool {
        self.bitmap[(b / 64) as usize] >> (b % 64) & 1 != 0
    }

    /// Mark `b` used. Returns false if it already was.
    pub fn mark(&mut self, b: u64) -> bool {
        if b >= self.nblocks || self.is_used(b) {
            return false;
        }
        self.bitmap[(b / 64) as usize] |= 1 << (b % 64);
        self.free -= 1;
        true
    }

    fn unmark(&mut self, b: u64) {
        if b < self.nblocks && self.is_used(b) {
            self.bitmap[(b / 64) as usize] &= !(1 << (b % 64));
            self.free += 1;
        }
    }

    pub fn alloc(&mut self) -> Result<u64> {
        if self.free == 0 {
            return Err(Errno::ENOSPC);
        }
        for i in 0..self.nblocks {
            let b = 2 + (self.hint - 2 + i) % (self.nblocks - 2);
            if !self.is_used(b) {
                self.mark(b);
                self.hint = b + 1;
                if self.hint >= self.nblocks {
                    self.hint = 2;
                }
                return Ok(b);
            }
        }
        Err(Errno::ENOSPC)
    }

    /// The decoded tree node at `p` (checksum verified), shared from the node cache.
    pub fn node(&self, p: BlockPtr) -> Result<alloc::sync::Arc<super::format::DiskNode>> {
        let key = (p.block, p.csum);
        {
            let mut c = self.nodes.lock();
            c.tick += 1;
            let now = c.tick;
            if let Some((n, t)) = c.map.get(&key).cloned() {
                c.lru.remove(&t);
                c.lru.insert(now, key);
                c.map.insert(key, (n.clone(), now));
                return Ok(n);
            }
        }
        let n = alloc::sync::Arc::new(super::btree::decode_at(self, p)?);
        let mut c = self.nodes.lock();
        if c.map.len() >= NODE_CACHE
            && let Some((&t, &old)) = c.lru.iter().next()
        {
            c.lru.remove(&t);
            c.map.remove(&old);
        }
        c.tick += 1;
        let now = c.tick;
        c.lru.insert(now, key);
        c.map.insert(key, (n.clone(), now));
        Ok(n)
    }

    /// Allocate up to `want` consecutive blocks (at least one): `(first, count)`.
    pub fn alloc_run(&mut self, want: usize) -> Result<(u64, usize)> {
        let first = self.alloc()?;
        let mut got = 1;
        while got < want {
            let b = first + got as u64;
            if b >= self.nblocks || !self.mark(b) {
                break; // used (mark refuses) or past the end
            }
            got += 1;
        }
        self.hint = first + got as u64;
        if self.hint >= self.nblocks {
            self.hint = 2;
        }
        Ok((first, got))
    }

    /// Read `csums.len()` consecutive blocks starting at `block` into `out` (one device
    /// request for whatever isn't cached), verifying every block's checksum.
    pub fn read_run(&self, block: u64, csums: &[u64], out: &mut [u8]) -> Result<()> {
        self.cache
            .read_run(block, &mut out[..csums.len() * BLOCK])?;
        for (i, c) in csums.iter().enumerate() {
            if xxh64(&out[i * BLOCK..(i + 1) * BLOCK]) != *c {
                crate::kprintln!("milkfs: checksum mismatch in block {}", block + i as u64);
                return Err(Errno::EIO);
            }
        }
        Ok(())
    }

    /// Drop a block the live tree referenced (see the module docs for the rules).
    pub fn release(&mut self, p: BlockPtr) {
        if p.is_none() {
            return;
        }
        if let Some(origin) = self.clone_origin {
            // Clone tree: no snapshots of clones, so a clone-born block is unshared.
            if p.birth >= self.txn {
                self.unmark(p.block);
            } else if p.birth > origin {
                self.pending_free.push(p.block);
            } else if let Some(d) = self.recv_drops.as_mut()
                && !self.branched
            {
                d.push(DeadEntry {
                    block: p.block,
                    birth: p.birth,
                });
            }
            return;
        }
        if p.birth >= self.txn {
            self.unmark(p.block);
        } else if p.birth > self.snap_gen {
            self.pending_free.push(p.block);
        } else if !self.branched {
            self.dead_new.push(DeadEntry {
                block: p.block,
                birth: p.birth,
            });
        }
    }

    /// Free a block once the next commit is durable, whatever its birth (dead-list chain
    /// blocks, and blocks a snapshot deletion or rollback found unreferenced).
    pub fn free_after_commit(&mut self, block: u64, birth: u64) {
        if birth >= self.txn {
            self.unmark(block);
        } else {
            self.pending_free.push(block);
        }
    }

    /// Free a block a sweep found unreachable from the durable committed state.
    pub fn sweep_free(&mut self, b: u64) {
        self.free_committed(b);
    }

    /// Free a block some committed state had (held back while a scrub runs).
    fn free_committed(&mut self, b: u64) {
        let used = self.is_used(b);
        match self.held.as_mut() {
            Some(h) if used => {
                h.insert(b);
            }
            _ => self.unmark(b),
        }
    }

    /// The commit is durable: hand back what it freed, start the next generation.
    pub fn committed(&mut self) {
        for b in core::mem::take(&mut self.pending_free) {
            self.free_committed(b);
        }
        self.txn += 1;
    }

    /// A scrub starts: hold freed blocks back until `release_held`.
    pub fn hold_frees(&mut self) {
        self.held.get_or_insert_with(Default::default);
    }

    /// The scrub is done: free what was held.
    pub fn release_held(&mut self) {
        for b in self.held.take().unwrap_or_default() {
            self.unmark(b);
        }
    }

    pub fn is_held(&self, b: u64) -> bool {
        self.held.as_ref().is_some_and(|h| h.contains(&b))
    }

    pub fn held_count(&self) -> u64 {
        self.held.as_ref().map_or(0, |h| h.len() as u64)
    }

    /// Something went wrong mid-commit: forget the frees so nothing reachable from the
    /// on-disk state is reused.
    pub fn abandon(&mut self) {
        self.pending_free.clear();
    }

    pub fn read(&self, p: BlockPtr, out: &mut [u8; BLOCK]) -> Result<()> {
        self.cache.read(p.block, out)?;
        if xxh64(out) != p.csum {
            crate::kprintln!(
                "milkfs: checksum mismatch in block {} (birth {})",
                p.block,
                p.birth
            );
            return Err(Errno::EIO);
        }
        Ok(())
    }

    /// Write `data` to a freshly allocated block, born in this transaction.
    pub fn write_new(&mut self, data: &[u8; BLOCK]) -> Result<BlockPtr> {
        let block = self.alloc()?;
        self.cache.write(block, data)?;
        Ok(BlockPtr {
            block,
            birth: self.txn,
            csum: xxh64(data),
        })
    }

    // ---- space map -----------------------------------------------------------------

    /// Write the space map of generation `generation` (the commit being built), after
    /// every other allocation of the commit. It records the blocks in use once the commit
    /// is durable: what is allocated now, minus what the commit frees (`pending_free`, the
    /// previous map among it), plus its own blocks. Returns the index pointer (none if the
    /// volume is too big for one index block: mounts then rebuild).
    pub fn write_spacemap(&mut self, generation: u64) -> Result<BlockPtr> {
        for p in core::mem::take(&mut self.map_blocks) {
            self.free_after_commit(p.block, p.birth);
        }
        let n = self.nblocks.div_ceil(MAP_BITS) as usize;
        if n > MAP_MAX_CHUNKS {
            return Ok(BlockPtr::NONE);
        }
        let index_block = self.alloc()?;
        let mut chunk_blocks = Vec::with_capacity(n);
        for _ in 0..n {
            chunk_blocks.push(self.alloc()?);
        }
        let mut bits = self.bitmap.clone();
        let held = self.held.iter().flatten();
        for &b in self.pending_free.iter().chain(held) {
            bits[(b / 64) as usize] &= !(1 << (b % 64));
        }
        let used: u64 = bits.iter().map(|w| w.count_ones() as u64).sum();
        let words = BLOCK / 8;
        let mut buf = [0u8; BLOCK];
        let mut chunks = Vec::with_capacity(n);
        for (i, &blk) in chunk_blocks.iter().enumerate() {
            buf.fill(0);
            for (j, w) in bits.iter().skip(i * words).take(words).enumerate() {
                buf[j * 8..j * 8 + 8].copy_from_slice(&w.to_le_bytes());
            }
            self.cache.write(blk, &buf)?;
            chunks.push(BlockPtr {
                block: blk,
                birth: self.txn,
                csum: xxh64(&buf),
            });
        }
        let idx = MapIndex {
            generation,
            total: self.nblocks,
            used,
            chunks,
        };
        idx.encode(&mut buf);
        self.cache.write(index_block, &buf)?;
        let ip = BlockPtr {
            block: index_block,
            birth: self.txn,
            csum: xxh64(&buf),
        };
        self.map_blocks.push(ip);
        self.map_blocks.extend(idx.chunks.iter().copied());
        Ok(ip)
    }

    /// The index and chunk pointers of the map at `p` (index checksum verified).
    pub fn read_map_index(&self, p: BlockPtr) -> Result<MapIndex> {
        let mut buf = [0u8; BLOCK];
        self.read(p, &mut buf)?;
        MapIndex::decode(&buf).ok_or(Errno::EIO)
    }

    /// Load the space map at `p` for generation `generation` as the allocator state.
    /// `must_be_used` are blocks the superblock points at directly (cheap sanity check).
    /// Err = a reason; the store is unchanged then and mount rebuilds.
    pub fn load_spacemap(
        &mut self,
        p: BlockPtr,
        generation: u64,
        must_be_used: &[u64],
    ) -> core::result::Result<(), &'static str> {
        let idx = self.read_map_index(p).map_err(|_| "index unreadable")?;
        if idx.generation != generation {
            return Err("is stale (another generation)");
        }
        if idx.total != self.nblocks || idx.chunks.len() != self.nblocks.div_ceil(MAP_BITS) as usize
        {
            return Err("doesn't match the volume size");
        }
        let words = BLOCK / 8;
        let mut bits = alloc::vec![0u64; self.bitmap.len()];
        let mut buf = [0u8; BLOCK];
        for (i, c) in idx.chunks.iter().enumerate() {
            self.read(*c, &mut buf).map_err(|_| "chunk unreadable")?;
            for (j, w) in bits.iter_mut().skip(i * words).take(words).enumerate() {
                *w = u64::from_le_bytes(buf[j * 8..j * 8 + 8].try_into().unwrap());
            }
        }
        // Nothing past the end, counts agree, the blocks we know of are in use.
        if !self.nblocks.is_multiple_of(64) {
            let last = bits.len() - 1;
            if bits[last] >> (self.nblocks % 64) != 0 {
                return Err("marks blocks past the end");
            }
        }
        let used: u64 = bits.iter().map(|w| w.count_ones() as u64).sum();
        if used != idx.used || used > self.nblocks {
            return Err("count doesn't match");
        }
        let is = |b: u64| b < self.nblocks && bits[(b / 64) as usize] >> (b % 64) & 1 != 0;
        let mine = core::iter::once(p.block).chain(idx.chunks.iter().map(|c| c.block));
        if !must_be_used.iter().copied().chain(mine).all(is) || !is(0) || !is(1) {
            return Err("marks a block in use as free");
        }
        self.bitmap = bits;
        self.free = self.nblocks - used;
        self.map_blocks = core::iter::once(p)
            .chain(idx.chunks.iter().copied())
            .collect();
        Ok(())
    }

    /// Blocks of the map the last commit wrote (for reachability checks).
    pub fn map_blocks(&self) -> &[BlockPtr] {
        &self.map_blocks
    }

    // ---- dead lists ----------------------------------------------------------------

    /// Every entry of the chain starting at `head`, and the chain blocks themselves.
    pub fn read_deadlist(&self, head: BlockPtr) -> Result<(Vec<DeadEntry>, Vec<BlockPtr>)> {
        let (mut entries, mut chain) = (Vec::new(), Vec::new());
        let mut p = head;
        let mut buf = [0u8; BLOCK];
        while !p.is_none() {
            if chain.len() as u64 > self.nblocks {
                return Err(Errno::EIO); // loop
            }
            self.read(p, &mut buf)?;
            let (e, next) = decode_dead(&buf)?;
            entries.extend(e);
            chain.push(p);
            p = next;
        }
        Ok((entries, chain))
    }

    /// Write `entries` as a new chain in front of `next`. Returns the new head.
    pub fn write_deadlist(&mut self, entries: &[DeadEntry], next: BlockPtr) -> Result<BlockPtr> {
        let mut head = next;
        let mut buf = [0u8; BLOCK];
        for chunk in entries.chunks(DEAD_PER_BLOCK).rev() {
            encode_dead(chunk, head, &mut buf);
            head = self.write_new(&buf)?;
        }
        Ok(head)
    }
}
