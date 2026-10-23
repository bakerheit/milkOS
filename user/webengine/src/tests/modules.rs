//! ES modules in pages (script/modrt.rs, script/modules.rs, script/loader.rs): pages with
//! `<script type=module>`, import maps and `import()` over a fake network. The page's requests
//! come out of `Document::take_net`, a closure plays every server, and its answers go back with
//! `Document::net_event`, one task each, the way the app does it. Tests that need the engine's
//! module support skip with a note while it can't compile one.

use alloc::string::String;
use alloc::vec::Vec;

use super::script::Host;
use crate::api::*;

/// Can the engine compile a module (is its module support in)?
fn modules_work() -> bool {
    let mut r = jsengine::Realm::new(jsengine::Options::default());
    let ok = r
        .compile_module("export const x = 1;", &jsengine::ModuleOptions::default())
        .is_ok();
    if !ok {
        std::println!("(skipped: the JavaScript engine can't compile modules yet)");
    }
    ok
}

/// What a fake server answers.
#[derive(Clone)]
pub struct Resp {
    /// Ticks (20 ms of the page's clock each) before the answer.
    pub after: u32,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub fail: Option<(NetFailureKind, String)>,
}

impl Resp {
    pub fn with(status: u16, ctype: Option<&str>, body: &str) -> Resp {
        let mut headers = Vec::new();
        if let Some(c) = ctype {
            headers.push((String::from("content-type"), String::from(c)));
        }
        Resp { after: 0, status, headers, body: body.as_bytes().to_vec(), fail: None }
    }
    pub fn js(body: &str) -> Resp {
        Resp::with(200, Some("text/javascript"), body)
    }
    pub fn json(body: &str) -> Resp {
        Resp::with(200, Some("application/json"), body)
    }
    pub fn late(mut self, ticks: u32) -> Resp {
        self.after = ticks;
        self
    }
    pub fn header(mut self, k: &str, v: &str) -> Resp {
        self.headers.push((String::from(k), String::from(v)));
        self
    }
    pub fn failed(kind: NetFailureKind, msg: &str) -> Resp {
        Resp { fail: Some((kind, String::from(msg))), ..Resp::with(0, None, "") }
    }
}

pub struct Out {
    pub lines: Vec<String>,
    pub reqs: Vec<NetRequest>,
    pub cancelled: Vec<u32>,
    pub doc: Document,
    pub host: Host,
    queue: Vec<(u32, u32, NetEvent)>,
    tick: u32,
}

impl Out {
    /// What the page logged with `console.log('T ...')`, without the `T `.
    pub fn t(&self) -> Vec<&str> {
        self.lines.iter().filter_map(|l| l.strip_prefix("T ")).collect()
    }
    pub fn has(&self, what: &str) -> bool {
        self.lines.iter().any(|l| l.contains(what))
    }
    pub fn asked(&self) -> Vec<&str> {
        self.reqs.iter().map(|r| r.url.as_str()).collect()
    }
}

fn answer(req: &NetRequest, r: Resp) -> Vec<(u32, NetEvent)> {
    if let Some((kind, message)) = r.fail {
        return alloc::vec![(r.after, NetEvent::Failed(NetFailure { kind, message }))];
    }
    let head = NetHead {
        status: r.status,
        status_text: String::from(match r.status {
            200 => "OK",
            404 => "Not Found",
            _ => "Status",
        }),
        headers: r.headers,
        url: req.url.clone(),
        redirected: false,
        kind: ResponseKind::Basic,
    };
    alloc::vec![(r.after, NetEvent::Head(head)), (r.after, NetEvent::Done(r.body))]
}

