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
    // (A debug build uses a lot of native stack per level of nesting; the test threads have
    // plenty.)
    r.set_stack_limit(48 << 20);
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
        if std::env::var("BIG_RUN").is_ok() {
            let mut r = realm(heap);
            let t1 = std::time::Instant::now();
            let out = r.eval(&src, "big.js").map(|_| ()).map_err(|e| alloc::format!("{}", r.report(&e)));
            let st = r.heap_stats();
            std::println!("run {:.1} ms, heap used {:.1} MB ({} objects, {} strings, code {:.1} MB) -> {:?}", t1.elapsed().as_secs_f64() * 1e3, st.used_bytes as f64 / 1048576.0, st.objects, st.strings, r.heap.code_acct.get() as f64 / 1048576.0, out);
        }
    });
}

/// Hostile shapes of big literals must not take quadratic time to compile.
#[test]
#[ignore]
fn float_table_time() {
    big_stack(|| {
        for n in [10_000usize, 40_000, 160_000] {
            let mut s = String::from("var a=[");
            for i in 0..n {
                s.push_str(&alloc::format!("{}.{},", i, i % 7 + 1));
            }
            s.push_str("0];globalThis.__r=a.length;");
            let mut r = realm(64 << 20);
            let t = std::time::Instant::now();
            let out = r.check_syntax(&s, "f.js");
            std::println!("floats {n}: {:?} {:?}", t.elapsed(), out.is_ok());
        }
    });
}

