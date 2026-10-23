//! ES module conformance: cases written from the spec, run through a small test host that
//! plays the part of the Browser's loader (a map of file name to source, relative
//! specifiers, a module map, `import()` answered between turns of the job queue).
//!
//! A case file in tests/modules holds cases:
//!
//! ```text
//! //# case: a name
//! //# file: main.js          (the entry; any other file is reached by import)
//! ...source...
//! //# file: dep.js
//! ...source...
//! //# expect
//! lines the `log(...)` calls must have produced, in order
//! ```
//!
//! The host adds lines of its own to the log: `link error: ...` when linking fails,
//! `eval error: ...` when the entry module fails and `load error: ...` when a file is
//! missing or does not compile. `//# oracle: no` marks a case whose output Node would not
//! reproduce (engine-specific messages); the other cases are also run under Node by
//! scripts/jsmodules-oracle.sh.

use crate::*;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

pub(crate) struct Case {
    pub name: String,
    pub files: Vec<(String, String)>,
    pub expect: Vec<String>,
    pub oracle: bool,
}

pub(crate) fn parse_cases(text: &str) -> Vec<Case> {
    let mut cases: Vec<Case> = Vec::new();
    #[derive(PartialEq)]
    enum Mode {
        None,
        File,
        Expect,
    }
    let mut mode = Mode::None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("//# ") {
            if let Some(n) = rest.strip_prefix("case: ") {
                cases.push(Case {
                    name: String::from(n.trim()),
                    files: Vec::new(),
                    expect: Vec::new(),
                    oracle: true,
                });
                mode = Mode::None;
                continue;
            }
            if let Some(n) = rest.strip_prefix("file: ") {
                if let Some(c) = cases.last_mut() {
                    c.files.push((String::from(n.trim()), String::new()));
                }
                mode = Mode::File;
                continue;
            }
            if rest.trim() == "expect" {
                mode = Mode::Expect;
                continue;
            }
            if rest.trim() == "oracle: no" {
                if let Some(c) = cases.last_mut() {
                    c.oracle = false;
                }
                continue;
            }
        }
        let Some(c) = cases.last_mut() else { continue };
        match mode {
            Mode::File => {
                if let Some(f) = c.files.last_mut() {
                    f.1.push_str(line);
                    f.1.push('\n');
                }
            }
            Mode::Expect => {
                if !line.is_empty() {
                    c.expect.push(String::from(line));
                }
            }
            Mode::None => {}
        }
    }
    cases
}

