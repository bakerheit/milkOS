//! Who may use a file or folder, in words, and the words that set it (`files access` and
//! `files owner`, which replaced chmod and chown).
//!
//! | word        | file | folder |
//! |-------------|------|--------|
//! | `private`   | 600  | 700    |
//! | `shared`    | 644  | 755    |
//! | `group`     | 660  | 770    |
//! | `read-only` | 444  | 555    |
//! | `open`      | 666  | 777    |
//!
//! A runnable file (any execute bit) stays runnable: it gets execute wherever the word
//! gives read, so a script set to `shared` is 755 and `private` is 700. A word never keeps
//! set-user-ID or set-group-ID on a file (only `--mode` sets those); on a folder it keeps
//! set-group-ID and the sticky bit, which say how the folder works, not who may use it.
//! `runnable` / `not-runnable` turn execute on for whoever can read, or off for everyone.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// An access word.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Word {
    Private,
    Shared,
    Group,
    ReadOnly,
    Open,
}

/// (word, what it is, file mode, folder mode), in the order help lists them.
pub const WORDS: [(&str, Word, u32, u32); 5] = [
    ("private", Word::Private, 0o600, 0o700),
    ("shared", Word::Shared, 0o644, 0o755),
    ("group", Word::Group, 0o660, 0o770),
    ("read-only", Word::ReadOnly, 0o444, 0o555),
    ("open", Word::Open, 0o666, 0o777),
];

impl Word {
    pub fn parse(s: &str) -> Option<Word> {
        WORDS.iter().find(|w| w.0 == s).map(|w| w.1)
    }

    pub fn name(self) -> &'static str {
        WORDS.iter().find(|w| w.1 == self).map_or("?", |w| w.0)
    }

    /// The mode for a file (not runnable) and for a folder.
    pub fn modes(self) -> (u32, u32) {
        WORDS
            .iter()
            .find(|w| w.1 == self)
            .map_or((0, 0), |w| (w.2, w.3))
    }
}

/// What `files access PATH HOW` asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum How {
    Word(Word),
    /// `runnable` (true) or `not-runnable`.
    Runnable(bool),
    /// `--mode NNNN`: exactly these bits.
    Mode(u32),
}

/// The words `files access` takes, for messages.
pub const HOW_WORDS: &str = "private, shared, group, read-only, open, runnable or not-runnable";

/// `private`, `runnable`... Err: the message.
pub fn parse_how(s: &str) -> Result<How, String> {
    match s {
        "runnable" => Ok(How::Runnable(true)),
        "not-runnable" => Ok(How::Runnable(false)),
        _ => Word::parse(s).map(How::Word).ok_or_else(|| {
            format!("{s} isn't a way to set access: use {HOW_WORDS} (or --mode NNNN)")
        }),
    }
}

/// `--mode`'s value: one to four octal digits (644, 0755, 4755, 1777).
pub fn parse_mode(s: &str) -> Result<u32, String> {
    let ok = !s.is_empty() && s.len() <= 4 && s.bytes().all(|b| (b'0'..=b'7').contains(&b));
    match (ok, u32::from_str_radix(s, 8)) {
        (true, Ok(m)) => Ok(m),
        _ => Err(format!(
            "--mode wants an octal number like 644, 755 or 4755 (up to four digits 0-7), not {s}"
        )),
    }
}

/// The mode `how` gives something whose mode is `cur` (permission bits; the type bits are
/// ignored). None: `runnable` / `not-runnable` on a folder, which isn't run.
pub fn new_mode(how: How, folder: bool, cur: u32) -> Option<u32> {
    let cur = cur & 0o7777;
    Some(match how {
        How::Mode(m) => m & 0o7777,
        How::Word(w) => {
            let (file, dir) = w.modes();
            if folder {
                dir | (cur & 0o3000)
            } else if cur & 0o111 != 0 {
                file | ((file & 0o444) >> 2)
            } else {
                file
            }
        }
        How::Runnable(_) if folder => return None,
        How::Runnable(true) => cur | ((cur & 0o444) >> 2),
        How::Runnable(false) => cur & !0o111,
    })
}