fn shapes(n: usize) -> Vec<(&'static str, String)> {
    let join = |f: &dyn Fn(usize) -> String, sep: &str| (0..n).map(f).collect::<Vec<_>>().join(sep);
    alloc::vec![
        ("many params", alloc::format!("'use strict'; function f({}) {{}}", join(&|i| alloc::format!("p{i}"), ","))),
        ("many params sloppy", alloc::format!("function f({}) {{ return p0 }}", join(&|i| alloc::format!("p{i}"), ","))),
        ("many vars", alloc::format!("function f() {{ var {}; }}", join(&|i| alloc::format!("v{i}"), ","))),
        ("many lets", alloc::format!("{{ let {}; }}", join(&|i| alloc::format!("v{i}"), ","))),
        ("many top lets", alloc::format!("let {};", join(&|i| alloc::format!("v{i}"), ","))),
        ("many cases", alloc::format!("switch (1) {{ {} }}", join(&|i| alloc::format!("case {i}: break;"), " "))),
        ("many labels", alloc::format!("{} 1;", join(&|i| alloc::format!("l{i}:"), " "))),
        ("many object keys", alloc::format!("var o = {{ {} }};", join(&|i| alloc::format!("k{i}: {i}"), ","))),
        ("many computed keys", alloc::format!("var o = {{ {} }};", join(&|i| alloc::format!("['k{i}']: {i}"), ","))),
        ("many methods", alloc::format!("var o = {{ {} }};", join(&|i| alloc::format!("m{i}() {{ return {i} }}"), ","))),
        ("many class members", alloc::format!("class A {{ {} }}", join(&|i| alloc::format!("m{i}() {{ return {i} }}"), " "))),
        ("many static fields", alloc::format!("class A {{ {} }}", join(&|i| alloc::format!("static f{i} = {i};"), " "))),
        ("many private", alloc::format!("class A {{ {} }}", join(&|i| alloc::format!("#p{i} = {i};"), " "))),
        ("many args", alloc::format!("function f() {{}} f({});", join(&|i| alloc::format!("{i}"), ","))),
        ("many array elems", alloc::format!("var a = [{}];", join(&|i| alloc::format!("{i}"), ","))),
        ("many strings", alloc::format!("var a = [{}];", join(&|i| alloc::format!("'s{i}'"), ","))),
        ("many regexps", alloc::format!("var a = [{}];", join(&|i| alloc::format!("/r{i}/"), ","))),
        ("many templates", alloc::format!("var a = [{}];", join(&|i| alloc::format!("`t{i}${{{i}}}`"), ","))),
        ("many pattern elems", alloc::format!("var [{}] = [];", join(&|i| alloc::format!("a{i}"), ","))),
        ("many object pattern", alloc::format!("var {{ {} }} = {{}};", join(&|i| alloc::format!("a{i}"), ","))),
        ("many functions", join(&|i| alloc::format!("function f{i}() {{ return {i} }}"), " ")),
        ("many arrows", alloc::format!("var a = [{}];", join(&|i| alloc::format!("() => {i}"), ","))),
        ("many closures capturing", alloc::format!("function f() {{ var x = 0; return [{}]; }}", join(&|i| alloc::format!("() => x + {i}"), ","))),
        ("many captured vars", alloc::format!("function f() {{ var {}; return () => [{}]; }}", join(&|i| alloc::format!("v{i}"), ","), join(&|i| alloc::format!("v{i}"), ","))),
        ("many try", join(&|i| alloc::format!("try {{ {i} }} catch (e) {{ }} finally {{ }}"), " ")),
        ("many ifs", join(&|i| alloc::format!("if ({i}) {{ }} else {{ }}"), " ")),
        ("many loops", join(&|i| alloc::format!("for (let i = 0; i < {i}; i++) {{ break }}"), " ")),
        ("many sequence", alloc::format!("({})", join(&|i| alloc::format!("{i}"), ","))),
        ("many ternary", alloc::format!("var x = {} 0;", join(&|i| alloc::format!("{i} ? {i} :"), " "))),
        ("many spread", alloc::format!("var a = [{}];", join(&|i| alloc::format!("...[{i}]"), ","))),
        ("many string concat", alloc::format!("var s = {};", join(&|i| alloc::format!("'a{i}'"), "+"))),
        ("many imports", join(&|i| alloc::format!("import('x{i}');"), " ")),
        ("many chained or", alloc::format!("var s = {};", join(&|i| alloc::format!("a{i}"), "||"))),
        ("many throw/catch", alloc::format!("function f() {{ {} }}", join(&|i| alloc::format!("try {{ throw {i} }} catch (e{i}) {{ }}"), " "))),
        ("many destructure defaults", alloc::format!("var {{ {} }} = {{}};", join(&|i| alloc::format!("a{i} = {i}"), ","))),
        ("many yields", alloc::format!("function* g() {{ {} }}", join(&|i| alloc::format!("yield {i};"), " "))),
        ("many awaits", alloc::format!("async function g() {{ {} }}", join(&|i| alloc::format!("await {i};"), " "))),
        ("many getters", alloc::format!("var o = {{ {} }};", join(&|i| alloc::format!("get g{i}() {{ return {i} }}"), ","))),
        ("many nested funcs flat", alloc::format!("function f() {{ {} }}", join(&|i| alloc::format!("function g{i}() {{ return {i} }}"), " "))),
        ("many block funcs", alloc::format!("{{ {} }}", join(&|i| alloc::format!("function g{i}() {{ return {i} }}"), " "))),
        ("many with same name", join(&|_| String::from("function f() { return 1 }"), " ")),
        ("many debugger", join(&|_| String::from("debugger;"), " ")),
    ]
}

/// Big flat shapes must compile in about linear time (nothing quadratic hides in the parser,
/// resolver or compiler): ten times the size takes about ten times as long.
#[test]
fn big_flat_shapes_compile_in_linear_time() {
    big_stack(|| {
        // (Debug build, so small: the limit is on the ratio and on the absolute time.)
        let small = std::env::var("BIG_SMALL").ok().and_then(|s| s.parse().ok()).unwrap_or(4_000usize);
        let big = std::env::var("BIG_BIG").ok().and_then(|s| s.parse().ok()).unwrap_or(32_000usize);
        let mut slow = Vec::new();
        let a = shapes(small);
        let b = shapes(big);
        for ((name, s_small), (_, s_big)) in a.iter().zip(b.iter()) {
            let time = |src: &str| {
                let mut r = realm(64 << 20);
                let t = std::time::Instant::now();
                let _ = r.check_syntax(src, "shape.js");
                t.elapsed().as_secs_f64()
            };
            let t1 = time(s_small).max(0.002);
            let t2 = time(s_big);
            // 8x the size: allow 20x (noise) and anything under a second.
            if t2 > 1.5 && t2 > t1 * 24.0 {
                slow.push(alloc::format!("{name}: {t1:.3}s -> {t2:.3}s"));
            }
            if std::env::var_os("BIG_VERBOSE").is_some() {
                std::println!("{name:<28} {t1:.4}s -> {t2:.4}s");
            }
        }
        assert!(slow.is_empty(), "{}", slow.join("\n"));
    });
}

