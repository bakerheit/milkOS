//! The system journal (kernel/src/journal.rs) as `services logs` reads it: one entry per line,
//! `seq boot mono wall pid uid unit level message` separated by tabs; the filters.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub seq: u64,
    pub boot: String,
    pub wall: i64,
    pub pid: u32,
    pub uid: u32,
    pub unit: String,
    pub level: String,
    pub msg: String,
}

/// One journal line, or `None` if it isn't one.
pub fn parse(line: &str) -> Option<Entry> {
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

/// syslog rank: err 3, warn 4, info 6, anything else (debug) 7.
pub fn rank(level: &str) -> u32 {
    match level {
        "err" => 3,
        "warn" => 4,
        "info" => 6,
        _ => 7,
    }
}

/// What `services logs` shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Opts {
    /// Services (journal units); empty = all. `NAME` also matches `NAME/...`.
    pub units: Vec<String>,
    pub since: Option<i64>,
    /// Only entries at least this important (rank at most this); 7 = everything.
    pub max_level: u32,
    pub this_boot: bool,
    pub last: Option<usize>,
    pub follow: bool,
}

impl Opts {
    pub fn keep(&self, e: &Entry, boot: Option<&str>) -> bool {
        (self.units.is_empty()
            || self
                .units
                .iter()
                .any(|u| e.unit == *u || e.unit.starts_with(&format!("{u}/"))))
            && self.since.is_none_or(|t| e.wall >= t)
            && rank(&e.level) <= self.max_level
            && (!self.this_boot || boot.is_none_or(|b| e.boot == b))
    }
}

/// "10m" / "2h" / "1d" / "30s" ago (from `now`), or a Unix time.
pub fn since(s: &str, now: i64) -> Option<i64> {
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

/// `services logs [NAME...] [--follow] [--since T] [--last N] [--level L] [--this-boot]`.
/// Err: the message (without the "services logs: " in front).
pub fn parse_args(args: &[&str], now: i64) -> Result<Opts, String> {
    let mut o = Opts {
        max_level: 7,
        ..Opts::default()
    };
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        match a {
            "--follow" | "-f" => o.follow = true,
            "--this-boot" => o.this_boot = true,
            "--since" => match it.next().and_then(|s| since(s, now)) {
                Some(t) => o.since = Some(t),
                None => {
                    return Err(String::from(
                        "--since wants 30s, 10m, 2h, 1d or a Unix time",
                    ));
                }
            },
            "--last" => match it.next().and_then(|s| s.parse().ok()) {
                Some(n) => o.last = Some(n),
                None => return Err(String::from("--last wants a number of entries")),
            },
            "--level" => match it.next() {
                Some(&l @ ("err" | "warn" | "info" | "debug")) => o.max_level = rank(l),
                _ => return Err(String::from("--level wants err, warn, info or debug")),
            },
            o2 if o2.starts_with('-') => {
                return Err(format!("there's no option {o2} (try services help)"));
            }
            unit => o.units.push(String::from(unit)),
        }
    }
    Ok(o)
}

/// "Oct 22 13:05:09 alarmd[12] !: message" (`!` an error, `?` a warning).
pub fn show(e: &Entry) -> String {
    let flag = match e.level.as_str() {
        "err" => " !",
        "warn" => " ?",
        _ => "",
    };
    format!(
        "{} {}[{}]{flag}: {}",
        crate::time::stamp(e.wall),
        e.unit,
        e.pid,
        e.msg
    )
}

/// Older boots' files (oldest first) and the live ring, merged: the files' entries of this boot
/// that the ring still holds are dropped, so nothing shows twice.
pub fn merge(files: Vec<Entry>, live: Vec<Entry>) -> Vec<Entry> {
    let cur = live.first().map(|e| e.boot.clone());
    let first_live = live.first().map_or(u64::MAX, |e| e.seq);
    let mut all: Vec<Entry> = files
        .into_iter()
        .filter(|e| Some(&e.boot) != cur.as_ref() || e.seq < first_live)
        .collect();
    all.extend(live);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(seq: u64, boot: &str, wall: i64, unit: &str, level: &str) -> Entry {
        parse(&format!(
            "{seq}\t{boot}\t0\t{wall}\t7\t0\t{unit}\t{level}\tmsg {seq}"
        ))
        .unwrap()
    }

    #[test]
    fn lines() {
        let x = parse("5\tb1\t100\t1792674309\t12\t0\tdrop/mdns\twarn\tit: has\ttabs").unwrap();
        assert_eq!(x.seq, 5);
        assert_eq!(x.unit, "drop/mdns");
        assert_eq!(x.msg, "it: has\ttabs");
        assert_eq!(show(&x), "Oct 22 13:05:09 drop/mdns[12] ?: it: has\ttabs");
        assert!(parse("not a line").is_none());
        assert!(parse("x\tb\t0\t1\t2\t3\tu\tinfo\tm").is_none());
    }

    #[test]
    fn options() {
        let now = 10_000_000;
        let o = parse_args(
            &["trashd", "--since", "10m", "--level", "warn", "--last", "5"],
            now,
        )
        .unwrap();
        assert_eq!(o.units, ["trashd"]);
        assert_eq!(o.since, Some(now - 600));
        assert_eq!(o.max_level, 4);
        assert_eq!(o.last, Some(5));
        assert!(!o.follow);
        let o = parse_args(&["--follow", "--this-boot"], now).unwrap();
        assert!(o.follow && o.this_boot && o.units.is_empty() && o.max_level == 7);
        assert_eq!(since("2h", now), Some(now - 7200));
        assert_eq!(since("1d", now), Some(now - 86_400));
        assert_eq!(since("30", now), Some(now - 30));
        assert_eq!(since("1792674309", now), Some(1_792_674_309));
        assert_eq!(since("5y", now), None);
        assert!(parse_args(&["--since", "soon"], now).is_err());
        assert!(parse_args(&["--last"], now).is_err());
        assert!(parse_args(&["--level", "loud"], now).is_err());
        assert_eq!(
            parse_args(&["-u", "x"], now),
            Err(String::from("there's no option -u (try services help)"))
        );
    }

    #[test]
    fn filters_and_merge() {
        let o = parse_args(&["drop", "--level", "warn"], 0).unwrap();
        assert!(o.keep(&e(1, "b", 5, "drop/mdns", "err"), None));
        assert!(o.keep(&e(1, "b", 5, "drop", "warn"), None));
        assert!(!o.keep(&e(1, "b", 5, "drop", "info"), None));
        assert!(!o.keep(&e(1, "b", 5, "dropper", "err"), None));
        let o = parse_args(&["--this-boot"], 0).unwrap();
        assert!(o.keep(&e(1, "b2", 5, "x", "info"), Some("b2")));
        assert!(!o.keep(&e(1, "b1", 5, "x", "info"), Some("b2")));
        let files = alloc::vec![
            e(1, "b1", 1, "x", "info"),
            e(1, "b2", 2, "x", "info"),
            e(2, "b2", 3, "x", "info")
        ];
        let live = alloc::vec![e(2, "b2", 3, "x", "info"), e(3, "b2", 4, "x", "info")];
        let m = merge(files, live);
        let seqs: Vec<(String, u64)> = m.iter().map(|x| (x.boot.clone(), x.seq)).collect();
        assert_eq!(
            seqs,
            [
                (String::from("b1"), 1),
                (String::from("b2"), 1),
                (String::from("b2"), 2),
                (String::from("b2"), 3)
            ]
        );
    }
}
