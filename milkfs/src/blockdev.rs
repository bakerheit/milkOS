//! The disk milkfs runs on: the [`BlockDevice`] trait (the kernel's drivers implement it,
//! and the kernel re-exports it as `fs::blockdev::BlockDevice`; the host tests have file-
//! and memory-backed ones).

use alloc::sync::Arc;

use crate::errno::Result;

/// A disk. Offsets and lengths are in blocks of [`block_size`](Self::block_size) bytes;
/// buffers must be a whole number of blocks. Calls are synchronous.
pub trait BlockDevice: Send + Sync {
    /// Short name, used for `/dev/<name>` (`vda`, `vdb`, ...).
    fn name(&self) -> &str;
    fn block_size(&self) -> usize;
    fn num_blocks(&self) -> u64;
    fn read_only(&self) -> bool;
    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<()>;
    fn write_blocks(&self, lba: u64, buf: &[u8]) -> Result<()>;
    /// Make completed writes durable (a cache flush on the device).
    fn flush(&self) -> Result<()>;
    /// I/O counters for /proc/diskstats, if the driver keeps them.
    fn io_stats(&self) -> Option<IoStats> {
        None
    }
}

/// Cumulative I/O counters of a disk (`/proc/diskstats`).
#[derive(Clone, Copy, Default, Debug)]
pub struct IoStats {
    pub reads: u64,
    pub sectors_read: u64,
    pub writes: u64,
    pub sectors_written: u64,
}

pub type BlockRef = Arc<dyn BlockDevice>;
