// Synthetic "bundle" generator for the large-script tests and the guest benchmark. It is
// `include!`d by the host tests (user/jsengine/src/tests/bigscripts.rs) and by user/jsbench,
// so it only uses `alloc` and `core`. Everything it makes is original, generated text that
// imitates the shape and size of minified bundles (webpack-style module tables, closure
// compiler style IIFEs, big data literals, long expression chains, a very large function); it
// is never a copy of anyone's script.
//
// `gen_bundle(kind, target_bytes, call_every)` returns the source and the number the script
// leaves in `globalThis.__r` when it runs; `call_every` is how sparsely the bundle's functions
// are called (1 = all of them, 100 = every hundredth), the way a page runs a small part of a
// big library.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

pub const BUNDLE_KINDS: &[&str] = &["funcs", "modules", "closures", "literals", "chains", "small", "huge"];

pub struct Bundle {
    pub src: String,
    /// What `globalThis.__r` holds after the script ran.
    pub expect: f64,
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn short_name(mut i: usize) -> String {
    // a, b, ..., z, A, ..., Z, aa, ab, ...: what a minifier hands out.
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut s = String::new();
    loop {
        s.push(A[i % A.len()] as char);
        i /= A.len();
        if i == 0 {
            break;
        }
        i -= 1;
    }
    s
}

pub fn gen_bundle(kind: &str, target: usize, call_every: usize) -> Bundle {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ (target as u64) ^ ((kind.len() as u64) << 32));
    let ce = call_every.max(1);
    match kind {
        "funcs" => funcs(&mut rng, target, ce),
        "modules" => modules(&mut rng, target, ce),
        "closures" => closures(&mut rng, target, ce),
        "literals" => literals(&mut rng, target),
        "chains" => chains(&mut rng, target),
        "small" => small(&mut rng, target, ce),
        "huge" => huge(&mut rng, target),
        _ => Bundle {
            src: String::from("globalThis.__r=0;"),
            expect: 0.0,
        },
    }
}

/// Plain top-level functions of a few shapes, then a table of them and a loop calling some.
fn funcs(rng: &mut Rng, target: usize, ce: usize) -> Bundle {
    let mut s = String::new();
    let mut vals: Vec<f64> = Vec::new();
    while s.len() + 40 * (vals.len() / 4 + 1) < target {
        let i = vals.len();
        let k1 = 1 + rng.below(90) as i64;
        let k2 = rng.below(1000) as i64;
        // f(2, 1) = 2 * k1 + k2 whatever the shape.
        let v = match rng.below(3) {
            0 => {
                let _ = write!(
                    s,
                    "function f{i}(a,b){{var c=a*{k1};if(c>=0){{c+={k2}}}else{{c+={k2}}}for(var d=0;d<2;d++){{c+=0}}return c}}"
                );
                2 * k1 + k2
            }
            1 => {
                let _ = write!(
                    s,
                    "function f{i}(a,b){{var o={{x:a,y:{k1},z:[{k2},b]}};o.w=o.x*o.y;try{{if(o.z.length>5)throw 1}}catch(e){{o.w=0}}return o.w+o.z[0]+b*0}}"
                );
                2 * k1 + k2
            }
            _ => {
                let _ = write!(
                    s,
                    "function f{i}(a,b){{var t=\"s{i}\";return t.length>0&&a>0?(a<<0)*{k1}+{k2}+(b-b):-1}}"
                );
                2 * k1 + k2
            }
        };
        vals.push(v as f64);
    }
    let n = vals.len();
    s.push_str("var F=[");
    for i in 0..n {
        let _ = write!(s, "f{i},");
    }
    s.push_str("];var t=0;");
    let _ = write!(s, "for(var i=0;i<{n};i+={ce})t+=F[i](2,1);globalThis.__r=t;");
    let mut expect = 0.0;
    let mut i = 0;
    while i < n {
        expect += vals[i];
        i += ce;
    }
    Bundle { src: s, expect }
}

