//! jsbench: the JavaScript engine on a few standard-style workloads, in the guest.
//!   jsbench [NAME]   run every workload (or the one named), printing each one's time in
//!                    ms, the heap in use and collections, then "jsbench: PASS" when every
//!                    result matched, or "jsbench: FAIL ...".
//!   jsbench big [KIND|all] [MB] [EVERY] [frame|page]
//!                    large generated scripts (user/jsengine/bench/biggen.rs) under the
//!                    Browser's page (or frame) limits: load (parse + compile) and run time,
//!                    peak memory of the process, and the result. KIND is one of funcs,
//!                    modules, closures, literals, chains, small, huge; MB defaults to 1, 3
//!                    and 8; EVERY is how sparsely the bundle's functions are called.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use jsengine::{Options, Realm};
use rt::{println, sys};

rt::main!(main);

fn now() -> u64 {
    sys::clock_ns(sys::CLOCK_MONOTONIC)
}

/// (name, source, expected result as the engine's inspect() prints it)
const WORKLOADS: &[(&str, &str, &str)] = &[
    (
        "richards",
        include_str!("../../jsengine/bench/richards.js"),
        include_str!("../../jsengine/bench/expected/richards.txt"),
    ),
    (
        "deltablue",
        include_str!("../../jsengine/bench/deltablue.js"),
        include_str!("../../jsengine/bench/expected/deltablue.txt"),
    ),
    (
        "json",
        include_str!("../../jsengine/bench/json.js"),
        include_str!("../../jsengine/bench/expected/json.txt"),
    ),
    (
        "strings",
        include_str!("../../jsengine/bench/strings.js"),
        include_str!("../../jsengine/bench/expected/strings.txt"),
    ),
    (
        "arrays",
        include_str!("../../jsengine/bench/arrays.js"),
        include_str!("../../jsengine/bench/expected/arrays.txt"),
    ),
    (
        "async",
        include_str!("../../jsengine/bench/async.js"),
        include_str!("../../jsengine/bench/expected/async.txt"),
    ),
    (
        "numeric",
        include_str!("../../jsengine/bench/numeric.js"),
        include_str!("../../jsengine/bench/expected/numeric.txt"),
    ),
    (
        "typed",
        include_str!("../../jsengine/bench/typed.js"),
        include_str!("../../jsengine/bench/expected/typed.txt"),
    ),
];

#[path = "../../jsengine/bench/biggen.rs"]
mod biggen;

/// The limits a page's scripts run under in the Browser (docs/browser.md): the top page, or
/// a frame (a quarter of the heap).
fn page_limits(frame: bool) -> jsengine::Limits {
    jsengine::Limits {
        max_heap_bytes: if frame { 16 << 20 } else { 64 << 20 },
        max_steps: 0,
        max_call_depth: 400,
        max_source_bytes: 8 << 20,
        max_compile_bytes: if frame { 64 << 20 } else { 96 << 20 },
    }
}

