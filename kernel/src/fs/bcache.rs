//! Block cache: a fixed number of 4 KiB buffers over a [`BlockDevice`], LRU replacement,
//! write-back.
//!
//! Cache blocks are [`BLOCK`] bytes (8 sectors on virtio-blk). `write` only marks a buffer
//! dirty; dirty buffers reach the disk when they are evicted or on [`BCache::flush`], which
//! writes every dirty buffer in block order and then issues a device cache flush.
//! [`BCache::write_through`] bypasses the write-back (and is what a file system uses for
//! its commit record, after a `flush`).
//!
//! The lock is a `sched::Mutex`: it is held across device I/O, which sleeps.
//!
//! Ordering guarantee callers can rely on: after `flush` returns Ok, everything written
//! before it is durable. Nothing else about write order is promised (evictions write early).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::blockdev::BlockRef;
use super::errno::{Errno, Result};
use crate::sched::Mutex;

/// Cache block size.
pub const BLOCK: usize = 4096;

pub type Block = [u8; BLOCK];

struct Buf {
    blk: u64,
    dirty: bool,
    /// LRU clock value of the last access.
    used: u64,
    data: Box<Block>,
}

struct Inner {
    bufs: Vec<Buf>,
    /// block number -> index into `bufs`
    map: BTreeMap<u64, usize>,
    clock: u64,
    stats: Stats,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub hits: u64,
    pub misses: u64,
    pub writebacks: u64,
    pub flushes: u64,
}

pub struct BCache {
    dev: BlockRef,
    /// Device sectors per cache block.
    per_block: u64,
    blocks: u64,
    capacity: usize,
    inner: Mutex<Inner>,
}

impl BCache {
    /// A cache of `capacity` buffers over `dev`. The device block size must divide 4096.
    pub fn new(dev: BlockRef, capacity: usize) -> Result<Arc<BCache>> {
        let bs = dev.block_size();
        if bs == 0 || !BLOCK.is_multiple_of(bs) || capacity == 0 {
            return Err(Errno::EINVAL);
        }
        let per_block = (BLOCK / bs) as u64;
        Ok(Arc::new(BCache {
            blocks: dev.num_blocks() / per_block,
            dev,
            per_block,
            capacity,
            inner: Mutex::new(Inner {
                bufs: Vec::new(),
                map: BTreeMap::new(),
                clock: 0,
                stats: Stats::default(),
            }),
        }))
    }

    pub fn device(&self) -> &BlockRef {
        &self.dev
    }

    /// Number of 4 KiB blocks on the device.
    pub fn num_blocks(&self) -> u64 {
        self.blocks
    }

    pub fn stats(&self) -> Stats {
        self.inner.lock().stats
    }

    /// Buffers in use / capacity, dirty count.
    pub fn usage(&self) -> (usize, usize, usize) {
        let i = self.inner.lock();
        (
            i.bufs.len(),
            self.capacity,
            i.bufs.iter().filter(|b| b.dirty).count(),
        )
    }

    fn check(&self, blk: u64) -> Result<()> {
        if blk >= self.blocks {
            return Err(Errno::ENXIO);
        }
        Ok(())
    }

    fn dev_read(&self, blk: u64, buf: &mut Block) -> Result<()> {
        self.dev.read_blocks(blk * self.per_block, buf)
    }

    fn dev_write(&self, blk: u64, buf: &Block) -> Result<()> {
        self.dev.write_blocks(blk * self.per_block, buf)
    }

    /// Find or make a buffer for `blk`. `load`: read it from the device on a miss.
    fn slot(&self, i: &mut Inner, blk: u64, load: bool) -> Result<usize> {
        i.clock += 1;
        let now = i.clock;
        if let Some(&idx) = i.map.get(&blk) {
            i.stats.hits += 1;
            i.bufs[idx].used = now;
            return Ok(idx);
        }
        i.stats.misses += 1;
        let idx = if i.bufs.len() < self.capacity {
            i.bufs.push(Buf {
                blk,
                dirty: false,
                used: now,
                data: Box::new([0; BLOCK]),
            });
            i.bufs.len() - 1
        } else {
            // Evict the least recently used buffer, writing it back first if dirty.
            let idx = (0..i.bufs.len()).min_by_key(|&k| i.bufs[k].used).unwrap();
            if i.bufs[idx].dirty {
                let old = i.bufs[idx].blk;
                self.dev_write(old, &i.bufs[idx].data)?;
                i.stats.writebacks += 1;
                i.bufs[idx].dirty = false;
            }
            let old = i.bufs[idx].blk;
            i.map.remove(&old);
            let b = &mut i.bufs[idx];
            b.blk = blk;
            b.used = now;
            idx
        };
        if load {
            let mut tmp = [0u8; BLOCK];
            if let Err(e) = self.dev_read(blk, &mut tmp) {
                // Leave the buffer unmapped (and first in line for reuse).
                i.bufs[idx].blk = u64::MAX;
                i.bufs[idx].used = 0;
                return Err(e);
            }
            *i.bufs[idx].data = tmp;
        }
        i.map.insert(blk, idx);
        Ok(idx)
    }

    /// Copy block `blk` into `out`.
    pub fn read(&self, blk: u64, out: &mut Block) -> Result<()> {
        self.check(blk)?;
        let mut i = self.inner.lock();
        let idx = self.slot(&mut i, blk, true)?;
        out.copy_from_slice(&*i.bufs[idx].data);
        Ok(())
    }

    /// Replace block `blk` with `data` (write-back: it reaches the disk later).
    pub fn write(&self, blk: u64, data: &Block) -> Result<()> {
        self.check(blk)?;
        let mut i = self.inner.lock();
        let idx = self.slot(&mut i, blk, false)?;
        let b = &mut i.bufs[idx];
        b.data.copy_from_slice(data);
        b.dirty = true;
        Ok(())
    }

    /// Write `data` to the device now (and keep the cached copy in sync). No device flush.
    pub fn write_through(&self, blk: u64, data: &Block) -> Result<()> {
        self.check(blk)?;
        let mut i = self.inner.lock();
        let idx = self.slot(&mut i, blk, false)?;
        self.dev_write(blk, data)?;
        let b = &mut i.bufs[idx];
        b.data.copy_from_slice(data);
        b.dirty = false;
        Ok(())
    }

    /// Write every dirty buffer (in block order), then flush the device's cache.
    pub fn flush(&self) -> Result<()> {
        let mut i = self.inner.lock();
        let dirty: Vec<(u64, usize)> = i
            .map
            .iter()
            .filter(|&(_, &idx)| i.bufs[idx].dirty)
            .map(|(&b, &idx)| (b, idx))
            .collect();
        for (blk, idx) in dirty {
            self.dev_write(blk, &i.bufs[idx].data)?;
            i.bufs[idx].dirty = false;
            i.stats.writebacks += 1;
        }
        i.stats.flushes += 1;
        drop(i);
        self.dev.flush()
    }

    /// Forget everything cached, without writing dirty buffers (they are lost). For
    /// re-reading a device after something else changed it, or for tests.
    pub fn discard(&self) {
        let mut i = self.inner.lock();
        i.bufs.clear();
        i.map.clear();
    }
}
