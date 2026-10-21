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
    let (out, st) = capture(&["/bin/hello", "piped"]);
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
        && cmdline.starts_with(b"utest\0")
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

fn t_par(n: usize, units: u64) -> bool {
    let cpus = sys::read_file("/proc/cpuinfo")
        .map(|c| String::from_utf8_lossy(&c).matches("processor").count())
        .unwrap_or(1);
    run_workers(1, units / 10 + 1); // warm-up (page cache, COW paths)
    let t1 = run_workers(1, units).min(run_workers(1, units));
    let tn = run_workers(n, units).min(run_workers(n, units));
    let speedup = (n as f64 * t1 as f64) / tn.max(1) as f64;
    check(
        "parallel speedup",
        true,
        &alloc::format!(
            "{cpus} CPUs: 1 job {t1} ms, {n} jobs {tn} ms -> speedup {speedup:.2}x (ideal {:.0}x)",
            (n.min(cpus)) as f64
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
    if what == "chroot" || what == "all" {
        ok &= t_chroot();
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
    if what == "signal" || what == "all" {
        ok &= t_signal();
    }
    println!("utest: {}", if ok { "ALL PASS" } else { "SOME FAILED" });
    if ok { 0 } else { 1 }
}
