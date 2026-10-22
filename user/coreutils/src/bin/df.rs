//! df [-h] [-a] [PATH...]: file system sizes and free space, from statfs(2).
//!
//! Without PATHs: every mount in /proc/mounts, skipping the ones with no storage of their
//! own (proc, sysfs, devfs) and repeats of the same file system (bind mounts like
//! /Users/milk); `-a` shows them all. Sizes in 1K blocks, `-h` human-readable. With
//! MILK_TABLE=1 the output is a table: mount, type, size, used, avail, use_pct, bsize,
//! blocks, bfree, bavail, files, ffree, ro.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use rt::io::flags;
use rt::sys::{self, StatFs};
rt::main!(main);

struct Row {
    mount: String,
    st: StatFs,
}

impl Row {
    fn size(&self) -> u64 {
        self.st.blocks * self.st.bsize
    }
    fn free(&self) -> u64 {
        self.st.bfree * self.st.bsize
    }
    fn avail(&self) -> u64 {
        self.st.bavail * self.st.bsize
    }
    fn used(&self) -> u64 {
        self.size() - self.free().min(self.size())
    }
    /// Like GNU df: used / (used + available), rounded up; None without storage.
    fn use_pct(&self) -> Option<u64> {
        let (u, a) = (self.used(), self.avail());
        (u + a > 0).then(|| (u * 100).div_ceil(u + a))
    }
}

fn mounts() -> Vec<String> {
    let m = sys::read_file("/proc/mounts").unwrap_or_default();
    String::from_utf8_lossy(&m)
        .lines()
        .filter_map(|l| l.split(' ').nth(1).map(String::from))
        .collect()
}

fn main(args: &rt::Args) -> i32 {
    let (f, paths) = flags(&args.argv[1..]);
    let (human, all) = (f.contains(&'h'), f.contains(&'a'));
    let mut rows = Vec::new();
    let mut status = 0;
    if paths.is_empty() {
        let mut seen: Vec<(u64, u64)> = Vec::new();
        for m in mounts() {
            let Ok(st) = sys::statfs(&m) else { continue };
            let id = (st.fs_type, st.fsid);
            if !all && (st.blocks == 0 || seen.contains(&id)) {
                continue;
            }
            seen.push(id);
            rows.push(Row { mount: m, st });
        }
    } else {
        for p in &paths {
            // fstatfs on the opened path (statfs if it can't be opened).
            let st = match sys::open(p, sys::O_RDONLY | sys::O_CLOEXEC) {
                Ok(fd) => {
                    let r = sys::fstatfs(fd);
                    let _ = sys::close(fd);
                    r
                }
                Err(_) => sys::statfs(p),
            };
            match st {
                Ok(st) => rows.push(Row {
                    mount: String::from(*p),
                    st,
                }),
                Err(e) => {
                    rt::eprintln!("df: {p}: {e}");
                    status = 1;
                }
            }
        }
    }
    if rt::table::requested(args) {
        use rt::table::{Table, Ty, Val};
        let mut t = Table::new(&[
            ("mount", Ty::Str),
            ("type", Ty::Str),
            ("size", Ty::Size),
            ("used", Ty::Size),
            ("avail", Ty::Size),
            ("use_pct", Ty::Int),
            ("bsize", Ty::Int),
            ("blocks", Ty::Int),
            ("bfree", Ty::Int),
            ("bavail", Ty::Int),
            ("files", Ty::Int),
            ("ffree", Ty::Int),
            ("ro", Ty::Str),
        ]);
        for r in &rows {
            t.push(alloc::vec![
                Val::str(&r.mount),
                Val::str(r.st.type_name()),
                Val::Int(r.size() as i64),
                Val::Int(r.used() as i64),
                Val::Int(r.avail() as i64),
                r.use_pct().map_or(Val::Nil, |p| Val::Int(p as i64)),
                Val::Int(r.st.bsize as i64),
                Val::Int(r.st.blocks as i64),
                Val::Int(r.st.bfree as i64),
                Val::Int(r.st.bavail as i64),
                Val::Int(r.st.files as i64),
                Val::Int(r.st.ffree as i64),
                Val::str(if r.st.read_only() { "ro" } else { "rw" }),
            ]);
        }
        t.write(&mut rt::io::Out::stdout());
        return status;
    }
    let show = |v: u64| -> String {
        if human {
            coreutils::human(v)
        } else {
            alloc::format!("{}", v / 1024)
        }
    };
    rt::println!(
        "{:<8} {:>10} {:>10} {:>10} {:>5}  Mounted on",
        "Type",
        if human { "Size" } else { "1K-blocks" },
        "Used",
        "Avail",
        "Use%"
    );
    for r in &rows {
        rt::println!(
            "{:<8} {:>10} {:>10} {:>10} {:>5}  {}{}",
            r.st.type_name(),
            show(r.size()),
            show(r.used()),
            show(r.avail()),
            r.use_pct()
                .map_or(String::from("-"), |p| alloc::format!("{p}%")),
            r.mount,
            if r.st.read_only() { " (ro)" } else { "" }
        );
    }
    status
}