/// A module-table bundle: a loader, then an array of module functions (`function(e,t,n){...}`)
/// that require each other and export numbers.
fn modules(rng: &mut Rng, target: usize, ce: usize) -> Bundle {
    let mut mods = String::new();
    let mut vals: Vec<f64> = Vec::new();
    while mods.len() < target {
        let i = vals.len();
        let k = 1 + rng.below(500) as i64;
        let dep = if i > 1 && rng.below(3) == 0 {
            Some(1 + rng.below(i as u64 - 1) as usize)
        } else {
            None
        };
        if i > 0 {
            mods.push(',');
        }
        if i == 0 {
            // Module 0 is the entry: it requires every ce-th module and sums their `v`.
            mods.push_str("function(e,t,n){for(var r=0,o=1;o<N;o+=CE)r+=n(o).v;e.exports=r}");
            vals.push(0.0);
            continue;
        }
        let _ = write!(
            mods,
            "function(e,t,n){{\"use strict\";var r=n.d,o={{}},i=function(){{var a=[];for(var u=0;u<3;u++)a.push(function(c){{return c+u+{k}}});return a}}();"
        );
        // A few helpers that never run unless called, the way library code is.
        for h in 0..3 + rng.below(4) {
            let m = 1 + rng.below(50);
            let _ = write!(
                mods,
                "function h{h}(s,l){{var p=[],q={{}};for(var z=0;z<s.length;z++){{if(s[z]>{m}){{p.push(s[z]*{m})}}else{{q[\"k\"+z]=l?l(s[z]):s[z]}}}}return p.length+Object.keys(q).length}}"
            );
        }
        match dep {
            Some(d) => {
                let _ = write!(mods, "var d=n({d}).v;t.v=i[0](0)+d*0;e.exports=t}}");
            }
            None => {
                let _ = write!(mods, "t.v=i[0](0);e.exports=t}}");
            }
        }
        // i[0](0) = 0 + u + k, and u ended the loop at 3.
        vals.push((3 + k) as f64);
    }
    let n = vals.len();
    let head = "!function(e){var t={};function n(r){if(t[r])return t[r].exports;var o=t[r]={i:r,l:!1,exports:{}};return e[r].call(o.exports,o,o.exports,n),o.l=!0,o.exports}n.d=1;globalThis.__r=n(0)}([";
    let mut s = String::from(head);
    let body = mods.replace("N;o+=CE", &format!("{n};o+={ce}"));
    s.push_str(&body);
    s.push_str("]);");
    let mut expect = 0.0;
    let mut o = 1;
    while o < n {
        expect += vals[o];
        o += ce;
    }
    Bundle { src: s, expect }
}

/// One closure-compiler style IIFE with lots of nested closures sharing variables.
fn closures(rng: &mut Rng, target: usize, ce: usize) -> Bundle {
    let mut s = String::from("(function(){var G=0;");
    let mut table = String::new();
    let mut vals: Vec<f64> = Vec::new();
    while s.len() + table.len() < target {
        let n = vals.len();
        let k = 1 + rng.below(40) as i64;
        let j = rng.below(100) as i64;
        let _ = write!(
            s,
            "function m{n}(x){{var y=x*{k};var o={{get:function(){{return y+{j}}},add:function(v){{y+=v;G+=0;return this}},arr:function(){{return[1,2,3].map(function(e){{return e*{k}+y*0}})}}}};return function(z){{return o.get()+z-z}}}}"
        );
        let _ = write!(table, "m{n},");
        vals.push((3 * k + j) as f64);
    }
    let n = vals.len();
    let _ = write!(
        s,
        "var M=[{table}];var t=0;for(var i=0;i<{n};i+={ce})t+=M[i](3)(0);globalThis.__r=t;}})();"
    );
    let mut expect = 0.0;
    let mut i = 0;
    while i < n {
        expect += vals[i];
        i += ce;
    }
    Bundle { src: s, expect }
}

