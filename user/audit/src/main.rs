//! audit: read and verify the kernel's tamper-evident audit log.
//!   audit [--file | FILE] [--app EXE] [--since DUR] [--event PREFIX]   show records
//!   audit verify [--file | FILE]                                         check the hash chain
//!   audit status                                                          /proc/audit_status
//!   audit selftest                                                        SHA-256 test vectors
//! Default source: /proc/audit (this boot's RAM ring). --file: the persisted log,
//! /Volumes/Milk/var/log/audit.log. DUR: 30s, 10m, 2h, 1d. EXE: a path or a basename.
//! `verify` with no source checks both; it prints OK or the first broken seq (exit 1).
//! The SHA-256 here is userland's own copy (rt::sha256), independent of the kernel's.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use rt::sha256::{Digest, hex, sha256};
use rt::{eprintln, println, sys};

rt::main!(main);

const PROC: &str = "/proc/audit";
const FILE: &str = "/Volumes/Milk/var/log/audit.log";

struct Rec<'a> {
    seq: u64,
    time_ns: u64,
    pid: u32,
    exe: String,
    event: String,
    detail: String,
    body: &'a str,
    prev: Digest,
    hash: Digest,
}

fn unescape(s: &str) -> String {
    if s == "-" {
        return String::new();
    }
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut k = 0;
    while k < b.len() {
        if b[k] == b'%'
            && k + 3 <= b.len()
            && let Some(v) = core::str::from_utf8(&b[k + 1..k + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            k += 3;
            continue;
        }
        out.push(b[k]);
        k += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn unhex(s: &str) -> Option<Digest> {
    if s.len() != 64 {
        return None;
    }
    let mut d = [0u8; 32];
    for (i, b) in d.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(d)
}

fn parse(line: &str) -> Option<Rec<'_>> {
    let (body, h) = line.rsplit_once(' ')?;
    let f: Vec<&str> = body.split(' ').collect();
    if f.len() != 7 {
        return None;
    }
    Some(Rec {
        seq: f[0].parse().ok()?,
        time_ns: f[1].parse().ok()?,
        pid: f[2].parse().ok()?,
        exe: unescape(f[3]),
        event: unescape(f[4]),
        detail: unescape(f[5]),
        body,
        prev: unhex(f[6])?,
        hash: unhex(h)?,
    })
}

/// (records, first seq, last seq, head hash) or Err((first bad seq or line, why)).
fn verify(
    text: &str,
    check_first_prev: bool,
) -> Result<(u64, u64, u64, Digest), (u64, &'static str)> {
    let mut prev: Option<Digest> = if check_first_prev {
        Some([0; 32])
    } else {
        None
    };
    let mut want: Option<u64> = None;
    let (mut n, mut first, mut last, mut head) = (0u64, 0u64, 0u64, [0u8; 32]);
    for (i, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let Some(r) = parse(line) else {
            return Err((i as u64 + 1, "unparsable line (line number)"));
        };
        if sha256(r.body.as_bytes()) != r.hash {
            return Err((r.seq, "hash does not match the record"));
        }
        if prev.is_some_and(|p| p != r.prev) {
            return Err((r.seq, "prev_hash does not match the previous record"));
        }
        if want.is_some_and(|w| w != r.seq) {
            return Err((r.seq, "sequence gap"));
        }
        if n == 0 {
            first = r.seq;
        }
        n += 1;
        last = r.seq;
        head = r.hash;
        prev = Some(r.hash);
        want = Some(r.seq + 1);
    }
    Ok((n, first, last, head))
}

fn read(path: &str) -> Option<String> {
    sys::read_file(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Days since 1970-01-01 -> (y, m, d) (Howard Hinnant's civil_from_days).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

fn when(ns: u64) -> String {
    let s = (ns / 1_000_000_000) as i64;
    let (y, m, d) = civil(s.div_euclid(86_400));
    let t = s.rem_euclid(86_400);
    alloc::format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        t / 3600,
        t / 60 % 60,
        t % 60
    )
}

fn duration_ns(s: &str) -> Option<u64> {
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = num.parse().ok()?;
    let mul = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return None,
    };
    Some(n * mul * 1_000_000_000)
}

fn usage() -> i32 {
    eprintln!(
        "usage: audit [--file|FILE] [--app EXE] [--since DUR] [--event PREFIX] | audit verify [--file|FILE] | audit status | audit selftest"
    );
    2
}

fn do_verify(src: Option<&str>) -> i32 {
    let sources: Vec<(&str, bool)> = match src {
        Some(p) => alloc::vec![(p, p != PROC)],
        None => alloc::vec![(PROC, false), (FILE, true)],
    };
    let mut status = 0;
    for (path, from_genesis) in sources {
        let Some(text) = read(path) else {
            if src.is_some() {
                println!("audit verify: {path}: cannot read");
                status = 1;
            } else {
                println!("audit verify: {path}: not present (no milkfs volume?)");
            }
            continue;
        };
        match verify(&text, from_genesis) {
            Ok((n, a, b, head)) => println!(
                "audit verify: {path}: OK ({n} records, seq {a}..{b}, head {})",
                &hex(&head)[..16]
            ),
            Err((seq, why)) => {
                println!("audit verify: {path}: BROKEN at seq {seq}: {why}");
                status = 1;
            }
        }
    }
    status
}

fn main(args: &rt::Args) -> i32 {
    let a: Vec<&str> = args.argv.iter().skip(1).copied().collect();
    match a.first().copied() {
        Some("verify") => {
            return match &a[1..] {
                [] => do_verify(None),
                ["--file"] => do_verify(Some(FILE)),
                [p] => do_verify(Some(p)),
                _ => usage(),
            };
        }
        Some("status") => {
            print_file("/proc/audit_status");
            return 0;
        }
        Some("selftest") => {
            let (ok, n) = rt::sha256::selftest();
            println!("audit: sha256 (userland copy): {ok}/{n} FIPS 180-4 vectors ok");
            return if ok == n { 0 } else { 1 };
        }
        _ => {}
    }
    let (mut src, mut app, mut since, mut event) = (PROC, None, None, None);
    let mut it = a.iter();
    while let Some(x) = it.next() {
        match *x {
            "--file" => src = FILE,
            "--app" => app = it.next().copied(),
            "--since" => match it.next().and_then(|d| duration_ns(d)) {
                Some(d) => since = Some(sys::clock_ns(sys::CLOCK_REALTIME).saturating_sub(d)),
                None => return usage(),
            },
            "--event" => event = it.next().copied(),
            p if !p.starts_with('-') => src = p,
            _ => return usage(),
        }
    }
    let Some(text) = read(src) else {
        eprintln!("audit: {src}: cannot read");
        return 1;
    };
    let mut shown = 0;
    for line in text.lines() {
        let Some(r) = parse(line) else { continue };
        if let Some(a) = app {
            let base = r.exe.rsplit('/').next().unwrap_or("");
            if r.exe != a && base != a {
                continue;
            }
        }
        if since.is_some_and(|t| r.time_ns < t) {
            continue;
        }
        if event.is_some_and(|e| !r.event.starts_with(e)) {
            continue;
        }
        println!(
            "{:>5} {} {:>4} {:<14} {:<14} {}",
            r.seq,
            when(r.time_ns),
            r.pid,
            r.exe,
            r.event,
            r.detail
        );
        shown += 1;
    }
    if shown == 0 {
        eprintln!("audit: no matching records in {src}");
    }
    0
}

fn print_file(path: &str) {
    match read(path) {
        Some(t) => {
            let _ = sys::write_all(1, t.as_bytes());
        }
        None => eprintln!("audit: {path}: cannot read"),
    }
}
