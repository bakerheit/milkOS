//! Kernel threads and the scheduler (SMP).
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
//! * Run queues ([`Policy`]): by default every CPU has its own ready queues (per priority).
//!   A thread stays on the CPU it last ran on (warm caches and TLB). A wakeup goes to its
//!   last CPU if that is idle; stays on the waker's CPU if that was its last (a pipe or
//!   futex partner, about to block), with an idle CPU kicked as backup; else to an idle
//!   CPU, else to one running something less important, else to the shortest queue. A CPU with nothing of its own *steals* the
//!   longest-waiting thread from the busiest queue, and on the tick a CPU pulls one thread
//!   from a queue two or more longer than its own. `Policy::Global` (one shared queue, the
//!   old behaviour) stays selectable at run time for comparison: `print global > /proc/sched`.
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
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use crate::exceptions::{self, TrapFrame};
use crate::percpu::{self, MAX_CPUS};
use crate::sync::{self, IrqGuard, SpinLock};
use crate::{gic, mmu, timer};
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
    pub q: [u128; 32],
    pub fpcr: u64,
    pub fpsr: u64,
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
    /// Root table | ASID << 48 (shared by every thread of the process).
    pub ttbr0: u64,
    pub tpidr_el0: u64,
    pub fp: Box<FpState>,
    /// CLONE_CHILD_CLEARTID / set_tid_address: on thread exit, zero `*clear_child_tid` and
    /// futex-wake it. 0 = none.
    pub clear_child_tid: usize,
}

pub struct Thread {
    tid: Tid,
    name: Comm,
    state: State,
    prio: Priority,
    ctx: Context,
    /// `None` only for thread 0 (boot stack); secondaries' idle threads own their boot stack.
    kstack: Option<KStack>,
    wake_at: u64,
    exit_code: i64,
    detached: bool,
    user: Option<UserState>,
    /// CPU time used, in ns.
    cpu_ns: u64,
    switched_in_at: u64,
    /// CPU it runs / last ran on (for a new thread: where it was created).
    cpu: usize,
    /// Has run at least once (so a change of `cpu` is a migration).
    ran: bool,
    /// Only ever runs on this CPU (idle threads; optional affinity).
    pinned: Option<usize>,
    /// A wake arrived while it was running (between "I'm on a wait queue" and "block"):
    /// the next block returns at once instead of sleeping. This is what makes waits race
    /// free across CPUs.
    wake_pending: bool,
    /// Set (under the lock) when its CPU decides to switch away from it, cleared once its
    /// registers are saved. Another CPU must not load its context, and nobody may free it,
    /// while this is set.
    switching_out: AtomicBool,
}

#[derive(Clone, Copy, Default)]
struct CpuSched {
    current: Tid,
    idle: Tid,
    /// This CPU has joined the scheduler (its idle thread exists and `current`/`idle` are
    /// real). A CPU is `percpu` online a little earlier; until it joins, nothing may be
    /// queued on it, kicked to it or scheduled on it: its `current`/`idle` read as tid 0
    /// (kmain), which made an early IPI or tick "switch away from kmain" on the wrong CPU.
    joined: bool,
}

/// How ready threads are queued (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    /// One ready queue per priority shared by all CPUs.
    Global,
    /// Per-CPU ready queues with wake affinity, work stealing and tick-time balancing.
    PerCpu,
}

type Queues = [VecDeque<Tid>; NPRIO];

/// Scheduler counters, for /proc/sched.
#[derive(Clone, Copy, Default)]
pub struct CpuStats {
    /// Threads taken from another CPU's queue by this CPU (idle steal or balance pull).
    pub steals: u64,
    /// Threads that started running here after last running on another CPU.
    pub migrations_in: u64,
    /// Ready threads queued here right now.
    pub queued: usize,
}

struct Sched {
    threads: BTreeMap<Tid, Box<Thread>>,
    policy: Policy,
    /// Policy::Global: the one ready queue per priority (all CPUs pick from it).
    global: Queues,
    /// Policy::PerCpu: each CPU's ready queues.
    rq: [Queues; MAX_CPUS],
    stats: [CpuStats; MAX_CPUS],
    sleeping: Vec<Tid>,
    /// Dead, detached threads waiting to have their stack freed.
    zombies: Vec<Tid>,
    cpus: [CpuSched; MAX_CPUS],
    next_tid: Tid,
}

impl Sched {
    fn cur_tid(&self) -> Tid {
        self.cpus[percpu::id()].current
    }

    fn cur(&mut self) -> &mut Thread {
        let c = self.cur_tid();
        self.threads.get_mut(&c).expect("current thread missing")
    }

    fn is_current_somewhere(&self, tid: Tid) -> bool {
        self.joined_cpus().any(|c| self.cpus[c].current == tid)
    }

