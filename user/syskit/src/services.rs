//! milkd's control socket as `services` talks to it: one request line, then status lines
//! (`unit state pid restarts uptime_s user detail description warnings`, tab separated) and
//! a last line `ok` or `error<TAB>why`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const SOCK: &str = "/System/Services/Sockets/milkd.sock";

/// Has the whole reply arrived?
pub fn complete(text: &str) -> bool {
    text.ends_with("ok\n")
        || (text.ends_with('\n') && text.lines().last().is_some_and(|l| l.starts_with("error")))
}

/// The reply's lines (without `ok`), or milkd's reason.
pub fn reply(text: &str) -> Result<Vec<String>, String> {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    match lines.pop() {
        Some(l) if l == "ok" => Ok(lines),
        Some(l) => Err(String::from(l.strip_prefix("error\t").unwrap_or(&l))),
        None => Err(String::from("milkd didn't answer")),
    }
}

/// milkd's reason in plain English. `verb` is what was asked (start, stop, ...).
pub fn say(why: &str, verb: &str) -> String {
    if why.starts_with("permission denied") {
        return format!(
            "only an administrator can {verb} services (try admin services {verb} ...)"
        );
    }
    if let Some(n) = why.strip_prefix("no such unit ") {
        return format!("there's no service called {n} (services list shows them all)");
    }
    if let Some(n) = why.strip_suffix(" is not running") {
        return format!("{n} isn't running");
    }
    if why == "which unit?" {
        return String::from("which service?");
    }
    String::from(why)
}

/// One status line's nine fields.
pub fn fields(l: &str) -> [&str; 9] {
    let mut f = [""; 9];
    for (i, v) in l.splitn(9, '\t').enumerate() {
        f[i] = v;
    }
    f
}

pub const HEADER: &str = "NAME           STATE         PID RESTARTS  DESCRIPTION";

/// A `services list` line.
pub fn line(l: &str) -> String {
    let f = fields(l);
    let pid = if f[2] == "0" || f[2].is_empty() {
        "-"
    } else {
        f[2]
    };
    let mut st = String::from(f[1]);
    if matches!(f[1], "failed" | "skipped" | "restarting") && !f[6].is_empty() {
        st = format!("{} ({})", f[1], f[6]);
    }
    format!("{:<14} {:<11} {:>5} {:>8}  {}", f[0], st, pid, f[3], f[7])
}

/// `services status NAME`: one service in a few lines.
pub fn detail(l: &str) -> Vec<String> {
    let f = fields(l);
    let mut st = String::from(f[1]);
    if !f[6].is_empty() {
        st += &format!(" ({})", f[6]);
    }
    let up = f[4].parse::<u64>().ok();
    let rows = [
        ("state", st),
        (
            "process",
            if f[2] == "0" || f[2].is_empty() {
                String::new()
            } else {
                format!(
                    "PID {}, up {}",
                    f[2],
                    up.map_or(String::from("?"), crate::time::words)
                )
            },
        ),
        ("runs as", String::from(f[5])),
        ("restarts", String::from(f[3])),
        ("warning", String::from(f[8])),
    ];
    let mut out = alloc::vec![if f[7].is_empty() {
        String::from(f[0])
    } else {
        format!("{} - {}", f[0], f[7])
    }];
    out.extend(
        crate::fmt::pairs(&rows)
            .into_iter()
            .map(|l| format!("  {l}")),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const UP: &str = "alarmd\trunning\t12\t0\t3725\troot\t\tAlarms and timers\t";
    const BAD: &str = "drop\tfailed\t0\t5\t0\troot\texit 1\tMilk Drop\tunknown key colour";

    #[test]
    fn replies() {
        assert!(complete("a\tb\nok\n"));
        assert!(complete("error\tno such unit x\n"));
        assert!(!complete("a\tb\n"));
        assert!(!complete("error\tno su"));
        assert_eq!(
            reply("a\nb\nok\n"),
            Ok(alloc::vec![String::from("a"), String::from("b")])
        );
        assert_eq!(
            reply("error\tno such unit x\n"),
            Err(String::from("no such unit x"))
        );
        assert_eq!(reply(""), Err(String::from("milkd didn't answer")));
    }

    #[test]
    fn plain_english() {
        assert_eq!(
            say(
                "permission denied: start/stop need root or group admin",
                "stop"
            ),
            "only an administrator can stop services (try admin services stop ...)"
        );
        assert_eq!(
            say("no such unit nope", "status"),
            "there's no service called nope (services list shows them all)"
        );
        assert_eq!(say("httpd is not running", "signal"), "httpd isn't running");
        assert_eq!(say("which unit?", "start"), "which service?");
        assert_eq!(say("dependency failed: x", "start"), "dependency failed: x");
    }

    #[test]
    fn output() {
        assert_eq!(fields("a\tb")[1], "b");
        assert_eq!(fields("a\tb")[8], "");
        assert_eq!(
            line(UP),
            "alarmd         running        12        0  Alarms and timers"
        );
        assert_eq!(
            line(BAD),
            "drop           failed (exit 1)     -        5  Milk Drop"
        );
        assert_eq!(HEADER.find("DESCRIPTION"), line(UP).find("Alarms"));
        assert_eq!(
            detail(UP),
            [
                "alarmd - Alarms and timers",
                "  state     running",
                "  process   PID 12, up 1 hour 2 minutes",
                "  runs as   root",
                "  restarts  0"
            ]
        );
        let d = detail(BAD);
        assert_eq!(d[1], "  state     failed (exit 1)");
        assert_eq!(d.last().unwrap(), "  warning   unknown key colour");
        assert!(!d.iter().any(|l| l.contains("process")));
    }
}
