//! Kernel feature tests from user space, one per subcommand. Prints PASS/FAIL lines.
//!   utest pipe    pipe + fork + dup2 + exec: read another program's stdout
//!   utest cow     fork shares memory copy-on-write; writes stay private
//!   utest proc    /proc/<pid> appears, describes the process, and goes away when reaped
//!   utest signal  handlers, sigreturn, masks, EINTR, SIGCHLD, process groups, defaults
//!   utest tlb     mmap/munmap churn in 4 processes across CPUs; unmapped memory faults
//!   utest cowsmp  4 children + parent write their copies of one buffer at once
//!   utest par [N] [ms]  N processes computing in parallel vs 1: prints the speedup
//!   utest threads 8 threads on a futex mutex, a condvar ping-pong, munmap under a thread
//!   utest chroot  a child chroots into a subdir; "/" is remapped and ".." can't escape
//!   utest sandbox a child drops caps; fs/net/spawn denials hit, grants still work
//!   utest all     everything except par

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use rt::{println, sys};

rt::main!(main);

fn check(name: &str, ok: bool, detail: &str) -> bool {
    println!(
        "utest: {} {name}: {detail}",
        if ok { "PASS" } else { "FAIL" }
    );
    ok
}

/// Run `argv` with stdout on a pipe, return everything it printed and its wait status.
fn capture(argv: &[&str]) -> (Vec<u8>, i32) {
    let (r, w) = sys::pipe().expect("pipe");
    let pid = match sys::fork().expect("fork") {
        0 => {
            sys::dup2(w, 1).expect("dup2");
            let e = sys::execve(argv[0], argv, &[]);
            rt::eprintln!("exec {}: {e}", argv[0]);
            sys::exit(127)
        }
        p => p,
    };
    let _ = sys::close(w);
    let mut out = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        match sys::read(r, &mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => {
                println!("utest: read: {e:?}");
                break;
            }
        }
    }
    let _ = sys::close(r);
    let (_, st) = sys::wait4(pid as i32, 0).unwrap_or((0, -1));
    (out, st)
}

fn t_pipe() -> bool {
    let (out, st) = capture(&["/System/Developer/hello", "piped"]);
    let text = String::from_utf8_lossy(&out);
    let lines = text.lines().count();
    let mut ok = check(
        "pipe+dup2+exec",
        st == 0 && text.starts_with("hello, world!") && text.contains("\"piped\""),
        &alloc::format!(
            "read {} bytes / {lines} lines from hello's stdout, status {st}",
            out.len()
        ),
    );
    // Big transfer through the pipe between two processes of ours.
    let (r, w) = sys::pipe().unwrap();
    const N: usize = 200_000;
    let pid = match sys::fork().unwrap() {
        0 => {
            let _ = sys::close(r);
            let chunk: Vec<u8> = (0..5000).map(|i| (i % 251) as u8).collect();
            for _ in 0..N / 5000 {
                sys::write_all(w, &chunk).unwrap();
            }
            sys::exit(0)
        }
        p => p,
    };
    let _ = sys::close(w);
    let mut total = 0;
    let mut good = true;
    let mut buf = [0u8; 3000];
    loop {
        let n = sys::read(r, &mut buf).unwrap();
        if n == 0 {
            break;
        }
        good &= buf[..n]
            .iter()
            .enumerate()
            .all(|(k, &b)| b == ((total + k) % 5000 % 251) as u8);
        total += n;
    }
    let _ = sys::close(r);
    let _ = sys::wait4(pid as i32, 0);
    ok &= check(
        "pipe 200 KB",
        total == N && good,
        &alloc::format!("{total} bytes, data {}", if good { "ok" } else { "BAD" }),
    );
    // Reader gone: SIGPIPE (ignored here, else it would kill us) and EPIPE (32).
    let old = sys::sigaction(sys::SIGPIPE, sys::SIG_IGN).unwrap();
    let (r, w) = sys::pipe().unwrap();
    let _ = sys::close(r);
    let e = sys::write(w, b"x");
    let _ = sys::close(w);
    sys::sigaction(sys::SIGPIPE, old).unwrap();
    ok &= check("EPIPE", e == Err(sys::Errno(32)), &alloc::format!("{e:?}"));
    ok
}

// ---- signals ----

static mut HITS: u32 = 0;
static mut LAST_SIG: i32 = 0;
static mut CHLD: u32 = 0;

extern "C" fn on_sig(sig: i32) {
    // SAFETY: single-threaded process; handlers don't nest here.
    unsafe {
        HITS += 1;
        LAST_SIG = sig;
        // Clobber FP/SIMD and callee-saved registers: sigreturn must put them back.
        core::arch::asm!(".arch_extension fp", "fmov d0, xzr", "fmov d8, xzr",
            out("x20") _, out("x21") _, out("x22") _);
    }
}

extern "C" fn on_chld(_sig: i32) {
    // SAFETY: as above.
    unsafe { CHLD += 1 };
}

extern "C" fn on_segv(_sig: i32) {
    rt::println!("utest:   (SIGSEGV handler ran, exiting 42)");
    sys::exit(42)
}

fn hits() -> u32 {
    // SAFETY: read of a plain static, single-threaded.
    unsafe { core::ptr::read_volatile(&raw const HITS) }
}

fn t_signal() -> bool {
    let me = sys::getpid() as i32;
    let mut ok = true;

    // 1. A handler runs and sigreturn brings back x19..x21 and d0/d8.
    sys::signal(sys::SIGUSR1, on_sig).unwrap();
    let (mut x19, mut d0, mut d8): (u64, u64, u64);
    // SAFETY: kill(getpid(), SIGUSR1) via raw syscall with live callee-saved/FP values.
    unsafe {
        core::arch::asm!(
            ".arch_extension fp",
            "mov x20, #0x1234",
            "movk x20, #0x5678, lsl #16",
            "mov x9, #0x4321",
            "fmov d0, x9",
            "fmov d8, x20",
            "mov x8, #129",
            "svc #0",
            "mov {x19}, x20",
            "fmov {d0}, d0",
            "fmov {d8}, d8",
            x19 = out(reg) x19, d0 = out(reg) d0, d8 = out(reg) d8,
            in("x0") me, in("x1") sys::SIGUSR1,
            out("x8") _, out("x9") _, out("x20") _,
        );
    }
    ok &= check(
        "handler + sigreturn (x20, d0, d8 restored)",
        hits() == 1 && x19 == 0x5678_1234 && d0 == 0x4321 && d8 == 0x5678_1234,
        &alloc::format!("hits {}, x20 {x19:#x}, d0 {d0:#x}, d8 {d8:#x}", hits()),
    );

    // 2. Blocked signals wait until unblocked.
    sys::sigprocmask(sys::SIG_BLOCK, sys::sigmask(sys::SIGUSR1)).unwrap();
    sys::kill(me, sys::SIGUSR1).unwrap();
    let held = hits();
    sys::sigprocmask(sys::SIG_UNBLOCK, sys::sigmask(sys::SIGUSR1)).unwrap();
    ok &= check(
        "sigprocmask",
        held == 1 && hits() == 2,
        &alloc::format!("while blocked {held}, after unblock {}", hits()),
    );

    // 3. A fault handler.
    let pid = sys::fork().unwrap();
    if pid == 0 {
        sys::signal(sys::SIGSEGV, on_segv).unwrap();
        // SAFETY: deliberately not.
        unsafe { core::ptr::read_volatile(core::ptr::null::<u8>()) };
        sys::exit(1);
    }
    let (_, st) = sys::wait4(pid as i32, 0).unwrap();
    ok &= check("SIGSEGV handler", st == 42 << 8, &sys::describe_status(st));

    // 4. kill interrupts a blocking read: EINTR with a handler, termination without.
    for with_handler in [true, false] {
        let (r, w) = sys::pipe().unwrap();
        let pid = sys::fork().unwrap();
        if pid == 0 {
            if with_handler {
                sys::signal(sys::SIGTERM, on_sig).unwrap();
            }
            let mut b = [0u8; 1];
            let e = sys::read(r, &mut b);
            sys::exit(if e == Err(sys::Errno(4)) { 7 } else { 1 });
        }
        sys::sleep_ms(30);
        sys::kill(pid as i32, sys::SIGTERM).unwrap();
        let (_, st) = sys::wait4(pid as i32, 0).unwrap();
        let _ = (sys::close(r), sys::close(w));
        if with_handler {
            ok &= check("EINTR on read", st == 7 << 8, &sys::describe_status(st));
        } else {
            ok &= check("default SIGTERM", st == 15, &sys::describe_status(st));
        }
    }

    // 5. SIGCHLD reaches the parent; nanosleep gets cut short by it.
    sys::signal(sys::SIGCHLD, on_chld).unwrap();
    let pid = sys::fork().unwrap();
    if pid == 0 {
        sys::exit(0);
    }
    let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
    let slept = sys::nanosleep(2_000_000_000);
    let dt = (sys::clock_ns(sys::CLOCK_MONOTONIC) - t0) / 1_000_000;
    let _ = sys::wait4(pid as i32, 0);
    let chld = unsafe { core::ptr::read_volatile(&raw const CHLD) };
    ok &= check(
        "SIGCHLD",
        chld == 1 && slept == Err(sys::Errno(4)) && dt < 1000,
        &alloc::format!("handler ran {chld}x, nanosleep {slept:?} after {dt} ms"),
    );
    sys::sigaction(sys::SIGCHLD, sys::SIG_DFL).unwrap();

    // 6. Process groups: kill(-pgid) reaches every member.
    let mut kids = Vec::new();
    let mut pgid = 0;
    for _ in 0..3 {
        let pid = sys::fork().unwrap();
        if pid == 0 {
            loop {
                sys::sleep_ms(1000);
            }
        }
        if pgid == 0 {
            pgid = pid;
        }
        sys::setpgid(pid, pgid).unwrap();
        kids.push(pid);
    }
    let groups: Vec<u32> = kids.iter().map(|&k| sys::getpgid(k).unwrap_or(0)).collect();
    sys::kill(-(pgid as i32), sys::SIGKILL).unwrap();
    let statuses: Vec<i32> = kids
        .iter()
        .map(|&k| sys::wait4(k as i32, 0).unwrap().1)
        .collect();
    ok &= check(
        "process group kill",
        groups.iter().all(|&g| g == pgid) && statuses.iter().all(|&s| s == 9),
        &alloc::format!("pgids {groups:?}, statuses {statuses:?}"),
    );

    // 7. Ignored signals do nothing; SIGKILL can't be caught.
    sys::sigaction(sys::SIGUSR2, sys::SIG_IGN).unwrap();
    sys::kill(me, sys::SIGUSR2).unwrap();
    let e = sys::sigaction(sys::SIGKILL, sys::SIG_IGN);
    ok &= check(
        "SIG_IGN / SIGKILL",
        e == Err(sys::Errno(22)),
        "SIGUSR2 ignored, sigaction(SIGKILL) -> EINVAL",
    );
    ok
}

