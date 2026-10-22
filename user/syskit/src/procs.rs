//! Processes: one `/proc/<pid>/status` as a row, the row as `processes list` prints it, and
//! picking the process a NAME or PID means.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub sid: u32,
    pub tid: u32,
    /// 'R', 'S', 'Z' ...
    pub state: char,
    /// "running", "sleeping", "zombie" (the kernel's word after the letter).
    pub state_word: String,
    pub name: String,
    /// real, effective
    pub uid: (u32, u32),
    pub gid: (u32, u32),
    pub groups: Vec<u32>,
    /// The capability line (`all`, or `fs=... net=...`).
    pub caps: String,
    /// "admin ticket from uid 501, 280 s left", if it has one.
    pub ticket: String,
    pub rss_kb: u64,
    pub threads: u32,
    pub fds: u32,
    pub cpu_ns: u64,
    /// The CPU it runs on or last ran on.
    pub core: u32,
    pub start_ns: u64,
}

fn num<T: core::str::FromStr + Default>(v: &str) -> T {
    v.split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_default()
}

fn pair(v: &str) -> (u32, u32) {
    let mut w = v.split_whitespace().map(|n| n.parse().unwrap_or(0));
    let r = w.next().unwrap_or(0);
    (r, w.next().unwrap_or(r))
}

/// One `/proc/<pid>/status`. `None` without a `Pid:` line.
pub fn parse_status(text: &str) -> Option<Row> {
    let mut r = Row::default();
    let mut got = false;
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k {
            "Name" => r.name = v.to_string(),
            "State" => {
                r.state = v.chars().next().unwrap_or('?');
                r.state_word = v
                    .split_once('(')
                    .and_then(|(_, w)| w.strip_suffix(')'))
                    .unwrap_or("")
                    .to_string();
            }
            "Pid" => {
                r.pid = v.parse().ok()?;
                got = true;
            }
            "PPid" => r.ppid = num(v),
            "Pgid" => r.pgid = num(v),
            "Sid" => r.sid = num(v),
            "Tid" => r.tid = num(v),
            "Uid" => r.uid = pair(v),
            "Gid" => r.gid = pair(v),
            "Groups" => {
                r.groups = v
                    .split_whitespace()
                    .filter_map(|g| g.parse().ok())
                    .collect()
            }
            "Caps" => r.caps = v.to_string(),
            "Auth" => r.ticket = v.to_string(),
            "VmRSS" => r.rss_kb = num(v),
            "Threads" => r.threads = num(v),
            "FDSize" => r.fds = num(v),
            "CpuNs" => r.cpu_ns = num(v),
            "Cpu" => r.core = num(v),
            "StartNs" => r.start_ns = num(v),
            _ => {}
        }
    }
    if r.state_word.is_empty() {
        r.state_word = String::from(state_word(r.state));
    }
    got.then_some(r)
}

/// The kernel's own threads from /proc/threads (`TID PID S PRIO PSR CPU_NS NAME`, pid 0).
pub fn parse_threads(text: &str) -> Vec<Row> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 7 || f[1] != "0" {
                return None;
            }
            let state = f[2].chars().next().unwrap_or('?');
            Some(Row {
                tid: f[0].parse().unwrap_or(0),
                state,
                state_word: String::from(state_word(state)),
                core: f[4].parse().unwrap_or(0),
                cpu_ns: f[5].parse().unwrap_or(0),
                name: f[6].to_string(),
                ..Row::default()
            })
        })
        .collect()
}

/// The word for a state letter.
pub fn state_word(c: char) -> &'static str {
    match c {
        'R' => "running",
        'S' => "sleeping",
        'D' => "waiting",
        'T' => "stopped",
        'Z' => "zombie",
        _ => "unknown",
    }
}

/// CPU time as "12.34s" (seconds and hundredths).
pub fn cpu_time(ns: u64) -> String {
    format!("{}.{:02}s", ns / 1_000_000_000, ns / 10_000_000 % 100)
}

/// The header of `processes list`.
pub const HEADER: &str = "  PID PARENT GROUP THREAD STATE    CORE   CPU TIME   MEMORY  NAME";

/// One line of `processes list` (a kernel thread has no pid, parent, group or memory).
pub fn line(r: &Row, kernel: bool) -> String {
    let dash = |v: String| if kernel { String::from("-") } else { v };
    format!(
        "{:>5} {:>6} {:>5} {:>6} {:<8} {:>4} {:>10} {:>8}  {}",
        dash(format!("{}", r.pid)),
        dash(format!("{}", r.ppid)),
        dash(format!("{}", r.pgid)),
        r.tid,
        r.state_word,
        r.core,
        cpu_time(r.cpu_ns),
        dash(format!("{}K", r.rss_kb)),
        if kernel {
            format!("[{}]", r.name)
        } else {
            r.name.clone()
        }
    )
}

