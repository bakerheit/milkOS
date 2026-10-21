//! User processes: one address space + fd table + one thread each, Linux-style lifecycle
//! (fork/clone, execve, exit, wait4, reparenting to init, zombies).
//!
//! Each process runs at EL0 in its own [`uspace::UserSpace`] (TTBR0 + ASID); the kernel
//! lives in the higher half (TTBR1), so all of TTBR0 is the process's. Syscalls use the
//! Linux aarch64 ABI and numbers ([`syscall`]). Faults kill the process with a message,
//! never the kernel. See docs/processes.md.

#![allow(dead_code)]

pub mod asid;
pub mod caps;
pub mod cred;
pub mod elf;
pub mod errno;
pub mod fd;
pub mod futex;
pub mod login;
pub mod pipe;
pub mod procfs;
mod selftest;
pub mod signal;
pub mod sock;
pub mod syscall;
pub mod tty;
pub mod uspace;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::exceptions::{self, TrapFrame};
use crate::fs::{self, InodeRef};
use crate::sched::{self, Comm, FpState, Tid, UserState, WaitQueue};
use crate::sync::SpinLock;
use crate::{kprintln, mmu, timer};
use errno::*;
use fd::FdTable;
use uspace::UserSpace;

pub type Pid = u32;

/// Linux wait status encodings.
pub fn wstatus_exited(code: i32) -> i32 {
    (code & 0xff) << 8
}
pub fn wstatus_signaled(sig: i32) -> i32 {
    sig & 0x7f
}

pub const SIGILL: i32 = 4;
pub const SIGTRAP: i32 = 5;
pub const SIGBUS: i32 = 7;
pub const SIGFPE: i32 = 8;
pub const SIGSEGV: i32 = 11;

