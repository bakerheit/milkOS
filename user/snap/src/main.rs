//! `snap`: milkfs snapshots from the shell, via the control files in /sys/fs/milkfs.
//!
//!   snap                  list snapshots of /Volumes/Milk
//!   snap create NAME      take one (browse it at /Volumes/Milk/.snapshots/NAME)
//!   snap delete NAME
//!   snap rollback NAME    make the volume what NAME captured (newer snapshots go away)
//!   snap info             volume status
//!   snap clones           list clones
//!   snap clone SNAP NAME  writable clone of SNAP, mounted at /Volumes/Milk-NAME
//!   snap promote NAME     the clone becomes the volume (old state kept as before-NAME)
//!   snap discard NAME     throw the clone away
//!   snap auto [SETTINGS]  automatic snapshots: show, or set e.g. `on interval=15m keep=8 daily=14`

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
        2 => "no such snapshot or clone",
        16 => "busy: a clone is built on that snapshot (discard or promote it first)",
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

fn main(args: &rt::Args) -> i32 {
    match (args.get(1), args.get(2)) {
        (None, _) | (Some("list"), _) => cat("/sys/fs/milkfs/snapshots"),
        (Some("info"), _) => cat("/sys/fs/milkfs/info"),
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
                "usage: snap [list | info | create NAME | delete NAME | rollback NAME | clones | clone SNAP NAME | promote NAME | discard NAME | auto [on|off|interval=15m|keep=K|daily=D|now]]"
            );
            2
        }
    }
}
