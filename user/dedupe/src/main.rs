//! `dedupe`: find identical files under a directory on milkfs and make them share their
//! data blocks (copy-on-write, like `cp --reflink`), so the copies take no space until one
//! is changed.
//!
//!   dedupe DIR        share identical files (contents compared byte by byte first)
//!   dedupe -n DIR     only report what would be shared
//!
//! Files are grouped by size and content hash (from milkfs's block checksums, no data
//! read), compared, then all but the first of a group get its blocks. File times and
//! owners stay as they were. Blocks a snapshot still has stay used until that snapshot
//! is deleted. Talks to /sys/fs/milkfs/dedupe.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use rt::{eprintln, print, sys};

rt::main!(main);

const CTL: &str = "/sys/fs/milkfs/dedupe";

fn read_all(path: &str) -> Result<String, rt::Errno> {
    let fd = sys::open(path, sys::O_RDONLY)?;
    let mut out = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = sys::read(fd, &mut buf)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    let _ = sys::close(fd);
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn main(args: &rt::Args) -> i32 {
    let mut dry = false;
    let mut dir = None;
    for &a in &args.argv[1..] {
        match a {
            "-n" | "--dry-run" => dry = true,
            "-h" | "--help" => {
                eprintln!("usage: dedupe [-n] DIR");
                return 0;
            }
            d => dir = Some(d),
        }
    }
    let Some(dir) = dir else {
        eprintln!("usage: dedupe [-n] DIR");
        return 2;
    };
    let abs = if dir.starts_with('/') {
        String::from(dir)
    } else {
        let cwd = sys::getcwd().unwrap_or_else(|_| String::from("/"));
        format!("{}/{dir}", cwd.trim_end_matches('/'))
    };
    let req = if dry {
        format!("{abs} dry")
    } else {
        abs.clone()
    };
    let fd = match sys::open(CTL, 1) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("dedupe: {CTL}: {e}");
            return 1;
        }
    };
    let r = sys::write(fd, req.as_bytes());
    let _ = sys::close(fd);
    if let Err(e) = r {
        eprintln!(
            "dedupe: {abs}: {e}{}",
            if e.0 == 95 { " (not on milkfs)" } else { "" }
        );
        return 1;
    }
    match read_all(CTL) {
        Ok(s) => {
            print!("{s}");
            0
        }
        Err(e) => {
            eprintln!("dedupe: {CTL}: {e}");
            1
        }
    }
}
