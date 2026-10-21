//! passwd [user]: change a password. Users change their own (they type the current one
//! first); root changes anyone's. Writes /etc/shadow (0600, salted `$5m$` hash) and a
//! copy on the milkfs disk so it survives reboots. Set-user-ID root.
#![no_std]
#![no_main]
extern crate alloc;
use rt::{sys, users};
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let ruid = sys::getuid();
    let me = users::user_name(ruid);
    let user: &str = args.get(1).unwrap_or(me.as_str());
    if users::by_name(user).is_none() {
        rt::eprintln!("passwd: user {user} does not exist");
        return 1;
    }
    if sys::geteuid() != 0 {
        rt::eprintln!("passwd: must be set-user-ID root");
        return 1;
    }
    if ruid != 0 {
        if user != me {
            rt::eprintln!("passwd: you may only change your own password");
            return 1;
        }
        let Some(old) = users::read_password(&alloc::format!("Current password for {me}: ")) else {
            return 1;
        };
        if !users::verify(&me, &old) {
            sys::sleep_ms(1000);
            rt::eprintln!("passwd: Authentication failure");
            return 1;
        }
    }
    let (Some(a), Some(b)) = (
        users::read_password("New password: "),
        users::read_password("Retype new password: "),
    ) else {
        return 1;
    };
    if a != b {
        rt::eprintln!("passwd: passwords do not match");
        return 1;
    }
    if a.is_empty() {
        rt::eprintln!("passwd: empty password refused");
        return 1;
    }
    match users::set_shadow_hash(user, &users::make_hash(&a)) {
        Ok(()) => {
            rt::println!("passwd: password for {user} updated");
            0
        }
        Err(e) => {
            rt::eprintln!("passwd: cannot write /etc/shadow: {e}");
            1
        }
    }
}
