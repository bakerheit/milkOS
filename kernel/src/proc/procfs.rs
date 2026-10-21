//! The process half of /proc: `/proc/<pid>/` for every process, `/proc/self`, and
//! `/proc/threads` (every thread, kernel ones included, for `ps -k`).
//!
//! ```text
//! /proc/<pid>/status    Linux-style "Key:\tvalue" lines (Name, State, Pid, PPid, Pgid,
//!                       Sid, VmRSS, SigPnd/SigBlk/SigIgn/SigCgt, plus milkOS CpuNs, Tid)
//! /proc/<pid>/stat      the one-line Linux format (pid (comm) state ppid pgrp ... rss)
//! /proc/<pid>/cmdline   argv, NUL separated
//! /proc/<pid>/maps      start-end perms offset dev inode name, one line per region
//! /proc/<pid>/fd/N      symlinks: "/dev/console", "/home/milk/x", "pipe:[3]"
//! /proc/<pid>/cwd, exe  symlinks
//! /proc/self            symlink to the caller's pid
//! /proc/threads         TID PID STATE PRIO CPU_NS NAME for every thread
//! ```
//! Directories are added when a process starts and removed when it is reaped.

use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::fmt::Write;

use super::{PState, Pid, Process, get};
use crate::fs::node::{DirEntry, Inode, InodeRef, InodeType, Stat};
use crate::fs::synth::SynthDir;
use crate::fs::{self, Errno, procfs};
use crate::{sched, timer};

/// A symlink whose target is computed on each readlink.
struct DynLink {
    dev: u64,
    ino: u64,
    target: alloc::boxed::Box<dyn Fn() -> fs::Result<String> + Send + Sync>,
}

impl Inode for DynLink {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn kind(&self) -> InodeType {
        InodeType::Symlink
    }
    fn stat(&self) -> Stat {
        Stat::new(self.dev, self.ino, InodeType::Symlink, 0o777)
    }
    fn readlink(&self) -> fs::Result<String> {
        (self.target)()
    }
}

fn link(dir: &SynthDir, f: impl Fn() -> fs::Result<String> + Send + Sync + 'static) -> InodeRef {
    Arc::new(DynLink {
        dev: dir.dev(),
        ino: dir.ids().next(),
        target: alloc::boxed::Box::new(f),
    })
}

/// `/proc/<pid>/fd`: one symlink per open fd.
struct FdDir {
    pid: Pid,
    dev: u64,
    ino: u64,
    parent: Weak<SynthDir>,
}

/// What an fd points at, as Linux prints it.
pub fn describe_file(f: &fs::OpenFile) -> String {
    if let Some(p) = fs::node::downcast::<super::pipe::PipeEnd>(f.inode()) {
        return p.label();
    }
    if let Ok(p) = fs::path_of(f.inode()) {
        return p;
    }
    // Device nodes: find the /dev entry with the same device number.
    let st = f.stat();
    if matches!(st.kind, InodeType::CharDev | InodeType::BlockDev) {
        for e in fs::readdir("/dev").unwrap_or_default() {
            let path = format!("/dev/{}", e.name);
            if fs::stat(&path).is_ok_and(|s| s.rdev == st.rdev && s.kind == st.kind) {
                return path;
            }
        }
    }
    format!("anon_inode:[{}]", f.inode().ino())
}

impl Inode for FdDir {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn kind(&self) -> InodeType {
        InodeType::Dir
    }
    fn stat(&self) -> Stat {
        Stat::new(self.dev, self.ino, InodeType::Dir, 0o500)
    }
    fn lookup(&self, name: &str) -> fs::Result<InodeRef> {
        if name == ".." {
            return self
                .parent
                .upgrade()
                .map(|p| p as InodeRef)
                .ok_or(Errno::ENOENT);
        }
        let fd: i64 = name.parse().map_err(|_| Errno::ENOENT)?;
        let p = get(self.pid).ok_or(Errno::ENOENT)?;
        let file = p.inner.lock().fds.get(fd).map_err(|_| Errno::ENOENT)?;
        Ok(Arc::new(DynLink {
            dev: self.dev,
            ino: 0x10_0000 + fd as u64,
            target: alloc::boxed::Box::new(move || Ok(describe_file(&file))),
        }))
    }
    fn readdir(&self) -> fs::Result<Vec<DirEntry>> {
        let p = get(self.pid).ok_or(Errno::ENOENT)?;
        let fds = p.inner.lock().fds.list();
        Ok(fds
            .into_iter()
            .map(|(fd, _)| DirEntry {
                name: format!("{fd}"),
                ino: 0x10_0000 + fd as u64,
                kind: InodeType::Symlink,
            })
            .collect())
    }
}

fn state_of(p: &Process) -> (char, &'static str) {
    let (state, tid) = {
        let i = p.inner.lock();
        (i.state, i.tid)
    };
    if let PState::Zombie(_) = state {
        return ('Z', "zombie");
    }
    match sched::threads()
        .iter()
        .find(|t| t.tid == tid)
        .map(|t| t.state)
    {
        Some(sched::State::Running) | Some(sched::State::Ready) => ('R', "running"),
        Some(sched::State::Dead) => ('Z', "zombie"),
        _ => ('S', "sleeping"),
    }
}

fn cpu_ns(p: &Process) -> u64 {
    let tid = p.inner.lock().tid;
    sched::threads()
        .iter()
        .find(|t| t.tid == tid)
        .map_or(0, |t| t.cpu_ns)
}

