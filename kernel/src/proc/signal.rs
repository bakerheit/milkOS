//! Signals: Linux numbers, `rt_sigaction` handlers with a Linux aarch64 `rt_sigframe`,
//! `rt_sigreturn`, masks, `kill` to a pid / process group, default actions.
//!
//! * State lives in the process (one thread per process): pending, blocked, 64 actions,
//!   and the siginfo of the latest pending instance of each signal (no queueing of
//!   repeats, like Linux's classic signals).
//! * Senders set the pending bit (dropped right away if the signal is ignored) and wake the
//!   target thread; any interruptible wait (`interrupted()`) then returns EINTR.
//! * Delivery happens on the way back to EL0 (`exceptions::set_user_return_hook`): the
//!   lowest pending unblocked signal either runs its default action (terminate, terminate
//!   with "core", ignore), or gets a signal frame on the user stack and the handler runs
//!   with x0 = signo, x1 = &siginfo, x2 = &ucontext, lr = sa_restorer.
//! * Not supported yet: stopping (SIGSTOP/SIGTSTP/SIGTTIN/SIGTTOU are ignored),
//!   sigaltstack, SA_RESTART (interrupted calls always return EINTR), queued RT signals.

use alloc::sync::Arc;

use super::errno::*;
use super::{Pid, Process};
use crate::exceptions::TrapFrame;
use crate::sched::{self, FpState};

pub const SIGHUP: u32 = 1;
pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGILL: u32 = 4;
pub const SIGTRAP: u32 = 5;
pub const SIGABRT: u32 = 6;
pub const SIGBUS: u32 = 7;
pub const SIGFPE: u32 = 8;
pub const SIGKILL: u32 = 9;
pub const SIGUSR1: u32 = 10;
pub const SIGSEGV: u32 = 11;
pub const SIGUSR2: u32 = 12;
pub const SIGPIPE: u32 = 13;
pub const SIGALRM: u32 = 14;
pub const SIGTERM: u32 = 15;
pub const SIGCHLD: u32 = 17;
pub const SIGCONT: u32 = 18;
pub const SIGSTOP: u32 = 19;
pub const SIGTSTP: u32 = 20;
pub const SIGTTIN: u32 = 21;
pub const SIGTTOU: u32 = 22;
pub const SIGURG: u32 = 23;
pub const SIGWINCH: u32 = 28;
pub const NSIG: u32 = 64;

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;
pub const SA_SIGINFO: u64 = 0x4;
pub const SA_RESTORER: u64 = 0x0400_0000;
pub const SA_NODEFER: u64 = 0x4000_0000;
pub const SA_RESETHAND: u64 = 0x8000_0000;

// si_code values
pub const SI_USER: i32 = 0;
pub const SI_KERNEL: i32 = 0x80;
pub const CLD_EXITED: i32 = 1;
pub const CLD_KILLED: i32 = 2;
pub const CLD_DUMPED: i32 = 3;

pub fn name(sig: u32) -> &'static str {
    match sig {
        SIGHUP => "SIGHUP",
        SIGINT => "SIGINT",
        SIGQUIT => "SIGQUIT",
        SIGILL => "SIGILL",
        SIGTRAP => "SIGTRAP",
        SIGABRT => "SIGABRT",
        SIGBUS => "SIGBUS",
        SIGFPE => "SIGFPE",
        SIGKILL => "SIGKILL",
        SIGUSR1 => "SIGUSR1",
        SIGSEGV => "SIGSEGV",
        SIGUSR2 => "SIGUSR2",
        SIGPIPE => "SIGPIPE",
        SIGALRM => "SIGALRM",
        SIGTERM => "SIGTERM",
        SIGCHLD => "SIGCHLD",
        SIGCONT => "SIGCONT",
        SIGSTOP => "SIGSTOP",
        SIGTSTP => "SIGTSTP",
        SIGWINCH => "SIGWINCH",
        _ => "signal",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DefAct {
    Term,
    Core,
    Ignore,
}

fn default_action(sig: u32) -> DefAct {
    match sig {
        SIGQUIT | SIGILL | SIGTRAP | SIGABRT | SIGBUS | SIGFPE | SIGSEGV | 24 | 25 | 31 => {
            DefAct::Core
        }
        // Job-control stops are not implemented: treated as ignored.
        SIGCHLD | SIGCONT | SIGURG | SIGWINCH | SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU => {
            DefAct::Ignore
        }
        _ => DefAct::Term,
    }
}

const fn bit(sig: u32) -> u64 {
    1 << (sig - 1)
}

/// Signals that can't be caught, blocked or ignored.
const UNBLOCKABLE: u64 = bit(SIGKILL) | bit(SIGSTOP);

#[derive(Clone, Copy, Default)]
pub struct SigAction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}

