//! The Trash: where deleted things wait, what is recorded about them, how long they stay.
//!
//! ```text
//! ~/.Trash/                         your Trash (0700, yours)
//!   notes.txt                       an item, renamed in from where it was (never copied)
//!   notes 2.txt                     another notes.txt: a free name, nothing is overwritten
//!   .trashinfo/notes.txt            where it came from and when (a small text file each)
//!   .trashinfo/notes 2.txt
//! /Volumes/<disk>/.Trash/           a disk's own Trash for things deleted on it (1777,
//!                                   sticky; each item's metadata says whose it is)
//! ```
//!
//! A metadata file is written (create-exclusive, so two deletes never pick the same name)
//! before the item is renamed in, and removed after it is renamed out (restore) or
//! deleted, so a crash leaves at worst a metadata file with no item (ignored, cleaned up by
//! `trashd`) or an item with no metadata (listed as "unknown origin", aged by its change
//! time). Renames only: an item never leaves its disk.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The folder (inside a Trash) that holds the metadata files.
pub const INFO_DIR: &str = ".trashinfo";
/// The Trash's name in a home folder and at the root of a disk.
pub const TRASH_DIR: &str = ".Trash";
/// How long things stay unless Settings > Trash says otherwise.
pub const DEFAULT_DAYS: u32 = 30;
/// The settings file key (`trash = 30`, `trash = never`) in ~/.config/milk/settings.
pub const SETTINGS_KEY: &str = "trash";
pub const DAY: i64 = 86_400;
/// The longest Settings accepts (ten years).
pub const MAX_DAYS: u32 = 3650;

/// What the metadata file of one item says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    /// Where it was (absolute).
    pub path: String,
    /// When it was deleted (seconds since 1970, milkOS time).
    pub deleted: i64,
    /// Who deleted it.
    pub uid: u32,
}

/// `\` and newlines escaped, so any path fits on one line.
fn escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c => o.push(c),
        }
    }
    o
}

fn unescape(s: &str) -> String {
    let mut o = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('n') => o.push('\n'),
            Some(c) => o.push(c),
            None => o.push('\\'),
        }
    }
    o
}

impl Info {
    /// The metadata file's text.
    pub fn encode(&self) -> String {
        format!(
            "# milkOS Trash: where this item came from (trash restore puts it back)\npath = {}\ndeleted = {}\nuid = {}\n",
            escape(&self.path),
            self.deleted,
            self.uid
        )
    }

    /// Read a metadata file. None if it has no path or no deletion time (a torn write).
    pub fn decode(text: &str) -> Option<Info> {
        let (mut path, mut deleted, mut uid) = (None, None, 0);
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("path = ") {
                path = Some(unescape(v));
            } else if let Some(v) = line.strip_prefix("deleted = ") {
                deleted = v.trim().parse().ok();
            } else if let Some(v) = line.strip_prefix("uid = ") {
                uid = v.trim().parse().unwrap_or(0);
            }
        }
        let path = path.filter(|p| p.starts_with('/'))?;
        Some(Info {
            path,
            deleted: deleted?,
            uid,
        })
    }
}

/// Names an item may not take in a Trash (the metadata folder's).
pub fn reserved(name: &str) -> bool {
    name == INFO_DIR
}

/// The `n`th name to try for `name`: itself, then "notes 2.txt", "notes 3.txt", ...
/// (".profile 2" for a dot file, "projects 2" without an extension).
pub fn candidate(name: &str, n: u32) -> String {
    if n <= 1 {
        return String::from(name);
    }
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{} {n}{}", &name[..i], &name[i..]),
        _ => format!("{name} {n}"),
    }
}

/// The first free name for `name` in a Trash; `taken` says whether a name is in use (by an
/// item or a metadata file).
pub fn free_name(name: &str, mut taken: impl FnMut(&str) -> bool) -> String {
    let mut n = 1;
    loop {
        let c = candidate(name, n);
        if !reserved(&c) && !taken(&c) {
            return c;
        }
        n += 1;
    }
}

/// How long things stay in the Trash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    Days(u32),
    Forever,
}

impl Keep {
    pub const DEFAULT: Keep = Keep::Days(DEFAULT_DAYS);

    /// "30", "30 days", "1 day", "never", "off". None if it isn't one of those.
    pub fn parse(v: &str) -> Option<Keep> {
        let v = v.trim();
        if matches!(v, "never" | "Never" | "off" | "forever") {
            return Some(Keep::Forever);
        }
        let n = v
            .strip_suffix("days")
            .or_else(|| v.strip_suffix("day"))
            .unwrap_or(v)
            .trim();
        match n.parse::<u32>() {
            Ok(d) if (1..=MAX_DAYS).contains(&d) => Some(Keep::Days(d)),
            _ => None,
        }
    }

