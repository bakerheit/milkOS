//! Errors in plain English: "notes.txt doesn't exist", not "notes.txt: ENOENT".

use alloc::format;
use alloc::string::String;

/// What went wrong with `shown` (the path as the user typed it), from a Linux errno.
pub fn say(shown: &str, errno: i32) -> String {
    match errno {
        1 | 13 => format!("you don't have permission to change {shown}"),
        2 => format!("{shown} doesn't exist"),
        5 => format!("{shown} couldn't be read or written (a disk error)"),
        16 => format!("{shown} is in use (a mounted disk can't be moved or deleted)"),
        17 => format!("{shown} already exists"),
        18 => format!("{shown} is on another disk"),
        20 => format!("{shown} doesn't exist (part of its path is a file, not a folder)"),
        21 => format!("{shown} is a folder, not a file"),
        22 => format!("{shown} can't be used that way"),
        28 => format!("{shown} doesn't fit: the disk is full"),
        30 => format!("{shown} is on a read-only disk"),
        36 => format!("{shown}: the name is too long"),
        39 => format!("{shown} isn't empty"),
        40 => format!("{shown} goes through too many links"),
        95 => format!("{shown}: that isn't possible on this disk"),
        e => format!("{shown}: failed (error {e})"),
    }
}

/// The same for reading (listing, info, copying from): permission is about reading.
pub fn say_read(shown: &str, errno: i32) -> String {
    match errno {
        1 | 13 => format!("you don't have permission to read {shown}"),
        e => say(shown, e),
    }
}

/// "1 item", "3 items".
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// 1536 -> "1.5K", like `ls -h`.
pub fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n}")
    } else if v < 10.0 {
        format!("{:.1}{}", v, UNITS[u])
    } else {
        format!("{:.0}{}", v, UNITS[u])
    }
}

/// Sizes as people read them: "0 B", "512 B", "1.5 KB", "20 MB".
pub fn size(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let h = human(n);
    let (num, unit) = h.split_at(h.len() - 1);
    format!("{num} {unit}B")
}

/// 1234567 -> "1,234,567".
pub fn grouped(n: u64) -> String {
    let s = format!("{n}");
    let mut o = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            o.push(',');
        }
        o.push(c);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain() {
        assert_eq!(say("notes.txt", 2), "notes.txt doesn't exist");
        assert_eq!(say("projects", 21), "projects is a folder, not a file");
        assert_eq!(say("x", 17), "x already exists");
        assert_eq!(say("x", 13), "you don't have permission to change x");
        assert_eq!(say_read("x", 13), "you don't have permission to read x");
        assert_eq!(say_read("x", 2), "x doesn't exist");
        assert_eq!(say("x", 9999), "x: failed (error 9999)");
    }

    #[test]
    fn numbers() {
        assert_eq!(count(1, "item", "items"), "1 item");
        assert_eq!(count(0, "item", "items"), "0 items");
        assert_eq!(human(1023), "1023");
        assert_eq!(human(1536), "1.5K");
        assert_eq!(human(20 * 1024 * 1024), "20M");
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(20 * 1024 * 1024), "20 MB");
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }
}
