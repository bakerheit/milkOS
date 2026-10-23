//! Fuzzing: random token soup, mutated test scripts, and randomly generated programs, each
//! run under tight limits. The engine must never panic, and every case must end (by
//! completing, throwing, or being stopped) quickly with bounded memory.
//!
//! JSENGINE_FUZZ_ITERS=N runs N cases of each kind (default 400); JSENGINE_FUZZ_SEED picks
//! the seed.

use crate::*;
use alloc::string::String;
use alloc::vec::Vec;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

const TOKENS: &[&str] = &[
    "var",
    "let",
    "const",
    "function",
    "function*",
    "async",
    "await",
    "yield",
    "class",
    "extends",
    "super",
    "new",
    "this",
    "return",
    "if",
    "else",
    "for",
    "while",
    "do",
    "break",
    "continue",
    "switch",
    "case",
    "default",
    "try",
    "catch",
    "finally",
    "throw",
    "typeof",
    "instanceof",
    "in",
    "of",
    "delete",
    "void",
    "null",
    "undefined",
    "true",
    "false",
    "get",
    "set",
    "static",
    "import",
    "export",
    "with",
    "debugger",
    "x",
    "y",
    "z",
    "a",
    "b",
    "f",
    "g",
    "o",
    "arguments",
    "eval",
    "0",
    "1",
    "42",
    "3.14",
    "1e309",
    "0x1F",
    "-0",
    "'s'",
    "\"t\"",
    "`tpl`",
    "`a${",
    "}b`",
    "/re/g",
    "/[",
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    ";",
    ",",
    ".",
    "...",
    "?.",
    "?",
    ":",
    "=",
    "==",
    "===",
    "!=",
    "!==",
    "+",
    "-",
    "*",
    "/",
    "%",
    "**",
    "++",
    "--",
    "<<",
    ">>",
    ">>>",
    "&",
    "|",
    "^",
    "!",
    "~",
    "&&",
    "||",
    "??",
    "+=",
    "-=",
    "**=",
    "&&=",
    "||=",
    "??=",
    "=>",
    "<",
    ">",
    "<=",
    ">=",
    "\n",
    " ",
    "#priv",
    "\\u0041",
    "'\\x'",
    "/*",
    "*/",
    "//",
    "<!--",
    "-->",
];

fn token_soup(rng: &mut Rng) -> String {
    let n = 1 + rng.below(120);
    let mut s = String::new();
    for _ in 0..n {
        s.push_str(rng.pick(TOKENS));
        s.push(' ');
    }
    s
}

fn mutate(rng: &mut Rng, src: &str) -> String {
    let mut b: Vec<char> = src.chars().collect();
    let rounds = 1 + rng.below(6);
    for _ in 0..rounds {
        if b.is_empty() {
            break;
        }
        let len = b.len();
        let a = rng.below(len);
        let span = 1 + rng.below(40.min(len - a));
        match rng.below(7) {
            0 => {
                b.drain(a..a + span);
            }
            1 => {
                let piece: Vec<char> = b[a..a + span].to_vec();
                let at = rng.below(b.len());
                for (i, c) in piece.into_iter().enumerate() {
                    b.insert(at + i, c);
                }
            }
            2 => {
                let t: Vec<char> = rng.pick(TOKENS).chars().collect();
                for (i, c) in t.into_iter().enumerate() {
                    b.insert(a + i, c);
                }
            }
            3 => {
                let pool = b"(){}[];,.+-*/=<>!?:'\"`\\0xa\n";
                let c = pool[rng.below(pool.len())] as char;
                b[a] = c;
            }
            4 => b.truncate(a),
            5 => {
                let other = rng.below(len);
                b.swap(a, other);
            }
            _ => {
                // Make a loop run forever-ish.
                let t: Vec<char> = "while(1){".chars().collect();
                for (i, c) in t.into_iter().enumerate() {
                    b.insert(a + i, c);
                }
            }
        }
    }
    b.into_iter().collect()
}

// ---- a random program generator (mostly valid code) ----

struct Gen<'a> {
    rng: &'a mut Rng,
    depth: u32,
    out: String,
}