/// Run `ticks` ticks of a page; `serve` answers each request (once).
pub fn run_more(o: &mut Out, ticks: u32, serve: &mut dyn FnMut(&NetRequest) -> Resp) {
    for _ in 0..ticks {
        let tick = o.tick;
        o.doc.run(&mut o.host);
        loop {
            let mut got = false;
            for out in o.doc.take_net() {
                got = true;
                match out {
                    NetOut::Start(req) => {
                        for (after, ev) in answer(&req, serve(&req)) {
                            o.queue.push((tick + after, req.id, ev));
                        }
                        o.reqs.push(req);
                    }
                    NetOut::Cancel(id) => {
                        o.cancelled.push(id);
                        o.queue.retain(|q| q.1 != id);
                    }
                }
            }
            let mut i = 0;
            while i < o.queue.len() {
                if o.queue[i].0 <= tick {
                    let (_, id, ev) = o.queue.remove(i);
                    o.doc.net_event(&mut o.host, id, ev);
                    got = true;
                } else {
                    i += 1;
                }
            }
            o.lines.extend(o.doc.take_console().into_iter().map(|l| l.text));
            if !got {
                break;
            }
        }
        o.host.now += 20.0;
        o.tick += 1;
    }
}

pub fn start(html: &str) -> Out {
    Out {
        lines: Vec::new(),
        reqs: Vec::new(),
        cancelled: Vec::new(),
        doc: super::script::doc(html),
        host: Host::new(),
        queue: Vec::new(),
        tick: 0,
    }
}

pub fn run_page(html: &str, ticks: u32, mut serve: impl FnMut(&NetRequest) -> Resp) -> Out {
    let mut o = start(html);
    run_more(&mut o, ticks, &mut serve);
    o
}

/// A web of `(path, response)` on www.milk.test; anything else is a 404.
pub fn site(files: &'static [(&'static str, &'static str)]) -> impl FnMut(&NetRequest) -> Resp {
    move |req: &NetRequest| {
        let path = req.url.strip_prefix("http://www.milk.test").unwrap_or(&req.url);
        let path = path.split('?').next().unwrap_or(path);
        match files.iter().find(|(p, _)| *p == path) {
            Some((p, body)) if p.ends_with(".json") => Resp::json(body),
            Some((_, body)) => Resp::js(body),
            None => Resp::with(404, Some("text/html"), "<h1>404</h1>"),
        }
    }
}

// ---- no engine needed: the requests ------------------------------------------------------------

#[test]
fn module_requests_are_cors_gets_with_the_right_credentials() {
    let o = run_page(
        "<!DOCTYPE html><body>\
         <script type=module src=/a.js></script>\
         <script type=module crossorigin=use-credentials src=/b.js></script>\
         <script type=module crossorigin src='http://cdn.other.test/c.js'></script>\
         <link rel=modulepreload href=/pre.js>",
        3,
        |_| Resp::failed(NetFailureKind::Network, "nope"),
    );
    let want = [
        ("http://www.milk.test/a.js", CredentialsMode::SameOrigin),
        ("http://www.milk.test/b.js", CredentialsMode::Include),
        ("http://cdn.other.test/c.js", CredentialsMode::SameOrigin),
        ("http://www.milk.test/pre.js", CredentialsMode::SameOrigin),
    ];
    assert_eq!(o.reqs.len(), 4, "{:?}", o.asked());
    for (r, (url, creds)) in o.reqs.iter().zip(want) {
        assert_eq!(r.url, url);
        assert_eq!(r.kind, NetKind::Module);
        assert_eq!(r.mode, RequestMode::Cors);
        assert_eq!(r.credentials, creds, "{url}");
        assert_eq!(r.method, "GET");
        assert!(r.body.is_empty());
    }
    // None of them went through the classic script path.
    assert!(o.doc.pending_scripts().is_empty());
}

