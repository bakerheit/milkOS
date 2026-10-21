//! Backups: `snap send` to a stream file on the Mac share and `snap receive` from one.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Where backups go on the share.
pub const DIR: &str = "/Volumes/Mac/backups";

/// The stream file for a snapshot: `name.stream`, or `name-from-older.stream` when incremental.
pub fn file_name(snap: &str, from: Option<&str>) -> String {
    match from {
        Some(f) => format!("{snap}-from-{f}.stream"),
        None => format!("{snap}.stream"),
    }
}

pub fn path(snap: &str, from: Option<&str>) -> String {
    format!("{DIR}/{}", file_name(snap, from))
}

/// The text for the `send` control.
pub fn send_request(snap: &str, from: Option<&str>, out: &str) -> String {
    match from {
        Some(f) => format!("{snap} {f} {out}"),
        None => format!("{snap} {out}"),
    }
}

/// The text for the `receive` control.
pub fn receive_request(stream: &str, target: Option<&str>, force: bool) -> String {
    let mut s = stream.to_string();
    if let Some(t) = target {
        s.push(' ');
        s.push_str(t);
    }
    if force {
        s.push_str(" force");
    }
    s
}

/// A backup passphrase is chosen like any other: at least 8 characters, one line, typed twice.
pub fn check_backup_passphrase(pass: &str, again: &str) -> Result<(), &'static str> {
    crate::fsinfo::check_passphrase(pass, again)
}

/// The text for the `send` control of a system that can encrypt backups: `SNAP [FROM] PATH`, then
/// ` plaintext` for an unencrypted one, or a second line holding the passphrase for an encrypted one.
/// (A plain volume's backup needs neither.)
pub fn send_request_with(snap: &str, from: Option<&str>, out: &str, how: Protect) -> String {
    let mut s = send_request(snap, from, out);
    match how {
        Protect::None => {}
        Protect::Plaintext => s.push_str(" plaintext"),
        Protect::Passphrase(p) => {
            s.push('\n');
            s.push_str(p);
        }
    }
    s
}

/// How a backup of an encrypted volume is protected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protect<'a> {
    /// The volume is not encrypted: nothing to say.
    None,
    /// Written without encryption, on purpose.
    Plaintext,
    /// Encrypted under this passphrase.
    Passphrase(&'a str),
}

/// `receive` with the passphrase of an encrypted backup on a second line (empty: none).
pub fn receive_request_with(
    stream: &str,
    target: Option<&str>,
    force: bool,
    passphrase: &str,
) -> String {
    let mut s = receive_request(stream, target, force);
    if !passphrase.is_empty() {
        s.push('\n');
        s.push_str(passphrase);
    }
    s
}

/// Which directory entries are backups.
pub fn streams(names: &[String]) -> Vec<&String> {
    let mut v: Vec<&String> = names.iter().filter(|n| n.ends_with(".stream")).collect();
    v.sort();
    v
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub ok: bool,
    /// The kernel's own sentence.
    pub text: String,
}

/// Read back `send` or `receive`'s last report: "sent ...", "received ..." or "... failed: ...".
pub fn parse_report(text: &str) -> Option<Report> {
    let t = text.lines().next()?.trim();
    if t.is_empty() {
        return None;
    }
    Some(Report {
        ok: !t.contains(" failed: "),
        text: t.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_requests() {
        assert_eq!(file_name("monday", None), "monday.stream");
        assert_eq!(
            path("tuesday", Some("monday")),
            "/Volumes/Mac/backups/tuesday-from-monday.stream"
        );
        assert_eq!(
            send_request("monday", None, "/x/m.stream"),
            "monday /x/m.stream"
        );
        assert_eq!(
            send_request("tuesday", Some("monday"), "/x/t.stream"),
            "tuesday monday /x/t.stream"
        );
        assert_eq!(receive_request("/x/m.stream", None, false), "/x/m.stream");
        assert_eq!(
            receive_request("/x/m.stream", Some("/Volumes/Milk"), true),
            "/x/m.stream /Volumes/Milk force"
        );
    }

    #[test]
    fn protected_requests() {
        assert_eq!(
            send_request_with("m", None, "/x/m.stream", Protect::None),
            "m /x/m.stream"
        );
        assert_eq!(
            send_request_with("m", Some("a"), "/x/m.stream", Protect::Plaintext),
            "m a /x/m.stream plaintext"
        );
        assert_eq!(
            send_request_with(
                "m",
                None,
                "/x/m.stream",
                Protect::Passphrase("correct horse")
            ),
            "m /x/m.stream\ncorrect horse"
        );
        assert_eq!(
            receive_request_with("/x/m.stream", None, false, ""),
            "/x/m.stream"
        );
        assert_eq!(
            receive_request_with("/x/m.stream", Some("/V"), true, "pass word"),
            "/x/m.stream /V force\npass word"
        );
        assert!(check_backup_passphrase("longenough", "longenough").is_ok());
        assert!(check_backup_passphrase("short", "short").is_err());
    }

    #[test]
    fn listing_and_reports() {
        let names = alloc::vec![
            "b.stream".to_string(),
            "notes.txt".to_string(),
            "a.stream".to_string()
        ];
        assert_eq!(streams(&names), ["a.stream", "b.stream"]);
        let ok = parse_report("sent monday to /Volumes/Mac/backups/monday.stream: 120 records, 40 data blocks, 163840 bytes in 53 ms\n").unwrap();
        assert!(ok.ok);
        let bad = parse_report("send monday failed: EBUSY\n").unwrap();
        assert!(!bad.ok);
        assert_eq!(parse_report(""), None);
        let rx = parse_report(
            "received tuesday (incremental) from /x: 5 items, 2 data blocks, 8192 bytes in 4 ms\n",
        )
        .unwrap();
        assert!(rx.ok);
    }
}
