//! What `text`, `print` and `pause` share: opening their inputs, with plain-English errors.
#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use filekit::words::say_read;
use rt::sys;

/// The inputs to read: the files given, or stdin ("-") when there are none.
pub fn inputs<'a>(files: &[&'a str]) -> Vec<&'a str> {
    if files.is_empty() {
        alloc::vec!["-"]
    } else {
        files.to_vec()
    }
}

/// An fd to read `p` from ("-": stdin, fd 0). Err: what went wrong, in plain English.
pub fn open(p: &str) -> Result<i32, String> {
    if p == "-" {
        return Ok(0);
    }
    if sys::stat_path(p, true).is_ok_and(|s| s.is_dir()) {
        return Err(say_read(p, 21));
    }
    sys::open(p, sys::O_RDONLY | sys::O_CLOEXEC).map_err(|e| say_read(p, e.0))
}

/// Close what [`open`] gave (not stdin).
pub fn close(fd: i32) {
    if fd != 0 {
        let _ = sys::close(fd);
    }
}