    /// The settings file's value: "30", "never".
    pub fn value(self) -> String {
        match self {
            Keep::Days(d) => format!("{d}"),
            Keep::Forever => String::from("never"),
        }
    }

    /// "30 days", "1 day", "never".
    pub fn label(self) -> String {
        match self {
            Keep::Days(1) => String::from("1 day"),
            Keep::Days(d) => format!("{d} days"),
            Keep::Forever => String::from("never"),
        }
    }

    /// From a settings file (`trash = ...`); the default if there is no such line or it
    /// doesn't make sense.
    pub fn from_settings(text: &str) -> Keep {
        let mut k = Keep::DEFAULT;
        for line in text.lines() {
            if let Some((key, v)) = line.split_once('=')
                && key.trim() == SETTINGS_KEY
            {
                k = Keep::parse(v).unwrap_or(Keep::DEFAULT);
            }
        }
        k
    }

    /// Whole days until an item deleted at `deleted` goes for good (rounded up: half a day
    /// left is 1), 0 when it is due. None: it never goes.
    pub fn days_left(self, deleted: i64, now: i64) -> Option<i64> {
        match self {
            Keep::Forever => None,
            Keep::Days(d) => {
                let left = deleted + d as i64 * DAY - now;
                Some(if left <= 0 { 0 } else { (left + DAY - 1) / DAY })
            }
        }
    }

    /// Is an item deleted at `deleted` due to be deleted for good at `now`?
    pub fn expired(self, deleted: i64, now: i64) -> bool {
        self.days_left(deleted, now) == Some(0)
    }
}

/// Which Trash item `trash restore ARG` means. `items` are (name in the Trash, metadata).
#[derive(Debug, PartialEq, Eq)]
pub enum Pick {
    One(usize),
    Nothing,
    /// Several items have that name; the user has to say which (by their Trash names).
    Several(Vec<usize>),
}