fn signame(sig: i32) -> &'static str {
    match sig {
        SIGILL => "SIGILL",
        SIGTRAP => "SIGTRAP",
        SIGBUS => "SIGBUS",
        SIGFPE => "SIGFPE",
        SIGSEGV => "SIGSEGV",
        _ => "signal",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PState {
    Alive,
    /// Exited with this wait status; waiting for the parent's wait4.
    Zombie(i32),
}

pub struct Process {
    pub pid: Pid,
    pub inner: SpinLock<Inner>,
    /// The parent sleeps here in wait4; children kick it when they exit.
    pub child_wq: WaitQueue,
    /// User / group ids (shared by the thread group, inherited; see cred.rs). Its own lock,
    /// never held across anything else: the VFS reads it on every permission check, also
    /// from code that holds `inner`.
    pub cred: SpinLock<cred::Cred>,
}

pub struct Inner {
    /// 0 = started by the kernel.
    pub ppid: Pid,
    pub name: Comm,
    pub state: PState,
    /// None once the process has exited.
    pub space: Option<UserSpace>,
    pub fds: FdTable,
    pub children: Vec<Pid>,
    /// Group-leader thread (= the pid's main thread).
    pub tid: Tid,
    /// Every live thread of this process (the leader included).
    pub threads: Vec<Tid>,
    pub cwd: InodeRef,
    /// This process's root directory (chroot); None = the real root.
    pub root_dir: Option<InodeRef>,
    pub start_ns: u64,
    /// Process group and session (job control).
    pub pgid: Pid,
    pub sid: Pid,
    pub sig: signal::SigState,
    /// Command line of the current image (for /proc/<pid>/cmdline).
    pub argv: Vec<String>,
    /// Absolute path of the current image (for /proc/<pid>/exe).
    pub exe: String,
    /// Set by exit_group / a fatal signal: every thread drains and the process zombifies
    /// with this wait status.
    pub group_exit: Option<i32>,
    /// Ever had more than one thread (so address-space teardown needs a TLB shootdown).
    pub ever_threaded: bool,
    /// Capability set (sandbox); shared by the thread group, inherited, only shrinks.
    pub caps: alloc::sync::Arc<caps::Caps>,
}

static PROCS: SpinLock<BTreeMap<Pid, Arc<Process>>> = SpinLock::new(BTreeMap::new());
/// PID 1 is reserved for init (start_init); everything else counts up from 2.
static NEXT_PID: AtomicU32 = AtomicU32::new(2);
static INIT_NEXT: AtomicBool = AtomicBool::new(false);
/// Kernel threads waiting for processes they started (ppid 0).
static KERNEL_CHILD_WQ: WaitQueue = WaitQueue::new();

/// Processes created since boot (fork + spawn), for /proc/stat.
pub fn forks() -> u32 {
    NEXT_PID.load(Ordering::Relaxed) - 1
}

/// Forget a reaped process: process table and /proc/<pid>.
fn release(pid: Pid) {
    PROCS.lock().remove(&pid);
    procfs::remove(pid);
}

pub fn get(pid: Pid) -> Option<Arc<Process>> {
    PROCS.lock().get(&pid).cloned()
}

/// The calling thread's process root (chroot), for the VFS resolver. None for kernel
/// threads and un-chrooted processes.
fn current_root_dir() -> Option<InodeRef> {
    let pid = sched::current_pid();
    if pid == 0 {
        return None;
    }
    get(pid).and_then(|p| p.inner.lock().root_dir.clone())
}

/// Capability check for opening a device by path (audio / clipboard / windows). EACCES if
/// the matching capability was dropped. No-op for non-device paths.
pub fn dev_caps_check(path: &str) -> KResult<()> {
    let caps = current().inner.lock().caps.clone();
    let (cap, name) = match path {
        "/dev/audio" | "/dev/dsp" => (caps.audio, "audio"),
        "/dev/clipboard" => (caps.clipboard, "clipboard"),
        "/dev/fb0" | "/dev/input" | "/dev/wm" => (caps.windows, "windows"),
        _ => return Ok(()),
    };
    if cap {
        Ok(())
    } else {
        Err(caps::deny(
            "open",
            &alloc::format!("dev={name} path={path}"),
        ))
    }
}

/// Who the VFS should check permissions for: the calling process's effective ids, or the
/// kernel (root) for kernel threads.
fn current_who() -> fs::perm::Who {
    let pid = sched::current_pid();
    let Some(p) = (pid != 0).then(|| get(pid)).flatten() else {
        return fs::perm::Who::KERNEL;
    };
    let c = *p.cred.lock();
    let mut groups = [0u32; 16];
    let g = c.groups();
    groups[..g.len()].copy_from_slice(g);
    fs::perm::Who {
        uid: c.euid,
        gid: c.egid,
        groups,
        ngroups: g.len() as u8,
        kernel: false,
    }
}

/// Credentials of the calling process (root for kernel threads).
pub fn current_cred() -> cred::Cred {
    let pid = sched::current_pid();
    (pid != 0)
        .then(|| get(pid))
        .flatten()
        .map_or(cred::Cred::root(), |p| *p.cred.lock())
}

/// The calling thread's capability set.
pub fn current_caps() -> alloc::sync::Arc<caps::Caps> {
    current().inner.lock().caps.clone()
}

/// Replace the current process's capability set (sandbox: only ever shrinks, enforced by
/// the caller computing the intersection).
pub fn set_caps(c: caps::Caps) {
    let p = current();
    p.cred.lock().no_new_privs = true; // sandboxed: no sudo / su way out
    p.inner.lock().caps = alloc::sync::Arc::new(c);
}

/// A user's grant of one file to process `pid` (a drag and drop onto its window, the
/// desktop's counterpart of the powerbox): the same `caps.with_fs_grant` as MILK_POWERBOX,
/// and the file opened into the process (returns the fd). Audited as
/// `powerbox.grant pid= path= mode= via=`.
pub fn grant_file(pid: Pid, path: &str, write: bool, via: &str) -> KResult<usize> {
    let p = get(pid).ok_or(ESRCH)?;
    let flags = if write {
        fs::abi::O_RDWR
    } else {
        fs::abi::O_RDONLY
    };
    let f = alloc::sync::Arc::new(fs::open(path, flags, 0)?);
    let fd = {
        let mut i = p.inner.lock();
        let grant = caps::FsGrant {
            prefix: String::from(path),
            write,
        };
        i.caps = alloc::sync::Arc::new(i.caps.with_fs_grant(grant));
        i.fds.alloc(f, false)? as usize
    };
    crate::audit::record(
        "powerbox.grant",
        &alloc::format!(
            "pid={pid} path={path} mode={} via={via}",
            if write { "rw" } else { "r" }
        ),
    );
    Ok(fd)
}

/// chroot: make `dir` the calling process's root. The cwd is left as is (as on Linux).
pub fn chroot(dir: InodeRef) -> KResult<()> {
    if dir.kind() != fs::InodeType::Dir {
        return Err(ENOTDIR);
    }
    let p = current();
    // Anyone may chroot (it only narrows what they see; `milk run-try` relies on it), but
    // afterwards set-ID bits are ignored: a jail can't fake /etc/passwd for `su`.
    p.cred.lock().no_new_privs = true;
    p.inner.lock().root_dir = Some(dir);
    Ok(())
}

/// The process of the calling thread. Panics on a kernel thread.
pub fn current() -> Arc<Process> {
    let pid = sched::current_pid();
    get(pid).expect("current(): not a user thread")
}

pub fn all() -> Vec<Arc<Process>> {
    PROCS.lock().values().cloned().collect()
}

/// Set up the process layer: syscalls, user-fault handling, the console. Runs the EL0
/// smoke test. Call after `sched::init`.
pub fn init() {
    tty::init();
    procfs::init();
    syscall::init();
    exceptions::set_user_fault_handler(on_user_fault);
    fs::set_root_resolver(current_root_dir);
    fs::perm::set_cred_resolver(current_who);
    cred::selftest();
    login::init();
    login::selftest();
    let sess = login::session();
    fs::perm::set_mount_owner(sess.uid, sess.gid);
    exceptions::set_user_return_hook(on_user_return);
    let bin: Vec<String> = fs::readdir("/bin")
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.name)
        .collect();
    kprintln!(
        "[proc] up: user range {:#x}.., programs linked at {:#x}, /bin: {}",
        mmu::USER_BASE,
        uspace::IMAGE_BASE,
        bin.join(" ")
    );
    selftest::run();
}

