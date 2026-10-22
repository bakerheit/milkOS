//! Block devices: the [`BlockDevice`] trait drivers implement, a registry, and the
//! `/dev/<name>` inode that gives byte-granular access to one.
//!
//! Drivers call [`register`] (any time, even before `fs::init`); devfs publishes every
//! registered device as a block-special file. Disk file systems (milkfs, later) take an
//! `Arc<dyn BlockDevice>` from [`get`].

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::errno::{Errno, Result};
use super::node::{Inode, InodeType, Stat, makedev};
use crate::sync::SpinLock;

// The trait (and its types) is defined in the milkfs crate, which runs on it.
pub use ::milkfs::blockdev::{BlockDevice, BlockRef, IoStats};

/// Linux's dynamic block major that virtio-blk usually gets (`vda` = 254:0).
const BLOCK_MAJOR: u32 = 254;

static DEVICES: SpinLock<Vec<BlockRef>> = SpinLock::new(Vec::new());

/// Add a disk. Shows up as `/dev/<name>` once devfs is mounted (immediately, if it is).
pub fn register(dev: BlockRef) {
    let index = {
        let mut d = DEVICES.lock();
        d.push(dev.clone());
        d.len() - 1
    };
    if super::devfs::ids().is_some() {
        publish(index, &dev);
    }
}

/// All registered disks, in registration order.
pub fn devices() -> Vec<BlockRef> {
    DEVICES.lock().clone()
}

pub fn get(name: &str) -> Option<BlockRef> {
    DEVICES.lock().iter().find(|d| d.name() == name).cloned()
}

/// Create the `/dev` node for device `index`.
fn publish(index: usize, dev: &BlockRef) {
    let Some(ids) = super::devfs::ids() else {
        return;
    };
    let node = Arc::new(BlockDevNode {
        dev_no: ids.dev,
        ino: ids.next(),
        rdev: makedev(BLOCK_MAJOR, index as u32 * 16),
        disk: dev.clone(),
    });
    let _ = super::devfs::add_device(dev.name(), node);
}

/// Called by devfs once it is mounted: publish what registered before that.
pub(super) fn publish_all() {
    for (i, d) in devices().iter().enumerate() {
        publish(i, d);
    }
}

/// Largest single transfer the `/dev` node issues (drivers may split further).
const CHUNK: usize = 64 * 1024;

/// `/dev/vdX`: byte offsets over a block device (read-modify-write at unaligned edges).
pub struct BlockDevNode {
    dev_no: u64,
    ino: u64,
    rdev: u64,
    disk: BlockRef,
}

impl BlockDevNode {
    fn size(&self) -> u64 {
        self.disk.num_blocks() * self.disk.block_size() as u64
    }
}

impl Inode for BlockDevNode {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.dev_no
    }
    fn kind(&self) -> InodeType {
        InodeType::BlockDev
    }
    fn stat(&self) -> Stat {
        let perm = if self.disk.read_only() { 0o440 } else { 0o660 };
        let mut st = Stat::new(self.dev_no, self.ino, InodeType::BlockDev, perm);
        st.rdev = self.rdev;
        st.size = self.size();
        st.blksize = self.disk.block_size() as u32;
        st
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize> {
        let size = self.size();
        if off >= size {
            return Ok(0);
        }
        let bs = self.disk.block_size() as u64;
        let n = (buf.len() as u64).min(size - off).min(CHUNK as u64) as usize;
        let first = off / bs;
        let last = (off + n as u64).div_ceil(bs);
        let mut tmp = vec![0u8; ((last - first) * bs) as usize];
        self.disk.read_blocks(first, &mut tmp)?;
        let skip = (off - first * bs) as usize;
        buf[..n].copy_from_slice(&tmp[skip..skip + n]);
        Ok(n)
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> Result<usize> {
        if self.disk.read_only() {
            return Err(Errno::EROFS);
        }
        let size = self.size();
        if off >= size {
            return Err(Errno::ENOSPC);
        }
        let bs = self.disk.block_size() as u64;
        let n = (buf.len() as u64).min(size - off).min(CHUNK as u64) as usize;
        let first = off / bs;
        let last = (off + n as u64).div_ceil(bs);
        let skip = (off - first * bs) as usize;
        let mut tmp = vec![0u8; ((last - first) * bs) as usize];
        if skip != 0 || !n.is_multiple_of(bs as usize) {
            self.disk.read_blocks(first, &mut tmp)?; // partial edge blocks
        }
        tmp[skip..skip + n].copy_from_slice(&buf[..n]);
        self.disk.write_blocks(first, &tmp)?;
        Ok(n)
    }

    fn truncate(&self, _len: u64) -> Result<()> {
        Ok(()) // O_TRUNC on a device is ignored, as on Linux
    }

    fn sync(&self) -> Result<()> {
        self.disk.flush()
    }
}

/// One line per disk, for logs: `vda: 131072 x 512 B (64 MiB), rw`.
pub fn describe(d: &BlockRef) -> String {
    let bytes = d.num_blocks() * d.block_size() as u64;
    alloc::format!(
        "{}: {} x {} B ({} MiB), {}",
        d.name(),
        d.num_blocks(),
        d.block_size(),
        bytes >> 20,
        if d.read_only() { "ro" } else { "rw" }
    )
}