fn mem_free_kb() -> u64 {
    let m = sys::read_file("/proc/meminfo").unwrap_or_default();
    String::from_utf8_lossy(&m)
        .lines()
        .find_map(|l| l.strip_prefix("MemFree:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap_or(0)
}

fn t_cow() -> bool {
    const N: usize = 1 << 20;
    let mut buf: Vec<u8> = (0..N).map(|i| (i % 253) as u8).collect();
    let before = mem_free_kb();
    let (r, w) = sys::pipe().unwrap();
    let (r2, w2) = sys::pipe().unwrap();
    let pid = sys::fork().unwrap();
    if pid == 0 {
        let _ = (sys::close(r), sys::close(w2));
        let after_fork = mem_free_kb();
        let shared_ok = buf.iter().enumerate().all(|(i, &b)| b == (i % 253) as u8);
        for b in buf.iter_mut() {
            *b = 0xee;
        }
        let after_write = mem_free_kb();
        // Tell the parent, then wait until it has scribbled on its own copy.
        let _ = sys::write(w, &[shared_ok as u8]);
        let msg = alloc::format!("{after_fork} {after_write}");
        let _ = sys::write(w, msg.as_bytes());
        let _ = sys::close(w);
        let mut b = [0u8; 1];
        let _ = sys::read(r2, &mut b);
        let mine = buf.iter().all(|&b| b == 0xee);
        sys::exit(if mine { 0 } else { 3 });
    }
    let _ = (sys::close(w), sys::close(r2));
    let mut got = Vec::new();
    let mut b = [0u8; 64];
    loop {
        match sys::read(r, &mut b) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&b[..n]),
        }
    }
    let _ = sys::close(r);
    let parent_ok = buf.iter().enumerate().all(|(i, &b)| b == (i % 253) as u8);
    buf[0] = 1;
    buf[N - 1] = 2;
    let _ = sys::write(w2, b"x");
    let _ = sys::close(w2);
    let (_, st) = sys::wait4(pid as i32, 0).unwrap();
    let nums: Vec<u64> = String::from_utf8_lossy(&got[1..])
        .split(' ')
        .filter_map(|v| v.parse().ok())
        .collect();
    let (af, aw) = (
        nums.first().copied().unwrap_or(0),
        nums.get(1).copied().unwrap_or(0),
    );
    let fork_cost = before.saturating_sub(af);
    let write_cost = af.saturating_sub(aw);
    check(
        "copy-on-write fork",
        got.first() == Some(&1) && parent_ok && st == 0 && fork_cost < 512 && write_cost >= 1000,
        &alloc::format!(
            "fork took {fork_cost} KiB with 1 MiB of heap, child's writes then copied {write_cost} KiB; parent kept its data, child kept its own (status {st})"
        ),
    )
}

fn t_proc() -> bool {
    let (r, w) = sys::pipe().unwrap();
    let pid = sys::fork().unwrap();
    if pid == 0 {
        let _ = sys::close(w);
        let mut b = [0u8; 1];
        let _ = sys::read(r, &mut b); // wait for the parent
        sys::exit(0);
    }
    let _ = sys::close(r);
    let status = String::from_utf8_lossy(
        &sys::read_file(&alloc::format!("/proc/{pid}/status")).unwrap_or_default(),
    )
    .into_owned();
    let cmdline = sys::read_file(&alloc::format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let fds: Vec<String> = sys::read_dir(&alloc::format!("/proc/{pid}/fd"))
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.0)
        .filter(|n| n != "." && n != "..")
        .collect();
    let fd3 = sys::readlink(&alloc::format!("/proc/{pid}/fd/3")).unwrap_or_default();
    let me = sys::readlink("/proc/self").unwrap_or_default();
    let _ = sys::close(w);
    let _ = sys::wait4(pid as i32, 0);
    let gone = sys::read_file(&alloc::format!("/proc/{pid}/status")).is_err();
    let ok = status.contains(&alloc::format!("PPid:\t{}", sys::getpid()))
        // argv[0] as typed: `utest` on a PATH, or `/System/Developer/utest`.
        && cmdline
            .split(|&b| b == 0)
            .next()
            .is_some_and(|a0| a0 == b"utest" || a0.ends_with(b"/utest"))
        && fd3.starts_with("pipe:[")
        && me == alloc::format!("{}", sys::getpid())
        && gone;
    check(
        "/proc/<pid>",
        ok,
        &alloc::format!("fds {fds:?}, fd/3 -> {fd3}, /proc/self -> {me}, gone after reap: {gone}"),
    )
}

// ---- SMP stress ----

extern "C" fn segv_exit42(_: i32) {
    sys::exit(42)
}

fn t_tlb() -> bool {
    let mut kids = Vec::new();
    for k in 0..4u8 {
        match sys::fork().unwrap() {
            0 => {
                for i in 0..300u32 {
                    let len = 64 << 10;
                    let p = sys::mmap_anon(len).unwrap_or(core::ptr::null_mut());
                    if p.is_null() {
                        sys::exit(2);
                    }
                    // SAFETY: fresh private mapping of `len` bytes.
                    let s = unsafe { core::slice::from_raw_parts_mut(p, len) };
                    let v = (i as u8) ^ k;
                    for b in s.iter_mut().step_by(512) {
                        *b = v;
                    }
                    if !s.iter().step_by(512).all(|&b| b == v) {
                        sys::exit(3);
                    }
                    let _ = sys::munmap(p, len);
                    sys::sched_yield(); // move around between CPUs
                }
                // A page we just unmapped must fault, on whatever CPU we are now.
                let p = sys::mmap_anon(4096).unwrap();
                // SAFETY: mapped.
                unsafe { core::ptr::write_volatile(p, 1) };
                let _ = sys::munmap(p, 4096);
                sys::sched_yield();
                sys::signal(sys::SIGSEGV, segv_exit42).unwrap();
                // SAFETY: deliberately not mapped any more.
                let v = unsafe { core::ptr::read_volatile(p) };
                rt::println!("utest: stale TLB? read {v} from an unmapped page");
                sys::exit(1)
            }
            pid => kids.push(pid),
        }
    }
    let st: Vec<i32> = kids
        .iter()
        .map(|&k| sys::wait4(k as i32, 0).unwrap().1)
        .collect();
    check(
        "TLB: 4 x 300 mmap/munmap across CPUs",
        st.iter().all(|&s| s == 42 << 8),
        &alloc::format!(
            "statuses {:?} (42 = unmapped page faulted, as it must)",
            st.iter().map(|s| s >> 8).collect::<Vec<_>>()
        ),
    )
}

fn t_cowsmp() -> bool {
    const N: usize = 256 << 10;
    let mut buf: Vec<u8> = alloc::vec![0xaa; N];
    let mut kids = Vec::new();
    for k in 1..=4u8 {
        match sys::fork().unwrap() {
            0 => {
                for round in 0..20u8 {
                    let v = k.wrapping_mul(16).wrapping_add(round);
                    for b in buf.iter_mut().step_by(64) {
                        *b = v;
                    }
                    sys::sched_yield();
                    if !buf.iter().step_by(64).all(|&b| b == v) {
                        sys::exit(3);
                    }
                }
                sys::exit(0)
            }
            pid => kids.push(pid),
        }
    }
    let mut parent_ok = true;
    for round in 0..20u8 {
        for b in buf.iter_mut().step_by(64) {
            *b = 0xf0 ^ round;
        }
        sys::sched_yield();
        parent_ok &= buf.iter().step_by(64).all(|&b| b == 0xf0 ^ round);
    }
    let st: Vec<i32> = kids
        .iter()
        .map(|&k| sys::wait4(k as i32, 0).unwrap().1)
        .collect();
    check(
        "COW with 5 writers at once",
        parent_ok && st.iter().all(|&s| s == 0),
        &alloc::format!("parent kept its copy: {parent_ok}, children {st:?}"),
    )
}

/// A chunk of pure CPU work (integer hashing), about `units` x 1 ms on one core.
fn work(units: u64) -> u64 {
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..units * 600_000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x = core::hint::black_box(x);
    }
    x
}

/// Run `n` processes each doing `units` of work; wall time in ms.
fn run_workers(n: usize, units: u64) -> u64 {
    let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
    let mut kids = Vec::new();
    for _ in 0..n {
        match sys::fork().unwrap() {
            0 => {
                let r = work(units);
                sys::exit((r & 1) as i32)
            }
            pid => kids.push(pid),
        }
    }
    for k in kids {
        let _ = sys::wait4(k as i32, 0);
    }
    (sys::clock_ns(sys::CLOCK_MONOTONIC) - t0) / 1_000_000
}

/// (steals, migrations) summed over CPUs from /proc/sched (0s if it's missing).
fn sched_counts() -> (u64, u64) {
    let t = sys::read_file("/proc/sched").unwrap_or_default();
    let t = String::from_utf8_lossy(&t);
    let mut out = (0, 0);
    for l in t
        .lines()
        .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
    {
        let f: Vec<u64> = l
            .split_whitespace()
            .filter_map(|x| x.parse().ok())
            .collect();
        if f.len() >= 4 {
            out.0 += f[2];
            out.1 += f[3];
        }
    }
    out
}

