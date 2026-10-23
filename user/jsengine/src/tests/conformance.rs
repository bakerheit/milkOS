//! Conformance tests written from the spec: each file in tests/js is a script that uses the
//! little harness below (assert, assertEq, assertThrows, test). Async tests register with
//! `asyncTest` and are checked after the job queue drains.

use crate::*;
use alloc::string::String;
use alloc::vec::Vec;

const HARNESS: &str = r#"
var __failures = [];
var __count = 0;
var __async = [];
function __fmt(v) {
  try {
    if (typeof v === 'string') return JSON.stringify(v);
    if (typeof v === 'number' && v === 0 && 1 / v < 0) return '-0';
    if (Array.isArray(v)) return '[' + v.map(__fmt).join(', ') + ']';
    return String(v);
  } catch (e) { return '<unprintable>'; }
}
function __same(a, b) {
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) return false;
    for (var i = 0; i < a.length; i++) if (!__same(a[i], b[i])) return false;
    return true;
  }
  return Object.is(a, b);
}
function assert(c, msg) { if (!c) throw new Error('assertion failed: ' + (msg || '')); }
function assertEq(a, b, msg) {
  if (!__same(a, b)) throw new Error((msg ? msg + ': ' : '') + 'expected ' + __fmt(b) + ', got ' + __fmt(a));
}
function assertThrows(ctor, f, msg) {
  try { f(); } catch (e) {
    if (ctor && !(e instanceof ctor)) throw new Error((msg || '') + ' threw the wrong error: ' + e);
    return e;
  }
  throw new Error((msg || '') + ' did not throw');
}
function test(name, f) {
  __count++;
  try { f(); } catch (e) { __failures.push(name + ': ' + (e && e.stack ? e.stack : e)); }
}
function asyncTest(name, f) {
  __count++;
  var rec = { name: name, done: false, err: null };
  __async.push(rec);
  try {
    Promise.resolve(f()).then(function () { rec.done = true; }, function (e) { rec.done = true; rec.err = e; });
  } catch (e) { rec.done = true; rec.err = e; }
}
function __finish() {
  for (var i = 0; i < __async.length; i++) {
    var r = __async[i];
    if (!r.done) __failures.push(r.name + ': never finished');
    else if (r.err) __failures.push(r.name + ': ' + (r.err && r.err.stack ? r.err.stack : r.err));
  }
  return __failures.length ? __failures.join('\n') : 'ok ' + __count;
}
"#;

pub(crate) fn run_file(name: &str, src: &str) -> (bool, String, usize) {
    let mut r = Realm::new(Options::default());
    r.set_clock(alloc::boxed::Box::new(|| 1792591380000.0));
    // Debug builds use much more stack per level than release ones.
    r.set_stack_limit(6 << 20);
    let mut limits = r.limits();
    limits.max_steps = 200_000_000;
    r.set_limits(limits);
    if let Err(e) = r.eval(HARNESS, "harness.js") {
        return (false, alloc::format!("harness: {}", r.report(&e)), 0);
    }
    if let Err(e) = r.eval(src, name) {
        return (
            false,
            alloc::format!("{name}: uncaught {}", r.report(&e)),
            0,
        );
    }
    for _ in 0..100 {
        match r.run_jobs() {
            Ok(()) => break,
            Err(e) => {
                return (
                    false,
                    alloc::format!("{name}: job threw {}", r.report(&e)),
                    0,
                );
            }
        }
    }
    let out = match r.eval("__finish()", "finish.js") {
        Ok(v) => r.get_string(v).unwrap_or_default(),
        Err(e) => alloc::format!("finish: {}", r.report(&e)),
    };
    let n = out
        .strip_prefix("ok ")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    (out.starts_with("ok "), alloc::format!("{name}:\n{out}"), n)
}

macro_rules! js_files {
    ($($name:literal),* $(,)?) => {
        const FILES: &[(&str, &str)] = &[$(($name, include_str!(concat!("../../tests/js/", $name)))),*];
    };
}

js_files!(
    "basics.js",
    "functions.js",
    "objects.js",
    "classes.js",
    "destructuring.js",
    "control.js",
    "iterators.js",
    "async.js",
    "arrays.js",
    "strings.js",
    "numbers.js",
    "json.js",
    "regexp.js",
    "date.js",
    "errors.js",
    "collections.js",
    "misc.js",
);

#[test]
fn conformance() {
    // The Browser runs script on its 8 MB main thread; do the same here.
    let h = std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(conformance_inner)
        .unwrap();
    h.join().unwrap();
}

fn conformance_inner() {
    let mut failures: Vec<String> = Vec::new();
    let mut total = 0;
    for (name, src) in FILES {
        // A big stack: deep recursion tests run on this thread.
        let (ok, msg, n) = run_file(name, src);
        total += n;
        if !ok {
            failures.push(msg);
        }
    }
    assert!(
        failures.is_empty(),
        "{} file(s) failed:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    assert!(total > 0);
}
