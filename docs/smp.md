# SMP

milkOS runs on every CPU the device tree lists (`-smp 4` with `-accel hvf -cpu host`;
`-smp 1` still works). Files: `kernel/src/{smp.rs,smp.S,percpu.rs}`, the scheduler in
`kernel/src/sched/`, small per-CPU parts in `timer.rs`, `gic.rs`, `sync.rs`, `sink.rs`.

## Bring-up (`smp::init`, right after `sched::init`)

1. The DTB gives the CPUs (`/cpus/cpu@N/reg` = MPIDR) and the PSCI conduit
   (`/psci/method`: `hvc` on qemu virt with `-kernel`, under TCG and HVF alike).
2. For each secondary: a 64 KiB kernel stack with guard (from the kstack window) and a
   `BootBlock` holding the boot CPU's MAIR, TCR, SCTLR, the boot identity tables (TTBR0),
   the kernel's tables (TTBR1), the empty TTBR0 to switch to, and the CPU index. The block is cleaned to the point of coherency (`dc civac`) because the
   secondary reads it with its MMU and caches still off. Then `PSCI CPU_ON(mpidr,
   secondary_entry, &block)` and a wait (1 s max) for it to report online.
3. `secondary_entry` (smp.S) turns on the MMU and caches with those values before any
   Rust runs, branches to the higher half, drops the identity map, sets sp and TPIDR_EL1
   (= CPU index), and calls `secondary_main`: VBAR, FP,
   `gic::init_cpu` (CPU interface + banked SGI/PPI enables for every registered SGI/PPI),
   `timer::init_cpu` (its own virtual-timer comparator), then `sched::run_secondary`: the
   code running on that stack becomes the CPU's idle thread.
4. **IRQs stay masked until the CPU has joined the scheduler** (`run_secondary` unmasks
   them), and `smp::init` waits for each CPU to *join*, not just come online. Before it
   joins, a CPU's scheduler slot reads current = idle = tid 0 (kmain): an IPI or tick
   taken there used to "switch away from kmain" on the wrong CPU and corrupt kmain's
   context (crash at PC 0 / hang during `[sched-test]`). The scheduler also ignores
   CPUs that haven't joined (no placement, stealing or kicks; `schedule` and the tick do
   nothing there). Regression: `MILK_FEATURES=smp-join-delay scripts/serial-soak.sh`.

Device interrupts (SPIs) are spread over the online CPUs once they are up
(`gic::spread_spis`, round robin through GICD_ITARGETSR; SPIs registered later get the
next CPU). `/proc/irq_affinity` lists INTID -> CPU; write `cpu0` to route them all back to
CPU 0, `spread` to spread again. Every CPU takes its own timer PPI and the SGIs. Handlers
must not assume they run on CPU 0 (none does: they all lock or use atomics).

## Per-CPU data (`percpu.rs`)

`TPIDR_EL1` holds the CPU index (boot.S sets 0, smp.S the others); `percpu::id()`,
`percpu::this()`, `percpu::get(cpu)`. Each `PerCpu` has: online, MPIDR, MIDR, current
and idle tid, need_resched, busy/idle ns, ticks, IPIs, context switches, and `load_pct`.

## Locks

`sync::SpinLock` is a real compare-exchange spinlock now (the `atomic-locks` feature is
gone). It still masks IRQs on the local CPU, and the lock word is the owner's CPU + 1,
so taking a lock this CPU already holds panics ("already held by this CPU") instead of
spinning forever. The MMU is on before the first lock everywhere (boot CPU: `mem::init`;
secondaries: smp.S). `kprintln!` takes a line lock (`sink::with_print_lock`) so lines
from different CPUs don't interleave; a nested print on the same CPU, or a holder that
died mid-line (~20 ms), doesn't hang it. Console writes from processes use it too.