fn sched_policy() -> String {
    let t = sys::read_file("/proc/sched").unwrap_or_default();
    String::from_utf8_lossy(&t)
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("policy "))
        .unwrap_or("?")
        .into()
}

/// Context-switch microbenchmarks (not pass/fail): pipe ping-pong between two processes
/// and between two threads (a wake + block on each side per hop), and a sched_yield storm
/// with 2 threads per CPU. Compare run-queue policies with `print global > /proc/sched`.
fn t_csw(rounds: u32) -> bool {
    let cpus = sys::read_file("/proc/cpuinfo")
        .map(|c| String::from_utf8_lossy(&c).matches("processor").count())
        .unwrap_or(1);
    let now = || sys::clock_ns(sys::CLOCK_MONOTONIC);

    // Ping-pong over two pipes; `spawn_peer` starts the echoing side.
    fn ping(rounds: u32, a_w: i32, b_r: i32) -> u64 {
        let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
        let mut b = [0u8; 1];
        for _ in 0..rounds {
            sys::write(a_w, b"x").unwrap();
            sys::read(b_r, &mut b).unwrap();
        }
        sys::clock_ns(sys::CLOCK_MONOTONIC) - t0
    }
    fn echo(rounds: u32, a_r: i32, b_w: i32) {
        let mut b = [0u8; 1];
        for _ in 0..rounds {
            sys::read(a_r, &mut b).unwrap();
            sys::write(b_w, &b).unwrap();
        }
    }

    let delta = |c0: (u64, u64)| {
        let c1 = sched_counts();
        (c1.0 - c0.0, c1.1 - c0.1)
    };

    // Processes.
    let c0 = sched_counts();
    let (a_r, a_w) = sys::pipe().unwrap();
    let (b_r, b_w) = sys::pipe().unwrap();
    let pid = match sys::fork().unwrap() {
        0 => {
            echo(rounds, a_r, b_w);
            sys::exit(0)
        }
        p => p,
    };
    let proc_ns = ping(rounds, a_w, b_r);
    let _ = sys::wait4(pid as i32, 0);
    for fd in [a_r, a_w, b_r, b_w] {
        let _ = sys::close(fd);
    }
    let proc_m = delta(c0);

    // Threads of one process.
    let c0 = sched_counts();
    let (a_r, a_w) = sys::pipe().unwrap();
    let (b_r, b_w) = sys::pipe().unwrap();
    let h = thread::spawn(move || echo(rounds, a_r, b_w));
    let thr_ns = ping(rounds, a_w, b_r);
    h.join();
    for fd in [a_r, a_w, b_r, b_w] {
        let _ = sys::close(fd);
    }
    let thr_m = delta(c0);

    // Yield storm.
    let c0 = sched_counts();
    let n = 2 * cpus;
    let per = rounds * 2;
    let t0 = now();
    let hs: Vec<_> = (0..n)
        .map(|_| {
            thread::spawn(move || {
                for _ in 0..per {
                    sys::sched_yield();
                }
            })
        })
        .collect();
    for h in hs {
        h.join();
    }
    let y_ns = now() - t0;
    let y_m = delta(c0);
    let total = n as u64 * per as u64;
    check(
        "context switch",
        true,
        &alloc::format!(
            "[{}] pipe round trip: processes {:.1} us (steals {}, migrations {}), threads {:.1} us \
             (steals {}, migrations {}); yield storm {n} threads x {per}: {:.2} us/yield, \
             {} yields/s (steals {}, migrations {})",
            sched_policy(),
            proc_ns as f64 / rounds as f64 / 1000.0,
            proc_m.0,
            proc_m.1,
            thr_ns as f64 / rounds as f64 / 1000.0,
            thr_m.0,
            thr_m.1,
            y_ns as f64 / total as f64 / 1000.0,
            total * 1_000_000_000 / y_ns.max(1),
            y_m.0,
            y_m.1
        ),
    )
}

fn t_par(n: usize, units: u64) -> bool {
    let cpus = sys::read_file("/proc/cpuinfo")
        .map(|c| String::from_utf8_lossy(&c).matches("processor").count())
        .unwrap_or(1);
    run_workers(1, units / 10 + 1); // warm-up (page cache, COW paths)
    let t1 = run_workers(1, units).min(run_workers(1, units));
    let c0 = sched_counts();
    let tn = run_workers(n, units).min(run_workers(n, units));
    let c1 = sched_counts();
    let speedup = (n as f64 * t1 as f64) / tn.max(1) as f64;
    check(
        "parallel speedup",
        true,
        &alloc::format!(
            "[{}] {cpus} CPUs: 1 job {t1} ms, {n} jobs {tn} ms -> speedup {speedup:.2}x \
             (ideal {:.0}x); 2 runs of {n} jobs: steals {}, migrations {}",
            sched_policy(),
            (n.min(cpus)) as f64,
            c1.0 - c0.0,
            c1.1 - c0.1
        ),
    )
}

// ---- threads ----

use rt::thread::{self, Condvar, Mutex};

fn t_threads() -> bool {
    let mut ok = true;

    // 8 threads each increment a shared counter 10_000 times under a futex mutex.
    let counter = alloc::sync::Arc::new(Mutex::new(0u64));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let c = counter.clone();
            thread::spawn(move || {
                for _ in 0..10_000 {
                    *c.lock() += 1;
                }
            })
        })
        .collect();
    for h in handles {
        h.join();
    }
    let total = *counter.lock();
    ok &= check(
        "futex mutex: 8 x 10000",
        total == 80_000,
        &alloc::format!("counter = {total}"),
    );

    // Condvar ping-pong: two threads hand a turn back and forth 1000 times.
    struct Pp {
        turn: Mutex<u32>,
        cv: Condvar,
    }
    let pp = alloc::sync::Arc::new(Pp {
        turn: Mutex::new(0),
        cv: Condvar::new(),
    });
    let n = 1000u32;
    let a = {
        let pp = pp.clone();
        thread::spawn(move || {
            for i in 0..n {
                let mut g = pp.turn.lock();
                while *g != i * 2 {
                    g = pp.cv.wait(g);
                }
                *g += 1;
                pp.cv.notify_all();
            }
        })
    };
    let b = {
        let pp = pp.clone();
        thread::spawn(move || {
            for i in 0..n {
                let mut g = pp.turn.lock();
                while *g != i * 2 + 1 {
                    g = pp.cv.wait(g);
                }
                *g += 1;
                pp.cv.notify_all();
            }
        })
    };
    a.join();
    b.join();
    let final_turn = *pp.turn.lock();
    ok &= check(
        "condvar ping-pong",
        final_turn == n * 2,
        &alloc::format!("{n} rounds, turn counter {final_turn}"),
    );

    // munmap in one thread while another keeps running (the kernel must not fault, and the
    // live mapping must stay valid). The worker spins touching its own buffer; the main
    // thread maps and unmaps 200 regions meanwhile.
    let stop = alloc::sync::Arc::new(core::sync::atomic::AtomicBool::new(false));
    let seen = alloc::sync::Arc::new(core::sync::atomic::AtomicU64::new(0));
    let worker = {
        let (stop, seen) = (stop.clone(), seen.clone());
        thread::spawn(move || {
            let mut buf = alloc::vec![0u8; 4096];
            let mut i = 0u64;
            while !stop.load(core::sync::atomic::Ordering::Relaxed) {
                let n = buf.len();
                buf[(i as usize) % n] = i as u8;
                i = i.wrapping_add(1);
                core::hint::black_box(&buf);
                seen.store(i, core::sync::atomic::Ordering::Relaxed);
            }
        })
    };
    for _ in 0..200 {
        if let Ok(p) = sys::mmap_anon(64 << 10) {
            // SAFETY: fresh mapping.
            unsafe { core::ptr::write_volatile(p, 1) };
            let _ = sys::munmap(p, 64 << 10);
        }
    }
    let before = seen.load(core::sync::atomic::Ordering::Relaxed);
    sys::sched_yield();
    sys::sleep_ms(5);
    let after = seen.load(core::sync::atomic::Ordering::Relaxed);
    stop.store(true, core::sync::atomic::Ordering::Relaxed);
    worker.join();
    ok &= check(
        "munmap while a thread runs",
        after > before,
        &alloc::format!("worker kept running ({before} -> {after} iterations)"),
    );
    ok
}

fn t_chroot() -> bool {
    // Build a jail with one file, in a fresh process so our own root is untouched.
    let _ = sys::mkdir("/tmp/jail", 0o755);
    let _ = sys::mkdir("/tmp/jail/sub", 0o755);
    {
        let fd =
            sys::open_mode("/tmp/jail/hello", sys::O_WRONLY | sys::O_CREAT, 0o644).unwrap_or(-1);
        if fd >= 0 {
            let _ = sys::write_all(
                fd,
                b"in jail
",
            );
            let _ = sys::close(fd);
        }
    }
    let (r, w) = sys::pipe().unwrap();
    let pid = sys::fork().unwrap();
    if pid == 0 {
        let _ = sys::close(r);
        let mut rc = 0u8;
        if sys::chroot("/tmp/jail").is_err() {
            rc = 1;
        }
        // "/hello" is now the jailed file; "/tmp" (the outer world) is gone.
        let inside = sys::read_file("/hello")
            .map(|d| {
                d == b"in jail
"
            })
            .unwrap_or(false);
        let outer_gone = sys::stat_path("/tmp/jail", true).is_err();
        // ".." from root can't escape: "/sub/../hello" stays inside.
        let dotdot = sys::read_file("/sub/../hello")
            .map(|d| {
                d == b"in jail
"
            })
            .unwrap_or(false);
        let escape = sys::read_file("/../../etc/motd").is_ok();
        if !(inside && outer_gone && dotdot && !escape) {
            rc |= 2;
        }
        let _ = sys::write(w, &[rc]);
        sys::exit(rc as i32)
    }
    let _ = sys::close(w);
    let mut b = [0u8; 1];
    let _ = sys::read(r, &mut b);
    let _ = sys::close(r);
    let (_, st) = sys::wait4(pid as i32, 0).unwrap();
    // Our own root is unchanged: the outer path still resolves here.
    let parent_unaffected = sys::stat_path("/tmp/jail/hello", true).is_ok();
    check(
        "chroot jail",
        st == 0 && parent_unaffected,
        &alloc::format!("child rc {}, parent root intact: {parent_unaffected}", b[0]),
    )
}

