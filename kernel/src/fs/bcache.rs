//! The block cache (4 KiB buffers, LRU, write-back) lives in the milkfs crate, its only
//! user; re-exported here for the rest of the kernel (the self-test).

pub use ::milkfs::bcache::*;
