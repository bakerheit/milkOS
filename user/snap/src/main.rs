//! `snap`: milkfs snapshots from the shell, via the control files in /sys/fs/milkfs.
//!
//!   snap                  list snapshots of /Volumes/Milk
//!   snap create NAME      take one (browse it at /Volumes/Milk/.snapshots/NAME)
//!   snap delete NAME
//!   snap rollback NAME    make the volume what NAME captured (newer snapshots go away)
//!   snap info             volume status
//!   snap diff A [B|live]  what changed between two snapshots (or a snapshot and now)
//!   snap check [--repair] check the volume (and fix what is safe to fix)
//!   snap clones           list clones
//!   snap clone SNAP NAME  writable clone of SNAP, mounted at /Volumes/Milk-NAME
//!   snap promote NAME     the clone becomes the volume (old state kept as before-NAME)
//!   snap discard NAME     throw the clone away
//!   snap auto [SETTINGS]  automatic snapshots: show, or set e.g. `on interval=15m keep=8 daily=14`
//!   snap send SNAP [--from OLDER] [-o FILE]
//!                         SNAP as a stream (incremental from OLDER) to FILE or stdout
//!   snap receive [FILE] [--to PATH] [--force]
//!                         recreate the snapshot in a stream (FILE or stdin) on the milkfs
//!                         volume PATH is on (default /Volumes/Milk); --force rolls the
//!                         target back to the stream's base first

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use rt::{eprintln, print, println, sys};

rt::main!(main);

const O_WRONLY: u32 = 1;

fn cat(path: &str) -> i32 {
    let fd = match sys::open(path, sys::O_RDONLY) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("snap: {path}: {e}");
            return 1;
        }
    };
    let mut out = Vec::new();
    let mut buf = [0u8; 512];
    while let Ok(n) = sys::read(fd, &mut buf) {
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    let _ = sys::close(fd);
    let text = core::str::from_utf8(&out).unwrap_or("");
    if text.is_empty() && path.ends_with("snapshots") {
        println!("(no snapshots)");
    } else if text.is_empty() && path.ends_with("clones") {
        println!("(no clones)");
    } else {
        print!("{text}");
    }
    0
}

/// Errors this tool can get from milkfs, in words (rt's table is short).
fn why(e: rt::Errno) -> alloc::string::String {
    let text = match e.0 {
        2 => {
            "no such snapshot or clone (for a receive: the stream's base snapshot isn't on the target)"
        }
        5 => "I/O error or damaged stream",
        16 => {
            "busy: a clone is built on that snapshot, a send is reading it, or (receive) the target changed since the base snapshot: --force rolls it back"
        }
        39 => "a full stream needs an empty volume",
        18 => "not a milkfs volume",
        17 => "that name is taken",
        19 => "no milkfs volume",
        22 => "invalid name or setting",
        28 => "no room: 30 snapshots + clones at most",
        30 => "read-only",
        36 => "name too long (47 max)",
        _ => e.name(),
    };
    alloc::format!("{text} (errno {})", e.0)
}

/// Like `control` but silent on success (for commands whose result is read back).
fn control_quiet(op: &str, arg: &str) -> i32 {
    let path = alloc::format!("/sys/fs/milkfs/{op}");
    let r = sys::open(&path, O_WRONLY).and_then(|fd| {
        let r = sys::write(fd, arg.as_bytes());
        let _ = sys::close(fd);
        r
    });
    match r {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("snap: {op} {arg}: {}", why(e));
            1
        }
    }
}

fn control(op: &str, name: &str) -> i32 {
    let path = alloc::format!("/sys/fs/milkfs/{op}");
    let r = sys::open(&path, O_WRONLY).and_then(|fd| {
        let r = sys::write(fd, name.as_bytes());
        let _ = sys::close(fd);
        r
    });
    match r {
        Ok(_) => {
            println!("snap: {op} {name}: ok");
            0
        }
        Err(e) => {
            eprintln!("snap: {op} {name}: {}", why(e));
            1
        }
    }
}

fn abs(p: &str) -> alloc::string::String {
    if p.starts_with('/') {
        alloc::string::String::from(p)
    } else {
        let cwd = sys::getcwd().unwrap_or_else(|_| alloc::string::String::from("/"));
        alloc::format!("{}/{p}", cwd.trim_end_matches('/'))
    }
}

/// Copy everything from `from` to `to` (fds).
fn pump(from: i32, to: i32) -> Result<u64, rt::Errno> {
    let mut buf = alloc::vec![0u8; 64 << 10];
    let mut n = 0u64;
    loop {
        let k = sys::read(from, &mut buf)?;
        if k == 0 {
            return Ok(n);
        }
        sys::write_all(to, &buf[..k])?;
        n += k as u64;
    }
}

