//! `fsstress`: parallel file-system stress (one process per worker, so on all CPUs).
//!
//!   fsstress DIR [WORKERS=4] [ROUNDS=20] [READFILE]
//!
//! Each worker, in DIR/wN: writes 8 files of varying size with a pattern derived from
//! (worker, round, file), reads each back and compares, renames it, reads it again,
//! deletes some; on milkfs also makes reflink copies (FICLONE), checks them, writes into
//! them and checks the original didn't change (skipped where the file system can't); and,
//! if READFILE is given (e.g. something on /Volumes/Mac), reads it every round and checks
//! it never changes. Prints a summary; exit status = number of errors.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use rt::{eprintln, println, sys};

rt::main!(main);

fn pattern(w: u32, r: u32, j: u32) -> Vec<u8> {
    let len = 1 + ((w * 7919 + r * 104_729 + j * 1_299_709) % 40_000) as usize;
    (0..len)
        .map(|i| (i as u32).wrapping_mul(31).wrapping_add(w * 17 + r * 5 + j) as u8)
        .collect()
}

fn write_file(path: &str, data: &[u8]) -> Result<(), rt::Errno> {
    let fd = sys::open_mode(path, sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC, 0o644)?;
    let r = sys::write_all(fd, data);
    let _ = sys::close(fd);
    r
}

fn read_file(path: &str) -> Result<Vec<u8>, rt::Errno> {
    let fd = sys::open(path, sys::O_RDONLY)?;
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match sys::read(fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => {
                let _ = sys::close(fd);
                return Err(e);
            }
        }
    }
    let _ = sys::close(fd);
    Ok(out)
}

/// Reflink `src` to `dst`. Ok(false): the file system can't (not milkfs).
fn reflink(src: &str, dst: &str) -> Result<bool, rt::Errno> {
    let s = sys::open(src, sys::O_RDONLY)?;
    let d = match sys::open_mode(dst, sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC, 0o644) {
        Ok(d) => d,
        Err(e) => {
            let _ = sys::close(s);
            return Err(e);
        }
    };
    let r = sys::ficlone(s, d);
    let _ = (sys::close(s), sys::close(d));
    match r {
        Ok(()) => Ok(true),
        Err(e) if e.0 == 95 || e.0 == 18 => Ok(false), // EOPNOTSUPP, EXDEV
        Err(e) => Err(e),
    }
}

/// Overwrite the first bytes of `path` in place.
fn poke(path: &str, bytes: &[u8]) -> Result<(), rt::Errno> {
    let fd = sys::open(path, sys::O_WRONLY)?;
    let r = sys::write_all(fd, bytes);
    let _ = sys::close(fd);
    r
}

fn worker(dir: &str, w: u32, rounds: u32, readfile: Option<&str>) -> i32 {
    let d = format!("{dir}/w{w}");
    let _ = sys::mkdir(&d, 0o755);
    let reference = readfile.and_then(|f| read_file(f).ok());
    let mut errors = 0;
    let mut reflinks = true;
    let mut bad = |what: &str| {
        errors += 1;
        if errors <= 5 {
            eprintln!("fsstress w{w}: {what}");
        }
    };
    for r in 0..rounds {
        for j in 0..8u32 {
            let (f, g) = (format!("{d}/f{j}"), format!("{d}/g{j}"));
            let data = pattern(w, r, j);
            if write_file(&f, &data).is_err() {
                bad("write");
                continue;
            }
            if read_file(&f).ok().as_deref() != Some(&data[..]) {
                bad("read after write differs");
            }
            if sys::rename(&f, &g).is_err() {
                bad("rename");
            }
            if read_file(&g).ok().as_deref() != Some(&data[..]) {
                bad("read after rename differs");
            }
            if reflinks && (r + j) % 2 == 1 {
                let c = format!("{d}/c{j}");
                match reflink(&g, &c) {
                    Ok(false) => reflinks = false,
                    Err(_) => bad("reflink"),
                    Ok(true) => {
                        if read_file(&c).ok().as_deref() != Some(&data[..]) {
                            bad("reflink copy differs");
                        }
                        let mark = [b'c', w as u8, r as u8, j as u8];
                        let n = mark.len().min(data.len());
                        if poke(&c, &mark[..n]).is_err() {
                            bad("write into reflink copy");
                        }
                        let mut want = data.clone();
                        want[..n].copy_from_slice(&mark[..n]);
                        if read_file(&c).ok().as_deref() != Some(&want[..]) {
                            bad("written reflink copy differs");
                        }
                        if read_file(&g).ok().as_deref() != Some(&data[..]) {
                            bad("original changed by writing its reflink copy");
                        }
                        if r % 2 == 0 && sys::unlink(&c).is_err() {
                            bad("unlink reflink copy");
                        }
                    }
                }
            }
            if (r + j) % 3 == 0 && sys::unlink(&g).is_err() {
                bad("unlink");
            }
        }
        if let (Some(f), Some(want)) = (readfile, reference.as_ref())
            && read_file(f).ok().as_ref() != Some(want)
        {
            bad("shared read changed");
        }
    }
    errors
}

fn main(args: &rt::Args) -> i32 {
    let Some(dir) = args.get(1) else {
        eprintln!("usage: fsstress DIR [WORKERS] [ROUNDS] [READFILE]");
        return 2;
    };
    let workers: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(4);
    let rounds: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(20);
    let readfile = args.get(4);
    let _ = sys::mkdir(dir, 0o755);
    let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
    let mut pids = Vec::new();
    for w in 0..workers {
        match sys::fork() {
            Ok(0) => sys::exit(worker(dir, w, rounds, readfile)),
            Ok(pid) => pids.push(pid),
            Err(e) => eprintln!("fsstress: fork: {e}"),
        }
    }
    let mut errors = 0;
    for pid in pids {
        if let Ok((_, st)) = sys::wait4(pid as i32, 0) {
            errors += (st >> 8) & 0xff; // exit code
        }
    }
    let ms = (sys::clock_ns(sys::CLOCK_MONOTONIC) - t0) / 1_000_000;
    let ops = workers * rounds * 8 * 5;
    println!(
        "fsstress: {workers} workers x {rounds} rounds on {dir}: {ops} file ops in {ms} ms, {errors} error(s)"
    );
    errors
}