/// Everything a new process starts with.
struct Launch<'a> {
    name: &'a str,
    ppid: Pid,
    /// None: a new process group / session led by the new process.
    pgid: Option<Pid>,
    sid: Option<Pid>,
    space: UserSpace,
    fds: FdTable,
    cwd: InodeRef,
    root_dir: Option<InodeRef>,
    frame: &'a TrapFrame,
    tpidr_el0: u64,
    fp: Box<FpState>,
    sig: signal::SigState,
    argv: Vec<String>,
    exe: String,
    clear_child_tid: usize,
    caps: alloc::sync::Arc<caps::Caps>,
    cred: cred::Cred,
}

/// Create a process and start its thread with `l.frame`.
fn start(l: Launch) -> KResult<Pid> {
    let pid = if INIT_NEXT.swap(false, Ordering::Relaxed) {
        1
    } else {
        NEXT_PID.fetch_add(1, Ordering::Relaxed)
    };
    let (name, ppid, frame, tpidr_el0, fp) = (l.name, l.ppid, l.frame, l.tpidr_el0, l.fp);
    let clear_child_tid = l.clear_child_tid;
    let ttbr0 = l.space.ttbr0();
    let p = Arc::new(Process {
        pid,
        inner: SpinLock::new(Inner {
            ppid,
            name: Comm::new(name),
            state: PState::Alive,
            space: Some(l.space),
            fds: l.fds,
            children: Vec::new(),
            tid: 0,
            threads: Vec::new(),
            cwd: l.cwd,
            root_dir: l.root_dir,
            start_ns: timer::now_ns(),
            pgid: l.pgid.unwrap_or(pid),
            sid: l.sid.unwrap_or(pid),
            sig: l.sig,
            argv: l.argv,
            exe: l.exe,
            group_exit: None,
            ever_threaded: false,
            caps: l.caps,
        }),
        child_wq: WaitQueue::new(),
        cred: SpinLock::new(l.cred),
    });
    // Visible before the thread can run (it may call getpid right away).
    PROCS.lock().insert(pid, p.clone());
    procfs::add(pid);
    if let Some(parent) = (ppid != 0).then(|| get(ppid)).flatten() {
        parent.inner.lock().children.push(pid);
    }
    let user = UserState {
        pid,
        ttbr0,
        tpidr_el0,
        fp,
        clear_child_tid,
    };
    match sched::spawn_user(name, frame, user) {
        Some(tid) => {
            let mut i = p.inner.lock();
            i.tid = tid;
            i.threads.push(tid);
            Ok(pid)
        }
        None => {
            release(pid);
            if let Some(parent) = get(ppid) {
                parent.inner.lock().children.retain(|&c| c != pid);
            }
            Err(ENOMEM)
        }
    }
}

