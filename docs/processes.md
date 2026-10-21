# Threads, processes and syscalls

Files: `kernel/src/sched/` (threads, scheduler, wait queues), `kernel/src/proc/` (address
spaces, processes, ELF, syscalls, console tty), `user/` (runtime + programs). Small hooks
outside: `exceptions.{S,rs}` (stack-overflow check, IRQ-exit hook), `uart.rs` (RX hook),
`mmu.rs` (user spaces, kernel-stack window), `fs/devfs.rs` (console reads go to the tty),
`linker.ld` (boot stack alignment), `kernel/build.rs` (builds `user/`).

Boot: `... fs::init → fs selftest → sched::init → sched selftest → proc::init (EL0 smoke
test) → proc::start_init()`. kmain then is thread 0 and keeps polling input.

SMP (bring-up, per-CPU data, locking, the scheduler's switch handshake, TLB policy,
CPU load): `docs/smp.md`.

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

**The kernel is in the higher half (TTBR1, `0xffff_...`); TTBR0 is purely user space:
`mmu::USER_BASE` (`0x1000`) .. `mmu::USER_END` (`0x1_0000_0000_0000`, 256 TiB).**

```text
TTBR0 (48-bit), the current process only (an empty table for kernel threads):
  0 .. 0x1000                   unmapped (null pointers fault)
  0x40_0000                     where static Linux binaries link (ET_EXEC loads as linked)
  0x100_0040_0000               IMAGE_BASE: milkOS programs (user/rt/user.ld), static-pie
                                brk right after the image
  0x200_0000_0000 (2 TiB) ..    MMAP_BASE: anonymous mmap, bump allocated upwards
  .. 0xffff_ffff_0000           stack (STACK_TOP), grows down on fault
TTBR1: the kernel (docs/memory.md): linear map of MMIO + RAM, kernel stacks. Global.
```

