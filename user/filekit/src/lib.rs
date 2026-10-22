//! The rules behind `files`, `trash`, `trashd` and the Files app's Trash (docs/filesystem.md,
//! "Files and the Trash"). Nothing here touches the system: callers pass in what is on disk
//! and the time, so the kernel and user space share one set of rules and every function is
//! tested on the host (`scripts/filekit-test.sh`).
//!
//! * [`path`]: paths, the trailing-slash rule, where a copy or move lands.
//! * [`trash`]: the Trash's layout, its metadata files, free names, how long items stay.
//! * [`words`]: errors in plain English ("notes.txt doesn't exist").
//! * [`date`]: Unix seconds as "Oct 20 2026, 23:01".

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod date;
pub mod path;
pub mod trash;
pub mod words;
