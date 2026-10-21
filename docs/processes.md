# Threads, processes and syscalls

Files: `kernel/src/sched/` (threads, scheduler, wait queues), `kernel/src/proc/` (address
spaces, processes, ELF, syscalls, console tty), `user/` (runtime + programs). Small hooks
outside: `exceptions.{S,rs}` (stack-overflow check, IRQ-exit hook), `uart.rs` (RX hook),
`mmu.rs` (user spaces, kernel-stack window), `fs/devfs.rs` (console reads go to the tty),
`linker.ld` (boot stack alignment), `kernel/build.rs` (builds `user/`).

Boot: `... fs::init → fs selftest → sched::init → sched selftest → proc::init (EL0 smoke
test) → proc::start_init()`. kmain then is thread 0 and keeps polling input.

## Kernel threads (`sched`)

```rust
let h = sched::spawn("name", move || { ... });   // Priority::Normal
let h = sched::spawn_prio("name", Priority::High, f);
let code = h.join();          // or drop(h) to detach; exit code from sched::exit(code)
sched::yield_now();  sched::sleep_ms(10);  sched::sleep_until(ns);  sched::exit(0);
sched::current_tid();  sched::threads();   // ps-style info
```

* Each thread: 64 KiB kernel stack, saved callee-saved regs + lr + sp, a `Comm` name
  (16 bytes, like Linux). Thread 0 ("kmain") runs on the boot stack; "idle" runs `wfi`.
* Strict priorities `Idle < Low < Normal < High`, round-robin within a level.
* Preemption: the 100 Hz tick sets "need resched" if a ready thread has the same or
  higher priority; `exceptions`' IRQ-exit hook (after EOI) switches. Syscalls also
  reschedule on the way back to EL0.
* Sleeping uses the timer's one-shot deadline, which **`sched` owns** from `sched::init`
  on (don't call `timer::set_deadline` elsewhere; the tick re-checks sleepers anyway).

### Blocking primitives

```rust
static WQ: WaitQueue = WaitQueue::new();
WQ.wait_until(|| condition());                 // thread context
let ok = WQ.wait_until_timeout(|| cond(), ns); // false on timeout
WQ.wake_one(); WQ.wake_all();                  // IRQ-safe
let m = sched::Mutex::new(T);  m.lock()        // sleeping mutex
let s = sched::Semaphore::new(n); s.acquire(); s.release()  // release is IRQ-safe
```

`cond` runs with IRQs masked (that's what makes check-then-sleep race free on one core):
it may take spinlocks, it must not block. Before `sched::init` the waits degrade to
`wfi` loops, so code shared with early boot still works.

### Rules

* `SpinLock` masks IRQs, so a spinlock holder is never preempted. **Never block** (sleep,
  `wait_until`, `Mutex::lock`, console read) while holding a `SpinLock` or inside an IRQ
  handler. Use `sched::Mutex` for anything held across slow work.
* `kprintln!` masks IRQs for the whole line (no interleaving mid-line).
* IRQ handlers may call `wake*`/`Semaphore::release`; the switch happens on IRQ exit.

### Kernel stacks and the guard page

Stacks live in a kernel-virtual window (`mmu::VMAP_BASE` = `0x7f_0000_0000`), one per
128 KiB slot: the upper 64 KiB is the stack, the lower 64 KiB is never mapped. Running
off the bottom faults, and the EL1 vectors (`VECTOR_KSTACK` in `exceptions.S`) notice
that the frame they are about to push would land in the guard (bit 16 of sp is 0) and
switch to an emergency stack to print `*** KERNEL STACK OVERFLOW ***` with the thread
name, instead of faulting forever. `--features kstack-overflow-test` demonstrates it.
Stack frames are physically contiguous and `mmu::virt_to_phys` knows the window, so DMA
from a stack buffer still works (as long as it doesn't cross the stack's top).

## Address spaces: the decision

**The kernel stays identity mapped in the low part of TTBR0, in every address space;
user space is `0x100_0000_0000` (1 TiB) .. `0x1_0000_0000_0000` (256 TiB).**

```text
TTBR0 (48-bit), shared by kernel and the current process:
  L0 slot 0   0 .. 512 GiB      kernel: devices 0..1 GiB, RAM, kernel-stack window   EL1 only, global
  L0 slot 1   512 GiB .. 1 TiB  kernel: reserved (high PCIe MMIO)                     EL1 only, global
  L0 slot 2+  1 TiB ..          user:   image at 1 TiB + 4 MiB, brk after it,         EL0, nG (ASID)
                                        mmap from 2 TiB up, stack below 256 TiB
TTBR1: unused (EPD1)
```

* A user root table copies L0 slots 0 and 1 from the kernel's root (pointers to the same
  L1 tables), so every current and future kernel mapping is visible in every process.
  `mmu::prepare_kernel_slots()` makes sure both slots have an L1 table before the first
  process. User mappings are `nG` + EL0, tagged with the process's ASID.
* **Why not the higher half now:** with VA == PA kept, every driver written in parallel
  (virtio-input, virtio-blk, ramfb/fw_cfg DMA descriptors, MMIO bases like
  `0x0a00_0000`) keeps working with no change. Moving the kernel to TTBR1 changes all of
  them at once and is better done as its own step after the merges.
* **Cost:** static Linux binaries linked at the usual `0x400000` can't load (the loader
  says so). Static-pie binaries can (loaded at 1 TiB + 4 MiB). Our own programs link at
  `0x10000400000` (`user/rt/user.ld`), which stays valid after the higher-half move.