    /// CPUs that have joined the scheduler (see [`CpuSched::joined`]).
    fn joined_cpus(&self) -> impl Iterator<Item = usize> + '_ {
        percpu::online().filter(|&c| self.cpus[c].joined)
    }

    fn joined_mask(&self) -> u32 {
        self.joined_cpus().fold(0, |m, c| m | 1 << c)
    }

    /// A dead thread whose stack nobody is using any more.
    fn removable(&self, tid: Tid) -> bool {
        self.threads.get(&tid).is_some_and(|t| {
            t.state == State::Dead
                && !t.switching_out.load(Ordering::Acquire)
                && !self.is_current_somewhere(tid)
        })
    }

    fn runnable_on(&self, tid: Tid, cpu: usize) -> bool {
        self.threads
            .get(&tid)
            .is_some_and(|t| t.pinned.is_none_or(|c| c == cpu))
    }

    fn prio_of(p: usize) -> Priority {
        match p {
            0 => Priority::Idle,
            1 => Priority::Low,
            2 => Priority::Normal,
            _ => Priority::High,
        }
    }

    /// Highest priority with a thread in `q` that may run on `cpu`, and its index there.
    fn best_in(&self, q: &Queues, cpu: usize) -> Option<(usize, usize)> {
        (1..NPRIO)
            .rev()
            .find_map(|p| Some((p, q[p].iter().position(|&t| self.runnable_on(t, cpu))?)))
    }

    fn queued(&self, cpu: usize) -> usize {
        self.rq[cpu].iter().map(|q| q.len()).sum()
    }

    /// Per-CPU mode: what `cpu` should run next from another CPU's queue, if anything
    /// beats its own best (`own`, a priority): a strictly higher priority, or anything at
    /// all when it has nothing. Among equals, the busiest queue. (prio, source cpu, index)
    fn steal_candidate(&self, cpu: usize, own: Option<usize>) -> Option<(usize, usize, usize)> {
        let mut best: Option<(usize, usize, usize)> = None;
        for c in self.joined_cpus().filter(|&c| c != cpu) {
            let Some((p, i)) = self.best_in(&self.rq[c], cpu) else {
                continue;
            };
            if own.is_some_and(|o| p <= o) {
                continue;
            }
            let better = match best {
                None => true,
                Some((bp, bc, _)) => p > bp || (p == bp && self.queued(c) > self.queued(bc)),
            };
            if better {
                best = Some((p, c, i));
            }
        }
        best
    }

    fn pick(&mut self, cpu: usize) -> Tid {
        match self.policy {
            Policy::Global => {
                if let Some((p, i)) = self.best_in(&self.global, cpu) {
                    return self.global[p].remove(i).unwrap();
                }
            }
            Policy::PerCpu => {
                let own = self.best_in(&self.rq[cpu], cpu);
                if let Some((p, c, i)) = self.steal_candidate(cpu, own.map(|o| o.0)) {
                    self.stats[cpu].steals += 1;
                    return self.rq[c][p].remove(i).unwrap();
                }
                if let Some((p, i)) = own {
                    return self.rq[cpu][p].remove(i).unwrap();
                }
            }
        }
        self.cpus[cpu].idle
    }

    fn has_ready_for(&self, cpu: usize) -> Option<Priority> {
        let p = match self.policy {
            Policy::Global => self.best_in(&self.global, cpu).map(|b| b.0),
            Policy::PerCpu => {
                let own = self.best_in(&self.rq[cpu], cpu).map(|b| b.0);
                self.steal_candidate(cpu, own).map(|b| b.0).or(own)
            }
        };
        p.map(Self::prio_of)
    }

    /// Should `cpu` switch away from what it runs now? (The tick's round-robin check.) A
    /// busy CPU only takes a thread from another queue if it is more important than its
    /// current one, so equal threads don't bounce between CPUs.
    fn should_preempt(&self, cpu: usize) -> bool {
        let idle = self.is_idle(cpu);
        let cur = self.running_prio(cpu) as usize;
        match self.policy {
            Policy::Global => self
                .best_in(&self.global, cpu)
                .is_some_and(|(p, _)| idle || p >= cur),
            Policy::PerCpu => {
                let own = self.best_in(&self.rq[cpu], cpu).map(|b| b.0);
                if idle {
                    own.is_some() || self.steal_candidate(cpu, own).is_some()
                } else {
                    own.is_some_and(|p| p >= cur) || self.steal_candidate(cpu, Some(cur)).is_some()
                }
            }
        }
    }

    /// Tick-time balancing (per-CPU mode): if some queue is at least two threads longer
    /// than ours, pull its longest-waiting thread over. Fixes imbalance that stealing
    /// alone doesn't (stealing only happens when a CPU runs dry).
    fn balance(&mut self, cpu: usize) {
        if self.policy != Policy::PerCpu {
            return;
        }
        let mine = self.queued(cpu);
        let Some(src) = self
            .joined_cpus()
            .filter(|&c| c != cpu && self.queued(c) >= mine + 2)
            .max_by_key(|&c| self.queued(c))
        else {
            return;
        };
        for p in (1..NPRIO).rev() {
            if let Some(i) = self.rq[src][p]
                .iter()
                .position(|&t| self.runnable_on(t, cpu))
            {
                let tid = self.rq[src][p].remove(i).unwrap();
                self.rq[cpu][p].push_back(tid);
                self.stats[cpu].steals += 1;
                return;
            }
        }
    }

    fn is_idle(&self, c: usize) -> bool {
        self.cpus[c].current == self.cpus[c].idle
    }

    fn running_prio(&self, c: usize) -> Priority {
        self.threads
            .get(&self.cpus[c].current)
            .map_or(Priority::Idle, |r| r.prio)
    }

    /// Make `tid` runnable and get a CPU to run it: an idle CPU if there is one, else one
    /// running something less important. Never allocates (capacity is reserved in `add`),
    /// so it is fine from IRQ context.
    fn make_ready(&mut self, tid: Tid) {
        let joined = self.joined_mask();
        let Some(t) = self.threads.get_mut(&tid) else {
            return;
        };
        t.state = State::Ready;
        t.wake_at = 0;
        let (prio, pinned, last) = (t.prio, t.pinned, t.cpu);
        let ran = t.ran;
        let me = percpu::id();
        let candidates = || {
            percpu::online().filter(move |&c| joined & 1 << c != 0 && pinned.is_none_or(|p| p == c))
        };
        if self.policy == Policy::Global {
            self.global[prio as usize].push_back(tid);
            // An idle CPU first (ourselves included).
            let target = candidates()
                .find(|&c| self.is_idle(c) && c == me)
                .or_else(|| candidates().find(|&c| self.is_idle(c)))
                .or_else(|| {
                    candidates()
                        .filter(|&c| self.running_prio(c) < prio)
                        .min_by_key(|&c| self.running_prio(c))
                });
            if let Some(c) = target {
                kick(c);
            }
            return;
        }
        // Per-CPU. "Free" = idle, nothing queued, not already kicked.
        // 1. Its last CPU if free (cache warm, nothing to wait for).
        // 2. Woken by a thread on its own last CPU (a pipe/futex partner, typically about
        //    to block): queue it right here, where it is cache hot; if we don't block soon,
        //    a free CPU we kick steals it. (Not for new threads: those spread out. Only if
        //    it preempts us or there is such a free CPU.)
        // 3. Any free CPU, this one first.
        // 4. One running something less important (its last CPU on a tie).
        // 5. Else the shortest queue (its last CPU on a tie): the tick round-robins it.
        let free = |c: usize| {
            self.is_idle(c)
                && self.queued(c) == 0
                && !percpu::get(c).need_resched.load(Ordering::Relaxed)
        };
        let last_ok = candidates().any(|c| c == last);
        if last_ok && free(last) {
            self.rq[last][prio as usize].push_back(tid);
            kick(last);
            return;
        }
        if ran && last == me && last_ok {
            let backup = percpu::online().find(|&c| c != me && joined & 1 << c != 0 && free(c));
            let preempt = self.running_prio(me) < prio;
            // Strict priorities: if we won't preempt here and there is no free CPU to fall
            // back on, but another CPU runs a less important (non-idle) thread, go there.
            let lower_elsewhere =
                candidates().any(|c| c != me && !self.is_idle(c) && self.running_prio(c) < prio);
            if preempt || backup.is_some() || !lower_elsewhere {
                self.rq[me][prio as usize].push_back(tid);
                if preempt {
                    kick(me);
                } else if let Some(c) = backup {
                    kick(c);
                }
                return;
            }
        }
        let target = candidates()
            .find(|&c| c == me && free(c))
            .or_else(|| candidates().find(|&c| free(c)))
            .or_else(|| {
                candidates()
                    .filter(|&c| self.running_prio(c) < prio)
                    .min_by_key(|&c| (self.running_prio(c), c != last))
            });
        let kick_it = target.is_some();
        let target = target.unwrap_or_else(|| {
            candidates()
                .min_by_key(|&c| (self.queued(c), c != last))
                .unwrap_or(last)
        });
        self.rq[target][prio as usize].push_back(tid);
        if kick_it {
            kick(target);
        }
    }

    /// Queue the current thread of `cpu` again after it was preempted / yielded.
    fn requeue(&mut self, cpu: usize, tid: Tid) {
        let prio = self.threads[&tid].prio as usize;
        match self.policy {
            Policy::Global => self.global[prio].push_back(tid),
            Policy::PerCpu => self.rq[cpu][prio].push_back(tid),
        }
    }

    fn add(&mut self, t: Box<Thread>) -> Tid {
        let tid = t.tid;
        self.threads.insert(tid, t);
        // Reserve so make_ready / sleep never allocate (they run from IRQs).
        let n = self.threads.len();
        for q in self.global.iter_mut().chain(self.rq.iter_mut().flatten()) {
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
                Some(t)
                    if t.state == State::Sleeping
                        || (t.state == State::Blocked && t.wake_at != 0) =>
                {
                    t.wake_at <= now
                }
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
            .filter(|t| t.state == State::Sleeping || (t.state == State::Blocked && t.wake_at != 0))
            .map(|t| t.wake_at)
            .min()
    }
}

