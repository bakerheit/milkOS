//! Shared bits for the desktop tools: send a command line to /dev/wm.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use rt::{eprintln, sys};

/// Write `line` (tab-separated fields, see wm/ctl.rs) to /dev/wm. Returns the exit code.
pub fn wm(line: &str) -> i32 {
    const O_WRONLY: u32 = 1;
    let fd = match sys::open("/dev/wm", O_WRONLY) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("no desktop here (/dev/wm: {e:?})");
            return 1;
        }
    };
    let r = sys::write_all(fd, format!("{line}\n").as_bytes());
    let _ = sys::close(fd);
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("the desktop refused '{line}': {e:?}");
            1
        }
    }
}

/// An argument made absolute against the current directory, if it looks like a path.
pub fn absolute(arg: &str) -> String {
    if arg.starts_with('/') {
        return String::from(arg);
    }
    let is_path = arg.contains('/') || arg.contains('.') || arg == "~";
    if !is_path {
        return String::from(arg); // an app name: files, notes, paint ...
    }
    if let Some(rest) = arg.strip_prefix("~") {
        return format!("/home/milk{rest}");
    }
    let cwd = sys::getcwd().unwrap_or_else(|_| String::from("/"));
    if cwd == "/" {
        format!("/{arg}")
    } else {
        format!("{cwd}/{arg}")
    }
}
