//! Runaway scripts: step budgets, the interrupt callback, memory budgets, deep recursion,
//! catastrophic regexps and endless job chains all stop cleanly, and the realm stays usable
//! (or, after running out of memory, at least never crashes).

use crate::*;
use alloc::boxed::Box;
use alloc::rc::Rc;
use core::cell::Cell;

fn realm_with(limits: Limits) -> Realm {
    let mut r = Realm::new(Options::default());
    r.set_limits(limits);
    r
}

fn term(r: &mut Realm, src: &str) -> Option<Termination> {
    match r.eval(src, "limits.js") {
        Ok(_) => None,
        Err(e) => e.termination(),
    }
}

#[test]
fn step_budget_stops_loops_uncatchably() {
    let mut r = realm_with(Limits {
        max_steps: 100_000,
        ..Limits::default()
    });
    assert_eq!(
        term(&mut r, "while (true) {}"),
        Some(Termination::StepBudget)
    );
    assert_eq!(
        term(
            &mut r,
            "try { for (;;) {} } catch (e) {} finally { globalThis.ranFinally = 1 }"
        ),
        Some(Termination::StepBudget)
    );
    // The termination skipped the finally block, and the realm still works.
    let v = r.eval("typeof ranFinally", "x.js").unwrap();
    assert_eq!(r.get_string(v).as_deref(), Some("undefined"));
    let v = r.eval("1 + 1", "x.js").unwrap();
    assert_eq!(v.as_number(), Some(2.0));
    assert_eq!(
        term(
            &mut r,
            "function f() { f2() } function f2() { while (1) {} } f()"
        ),
        Some(Termination::StepBudget)
    );
    assert_eq!(
        term(&mut r, "[1,2,3].forEach(function () { while (true) {} })"),
        Some(Termination::StepBudget)
    );
    assert_eq!(
        term(
            &mut r,
            "function* g() { while (true) yield 1 } for (var x of g()) {}"
        ),
        Some(Termination::StepBudget)
    );
}

#[test]
fn interrupt_callback_stops_scripts() {
    let mut r = Realm::new(Options::default());
    let polls = Rc::new(Cell::new(0u32));
    let p2 = polls.clone();
    r.set_interrupt(Some(Box::new(move || {
        p2.set(p2.get() + 1);
        p2.get() < 5
    })));
    assert_eq!(
        term(&mut r, "var n = 0; while (true) { n++ }"),
        Some(Termination::Interrupted)
    );
    assert!(polls.get() >= 5);
    // An endless chain of microtasks is polled too.
    polls.set(0);
    r.eval(
        "function again() { Promise.resolve().then(again) } again()",
        "jobs.js",
    )
    .unwrap();
    let e = r.run_jobs().unwrap_err();
    assert_eq!(e.termination(), Some(Termination::Interrupted));
    r.clear_jobs();
    r.set_interrupt(None);
    assert!(r.eval("2", "x.js").is_ok());
}

#[test]
fn memory_budget() {
    for src in [
        "var a = []; while (true) a.push({ x: [1, 2, 3] })",
        "var s = 'x'; while (true) s += s",
        "var m = new Map(); for (var i = 0; ; i++) m.set(i, 'v' + i)",
        "function* inf() { while (true) yield {} } [...inf()]",
        "var o = {}; for (var i = 0; ; i++) o['k' + i] = i",
        "new Array(1e9).fill(0)",
        "'x'.repeat(1 << 28)",
        "var a = []; a.length = 4294967295; a.join('xx')",
        "JSON.stringify(Array(5e6).fill('aaaaaaaaaaaaaaaaaaaa'))",
    ] {
        let mut r = realm_with(Limits {
            max_heap_bytes: 16 << 20,
            max_steps: 50_000_000,
            ..Limits::default()
        });
        let out = r.eval(src, "mem.js");
        match out {
            Ok(_) => panic!("{src}: should have failed"),
            Err(e) => {
                let rep = r.report(&e);
                assert!(
                    e.termination() == Some(Termination::OutOfMemory) || rep.name == "RangeError",
                    "{src}: {rep}"
                );
            }
        }
        assert!(
            r.heap_stats().used_bytes < 64 << 20,
            "{src}: {} bytes",
            r.heap_stats().used_bytes
        );
    }
}