/// Ask `cpu` to reschedule: locally just a flag, remotely the flag plus an SGI.
fn kick(cpu: usize) {
    percpu::get(cpu).need_resched.store(true, Ordering::Release);
    if cpu != percpu::id() {
        gic::send_sgi(IPI_RESCHED, 1 << cpu);
    }
}

/// SGI used to make another CPU reschedule.
pub const IPI_RESCHED: u32 = 0;

fn on_ipi(_irq: u32, _frame: &mut TrapFrame) {
    percpu::this().ipis.fetch_add(1, Ordering::Relaxed);
    // The work happens in on_irq_exit (need_resched is set).
}

static SCHED: SpinLock<Option<Sched>> = SpinLock::new(None);
static STARTED: AtomicBool = AtomicBool::new(false);
/// Woken whenever any thread exits (joiners wait on it).
static EXIT_WQ: WaitQueue = WaitQueue::new();
/// Per CPU: the thread whose `switching_out` to clear once the switch has happened.
static PREV: [AtomicPtr<Thread>; MAX_CPUS] =
    [const { AtomicPtr::new(core::ptr::null_mut()) }; MAX_CPUS];

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

fn thread_struct(tid: Tid, name: &str, prio: Priority, kstack: Option<KStack>) -> Box<Thread> {
    Box::new(Thread {
        tid,
        name: Comm::new(name),
        state: State::Ready,
        prio,
        ctx: Context::default(),
        kstack,
        wake_at: 0,
        exit_code: 0,
        detached: false,
        user: None,
        cpu_ns: 0,
        switched_in_at: 0,
        cpu: percpu::id(),
        ran: false,
        pinned: None,
        wake_pending: false,
        switching_out: AtomicBool::new(false),
    })
}

