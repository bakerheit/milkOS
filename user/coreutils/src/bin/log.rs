//! log [-u UNIT]... [--since 10m|2h|1d|UNIXTIME] [-n N] [-p LEVEL] [-b] [-f]: the system
//! journal (kernel/src/journal.rs): previous boots from /Volumes/Milk/var/log/journal, then
//! the live ring from /dev/journal. -f follows new entries. MILK_TABLE=1 for a table.
//! Reading the journal needs root or group admin.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use coreutils::fmt_time;
use rt::io::Out;
use rt::sys;
rt::main!(main);

const DIR: &str = "/Volumes/Milk/var/log/journal";

struct Entry {
    seq: u64,
    boot: String,
    wall: i64,
    pid: u32,
    uid: u32,
    unit: String,
    level: String,
    msg: String,
}

fn parse(line: &str) -> Option<Entry> {
    let f: Vec<&str> = line.splitn(9, '\t').collect();
    if f.len() != 9 {
        return None;
    }
    Some(Entry {
        seq: f[0].parse().ok()?,
        boot: String::from(f[1]),
        wall: f[3].parse().ok()?,
        pid: f[4].parse().ok()?,
        uid: f[5].parse().ok()?,
        unit: String::from(f[6]),
        level: String::from(f[7]),
        msg: String::from(f[8]),
    })
}

fn rank(level: &str) -> u32 {
    match level {
        "err" => 3,
        "warn" => 4,
        "info" => 6,
        _ => 7,
    }
}

struct Filter {
    units: Vec<String>,
    since: Option<i64>,
    max_level: u32,
    boot: Option<String>,
}

impl Filter {
    fn keep(&self, e: &Entry) -> bool {
        (self.units.is_empty()
            || self
                .units
                .iter()
                .any(|u| e.unit == *u || e.unit.starts_with(&alloc::format!("{u}/"))))
            && self.since.is_none_or(|t| e.wall >= t)
            && rank(&e.level) <= self.max_level
            && self.boot.as_ref().is_none_or(|b| e.boot == *b)
    }
}

/// "10m" / "2h" / "1d" / "30s" ago, or a unix time.
fn since(s: &str, now: i64) -> Option<i64> {
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: i64 = num.parse().ok()?;
    Some(match unit {
        "" if n > 1_000_000 => n,
        "s" | "" => now - n,
        "m" | "min" => now - n * 60,
        "h" => now - n * 3600,
        "d" => now - n * 86_400,
        _ => return None,
    })
}

fn show(out: &mut Out, e: &Entry) {
    let flag = match e.level.as_str() {
        "err" => " !",
        "warn" => " ?",
        _ => "",
    };
    let mut when = fmt_time(e.wall);
    if when.contains(':') {
        when += &alloc::format!(":{:02}", e.wall.rem_euclid(60)); // HH:MM -> HH:MM:SS
    }
    out.line(&alloc::format!(
        "{when} {}[{}]{flag}: {}",
        e.unit,
        e.pid,
        e.msg
    ));
}

fn main(args: &rt::Args) -> i32 {
    let now = rt::sys::clock_ns(sys::CLOCK_REALTIME) as i64 / 1_000_000_000;
    let mut f = Filter {
        units: Vec::new(),
        since: None,
        max_level: 7,
        boot: None,
    };
    let (mut follow, mut last_n, mut this_boot) = (false, None::<usize>, false);
    let mut it = args.argv[1..].iter();
    while let Some(a) = it.next() {
        match *a {
            "-u" | "--unit" => f.units.extend(it.next().map(|s| String::from(*s))),
            "--since" | "-S" => match it.next().and_then(|s| since(s, now)) {
                Some(t) => f.since = Some(t),
                None => {
                    rt::eprintln!("log: --since wants 30s, 10m, 2h, 1d or a unix time");
                    return 2;
                }
            },
            "-n" => last_n = it.next().and_then(|s| s.parse().ok()),
            "-p" => f.max_level = it.next().map_or(7, |l| rank(l)),
            "-b" => this_boot = true,
            "-f" | "--follow" => follow = true,
            o => {
                rt::eprintln!("log: unknown option {o}");
                return 2;
            }
        }
    }
    // The live ring (this boot), then the files (older boots and the flushed part of this
    // one): deduplicated on (boot, seq).
    let live_fd = match sys::open("/dev/journal", sys::O_RDONLY | sys::O_CLOEXEC) {
        Ok(fd) => fd,
        Err(e) => {
            rt::eprintln!("log: /dev/journal: {e} (the journal is for root and group admin)");
            return 1;
        }
    };
    let mut live = Vec::new();
    let mut buf = alloc::vec![0u8; 16384];
    loop {
        match sys::read(live_fd, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => live.extend_from_slice(&buf[..n]),
        }
    }
    let live: Vec<Entry> = String::from_utf8_lossy(&live)
        .lines()
        .filter_map(parse)
        .collect();
    let cur_boot = live.first().map(|e| e.boot.clone());
    if this_boot {
        f.boot = cur_boot.clone();
    }
    let mut all: Vec<Entry> = Vec::new();
    for name in ["journal.3", "journal.2", "journal.1", "current.log"] {
        if let Ok(b) = sys::read_file(&alloc::format!("{DIR}/{name}")) {
            all.extend(String::from_utf8_lossy(&b).lines().filter_map(parse));
        }
    }
    let first_live = live.first().map_or(u64::MAX, |e| e.seq);
    all.retain(|e| Some(&e.boot) != cur_boot.as_ref() || e.seq < first_live);
    all.extend(live);
    let mut shown: Vec<&Entry> = all.iter().filter(|e| f.keep(e)).collect();
    if let Some(n) = last_n {
        let k = shown.len().saturating_sub(n);
        shown.drain(..k);
    }
    let mut out = Out::stdout();
    if rt::table::requested(args) && !follow {
        use rt::table::{Table, Ty, Val};
        let mut t = Table::new(&[
            ("time", Ty::Int),
            ("boot", Ty::Str),
            ("seq", Ty::Int),
            ("unit", Ty::Str),
            ("pid", Ty::Int),
            ("uid", Ty::Int),
            ("level", Ty::Str),
            ("message", Ty::Str),
        ]);
        for e in shown {
            t.push(alloc::vec![
                Val::Int(e.wall),
                Val::str(&e.boot),
                Val::Int(e.seq as i64),
                Val::str(&e.unit),
                Val::Int(e.pid as i64),
                Val::Int(e.uid as i64),
                Val::str(&e.level),
                Val::str(&e.msg),
            ]);
        }
        t.write(&mut out);
        return 0;
    }
    for e in shown {
        show(&mut out, e);
    }
    out.flush();
    if !follow {
        return 0;
    }
    // Follow: /dev/journal's offset is a cursor; at EOF, wait and read on.
    let mut pending = String::new();
    loop {
        match sys::read(live_fd, &mut buf) {
            Ok(0) | Err(_) => sys::sleep_ms(200),
            Ok(n) => {
                pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                while let Some(i) = pending.find('\n') {
                    let line: String = pending.drain(..=i).collect();
                    if let Some(e) = parse(line.trim_end())
                        && f.keep(&e)
                    {
                        show(&mut out, &e);
                    }
                }
                out.flush();
            }
        }
    }
}
