//! Kernel threads and the scheduler (single core).
//!
//! * Every thread has its own kernel stack ([`kstack::KStack`], guard page below) and a
//!   saved [`Context`] (callee-saved registers, lr, sp). [`switch_context`] swaps them.
//! * Strict priorities ([`Priority`]), round-robin inside a level. The idle thread runs
//!   when nothing else is ready and sits in `wfi`.
//! * Preemption: the 100 Hz tick sets `NEED_RESCHED` when another thread of the same or
//!   higher priority is ready; the IRQ-exit hook (after EOI) then switches. Kernel code is
//!   preemptible wherever IRQs are unmasked; holding a `SpinLock` masks them, so a spinlock
//!   holder is never preempted. Never block (sleep, wait, Mutex::lock) holding a SpinLock.
//! * Sleeping uses the timer's one-shot deadline (this module owns it from [`init`] on),
//!   armed for the earliest sleeper; the tick re-checks sleepers as a fallback.
//! * A thread that belongs to a user process also carries its TTBR0 (address space + ASID),
//!   TPIDR_EL0 and FP/SIMD registers, swapped on every switch.
//!
//! Locking: all scheduler state is in one `SpinLock<Sched>`. `schedule` drops it before the
//! actual register switch but keeps IRQs masked across it; the resumed thread puts its own
//! IRQ state back.

#![allow(dead_code)]

pub mod kstack;
pub mod selftest;
mod wait;

#[allow(unused_imports)]
pub use wait::{Mutex, MutexGuard, Semaphore, WaitQueue};

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::arch::{asm, global_asm};
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::exceptions::{self, TrapFrame};
use crate::sync::{self, IrqGuard, SpinLock};
use crate::{mmu, timer};
use kstack::KStack;

pub type Tid = u32;

/// Scheduling priority. Higher runs first; equal priorities share the CPU round-robin.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Priority {
    Idle = 0,
    Low = 1,
    Normal = 2,
    High = 3,
}
const NPRIO: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Ready,
    Running,
    /// Waiting on a [`WaitQueue`].
    Blocked,
    /// In [`sleep_until`].
    Sleeping,
    /// Exited, waiting to be joined or reaped.
    Dead,
}

impl State {
    pub fn letter(self) -> char {
        match self {
            State::Ready => 'R',
            State::Running => 'R',
            State::Blocked => 'S',
            State::Sleeping => 'S',
            State::Dead => 'Z',
        }
    }
}

/// Callee-saved registers, lr and sp. Layout is shared with `switch.S`.
#[repr(C)]
#[derive(Default)]
struct Context {
    /// x19..x29, then x30 (lr).
    regs: [u64; 12],
    sp: u64,
}

/// FP/SIMD state of a user thread. Layout is shared with `switch.S`.
#[repr(C, align(16))]
#[derive(Clone)]
pub struct FpState {
    q: [u128; 32],
    fpcr: u64,
    fpsr: u64,
}

impl FpState {
    pub const fn zeroed() -> Self {
        Self {
            q: [0; 32],
            fpcr: 0,
            fpsr: 0,
        }
    }
}

/// A short thread / process name, like Linux's `comm` (15 bytes + NUL).
#[derive(Clone, Copy, Default)]
pub struct Comm([u8; 16]);

impl Comm {
    pub fn new(s: &str) -> Self {
        let mut c = [0u8; 16];
        let n = s.len().min(15);
        c[..n].copy_from_slice(&s.as_bytes()[..n]);
        Comm(c)
    }
    pub fn as_str(&self) -> &str {
        let n = self.0.iter().position(|&b| b == 0).unwrap_or(16);
        core::str::from_utf8(&self.0[..n]).unwrap_or("?")
    }
}

impl fmt::Display for Comm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// State a thread needs only when it runs user code.
pub struct UserState {
    /// Owning process.
    pub pid: u32,
    /// Root table | ASID << 48.
    pub ttbr0: u64,
    pub tpidr_el0: u64,
    pub fp: Box<FpState>,
}