/// Turn the running boot code into thread 0 ("kmain"), create the boot CPU's idle thread
/// and start preemption. Call once, after `timer::init`, before `smp::init`.
pub fn init() {
    enable_fp();
    let mut main = thread_struct(0, "kmain", Priority::Normal, None);
    main.state = State::Running;
    main.detached = true;
    main.switched_in_at = timer::now_ns();
    let mut s = Sched {
        threads: BTreeMap::new(),
        policy: Policy::PerCpu,
        global: [const { VecDeque::new() }; NPRIO],
        rq: [const { [const { VecDeque::new() }; NPRIO] }; MAX_CPUS],
        stats: [CpuStats::default(); MAX_CPUS],
        sleeping: Vec::new(),
        zombies: Vec::new(),
        cpus: [CpuSched::default(); MAX_CPUS],
        next_tid: 1,
    };
    s.add(main);
    *SCHED.lock() = Some(s);

    let mut idle = new_kernel_thread("idle", Priority::Idle, idle_main, 0).expect("idle thread");
    idle.pinned = Some(0);
    with(|s| {
        s.cpus[0].idle = idle.tid;
        s.cpus[0].joined = true;
        s.add(idle);
    });
    percpu::get(0)
        .idle
        .store(with(|s| s.cpus[0].idle), Ordering::Relaxed);

    gic::register(IPI_RESCHED, on_ipi).expect("reschedule IPI");
    gic::enable(IPI_RESCHED).ok();
    gic::register(crate::tlb::IPI_SYNC, crate::tlb::on_sync_ipi).expect("TLB sync IPI");
    gic::enable(crate::tlb::IPI_SYNC).ok();
    timer::set_tick_hook(on_tick);
    exceptions::set_irq_exit_hook(on_irq_exit);
    STARTED.store(true, Ordering::Release);
    crate::kprintln!(
        "[sched] up: kmain is tid 0, idle tid {}, {} KiB kernel stacks with guard, {} Hz preemption",
        with(|s| s.cpus[0].idle),
        kstack::KSTACK_SIZE >> 10,
        timer::TICK_HZ
    );
}

