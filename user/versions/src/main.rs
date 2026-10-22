//! `versions`: a file's history across milkfs snapshots ("Browse All Versions").
//!
//!   versions FILE                     list the distinct versions, oldest first
//!   versions restore FILE SNAPSHOT    put that version back (the current one is kept in a
//!                                     snapshot first if it isn't in one already)
//!
//! Talks to /System/Kernel/fs/milkfs/versions and /System/Kernel/fs/milkfs/restore.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use rt::{eprintln, println, sys};

rt::main!(main);

const O_WRONLY: u32 = 1;

fn write_ctl(path: &str, text: &str) -> Result<(), rt::Errno> {
    let fd = sys::open(path, O_WRONLY)?;
    let r = sys::write(fd, text.as_bytes());
    let _ = sys::close(fd);
    r.map(|_| ())
}

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

/// Unix seconds -> "YYYY-MM-DD HH:MM" (UTC).
fn date(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86400), t.rem_euclid(86400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    alloc::format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs / 3600,
        secs / 60 % 60
    )
}

fn size(n: u64) -> String {
    match n {
        0..1024 => alloc::format!("{n} B"),
        1024..1_048_576 => alloc::format!("{}.{} KB", n / 1024, n % 1024 * 10 / 1024),
        _ => alloc::format!("{}.{} MB", n >> 20, ((n & 0xfffff) * 10) >> 20),
    }
}

/// Absolute path for `p` (the kernel resolves from /, not our cwd).
fn absolute(p: &str) -> String {
    if p.starts_with('/') {
        return String::from(p);
    }
    let cwd = sys::getcwd().unwrap_or_else(|_| String::from("/"));
    alloc::format!("{}/{p}", cwd.trim_end_matches('/'))
}

fn why(e: rt::Errno) -> &'static str {
    match e.0 {
        2 => "no such file, or not in that snapshot",
        18 => "not on the milkfs volume (only /Volumes/Milk and /Users/milk have versions)",
        21 => "not a regular file",
        28 => "no room for the safety snapshot (30 snapshots + clones at most)",
        _ => e.msg(),
    }
}

fn list(file: &str) -> i32 {
    let path = absolute(file);
    if let Err(e) = write_ctl("/System/Kernel/fs/milkfs/versions", &path) {
        eprintln!("versions: {path}: {}", why(e));
        return 1;
    }
    let text = read_all("/System/Kernel/fs/milkfs/versions").unwrap_or_default();
    let rows: Vec<Vec<&str>> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.split('\t').collect())
        .filter(|r: &Vec<&str>| r.len() == 5)
        .collect();
    println!("{path}: {} version(s)", rows.len());
    for (i, r) in rows.iter().enumerate() {
        let mtime: i64 = r[2].parse().unwrap_or(0);
        let bytes: u64 = r[3].parse().unwrap_or(0);
        let label = if r[0] == "current" {
            String::from("(current)")
        } else {
            alloc::format!("in snapshot {}", r[0])
        };
        let n = if r[0] == "current" {
            String::from("*")
        } else {
            alloc::format!("{}", i + 1)
        };
        println!(
            "  {n:>2}  {}  {:>8}  {}  {}",
            date(mtime),
            size(bytes),
            &r[4][..8],
            label
        );
    }
    0
}

fn restore(file: &str, snap: &str) -> i32 {
    let path = absolute(file);
    match write_ctl(
        "/System/Kernel/fs/milkfs/restore",
        &alloc::format!("{snap} {path}"),
    ) {
        Ok(()) => {
            println!("versions: restored {path} from {snap}");
            0
        }
        Err(e) => {
            eprintln!("versions: restore {path} from {snap}: {}", why(e));
            1
        }
    }
}

fn main(args: &rt::Args) -> i32 {
    match (args.get(1), args.get(2), args.get(3)) {
        (Some("restore"), Some(f), Some(s)) => restore(f, s),
        (Some(f), None, None) if f != "restore" => list(f),
        _ => {
            eprintln!("usage: versions FILE | versions restore FILE SNAPSHOT");
            2
        }
    }
}