#[test]
fn a_failed_module_fetch_is_an_error_event_and_a_console_line() {
    let o = run_page(
        "<!DOCTYPE html><body>\
         <script type=module src=/gone.js onerror=\"console.log('T error gone')\" onload=\"console.log('T load gone')\"></script>\
         <script type=module src=/net.js onerror=\"console.log('T error net')\"></script>\
         <script type=module src=/html.js onerror=\"console.log('T error html')\"></script>\
         <script type=module src=/gone.js onerror=\"console.log('T error again')\"></script>\
         <script>window.onload = function () { console.log('T window load'); };</script>",
        6,
        |req| match req.url.rsplit('/').next().unwrap_or("") {
            "gone.js" => Resp::with(404, Some("text/javascript"), "export {}"),
            "net.js" => Resp::failed(NetFailureKind::Blocked, "Blocked by CORS: no Access-Control-Allow-Origin"),
            _ => Resp::with(200, Some("text/html"), "<p>not a script</p>"),
        },
    );
    let t = o.t();
    assert!(t.contains(&"error gone") && t.contains(&"error net") && t.contains(&"error html") && t.contains(&"error again"), "{:?}", o.lines);
    assert!(!t.contains(&"load gone"));
    // The page's load event still comes.
    assert!(t.contains(&"window load"), "{:?}", o.lines);
    assert!(o.has("Failed to load module script http://www.milk.test/gone.js: the server responded with a status of 404 (Not Found)"), "{:?}", o.lines);
    assert!(o.has("Failed to load module script http://www.milk.test/net.js: Blocked by CORS"), "{:?}", o.lines);
    assert!(o.has("MIME type of \"text/html\""), "{:?}", o.lines);
    // The two that were refused at the head are cancelled (no body is downloaded), and gone.js
    // was asked for once.
    assert_eq!(o.reqs.iter().filter(|r| r.url.ends_with("/gone.js")).count(), 1);
    assert_eq!(o.cancelled.len(), 2, "{:?}", o.cancelled);
    assert_eq!(o.doc.net_pending(), 0);
}

#[test]
fn nomodule_scripts_are_skipped_and_classic_ones_run() {
    let o = run_page(
        "<!DOCTYPE html><body>\
         <script nomodule>console.log('T nomodule inline')</script>\
         <script nomodule src=/old.js></script>\
         <script>console.log('T classic')</script>\
         <script nomodule type=text/javascript>console.log('T nomodule again')</script>",
        3,
        |_| Resp::js("console.log('T old.js')"),
    );
    assert_eq!(o.t(), ["classic"]);
    assert!(o.reqs.is_empty());
    assert!(o.doc.pending_scripts().is_empty());
}

#[test]
fn import_maps_come_before_module_loading_and_only_one_counts() {
    let o = run_page(
        "<!DOCTYPE html><body>\
         <script type=importmap>{\"imports\": {\"lib\": \"/vendor/lib.js\"}}</script>\
         <script type=importmap>{\"imports\": {\"lib\": \"/other.js\"}}</script>\
         <script type=importmap src=/map.json></script>\
         <script type=module src=/main.js></script>\
         <script type=importmap>{\"imports\": {\"late\": \"/late.js\"}}</script>",
        3,
        |req| {
            if req.url.ends_with("/main.js") {
                Resp::js("import \"lib\";")
            } else {
                Resp::js("")
            }
        },
    );
    if !modules_work() {
        return;
    }
    assert_eq!(o.asked(), ["http://www.milk.test/main.js", "http://www.milk.test/vendor/lib.js"], "{:?}", o.lines);
    assert!(o.has("second import map was ignored"), "{:?}", o.lines);
    assert!(o.has("External import maps"), "{:?}", o.lines);
    assert!(o.has("after module loading had started"), "{:?}", o.lines);
}

#[test]
fn stopping_the_scripts_cancels_module_fetches() {
    let mut o = start(
        "<!DOCTYPE html><body><script type=module src=/slow.js></script><script type=module src=/slow2.js></script>",
    );
    run_more(&mut o, 2, &mut |_| Resp::js("export {}").late(50));
    assert_eq!(o.reqs.len(), 2);
    assert_eq!(o.doc.net_pending(), 2);
    o.doc.stop_scripts();
    let outs = o.doc.take_net();
    let ids: Vec<u32> = o.reqs.iter().map(|r| r.id).collect();
    for id in ids {
        assert!(outs.contains(&NetOut::Cancel(id)), "{outs:?}");
    }
    assert_eq!(o.doc.net_pending(), 0);
    // A late answer is ignored.
    run_more(&mut o, 60, &mut |_| Resp::js("export {}"));
    assert!(o.doc.take_net().is_empty());
}

// ---- with the engine -----------------------------------------------------------------------------