* **Higher-half later** (docs/memory.md has the recipe): set `PHYS_OFFSET`, build the
  kernel map for TTBR1, clear `TCR.EPD1`, make every MMIO base go through
  `phys_to_virt`/`map_device`, then drop `KERNEL_L0_SLOTS` to 0 so user space starts at
  4 KiB. Nothing in `proc/` depends on where the kernel is, except `USER_BASE`.

### ASIDs and switching

8-bit ASIDs (`proc/asid.rs`), 255 live user spaces, ASID 0 = kernel-only threads (whose
TTBR0 is the kernel root). TTBR0 = root | ASID << 48 is written on every switch between
threads with different spaces; no TLB flush, because an ASID's entries are flushed
(`tlbi aside1is`) when it is freed. Kernel entries are global and identical everywhere.

### User memory from the kernel

The kernel never dereferences user pointers. `UserSpace::copy_in/copy_out/read_cstr`
walk the process's page map and copy through the identity map of the frame, after
checking the whole range (and write permission) first. A bad pointer is `EFAULT`, never
a kernel abort, and PAN doesn't matter.

## Processes (`proc`)

* One thread per process (no CLONE_THREAD yet). `Process { pid, inner: SpinLock<Inner>,
  child_wq }`; `Inner` has the `UserSpace`, the fd table, cwd (an `InodeRef`), children,
  state (`Alive` / `Zombie(wait status)`), pgid / sid, signal state, argv, exe path.
* **fork** (`clone` with flags = SIGCHLD): **copy-on-write**. Both spaces share every
  frame (a global share count per frame); writable pages turn read-only in both, and the
  first write faults and copies (or just re-enables write for the last owner). fds are
  shared `Arc<OpenFile>`s (shared offsets); FP regs, TPIDR_EL0, pgid/sid, signal actions
  and mask are inherited; pending signals are not. The kernel's `copy_out` breaks COW
  first, because it writes through the identity map, which ignores the read-only bit.
* **execve**: reads the file through the VFS (relative to cwd), loads it into a new
  space, only then swaps it in (a failed exec leaves the caller intact); drops
  close-on-exec fds; handlers go back to SIG_DFL (SIG_IGN stays).
* **exit**: switch TTBR0 away, free the address space and fds immediately, become a
  zombie, hand children to init (pid 1), SIGCHLD + wake the parent. init exiting panics.
* **wait4**: a pid, any child (-1), own group (0) or group -N; `WNOHANG`; Linux wait
  status encoding (signal | 0x80 for "core dumped"); EINTR on a signal.