pub struct Thread {
    tid: Tid,
    name: Comm,
    state: State,
    prio: Priority,
    ctx: Context,
    /// `None` only for thread 0, which runs on the boot stack.
    kstack: Option<KStack>,
    wake_at: u64,
    exit_code: i64,
    detached: bool,
    user: Option<UserState>,
    /// CPU time used, in ns.
    cpu_ns: u64,
    switched_in_at: u64,
}

struct Sched {
    threads: BTreeMap<Tid, Box<Thread>>,
    ready: [VecDeque<Tid>; NPRIO],
    sleeping: Vec<Tid>,
    /// Dead, detached threads waiting to have their stack freed.
    zombies: Vec<Tid>,
    current: Tid,
    idle: Tid,
    next_tid: Tid,
}

impl Sched {
    fn cur(&mut self) -> &mut Thread {
        let c = self.current;
        self.threads.get_mut(&c).expect("current thread missing")
    }

    fn pick(&mut self) -> Tid {
        for p in (1..NPRIO).rev() {
            if let Some(t) = self.ready[p].pop_front() {
                return t;
            }
        }
        self.idle
    }

    fn highest_ready(&self) -> Option<Priority> {
        (1..NPRIO)
            .rev()
            .find(|&p| !self.ready[p].is_empty())
            .map(|p| match p {
                1 => Priority::Low,
                2 => Priority::Normal,
                _ => Priority::High,
            })
    }

    /// Make `tid` runnable. Never allocates (capacity is reserved in `add`), so it is fine
    /// from IRQ context.
    fn make_ready(&mut self, tid: Tid) {
        let idle = self.idle;
        let cur_prio = self.cur().prio;
        let cur_is_idle = self.current == idle;
        let Some(t) = self.threads.get_mut(&tid) else {
            return;
        };
        t.state = State::Ready;
        let prio = t.prio;
        self.ready[prio as usize].push_back(tid);
        if cur_is_idle || prio > cur_prio {
            NEED_RESCHED.store(true, Ordering::Relaxed);
        }
    }

    fn add(&mut self, t: Box<Thread>) -> Tid {
        let tid = t.tid;
        self.threads.insert(tid, t);
        // Reserve so make_ready / sleep never allocate (they run from IRQs).
        let n = self.threads.len();
        for q in self.ready.iter_mut() {
            q.reserve(n.saturating_sub(q.len()));
        }
        self.sleeping.reserve(n.saturating_sub(self.sleeping.len()));
        tid
    }

    fn alloc_tid(&mut self) -> Tid {
        let t = self.next_tid;
        self.next_tid += 1;
        t
    }

    fn wake_expired(&mut self, now: u64) {
        let mut i = 0;
        while i < self.sleeping.len() {
            let tid = self.sleeping[i];
            let due = match self.threads.get(&tid) {
                Some(t) if t.state == State::Sleeping => t.wake_at <= now,
                _ => {
                    self.sleeping.swap_remove(i);
                    continue;
                }
            };
            if due {
                self.sleeping.swap_remove(i);
                self.make_ready(tid);
            } else {
                i += 1;
            }
        }
    }

    fn earliest_wake(&self) -> Option<u64> {
        self.sleeping
            .iter()
            .filter_map(|t| self.threads.get(t))
            .filter(|t| t.state == State::Sleeping)
            .map(|t| t.wake_at)
            .min()
    }
}

static SCHED: SpinLock<Option<Sched>> = SpinLock::new(None);
static NEED_RESCHED: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
/// Mirror of `Sched::current`, readable without the lock (crash dumps, current_tid).
static CURRENT: AtomicU32 = AtomicU32::new(0);
/// Woken whenever any thread exits (joiners wait on it).
static EXIT_WQ: WaitQueue = WaitQueue::new();

fn with<R>(f: impl FnOnce(&mut Sched) -> R) -> R {
    f(SCHED.lock().as_mut().expect("scheduler not initialised"))
}

global_asm!(
    include_str!("switch.S"),
    THREAD_START = sym thread_start,
    FIRST_RUN = sym first_run,
);