#[test]
fn inline_and_external_modules_import_each_other() {
    if !modules_work() {
        return;
    }
    let o = run_page(
        "<!DOCTYPE html><body>\
         <script type=module>\
           import { a } from './a.js';\
           import b from './b.js';\
           console.log('T inline ' + a + ' ' + b + ' ' + (document.currentScript === null) + ' ' + (typeof this));\
         </script>\
         <script type=module src=/c.js></script>",
        8,
        site(&[
            ("/a.js", "export const a = 'A'; console.log('T a');"),
            ("/b.js", "import { a } from './a.js'; console.log('T b ' + a); export default 'B';"),
            ("/c.js", "import b from './b.js'; import * as ns from './a.js'; console.log('T c ' + b + ' ' + Object.keys(ns));"),
        ]),
    );
    assert_eq!(o.t(), ["a", "b A", "inline A B true undefined", "c B a"], "{:?}", o.lines);
    // Each module was fetched once.
    let mut asked = o.asked();
    asked.sort();
    assert_eq!(
        asked,
        ["http://www.milk.test/a.js", "http://www.milk.test/b.js", "http://www.milk.test/c.js"]
    );
    assert_eq!(o.doc.script_stats().errors, 0, "{:?}", o.lines);
}

#[test]
fn a_cycle() {
    if !modules_work() {
        return;
    }
    let o = run_page(
        "<!DOCTYPE html><body><script type=module src=/even.js></script>",
        8,
        site(&[
            ("/even.js", "import { odd } from './odd.js'; export function even(n) { return n === 0 ? true : odd(n - 1); } console.log('T even ' + odd(7));"),
            ("/odd.js", "import { even } from './even.js'; export function odd(n) { return n === 0 ? false : even(n - 1); } console.log('T odd loaded');"),
        ]),
    );
    assert_eq!(o.t(), ["odd loaded", "even true"], "{:?}", o.lines);
}

#[test]
fn bare_specifiers_fail_until_an_import_map_maps_them() {
    if !modules_work() {
        return;
    }
    let files: &'static [(&str, &str)] = &[
        ("/main.js", "import { f } from 'lib'; console.log('T main ' + f());"),
        ("/vendor/lib.js", "export const f = () => 'lib!';"),
    ];
    // Without a map: a TypeError through window.onerror, nothing fetched but main.js.
    let o = run_page(
        "<!DOCTYPE html><body><script>window.onerror = function (m, f, l, c) { console.log('T onerror ' + m + ' @' + f); };</script>\
         <script type=module src=/main.js></script>",
        6,
        site(files),
    );
    assert_eq!(o.asked(), ["http://www.milk.test/main.js"], "{:?}", o.lines);
    assert!(o.t().iter().any(|l| l.starts_with("onerror Uncaught TypeError: Failed to resolve module specifier \"lib\"") && l.ends_with("@http://www.milk.test/main.js")), "{:?}", o.lines);
    assert!(!o.t().iter().any(|l| l.starts_with("main")));
    // With one.
    let o = run_page(
        "<!DOCTYPE html><body><script type=importmap>{\"imports\": {\"lib\": \"/vendor/lib.js\"}}</script>\
         <script type=module src=/main.js></script>",
        6,
        site(files),
    );
    assert_eq!(o.t(), ["main lib!"], "{:?}", o.lines);
    assert_eq!(o.asked(), ["http://www.milk.test/main.js", "http://www.milk.test/vendor/lib.js"]);
}

