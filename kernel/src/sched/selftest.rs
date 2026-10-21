//! Boot self-test for threads: interleaving, sleeping, joining, mutex, semaphore,
//! preemption, and stack reuse. Panics on failure.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{Mutex, Semaphore, spawn};
use crate::{kprintln, pmm, timer};

pub fn run() {
    kprintln!("[sched-test] begin");
    interleave();
    sleep_and_join();
    mutex();
    semaphore();
    preemption();
    churn();
    timeout();
    stale_waiter();
    #[cfg(feature = "kstack-overflow-test")]
    overflow();
    kprintln!("[sched-test] PASS");
}

/// Three threads that yield after every step take turns.
fn interleave() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let hs: Vec<_> = b"ABC"
        .iter()
        .map(|c| {
            let log = log.clone();
            let c = *c;
            spawn("interleave", move || {
                for _ in 0..5 {
                    log.lock().push(c);
                    super::yield_now();
                }
            })
        })
        .collect();
    for h in hs {
        h.join();
    }
    let log = log.lock();
    let s = core::str::from_utf8(&log).unwrap();
    let switches = log.windows(2).filter(|w| w[0] != w[1]).count();
    kprintln!("[sched-test] interleave: {s} ({switches} switches)");
    if crate::percpu::count() == 1 {
        assert!(
            log.len() == 15 && switches >= 12,
            "threads did not interleave"
        );
    } else {
        // In parallel on several CPUs: no fixed order, but everyone finishes.
        for c in *b"ABC" {
            assert!(
                log.iter().filter(|&&x| x == c).count() == 5,
                "a thread lost steps"
            );
        }
    }
}

/// Sleepers wake in deadline order, not too early; join returns exit codes.
fn sleep_and_join() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let t0 = timer::now_ns();
    let hs: Vec<_> = [30u64, 10, 20]
        .into_iter()
        .map(|ms| {
            let order = order.clone();
            spawn("sleeper", move || {
                let a = timer::now_ns();
                super::sleep_ms(ms);
                let slept = (timer::now_ns() - a) / 1000;
                assert!(slept >= ms * 1000, "woke early: {slept} us < {ms} ms");
                order.lock().push((ms, slept));
                super::exit(ms as i64 + 100);
            })
        })
        .collect();
    let codes: Vec<i64> = hs.into_iter().map(|h| h.join()).collect();
    let order = order.lock();
    kprintln!(
        "[sched-test] sleep: woke in order {:?} (ms, us slept), exit codes {:?}, {} ms total",
        *order,
        codes,
        (timer::now_ns() - t0) / 1_000_000
    );
    assert!(order.iter().map(|o| o.0).eq([10, 20, 30]));
    assert!(codes == [130, 110, 120]);
}

/// Four threads hammer one counter under a Mutex, yielding while holding it.
fn mutex() {
    let m = Arc::new(Mutex::new(0u64));
    let inside = Arc::new(AtomicBool::new(false));
    let hs: Vec<_> = (0..4)
        .map(|_| {
            let (m, inside) = (m.clone(), inside.clone());
            spawn("mutex", move || {
                for i in 0..2000 {
                    let mut g = m.lock();
                    assert!(
                        !inside.swap(true, Ordering::Relaxed),
                        "two threads in the mutex"
                    );
                    *g += 1;
                    if i % 100 == 0 {
                        super::yield_now(); // others must block, not get in
                    }
                    inside.store(false, Ordering::Relaxed);
                }
            })
        })
        .collect();
    for h in hs {
        h.join();
    }
    let v = *m.lock();
    kprintln!("[sched-test] mutex: 4 x 2000 increments = {v}");
    assert!(v == 8000);
}

/// Bounded buffer with two semaphores: producer faster than consumer and vice versa.
fn semaphore() {
    const N: usize = 200;
    struct Ring {
        buf: Mutex<alloc::collections::VecDeque<usize>>,
        slots: Semaphore,
        items: Semaphore,
    }
    let r = Arc::new(Ring {
        buf: Mutex::new(alloc::collections::VecDeque::new()),
        slots: Semaphore::new(4),
        items: Semaphore::new(0),
    });
    let p = {
        let r = r.clone();
        spawn("producer", move || {
            for i in 0..N {
                r.slots.acquire();
                r.buf.lock().push_back(i);
                r.items.release();
                if i % 50 == 0 {
                    super::sleep_ms(2);
                }
            }
        })
    };
    let c = {
        let r = r.clone();
        spawn("consumer", move || {
            let mut sum = 0usize;
            for i in 0..N {
                r.items.acquire();
                let v = r.buf.lock().pop_front().unwrap();
                assert!(v == i, "out of order: {v} != {i}");
                sum += v;
                r.slots.release();
                if i % 70 == 0 {
                    super::sleep_ms(3);
                }
            }
            super::exit(sum as i64)
        })
    };
    p.join();
    let sum = c.join();
    kprintln!("[sched-test] semaphore: {N} items through a 4-slot buffer, sum {sum}");
    assert!(sum == (N * (N - 1) / 2) as i64);
}

