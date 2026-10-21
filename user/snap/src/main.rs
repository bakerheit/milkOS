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
//!   snap scrub [start|stop|status|rate=MB/s|auto=off|on|7d]
//!                         verify every block's checksum in the background (no argument:
//!                         start and wait for it, printing progress)
//!   snap unlock [DEV]     unlock an encrypted volume (asks for the passphrase, or the
//!                         recovery key) and mount it
//!   snap passwd           change the primary volume's passphrase (rewraps the key)
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
        1 => "needs an administrator's authorisation: run it as `auth snap ...` (or as root)",
        5 => "I/O error or damaged stream",
        13 => "wrong passphrase or recovery key",
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

/// `snap scrub`: start it and follow /sys/fs/milkfs/scrub until it is done.
fn scrub_wait() -> i32 {
    const CTL: &str = "/sys/fs/milkfs/scrub";
    let text = || {
        sys::read_file(CTL)
            .map(|d| alloc::string::String::from_utf8_lossy(&d).into_owned())
            .unwrap_or_default()
    };
    let run = |t: &str| -> u64 {
        t.split("[run ")
            .nth(1)
            .and_then(|x| x.split(']').next())
            .and_then(|x| x.parse().ok())
            .unwrap_or(0)
    };
    let before = run(&text());
    if control_quiet("scrub", "start") != 0 {
        return 1;
    }
    let mut shown = u64::MAX;
    for _ in 0..360_000 {
        sys::sleep_ms(100);
        let t = text();
        let first = t.lines().next().unwrap_or("");
        if run(first) <= before {
            continue; // not picked up yet
        }
        if first.contains("] running") {
            let pct = first
                .split("blocks (")
                .nth(1)
                .and_then(|x| x.split('%').next())
                .and_then(|x| x.parse::<u64>().ok())
                .unwrap_or(0);
            if shown == u64::MAX || pct / 25 != shown / 25 {
                eprintln!("{first}");
                shown = pct;
            }
            continue;
        }
        print!("{t}");
        return if t.contains("\nerror:") { 3 } else { 0 };
    }
    eprintln!("snap: scrub: timed out waiting");
    1
}

const TCGETS: usize = 0x5401;
const TCSETS: usize = 0x5402;
const ECHO: u32 = 0o10;

fn tty(cmd: usize, t: &mut [u8; 60]) -> bool {
    // SAFETY: `t` is valid for the 36-byte termios TCGETS/TCSETS read and write.
    unsafe { sys::syscall(sys::nr::IOCTL, [0, cmd, t.as_mut_ptr() as usize, 0, 0, 0]) >= 0 }
}

/// Ask for a secret on the terminal without echoing it (or read a line from a pipe).
fn ask(prompt: &str) -> Option<alloc::string::String> {
    let mut t = [0u8; 60];
    let is_tty = tty(TCGETS, &mut t);
    let saved = t;
    if is_tty {
        let l = u32::from_le_bytes(t[12..16].try_into().unwrap()) & !ECHO;
        t[12..16].copy_from_slice(&l.to_le_bytes());
        tty(TCSETS, &mut t);
        let _ = sys::write_all(2, prompt.as_bytes());
    }
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    while let Ok(1) = sys::read(0, &mut b) {
        if b[0] == b'\n' || b[0] == b'\r' {
            break;
        }
        line.push(b[0]);
    }
    if is_tty {
        let mut t = saved;
        tty(TCSETS, &mut t);
        let _ = sys::write_all(2, b"\n");
    }
    let s = alloc::string::String::from_utf8(line).ok()?;
    (!s.is_empty()).then_some(s)
}

/// Write a secret to a control file; errors are reported without it.
fn send_secret(op: &str, text: &str) -> i32 {
    let path = alloc::format!("/sys/fs/milkfs/{op}");
    let r = sys::open(&path, O_WRONLY).and_then(|fd| {
        let r = sys::write(fd, text.as_bytes());
        let _ = sys::close(fd);
        r
    });
    match r {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("snap: {op}: {}", why(e));
            1
        }
    }
}

fn unlock(dev: Option<&str>) -> i32 {
    let locked = sys::read_file("/sys/fs/milkfs/unlock").unwrap_or_default();
    let locked = core::str::from_utf8(&locked).unwrap_or("");
    let target = locked
        .lines()
        .find(|l| dev.is_none_or(|d| l.split(' ').next() == Some(d)));
    let Some(target) = target else {
        eprintln!(
            "snap: nothing to unlock{}",
            dev.map(|d| alloc::format!(" on {d}")).unwrap_or_default()
        );
        return 1;
    };
    let mut f = target.split(' ');
    let (d, label) = (f.next().unwrap_or(""), f.next().unwrap_or(""));
    for attempt in 0..3 {
        let Some(secret) = ask(&alloc::format!(
            "Passphrase (or recovery key) for {label} on {d}: "
        )) else {
            return 1;
        };
        if send_secret("unlock", &alloc::format!("dev={d} {secret}")) == 0 {
            println!("snap: {label} unlocked");
            return 0;
        }
        if attempt < 2 {
            eprintln!("snap: try again");
        }
    }
    1
}

fn passwd() -> i32 {
    let Some(old) = ask("Current passphrase (or recovery key): ") else {
        return 1;
    };
    let Some(new) = ask("New passphrase (8 characters or more): ") else {
        return 1;
    };
    let Some(again) = ask("New passphrase again: ") else {
        return 1;
    };
    if new != again {
        eprintln!("snap: the new passphrases differ");
        return 1;
    }
    match send_secret("passwd", &alloc::format!("{old}\n{new}")) {
        0 => {
            println!("snap: passphrase changed (the volume key was rewrapped)");
            0
        }
        e => e,
    }
}

fn main(args: &rt::Args) -> i32 {
    match (args.get(1), args.get(2)) {
        (Some("unlock"), d) => unlock(d),
        (Some("passwd"), _) => passwd(),
        (Some("scrub"), None) => scrub_wait(),
        (Some("scrub"), Some("status")) => cat("/sys/fs/milkfs/scrub"),
        (Some("scrub"), Some(_)) => match control_quiet("scrub", &args.argv[2..].join(" ")) {
            0 => cat("/sys/fs/milkfs/scrub"),
            e => e,
        },
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