/// What NAME|PID means among `rows`: the PIDs to act on, or why not (in plain English).
/// A number is a PID (whether or not it is in `rows`: the kernel has the last word). A name
/// must match exactly; when several processes have it, `all` takes them all, otherwise the
/// answer lists their PIDs. `me` (the caller's own pid) is never picked by name. `verb` is
/// the subcommand, for the hint ("stop", "signal", "info").
pub fn pick(rows: &[Row], what: &str, all: bool, me: u32, verb: &str) -> Result<Vec<u32>, String> {
    if let Ok(pid) = what.parse::<u32>() {
        return Ok(alloc::vec![pid]);
    }
    let hits: Vec<u32> = rows
        .iter()
        .filter(|r| r.name == what && r.pid != me)
        .map(|r| r.pid)
        .collect();
    match hits.len() {
        0 => Err(format!(
            "there's no process called {what} (processes list shows them all)"
        )),
        1 => Ok(hits),
        _ if all => Ok(hits),
        n => {
            let pids: Vec<String> = hits.iter().map(|p| format!("{p}")).collect();
            let more = if verb == "info" {
                ""
            } else {
                ", or add --all for every one"
            };
            Err(format!(
                "{n} processes are called {what} (PIDs {}): say which with processes {verb} PID{more}",
                pids.join(", ")
            ))
        }
    }
}

/// Why `kill` failed for `pid`, from its errno: `doing` is "stop" or "signal".
pub fn say_kill(pid: u32, name: &str, errno: i32, doing: &str) -> String {
    let who = if name.is_empty() {
        format!("process {pid}")
    } else {
        format!("{name} (PID {pid})")
    };
    match errno {
        3 => format!("there's no process {pid}"),
        1 | 13 => format!("you don't have permission to {doing} {who}"),
        22 => format!("{who}: that signal can't be sent"),
        e => format!("{who}: failed (error {e})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MSH: &str = "Name:\tmsh\nState:\tS (sleeping)\nTgid:\t12\nPid:\t12\nPPid:\t1\nPgid:\t12\nSid:\t12\n\
Uid:\t501\t0\t0\t0\nGid:\t20\t20\t20\t20\nGroups:\t20 80\nCaps:\tall\n\
Auth:\tadmin ticket from uid 501, 280 s left\nFDSize:\t4\nVmRSS:\t312 kB\nThreads:\t1\n\
Tid:\t12\nCpuNs:\t1234567890\nCpu:\t2\nStartNs:\t5000\n";

    fn row(pid: u32, name: &str) -> Row {
        Row {
            pid,
            name: String::from(name),
            ..Row::default()
        }
    }

    #[test]
    fn status() {
        let r = parse_status(MSH).unwrap();
        assert_eq!((r.pid, r.ppid, r.pgid, r.sid, r.tid), (12, 1, 12, 12, 12));
        assert_eq!((r.state, r.state_word.as_str()), ('S', "sleeping"));
        assert_eq!(r.uid, (501, 0));
        assert_eq!(r.gid, (20, 20));
        assert_eq!(r.groups, [20, 80]);
        assert_eq!(r.caps, "all");
        assert_eq!(r.ticket, "admin ticket from uid 501, 280 s left");
        assert_eq!((r.rss_kb, r.threads, r.fds, r.core), (312, 1, 4, 2));
        assert_eq!(r.cpu_ns, 1_234_567_890);
        assert!(parse_status("Name:\tx\n").is_none());
        let z = parse_status("Pid:\t3\nState:\tZ\n").unwrap();
        assert_eq!(z.state_word, "zombie");
    }

    #[test]
    fn lines() {
        let r = parse_status(MSH).unwrap();
        assert_eq!(cpu_time(1_234_567_890), "1.23s");
        assert_eq!(
            line(&r, false),
            "   12      1    12     12 sleeping    2      1.23s     312K  msh"
        );
        let k = parse_threads(
            "  TID   PID S PRIO PSR        CPU_NS NAME\n    3     0 S    0   1          5000 idle1\n   40    12 R    1   0           100 msh\n",
        );
        assert_eq!(k.len(), 1);
        assert_eq!((k[0].tid, k[0].core, k[0].cpu_ns), (3, 1, 5000));
        assert_eq!(
            line(&k[0], true),
            "    -      -     -      3 sleeping    1      0.00s        -  [idle1]"
        );
        assert_eq!(HEADER.find("NAME"), line(&r, false).find("msh"));
    }

    #[test]
    fn picking() {
        let rows = [
            row(1, "init"),
            row(12, "msh"),
            row(15, "msh"),
            row(20, "processes"),
        ];
        assert_eq!(pick(&rows, "init", false, 20, "stop"), Ok(alloc::vec![1]));
        assert_eq!(pick(&rows, "99", false, 20, "stop"), Ok(alloc::vec![99]));
        assert_eq!(
            pick(&rows, "msh", true, 20, "stop"),
            Ok(alloc::vec![12, 15])
        );
        assert_eq!(
            pick(&rows, "msh", false, 20, "stop"),
            Err(String::from(
                "2 processes are called msh (PIDs 12, 15): say which with processes stop PID, or add --all for every one"
            ))
        );
        assert_eq!(
            pick(&rows, "msh", false, 20, "info"),
            Err(String::from(
                "2 processes are called msh (PIDs 12, 15): say which with processes info PID"
            ))
        );
        assert_eq!(
            pick(&rows, "processes", false, 20, "stop"),
            Err(String::from(
                "there's no process called processes (processes list shows them all)"
            ))
        );
        assert!(pick(&rows, "ms", false, 20, "stop").is_err());
    }

    #[test]
    fn kill_words() {
        assert_eq!(say_kill(7, "", 3, "stop"), "there's no process 7");
        assert_eq!(
            say_kill(1, "init", 1, "stop"),
            "you don't have permission to stop init (PID 1)"
        );
        assert_eq!(
            say_kill(9, "x", 99, "signal"),
            "x (PID 9): failed (error 99)"
        );
    }
}
