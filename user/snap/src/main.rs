//! `snap`: milkfs snapshots from the shell, via the control files in /sys/fs/milkfs.
//!
//!   snap                  list snapshots of /Volumes/Milk
//!   snap create NAME      take one (browse it at /Volumes/Milk/.snapshots/NAME)
//!   snap delete NAME
//!   snap rollback NAME    make the volume what NAME captured (newer snapshots go away)
//!   snap info             volume status

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
    } else {
        print!("{text}");
    }
    0
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
            eprintln!("snap: {op} {name}: {} ({})", e, e.name());
            1
        }
    }
}

fn main(args: &rt::Args) -> i32 {
    match (args.get(1), args.get(2)) {
        (None, _) | (Some("list"), _) => cat("/sys/fs/milkfs/snapshots"),
        (Some("info"), _) => cat("/sys/fs/milkfs/info"),
        (Some(op @ ("create" | "delete" | "rollback")), Some(name)) => {
            control(if op == "create" { "snapshot" } else { op }, name)
        }
        _ => {
            eprintln!("usage: snap [list | info | create NAME | delete NAME | rollback NAME]");
            2
        }
    }
}