#[test]
fn execution_order_defer_async_and_documents_events() {
    if !modules_work() {
        return;
    }
    let mut serve = |req: &NetRequest| {
        let name = String::from(req.url.rsplit('/').next().unwrap_or(""));
        match name.as_str() {
            "slow.js" => Resp::js("console.log('T slow module');").late(4),
            "fast.js" => Resp::js("console.log('T fast module');"),
            "async.js" => Resp::js("console.log('T async module');").late(1),
            "classic.js" => Resp::js("console.log('T classic defer');").late(2),
            _ => Resp::with(404, None, ""),
        }
    };
    let o = run_page(
        "<!DOCTYPE html><body>\
         <script defer src=/classic.js></script>\
         <script type=module src=/slow.js></script>\
         <script type=module src=/fast.js></script>\
         <script type=module async src=/async.js></script>\
         <script type=module>console.log('T inline module ' + document.readyState);</script>\
         <script>console.log('T blocking ' + document.readyState);\
           document.addEventListener('DOMContentLoaded', function () { console.log('T DOMContentLoaded'); });\
           window.addEventListener('load', function () { console.log('T load'); });</script>",
        12,
        &mut serve,
    );
    // The blocking script first; then the async module as soon as it is in; then the deferred
    // ones in document order (the fast module waits for the slow one before it); then
    // DOMContentLoaded and load.
    assert_eq!(
        o.t(),
        [
            "blocking loading",
            "async module",
            "classic defer",
            "slow module",
            "fast module",
            "inline module interactive",
            "DOMContentLoaded",
            "load"
        ],
        "{:?}",
        o.lines
    );
}

#[test]
fn top_level_await_does_not_hold_up_sibling_scripts() {
    if !modules_work() {
        return;
    }
    let o = run_page(
        "<!DOCTYPE html><body>\
         <script type=module src=/tla.js></script>\
         <script type=module>console.log('T sibling');</script>\
         <script type=module src=/dep.js></script>\
         <script>window.addEventListener('load', function () { console.log('T load'); });</script>",
        12,
        site(&[
            ("/tla.js", "console.log('T tla start'); await new Promise(r => setTimeout(r, 100)); console.log('T tla end');"),
            ("/dep.js", "import './tla.js'; console.log('T dep after tla');"),
        ]),
    );
    let t = o.t();
    assert_eq!(&t[..2], ["tla start", "sibling"], "{:?}", o.lines);
    // dep.js waits for tla.js (its import); the load event does not wait for the timer.
    let pos = |s: &str| t.iter().position(|x| *x == s).unwrap_or(usize::MAX);
    assert!(pos("tla end") < pos("dep after tla"), "{t:?}");
    assert!(pos("load") < pos("tla end"), "{t:?}");
}

#[test]
fn errors_in_one_module_do_not_break_others() {
    if !modules_work() {
        return;
    }
    let o = run_page(
        "<!DOCTYPE html><body>\
         <script>window.onerror = function (m, f, l, c) { console.log('T onerror ' + m); return true; };</script>\
         <script type=module src=/throws.js></script>\
         <script type=module src=/syntax.js></script>\
         <script type=module src=/importsbad.js></script>\
         <script type=module src=/missingexport.js></script>\
         <script type=module>console.log('T fine');</script>",
        8,
        site(&[
            ("/throws.js", "console.log('T before'); throw new Error('boom'); "),
            ("/syntax.js", "export const = ;"),
            ("/importsbad.js", "import './nothere.js'; console.log('T not reached');"),
            ("/missingexport.js", "import { nope } from './a.js';"),
            ("/a.js", "export const a = 1;"),
        ]),
    );
    let t = o.t();
    assert!(t.contains(&"before") && t.contains(&"fine"), "{:?}", o.lines);
    assert!(t.contains(&"onerror Uncaught Error: boom"), "{:?}", o.lines);
    assert!(t.iter().any(|l| l.starts_with("onerror Uncaught SyntaxError")), "{:?}", o.lines);
    assert!(t.iter().filter(|l| l.starts_with("onerror Uncaught SyntaxError")).count() >= 2, "{:?}", o.lines);
    assert!(!t.contains(&"not reached"));
    assert!(o.has("nothere.js"), "{:?}", o.lines);
}