// ---- the tests that run every time ----

fn run_bundle(b: &Bundle, heap: usize) -> Result<Option<f64>, String> {
    let mut r = realm(heap);
    match r.eval(&b.src, "big.js") {
        Ok(_) => {
            let v = r.eval("globalThis.__r", "r.js").map_err(|e| alloc::format!("{}", r.report(&e)))?;
            Ok(v.as_number())
        }
        Err(e) => Err(alloc::format!("{}", r.report(&e))),
    }
}

/// Every kind of generated bundle runs and gives its number, whatever part of it runs, and
/// the same whether functions are compiled at the start or on their first call.
#[test]
fn generated_bundles_run_right() {
    big_stack(|| {
        for lazy in [None, Some(0), Some(u32::MAX - 1)] {
            crate::set_lazy_min_bytes(lazy);
            for kind in BUNDLE_KINDS {
                for every in [1usize, 7, 300] {
                    let b = gen_bundle(kind, 250_000, every);
                    match run_bundle(&b, 64 << 20) {
                        Ok(Some(n)) => assert_eq!(n, b.expect, "{kind} every {every} lazy {lazy:?}"),
                        other => panic!("{kind} every {every} lazy {lazy:?}: {other:?}"),
                    }
                }
            }
        }
        crate::set_lazy_min_bytes(None);
    });
}

/// A bundle of a few megabytes loads in a fraction of its size in memory: its functions are
/// parsed and checked one at a time and compiled when called.
#[test]
fn big_bundles_load_in_a_small_peak() {
    big_stack(|| {
        for (kind, mb, peak_limit) in [("modules", 3usize, 40usize), ("closures", 3, 60), ("funcs", 2, 80)] {
            let b = gen_bundle(kind, mb * 1_000_000, 50);
            let mut r = realm(64 << 20);
            super::alloc_count::reset_peak();
            let base = super::alloc_count::current();
            r.check_syntax(&b.src, "big.js").unwrap_or_else(|e| panic!("{kind}: {e}"));
            let peak = super::alloc_count::peak().saturating_sub(base);
            assert!(
                peak < peak_limit << 20,
                "{kind} {mb} MB: the peak while loading was {} MB",
                peak >> 20
            );
        }
    });
}

/// Under a frame's limits (16 MB heap), a megabytes-long module bundle runs.
#[test]
fn frame_limits_hold_a_big_bundle() {
    big_stack(|| {
        for kind in ["modules", "closures"] {
            let b = gen_bundle(kind, 2_500_000, 50);
            match run_bundle(&b, 16 << 20) {
                Ok(Some(n)) => assert_eq!(n, b.expect, "{kind}"),
                other => panic!("{kind}: {other:?}"),
            }
        }
    });
}

/// The compile allowance is for one piece at a time: a script of many statements loads where
/// one function (or one statement) as big as the whole is refused, and so is the heap budget.
#[test]
fn the_compile_allowance_bounds_the_biggest_piece() {
    big_stack(|| {
        let limits = |compile: usize| Limits {
            max_compile_bytes: compile,
            max_heap_bytes: 256 << 20,
            max_source_bytes: 32 << 20,
            ..Limits::default()
        };
        let mut many = String::new();
        let mut one = String::from("function f() {");
        while many.len() < 1_500_000 {
            many.push_str("x = 1 + 2 * 3; y = [x, 'abc', {k: x}];\n");
            one.push_str("x = 1 + 2 * 3; y = [x, 'abc', {k: x}];\n");
        }
        one.push('}');
        let mut r = Realm::new(Options {
            limits: limits(6 << 20),
            ..Options::default()
        });
        r.check_syntax(&many, "many.js").expect("statements are compiled one at a time");
        let e = r.check_syntax(&one, "one.js").expect_err("one huge function does not fit");
        assert!(alloc::format!("{e}").contains("too large"), "{e}");
        // As statements of a function that is called, the same (each is under the allowance).
        let mut r = Realm::new(Options {
            limits: limits(256 << 20),
            ..Options::default()
        });
        r.check_syntax(&one, "one.js").expect("fits a big allowance");
    });
}

