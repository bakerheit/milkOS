//! su [-] [user] [-c command]: run a shell (or one command) as another user (default
//! root) after asking for *that* user's password. Root needs no password. Set-user-ID
//! root (initramfs.list mode 4755). Every attempt is audited by the kernel's id changes.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use rt::{sys, users};
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let mut user = "root";
    let mut cmd: Option<&str> = None;
    let mut it = args.argv[1..].iter();
    while let Some(a) = it.next() {
        match *a {
            "-" | "-l" | "--login" => {}
            "-c" => cmd = it.next().copied(),
            u if !u.starts_with('-') => user = u,
            o => {
                rt::eprintln!("su: unknown option {o}");
                return 2;
            }
        }
    }
    let Some(target) = users::by_name(user) else {
        rt::eprintln!("su: user {user} does not exist");
        return 1;
    };
    if sys::geteuid() != 0 {
        rt::eprintln!("su: must be set-user-ID root");
        return 1;
    }
    if sys::getuid() != 0 {
        let Some(pw) = users::read_password("Password: ") else {
            return 1;
        };
        if !users::verify(&target.name, &pw) {
            sys::sleep_ms(1000);
            rt::eprintln!("su: Authentication failure");
            return 1;
        }
    }
    if let Err(e) = users::become_user(&target) {
        rt::eprintln!("su: cannot switch to {}: {e}", target.name);
        return 1;
    }
    let env: Vec<String> = users::login_env(&target);
    let env: Vec<&str> = env.iter().map(String::as_str).collect();
    let _ = sys::chdir(&target.home);
    let shell = if target.shell.is_empty() {
        "/bin/msh"
    } else {
        target.shell.as_str()
    };
    let e = match cmd {
        Some(c) => sys::execve(shell, &[shell, "-c", c], &env),
        None => sys::execve(shell, &[shell], &env),
    };
    rt::eprintln!("su: {shell}: {e}");
    127
}