/// Resolves `spec` against the importing file `base` (relative and absolute paths only;
/// anything else is a bare name, kept as is).
pub(crate) fn resolve(base: &str, spec: &str) -> String {
    if !(spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/')) {
        return String::from(spec);
    }
    let mut parts: Vec<&str> = if spec.starts_with('/') {
        Vec::new()
    } else {
        let mut p: Vec<&str> = base.split('/').collect();
        p.pop();
        p
    };
    for seg in spec.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// A test host: files, a module map, a log.
pub(crate) struct Host {
    pub files: Vec<(String, String)>,
    map: Vec<(String, ModuleId)>,
    pub log: Rc<RefCell<Vec<String>>>,
    pending: Rc<RefCell<Vec<DynamicImport>>>,
}

pub(crate) fn describe(r: &mut Realm, v: Value) -> String {
    let rep = r.report_value(v);
    if rep.message.is_empty() {
        rep.name
    } else {
        alloc::format!("{}: {}", rep.name, rep.message)
    }
}

impl Host {
    pub fn new(files: Vec<(String, String)>) -> Host {
        Host {
            files,
            map: Vec::new(),
            log: Rc::new(RefCell::new(Vec::new())),
            pending: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// A realm with `log`, the hooks, and a generous stack for debug builds.
    pub fn realm(&self, stress: u32) -> Realm {
        let mut r = Realm::new(Options::default());
        r.heap.stress = stress;
        r.set_stack_limit(6 << 20);
        let log = self.log.clone();
        let f = r.new_function("log", 1, move |r, _this, args| {
            let mut parts: Vec<String> = Vec::new();
            for a in args {
                let s = match a.kind() {
                    Kind::String => r.get_string(*a).unwrap_or_default(),
                    _ => r.inspect(*a),
                };
                parts.push(s);
            }
            log.borrow_mut().push(parts.join(" "));
            Ok(Value::UNDEFINED)
        });
        let g = r.global();
        r.set(g, "log", f).unwrap();
        r.set_import_meta_hook(Some(Box::new(|r, m, meta| {
            let file = r.module_file(m).unwrap_or_default();
            let url = r.string(&alloc::format!("test:///{file}"));
            r.set(meta, "url", url)
        })));
        let pending = self.pending.clone();
        r.set_dynamic_import_hook(Some(Box::new(move |_r, di| {
            pending.borrow_mut().push(di);
            Ok(())
        })));
        r
    }

    fn fail(&mut self, r: &mut Realm, name: &str, e: &Exception) -> Value {
        let v = e.value().unwrap_or(Value::UNDEFINED);
        let _ = name;
        v
    }

    /// Compiles `name` and everything it imports (cycles share the module map entry).
    pub fn load(
        &mut self,
        r: &mut Realm,
        name: &str,
        attrs: &[(String, String)],
    ) -> Result<ModuleId, Value> {
        let key = alloc::format!("{name}|{attrs:?}");
        if let Some((_, m)) = self.map.iter().find(|(k, _)| *k == key) {
            return Ok(*m);
        }
        let Some((_, src)) = self.files.iter().find(|(n, _)| n == name).cloned() else {
            return Err(r.new_error(
                ErrorKind::TypeError,
                &alloc::format!("Failed to fetch {name}"),
            ));
        };
        if name.ends_with(".json") {
            if attrs != [(String::from("type"), String::from("json"))] {
                return Err(r.new_error(
                    ErrorKind::TypeError,
                    &alloc::format!("{name} needs type: json"),
                ));
            }
            let v = r.json_parse(&src).map_err(|e| self.fail(r, name, &e))?;
            let m = r
                .compile_synthetic_module(name, &[("default", v)])
                .map_err(|e| self.fail(r, name, &e))?;
            self.map.push((key, m));
            return Ok(m);
        }
        let opts = ModuleOptions {
            file: name,
            ..ModuleOptions::default()
        };
        let m = r
            .compile_module(&src, &opts)
            .map_err(|e| self.fail(r, name, &e))?;
        self.map.push((key, m));
        let reqs = r.module_requests(m);
        for (i, q) in reqs.iter().enumerate() {
            let target = resolve(name, &q.specifier);
            let dep = self.load(r, &target, &q.attributes)?;
            r.set_module_dependency(m, i, dep).unwrap();
        }
        Ok(m)
    }

    /// Answers the `import()` calls that are waiting, until there are none.
    pub fn pump(&mut self, r: &mut Realm) {
        for _ in 0..10_000 {
            if let Err(e) = r.run_jobs() {
                let rep = r.report(&e);
                self.log.borrow_mut().push(alloc::format!("job error: {}: {}", rep.name, rep.message));
            }
            let batch: Vec<DynamicImport> = core::mem::take(&mut *self.pending.borrow_mut());
            if batch.is_empty() {
                break;
            }
            for di in batch {
                let base = match di.referrer {
                    Some(m) => r.module_file(m).unwrap_or_default(),
                    None => di.referrer_file.clone(),
                };
                let target = resolve(&base, &di.request.specifier);
                let res = self.load(r, &target, &di.request.attributes);
                let _ = r.finish_dynamic_import(di.id, res);
            }
        }
    }

    /// Runs `entry`: load, link, evaluate, drain.
    pub fn run(&mut self, r: &mut Realm, entry: &str) {
        let m = match self.load(r, entry, &[]) {
            Ok(m) => m,
            Err(v) => {
                let d = describe(r, v);
                self.log.borrow_mut().push(alloc::format!("load error: {d}"));
                return;
            }
        };
        if let Err(e) = r.link_module(m) {
            let v = e.value().unwrap_or(Value::UNDEFINED);
            let d = describe(r, v);
            self.log.borrow_mut().push(alloc::format!("link error: {d}"));
            return;
        }
        match r.evaluate_module(m) {
            Ok(_) => {}
            Err(e) => {
                let rep = r.report(&e);
                self.log.borrow_mut().push(alloc::format!("evaluate failed: {}", rep.name));
                return;
            }
        }
        self.pump(r);
        if let Some(v) = r.module_error(m) {
            let d = describe(r, v);
            self.log.borrow_mut().push(alloc::format!("eval error: {d}"));
        }
        // Unhandled rejections show up in the log too.
        for v in r.take_unhandled_rejections() {
            let d = describe(r, v);
            self.log.borrow_mut().push(alloc::format!("unhandled rejection: {d}"));
        }
    }
}

pub(crate) fn run_case(case: &Case, stress: u32) -> Result<(), String> {
    let mut host = Host::new(case.files.clone());
    let mut r = host.realm(stress);
    if case.files.iter().any(|(n, _)| n == "main.js") {
        host.run(&mut r, "main.js");
    } else if let Some((_, src)) = case.files.iter().find(|(n, _)| n == "script.js") {
        // A classic script as the entry.
        if let Err(e) = r.eval(src, "script.js") {
            let rep = r.report(&e);
            host.log
                .borrow_mut()
                .push(alloc::format!("script error: {}: {}", rep.name, rep.message));
        }
        host.pump(&mut r);
        for v in r.take_unhandled_rejections() {
            let d = describe(&mut r, v);
            host.log.borrow_mut().push(alloc::format!("unhandled rejection: {d}"));
        }
    }
    let got = host.log.borrow().clone();
    if got == case.expect {
        Ok(())
    } else {
        Err(alloc::format!(
            "case '{}':\n  expected: {:?}\n  got:      {:?}",
            case.name, case.expect, got
        ))
    }
}

macro_rules! mod_files {
    ($($name:literal),* $(,)?) => {
        pub(crate) const FILES: &[(&str, &str)] = &[$(($name, include_str!(concat!("../../tests/modules/", $name)))),*];
    };
}

mod_files!("basics.txt", "static.txt", "cycles.txt", "namespace.txt", "tla.txt", "errors.txt", "dynamic.txt");

fn run_all(stress: u32) {
    let mut failures: Vec<String> = Vec::new();
    let mut total = 0;
    for (name, text) in FILES {
        for case in parse_cases(text) {
            total += 1;
            if let Err(m) = run_case(&case, stress) {
                failures.push(alloc::format!("{name}: {m}"));
            }
        }
    }
    assert!(total > 0);
    assert!(
        failures.is_empty(),
        "{} of {} module case(s) failed:\n{}",
        failures.len(),
        total,
        failures.join("\n")
    );
}

#[test]
fn module_conformance() {
    let h = std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(|| run_all(0))
        .unwrap();
    h.join().unwrap();
}

#[test]
fn module_conformance_gc_stress() {
    let h = std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(|| run_all(1))
        .unwrap();
    h.join().unwrap();
}
