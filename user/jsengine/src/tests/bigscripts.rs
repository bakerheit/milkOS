//! Large scripts: generated minified-style bundles of 1 to 8 MB under the real per-frame and
//! per-page limits (bench/biggen.rs makes them; user/jsbench runs the same ones in the guest).
//! `big_times` (ignored) prints parse and compile time, run time and peak memory.

use crate::*;
use alloc::string::String;

#[path = "../../bench/biggen.rs"]
mod biggen;
use biggen::*;

fn realm(heap: usize) -> Realm {
    let mut r = Realm::new(Options {
        limits: Limits {
            max_heap_bytes: heap,
            max_steps: 0,
            max_call_depth: 400,
            max_source_bytes: 8 << 20,
            max_compile_bytes: 96 << 20,
        },
        ..Options::default()
    });
    r.set_clock(alloc::boxed::Box::new(|| 0.0));
    r
}

fn big_stack<F: FnOnce() + Send + 'static>(f: F) {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
#[ignore]
fn big_times() {
    big_stack(|| {
        let sizes: Vec<usize> = std::env::var("BIG_SIZES")
            .unwrap_or("1,3,8".into())
            .split(',')
            .map(|s| s.parse::<usize>().unwrap() * 1_000_000)
            .collect();
        let kinds: Vec<String> = match std::env::var("BIG_KINDS") {
            Ok(k) => k.split(',').map(String::from).collect(),
            Err(_) => BUNDLE_KINDS.iter().map(|s| String::from(*s)).collect(),
        };
        let heap: usize = std::env::var("BIG_HEAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(16) << 20;
        let every: usize = std::env::var("BIG_EVERY").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
        std::println!(
            "{:<9} {:>4} {:>8} {:>9} {:>8} {:>10} {:>9} {:>8}  {}",
            "kind", "MB", "bytes", "load ms", "peak MB", "run ms", "heap KB", "kept MB", "result"
        );
        for kind in &kinds {
            for &size in &sizes {
                let b = gen_bundle(kind, size, every);
                let mut r = realm(heap);
                super::alloc_count::reset_peak();
                let base = super::alloc_count::current();
                let t0 = std::time::Instant::now();
                let chk = r.check_syntax(&b.src, "big.js");
                let load = t0.elapsed();
                let peak = super::alloc_count::peak().saturating_sub(base);
                let res = match chk {
                    Err(e) => alloc::format!("LOAD {e}"),
                    Ok(()) => {
                        let t1 = std::time::Instant::now();
                        let out = r.eval(&b.src, "big.js");
                        let run = t1.elapsed();
                        let msg = match out {
                            Ok(_) => {
                                let v = r.eval("globalThis.__r", "r.js").map(|v| v.as_number());
                                match v {
                                    Ok(Some(n)) if n == b.expect => String::from("ok"),
                                    Ok(n) => alloc::format!("WRONG {n:?} != {}", b.expect),
                                    Err(e) => alloc::format!("{}", r.report(&e)),
                                }
                            }
                            Err(e) => alloc::format!("RUN {}", r.report(&e)),
                        };
                        std::println!(
                            "{:<9} {:>4} {:>8} {:>9.1} {:>8.1} {:>10.1} {:>9} {:>8.1}  {}",
                            kind,
                            size / 1_000_000,
                            b.src.len(),
                            load.as_secs_f64() * 1e3,
                            peak as f64 / 1048576.0,
                            run.as_secs_f64() * 1e3,
                            r.heap_stats().used_bytes / 1024,
                            super::alloc_count::current().saturating_sub(base) as f64 / 1048576.0,
                            msg
                        );
                        continue;
                    }
                };
                std::println!(
                    "{:<9} {:>4} {:>8} {:>9.1} {:>8.1} {:>10} {:>9} {:>8}  {}",
                    kind,
                    size / 1_000_000,
                    b.src.len(),
                    load_ms(&t0),
                    peak as f64 / 1048576.0,
                    "-",
                    "-",
                    "-",
                    res
                );
            }
        }
    });
}

fn load_ms(t0: &std::time::Instant) -> f64 {
    t0.elapsed().as_secs_f64() * 1e3
}

#[test]
#[ignore]
fn node_sizes() {
    use core::mem::size_of;
    std::println!(
        "Expr {} Stmt {} ExprKind {} Func {} Pat {} PropDef {} ArrElem {} Arg {} Class {} Op {} Code {} Scope {} Binding {} Token {} Tok {} VarDecl {}",
        size_of::<crate::ast::Expr>(),
        size_of::<crate::ast::Stmt>(),
        size_of::<crate::ast::ExprKind>(),
        size_of::<crate::ast::Func>(),
        size_of::<crate::ast::Pat>(),
        size_of::<crate::ast::PropDef>(),
        size_of::<crate::ast::ArrElem>(),
        size_of::<crate::ast::Arg>(),
        size_of::<crate::ast::Class>(),
        size_of::<crate::bytecode::Op>(),
        size_of::<crate::bytecode::Code>(),
        size_of::<crate::resolve::Scope>(),
        size_of::<crate::resolve::Binding>(),
        size_of::<crate::lexer::Token>(),
        size_of::<crate::lexer::Tok>(),
        size_of::<crate::ast::VarDecl>(),
    );
}

/// BIG_FILE: loads one script file (no run) and prints the load time and peak memory.
#[test]
#[ignore]
fn big_file() {
    big_stack(|| {
        let path = std::env::var("BIG_FILE").unwrap();
        let src = std::fs::read_to_string(&path).unwrap();
        let heap: usize = std::env::var("BIG_HEAP").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(16) << 20;
        let mut r = realm(heap);
        super::alloc_count::reset_peak();
        let base = super::alloc_count::current();
        let t0 = std::time::Instant::now();
        let chk = r.check_syntax(&src, "big.js");
        let load = t0.elapsed();
        let peak = super::alloc_count::peak().saturating_sub(base);
        std::println!(
            "{} bytes: load {:.1} ms, peak {:.1} MB, retained {:.1} MB -> {:?}",
            src.len(),
            load.as_secs_f64() * 1e3,
            peak as f64 / 1048576.0,
            super::alloc_count::current().saturating_sub(base) as f64 / 1048576.0,
            chk.map_err(|e| alloc::format!("{e}"))
        );
    });
}