unsafe extern "C" {
    fn switch_context(prev: *mut Context, next: *const Context);
    fn thread_trampoline();
    fn user_trampoline();
    fn fp_save(s: *mut FpState);
    fn fp_restore(s: *const FpState);
}

// ---- setup ---------------------------------------------------------------------------

/// Turn the running boot code into thread 0 ("kmain"), create the idle thread and start
/// preemption. Call once, after `timer::init`, with IRQs enabled or not.
pub fn init() {
    // FP/SIMD on for EL0 and EL1 (CPACR_EL1.FPEN = 0b11). Only user threads use it; the
    // kernel is soft-float. Saved per user thread on switch.
    // SAFETY: system register write.
    unsafe {
        asm!(
            "mrs {t}, cpacr_el1",
            "orr {t}, {t}, #(3 << 20)",
            "msr cpacr_el1, {t}",
            "isb",
            t = out(reg) _,
            options(nostack)
        )
    };

    let main = Box::new(Thread {
        tid: 0,
        name: Comm::new("kmain"),
        state: State::Running,
        prio: Priority::Normal,
        ctx: Context::default(),
        kstack: None,
        wake_at: 0,
        exit_code: 0,
        detached: true,
        user: None,
        cpu_ns: 0,
        switched_in_at: timer::now_ns(),
    });
    let mut s = Sched {
        threads: BTreeMap::new(),
        ready: [const { VecDeque::new() }; NPRIO],
        sleeping: Vec::new(),
        zombies: Vec::new(),
        current: 0,
        idle: 0,
        next_tid: 1,
    };
    s.add(main);
    *SCHED.lock() = Some(s);

    let idle = new_kernel_thread("idle", Priority::Idle, idle_main, 0).expect("idle thread");
    with(|s| {
        s.idle = idle.tid;
        s.add(idle);
    });

    timer::set_tick_hook(on_tick);
    exceptions::set_irq_exit_hook(on_irq_exit);
    STARTED.store(true, Ordering::Release);
    crate::kprintln!(
        "[sched] up: kmain is tid 0, idle tid {}, {} KiB kernel stacks with guard, {} Hz preemption",
        with(|s| s.idle),
        kstack::KSTACK_SIZE >> 10,
        timer::TICK_HZ
    );
}

fn idle_main(_: usize) {
    loop {
        reap();
        // SAFETY: wait for an interrupt; IRQs are unmasked in thread context.
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}

// ---- thread creation -------------------------------------------------------------------

fn blank_thread(name: &str, prio: Priority, kstack: KStack) -> Box<Thread> {
    Box::new(Thread {
        tid: 0,
        name: Comm::new(name),
        state: State::Ready,
        prio,
        ctx: Context::default(),
        kstack: Some(kstack),
        wake_at: 0,
        exit_code: 0,
        detached: false,
        user: None,
        cpu_ns: 0,
        switched_in_at: 0,
    })
}

fn new_kernel_thread(
    name: &str,
    prio: Priority,
    entry: fn(usize),
    arg: usize,
) -> Option<Box<Thread>> {
    let kstack = KStack::new()?;
    let top = kstack.top() as u64;
    let mut t = blank_thread(name, prio, kstack);
    t.ctx.regs[0] = entry as *const () as u64; // x19
    t.ctx.regs[1] = arg as u64; // x20
    t.ctx.regs[11] = thread_trampoline as *const () as u64; // lr
    t.ctx.sp = top;
    t.tid = with(|s| s.alloc_tid());
    Some(t)
}

extern "C" fn thread_start(entry: usize, arg: usize) -> ! {
    first_run();
    sync::irq_enable();
    // SAFETY: `entry` was stored from a `fn(usize)` in new_kernel_thread.
    let f: fn(usize) = unsafe { core::mem::transmute(entry) };
    f(arg);
    exit(0)
}

/// Runs first thing in every new thread, IRQs still masked.
extern "C" fn first_run() {}

/// Handle to a spawned thread. Dropping it detaches the thread.
pub struct JoinHandle {
    tid: Tid,
}

impl JoinHandle {
    pub fn tid(&self) -> Tid {
        self.tid
    }

    /// Wait for the thread to exit and return its exit code.
    pub fn join(self) -> i64 {
        let tid = self.tid;
        core::mem::forget(self);
        join_tid(tid)
    }
}

impl Drop for JoinHandle {
    fn drop(&mut self) {
        let t = with(|s| {
            let t = s.threads.get_mut(&self.tid)?;
            if t.state == State::Dead {
                s.threads.remove(&self.tid)
            } else {
                t.detached = true;
                None
            }
        });
        drop(t);
    }
}

fn join_tid(tid: Tid) -> i64 {
    EXIT_WQ.wait_until(|| with(|s| s.threads.get(&tid).is_none_or(|t| t.state == State::Dead)));
    let t = with(|s| s.threads.remove(&tid));
    t.map_or(-1, |t| t.exit_code)
}

fn closure_entry<F: FnOnce() + Send + 'static>(arg: usize) {
    // SAFETY: `arg` is the Box<F> leaked by spawn_prio, used exactly once.
    let f = unsafe { Box::from_raw(arg as *mut F) };
    f();
}

