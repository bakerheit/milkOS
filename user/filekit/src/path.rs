//! Paths as `files` sees them: the trailing-slash rule, normalising, where a copy or a move
//! lands.
//!
//! The trailing-slash rule: on `files create` the slash decides (`notes.txt` is a file,
//! `projects/` a folder). Everywhere else what is on disk decides, and a slash only says
//! "I mean a folder": `files delete projects` and `files delete projects/` both work on a
//! folder, but `files delete notes.txt/` on a file is an error, never a guess.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// What a path names, from its mode bits (or the kernel's inode type).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Folder,
    Link,
    /// A device, a pipe or a socket.
    Special,
}

impl Kind {
    /// From `st_mode`'s type bits.
    pub fn from_mode(mode: u32) -> Kind {
        match mode & 0o170000 {
            0o040000 => Kind::Folder,
            0o120000 => Kind::Link,
            0o100000 => Kind::File,
            _ => Kind::Special,
        }
    }

    /// "file", "folder", "link", "special file".
    pub fn word(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Folder => "folder",
            Kind::Link => "link",
            Kind::Special => "special file",
        }
    }
}

/// An argument as typed, split into the path (trailing slashes off) and whether it had
/// one. `/` stays `/` (and says nothing: the root is a folder anyway).
pub fn split_slash(arg: &str) -> (&str, bool) {
    let t = arg.trim_end_matches('/');
    if t.is_empty() {
        return (if arg.is_empty() { "" } else { "/" }, false);
    }
    (t, t.len() != arg.len())
}

/// The trailing-slash rule for something that exists: a slash means "this is a folder".
/// `kind` is what is on disk (not following a final link). Err is the message.
pub fn check_slash(shown: &str, slash: bool, kind: Kind) -> Result<(), String> {
    if !slash {
        return Ok(());
    }
    match kind {
        Kind::Folder => Ok(()),
        Kind::Link => Err(format!(
            "{shown} is a link, not a folder (leave off the slash to mean the link)"
        )),
        k => Err(format!("{shown} is a {}, not a folder", k.word())),
    }
}

/// `p` made absolute against `cwd`, with `.`, `..` and repeated slashes resolved (by name:
/// links are not followed).
pub fn resolve(cwd: &str, p: &str) -> String {
    let full = if p.starts_with('/') {
        String::from(p)
    } else {
        format!("{cwd}/{p}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for s in full.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        return String::from("/");
    }
    let mut out = String::new();
    for s in parts {
        out.push('/');
        out.push_str(s);
    }
    out
}

/// The last part of a path ("/" for the root).
pub fn basename(p: &str) -> &str {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        return "/";
    }
    t.rsplit('/').next().unwrap_or(t)
}

/// Everything before the last part: "." for a bare name, "/" for "/x".
pub fn parent(p: &str) -> String {
    let t = p.trim_end_matches('/');
    match t.rfind('/') {
        None => String::from("."),
        Some(0) => String::from("/"),
        Some(i) => String::from(&t[..i]),
    }
}

/// `dir` + "/" + `name`, without doubling the slash.
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Is `inner` the same as `outer` or somewhere inside it? (Both absolute and resolved.)
pub fn is_within(inner: &str, outer: &str) -> bool {
    if outer == "/" {
        return true;
    }
    inner == outer
        || inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The Unix plumbing at the top of the tree: Linux and POSIX programs expect `/dev`,
/// `/proc` and `/tmp` there, so they stay, but listings of `/` leave them out (`files list`
/// without `--all`, Files). Everything milkOS's own code uses lives under `/System`.
pub const HIDDEN_AT_ROOT: &[&str] = &["dev", "proc", "tmp"];

/// Does a listing of `dir` leave `name` out unless asked for everything? Dot files
/// everywhere, and [`HIDDEN_AT_ROOT`] in the root. `dir` is absolute (any spelling of the
/// root counts: `/`, `//`, `/.`, `/System/..`).
pub fn hidden(dir: &str, name: &str) -> bool {
    name.starts_with('.') || (HIDDEN_AT_ROOT.contains(&name) && resolve("/", dir) == "/")
}

/// A new name for `files rename` (a name, not a path). Err is the message.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(String::from("the new name is empty"));
    }
    if name.contains('/') {
        return Err(format!(
            "{name} is a path, not a name (to move something to another folder, use files move)"
        ));
    }
    if name == "." || name == ".." {
        return Err(format!("{name} can't be used as a name"));
    }
    if name.contains('\0') {
        return Err(String::from("a name can't contain a NUL byte"));
    }
    Ok(())
}

/// What is at a copy's or move's destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dest {
    Missing,
    Folder,
    /// A file, link or anything else that isn't a folder.
    NotFolder,
}

