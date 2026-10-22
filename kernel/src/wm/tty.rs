//! `/dev/tty1` .. `/dev/tty9`: the desktop Terminal's terminal devices, one per tab. Each is
//! a `proc::tty::Tty` (the same line discipline as the UART console: echo, editing, ^C ->
//! SIGINT to the foreground process group, termios, window size), whose input comes from
//! keys typed into its tab ([`input`]) and whose output goes to a buffer the tab drains and
//! draws ([`take_output`]).
//!
//! A kernel thread per tty ("term-shN") keeps a `msh` running on it while the tty is open,
//! restarting it when it exits. tty1 is the Terminal's first tab and stays open for good
//! (closing the window leaves its shell running, so reopening finds it as you left it);
//! tty2.. belong to extra tabs: [`open_tab`] takes a free one, [`hangup`] closes it again
//! (SIGHUP to its foreground job, SIGKILL to its msh; the thread then exits).
//!
//! The desktop also runs a few command line tools for itself on a free tty, with no shell
//! and no tab: [`Job`] (the Milk Drop sheet drives `drop list / send / recv` that way, reading
//! what they print and answering their prompts).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::fs::node::makedev;
use crate::fs::{self, Inode, InodeType, Stat};
use crate::proc::tty::Tty;
use crate::sched;
use crate::sync::SpinLock;
use crate::{kprintln, proc};

/// How many ttys (and so Terminal tabs) there can be.
pub const MAX: usize = 9;

fn out1(b: &[u8]) {
    push_out(1, b)
}
fn out2(b: &[u8]) {
    push_out(2, b)
}
fn out3(b: &[u8]) {
    push_out(3, b)
}
fn out4(b: &[u8]) {
    push_out(4, b)
}
fn out5(b: &[u8]) {
    push_out(5, b)
}
fn out6(b: &[u8]) {
    push_out(6, b)
}
fn out7(b: &[u8]) {
    push_out(7, b)
}
fn out8(b: &[u8]) {
    push_out(8, b)
}
fn out9(b: &[u8]) {
    push_out(9, b)
}

/// The terminals. 24 x 80 until their tab reports its size.
static TTYS: [Tty; MAX] = [
    Tty::new("tty1", out1, 24, 80),
    Tty::new("tty2", out2, 24, 80),
    Tty::new("tty3", out3, 24, 80),
    Tty::new("tty4", out4, 24, 80),
    Tty::new("tty5", out5, 24, 80),
    Tty::new("tty6", out6, 24, 80),
    Tty::new("tty7", out7, 24, 80),
    Tty::new("tty8", out8, 24, 80),
    Tty::new("tty9", out9, 24, 80),
];

/// Bytes written by programs (and echo), waiting for the tab to draw them.
static OUT: [SpinLock<Vec<u8>>; MAX] = [const { SpinLock::new(Vec::new()) }; MAX];
/// Bumped on every output, so a tab can tell cheaply whether to drain.
static OUT_GEN: [AtomicU64; MAX] = [const { AtomicU64::new(0) }; MAX];
/// /dev/ttyN exists and is in the tty registry.
static PUBLISHED: [AtomicBool; MAX] = [const { AtomicBool::new(false) }; MAX];
/// A tab wants a shell on it.
static OPEN: [AtomicBool; MAX] = [const { AtomicBool::new(false) }; MAX];
/// Its shell thread is running (a closed tty is free again once this is false).
static THREAD: [AtomicBool; MAX] = [const { AtomicBool::new(false) }; MAX];
/// The msh on it (0 while none).
static SHELL: [AtomicU32; MAX] = [const { AtomicU32::new(0) }; MAX];

const OUT_MAX: usize = 64 * 1024;

fn idx(n: usize) -> usize {
    n.clamp(1, MAX) - 1
}

pub fn path(n: usize) -> alloc::string::String {
    alloc::format!("/dev/tty{}", idx(n) + 1)
}

fn push_out(n: usize, b: &[u8]) {
    let i = idx(n);
    {
        let mut out = OUT[i].lock();
        if out.len() + b.len() > OUT_MAX {
            let drop = (out.len() + b.len() - OUT_MAX).min(out.len());
            out.drain(..drop);
        }
        out.extend_from_slice(b);
    }
    OUT_GEN[i].fetch_add(1, Ordering::Relaxed);
    super::WAKE.wake_all();
}

/// Feed one byte typed into tty `n`'s tab (wm thread).
pub fn input(n: usize, b: u8) {
    TTYS[idx(n)].input(&[b]);
}

/// Tab `n`'s text area changed size: rows x cols character cells. Sends SIGWINCH to the
/// foreground job when it actually changes.
pub fn set_winsize(n: usize, rows: u16, cols: u16) {
    TTYS[idx(n)].set_winsize(rows, cols);
}

/// Take everything written to tty `n` since the last call. Cheap when nothing is new.
pub fn take_output(n: usize, seen_gen: &mut u64) -> Option<Vec<u8>> {
    let i = idx(n);
    let g = OUT_GEN[i].load(Ordering::Relaxed);
    if g == *seen_gen {
        return None;
    }
    *seen_gen = g;
    Some(core::mem::take(&mut *OUT[i].lock()))
}