/// ARG as typed, and `resolved` = ARG made absolute against the current folder. In order:
/// an item's name in the Trash ("notes 2.txt"), then where it was (the newest one deleted
/// from there), then the name it had (if only one item had it).
pub fn pick(arg: &str, resolved: &str, items: &[(String, Option<Info>)]) -> Pick {
    if let Some(i) = items.iter().position(|(n, _)| n == arg) {
        return Pick::One(i);
    }
    let from_there = items
        .iter()
        .enumerate()
        .filter(|(_, (_, inf))| inf.as_ref().is_some_and(|f| f.path == resolved))
        .max_by_key(|(_, (_, inf))| inf.as_ref().map_or(0, |f| f.deleted));
    if let Some((i, _)) = from_there {
        return Pick::One(i);
    }
    let named: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, (_, inf))| {
            inf.as_ref()
                .is_some_and(|f| crate::path::basename(&f.path) == arg)
        })
        .map(|(i, _)| i)
        .collect();
    match named.len() {
        0 => Pick::Nothing,
        1 => Pick::One(named[0]),
        _ => Pick::Several(named),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn info_round_trip() {
        let i = Info {
            path: String::from("/Users/milk/Documents/notes.txt"),
            deleted: 1_792_500_000,
            uid: 501,
        };
        let t = i.encode();
        assert!(t.contains("path = /Users/milk/Documents/notes.txt\n"));
        assert_eq!(Info::decode(&t), Some(i));
        // Odd names survive: spaces at the ends, backslashes, newlines.
        let odd = Info {
            path: String::from("/tmp/ a\\b\nc "),
            deleted: 5,
            uid: 0,
        };
        assert_eq!(Info::decode(&odd.encode()), Some(odd));
    }

    #[test]
    fn info_torn() {
        assert_eq!(Info::decode(""), None);
        assert_eq!(Info::decode("path = /a\n"), None);
        assert_eq!(Info::decode("deleted = 5\n"), None);
        assert_eq!(Info::decode("path = relative\ndeleted = 5\n"), None);
        assert_eq!(
            Info::decode("path = /a\ndeleted = 5\n").map(|i| i.uid),
            Some(0)
        );
    }

    #[test]
    fn names() {
        assert_eq!(candidate("notes.txt", 1), "notes.txt");
        assert_eq!(candidate("notes.txt", 2), "notes 2.txt");
        assert_eq!(candidate("projects", 3), "projects 3");
        assert_eq!(candidate(".profile", 2), ".profile 2");
        assert_eq!(candidate("a.tar.gz", 2), "a.tar 2.gz");
        let used = ["notes.txt", "notes 2.txt"];
        assert_eq!(free_name("notes.txt", |n| used.contains(&n)), "notes 3.txt");
        assert_eq!(free_name("other.txt", |n| used.contains(&n)), "other.txt");
        // Never the metadata folder's name.
        assert_eq!(free_name(INFO_DIR, |_| false), ".trashinfo 2");
    }

    #[test]
    fn keep_parse() {
        assert_eq!(Keep::parse("30"), Some(Keep::Days(30)));
        assert_eq!(Keep::parse(" 7 days"), Some(Keep::Days(7)));
        assert_eq!(Keep::parse("1 day"), Some(Keep::Days(1)));
        assert_eq!(Keep::parse("never"), Some(Keep::Forever));
        assert_eq!(Keep::parse("0"), None);
        assert_eq!(Keep::parse("soon"), None);
        assert_eq!(Keep::parse("99999"), None);
        assert_eq!(Keep::Days(30).value(), "30");
        assert_eq!(Keep::Forever.value(), "never");
        assert_eq!(Keep::Days(1).label(), "1 day");
        assert_eq!(Keep::Days(30).label(), "30 days");
    }

    #[test]
    fn keep_from_settings() {
        assert_eq!(Keep::from_settings(""), Keep::DEFAULT);
        assert_eq!(Keep::from_settings("theme = whole\n"), Keep::Days(30));
        assert_eq!(
            Keep::from_settings("theme = whole\ntrash = 7\n"),
            Keep::Days(7)
        );
        assert_eq!(Keep::from_settings("trash = never\n"), Keep::Forever);
        assert_eq!(Keep::from_settings("trash = nonsense\n"), Keep::DEFAULT);
        assert_eq!(Keep::from_settings("trashcan = 3\n"), Keep::DEFAULT);
    }

    #[test]
    fn ageing() {
        let k = Keep::Days(30);
        let t0 = 1_000_000;
        assert_eq!(k.days_left(t0, t0), Some(30));
        assert_eq!(k.days_left(t0, t0 + 1), Some(30));
        assert_eq!(k.days_left(t0, t0 + 29 * DAY + DAY / 2), Some(1));
        assert_eq!(k.days_left(t0, t0 + 30 * DAY), Some(0));
        assert_eq!(k.days_left(t0, t0 + 400 * DAY), Some(0));
        assert!(!k.expired(t0, t0 + 30 * DAY - 1));
        assert!(k.expired(t0, t0 + 30 * DAY));
        assert_eq!(Keep::Forever.days_left(t0, t0 + 9999 * DAY), None);
        assert!(!Keep::Forever.expired(t0, t0 + 9999 * DAY));
        // A clock that went backwards doesn't make anything due.
        assert_eq!(Keep::Days(1).days_left(t0, t0 - 5 * DAY), Some(6));
    }

    #[test]
    fn picking() {
        let info = |p: &str, t: i64| {
            Some(Info {
                path: String::from(p),
                deleted: t,
                uid: 501,
            })
        };
        let items = vec![
            (String::from("notes.txt"), info("/Users/milk/notes.txt", 10)),
            (
                String::from("notes 2.txt"),
                info("/Users/milk/notes.txt", 20),
            ),
            (String::from("plan.txt"), info("/tmp/x/plan.txt", 5)),
            (String::from("a.txt"), info("/one/a.txt", 1)),
            (String::from("a 2.txt"), info("/two/a.txt", 2)),
            (String::from("lost"), None),
        ];
        assert_eq!(pick("notes 2.txt", "/x/notes 2.txt", &items), Pick::One(1));
        // By where it was: the newest one deleted from there.
        assert_eq!(
            pick("/Users/milk/notes.txt", "/Users/milk/notes.txt", &items),
            Pick::One(1)
        );
        // A Trash name wins over the name things had.
        assert_eq!(
            pick("notes.txt", "/elsewhere/notes.txt", &items),
            Pick::One(0)
        );
        assert_eq!(
            pick("Users/milk/notes.txt", "/Users/milk/notes.txt", &items),
            Pick::One(1)
        );
        assert_eq!(
            pick("plan.txt", "/Users/milk/plan.txt", &items),
            Pick::One(2)
        );
        assert_eq!(
            pick("/tmp/x/plan.txt", "/tmp/x/plan.txt", &items),
            Pick::One(2)
        );
        assert_eq!(pick("lost", "/lost", &items), Pick::One(5));
        assert_eq!(pick("nothing", "/nothing", &items), Pick::Nothing);
        // Two items were called a.txt, neither in the Trash under that name... a.txt is one.
        assert_eq!(pick("a.txt", "/x/a.txt", &items), Pick::One(3));
        let items2 = vec![
            (String::from("a 3.txt"), info("/one/a.txt", 1)),
            (String::from("a 2.txt"), info("/two/a.txt", 2)),
        ];
        assert_eq!(
            pick("a.txt", "/x/a.txt", &items2),
            Pick::Several(vec![0, 1])
        );
    }
}