const VARS: &[&str] = &["a", "b", "c", "o", "arr", "s", "m", "f"];

impl Gen<'_> {
    fn expr(&mut self) {
        self.depth += 1;
        if self.depth > 6 {
            self.atom();
            self.depth -= 1;
            return;
        }
        match self.rng.below(22) {
            0..=3 => self.atom(),
            4 => {
                self.out.push('(');
                self.expr();
                self.out.push(')');
                let op = self.rng.pick(&[
                    " + ",
                    " - ",
                    " * ",
                    " / ",
                    " % ",
                    " ** ",
                    " & ",
                    " | ",
                    " ^ ",
                    " << ",
                    " >> ",
                    " >>> ",
                    " == ",
                    " === ",
                    " != ",
                    " < ",
                    " >= ",
                    " && ",
                    " || ",
                    " ?? ",
                    " instanceof Object ",
                    " in o ",
                ]);
                self.out.push_str(op);
                self.out.push('(');
                self.expr();
                self.out.push(')');
            }
            5 => {
                self.out
                    .push_str(self.rng.pick(&["!", "-", "+", "~", "typeof ", "void "]));
                self.out.push('(');
                self.expr();
                self.out.push(')');
            }
            6 => {
                self.out.push('(');
                self.expr();
                self.out.push_str(" ? ");
                self.expr();
                self.out.push_str(" : ");
                self.expr();
                self.out.push(')');
            }
            7 => {
                self.out.push('[');
                for i in 0..self.rng.below(4) {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    if self.rng.chance(15) {
                        self.out.push_str("...");
                    }
                    self.expr();
                }
                self.out.push(']');
            }
            8 => {
                self.out.push('{');
                for i in 0..self.rng.below(4) {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    let k = self.rng.pick(&["k", "x", "'q'", "[s]", "0", "get g", "m"]);
                    match k {
                        "get g" => self.out.push_str("get g() { return 1 }"),
                        "m" => {
                            self.out.push_str("m() { return ");
                            self.expr();
                            self.out.push_str(" }");
                        }
                        k => {
                            self.out.push_str(k);
                            self.out.push_str(": ");
                            self.expr();
                        }
                    }
                }
                self.out.push('}');
            }
            9 => {
                self.out.push_str("(function (p, q) { ");
                self.stmts(2);
                self.out.push_str(" return p; })(");
                self.expr();
                self.out.push(')');
            }
            10 => {
                self.out.push_str("((p) => ");
                self.expr();
                self.out.push_str(")(");
                self.expr();
                self.out.push(')');
            }
            11 => {
                let v = self.rng.pick(VARS);
                self.out.push('(');
                self.out.push_str(v);
                self.out
                    .push_str(self.rng.pick(&[" = ", " += ", " -= ", " ||= ", " ??= "]));
                self.expr();
                self.out.push(')');
            }
            12 => {
                let m = self.rng.pick(&[
                    "map(x => x)",
                    "filter(Boolean)",
                    "concat([1])",
                    "slice(1)",
                    "join('-')",
                    "reverse()",
                    "sort()",
                    "indexOf(1)",
                    "push(2)",
                    "pop()",
                    "splice(0, 1)",
                    "flat()",
                    "includes(0)",
                    "length",
                    "toString()",
                    "at(-1)",
                    "keys()",
                    "reduce((p, q) => p + q, 0)",
                    "find(x => x)",
                    "fill(0)",
                ]);
                self.out.push_str("[1, 2, 3].");
                self.out.push_str(m);
            }
            13 => {
                let m = self.rng.pick(&[
                    "toUpperCase()",
                    "slice(1, 2)",
                    "split('')",
                    "replace(/a/g, 'b')",
                    "padStart(5)",
                    "repeat(2)",
                    "trim()",
                    "charCodeAt(0)",
                    "indexOf('a')",
                    "match(/(a)(b)?/)",
                    "localeCompare('b')",
                    "normalize()",
                    "at(0)",
                    "codePointAt(0)",
                    "search(/x/)",
                    "concat(1, 2)",
                ]);
                self.out.push_str("String(");
                self.expr();
                self.out.push_str(").");
                self.out.push_str(m);
            }
            14 => {
                let f = self.rng.pick(&[
                    "JSON.stringify",
                    "JSON.parse",
                    "Math.max",
                    "Math.floor",
                    "Number",
                    "parseInt",
                    "String",
                    "Boolean",
                    "Object.keys",
                    "Object.freeze",
                    "Array.from",
                    "Array.isArray",
                    "isNaN",
                    "encodeURIComponent",
                    "Object.entries",
                    "Symbol",
                    "new Date",
                    "new Map",
                    "new Set",
                    "Promise.resolve",
                    "new Error",
                    "Object.assign",
                    "Reflect.ownKeys",
                    "new RegExp",
                ]);
                self.out.push_str(f);
                self.out.push('(');
                self.expr();
                self.out.push(')');
            }
            15 => {
                self.out.push('(');
                self.expr();
                self.out.push(')');
                self.out.push_str(
                    self.rng
                        .pick(&[".x", "?.y", "[0]", ".length", "['k']", "?.()"]),
                );
            }
            16 => {
                self.out.push_str("`t${");
                self.expr();
                self.out.push_str("}u`");
            }
            17 => {
                self.out
                    .push_str("(class extends Object { constructor() { super(); this.v = ");
                self.expr();
                self.out
                    .push_str(" } get w() { return this.v } static s() { return 1 } })");
            }
            18 => {
                self.out.push_str("[...(function* () { yield ");
                self.expr();
                self.out.push_str("; yield* [1, 2] })()]");
            }
            19 => {
                self.out.push_str("(async () => { await ");
                self.expr();
                self.out.push_str("; return 1 })()");
            }
            20 => {
                self.out.push_str("f(");
                self.expr();
                self.out.push(')');
            }
            _ => {
                self.out.push('(');
                self.expr();
                self.out.push_str(", ");
                self.expr();
                self.out.push(')');
            }
        }
        self.depth -= 1;
    }

    fn atom(&mut self) {
        let a = self.rng.pick(&[
            "0",
            "1",
            "-1",
            "2.5",
            "NaN",
            "Infinity",
            "-0",
            "1e21",
            "2147483647",
            "'a'",
            "'ab'",
            "''",
            "\"1\"",
            "null",
            "undefined",
            "true",
            "false",
            "a",
            "b",
            "c",
            "o",
            "arr",
            "s",
            "m",
            "[]",
            "{}",
            "this",
            "Symbol.iterator",
            "Math.PI",
            "new Date(0)",
            "/a+/g",
            "arguments",
            "globalThis",
        ]);
        self.out.push_str(a);
    }

    fn stmts(&mut self, n: usize) {
        for _ in 0..n {
            self.stmt();
        }
    }

    fn stmt(&mut self) {
        self.depth += 1;
        if self.depth > 5 {
            self.out.push('(');
            self.expr();
            self.out.push_str(");\n");
            self.depth -= 1;
            return;
        }
        match self.rng.below(14) {
            0..=3 => {
                self.out.push('(');
                self.expr();
                self.out.push_str(");\n");
            }
            4 => {
                self.out
                    .push_str(self.rng.pick(&["var ", "let ", "const "]));
                let n = self.rng.below(1000);
                let pat = match self.rng.below(4) {
                    0 => alloc::format!("t{n}"),
                    1 => alloc::format!("[t{n}, u{n}]"),
                    2 => alloc::format!("{{ t{n}, u{n} = 1 }}"),
                    _ => alloc::format!("[t{n}, ...u{n}]"),
                };
                self.out.push_str(&pat);
                self.out.push_str(" = ");
                self.expr();
                self.out.push_str(";\n");
            }
            5 => {
                self.out.push_str("if (");
                self.expr();
                self.out.push_str(") { ");
                self.stmts(2);
                self.out.push_str("} else { ");
                self.stmt();
                self.out.push_str("}\n");
            }
            6 => {
                self.out.push_str("for (let i = 0; i < 5; i++) { ");
                self.stmts(2);
                self.out.push_str("}\n");
            }
            7 => {
                self.out.push_str("for (const k of ");
                self.out.push_str(self.rng.pick(&[
                    "arr",
                    "'str'",
                    "[1, 2]",
                    "new Set([1])",
                    "Object.keys(o)",
                ]));
                self.out.push_str(") { ");
                self.stmts(2);
                self.out.push_str("}\n");
            }
            8 => {
                self.out.push_str("try { ");
                self.stmts(2);
                self.out.push_str("} catch (e) { ");
                self.stmt();
                self.out.push_str("} finally { ");
                self.stmt();
                self.out.push_str("}\n");
            }
            9 => {
                self.out.push_str("switch (");
                self.expr();
                self.out.push_str(") { case 1: ");
                self.stmt();
                self.out.push_str(" break; default: ");
                self.stmt();
                self.out.push_str("}\n");
            }
            10 => {
                self.out.push_str("function f(x, y = 1, ...z) { ");
                self.stmts(2);
                self.out.push_str("return x; }\n");
            }
            11 => {
                self.out.push_str("throw ");
                self.expr();
                self.out.push_str(";\n");
            }
            12 => {
                self.out.push_str("while (");
                self.expr();
                self.out.push_str(") { ");
                self.stmts(1);
                self.out.push_str(" break; }\n");
            }
            _ => {
                self.out.push_str("for (const k in ");
                self.expr();
                self.out.push_str(") { ");
                self.stmt();
                self.out.push_str("}\n");
            }
        }
        self.depth -= 1;
    }
}