/// One generated bundle through the engine as the Browser would run it: parse and compile
/// ("load"), then run it. Prints a line with the times and the memory; true if it either ran
/// right or stopped on a limit with a plain error (a wrong result is a failure).
fn big_case(kind: &str, mb: usize, every: usize, frame: bool) -> bool {
    let b = biggen::gen_bundle(kind, mb * 1_000_000, every);
    let bytes = b.src.len();
    let mut r = Realm::new(Options {
        limits: page_limits(frame),
        ..Options::default()
    });
    let t_clock = sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000;
    r.set_clock(Box::new(move || t_clock as f64));
    let st0 = rt::heap::stats();
    rt::heap::reset_peak();
    let t0 = now();
    let loaded = r.check_syntax(&b.src, "big.js");
    let load_ms = (now() - t0) / 1_000_000;
    let peak_kb = rt::heap::stats().peak.saturating_sub(st0.used) / 1024;
    let who = if frame { "frame" } else { "page" };
    if let Err(e) = loaded {
        let first = alloc::format!("{e}");
        println!(
            "jsbench-big: {kind} {mb}MB {who} every={every} bytes={bytes} load={load_ms}ms peak={peak_kb}KB refused: {}",
            first.lines().next().unwrap_or("")
        );
        return true;
    }
    rt::heap::reset_peak();
    let t1 = now();
    let out = r.eval(&b.src, "big.js");
    let run_ms = (now() - t1) / 1_000_000;
    let peak2_kb = rt::heap::stats().peak.saturating_sub(st0.used) / 1024;
    let st = r.heap_stats();
    let (ok, res) = match out {
        Ok(_) => match r.eval("globalThis.__r", "r.js") {
            Ok(v) if v.as_number() == Some(b.expect) => (true, String::from("ok")),
            Ok(v) => (false, alloc::format!("WRONG {:?} expected {}", v.as_number(), b.expect)),
            Err(e) => (false, alloc::format!("{}", r.report(&e))),
        },
        Err(e) => {
            let rep = alloc::format!("{}", r.report(&e));
            // Running out of the heap budget is the limit doing its job.
            (rep.contains("memory"), alloc::format!("stopped: {}", rep.lines().next().unwrap_or("")))
        }
    };
    println!(
        "jsbench-big: {kind} {mb}MB {who} every={every} bytes={bytes} load={load_ms}ms peak={peak_kb}KB run={run_ms}ms runpeak={peak2_kb}KB heap={}KB {res}",
        st.used_bytes / 1024
    );
    ok
}

fn big(args: &rt::Args) -> i32 {
    // jsbench big [KIND|all] [MB] [EVERY] [frame|page]
    let kind = args.get(2).unwrap_or("all");
    let mb: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let every: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);
    let frame = args.get(5) == Some("frame");
    let mut fails = 0;
    let kinds: alloc::vec::Vec<&str> = if kind == "all" {
        biggen::BUNDLE_KINDS.to_vec()
    } else {
        alloc::vec![kind]
    };
    let sizes: alloc::vec::Vec<usize> = if mb == 0 { alloc::vec![1, 3, 8] } else { alloc::vec![mb] };
    for k in kinds {
        for &m in &sizes {
            if !big_case(k, m, every, frame) {
                fails += 1;
            }
        }
    }
    if fails == 0 {
        println!("jsbench-big: PASS");
        0
    } else {
        println!("jsbench-big: FAIL ({fails})");
        1
    }
}

fn main(args: &rt::Args) -> i32 {
    if args.get(1) == Some("big") {
        return big(args);
    }
    let only = args.get(1);
    let mut fails = 0;
    let mut total_ms = 0u64;
    for (name, src, expected) in WORKLOADS {
        if only.is_some_and(|o| o != *name) {
            continue;
        }
        let mut r = Realm::new(Options::default());
        let t_clock = sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000;
        r.set_clock(Box::new(move || t_clock as f64));
        let t0 = now();
        let res = r.eval(src, name);
        let got: String = match res {
            Ok(v) => {
                if let Err(e) = r.run_jobs() {
                    let rep = r.report(&e);
                    alloc::format!("job error: {rep}")
                } else {
                    r.inspect(v)
                }
            }
            Err(e) => {
                let rep = r.report(&e);
                alloc::format!("error: {rep}")
            }
        };
        let t1 = now();
        let ms = (t1 - t0) / 1_000_000;
        total_ms += ms;
        let st = r.heap_stats();
        let ok = got.trim() == expected.trim();
        println!(
            "jsbench: {name} {ms} ms ({} KB heap, {} collections){}",
            st.used_bytes / 1024,
            st.collections,
            if ok { "" } else { " WRONG" }
        );
        if !ok {
            println!("jsbench:   expected {}", expected.trim());
            println!("jsbench:   got      {}", got.trim());
            fails += 1;
        }
    }
    println!("jsbench: total {total_ms} ms");
    if fails == 0 {
        println!("jsbench: PASS");
        0
    } else {
        println!("jsbench: FAIL ({fails})");
        1
    }
}
