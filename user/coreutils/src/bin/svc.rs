//! svc list | status [UNIT] | start UNIT | stop UNIT | restart UNIT | kill UNIT [SIG] |
//! reload | logs UNIT
//! Talks to milkd over /run/milkd.sock. Anyone may list / status; start, stop, restart,
//! kill and reload need root or group admin (milkd checks the caller with SO_PEERCRED).
//! `kill` signals the unit's process (default 9) without stopping the unit, so milkd
//! restarts it per its policy (crash tests).
//! MILK_TABLE=1: list / status as a table.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use rt::io::Out;
use rt::sys;
use rt::unix::{self, Name};
rt::main!(main);

const SOCK: &str = "/run/milkd.sock";

/// Send one request, return the reply lines (without the final ok / error line) or the error.
fn ask(req: &str) -> Result<Vec<String>, String> {
    let fd = unix::socket(unix::SOCK_STREAM | unix::SOCK_CLOEXEC)
        .map_err(|e| alloc::format!("socket: {e}"))?;
    unix::connect(fd, &Name::Path(SOCK))
        .map_err(|e| alloc::format!("{SOCK}: {e} (is milkd running?)"))?;
    sys::write_all(fd, alloc::format!("{req}\n").as_bytes()).map_err(|e| alloc::format!("{e}"))?;
    let mut data = Vec::new();
    let mut b = [0u8; 4096];
    loop {
        match sys::read(fd, &mut b) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                data.extend_from_slice(&b[..n]);
                let t = String::from_utf8_lossy(&data);
                if t.ends_with("ok\n")
                    || t.lines().last().is_some_and(|l| l.starts_with("error")) && t.ends_with('\n')
                {
                    break;
                }
            }
        }
    }
    let _ = sys::close(fd);
    let text = String::from_utf8_lossy(&data).into_owned();
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    match lines.pop() {
        Some(l) if l == "ok" => Ok(lines),
        Some(l) => Err(String::from(l.strip_prefix("error\t").unwrap_or(&l))),
        None => Err(String::from("no answer from milkd")),
    }
}

/// unit state pid restarts uptime_s user detail description warnings
fn fields(l: &str) -> Vec<&str> {
    let mut f: Vec<&str> = l.splitn(9, '\t').collect();
    f.resize(9, "");
    f
}

fn show(args: &rt::Args, lines: &[String], long: bool) {
    let mut out = Out::stdout();
    if rt::table::requested(args) {
        use rt::table::{Table, Ty, Val};
        let mut t = Table::new(&[
            ("unit", Ty::Str),
            ("state", Ty::Str),
            ("pid", Ty::Int),
            ("restarts", Ty::Int),
            ("uptime", Ty::Int),
            ("user", Ty::Str),
            ("detail", Ty::Str),
            ("description", Ty::Str),
        ]);
        for l in lines {
            let f = fields(l);
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
        return;
    }
    if !long {
        out.line(&alloc::format!(
            "{:<14} {:<11} {:>5} {:>8}  {}",
            "UNIT",
            "STATE",
            "PID",
            "RESTARTS",
            "DESCRIPTION"
        ));
    }
    for l in lines {
        let f = fields(l);
        if long {
            out.line(&alloc::format!("{} - {}", f[0], f[7]));
            let mut st = String::from(f[1]);
            if !f[6].is_empty() {
                st += &alloc::format!(" ({})", f[6]);
            }
            out.line(&alloc::format!("   state: {st}"));
            if f[2] != "0" {
                out.line(&alloc::format!("     pid: {}  up {} s", f[2], f[4]));
            }
            out.line(&alloc::format!("    user: {}  restarts: {}", f[5], f[3]));
            if !f[8].is_empty() {
                out.line(&alloc::format!(" warning: {}", f[8]));
            }
        } else {
            let pid = if f[2] == "0" { "-" } else { f[2] };
            let mut st = String::from(f[1]);
            if matches!(f[1], "failed" | "skipped" | "restarting") && !f[6].is_empty() {
                st = alloc::format!("{} ({})", f[1], f[6]);
            }
            out.line(&alloc::format!(
                "{:<14} {:<11} {:>5} {:>8}  {}",
                f[0],
                st,
                pid,
                f[3],
                f[7]
            ));
        }
    }
    out.flush();
}

fn main(args: &rt::Args) -> i32 {
    let cmd = args.get(1).unwrap_or("list");
    let unit = args.get(2);
    let req = match (cmd, unit) {
        ("list", _) => String::from("list"),
        ("status", None) => String::from("status"),
        ("status" | "start" | "stop" | "restart", Some(u)) => alloc::format!("{cmd} {u}"),
        ("reload", _) => String::from("reload"),
        ("kill", Some(u)) => alloc::format!("kill {u} {}", args.get(3).unwrap_or("9")),
        ("logs", Some(u)) => {
            let e = sys::execve("/bin/log", &["log", "-u", u], &[]);
            rt::eprintln!("svc: /bin/log: {e}");
            return 1;
        }
        _ => {
            rt::eprintln!(
                "usage: svc list | status [UNIT] | start|stop|restart UNIT | kill UNIT [SIG] | reload | logs UNIT"
            );
            return 2;
        }
    };
    match ask(&req) {
        Ok(lines) if cmd == "kill" => {
            let pid = lines
                .first()
                .and_then(|l| l.split('\t').nth(1))
                .unwrap_or("?");
            rt::println!(
                "svc: signal {} sent to {} (pid {pid}); its restart policy applies",
                args.get(3).unwrap_or("9"),
                unit.unwrap_or("")
            );
            0
        }
        Ok(lines) => {
            if !lines.is_empty() {
                show(args, &lines, cmd == "status" && unit.is_some());
            }
            0
        }
        Err(e) => {
            rt::eprintln!("svc: {e}");
            1
        }
    }
}
