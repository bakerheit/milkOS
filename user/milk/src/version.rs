//! Versions and dependencies, semver-ish.
//!
//! `1.2.10 > 1.2.9` (numeric parts compare as numbers, missing parts are 0, so `1.2 ==
//! 1.2.0`); non-numeric parts compare as text; a pre-release (`1.0.0-rc1`, after a `-`)
//! sorts before its release. Dependencies in a manifest: `depends a b>=1.2 c`.

use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

fn part_cmp(a: &str, b: &str) -> Ordering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        (Ok(_), Err(_)) => Ordering::Less, // numbers before words, as semver
        (Err(_), Ok(_)) => Ordering::Greater,
        _ => a.cmp(b),
    }
}

fn parts_cmp(a: &str, b: &str, pad_zero: bool) -> Ordering {
    let pa: Vec<&str> = a.split('.').collect();
    let pb: Vec<&str> = b.split('.').collect();
    for i in 0..pa.len().max(pb.len()) {
        let (x, y) = (pa.get(i), pb.get(i));
        let o = match (x, y) {
            (Some(x), Some(y)) => part_cmp(x, y),
            (Some(x), None) if pad_zero => part_cmp(x, "0"),
            (None, Some(y)) if pad_zero => part_cmp("0", y),
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => Ordering::Equal,
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

pub fn cmp(a: &str, b: &str) -> Ordering {
    let (am, ap) = a.split_once('-').map_or((a, None), |(m, p)| (m, Some(p)));
    let (bm, bp) = b.split_once('-').map_or((b, None), |(m, p)| (m, Some(p)));
    parts_cmp(am, bm, true).then(match (ap, bp) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => parts_cmp(x, y, false),
    })
}

#[derive(Clone, Debug)]
pub struct Dep {
    pub name: String,
    pub min: Option<String>,
}

impl Dep {
    pub fn parse(s: &str) -> Dep {
        match s.split_once(">=") {
            Some((n, v)) => Dep {
                name: String::from(n),
                min: Some(String::from(v)),
            },
            None => Dep {
                name: String::from(s),
                min: None,
            },
        }
    }
    /// Is `version` good enough?
    pub fn ok(&self, version: &str) -> bool {
        self.min
            .as_deref()
            .is_none_or(|m| cmp(version, m) != Ordering::Less)
    }
}

/// Checks for `milk selftest`: (passed, total).
pub fn selftest() -> (usize, usize) {
    use Ordering::*;
    let cases = [
        ("1.0.0", "1.0.0", Equal),
        ("1.2", "1.2.0", Equal),
        ("1.2.10", "1.2.9", Greater),
        ("1.10.0", "1.9.9", Greater),
        ("2.0.0", "10.0.0", Less),
        ("1.0.0-rc1", "1.0.0", Less),
        ("1.0.0-rc.2", "1.0.0-rc.10", Less),
        ("1.0.0-alpha", "1.0.0-beta", Less),
        ("1.1.0", "1.0.9", Greater),
    ];
    let ok = cases.iter().filter(|(a, b, o)| cmp(a, b) == *o).count();
    let d = Dep::parse("fortune>=1.1");
    let dep_ok = d.name == "fortune" && d.ok("1.1.0") && d.ok("1.2") && !d.ok("1.0.9");
    (ok + dep_ok as usize, cases.len() + 1)
}
