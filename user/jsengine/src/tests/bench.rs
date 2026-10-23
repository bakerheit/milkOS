//! The benchmark workloads (bench/*.js, also run in the guest by user/jsbench): their
//! results must match bench/expected/*.txt. `bench_times` (ignored) prints host timings;
//! JSENGINE_BENCH_WRITE=1 rewrites the expected files.

use crate::*;
use alloc::boxed::Box;
use alloc::string::String;

pub(crate) const WORKLOADS: &[(&str, &str)] = &[
    ("richards", include_str!("../../bench/richards.js")),
    ("deltablue", include_str!("../../bench/deltablue.js")),
    ("json", include_str!("../../bench/json.js")),
    ("strings", include_str!("../../bench/strings.js")),
    ("arrays", include_str!("../../bench/arrays.js")),
    ("async", include_str!("../../bench/async.js")),
    ("numeric", include_str!("../../bench/numeric.js")),
];

fn run(src: &str, name: &str) -> String {
    let mut r = Realm::new(Options::default());
    r.set_clock(Box::new(|| 0.0));
    match r.eval(src, name) {
        Ok(v) => match r.run_jobs() {
            Ok(()) => r.inspect(v),
            Err(e) => alloc::format!("job error: {}", r.report(&e)),
        },
        Err(e) => alloc::format!("error: {}", r.report(&e)),
    }
}

fn expected_path(name: &str) -> String {
    alloc::format!("{}/bench/expected/{name}.txt", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn bench_results() {
    let h = std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(|| {
            let write = std::env::var("JSENGINE_BENCH_WRITE").is_ok();
            for (name, src) in WORKLOADS {
                let got = run(src, name);
                assert!(
                    !got.starts_with("error") && !got.starts_with("job error"),
                    "{name}: {got}"
                );
                if write {
                    std::fs::write(expected_path(name), alloc::format!("{got}\n")).unwrap();
                } else {
                    let exp = std::fs::read_to_string(expected_path(name)).unwrap_or_default();
                    assert_eq!(got.trim(), exp.trim(), "{name}");
                }
            }
        })
        .unwrap();
    h.join().unwrap();
}

#[test]
#[ignore]
fn bench_times() {
    for (name, src) in WORKLOADS {
        let t = std::time::Instant::now();
        let got = run(src, name);
        std::println!("{name}: {:?} -> {got}", t.elapsed());
    }
}

/// JSENGINE_BENCH_FILE: run one file with a step budget and print the time and steps.
#[test]
#[ignore]
fn bench_file() {
    let path = std::env::var("JSENGINE_BENCH_FILE").unwrap();
    let src = std::fs::read_to_string(&path).unwrap();
    let mut r = Realm::new(Options::default());
    let steps: u64 = std::env::var("JSENGINE_BENCH_STEPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    r.set_limits(Limits {
        max_steps: steps,
        ..Limits::default()
    });
    let t = std::time::Instant::now();
    let out = match r.eval(&src, "bench.js") {
        Ok(v) => {
            let _ = r.run_jobs();
            r.inspect(v)
        }
        Err(e) => alloc::format!("error: {}", r.report(&e)),
    };
    let st = r.heap_stats();
    std::println!(
        "{:?} steps {} gcs {} used {} KB -> {out}",
        t.elapsed(),
        r.steps(),
        st.collections,
        st.used_bytes / 1024
    );
}

/// Runs JSENGINE_BENCH_FILE JSENGINE_BENCH_REPEAT times (for a profiler to sample).
#[test]
#[ignore]
fn bench_loop() {
    let path = std::env::var("JSENGINE_BENCH_FILE").unwrap();
    let n: usize = std::env::var("JSENGINE_BENCH_REPEAT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let src = std::fs::read_to_string(&path).unwrap();
    for _ in 0..n {
        let mut r = Realm::new(Options::default());
        let _ = r.eval(&src, "bench.js");
    }
}