fn program(rng: &mut Rng) -> String {
    let mut g = Gen {
        rng,
        depth: 0,
        out: String::new(),
    };
    g.out.push_str("var a = 1, b = 'x', c = [1, 'two', {}], o = { k: 1, x: [2] }, arr = [3, 1, 2], s = 'abc', m = new Map([[1, 2]]);\nfunction f(v) { return v; }\n");
    let n = 2 + g.rng.below(10);
    for _ in 0..n {
        // Most statements are wrapped so one runtime error doesn't end the program.
        let wrap = g.rng.chance(75);
        if wrap {
            g.out.push_str("try { ");
        }
        g.stmt();
        if wrap {
            g.out.push_str("} catch (e) { String(e); }\n");
        }
    }
    g.out
}

fn run_case(src: &str) {
    let mut r = Realm::new(Options::default());
    r.set_limits(Limits {
        max_heap_bytes: 8 << 20,
        max_steps: 200_000,
        max_call_depth: 200,
        ..Limits::default()
    });
    r.set_stack_limit(2 << 20);
    let t = std::time::Instant::now();
    let res = r.eval(src, "fuzz.js");
    if let Err(e) = &res {
        let rep = r.report(e);
        let _ = alloc::format!("{rep}");
    } else if let Ok(v) = res {
        let _ = r.inspect(v);
    }
    for _ in 0..4 {
        if r.run_jobs().is_ok() {
            break;
        }
    }
    let _ = r.take_unhandled_rejections();
    r.release_temps();
    r.gc();
    let el = t.elapsed();
    assert!(el.as_secs() < 20, "case took {el:?}:\n{src}");
    assert!(
        r.heap_stats().used_bytes < 64 << 20,
        "case kept {} bytes:\n{src}",
        r.heap_stats().used_bytes
    );
}

