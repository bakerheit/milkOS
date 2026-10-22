//! The rules behind `text`, `print`, `pause` and msh's `variables` (docs/shell.md, "Text,
//! print, pause and variables"). Nothing here touches the system: callers pass in the
//! arguments and the bytes, so every function is tested on the host
//! (`scripts/textkit-test.sh`).
//!
//! * [`duration`]: "5", "5s", "2m", "500ms" as nanoseconds (`pause`).
//! * [`escape`]: `print --escapes` (`\n`, `\t`, `\e`, `\\`, `\c`).
//! * [`lines`]: matching (`text find`), ordering (`text sort`), runs of equal lines
//!   (`text unique`), counting lines, words and bytes (`text count`), `first`/`last` counts.
//! * [`vars`]: variable names and `variables run NAME=v... COMMAND`.
//! * [`help`]: the `help` lists, in the same layout as `files help`.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod duration;
pub mod escape;
pub mod help;
pub mod lines;
pub mod vars;
