//! id [-u|-g|-G|-n] [user]: user and group ids, like coreutils `id`.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use rt::users;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let mut what = ' ';
    let mut names = false;
    let mut user = None;
    for a in &args.argv[1..] {
        match *a {
            "-u" => what = 'u',
            "-g" => what = 'g',
            "-G" => what = 'G',
            "-n" => names = true,
            "-un" | "-nu" => (what, names) = ('u', true),
            "-gn" | "-ng" => (what, names) = ('g', true),
            "-Gn" | "-nG" => (what, names) = ('G', true),
            u if !u.starts_with('-') => user = Some(u),
            o => {
                rt::eprintln!("id: unknown option {o}");
                return 2;
            }
        }
    }
    let (ruid, euid, rgid, egid, groups) = match user {
        Some(u) => match users::by_name(u) {
            Some(p) => (p.uid, p.uid, p.gid, p.gid, users::groups_of(&p.name, p.gid)),
            None => {
                rt::eprintln!("id: {u}: no such user");
                return 1;
            }
        },
        None => {
            let mut g = alloc::vec![rt::sys::getegid()];
            for x in rt::sys::getgroups() {
                if !g.contains(&x) {
                    g.push(x);
                }
            }
            (
                rt::sys::getuid(),
                rt::sys::geteuid(),
                rt::sys::getgid(),
                rt::sys::getegid(),
                g,
            )
        }
    };
    let un = |u: u32| {
        if names {
            users::user_name(u)
        } else {
            alloc::format!("{u}")
        }
    };
    let gn = |g: u32| {
        if names {
            users::group_name(g)
        } else {
            alloc::format!("{g}")
        }
    };
    match what {
        'u' => rt::println!("{}", un(euid)),
        'g' => rt::println!("{}", gn(egid)),
        'G' => {
            let v: Vec<String> = groups.iter().map(|&g| gn(g)).collect();
            rt::println!("{}", v.join(" "));
        }
        _ => {
            let mut s = alloc::format!(
                "uid={ruid}({}) gid={rgid}({})",
                users::user_name(ruid),
                users::group_name(rgid)
            );
            if euid != ruid {
                s += &alloc::format!(" euid={euid}({})", users::user_name(euid));
            }
            if egid != rgid {
                s += &alloc::format!(" egid={egid}({})", users::group_name(egid));
            }
            let v: Vec<String> = groups
                .iter()
                .map(|&g| alloc::format!("{g}({})", users::group_name(g)))
                .collect();
            s += &alloc::format!(" groups={}", v.join(","));
            rt::println!("{s}");
        }
    }
    0
}
