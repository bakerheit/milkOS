//! Disk Utility's logic: text in, values out.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod backup;
pub mod disks;
pub mod fmt;
pub mod fsinfo;
pub mod mounts;
pub mod names;
pub mod scrub;
pub mod snaps;