/// FP/SIMD on for EL0 and EL1 (CPACR_EL1.FPEN = 0b11), on this CPU. Only user threads use
/// it; the kernel is soft-float. Saved per user thread on switch.
pub fn enable_fp() {
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
}

/// A secondary CPU joins the scheduler: the code running now (on `stack`) becomes that
/// CPU's idle thread. Call with IRQs masked; they are unmasked once the CPU has joined.
pub fn run_secondary(cpu: usize, stack: KStack) -> ! {
    let idle = {
        let _g = IrqGuard::new();
        with(|s| {
            let tid = s.alloc_tid();
            let mut t = thread_struct(tid, "idle", Priority::Idle, Some(stack));
            t.name = Comm::new(match cpu {
                1 => "idle/1",
                2 => "idle/2",
                3 => "idle/3",
                4 => "idle/4",
                5 => "idle/5",
                6 => "idle/6",
                _ => "idle/7",
            });
            t.state = State::Running;
            t.pinned = Some(cpu);
            t.cpu = cpu;
            t.switched_in_at = timer::now_ns();
            s.add(t);
            s.cpus[cpu] = CpuSched {
                current: tid,
                idle: tid,
                joined: true,
            };
            tid
        })
    };
    percpu::get(cpu).current.store(idle, Ordering::Relaxed);
    percpu::get(cpu).idle.store(idle, Ordering::Release); // smp::init waits for this
    // Only now: an IPI or tick before the CPU had joined would schedule with garbage
    // per-CPU state (smp.rs keeps IRQs masked until here).
    sync::irq_enable();
    idle_main(cpu);
    unreachable!()
}

fn idle_main(_: usize) {
    loop {
        reap();
        let cpu = percpu::id();
        if with(|s| s.has_ready_for(cpu).is_some()) {
            yield_now();
        }
        // SAFETY: wait for an interrupt (a tick, a device, or a reschedule IPI); IRQs are
        // unmasked in thread context, so a kick that races with this is taken first.
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}

// ---- thread creation -------------------------------------------------------------------

fn new_kernel_thread(
    name: &str,
    prio: Priority,
    entry: fn(usize),
    arg: usize,
) -> Option<Box<Thread>> {
    let kstack = KStack::new()?;
    let top = kstack.top() as u64;
    let tid = with(|s| s.alloc_tid());
    let mut t = thread_struct(tid, name, prio, Some(kstack));
    t.ctx.regs[0] = entry as *const () as u64; // x19
    t.ctx.regs[1] = arg as u64; // x20
    t.ctx.regs[11] = thread_trampoline as *const () as u64; // lr
    t.ctx.sp = top;
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

/// Runs first thing in every new thread (IRQs still masked): finish the switch that got us
/// here, as `schedule` does after `switch_context` returns.
extern "C" fn first_run() {
    after_switch();
}

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
            let state = s.threads.get(&self.tid)?.state;
            if state == State::Dead {
                if s.removable(self.tid) {
                    return s.threads.remove(&self.tid);
                }
                s.zombies.push(self.tid); // still switching out: reap() frees it
                return None;
            }
            s.threads.get_mut(&self.tid)?.detached = true;
            None
        });
        drop(t);
    }
}

fn join_tid(tid: Tid) -> i64 {
    EXIT_WQ.wait_until(|| with(|s| s.threads.get(&tid).is_none_or(|t| t.state == State::Dead)));
    // It is dead, but its CPU may still be switching away from it: wait for that.
    loop {
        let r = with(|s| {
            if !s.threads.contains_key(&tid) {
                return Some(None);
            }
            s.removable(tid).then(|| s.threads.remove(&tid))
        });
        match r {
            Some(t) => return t.map_or(-1, |t| t.exit_code),
            None => core::hint::spin_loop(),
        }
    }
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
    spawn_on(name, prio, None, f)
}