#[test]
fn dynamic_import_from_classic_scripts_and_modules() {
    if !modules_work() {
        return;
    }
    let o = run_page(
        "<!DOCTYPE html><body>\
         <script>\
           import('/lib/x.js').then(function (ns) { console.log('T classic got ' + ns.x + ' ' + Object.prototype.toString.call(ns)); });\
           import('./lib/x.js').then(function (ns) { console.log('T same module ' + ns.count); });\
           import('/nothing.js').catch(function (e) { console.log('T rejected ' + e.name + ' ' + e.message); });\
           import('bare').catch(function (e) { console.log('T bare ' + e.name); });\
         </script>\
         <script type=module>\
           const m = await import('./lib/y.js');\
           console.log('T module got ' + m.default + ' ' + import.meta.url);\
         </script>",
        10,
        site(&[
            ("/lib/x.js", "export const x = 'X'; export let count = 1; console.log('T x runs');"),
            ("/lib/y.js", "import { x } from './x.js'; export default 'Y' + x;"),
        ]),
    );
    let t = o.t();
    assert_eq!(t.iter().filter(|l| **l == "x runs").count(), 1, "{:?}", o.lines);
    assert!(t.contains(&"classic got X [object Module]"), "{:?}", o.lines);
    assert!(t.contains(&"same module 1"), "{:?}", o.lines);
    assert!(t.iter().any(|l| l.starts_with("rejected TypeError Failed to fetch dynamically imported module: http://www.milk.test/nothing.js")), "{:?}", o.lines);
    assert!(t.contains(&"bare TypeError"), "{:?}", o.lines);
    assert!(t.contains(&"module got YX http://www.milk.test/page.html"), "{:?}", o.lines);
}

#[test]
fn import_meta_and_json_modules() {
    if !modules_work() {
        return;
    }
    let o = run_page(
        "<!DOCTYPE html><body>\
         <script type=importmap>{\"imports\": {\"cfg\": \"/data/config.json\"}}</script>\
         <script type=module src=/app/main.js></script>",
        10,
        site(&[
            (
                "/app/main.js",
                "import cfg from 'cfg' with { type: 'json' };\
                 import * as meta from './meta.js';\
                 console.log('T json ' + cfg.name + ' ' + cfg.list.length);\
                 console.log('T url ' + import.meta.url);\
                 console.log('T resolve ' + import.meta.resolve('./z.js') + ' ' + import.meta.resolve('cfg'));\
                 console.log('T meta ' + meta.url);",
            ),
            ("/app/meta.js", "export const url = import.meta.url;"),
            ("/data/config.json", "{\"name\": \"milk\", \"list\": [1, 2, 3], \"__proto__\": 1}"),
        ]),
    );
    let t = o.t();
    assert!(t.contains(&"json milk 3"), "{:?}", o.lines);
    assert!(t.contains(&"url http://www.milk.test/app/main.js"), "{:?}", o.lines);
    assert!(t.contains(&"resolve http://www.milk.test/app/z.js http://www.milk.test/data/config.json"), "{:?}", o.lines);
    assert!(t.contains(&"meta http://www.milk.test/app/meta.js"), "{:?}", o.lines);
    // A JSON module imported without the attribute is refused (the type is JSON, not JavaScript).
    let o = run_page(
        "<!DOCTYPE html><body><script type=module src=/m.js onerror=\"console.log('T error')\"></script>",
        6,
        site(&[("/m.js", "import x from './d.json'; console.log('T no')"), ("/d.json", "{}")]),
    );
    assert!(o.t().contains(&"error") && !o.t().contains(&"no"), "{:?}", o.lines);
    assert!(o.has("MIME type of \"application/json\""), "{:?}", o.lines);
    // CSS modules and unknown attributes are not supported.
    let o = run_page(
        "<!DOCTYPE html><body><script>window.onerror = function (m) { console.log('T onerror ' + m); };</script><script type=module src=/m.js></script>",
        6,
        site(&[("/m.js", "import x from './d.json' with { type: 'css' };")]),
    );
    assert!(o.t().iter().any(|l| l.contains("\"css\" is not supported")), "{:?}", o.lines);
}

#[test]
fn modulepreload_warms_the_map_and_fetches_the_imports_too() {
    if !modules_work() {
        return;
    }
    let o = run_page(
        "<!DOCTYPE html><head><link rel=modulepreload href=/main.js></head><body>\
         <script type=module src=/main.js></script><script type=module>import './dep.js'; console.log('T inline');</script>",
        10,
        site(&[("/main.js", "import './dep.js'; console.log('T main');"), ("/dep.js", "console.log('T dep');")]),
    );
    assert_eq!(o.t(), ["dep", "main", "inline"], "{:?}", o.lines);
    let mut asked = o.asked();
    asked.sort();
    assert_eq!(asked, ["http://www.milk.test/dep.js", "http://www.milk.test/main.js"], "once each");
}