/// Big data literals: arrays of numbers and strings, objects of objects.
fn literals(rng: &mut Rng, target: usize) -> Bundle {
    let mut s = String::from("var D={");
    let mut count = 0usize;
    let mut first = true;
    let mut key = 0usize;
    while s.len() < target {
        if !first {
            s.push(',');
        }
        first = false;
        let k = short_name(key);
        key += 1;
        match rng.below(3) {
            0 => {
                let _ = write!(s, "{k}:[");
                let m = 200 + rng.below(800);
                for j in 0..m {
                    let _ = write!(s, "{},", (j * 7 + rng.below(5)) % 100_000);
                }
                s.push_str("0]");
                count += m as usize + 1;
            }
            1 => {
                let _ = write!(s, "{k}:[");
                let m = 50 + rng.below(200);
                for j in 0..m {
                    let _ = write!(s, "\"item-{}-{}\",", key, j);
                }
                s.push_str("\"\"]");
                count += m as usize + 1;
            }
            _ => {
                let _ = write!(s, "{k}:{{");
                let m = 20 + rng.below(60);
                for j in 0..m {
                    let _ = write!(s, "p{j}:{{id:{j},n:\"v{j}\",t:[{j},{}]}},", j + 1);
                }
                s.push_str("z:null}");
                count += m as usize + 1;
            }
        }
    }
    s.push_str("};var t=0;for(var k in D){var v=D[k];t+=Array.isArray(v)?v.length:Object.keys(v).length}globalThis.__r=t;");
    Bundle {
        src: s,
        expect: count as f64,
    }
}

/// Long expression chains: sums of many terms, and chained calls.
fn chains(rng: &mut Rng, target: usize) -> Bundle {
    let mut s = String::from("var o={a:function(){return this},b:function(){return this},n:7},t=0;");
    let mut expect = 0.0;
    while s.len() < target {
        match rng.below(2) {
            0 => {
                let m = 100 + rng.below(700);
                s.push_str("t+=0");
                for _ in 0..m {
                    let k = rng.below(10);
                    let _ = write!(s, "+{k}");
                    expect += k as f64;
                }
                s.push(';');
            }
            _ => {
                let m = 20 + rng.below(60);
                s.push_str("t+=o");
                for j in 0..m {
                    s.push_str(if j % 2 == 0 { ".a()" } else { ".b()" });
                }
                s.push_str(".n;");
                expect += 7.0;
            }
        }
    }
    s.push_str("globalThis.__r=t;");
    Bundle { src: s, expect }
}

/// Very many tiny functions.
fn small(rng: &mut Rng, target: usize, ce: usize) -> Bundle {
    let mut s = String::new();
    let mut vals: Vec<f64> = Vec::new();
    let mut table = String::new();
    while s.len() + table.len() < target {
        let i = vals.len();
        let k = rng.below(1000);
        let _ = write!(s, "function s{i}(){{return {k}}}");
        let _ = write!(table, "s{i},");
        vals.push(k as f64);
    }
    let n = vals.len();
    let _ = write!(
        s,
        "var T=[{table}];var t=0;for(var i=0;i<{n};i+={ce})t+=T[i]();globalThis.__r=t;"
    );
    let mut expect = 0.0;
    let mut i = 0;
    while i < n {
        expect += vals[i];
        i += ce;
    }
    Bundle { src: s, expect }
}

/// One enormous function made of statements.
fn huge(rng: &mut Rng, target: usize) -> Bundle {
    let mut s = String::from("function main(){var s=0;");
    let mut expect = 0.0;
    while s.len() < target {
        let k = 1 + rng.below(9);
        match rng.below(5) {
            0 => {
                let _ = write!(s, "s+={k};");
            }
            1 => {
                let _ = write!(s, "if(s>=0){{s+={k}}}else{{s+={k}}}");
            }
            2 => {
                let _ = write!(s, "s=s+{k}-0;");
            }
            3 => {
                let _ = write!(s, "{{var v={k};s+=v}}");
            }
            _ => {
                let _ = write!(s, "for(var q=0;q<1;q++){{s+={k}}}");
            }
        }
        expect += k as f64;
    }
    s.push_str("return s}globalThis.__r=main();");
    Bundle { src: s, expect }
}
