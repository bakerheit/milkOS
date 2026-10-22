//! The rules behind `processes`, `services`, `system`, `users` and `admin` (user/systools, which
//! replaced ps top kill, svc log, uname uptime free, whoami id passwd su, and sudo auth).
//! Nothing here touches the system: callers pass in what /proc, milkd or the journal said and
//! the time, so every function is tested on the host (`scripts/syskit-test.sh`).
//!
//! * [`help`]: each command's `help` text, in the format `files help` uses.
//! * [`args`]: how each command's words are read (subcommands, options, plain-English usage).
//! * [`signals`]: signal names and numbers.
//! * [`procs`]: `/proc/<pid>/status` as a row, picking a process by name or PID.
//! * [`journal`]: the journal's lines, the `services logs` filters.
//! * [`services`]: milkd's replies and what to print for them.
//! * [`time`]: uptimes in words, clock times.
//! * [`memory`]: `/proc/meminfo` in Activity Monitor's groups.
//! * [`fmt`]: aligned `key  value` lines.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod args;
pub mod fmt;
pub mod help;
pub mod journal;
pub mod memory;
pub mod procs;
pub mod services;
pub mod signals;
pub mod time;