fn t_sandbox() -> bool {
    let _ = sys::mkdir("/tmp/sb", 0o755);
    {
        let fd = sys::open_mode("/tmp/sb/ok", sys::O_WRONLY | sys::O_CREAT, 0o644).unwrap_or(-1);
        if fd >= 0 {
            let _ = sys::write_all(
                fd, b"ok
",
            );
            let _ = sys::close(fd);
        }
    }
    let (r, w) = sys::pipe().unwrap();
    let pid = sys::fork().unwrap();
    if pid == 0 {
        let _ = sys::close(r);
        // Restrict: read-only under /tmp/sb, no network, no spawn.
        sys::sandbox("fs=/tmp/sb:r net=none spawn=0").expect("sandbox");
        let mut bits = 0u8;
        // Granted read works.
        if sys::read_file("/tmp/sb/ok")
            .map(|d| {
                d == b"ok
"
            })
            .unwrap_or(false)
        {
            bits |= 1;
        }
        // Write under the read-only grant is denied.
        if sys::open_mode("/tmp/sb/no", sys::O_WRONLY | sys::O_CREAT, 0o644).is_err() {
            bits |= 2;
        }
        // Reading outside the grant is denied.
        if sys::read_file("/System/Settings/motd").is_err() {
            bits |= 4;
        }
        // Opening a socket is denied (net=none).
        if rt::net::socket(1).is_err() {
            bits |= 8;
        }
        // fork is denied (spawn dropped).
        match sys::fork() {
            Ok(0) => sys::exit(0),
            Ok(_) => {}
            Err(_) => bits |= 16,
        }
        // Can't widen back: asking for full fs again stays denied.
        let _ = sys::sandbox("net=all");
        if sys::read_file("/System/Settings/motd").is_err() {
            bits |= 32;
        }
        let _ = sys::write(w, &[bits]);
        sys::exit(bits as i32)
    }
    let _ = sys::close(w);
    let mut b = [0u8; 1];
    let _ = sys::read(r, &mut b);
    let _ = sys::close(r);
    let (_, st) = sys::wait4(pid as i32, 0).unwrap();
    // Our own caps are untouched: we can still read /System/Settings/motd and open a socket.
    let parent_ok = sys::read_file("/System/Settings/motd").is_ok();
    check(
        "capability sandbox",
        b[0] == 0b111111 && st == (0b111111 << 8) && parent_ok,
        &alloc::format!(
            "child checks {:#08b} (want 0b111111), parent unrestricted: {parent_ok}",
            b[0]
        ),
    )
}

/// The kernel lives in the higher half (TTBR1): a user pointer into it must be EFAULT for a
/// syscall and SIGSEGV for a load, never a kernel read.
fn t_kaddr() -> bool {
    const KERNEL_IMAGE: usize = 0xffff_0000_4008_0000;
    // SAFETY: never dereferenced here; the kernel must refuse it.
    let kbuf = unsafe { core::slice::from_raw_parts(KERNEL_IMAGE as *const u8, 16) };
    let w = sys::write(1, kbuf);
    let pid = sys::fork().unwrap();
    if pid == 0 {
        // SAFETY: deliberately not: must fault.
        let v = unsafe { core::ptr::read_volatile(KERNEL_IMAGE as *const u8) };
        sys::exit(v as i32);
    }
    let (_, st) = sys::wait4(pid as i32, 0).unwrap();
    check(
        "kernel addresses",
        w == Err(sys::Errno(14)) && st & 0x7f == sys::SIGSEGV,
        &alloc::format!(
            "write(kernel ptr) -> {w:?}, load from {KERNEL_IMAGE:#x}: {}",
            sys::describe_status(st)
        ),
    )
}

/// A static Linux/aarch64 program built with no milkOS knowledge (tests/linux/), linked at
/// Linux's 0x400000: runs and passes its own checks.
fn t_linux() -> bool {
    let (out, st) = capture(&["/System/Developer/linux-hello", "x"]);
    let text = core::str::from_utf8(&out).unwrap_or("");
    let oks = text.matches("  ok ").count();
    check(
        "Linux static program at 0x400000",
        st == 0 && text.contains("linux-hello: ALL OK"),
        &alloc::format!("{oks} checks ok, {}", sys::describe_status(st)),
    )
}

/// Credentials: an unprivileged process can't raise its ids; `admin` (set-user-ID root, asks
/// for the caller's password) and `users switch` can.
fn t_creds() -> bool {
    if sys::geteuid() == 0 {
        return as_milk("creds", "credentials");
    }
    let (r, e, sv) = sys::getresuid();
    let me = sys::geteuid();
    let mut ok = true;
    let mut detail = alloc::format!("ids {r}/{e}/{sv}");
    if me != 0 {
        ok &= sys::setuid(0).is_err() && sys::setgroups(&[0]).is_err();
        ok &= sys::setresuid(u32::MAX, 0, u32::MAX).is_err();
        detail += ", setuid(0)/setgroups/setresuid refused";
    }
    // admin with the right and the wrong password (from stdin).
    let run = |pw: &str| {
        let (r, w) = sys::pipe().unwrap();
        let (r2, w2) = sys::pipe().unwrap();
        let pid = match sys::fork().unwrap() {
            0 => {
                sys::dup2(r, 0).unwrap();
                sys::dup2(w2, 1).unwrap();
                let _ = sys::execve(
                    "/Commands/admin",
                    &["admin", "--stdin", "/Commands/users", "me"],
                    &[],
                );
                sys::exit(127)
            }
            p => p,
        };
        let _ = sys::close(r);
        let _ = sys::close(w2);
        let _ = sys::write_all(w, alloc::format!("{pw}\n").as_bytes());
        let _ = sys::close(w);
        let mut out = alloc::vec::Vec::new();
        let mut b = [0u8; 64];
        while let Ok(n @ 1..) = sys::read(r2, &mut b) {
            out.extend_from_slice(&b[..n]);
        }
        let _ = sys::close(r2);
        let (_, st) = sys::wait4(pid as i32, 0).unwrap();
        (String::from(String::from_utf8_lossy(&out).trim()), st)
    };
    let (good, gst) = run("milk");
    let (bad, bst) = run("wrong");
    // `users me` as root: an "id  0" line.
    let root = good.lines().any(|l| l.split_whitespace().eq(["id", "0"]));
    ok &= root && gst == 0 && bad.is_empty() && bst != 0;
    let first = good.lines().find(|l| l.starts_with("id")).unwrap_or("");
    detail += &alloc::format!("; admin good -> {first:?} ({gst}), bad -> {bad:?} ({bst})");
    check("credentials", ok, &detail)
}

/// Run `utest <what>` as the milk user (`users switch milk --command`: root needs no
/// password), for the
/// tests that are about being unprivileged, when utest itself runs as root (the console).
fn as_milk(what: &str, name: &str) -> bool {
    let cmd = alloc::format!("/System/Developer/utest {what}");
    let (out, st) = capture(&["/Commands/users", "switch", "milk", "--command", &cmd]);
    let text = String::from_utf8_lossy(&out);
    for l in text.lines() {
        println!("  (as milk) {l}");
    }
    check(
        name,
        st == 0 && text.contains("ALL PASS"),
        &alloc::format!("as milk via users switch: {}", sys::describe_status(st)),
    )
}

/// Run `argv` through `admin --stdin` with the default test password ("milk", see
/// docs/security.md). Returns the wait status.
fn as_admin(argv: &[&str]) -> i32 {
    let (r, w) = sys::pipe().unwrap();
    let pid = match sys::fork().unwrap() {
        0 => {
            sys::dup2(r, 0).unwrap();
            let mut v = alloc::vec!["admin", "--stdin"];
            v.extend_from_slice(argv);
            let _ = sys::execve("/Commands/admin", &v, &[]);
            sys::exit(127)
        }
        p => p,
    };
    let _ = sys::close(r);
    let _ = sys::write_all(w, b"milk\n");
    let _ = sys::close(w);
    sys::wait4(pid as i32, 0).map_or(-1, |x| x.1)
}