fn iters() -> usize {
    std::env::var("JSENGINE_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1500)
}

fn seed() -> u64 {
    std::env::var("JSENGINE_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5eed_f00d_1234_9876)
}

fn fuzz_with(kind: &str, make: &dyn Fn(&mut Rng) -> String) {
    let mut rng = Rng(seed() ^ (kind.len() as u64 * 0x9E37_79B9));
    for i in 0..iters() {
        let src = make(&mut rng);
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_case(&src)));
        if out.is_err() {
            panic!("{kind} case {i} panicked; source:\n{src}");
        }
    }
}

fn on_big_stack(f: fn()) {
    let h = std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(f)
        .unwrap();
    h.join().unwrap();
}

#[test]
fn fuzz_token_soup() {
    on_big_stack(|| fuzz_with("soup", &token_soup));
}

#[test]
fn fuzz_mutated_tests() {
    on_big_stack(|| {
        let corpus: Vec<&str> = super::conformance::FILES.iter().map(|f| f.1).collect();
        fuzz_with("mutate", &|rng: &mut Rng| {
            let base = corpus[rng.below(corpus.len())];
            // Cut a window so cases stay small.
            let chars: Vec<char> = base.chars().collect();
            let start = rng.below(chars.len());
            let len = 200 + rng.below(1500);
            let window: String = chars[start..(start + len).min(chars.len())]
                .iter()
                .collect();
            mutate(rng, &window)
        });
    });
}

