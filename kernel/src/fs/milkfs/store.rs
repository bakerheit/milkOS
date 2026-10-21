//! Block allocation and checksummed block I/O for one milkfs volume.
//!
//! The free map is not stored on disk: at mount it is rebuilt by marking every block
//! reachable from the committed superblock (trees, data, dead lists). So the allocator can
//! never disagree with the committed state after a crash.
//!
//! While a transaction (generation `txn`) is open, a block the live tree drops is handled
//! by its birth generation:
//! - born in `txn`: nothing committed can see it, reuse it right away;
//! - born after the newest snapshot: only the last commit sees it, free it once the next
//!   commit is durable (`pending_free`);
//! - otherwise a snapshot still uses it: it goes on the live dead list (`dead_new`).

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::format::{BLOCK, BlockPtr, DEAD_PER_BLOCK, DeadEntry, decode_dead, encode_dead, xxh64};
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
}

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
        self.unmark(b);
    }

    /// The commit is durable: hand back what it freed, start the next generation.
    pub fn committed(&mut self) {
        for b in core::mem::take(&mut self.pending_free) {
            self.unmark(b);
        }
        self.txn += 1;
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
