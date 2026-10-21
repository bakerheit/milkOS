//! Processes: `/proc/<pid>/status`, CPU percentages between two looks, sorting and filtering.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    /// 'R', 'S', 'D', 'Z' ...
    pub state: char,
    pub rss_kb: u64,
    pub threads: u32,
    /// CPU time used so far (all threads).
    pub cpu_ns: u64,
    pub start_ns: u64,
    /// The effective user id.
    pub uid: u32,
    /// The capability line: `all`, or `fs=<paths|all|none> net=<none|local|all> audio=0|1 ...`.
    pub caps: Option<String>,
    /// CPU use over the last interval in tenths of a percent (1000 = one whole core).
    pub cpu10: u32,
}

/// Parse one `/proc/<pid>/status`. `None` if it has no pid.
pub fn parse_status(text: &str) -> Option<Proc> {
    let mut p = Proc::default();
    let mut got_pid = false;
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k {
            "Name" => p.name = v.to_string(),
            "State" => p.state = v.chars().next().unwrap_or('?'),
            "Pid" => {
                p.pid = v.parse().ok()?;
                got_pid = true;
            }
            "PPid" => p.ppid = v.parse().unwrap_or(0),
            "VmRSS" => {
                p.rss_kb = v
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0)
            }
            "Threads" => p.threads = v.parse().unwrap_or(1),
            "CpuNs" => p.cpu_ns = v.parse().unwrap_or(0),
            "StartNs" => p.start_ns = v.parse().unwrap_or(0),
            "Caps" => p.caps = Some(v.to_string()),
            "Uid" => {
                p.uid = v
                    .split_whitespace()
                    .nth(1)
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0)
            }
            _ => {}
        }
    }
    got_pid.then_some(p)
}

/// Fill in `cpu10` for `now` from the previous look: the change in CPU time over the wall time
/// between the two looks, 1000 = one whole core. New processes get 0.
pub fn fill_cpu(now: &mut [Proc], prev: &[Proc], dt_ns: u64) {
    if dt_ns == 0 {
        return;
    }
    for p in now {
        p.cpu10 = prev
            .iter()
            .find(|o| o.pid == p.pid && o.start_ns == p.start_ns)
            .map_or(0, |o| {
                (p.cpu_ns.saturating_sub(o.cpu_ns) as u128 * 1000 / dt_ns as u128).min(100_000)
                    as u32
            });
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    Pid,
    State,
    Cpu,
    Memory,
    Threads,
}

/// Sort in place. `desc` reverses it. Ties break on pid so the order never jitters.
pub fn sort(v: &mut [Proc], key: SortKey, desc: bool) {
    v.sort_by(|a, b| {
        let o = match key {
            SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortKey::Pid => a.pid.cmp(&b.pid),
            SortKey::State => a.state.cmp(&b.state),
            SortKey::Cpu => a.cpu10.cmp(&b.cpu10),
            SortKey::Memory => a.rss_kb.cmp(&b.rss_kb),
            SortKey::Threads => a.threads.cmp(&b.threads),
        };
        let o = if desc { o.reverse() } else { o };
        o.then(a.pid.cmp(&b.pid))
    });
}

/// Keep the processes whose name contains `q` (case-insensitive); an empty query keeps all.
pub fn filter<'a>(v: &'a [Proc], q: &str) -> Vec<&'a Proc> {
    let q = q.to_lowercase();
    v.iter()
        .filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q))
        .collect()
}

/// Processes the Quit button must refuse: the window server, init and the shell the desktop
/// hangs off (pid 1 and anything whose name says it is the system).
pub fn is_system(p: &Proc) -> bool {
    p.pid <= 1 || matches!(p.name.as_str(), "init" | "wm" | "kernel")
}

/// "Sandboxed" when the capability line is anything but `all` (a restricted set of files, network,
/// devices or spawning), `None` when the process is unrestricted or the kernel gives no line.
pub fn sandbox_label(p: &Proc) -> Option<&'static str> {
    let caps = p.caps.as_deref()?.trim();
    (!caps.is_empty() && caps != "all").then_some("Sandboxed")
}

/// What a capability line means for a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapsView {
    /// "Everything", "Nothing", "Its own folder", "Its own folder and 2 files you chose" ...
    pub can_read: String,
    /// "Yes", "Local only", "None".
    pub network: &'static str,
}

