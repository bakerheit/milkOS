//! Host tests (`scripts/milkfs-test.sh`): the file system on a disk that is a byte array,
//! a fake clock and a seeded RNG, covering what the guest's milkfs tests check (each file
//! names the script it stands in for).

mod harness;
mod workload;

mod basic;
mod btree;
mod compat;
mod convert;
mod crash;
mod crypt;
mod fixture;
mod fsck;
mod reflink;
mod scrub;
mod send;
mod snapshots;
mod upgrade;
