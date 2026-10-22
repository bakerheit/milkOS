//! sudo [-S] [-u user] command [args...]: run a command as root (or `user`) if you are an
//! administrator (group `admin`, gid 80), after typing *your own* password. `-S` reads the
//! password from stdin without a prompt check (for scripts). Set-user-ID root.
//! No credential caching: every sudo asks.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use rt::{sys, users};
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let mut target = "root";
    let mut i = 1;
    while let Some(a) = args.argv.get(i) {
        match *a {
            "-S" => {}
            "-u" => {
                i += 1;
                target = args.argv.get(i).copied().unwrap_or("root");
            }
            _ => break,
        }
        i += 1;
    }
    let cmdv = &args.argv[i.min(args.argv.len())..];
    if cmdv.is_empty() {
        rt::eprintln!("usage: sudo [-S] [-u user] command [args...]");
        return 2;
    }
    if sys::geteuid() != 0 {
        rt::eprintln!("sudo: must be set-user-ID root");
        return 1;
    }
    let me = users::user_name(sys::getuid());
    let Some(to) = users::by_name(target) else {
        rt::eprintln!("sudo: unknown user {target}");
        return 1;
    };
    if sys::getuid() != 0 {
        let admin = users::by_name(&me)
            .is_some_and(|p| users::groups_of(&p.name, p.gid).contains(&users::ADMIN_GID));
        if !admin {
            rt::eprintln!("sudo: {me} is not an administrator (group admin)");
            return 1;
        }
        let mut ok = false;
        for _ in 0..3 {
            let Some(pw) = users::read_password(&alloc::format!("Password for {me}: ")) else {
                break;
            };
            if users::verify(&me, &pw) {
                ok = true;
                break;
            }
            sys::sleep_ms(1000);
            rt::eprintln!("Sorry, try again.");
        }
        if !ok {
            rt::eprintln!("sudo: incorrect password");
            return 1;
        }
    }
    if let Err(e) = users::become_user(&to) {
        rt::eprintln!("sudo: cannot become {}: {e}", to.name);
        return 1;
    }
    // Keep the caller's environment, but say who we are now.
    let mut env: Vec<String> = args
        .envp
        .iter()
        .filter(|e| !e.starts_with("USER=") && !e.starts_with("LOGNAME="))
        .map(|e| String::from(*e))
        .collect();
    env.push(alloc::format!("USER={}", to.name));
    env.push(alloc::format!("SUDO_USER={me}"));
    let env: Vec<&str> = env.iter().map(String::as_str).collect();
    let prog = cmdv[0];
    let path = if prog.contains('/') {
        String::from(prog)
    } else {
        alloc::format!("/Commands/{prog}")
    };
    let e = sys::execve(&path, cmdv, &env);
    rt::eprintln!("sudo: {prog}: {e}");
    127
}
