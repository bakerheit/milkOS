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

/// Why the shell's `go` couldn't get into `shown`. `is_file`: `shown` is there but isn't a
/// folder.
pub fn say_go(shown: &str, errno: i32, is_file: bool) -> String {
    match errno {
        _ if is_file => format!("{shown} is a file, not a folder"),
        1 | 13 => format!("you can't open {shown}"),
        e => say(shown, e),
    }
}

/// Why `files access` couldn't change `shown` (owned by `owner`).
pub fn say_access(shown: &str, errno: i32, owner: &str) -> String {
    match errno {
        1 => format!(
            "you can't change who may use {shown}: only its owner ({owner}) or an admin can"
        ),
        e => say(shown, e),
    }
}

/// Why `files owner` couldn't change `shown` (the kernel said EPERM). `new_user`: a
/// different owner was asked for; `is_owner`: the caller owns it; `owner`, `group`: its
/// owner now and the group asked for.
pub fn say_owner(shown: &str, new_user: bool, is_owner: bool, owner: &str, group: &str) -> String {
    if new_user {
        format!("only an admin can change who owns {shown} (admin files owner {shown} ... does it)")
    } else if !is_owner {
        format!("you can't change the group of {shown}: only its owner ({owner}) or an admin can")
    } else {
        format!("you're not in the group {group}, so you can't give {shown} to it")
    }
}

/// A user or group name that isn't there.
pub fn no_user(name: &str) -> String {
    format!("there's no user called {name} (users list shows them)")
}
pub fn no_group(name: &str) -> String {
    format!("there's no group called {name}")
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
        assert_eq!(say_go("Documents", 2, false), "Documents doesn't exist");
        assert_eq!(
            say_go("notes.txt", 20, true),
            "notes.txt is a file, not a folder"
        );
        assert_eq!(say_go("/locked", 13, false), "you can't open /locked");
    }

    #[test]
    fn refusals() {
        assert_eq!(
            say_access("notes.txt", 1, "milk"),
            "you can't change who may use notes.txt: only its owner (milk) or an admin can"
        );
        assert_eq!(say_access("x", 2, "milk"), "x doesn't exist");
        assert_eq!(
            say_owner("notes.txt", true, true, "milk", "staff"),
            "only an admin can change who owns notes.txt (admin files owner notes.txt ... does it)"
        );
        assert_eq!(
            say_owner("notes.txt", false, false, "root", "staff"),
            "you can't change the group of notes.txt: only its owner (root) or an admin can"
        );
        assert_eq!(
            say_owner("notes.txt", false, true, "milk", "wheel"),
            "you're not in the group wheel, so you can't give notes.txt to it"
        );
        assert_eq!(
            no_user("bob"),
            "there's no user called bob (users list shows them)"
        );
        assert_eq!(no_group("devs"), "there's no group called devs");
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
