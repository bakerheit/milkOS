//! services: background services, which milkd starts and watches (it replaced svc and log).
//!
//!   services list            services status [NAME]      services start|stop|restart NAME
//!   services signal NAME [SIGNAL]                         services reload
//!   services logs [NAME...] [--follow] [--since T] [--last N] [--level L] [--this-boot]
//!
//! Talks to milkd over /System/Services/Sockets/milkd.sock. Anyone may list and see status;
//! start, stop, restart, signal and reload need root or group admin (milkd checks the caller
//! with SO_PEERCRED). `signal` signals the service's process (default KILL) without stopping
//! the service, so milkd restarts it per its policy (crash tests). `logs` is the journal
//! (kernel/src/journal.rs): earlier boots from /Volumes/Milk/var/log/journal, then the live
//! ring in /dev/journal; reading it needs root or group admin. list, status and logs give msh
//! a table when asked (MILK_TABLE=1). Rules: user/syskit.
#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use rt::io::Out;
use rt::sys;
use rt::table::{Table, Ty, Val};
use syskit::args::{self, Services};
use syskit::journal::{self, Entry};
use syskit::{help, services as kit, signals};

rt::main!(main);

const JOURNAL_DIR: &str = "/Volumes/Milk/var/log/journal";

fn main(args: &rt::Args) -> i32 {
    let a = &args.argv[1..];
    let cmd = match args::services(a) {
        Ok(c) => c,
        Err(u) => {
            rt::eprintln!("{}", u.0);
            return 2;
        }
    };
    match cmd {
        Services::Help => {
            rt::print!("{}", help::SERVICES);
            0
        }
        Services::List { long } => {
            match systools::ask_milkd(if long { "status" } else { "list" }) {
                Ok(lines) => {
                    show(args, &lines, false);
                    0
                }
                Err(e) => fail("list", &e),
            }
        }
        Services::Ask { verb, name } => {
            let req = match name {
                Some(n) => format!("{verb} {n}"),
                None => String::from(verb),
            };
            match systools::ask_milkd(&req) {
                Ok(lines) => {
                    match verb {
                        "status" => show(args, &lines, true),
                        "reload" => rt::println!("read the service files again"),
                        _ => {
                            let n = name.unwrap_or("");
                            let past = match verb {
                                "start" => "started",
                                "stop" => "stopped",
                                _ => "restarted",
                            };
                            rt::println!("{past} {n}");
                            show(args, &lines, false);
                        }
                    }
                    0
                }
                Err(e) => fail(verb, &e),
            }
        }
        Services::Signal { name, signal } => {
            match systools::ask_milkd(&format!("kill {name} {signal}")) {
                Ok(lines) => {
                    let pid = lines
                        .first()
                        .and_then(|l| l.split('\t').nth(1))
                        .unwrap_or("?");
                    rt::println!(
                        "sent {} to {name} (PID {pid}); its restart policy applies",
                        signals::describe(signal)
                    );
                    0
                }
                Err(e) => fail("signal", &e),
            }
        }
        Services::Logs(words) => logs(args, words),
    }
}

fn fail(verb: &str, why: &str) -> i32 {
    rt::eprintln!("services {verb}: {}", kit::say(why, verb));
    1
}

fn show(args: &rt::Args, lines: &[String], long: bool) {
    let mut out = Out::stdout();
    if rt::table::requested(args) {
        let mut t = Table::new(&[
            ("name", Ty::Str),
            ("state", Ty::Str),
            ("pid", Ty::Int),
            ("restarts", Ty::Int),
            ("uptime", Ty::Int),
            ("user", Ty::Str),
            ("detail", Ty::Str),
            ("description", Ty::Str),
        ]);
        for l in lines {
            let f = kit::fields(l);
            let int = |s: &str| s.parse::<i64>().map_or(Val::Nil, Val::Int);
            t.push(alloc::vec![
                Val::str(f[0]),
                Val::str(f[1]),
                int(f[2]),
                int(f[3]),
                int(f[4]),
                Val::str(f[5]),
                Val::str(f[6]),
                Val::str(f[7]),
            ]);
        }
        t.write(&mut out);
        out.flush();
        return;
    }
    if long {
        for (i, l) in lines.iter().enumerate() {
            if i > 0 {
                out.line("");
            }
            for d in kit::detail(l) {
                out.line(&d);
            }
        }
    } else {
        out.line(kit::HEADER);
        for l in lines {
            out.line(&kit::line(l));
        }
    }
    out.flush();
}