/// What a handler sees in its siginfo_t.
#[derive(Clone, Copy, Default)]
pub struct SigInfo {
    pub code: i32,
    /// Sender pid (kill, SIGCHLD).
    pub pid: u32,
    /// Fault address (SIGSEGV/SIGBUS/...).
    pub addr: u64,
    /// SIGCHLD: exit code or signal.
    pub status: i32,
}

#[derive(Clone)]
pub struct SigState {
    pub pending: u64,
    pub blocked: u64,
    pub actions: [SigAction; NSIG as usize],
    pub info: [SigInfo; NSIG as usize],
}

impl Default for SigState {
    fn default() -> Self {
        Self::new()
    }
}

impl SigState {
    pub fn new() -> Self {
        Self {
            pending: 0,
            blocked: 0,
            actions: [SigAction::default(); NSIG as usize],
            info: [SigInfo::default(); NSIG as usize],
        }
    }

    fn ignored(&self, sig: u32) -> bool {
        let a = &self.actions[sig as usize - 1];
        a.handler == SIG_IGN || (a.handler == SIG_DFL && default_action(sig) == DefAct::Ignore)
    }

    /// Pending, unblocked, and would actually do something.
    fn deliverable(&self) -> u64 {
        let mut m = self.pending & !(self.blocked & !UNBLOCKABLE);
        let mut out = 0;
        while m != 0 {
            let sig = m.trailing_zeros() + 1;
            m &= m - 1;
            if !self.ignored(sig) {
                out |= bit(sig);
            }
        }
        out
    }

    /// fork: the child inherits actions and mask, not pending signals.
    pub fn for_child(&self) -> Self {
        let mut s = self.clone();
        s.pending = 0;
        s
    }

    /// execve: handlers go back to default; ignored stays ignored; mask and pending stay.
    pub fn on_exec(&mut self) {
        for a in self.actions.iter_mut() {
            if a.handler != SIG_IGN {
                *a = SigAction::default();
            }
        }
    }
}

// ---- sending ---------------------------------------------------------------------------

/// Post `sig` to `p`. Returns false if it was discarded (ignored).
pub fn post(p: &Process, sig: u32, info: SigInfo) -> bool {
    let tid = {
        let mut i = p.inner.lock();
        if !matches!(i.state, super::PState::Alive) {
            return false;
        }
        if sig != SIGKILL && i.sig.ignored(sig) {
            return false;
        }
        i.sig.pending |= bit(sig);
        i.sig.info[sig as usize - 1] = info;
        i.tid
    };
    // Kick it out of any interruptible wait; it re-checks and sees the signal.
    sched::wake(tid);
    true
}

fn sender_info() -> SigInfo {
    SigInfo {
        code: SI_USER,
        pid: sched::current_pid(),
        ..SigInfo::default()
    }
}

/// Raise `sig` on the calling process (SIGPIPE from a write, ...).
pub fn send_current(sig: u32) {
    let pid = sched::current_pid();
    if let Some(p) = super::get(pid) {
        post(&p, sig, sender_info());
    }
}

/// Send to every live process in process group `pgid`. Returns how many got it.
pub fn send_pgrp(pgid: Pid, sig: u32, info: SigInfo) -> usize {
    let members: alloc::vec::Vec<Arc<Process>> = super::all()
        .into_iter()
        .filter(|p| p.inner.lock().pgid == pgid)
        .collect();
    members.iter().filter(|p| post(p, sig, info)).count()
}