#[test]
fn scripts_a_page_inserts() {
    if !modules_work() {
        return;
    }
    let o = run_page(
        "<!DOCTYPE html><body><script>\
           var s = document.createElement('script'); s.type = 'module'; s.src = '/ins.js';\
           s.onload = function () { console.log('T onload ' + s.noModule); };\
           document.body.appendChild(s);\
           var t = document.createElement('script'); t.type = 'module';\
           t.textContent = \"console.log('T inline inserted'); import('/ins.js').then(function () { console.log('T again'); });\";\
           document.body.appendChild(t);\
           console.log('T after append');\
           var m = document.createElement('script'); m.type = 'importmap'; m.textContent = '{}';\
           document.body.appendChild(m);\
         </script>",
        8,
        site(&[("/ins.js", "console.log('T ins runs');")]),
    );
    let t = o.t();
    assert_eq!(t[0], "after append", "{:?}", o.lines);
    for want in ["inline inserted", "ins runs", "onload false", "again"] {
        assert!(t.contains(&want), "{want}: {:?}", o.lines);
    }
    assert!(o.has("after module loading had started"), "{:?}", o.lines);
}

#[test]
fn per_page_limits() {
    if !modules_work() {
        return;
    }
    // An endless chain: each module imports the next.
    let mut served = 0u32;
    let o = run_page(
        "<!DOCTYPE html><body><script>window.onerror = function (m) { console.log('T onerror ' + m.slice(0, 80)); };</script>\
         <script type=module src=/chain/0.js></script>",
        400,
        |req| {
            served += 1;
            let n: u32 = req.url.rsplit('/').next().unwrap().trim_end_matches(".js").parse().unwrap_or(0);
            Resp::js(&alloc::format!("import './{}.js';", n + 1))
        },
    );
    assert!(served <= 300, "the chain was cut at its depth limit, not followed for ever: {served}");
    assert!(o.t().iter().any(|l| l.contains("nest too deeply")), "{:?}", o.lines);
    assert_eq!(o.doc.net_pending(), 0);
    // A module that imports thousands of others.
    let mut text = String::new();
    for i in 0..2500 {
        text.push_str(&alloc::format!("import './m{i}.js';\n"));
    }
    let text: &'static str = alloc::boxed::Box::leak(text.into_boxed_str());
    let mut served = 0u32;
    let o = run_page(
        "<!DOCTYPE html><body><script>window.onerror = function (m) { console.log('T onerror ' + m.slice(0, 80)); };</script>\
         <script type=module src=/wide.js></script>",
        200,
        |req| {
            served += 1;
            if req.url.ends_with("/wide.js") { Resp::js(text) } else { Resp::js("") }
        },
    );
    assert!(o.t().iter().any(|l| l.contains("too many modules")), "{:?}", o.lines);
    assert!(served <= 2001, "{served}");
    assert_eq!(o.doc.net_pending(), 0);
}

#[test]
fn hostile_import_maps_and_specifiers_stay_bounded() {
    let big_scopes: String = {
        let mut s = String::from("{\"scopes\": {");
        for i in 0..3000 {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&alloc::format!("\"/s{i}/\": {{\"a\": \"/x{i}.js\"}}"));
        }
        s.push_str("}}");
        s
    };
    let html = alloc::format!("<!DOCTYPE html><body><script type=importmap>{big_scopes}</script><script type=module>import 'a';</script>");
    let mut o = run_page(&html, 3, |_| Resp::js(""));
    assert!(o.doc.take_net().is_empty());
    for map in ["", "null", "[]", "{\"imports\": 1}", "{\"imports\": {\"a\": [1]}}", "{{{{", "{\"scopes\": {\"\": {}}}", "\u{0}"] {
        let html = alloc::format!("<!DOCTYPE html><body><script type=importmap>{map}</script><script type=module>import 'a';</script>");
        let o = run_page(&html, 3, |_| Resp::js(""));
        assert!(o.reqs.is_empty(), "{map:?}");
    }
}