/// Unix permissions as a non-root user: rwx on files and directories, ownership of new
/// files, sticky /tmp, chmod/chown rules, exec bits; and the sandbox still on top.
fn t_perms() -> bool {
    if sys::geteuid() == 0 {
        return as_milk("perms", "permissions");
    }
    let me = sys::geteuid();
    let eacces = |r: sys::Result<i32>| matches!(r, Err(sys::Errno(13)));
    let mut bits = 0u32;
    let mut n = 0;
    let mut t = |ok: bool| {
        if ok {
            bits |= 1 << n;
        }
        n += 1;
    };
    // 0: /System/Settings/shadow unreadable; 1: /System/Settings/passwd not writable; 2: can't create in /System/Settings.
    t(eacces(sys::open("/System/Settings/shadow", sys::O_RDONLY)));
    t(eacces(sys::open("/System/Settings/passwd", sys::O_WRONLY)));
    t(eacces(sys::open_mode(
        "/System/Settings/utest",
        sys::O_WRONLY | sys::O_CREAT,
        0o644,
    )));
    // 3: a new file is mine.
    let f = "/tmp/utest-perm";
    let _ = sys::unlink(f);
    if let Ok(fd) = sys::open_mode(f, sys::O_WRONLY | sys::O_CREAT, 0o644) {
        let _ = sys::write_all(fd, b"#!x\n");
        let _ = sys::close(fd);
    }
    t(sys::stat_path(f, true).is_ok_and(|s| s.uid == me));
    // 4: mode 000 locks even the owner out; 5: the owner can chmod it back.
    let _ = sys::chmod(f, 0);
    t(eacces(sys::open(f, sys::O_RDONLY)));
    t(sys::chmod(f, 0o644).is_ok() && sys::open(f, sys::O_RDONLY).map(sys::close).is_ok());
    // 6: no exec without x; 7: chown to root refused.
    t(matches!(sys::execve(f, &[f], &[]), sys::Errno(13)));
    t(sys::chown(f, 0, u32::MAX).is_err());
    // 8: a directory without x can't be searched.
    let d = "/tmp/utest-dir";
    let _ = sys::mkdir(d, 0o755);
    let _ = sys::open_mode("/tmp/utest-dir/x", sys::O_WRONLY | sys::O_CREAT, 0o644).map(sys::close);
    let _ = sys::chmod(d, 0o600);
    t(matches!(
        sys::stat_path("/tmp/utest-dir/x", true),
        Err(sys::Errno(13))
    ));
    let _ = sys::chmod(d, 0o755);
    let _ = sys::unlink("/tmp/utest-dir/x");
    let _ = sys::rmdir(d);
    // 9: sticky /tmp: root's file there can't be removed by us; 10: but by root.
    let rf = "/tmp/utest-rootfile";
    let made = as_admin(&["/Commands/files", "create", rf]) == 0;
    t(made && matches!(sys::unlink(rf), Err(sys::Errno(1))));
    t(as_admin(&["/Commands/files", "delete", "--now", rf]) == 0
        && sys::stat_path(rf, true).is_err());
    // 11: both must allow: a sandbox grant of /System/Settings doesn't open /System/Settings/shadow, and a file
    // the permissions allow is still out of reach outside the grant.
    let pid = sys::fork().unwrap();
    if pid == 0 {
        sys::sandbox("fs=/System/Settings:r net=none").expect("sandbox");
        let shadow = matches!(
            sys::open("/System/Settings/shadow", sys::O_RDONLY),
            Err(sys::Errno(13))
        );
        let passwd = sys::read_file("/System/Settings/passwd").is_ok();
        let outside = sys::read_file(f).is_err();
        sys::exit(if shadow && passwd && outside { 0 } else { 1 });
    }
    let (_, st) = sys::wait4(pid as i32, 0).unwrap();
    t(st == 0);
    let _ = sys::unlink(f);
    // 12-16: /proc as a non-root user. Anyone's status (with the Caps line) and the audit
    // log are readable; another user's memory map isn't, our own is; no signals to root's
    // processes (init), signal 0 included.
    let st1 = sys::read_file("/proc/1/status").unwrap_or_default();
    t(String::from_utf8_lossy(&st1).contains("Caps:\tall"));
    t(sys::read_file("/proc/audit").is_ok());
    t(matches!(
        sys::read_file("/proc/1/maps"),
        Err(sys::Errno(13))
    ));
    t(sys::read_file("/proc/self/maps").is_ok_and(|m| !m.is_empty()));
    t(matches!(sys::kill(1, 0), Err(sys::Errno(1))));
    let want = (1u32 << n) - 1;
    check(
        "permissions",
        bits == want,
        &alloc::format!("checks {bits:#b} (want {want:#b}) as uid {me}"),
    )
}

/// User-space W^X: the heap and the stack don't execute, text isn't writable, and
/// mmap / mprotect refuse PROT_WRITE | PROT_EXEC. Each probe runs in a child, which must
/// die of SIGSEGV.
fn t_wx() -> bool {
    const RET: u32 = 0xd65f_03c0;
    fn segv(f: fn()) -> bool {
        let pid = sys::fork().unwrap();
        if pid == 0 {
            f();
            sys::exit(0); // it ran: W^X is broken
        }
        let (_, st) = sys::wait4(pid as i32, 0).unwrap();
        st & 0x7f == sys::SIGSEGV
    }
    fn jump(addr: usize) {
        // SAFETY: deliberately not; a `ret` if it runs at all.
        let f: extern "C" fn() = unsafe { core::mem::transmute(addr) };
        f();
    }
    let heap = segv(|| {
        let code = alloc::boxed::Box::new([RET; 4]);
        jump(code.as_ptr() as usize);
    });
    let stack = segv(|| {
        let code = [RET; 4];
        jump(core::hint::black_box(&code).as_ptr() as usize);
    });
    let text = segv(|| {
        let p = t_wx as *const () as *mut u8;
        // SAFETY: deliberately not: must fault.
        unsafe { core::ptr::write_volatile(p, 0) };
    });
    let mut page = alloc::vec![0u8; 8192];
    let addr = (page.as_mut_ptr() as usize + 4095) & !4095;
    let mprot = sys::mprotect(addr, 4096, 7).is_err();
    let mmap_rwx = sys::mmap_prot(4096, 7).is_err();
    check(
        "W^X",
        heap && stack && text && mprot && mmap_rwx,
        &alloc::format!(
            "SIGSEGV: jump into heap {heap}, into stack {stack}, write to text {text}; \
             RWX refused: mprotect {mprot}, mmap {mmap_rwx}"
        ),
    )
}

/// AF_UNIX: socketpair, a path-named listener with peer credentials, fd passing across
/// processes both ways, SCM_CREDENTIALS rules, abstract datagrams, socket file permission.
fn t_unix() -> bool {
    use rt::unix::{self, Name};
    let mut bits = 0u32;
    let mut n = 0;
    let mut t = |ok: bool| {
        if ok {
            bits |= 1 << n;
        }
        n += 1;
    };
    let me = sys::getpid();
    let uid = sys::geteuid();
    // 0: socketpair round trip.
    let (a, b) = unix::socketpair(unix::SOCK_STREAM).unwrap();
    let _ = sys::write_all(a, b"ping");
    let mut buf = [0u8; 64];
    t(sys::read(b, &mut buf).ok() == Some(4) && &buf[..4] == b"ping");
    // 1: fd passing, parent -> child -> parent: the child gets our pipe's read end, reads
    // what we wrote into it, and sends us back the read end of a pipe of its own.
    let (pr, pw) = sys::pipe().unwrap();
    let _ = sys::write_all(pw, b"through the pipe");
    let child = sys::fork().unwrap();
    if child == 0 {
        let _ = sys::close(a);
        let r = unix::recv_with(b, &mut buf, 4).unwrap();
        let mut got = [0u8; 64];
        let k = r
            .fds
            .first()
            .map_or(0, |&fd| sys::read(fd, &mut got).unwrap_or(0));
        let ok = r.n == 4 && &buf[..4] == b"fd!!" && &got[..k] == b"through the pipe";
        let (cr, cw) = sys::pipe().unwrap();
        let _ = sys::write_all(cw, b"from the child");
        let _ = unix::send_with(b, if ok { b"ok" } else { b"no" }, &[cr], None);
        sys::exit(0);
    }
    let _ = unix::send_with(a, b"fd!!", &[pr], None);
    let _ = sys::close(pr); // the child has its own reference to it now
    let r = unix::recv_with(a, &mut buf, 4).unwrap_or_default();
    let mut got = [0u8; 64];
    let k = r
        .fds
        .first()
        .map_or(0, |&fd| sys::read(fd, &mut got).unwrap_or(0));
    let _ = sys::wait4(child as i32, 0);
    t(r.n == 2 && &buf[..2] == b"ok" && &got[..k] == b"from the child");
    for fd in r.fds.iter().copied().chain([a, b, pw]) {
        let _ = sys::close(fd);
    }
    // 2-3: a listener on a path; the client's and the server's view of each other.
    let path = "/tmp/utest-unix.sock";
    let _ = sys::unlink(path);
    let l = unix::socket(unix::SOCK_STREAM).unwrap();
    t(unix::bind(l, &Name::Path(path)).is_ok()
        && unix::listen(l, 8).is_ok()
        && sys::stat_path(path, false).is_ok_and(|s| s.type_char() == 's'));
    let child = sys::fork().unwrap();
    if child == 0 {
        let c = unix::socket(unix::SOCK_STREAM).unwrap();
        let ok = unix::connect(c, &Name::Path(path)).is_ok()
            && unix::peer_cred(c).is_ok_and(|p| p.pid == me && p.uid == uid);
        let _ = sys::write_all(c, if ok { b"y" } else { b"n" });
        sys::exit(0);
    }
    let s = unix::accept(l, 0).unwrap();
    let pc = unix::peer_cred(s).unwrap_or_default();
    let k = sys::read(s, &mut buf).unwrap_or(0);
    let _ = sys::wait4(child as i32, 0);
    t(pc.pid == child && pc.uid == uid && k == 1 && buf[0] == b'y');
    let _ = sys::close(s);
    // 4: SO_PASSCRED: an honest record arrives; 5: a fake one is refused (unless root).
    let (a, b) = unix::socketpair(unix::SOCK_DGRAM).unwrap();
    let _ = unix::set_passcred(b, true);
    let _ = sys::write_all(a, b"hi");
    let r = unix::recv_with(b, &mut buf, 0).unwrap_or_default();
    t(r.cred
        .is_some_and(|c| c.pid == me && c.uid == sys::getuid()));
    let fake = unix::Cred {
        pid: 1,
        uid: 0,
        gid: 0,
    };
    let refused = matches!(
        unix::send_with(a, b"x", &[], Some(fake)),
        Err(sys::Errno(1))
    );
    t(refused || uid == 0);
    let _ = sys::close(a);
    let _ = sys::close(b);
    // 6: abstract datagram names.
    let srv = unix::socket(unix::SOCK_DGRAM).unwrap();
    let cli = unix::socket(unix::SOCK_DGRAM).unwrap();
    let name = alloc::format!("utest-{me}");
    let _ = unix::bind(srv, &Name::Abstract(name.as_bytes()));
    let _ = unix::sendto(cli, b"dgram", &Name::Abstract(name.as_bytes()));
    let k = sys::read(srv, &mut buf).unwrap_or(0);
    t(k == 5 && &buf[..5] == b"dgram");
    let _ = sys::close(srv);
    let _ = sys::close(cli);
    // 7: the socket file's permissions apply: owner-only (0700) keeps others out.
    let ok7 = if uid == 0 {
        let _ = sys::chmod(path, 0o700);
        let child = sys::fork().unwrap();
        if child == 0 {
            let _ = sys::setgroups(&[20]);
            let _ = sys::setgid(20);
            let _ = sys::setuid(501);
            let c = unix::socket(unix::SOCK_STREAM).unwrap();
            let denied = matches!(unix::connect(c, &Name::Path(path)), Err(sys::Errno(13)));
            sys::exit(if denied { 0 } else { 1 });
        }
        let (_, st) = sys::wait4(child as i32, 0).unwrap();
        st == 0
    } else {
        true // covered when utest runs as root
    };
    t(ok7);
    let _ = sys::close(l);
    let _ = sys::unlink(path);
    let want = (1u32 << n) - 1;
    check(
        "AF_UNIX",
        bits == want,
        &alloc::format!("checks {bits:#b} (want {want:#b}); peer cred {pc:?}"),
    )
}