/// kill(2).
pub fn kill(pid: i64, sig: u32) -> KResult<()> {
    if sig >= NSIG {
        return Err(EINVAL);
    }
    let info = sender_info();
    let me = sched::current_pid();
    let targets: alloc::vec::Vec<Arc<Process>> = match pid {
        1.. => alloc::vec![super::get(pid as Pid).ok_or(ESRCH)?],
        _ => {
            let pgid = match pid {
                0 => super::current().inner.lock().pgid,
                -1 => 0,
                p => (-p) as Pid,
            };
            super::all()
                .into_iter()
                .filter(|p| {
                    if pid == -1 {
                        p.pid != 1 && p.pid != me
                    } else {
                        p.inner.lock().pgid == pgid
                    }
                })
                .collect()
        }
    };
    let alive: alloc::vec::Vec<_> = targets
        .into_iter()
        .filter(|p| matches!(p.inner.lock().state, super::PState::Alive))
        .collect();
    if alive.is_empty() {
        return Err(ESRCH);
    }
    if sig != 0 {
        for p in &alive {
            post(p, sig, info);
        }
    }
    Ok(())
}

/// True if the calling thread has a signal that should interrupt a blocking wait.
/// Callable with IRQs masked (wait conditions).
pub fn interrupted() -> bool {
    let pid = sched::current_pid();
    if pid == 0 {
        return false;
    }
    super::get(pid).is_some_and(|p| p.inner.lock().sig.deliverable() != 0)
}

// ---- syscalls ----------------------------------------------------------------------------

/// rt_sigaction: install `new` (if any), return the old action.
pub fn sigaction(sig: u32, new: Option<SigAction>) -> KResult<SigAction> {
    if sig == 0 || sig > NSIG || (new.is_some() && (sig == SIGKILL || sig == SIGSTOP)) {
        return Err(EINVAL);
    }
    let p = super::current();
    let mut i = p.inner.lock();
    let old = i.sig.actions[sig as usize - 1];
    if let Some(mut a) = new {
        a.mask &= !UNBLOCKABLE;
        i.sig.actions[sig as usize - 1] = a;
        if i.sig.ignored(sig) {
            i.sig.pending &= !bit(sig); // POSIX: setting SIG_IGN discards pending
        }
    }
    Ok(old)
}

/// rt_sigprocmask. Returns the old mask.
pub fn sigprocmask(how: u32, set: Option<u64>) -> KResult<u64> {
    let p = super::current();
    let mut i = p.inner.lock();
    let old = i.sig.blocked;
    if let Some(set) = set {
        i.sig.blocked = match how {
            0 => old | set,  // SIG_BLOCK
            1 => old & !set, // SIG_UNBLOCK
            2 => set,        // SIG_SETMASK
            _ => return Err(EINVAL),
        } & !UNBLOCKABLE;
    }
    Ok(old)
}

pub fn pending() -> u64 {
    let p = super::current();
    let i = p.inner.lock();
    i.sig.pending & i.sig.blocked
}

// ---- delivery ----------------------------------------------------------------------------

/// Result of one delivery step.
enum Step {
    Nothing,
    Ignored,
    Handler(u32, SigAction, SigInfo, u64),
    Kill(u32, bool),
}

fn next(p: &Process) -> Step {
    let mut i = p.inner.lock();
    let d = i.sig.deliverable();
    // Discard ignored pending ones so they don't linger.
    let ignored = i.sig.pending & !d & !(i.sig.blocked & !UNBLOCKABLE);
    i.sig.pending &= !ignored;
    if d == 0 {
        return if ignored != 0 {
            Step::Ignored
        } else {
            Step::Nothing
        };
    }
    let sig = d.trailing_zeros() + 1;
    i.sig.pending &= !bit(sig);
    let a = i.sig.actions[sig as usize - 1];
    let info = i.sig.info[sig as usize - 1];
    if a.handler == SIG_DFL || sig == SIGKILL {
        return Step::Kill(sig, default_action(sig) == DefAct::Core);
    }
    let old_mask = i.sig.blocked;
    i.sig.blocked |= a.mask
        | if a.flags & SA_NODEFER != 0 {
            0
        } else {
            bit(sig)
        };
    i.sig.blocked &= !UNBLOCKABLE;
    if a.flags & SA_RESETHAND != 0 {
        i.sig.actions[sig as usize - 1] = SigAction::default();
    }
    Step::Handler(sig, a, info, old_mask)
}