/// Load an executable into a fresh address space and build its stack.
fn load_image(
    data: &[u8],
    argv: &[String],
    envp: &[String],
    path: &str,
) -> KResult<(UserSpace, usize, usize)> {
    let mut space = UserSpace::new()?;
    let img = elf::load(&mut space, data)?;
    space.set_brk_start(img.end);
    let top = space.map_stack()?;
    let sp = elf::build_stack(&space, top, argv, envp, &img, path)?;
    Ok((space, img.entry, sp))
}

fn user_frame(entry: usize, sp: usize) -> TrapFrame {
    let mut f = TrapFrame::zeroed();
    f.elr = entry as u64;
    f.sp_el0 = sp as u64;
    f.spsr = 0; // EL0t, all exceptions unmasked
    f
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Start the program at `path` as a new process with the kernel as parent, on the console,
/// as the session user ([`login::session`]: the desktop starts apps this way).
pub fn spawn(path: &str, argv: &[&str]) -> KResult<Pid> {
    spawn_with_caps(path, argv, caps::Caps::all())
}

/// [`spawn`] with a capability set from the first instruction (the desktop runs packaged
/// apps sandboxed this way; no window in which the program runs unrestricted). `cwd` is
/// `/`, HOME is the user's home. The binary is loaded by the kernel, so it needs no grant.
/// Runs as the session user.
pub fn spawn_with_caps(
    path: &str,
    argv: &[&str],
    caps: alloc::sync::Arc<caps::Caps>,
) -> KResult<Pid> {
    let acct = login::session();
    spawn_as(path, argv, caps, acct.cred(), &acct.env())
}

fn spawn_as(
    path: &str,
    argv: &[&str],
    caps: alloc::sync::Arc<caps::Caps>,
    cred: cred::Cred,
    envp: &[String],
) -> KResult<Pid> {
    let data = fs::read_file(path)?;
    let argv: Vec<String> = argv.iter().map(|s| String::from(*s)).collect();
    let (space, entry, sp) = load_image(&data, &argv, envp, path)?;
    start(Launch {
        name: basename(path),
        ppid: 0,
        pgid: None,
        sid: None,
        space,
        fds: FdTable::with_console()?,
        cwd: fs::root(),
        root_dir: None,
        frame: &user_frame(entry, sp),
        tpidr_el0: 0,
        fp: Box::new(FpState::zeroed()),
        sig: signal::SigState::new(),
        argv,
        exe: String::from(path),
        clear_child_tid: 0,
        caps,
        cred,
    })
}

/// Like [`spawn`], but with fds 0/1/2 on the terminal device `tty`, the given environment and
/// working directory. The desktop's Terminal window uses it to run `msh` on `/dev/tty1`.
/// Runs as the session user.
pub fn spawn_on(path: &str, argv: &[&str], envp: &[&str], tty: &str, cwd: &str) -> KResult<Pid> {
    let data = fs::read_file(path)?;
    let argv: Vec<String> = argv.iter().map(|s| String::from(*s)).collect();
    let envp: Vec<String> = envp.iter().map(|s| String::from(*s)).collect();
    let (space, entry, sp) = load_image(&data, &argv, &envp, path)?;
    let cwd = fs::lookup(cwd).unwrap_or_else(|_| fs::root());
    start(Launch {
        name: basename(path),
        ppid: 0,
        pgid: None,
        sid: None,
        space,
        fds: FdTable::with_tty(tty)?,
        cwd,
        root_dir: None,
        frame: &user_frame(entry, sp),
        tpidr_el0: 0,
        fp: Box::new(FpState::zeroed()),
        sig: signal::SigState::new(),
        argv,
        exe: String::from(path),
        clear_child_tid: 0,
        caps: caps::Caps::all(),
        cred: login::session_cred(),
    })
}

/// Start `/bin/init` as PID 1. Call at the end of boot.
pub fn start_init() {
    assert!(get(1).is_none(), "init already running");
    INIT_NEXT.store(true, Ordering::Relaxed);
    let env = [String::from("PATH=/bin"), String::from("HOME=/")];
    match spawn_as(
        "/bin/init",
        &["init"],
        caps::Caps::all(),
        cred::Cred::root(),
        &env,
    ) {
        Ok(pid) => kprintln!("[proc] started /bin/init as pid {pid}"),
        Err(e) => panic!("could not start /bin/init: errno {}", e.0),
    }
}

/// Wait for a process the kernel started; returns its wait status.
pub fn wait_kernel_child(pid: Pid) -> i32 {
    let mut status = 0;
    KERNEL_CHILD_WQ.wait_until(|| match get(pid) {
        None => true,
        Some(p) => match p.inner.lock().state {
            PState::Zombie(s) => {
                status = s;
                true
            }
            PState::Alive => false,
        },
    });
    release(pid);
    status
}

// ---- fork / exec / exit / wait --------------------------------------------------------

/// fork(): copy the caller. The child resumes from the same syscall with x0 = 0.
pub fn fork(frame: &TrapFrame) -> KResult<Pid> {
    fork_with(frame, 0)
}

/// fork(), recording `clear_child_tid` for the child (CLONE_CHILD_CLEARTID).
pub fn fork_with(frame: &TrapFrame, clear_child_tid: usize) -> KResult<Pid> {
    let parent = current();
    let (space, fds, name, cwd, pgid, sid, sig, argv, exe, root_dir, caps, cred) = {
        let mut i = parent.inner.lock();
        let space = i.space.as_mut().ok_or(ESRCH)?.fork()?;
        (
            space,
            i.fds.clone(),
            i.name,
            i.cwd.clone(),
            i.pgid,
            i.sid,
            i.sig.for_child(),
            i.argv.clone(),
            i.exe.clone(),
            i.root_dir.clone(),
            i.caps.clone(),
            *parent.cred.lock(),
        )
    };
    let mut cf = *frame;
    cf.x[0] = 0;
    let mut fp = Box::new(FpState::zeroed());
    sched::save_current_fp(&mut fp);
    start(Launch {
        name: name.as_str(),
        ppid: parent.pid,
        pgid: Some(pgid),
        sid: Some(sid),
        space,
        fds,
        cwd,
        root_dir,
        caps,
        cred,
        frame: &cf,
        tpidr_el0: sched::current_tpidr_el0(),
        fp,
        sig,
        argv,
        exe,
        clear_child_tid,
    })
}

/// set_tid_address: record where to clear + futex-wake on this thread's exit.
pub fn set_clear_child_tid(addr: usize) {
    sched::set_clear_child_tid(addr);
}

/// clone() with CLONE_THREAD: add a thread to the *current* process. It shares the address
/// space, fds, signal handlers, cwd and pgid; it has its own kernel stack, TPIDR_EL0
/// (`tls`), FP state and user stack (`frame.sp_el0`). Returns the new tid.
pub fn spawn_thread(frame: &TrapFrame, tls: Option<u64>, clear_child_tid: usize) -> KResult<Tid> {
    let me = current();
    let (ttbr0, name) = {
        let i = me.inner.lock();
        (i.space.as_ref().ok_or(ESRCH)?.ttbr0(), i.name)
    };
    let user = UserState {
        pid: me.pid,
        ttbr0,
        tpidr_el0: tls.unwrap_or_else(sched::current_tpidr_el0),
        fp: Box::new(FpState::zeroed()),
        clear_child_tid,
    };
    let tid = sched::spawn_user(name.as_str(), frame, user).ok_or(ENOMEM)?;
    let mut i = me.inner.lock();
    i.threads.push(tid);
    i.ever_threaded = true;
    Ok(tid)
}

/// execve(): replace the caller's image. On success `frame` is rewritten to enter the new
/// program; on failure the old image is untouched.
pub fn exec(frame: &mut TrapFrame, path: &str, argv: &[String], envp: &[String]) -> KResult<()> {
    let cwd = current().inner.lock().cwd.clone();
    let node = fs::lookup_at(Some(&cwd), path, true)?;
    let st = fs::perm::view(&node);
    if st.kind != fs::InodeType::File {
        return Err(EACCES);
    }
    // Execute permission is what counts (read is not needed: the kernel reads it).
    fs::perm::may(&node, fs::perm::X)?;
    let f = fs::OpenFile::new(node, fs::abi::O_RDONLY);
    let mut data = alloc::vec![0u8; st.size as usize];
    let mut done = 0;
    while done < data.len() {
        match f.read(&mut data[done..])? {
            0 => break,
            n => done += n,
        }
    }
    data.truncate(done);
    let (space, entry, sp) = load_image(&data, argv, envp, path)?;
    let me = current();
    let ttbr0 = space.ttbr0();
    let nosuid = fs::perm::nosuid(f.inode());
    {
        let mut c = me.cred.lock();
        let before = *c;
        let changed = c.on_exec(st.perm, st.uid, st.gid, nosuid);
        let after = *c;
        drop(c);
        if changed {
            crate::audit!(
                "setid.exec",
                "path={path} uid={}->{} gid={}->{}",
                before.euid,
                after.euid,
                before.egid,
                after.egid
            );
        }
    }
    let (old, cloexec) = {
        let mut i = me.inner.lock();
        i.name = Comm::new(basename(path));
        i.argv = argv.to_vec();
        i.exe = if path.starts_with('/') {
            String::from(path)
        } else {
            let base = fs::path_of(&i.cwd).unwrap_or_default();
            alloc::format!("{}/{path}", base.trim_end_matches('/'))
        };
        i.sig.on_exec();
        (i.space.replace(space), i.fds.take_cloexec())
    };
    sched::with_current_user(|u| {
        if let Some(u) = u {
            u.ttbr0 = ttbr0;
            u.tpidr_el0 = 0;
            *u.fp = FpState::zeroed();
        }
    });
    sched::set_ttbr0(ttbr0);
    sched::set_current_name(basename(path));
    sched::reset_current_fp();
    // SAFETY: user TLS register.
    unsafe { core::arch::asm!("msr tpidr_el0, xzr", options(nomem, nostack)) };
    drop(old); // no longer in TTBR0
    drop(cloexec);
    *frame = user_frame(entry, sp);
    let ppid = me.inner.lock().ppid;
    crate::audit!("exec", "ppid={ppid} argv={}", argv.join(" "));
    Ok(())
}

/// End the calling thread. If it is the last thread of its process, the process zombifies;
/// otherwise only this thread dies (its stack is reaped). Handles clear_child_tid.
pub fn thread_exit(code: i32) -> ! {
    let me = current();
    let tid = sched::current_tid();
    let ctid = sched::current_clear_child_tid();
    if ctid != 0 {
        if let Some(s) = me.inner.lock().space.as_mut() {
            let _ = s.copy_out(ctid, &0u32.to_le_bytes());
        }
        futex::wake_addr(ctid);
    }
    let last = {
        let mut i = me.inner.lock();
        i.threads.retain(|&t| t != tid);
        i.threads.is_empty()
    };
    if last {
        drop(me);
        finish_process(code);
    }
    drop(me);
    sched::exit(code as i64)
}

/// exit_group / a fatal signal: end every thread of the process, then zombify it.
pub fn exit_current(wstatus: i32) -> ! {
    let me = current();
    let my_tid = sched::current_tid();
    let others: Vec<Tid> = {
        let mut i = me.inner.lock();
        if i.group_exit.is_none() {
            i.group_exit = Some(wstatus);
        }
        i.threads.iter().copied().filter(|&t| t != my_tid).collect()
    };
    // Nudge the others: interrupted() is now true for them (group_exit set), so their
    // blocking syscalls return EINTR and they exit on the way back to EL0.
    for t in &others {
        sched::wake(*t);
    }
    // Wait for them to leave (they remove themselves from `threads`).
    let t0 = timer::now_ns();
    while me.inner.lock().threads.iter().any(|&t| t != my_tid) {
        if timer::now_ns() - t0 > 2_000_000_000 {
            break; // give up after 2 s rather than hang; should not happen
        }
        sched::sleep_ms(1);
    }
    me.inner.lock().threads.clear();
    drop(me);
    finish_process(wstatus)
}

/// Zombify the current process (its last thread is exiting) with `wstatus`.
fn finish_process(wstatus: i32) -> ! {
    let me = current();
    let kttbr = mmu::kernel_ttbr0();
    sched::with_current_user(|u| {
        if let Some(u) = u {
            u.ttbr0 = kttbr;
        }
    });
    sched::set_ttbr0(kttbr);
    let (space, fds, children, ppid) = {
        let mut i = me.inner.lock();
        i.state = PState::Zombie(wstatus);
        (
            i.space.take(),
            core::mem::take(&mut i.fds),
            core::mem::take(&mut i.children),
            i.ppid,
        )
    };
    // If other threads ever ran in this space on other CPUs, make sure none is mid
    // page-table walk before we free the tables.
    if me.inner.lock().ever_threaded {
        crate::tlb::shootdown();
    }
    drop(space);
    drop(fds);
    let pid = me.pid;
    drop(me); // sched::exit never returns: hold no references past here
    if pid == 1 {
        panic!("init (pid 1) exited, wait status {wstatus:#x}");
    }
    // Orphans go to init, which reaps them.
    if !children.is_empty()
        && let Some(init) = get(1)
    {
        for c in &children {
            if let Some(p) = get(*c) {
                p.inner.lock().ppid = 1;
            }
        }
        init.inner.lock().children.extend(children);
        init.child_wq.wake_all();
    }
    match ppid {
        0 => KERNEL_CHILD_WQ.wake_all(),
        p => {
            if let Some(parent) = get(p) {
                signal::notify_parent(&parent, pid, wstatus);
                parent.child_wq.wake_all();
            }
        }
    }
    sched::exit(wstatus as i64)
}

pub const WNOHANG: u64 = 1;

/// wait4(pid, options): reap one child. `want` = -1 (or 0, < -1: no process groups) for any.
/// Returns (pid, wait status), or (0, 0) with WNOHANG when no child has exited yet.
pub fn wait(want: i64, options: u64) -> KResult<(Pid, i32)> {
    let me = current();
    let mut result = None;
    me.child_wq.wait_until(|| {
        result = try_reap(&me, want, options & WNOHANG != 0);
        result.is_some() || signal::interrupted()
    });
    result.unwrap_or(Err(EINTR))
}

fn try_reap(me: &Process, want: i64, nohang: bool) -> Option<KResult<(Pid, i32)>> {
    // want > 0: that pid; -1: any; 0: my process group; < -1: process group -want.
    let my_pgid = me.inner.lock().pgid;
    let want_pgid = match want {
        0 => Some(my_pgid),
        w if w < -1 => Some((-w) as Pid),
        _ => None,
    };
    let children: Vec<Pid> = me
        .inner
        .lock()
        .children
        .clone()
        .into_iter()
        .filter(|&c| match (want, want_pgid) {
            (_, Some(g)) => get(c).is_some_and(|p| p.inner.lock().pgid == g),
            (-1, None) => true,
            (w, None) => c as i64 == w,
        })
        .collect();
    if children.is_empty() {
        return Some(Err(ECHILD));
    }
    for c in children {
        let Some(p) = get(c) else { continue };
        let st = p.inner.lock().state;
        if let PState::Zombie(s) = st {
            me.inner.lock().children.retain(|&x| x != c);
            release(c);
            return Some(Ok((c, s)));
        }
    }
    nohang.then_some(Ok((0, 0)))
}

/// Last thing before every return to EL0: reschedule if asked, deliver signals.
fn on_user_return(frame: &mut TrapFrame) {
    if sched::need_resched() {
        sched::yield_now();
    }
    // A sibling called exit_group (or took a fatal signal): this thread exits too.
    if let Some(p) = get(sched::current_pid()) {
        let code = p.inner.lock().group_exit;
        drop(p);
        if let Some(code) = code {
            thread_exit(code);
        }
    }
    signal::deliver(frame);
}

// ---- faults ---------------------------------------------------------------------------

const EC_UNKNOWN: u64 = 0x00;
const EC_FP: u64 = 0x07;
const EC_IABT_LOWER: u64 = 0x20;
const EC_PC_ALIGN: u64 = 0x22;
const EC_DABT_LOWER: u64 = 0x24;
const EC_SP_ALIGN: u64 = 0x26;
const EC_FP_EXC: u64 = 0x2c;
const EC_BRK64: u64 = 0x3c;

/// A synchronous exception from EL0 that isn't a syscall: grow the stack if that is what
/// it was, otherwise kill the process with a readable message.
fn on_user_fault(frame: &mut TrapFrame) -> bool {
    if sched::current_pid() == 0 {
        return false;
    }
    let ec = (frame.esr >> 26) & 0x3f;
    let iss = frame.esr & 0x01ff_ffff;
    let fsc = iss & 0x3f;
    // Write to a copy-on-write page (permission fault, WnR set): copy it and retry.
    if ec == EC_DABT_LOWER && (0x0d..=0x0f).contains(&fsc) && iss & (1 << 6) != 0 {
        let me = current();
        let fixed = me
            .inner
            .lock()
            .space
            .as_mut()
            .is_some_and(|s| s.cow_fault(frame.far as usize));
        if fixed {
            return true;
        }
    }
    if ec == EC_DABT_LOWER && (0x04..=0x07).contains(&fsc) {
        let me = current();
        let grown = me
            .inner
            .lock()
            .space
            .as_mut()
            .is_some_and(|s| s.grow_stack(frame.far as usize));
        if grown {
            return true;
        }
    }
    let (sig, what) = match ec {
        EC_DABT_LOWER if fsc == 0x21 => (SIGBUS, "alignment fault"),
        EC_DABT_LOWER | EC_IABT_LOWER => (SIGSEGV, exceptions::fault_status(fsc)),
        EC_PC_ALIGN => (SIGBUS, "PC alignment fault"),
        EC_SP_ALIGN => (SIGBUS, "SP alignment fault"),
        EC_BRK64 => (SIGTRAP, "breakpoint (BRK)"),
        EC_FP_EXC => (SIGFPE, "floating-point exception"),
        EC_FP => (SIGILL, "FP/SIMD access trapped"),
        EC_UNKNOWN => (SIGILL, "undefined instruction"),
        _ => (SIGILL, exceptions::ec_name(ec)),
    };
    let access = match ec {
        EC_DABT_LOWER => {
            if iss & (1 << 6) != 0 {
                " writing"
            } else {
                " reading"
            }
        }
        EC_IABT_LOWER => " fetching",
        _ => "",
    };
    let addr = if matches!(ec, EC_DABT_LOWER | EC_IABT_LOWER | EC_PC_ALIGN) {
        let hint = if !(mmu::USER_BASE..mmu::USER_END).contains(&(frame.far as usize)) {
            " (not a user address)"
        } else {
            ""
        };
        alloc::format!(" {:#x}{hint}", frame.far)
    } else {
        String::new()
    };
    // A handler for it? Then it runs on the way out (the faulting instruction is retried
    // when it returns, as on Linux).
    let usig = sig as u32;
    if signal::has_handler(usig) {
        let code = if ec == EC_DABT_LOWER && (0x0d..=0x0f).contains(&fsc) {
            2
        } else {
            1
        };
        signal::post_fault(usig, code, frame.far);
        return true;
    }
    let me = current();
    let name = me.inner.lock().name;
    kprintln!(
        "[proc] pid {} ({}) killed by {}: {}{}{} at pc {:#x}",
        me.pid,
        name,
        signame(sig),
        what,
        access,
        addr,
        frame.elr
    );
    drop(me);
    exit_current(wstatus_signaled(sig) | 0x80) // "core dumped", like Linux's default
}
