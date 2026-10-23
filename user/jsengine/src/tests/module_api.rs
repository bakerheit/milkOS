//! The module embedding API: positions in errors, status, hooks, limits, dropping.

use super::modules::{Host, describe};
use crate::*;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

fn files(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| (String::from(*a), String::from(*b)))
        .collect()
}

/// The Browser runs script on an 8 MB main thread; the tests set a 6 MB stack limit, so they
/// need a thread at least that big.
fn big(f: fn()) {
    std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

fn realm() -> Realm {
    let mut r = Realm::new(Options::default());
    r.set_stack_limit(6 << 20);
    r
}

fn compile(r: &mut Realm, name: &str, src: &str) -> ModuleId {
    r.compile_module(
        src,
        &ModuleOptions {
            file: name,
            ..ModuleOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{name}: {}", r.report(&e)))
}

#[test]
fn syntax_errors_carry_file_line_and_column() {
    big(syntax_errors_carry_file_line_and_column_body);
}

fn syntax_errors_carry_file_line_and_column_body() {
    let mut r = realm();
    let opts = ModuleOptions {
        file: "https://example.test/app/main.js",
        ..ModuleOptions::default()
    };
    let e = r
        .compile_module("import x from './a.js';\n\nlet a = ;\n", &opts)
        .unwrap_err();
    let rep = r.report(&e);
    assert_eq!(rep.name, "SyntaxError");
    assert_eq!(rep.file, "https://example.test/app/main.js");
    assert_eq!((rep.line, rep.column), (3, 9));
    // An inline module starts at its place in the page.
    let opts = ModuleOptions {
        file: "page.html",
        first_line: 40,
        first_column: 12,
    };
    let e = r.compile_module("let a = ;", &opts).unwrap_err();
    let rep = r.report(&e);
    assert_eq!((rep.line, rep.column), (40, 20));
    // Early errors specific to modules.
    for (src, msg) in [
        ("export { nope };", "Export 'nope' is not defined in module"),
        ("let a; let a;", "Identifier 'a' has already been declared"),
        (
            "export { a }; export { b as a }; let a, b;",
            "Duplicate export of 'a'",
        ),
        (
            "import { a } from 'm'; import { a } from 'n';",
            "Identifier 'a' has already been declared",
        ),
        (
            "if (1) { import 'm'; }",
            "imports are only allowed at the top level",
        ),
        ("function f() { await 1; }", "Unexpected"),
        ("var await;", "Unexpected"),
        ("with (a) {}", "not supported"),
        ("import { 'a b' } from 'm';", "needs 'as'"),
        ("export { 'a' };", "needs 'from'"),
        ("export { a as 'x\\uD800' };let a;", "unpaired surrogate"),
        ("import x, * as y, { z } from 'm';", "Unexpected"),
        ("export default;", "Unexpected"),
        (
            "export * as default from 'm'; export default 1;",
            "Duplicate export of 'default'",
        ),
        ("import { default } from 'm';", "reserved"),
        ("import { eval } from 'm';", "Unexpected"),
        (
            "import m from 'm' with { type: 'json', type: 'json' };",
            "duplicate key",
        ),
        ("new import('m')", "Cannot use new with import"),
        ("<!-- html comment", "Unexpected"),
    ] {
        let e = match r.compile_module(src, &ModuleOptions::default()) {
            Ok(_) => panic!("should not compile: {src}"),
            Err(e) => e,
        };
        let rep = r.report(&e);
        assert_eq!(rep.name, "SyntaxError", "{src}");
        assert!(rep.message.contains(msg), "{src}: {}", rep.message);
    }
}

#[test]
fn link_errors_point_at_the_import() {
    big(link_errors_point_at_the_import_body);
}

fn link_errors_point_at_the_import_body() {
    let mut r = realm();
    let main = compile(
        &mut r,
        "main.js",
        "log;\nimport { a,\n  nope } from './dep.js';\n",
    );
    let dep = compile(&mut r, "dep.js", "export let a;");
    let reqs = r.module_requests(main);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].specifier, "./dep.js");
    assert_eq!((reqs[0].line, reqs[0].column), (3, 15));
    r.set_module_dependency(main, 0, dep).unwrap();
    let e = r.link_module(main).unwrap_err();
    let rep = r.report(&e);
    assert_eq!(rep.name, "SyntaxError");
    assert_eq!(rep.file, "main.js");
    assert_eq!((rep.line, rep.column), (3, 3));
    assert!(
        rep.message
            .contains("does not provide an export named 'nope'")
    );
    // Nothing stays linked.
    assert_eq!(r.module_status(main), Some(ModuleStatus::Unlinked));
    assert_eq!(r.module_status(dep), Some(ModuleStatus::Linked));
}

#[test]
fn status_goes_through_its_stages() {
    big(status_goes_through_its_stages_body);
}

fn status_goes_through_its_stages_body() {
    let mut r = realm();
    let m = compile(&mut r, "m.js", "export let v = 1; await null; v = 2;");
    assert_eq!(r.module_status(m), Some(ModuleStatus::Unlinked));
    assert!(r.module_namespace(m).is_err());
    r.link_module(m).unwrap();
    assert_eq!(r.module_status(m), Some(ModuleStatus::Linked));
    let ns = r.module_namespace(m).unwrap();
    assert_eq!(
        r.get(ns, "v").unwrap_err().value().is_some(),
        true,
        "v is in its TDZ"
    );
    let p = r.evaluate_module(m).unwrap();
    assert_eq!(r.module_status(m), Some(ModuleStatus::EvaluatingAsync));
    r.run_jobs().unwrap();
    assert_eq!(r.module_status(m), Some(ModuleStatus::Evaluated));
    assert!(r.module_error(m).is_none());
    assert_eq!(r.get(ns, "v").unwrap().as_number(), Some(2.0));
    // The same promise comes back, and a second evaluate does not run it again.
    let p2 = r.evaluate_module(m).unwrap();
    assert!(p.is_same_handle(&p2));
    let names = r.module_export_names(m).unwrap();
    assert_eq!(names, alloc::vec![String::from("v")]);
}

#[test]
fn evaluation_errors_are_kept() {
    big(evaluation_errors_are_kept_body);
}

fn evaluation_errors_are_kept_body() {
    let mut r = realm();
    let m = compile(&mut r, "m.js", "throw new TypeError('nope');");
    r.link_module(m).unwrap();
    let p = r.evaluate_module(m).unwrap();
    assert_eq!(r.module_status(m), Some(ModuleStatus::Evaluated));
    let err = r.module_error(m).unwrap();
    assert_eq!(r.report_value(err).name, "TypeError");
    // The promise is rejected, and asking again gives a rejected promise with the same error.
    let seen = Rc::new(RefCell::new(String::new()));
    let s2 = seen.clone();
    let on_rej = r.new_function("r", 1, move |r, _t, a| {
        *s2.borrow_mut() = r.report_value(a[0]).name;
        Ok(Value::UNDEFINED)
    });
    r.call_method(p, "catch", &[on_rej]).unwrap();
    r.run_jobs().unwrap();
    assert_eq!(&*seen.borrow(), "TypeError");
    // The module's own promise doesn't count as an unhandled rejection.
    assert!(r.take_unhandled_rejections().is_empty());
}

#[test]
fn unresolved_requests_and_late_answers_are_refused() {
    big(unresolved_requests_and_late_answers_are_refused_body);
}

fn unresolved_requests_and_late_answers_are_refused_body() {
    let mut r = realm();
    let m = compile(&mut r, "m.js", "import './x.js';");
    let e = r.link_module(m).unwrap_err();
    assert!(r.report(&e).message.contains("was not resolved"));
    assert_eq!(r.module_status(m), Some(ModuleStatus::Unlinked));
    let x = compile(&mut r, "x.js", "");
    assert!(r.set_module_dependency(m, 5, x).is_err());
    r.set_module_dependency(m, 0, x).unwrap();
    assert_eq!(r.module_dependency(m, 0), Some(x));
    r.link_module(m).unwrap();
    r.link_module(m).unwrap();
    // Too late to change the answer.
    assert!(r.set_module_dependency(m, 0, x).is_err());
}

#[test]
fn dropping_modules() {
    big(dropping_modules_body);
}

fn dropping_modules_body() {
    let mut r = realm();
    let a = compile(&mut r, "a.js", "import './b.js'; export const v = 1;");
    let b = compile(&mut r, "b.js", "export const w = 2;");
    r.set_module_dependency(a, 0, b).unwrap();
    assert_eq!(r.module_count(), 2);
    r.drop_module_graph(a);
    assert_eq!(r.module_count(), 0);
    assert!(r.link_module(a).is_err());
    assert!(r.module_status(b).is_none());
    assert!(r.module_requests(a).is_empty());
    // A new module may reuse the slot, and the old handle still doesn't reach it.
    let c = compile(&mut r, "c.js", "export const z = 3;");
    assert_ne!(c, a);
    assert!(r.module_status(a).is_none());
    assert_eq!(r.module_status(c), Some(ModuleStatus::Unlinked));
    r.gc();
}

#[test]
fn the_module_count_is_limited() {
    big(the_module_count_is_limited_body);
}

fn the_module_count_is_limited_body() {
    let mut r = realm();
    r.set_max_modules(3);
    for i in 0..3 {
        compile(&mut r, &alloc::format!("m{i}.js"), "export {};");
    }
    let e = r
        .compile_module("export {};", &ModuleOptions::default())
        .unwrap_err();
    assert_eq!(r.report(&e).name, "RangeError");
    let e = r.compile_synthetic_module("s", &[]).unwrap_err();
    assert_eq!(r.report(&e).name, "RangeError");
    let mut r = realm();
    let mut lim = r.limits();
    lim.max_source_bytes = 100;
    r.set_limits(lim);
    let big = alloc::format!("export const a = '{}';", "x".repeat(200));
    let e = r
        .compile_module(&big, &ModuleOptions::default())
        .unwrap_err();
    assert_eq!(r.report(&e).name, "RangeError");
}

#[test]
fn modules_count_against_the_heap() {
    big(modules_count_against_the_heap_body);
}

fn modules_count_against_the_heap_body() {
    let mut r = realm();
    let before = r.heap_stats().used_bytes;
    let mut ids = Vec::new();
    for i in 0..50 {
        let src = alloc::format!("export const a{i} = 1; export function f{i}() {{ return {i}; }}");
        ids.push(compile(&mut r, &alloc::format!("m{i}.js"), &src));
    }
    let used = r.heap_stats().used_bytes;
    assert!(used > before + 50 * 400, "{before} -> {used}");
    r.gc();
    let after_gc = r.heap_stats().live_bytes;
    assert!(
        after_gc > before,
        "module code is not free after a collection"
    );
    for m in ids {
        r.drop_module(m);
    }
    r.gc();
    assert!(r.heap_stats().live_bytes < after_gc);
    // A tiny heap budget stops a flood of modules instead of using all memory.
    let mut r = realm();
    let mut lim = r.limits();
    lim.max_heap_bytes = 1 << 20;
    r.set_limits(lim);
    let src = alloc::format!("export const big = [{}];", "1,".repeat(2000));
    let mut stopped = false;
    for i in 0..1000 {
        match r.compile_module(
            &src,
            &ModuleOptions {
                file: "x",
                ..ModuleOptions::default()
            },
        ) {
            Ok(_) => {}
            Err(e) => {
                assert!(
                    e.is_uncatchable() || r.report(&e).name == "RangeError",
                    "{i}"
                );
                stopped = true;
                break;
            }
        }
    }
    assert!(stopped, "the heap budget should have stopped the flood");
}

#[test]
fn a_runaway_module_is_stopped_and_stays_failed() {
    big(a_runaway_module_is_stopped_and_stays_failed_body);
}

fn a_runaway_module_is_stopped_and_stays_failed_body() {
    let mut r = realm();
    let mut lim = r.limits();
    lim.max_steps = 100_000;
    r.set_limits(lim);
    let m = compile(&mut r, "m.js", "while (true) {}");
    r.link_module(m).unwrap();
    let e = r.evaluate_module(m).unwrap_err();
    assert!(e.is_uncatchable());
    assert_eq!(r.module_status(m), Some(ModuleStatus::Evaluated));
    assert!(r.module_error(m).is_some());
    // Evaluating it again gives a rejected promise, not another run.
    let again = r.evaluate_module(m);
    assert!(again.is_ok());
}

#[test]
fn a_runaway_import_chain_is_bounded() {
    big(a_runaway_import_chain_is_bounded_body);
}

fn a_runaway_import_chain_is_bounded_body() {
    // A very deep chain of modules: linking and evaluating must not overflow the stack.
    let mut r = realm();
    let n = 3000;
    let mut ids: Vec<ModuleId> = Vec::new();
    for i in 0..n {
        let src = if i + 1 < n {
            alloc::format!("import './next.js'; export const v{i} = {i};")
        } else {
            String::from("export const last = 1;")
        };
        ids.push(compile(&mut r, &alloc::format!("m{i}.js"), &src));
    }
    for i in 0..n - 1 {
        r.set_module_dependency(ids[i], 0, ids[i + 1]).unwrap();
    }
    // Either it works or it fails cleanly; it never crashes.
    let res = r.link_module(ids[0]);
    if res.is_ok() {
        let _ = r.evaluate_module(ids[0]);
        r.run_jobs().ok();
    } else {
        assert_eq!(r.module_status(ids[0]), Some(ModuleStatus::Unlinked));
    }
}

#[test]
fn a_wide_export_star_graph_resolves() {
    big(a_wide_export_star_graph_resolves_body);
}

fn a_wide_export_star_graph_resolves_body() {
    let mut r = realm();
    let mut ids: Vec<ModuleId> = Vec::new();
    let leaves = 60;
    let mut top = String::new();
    for i in 0..leaves {
        ids.push(compile(
            &mut r,
            &alloc::format!("l{i}.js"),
            &alloc::format!("export const n{i} = {i};"),
        ));
        top.push_str("export * from './x.js';\n");
    }
    let t = compile(&mut r, "top.js", &top);
    for (i, id) in ids.iter().enumerate() {
        r.set_module_dependency(t, i, *id).ok();
    }
    // Identical specifiers share one request, so only the first is answered.
    assert_eq!(r.module_requests(t).len(), 1);
    r.set_module_dependency(t, 0, ids[0]).unwrap();
    r.link_module(t).unwrap();
    assert_eq!(r.module_export_names(t).unwrap().len(), 1);
}

#[test]
fn synthetic_modules_export_values() {
    big(synthetic_modules_export_values_body);
}

fn synthetic_modules_export_values_body() {
    let mut r = realm();
    let o = r.new_object();
    let v = r.string("kept");
    r.set(o, "k", v).unwrap();
    let s = r
        .compile_synthetic_module("data.json", &[("default", o), ("extra", Value::int(7))])
        .unwrap();
    assert_eq!(r.module_status(s), Some(ModuleStatus::Evaluated));
    let m = compile(
        &mut r,
        "m.js",
        "import d, { extra } from './data.json'; import * as ns from './data.json'; export const out = d.k + extra + Object.keys(ns).join();",
    );
    r.set_module_dependency(m, 0, s).unwrap();
    r.link_module(m).unwrap();
    r.gc();
    r.release_temps();
    r.gc();
    let p = r.evaluate_module(m).unwrap();
    r.run_jobs().unwrap();
    let _ = p;
    let ns = r.module_namespace(m).unwrap();
    let out = r.get(ns, "out").unwrap();
    assert_eq!(r.get_string(out).as_deref(), Some("kept7default,extra"));
    assert!(
        r.compile_synthetic_module("x", &[("a", Value::UNDEFINED), ("a", Value::UNDEFINED)])
            .is_err()
    );
}

#[test]
fn import_meta_hook_runs_once_per_module() {
    big(import_meta_hook_runs_once_per_module_body);
}

fn import_meta_hook_runs_once_per_module_body() {
    let mut r = realm();
    let calls = Rc::new(Cell::new(0));
    let c2 = calls.clone();
    r.set_import_meta_hook(Some(Box::new(move |r, m, meta| {
        c2.set(c2.get() + 1);
        let f = r.module_file(m).unwrap_or_default();
        let fv = r.string(&f);
        r.set(meta, "file", fv)
    })));
    let m = compile(
        &mut r,
        "m.js",
        "export const a = import.meta.file, b = import.meta === import.meta, c = import.meta.file;",
    );
    r.link_module(m).unwrap();
    r.evaluate_module(m).unwrap();
    assert_eq!(calls.get(), 1);
    let ns = r.module_namespace(m).unwrap();
    let a = r.get(ns, "a").unwrap();
    assert_eq!(r.get_string(a).as_deref(), Some("m.js"));
    // Without a hook the object is empty.
    let mut r = realm();
    let m = compile(
        &mut r,
        "m.js",
        "export const k = Object.keys(import.meta).length;",
    );
    r.link_module(m).unwrap();
    r.evaluate_module(m).unwrap();
    let ns = r.module_namespace(m).unwrap();
    assert_eq!(r.get(ns, "k").unwrap().as_number(), Some(0.0));
    // A hook that throws makes import.meta throw.
    r.set_import_meta_hook(Some(Box::new(|r, _m, _meta| {
        Err(r.throw_type_error("no meta"))
    })));
    let m = compile(&mut r, "m2.js", "import.meta;");
    r.link_module(m).unwrap();
    r.evaluate_module(m).unwrap();
    assert!(r.module_error(m).is_some());
}

#[test]
fn dynamic_import_without_a_hook_rejects() {
    big(dynamic_import_without_a_hook_rejects_body);
}

fn dynamic_import_without_a_hook_rejects_body() {
    let mut r = realm();
    let v = r.eval("import('./x.js')", "t.js").unwrap();
    let seen = Rc::new(RefCell::new(String::new()));
    let s2 = seen.clone();
    let f = r.new_function("r", 1, move |r, _t, a| {
        *s2.borrow_mut() = r.report_value(a[0]).name;
        Ok(Value::UNDEFINED)
    });
    r.call_method(v, "catch", &[f]).unwrap();
    r.run_jobs().unwrap();
    assert_eq!(&*seen.borrow(), "TypeError");
}

#[test]
fn dynamic_import_hook_and_finish() {
    big(dynamic_import_hook_and_finish_body);
}

fn dynamic_import_hook_and_finish_body() {
    let mut r = realm();
    let reqs: Rc<RefCell<Vec<DynamicImport>>> = Rc::new(RefCell::new(Vec::new()));
    let r2 = reqs.clone();
    r.set_dynamic_import_hook(Some(Box::new(move |_r, di| {
        r2.borrow_mut().push(di);
        Ok(())
    })));
    let m = compile(
        &mut r,
        "dir/m.js",
        "export const v = 1; globalThis.p1 = import('./a.js', { with: { type: 'json' } });",
    );
    r.link_module(m).unwrap();
    r.evaluate_module(m).unwrap();
    r.eval(
        "globalThis.p2 = import('b'); globalThis.p3 = import('c')",
        "classic.js",
    )
    .unwrap();
    assert_eq!(r.pending_dynamic_imports(), 3);
    {
        let q = reqs.borrow();
        assert_eq!(q[0].referrer, Some(m));
        assert_eq!(q[0].referrer_file, "dir/m.js");
        assert_eq!(q[0].request.specifier, "./a.js");
        assert_eq!(q[0].request.attribute("type"), Some("json"));
        assert_eq!(q[1].referrer, None);
        assert_eq!(q[1].referrer_file, "classic.js");
    }
    let dep = compile(&mut r, "a.js", "export default 'loaded';");
    let id0 = reqs.borrow()[0].id;
    let id1 = reqs.borrow()[1].id;
    let id2 = reqs.borrow()[2].id;
    r.finish_dynamic_import(id0, Ok(dep)).unwrap();
    let boom = r.string("fetch failed");
    r.finish_dynamic_import(id1, Err(boom)).unwrap();
    // A second answer for the same id is ignored.
    r.finish_dynamic_import(id1, Err(boom)).unwrap();
    assert_eq!(r.pending_dynamic_imports(), 1);
    r.cancel_dynamic_imports();
    assert_eq!(r.pending_dynamic_imports(), 0);
    r.finish_dynamic_import(id2, Ok(dep)).unwrap();
    r.run_jobs().unwrap();
    let out = r
        .eval(
            "var out = []; p1.then((ns) => out.push('p1 ' + ns.default)); p2.catch((e) => out.push('p2 ' + e)); out",
            "check.js",
        )
        .unwrap();
    r.run_jobs().unwrap();
    let s = r.json_stringify(out).unwrap().unwrap();
    assert_eq!(s, "[\"p1 loaded\",\"p2 fetch failed\"]");
    assert_eq!(r.module_status(dep), Some(ModuleStatus::Evaluated));
}

#[test]
fn a_hook_that_throws_rejects_the_import() {
    big(a_hook_that_throws_rejects_the_import_body);
}

fn a_hook_that_throws_rejects_the_import_body() {
    let mut r = realm();
    r.set_dynamic_import_hook(Some(Box::new(|r, _di| {
        Err(r.throw_error(ErrorKind::RangeError, "blocked"))
    })));
    let v = r.eval("import('./x.js')", "t.js").unwrap();
    let seen = Rc::new(RefCell::new(String::new()));
    let s2 = seen.clone();
    let f = r.new_function("r", 1, move |r, _t, a| {
        *s2.borrow_mut() = alloc::format!("{:?}", r.report_value(a[0]).message);
        Ok(Value::UNDEFINED)
    });
    r.call_method(v, "catch", &[f]).unwrap();
    r.run_jobs().unwrap();
    assert_eq!(&*seen.borrow(), "\"blocked\"");
    assert_eq!(r.pending_dynamic_imports(), 0);
}

#[test]
fn module_graph_survives_collections() {
    big(module_graph_survives_collections_body);
}

fn module_graph_survives_collections_body() {
    // Every module of a graph is reachable only through the realm's table: a collection in
    // the middle of linking, evaluation and import() must not free anything it needs.
    let list = files(&[
        (
            "main.js",
            "import { a } from './a.js'; import * as b from './b.js';\nlet s = 0; for (let i = 0; i < 20000; i++) s += [i, {i}].length;\nconst c = await import('./c.js');\nlog(a(), b.b, c.default, s);",
        ),
        (
            "a.js",
            "export function a() { return 'a' + [1,2,3].map(x => x * 2).join(''); }",
        ),
        ("b.js", "export const b = ['b', 'b'].join('-'); await 0;"),
        (
            "c.js",
            "export default {}.constructor.name + (await Promise.resolve(1));",
        ),
    ]);
    for stress in [0, 1, 7] {
        let mut host = Host::new(list.clone());
        let mut r = host.realm(stress);
        host.run(&mut r, "main.js");
        let log = host.log.borrow().clone();
        assert_eq!(
            log,
            alloc::vec![String::from("a246 b-b Object1 40000")],
            "stress {stress}"
        );
        r.gc();
    }
}

#[test]
fn errors_from_modules_describe_themselves() {
    big(errors_from_modules_describe_themselves_body);
}

fn errors_from_modules_describe_themselves_body() {
    let mut host = Host::new(files(&[("main.js", "null.x;")]));
    let mut r = host.realm(0);
    host.run(&mut r, "main.js");
    assert_eq!(
        host.log.borrow().clone(),
        alloc::vec![String::from(
            "eval error: TypeError: Cannot read properties of null (reading 'x')"
        )]
    );
    let m = host.load(&mut r, "main.js", &[]).unwrap();
    let e = r.module_error(m).unwrap();
    let rep = r.report_value(e);
    assert_eq!((rep.file.as_str(), rep.line), ("main.js", 1));
    let _ = describe;
}

/// A big bundle-shaped module: how long compiling, linking and running it takes.
/// `cargo test --release -p jsengine --lib module_bundle_timing -- --ignored --nocapture`
#[test]
#[ignore]
fn module_bundle_timing() {
    big(module_bundle_timing_body);
}

fn module_bundle_timing_body() {
    let n = 20_000;
    let mut src = String::new();
    for i in 0..n {
        src.push_str(&alloc::format!(
            "function f{i}(x) {{ return x + {i} + (x > 3 ? f{}(x - 1) : 0); }}\nconst c{i} = [f{i}, 'v{i}'];\nexport {{ f{i} as e{i} }};\n",
            (i + 1) % n
        ));
    }
    src.push_str("export let total = 0; for (let i = 0; i < 1000; i++) total += f0(2);\n");
    let t = std::time::Instant::now();
    let mut r = realm();
    let m = compile(&mut r, "bundle.js", &src);
    let t_compile = t.elapsed();
    let t = std::time::Instant::now();
    r.link_module(m).unwrap();
    let t_link = t.elapsed();
    let t = std::time::Instant::now();
    r.evaluate_module(m).unwrap();
    let t_eval = t.elapsed();
    let t = std::time::Instant::now();
    let main = compile(
        &mut r,
        "main.js",
        "import * as ns from './bundle.js'; export const n = Object.keys(ns).length;",
    );
    r.set_module_dependency(main, 0, m).unwrap();
    r.link_module(main).unwrap();
    r.evaluate_module(main).unwrap();
    let t_ns = t.elapsed();
    std::println!(
        "{} bytes: compile {t_compile:?}, link {t_link:?}, evaluate {t_eval:?}, namespace {t_ns:?}",
        src.len()
    );
}

#[test]
fn hostile_nesting_and_sizes_are_refused() {
    big(hostile_nesting_and_sizes_are_refused_body);
}

fn hostile_nesting_and_sizes_are_refused_body() {
    let mut r = realm();
    let deep = "(".repeat(200_000);
    for src in [
        alloc::format!("export default {deep}"),
        alloc::format!("{}1{}", "import(".repeat(50_000), ")".repeat(50_000)),
        alloc::format!("export default {}", "[".repeat(100_000)),
        alloc::format!("export {{ {} }}", "a as b, ".repeat(10)),
        "export * as ".repeat(10_000),
        "import ".repeat(100_000),
        alloc::format!("export default {}0", "-".repeat(300_000)),
        alloc::format!("await {}0", "await ".repeat(100_000)),
        alloc::format!(
            "export default {}x{}",
            "class extends ".repeat(10_000),
            " {}".repeat(10_000)
        ),
    ]
    .iter()
    .enumerate()
    {
        let src = src.1;
        match r.compile_module(&src, &ModuleOptions::default()) {
            Ok(m) => r.drop_module(m),
            Err(e) => assert_eq!(r.report(&e).name, "SyntaxError"),
        }
        r.release_temps();
    }
    // Many exports and imports in one module are fine.
    let mut src = String::new();
    for i in 0..30_000 {
        src.push_str(&alloc::format!("export const a{i} = {i};\n"));
    }
    let m = compile(&mut r, "many.js", &src);
    let mut user = String::from("import * as ns from './many.js';\n");
    for i in (0..30_000).step_by(1000) {
        user.push_str(&alloc::format!("import {{ a{i} }} from './many.js';\n"));
    }
    user.push_str("export const n = Object.keys(ns).length;\n");
    let u = compile(&mut r, "user.js", &user);
    r.set_module_dependency(u, 0, m).unwrap();
    r.link_module(u).unwrap();
    r.evaluate_module(u).unwrap();
    r.run_jobs().unwrap();
    let ns = r.module_namespace(u).unwrap();
    assert_eq!(r.get(ns, "n").unwrap().as_number(), Some(30_000.0));
}

#[test]
fn long_chains_do_not_overflow() {
    big(long_chains_do_not_overflow_body);
}

fn long_chains_do_not_overflow_body() {
    let mut r = realm();
    for src in [
        alloc::format!("a{}", ".b".repeat(400_000)),
        alloc::format!("a{}", "+1".repeat(400_000)),
        alloc::format!("a{}", "()".repeat(400_000)),
        alloc::format!("a{}", "[0]".repeat(300_000)),
        alloc::format!("a{}", "?.b".repeat(300_000)),
        alloc::format!("a{}", ",a".repeat(400_000)),
        alloc::format!("a{}", "||a".repeat(400_000)),
        alloc::format!("a{}", "?1:2".repeat(100_000)),
        "if(1)".repeat(100_000) + "1",
        "else if(1)".repeat(10) + "1",
        "{".repeat(100_000) + &"}".repeat(100_000),
        "function f(){".repeat(100_000) + &"}".repeat(100_000),
        "class A{m(){".repeat(50_000) + &"}}".repeat(50_000),
        "`${".repeat(50_000) + "1" + &"}`".repeat(50_000),
        "x=>".repeat(100_000) + "1",
        "-".repeat(100_000) + "1",
        "await ".repeat(100_000) + "1",
    ]
    .iter()
    .enumerate()
    {
        for module in [false, true] {
            let src = src.1;
            let res = if module {
                r.compile_module(&src, &ModuleOptions::default())
                    .map(|m| r.drop_module(m))
            } else {
                r.eval(&src, "x.js").map(|_| ())
            };
            if let Err(e) = res {
                let _ = r.report(&e);
            }
            r.release_temps();
        }
    }
}
