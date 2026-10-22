//! system: this computer (it replaced uname, uptime and free).
//!
//!   system info      name, version, kernel, uptime, CPUs, memory (also plain `system`)
//!   system memory    /proc/meminfo in Activity Monitor's groups (a table for msh when asked)
//!   system uptime    how long since boot, the processes, the load averages
//!
//! Restart and shut down are only in the desktop's milk menu (kernel/src/wm/power.rs); there
//! is no system call for them yet, so `system` has neither. Rules: user/syskit.
#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;

use rt::io::Out;
use rt::sys;
use rt::table::{Table, Ty, Val};
use syskit::args::{self, System};
use syskit::{fmt, help, memory, time};

rt::main!(main);

fn main(args: &rt::Args) -> i32 {
    let cmd = match args::system(&args.argv[1..]) {
        Ok(c) => c,
        Err(u) => {
            rt::eprintln!("{}", u.0);
            return 2;
        }
    };
    let mut out = Out::stdout();
    match cmd {
        System::Help => out.write(help::SYSTEM.as_bytes()),
        System::Info => {
            let Ok(u) = sys::uname() else {
                rt::eprintln!("system info: the kernel didn't say what it is");
                return 1;
            };
            let up = time::parse_uptime(&systools::text("/proc/uptime"));
            let cpus = systools::text("/proc/cpuload")
                .lines()
                .filter(|l| l.starts_with("cpu"))
                .count();
            let m = sysview::system::parse_meminfo(&systools::text("/proc/meminfo"));
            for l in fmt::pairs(&[
                ("name", u[1].clone()),
                ("system", format!("{} {}", u[0], u[2])),
                ("kernel", u[3].clone()),
                ("machine", u[4].clone()),
                ("uptime", time::words(up)),
                ("CPUs", format!("{cpus}")),
                (
                    "memory",
                    format!("{}, {} free", fmt::kb(m.total_kb), fmt::kb(m.free_kb)),
                ),
            ]) {
                out.line(&l);
            }
        }
        System::Uptime => {
            let up = time::parse_uptime(&systools::text("/proc/uptime"));
            let now = sys::clock_ns(sys::CLOCK_REALTIME) as i64 / 1_000_000_000;
            let procs = systools::processes().len();
            let load = time::parse_load(&systools::text("/proc/loadavg"));
            out.line(&time::uptime_line(up, now, procs, load.as_ref()));
        }
        System::Memory => {
            let text = systools::text("/proc/meminfo");
            let apps: u64 = systools::processes().iter().map(|p| p.rss_kb).sum();
            if rt::table::requested(args) {
                let m = sysview::system::parse_meminfo(&text);
                let mut t =
                    Table::new(&[("group", Ty::Str), ("size", Ty::Size), ("percent", Ty::Str)]);
                for g in memory::groups(&m, apps) {
                    t.push(alloc::vec![
                        Val::str(g.label),
                        Val::Int((g.kb * 1024) as i64),
                        Val::Str(format!("{}.{}", g.pct10 / 10, g.pct10 % 10)),
                    ]);
                }
                t.write(&mut out);
            } else {
                for l in memory::report(&text, apps) {
                    out.line(&l);
                }
            }
        }
    }
    out.flush();
    0
}
