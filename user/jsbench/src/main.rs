//! jsbench: the JavaScript engine on a few standard-style workloads, in the guest.
//!   jsbench [NAME]   run every workload (or the one named), printing each one's time in
//!                    ms, the heap in use and collections, then "jsbench: PASS" when every
//!                    result matched, or "jsbench: FAIL ...".

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

fn main(args: &rt::Args) -> i32 {
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