/// A native that calls script over and over (a proxy trap asked again and again by a loop
/// over a prototype chain that never ends) is stopped by the step budget: each call from
/// native code costs steps, even when the called function has no loop or call of its own.
#[test]
fn natives_calling_script_pay_steps() {
    for src in [
        "var r, p; var h = { getPrototypeOf: function () { return p }, ownKeys: function () { p instanceof Object; return Reflect.ownKeys.apply(null, arguments) } }; r = Proxy.revocable([1, 2, 3], h); p = r.proxy; for (var k in p) {}",
        "var p = new Proxy({}, { getPrototypeOf: function () { return p } }); for (var i = 0; ; i++) p instanceof Object",
        "var o = { get a() { return 1 } }; for (;;) { [1, 2, 3].map(function (x) { return x }); Object.keys(o); JSON.stringify({ toJSON: function () { return 1 } }) }",
    ] {
        let mut r = realm_with(Limits {
            max_steps: 200_000,
            ..Limits::default()
        });
        let t = std::time::Instant::now();
        let e = r.eval(src, "loop.js").expect_err(src);
        assert_eq!(e.termination(), Some(Termination::StepBudget), "{src}");
        assert!(t.elapsed().as_secs() < 20, "{src}: took {:?}", t.elapsed());
    }
}

/// Code a script keeps alive (functions made by eval and `new Function`, each with its own
/// source text) counts against the heap budget like objects do.
#[test]
fn retained_code_counts_against_the_heap() {
    for src in [
        "var keep = []; for (var i = 0; ; i++) keep.push(eval('(function f' + i + '() { return ' + i + ' + \"' + 'x'.repeat(4000) + '\" })'))",
        "var keep = []; for (var i = 0; ; i++) keep.push(new Function('return ' + i + ' + \"' + 'y'.repeat(4000) + '\"'))",
        "var keep = []; for (var i = 0; ; i++) keep.push(eval('[' + ('(function () { return ' + i + ' }),').repeat(100) + ']'))",
    ] {
        let mut r = realm_with(Limits {
            max_heap_bytes: 8 << 20,
            max_steps: 50_000_000,
            ..Limits::default()
        });
        let e = r.eval(src, "code.js").expect_err(src);
        let rep = r.report(&e);
        assert!(
            e.termination() == Some(Termination::OutOfMemory) || rep.name == "RangeError",
            "{src}: {rep}"
        );
        r.release_temps();
        r.gc();
        assert!(r.heap_stats().used_bytes < 64 << 20);
    }
    // And it is given back when the functions go.
    let mut r = realm_with(Limits {
        max_heap_bytes: 8 << 20,
        ..Limits::default()
    });
    r.eval("for (var i = 0; i < 300; i++) eval('(function () { return \"' + 'z'.repeat(3000) + '\" })')", "a.js").unwrap();
    r.release_temps();
    r.gc();
    assert!(r.heap_stats().used_bytes < 2 << 20, "{}", r.heap_stats().used_bytes);
}

#[test]
fn recursion_is_a_catchable_range_error() {
    let mut r = Realm::new(Options::default());
    let v = r
        .eval("function f(n) { return f(n + 1) } try { f(0) } catch (e) { e instanceof RangeError && e.message }", "rec.js")
        .unwrap();
    assert_eq!(
        r.get_string(v).as_deref(),
        Some("Maximum call stack size exceeded")
    );
    // Through natives and getters too.
    let v = r
        .eval(
            "var o = { get g() { return [1].map(() => o.g)[0] } }; try { o.g } catch (e) { e.constructor.name }",
            "rec2.js",
        )
        .unwrap();
    assert_eq!(r.get_string(v).as_deref(), Some("RangeError"));
    let v = r
        .eval(
            "var t = { toString() { return '' + t } }; try { '' + t } catch (e) { 'caught' }",
            "rec3.js",
        )
        .unwrap();
    assert_eq!(r.get_string(v).as_deref(), Some("caught"));
    let v = r.eval("var deep = {}; for (var i = 0; i < 100000; i++) deep = { d: deep }; try { JSON.stringify(deep); 'no' } catch (e) { e.name }", "rec4.js").unwrap();
    assert_eq!(r.get_string(v).as_deref(), Some("RangeError"));
}

#[test]
fn regexp_complexity() {
    let mut r = Realm::new(Options::default());
    let v = r
        .eval(
            "try { /(x+x+)+y/.test('x'.repeat(50)) } catch (e) { e.name }",
            "re.js",
        )
        .unwrap();
    assert_eq!(r.get_string(v).as_deref(), Some("RangeError"));
}

#[test]
fn errors_from_hostile_values() {
    let mut r = Realm::new(Options::default());
    for src in [
        "null.x",
        "undefined()",
        "({}).x.y.z",
        "new 1",
        "[].reduce(function(){})",
        "Symbol() + ''",
        "Object.defineProperty(1, 'x', {})",
        "new Proxy(1, {})",
        "x = y",
        "JSON.parse('{')",
        "'use strict'; undeclaredVar = 1",
        "class A { constructor() { super() } }",
    ] {
        let e = r.eval(src, "bad.js").expect_err(src);
        assert!(!e.is_uncatchable(), "{src}");
        let rep = r.report(&e);
        assert!(!rep.name.is_empty(), "{src}");
    }
}