/// Deliver pending signals before returning to EL0 with `frame`. May not return (the
/// process is terminated by a default action).
pub fn deliver(frame: &mut TrapFrame) {
    let Some(p) = super::get(sched::current_pid()) else {
        return;
    };
    loop {
        match next(&p) {
            Step::Nothing => return,
            Step::Ignored => continue,
            Step::Kill(sig, core) => {
                drop(p);
                let st = super::wstatus_signaled(sig as i32) | if core { 0x80 } else { 0 };
                super::exit_current(st);
            }
            Step::Handler(sig, a, info, old_mask) => {
                if setup_frame(&p, frame, sig, &a, &info, old_mask).is_err() {
                    // Can't build a frame (bad stack): Linux kills with SIGSEGV.
                    let name = p.inner.lock().name;
                    crate::kprintln!(
                        "[proc] pid {} ({}) killed: no room for a {} frame on its stack (sp {:#x})",
                        p.pid,
                        name,
                        self::name(sig),
                        frame.sp_el0
                    );
                    drop(p);
                    super::exit_current(super::wstatus_signaled(SIGSEGV as i32) | 0x80);
                }
                return;
            }
        }
    }
}

/// Is a handler installed for `sig` that would run now (not blocked)? Used by the fault
/// path: a synchronous fault nobody handles kills right away, with a message.
pub fn has_handler(sig: u32) -> bool {
    let Some(p) = super::get(sched::current_pid()) else {
        return false;
    };
    let i = p.inner.lock();
    let a = &i.sig.actions[sig as usize - 1];
    a.handler > SIG_IGN && i.sig.blocked & bit(sig) == 0
}

/// Post a fault signal to the current process (it is delivered on the way out).
pub fn post_fault(sig: u32, code: i32, addr: u64) {
    if let Some(p) = super::get(sched::current_pid()) {
        post(
            &p,
            sig,
            SigInfo {
                code,
                addr,
                ..SigInfo::default()
            },
        );
    }
}

// Linux aarch64 rt_sigframe layout (arch/arm64/kernel/signal.c, uapi sigcontext.h):
//   siginfo_t info        @ 0     (128 bytes)
//   ucontext uc           @ 128
//     uc_flags @0, uc_link @8, uc_stack @16 (24), uc_sigmask @40 (8), pad to 1024-bit set
//     uc_mcontext (sigcontext, 16-aligned) @176:
//       fault_address @0, regs[31] @8, sp @256, pc @264, pstate @272, __reserved[4096] @288
//   then a frame record {fp, lr}
const UC: usize = 128;
const MC: usize = UC + 176;
const MC_RESERVED: usize = MC + 288;
const FRAME_SIZE: usize = MC_RESERVED + 4096;
const RECORD: usize = FRAME_SIZE; // {x29, x30} for unwinders
const TOTAL: usize = FRAME_SIZE + 16;
const FPSIMD_MAGIC: u32 = 0x4650_8001;
const FPSIMD_SIZE: u32 = 528;