/// Start a kernel thread at normal priority running `f`.
pub fn spawn<F: FnOnce() + Send + 'static>(name: &str, f: F) -> JoinHandle {
    spawn_prio(name, Priority::Normal, f)
}

/// Start a kernel thread. Panics if out of memory for its stack.
pub fn spawn_prio<F: FnOnce() + Send + 'static>(name: &str, prio: Priority, f: F) -> JoinHandle {
    reap();
    let arg = Box::into_raw(Box::new(f)) as usize;
    let t = new_kernel_thread(name, prio, closure_entry::<F>, arg).expect("spawn: out of memory");
    let _g = IrqGuard::new();
    let tid = with(|s| {
        let tid = s.add(t);
        s.make_ready(tid);
        tid
    });
    JoinHandle { tid }
}

/// Start a thread that enters user mode with `frame` (a TrapFrame for EL0). The frame is
/// copied to the top of the new kernel stack, where every later exception from EL0 will put
/// its frame too. Returns the new tid; the thread is detached (processes reap themselves).
pub fn spawn_user(name: &str, frame: &TrapFrame, user: UserState) -> Option<Tid> {
    reap();
    let kstack = KStack::new()?;
    let fsz = core::mem::size_of::<TrapFrame>();
    let frame_at = kstack.top() - fsz;
    // SAFETY: the top `fsz` bytes of a fresh stack we own; 16-byte aligned (fsz is).
    unsafe { (frame_at as *mut TrapFrame).write(*frame) };
    let mut t = blank_thread(name, Priority::Normal, kstack);
    t.ctx.regs[11] = user_trampoline as *const () as u64;
    t.ctx.sp = frame_at as u64;
    t.user = Some(user);
    t.detached = true;
    let _g = IrqGuard::new();
    Some(with(|s| {
        t.tid = s.alloc_tid();
        let tid = s.add(t);
        s.make_ready(tid);
        tid
    }))
}

/// Free the stacks of dead, detached threads.
pub fn reap() {
    let dead: Vec<Box<Thread>> = with(|s| {
        let cur = s.current;
        let mut out = Vec::new();
        s.zombies.retain(|&tid| {
            if tid == cur {
                return true;
            }
            if let Some(t) = s.threads.remove(&tid) {
                out.push(t);
            }
            false
        });
        out
    });
    drop(dead); // outside the lock: frees stacks (kernel page tables, pmm)
}

// ---- switching -----------------------------------------------------------------------

