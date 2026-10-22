//! Snapshots, clones, automatic snapshots (Time Machine) and `snap diff` output.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use timekit::date::split_secs;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SnapKind {
    /// Taken by Time Machine (`auto-...`).
    Auto,
    /// The live state a promoted clone replaced (`before-...`): rolling back to it undoes the promote.
    Before,
    Manual,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snap {
    pub name: String,
    pub generation: u64,
    /// Wall-clock seconds (the RTC, local time).
    pub created: u64,
    pub kind: SnapKind,
}

impl Snap {
    /// "2026-10-21 13:42".
    pub fn when(&self) -> String {
        let (d, sod) = split_secs(self.created as i64);
        format!("{} {:02}:{:02}", d.fmt(), sod / 3600, sod / 60 % 60)
    }
}

pub fn kind_of(name: &str) -> SnapKind {
    if name.starts_with("auto-") {
        SnapKind::Auto
    } else if name.starts_with("before-") {
        SnapKind::Before
    } else {
        SnapKind::Manual
    }
}

/// Parse `/sys/fs/milkfs/snapshots` ("name gen=N created=SECS"), oldest first as the kernel gives it.
pub fn parse_snapshots(text: &str) -> Vec<Snap> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let name = it.next()?;
            let (mut gen_, mut created) = (None, None);
            for w in it {
                if let Some(v) = w.strip_prefix("gen=") {
                    gen_ = v.parse().ok();
                } else if let Some(v) = w.strip_prefix("created=") {
                    created = v.parse().ok();
                }
            }
            Some(Snap {
                name: name.to_string(),
                generation: gen_?,
                created: created?,
                kind: kind_of(name),
            })
        })
        .collect()
}

/// Which snapshots Rollback to `name` would delete: every newer one.
pub fn lost_by_rollback<'a>(snaps: &'a [Snap], name: &str) -> Vec<&'a Snap> {
    let Some(target) = snaps.iter().find(|s| s.name == name) else {
        return Vec::new();
    };
    snaps
        .iter()
        .filter(|s| s.generation > target.generation)
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clone_ {
    pub name: String,
    pub origin: String,
    /// Where it is mounted (`/Volumes/Milk-name`), if it is.
    pub mount: Option<String>,
}

/// Parse `/sys/fs/milkfs/clones` ("name of=SNAP at=/Volumes/Milk-name").
pub fn parse_clones(text: &str) -> Vec<Clone_> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let name = it.next()?;
            let (mut origin, mut at) = (None, None);
            for w in it {
                if let Some(v) = w.strip_prefix("of=") {
                    origin = Some(v.to_string());
                } else if let Some(v) = w.strip_prefix("at=") {
                    at = Some(v.to_string()).filter(|a| a != "-");
                }
            }
            Some(Clone_ {
                name: name.to_string(),
                origin: origin?,
                mount: at,
            })
        })
        .collect()
}

/// Can this snapshot be deleted? Not while a clone is built on it.
pub fn clones_of<'a>(clones: &'a [Clone_], snap: &str) -> Vec<&'a Clone_> {
    clones.iter().filter(|c| c.origin == snap).collect()
}

