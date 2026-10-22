//! users: the accounts on this computer (it replaced whoami, id, passwd and su).
//!
//!   users me                 who you are: name, id, groups (also plain `users`)
//!   users list               every account (a table for msh when asked)
//!   users password [NAME]    change a password: your own after the current one; root
//!                            changes anyone's. Writes /System/Settings/shadow (0600, salted
//!                            `$5m$` hash) and a copy on the milkfs disk so it survives reboots.
//!   users switch [NAME] [--command COMMAND]
//!                            a login shell (or one command) as NAME (default root) after
//!                            *NAME's* password; root needs none.
//!
//! Set-user-ID root (initramfs.list mode 4755), for password and switch. `me`, `list` and
//! `help` give that up first (back to the caller's real user id for good), so they run with
//! the caller's own rights; `me` is the real user id (whoami used to print the effective one,
//! which a set-user-ID program can't see). A wrong password costs one second, and is
//! written to the audit log as `user.password.fail` (the kernel adds the pid and program);
//! a switch that works is in the log as the kernel's id changes. Rules: user/syskit.
#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use rt::io::Out;
use rt::table::{Table, Ty, Val};
use rt::{sys, users};
use syskit::args::{self, Users};
use syskit::{fmt, help};

rt::main!(main);

fn main(args: &rt::Args) -> i32 {
    let cmd = match args::users(&args.argv[1..]) {
        Ok(c) => c,
        Err(u) => {
            let _ = systools::drop_root();
            rt::eprintln!("{}", u.0);
            return 2;
        }
    };
    match cmd {
        Users::Password(name) => return password(name),
        Users::Switch { name, command } => return switch(name, command),
        _ => {}
    }
    if !systools::drop_root() {
        rt::eprintln!("users: couldn't give up the root this program starts with");
        return 1;
    }
    match cmd {
        Users::Help => {
            rt::print!("{}", help::USERS);
            0
        }
        Users::List => list(args),
        _ => me(),
    }
}

fn group(g: u32) -> String {
    format!("{} ({g})", users::group_name(g))
}

fn me() -> i32 {
    let uid = sys::getuid();
    let (rgid, egid) = (sys::getgid(), sys::getegid());
    let mut gs = alloc::vec![egid];
    for g in sys::getgroups() {
        if !gs.contains(&g) {
            gs.push(g);
        }
    }
    let acct = users::by_uid(uid);
    let mut primary = group(rgid);
    if egid != rgid {
        primary += &format!(", acting as {}", group(egid));
    }
    let mut out = Out::stdout();
    for l in fmt::pairs(&[
        ("name", users::user_name(uid)),
        ("id", format!("{uid}")),
        ("group", primary),
        (
            "groups",
            gs.iter().map(|&g| group(g)).collect::<Vec<_>>().join(", "),
        ),
        (
            "administrator",
            fmt::yes(uid == 0 || gs.contains(&users::ADMIN_GID)),
        ),
        (
            "home",
            acct.as_ref().map_or(String::new(), |a| a.home.clone()),
        ),
        (
            "shell",
            acct.as_ref().map_or(String::new(), |a| a.shell.clone()),
        ),
    ]) {
        out.line(&l);
    }
    out.flush();
    0
}

fn list(args: &rt::Args) -> i32 {
    let all = users::all();
    if all.is_empty() {
        rt::eprintln!("users list: /System/Settings/passwd can't be read");
        return 1;
    }
    let mut out = Out::stdout();
    let groups_of = |p: &users::Passwd| -> Vec<String> {
        users::groups_of(&p.name, p.gid)
            .into_iter()
            .map(users::group_name)
            .collect()
    };
    let is_admin = |p: &users::Passwd| {
        p.uid == 0 || users::groups_of(&p.name, p.gid).contains(&users::ADMIN_GID)
    };
    if rt::table::requested(args) {
        let mut t = Table::new(&[
            ("name", Ty::Str),
            ("id", Ty::Int),
            ("group", Ty::Str),
            ("groups", Ty::Str),
            ("admin", Ty::Str),
            ("home", Ty::Str),
            ("shell", Ty::Str),
            ("full_name", Ty::Str),
        ]);
        for p in &all {
            t.push(alloc::vec![
                Val::str(&p.name),
                Val::Int(p.uid.into()),
                Val::Str(users::group_name(p.gid)),
                Val::Str(groups_of(p).join(",")),
                Val::Str(fmt::yes(is_admin(p))),
                Val::str(&p.home),
                Val::str(&p.shell),
                Val::str(&p.gecos),
            ]);
        }
        t.write(&mut out);
        out.flush();
        return 0;
    }
    let w = all.iter().map(|p| p.name.len()).max().unwrap_or(4).max(4);
    let hw = all.iter().map(|p| p.home.len()).max().unwrap_or(4).max(4);
    out.line(&format!(
        "{:<w$} {:>5}  {:<5}  {:<hw$}  GROUPS",
        "NAME", "ID", "ADMIN", "HOME"
    ));
    for p in &all {
        out.line(&format!(
            "{:<w$} {:>5}  {:<5}  {:<hw$}  {}",
            p.name,
            p.uid,
            fmt::yes(is_admin(p)),
            p.home,
            groups_of(p).join(", ")
        ));
    }
    out.flush();
    0
}

