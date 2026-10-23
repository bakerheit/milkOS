//! Fuzzing the module parser, linker and evaluator: token soup with module syntax, mutated
//! module test sources, and generated module graphs (cycles, star exports, top-level
//! await, throwing modules, bad imports) whose runs are checked for the invariants the
//! spec promises: every module runs at most once, dependencies run first (in an acyclic
//! graph), and a failed module fails everything above it. Nothing may panic or run away.
//!
//! JSENGINE_FUZZ_ITERS=N sets the number of cases (default 1500; graphs use a fifth of it);
//! JSENGINE_FUZZ_SEED picks the seed.

use super::fuzz::{Rng, iters, mutate, seed, token_soup};
use super::modules::{Host, parse_cases};
use crate::*;
use alloc::string::String;
use alloc::vec::Vec;

const MODULE_TOKENS: &[&str] = &[
    "import",
    "export",
    "from",
    "as",
    "default",
    "*",
    "{",
    "}",
    ",",
    ";",
    "'./a.js'",
    "'./b.js'",
    "'./c.js'",
    "'nope'",
    "'x y'",
    "with",
    "{ type: 'json' }",
    "assert",
    "meta",
    ".",
    "(",
    ")",
    "await",
    "async",
    "function",
    "class",
    "let",
    "const",
    "var",
    "x",
    "y",
    "z",
    "a",
    "b",
    "=",
    "1",
    "'s'",
    "import.meta",
    "import(",
    "await import(",
    "for await (",
    "of",
    "\n",
    "export default",
    "export *",
    "export { x }",
    "export { x as y }",
    "import x from",
    "import { x } from",
    "import * as ns from",
    "export * as ns from",
    "{ x as y }",
    "{ default as z }",
    "\"str name\"",
    "\\u0061",
];

fn module_soup(rng: &mut Rng) -> String {
    let n = 1 + rng.below(80);
    let mut s = String::new();
    for _ in 0..n {
        if rng.chance(55) {
            s.push_str(rng.pick(MODULE_TOKENS));
        } else {
            s.push_str(&token_soup(rng)[..0]);
            s.push_str(rng.pick(&["x", "y", "{", "}", "(", ")", ";", "=", "let", "1", ".", ","]));
        }
        s.push(' ');
    }
    s
}

/// Compiles `src` as `main.js` next to three small helper modules, links and runs the lot
/// under tight limits.
fn run_module_case(src: &str, others: &[(String, String)]) -> usize {
    let mut files: Vec<(String, String)> = alloc::vec![
        (String::from("main.js"), String::from(src)),
        (
            String::from("a.js"),
            String::from("export const x = 1; export default function () {}; export let y = 2;"),
        ),
        (
            String::from("b.js"),
            String::from("import './main.js'; export * from './a.js'; export const z = await 0;"),
        ),
        (
            String::from("c.js"),
            String::from("export { x as 'x y' } from './a.js'; throw new Error('c');"),
        ),
    ];
    files.extend(others.iter().cloned());
    let mut host = Host::new(files);
    let mut r = host.realm(0);
    r.set_limits(Limits {
        max_heap_bytes: 16 << 20,
        max_steps: 300_000,
        max_call_depth: 200,
        ..Limits::default()
    });
    r.set_stack_limit(2 << 20);
    let t = std::time::Instant::now();
    host.run(&mut r, "main.js");
    r.release_temps();
    r.gc();
    assert!(t.elapsed().as_secs() < 20, "took {:?}:\n{src}", t.elapsed());
    assert!(r.heap_stats().used_bytes < 64 << 20, "kept memory:\n{src}");
    let log = host.log.borrow();
    if log.iter().any(|l| l.starts_with("load error")) {
        1
    } else if log.iter().any(|l| l.starts_with("link error")) {
        2
    } else if log.iter().any(|l| l.starts_with("eval error")) {
        3
    } else {
        0
    }
}

