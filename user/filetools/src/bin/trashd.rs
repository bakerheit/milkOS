//! trashd: empties the Trash on its own. Anything that has been in a Trash longer than its
//! owner's Settings > Trash says (`trash = 30` in ~/.config/milk/settings; 30 days unless
//! changed, `never` keeps everything) is deleted for good, at boot and then about once a
//! day. A milkd service (/System/Settings/milk/services/trashd.service).
//!
//!   trashd [-q] [--once] [--later DAYS]
//!
//! As root it does every home's Trash (/Users/*/.Trash) and every disk's
//! (/Volumes/*/.Trash), each as the person whose things they are (a forked child that has
//! become them), so it never deletes with more rights than they have. Run by anyone else it
//! does only theirs. `--once`: one pass, then exit. `--later DAYS`: pretend it is DAYS days
//! later than it is (for tests).
#![no_std]
#![no_main]
extern crate alloc;

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use filekit::path;
use filekit::trash::{DAY, Keep, TRASH_DIR};
use filekit::words::count;
use rt::sys;

rt::main!(main);

struct Run {
    quiet: bool,
    now: i64,
}

fn main(args: &rt::Args) -> i32 {
    let a = &args.argv[1..];
    let mut quiet = false;
    let mut once = false;
    let mut later = 0i64;
    let mut i = 0;
    while i < a.len() {
        match a[i] {
            "-q" | "--quiet" => quiet = true,
            "--once" => once = true,
            "--later" => {
                i += 1;
                match a.get(i).and_then(|d| d.parse::<i64>().ok()) {
                    Some(d) => later = d,
                    None => {
                        rt::eprintln!("trashd: --later wants a number of days");
                        return 2;
                    }
                }
            }
            other => {
                rt::eprintln!("usage: trashd [-q] [--once] [--later DAYS]   (not {other})");
                return 2;
            }
        }
        i += 1;
    }
    let home = filetools::home(args);
    loop {
        let r = Run {
            quiet,
            now: filetools::now() + later * DAY,
        };
        if sys::geteuid() == 0 {
            r.everyone();
        } else {
            r.mine(&home);
        }
        if once {
            return 0;
        }
        // About once a day (a sleep is cheap; the box may well be off by then).
        for _ in 0..24 {
            sys::sleep_ms(3600 * 1000);
        }
    }
}

impl Run {
    /// One Trash: delete what is due, as whoever runs this.
    fn one(&self, dir: &str, keep: Keep, uid: Option<u32>) {
        let e = filetools::expire(dir, keep, self.now, uid);
        if e.deleted > 0 || e.cleaned > 0 || !self.quiet {
            let how = match keep {
                Keep::Forever => String::from("kept forever"),
                k => format!("after {}", k.label()),
            };
            let mut msg = format!(
                "{dir}: deleted {} for good ({how})",
                count(e.deleted, "item", "items")
            );
            if e.cleaned > 0 {
                msg.push_str(&format!(
                    ", cleaned up {}",
                    count(e.cleaned, "leftover record", "leftover records")
                ));
            }
            rt::println!("trashd: {msg}");
        }
        for f in e.failed {
            rt::println!("trashd: {dir}: {f}");
        }
    }

    /// Not root: your own home's Trash and your things in the disks' Trashes.
    fn mine(&self, home: &str) {
        let keep = filetools::keep_for(home);
        let uid = sys::getuid();
        for (dir, shared) in filetools::trashes(home) {
            self.one(&dir, keep, shared.then_some(uid));
        }
    }

    /// Root: as each owner in turn.
    fn everyone(&self) {
        // Homes: /Users/<name>/.Trash, as the home folder's owner.
        let mut homes: Vec<String> = sys::read_dir("/Users")
            .unwrap_or_default()
            .into_iter()
            .filter(|(n, _)| n != "." && n != "..")
            .map(|(n, _)| path::join("/Users", &n))
            .collect();
        for p in rt::users::all() {
            if p.home.starts_with('/') && p.home != "/" && !homes.contains(&p.home) {
                homes.push(p.home);
            }
        }
        for h in homes {
            let t = path::join(&h, TRASH_DIR);
            let Ok(st) = sys::stat_path(&h, true) else {
                continue;
            };
            if !filetools::is_folder(&t) {
                continue;
            }
            let keep = filetools::keep_for(&h);
            self.as_user(st.uid, st.gid, || self.one(&t, keep, None));
        }
        // Disks: /Volumes/<disk>/.Trash, each person's items as them, with their settings.
        for dir in filetools::disk_trashes() {
            let owners: BTreeSet<u32> = filetools::read_trash(&dir)
                .iter()
                .map(|it| it.uid())
                .collect();
            for uid in owners {
                let pw = rt::users::by_uid(uid);
                let keep = pw
                    .as_ref()
                    .map_or(Keep::DEFAULT, |p| filetools::keep_for(&p.home));
                let gid = pw.as_ref().map_or(uid, |p| p.gid);
                self.as_user(uid, gid, || self.one(&dir, keep, Some(uid)));
            }
        }
    }

    /// Run `f` as `uid` (a child that becomes them; root runs it directly).
    fn as_user(&self, uid: u32, gid: u32, f: impl FnOnce()) {
        if uid == 0 {
            f();
            return;
        }
        match sys::fork() {
            Ok(0) => {
                let groups = rt::users::by_uid(uid)
                    .map(|p| rt::users::groups_of(&p.name, p.gid))
                    .unwrap_or_else(|| alloc::vec![gid]);
                let became = sys::setgroups(&groups)
                    .and_then(|_| sys::setresgid(gid, gid, gid))
                    .and_then(|_| sys::setresuid(uid, uid, uid));
                if let Err(e) = became {
                    rt::println!("trashd: can't become user {uid}: {e}");
                    sys::exit(1);
                }
                f();
                sys::exit(0);
            }
            Ok(pid) => {
                let _ = sys::wait4(pid as i32, 0);
            }
            Err(e) => rt::println!("trashd: fork: {e}"),
        }
    }
}