fn entries(bytes: &[u8]) -> Vec<Entry> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(journal::parse)
        .collect()
}

fn logs(args: &rt::Args, words: &[&str]) -> i32 {
    let now = sys::clock_ns(sys::CLOCK_REALTIME) as i64 / 1_000_000_000;
    let o = match journal::parse_args(words, now) {
        Ok(o) => o,
        Err(m) => {
            rt::eprintln!("services logs: {m}");
            return 2;
        }
    };
    let live_fd = match sys::open("/dev/journal", sys::O_RDONLY | sys::O_CLOEXEC) {
        Ok(fd) => fd,
        Err(e) if e.0 == 13 || e.0 == 1 => {
            rt::eprintln!(
                "services logs: only root and administrators can read the journal (try admin services logs)"
            );
            return 1;
        }
        Err(e) => {
            rt::eprintln!("services logs: /dev/journal can't be read ({e})");
            return 1;
        }
    };
    // The live ring (this boot), then the files (older boots and the flushed part of this
    // one), without the entries the ring still has.
    let mut buf = alloc::vec![0u8; 16384];
    let mut raw = Vec::new();
    loop {
        match sys::read(live_fd, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
    }
    let live = entries(&raw);
    let boot = live.first().map(|e| e.boot.clone());
    let mut files = Vec::new();
    for name in ["journal.3", "journal.2", "journal.1", "current.log"] {
        if let Ok(b) = sys::read_file(&format!("{JOURNAL_DIR}/{name}")) {
            files.extend(entries(&b));
        }
    }
    let all = journal::merge(files, live);
    let mut shown: Vec<&Entry> = all.iter().filter(|e| o.keep(e, boot.as_deref())).collect();
    if let Some(n) = o.last {
        let k = shown.len().saturating_sub(n);
        shown.drain(..k);
    }
    let mut out = Out::stdout();
    if rt::table::requested(args) && !o.follow {
        let mut t = Table::new(&[
            ("time", Ty::Int),
            ("boot", Ty::Str),
            ("seq", Ty::Int),
            ("name", Ty::Str),
            ("pid", Ty::Int),
            ("uid", Ty::Int),
            ("level", Ty::Str),
            ("message", Ty::Str),
        ]);
        for e in shown {
            t.push(alloc::vec![
                Val::Int(e.wall),
                Val::str(&e.boot),
                Val::Int(e.seq as i64),
                Val::str(&e.unit),
                Val::Int(e.pid as i64),
                Val::Int(e.uid as i64),
                Val::str(&e.level),
                Val::str(&e.msg),
            ]);
        }
        t.write(&mut out);
        out.flush();
        return 0;
    }
    for e in shown {
        out.line(&journal::show(e));
    }
    out.flush();
    if !o.follow {
        return 0;
    }
    // Follow: /dev/journal's offset is a cursor; at the end, wait and read on.
    let mut pending = String::new();
    loop {
        match sys::read(live_fd, &mut buf) {
            Ok(0) | Err(_) => sys::sleep_ms(200),
            Ok(n) => {
                pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                while let Some(i) = pending.find('\n') {
                    let line: String = pending.drain(..=i).collect();
                    if let Some(e) = journal::parse(line.trim_end())
                        && o.keep(&e, boot.as_deref())
                    {
                        out.line(&journal::show(&e));
                    }
                }
                out.flush();
            }
        }
    }
}