/// Start a kernel thread that only ever runs on `cpu` (None: any CPU).
pub fn spawn_on<F: FnOnce() + Send + 'static>(
    name: &str,
    prio: Priority,
    cpu: Option<usize>,
    f: F,
) -> JoinHandle {
    reap();
    let arg = Box::into_raw(Box::new(f)) as usize;
    let mut t =
        new_kernel_thread(name, prio, closure_entry::<F>, arg).expect("spawn: out of memory");
    t.pinned = cpu;
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
    let mut t = thread_struct(0, name, Priority::Normal, Some(kstack));
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

/// Free the stacks of dead, detached threads (those no CPU is still leaving).
pub fn reap() {
    let dead: Vec<Box<Thread>> = with(|s| {
        let mut out = Vec::new();
        let zombies = core::mem::take(&mut s.zombies);
        for tid in zombies {
            if s.removable(tid) {
                if let Some(t) = s.threads.remove(&tid) {
                    out.push(t);
                }
            } else if s.threads.contains_key(&tid) {
                s.zombies.push(tid);
            }
        }
        out
    });
    drop(dead); // outside the lock: frees stacks (kernel page tables, pmm)
}

/// `reap`, and keep at it (up to 200 ms) until no thread of the given processes is left
/// and no dead thread is waiting for its CPU to finish leaving it. For tests that count
/// frames (a process can be a zombie while its thread is still on its way out).
pub fn reap_all_of(pids: &[u32]) {
    let t0 = timer::now_ns();
    loop {
        reap();
        let done = with(|s| {
            s.zombies.is_empty()
                && !s
                    .threads
                    .values()
                    .any(|t| t.user.as_ref().is_some_and(|u| pids.contains(&u.pid)))
        });
        if done || timer::now_ns() - t0 > 200_000_000 {
            return;
        }
        core::hint::spin_loop();
    }
}

pub fn reap_all() {
    reap_all_of(&[]);
}

// ---- switching -----------------------------------------------------------------------

/// What the current thread does as it calls into the scheduler.
#[derive(Clone, Copy)]
enum Leave {
    /// Stays runnable (yield, preemption).
    Yield,
    /// Waits for `wake` (unless one is already pending); optional timeout (now_ns time).
    Block(Option<u64>),
    /// Sleeps until the given now_ns time.
    Sleep(u64),
    Exit(i64),
}

/// Leave the CPU as `how` says and run the best ready thread (possibly the current one
/// again). IRQs must be masked. Returns when the calling thread runs again.
fn schedule(how: Leave) {
    debug_assert!(!sync::irqs_enabled());
    let cpu = percpu::id();
    percpu::get(cpu)
        .need_resched
        .store(false, Ordering::Relaxed);
    let sw = with(|s| {
        if !s.cpus[cpu].joined {
            return None; // not part of the scheduler yet (run_secondary): nothing to switch
        }
        let now = timer::now_ns();
        let cur = s.cpus[cpu].current;
        let idle = s.cpus[cpu].idle;
        let mut arm = false;
        {
            let t = s.threads.get_mut(&cur).expect("current thread missing");
            let ran = now.saturating_sub(t.switched_in_at);
            t.cpu_ns += ran;
            t.switched_in_at = now;
            let pc = percpu::get(cpu);
            if cur == idle {
                pc.idle_ns.fetch_add(ran, Ordering::Relaxed);
            } else {
                pc.busy_ns.fetch_add(ran, Ordering::Relaxed);
            }
            match how {
                Leave::Yield => {
                    if t.state == State::Running {
                        t.state = State::Ready;
                    }
                }
                Leave::Block(deadline) => {
                    if t.wake_pending {
                        t.wake_pending = false; // the wakeup already came: don't sleep
                        return None;
                    }
                    t.state = State::Blocked;
                    if let Some(d) = deadline {
                        t.wake_at = d.max(1);
                        arm = true;
                    }
                }
                Leave::Sleep(d) => {
                    t.state = State::Sleeping;
                    t.wake_at = d;
                    arm = true;
                }
                Leave::Exit(code) => {
                    t.state = State::Dead;
                    t.exit_code = code;
                    if t.detached && t.kstack.is_some() {
                        s.zombies.push(cur);
                    }
                }
            }
        }
        if arm {
            if !s.sleeping.contains(&cur) {
                s.sleeping.push(cur);
            }
            arm_deadline(s);
        }
        let runnable_again = s.threads[&cur].state == State::Ready;
        if runnable_again && cur != idle {
            s.requeue(cpu, cur);
        }
        let next = s.pick(cpu);
        if next == cur {
            s.threads.get_mut(&cur).unwrap().state = State::Running;
            return None;
        }
        let nt = s.threads.get_mut(&next).expect("picked a missing thread");
        nt.state = State::Running;
        nt.switched_in_at = now;
        let migrated = nt.ran && nt.cpu != cpu;
        nt.ran = true;
        nt.cpu = cpu;
        if migrated {
            s.stats[cpu].migrations_in += 1;
        }
        let nt = s.threads.get_mut(&next).unwrap();
        let next_ptr: *mut Thread = &mut **nt;
        s.cpus[cpu].current = next;
        percpu::get(cpu).current.store(next, Ordering::Relaxed);
        percpu::get(cpu).switches.fetch_add(1, Ordering::Relaxed);
        let pt = s.threads.get_mut(&cur).unwrap();
        pt.switching_out.store(true, Ordering::Relaxed);
        let prev_ptr: *mut Thread = &mut **pt;
        Some((prev_ptr, next_ptr))
    });
    if let Some((prev, next)) = sw {
        // SAFETY: `next` is current on this CPU now, so nobody frees it; `prev` has
        // switching_out set, so nobody frees or runs it until after_switch clears it.
        unsafe {
            // It may still be leaving another CPU: wait until its registers are saved.
            while (*next).switching_out.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
            PREV[cpu].store(prev, Ordering::Relaxed);
            switch(prev, next);
        }
        after_switch();
    }
}

/// On the CPU that just switched: the previous thread's registers are saved, so it may
/// now be run elsewhere or freed.
fn after_switch() {
    let p = PREV[percpu::id()].swap(core::ptr::null_mut(), Ordering::Relaxed);
    if !p.is_null() {
        // SAFETY: switching_out kept it alive until this store.
        unsafe { (*p).switching_out.store(false, Ordering::Release) };
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
    schedule(Leave::Yield);
}

/// Switch now if a wakeup asked for it and IRQs are on (i.e. we are allowed to block).
pub fn preempt_point() {
    if need_resched() && sync::irqs_enabled() {
        yield_now();
    }
}

/// True if something more deserving than the current thread is waiting (on this CPU).
pub fn need_resched() -> bool {
    percpu::this().need_resched.load(Ordering::Relaxed)
}

/// Block the current thread until someone calls [`wake`] on it (returns at once if a wake
/// already came since it last blocked). IRQs must be masked and the caller must already
/// have recorded the thread somewhere a waker will find it.
pub(crate) fn block_current() {
    debug_assert!(!sync::irqs_enabled());
    schedule(Leave::Block(None));
}

/// Like [`block_current`], but also wake up at `deadline_ns` (now_ns time) if nobody
/// calls [`wake`] first. The caller re-checks its condition either way.
pub(crate) fn block_current_until(deadline_ns: u64) {
    debug_assert!(!sync::irqs_enabled());
    schedule(Leave::Block(Some(deadline_ns)));
}

/// Wake `tid`: a blocked thread becomes ready; one that is still running (about to block)
/// gets a pending wake so its block returns at once. Returns false if it is neither
/// (sleeping in sleep_until, dead, gone). IRQ-safe.
pub fn wake(tid: Tid) -> bool {
    let _g = IrqGuard::new();
    with(|s| match s.threads.get_mut(&tid) {
        Some(t) if t.state == State::Blocked => {
            s.make_ready(tid);
            true
        }
        Some(t) if t.state == State::Running || t.state == State::Ready => {
            t.wake_pending = true;
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
    schedule(Leave::Sleep(deadline_ns));
}

pub fn sleep_ns(ns: u64) {
    sleep_until(timer::now_ns().saturating_add(ns));
}

pub fn sleep_ms(ms: u64) {
    sleep_ns(ms.saturating_mul(1_000_000));
}

/// Program this CPU's one-shot timer for the earliest sleeper. Any CPU can wake sleepers
/// (they are on one global list); the tick re-checks as a fallback.
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
    let cpu = percpu::id();
    // Every 50 ticks (0.5 s): this CPU's busy percentage, for the menu bar / /proc/cpuload.
    let pc = percpu::get(cpu);
    if pc.ticks.load(Ordering::Relaxed).is_multiple_of(50) {
        let (busy, idle) = cpu_times(cpu);
        let db = busy.saturating_sub(pc.sample_busy.swap(busy, Ordering::Relaxed));
        let di = idle.saturating_sub(pc.sample_idle.swap(idle, Ordering::Relaxed));
        if let Some(pct) = (db * 100).checked_div(db + di) {
            pc.load_pct.store(pct as u32, Ordering::Relaxed);
        }
    }
    with(|s| {
        s.wake_expired(timer::now_ns());
        if !s.cpus[cpu].joined {
            return;
        }
        s.balance(cpu);
        if s.should_preempt(cpu) {
            percpu::get(cpu).need_resched.store(true, Ordering::Relaxed);
        }
    });
}

fn on_irq_exit(_frame: &mut TrapFrame) {
    if need_resched() && STARTED.load(Ordering::Relaxed) {
        schedule(Leave::Yield);
    }
}

// ---- exit ----------------------------------------------------------------------------

/// End the current thread with `code`.
pub fn exit(code: i64) -> ! {
    sync::irq_disable();
    // Dead first (joiners check for it), then tell them, then leave for good.
    with(|s| s.cur().state = State::Dead);
    EXIT_WQ.wake_all();
    schedule(Leave::Exit(code));
    unreachable!("dead thread rescheduled");
}

// ---- queries -------------------------------------------------------------------------

pub fn current_tid() -> Tid {
    percpu::this().current.load(Ordering::Relaxed)
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

/// Save the live FP/SIMD registers (the current user thread's) into `fp`.
pub fn save_current_fp(fp: &mut FpState) {
    // SAFETY: FP is enabled (init); writes 528 bytes into `fp`.
    unsafe { fp_save(fp) };
}

/// Load `fp` into the live FP/SIMD registers (sigreturn).
pub fn load_current_fp(fp: &FpState) {
    // SAFETY: FP is enabled; reads 528 bytes from `fp`.
    unsafe { fp_restore(fp) };
}

/// Zero the live FP/SIMD registers (execve).
pub fn reset_current_fp() {
    let z = FpState::zeroed();
    // SAFETY: as above.
    unsafe { fp_restore(&z) };
}

/// CPU time of the current thread so far, in ns.
pub fn current_cpu_ns() -> u64 {
    let now = timer::now_ns();
    with(|s| {
        let t = s.cur();
        t.cpu_ns + now.saturating_sub(t.switched_in_at)
    })
}

/// Set the current thread's clear_child_tid (set_tid_address).
pub fn set_clear_child_tid(addr: usize) {
    with(|s| {
        if let Some(u) = s.cur().user.as_mut() {
            u.clear_child_tid = addr;
        }
    });
}

/// Current thread's clear_child_tid (0 = none), for exit.
pub fn current_clear_child_tid() -> usize {
    with(|s| s.cur().user.as_ref().map_or(0, |u| u.clear_child_tid))
}

/// The live TPIDR_EL0 (the current user thread's TLS pointer).
pub fn current_tpidr_el0() -> u64 {
    let v: u64;
    // SAFETY: register read.
    unsafe { asm!("mrs {}, tpidr_el0", out(reg) v, options(nomem, nostack)) };
    v
}

/// For the stack-overflow report: no locking, no allocation.
pub fn current_for_crash() -> (Tid, Comm) {
    let tid = current_tid();
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
    /// CPU it runs on (Running) or last ran on.
    pub cpu: usize,
}

pub fn threads() -> Vec<ThreadInfo> {
    let now = timer::now_ns();
    with(|s| {
        s.threads
            .values()
            .map(|t| ThreadInfo {
                tid: t.tid,
                pid: t.user.as_ref().map_or(0, |u| u.pid),
                name: t.name,
                state: t.state,
                prio: t.prio,
                cpu_ns: t.cpu_ns
                    + if t.state == State::Running {
                        now.saturating_sub(t.switched_in_at)
                    } else {
                        0
                    },
                cpu: t.cpu,
            })
            .collect()
    })
}

pub fn started() -> bool {
    STARTED.load(Ordering::Acquire)
}

/// (busy ns, idle ns) of `cpu`, including the slice it is in right now.
pub fn cpu_times(cpu: usize) -> (u64, u64) {
    let pc = percpu::get(cpu);
    let (mut busy, mut idle) = (
        pc.busy_ns.load(Ordering::Relaxed),
        pc.idle_ns.load(Ordering::Relaxed),
    );
    let now = timer::now_ns();
    if let Some(g) = SCHED.try_lock()
        && let Some(s) = g.as_ref()
        && let Some(t) = s.threads.get(&s.cpus[cpu].current)
    {
        let running = now.saturating_sub(t.switched_in_at);
        if s.cpus[cpu].current == s.cpus[cpu].idle {
            idle += running;
        } else {
            busy += running;
        }
    }
    (busy, idle)
}

// ---- run-queue policy ------------------------------------------------------------------

pub fn policy() -> Policy {
    with(|s| s.policy)
}

/// Switch between the shared queue and per-CPU queues; queued threads move over.
pub fn set_policy(p: Policy) {
    let _g = IrqGuard::new();
    with(|s| {
        if s.policy == p {
            return;
        }
        let mut moved: Vec<Tid> = Vec::new();
        for q in s.global.iter_mut().chain(s.rq.iter_mut().flatten()) {
            moved.extend(q.drain(..));
        }
        s.policy = p;
        for tid in moved {
            let (prio, cpu) = {
                let t = &s.threads[&tid];
                (t.prio as usize, t.pinned.unwrap_or(t.cpu))
            };
            match p {
                Policy::Global => s.global[prio].push_back(tid),
                Policy::PerCpu => s.rq[cpu][prio].push_back(tid),
            }
        }
        let joined: Vec<usize> = s.joined_cpus().collect();
        for c in joined {
            kick(c);
        }
    });
}

/// Per-CPU counters (steals, migrations, queue length).
pub fn cpu_stats(cpu: usize) -> CpuStats {
    with(|s| {
        let mut st = s.stats[cpu];
        st.queued = s.queued(cpu);
        st
    })
}
