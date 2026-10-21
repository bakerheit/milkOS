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
//!   snap send SNAP [--from OLDER] [-o FILE] [--encrypt | --plaintext] [--passphrase-file F]
//!                         SNAP as a stream (incremental from OLDER) to FILE or stdout;
//!                         encrypted (asks for a passphrase) with --encrypt, and by
//!                         default when the volume is encrypted (--plaintext: not)
//!   snap scrub [start|stop|status|rate=MB/s|auto=off|on|7d]
//!                         verify every block's checksum in the background (no argument:
//!                         start and wait for it, printing progress)
//!   snap unlock [DEV]     unlock an encrypted volume (asks for the passphrase, or the
//!                         recovery key) and mount it
//!   snap lock [DEV|PATH]  lock unlocked encrypted volumes again (sync, unmount, wipe the
//!                         key); not the one /home/milk is on. No argument: every one
//!   snap passwd           change the primary volume's passphrase (rewraps the key)
//!   snap encrypt [DEV] [--recovery-out FILE | --no-recovery] [--rate MB] [--wait]
//!                         encrypt a plain volume in place, in the background (asks for
//!                         the new passphrase, shows the recovery key once)
//!   snap decrypt [DEV] [--wait]  decrypt an encrypted volume in place (asks for its
//!                         passphrase or the recovery key)
//!   snap disks            every disk: size, milkfs or not, encrypted, mounted or locked
//!   snap mkfs DEV [LABEL] [--force] [--encrypt [--recovery-out FILE | --no-recovery]]
//!                         format DEV as milkfs; --encrypt asks for a passphrase and
//!                         shows the recovery key once (or writes it to FILE)
//!   snap receive [FILE] [--to PATH] [--force] [--passphrase-file F]
//!                         recreate the snapshot in a stream (FILE or stdin) on the milkfs
//!                         volume PATH is on (default /Volumes/Milk); --force rolls the
//!                         target back to the stream's base first; an encrypted stream
//!                         asks for its passphrase

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