fn setup_frame(
    p: &Process,
    frame: &mut TrapFrame,
    sig: u32,
    a: &SigAction,
    info: &SigInfo,
    old_mask: u64,
) -> KResult<()> {
    if a.flags & SA_RESTORER == 0 || a.restorer == 0 {
        // No vDSO trampoline yet: the C library / rt must supply sa_restorer.
        return Err(EINVAL);
    }
    let sp = ((frame.sp_el0 as usize).checked_sub(TOTAL).ok_or(EFAULT)?) & !15;
    let mut f = alloc::vec![0u8; TOTAL];
    let put = |f: &mut [u8], off: usize, v: &[u8]| f[off..off + v.len()].copy_from_slice(v);
    // siginfo
    put(&mut f, 0, &(sig as i32).to_le_bytes());
    put(&mut f, 8, &info.code.to_le_bytes());
    if sig == SIGSEGV || sig == SIGBUS || sig == SIGILL || sig == SIGFPE || sig == SIGTRAP {
        put(&mut f, 16, &info.addr.to_le_bytes());
    } else {
        put(&mut f, 16, &(info.pid as i32).to_le_bytes());
        put(&mut f, 20, &0u32.to_le_bytes()); // uid
        if sig == SIGCHLD {
            put(&mut f, 24, &info.status.to_le_bytes());
        }
    }
    // ucontext
    put(&mut f, UC + 40, &old_mask.to_le_bytes());
    put(&mut f, MC, &info.addr.to_le_bytes());
    for (r, v) in frame.x.iter().enumerate() {
        put(&mut f, MC + 8 + r * 8, &v.to_le_bytes());
    }
    put(&mut f, MC + 256, &frame.sp_el0.to_le_bytes());
    put(&mut f, MC + 264, &frame.elr.to_le_bytes());
    put(&mut f, MC + 272, &frame.spsr.to_le_bytes());
    // FP/SIMD state as an fpsimd_context record, then the terminating null record.
    let mut fp = alloc::boxed::Box::new(FpState::zeroed());
    sched::save_current_fp(&mut fp);
    put(&mut f, MC_RESERVED, &FPSIMD_MAGIC.to_le_bytes());
    put(&mut f, MC_RESERVED + 4, &FPSIMD_SIZE.to_le_bytes());
    put(&mut f, MC_RESERVED + 8, &(fp.fpsr as u32).to_le_bytes());
    put(&mut f, MC_RESERVED + 12, &(fp.fpcr as u32).to_le_bytes());
    for (r, q) in fp.q.iter().enumerate() {
        put(&mut f, MC_RESERVED + 16 + r * 16, &q.to_le_bytes());
    }
    // frame record
    put(&mut f, RECORD, &frame.x[29].to_le_bytes());
    put(&mut f, RECORD + 8, &frame.elr.to_le_bytes());

    {
        let mut i = p.inner.lock();
        let space = i.space.as_mut().ok_or(ESRCH)?;
        space.copy_out(sp, &f)?;
    }
    frame.x[0] = sig as u64;
    frame.x[1] = sp as u64; // &info
    frame.x[2] = (sp + UC) as u64; // &uc
    frame.x[29] = (sp + RECORD) as u64;
    frame.x[30] = a.restorer;
    frame.sp_el0 = sp as u64;
    frame.elr = a.handler;
    frame.spsr = 0; // EL0t; NZCV cleared for the handler
    Ok(())
}

/// rt_sigreturn: restore the context saved by `setup_frame` (sp points at the frame).
/// Returns the restored x0 (the syscall layer writes it back into x0).
pub fn sigreturn(frame: &mut TrapFrame) -> KResult<usize> {
    let sp = frame.sp_el0 as usize;
    let p = super::current();
    let mut f = alloc::vec![0u8; FRAME_SIZE];
    {
        let i = p.inner.lock();
        i.space.as_ref().ok_or(ESRCH)?.copy_in(sp, &mut f)?;
    }
    let u64_at = |o: usize| u64::from_le_bytes(f[o..o + 8].try_into().unwrap());
    let u32_at = |o: usize| u32::from_le_bytes(f[o..o + 4].try_into().unwrap());
    for (r, x) in frame.x.iter_mut().enumerate() {
        *x = u64_at(MC + 8 + r * 8);
    }
    frame.sp_el0 = u64_at(MC + 256);
    frame.elr = u64_at(MC + 264);
    // Only the condition flags come back: never let user space pick EL1 or unmask
    // anything odd.
    frame.spsr = u64_at(MC + 272) & 0xf000_0000;
    if u32_at(MC_RESERVED) == FPSIMD_MAGIC {
        let mut fp = alloc::boxed::Box::new(FpState::zeroed());
        fp.fpsr = u32_at(MC_RESERVED + 8) as u64;
        fp.fpcr = u32_at(MC_RESERVED + 12) as u64;
        for (r, q) in fp.q.iter_mut().enumerate() {
            let o = MC_RESERVED + 16 + r * 16;
            *q = u128::from_le_bytes(f[o..o + 16].try_into().unwrap());
        }
        sched::load_current_fp(&fp);
    }
    p.inner.lock().sig.blocked = u64_at(UC + 40) & !UNBLOCKABLE;
    Ok(frame.x[0] as usize)
}

/// SIGCHLD to `parent` about `child` exiting with wait status `wstatus`.
pub fn notify_parent(parent: &Process, child: Pid, wstatus: i32) {
    let (code, status) = if wstatus & 0x7f == 0 {
        (CLD_EXITED, (wstatus >> 8) & 0xff)
    } else if wstatus & 0x80 != 0 {
        (CLD_DUMPED, wstatus & 0x7f)
    } else {
        (CLD_KILLED, wstatus & 0x7f)
    };
    post(
        parent,
        SIGCHLD,
        SigInfo {
            code,
            pid: child,
            status,
            addr: 0,
        },
    );
}