/// Where `files copy SRC DST` / `files move SRC DST` puts SRC: into DST when DST is a folder,
/// else at DST itself. A slash on DST means "a folder that is there". Err is the message.
pub fn landing(src: &str, dst: &str, dst_slash: bool, dest: Dest) -> Result<String, String> {
    match dest {
        Dest::Folder => Ok(join(dst, basename(src))),
        Dest::Missing if dst_slash => Err(format!("{dst} doesn't exist")),
        Dest::NotFolder if dst_slash => Err(format!("{dst} is not a folder")),
        Dest::Missing | Dest::NotFolder => Ok(String::from(dst)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_split() {
        assert_eq!(split_slash("notes.txt"), ("notes.txt", false));
        assert_eq!(split_slash("projects/"), ("projects", true));
        assert_eq!(split_slash("projects//"), ("projects", true));
        assert_eq!(split_slash("a/b/"), ("a/b", true));
        assert_eq!(split_slash("/"), ("/", false));
        assert_eq!(split_slash("//"), ("/", false));
        assert_eq!(split_slash(""), ("", false));
    }

    #[test]
    fn slash_rule() {
        assert!(check_slash("projects", false, Kind::Folder).is_ok());
        assert!(check_slash("projects", true, Kind::Folder).is_ok());
        assert!(check_slash("notes.txt", false, Kind::File).is_ok());
        assert_eq!(
            check_slash("notes.txt", true, Kind::File).unwrap_err(),
            "notes.txt is a file, not a folder"
        );
        assert!(
            check_slash("ln", true, Kind::Link)
                .unwrap_err()
                .starts_with("ln is a link, not a folder")
        );
        assert!(check_slash("tty", true, Kind::Special).is_err());
    }

    #[test]
    fn resolving() {
        assert_eq!(resolve("/Users/milk", "notes.txt"), "/Users/milk/notes.txt");
        assert_eq!(resolve("/Users/milk", "./a/../b//c/"), "/Users/milk/b/c");
        assert_eq!(resolve("/Users/milk", "/tmp/x"), "/tmp/x");
        assert_eq!(resolve("/", ".."), "/");
        assert_eq!(resolve("/a", "../../.."), "/");
        assert_eq!(resolve("/a/b", ".."), "/a");
    }

    #[test]
    fn names() {
        assert_eq!(basename("/Users/milk/notes.txt"), "notes.txt");
        assert_eq!(basename("projects/"), "projects");
        assert_eq!(basename("notes.txt"), "notes.txt");
        assert_eq!(basename("/"), "/");
        assert_eq!(parent("/Users/milk/notes.txt"), "/Users/milk");
        assert_eq!(parent("/notes.txt"), "/");
        assert_eq!(parent("notes.txt"), ".");
        assert_eq!(parent("a/b/"), "a");
        assert_eq!(join("/", "x"), "/x");
        assert_eq!(join("/a", "x"), "/a/x");
        assert!(check_name("new name.txt").is_ok());
        assert!(check_name("a/b").unwrap_err().contains("files move"));
        assert!(check_name("..").is_err());
        assert!(check_name("").is_err());
    }

    #[test]
    fn hiding() {
        assert!(hidden("/", "dev") && hidden("/", "proc") && hidden("/", "tmp"));
        assert!(hidden("//", "tmp") && hidden("/.", "proc") && hidden("/System/..", "dev"));
        assert!(!hidden("/", "System") && !hidden("/", "Apps") && !hidden("/", "Volumes"));
        assert!(!hidden("/Users/milk", "tmp") && !hidden("/System", "dev"));
        assert!(hidden("/Users/milk", ".Trash") && hidden("/", ".milkos"));
    }

    #[test]
    fn within() {
        assert!(is_within("/a/b", "/a"));
        assert!(is_within("/a", "/a"));
        assert!(!is_within("/ab", "/a"));
        assert!(!is_within("/a", "/a/b"));
        assert!(is_within("/anything", "/"));
    }

    #[test]
    fn landings() {
        assert_eq!(
            landing("notes.txt", "backup", false, Dest::Folder).unwrap(),
            "backup/notes.txt"
        );
        assert_eq!(
            landing("/x/projects", "backup/", true, Dest::Folder).unwrap(),
            "backup/projects"
        );
        assert_eq!(
            landing("notes.txt", "new.txt", false, Dest::Missing).unwrap(),
            "new.txt"
        );
        assert_eq!(
            landing("notes.txt", "old.txt", false, Dest::NotFolder).unwrap(),
            "old.txt"
        );
        assert_eq!(
            landing("notes.txt", "backup", true, Dest::Missing).unwrap_err(),
            "backup doesn't exist"
        );
        assert_eq!(
            landing("notes.txt", "old.txt", true, Dest::NotFolder).unwrap_err(),
            "old.txt is not a folder"
        );
    }

    #[test]
    fn kinds() {
        assert_eq!(Kind::from_mode(0o100644), Kind::File);
        assert_eq!(Kind::from_mode(0o040755), Kind::Folder);
        assert_eq!(Kind::from_mode(0o120777), Kind::Link);
        assert_eq!(Kind::from_mode(0o020666), Kind::Special);
        assert_eq!(Kind::Folder.word(), "folder");
    }
}