/// Explain a capability line in plain English. `None` for an empty or unreadable line.
pub fn explain_caps(caps: &str) -> Option<CapsView> {
    let caps = caps.trim();
    if caps.is_empty() {
        return None;
    }
    if caps == "all" {
        return Some(CapsView {
            can_read: "Everything".to_string(),
            network: "Yes",
        });
    }
    let get = |k: &str| caps.split_whitespace().find_map(|w| w.strip_prefix(k));
    let fs = get("fs=")?;
    let can_read = match fs {
        "all" => "Everything".to_string(),
        "none" => "Nothing".to_string(),
        list => {
            let grants: Vec<&str> = list.split(',').filter(|g| !g.is_empty()).collect();
            // Its own folder: /Applications/<app> or ~/Library/<app> (the two the system gives a packaged app).
            let own = |g: &str| {
                let path = g.rsplit_once(':').map_or(g, |(p, _)| p);
                path.starts_with("/Applications/") || path.contains("/Library/")
            };
            let own_n = grants.iter().filter(|g| own(g)).count();
            let files = grants.len() - own_n;
            match (own_n > 0, files) {
                (true, 0) => "Its own folder".to_string(),
                (true, n) => alloc::format!(
                    "Its own folder and {n} file{} you chose",
                    if n == 1 { "" } else { "s" }
                ),
                (false, 0) => "Nothing".to_string(),
                (false, n) => {
                    alloc::format!("{n} place{} you chose", if n == 1 { "" } else { "s" })
                }
            }
        }
    };
    let network = match get("net=") {
        Some("none") => "None",
        Some("local") => "Local only",
        _ => "Yes",
    };
    Some(CapsView { can_read, network })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "Name:\tmoo2048\nState:\tS (sleeping)\nTgid:\t12\nPid:\t12\nPPid:\t4\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t2048 kB\nThreads:\t3\nCpuNs:\t5000000\nStartNs:\t1000\nCaps:\tfs=/Applications/moo2048:r,/home/milk/Library/moo2048:rw net=none audio=1 clipboard=0 windows=1 spawn=0\n";

    #[test]
    fn parses_a_status() {
        let p = parse_status(STATUS).unwrap();
        assert_eq!(
            (p.pid, p.ppid, p.name.as_str(), p.state),
            (12, 4, "moo2048", 'S')
        );
        assert_eq!(
            (p.rss_kb, p.threads, p.cpu_ns, p.start_ns),
            (2048, 3, 5_000_000, 1000)
        );
        assert_eq!(p.uid, 1000);
        assert_eq!(sandbox_label(&p), Some("Sandboxed"));
        assert!(parse_status("Name:\tx\n").is_none());
        let mut q = p.clone();
        q.caps = None;
        assert_eq!(sandbox_label(&q), None);
        q.caps = Some("all".into());
        assert_eq!(sandbox_label(&q), None);
        q.caps = Some("fs=all net=none audio=1 clipboard=1 windows=1 spawn=1".into());
        assert_eq!(sandbox_label(&q), Some("Sandboxed")); // only the network is restricted
    }

    fn proc(pid: u32, name: &str, cpu_ns: u64, rss: u64) -> Proc {
        Proc {
            pid,
            name: name.into(),
            state: 'S',
            cpu_ns,
            rss_kb: rss,
            threads: 1,
            ..Proc::default()
        }
    }

    #[test]
    fn caps_in_plain_english() {
        let v = explain_caps("all").unwrap();
        assert_eq!((v.can_read.as_str(), v.network), ("Everything", "Yes"));
        let v = explain_caps("fs=/Applications/moo2048:r,/home/milk/Library/moo2048:rw net=none audio=1 clipboard=0 windows=1 spawn=0").unwrap();
        assert_eq!((v.can_read.as_str(), v.network), ("Its own folder", "None"));
        let v = explain_caps("fs=/Applications/x:r,/home/milk/Documents/a.txt:r,/home/milk/Documents/b.txt:rw net=local audio=0 clipboard=0 windows=1 spawn=0").unwrap();
        assert_eq!(
            (v.can_read.as_str(), v.network),
            ("Its own folder and 2 files you chose", "Local only")
        );
        let v = explain_caps("fs=none net=none audio=0 clipboard=0 windows=0 spawn=0").unwrap();
        assert_eq!(v.can_read, "Nothing");
        let v = explain_caps("fs=/home/milk/a.txt:r net=all audio=1 clipboard=1 windows=1 spawn=1")
            .unwrap();
        assert_eq!(v.can_read, "1 place you chose");
        assert_eq!(explain_caps(""), None);
        assert_eq!(explain_caps("garbage"), None);
    }

    #[test]
    fn cpu_percent_from_two_looks() {
        let prev = [proc(1, "a", 0, 0), proc(2, "b", 100, 0)];
        let mut now = [
            proc(1, "a", 500_000_000, 0),
            proc(2, "b", 100, 0),
            proc(3, "new", 9_000_000_000, 0),
        ];
        fill_cpu(&mut now, &prev, 1_000_000_000);
        assert_eq!(now[0].cpu10, 500); // half a core
        assert_eq!(now[1].cpu10, 0);
        assert_eq!(now[2].cpu10, 0); // no previous look
        // A reused pid (different start time) does not inherit the old counter.
        let mut p2 = proc(1, "a", 10, 0);
        p2.start_ns = 77;
        let mut now2 = [p2];
        fill_cpu(&mut now2, &prev, 1_000_000_000);
        assert_eq!(now2[0].cpu10, 0);
        // Zero interval: leave alone.
        let mut z = [proc(1, "a", 99, 0)];
        z[0].cpu10 = 42;
        fill_cpu(&mut z, &prev, 0);
        assert_eq!(z[0].cpu10, 42);
    }

    #[test]
    fn sort_and_filter() {
        let mut v = alloc::vec![
            proc(3, "zsh", 0, 10),
            proc(1, "Init", 0, 30),
            proc(2, "alpha", 0, 20)
        ];
        v[0].cpu10 = 900;
        sort(&mut v, SortKey::Name, false);
        assert_eq!(v.iter().map(|p| p.pid).collect::<Vec<_>>(), [2, 1, 3]);
        sort(&mut v, SortKey::Memory, true);
        assert_eq!(v.iter().map(|p| p.pid).collect::<Vec<_>>(), [1, 2, 3]);
        sort(&mut v, SortKey::Cpu, true);
        assert_eq!(v[0].pid, 3);
        sort(&mut v, SortKey::Pid, false);
        assert_eq!(v.iter().map(|p| p.pid).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(filter(&v, "AL").len(), 1);
        assert_eq!(filter(&v, "").len(), 3);
        assert!(filter(&v, "nope").is_empty());
    }

    #[test]
    fn system_processes() {
        assert!(is_system(&proc(1, "init", 0, 0)));
        assert!(!is_system(&proc(14, "moo2048", 0, 0)));
    }
}