struct TtyNode {
    n: usize,
}

impl Inode for TtyNode {
    fn ino(&self) -> u64 {
        0x7770 + self.n as u64
    }
    fn dev(&self) -> u64 {
        0
    }
    fn kind(&self) -> InodeType {
        InodeType::CharDev
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(0, self.ino(), InodeType::CharDev, 0o620);
        st.rdev = makedev(4, self.n as u32); // Linux ttyN
        st.blksize = 1024;
        st
    }
    fn read_at(&self, _off: u64, buf: &mut [u8]) -> fs::Result<usize> {
        TTYS[idx(self.n)].read(buf)
    }
    fn write_at(&self, _off: u64, buf: &[u8]) -> fs::Result<usize> {
        TTYS[idx(self.n)].write(buf)
    }
    fn truncate(&self, _len: u64) -> fs::Result<()> {
        Ok(())
    }
    /// poll(2): readable once a whole line (or raw bytes / ^D) is waiting; always writable.
    fn poll(&self) -> u32 {
        if TTYS[idx(self.n)].readable() {
            0x1 | 0x4
        } else {
            0x4
        }
    }
    // ioctls (termios, winsize, foreground group) are handled by the syscall layer through
    // the tty registry (`proc::tty::register`, done in `publish`), not here.
}

fn publish(n: usize) -> bool {
    let i = idx(n);
    if PUBLISHED[i].load(Ordering::Acquire) {
        return true;
    }
    let name = alloc::format!("tty{}", i + 1);
    if fs::stat(&path(n)).is_err()
        && let Err(e) = fs::devfs::add_device(&name, Arc::new(TtyNode { n: i + 1 }))
    {
        kprintln!("wm: cannot publish {}: {e}", path(n));
        return false;
    }
    proc::tty::register(makedev(4, (i + 1) as u32), &TTYS[i]);
    PUBLISHED[i].store(true, Ordering::Release);
    true
}

/// Publish `/dev/ttyN` and start the thread that keeps a shell on it while it is open.
/// Idempotent. False if there is no `/Commands/msh` (then the Terminal keeps its built-in shell).
pub fn start_shell(n: usize) -> bool {
    let i = idx(n);
    if fs::stat("/Commands/msh").is_err() || !publish(n) {
        return false;
    }
    OPEN[i].store(true, Ordering::Release);
    if THREAD[i].swap(true, Ordering::AcqRel) {
        return true;
    }
    let name = alloc::format!("term-sh{}", i + 1);
    drop(sched::spawn(&name, move || {
        let tty = path(i + 1);
        while OPEN[i].load(Ordering::Acquire) {
            const ENV: &[&str] = &["PATH=/Commands", "HOME=/Users/milk", "TERM=dumb"];
            match proc::spawn_on("/Commands/msh", &["msh"], ENV, &tty, "/Users/milk") {
                Ok(pid) => {
                    SHELL[i].store(pid, Ordering::Release);
                    kprintln!("wm: msh pid {pid} on {tty}");
                    let st = proc::wait_kernel_child(pid);
                    SHELL[i].store(0, Ordering::Release);
                    if OPEN[i].load(Ordering::Acquire) {
                        push_out(
                            i + 1,
                            alloc::format!("\n[msh exited ({st:#x}), starting a new one]\n")
                                .as_bytes(),
                        );
                    }
                }
                Err(e) => {
                    kprintln!("wm: cannot start msh on {tty}: errno {}", e.0);
                    sched::sleep_ms(2000);
                }
            }
        }
        // Closed: forget what it printed on the way out, then the tty is free again.
        OUT[i].lock().clear();
        THREAD[i].store(false, Ordering::Release);
    }));
    true
}

/// A free tty for a new tab (2..=MAX), with a shell starting on it. None if all are taken
/// (or there is no msh).
pub fn open_tab() -> Option<usize> {
    let n = (2..=MAX).find(|&n| {
        !OPEN[idx(n)].load(Ordering::Acquire) && !THREAD[idx(n)].load(Ordering::Acquire)
    })?;
    // Skip output left over from the tty's last user.
    OUT[idx(n)].lock().clear();
    start_shell(n).then_some(n)
}

/// A tab closed: hang its tty up. SIGHUP to the foreground job, SIGKILL to its msh; the
/// shell thread notices and exits. tty1 is never hung up.
pub fn hangup(n: usize) {
    let i = idx(n);
    if i == 0 {
        return;
    }
    OPEN[i].store(false, Ordering::Release);
    let fg = TTYS[i].foreground();
    if fg != 0 {
        let _ = proc::signal::kill(-(fg as i64), proc::signal::SIGHUP);
    }
    let pid = SHELL[i].load(Ordering::Acquire);
    if pid != 0 {
        let _ = proc::signal::kill(pid as i64, proc::signal::SIGKILL);
    }
}