/// `users password [NAME]` (was passwd): the same checks in the same order.
fn password(name: Option<&str>) -> i32 {
    let ruid = sys::getuid();
    let me = users::user_name(ruid);
    let user: &str = name.unwrap_or(me.as_str());
    if users::by_name(user).is_none() {
        rt::eprintln!("users password: there's no user called {user}");
        return 1;
    }
    if sys::geteuid() != 0 {
        rt::eprintln!("users password: this program must be set-user-ID root (mode 4755)");
        return 1;
    }
    let mut old_pw: Option<String> = None;
    if ruid != 0 {
        if user != me {
            rt::eprintln!("users password: you can only change your own password");
            return 1;
        }
        let Some(old) = users::read_password(&format!("Current password for {me}: ")) else {
            return 1;
        };
        if !users::verify(&me, &old) {
            systools::audit(
                "password.fail",
                &format!("via=users-password user={me} uid={ruid}"),
            );
            sys::sleep_ms(1000);
            rt::eprintln!("users password: that's not the password for {me}");
            return 1;
        }
        old_pw = Some(old);
    }
    let (Some(a), Some(b)) = (
        users::read_password("New password: "),
        users::read_password("Type it again: "),
    ) else {
        return 1;
    };
    if a != b {
        rt::eprintln!("users password: the two passwords don't match; nothing changed");
        return 1;
    }
    if a.is_empty() {
        rt::eprintln!("users password: an empty password isn't allowed; nothing changed");
        return 1;
    }
    match users::set_shadow_hash(user, &users::make_hash(&a)) {
        Ok(()) => {
            rt::println!("users password: the password for {user} is changed");
            if let Some(old) = old_pw.as_deref() {
                sync_disk_passphrase(old, &a);
            }
            0
        }
        Err(e) => {
            rt::eprintln!("users password: /System/Settings/shadow can't be written ({e})");
            1
        }
    }
}

/// Keep an encrypted disk whose passphrase was the login password opening at login: try
/// the same change on the primary milkfs volume (/System/Kernel/fs/milkfs/passwd, `OLD\nNEW`).
fn sync_disk_passphrase(old: &str, new: &str) {
    if new.len() < 8 {
        return; // milkfs passphrases need 8+ characters: the disk keeps its own
    }
    let Ok(fd) = sys::open(
        "/System/Kernel/fs/milkfs/passwd",
        sys::O_WRONLY | sys::O_CLOEXEC,
    ) else {
        return; // no milkfs
    };
    let r = sys::write(fd, format!("{old}\n{new}").as_bytes());
    let _ = sys::close(fd);
    match r {
        Ok(_) => rt::println!(
            "users password: the disk's passphrase (the same as the old password) is changed too"
        ),
        // EACCES: the disk's passphrase isn't the old login password; EINVAL / ENODEV: the
        // volume isn't encrypted / there is none.
        Err(sys::Errno(13)) | Err(sys::Errno(19)) | Err(sys::Errno(22)) => {}
        Err(e) => rt::println!("users password: the disk's passphrase didn't change ({e})"),
    }
}

/// `users switch [NAME] [--command COMMAND]` (was su): the same checks in the same order.
fn switch(name: &str, command: Option<&str>) -> i32 {
    let Some(target) = users::by_name(name) else {
        rt::eprintln!("users switch: there's no user called {name}");
        return 1;
    };
    if sys::geteuid() != 0 {
        rt::eprintln!("users switch: this program must be set-user-ID root (mode 4755)");
        return 1;
    }
    let ruid = sys::getuid();
    if ruid != 0 {
        let Some(pw) = users::read_password(&format!("Password for {}: ", target.name)) else {
            return 1;
        };
        if !users::verify(&target.name, &pw) {
            systools::audit(
                "password.fail",
                &format!("via=users-switch user={} uid={ruid}", target.name),
            );
            sys::sleep_ms(1000);
            rt::eprintln!("users switch: that's not the password for {}", target.name);
            return 1;
        }
    }
    if let Err(e) = users::become_user(&target) {
        rt::eprintln!("users switch: can't switch to {} ({e})", target.name);
        return 1;
    }
    let env: Vec<String> = users::login_env(&target);
    let env: Vec<&str> = env.iter().map(String::as_str).collect();
    let _ = sys::chdir(&target.home);
    let shell = if target.shell.is_empty() {
        "/Commands/msh"
    } else {
        target.shell.as_str()
    };
    let e = match command {
        Some(c) => sys::execve(shell, &[shell, "-c", c], &env),
        None => sys::execve(shell, &[shell], &env),
    };
    rt::eprintln!("users switch: {shell} can't be started ({e})");
    127
}