Audited for SMP: gic/uart/input counters are atomic adds now; the uart RX ring has one
producer at a time (`uart::drain` under a lock: the UART IRQ on whichever CPU it is
routed to, or the 1 s fallback poll) and one consumer (tty-uart thread). The RX handler
clears ICR *before* draining the FIFO: draining first lost the RX edge when a byte
landed in between and wedged the console (see uart.rs, `scripts/serial-soak.sh`); the screen-console
flag uses compare-exchange once the MMU is on; klog, input queue, tty, pipes, net are
all behind SpinLocks or proper atomics; net's "set flag, wake queue" pattern is safe with
the new wait-queue protocol below.

## Scheduler

* **Per-CPU ready queues** (one per priority per CPU), still under the one `SCHED` lock
  (at 4 CPUs the lock isn't the cost; cross-CPU traffic is). A thread runs where it ran
  last. `Policy::Global` (the old single shared queue) can be switched to at run time
  for comparison: `print global > /proc/sched` (`percpu` to go back). `/proc/sched` shows
  the policy and per CPU: queued, steals, migrations in, switches.
* **Placement** (`make_ready`, per-CPU policy): "free" = idle, nothing queued, no kick
  pending. (1) its last CPU if free; (2) woken from its own last CPU (pipe/futex
  partner, likely about to block): queue it right there, cache hot, and kick a free CPU
  as backup that steals it if the waker keeps running (skipped if that would leave it
  waiting behind higher priority while another CPU runs something less important);
  (3) a free CPU, this one first (new threads always spread this way); (4) a CPU
  running lower priority; (5) the shortest queue.
* **Work stealing**: a CPU with nothing of its own takes the longest-waiting thread from
  the busiest queue; a busy CPU only takes a strictly higher-priority one (so equal
  threads don't bounce). The tick also pulls one thread from any queue two or more
  longer than its own (stealing alone never fixes a 3-vs-1 imbalance).
* **Numbers** (release kernel, -smp 4, HVF on an M-series Mac, `utest csw` / `utest par`,
  three alternating runs each): pipe ping-pong round trip 8.1-8.5 us global vs 2.6-3.2 us
  per-CPU (the partner runs on the waker's CPU instead of an IPI'd one); sched_yield
  storm (8 threads) ~0.43 us/yield both, but ~43 000 migrations per run global vs ~6
  per-CPU; `par 4` / `par 8` the same within noise (3.7x / 3.7x). `-smp 1`: the
  policies behave the same.
* **Switch handshake**: when a CPU decides to leave thread T it sets T.switching_out
  (under the lock); after `switch_context` has saved T's registers, the CPU clears it
  (`after_switch`, also run by new threads' first code). Another CPU that picked T waits
  for the flag before loading T's context, and nobody frees a dead thread while it is
  set or while it is current anywhere.
* **Blocking is part of `schedule`** (`Leave::Block / Sleep / Exit / Yield`), done under
  the same lock hold that picks the next thread, so a waker on another CPU can never see
  "blocked" before the CPU has committed to switching away.
* **No lost wakeups**: a `WaitQueue` waiter enqueues itself, then checks its condition,
  then blocks; `wake()` on a thread that is still running sets `wake_pending`, and its
  next block returns immediately. Wakeups can be spurious; conditions decide. However a
  wait ends (condition true, timeout, a wakeup from elsewhere or a leftover pending
  wake), the waiter takes itself off the queue: a stale entry used to let `wake_one`
  "wake" a thread that wasn't waiting and swallow the wakeup of the real waiter behind
  it (boots hung in the mutex self-test, all CPUs idle). `[sched-test] stale waiter`
  reproduces that exact sequence.
* **Wakeups find a CPU** (see Placement): the chosen CPU is kicked with `need_resched`
  + SGI 0 (`IPI_RESCHED`); the reschedule happens on the IRQ-exit path. Each CPU's tick
  handles its own time slice (round robin within its own queue).
* **Idle per CPU**: `idle` (CPU 0), `idle/N`; they never sit in a ready queue.
* **Affinity**: `sched::spawn_on(name, prio, Some(cpu), f)` pins a kernel thread.
* Sleepers are on one global list; every CPU's tick and one-shot deadline can wake them.

## TLB and ASIDs

All TLB maintenance is the **inner-shareable broadcast** form, so no IPI shootdown is
needed: `tlbi vaae1is` for a page (unmap, munmap, mprotect, COW break, kernel stack
free), `tlbi aside1is` when an address space dies or its ASID is freed (also drops walk
caches), `ic ialluis` after writing code. Every CPU qemu virt / HVF gives us is in one
inner-shareable domain, so these reach all of them; the `dsb ish` after each waits
for completion everywhere. A process has one thread, so its page tables are only ever
changed by the CPU running it; other CPUs can only hold stale TLB entries from earlier
runs, which the broadcasts remove. New mappings need no TLBI (invalid -> valid), just
`dsb ishst` so other CPUs' walkers see the table write. ASIDs come from one global
allocator under a lock; TTBR0 (root | ASID) is loaded on every switch on whatever CPU.
If milkOS ever gets multi-threaded processes or a second shareability domain, page
table frees will need an IPI-based shootdown (or RCU-style deferral) on top.

COW across CPUs: two sharers can break COW on the same frame at once; each copies, then
drops its share, and whoever turns out to be the last owner frees the original.

## /proc and CPU load

* `/proc/cpuinfo`: Linux arm64 blocks (processor, BogoMIPS, Features from
  ID_AA64ISAR0, MIDR fields read on each CPU).
* `/proc/stat`: `cpu` and `cpuN` lines in USER_HZ (100) ticks (non-idle time is all in
  the user column: milkOS doesn't split user/system yet), plus `intr`, `ctxt`, `btime`,
  `processes`, `procs_running`.
* `/proc/interrupts`: one column per CPU; SGI 0 is "IPI rescheduling".
* `/proc/threads`: has a PSR column (CPU); `/proc/<pid>/status` has `Cpu:`; `processes list` shows it (CORE).
* **CPU % per core (for the wm's menu bar)**: `crate::percpu::load(cpu) -> u32` (busy
  percent over the last ~0.5 s, sampled on each CPU's tick), `crate::percpu::online()`
  and `count()`. For user space: `/proc/cpuload` ("cpuN PERCENT" lines). Cumulative:
  `sched::cpu_times(cpu) -> (busy_ns, idle_ns)`.

## Testing

* Boot self-tests run on all CPUs (`sched::selftest` after `smp::init`): parallel
  interleave, sleep order, mutex (4 x 2000 increments under contention), semaphore,
  preemption, 220-thread churn, timed waits.
* `utest` (user space) adds SMP stress: `utest tlb` (4 processes x 300 mmap/touch/munmap
  while migrating between CPUs; a just-unmapped page must fault) and `utest cowsmp` (a
  parent and 4 children writing their COW copies of one 256 KiB buffer at once).
* Benchmarks (not pass/fail): `utest par N [units]` (N CPU-bound processes vs 1; prints
  speedup, steals, migrations) and `utest csw [rounds]` (pipe ping-pong between
  processes and between threads, sched_yield storm). Compare policies with
  `print global > /proc/sched` / `print percpu > /proc/sched`.
* `utest par N MS`: N processes doing MS units of pure CPU work vs one; prints the
  speedup. Measured (debug kernel, HVF on the host's cores):

  | | 1 job | 4 jobs | speedup |
  |---|---|---|---|
  | `-smp 4` | 173 ms | 188 ms | 3.68x (3.85x with the desktop running) |
  | `-smp 4`, 8 jobs x 100 | 87 ms | 188 ms | 3.70x |
  | `-smp 1` | 187 ms | 742 ms | 1.01x |

* Stress loop used for this work: 12 boots of `-smp 4` (+ 3 of `-smp 1`), each running
  the boot self-tests and `utest`: all pass.
* `scripts/run.sh`, `screenshot.sh`, `wm-demo.sh` boot 4 CPUs by default; `MILK_SMP=1`
  (or any N) overrides.