/// epoll: level vs edge-triggered vs one-shot on a pipe, EPOLLHUP, select, and an epoll
/// server (one thread, non-blocking) answering 100 AF_UNIX clients at once.
fn t_epoll() -> bool {
    use rt::epoll::{self as ep, EPOLLET, EPOLLHUP, EPOLLIN, EPOLLONESHOT};
    use rt::unix::{self, Name};
    let mut bits = 0u32;
    let mut n = 0;
    let mut t = |ok: bool| {
        if ok {
            bits |= 1 << n;
        }
        n += 1;
    };
    let mut buf = [0u8; 256];
    let e = ep::create().unwrap();
    // Level-triggered: reported while data is there.
    let (r, w) = sys::pipe().unwrap();
    ep::add(e, r, EPOLLIN, 7).unwrap();
    t(ep::wait(e, 8, 0).unwrap().is_empty());
    let _ = sys::write_all(w, b"0123456789");
    let a = ep::wait(e, 8, 100).unwrap();
    let b = ep::wait(e, 8, 0).unwrap();
    t(a.len() == 1 && a[0].data == 7 && a[0].events & EPOLLIN != 0 && b.len() == 1); // 1
    // Edge-triggered: once per edge. No new data -> nothing, even after a partial read.
    ep::modify(e, r, EPOLLIN | EPOLLET, 8).unwrap();
    let first = ep::wait(e, 8, 0).unwrap();
    let again = ep::wait(e, 8, 0).unwrap();
    let _ = sys::read(r, &mut buf[..5]);
    let after_partial = ep::wait(e, 8, 0).unwrap();
    let _ = sys::write_all(w, b"more");
    let new_data = ep::wait(e, 8, 100).unwrap();
    t(first.len() == 1 && again.is_empty() && after_partial.is_empty() && new_data.len() == 1); // 2
    let _ = sys::read(r, &mut buf);
    t(ep::wait(e, 8, 0).unwrap().is_empty()); // 3: drained
    // One-shot: one report until re-armed.
    ep::modify(e, r, EPOLLIN | EPOLLONESHOT, 9).unwrap();
    let _ = sys::write_all(w, b"x");
    let one = ep::wait(e, 8, 100).unwrap();
    let none = ep::wait(e, 8, 0).unwrap();
    ep::modify(e, r, EPOLLIN | EPOLLONESHOT, 9).unwrap();
    let rearmed = ep::wait(e, 8, 0).unwrap();
    t(one.len() == 1 && none.is_empty() && rearmed.len() == 1); // 4
    // select: readable and writable bits; then EPOLLHUP once the writer is gone.
    let mut rs = [1u64 << r];
    let mut ws = [1u64 << w];
    let k = ep::select(64, Some(&mut rs), Some(&mut ws), None, Some(0)).unwrap_or(0);
    t(k == 2 && rs[0] == 1 << r && ws[0] == 1 << w); // 5
    let _ = sys::read(r, &mut buf);
    let _ = sys::close(w);
    ep::modify(e, r, EPOLLIN, 10).unwrap();
    let h = ep::wait(e, 8, 100).unwrap();
    t(h.len() == 1 && h[0].events & EPOLLHUP != 0); // 6
    let _ = sys::close(r);
    // A closed fd's entry disappears by itself.
    t(ep::wait(e, 8, 0).unwrap().is_empty()); // 7
    let _ = sys::close(e);

    // The server: listener + 100 clients in another process.
    const CLIENTS: usize = 100;
    let name = alloc::format!("utest-epoll-{}", sys::getpid());
    let l = unix::socket(unix::SOCK_STREAM | unix::SOCK_NONBLOCK).unwrap();
    let _ = unix::bind(l, &Name::Abstract(name.as_bytes()));
    let _ = unix::listen(l, 128);
    let child = sys::fork().unwrap();
    if child == 0 {
        let mut socks = Vec::new();
        for i in 0..CLIENTS {
            let c = unix::socket(unix::SOCK_STREAM).unwrap();
            if unix::connect(c, &Name::Abstract(name.as_bytes())).is_err() {
                sys::exit(2);
            }
            let _ = sys::write_all(c, alloc::format!("hello {i}").as_bytes());
            socks.push(c);
        }
        let mut good = 0;
        for (i, &c) in socks.iter().enumerate() {
            let mut b = [0u8; 64];
            let k = sys::read(c, &mut b).unwrap_or(0);
            if &b[..k] == alloc::format!("ok {i}").as_bytes() {
                good += 1;
            }
        }
        sys::exit(if good == CLIENTS { 0 } else { 1 });
    }
    let e = ep::create().unwrap();
    ep::add(e, l, EPOLLIN, u64::MAX).unwrap();
    let (mut replied, mut accepted, mut wakeups) = (0, 0, 0);
    let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
    while replied < CLIENTS && sys::clock_ns(sys::CLOCK_MONOTONIC) - t0 < 20_000_000_000 {
        let evs = ep::wait(e, 64, 1000).unwrap_or_default();
        wakeups += 1;
        for ev in evs {
            if ev.data == u64::MAX {
                // Edge or not, drain the backlog.
                while let Ok(c) = unix::accept(l, unix::SOCK_NONBLOCK) {
                    accepted += 1;
                    let _ = ep::add(e, c, EPOLLIN | EPOLLET, c as u64);
                }
                continue;
            }
            let c = ev.data as i32;
            // Edge-triggered: read until EAGAIN.
            let mut msg = Vec::new();
            loop {
                match sys::read(c, &mut buf) {
                    Ok(0) => break,
                    Ok(k) => msg.extend_from_slice(&buf[..k]),
                    Err(_) => break,
                }
            }
            if let Some(i) = core::str::from_utf8(&msg)
                .ok()
                .and_then(|m| m.strip_prefix("hello "))
                .and_then(|x| x.parse::<usize>().ok())
            {
                let _ = sys::write_all(c, alloc::format!("ok {i}").as_bytes());
                replied += 1;
            }
        }
    }
    let (_, st) = sys::wait4(child as i32, 0).unwrap();
    let us = (sys::clock_ns(sys::CLOCK_MONOTONIC) - t0) / 1000;
    t(replied == CLIENTS && accepted == CLIENTS && st == 0); // 8
    let _ = sys::close(e);
    let _ = sys::close(l);
    let want = (1u32 << n) - 1;
    check(
        "epoll",
        bits == want,
        &alloc::format!(
            "checks {bits:#b} (want {want:#b}); server: {accepted} accepted, {replied} \
             answered in {us} us, {wakeups} epoll_wait calls, client {}",
            sys::describe_status(st)
        ),
    )
}

const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_SECURE: u64 = 23;

/// `utest auxv-print`: our auxv ids, for t_auxv's set-user-ID copy.
fn auxv_print(args: &rt::Args) -> i32 {
    let g = |t| args.aux(t).map_or(-1, |v| v as i64);
    println!(
        "AUXV {} {} {} {} {}",
        g(AT_UID),
        g(AT_EUID),
        g(AT_GID),
        g(AT_EGID),
        g(AT_SECURE)
    );
    0
}

/// The auxv carries the real ids; AT_SECURE is set for a set-user-ID exec.
fn t_auxv(args: &rt::Args) -> bool {
    let g = |t| args.aux(t).unwrap_or(u64::MAX);
    let mut ok = g(AT_UID) == sys::getuid() as u64
        && g(AT_EUID) == sys::geteuid() as u64
        && g(AT_GID) == sys::getgid() as u64
        && g(AT_EGID) == sys::getegid() as u64
        && g(AT_SECURE) == (sys::getuid() != sys::geteuid()) as u64;
    let mut detail = alloc::format!(
        "self: uid {} euid {} gid {} egid {} secure {}",
        g(AT_UID),
        g(AT_EUID),
        g(AT_GID),
        g(AT_EGID),
        g(AT_SECURE)
    );
    if sys::geteuid() == 0 {
        // A set-user-ID root copy of utest, run by milk: euid 0, uid 501, AT_SECURE 1.
        let copy = "/tmp/utest-suid";
        let made = sys::read_file("/System/Developer/utest")
            .ok()
            .and_then(|data| {
                let fd = sys::open_mode(copy, sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC, 0o755)
                    .ok()?;
                let r = sys::write_all(fd, &data);
                let _ = sys::close(fd);
                r.ok()?;
                sys::chmod(copy, 0o4755).ok()
            });
        let (r, w) = sys::pipe().unwrap();
        let pid = sys::fork().unwrap();
        if pid == 0 {
            let _ = sys::dup2(w, 1);
            let _ = sys::setgroups(&[20]);
            let _ = sys::setgid(20);
            let _ = sys::setuid(501);
            let _ = sys::execve(copy, &[copy, "auxv-print"], &[]);
            sys::exit(127);
        }
        let _ = sys::close(w);
        let mut out = Vec::new();
        let mut b = [0u8; 128];
        while let Ok(n @ 1..) = sys::read(r, &mut b) {
            out.extend_from_slice(&b[..n]);
        }
        let _ = sys::close(r);
        let _ = sys::wait4(pid as i32, 0);
        let _ = sys::unlink(copy);
        let line = String::from(String::from_utf8_lossy(&out).trim());
        ok &= made.is_some() && line == "AUXV 501 0 20 20 1";
        detail += &alloc::format!("; set-user-ID copy run by milk: {line:?}");
    }
    check("auxv ids", ok, &detail)
}