/// Early errors inside functions that are not called are still reported when the script is
/// loaded.
#[test]
fn errors_in_uncalled_functions_come_at_load() {
    let pad = " ".repeat(200);
    for src in [
        "function f() { let a = 1; var a; }",
        "function f() { return 1 +; }",
        "function f() { 'use strict'; var yield = 1; }",
        "function f() { class A { m() { return this.#nope } } }",
        "var o = { m() { break; } };",
        "function f() { 'use strict'; with (x) {} }",
        "function f() { function g() { let b; let b; } }",
        "var f = () => { let c; { var c; } }",
        "function f(a, a) { 'use strict'; }",
    ] {
        let src = alloc::format!("{pad}\n{src}\n{pad}");
        for lazy in [None, Some(0)] {
            crate::set_lazy_min_bytes(lazy);
            let mut r = realm(16 << 20);
            let e = r.eval(&src, "early.js").expect_err(&src);
            assert_eq!(r.report(&e).name, "SyntaxError", "{src}");
        }
    }
    crate::set_lazy_min_bytes(None);
}

/// Hostile big inputs end with an error or a result, in bounded memory, without panics.
#[test]
fn hostile_big_inputs_stay_bounded() {
    big_stack(|| {
        let n = 2_000_000usize;
        let mut cases: Vec<(&str, String)> = alloc::vec![
            ("open parens", "(".repeat(n)),
            ("open brackets", "[".repeat(n)),
            ("open functions", "function f(){".repeat(n / 8)),
            ("open arrows", "x=>".repeat(n / 3)),
            ("braces", "{".repeat(n)),
            ("a long chain", alloc::format!("x = 1{}", "+1".repeat(n / 2))),
            ("a long call chain", alloc::format!("x{}", ".a()".repeat(n / 4))),
            ("many unary", alloc::format!("x = {}1", "-".repeat(n))),
            ("a huge string", alloc::format!("x = '{}'", "a".repeat(n * 3))),
            ("a huge template", alloc::format!("x = `{}`", "${1}".repeat(n / 4))),
            ("many commas", alloc::format!("x = [{}]", ",".repeat(n))),
            ("a long regexp", alloc::format!("x = /{}/", "a?".repeat(n / 2))),
            ("many ifs", "if(1)".repeat(n / 5)),
            ("many else", alloc::format!("if(1);{}", "else if(1);".repeat(n / 11))),
            ("many classes", "class A{".repeat(n / 8)),
            ("many ternaries", alloc::format!("x = {}1", "1?".repeat(n / 2))),
            ("many labels", "a:".repeat(n / 2)),
            ("many arguments", alloc::format!("f({})", "1,".repeat(n / 2))),
            ("many parameters", alloc::format!("function f({}){{}}", "a,".repeat(n / 4))),
        ];
        // The same inside a function that is called later.
        let wrapped: Vec<(&str, String)> = cases
            .iter()
            .map(|(n, s)| (*n, alloc::format!("function w() {{ {s} }}")))
            .collect();
        cases.extend(wrapped);
        for (name, src) in &cases {
            let mut r = realm(16 << 20);
            r.set_limits(Limits {
                max_heap_bytes: 16 << 20,
                max_steps: 5_000_000,
                max_call_depth: 200,
                max_source_bytes: 8 << 20,
                max_compile_bytes: 96 << 20,
            });
            super::alloc_count::reset_peak();
            let base = super::alloc_count::current();
            let t = std::time::Instant::now();
            let out = r.eval(src, "hostile.js");
            let _ = r.run_jobs();
            let peak = super::alloc_count::peak().saturating_sub(base);
            // Whatever it did, it was quick and its memory was bounded (the allowance is 96
            // MB of tree estimate; the real peak may be a bit above that, never wild).
            assert!(
                peak < 700 << 20,
                "{name}: peak {} MB ({:?})",
                peak >> 20,
                out.as_ref().err().map(|e| r.report(e).to_string())
            );
            assert!(t.elapsed().as_secs() < 60, "{name}: {:?}", t.elapsed());
        }
    });
}