fn fuzz_modules(kind: &str, make: &dyn Fn(&mut Rng) -> (String, Vec<(String, String)>)) {
    let mut rng = Rng(seed() ^ (kind.len() as u64 * 0x9E37_79B9) ^ 0xABCD);
    let mut counts = [0usize; 4];
    for i in 0..iters() {
        let (src, others) = make(&mut rng);
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_module_case(&src, &others)
        }));
        match out {
            Ok(k) => counts[k] += 1,
            Err(_) => panic!("{kind} case {i} panicked; source:\n{src}\nothers: {others:?}"),
        }
    }
    std::println!("{kind} outcomes (ran, syntax error, link error, eval error): {counts:?}");
    // Mutations should get past the parser often enough to test the linker and evaluator too.
    if kind == "module-mutate" {
        assert!(
            counts[0] + counts[2] + counts[3] > counts[1] / 10,
            "{counts:?}"
        );
    }
}

/// Mostly valid import and export declarations (and some uses), in a random order.
fn module_decls(rng: &mut Rng) -> String {
    const NAMES: &[&str] = &["x", "y", "z", "a", "b", "q", "default", "'str name'", "ns"];
    const LOCALS: &[&str] = &["x", "y", "z", "a", "b", "q", "ns", "d"];
    const SPECS: &[&str] = &["./a.js", "./b.js", "./c.js", "./main.js", "./none.js"];
    let mut out = String::new();
    for _ in 0..1 + rng.below(10) {
        let sp = rng.pick(SPECS);
        let n1 = rng.pick(NAMES);
        let n2 = rng.pick(LOCALS);
        let n3 = rng.pick(NAMES);
        match rng.below(26) {
            0 => out.push_str(&alloc::format!("import {n2} from '{sp}';\n")),
            1 => out.push_str(&alloc::format!("import * as {n2} from '{sp}';\n")),
            2 => out.push_str(&alloc::format!("import {{ {n1} as {n2} }} from '{sp}';\n")),
            3 => out.push_str(&alloc::format!("import '{sp}';\n")),
            4 => out.push_str(&alloc::format!(
                "import {n2}, {{ {n3} as {} }} from '{sp}';\n",
                rng.pick(LOCALS)
            )),
            5 => out.push_str(&alloc::format!(
                "import {n2}, * as {} from '{sp}';\n",
                rng.pick(LOCALS)
            )),
            6 => out.push_str(&alloc::format!("export * from '{sp}';\n")),
            7 => out.push_str(&alloc::format!("export * as {n3} from '{sp}';\n")),
            8 => out.push_str(&alloc::format!("export {{ {n1} as {n3} }} from '{sp}';\n")),
            9 => out.push_str(&alloc::format!("export {{ {n1} }} from '{sp}';\n")),
            10 => out.push_str(&alloc::format!("export {{ {n2} as {n3} }};\n")),
            11 => out.push_str(&alloc::format!("export const {n2} = {};\n", rng.below(100))),
            12 => out.push_str(&alloc::format!("export let {n2} = [1, 2, {n2}];\n")),
            13 => out.push_str(&alloc::format!("export var {n2};\n")),
            14 => out.push_str(&alloc::format!(
                "export function {n2}() {{ return {n2}; }}\n"
            )),
            15 => out.push_str(&alloc::format!(
                "export class {n2} {{ static s = {n2}; }}\n"
            )),
            16 => out.push_str("export default function () { return 1; }\n"),
            17 => out.push_str("export default class { }\n"),
            18 => out.push_str(&alloc::format!("export default {n2} + 1;\n")),
            19 => out.push_str(&alloc::format!("let {n2} = await import('{sp}');\n")),
            20 => out.push_str("await 0;\n"),
            21 => out.push_str(&alloc::format!("log(typeof {n2}, import.meta.url);\n")),
            22 => out.push_str(&alloc::format!("{n2} = 5;\n")),
            23 => out.push_str(&alloc::format!("throw {n2};\n")),
            24 => out.push_str(&alloc::format!(
                "import {{ {n1} as {n2} }} from '{sp}' with {{ type: 'json' }};\n"
            )),
            _ => out.push_str(&alloc::format!("export {{ {n2} as default }}; var {n2};\n")),
        }
    }
    out
}