/// One request to milkd over /System/Services/Sockets/milkd.sock; the reply up to and including ok / error.
fn milkd(req: &str) -> String {
    use rt::unix::{self, Name};
    let Ok(fd) = unix::socket(unix::SOCK_STREAM) else {
        return String::from("error\tsocket");
    };
    if unix::connect(fd, &Name::Path("/System/Services/Sockets/milkd.sock")).is_err() {
        let _ = sys::close(fd);
        return String::from("error\tconnect");
    }
    let _ = sys::write_all(fd, alloc::format!("{req}\n").as_bytes());
    let mut out = Vec::new();
    let mut b = [0u8; 1024];
    loop {
        match sys::read(fd, &mut b) {
            Ok(0) | Err(_) => break,
            Ok(n) => out.extend_from_slice(&b[..n]),
        }
        let t = String::from_utf8_lossy(&out);
        if t.ends_with("ok\n") || t.lines().last().is_some_and(|l| l.starts_with("error")) {
            break;
        }
    }
    let _ = sys::close(fd);
    String::from_utf8_lossy(&out).into_owned()
}

/// milkd: a crashing service is restarted with doubling backoff, a missing / failed
/// dependency is reported, start / stop over the socket work for root and admins and are
/// refused for everyone else. Needs root (it writes /System/Settings/milk/services).
fn t_svc() -> bool {
    if sys::geteuid() != 0 {
        return check("services", true, "skipped: needs root");
    }
    let write = |name: &str, body: &str| {
        let p = alloc::format!("/System/Settings/milk/services/{name}.service");
        if let Ok(fd) = sys::open_mode(&p, sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC, 0o644) {
            let _ = sys::write_all(fd, body.as_bytes());
            let _ = sys::close(fd);
        }
    };
    write(
        "ut-crashy",
        "description = crashes at once\nexec = /Commands/false\nrestart = always\n",
    );
    write("ut-oneshot", "exec = /Commands/false\nrestart = never\n");
    write(
        "ut-needs-missing",
        "exec = /Commands/pause 60\nneeds = ut-no-such-unit\n",
    );
    write(
        "ut-needs-dead",
        "exec = /Commands/pause 60\nneeds = ut-oneshot\nafter = ut-oneshot\n",
    );
    write(
        "ut-sleeper",
        "description = sleeps\nexec = /Commands/pause 600\nuser = milk\nrestart = on-failure\n",
    );
    let reload = milkd("reload");
    let status = |u: &str| milkd(&alloc::format!("status {u}"));
    // Let the crasher go round a few times (100 + 200 + 400 + 800 ms ...).
    let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
    let mut st = String::new();
    while sys::clock_ns(sys::CLOCK_MONOTONIC) - t0 < 6_000_000_000 {
        st = status("ut-crashy");
        let restarts = st
            .split('\t')
            .nth(3)
            .and_then(|x| x.parse::<u32>().ok())
            .unwrap_or(0);
        if restarts >= 4 {
            break;
        }
        sys::sleep_ms(100);
    }
    // The journal has the delays: they double.
    let delays: Vec<u64> = sys::read_file("/dev/journal")
        .map(|j| {
            String::from_utf8_lossy(&j)
                .lines()
                .filter(|l| l.split('\t').nth(6) == Some("ut-crashy"))
                .filter_map(|l| {
                    let m = l.split('\t').nth(8)?;
                    let i = m.find(" in ")?;
                    m[i + 4..].strip_suffix(" ms")?.parse().ok()
                })
                .collect()
        })
        .unwrap_or_default();
    let backoff = delays.len() >= 4
        && delays.windows(2).take(3).all(|w| w[1] == 2 * w[0])
        && delays[0] == 100;
    // Dependencies. ut-needs-dead may start as soon as ut-oneshot is spawned (`after` is
    // ordering on start, as in systemd) and is then stopped when the oneshot fails: give
    // it up to 3 s to settle (stopping -> failed) before judging.
    let missing = status("ut-needs-missing");
    let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
    let mut dead = status("ut-needs-dead");
    while !dead.contains("dependency failed: ut-oneshot")
        && sys::clock_ns(sys::CLOCK_MONOTONIC) - t0 < 3_000_000_000
    {
        sys::sleep_ms(100);
        dead = status("ut-needs-dead");
    }
    let deps = missing.contains("dependency failed: ut-no-such-unit")
        && dead.contains("dependency failed: ut-oneshot");
    if !deps {
        println!("  needs-missing: {missing:?}\n  needs-dead: {dead:?}");
    }
    // Start / stop: root yes; milk (admin) yes; nobody refused.
    sys::sleep_ms(300);
    let up =
        status("ut-sleeper").contains("\trunning\t") && status("ut-sleeper").contains("\tmilk\t");
    let as_user = |uid: u32, gid: u32, req: &str| -> bool {
        let pid = sys::fork().unwrap();
        if pid == 0 {
            let _ = sys::setgroups(&rt::users::groups_of(&rt::users::user_name(uid), gid));
            let _ = sys::setgid(gid);
            let _ = sys::setuid(uid);
            let r = milkd(req);
            sys::exit(if r.ends_with("ok\n") { 0 } else { 1 });
        }
        sys::wait4(pid as i32, 0).is_ok_and(|(_, st)| st == 0)
    };
    let nobody_stop = as_user(65534, 65534, "stop ut-sleeper");
    let nobody_status = as_user(65534, 65534, "status ut-sleeper");
    let still_up = status("ut-sleeper").contains("\trunning\t");
    let milk_stop = as_user(501, 20, "stop ut-sleeper");
    sys::sleep_ms(500);
    let stopped = status("ut-sleeper").contains("\tinactive\t");
    let root_start = milkd("start ut-sleeper").ends_with("ok\n");
    sys::sleep_ms(300);
    let restarted = status("ut-sleeper").contains("\trunning\t");
    let control = !nobody_stop
        && nobody_status
        && still_up
        && milk_stop
        && stopped
        && root_start
        && restarted;
    // Clean up: stop the test units, drop their files.
    for u in [
        "ut-crashy",
        "ut-sleeper",
        "ut-needs-missing",
        "ut-needs-dead",
        "ut-oneshot",
    ] {
        let _ = milkd(&alloc::format!("stop {u}"));
        let _ = sys::unlink(&alloc::format!(
            "/System/Settings/milk/services/{u}.service"
        ));
    }
    let ok = reload.ends_with("ok\n") && backoff && deps && up && control;
    check(
        "services",
        ok,
        &alloc::format!(
            "crasher delays {delays:?} ({}); deps {deps}; sleeper up as milk {up}; nobody stop \
             refused {}, nobody status {nobody_status}, milk (admin) stop {milk_stop}, root \
             start {root_start}; last crasher status {:?}",
            if backoff { "doubling" } else { "NOT doubling" },
            !nobody_stop,
            st.lines().next().unwrap_or("")
        ),
    )
}

/// Admin authorisation tickets: destructive milkfs controls need one (not just group
/// admin); wrong password refused; a ticket works, isn't inherited by fork, survives exec
/// (`admin ticket`), shows in /proc, can be revoked; 3 wrong passwords lock the user out.
fn t_auth() -> bool {
    if sys::geteuid() == 0 {
        return as_milk("auth", "authorisation");
    }
    let ctl = "/System/Kernel/fs/milkfs/delete";
    if sys::stat_path(ctl, true).is_err() {
        return check("authorisation", true, "skipped: no milkfs volume");
    }
    let del = || -> Option<i32> {
        let fd = sys::open(ctl, sys::O_WRONLY).ok()?;
        let r = sys::write(fd, b"no-such-snapshot-utest");
        let _ = sys::close(fd);
        Some(match r {
            Ok(_) => 0,
            Err(e) => e.0,
        })
    };
    let mut bits = 0u32;
    let mut n = 0;
    let mut t = |ok: bool| {
        if ok {
            bits |= 1 << n;
        }
        n += 1;
    };
    t(del() == Some(1)); // 0: admin group alone: EPERM
    t(matches!(
        sys::authorize_password("wrong", ""),
        Err(sys::Errno(13))
    )); // 1
    let granted = sys::authorize_password("milk", "");
    t(granted.is_ok_and(|ms| ms >= 299_000)); // 2: 5 minutes
    t(del().is_some_and(|e| e != 1)); // 3: past the permission check (no such snapshot)
    let status = sys::read_file("/proc/self/status").unwrap_or_default();
    t(String::from_utf8_lossy(&status).contains("Auth:\tadmin ticket from uid 501")); // 4
    let pid = sys::fork().unwrap();
    if pid == 0 {
        sys::exit(if sys::auth_left() == 0 && del() == Some(1) {
            0
        } else {
            1
        });
    }
    let (_, st) = sys::wait4(pid as i32, 0).unwrap();
    t(st == 0); // 5: not inherited
    sys::auth_revoke();
    t(sys::auth_left() == 0 && del() == Some(1)); // 6: revoked
    // 7: `admin ticket` keeps the ticket across exec (`admin status` reports it).
    let (r, w) = sys::pipe().unwrap();
    let (r2, w2) = sys::pipe().unwrap();
    let pid = sys::fork().unwrap();
    if pid == 0 {
        let _ = sys::dup2(r, 0);
        let _ = sys::dup2(w2, 1);
        let _ = sys::execve(
            "/Commands/admin",
            &["admin", "ticket", "--stdin", "/Commands/admin", "status"],
            &[],
        );
        sys::exit(127);
    }
    let _ = sys::close(r);
    let _ = sys::close(w2);
    let _ = sys::write_all(w, b"milk\n");
    let _ = sys::close(w);
    let mut out = Vec::new();
    let mut b = [0u8; 256];
    while let Ok(k @ 1..) = sys::read(r2, &mut b) {
        out.extend_from_slice(&b[..k]);
    }
    let _ = sys::close(r2);
    let _ = sys::wait4(pid as i32, 0);
    let out = String::from_utf8_lossy(&out);
    t(out.contains("ticket") && out.contains("s left (this process only)")); // 7
    // 8: rate limit: three wrong in a row, then even the right one is refused for a while.
    for _ in 0..3 {
        let _ = sys::authorize_password("wrong", "");
    }
    let locked = sys::authorize_password("milk", "");
    t(matches!(locked, Err(sys::Errno(11)))); // 8
    let audit = sys::read_file("/proc/audit").unwrap_or_default();
    let audit = String::from_utf8_lossy(&audit);
    t(audit.contains("auth.grant")
        && audit.contains("auth.deny")
        && audit.contains("auth.required")); // 9
    let want = (1u32 << n) - 1;
    check(
        "authorisation",
        bits == want,
        &alloc::format!(
            "checks {bits:#b} (want {want:#b}); admin status said {:?}",
            out.lines()
                .find(|l| l.starts_with("ticket"))
                .unwrap_or("")
                .trim()
        ),
    )
}