fn status(pid: Pid) -> String {
    let Some(p) = get(pid) else {
        return String::new();
    };
    let (st, st_name) = state_of(&p);
    let cpu = cpu_ns(&p);
    let i = p.inner.lock();
    let rss = i.space.as_ref().map_or(0, |s| s.resident()) / 1024;
    let (mut ign, mut cgt) = (0u64, 0u64);
    for (n, a) in i.sig.actions.iter().enumerate() {
        match a.handler {
            super::signal::SIG_DFL => {}
            super::signal::SIG_IGN => ign |= 1 << n,
            _ => cgt |= 1 << n,
        }
    }
    let mut s = String::new();
    let _ = writeln!(s, "Name:\t{}", i.name);
    let _ = writeln!(s, "State:\t{st} ({st_name})");
    let _ = writeln!(s, "Tgid:\t{pid}");
    let _ = writeln!(s, "Pid:\t{pid}");
    let _ = writeln!(s, "PPid:\t{}", i.ppid);
    let _ = writeln!(s, "Pgid:\t{}", i.pgid);
    let _ = writeln!(s, "Sid:\t{}", i.sid);
    let _ = writeln!(s, "Uid:\t0\t0\t0\t0");
    let _ = writeln!(s, "Gid:\t0\t0\t0\t0");
    let _ = writeln!(s, "FDSize:\t{}", i.fds.count());
    let _ = writeln!(s, "VmRSS:\t{rss} kB");
    let _ = writeln!(s, "Threads:\t1");
    let _ = writeln!(s, "SigPnd:\t{:016x}", i.sig.pending);
    let _ = writeln!(s, "SigBlk:\t{:016x}", i.sig.blocked);
    let _ = writeln!(s, "SigIgn:\t{ign:016x}");
    let _ = writeln!(s, "SigCgt:\t{cgt:016x}");
    let _ = writeln!(s, "Tid:\t{}", i.tid);
    let _ = writeln!(s, "CpuNs:\t{cpu}");
    let _ = writeln!(s, "StartNs:\t{}", i.start_ns);
    s
}

fn stat_line(pid: Pid) -> String {
    let Some(p) = get(pid) else {
        return String::new();
    };
    let (st, _) = state_of(&p);
    let ticks = cpu_ns(&p) / (1_000_000_000 / timer::TICK_HZ);
    let i = p.inner.lock();
    let (vsize, rss) = i
        .space
        .as_ref()
        .map_or((0, 0), |s| (s.resident(), s.resident() / 4096));
    let start = i.start_ns / (1_000_000_000 / timer::TICK_HZ);
    // pid (comm) state ppid pgrp session tty_nr tpgid flags minflt cminflt majflt cmajflt
    // utime stime cutime cstime priority nice num_threads itrealvalue starttime vsize rss
    format!(
        "{pid} ({}) {st} {} {} {} 0 -1 0 0 0 0 0 {ticks} 0 0 0 20 0 1 0 {start} {vsize} {rss}\n",
        i.name, i.ppid, i.pgid, i.sid
    )
}

fn cmdline(pid: Pid) -> String {
    let Some(p) = get(pid) else {
        return String::new();
    };
    let i = p.inner.lock();
    let mut s = String::new();
    for a in &i.argv {
        s.push_str(a);
        s.push('\0');
    }
    s
}

fn maps(pid: Pid) -> String {
    let Some(p) = get(pid) else {
        return String::new();
    };
    let i = p.inner.lock();
    let Some(space) = i.space.as_ref() else {
        return String::new();
    };
    let mut s = String::new();
    for r in space.regions() {
        let perms = format!(
            "r{}{}p",
            if r.perm.write { 'w' } else { '-' },
            if r.perm.exec { 'x' } else { '-' }
        );
        let _ = writeln!(
            s,
            "{:x}-{:x} {perms} 00000000 00:00 0{:>26}",
            r.start,
            r.end,
            match r.kind {
                super::uspace::RegionKind::Image => i.exe.as_str(),
                super::uspace::RegionKind::Heap => "[heap]",
                super::uspace::RegionKind::Stack => "[stack]",
                super::uspace::RegionKind::Anon => "",
            }
        );
    }
    s
}

/// `/proc/threads`.
fn threads() -> String {
    let mut s = String::from("  TID   PID S PRIO        CPU_NS NAME\n");
    for t in sched::threads() {
        let _ = writeln!(
            s,
            "{:>5} {:>5} {} {:>4} {:>13} {}",
            t.tid,
            t.pid,
            t.state.letter(),
            t.prio as u8,
            t.cpu_ns,
            t.name
        );
    }
    s
}

/// Install /proc/self and /proc/threads. After fs::init.
pub fn init() {
    let Some(root) = procfs::root() else { return };
    let selfl = link(&root, || Ok(format!("{}", sched::current_pid())));
    root.add("self", selfl);
    root.add_file("threads", threads);
}

/// Create /proc/<pid>.
pub fn add(pid: Pid) {
    let Some(root) = procfs::root() else { return };
    let d = root.add_dir(&format!("{pid}"));
    d.add_file("status", move || status(pid));
    d.add_file("stat", move || stat_line(pid));
    d.add_file("cmdline", move || cmdline(pid));
    d.add_file("maps", move || maps(pid));
    let cwd = link(&d, move || {
        let p = get(pid).ok_or(Errno::ENOENT)?;
        let c = p.inner.lock().cwd.clone();
        fs::path_of(&c)
    });
    d.add("cwd", cwd);
    let exe = link(&d, move || {
        let p = get(pid).ok_or(Errno::ENOENT)?;
        Ok(p.inner.lock().exe.clone())
    });
    d.add("exe", exe);
    let fd = Arc::new(FdDir {
        pid,
        dev: d.dev(),
        ino: d.ids().next(),
        parent: Arc::downgrade(&d),
    });
    d.add("fd", fd);
}

/// Remove /proc/<pid> (the process was reaped).
pub fn remove(pid: Pid) {
    if let Some(root) = procfs::root() {
        root.remove(&format!("{pid}"));
    }
}