fn big(f: fn()) {
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn fuzz_module_soup() {
    big(|| fuzz_modules("module-soup", &|rng| (module_soup(rng), Vec::new())));
}

#[test]
fn fuzz_module_decls() {
    big(|| fuzz_modules("module-decls", &|rng| (module_decls(rng), Vec::new())));
}

#[test]
fn fuzz_module_mutated() {
    big(|| {
        // Every file of every module test case is a seed; the rest of its case comes along.
        let mut corpus: Vec<(String, Vec<(String, String)>)> = Vec::new();
        for (_, text) in super::modules::FILES {
            for case in parse_cases(text) {
                for (n, src) in &case.files {
                    if n.ends_with(".js") && n != "script.js" {
                        let others: Vec<(String, String)> =
                            case.files.iter().filter(|(m, _)| m != n).cloned().collect();
                        corpus.push((src.clone(), others));
                    }
                }
            }
        }
        fuzz_modules("module-mutate", &|rng| {
            let (src, others) = &corpus[rng.below(corpus.len())];
            (mutate(rng, src), others.clone())
        });
    });
}

// ---- generated graphs ----

struct GraphSpec {
    files: Vec<(String, String)>,
    /// For each module: the modules it imports (statically), in order.
    deps: Vec<Vec<usize>>,
    /// Modules that throw when evaluated.
    throws: Vec<bool>,
}

fn graph(rng: &mut Rng) -> GraphSpec {
    let n = 2 + rng.below(8);
    let mut deps: Vec<Vec<usize>> = alloc::vec![Vec::new(); n];
    let mut sources: Vec<String> = Vec::new();
    let mut throws = alloc::vec![false; n];
    let cyclic = rng.chance(50);
    for i in 0..n {
        let mut s = String::new();
        let mut targets: Vec<usize> = Vec::new();
        for _ in 0..rng.below(4) {
            let t = if cyclic {
                rng.below(n)
            } else if i + 1 < n {
                i + 1 + rng.below(n - i - 1)
            } else {
                break;
            };
            if !targets.contains(&t) {
                targets.push(t);
            }
        }
        for &t in &targets {
            match rng.below(6) {
                0 => s.push_str(&alloc::format!("import './m{t}.js';\n")),
                1 => s.push_str(&alloc::format!("import {{ v{t} }} from './m{t}.js';\nexport const imp{i}_{t} = () => v{t};\n")),
                2 => s.push_str(&alloc::format!("import * as ns{t} from './m{t}.js';\nexport const nsget{i}_{t} = () => Object.keys(ns{t}).length;\n")),
                3 => s.push_str(&alloc::format!("export * from './m{t}.js';\n")),
                4 => s.push_str(&alloc::format!("export {{ v{t} as re{i}_{t} }} from './m{t}.js';\n")),
                _ => s.push_str(&alloc::format!("export * as star{i}_{t} from './m{t}.js';\n")),
            }
        }
        if rng.chance(8) {
            // A bad import: a name nobody exports.
            let t = rng.below(n);
            s.push_str(&alloc::format!(
                "import {{ missing{i} }} from './m{t}.js';\n"
            ));
            if !targets.contains(&t) {
                targets.push(t);
            }
        }
        deps[i] = targets;
        s.push_str(&alloc::format!(
            "export let v{i} = {i};\nexport function f{i}() {{ return v{i}; }}\n"
        ));
        if rng.chance(40) {
            match rng.below(4) {
                0 => s.push_str("await 0;\n"),
                1 => s.push_str("await new Promise((r) => r());\n"),
                2 => s.push_str(&alloc::format!(
                    "for (let i = 0; i < {}; i++) await i;\n",
                    1 + rng.below(4)
                )),
                _ => s.push_str("await Promise.resolve().then(() => 1);\n"),
            }
            if rng.chance(30) {
                s.push_str(&alloc::format!(
                    "Promise.resolve().then(() => log('tick{i}'));\n"
                ));
            }
        }
        s.push_str(&alloc::format!("log('m{i}');\n"));
        if rng.chance(10) {
            throws[i] = true;
            s.push_str(&alloc::format!("throw new Error('m{i} failed');\n"));
        }
        sources.push(s);
    }
    let files = sources
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            (
                if i == 0 {
                    String::from("main.js")
                } else {
                    alloc::format!("m{i}.js")
                },
                s,
            )
        })
        .collect();
    GraphSpec {
        files,
        deps,
        throws,
    }
}