/// User-space driver interface (docs/drivers.md), on the virtio-rng device: rngd is
/// stopped for the test and started again after.
fn t_driver() -> bool {
    use udrv::{Device, Queue};
    if sys::geteuid() != 0 {
        return check("driver", true, "skipped: needs root");
    }
    let drivers = || {
        String::from_utf8_lossy(&sys::read_file("/proc/drivers").unwrap_or_default()).into_owned()
    };
    if !drivers().contains("rng policy=user") {
        return check("driver", true, "skipped: no virtio-rng, or rng=kernel");
    }
    let owner_is = |who: &str| {
        let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
        loop {
            let d = drivers();
            if d.lines().any(|l| l.starts_with("rng ") && l.contains(who)) {
                return true;
            }
            if sys::clock_ns(sys::CLOCK_MONOTONIC) - t0 > 3_000_000_000 {
                return false;
            }
            sys::sleep_ms(50);
        }
    };
    let _ = milkd("stop rngd");
    let mut bits = 0u32;
    let mut n = 0;
    let mut t = |ok: bool| {
        if ok {
            bits |= 1 << n;
        }
        n += 1;
    };
    t(owner_is("owner=kernel")); // 0: rngd let go, the kernel driver has it
    let fd = sys::driver_claim(udrv::VIRTIO_RNG);
    t(fd.is_ok()); // 1
    let Ok(fd) = fd else {
        let _ = milkd("start rngd");
        return check("driver", false, &alloc::format!("claim: {fd:?}"));
    };
    let dev = Device { fd };
    t(owner_is("/System/Developer/utest")); // 2
    t(matches!(
        sys::driver_claim(udrv::VIRTIO_RNG),
        Err(sys::Errno(16))
    )); // 3: EBUSY
    // 4: registers: our window only, 32-bit below the config space.
    t(dev.r32(0) == 0x7472_6976
        && matches!(sys::driver_read(fd, 0x200, 4), Err(sys::Errno(22)))
        && matches!(sys::driver_read(fd, 0x70, 2), Err(sys::Errno(22))));
    // 5: a queue pointed at kernel memory is refused at QueueReady.
    let _ = dev.init(0);
    let bad = dev.w32(udrv::QUEUE_SEL, 0).is_ok()
        && dev.w32(udrv::QUEUE_NUM, 8).is_ok()
        && dev.w32(udrv::QUEUE_DESC_LOW, 0x4008_0000).is_ok()
        && dev.w32(udrv::QUEUE_DESC_HIGH, 0).is_ok()
        && dev.w32(udrv::QUEUE_READY, 1).is_err();
    t(bad);
    // A real queue in our DMA memory.
    let mut q = Queue::new(&dev, 8).expect("queue");
    let set = dev.setup_queue(0, &q).and_then(|_| dev.driver_ok());
    let _ = sys::driver_arm(fd);
    // 6: a descriptor pointing outside our buffers is refused at notify (planted in the
    // table only: the device never sees it).
    q.set_desc(7, 0x4008_0000, 64);
    t(set.is_ok() && dev.notify(0).is_err());
    q.set_desc(7, 0, 0);
    // 7: a good request goes through, and the interrupt arrives on the fd.
    let (bva, bpa) = dev.dma(4096).expect("dma");
    let head = q.add(&[], &[(bpa, 64)]).expect("add");
    let sent = dev.notify(0).is_ok();
    let mut got = 0u32;
    let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
    while sent && sys::clock_ns(sys::CLOCK_MONOTONIC) - t0 < 2_000_000_000 {
        if let Some((h, len)) = q.pop_used() {
            q.free(h);
            if h == head {
                got = len;
                break;
            }
        }
        let mut pf = [rt::net::PollFd {
            fd,
            events: rt::net::POLLIN,
            revents: 0,
        }];
        if matches!(rt::net::poll(&mut pf, Some(500)), Ok(1..)) {
            let _ = dev.wait();
            dev.ack_and_arm();
        }
    }
    // SAFETY: our DMA buffer.
    let bytes = unsafe { core::slice::from_raw_parts(bva as *const u8, 64) };
    t(got == 64 && bytes.iter().any(|&b| b != 0));
    // 8: a forked child doesn't get the DMA page (touching it is a segfault).
    let pid = sys::fork().unwrap();
    if pid == 0 {
        // SAFETY: deliberately touching an address that must not be mapped here.
        let v = unsafe { core::ptr::read_volatile(bva as *const u8) };
        sys::exit(if v == 0xff { 0 } else { 1 }); // not reached: the read faults
    }
    let (_, st) = sys::wait4(pid as i32, 0).unwrap();
    t(st & 0x7f == 11);
    // 9: a sandboxed (driver=0) or non-root process may not claim.
    let pid = sys::fork().unwrap();
    if pid == 0 {
        let a = sys::sandbox("driver=0").is_ok()
            && matches!(sys::driver_claim(udrv::VIRTIO_RNG), Err(sys::Errno(1)));
        sys::exit(if a { 0 } else { 1 });
    }
    let (_, st1) = sys::wait4(pid as i32, 0).unwrap();
    let pid = sys::fork().unwrap();
    if pid == 0 {
        let _ = sys::setresuid(65534, 65534, 65534);
        sys::exit(
            if matches!(sys::driver_claim(udrv::VIRTIO_RNG), Err(sys::Errno(1))) {
                0
            } else {
                1
            },
        );
    }
    let (_, st2) = sys::wait4(pid as i32, 0).unwrap();
    t(st1 == 0 && st2 == 0);
    // 10: closing the fd hands the device back to the kernel driver; the refusals were audited.
    let _ = sys::close(fd);
    t(owner_is("owner=kernel"));
    let audit =
        String::from_utf8_lossy(&sys::read_file("/proc/audit").unwrap_or_default()).into_owned();
    t(audit.contains("driver.claim")
        && audit.contains("driver.dma_refused")
        && audit.contains("driver.release"));
    let _ = milkd("start rngd");
    let back = owner_is("/System/Services/rngd");
    let want = (1u32 << n) - 1;
    check(
        "driver",
        bits == want && back,
        &alloc::format!("checks {bits:#b} (want {want:#b}); rngd back {back}"),
    )
}

fn main(args: &rt::Args) -> i32 {
    let what = args.get(1).unwrap_or("all");
    let mut ok = true;
    if what == "pipe" || what == "all" {
        ok &= t_pipe();
    }
    if what == "tlb" || what == "all" {
        ok &= t_tlb();
    }
    if what == "cowsmp" || what == "all" {
        ok &= t_cowsmp();
    }
    if what == "par" {
        let n = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(4);
        let ms = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(500);
        ok &= t_par(n, ms);
    }
    if what == "csw" {
        let rounds = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(5000);
        ok &= t_csw(rounds);
    }
    if what == "chroot" || what == "all" {
        ok &= t_chroot();
    }
    if what == "sandbox" || what == "all" {
        ok &= t_sandbox();
    }
    if what == "threads" || what == "all" {
        ok &= t_threads();
    }
    if what == "cow" || what == "all" {
        ok &= t_cow();
    }
    if what == "proc" || what == "all" {
        ok &= t_proc();
    }
    if what == "linux" || what == "all" {
        ok &= t_linux();
    }
    if what == "perms" || what == "all" {
        ok &= t_perms();
    }
    if what == "wx" || what == "all" {
        ok &= t_wx();
    }
    if what == "unix" || what == "all" {
        ok &= t_unix();
    }
    if what == "epoll" || what == "all" {
        ok &= t_epoll();
    }
    if what == "auxv-print" {
        return auxv_print(args);
    }
    if what == "auxv" || what == "all" {
        ok &= t_auxv(args);
    }
    if what == "driver" || what == "all" {
        ok &= t_driver();
    }
    if what == "svc" || what == "all" {
        ok &= t_svc();
    }
    if what == "auth" || what == "all" {
        ok &= t_auth();
    }
    if what == "creds" || what == "all" {
        ok &= t_creds();
    }
    if what == "kaddr" || what == "all" {
        ok &= t_kaddr();
    }
    if what == "signal" || what == "all" {
        ok &= t_signal();
    }
    println!("utest: {}", if ok { "ALL PASS" } else { "SOME FAILED" });
    if ok { 0 } else { 1 }
}