/// Is tty `n` in canonical (line) mode? (The Terminal keeps Ctrl+L for itself only then.)
pub fn canonical(n: usize) -> bool {
    const ICANON: u32 = 0o2;
    match TTYS[idx(n)].ioctl(proc::tty::TCGETS, &[]) {
        Ok(Some(t)) if t.len() >= 16 => {
            u32::from_le_bytes([t[12], t[13], t[14], t[15]]) & ICANON != 0
        }
        _ => true,
    }
}

// ---- jobs: a program on a free tty, for the desktop itself -----------------------------

/// Exit status of a job's program, once it is done (`u32::MAX` while running).
static EXIT: [AtomicU32; MAX] = [const { AtomicU32::new(u32::MAX) }; MAX];
/// The job's pid (0 before it started / after a failed start).
static JOB_PID: [AtomicU32; MAX] = [const { AtomicU32::new(0) }; MAX];

/// A program the desktop runs on its own tty: its output is read with [`Job::output`], its
/// input written with [`Job::write`]. Dropping it kills the program (if still running) and
/// frees the tty.
pub struct Job {
    n: usize,
    seen: u64,
    /// Output not yet split into lines.
    partial: alloc::string::String,
}

impl Job {
    /// Start `path` with `argv` and `env` (cwd the home folder) on a free tty, from the top
    /// (tty9 down, so Terminal tabs keep the low numbers). None if all are taken.
    pub fn start(exe: &str, argv: &[&str], env: &[&str]) -> Option<Job> {
        let n = (2..=MAX).rev().find(|&n| {
            !OPEN[idx(n)].load(Ordering::Acquire) && !THREAD[idx(n)].load(Ordering::Acquire)
        })?;
        let i = idx(n);
        if !publish(n) || THREAD[i].swap(true, Ordering::AcqRel) {
            return None;
        }
        OUT[i].lock().clear();
        EXIT[i].store(u32::MAX, Ordering::Release);
        JOB_PID[i].store(0, Ordering::Release);
        // Raw-ish: no echo would be nicer, but the prompts we answer are line based and
        // the echoed answer is harmless.
        let tty = path(n);
        match proc::spawn_on(exe, argv, env, &tty, "/Users/milk") {
            Ok(pid) => {
                JOB_PID[i].store(pid, Ordering::Release);
                let name = alloc::format!("job-tty{}", i + 1);
                drop(sched::spawn(&name, move || {
                    let st = proc::wait_kernel_child(pid) as u32;
                    // Only if the tty still belongs to this job (not a newer one).
                    if JOB_PID[i].load(Ordering::Acquire) == pid {
                        EXIT[i].store(st & 0xffff, Ordering::Release);
                    }
                    super::WAKE.wake_all();
                }));
            }
            Err(e) => {
                push_out(
                    n,
                    alloc::format!("{exe}: cannot start: errno {}\n", e.0).as_bytes(),
                );
                EXIT[i].store(0x100, Ordering::Release);
            }
        }
        Some(Job {
            n,
            seen: 0,
            partial: alloc::string::String::new(),
        })
    }

    /// New output since the last call, as whole lines (\r and \n both end a line). A
    /// prompt with no newline yet stays buffered until its line ends.
    pub fn output(&mut self) -> Vec<alloc::string::String> {
        if let Some(b) = take_output(self.n, &mut self.seen) {
            self.partial
                .push_str(&alloc::string::String::from_utf8_lossy(&b));
        }
        let mut lines = Vec::new();
        while let Some(k) = self.partial.find(['\n', '\r']) {
            let line: alloc::string::String = self.partial[..k].into();
            self.partial.drain(..=k);
            if !line.is_empty() {
                lines.push(line);
            }
        }
        lines
    }

    /// Type `s` into the program's terminal.
    pub fn write(&self, s: &str) {
        for b in s.bytes() {
            input(self.n, b);
        }
    }

    /// The exit status once the program has finished: `Some(code)` (a signal shows as
    /// 128 + its number), or None while it runs.
    pub fn exit(&self) -> Option<i32> {
        let st = EXIT[idx(self.n)].load(Ordering::Acquire);
        (st != u32::MAX).then(|| {
            if st & 0x7f != 0 {
                128 + (st & 0x7f) as i32
            } else {
                ((st >> 8) & 0xff) as i32
            }
        })
    }

    /// Stop it (SIGINT, like Ctrl+C: the drop tools clean up their partial files).
    pub fn interrupt(&self) {
        let pid = JOB_PID[idx(self.n)].load(Ordering::Acquire);
        if pid != 0 && self.exit().is_none() {
            let _ = proc::signal::kill(pid as i64, proc::signal::SIGINT);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let i = idx(self.n);
        let pid = JOB_PID[i].load(Ordering::Acquire);
        if pid != 0 && self.exit().is_none() {
            let _ = proc::signal::kill(pid as i64, proc::signal::SIGKILL);
        }
        OUT[i].lock().clear();
        // The waiter thread only records the status; the tty is free again now (a new
        // job resets EXIT before starting).
        THREAD[i].store(false, Ordering::Release);
    }
}