/// Switch to the best ready thread (possibly the current one again). IRQs must be masked.
/// Returns when the calling thread is scheduled again.
fn schedule() {
    debug_assert!(!sync::irqs_enabled());
    NEED_RESCHED.store(false, Ordering::Relaxed);
    let sw = with(|s| {
        let now = timer::now_ns();
        let cur = s.current;
        let idle = s.idle;
        {
            let t = s.cur();
            t.cpu_ns += now.saturating_sub(t.switched_in_at);
            if t.state == State::Running {
                t.state = State::Ready;
                let prio = t.prio as usize;
                if cur != idle {
                    s.ready[prio].push_back(cur);
                }
            }
        }
        let next = s.pick();
        let nt = s.threads.get_mut(&next).expect("picked a missing thread");
        nt.state = State::Running;
        nt.switched_in_at = now;
        let next_ptr: *mut Thread = &mut **nt;
        if next == cur {
            return None;
        }
        s.current = next;
        CURRENT.store(next, Ordering::Relaxed);
        let prev_ptr: *mut Thread = &mut **s.threads.get_mut(&cur).unwrap();
        Some((prev_ptr, next_ptr))
    });
    if let Some((prev, next)) = sw {
        // SAFETY: both threads stay allocated: prev is not reaped while it is current or
        // until it is switched out (zombies skip the current thread), next is current now.
        unsafe { switch(prev, next) };
    }
}

unsafe fn switch(prev: *mut Thread, next: *mut Thread) {
    // SAFETY: IRQs masked, pointers valid (see schedule).
    unsafe {
        if let Some(u) = (*prev).user.as_mut() {
            asm!("mrs {}, tpidr_el0", out(reg) u.tpidr_el0, options(nomem, nostack));
            fp_save(&mut *u.fp);
        }
        let ttbr0 = (*next)
            .user
            .as_ref()
            .map_or_else(mmu::kernel_ttbr0, |u| u.ttbr0);
        set_ttbr0(ttbr0);
        if let Some(u) = (*next).user.as_ref() {
            asm!("msr tpidr_el0, {}", in(reg) u.tpidr_el0, options(nomem, nostack));
            fp_restore(&*u.fp);
        }
        switch_context(&mut (*prev).ctx, &(*next).ctx);
    }
}

/// Load TTBR0_EL1 if it differs. With per-process ASIDs no TLB flush is needed.
pub fn set_ttbr0(v: u64) {
    let cur: u64;
    // SAFETY: system register access; the kernel half (L0 slots 0..2) is identical in every
    // space, so switching never changes the mapping of the code doing it.
    unsafe {
        asm!("mrs {}, ttbr0_el1", out(reg) cur, options(nomem, nostack));
        if cur != v {
            asm!("msr ttbr0_el1, {}", "isb", in(reg) v, options(nostack));
        }
    }
}

/// Give up the CPU to another ready thread of the same or higher priority.
pub fn yield_now() {
    if !STARTED.load(Ordering::Acquire) {
        return;
    }
    let _g = IrqGuard::new();
    schedule();
}

/// Switch now if a wakeup asked for it and IRQs are on (i.e. we are allowed to block).
pub fn preempt_point() {
    if NEED_RESCHED.load(Ordering::Relaxed) && sync::irqs_enabled() {
        yield_now();
    }
}

/// True if something more deserving than the current thread is waiting.
pub fn need_resched() -> bool {
    NEED_RESCHED.load(Ordering::Relaxed)
}

/// Block the current thread until someone calls [`wake`] on it. IRQs must be masked and the
/// caller must already have recorded the thread somewhere a waker will find it.
pub(crate) fn block_current() {
    debug_assert!(!sync::irqs_enabled());
    with(|s| s.cur().state = State::Blocked);
    schedule();
}

/// Make a blocked thread ready. Returns false if it was not blocked. IRQ-safe.
pub fn wake(tid: Tid) -> bool {
    let _g = IrqGuard::new();
    with(|s| match s.threads.get(&tid) {
        Some(t) if t.state == State::Blocked => {
            s.make_ready(tid);
            true
        }
        _ => false,
    })
}

// ---- sleeping ------------------------------------------------------------------------

