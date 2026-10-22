//! The audit log in plain English: parse a `/proc/audit` line, decide what to call it, how to
//! colour it and which filter chip it belongs to. Unknown events show as their raw name.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub seq: u64,
    pub time_ns: u64,
    pub pid: u32,
    pub exe: String,
    pub event: String,
    /// The detail with %-escapes decoded.
    pub detail: String,
    pub line: String,
}

fn unescape(s: &str) -> String {
    if s == "-" {
        return String::new();
    }
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut k = 0;
    while k < b.len() {
        if b[k] == b'%'
            && k + 3 <= b.len()
            && let Some(v) = core::str::from_utf8(&b[k + 1..k + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            k += 3;
            continue;
        }
        out.push(b[k]);
        k += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse one record line: `seq time_ns pid exe event detail prev_hash hash`.
pub fn parse_record(line: &str) -> Option<Record> {
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() < 8 {
        return None;
    }
    Some(Record {
        seq: f[0].parse().ok()?,
        time_ns: f[1].parse().ok()?,
        pid: f[2].parse().ok()?,
        exe: unescape(f[3]),
        event: unescape(f[4]),
        detail: unescape(f[5]),
        line: line.to_string(),
    })
}

/// Parse the whole log, newest last. Lines that do not parse are skipped.
pub fn parse_log(text: &str) -> Vec<Record> {
    text.lines().filter_map(parse_record).collect()
}

/// The value of `key=` in a detail string (details are `k=v k2=v2`, values may contain spaces,
/// so a value runs until the next ` word=`).
pub fn kv<'a>(detail: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("{key}=");
    let start = if detail.starts_with(&pat) {
        0
    } else {
        detail.find(&format!(" {pat}"))? + 1
    };
    let rest = &detail[start + pat.len()..];
    let mut end = rest.len();
    for (i, _) in rest.match_indices(' ') {
        let tail = &rest[i + 1..];
        if let Some(eq) = tail.find('=')
            && eq > 0
            && tail[..eq]
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            end = i;
            break;
        }
    }
    Some(&rest[..end])
}

fn base(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dot {
    Danger,
    Good,
    Info,
    Trust,
    Plain,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    All,
    Refused,
    Files,
    Programs,
    Installs,
    Drop,
}

pub const FILTERS: [(Filter, &str); 6] = [
    (Filter::All, "All"),
    (Filter::Refused, "Refused"),
    (Filter::Files, "Files"),
    (Filter::Programs, "Programs"),
    (Filter::Installs, "Installs"),
    (Filter::Drop, "Milk Drop"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Described {
    pub sentence: String,
    pub detail: String,
    pub dot: Dot,
    pub filters: &'static [Filter],
}

/// What a record says, for a person.
pub fn describe(r: &Record) -> Described {
    use Filter::*;
    let app = if r.exe.is_empty() || r.exe == "[kernel]" {
        "The system"
    } else {
        base(&r.exe)
    };
    let d = r.detail.as_str();
    let who = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| kv(d, k))
            .map(|s| base(s).to_string())
    };
    let (sentence, detail, dot, filters): (String, String, Dot, &'static [Filter]) =
        match r.event.as_str() {
            "sandbox.deny" => {
                let what = kv(d, "path")
                    .or(kv(d, "addr"))
                    .map_or(d.to_string(), |s| s.to_string());
                let op = kv(d, "op").unwrap_or("access");
                let s = match op {
                    "connect" | "socket" | "sendto" | "net" => {
                        format!("{app} was refused a network connection")
                    }
                    "fork" | "spawn" => format!("{app} was refused starting a program"),
                    _ => format!("{app} was refused {what}"),
                };
                (s, d.to_string(), Dot::Danger, &[Refused, Files][..])
            }
            "powerbox.grant" => (
                format!("You gave {app} a file"),
                kv(d, "path").unwrap_or(d).to_string(),
                Dot::Good,
                &[Files][..],
            ),
            "powerbox.cancel" => (
                format!("You declined to share a file with {app}"),
                d.to_string(),
                Dot::Plain,
                &[Files][..],
            ),
            "sandbox.enter" => {
                let name = who(&["exe"]).unwrap_or_else(|| app.to_string());
                (
                    format!("{name} started, sandboxed"),
                    kv(d, "spec").unwrap_or(d).to_string(),
                    Dot::Good,
                    &[Programs][..],
                )
            }
            "exec" => {
                let argv = kv(d, "argv").unwrap_or("");
                let prog = argv.split_whitespace().next().map_or(base(&r.exe), base);
                (
                    format!("{prog} was started"),
                    argv.to_string(),
                    Dot::Plain,
                    &[Programs][..],
                )
            }
            e if e.starts_with("user.pkg.") => {
                let name = who(&["name", "pkg", "package"])
                    .or_else(|| d.split_whitespace().next().map(base).map(str::to_string))
                    .unwrap_or_default();
                let verb = match &e[9..] {
                    "install" => "was installed",
                    "remove" => "was removed",
                    "try" => "was tried",
                    "keep" => "was kept",
                    "discard" => "was discarded",
                    "upgrade" => "was upgraded",
                    "verify_fail" => "failed its signature check",
                    other => other,
                };
                let danger = e.ends_with("verify_fail");
                (
                    format!(
                        "{} {verb}",
                        if name.is_empty() {
                            "A package".to_string()
                        } else {
                            name
                        }
                    ),
                    d.to_string(),
                    if danger { Dot::Danger } else { Dot::Info },
                    if danger {
                        &[Installs, Refused][..]
                    } else {
                        &[Installs][..]
                    },
                )
            }
            "user.drop.recv" | "user.drop.send" => {
                let file = who(&["file", "name", "path"]).unwrap_or_default();
                let peer = who(&["peer", "from", "to", "who"]).unwrap_or_default();
                let (verb, prep) = if r.event.ends_with("recv") {
                    ("received", "from")
                } else {
                    ("sent", "to")
                };
                let mut s = format!(
                    "Milk Drop {verb} {}",
                    if file.is_empty() {
                        "a file".to_string()
                    } else {
                        file
                    }
                );
                if !peer.is_empty() {
                    s.push_str(&format!(" {prep} {peer}"));
                }
                (s, d.to_string(), Dot::Info, &[Drop, Files][..])
            }
            "user.drop.trust" => {
                let peer = who(&["peer", "name", "who"]).unwrap_or_else(|| "A device".to_string());
                (
                    format!("{peer}'s key was trusted"),
                    d.to_string(),
                    Dot::Trust,
                    &[Drop][..],
                )
            }
            "user.drop.key_changed" => {
                let peer = who(&["peer", "name", "who"]).unwrap_or_else(|| "A device".to_string());
                (
                    format!("{peer}'s key changed"),
                    d.to_string(),
                    Dot::Danger,
                    &[Drop, Refused][..],
                )
            }
            "audit.tamper" => (
                "The log has been changed".to_string(),
                format!("first bad record: {d}"),
                Dot::Danger,
                &[Refused][..],
            ),
            "audit.boot" => (
                "milkOS started".to_string(),
                d.to_string(),
                Dot::Plain,
                &[][..],
            ),
            "fs.mount" => (
                format!("{} was mounted", kv(d, "path").unwrap_or(d)),
                d.to_string(),
                Dot::Plain,
                &[Files][..],
            ),
            "fs.unmount" => (
                format!("{} was unmounted", kv(d, "path").unwrap_or(d)),
                d.to_string(),
                Dot::Plain,
                &[Files][..],
            ),
            e if e.starts_with("milkfs.snapshot") => (
                "A snapshot was taken".to_string(),
                d.to_string(),
                Dot::Info,
                &[Files][..],
            ),
            e if e.starts_with("milkfs.rollback") => (
                "The disk was rolled back".to_string(),
                d.to_string(),
                Dot::Trust,
                &[Files][..],
            ),
            "net.connect" => (
                format!(
                    "{app} connected to {}",
                    kv(d, "addr").or(kv(d, "to")).unwrap_or(d)
                ),
                d.to_string(),
                Dot::Plain,
                &[][..],
            ),
            "net.listen" => (
                format!("{app} started listening"),
                d.to_string(),
                Dot::Plain,
                &[][..],
            ),
            "audio.open" => (
                format!("{app} used the speakers"),
                d.to_string(),
                Dot::Plain,
                &[][..],
            ),
            other => (other.to_string(), d.to_string(), Dot::Plain, &[][..]),
        };
    Described {
        sentence,
        detail,
        dot,
        filters,
    }
}

/// What `audit verify` said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub ok: bool,
    /// Records in the checked log.
    pub records: u64,
    /// When broken: the first bad record and why.
    pub broken_at: Option<(u64, String)>,
}

/// Read the output of `audit verify` ("audit verify: PATH: OK (42 records, ...)" or "... BROKEN at
/// seq 7: why"), one line per log checked. Broken anywhere is broken; otherwise the largest log.
pub fn parse_verify(text: &str) -> Option<Verdict> {
    let mut v: Option<Verdict> = None;
    for l in text.lines() {
        let Some(rest) = l.strip_prefix("audit verify: ") else {
            continue;
        };
        let Some((_, res)) = rest.split_once(": ") else {
            continue;
        };
        if let Some(b) = res.strip_prefix("BROKEN at seq ") {
            let (seq, why) = b.split_once(": ").unwrap_or((b, ""));
            return Some(Verdict {
                ok: false,
                records: v.as_ref().map_or(0, |x| x.records),
                broken_at: Some((seq.trim().parse().unwrap_or(0), why.to_string())),
            });
        }
        if let Some(ok) = res.strip_prefix("OK (") {
            let n = ok
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            if v.as_ref().is_none_or(|x| n > x.records) {
                v = Some(Verdict {
                    ok: true,
                    records: n,
                    broken_at: None,
                });
            }
        }
    }
    v
}

/// Does the record belong under this chip?
pub fn matches(r: &Record, f: Filter) -> bool {
    f == Filter::All || describe(r).filters.contains(&f)
}

/// Does a search string appear in the sentence, the detail or the raw event name?
pub fn search_hit(r: &Record, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    let q = q.to_lowercase();
    let d = describe(r);
    d.sentence.to_lowercase().contains(&q)
        || r.detail.to_lowercase().contains(&q)
        || r.event.to_lowercase().contains(&q)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(event: &str, exe: &str, detail: &str) -> Record {
        let line = format!(
            "7 1792589641138472000 16 {exe} {event} {detail} {} {}",
            "0".repeat(64),
            "1".repeat(64)
        );
        parse_record(&line).unwrap()
    }

    #[test]
    fn parses_and_unescapes() {
        let r = parse_record(
            "65 1792589641138472000 16 /Commands/text exec ppid=10%20argv=text%20show%20/proc/meminfo e4a6 1c73",
        )
        .unwrap_or_else(|| rec("exec", "/Commands/text", "ppid=10%20argv=text%20show%20/proc/meminfo"));
        assert_eq!(r.event, "exec");
        assert_eq!(r.detail, "ppid=10 argv=text show /proc/meminfo");
        assert_eq!(kv(&r.detail, "argv"), Some("text show /proc/meminfo"));
        assert_eq!(kv(&r.detail, "ppid"), Some("10"));
        assert_eq!(kv(&r.detail, "nope"), None);
        assert!(parse_record("garbage").is_none());
        assert_eq!(parse_log("garbage\n").len(), 0);
    }

    #[test]
    fn kv_values_may_hold_spaces_and_equals() {
        let d = "op=open path=/Users/milk/My%20File=1 need=r";
        assert_eq!(kv(d, "op"), Some("open"));
        assert_eq!(kv(d, "need"), Some("r"));
        let d2 = "op=open path=/a b/c need=r";
        assert_eq!(kv(d2, "path"), Some("/a b/c"));
    }

    #[test]
    fn the_spec_table() {
        let d = describe(&rec(
            "sandbox.deny",
            "/Apps/peek/peek",
            "op=open%20path=/System/Settings/passwd%20need=r",
        ));
        assert_eq!(d.sentence, "peek was refused /System/Settings/passwd");
        assert_eq!(d.dot, Dot::Danger);
        assert!(d.filters.contains(&Filter::Refused));
        let d = describe(&rec(
            "powerbox.grant",
            "/Apps/peek/peek",
            "pid=5%20path=/Users/milk/a.txt%20mode=r",
        ));
        assert_eq!(d.sentence, "You gave peek a file");
        assert_eq!(d.detail, "/Users/milk/a.txt");
        assert_eq!(d.dot, Dot::Good);
        let d = describe(&rec(
            "sandbox.enter",
            "[kernel]",
            "pid=9%20exe=/Apps/moo2048/moo2048%20spec=fs-none",
        ));
        assert_eq!(d.sentence, "moo2048 started, sandboxed");
        let d = describe(&rec(
            "exec",
            "/Commands/text",
            "ppid=10%20argv=text%20show%20/proc/meminfo",
        ));
        assert_eq!(d.sentence, "text was started");
        assert_eq!(d.detail, "text show /proc/meminfo");
        let d = describe(&rec(
            "user.pkg.install",
            "/Commands/apps",
            "name=fortune%20version=1",
        ));
        assert_eq!(d.sentence, "fortune was installed");
        assert_eq!(d.dot, Dot::Info);
        let d = describe(&rec(
            "user.drop.recv",
            "/Commands/drop",
            "file=bike.png%20from=alice",
        ));
        assert_eq!(d.sentence, "Milk Drop received bike.png from alice");
        let d = describe(&rec("user.drop.trust", "/Commands/drop", "peer=bob"));
        assert_eq!(
            (d.sentence.as_str(), d.dot),
            ("bob's key was trusted", Dot::Trust)
        );
        let d = describe(&rec("user.drop.key_changed", "/Commands/drop", "peer=bob"));
        assert_eq!(
            (d.sentence.as_str(), d.dot),
            ("bob's key changed", Dot::Danger)
        );
        let d = describe(&rec("audit.tamper", "[kernel]", "12"));
        assert_eq!(
            (d.sentence.as_str(), d.dot),
            ("The log has been changed", Dot::Danger)
        );
    }

    #[test]
    fn unknown_events_show_their_name() {
        let d = describe(&rec("weird.thing", "/Commands/x", "a=b"));
        assert_eq!(d.sentence, "weird.thing");
        assert_eq!(d.dot, Dot::Plain);
        assert!(d.filters.is_empty());
    }

    #[test]
    fn filters_and_search() {
        let deny = rec(
            "sandbox.deny",
            "/Apps/peek/peek",
            "op=open%20path=/System/Settings/passwd",
        );
        let exec = rec("exec", "/Commands/text", "argv=text%20show%20x");
        assert!(
            matches(&deny, Filter::All)
                && matches(&deny, Filter::Refused)
                && matches(&deny, Filter::Files)
        );
        assert!(!matches(&deny, Filter::Programs));
        assert!(matches(&exec, Filter::Programs) && !matches(&exec, Filter::Refused));
        assert!(
            search_hit(&deny, "PASSWD")
                && search_hit(&deny, "sandbox.deny")
                && search_hit(&deny, "")
        );
        assert!(!search_hit(&exec, "passwd"));
    }

    #[test]
    fn verify_output() {
        let ok = "audit verify: /proc/audit: OK (42 records, seq 37..78, head 3f36)\naudit verify: /Volumes/Milk/var/log/audit.log: OK (78 records, seq 1..78, head 3f36)\n";
        assert_eq!(
            parse_verify(ok),
            Some(Verdict {
                ok: true,
                records: 78,
                broken_at: None
            })
        );
        let bad = "audit verify: /proc/audit: OK (42 records, seq 37..78, head 3f36)\naudit verify: /Volumes/Milk/var/log/audit.log: BROKEN at seq 12: hash mismatch\n";
        let v = parse_verify(bad).unwrap();
        assert!(!v.ok);
        assert_eq!(v.broken_at, Some((12, "hash mismatch".to_string())));
        assert_eq!(parse_verify("audit verify: /x: cannot read\n"), None);
        assert_eq!(parse_verify(""), None);
    }

    #[test]
    fn network_denials_read_naturally() {
        let d = describe(&rec(
            "sandbox.deny",
            "/Apps/peek/peek",
            "op=connect%20addr=10.0.2.2:80",
        ));
        assert_eq!(d.sentence, "peek was refused a network connection");
    }
}
