//! Version numbers compared semver-ish (shared by `apps` and the `ver` table column type):
//! numeric parts as numbers (`1.2.10 > 1.2.9`, missing parts are 0 so `1.2 == 1.2.0`),
//! other parts as text, and a pre-release (`1.0.0-rc.1`, after a `-`) before its release.

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