/// Sleep until `timer::now_ns()` reaches `deadline_ns`.
pub fn sleep_until(deadline_ns: u64) {
    if !STARTED.load(Ordering::Acquire) {
        while timer::now_ns() < deadline_ns {
            core::hint::spin_loop();
        }
        return;
    }
    let _g = IrqGuard::new();
    if timer::now_ns() >= deadline_ns {
        return;
    }
    with(|s| {
        let me = s.current;
        let t = s.cur();
        t.wake_at = deadline_ns;
        t.state = State::Sleeping;
        if !s.sleeping.contains(&me) {
            s.sleeping.push(me);
        }
        arm_deadline(s);
    });
    schedule();
}

pub fn sleep_ns(ns: u64) {
    sleep_until(timer::now_ns().saturating_add(ns));
}

pub fn sleep_ms(ms: u64) {
    sleep_ns(ms.saturating_mul(1_000_000));
}

fn arm_deadline(s: &Sched) {
    if let Some(at) = s.earliest_wake() {
        timer::set_deadline(at, on_deadline);
    }
}

fn on_deadline(_frame: &mut TrapFrame) {
    with(|s| {
        s.wake_expired(timer::now_ns());
        arm_deadline(s);
    });
}

fn on_tick(_frame: &mut TrapFrame) {
    with(|s| {
        s.wake_expired(timer::now_ns());
        let cur_prio = s.cur().prio;
        let is_idle = s.current == s.idle;
        if let Some(p) = s.highest_ready()
            && (is_idle || p >= cur_prio)
        {
            NEED_RESCHED.store(true, Ordering::Relaxed);
        }
    });
}

fn on_irq_exit(_frame: &mut TrapFrame) {
    if NEED_RESCHED.load(Ordering::Relaxed) && STARTED.load(Ordering::Relaxed) {
        schedule();
    }
}

// ---- exit ----------------------------------------------------------------------------

/// End the current thread with `code`.
pub fn exit(code: i64) -> ! {
    sync::irq_disable();
    with(|s| {
        let me = s.current;
        let t = s.cur();
        t.state = State::Dead;
        t.exit_code = code;
        if t.detached && t.kstack.is_some() {
            s.zombies.push(me);
        }
    });
    EXIT_WQ.wake_all();
    schedule();
    unreachable!("dead thread rescheduled");
}

// ---- queries -------------------------------------------------------------------------

pub fn current_tid() -> Tid {
    CURRENT.load(Ordering::Relaxed)
}

/// Pid of the process the current thread belongs to (0 for kernel threads).
pub fn current_pid() -> u32 {
    with(|s| s.cur().user.as_ref().map_or(0, |u| u.pid))
}

/// Run `f` on the current thread's user state (None for kernel threads).
pub fn with_current_user<R>(f: impl FnOnce(Option<&mut UserState>) -> R) -> R {
    with(|s| f(s.cur().user.as_mut()))
}

/// Rename the current thread.
pub fn set_current_name(name: &str) {
    with(|s| s.cur().name = Comm::new(name));
}

/// For the stack-overflow report: no locking, no allocation.
pub fn current_for_crash() -> (Tid, Comm) {
    let tid = CURRENT.load(Ordering::Relaxed);
    let name = SCHED
        .try_lock()
        .and_then(|g| g.as_ref().and_then(|s| s.threads.get(&tid).map(|t| t.name)))
        .unwrap_or_else(|| Comm::new("?"));
    (tid, name)
}

/// One line of `ps`-style information about a thread.
#[derive(Clone, Copy)]
pub struct ThreadInfo {
    pub tid: Tid,
    pub pid: u32,
    pub name: Comm,
    pub state: State,
    pub prio: Priority,
    pub cpu_ns: u64,
}

pub fn threads() -> Vec<ThreadInfo> {
    with(|s| {
        s.threads
            .values()
            .map(|t| ThreadInfo {
                tid: t.tid,
                pid: t.user.as_ref().map_or(0, |u| u.pid),
                name: t.name,
                state: t.state,
                prio: t.prio,
                cpu_ns: t.cpu_ns,
            })
            .collect()
    })
}

pub fn started() -> bool {
    STARTED.load(Ordering::Acquire)
}