* A user root table starts empty and holds only user mappings (`nG` + EL0, tagged with
  the process's ASID). Nothing is copied from the kernel.
* `IMAGE_BASE`/`MMAP_BASE` are absolute constants in `proc/uspace.rs`, and `USER_BASE`
  is one constant in `mmu.rs`: move them there, nothing else in `proc/` cares.
* A user pointer into the kernel half fails `user_range_ok` (`EFAULT` from a syscall) and
  faults at EL0 (`SIGSEGV`); `utest kaddr` checks both.

### ASIDs and switching

8-bit ASIDs (`proc/asid.rs`), 255 live user spaces, ASID 0 = kernel-only threads (whose
TTBR0 is an empty table). TTBR0 = root | ASID << 48 is written on every switch between
threads with different spaces; no TLB flush, because an ASID's entries are flushed
(`tlbi aside1is`) when it is freed. Kernel entries are global and identical everywhere.

### User memory from the kernel

The kernel never dereferences user pointers. `UserSpace::copy_in/copy_out/read_cstr`
walk the process's page map and copy through the kernel's linear map of the frame, after
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
  first, because it writes through the linear map, which ignores the read-only bit.
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

### AF_UNIX sockets (`proc/unix.rs`, `proc/unixsys.rs`)

Local IPC, Linux API: `socket(AF_UNIX, SOCK_STREAM | SOCK_DGRAM)`, `socketpair` (199),
bind / listen / accept4 / connect / sendto / recvfrom / shutdown / getsockname /
getpeername, and `sendmsg` (211) / `recvmsg` (212) with control messages. User side:
`rt::unix`.

* **Names.** A path: `bind` makes a socket node (`S_IFSOCK`, `ls -l` shows `s`, mode
  0755 like Linux with umask 022) on tmpfs (`/tmp`, `/run`); connecting needs write
  permission on it (fs/perm.rs) and, under the capability sandbox, an fs grant for it.
  Unlinking the node only hides the socket. An abstract name (`sun_path[0] == 0`) lives
  in a kernel table until the socket closes; the sandbox treats it like the network
  (denied under `net=none`). Binding with just the family autobinds an abstract
  5-hex-digit name.
* **Stream:** connect is immediate (the server's end waits in the listener's backlog;
  a full backlog blocks the connecter, or EAGAIN). 256 KiB receive buffer per socket;
  writers block on a full peer. Closing gives the peer EOF, writing to a closed peer
  SIGPIPE + EPIPE. **Dgram:** messages, `sendto` a name or `connect` for a default.
* **SCM_RIGHTS:** files sent with `sendmsg` arrive as new fds (same open file
  description: shared offset, flags), close-on-exec with MSG_CMSG_CLOEXEC; too little
  control room gives MSG_CTRUNC and closes the extra files. On a stream they travel with
  the first byte of their write and a read stops before the next write that carries
  files. Up to 253 per message; each send is audited (`unix.scm_rights`). Files in
  flight on a socket that closes are closed; a socket sent through itself leaks (no
  garbage collector).
* **Credentials:** `SO_PEERCRED` = the peer's pid / euid / egid at connect (client side:
  the listener's, as of `listen`) or socketpair. With `SO_PASSCRED`, every message comes
  with `SCM_CREDENTIALS` (pid, real uid, real gid of the sender); a sender may claim them
  explicitly, but only its own pid and its own real/effective/saved ids (root: anything);
  anything else is EPERM.
* `read` / `write` work on connected sockets; poll / select / epoll see readiness.
* Not done: `SOCK_SEQPACKET`, `MSG_WAITALL`, sendmsg/recvmsg on AF_INET.

**Design note (not done yet): system services over AF_UNIX.** The Powerbox and other
services that today are `/dev` files (`/dev/wm`, `/dev/clipboard`, `/dev/audit`) could be
sockets under `/run` (e.g. `/run/powerbox.sock`, root:admin 0660 or per-service groups)
served by a kernel thread or a user daemon. The server checks `SO_PEERCRED` (and the
caller's `/proc/<pid>/status` `Caps:` line) instead of trusting whoever opened a device,
answers requests with SCM_RIGHTS (the Powerbox hands back the chosen file as an fd, so
the sandboxed caller never needs an fs grant), and gets one connection per client with a
clean hangup when the client dies. Migrating means: a socket per service in `/run`, a small
request/response protocol, and the clients (rt, the wm) switching from `open("/dev/..")`
to `connect`.

### poll, select, epoll (`kernel/src/poll.rs`, `proc/epoll.rs`)

One readiness core: each object that can block answers `Inode::poll()` (Linux `POLL*`
bits) and calls `poll::notify()` when that may have changed (pipes, ttys, AF_UNIX sockets,
the TCP/UDP stack); waiters scan their fds, sleep until a notify or 20 ms (for objects that
never notify), and scan again. `ppoll` (73), `pselect6` (72) and epoll all sit on it.

* `epoll_create1` (20), `epoll_ctl` (21: ADD / MOD / DEL), `epoll_pwait` (22). Any fd with a
  poll method; regular files, directories and other epoll fds are EPERM / EINVAL. An entry
  disappears when its file is closed everywhere. Level-triggered by default;
  **EPOLLET** reports a ready fd once, then again only after new input / room
  (`Inode::poll_seq`, per object and per direction for pipes and AF_UNIX sockets; other
  objects use the global generation, so they may report extra edges but never miss one)
  or after it was seen not ready; **EPOLLONESHOT** disables an entry after one report
  until EPOLL_CTL_MOD. EPOLLERR / EPOLLHUP always. The sigmask argument is ignored; a
  pending signal is EINTR. `rt::epoll` on the user side.
* Pipes now report real readiness (they used to look always ready): POLLIN with data,
  POLLHUP when writers are gone, POLLOUT with room for an atomic write, POLLERR when
  readers are gone.

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
| files | 51 chroot, 56 openat, 57 close, 59 pipe2, 81 sync, 82 fsync, 83 fdatasync, 63 read, 64 write, 65 readv, 66 writev, 67 pread64, 68 pwrite64, 62 lseek, 80 fstat, 79 newfstatat, 78 readlinkat, 61 getdents64, 17 getcwd, 49 chdir, 50 fchdir, 34 mkdirat, 35 unlinkat, 36 symlinkat, 37 linkat, 276 renameat2, 46 ftruncate, 53 fchmodat, 52 fchmod, 54 fchownat, 55 fchown, 48 faccessat, 88 utimensat, 23 dup, 24 dup3, 25 fcntl, 29 ioctl (ttys: TCGETS, TCSETS*, TIOCGWINSZ, TIOCSWINSZ, TIOCGPGRP, TIOCSPGRP) |
| processes | 220 clone (fork **and** CLONE_THREAD), 221 execve, 260 wait4, 93 exit (one thread), 94 exit_group, 98 futex (WAIT/WAKE + _PRIVATE), 172 getpid, 173 getppid, 178 gettid, 96 set_tid_address, 99 set_robust_list (ignored), 154 setpgid, 155 getpgid, 156 getsid, 157 setsid |
| credentials | 174 getuid, 175 geteuid, 176 getgid, 177 getegid, 146 setuid, 144 setgid, 145 setreuid, 143 setregid, 147 setresuid, 148 getresuid, 149 setresgid, 150 getresgid, 158 getgroups, 159 setgroups (docs/security.md "Users and credentials") |
| signals | 129 kill, 130 tkill, 131 tgkill, 134 rt_sigaction, 135 rt_sigprocmask, 136 rt_sigpending, 139 rt_sigreturn |
| time | 101 nanosleep, 115 clock_nanosleep, 113 clock_gettime, 114 clock_getres, 169 gettimeofday, 124 sched_yield |
| memory | 214 brk, 222 mmap (anonymous private only), 215 munmap, 226 mprotect |
| sockets | 198 socket (AF_INET, AF_UNIX), 199 socketpair, 211 sendmsg, 212 recvmsg (AF_UNIX), 200 bind, 201 listen, 202 accept, 242 accept4, 203 connect, 204 getsockname, 205 getpeername, 206 sendto, 207 recvfrom, 208 setsockopt, 209 getsockopt, 210 shutdown, 73 ppoll (`proc/sock.rs`, see docs/networking.md) |
| readiness | 73 ppoll, 72 pselect6, 20 epoll_create1, 21 epoll_ctl, 22 epoll_pwait |
| misc | 160 uname (sysname "milkOS"), 278 getrandom |
| milkOS | 0x1000 sandbox (shrink capabilities; see docs/security.md), 0x1001 powerbox (ask the user for one file through the wm's Open/Save panel; docs/security.md) |

### Threads, chroot, sandbox

* **Threads** (`clone` with CLONE_VM|FS|FILES|SIGHAND|THREAD|SETTLS|PARENT_SETTID|
  CHILD_SETTID|CHILD_CLEARTID): a thread shares the process's address space, fds, signal
  handlers, cwd, pgid and capabilities; it has its own kernel stack, TPIDR_EL0, FP and
  user stack. `Inner.threads` lists them. `exit` (93) ends one thread (clears
  clear_child_tid + futex-wakes it); the last thread, or `exit_group` (94) / a fatal
  signal, zombifies the whole process (siblings get EINTR out of blocking calls and exit
  on return to EL0). Signals are posted to the thread group. **futex** (98) backs the
  pthread mutex/condvar in `user/rt` (`rt::thread`). A multi-threaded process's
  address-space teardown does an IPI TLB shootdown (`kernel/src/tlb.rs`); leaf
  invalidation still uses the broadcast `tlbi…is`.
* **Per-process root** (`chroot`, 51): the VFS resolver uses the calling thread's root for
  absolute paths and clamps `..` at it (docs/packages.md for the `run a tried app with its
  clone as /` flow).
* **Capability sandbox**: each process carries a capability set, inherited and shrink-only,
  enforced on openat/execve/fork/socket/device opens; denials are audited. Full model and
  the powerbox design in docs/security.md.
| milkOS desktop | 4100 MILK_WIN_CREATE, 4101 MILK_WIN_COMMIT, 4102 MILK_WIN_EVENT, 4103 MILK_WIN_TITLE, 4104 MILK_WIN_DESTROY (see "Desktop windows" below) |

Anything else returns ENOSYS and logs `syscall N not implemented` once per number, which
is the to-do list for running a static musl binary. `--features strace` logs every call.

### ELF loading and the initial stack

Linux compatibility check: `tests/linux/linux-hello.c` is a freestanding C program
(raw `svc #0` syscalls, no libc, no milkOS headers) built by `tests/linux/build.sh` with
the host's bare-metal `aarch64-elf-gcc` as a static ET_EXEC at `0x400000`. The binary is
committed and packed as `/bin/linux-hello`; `utest linux` runs it.

`proc/elf.rs`: ELF64 aarch64, `ET_EXEC` in the user range, or `ET_DYN` without
`PT_INTERP` (static-pie, loaded at `IMAGE_BASE`). Each `PT_LOAD` gets exact R/W/X page
permissions; overlapping pages take the union. The I-cache is synced after loading.
The stack is the Linux layout: `argc, argv[], NULL, envp[], NULL, auxv` (AT_PHDR,
AT_PHENT, AT_PHNUM, AT_PAGESZ, AT_ENTRY, AT_HWCAP = FP|ASIMD, AT_CLKTCK, AT_RANDOM, AT_UID / AT_EUID / AT_GID / AT_EGID (the new image's real and effective ids), AT_SECURE (1 for a set-ID exec or when real and effective ids differ),
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
* The UART is fed by the "tty-uart" kernel thread (sleeps on the RX interrupt, with a
  1 s timeout that runs `uart::poll` as a fallback: it rescues bytes if an RX interrupt
  was ever lost and logs "recovered by poll" once), so ^C works while nobody reads.
  `uart::read_byte` belongs to it. Regression test: `scripts/serial-soak.sh [boots]
  [commands]` types `echo tickK` once a second over serial; every tick must come back
  and the fallback must never fire.
* Not yet: sessions / controlling-tty rules (any process may TIOCSPGRP), SIGTTIN/SIGTTOU,
  job stop/continue.

### The desktop Terminal's tty: `/dev/tty1`

`kernel/src/wm/tty.rs`: `TTY1`, a `proc::tty::Tty` (above) registered as rdev 4:1, so
termios, window size, process groups and ^C -> SIGINT all come from the shared line
discipline and the syscall layer's tty registry. What the Terminal window adds:

* It feeds `TTY1.input` with the keys typed into it. Arrow keys, Home/End, Delete and Esc go
  in as xterm sequences (`ESC [ A`, `ESC [ H`, `ESC [ 3 ~`, ...): cooked mode swallows them,
  raw-mode programs read them. Ctrl+letters become control bytes; Cmd+C / Cmd+V copy and
  paste instead.
* It calls `TTY1.set_winsize(rows, cols)` with its real text area on every layout, so
  `TIOCGWINSZ` follows window resizes and the foreground job gets SIGWINCH on a change.
* A `term-sh` kernel thread keeps `msh` running on it (`proc::spawn_on`).

`/bin/stty` shows and changes it (`stty`, `stty size`, `stty raw`, `stty sane`, `stty -echo`).

### Desktop windows (`MILK_WIN_*`)

A process can own a desktop window. Kernel side: `kernel/src/wm/client.rs` (+ the arms in
`proc/syscall.rs`); user side: the `user/gui` crate. The window's body is a pixel buffer of
contiguous PMM frames owned by the window system and mapped into the process with
`UserSpace::map_shared` (pages marked `shared`: the space never frees them; `fork` gives the
child a private copy).

| nr | call | args → result |
|---|---|---|
| 4100 | `MILK_WIN_CREATE` | `(w, h, title, title_len, info)` → window id. Body up to 1000 x 700. Writes `struct { u64 addr; u32 w, h, stride, id; }` (24 bytes) to `info`: the XRGB pixels (`0x00RRGGBB`, `stride` = w) at `addr`, initially milk white. |
| 4101 | `MILK_WIN_COMMIT` | `(id, x, y, w, h)` → 0. Shows what changed in that rect (w = 0: everything). The wm redraws the window on its next frame. |
| 4102 | `MILK_WIN_EVENT` | `(id, ev, timeout_ms)` → 1 with a 32-byte event at `ev`, or 0 on timeout. Blocks up to `timeout_ms` (0 = poll). |
| 4103 | `MILK_WIN_TITLE` | `(id, title, len)` → 0. |
| 4104 | `MILK_WIN_DESTROY` | `(id)` → 0. Unmaps the buffer from the caller, frees it, removes the window. |

Errors: `EINVAL` (bad size), `EBADF` (no such window, or not the caller's), `EFAULT`.

Event (little endian): `u32 kind, i32 x, i32 y, u32 key, u32 mods, i32 extra, 8 bytes reserved`.
Kinds: 1 key down (`key` = Unicode code point, or 0x10001 Enter, 0x10002 Backspace, 0x10003
Tab, 0x10004 Esc, 0x10005..8 Up/Down/Left/Right, 0x10009 Delete; `mods` bits shift 1, ctrl 2,
alt 4, meta 8), 3 pointer moved with a button held (x, y in body coordinates), 4 / 5 button
down / up (`extra` = 1 left, 2 right, 3 middle), 6 scroll (`extra` = dy, positive = towards
the end), 7 close (the user closed the window: clean up and exit), 8 drop (a file was dragged
from Files onto the window at x, y: `key` is a new fd for it, opened read-only and granted
to the process; read it and close it). 2 (key up) is reserved.

Lifetime: closing the window on screen hides it at once and sends a close event; the buffer
stays mapped until the process destroys the window or exits. Windows of processes that exit
without destroying them are closed and freed by the wm within a second.

`/bin/paint` is the example client (dock, Skim, or `paint` in a shell).

### `/dev/wm` and the desktop tools

`kernel/src/wm/ctl.rs`. Write one command per line (fields tab-separated, or split at the
first space): `open <app|absolute path>` (apps: files terminal notes about paint today
spaces; a folder opens in Files, a text file in Notes, a `/bin` program runs), `notify
<title>[\t<subtitle>]`, `today`, `spaces`, `desktop`, `skim`. An unknown command fails the
write with `EINVAL`. Reading gives a status snapshot (refreshed once a second): `mode`,
`space`, `focus`, and one `window <id> <w>x<h>+<x>+<y> space <n> <title>` line per window.

`user/desk` builds the wrappers (one crate, `src/bin/*.rs`): `open <app|path>...` (relative
paths resolved against the cwd), `notify <title> [subtitle...]`, `today`, `spaces`, and
`cowsay [text]` (which just prints).

## Services: `milkd` and service files

`milkd` (`user/milkd`) is the service manager. init (PID 1) starts it first thing and
restarts it if it ever exits; init itself keeps only reaping orphans and the console
shell. milkd reads every `/etc/milk/services/<name>.service`, starts the enabled ones in
dependency order, supervises them (restart with backoff), captures their stdout and
stderr into the journal, and answers `svc` over `/run/milkd.sock`. Kernel threads are not
services.

### The service file format

Plain text, one `key = value` per line, `#` starts a comment, blank lines ignored, keys
case-sensitive, unknown keys are reported (`svc status` shows a warning) and ignored. The
file name (without `.service`) is the unit name: `[a-z0-9-]`, at most 32 characters.

| key | value | default |
|---|---|---|
| `exec` | the program and its arguments, split on spaces; `"..."` quotes one argument. The program is an absolute path. **Required.** | |
| `description` | one line for `svc list` | |
| `user` | account to run as (name from /etc/passwd). The service gets that user's uid, gid, groups, HOME, USER; never set-ID tricks needed. | `root` |
| `restart` | `always` (whenever it exits), `on-failure` (exit status != 0 or killed by a signal), `never` | `on-failure` |
| `after` | units (space separated) to start before this one: ordering only | |
| `needs` | units that must be running for this one to start; if one of them fails or isn't there, this unit is not started and its status says `dependency failed: <unit>` | |
| `sandbox` | a capability spec, exactly as for the `sandbox` tool (docs/security.md): `fs=/srv/www:r net=local spawn=0` ... The service's own binary is granted read automatically. | none (unrestricted) |
| `socket` | socket activation: milkd creates this AF_UNIX listening socket (a path, mode 0660, owned by `user`, group admin), and starts the service on the first connection, handing it the listener as **fd 3** with `LISTEN_FDS=1` / `LISTEN_PID` in the environment, like systemd. | |
| `cwd` | working directory | `/` |
| `env` | extra environment, space-separated `KEY=VALUE` | |
| `if-exists` | start only if this path exists (else status `skipped`) | |
| `if-contains` | `PATH TEXT...`: start only if the file contains TEXT | |
| `enabled` | `yes` / `no`: started at boot | `yes` |

Example (`/etc/milk/services/clockd.service`):

```text
# Alarms and timers for the desktop.
description = clock daemon: alarms
exec = /bin/clockd --quiet
user = milk
restart = always
after = drop
sandbox = fs=/home/milk/Library/Clock:rw net=none spawn=0
socket = /run/clockd.sock
```

**Restart and backoff.** After an unexpected exit milkd waits 100 ms, then doubles the
wait on every quick failure (one that ran less than 10 s) up to 30 s; a run of 10 s or
more resets it. After 8 quick failures in a row the unit is `failed` and left alone
until `svc start` / `svc restart`. `svc stop` sends SIGTERM, then SIGKILL after 3 s.

**Logs.** A service's stdout and stderr go (line by line) to the journal with its unit
name and level `info` (stdout) / `err` (stderr); `log -u <unit>` shows them.

**`svc`** (`svc list`, `svc status [unit]`, `svc start|stop|restart <unit>`,
`svc logs <unit>` = `log -u <unit>`): talks to milkd over `/run/milkd.sock` (AF_UNIX
stream, one text request per line, `SO_PEERCRED` decides): anyone may `list` / `status`;
`start`, `stop`, `restart` need root or the `admin` group. `MILK_TABLE=1` gives tables.

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

## Monitoring: `top`, `uptime`, `/proc/loadavg`, `/proc/diskstats`

`kernel/src/sysstat.rs` (separate from the subsystems it measures) adds:

* `/proc/loadavg`, Linux format (`0.08 0.02 0.01 3/13 14`): a `loadavg` kernel thread
  counts runnable threads (Ready or Running, idle threads and itself excluded) every 5 s
  into 1/5/15-minute exponential averages, in Linux's fixed point (FSHIFT 11).
* `/proc/diskstats`, Linux's first 14 columns for disks whose driver counts I/O
  (`BlockDevice::io_stats`, default `None`; virtio-blk keeps atomic counters).

`top` (`user/top`): raw tty + ANSI, redraws in place; per-CPU bars (`/proc/cpuload`),
memory, network (`/proc/net/dev`, all but lo) and disk (`/proc/diskstats`) rates, processes
sorted by CPU (`/proc/<pid>/{stat,status,cmdline}`; %CPU of one CPU over the refresh). Keys:
q quit, c / m / p sort by CPU / memory / pid, + / - refresh. Every line is clipped to the
terminal width and no more rows than TIOCGWINSZ reports are drawn, so it also fits the
desktop Terminal window. `top -b [-n N] [-d SECS]` prints plain frames (net-test checks
it). `uptime` shows the load averages when `/proc/loadavg` exists.
