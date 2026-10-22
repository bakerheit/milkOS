//! processes: what's running, and stopping it (it replaced ps, top and kill).
//!
//!   processes list [--kernel]            processes watch [--plain] [--frames N] [--every S]
//!   processes stop [--now] NAME|PID      processes signal NAME|PID SIGNAL
//!   processes signals                    processes info NAME|PID        processes help
//!
//! stop and signal take --all (every process with that NAME) and --group PGID. `list` gives
//! msh a table when asked (MILK_TABLE=1, docs/shell.md). Rules: user/syskit.
#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use rt::io::Out;
use rt::sys;
use rt::table::{Table, Ty, Val};
use syskit::args::{self, Processes, Target};
use syskit::procs::{self, Row};
use syskit::{fmt, help, signals};

rt::main!(main);

fn main(args: &rt::Args) -> i32 {
    let a = &args.argv[1..];
    let cmd = match args::processes(a) {
        Ok(c) => c,
        Err(u) => {
            rt::eprintln!("{}", u.0);
            return 2;
        }
    };
    match cmd {
        Processes::Help => {
            rt::print!("{}", help::PROCESSES);
            0
        }
        Processes::List { kernel } => list(args, kernel),
        Processes::Watch {
            plain,
            frames,
            every,
        } => systools::watch::run(plain, frames, every),
        Processes::Signal {
            target,
            signal,
            all,
        } => send(target, signal, all, a.first() == Some(&"stop")),
        Processes::Signals => {
            let mut out = Out::stdout();
            for (n, name, what) in signals::ALL {
                out.line(&format!("{n:>2}  {name:<6} {what}"));
            }
            out.flush();
            0
        }
        Processes::Info { what } => info(what),
    }
}

fn list(args: &rt::Args, kernel: bool) -> i32 {
    let mut rows: Vec<(Row, bool)> = systools::processes()
        .into_iter()
        .map(|r| (r, false))
        .collect();
    if rows.is_empty() {
        rt::eprintln!("processes: /proc can't be read");
        return 1;
    }
    if kernel {
        rows.extend(
            procs::parse_threads(&systools::text("/proc/threads"))
                .into_iter()
                .map(|r| (r, true)),
        );
    }
    let mut out = Out::stdout();
    if rt::table::requested(args) {
        let mut t = Table::new(&[
            ("pid", Ty::Int),
            ("parent", Ty::Int),
            ("group", Ty::Int),
            ("thread", Ty::Int),
            ("state", Ty::Str),
            ("core", Ty::Int),
            ("cpu_ms", Ty::Int),
            ("memory", Ty::Size),
            ("name", Ty::Str),
        ]);
        for (r, k) in &rows {
            let n = |v: u64| if *k { Val::Nil } else { Val::Int(v as i64) };
            t.push(alloc::vec![
                n(r.pid.into()),
                n(r.ppid.into()),
                n(r.pgid.into()),
                Val::Int(r.tid.into()),
                Val::str(&r.state_word),
                Val::Int(r.core.into()),
                Val::Int((r.cpu_ns / 1_000_000) as i64),
                n(r.rss_kb * 1024),
                Val::Str(if *k {
                    format!("[{}]", r.name)
                } else {
                    r.name.clone()
                }),
            ]);
        }
        t.write(&mut out);
        out.flush();
        return 0;
    }
    out.line(procs::HEADER);
    for (r, k) in &rows {
        out.line(&procs::line(r, *k));
    }
    out.flush();
    0
}

