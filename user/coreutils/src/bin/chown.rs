//! chown USER[:GROUP] FILE... / chown :GROUP FILE...: change owner and/or group (names
//! or numbers). Changing the owner needs root; an owner may change the group to one of
//! its own.
#![no_std]
#![no_main]
use rt::users;
rt::main!(main);
fn id(s: &str, by_name: impl Fn(&str) -> Option<u32>) -> Option<u32> {
    s.parse().ok().or_else(|| by_name(s))
}
fn main(args: &rt::Args) -> i32 {
    let (Some(spec), true) = (args.get(1), args.argv.len() > 2) else {
        rt::eprintln!("usage: chown USER[:GROUP] FILE...");
        return 2;
    };
    let (u, g) = spec.split_once(':').unwrap_or((spec, ""));
    let uid = if u.is_empty() {
        u32::MAX
    } else {
        match id(u, |n| users::by_name(n).map(|p| p.uid)) {
            Some(x) => x,
            None => {
                rt::eprintln!("chown: unknown user {u}");
                return 1;
            }
        }
    };
    let gid = if g.is_empty() {
        u32::MAX
    } else {
        match id(g, users::group_id) {
            Some(x) => x,
            None => {
                rt::eprintln!("chown: unknown group {g}");
                return 1;
            }
        }
    };
    let mut rc = 0;
    for f in &args.argv[2..] {
        if let Err(e) = rt::sys::chown(f, uid, gid) {
            rt::eprintln!("chown: {f}: {e}");
            rc = 1;
        }
    }
    rc
}
