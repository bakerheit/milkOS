//! Fuzzing: random token soup, mutated test scripts, and randomly generated programs, each
//! run under tight limits. The engine must never panic, and every case must end (by
//! completing, throwing, or being stopped) quickly with bounded memory.
//!
//! JSENGINE_FUZZ_ITERS=N runs N cases of each kind (default 400); JSENGINE_FUZZ_SEED picks
//! the seed.

use crate::*;
use alloc::string::String;
use alloc::vec::Vec;

pub(super) struct Rng(pub u64);

impl Rng {
    pub(super) fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub(super) fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    pub(super) fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
    pub(super) fn chance(&mut self, pct: u64) -> bool {
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

pub(super) fn token_soup(rng: &mut Rng) -> String {
    let n = 1 + rng.below(120);
    let mut s = String::new();
    for _ in 0..n {
        s.push_str(rng.pick(TOKENS));
        s.push(' ');
    }
    s
}

pub(super) fn mutate(rng: &mut Rng, src: &str) -> String {
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

/// Programs over ArrayBuffer, typed arrays, DataView, TextEncoder/Decoder, atob/btoa and
/// structuredClone, with hostile arguments (huge or negative lengths, NaN, objects whose
/// valueOf detaches a buffer, callbacks that throw).
fn typed_program(rng: &mut Rng) -> String {
    const KINDS: &[&str] = &[
        "Int8Array",
        "Uint8Array",
        "Uint8ClampedArray",
        "Int16Array",
        "Uint16Array",
        "Int32Array",
        "Uint32Array",
        "Float32Array",
        "Float64Array",
    ];
    const NUMS: &[&str] = &[
        "0",
        "1",
        "2",
        "3",
        "7",
        "16",
        "-1",
        "-3",
        "1.5",
        "NaN",
        "Infinity",
        "-Infinity",
        "1e10",
        "2**32",
        "2**31",
        "'2'",
        "'x'",
        "null",
        "undefined",
        "true",
        "-0",
        "255",
        "256",
        "65536",
        "1e300",
        "{ valueOf() { return 2 } }",
        "{ valueOf() { buf.transfer(); return 1 } }",
        "{ valueOf() { throw new RangeError('v') } }",
        "Symbol()",
    ];
    const CTOR_ARGS: &[&str] = &[
        "",
        "0",
        "4",
        "16",
        "-1",
        "1e10",
        "NaN",
        "'3'",
        "[1, 2, 3]",
        "[1.5, -2, 300, NaN]",
        "{ length: 3, 0: 1 }",
        "{ length: 1e10 }",
        "buf",
        "buf, 4",
        "buf, 4, 2",
        "buf, 3",
        "buf, 0, 100",
        "u8",
        "f64",
        "new Set([1, 2])",
        "'abc'",
        "{}",
        "null",
        "Symbol()",
        "{ [Symbol.iterator]() { throw 1 } }",
        "{ length: 2, get 0() { buf.transfer(); return 5 } }",
    ];
    const METHODS: &[&str] = &[
        "at",
        "copyWithin",
        "entries",
        "every",
        "fill",
        "filter",
        "find",
        "findIndex",
        "findLast",
        "findLastIndex",
        "forEach",
        "includes",
        "indexOf",
        "join",
        "keys",
        "lastIndexOf",
        "map",
        "reduce",
        "reduceRight",
        "reverse",
        "set",
        "slice",
        "some",
        "sort",
        "subarray",
        "toLocaleString",
        "toString",
        "values",
        "toReversed",
        "toSorted",
        "with",
    ];
    const FNS: &[&str] = &[
        "(x, i) => x + i",
        "(a, b) => b - a",
        "(a, b) => { buf.transfer(); return 0 }",
        "() => { throw new TypeError('cb') }",
        "x => x > 1",
        "(a, b) => NaN",
        "function () { return this }",
    ];
    let mut out = String::from(
        "var buf = new ArrayBuffer(16), u8 = new Uint8Array(buf), f64 = new Float64Array(4), t, d, r;\n",
    );
    let n = 3 + rng.below(12);
    for _ in 0..n {
        let kind = rng.pick(KINDS);
        let args = rng.pick(CTOR_ARGS);
        out.push_str("try { ");
        match rng.below(10) {
            0..=4 => {
                out.push_str(&alloc::format!("t = new {kind}({args}); "));
                for _ in 0..1 + rng.below(3) {
                    let m = rng.pick(METHODS);
                    let a1 = if rng.chance(40) {
                        rng.pick(FNS)
                    } else {
                        rng.pick(NUMS)
                    };
                    let a2 = rng.pick(NUMS);
                    let a3 = rng.pick(NUMS);
                    out.push_str(&alloc::format!(
                        "try {{ r = t.{m}({a1}, {a2}, {a3}); String(r); if (r && r.next) {{ r.next(); [...r]; }} }} catch (e) {{ String(e) }} "
                    ));
                }
                let i = rng.pick(NUMS);
                let v = rng.pick(NUMS);
                out.push_str(&alloc::format!(
                    "t[{i}] = {v}; r = t[{i}]; delete t[{i}]; ({i}) in t; Object.keys(t).length; "
                ));
            }
            5 => {
                let m = rng.pick(&[
                    "getInt8",
                    "getUint8",
                    "getInt16",
                    "getUint16",
                    "getInt32",
                    "getUint32",
                    "getFloat32",
                    "getFloat64",
                    "setInt8",
                    "setUint8",
                    "setInt16",
                    "setUint16",
                    "setInt32",
                    "setUint32",
                    "setFloat32",
                    "setFloat64",
                ]);
                let a = rng.pick(NUMS);
                let b = rng.pick(NUMS);
                let c = rng.pick(NUMS);
                out.push_str(&alloc::format!(
                    "d = new DataView(buf, {a}, {b}); d.{m}({b}, {a}, {c}); d.byteLength; d.byteOffset; "
                ));
            }
            6 => {
                let a = rng.pick(NUMS);
                let b = rng.pick(NUMS);
                out.push_str(&alloc::format!(
                    "buf.slice({a}, {b}); new ArrayBuffer({a}); buf.transfer({b}); buf = new ArrayBuffer(16); u8 = new Uint8Array(buf); "
                ));
            }
            7 => {
                let a = rng.pick(&[
                    "u8",
                    "buf",
                    "new Uint8Array(0)",
                    "new Uint8Array([0xff, 0xfe, 0xfd])",
                    "new Uint8Array([0xe2, 0x82])",
                    "new Uint8Array([0xf0, 0x9f, 0x98, 0x80])",
                    "new Uint16Array([0xd800])",
                    "'str'",
                    "null",
                    "[1]",
                    "f64",
                ]);
                let label =
                    rng.pick(&["'utf-8'", "'utf-16le'", "'latin1'", "'bogus'", "undefined"]);
                let opts = rng.pick(&["{}", "{ fatal: true }", "{ ignoreBOM: true }", "undefined"]);
                out.push_str(&alloc::format!(
                    "d = new TextDecoder({label}, {opts}); d.decode({a}, {{ stream: true }}); d.decode({a}); d.decode(); \
                     new TextEncoder().encode(String(r)); new TextEncoder().encodeInto('a\\u{{d800}}\\u{{1f600}}', {a}); "
                ));
            }
            8 => {
                let a = rng.pick(&[
                    "''",
                    "'abc'",
                    "'YWJj'",
                    "'YWJj='",
                    "'YW=Jj'",
                    "'\\xff'",
                    "'\\u0100'",
                    "' Y W J j '",
                    "'a'",
                    "'!!!!'",
                    "String(r)",
                    "'A'.repeat(1000)",
                    "undefined",
                ]);
                out.push_str(&alloc::format!(
                    "try {{ atob({a}) }} catch (e) {{}} try {{ btoa({a}) }} catch (e) {{}} "
                ));
            }
            _ => {
                let a = rng.pick(&[
                    "u8",
                    "buf",
                    "f64.subarray(1)",
                    "new DataView(buf, 2, 4)",
                    "[u8, buf]",
                    "{ a: u8, b: { c: buf } }",
                    "new Map([[1, u8]])",
                    "[new Date(0), /x/g, new Error('e')]",
                    "function () {}",
                    "Symbol()",
                    "{ get x() { throw 1 } }",
                    "new Set([buf])",
                ]);
                let o = rng.pick(&[
                    "undefined",
                    "{ transfer: [buf] }",
                    "{ transfer: [u8] }",
                    "{ transfer: [] }",
                    "{ transfer: [buf, buf] }",
                    "null",
                ]);
                out.push_str(&alloc::format!(
                    "r = structuredClone({a}, {o}); String(r); "
                ));
            }
        }
        out.push_str("} catch (e) { String(e) }\n");
    }
    out
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

pub(super) fn iters() -> usize {
    std::env::var("JSENGINE_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1500)
}

pub(super) fn seed() -> u64 {
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

/// What a program does, as text: its value or error (with position and stack), the job queue
/// result and what is left unhandled.
fn observe(src: &str) -> String {
    let mut r = Realm::new(Options::default());
    r.set_limits(Limits {
        max_heap_bytes: 8 << 20,
        max_steps: 100_000,
        max_call_depth: 200,
        ..Limits::default()
    });
    r.set_stack_limit(2 << 20);
    let mut out = String::new();
    match r.eval(src, "fuzz.js") {
        Ok(v) => out.push_str(&r.inspect(v)),
        Err(e) => out.push_str(&alloc::format!("{}", r.report(&e))),
    }
    match r.run_jobs() {
        Ok(()) => {}
        Err(e) => out.push_str(&alloc::format!("|job {}", r.report(&e))),
    }
    for v in r.take_unhandled_rejections() {
        out.push_str(&alloc::format!("|unhandled {}", r.inspect(v)));
    }
    out
}

/// Fused instruction pairs (bytecode::fuse) must change nothing observable: the same
/// results, errors, error positions and stacks with and without them.
#[test]
fn fusion_is_invisible() {
    use core::sync::atomic::Ordering;
    on_big_stack(|| {
        let mut rng = Rng(seed() ^ 0xf05e);
        let corpus: Vec<&str> = super::conformance::FILES.iter().map(|f| f.1).collect();
        let mut cases: Vec<String> = Vec::new();
        for _ in 0..iters() / 3 {
            cases.push(program(&mut rng));
        }
        for _ in 0..iters() / 6 {
            cases.push(typed_program(&mut rng));
        }
        for _ in 0..iters() / 6 {
            let base = corpus[rng.below(corpus.len())];
            let chars: Vec<char> = base.chars().collect();
            let start = rng.below(chars.len());
            let len = 200 + rng.below(1500);
            let window: String = chars[start..(start + len).min(chars.len())]
                .iter()
                .collect();
            cases.push(mutate(&mut rng, &window));
        }
        // Hand-written ones: every fused pair, in error and non-error situations.
        for extra in FUSION_CASES {
            cases.push(String::from(*extra));
        }
        let mut diffs = Vec::new();
        for (i, src) in cases.iter().enumerate() {
            crate::bytecode::NO_FUSE.store(true, Ordering::Relaxed);
            let plain = observe(src);
            crate::bytecode::NO_FUSE.store(false, Ordering::Relaxed);
            let fused = observe(src);
            if plain != fused {
                diffs.push(alloc::format!(
                    "case {i}:\n{src}\n--- unfused:\n{plain}\n--- fused:\n{fused}"
                ));
                if diffs.len() >= 3 {
                    break;
                }
            }
        }
        crate::bytecode::NO_FUSE.store(false, Ordering::Relaxed);
        assert!(diffs.is_empty(), "{}", diffs.join("\n\n"));
    });
}

/// Compiling functions on their first call (lazy.rs) must change nothing observable: the same
/// results, errors, error positions and stacks whether every function is left for later or
/// none is.
#[test]
fn lazy_is_invisible() {
    on_big_stack(|| {
        let mut rng = Rng(seed() ^ 0x1a2b);
        let corpus: Vec<&str> = super::conformance::FILES.iter().map(|f| f.1).collect();
        let mut cases: Vec<String> = Vec::new();
        for _ in 0..iters() / 3 {
            cases.push(program(&mut rng));
        }
        for _ in 0..iters() / 6 {
            cases.push(typed_program(&mut rng));
        }
        for _ in 0..iters() / 6 {
            let base = corpus[rng.below(corpus.len())];
            let chars: Vec<char> = base.chars().collect();
            let start = rng.below(chars.len());
            let len = 200 + rng.below(1500);
            let window: String = chars[start..(start + len).min(chars.len())]
                .iter()
                .collect();
            cases.push(mutate(&mut rng, &window));
        }
        for _ in 0..iters() / 6 {
            cases.push(token_soup(&mut rng));
        }
        for extra in LAZY_CASES {
            cases.push(String::from(*extra));
        }
        let mut diffs = Vec::new();
        for (i, src) in cases.iter().enumerate() {
            crate::set_lazy_min_bytes(Some(u32::MAX - 1));
            let eager = observe(src);
            crate::set_lazy_min_bytes(Some(0));
            let lazy = observe(src);
            // (Where exactly a runaway recursion runs out of stack may differ: compiling a
            // function on its first call needs some native stack too.)
            if eager != lazy && !eager.contains("Maximum call stack") {
                diffs.push(alloc::format!(
                    "case {i}:\n{src}\n--- eager:\n{eager}\n--- lazy:\n{lazy}"
                ));
                if diffs.len() >= 3 {
                    break;
                }
            }
        }
        crate::set_lazy_min_bytes(None);
        assert!(diffs.is_empty(), "{}", diffs.join("\n\n"));
        // (and the lazy path did run)
        assert!(crate::lazy::COMPILES.load(core::sync::atomic::Ordering::Relaxed) > 1000);
    });
}

const LAZY_CASES: &[&str] = &[
    // early errors inside functions that are never called
    "function f() { let a = 1; var a; } 1",
    "function f() { return 1; } function g() { class A { m() { this.#x } } } 1",
    "var o = { m() { 'use strict'; var x = 010; } }; 1",
    "function f(a, a) { 'use strict'; } 1",
    "(function () { break; })",
    "function f() { function g() { let b; let b; } } 1",
    // scoping
    "var x = 'g'; function f() { var x = 'f'; return function () { return x }; } f()()",
    "function f(n) { var r = []; for (let i = 0; i < n; i++) r.push(() => i); return r.map(g => g()).join() } f(4)",
    "function f() { return typeof arguments + [].slice.call((() => arguments)()).length } f(1, 2, 3)",
    "function f() { var a = arguments; return (() => a[0] + arguments[1])() } f(10, 20)",
    "var f = function g() { return typeof g }; f()",
    "var f = function g() { g = 1; return typeof g }; f()",
    "class A { constructor() { this.v = 1 } get x() { return this.v } set x(n) { this.v = n } static s() { return 's' } #p = 5; q() { return this.#p } } var a = new A(); a.x = 7; [a.x, A.s(), a.q()].join()",
    "class B { m() { return 'B' } } class C extends B { m() { return super.m() + 'C' } constructor() { super(); this.z = () => this.m() } } new C().z()",
    "function* g(a) { var b = yield a; yield b * 2 } var it = g(1); it.next(); it.next(5).value",
    "async function f() { await null; return 1 } f().then(v => globalThis.r = v); 0",
    "function F() { if (!new.target) return 'call'; this.t = new.target === F } [F(), new F().t].join()",
    "var o = { a: 1, get b() { return this.a + 1 }, m() { return () => this.a } }; o.m()() + o.b",
    "function f() { 'use strict'; return this } f() === undefined",
    "function f() { return this === globalThis } f()",
    "function outer() { var c = 0; function inc() { return ++c } function get() { return c } return [inc, get] } var [i, g] = outer(); i(); i(); g()",
    "eval('var ev = 1'); (function () { return typeof ev })()",
    "function f() { return f.toString().length } f()",
    "var f = (a, b = 2, ...c) => a + b + c.length; [f(1), f.length, f.name].join()",
    "function f() { try { null.x } catch (e) { return e.stack } } f()",
    "function f() {\n  g();\n}\nfunction g() {\n  throw new Error('boom')\n}\ntry { f() } catch (e) { e.stack }",
    "function deep(n) { return n ? deep(n - 1) + 1 : 0 } try { deep(100000) } catch (e) { e.name }",
    "var s = 0; [1, 2, 3].forEach(function (v) { s += v }); s",
    "function f(a, b) { return a + b } f.call(null, 1, 2) + f.apply(null, [3, 4]) + f.bind(null, 5)(6)",
    "var t = []; for (var i = 0; i < 3; i++) t.push(function () { return i }); t.map(f => f()).join()",
    "label: { function f() { return 1 } break label } typeof f",
    "if (true) { function h() { return 'h' } } h()",
    "'use strict'; { function k() { return 'k' } } typeof k",
];

const FUSION_CASES: &[&str] = &[
    // this.x / local.x, plain and as a method receiver, on missing receivers
    "class A { constructor() { this.x = 1 } get() { return this.x } m() { return this.nope.deeper } } var a = new A(); [a.get(), (() => { try { return a.m() } catch (e) { return e.message + '@' + e.stack } })()].join()",
    "var o = { a: { b: { c: 7 } } }; function f(p) { return p.a.b.c + p.a.b.c } f(o)",
    "var u; function g(p) { return p.a.b } try { g(u) } catch (e) { e.message + e.stack }",
    "var u = { a: null }; function g(p) { return p.a.b } try { g(u) } catch (e) { e.message + e.stack }",
    "function g(p, q) { return p.x + q.y } try { g({ x: 1 }, null) } catch (e) { e.message + e.stack }",
    "function h(a, b) { return a + b } h(1, 2) + h('a', 'b')",
    // compares feeding branches, with objects whose valueOf throws or counts
    "var n = 0; var o = { valueOf() { n++; return 5 } }; function f(a, b) { if (a < b) return 'lt'; else if (a > b) return 'gt'; else return 'eq' } [f(o, 6), f(7, o), f(o, o), n].join()",
    "var o = { valueOf() { throw new RangeError('v') } }; function f(a) { if (a <= 3) return 1; return 2 } try { f(o) } catch (e) { e.name + e.stack }",
    "function f(a, b) { if (a === b) return 1; if (a !== b) return 2 } [f(1, 1), f('a', 'b'), f(NaN, NaN), f(null, null), f(undefined, null)].join()",
    "function f(a) { if (a === 1) return 'one'; if (a === null) return 'null'; if (a !== null) return 'other' } [f(1), f(null), f(2), f('1')].join()",
    "function f(a) { return a === 3 } [f(3), f('3'), f(3.5)].join()",
    "function f(a) { return a === null || a !== null } f(0)",
    // setting then discarding; frozen and accessor targets
    "'use strict'; var o = Object.freeze({ a: 1 }); try { o.a = 2; } catch (e) { e.message + e.stack }",
    "var log = []; var o = { set a(v) { log.push(v) } }; o.a = 5; o.a = 6; log.join()",
    "var p = { set z(v) { this._z = v } }; var o = Object.create(p); o.z = 1; o.q = 2; [o._z, o.q, Object.keys(o).join()].join()",
    "var o = {}; Object.defineProperty(o, 'r', { value: 1, writable: false }); o.r = 2; var d = Object.create(o); d.r = 3; [o.r, d.r, d.hasOwnProperty('r')].join()",
    "var a = [1, 2, 3]; a.foo = 1; a.length = 1; a.bar = 2; [a.length, a.foo, a.bar].join()",
    "var s = 'hello'; [s.length, s.charAt(1), s.nope, s.toUpperCase()].join()",
    "var o = { get g() { return this.k }, k: 4 }; function f(x) { return x.g + x.g } f(o)",
    // jump into the middle of a pair: loops and conditionals
    "var s = 0; for (var i = 0; i < 10; i++) { var o = { v: i }; s += o.v; if (o.v === 5) continue; s += 1 } s",
    "function f(n) { var s = 0; while (n > 0) { s += n; n--; if (n < 3) break } return s } f(10)",
    "function f(a) { return a ? a.b.c : a.d } try { f({ b: { c: 1 } }) + f(0) } catch (e) { e.message + e.stack }",
    "function f(a, b) { try { return a.x.y + b.z } catch (e) { return 'caught ' + e.message } finally { var t = a } } [f({ x: { y: 1 } }, { z: 2 }), f(null, 1), f({ x: {} }, 1)].join()",
    "function* g(o) { var a = o.x; yield a; yield o.y.z; } var it = g({ x: 1, y: { z: 2 } }); [it.next().value, it.next().value, it.next().done].join()",
    "async function f(o) { var a = await o.p; return a.q.r } f({ p: { q: { r: 5 } } }).then(v => { globalThis.out = v }); 1",
    "var x = { a: 1 }; with_ = function (o) { return o.a === 1 ? o.a : o.b }; with_(x)",
    "class B { constructor() { this.v = 1 } } class D extends B { constructor() { super(); this.w = this.v === 1 ? this.v : 0 } } new D().w",
    "var o = { a: 1 }; function f() { return o.a === o.a } f()",
    "function f(a, b) { return a.length === b.length } f([1, 2], 'ab')",
    "var o; function f() { return o.a.b } try { f() } catch (e) { e.stack }",
    "function f(a) { var t = a.x; var u = a.y; return t + u } try { f({ x: 1, get y() { throw new TypeError('in getter') } }) } catch (e) { e.stack }",
];

/// Programs that put proxies in front of every kind of target and every operation, with traps
/// that return odd things, throw, revoke the proxy, change the target, or call back into the
/// proxy.
fn proxy_program(rng: &mut Rng) -> String {
    const TARGETS: &[&str] = &[
        "{}", "{a: 1, b: 2}", "[1, 2, 3]", "function f(x) { return x }", "class C { constructor(v) { this.v = v } }",
        "new Map([[1, 2]])", "Object.freeze({z: 1})", "Object.preventExtensions({q: 1})", "Object.create(null)",
        "(function () { var o = {}; Object.defineProperty(o, 'k', {value: 1}); return o })()", "new Date(0)", "p",
        "new Proxy({}, {})", "function () { return this }",
    ];
    const RESULTS: &[&str] = &[
        "undefined", "null", "true", "false", "1", "'a'", "{}", "[]", "['a', 'b']", "['a', 'a']", "[1]", "Symbol('s')",
        "function () {}", "Object.prototype", "Array.prototype", "p", "r", "{value: 1, configurable: true}",
        "{value: 1, configurable: false}", "{get: undefined, configurable: true}", "target", "NaN",
    ];
    const OPS: &[&str] = &[
        "p.a", "p.a = 1", "p[0]", "p[0] = 5", "'a' in p", "delete p.a", "Object.keys(p)", "Object.getOwnPropertyNames(p)",
        "Reflect.ownKeys(p)", "Object.entries(p)", "JSON.stringify(p)", "Object.getPrototypeOf(p)", "Object.setPrototypeOf(p, null)",
        "p instanceof Object", "Object.isFrozen(p)", "Object.freeze(p)", "Object.seal(p)", "Object.preventExtensions(p)",
        "Object.isExtensible(p)", "Object.defineProperty(p, 'n', {value: 1})", "Object.getOwnPropertyDescriptor(p, 'a')",
        "Object.assign({}, p)", "({...p})", "for (var k in p) {}", "for (var v of p) {}", "p()", "new p()", "p.call(null, 1)",
        "Array.isArray(p)", "Object.prototype.toString.call(p)", "[].concat(p)", "Array.prototype.slice.call(p)",
        "Array.from(p)", "p.length", "p.push && p.push(1)", "String(p)", "p + ''", "structuredClone(p)", "Object.create(p).a",
        "Reflect.get(p, 'a', {})", "Reflect.set(p, 'a', 1, {})", "p.hasOwnProperty('a')", "Object.fromEntries([['a', p]])",
        "r.revoke()", "target.a = 9", "Object.defineProperty(target, 'a', {value: 3, configurable: false})",
        "Object.freeze(target)", "p.__proto__", "p.__proto__ = {}", "Reflect.apply(p, null, [])", "Reflect.construct(p, [])",
        "[...p]", "Math.max(...p)", "new Set(p)", "new Map(p)", "Promise.resolve(p)", "(async () => await p)()",
    ];
    const TRAPS: &[&str] = &[
        "get", "set", "has", "deleteProperty", "ownKeys", "getOwnPropertyDescriptor", "defineProperty", "getPrototypeOf",
        "setPrototypeOf", "isExtensible", "preventExtensions", "apply", "construct",
    ];
    let mut s = String::from("var log = []; var p, r, target; function t(f) { try { var v = f(); log.push(typeof v) } catch (e) { log.push(e && e.name) } }\n");
    for _ in 0..(1 + rng.below(3)) {
        let tgt = rng.pick(TARGETS);
        s.push_str(&alloc::format!("target = {tgt}; var h = {{"));
        for tr in TRAPS {
            if rng.chance(45) {
                let body = match rng.below(6) {
                    0 => alloc::format!("return {}", rng.pick(RESULTS)),
                    1 => String::from("throw new RangeError('trap')"),
                    2 => String::from("r.revoke(); return Reflect.get.apply(null, arguments)"),
                    3 => alloc::format!("{}; return Reflect.{tr}.apply(null, arguments)", rng.pick(OPS)),
                    4 => alloc::format!("return Reflect.apply(Reflect.{tr}, null, arguments)"),
                    _ => alloc::format!("return Reflect.{tr}.apply(null, arguments)"),
                };
                s.push_str(&alloc::format!("{tr}: function () {{ {body} }}, "));
            }
        }
        s.push_str("}; r = Proxy.revocable(target, h); p = r.proxy;\n");
        for _ in 0..(3 + rng.below(8)) {
            let op = rng.pick(OPS);
            let ret = if op.starts_with("for") { "" } else { "return " };
            s.push_str(&alloc::format!("t(function () {{ {ret}{op} }});\n"));
        }
        if rng.chance(30) {
            s.push_str("p = new Proxy(p, h); t(function () { return p.a }); t(function () { return Object.keys(p) });\n");
        }
    }
    s.push_str("log.length");
    s
}

#[test]
fn fuzz_typed_programs() {
    on_big_stack(|| fuzz_with("typed", &typed_program));
}

/// The generator makes valid programs (so the fuzzing exercises the builtins, not the parser)
/// that do run to the end often enough.
#[test]
fn typed_programs_are_valid() {
    on_big_stack(|| {
        let mut rng = Rng(seed() ^ 0x1234);
        let mut bad = 0;
        let mut r = Realm::new(Options::default());
        for _ in 0..300 {
            let src = typed_program(&mut rng);
            if let Err(e) = r.check_syntax(&src, "t.js") {
                bad += 1;
                if bad == 1 {
                    std::println!("{e}\n{src}");
                }
            }
        }
        assert_eq!(bad, 0, "generated programs with syntax errors");
    });
}

/// The typed-array programs under GC stress as well.
#[test]
fn fuzz_typed_gc_stress() {
    on_big_stack(|| {
        let mut rng = Rng(seed() ^ 0x77aa);
        for i in 0..iters() / 4 {
            let src = typed_program(&mut rng);
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut r = Realm::new(Options::default());
                r.heap.stress = 2;
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
                panic!("typed gc stress case {i} panicked; source:\n{src}");
            }
        }
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

#[test]
fn fuzz_proxy_programs() {
    on_big_stack(|| fuzz_with("proxy", &proxy_program));
}

#[test]
fn proxy_programs_are_valid() {
    on_big_stack(|| {
        let mut rng = Rng(seed() ^ 0x5151);
        let mut r = Realm::new(Options::default());
        for _ in 0..300 {
            let src = proxy_program(&mut rng);
            if let Err(e) = r.check_syntax(&src, "t.js") {
                panic!("{e}\n{src}");
            }
        }
    });
}

/// The proxy programs under GC stress as well.
#[test]
fn fuzz_proxy_gc_stress() {
    on_big_stack(|| {
        let mut rng = Rng(seed() ^ 0x9191);
        for i in 0..iters() / 4 {
            let src = proxy_program(&mut rng);
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut r = Realm::new(Options::default());
                r.heap.stress = 2;
                r.set_limits(Limits {
                    max_heap_bytes: 8 << 20,
                    max_steps: 200_000,
                    max_call_depth: 200,
                    ..Limits::default()
                });
                let _ = r.eval(&src, "fuzz.js");
                let _ = r.run_jobs();
            }));
            if out.is_err() {
                panic!("proxy gc stress case {i} panicked; source:\n{src}");
            }
        }
    });
}
