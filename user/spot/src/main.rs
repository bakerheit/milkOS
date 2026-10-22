//! `spot`: search files by name and content (the search index, like macOS `mdfind`).
//!
//!   spot WORDS...           paths of files whose name or text has every word (or a word
//!                           starting with it), best first
//!   spot -l WORDS...        with kind, size, date and where it matched
//!   spot -n N ...           at most N results (default 50)
//!   spot --status           the index: state, size, roots
//!   spot --wait             wait until the index is ready (built, or loaded and verified)
//!   spot --wait-index       wait until there is an index to query (a saved one answers
//!                           while it is being verified)
//!   spot --rebuild          index again from scratch
//!
//! Filters: kind:md (or text, image, audio, folder), in:Documents (or in:/path),
//! modified:today|yesterday|week|month|3d|12h. Under msh's structured pipes
//! (MILK_TABLE=1) it emits a table: path, name, kind, size, modified, score, matched.
//! Talks to /System/Kernel/fs/search.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use rt::{eprintln, print, println, sys};

rt::main!(main);

const DIR: &str = "/System/Kernel/fs/search";

fn read(path: &str) -> Result<String, rt::Errno> {
    sys::read_file(path).map(|d| String::from_utf8_lossy(&d).into_owned())
}

fn write(path: &str, s: &str) -> Result<(), rt::Errno> {
    let fd = sys::open(path, 1)?;
    let r = sys::write(fd, s.as_bytes());
    let _ = sys::close(fd);
    r.map(|_| ())
}

/// Unix seconds -> "YYYY-MM-DD HH:MM".
fn date(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86400), t.rem_euclid(86400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs / 3600,
        secs / 60 % 60
    )
}

fn human(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.1} K", n as f64 / 1024.0),
        _ => format!("{:.1} M", n as f64 / 1_048_576.0),
    }
}

struct Hit {
    score: i64,
    kind: String,
    size: i64,
    mtime: i64,
    matched: String,
    path: String,
}

fn main(args: &rt::Args) -> i32 {
    let mut long = false;
    let mut limit: Option<String> = None;
    let mut words: Vec<&str> = Vec::new();
    let mut it = args.argv[1..].iter();
    while let Some(&a) = it.next() {
        match a {
            "-l" => long = true,
            "-n" => limit = it.next().map(|s| String::from(*s)),
            "--status" => {
                return match read(&format!("{DIR}/status")) {
                    Ok(s) => {
                        print!("{s}");
                        0
                    }
                    Err(e) => {
                        eprintln!("spot: {DIR}/status: {e}");
                        1
                    }
                };
            }
            "--wait" | "--wait-index" => {
                // --wait: until the index is ready (built, or loaded and verified);
                // --wait-index: until there is one to query (a loaded one answers while it
                // is verified). At most 10 minutes.
                let any = a == "--wait-index";
                for _ in 0..6000 {
                    let s = read(&format!("{DIR}/status")).unwrap_or_default();
                    let ready = s
                        .lines()
                        .any(|l| l.split_whitespace().collect::<Vec<_>>() == ["state:", "ready"]);
                    let some = s.lines().any(|l| {
                        l.strip_prefix("index:")
                            .and_then(|r| r.split_whitespace().next())
                            .is_some_and(|n| n != "0")
                    });
                    if ready || (any && some) {
                        println!(
                            "spot: the index is {}",
                            if ready { "ready" } else { "loaded" }
                        );
                        return 0;
                    }
                    sys::sleep_ms(100);
                }
                eprintln!("spot: the index still isn't ready after 10 minutes");
                return 1;
            }
            "--rebuild" => {
                return match write(&format!("{DIR}/rebuild"), "1") {
                    Ok(()) => {
                        println!("spot: rebuilding the index in the background");
                        0
                    }
                    Err(e) => {
                        eprintln!("spot: {e}");
                        1
                    }
                };
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: spot [-l] [-n N] WORDS... (kind:md in:Documents modified:today) | --status | --rebuild"
                );
                return 0;
            }
            w => words.push(w),
        }
    }
    if words.is_empty() {
        eprintln!("usage: spot [-l] [-n N] WORDS... | --status | --rebuild");
        return 2;
    }
    let q = words.join(" ");
    let req = match &limit {
        Some(n) => format!("limit={n} {q}"),
        None => q.clone(),
    };
    if let Err(e) = write(&format!("{DIR}/query"), &req) {
        eprintln!(
            "spot: {}",
            match e.0 {
                11 => String::from("the index isn't ready yet (spot --status)"),
                22 => format!("bad query {q:?}"),
                _ => format!("{e}"),
            }
        );
        return 1;
    }
    let text = match read(&format!("{DIR}/query")) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("spot: {e}");
            return 1;
        }
    };
    let hits: Vec<Hit> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(6, '\t').collect();
            (f.len() == 6).then(|| Hit {
                score: f[0].parse().unwrap_or(0),
                kind: String::from(f[1]),
                size: f[2].parse().unwrap_or(0),
                mtime: f[3].parse().unwrap_or(0),
                matched: String::from(f[4]),
                path: String::from(f[5]),
            })
        })
        .collect();
    if rt::table::requested(args) {
        use rt::table::{Table, Ty, Val};
        let mut t = Table::new(&[
            ("path", Ty::Str),
            ("name", Ty::Str),
            ("kind", Ty::Str),
            ("size", Ty::Size),
            ("modified", Ty::Str),
            ("score", Ty::Int),
            ("matched", Ty::Str),
        ]);
        for h in &hits {
            t.push(alloc::vec![
                Val::str(&h.path),
                Val::str(h.path.rsplit('/').next().unwrap_or("")),
                Val::str(&h.kind),
                Val::Int(h.size),
                Val::str(&date(h.mtime)),
                Val::Int(h.score),
                Val::str(&h.matched),
            ]);
        }
        t.write(&mut rt::io::Out::stdout());
        return 0;
    }
    for h in &hits {
        if long {
            println!(
                "{:<7} {:>8}  {}  {:<12} {}",
                h.kind,
                human(h.size as u64),
                date(h.mtime),
                h.matched,
                h.path
            );
        } else {
            println!("{}", h.path);
        }
    }
    if hits.is_empty() {
        eprintln!("spot: nothing found for {q:?}");
        return 1;
    }
    0
}