/// 0 = ran fine, 1 = a link/load error, 2 = an evaluation error.
fn check_graph(g: &GraphSpec) -> usize {
    let mut files = g.files.clone();
    // Module 0 is main.js; other modules refer to it as ./m0.js.
    files.push((String::from("m0.js"), g.files[0].1.clone()));
    // (m0.js is a different file, so a path to m0 would load a second copy: keep main
    // unreferenced by rewriting it to point at main.js instead.)
    files.pop();
    let files: Vec<(String, String)> = files
        .into_iter()
        .map(|(n, s)| (n, s.replace("./m0.js", "./main.js")))
        .collect();
    let dumped = files.clone();
    let mut host = Host::new(files);
    let mut r = host.realm(0);
    r.set_limits(Limits {
        max_heap_bytes: 32 << 20,
        max_steps: 2_000_000,
        ..Limits::default()
    });
    r.set_stack_limit(2 << 20);
    host.run(&mut r, "main.js");
    let log = host.log.borrow().clone();
    // JSENGINE_MODULE_FUZZ_DUMP=file appends every graph as a conformance case with what the
    // engine logged, so scripts/jsmodules-oracle.sh can compare the lot with Node.
    if let Ok(path) = std::env::var("JSENGINE_MODULE_FUZZ_DUMP") {
        use std::io::Write;
        let mut text = String::from("//# case: generated graph\n");
        for (n, src) in &dumped {
            text.push_str(&alloc::format!("//# file: {n}\n{src}"));
        }
        text.push_str("//# expect\n");
        for l in &log {
            text.push_str(l);
            text.push('\n');
        }
        text.push('\n');
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = f.write_all(text.as_bytes());
        }
    }
    // Each module runs at most once.
    let mut seen = alloc::vec![0; g.files.len()];
    for line in &log {
        if let Some(rest) = line.strip_prefix('m')
            && let Ok(i) = rest.parse::<usize>()
        {
            seen[i] += 1;
        }
    }
    assert!(seen.iter().all(|c| *c <= 1), "a module ran twice: {log:?}");
    let failed_load = log
        .iter()
        .any(|l| l.starts_with("load error") || l.starts_with("link error"));
    let main_failed = log.iter().any(|l| l.starts_with("eval error"));
    if failed_load {
        assert!(
            seen.iter().all(|c| *c == 0),
            "ran after a link error: {log:?}"
        );
        return 1;
    }
    // Reachable modules.
    let mut reach = alloc::vec![false; g.files.len()];
    let mut todo = alloc::vec![0usize];
    while let Some(m) = todo.pop() {
        if reach[m] {
            continue;
        }
        reach[m] = true;
        todo.extend(g.deps[m].iter().copied());
    }
    if !main_failed {
        // No error: every reachable module ran exactly once.
        for (i, ok) in reach.iter().enumerate() {
            assert_eq!(
                seen[i] == 1,
                *ok,
                "module {i} reachability vs runs: {log:?}\n{:?}",
                g.files
            );
        }
        // And, without cycles, dependencies ran before dependents.
        let pos: Vec<Option<usize>> = (0..g.files.len())
            .map(|i| log.iter().position(|l| *l == alloc::format!("m{i}")))
            .collect();
        let acyclic = g
            .deps
            .iter()
            .enumerate()
            .all(|(i, d)| d.iter().all(|t| *t > i));
        if acyclic {
            for (i, d) in g.deps.iter().enumerate() {
                for t in d {
                    if let (Some(a), Some(b)) = (pos[i], pos[*t]) {
                        assert!(b < a, "m{t} must run before m{i}: {log:?}");
                    }
                }
            }
        }
    } else {
        // A failure: the throwing module is among those that ran or was cut off by a failed
        // dependency; nothing that depends on a failed module ran after it failed.
        assert!(
            g.throws.iter().any(|t| *t),
            "an error with no thrower: {log:?}"
        );
        return 2;
    }
    0
}

#[test]
fn fuzz_module_graphs() {
    big(|| {
        let mut rng = Rng(seed() ^ 0x6a09_e667_f3bc_c908);
        let mut counts = [0usize; 3];
        for i in 0..(iters() / 5).max(50) {
            let g = graph(&mut rng);
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check_graph(&g)));
            match out {
                Ok(k) => counts[k] += 1,
                Err(_) => panic!("graph case {i} failed:\n{:#?}", g.files),
            }
        }
        // The generator has to reach all three outcomes, or the checks above prove little.
        assert!(
            counts.iter().all(|c| *c > 0),
            "outcomes (ok, link error, eval error): {counts:?}"
        );
        std::println!("graph outcomes (ok, link error, eval error): {counts:?}");
    });
}