/// The text for the `clone` control: "SNAP NAME".
pub fn clone_request(snap: &str, name: &str) -> String {
    format!("{snap} {name}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoSettings {
    pub enabled: bool,
    pub interval: String,
    pub keep: u32,
    pub daily: u32,
    /// "12m" until the next automatic snapshot.
    pub next_in: Option<String>,
    pub count: u32,
}

/// Parse `/sys/fs/milkfs/auto`: "on interval=15m keep=8 daily=14" / "next in 12m" / "N automatic snapshot(s)".
pub fn parse_auto(text: &str) -> Option<AutoSettings> {
    let mut a = AutoSettings {
        enabled: false,
        interval: String::new(),
        keep: 0,
        daily: 0,
        next_in: None,
        count: 0,
    };
    let mut first = true;
    for l in text.lines() {
        if first {
            first = false;
            let mut it = l.split_whitespace();
            a.enabled = match it.next()? {
                "on" => true,
                "off" => false,
                _ => return None,
            };
            for w in it {
                match w.split_once('=') {
                    Some(("interval", v)) => a.interval = v.to_string(),
                    Some(("keep", v)) => a.keep = v.parse().ok()?,
                    Some(("daily", v)) => a.daily = v.parse().ok()?,
                    _ => {}
                }
            }
        } else if let Some(n) = l.strip_prefix("next in ") {
            a.next_in = Some(n.trim().to_string());
        } else if l.contains("automatic snapshot") {
            a.count = l
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        }
    }
    Some(a)
}

/// "15m" as "15 minutes", "1h" as "1 hour", "30s" as "30 seconds".
pub fn interval_words(v: &str) -> String {
    let Some(unit) = v.chars().last() else {
        return String::new();
    };
    let n = &v[..v.len() - unit.len_utf8()];
    let (one, many) = match unit {
        's' => ("second", "seconds"),
        'm' => ("minute", "minutes"),
        'h' => ("hour", "hours"),
        'd' => ("day", "days"),
        _ => return v.to_string(),
    };
    if n == "1" {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// "878s" as "about 15 minutes": a countdown for people (rounded up to whole minutes or hours).
pub fn eta_words(v: &str) -> String {
    let Some(unit) = v.chars().last() else {
        return String::new();
    };
    let Ok(n) = v[..v.len() - unit.len_utf8()].parse::<u64>() else {
        return v.to_string();
    };
    let secs = match unit {
        's' => n,
        'm' => n * 60,
        'h' => n * 3600,
        'd' => n * 86_400,
        _ => return v.to_string(),
    };
    match secs {
        0..=59 => format!("{secs} seconds"),
        60..=3599 => {
            let m = secs.div_ceil(60);
            if m == 1 {
                String::from("about 1 minute")
            } else {
                format!("about {m} minutes")
            }
        }
        _ => {
            let h = secs.div_ceil(3600);
            if h == 1 {
                String::from("about 1 hour")
            } else {
                format!("about {h} hours")
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
    Renamed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    pub path: String,
    /// For a rename, the new path.
    pub to: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Diff {
    /// "before..live: 5 change(s) ..."
    pub header: String,
    pub changes: Vec<Change>,
}

/// Parse what `diff` reads back: a header line then `A path`, `D path`, `M path`, `R old -> new`.
pub fn parse_diff(text: &str) -> Diff {
    let mut d = Diff::default();
    for l in text.lines() {
        if let Some(h) = l.strip_prefix("diff ") {
            d.header = h.to_string();
            continue;
        }
        let Some((tag, rest)) = l.split_once(' ') else {
            continue;
        };
        let change = match tag {
            "A" => Change {
                kind: ChangeKind::Added,
                path: rest.to_string(),
                to: None,
            },
            "D" => Change {
                kind: ChangeKind::Removed,
                path: rest.to_string(),
                to: None,
            },
            "M" => Change {
                kind: ChangeKind::Modified,
                path: rest.to_string(),
                to: None,
            },
            "R" => {
                let Some((a, b)) = rest.split_once(" -> ") else {
                    continue;
                };
                Change {
                    kind: ChangeKind::Renamed,
                    path: a.to_string(),
                    to: Some(b.to_string()),
                }
            }
            _ => continue,
        };
        d.changes.push(change);
    }
    d
}

/// The text for the `diff` control: "A" or "A B" (B defaults to the live volume).
pub fn diff_request(a: &str, b: Option<&str>) -> String {
    match b {
        Some(b) => format!("{a} {b}"),
        None => a.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots() {
        let t = "auto-2026-10-21T06:58:30 gen=12 created=1792565910\nmonday gen=20 created=1792589641\nbefore-fortune gen=25 created=1792590000\nbroken line\n";
        let v = parse_snapshots(t);
        assert_eq!(v.len(), 3);
        assert_eq!(
            (v[0].kind, v[1].kind, v[2].kind),
            (SnapKind::Auto, SnapKind::Manual, SnapKind::Before)
        );
        assert_eq!(v[1].generation, 20);
        assert_eq!(v[1].when(), "2026-10-21 13:34");
        let lost = lost_by_rollback(&v, "auto-2026-10-21T06:58:30");
        assert_eq!(
            lost.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["monday", "before-fortune"]
        );
        assert!(lost_by_rollback(&v, "before-fortune").is_empty());
        assert!(lost_by_rollback(&v, "missing").is_empty());
    }

    #[test]
    fn clones() {
        let t = "try1 of=monday at=/Volumes/Milk-try1\nfloating of=auto-1 at=-\nbad\n";
        let c = parse_clones(t);
        assert_eq!(c.len(), 2);
        assert_eq!(
            (c[0].origin.as_str(), c[0].mount.as_deref()),
            ("monday", Some("/Volumes/Milk-try1"))
        );
        assert_eq!(c[1].mount, None);
        assert_eq!(clones_of(&c, "monday").len(), 1);
        assert!(clones_of(&c, "nothing").is_empty());
        assert_eq!(clone_request("monday", "try2"), "monday try2");
    }

    #[test]
    fn auto() {
        let a =
            parse_auto("on interval=15m keep=8 daily=14\nnext in 12m\n3 automatic snapshot(s)\n")
                .unwrap();
        assert!(a.enabled);
        assert_eq!(
            (a.interval.as_str(), a.keep, a.daily, a.count),
            ("15m", 8, 14, 3)
        );
        assert_eq!(a.next_in.as_deref(), Some("12m"));
        let off = parse_auto("off interval=1h keep=4 daily=7\n0 automatic snapshot(s)\n").unwrap();
        assert!(!off.enabled && off.next_in.is_none());
        assert!(parse_auto("no milkfs volume\n").is_none());
    }

    #[test]
    fn interval_text() {
        assert_eq!(interval_words("15m"), "15 minutes");
        assert_eq!(interval_words("1h"), "1 hour");
        assert_eq!(interval_words("2h"), "2 hours");
        assert_eq!(interval_words("30s"), "30 seconds");
        assert_eq!(interval_words("1d"), "1 day");
        assert_eq!(interval_words(""), "");
        assert_eq!(interval_words("7x"), "7x");
    }

    #[test]
    fn countdown_text() {
        assert_eq!(eta_words("878s"), "about 15 minutes");
        assert_eq!(eta_words("12m"), "about 12 minutes");
        assert_eq!(eta_words("45s"), "45 seconds");
        assert_eq!(eta_words("60s"), "about 1 minute");
        assert_eq!(eta_words("1h"), "about 1 hour");
        assert_eq!(eta_words("2h"), "about 2 hours");
        assert_eq!(eta_words("x"), "x");
    }

    #[test]
    fn diffs() {
        let t = "diff before..live: 5 change(s) (2 tree nodes read, 0 unchanged subtrees skipped)\nA /Users/milk/Documents/Plans 2026\nD /Users/milk/Documents/todo.txt\nM /Users/milk/README.txt\nR /dd/a -> /dd/sub/a2\nweird\n";
        let d = parse_diff(t);
        assert!(d.header.starts_with("before..live: 5 change(s)"));
        assert_eq!(d.changes.len(), 4);
        assert_eq!(
            d.changes[0],
            Change {
                kind: ChangeKind::Added,
                path: "/Users/milk/Documents/Plans 2026".into(),
                to: None
            }
        );
        assert_eq!(d.changes[3].to.as_deref(), Some("/dd/sub/a2"));
        assert_eq!(diff_request("a", None), "a");
        assert_eq!(diff_request("a", Some("b")), "a b");
    }
}
