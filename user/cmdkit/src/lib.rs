//! The parsing and wording behind `network`, `disks`, `apps` and `search`. Nothing here
//! touches the system: callers pass in the text they read, so every function is tested on the
//! host (`scripts/cmdkit-test.sh`).
//!
//! * [`args`]: options, spelled out (`--count 3`) or short (`-c 3`), and the rest.
//! * [`net`]: /proc/net/ifaddr, /proc/net/dev and /proc/net/tcp text; URLs.
//! * [`disks`]: milkfs's errors and progress lines in words.
//! * [`search`]: the search index's answer lines, dates and sizes.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod args;
pub mod disks;
pub mod net;
pub mod search;