/// Two threads that never yield both make progress: the tick preempts them.
fn preemption() {
    let a = Arc::new(AtomicU64::new(0));
    let b = Arc::new(AtomicU64::new(0));
    let spin = |mine: Arc<AtomicU64>, other: Arc<AtomicU64>| {
        move || {
            let end = timer::now_ns() + 150_000_000;
            let mut seen_other_move = 0u64;
            let mut last = other.load(Ordering::Relaxed);
            while timer::now_ns() < end {
                mine.fetch_add(1, Ordering::Relaxed);
                let o = other.load(Ordering::Relaxed);
                if o != last {
                    seen_other_move += 1;
                    last = o;
                }
            }
            super::exit(seen_other_move as i64)
        }
    };
    let t0 = timer::ticks();
    let h1 = spawn("spin-a", spin(a.clone(), b.clone()));
    let h2 = spawn("spin-b", spin(b.clone(), a.clone()));
    let (s1, s2) = (h1.join(), h2.join());
    kprintln!(
        "[sched-test] preemption: two busy threads, {} ticks, a={} b={}, each saw the other run {}/{} times",
        timer::ticks() - t0,
        a.load(Ordering::Relaxed),
        b.load(Ordering::Relaxed),
        s1,
        s2
    );
    assert!(s1 > 0 && s2 > 0, "busy threads were not preempted");
}

/// wait_until_timeout: times out when nobody wakes us, returns early when someone does.
fn timeout() {
    let wq = Arc::new(super::WaitQueue::new());
    let t0 = timer::now_ns();
    let r = wq.wait_until_timeout(|| false, 20_000_000);
    let waited = (timer::now_ns() - t0) / 1000;
    assert!(
        !r && waited >= 20_000,
        "timed wait returned after {waited} us"
    );
    let flag = Arc::new(AtomicBool::new(false));
    let h = {
        let (wq, flag) = (wq.clone(), flag.clone());
        spawn("waker", move || {
            super::sleep_ms(5);
            flag.store(true, Ordering::Relaxed);
            wq.wake_all();
        })
    };
    let t1 = timer::now_ns();
    let r2 = wq.wait_until_timeout(|| flag.load(Ordering::Relaxed), 1_000_000_000);
    let early = (timer::now_ns() - t1) / 1000;
    h.join();
    kprintln!(
        "[sched-test] timed wait: timed out after {waited} us (20 ms), woken after {early} us (of 1 s)"
    );
    assert!(r2 && early < 500_000, "timed wait missed its wakeup");
}

/// Regression test for a lost wakeup: a waiter whose condition came true after a wakeup
/// that wasn't from this queue (here: a leftover `wake_pending`) used to leave without
/// taking itself off the queue. The next `wake_one` then popped that stale entry, "woke"
/// a thread that wasn't waiting, and the real waiter behind it slept forever (seen as
/// boots hanging in the mutex test with every CPU idle).
fn stale_waiter() {
    let q = Arc::new(super::WaitQueue::new());
    // 1. Leave a pending wake on ourselves (a wake that arrived while we were running).
    super::wake(super::current_tid());
    // 2. Wait on `q` with a condition that is false, false, then true: enqueue, block
    //    (returns at once on the pending wake), condition true -> leave.
    let mut calls = 0;
    q.wait_until(|| {
        calls += 1;
        calls >= 3
    });
    // 3. A real waiter blocks on `q`.
    let flag = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let h = {
        let (q, flag, done) = (q.clone(), flag.clone(), done.clone());
        spawn("stale-waiter", move || {
            q.wait_until(|| flag.load(Ordering::Acquire));
            done.store(true, Ordering::Release);
        })
    };
    super::sleep_ms(20); // let it block
    // 4. One wake_one must reach it.
    flag.store(true, Ordering::Release);
    q.wake_one();
    let t0 = timer::now_ns();
    while !done.load(Ordering::Acquire) && timer::now_ns() - t0 < 200_000_000 {
        super::sleep_ms(1);
    }
    let ok = done.load(Ordering::Acquire);
    if !ok {
        q.wake_all(); // don't leave it hanging, then fail
    }
    h.join();
    kprintln!(
        "[sched-test] stale waiter: wake_one reached the real waiter: {ok} (condition checks {calls})"
    );
    assert!(
        ok,
        "wake_one was swallowed by a stale wait-queue entry (lost wakeup)"
    );
}

/// Spawn and join many threads; stacks and page tables must come back.
fn churn() {
    // Warm up to the same peak concurrency, so the window's page tables (kept forever)
    // already exist before we count frames.
    let warm: Vec<_> = (0..24)
        .map(|_| spawn("warmup", || super::sleep_ms(2)))
        .collect();
    for h in warm {
        h.join();
    }
    super::reap_all();
    let before = pmm::stats().free_frames;
    for round in 0..10 {
        let hs: Vec<_> = (0..20)
            .map(|i| spawn("churn", move || super::exit(round * 100 + i)))
            .collect();
        for (i, h) in hs.into_iter().enumerate() {
            assert!(h.join() == round * 100 + i as i64);
        }
    }
    // Detached threads get reaped too.
    for _ in 0..20 {
        drop(spawn("detached", || super::sleep_ms(1)));
    }
    super::sleep_ms(20);
    super::reap_all();
    let after = pmm::stats().free_frames;
    kprintln!(
        "[sched-test] churn: 220 threads created and freed, pmm free frames {before} -> {after}, {} threads live",
        super::threads().len()
    );
    // On SMP other threads may allocate/free meanwhile: only frames *missing* are a leak.
    assert!(after >= before, "kernel stacks leaked");
}

#[cfg(feature = "kstack-overflow-test")]
fn overflow() {
    #[inline(never)]
    #[allow(unconditional_recursion)]
    fn recurse(n: u64) -> u64 {
        let buf = [n; 64];
        core::hint::black_box(&buf);
        recurse(n + 1) + buf[3]
    }
    kprintln!("[sched-test] kstack-overflow-test: recursing forever in a thread...");
    spawn("recurse", || {
        recurse(0);
    })
    .join();
}