/// "a", "a and b", "a, b and c".
fn and_list(v: &[&str]) -> String {
    match v {
        [] => String::new(),
        [one] => String::from(*one),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What one rwx triple lets you do. Files: read, change, run. Folders: open (go in, x),
/// list (see the names, r), change (add, remove and rename things in it, w).
fn abilities(bits: u32, folder: bool) -> String {
    let mut v: Vec<&str> = Vec::new();
    if folder {
        if bits & 1 != 0 {
            v.push("open");
        }
        if bits & 4 != 0 {
            v.push("list");
        }
        if bits & 2 != 0 {
            v.push("change");
        }
    } else {
        if bits & 4 != 0 {
            v.push("read");
        }
        if bits & 2 != 0 {
            v.push("change");
        }
        if bits & 1 != 0 {
            v.push("run");
        }
    }
    and_list(&v)
}

/// "milk can read and change it", or "milk can't use it" ("can't" alone at the end).
fn clause(who: &str, bits: u32, folder: bool, last: bool) -> String {
    match (bits, last) {
        (0, true) => format!("{who} can't"),
        (0, false) => format!("{who} can't use it"),
        _ => format!("{who} can {} it", abilities(bits, folder)),
    }
}

/// Who may use it, in one sentence: "milk can read and change it, staff can read it,
/// everyone else can't". `mode` is st_mode (only the permission bits count); `owner` and
/// `group` are names. Unix checks owner, then group, then everyone else, and the first
/// that matches decides, so each clause is exactly what that class gets. Set-ID and sticky
/// bits are added at the end.
pub fn sentence(mode: u32, folder: bool, owner: &str, group: &str) -> String {
    let (u, g, o) = (mode >> 6 & 7, mode >> 3 & 7, mode & 7);
    let mut s = if u == g && g == o {
        if u == 0 {
            String::from("nobody can use it except root")
        } else {
            format!("everyone can {} it", abilities(u, folder))
        }
    } else if g == o {
        format!(
            "{}, {}",
            clause(owner, u, folder, false),
            clause("everyone else", o, folder, true)
        )
    } else if u == g {
        format!(
            "{}, {}",
            clause(&format!("{owner} and {group}"), u, folder, false),
            clause("everyone else", o, folder, true)
        )
    } else {
        format!(
            "{}, {}, {}",
            clause(owner, u, folder, false),
            clause(group, g, folder, false),
            clause("everyone else", o, folder, true)
        )
    };
    if folder {
        if mode & 0o2000 != 0 {
            s.push_str(&format!(
                "; new things in it get group {group} (set-group-ID)"
            ));
        }
        if mode & 0o1000 != 0 {
            s.push_str("; only their owners can delete or rename things in it (sticky)");
        }
    } else {
        if mode & 0o4000 != 0 {
            s.push_str(&format!("; it runs as {owner} (set-user-ID)"));
        }
        if mode & 0o2000 != 0 {
            s.push_str(&format!("; it runs with group {group} (set-group-ID)"));
        }
    }
    s
}

/// `files owner`'s new owner: `USER`, `USER:GROUP` or `:GROUP` (names or numbers), as
/// (user, group). `USER:` is just the user.
pub fn parse_owner(spec: &str) -> Result<(Option<&str>, Option<&str>), String> {
    let (u, g) = spec.split_once(':').unwrap_or((spec, ""));
    if g.contains(':') {
        return Err(format!(
            "{spec} isn't an owner: use USER, USER:GROUP or :GROUP (files owner notes.txt milk:staff)"
        ));
    }
    let u = (!u.is_empty()).then_some(u);
    let g = (!g.is_empty()).then_some(g);
    if u.is_none() && g.is_none() {
        return Err(String::from(
            "say who: USER, USER:GROUP or :GROUP (files owner notes.txt milk:staff)",
        ));
    }
    Ok((u, g))
}

/// `files owner PATH`: "milk (group: staff)".
pub fn owner_words(owner: &str, group: &str) -> String {
    format!("{owner} (group: {group})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_to_modes() {
        let cases = [
            ("private", 0o600, 0o700),
            ("shared", 0o644, 0o755),
            ("group", 0o660, 0o770),
            ("read-only", 0o444, 0o555),
            ("open", 0o666, 0o777),
        ];
        for (w, file, dir) in cases {
            let how = parse_how(w).unwrap();
            assert_eq!(new_mode(how, false, 0o644), Some(file), "{w} file");
            assert_eq!(new_mode(how, true, 0o755), Some(dir), "{w} folder");
            assert_eq!(Word::parse(w).unwrap().name(), w);
        }
        assert!(Word::parse("Private").is_none());
        assert!(
            parse_how("public")
                .unwrap_err()
                .contains("public isn't a way to set access")
        );
    }

    #[test]
    fn runnable_files_stay_runnable() {
        let w = |s| parse_how(s).unwrap();
        // A script (755, or just owner-runnable 744) keeps running for whoever may read it.
        assert_eq!(new_mode(w("shared"), false, 0o700), Some(0o755));
        assert_eq!(new_mode(w("shared"), false, 0o744), Some(0o755));
        assert_eq!(new_mode(w("private"), false, 0o755), Some(0o700));
        assert_eq!(new_mode(w("group"), false, 0o755), Some(0o770));
        assert_eq!(new_mode(w("read-only"), false, 0o755), Some(0o555));
        assert_eq!(new_mode(w("open"), false, 0o711), Some(0o777));
        // Not runnable: plain modes.
        assert_eq!(new_mode(w("open"), false, 0o600), Some(0o666));
        // Type bits in st_mode don't matter.
        assert_eq!(new_mode(w("shared"), false, 0o100755), Some(0o755));
    }

    #[test]
    fn special_bits() {
        let w = |s| parse_how(s).unwrap();
        // Files: a word drops set-user-ID and set-group-ID (only --mode sets them).
        assert_eq!(new_mode(w("shared"), false, 0o4755), Some(0o755));
        assert_eq!(new_mode(w("open"), false, 0o2755), Some(0o777));
        // Folders keep set-group-ID and sticky.
        assert_eq!(new_mode(w("group"), true, 0o2770), Some(0o2770));
        assert_eq!(new_mode(w("open"), true, 0o1777), Some(0o1777));
        assert_eq!(new_mode(w("private"), true, 0o3755), Some(0o3700));
        // --mode is exact.
        assert_eq!(new_mode(How::Mode(0o4755), false, 0o644), Some(0o4755));
        assert_eq!(new_mode(How::Mode(0o644), true, 0o2755), Some(0o644));
    }

    #[test]
    fn runnable_toggle() {
        let on = How::Runnable(true);
        let off = How::Runnable(false);
        assert_eq!(new_mode(on, false, 0o644), Some(0o755));
        assert_eq!(new_mode(on, false, 0o600), Some(0o700));
        assert_eq!(new_mode(on, false, 0o640), Some(0o750));
        assert_eq!(new_mode(on, false, 0o200), Some(0o200)); // nobody can read: no change
        assert_eq!(new_mode(off, false, 0o755), Some(0o644));
        assert_eq!(new_mode(off, false, 0o4755), Some(0o4644));
        assert_eq!(new_mode(on, true, 0o755), None);
        assert_eq!(new_mode(off, true, 0o755), None);
        assert_eq!(parse_how("runnable"), Ok(on));
        assert_eq!(parse_how("not-runnable"), Ok(off));
    }

    #[test]
    fn modes() {
        assert_eq!(parse_mode("644"), Ok(0o644));
        assert_eq!(parse_mode("0755"), Ok(0o755));
        assert_eq!(parse_mode("4755"), Ok(0o4755));
        assert_eq!(parse_mode("1777"), Ok(0o1777));
        assert_eq!(parse_mode("0"), Ok(0));
        for bad in ["", "8", "789", "07777", "u+x", "-644", "0o644"] {
            assert!(
                parse_mode(bad)
                    .unwrap_err()
                    .starts_with("--mode wants an octal number"),
                "{bad}"
            );
        }
    }

    #[test]
    fn sentences() {
        let s = |m, d| sentence(m, d, "milk", "staff");
        assert_eq!(
            s(0o640, false),
            "milk can read and change it, staff can read it, everyone else can't"
        );
        assert_eq!(
            s(0o644, false),
            "milk can read and change it, everyone else can read it"
        );
        assert_eq!(
            s(0o600, false),
            "milk can read and change it, everyone else can't"
        );
        assert_eq!(
            s(0o660, false),
            "milk and staff can read and change it, everyone else can't"
        );
        assert_eq!(
            s(0o755, false),
            "milk can read, change and run it, everyone else can read and run it"
        );
        assert_eq!(s(0o444, false), "everyone can read it");
        assert_eq!(s(0o666, false), "everyone can read and change it");
        assert_eq!(s(0o000, false), "nobody can use it except root");
        assert_eq!(
            s(0o604, false),
            "milk can read and change it, staff can't use it, everyone else can read it"
        );
        assert_eq!(
            s(0o755, true),
            "milk can open, list and change it, everyone else can open and list it"
        );
        assert_eq!(
            s(0o700, true),
            "milk can open, list and change it, everyone else can't"
        );
        assert_eq!(s(0o777, true), "everyone can open, list and change it");
        assert_eq!(
            s(0o4755, false),
            "milk can read, change and run it, everyone else can read and run it; it runs as milk (set-user-ID)"
        );
        assert_eq!(
            s(0o2750, false),
            "milk can read, change and run it, staff can read and run it, everyone else can't; it runs with group staff (set-group-ID)"
        );
        assert_eq!(
            s(0o1777, true),
            "everyone can open, list and change it; only their owners can delete or rename things in it (sticky)"
        );
        assert_eq!(
            s(0o2770, true),
            "milk and staff can open, list and change it, everyone else can't; new things in it get group staff (set-group-ID)"
        );
        // Type bits are ignored.
        assert_eq!(s(0o100644, false), s(0o644, false));
    }

    #[test]
    fn owners() {
        assert_eq!(parse_owner("milk"), Ok((Some("milk"), None)));
        assert_eq!(parse_owner("milk:staff"), Ok((Some("milk"), Some("staff"))));
        assert_eq!(parse_owner(":staff"), Ok((None, Some("staff"))));
        assert_eq!(parse_owner("milk:"), Ok((Some("milk"), None)));
        assert_eq!(parse_owner("501:20"), Ok((Some("501"), Some("20"))));
        assert!(parse_owner(":").unwrap_err().starts_with("say who"));
        assert!(parse_owner("").unwrap_err().starts_with("say who"));
        assert!(parse_owner("a:b:c").unwrap_err().contains("isn't an owner"));
        assert_eq!(owner_words("milk", "staff"), "milk (group: staff)");
    }
}
