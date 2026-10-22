//! auth [-S] [-u ADMIN] COMMAND [ARGS...]: run COMMAND with an admin authorisation ticket
//! (5 minutes, this process only, kept across the exec), after an administrator's
//! password: the terminal fallback for the desktop's trusted panel. The command keeps your
//! own user id; the ticket only unlocks what asks for one (the destructive
//! /System/Kernel/fs/milkfs controls, `fs::milkfs::DESTRUCTIVE`: delete, rollback, discard, promote,
//! receive, send, lock, passwd, check repair, ...). -S reads the password from stdin (scripts). `auth -s`: this process's
//! ticket (`auth /Commands/auth -s` shows a fresh one). Unlike sudo, COMMAND runs as you.
#![no_std]
#![no_main]
extern crate alloc;
use rt::{sys, users};
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let mut user = None;
    let mut i = 1;
    while let Some(a) = args.argv.get(i) {
        match *a {
            "-S" => {}
            "-s" => {
                let ms = sys::auth_left();
                if ms == 0 {
                    rt::println!("no ticket");
                } else {
                    rt::println!("ticket: {} s left", ms / 1000);
                }
                return 0;
            }
            "-u" => {
                i += 1;
                user = args.argv.get(i).copied();
            }
            _ => break,
        }
        i += 1;
    }
    let cmd = &args.argv[i.min(args.argv.len())..];
    if cmd.is_empty() {
        rt::eprintln!("usage: auth [-S] [-u ADMIN] COMMAND [ARGS...] | auth -s");
        return 2;
    }
    let who = user.map_or_else(
        || users::user_name(sys::getuid()),
        alloc::string::String::from,
    );
    let Some(pw) = users::read_password(&alloc::format!("Password for {who} (administrator): "))
    else {
        return 1;
    };
    match sys::authorize_password(&pw, user.unwrap_or("")) {
        Ok(_) => {}
        Err(sys::Errno(13)) => {
            rt::eprintln!("auth: wrong password");
            return 1;
        }
        Err(sys::Errno(11)) => {
            rt::eprintln!("auth: too many wrong passwords; try again later");
            return 1;
        }
        Err(sys::Errno(1)) => {
            rt::eprintln!("auth: {who} is not an administrator");
            return 1;
        }
        Err(e) => {
            rt::eprintln!("auth: not authorised ({e})");
            return 1;
        }
    }
    let path = if cmd[0].contains('/') {
        alloc::string::String::from(cmd[0])
    } else {
        alloc::format!("/Commands/{}", cmd[0])
    };
    let e = sys::execve(&path, cmd, &args.envp);
    rt::eprintln!("auth: {}: {e}", cmd[0]);
    127
}