#[test]
#[ignore]
fn memory_budget_timing() {
    for src in [
        "var a = []; while (true) a.push({ x: [1, 2, 3] })",
        "var s = 'x'; while (true) s += s",
        "var m = new Map(); for (var i = 0; ; i++) m.set(i, 'v' + i)",
        "function* inf() { while (true) yield {} } [...inf()]",
        "var o = {}; for (var i = 0; ; i++) o['k' + i] = i",
        "new Array(1e9).fill(0)",
        "'x'.repeat(1 << 28)",
        "var a = []; a.length = 4294967295; a.join('xx')",
        "JSON.stringify(Array(5e6).fill('aaaaaaaaaaaaaaaaaaaa'))",
    ] {
        let t = std::time::Instant::now();
        let mut r = realm_with(Limits {
            max_heap_bytes: 16 << 20,
            max_steps: 50_000_000,
            ..Limits::default()
        });
        let _ = r.eval(src, "mem.js");
        std::println!("{:?} gcs={} {src}", t.elapsed(), r.heap_stats().collections);
    }
}

#[test]
fn heap_limit_moves_while_script_runs() {
    // A script that holds more and more: the hook gives it room in steps, then takes it back.
    let mut r = realm_with(Limits {
        max_heap_bytes: 16 << 20,
        ..Limits::default()
    });
    let calls = Rc::new(Cell::new(0u32));
    let c = calls.clone();
    r.set_memory_hook(Some(Box::new(move |used| {
        c.set(c.get() + 1);
        // Always 16 MiB above what is held, up to 200 MiB.
        HeapAnswer::Limit((used + (16 << 20)).clamp(16 << 20, 200 << 20))
    })));
    let src = "var keep = []; for (var i = 0; i < 20000; i++) { keep.push({ a: i, b: 'x' + i }); for (var j = 0; j < 20; j++) {} } keep.length";
    let v = r.eval(src, "grow.js").expect("the hook gave it room");
    assert_eq!(v.as_number(), Some(20000.0));
    assert!(calls.get() > 1, "the hook was asked more than once");
    let lim = r.heap_stats().limit_bytes;
    assert!(lim > 16 << 20 && lim <= 200 << 20, "limit {lim}");
    // Now it takes the room back: what is held is more than the budget, and the next
    // allocation loop runs out of memory (and only this realm).
    r.set_memory_hook(Some(Box::new(|_| HeapAnswer::Limit(2 << 20))));
    assert_eq!(
        term(&mut r, "var more = []; for (var i = 0; i < 3000000; i++) more.push('y' + i); 1"),
        Some(Termination::OutOfMemory)
    );
    assert_eq!(r.heap_stats().limit_bytes, 2 << 20);
}

#[test]
fn set_heap_limit_moves_the_budget() {
    let mut r = realm_with(Limits::default());
    r.eval("var a = []; for (var i = 0; i < 30000; i++) a.push({ i: i });", "a.js").unwrap();
    let held = r.heap_stats().used_bytes;
    assert!(held > 1 << 20);
    // More room: it can keep growing.
    r.set_heap_limit(held * 4);
    assert_eq!(r.heap_stats().limit_bytes, held * 4);
    assert_eq!(r.limits().max_heap_bytes, held * 4);
    r.eval("for (var i = 0; i < 30000; i++) a.push({ i: i });", "b.js").unwrap();
    // Less room than it holds: a script that wants to grow is refused (it cannot even be
    // compiled, or it runs out of memory at the first collection); the realm does not crash.
    r.set_heap_limit(1 << 20);
    assert!(r.eval("for (var i = 0; i < 1000000; i++) a.push({ i: i }); 1", "c.js").is_err());
    // Room again, and it works.
    r.set_heap_limit(1 << 30);
    assert_eq!(r.eval("a.length", "d.js").unwrap().as_number(), Some(60000.0));
}

#[test]
fn memory_hook_can_stop_the_script() {
    let mut r = realm_with(Limits::default());
    let n = Rc::new(Cell::new(0));
    let n2 = n.clone();
    r.set_memory_hook(Some(Box::new(move |_| {
        n2.set(n2.get() + 1);
        if n2.get() >= 3 { HeapAnswer::Stop } else { HeapAnswer::Keep }
    })));
    assert_eq!(term(&mut r, "var k = 0; for (;;) { k++; }"), Some(Termination::Interrupted));
    assert_eq!(n.get(), 3);
}
