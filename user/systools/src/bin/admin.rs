//! admin: run a command as an administrator (it replaced sudo and auth).
//!
//!   admin [--user NAME] [--stdin] COMMAND [ARGS...]
//!       (was sudo) run COMMAND as root (or NAME) if you are an administrator (group admin,
//!       gid 80), after typing *your own* password: three tries, a second's wait after each
//!       wrong one. Nothing is remembered: every admin asks. Root needs no password.
//!   admin run ...
//!       the same, spelled out, for a command called help, status, ticket or run.
//!   admin ticket [--user ADMIN] [--stdin] COMMAND [ARGS...]
//!       (was auth) run COMMAND as *you*, with an administrator's authorisation ticket (5
//!       minutes, this process only, kept across the exec) after that administrator's
//!       password; the ticket unlocks what asks for one (the destructive
//!       /System/Kernel/fs/milkfs controls, `fs::milkfs::DESTRUCTIVE`). The kernel checks the
//!       password, rate-limits wrong ones per user and audits every try (proc/auth.rs).
//!   admin status
//!       you, whether you are an administrator, and this process's ticket
//!       (`admin ticket admin status` shows a fresh one).
//!
//! --stdin: the password comes from stdin, with no terminal needed (scripts); that is how
//! the password is always read, so it only says so. COMMAND without a slash is
//! /Commands/COMMAND. Set-user-ID root (initramfs.list mode 4755) for the first form; ticket,
//! status and help give root up first (back to the caller's real user id for good), so a
//! ticket's command runs exactly as you, as it did under auth.
//!
//! The command run as root gets a clean environment made from the target account (PATH,
//! HOME, USER, LOGNAME, the caller's TERM) plus ADMIN_USER=<you>; sudo used to pass the
//! caller's whole environment, its PATH and HOME included. A wrong password is written to the
//! audit log as `user.password.fail` (sudo left no record); the wait is still one second per
//! wrong password in this process, as before. Rules: user/syskit.
#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use rt::io::Out;
use rt::{sys, users};
use syskit::args::{self, Admin, command_path};
use syskit::{fmt, help};

rt::main!(main);

fn main(args: &rt::Args) -> i32 {
    let cmd = match args::admin(&args.argv[1..]) {
        Ok(c) => c,
        Err(u) => {
            let _ = systools::drop_root();
            rt::eprintln!("{}", u.0);
            return 2;
        }
    };
    if let Admin::Run { user, command } = cmd {
        return run(args, user, command);
    }
    if !systools::drop_root() {
        rt::eprintln!("admin: couldn't give up the root this program starts with");
        return 1;
    }
    match cmd {
        Admin::Ticket { admin, command } => ticket(args, admin, command),
        Admin::Status => status(),
        _ => {
            rt::print!("{}", help::ADMIN);
            0
        }
    }
}

fn say_exec(prog: &str, e: sys::Errno) -> String {
    match e.0 {
        2 => format!("there's no command {prog}"),
        13 => format!("{prog} can't be run (you don't have permission)"),
        8 => format!("{prog} isn't a program"),
        _ => format!("{prog} can't be started ({e})"),
    }
}

/// `admin COMMAND...` (was sudo): the same checks in the same order.
fn run(args: &rt::Args, target: &str, cmdv: &[&str]) -> i32 {
    if sys::geteuid() != 0 {
        rt::eprintln!("admin: this program must be set-user-ID root (mode 4755)");
        return 1;
    }
    let ruid = sys::getuid();
    let me = users::user_name(ruid);
    let Some(to) = users::by_name(target) else {
        rt::eprintln!("admin: there's no user called {target}");
        return 1;
    };
    if ruid != 0 {
        let admin = users::by_name(&me)
            .is_some_and(|p| users::groups_of(&p.name, p.gid).contains(&users::ADMIN_GID));
        if !admin {
            rt::eprintln!("admin: {me} isn't an administrator (only group admin can use admin)");
            return 1;
        }
        let mut ok = false;
        for _ in 0..3 {
            let Some(pw) = users::read_password(&format!("Password for {me}: ")) else {
                break;
            };
            if users::verify(&me, &pw) {
                ok = true;
                break;
            }
            systools::audit(
                "password.fail",
                &format!("via=admin user={me} uid={ruid} as={}", to.name),
            );
            sys::sleep_ms(1000);
            rt::eprintln!("That's not the password. Try again.");
        }
        if !ok {
            rt::eprintln!("admin: that's not the password for {me}; nothing was run");
            return 1;
        }
    }
    if let Err(e) = users::become_user(&to) {
        rt::eprintln!("admin: can't become {} ({e})", to.name);
        return 1;
    }
    // A clean environment from the target account; only the terminal type comes along.
    let mut env: Vec<String> = users::login_env(&to)
        .into_iter()
        .filter(|e| !e.starts_with("TERM="))
        .collect();
    let term = args
        .env("TERM")
        .filter(|t| {
            (1..=32).contains(&t.len())
                && t.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        })
        .unwrap_or("dumb");
    env.push(format!("TERM={term}"));
    env.push(format!("ADMIN_USER={me}"));
    let env: Vec<&str> = env.iter().map(String::as_str).collect();
    let prog = cmdv[0];
    let e = sys::execve(&command_path(prog), cmdv, &env);
    rt::eprintln!("admin: {}", say_exec(prog, e));
    127
}

/// `admin ticket COMMAND...` (was auth): as the caller, with the kernel's ticket.
fn ticket(args: &rt::Args, admin: Option<&str>, cmd: &[&str]) -> i32 {
    let who = admin.map_or_else(|| users::user_name(sys::getuid()), String::from);
    let Some(pw) = users::read_password(&format!("Password for {who} (administrator): ")) else {
        return 1;
    };
    match sys::authorize_password(&pw, admin.unwrap_or("")) {
        Ok(_) => {}
        Err(sys::Errno(13)) => {
            rt::eprintln!("admin ticket: that's not the password for {who}");
            return 1;
        }
        Err(sys::Errno(11)) => {
            rt::eprintln!("admin ticket: too many wrong passwords; try again in a while");
            return 1;
        }
        Err(sys::Errno(1)) => {
            rt::eprintln!("admin ticket: {who} isn't an administrator");
            return 1;
        }
        Err(e) => {
            rt::eprintln!("admin ticket: not authorised ({e})");
            return 1;
        }
    }
    let e = sys::execve(&command_path(cmd[0]), cmd, &args.envp);
    rt::eprintln!("admin ticket: {}", say_exec(cmd[0], e));
    127
}

fn status() -> i32 {
    let uid = sys::getuid();
    // The same test `admin COMMAND` makes: the account's groups in /System/Settings/group.
    let admin = users::by_uid(uid)
        .is_some_and(|p| users::groups_of(&p.name, p.gid).contains(&users::ADMIN_GID));
    let left = sys::auth_left();
    let mut out = Out::stdout();
    for l in fmt::pairs(&[
        ("you", format!("{} ({uid})", users::user_name(uid))),
        (
            "administrator",
            if uid == 0 {
                String::from("yes (root)")
            } else if admin {
                String::from("yes (group admin)")
            } else {
                String::from("no")
            },
        ),
        (
            "ticket",
            if left == 0 {
                String::from("none (admin ticket COMMAND gives COMMAND one)")
            } else {
                format!("{} s left (this process only)", left / 1000)
            },
        ),
    ]) {
        out.line(&l);
    }
    out.flush();
    0
}