/// Why control `op` just failed: the kernel's reason from /sys/fs/milkfs/error when it
/// is about `op` (`op: ERRNO: reason`), else the errno in words.
fn why_op(op: &str, e: rt::Errno) -> alloc::string::String {
    let text = sys::read_file("/sys/fs/milkfs/error").unwrap_or_default();
    let text = core::str::from_utf8(&text).unwrap_or("").trim_end();
    if let Some(rest) = text.strip_prefix(op).and_then(|r| r.strip_prefix(": "))
        && let Some((name, reason)) = rest.split_once(": ")
    {
        // The kernel's errno name (rt's table is short).
        return alloc::format!("{reason} ({name}, errno {})", e.0);
    }
    why(e)
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
            eprintln!("snap: {op} {arg}: {}", why_op(op, e));
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
            eprintln!("snap: {op} {name}: {}", why_op(op, e));
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

/// Is the primary volume encrypted (its `info` says `encryption: on`)?
fn volume_encrypted() -> bool {
    let i = sys::read_file("/sys/fs/milkfs/info").unwrap_or_default();
    core::str::from_utf8(&i)
        .unwrap_or("")
        .lines()
        .any(|l| l.starts_with("encryption:") && l.ends_with(" on"))
}

/// A stream passphrase: from `file` (its first line), else asked for (twice when `new`).
fn stream_passphrase(file: Option<&str>, new: bool) -> Option<alloc::string::String> {
    if let Some(f) = file {
        let d = sys::read_file(f).ok()?;
        let s = core::str::from_utf8(&d).ok()?.lines().next()?.trim_end();
        return (!s.is_empty()).then(|| alloc::string::String::from(s));
    }
    let p = ask(if new {
        "Passphrase for the stream (8 characters or more): "
    } else {
        "Passphrase of the stream: "
    })?;
    if new && ask("Again: ")? != p {
        eprintln!("snap: the passphrases differ");
        return None;
    }
    Some(p)
}

/// Write `text` to control `op`; the error (with the kernel's reason) is printed, and
/// returned for retries.
fn write_ctl(op: &str, text: &str) -> Result<(), rt::Errno> {
    let path = alloc::format!("/sys/fs/milkfs/{op}");
    let r = sys::open(&path, O_WRONLY).and_then(|fd| {
        let r = sys::write(fd, text.as_bytes());
        let _ = sys::close(fd);
        r
    });
    if let Err(e) = r {
        eprintln!("snap: {op}: {}", why_op(op, e));
        return Err(e);
    }
    Ok(())
}

fn send(args: &[&str]) -> i32 {
    let (mut snap, mut from, mut out) = (None, None, None);
    let (mut encrypt, mut plaintext, mut pfile) = (false, false, None);
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        match a {
            "--from" => from = it.next().copied(),
            "-o" => out = it.next().copied(),
            "--encrypt" => encrypt = true,
            "--plaintext" => plaintext = true,
            "--passphrase-file" => pfile = it.next().copied(),
            s => snap = Some(s),
        }
    }
    let Some(snap) = snap.filter(|_| !(plaintext && (encrypt || pfile.is_some()))) else {
        eprintln!(
            "usage: snap send SNAP [--from OLDER] [-o FILE] [--encrypt | --plaintext] [--passphrase-file FILE]"
        );
        return 2;
    };
    let tmp = alloc::format!("/tmp/.snap-send-{}", sys::getpid());
    let path = out.map(abs).unwrap_or_else(|| tmp.clone());
    let mut req = match from {
        Some(f) => alloc::format!("{snap} {f} {path}"),
        None => alloc::format!("{snap} {path}"),
    };
    // Encrypted when asked, or by default from an encrypted volume.
    let sealed = !plaintext && (encrypt || pfile.is_some() || volume_encrypted());
    if plaintext {
        req.push_str(" plaintext");
    }
    let ok = if sealed {
        let Some(p) = stream_passphrase(pfile, true) else {
            return 1;
        };
        req.push('\n');
        req.push_str(&p);
        let r = write_ctl("send", &req).is_ok();
        // The secret was in `req`: overwrite it.
        // SAFETY: zero bytes are valid UTF-8.
        unsafe { req.as_bytes_mut().fill(0) };
        r
    } else {
        write_ctl("send", &req).is_ok()
    };
    if !ok {
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

/// Does the stream file start with the encrypted-stream envelope?
fn stream_sealed(path: &str) -> bool {
    let Ok(fd) = sys::open(path, sys::O_RDONLY) else {
        return false;
    };
    let mut m = [0u8; 8];
    let n = sys::read(fd, &mut m).unwrap_or(0);
    let _ = sys::close(fd);
    n == 8 && &m == b"MILKSTRX"
}

fn receive(args: &[&str]) -> i32 {
    let (mut file, mut to, mut force, mut pfile) = (None, None, false, None);
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        match a {
            "--to" => to = it.next().copied(),
            "--force" | "-F" => force = true,
            "--passphrase-file" => pfile = it.next().copied(),
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
    let mut req = path.clone();
    if let Some(t) = to {
        req = alloc::format!("{req} {}", abs(t));
    }
    if force {
        req.push_str(" force");
    }
    let r = if !stream_sealed(&path) {
        write_ctl("receive", &req).is_ok()
    } else if file.is_none() && pfile.is_none() {
        eprintln!(
            "snap: the stream on stdin is encrypted: give its passphrase with --passphrase-file FILE"
        );
        false
    } else {
        let mut ok = false;
        for attempt in 0..if pfile.is_some() { 1 } else { 3 } {
            let Some(p) = stream_passphrase(pfile, false) else {
                break;
            };
            let mut t = alloc::format!("{req}\n{p}");
            let r = write_ctl("receive", &t);
            // SAFETY: zero bytes are valid UTF-8.
            unsafe { t.as_bytes_mut().fill(0) };
            match r {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(e) if e.0 == 13 && attempt < 2 && pfile.is_none() => {
                    eprintln!("snap: try again")
                }
                Err(_) => break,
            }
        }
        ok
    };
    let _ = sys::unlink(&tmp);
    if r {
        report("/sys/fs/milkfs/receive");
    }
    if r { 0 } else { 1 }
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
            eprintln!("snap: {op}: {}", why_op(op, e));
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

fn lock(target: Option<&str>) -> i32 {
    let r = sys::open("/sys/fs/milkfs/lock", O_WRONLY).and_then(|fd| {
        let r = sys::write(fd, target.unwrap_or("all").as_bytes());
        let _ = sys::close(fd);
        r
    });
    match r {
        Ok(_) => {
            let st = sys::read_file("/sys/fs/milkfs/lock").unwrap_or_default();
            let st = core::str::from_utf8(&st).unwrap_or("");
            // Every locked volume, the one just locked among them.
            for l in st.lines().filter(|l| l.ends_with(" locked")) {
                println!("snap: {l}");
            }
            0
        }
        Err(e) => {
            eprintln!(
                "snap: lock{}: {}",
                target.map(|t| alloc::format!(" {t}")).unwrap_or_default(),
                why_op("lock", e)
            );
            1
        }
    }
}

/// `snap mkfs DEV [LABEL] [--force] [--encrypt [--recovery-out FILE | --no-recovery]
/// [--passphrase-file F]]`.
fn mkfs(args: &[&str], home: Option<&str>) -> i32 {
    let (mut dev, mut label, mut force) = (None, None, false);
    let (mut encrypt, mut rec_out, mut no_rec, mut pfile) = (false, None, false, None);
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        match a {
            "--force" => force = true,
            "--encrypt" => encrypt = true,
            "--recovery-out" => rec_out = it.next().copied(),
            "--no-recovery" => no_rec = true,
            "--passphrase-file" => pfile = it.next().copied(),
            _ if dev.is_none() => dev = Some(a),
            _ => label = Some(a),
        }
    }
    let Some(dev) = dev.filter(|_| encrypt || (rec_out.is_none() && !no_rec && pfile.is_none()))
    else {
        eprintln!(
            "usage: snap mkfs DEVICE [LABEL] [--force] [--encrypt [--recovery-out FILE | --no-recovery] [--passphrase-file FILE]]"
        );
        return 2;
    };
    let mut req = alloc::format!("{dev} {}", label.unwrap_or("Milk"));
    if force {
        req.push_str(" force");
    }
    if !encrypt {
        return match write_ctl("mkfs", &req) {
            Ok(()) => {
                println!("snap: {dev} formatted (milkfs, mounted at the next boot)");
                0
            }
            Err(_) => 1,
        };
    }
    // The recovery key goes to a file the kernel creates (0600, owned like its folder);
    // without --recovery-out, a temporary one in the home folder, shown and removed.
    let tmp = alloc::format!(
        "{}/.milkfs-recovery-{}",
        home.unwrap_or("/tmp"),
        sys::getpid()
    );
    let rec = if no_rec {
        None
    } else {
        Some(rec_out.map(abs).unwrap_or_else(|| tmp.clone()))
    };
    req.push_str(" encrypt");
    if let Some(r) = &rec {
        req.push_str(&alloc::format!(" recovery={r}"));
    }
    let Some(p) = (match pfile {
        Some(_) => stream_passphrase(pfile, false),
        None => {
            let a = ask("Passphrase for the new volume (8 characters or more): ");
            let b = a.as_ref().and_then(|_| ask("Again: "));
            match (a, b) {
                (Some(a), Some(b)) if a == b => Some(a),
                (Some(_), Some(_)) => {
                    eprintln!("snap: the passphrases differ");
                    None
                }
                _ => None,
            }
        }
    }) else {
        return 1;
    };
    let mut t = alloc::format!("{req}\n{p}");
    let r = write_ctl("mkfs", &t);
    // SAFETY: zero bytes are valid UTF-8.
    unsafe { t.as_bytes_mut().fill(0) };
    if r.is_err() {
        return 1;
    }
    println!(
        "snap: {dev} formatted as an encrypted milkfs volume (locked: `snap unlock {dev}` mounts it)"
    );
    match (&rec, rec_out) {
        (Some(path), None) => {
            let k = sys::read_file(path).unwrap_or_default();
            let _ = sys::unlink(path);
            println!(
                "snap: recovery key: {}",
                core::str::from_utf8(&k).unwrap_or("?").trim_end()
            );
            println!("snap: write it down and keep it apart from the disk: it is shown only now");
        }
        (Some(path), Some(_)) => println!("snap: recovery key written to {path}"),
        _ => println!("snap: no recovery key (only the passphrase opens it)"),
    }
    0
}

/// Follow `/sys/fs/milkfs/encrypt` until no volume is being converted.
fn convert_wait() -> i32 {
    let mut shown = u64::MAX;
    loop {
        let t = sys::read_file("/sys/fs/milkfs/encrypt").unwrap_or_default();
        let t = alloc::string::String::from_utf8_lossy(&t).into_owned();
        let Some(l) = t.lines().find(|l| l.contains(": converting: ")) else {
            print!(
                "{}",
                t.lines()
                    .filter(|l| !l.starts_with("rate"))
                    .map(|l| alloc::format!("snap: {l}\n"))
                    .collect::<alloc::string::String>()
            );
            return if t.contains("stopped") { 1 } else { 0 };
        };
        let pct = l
            .split(": converting: ")
            .nth(1)
            .and_then(|x| x.split('%').next())
            .and_then(|x| x.split(' ').nth(1))
            .and_then(|x| x.parse::<u64>().ok())
            .unwrap_or(0);
        if shown == u64::MAX || pct / 25 != shown / 25 {
            eprintln!("snap: {l}");
            shown = pct;
        }
        sys::sleep_ms(500);
    }
}

/// `snap encrypt [DEV] [--recovery-out FILE | --no-recovery] [--passphrase-file F]
/// [--rate MB] [--wait]` and `snap decrypt [DEV] [--passphrase-file F] [--wait]`.
fn convert(args: &[&str], encrypt: bool, home: Option<&str>) -> i32 {
    let (mut dev, mut rec_out, mut no_rec, mut pfile, mut rate, mut wait) =
        (None, None, false, None, None, false);
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        match a {
            "--recovery-out" if encrypt => rec_out = it.next().copied(),
            "--no-recovery" if encrypt => no_rec = true,
            "--passphrase-file" => pfile = it.next().copied(),
            "--rate" => rate = it.next().copied(),
            "--wait" => wait = true,
            _ if !a.starts_with('-') && dev.is_none() => dev = Some(a),
            _ => {
                eprintln!(
                    "usage: snap encrypt [DEV] [--recovery-out FILE | --no-recovery] [--passphrase-file F] [--rate MB] [--wait]\n       snap decrypt [DEV] [--passphrase-file F] [--wait]"
                );
                return 2;
            }
        }
    }
    let mut req = alloc::string::String::new();
    if let Some(d) = dev {
        req.push_str(&alloc::format!("dev={d} "));
    }
    let tmp = alloc::format!(
        "{}/.milkfs-recovery-{}",
        home.unwrap_or("/tmp"),
        sys::getpid()
    );
    let rec = (encrypt && !no_rec).then(|| rec_out.map(abs).unwrap_or_else(|| tmp.clone()));
    if let Some(r) = &rec {
        req.push_str(&alloc::format!("recovery={r} "));
    }
    if let Some(r) = rate
        && write_ctl("encrypt", &alloc::format!("rate={r}")).is_err()
    {
        return 1;
    }
    let secret = match pfile {
        Some(_) => stream_passphrase(pfile, false),
        None if encrypt => {
            let a = ask("New passphrase for the volume (8 characters or more): ");
            let b = a.as_ref().and_then(|_| ask("Again: "));
            match (a, b) {
                (Some(a), Some(b)) if a == b => Some(a),
                (Some(_), Some(_)) => {
                    eprintln!("snap: the passphrases differ");
                    None
                }
                _ => None,
            }
        }
        None => ask("Passphrase (or recovery key): "),
    };
    let Some(p) = secret else {
        return 1;
    };
    let op = if encrypt { "encrypt" } else { "decrypt" };
    let mut t = alloc::format!("{}\n{p}", req.trim_end());
    let r = write_ctl(op, &t);
    // SAFETY: zero bytes are valid UTF-8.
    unsafe { t.as_bytes_mut().fill(0) };
    if r.is_err() {
        return 1;
    }
    println!(
        "snap: {} in place started (the volume stays usable; progress: snap info)",
        if encrypt { "encrypting" } else { "decrypting" }
    );
    match (&rec, rec_out) {
        (Some(path), None) => {
            let k = sys::read_file(path).unwrap_or_default();
            let _ = sys::unlink(path);
            println!(
                "snap: recovery key: {}",
                core::str::from_utf8(&k).unwrap_or("?").trim_end()
            );
            println!("snap: write it down and keep it apart from the disk: it is shown only now");
        }
        (Some(path), Some(_)) => println!("snap: recovery key written to {path}"),
        _ => {}
    }
    if wait { convert_wait() } else { 0 }
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
        (Some("lock"), t) => lock(t),
        (Some("mkfs"), _) => mkfs(&args.argv[2..], args.env("HOME")),
        (Some("encrypt"), _) => convert(&args.argv[2..], true, args.env("HOME")),
        (Some("decrypt"), _) => convert(&args.argv[2..], false, args.env("HOME")),
        (Some("disks"), _) => cat("/sys/fs/milkfs/disks"),
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
                "usage: snap [list | info | create NAME | delete NAME | rollback NAME | diff A [B|live] | check [--repair] | clones | clone SNAP NAME | promote NAME | discard NAME | unlock [DEV] | lock [DEV|PATH] | passwd | scrub [...] | auto [on|off|interval=15m|keep=K|daily=D|now] | send SNAP [--from OLDER] [-o FILE] [--encrypt|--plaintext] | receive [FILE] [--to PATH] [--force] [--passphrase-file F]]"
            );
            2
        }
    }
}
