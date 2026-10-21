//! Snapshot and clone names, and the words for the errors the control files give.

use alloc::format;
use alloc::string::String;

pub const MAX_NAME: usize = 47;
pub const MAX_SNAPSHOTS: usize = 30;

/// Names are `[A-Za-z0-9._:-]`, 1 to 47 characters.
pub fn check_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("Give it a name");
    }
    if name.chars().count() > MAX_NAME {
        return Err("The name is too long (47 characters at most)");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
    {
        return Err("Use letters, numbers and . _ : - only");
    }
    Ok(())
}

/// A name for a snapshot taken now: `snap-2026-10-21-1342`.
pub fn suggest(date: timekit::Date, minutes: u16) -> String {
    format!("snap-{}-{:02}{:02}", date.fmt(), minutes / 60, minutes % 60)
}

/// What an errno from a control file means here, in words.
pub fn why(errno: i32) -> &'static str {
    match errno {
        2 => "It is not there (for a receive: the stream's base snapshot is missing)",
        5 => "An I/O error, or the stream is damaged",
        13 => "Wrong passphrase or recovery key",
        16 => {
            "Busy: a clone is built on it, a backup is reading it, or the volume changed since the base snapshot"
        }
        17 => "That name is already used",
        18 => "Not a milkfs volume",
        19 => "No milkfs volume",
        22 => "That name or setting is not valid",
        28 => "No room (30 snapshots, clones included, at most)",
        30 => "Read-only",
        36 => "The name is too long (47 characters at most)",
        39 => "A full backup needs an empty volume",
        _ => "Something went wrong",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(check_name("monday").is_ok());
        assert!(check_name("auto-2026-10-21T06:58:30").is_ok());
        assert!(check_name("a.b_c-d:e").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name("has space").is_err());
        assert!(check_name("slash/no").is_err());
        assert!(check_name("é").is_err());
        assert!(check_name(&"x".repeat(47)).is_ok());
        assert!(check_name(&"x".repeat(48)).is_err());
    }

    #[test]
    fn suggestion_is_a_valid_name() {
        let d = timekit::Date::new(2026, 10, 21).unwrap();
        let s = suggest(d, 13 * 60 + 42);
        assert_eq!(s, "snap-2026-10-21-1342");
        assert!(check_name(&s).is_ok());
    }

    #[test]
    fn error_words() {
        assert!(why(13).contains("passphrase"));
        assert!(why(16).starts_with("Busy"));
        assert_eq!(why(999), "Something went wrong");
    }
}