#[test]
fn fuzz_generated_programs() {
    on_big_stack(|| fuzz_with("program", &program));
}

/// Generated programs also run under GC stress.
#[test]
fn fuzz_generated_gc_stress() {
    on_big_stack(|| {
        let mut rng = Rng(seed() ^ 0xabcdef);
        for i in 0..iters() / 4 {
            let src = program(&mut rng);
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut r = Realm::new(Options::default());
                r.heap.stress = 3;
                r.set_limits(Limits {
                    max_heap_bytes: 8 << 20,
                    max_steps: 100_000,
                    max_call_depth: 200,
                    ..Limits::default()
                });
                let _ = r.eval(&src, "fuzz.js");
                let _ = r.run_jobs();
            }));
            if out.is_err() {
                panic!("gc stress case {i} panicked; source:\n{src}");
            }
        }
    });
}

#[test]
#[ignore]
fn fuzz_stats() {
    let mut rng = Rng(seed());
    let mut ok = 0;
    let mut syntax = 0;
    let mut thrown = 0;
    let mut term = 0;
    for i in 0..300 {
        let src = program(&mut rng);
        let mut r = Realm::new(Options::default());
        r.set_limits(Limits {
            max_heap_bytes: 8 << 20,
            max_steps: 200_000,
            max_call_depth: 200,
            ..Limits::default()
        });
        match r.eval(&src, "fuzz.js") {
            Ok(_) => ok += 1,
            Err(e) => {
                if e.is_uncatchable() {
                    term += 1;
                } else {
                    let rep = r.report(&e);
                    if rep.name == "SyntaxError" {
                        syntax += 1;
                        if syntax < 4 {
                            std::println!("--- {i}: {rep}\n{src}");
                        }
                    } else {
                        thrown += 1;
                    }
                }
            }
        }
    }
    std::println!("ok {ok} syntax {syntax} thrown {thrown} term {term}");
}

/// JSENGINE_FUZZ_REPRO=index (with JSENGINE_FUZZ_SEED): writes generated program `index`
/// to JSENGINE_FUZZ_OUT.
#[test]
#[ignore]
fn fuzz_repro() {
    let idx: usize = std::env::var("JSENGINE_FUZZ_REPRO")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut rng = Rng(seed() ^ ("program".len() as u64 * 0x9E37_79B9));
    let mut src = String::new();
    for _ in 0..=idx {
        src = program(&mut rng);
    }
    let out = std::env::var("JSENGINE_FUZZ_OUT").unwrap_or_else(|_| String::from("fuzz-case.js"));
    std::fs::write(&out, &src).unwrap();
}

/// Runs JSENGINE_FUZZ_FILE with the fuzz limits and prints how long each statement took.
#[test]
#[ignore]
fn fuzz_run_file() {
    let path = std::env::var("JSENGINE_FUZZ_FILE").unwrap();
    let src = std::fs::read_to_string(path).unwrap();
    let t = std::time::Instant::now();
    let mut r = Realm::new(Options::default());
    r.set_limits(Limits {
        max_heap_bytes: 8 << 20,
        max_steps: 200_000,
        max_call_depth: 200,
        ..Limits::default()
    });
    let res = r.eval(&src, "fuzz.js");
    match res {
        Ok(_) => std::println!("ok {:?} steps {}", t.elapsed(), r.steps()),
        Err(e) => std::println!("err {} {:?} steps {}", r.report(&e), t.elapsed(), r.steps()),
    }
}