/// stop / signal: TERM or KILL (stop) or any signal, to one process, every process with a
/// name, or a process group.
fn send(target: Target, sig: i32, all: bool, stop: bool) -> i32 {
    let verb = if stop { "stop" } else { "signal" };
    let rows = systools::processes();
    let name_of = |pid: u32| {
        rows.iter()
            .find(|r| r.pid == pid)
            .map_or(String::new(), |r| r.name.clone())
    };
    let (pids, group) = match target {
        Target::Group(g) => (alloc::vec![g], true),
        Target::One(what) => match procs::pick(&rows, what, all, sys::getpid(), verb) {
            Ok(p) => (p, false),
            Err(m) => {
                rt::eprintln!("processes {verb}: {m}");
                return 1;
            }
        },
    };
    let mut rc = 0;
    for pid in pids {
        let name = name_of(pid);
        let target = if group { -(pid as i32) } else { pid as i32 };
        let who = if group {
            format!("process group {pid}")
        } else if name.is_empty() {
            format!("process {pid}")
        } else {
            format!("{name} (PID {pid})")
        };
        match sys::kill(target, sig) {
            Ok(()) if stop && sig == signals::KILL => rt::println!("stopped {who}"),
            Ok(()) if stop => rt::println!("asked {who} to stop"),
            Ok(()) => rt::println!("sent {} to {who}", signals::describe(sig)),
            Err(e) if group && e.0 == 3 => {
                rt::eprintln!("processes {verb}: there's no process group {pid}");
                rc = 1;
            }
            Err(e) => {
                rt::eprintln!(
                    "processes {verb}: {}",
                    procs::say_kill(pid, &name, e.0, verb)
                );
                rc = 1;
            }
        }
    }
    rc
}

fn info(what: &str) -> i32 {
    let rows = systools::processes();
    let pid = match procs::pick(&rows, what, false, sys::getpid(), "info") {
        Ok(p) => p[0],
        Err(m) => {
            rt::eprintln!("processes info: {m}");
            return 1;
        }
    };
    let Some(r) = procs::parse_status(&systools::text(&format!("/proc/{pid}/status"))) else {
        rt::eprintln!("processes info: there's no process {pid}");
        return 1;
    };
    let cmd: Vec<String> = systools::text(&format!("/proc/{pid}/cmdline"))
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    let exe = sys::readlink(&format!("/proc/{pid}/exe")).unwrap_or_default();
    let user = |(r, e): (u32, u32)| {
        let mut s = format!("{} ({r})", rt::users::user_name(r));
        if e != r {
            s += &format!(", acting as {} ({e})", rt::users::user_name(e));
        }
        s
    };
    let groups: Vec<String> = r
        .groups
        .iter()
        .map(|&g| format!("{} ({g})", rt::users::group_name(g)))
        .collect();
    let sandbox = match sysview::proc::explain_caps(&r.caps) {
        _ if r.caps.trim() == "all" => String::from("none (it can use everything its user can)"),
        Some(v) => format!(
            "sandboxed: reads {}, network {} ({})",
            v.can_read.to_lowercase(),
            v.network.to_lowercase(),
            r.caps
        ),
        None => r.caps.clone(),
    };
    let up = syskit::time::parse_uptime(&systools::text("/proc/uptime"));
    let age = up.saturating_sub(r.start_ns / 1_000_000_000);
    let mut out = Out::stdout();
    for l in fmt::pairs(&[
        ("name", r.name.clone()),
        ("pid", format!("{}", r.pid)),
        ("command", cmd.join(" ")),
        ("program", exe),
        ("state", r.state_word.clone()),
        ("parent", {
            let p = rows.iter().find(|x| x.pid == r.ppid);
            match p {
                Some(p) => format!("{} ({})", p.name, r.ppid),
                None if r.ppid == 0 => String::from("the kernel (0)"),
                None => format!("{}", r.ppid),
            }
        }),
        ("group", format!("{} (session {})", r.pgid, r.sid)),
        ("user", user(r.uid)),
        ("group id", {
            let (g, e) = r.gid;
            let mut s = format!("{} ({g})", rt::users::group_name(g));
            if e != g {
                s += &format!(", acting as {} ({e})", rt::users::group_name(e));
            }
            s
        }),
        ("groups", groups.join(", ")),
        ("memory", fmt::kb(r.rss_kb)),
        ("cpu time", procs::cpu_time(r.cpu_ns)),
        ("core", format!("{}", r.core)),
        ("threads", format!("{}", r.threads.max(1))),
        ("open files", format!("{}", r.fds)),
        ("started", format!("{} ago", syskit::time::words(age))),
        ("sandbox", sandbox),
        ("ticket", r.ticket.clone()),
    ]) {
        out.line(&l);
    }
    out.flush();
    0
}