fn report(ctl: &str) {
    if let Ok(d) = sys::read_file(ctl) {
        eprintln!(
            "snap: {}",
            core::str::from_utf8(&d).unwrap_or("").trim_end()
        );
    }
}

fn send(args: &[&str]) -> i32 {
    let (mut snap, mut from, mut out) = (None, None, None);
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        match a {
            "--from" => from = it.next().copied(),
            "-o" => out = it.next().copied(),
            s => snap = Some(s),
        }
    }
    let Some(snap) = snap else {
        eprintln!("usage: snap send SNAP [--from OLDER] [-o FILE]");
        return 2;
    };
    let tmp = alloc::format!("/tmp/.snap-send-{}", sys::getpid());
    let path = out.map(abs).unwrap_or_else(|| tmp.clone());
    let req = match from {
        Some(f) => alloc::format!("{snap} {f} {path}"),
        None => alloc::format!("{snap} {path}"),
    };
    if control_quiet("send", &req) != 0 {
        let _ = sys::unlink(&tmp);
        return 1;
    }
    report("/sys/fs/milkfs/send");
    if out.is_none() {
        let r = sys::open(&tmp, sys::O_RDONLY).and_then(|fd| {
            let r = pump(fd, 1);
            let _ = sys::close(fd);
            r
        });
        let _ = sys::unlink(&tmp);
        if let Err(e) = r {
            eprintln!("snap: send to stdout: {e}");
            return 1;
        }
    }
    0
}

fn receive(args: &[&str]) -> i32 {
    let (mut file, mut to, mut force) = (None, None, false);
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        match a {
            "--to" => to = it.next().copied(),
            "--force" | "-F" => force = true,
            f => file = Some(f),
        }
    }
    let tmp = alloc::format!("/tmp/.snap-recv-{}", sys::getpid());
    let path = match file {
        Some(f) => abs(f),
        None => {
            // A stream on stdin: the kernel reads it from a file.
            let r = sys::open_mode(&tmp, O_WRONLY | sys::O_CREAT | sys::O_TRUNC, 0o600).and_then(
                |fd| {
                    let r = pump(0, fd);
                    let _ = sys::close(fd);
                    r
                },
            );
            if let Err(e) = r {
                eprintln!("snap: receive from stdin: {e}");
                let _ = sys::unlink(&tmp);
                return 1;
            }
            tmp.clone()
        }
    };
    let mut req = path;
    if let Some(t) = to {
        req = alloc::format!("{req} {}", abs(t));
    }
    if force {
        req.push_str(" force");
    }
    let r = control_quiet("receive", &req);
    let _ = sys::unlink(&tmp);
    if r == 0 {
        report("/sys/fs/milkfs/receive");
    }
    r
}

fn main(args: &rt::Args) -> i32 {
    match (args.get(1), args.get(2)) {
        (Some("send"), _) => send(&args.argv[2..]),
        (Some("receive"), _) => receive(&args.argv[2..]),
        (None, _) | (Some("list"), _) => cat("/sys/fs/milkfs/snapshots"),
        (Some("info"), _) => cat("/sys/fs/milkfs/info"),
        (Some("diff"), Some(a)) => {
            let b = args.get(3).unwrap_or("live");
            match control_quiet("diff", &alloc::format!("{a} {b}")) {
                0 => cat("/sys/fs/milkfs/diff"),
                e => e,
            }
        }
        (Some("check"), opt) => {
            let mode = if opt == Some("--repair") {
                "repair"
            } else {
                "check"
            };
            match control_quiet("check", mode) {
                0 => {
                    cat("/sys/fs/milkfs/check");
                    0
                }
                e => e,
            }
        }
        (Some("clones"), _) => cat("/sys/fs/milkfs/clones"),
        (Some("clone"), Some(snap)) => match args.get(3) {
            Some(name) => control("clone", &alloc::format!("{snap} {name}")),
            None => {
                eprintln!("usage: snap clone SNAPSHOT NAME");
                2
            }
        },
        (Some(op @ ("promote" | "discard")), Some(name)) => control(op, name),
        (Some("auto"), None) => cat("/sys/fs/milkfs/auto"),
        (Some("auto"), Some(_)) => control("auto", &args.argv[2..].join(" ")),
        (Some(op @ ("create" | "delete" | "rollback")), Some(name)) => {
            control(if op == "create" { "snapshot" } else { op }, name)
        }
        _ => {
            eprintln!(
                "usage: snap [list | info | create NAME | delete NAME | rollback NAME | diff A [B|live] | check [--repair] | clones | clone SNAP NAME | promote NAME | discard NAME | auto [on|off|interval=15m|keep=K|daily=D|now] | send SNAP [--from OLDER] [-o FILE] | receive [FILE] [--to PATH] [--force]]"
            );
            2
        }
    }
}