* **Faults** from EL0 go to the process's handler if it has one (SIGSEGV / SIGBUS /
  SIGILL / SIGTRAP / SIGFPE), otherwise kill only the process with a message, e.g.
  `[proc] pid 8 (fault) killed by SIGSEGV: permission fault, level 1 reading 0x0 (not a
  user address) at pc 0x100004000e8`. A translation fault just below the stack grows it
  (up to 8 MiB); a write fault on a COW page copies it.
* PID 1 is reserved for `/bin/init`; the EL0 boot test's processes get 2.. .

### Pipes (`proc/pipe.rs`)

`pipe2` gives two fds whose `OpenFile`s wrap a read-end and a write-end inode (Fifo) of a
64 KiB ring, so dup/fork/close need nothing special: an end closes when its last
`OpenFile` drops. Reads block while empty (EOF once no writer is left); writes block
while full, are atomic up to 4 KiB (PIPE_BUF), and give SIGPIPE + EPIPE with no reader.
Waits are interruptible (EINTR). O_NONBLOCK, O_CLOEXEC.

### Signals (`proc/signal.rs`)

* Per process: pending and blocked masks, 64 sigactions, the latest siginfo per signal
  (classic signals don't queue). `kill(pid | 0 | -1 | -pgid, sig)`, `tkill`, `tgkill`.
* Delivery on every return to EL0 (`exceptions::set_user_return_hook`, which also does
  the deferred reschedule): default action terminate / terminate+core / ignore, or a
  handler. The handler frame is the Linux aarch64 `rt_sigframe` (siginfo, ucontext with
  sigcontext, an `fpsimd_context` record, a frame record); the handler gets x0 = signo,
  x1 = &siginfo, x2 = &ucontext, lr = `sa_restorer` (required: there is no vDSO
  trampoline yet; rt and libc set it). `rt_sigreturn` restores registers, FP/SIMD and the
  mask, and only NZCV from the saved pstate.
* SA_RESETHAND, SA_NODEFER, sa_mask; SIGKILL/SIGSTOP can't be caught, blocked or ignored.
  Stopping (SIGSTOP/SIGTSTP/SIGTTIN/SIGTTOU) is not implemented: treated as ignored.
  No SA_RESTART: interrupted calls return EINTR. No sigaltstack.
* Blocking syscalls that return EINTR: read/write on pipes and ttys, wait4, nanosleep,
  clock_nanosleep. `signal::interrupted()` is the check to use in any new wait.
* Process groups and sessions: `setpgid`, `getpgid`, `setsid`, `getsid`; the terminal's
  foreground group is set with TIOCSPGRP, and ^C / ^\ on a tty signal that group.

### /proc (`proc/procfs.rs`)

`/proc/<pid>/{status, stat, cmdline, maps, fd/N, cwd, exe}`, `/proc/self` and
`/proc/threads` (every thread, kernel ones too), hung on the vfs's procfs root. Each
pid directory appears when the process starts and goes when it is reaped. `ps` reads
these (there are no milkOS-specific syscalls any more).

### Syscalls

Linux aarch64: `svc #0`, number in x8, args x0..x5, result in x0, `-errno` on error.
Implemented (number: name):

| | |
|---|---|
| files | 56 openat, 57 close, 59 pipe2, 63 read, 64 write, 65 readv, 66 writev, 67 pread64, 68 pwrite64, 62 lseek, 80 fstat, 79 newfstatat, 78 readlinkat, 61 getdents64, 17 getcwd, 49 chdir, 50 fchdir, 34 mkdirat, 35 unlinkat, 36 symlinkat, 37 linkat, 276 renameat2, 46 ftruncate, 53 fchmodat, 88 utimensat, 23 dup, 24 dup3, 25 fcntl, 29 ioctl (ttys: TCGETS, TCSETS*, TIOCGWINSZ, TIOCSWINSZ, TIOCGPGRP, TIOCSPGRP) |
| processes | 220 clone (fork only), 221 execve, 260 wait4, 93 exit, 94 exit_group, 172 getpid, 173 getppid, 178 gettid, 174-177 get[e]uid/get[e]gid (0), 96 set_tid_address, 154 setpgid, 155 getpgid, 156 getsid, 157 setsid |
| signals | 129 kill, 130 tkill, 131 tgkill, 134 rt_sigaction, 135 rt_sigprocmask, 136 rt_sigpending, 139 rt_sigreturn |
| time | 101 nanosleep, 115 clock_nanosleep, 113 clock_gettime, 114 clock_getres, 169 gettimeofday, 124 sched_yield |
| memory | 214 brk, 222 mmap (anonymous private only), 215 munmap, 226 mprotect |
| misc | 160 uname (sysname "milkOS"), 278 getrandom |

Anything else returns ENOSYS and logs `syscall N not implemented` once per number, which
is the to-do list for running a static musl binary. `--features strace` logs every call.

### ELF loading and the initial stack

`proc/elf.rs`: ELF64 aarch64, `ET_EXEC` in the user range, or `ET_DYN` without
`PT_INTERP` (static-pie, loaded at 1 TiB + 4 MiB). Each `PT_LOAD` gets exact R/W/X page
permissions; overlapping pages take the union. The I-cache is synced after loading.
The stack is the Linux layout: `argc, argv[], NULL, envp[], NULL, auxv` (AT_PHDR,
AT_PHENT, AT_PHNUM, AT_PAGESZ, AT_ENTRY, AT_HWCAP = FP|ASIMD, AT_CLKTCK, AT_RANDOM,
AT_EXECFN, ...), strings and 16 random bytes above it, sp 16-byte aligned.
FP/SIMD is enabled at EL0 and saved per user thread on every switch (the kernel itself is
soft-float and never touches it).

### Terminals (`proc/tty.rs`): the API for anyone adding one

One line discipline, instantiated per terminal. Today: `proc::tty::CONSOLE` (UART,
`/dev/console`, rdev 5:1) and `wm::tty::TTY1` (desktop Terminal, `/dev/tty1`, rdev 4:1).

```rust
pub static TERM: Tty = Tty::new("tty1", output_sink, rows, cols); // fn(&[u8]) sink
proc::tty::register(makedev(4, 1), &TERM);   // ioctls on that device node route here
TERM.input(bytes);                 // typed bytes; thread context (allocates, signals)
TERM.read(buf) / TERM.write(buf)   // call from the device inode's read_at / write_at
TERM.set_winsize(rows, cols);      // window resized: TIOCGWINSZ + SIGWINCH on change
TERM.foreground() / set_foreground(pgid)   // what TIOCGPGRP / TIOCSPGRP do
proc::spawn_on("/bin/msh", argv, envp, "/dev/tty1", cwd)   // a process on that tty
```

* Cooked mode by default: echo, backspace, ^U, Enter delivers a line, ^D at line start is
  EOF, one line per read. ^C / ^\ flush input and send SIGINT / SIGQUIT to the
  foreground process group (ISIG). Escape sequences are swallowed in cooked mode.
* termios: TCGETS/TCSETS/TCSETSW/TCSETSF round-trip a Linux `struct termios` (60 bytes);
  ICANON (off = raw bytes, read returns what's there), ECHO, ISIG, ICRNL are honoured.
* ioctls are handled in the syscall layer via the registry (it needs user-memory copies):
  TCGETS, TCSETS*, TIOCGWINSZ, TIOCSWINSZ, TIOCGPGRP, TIOCSPGRP. The device inode's own
  `ioctl` is not used for a registered tty.
* `read` blocks (interruptible: EINTR), except before `sched::init` where it is EAGAIN
  when empty (the fs self-test relies on that).
* The UART is fed by the "tty-uart" kernel thread (sleeps on the RX interrupt), so ^C
  works while nobody reads. `uart::read_byte` belongs to it.
* Not yet: sessions / controlling-tty rules (any process may TIOCSPGRP), SIGTTIN/SIGTTOU,
  job stop/continue.

## Userland (`user/`)

A separate cargo workspace (soft-float target, `user/rt/user.ld`). `kernel/build.rs` runs
`cargo build --release` in `user/` into `target/user/` **before** it packs the
initramfs, and fails the build if a program isn't listed in `initramfs.list`. Programs
are `user/<crate>/src/main.rs` (binary `<crate>`) and `user/<crate>/src/bin/<x>.rs`
(binary `<x>`). So `cargo build` at the root builds everything and the programs land in
`/bin`. Adding one: the source file (+ workspace member for a new crate) + one line in
`initramfs.list`.

* `rt`: `_start`, syscall wrappers (`rt::sys`: files, processes, signals with an
  `sa_restorer` trampoline, job control, /proc-based `ps`), `print!`/`println!`,
  `rt::io` (buffered `Out`, `Lines`, `each_input`, flag parsing), `rt::table`
  (structured pipes), brk bump allocator, panic handler, `rt::main!(main)`.
* `init`: banner, runs `hello`, keeps `msh` running on the console, reaps orphans.
* `msh`: the shell, see docs/shell.md (pipelines, redirection, && || ; &, variables,
  globs, job control with Ctrl-C, structured pipes).
* `coreutils`: ls cat echo wc head tail grep sort uniq ps kill sleep mkdir rm mv cp ln
  touch env uptime free uname true false clear, one binary each.
* `hello`, `fault` (deliberate crashes), `utest` (kernel feature tests from user space:
  `utest pipe | cow | proc | signal | all`).

## Moving the window server into a kernel thread

Today `kmain` ends in `loop { input_demo(); sched::sleep_ms(10); }`. To give the wm its
own thread:

```rust
// in kmain, after proc::start_init():
sched::spawn_prio("wm", sched::Priority::High, || wm::run());   // or Normal
loop { sched::sleep_ms(1000); }        // kmain idles (or just let it be the wm)
```

and inside `wm::run()`: `loop { drain input; composite dirty rects; sched::sleep_until(next_frame_ns) }`,
where `next_frame_ns += 16_666_667` (60 Hz). Things to respect:

* **Never spin or `wfi` in the loop**; sleep until the next frame (or wait on an input
  wait queue, see below). A busy loop at High priority starves everything else.
* High priority makes the wm preempt user programs as soon as its frame time comes
  (frame pacing); Normal shares the CPU round-robin with them. Start with High + sleep.
* Don't hold a `SpinLock` around compositing (IRQs off for milliseconds, no
  preemption). Use `sched::Mutex` for window lists / surfaces shared with other threads.
* Input: today `input::poll()` is polled. Better: the virtio-input IRQ handler pushes
  events and calls `INPUT_WQ.wake_all()`; the wm does
  `INPUT_WQ.wait_until_timeout(|| input::pending(), ns_until_next_frame)`.
* A Terminal window for msh = a second tty: same line discipline as `proc/console.rs`
  fed by keyboard events, output drawn into the window, exposed as a device node (e.g.
  `/dev/tty1`) that init opens for that msh's fds 0/1/2.

## Making drivers sleep instead of poll

**virtio-blk** (`virtio/blk.rs`, polled under a `SpinLock` with IRQs masked, 5 s timeout):

1. `inner: SpinLock<Inner>` → `sched::Mutex<Inner>` (requests are thread context).
2. Add a `WaitQueue` (static or per device). After notifying the queue:
   `let done = WQ.wait_until_timeout(|| vq.used_idx() != last_seen, TIMEOUT_NS);`
   (the condition only reads the used ring: fine with IRQs masked).
3. The IRQ handler acks the interrupt status as now, then `WQ.wake_all()`.
4. Before `sched::init` (the fs self-test touches the disk) `wait_until_timeout` falls back
   to a `wfi` loop, so the same code works there; the IRQ just has to be enabled.

**Console reads**: done (above).

## Not done yet / next

* File-backed and shared mmap (tmpfs frames are page aligned already), demand paging for
  brk/mmap, `PROT_NONE` (accepted, ignored).
* Threads in a process (`CLONE_VM|CLONE_THREAD`, futex), job stop/continue (SIGTSTP,
  `bg`), SA_RESTART, sigaltstack, a vDSO with a sigreturn trampoline.
* Controlling-terminal rules (any process may TIOCSPGRP today), SIGTTIN/SIGTTOU.
* 16-bit ASIDs (Apple silicon has them), SMP (scheduler lock across check+block,
  per-CPU current thread via TPIDR_EL1, an atomic COW share-and-copy).
* The higher-half kernel move (above), then static Linux binaries at 0x400000.
