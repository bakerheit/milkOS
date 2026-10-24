//! Dedicated workers, end to end on the Mac: a page's `Worker` objects, a real thread per worker
//! running [`run_worker`], and a small app in between ([`Sim`]) that does what the Browser does:
//! carries the messages both ways, answers the workers' requests from a table of files, and
//! terminates on request. The pages and workers report through `console.log`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::script::{Host, doc};
use crate::api::*;

struct Chan {
    to: Mutex<VecDeque<ToWorker>>,
    from: Mutex<VecDeque<FromWorker>>,
    cv: Condvar,
    term: AtomicBool,
}

struct Io {
    ch: Arc<Chan>,
    t0: Instant,
}

impl WorkerIo for Io {
    fn now(&self) -> f64 {
        self.t0.elapsed().as_secs_f64() * 1000.0
    }
    fn time(&self) -> f64 {
        1_800_000_000_000.0 + self.now()
    }
    fn random(&mut self, out: &mut [u8]) -> bool {
        let mut x = 0x9e37_79b9_7f4a_7c15u64 ^ self.t0.elapsed().as_nanos() as u64;
        for b in out.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
        true
    }
    fn recv(&mut self, timeout_ms: Option<f64>) -> Recv {
        let deadline = timeout_ms.map(|t| Instant::now() + Duration::from_micros((t * 1000.0) as u64));
        let mut q = self.ch.to.lock().unwrap();
        loop {
            if self.ch.term.load(Ordering::SeqCst) {
                return Recv::Terminated;
            }
            if let Some(m) = q.pop_front() {
                return Recv::Msg(m);
            }
            match deadline {
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        return Recv::Timeout;
                    }
                    q = self.ch.cv.wait_timeout(q, d - now).unwrap().0;
                }
                None => q = self.ch.cv.wait(q).unwrap(),
            }
        }
    }
    fn send(&mut self, m: FromWorker) -> bool {
        self.ch.from.lock().unwrap().push_back(m);
        true
    }
    fn terminated(&self) -> bool {
        self.ch.term.load(Ordering::SeqCst)
    }
}

struct W {
    ch: Arc<Chan>,
    name: String,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// What a served file looks like to a worker.
pub struct File {
    pub url: String,
    pub ctype: String,
    pub body: Vec<u8>,
    pub status: u16,
}

pub struct Sim {
    pub doc: Document,
    pub host: Host,
    pub files: Vec<File>,
    workers: BTreeMap<u32, W>,
    /// The page's console.
    pub lines: Vec<String>,
    /// The workers' consoles, `[name] text`.
    pub wlines: Vec<String>,
    pub requests: Vec<NetRequest>,
    pub closed: Vec<(u32, CloseWhy)>,
    pub spawns: Vec<WorkerSpawn>,
    pub limits: WorkerLimits,
    /// Refuse to start more than this many at once (the app's cap).
    pub max_live: usize,
    pub heap: BTreeMap<u32, usize>,
}

pub fn file(url: &str, body: &str) -> File {
    File { url: String::from(url), ctype: String::from("text/javascript"), body: body.as_bytes().to_vec(), status: 200 }
}

impl Sim {
    pub fn new(html_script: &str, files: Vec<File>) -> Sim {
        let html = format!("<!DOCTYPE html><body><script>{html_script}</script></body>");
        Sim {
            doc: doc(&html),
            host: Host::new(),
            files,
            workers: BTreeMap::new(),
            lines: Vec::new(),
            wlines: Vec::new(),
            requests: Vec::new(),
            closed: Vec::new(),
            spawns: Vec::new(),
            limits: WorkerLimits { stack: 4 << 20, ..WorkerLimits::default() },
            max_live: 16,
            heap: BTreeMap::new(),
        }
    }

    /// Every `T ` line the page and the workers logged, without the prefix.
    pub fn t(&self) -> Vec<String> {
        self.lines
            .iter()
            .chain(self.wlines.iter())
            .filter_map(|l| {
                let l = l.strip_prefix('[').and_then(|x| x.split_once("] ")).map_or(l.as_str(), |x| x.1);
                l.strip_prefix("T ").map(String::from)
            })
            .collect()
    }

    pub fn has(&self, want: &str) -> bool {
        self.t().iter().any(|l| l == want)
    }

    fn serve(&mut self, w: u32, req: NetRequest) {
        self.requests.push(req.clone());
        let found = self.files.iter().find(|f| f.url == req.url);
        let ch = self.workers.get(&w).map(|x| x.ch.clone());
        let Some(ch) = ch else { return };
        let mut q = ch.to.lock().unwrap();
        match found {
            Some(f) => {
                let h = NetHead {
                    status: f.status,
                    status_text: String::from("OK"),
                    headers: vec![(String::from("content-type"), f.ctype.clone())],
                    url: req.url.clone(),
                    redirected: false,
                    kind: ResponseKind::Basic,
                };
                if matches!(req.kind, NetKind::Script | NetKind::Module) && !f.ctype.contains("javascript") {
                    q.push_back(ToWorker::Net {
                        id: req.id,
                        ev: NetEvent::Failed(NetFailure {
                            kind: NetFailureKind::Network,
                            message: format!("Failed to load script {}: the server answered with the type {}, not JavaScript", req.url, f.ctype),
                        }),
                    });
                } else {
                    q.push_back(ToWorker::Net { id: req.id, ev: NetEvent::Head(h) });
                    q.push_back(ToWorker::Net { id: req.id, ev: NetEvent::Done(f.body.clone()) });
                }
            }
            None => q.push_back(ToWorker::Net {
                id: req.id,
                ev: NetEvent::Failed(NetFailure {
                    kind: NetFailureKind::Network,
                    message: format!("{} {} failed: no such file", req.method, req.url),
                }),
            }),
        }
        ch.cv.notify_all();
    }

    pub fn step(&mut self) {
        self.host.now += 5.0;
        self.doc.run(&mut self.host);
        for o in self.doc.take_workers() {
            match o {
                WorkerOut::Spawn(sp) => {
                    self.spawns.push(sp.clone());
                    if self.workers.len() >= self.max_live {
                        self.doc.worker_event(
                            &mut self.host,
                            sp.id,
                            WorkerEvent::Error { message: String::new(), filename: sp.url.clone(), line: 0, column: 0 },
                        );
                        self.doc.worker_event(&mut self.host, sp.id, WorkerEvent::Closed);
                        continue;
                    }
                    let ch = Arc::new(Chan {
                        to: Mutex::new(VecDeque::new()),
                        from: Mutex::new(VecDeque::new()),
                        cv: Condvar::new(),
                        term: AtomicBool::new(false),
                    });
                    let boot = WorkerBoot {
                        spawn: sp.clone(),
                        limits: self.limits,
                        user_agent: String::from("Mozilla/5.0 (milkOS) Test/1.0"),
                        tz_offset: 0,
                    };
                    let ch2 = ch.clone();
                    let handle = std::thread::Builder::new()
                        .stack_size(self.limits.stack)
                        .spawn(move || {
                            let mut io = Io { ch: ch2, t0: Instant::now() };
                            crate::run_worker(boot, &mut io);
                        })
                        .unwrap();
                    self.workers.insert(sp.id, W { ch, name: sp.name.clone(), handle: Some(handle) });
                }
                WorkerOut::Post { worker, msg } => {
                    if let Some(w) = self.workers.get(&worker) {
                        w.ch.to.lock().unwrap().push_back(ToWorker::Message(msg));
                        w.ch.cv.notify_all();
                    }
                }
                WorkerOut::Port { worker, port, msg } => {
                    if let Some(w) = self.workers.get(&worker) {
                        w.ch.to.lock().unwrap().push_back(ToWorker::Port { port, msg });
                        w.ch.cv.notify_all();
                    }
                }
                WorkerOut::Terminate(id) => {
                    if let Some(w) = self.workers.get(&id) {
                        w.ch.term.store(true, Ordering::SeqCst);
                        w.ch.cv.notify_all();
                    }
                }
            }
        }
        let ids: Vec<u32> = self.workers.keys().copied().collect();
        for id in ids {
            let items: Vec<FromWorker> = {
                let w = &self.workers[&id];
                let mut q = w.ch.from.lock().unwrap();
                q.drain(..).collect()
            };
            for f in items {
                let name = self.workers[&id].name.clone();
                match f {
                    FromWorker::Message(m) => {
                        self.doc.worker_event(&mut self.host, id, WorkerEvent::Message(m));
                    }
                    FromWorker::Net(NetOut::Start(req)) => self.serve(id, req),
                    FromWorker::Net(NetOut::Cancel(_) | NetOut::Ack { .. }) => {}
                    FromWorker::Port { port, msg } => {
                        self.doc.worker_event(&mut self.host, id, WorkerEvent::Port { port, msg });
                    }
                    FromWorker::Console(l) => self.wlines.push(format!("[{name}] {}", l.text)),
                    FromWorker::Error { message, filename, line, column } => {
                        self.doc.worker_event(
                            &mut self.host,
                            id,
                            WorkerEvent::Error { message, filename, line, column },
                        );
                    }
                    FromWorker::Heap(n) => {
                        self.heap.insert(id, n);
                    }
                    FromWorker::Closed(why) => {
                        self.closed.push((id, why));
                        self.doc.worker_event(&mut self.host, id, WorkerEvent::Closed);
                        if let Some(mut w) = self.workers.remove(&id)
                            && let Some(h) = w.handle.take()
                        {
                            h.join().unwrap();
                        }
                        break;
                    }
                }
            }
        }
        self.lines.extend(self.doc.take_console().into_iter().map(|l| l.text));
    }

    /// Step until `pred` holds or `ms` real milliseconds pass.
    pub fn run_until(&mut self, ms: u64, pred: impl Fn(&Sim) -> bool) -> bool {
        let end = Instant::now() + Duration::from_millis(ms);
        loop {
            self.step();
            if pred(self) {
                return true;
            }
            if Instant::now() > end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub fn run_for(&mut self, ms: u64) {
        self.run_until(ms, |_| false);
    }

    /// Run until the page or a worker logs `T done`.
    pub fn finish(&mut self) -> Vec<String> {
        self.run_until(40_000, |s| s.has("done"));
        // A few more steps for what follows it.
        for _ in 0..5 {
            self.step();
            std::thread::sleep(Duration::from_millis(2));
        }
        self.t()
    }
}

impl Drop for Sim {
    fn drop(&mut self) {
        for (_, w) in self.workers.iter_mut() {
            w.ch.term.store(true, Ordering::SeqCst);
            w.ch.cv.notify_all();
        }
        for (_, w) in self.workers.iter_mut() {
            if let Some(h) = w.handle.take() {
                let _ = h.join();
            }
        }
    }
}

fn show(s: &Sim) -> String {
    format!("page: {:?}\nworkers: {:?}\nclosed: {:?}", s.lines, s.wlines, s.closed)
}

#[test]
fn a_classic_worker_computes_and_replies() {
    let mut s = Sim::new(
        "const w = new Worker('w.js'); w.onmessage = (e) => { console.log('T got ' + JSON.stringify(e.data)); console.log('T done'); };
         w.postMessage({n: 20});",
        vec![file(
            "http://www.milk.test/w.js",
            "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
             self.onmessage = (e) => postMessage({fib: fib(e.data.n), self: self === globalThis, win: typeof window, doc: typeof document});",
        )],
    );
    let t = s.finish();
    assert!(t.contains(&String::from("got {\"fib\":6765,\"self\":true,\"win\":\"undefined\",\"doc\":\"undefined\"}")), "{t:?}\n{}", show(&s));
    assert_eq!(s.spawns.len(), 1);
    assert_eq!(s.spawns[0].url, "http://www.milk.test/w.js");
    assert_eq!(s.spawns[0].origin, "http://www.milk.test");
}

#[test]
fn the_workers_global_has_the_platform_and_no_dom() {
    let mut s = Sim::new(
        "const w = new Worker('plat.js', {name: 'plat'}); w.onmessage = (e) => { console.log('T ' + e.data); };
         w.onerror = (e) => console.log('T error ' + e.message);",
        vec![file(
            "http://www.milk.test/plat.js",
            r#"
            const out = [];
            const t = (k, v) => postMessage(k + ' ' + v);
            t('name', self.name);
            t('scope', Object.prototype.toString.call(self) + ' ' + (self instanceof WorkerGlobalScope) + ' ' + (self instanceof EventTarget));
            t('no-dom', [typeof window, typeof document, typeof Node, typeof Element, typeof localStorage, typeof alert, typeof HTMLElement, typeof XMLSerializer, typeof MutationObserver, typeof requestAnimationFrame, typeof matchMedia].join());
            t('loc', location.href + ' ' + location.origin + ' ' + location.pathname + ' ' + String(location));
            t('origin', self.origin + ' ' + isSecureContext);
            t('nav', typeof navigator.userAgent + ' ' + navigator.language + ' ' + typeof navigator.hardwareConcurrency + ' ' + typeof navigator.cookieEnabled + ' ' + typeof navigator.sendBeacon);
            t('url', new URL('../x?a=1#h', location.href).href + ' ' + new URLSearchParams('a=1&b=2').get('b'));
            t('text', new TextDecoder().decode(new TextEncoder().encode('héllo')) + ' ' + btoa('abc') + ' ' + atob('YWJj'));
            t('clone', JSON.stringify(structuredClone({a: [1, new Map([[1, 2]])], d: new Date(5)}, undefined)) );
            t('crypto', (crypto.getRandomValues(new Uint8Array(8)).length) + ' ' + crypto.randomUUID().length);
            t('perf', (typeof performance.now()) + ' ' + (performance.now() >= 0) + ' ' + typeof performance.mark);
            t('intl', new Intl.NumberFormat('en-US').format(1234567.891) + ' ' + (1234.5).toLocaleString('en-US'));
            t('blob', new Blob(['ab', 'cd'], {type: 'text/plain'}).size);
            t('abort', (() => { const c = new AbortController(); c.abort(); return c.signal.aborted; })());
            t('proxy', new Proxy({}, { get: (_, k) => 'p' + String(k) }).foo);
            t('events', typeof Event + ' ' + typeof CustomEvent + ' ' + typeof ErrorEvent + ' ' + typeof MessageEvent + ' ' + typeof MessageChannel + ' ' + typeof BroadcastChannel + ' ' + typeof Headers + ' ' + typeof Request + ' ' + typeof Response + ' ' + typeof FormData + ' ' + typeof XMLHttpRequest + ' ' + typeof fetch + ' ' + typeof importScripts);
            queueMicrotask(() => t('micro', 'ran'));
            Promise.resolve().then(() => t('promise', 'ran'));
            setTimeout(() => t('timeout', 'ran'), 5);
            let n = 0; const iv = setInterval(() => { n++; if (n === 3) { clearInterval(iv); t('interval', n); } }, 2);
            const mc = new MessageChannel();
            mc.port1.onmessage = (e) => t('mc', e.data.x);
            mc.port2.postMessage({x: 'ok'});
            const bc1 = new BroadcastChannel('c'), bc2 = new BroadcastChannel('c');
            bc2.onmessage = (e) => t('bc', e.data);
            bc1.postMessage('hi');
            self.addEventListener('message', (e) => t('echo', e.data));
            t('done', 1);
            "#,
        )],
    );
    s.run_until(40_000, |s| s.has("bc hi") && s.has("interval 3") && s.has("mc ok") && s.has("timeout ran") && s.has("done 1"));
    let t: Vec<String> = s.t();
    let want = [
        "name plat",
        "scope [object DedicatedWorkerGlobalScope] true true",
        "no-dom undefined,undefined,undefined,undefined,undefined,undefined,undefined,undefined,undefined,undefined,undefined",
        "loc http://www.milk.test/plat.js http://www.milk.test /plat.js http://www.milk.test/plat.js",
        "origin http://www.milk.test false",
        "nav string en-US number undefined undefined",
        "url http://www.milk.test/x?a=1#h 2",
        "text héllo YWJj abc",
        "clone {\"a\":[1,{}],\"d\":\"1970-01-01T00:00:00.005Z\"}",
        "crypto 8 36",
        "perf number true function",
        "intl 1,234,567.891 1,234.5",
        "blob 4",
        "abort true",
        "proxy pfoo",
        "events function function function function function function function function function function function function function",
        "micro ran",
        "promise ran",
        "timeout ran",
        "interval 3",
        "mc ok",
        "bc hi",
    ];
    for w in want {
        assert!(t.iter().any(|l| l.ends_with(w) || l == w), "missing {w:?} in {t:#?}\n{}", show(&s));
    }
    assert!(s.wlines.iter().all(|l| l.starts_with("[plat] ")));
}

fn has_line(t: &[String], want: &str) -> bool {
    t.iter().any(|l| l == want)
}

#[test]
fn a_module_worker_imports_statically_and_dynamically() {
    let mut s = Sim::new(
        "const w = new Worker('m.mjs', {type: 'module', name: 'mod'});
         w.onmessage = (e) => { console.log('T ' + e.data); if (e.data === 'dyn 42') console.log('T done'); };
         w.onerror = (e) => console.log('T error ' + e.message);",
        vec![
            file("http://www.milk.test/m.mjs", "import { double, name } from './lib/util.mjs'; import def from './lib/def.mjs';
                postMessage('static ' + double(21) + ' ' + name + ' ' + def + ' ' + import.meta.url + ' ' + (typeof importScripts) + ' ' + (typeof window));
                const m = await import('./lib/dyn.mjs');
                postMessage('dyn ' + m.answer);"),
            file("http://www.milk.test/lib/util.mjs", "export const double = (x) => x * 2; export const name = 'util';"),
            file("http://www.milk.test/lib/def.mjs", "export default 'D';"),
            file("http://www.milk.test/lib/dyn.mjs", "export const answer = 42;"),
        ],
    );
    let t = s.finish();
    assert!(has_line(&t, "static 42 util D http://www.milk.test/m.mjs undefined undefined"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "dyn 42"), "{t:?}\n{}", show(&s));
    // The module requests were module-kind, same origin.
    assert!(s.requests.iter().any(|r| r.url.ends_with("util.mjs") && r.kind == NetKind::Module));
    assert!(s.requests.iter().all(|r| r.credentials == CredentialsMode::SameOrigin));
}

#[test]
fn import_scripts_runs_scripts_in_order_and_fails_with_a_network_error() {
    let mut s = Sim::new(
        "const w = new Worker('is.js'); w.onmessage = (e) => { console.log('T ' + e.data); if (e.data.startsWith('end')) console.log('T done'); };",
        vec![
            file("http://www.milk.test/is.js", "importScripts('a.js', 'b.js');
                postMessage('after ' + typeof fa + ' ' + typeof fb + ' ' + order.join());
                try { importScripts('missing.js'); } catch (e) { postMessage('caught ' + e.name); }
                try { importScripts('bad.js'); } catch (e) { postMessage('syntax ' + e.name); }
                try { importScripts('http://other.test/x.js'); } catch (e) { postMessage('cross ' + e.name); }
                postMessage('end');"),
            file("http://www.milk.test/a.js", "var order = ['a']; function fa() {}"),
            file("http://www.milk.test/b.js", "order.push('b'); function fb() {}"),
            file("http://www.milk.test/bad.js", "var = ;"),
        ],
    );
    let t = s.finish();
    assert!(has_line(&t, "after function function a,b"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "caught NetworkError"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "syntax SyntaxError"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "cross NetworkError"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "end"), "{t:?}");
    // Each script was one request of kind Script, in the order asked.
    let urls: Vec<&str> = s.requests.iter().filter(|r| r.kind == NetKind::Script).map(|r| r.url.as_str()).collect();
    assert_eq!(&urls[..3], ["http://www.milk.test/is.js", "http://www.milk.test/a.js", "http://www.milk.test/b.js"]);
}

#[test]
fn a_blob_url_worker_runs_the_script_the_page_made() {
    let mut s = Sim::new(
        "const blob = new Blob(['onmessage = (e) => postMessage(\"blob \" + e.data + \" \" + location.protocol + \" \" + self.origin);'], {type: 'text/javascript'});
         const url = URL.createObjectURL(blob);
         const w = new Worker(url);
         w.onmessage = (e) => { console.log('T ' + e.data); URL.revokeObjectURL(url); const w2 = new Worker(url); w2.onerror = () => console.log('T revoked fails'); console.log('T done'); };
         w.postMessage('hi');",
        vec![],
    );
    let t = s.finish();
    assert!(has_line(&t, "blob hi blob: http://www.milk.test"), "{t:?}\n{}", show(&s));
    s.run_until(30_000, |s| s.has("revoked fails"));
    assert!(has_line(&s.t(), "revoked fails"), "{:?}\n{}", s.t(), show(&s));
    // The page had the script; the worker fetched nothing.
    assert!(s.requests.is_empty(), "{:?}", s.requests);
    assert!(s.spawns[0].source.is_some());
    assert!(s.lines.iter().any(|l| l.contains("revoked")), "{:?}", s.lines);
}

#[test]
fn a_data_url_worker_has_an_origin_of_its_own() {
    let mut s = Sim::new(
        "const w = new Worker('data:text/javascript,postMessage(self.origin%2B%22 %22%2Blocation.protocol)'); w.onmessage = (e) => { console.log('T ' + e.data); console.log('T done'); };",
        vec![file("data:text/javascript,postMessage(self.origin%2B%22 %22%2Blocation.protocol)", "postMessage(self.origin+' '+location.protocol)")],
    );
    let t = s.finish();
    assert!(has_line(&t, "null data:"), "{t:?}\n{}", show(&s));
    assert_eq!(s.spawns[0].origin, "null");
}

#[test]
fn a_worker_from_another_origin_or_a_bad_address_is_refused_with_an_error_event() {
    let mut s = Sim::new(
        "let n = 0;
         for (const u of ['http://other.test/w.js', 'https://www.milk.test/w.js', 'ftp://www.milk.test/w.js', 'file:///x.js', 'blob:http://www.milk.test/none', 'javascript:1']) {
           const w = new Worker(u);
           w.onerror = (e) => { console.log('T refused ' + u + ' ' + (e instanceof ErrorEvent) + ' ' + e.message); if (++n === 6) console.log('T done'); };
         }
         try { new Worker('http://[bad'); } catch (e) { console.log('T syntax ' + e.name); }
         try { new Worker(); } catch (e) { console.log('T args ' + e.name); }
         try { new Worker('w.js', {type: 'nope'}); } catch (e) { console.log('T type ' + e.name); }",
        vec![],
    );
    let t = s.finish();
    assert_eq!(t.iter().filter(|l| l.starts_with("refused ")).count(), 6, "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "syntax SyntaxError"), "{t:?}");
    assert!(has_line(&t, "args TypeError"), "{t:?}");
    assert!(has_line(&t, "type TypeError"), "{t:?}");
    assert!(s.spawns.is_empty(), "nothing was started: {:?}", s.spawns);
    assert!(s.lines.iter().any(|l| l.contains("cannot be accessed from origin 'http://www.milk.test'")), "{:?}", s.lines);
}

#[test]
fn a_missing_or_wrongly_typed_script_is_an_error_event_and_the_worker_ends() {
    let mut s = Sim::new(
        "let n = 0;
         for (const u of ['nope.js', 'text.js']) {
           const w = new Worker(u);
           w.onerror = (e) => { console.log('T failed ' + u + ' [' + e.message + '] ' + e.lineno); if (++n === 2) console.log('T done'); };
         }",
        vec![File { url: String::from("http://www.milk.test/text.js"), ctype: String::from("text/html"), body: b"<p>no</p>".to_vec(), status: 200 }],
    );
    let t = s.finish();
    assert!(has_line(&t, "failed nope.js [] 0"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "failed text.js [] 0"), "{t:?}\n{}", show(&s));
    s.run_until(30_000, |s| s.closed.len() == 2);
    assert_eq!(s.closed.len(), 2, "{:?}", s.closed);
    assert!(s.closed.iter().all(|(_, w)| *w == CloseWhy::Failed));
}

#[test]
fn errors_in_a_worker_reach_onerror_with_where_they_happened() {
    let mut s = Sim::new(
        "const w = new Worker('err.js');
         w.onerror = (e) => { e.preventDefault(); console.log('T error ' + e.message + ' | ' + e.filename + ' | ' + e.lineno + ':' + e.colno); };
         w.onmessage = (e) => { console.log('T msg ' + e.data); if (e.data === 'second') console.log('T done'); };
         w.postMessage('go');",
        vec![file("http://www.milk.test/err.js", "onmessage = () => {\n  postMessage('first');\n  null.boom;\n};\nsetTimeout(() => { throw new Error('later'); }, 1);\nsetTimeout(() => postMessage('second'), 30);")],
    );
    let t = s.finish();
    assert!(t.iter().any(|l| l.starts_with("error Uncaught TypeError") && l.contains("| http://www.milk.test/err.js | 3:")), "{t:?}\n{}", show(&s));
    assert!(t.iter().any(|l| l.starts_with("error Uncaught Error: later") && l.contains("| http://www.milk.test/err.js | 5:")), "{t:?}\n{}", show(&s));
    // The worker goes on after an error.
    assert!(has_line(&t, "msg first"), "{t:?}");
    assert!(has_line(&t, "msg second"), "{t:?}");
}

#[test]
fn an_error_nobody_handles_goes_to_the_pages_console_and_self_onerror_can_take_it() {
    let mut s = Sim::new(
        "const a = new Worker('e1.js'); const b = new Worker('e2.js', {name: 'b'}); b.onmessage = (e) => console.log('T done');",
        vec![
            file("http://www.milk.test/e1.js", "throw new RangeError('plain');"),
            file("http://www.milk.test/e2.js", "self.onerror = (m, f, l, c, e) => { postMessage('handled ' + m + ' ' + l + ' ' + (e instanceof Error)); return true; }; setTimeout(() => { null.x; }, 1);"),
        ],
    );
    s.run_until(40_000, |s| s.lines.iter().any(|l| l.starts_with("Uncaught RangeError: plain")) && s.has("done"));
    s.run_for(200);
    assert!(s.lines.iter().any(|l| l.starts_with("Uncaught RangeError: plain (http://www.milk.test/e1.js:1:")), "{:?}", s.lines);
    // (e2's error was handled in the worker: the page heard nothing of it.)
    assert!(!s.lines.iter().any(|l| l.contains("null")), "{:?}", s.lines);
}

#[test]
fn array_buffers_in_the_transfer_list_move_and_the_senders_are_detached() {
    let mut s = Sim::new(
        "const w = new Worker('xf.js');
         const buf = new ArrayBuffer(16); const u8 = new Uint8Array(buf); for (let i = 0; i < 16; i++) u8[i] = i;
         const copy = new Uint8Array(8); copy[0] = 7;
         w.postMessage({buf, view: new Uint32Array(buf, 4, 2), copy}, [buf]);
         console.log('T sender ' + buf.byteLength + ' ' + u8.length + ' ' + u8.byteLength);
         w.onmessage = (e) => { console.log('T back ' + e.data.byteLength + ' ' + new Uint8Array(e.data)[5]); console.log('T done'); };
         try { w.postMessage(1, [buf]); } catch (e) { console.log('T again ' + e.name); }
         try { w.postMessage(1, [new ArrayBuffer(1), 5]); } catch (e) { console.log('T bad ' + e.name); }
         const b2 = new ArrayBuffer(4); try { w.postMessage(1, [b2, b2]); } catch (e) { console.log('T dup ' + e.name); }",
        vec![file("http://www.milk.test/xf.js", "onmessage = (e) => {
            const d = e.data;
            console.log('T worker ' + d.buf.byteLength + ' ' + d.view.length + ' ' + d.view.byteOffset + ' ' + (d.view.buffer === d.buf) + ' ' + d.copy[0] + ' ' + new Uint8Array(d.buf)[15]);
            const out = new ArrayBuffer(8); new Uint8Array(out)[5] = 99;
            self.postMessage(out, [out]);
            console.log('T after ' + out.byteLength);
        };")],
    );
    let t = s.finish();
    assert!(has_line(&t, "sender 0 0 0"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "worker 16 2 4 true 7 15"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "back 8 99"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "after 0"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "again DataCloneError"), "{t:?}");
    assert!(has_line(&t, "bad DataCloneError"), "{t:?}");
    assert!(has_line(&t, "dup DataCloneError"), "{t:?}");
}

#[test]
fn terminate_ends_a_worker_and_what_it_still_sends_is_dropped() {
    let mut s = Sim::new(
        "const w = new Worker('t.js'); let got = 0;
         w.onmessage = (e) => { got++; if (e.data === 'ready') { w.terminate(); w.postMessage('after'); console.log('T terminated'); } else console.log('T late'); };",
        vec![file("http://www.milk.test/t.js", "postMessage('ready'); setInterval(() => postMessage('tick'), 1); onmessage = () => postMessage('echo');")],
    );
    s.run_until(30_000, |s| !s.closed.is_empty());
    s.run_for(100);
    assert_eq!(s.closed.len(), 1, "{:?}\n{}", s.closed, show(&s));
    assert_eq!(s.closed[0].1, CloseWhy::Terminated);
    let t = s.t();
    assert!(has_line(&t, "terminated"));
    assert!(!has_line(&t, "late"), "events after terminate: {t:?}");
}

#[test]
fn a_worker_can_close_itself() {
    let mut s = Sim::new(
        "const w = new Worker('c.js'); w.onmessage = (e) => { console.log('T ' + e.data); }; w.postMessage(1); w.postMessage(2);",
        vec![file("http://www.milk.test/c.js", "let n = 0; onmessage = (e) => { n++; postMessage('m' + e.data); close(); postMessage('after close'); setTimeout(() => postMessage('never'), 1); };")],
    );
    s.run_until(30_000, |s| !s.closed.is_empty());
    s.run_for(100);
    assert_eq!(s.closed.len(), 1, "{:?}", s.closed);
    assert_eq!(s.closed[0].1, CloseWhy::Closed);
    let t = s.t();
    assert!(has_line(&t, "m1"), "{t:?}");
    assert!(has_line(&t, "after close"), "{t:?}");
    assert!(!has_line(&t, "m2") && !has_line(&t, "never"), "{t:?}");
}

#[test]
fn a_runaway_worker_is_stopped_alone() {
    let mut s = Sim::new(
        "const good = new Worker('good.js'); const bad = new Worker('bad.js', {name: 'bad'});
         good.onmessage = (e) => { console.log('T good ' + e.data); };
         good.postMessage(1); bad.postMessage(1);",
        vec![
            file("http://www.milk.test/good.js", "onmessage = (e) => postMessage('alive');"),
            file("http://www.milk.test/bad.js", "onmessage = () => { while (true) {} };"),
        ],
    );
    s.limits.task_ms = 1500;
    s.run_until(40_000, |s| !s.closed.is_empty());
    assert_eq!(s.closed.len(), 1, "{:?}\n{}", s.closed, show(&s));
    match &s.closed[0].1 {
        CloseWhy::Stopped(why) => assert!(why.contains("ran for more than"), "{why}"),
        w => panic!("{w:?}"),
    }
    // The other worker and the page carried on.
    s.run_until(30_000, |s| s.has("good alive"));
    assert!(has_line(&s.t(), "good alive"), "{:?}\n{}", s.t(), show(&s));
}

#[test]
fn a_worker_that_eats_memory_is_stopped_alone() {
    let mut s = Sim::new(
        "const w = new Worker('mem.js'); w.postMessage(1);",
        vec![file("http://www.milk.test/mem.js", "onmessage = () => { const a = []; for (;;) a.push(new Array(10000).fill('x'.repeat(100))); };")],
    );
    s.limits.heap = 8 << 20;
    s.run_until(60_000, |s| !s.closed.is_empty());
    assert!(matches!(&s.closed[0].1, CloseWhy::Stopped(w) if w.contains("memory")), "{:?}", s.closed);
}

#[test]
fn the_page_can_have_sixteen_workers_and_the_next_is_refused() {
    let mut s = Sim::new(
        "const ws = []; for (let i = 0; i < 17; i++) { const w = new Worker('n.js', {name: 'w' + i}); w.onerror = () => console.log('T refused ' + i); ws.push(w); }
         ws.forEach((w, i) => { w.onmessage = (e) => console.log('T up ' + e.data); });",
        vec![file("http://www.milk.test/n.js", "postMessage(name);")],
    );
    s.run_until(40_000, |s| s.t().iter().filter(|l| l.starts_with("up ")).count() == 16 && s.has("refused 16"));
    let t = s.t();
    assert!(has_line(&t, "refused 16"), "{t:?}\n{}", show(&s));
    assert_eq!(t.iter().filter(|l| l.starts_with("up ")).count(), 16, "{t:?}");
    assert_eq!(s.spawns.len(), 16);
}

#[test]
fn messages_sent_before_the_script_has_run_are_not_lost() {
    let mut s = Sim::new(
        "const w = new Worker('late.js'); w.onmessage = (e) => { console.log('T ' + e.data); if (e.data === 'c') console.log('T done'); };
         w.postMessage('a'); w.postMessage('b'); w.postMessage('c');",
        vec![file("http://www.milk.test/late.js", "importScripts('dep.js'); onmessage = (e) => postMessage(e.data);"), file("http://www.milk.test/dep.js", "var x = 1;")],
    );
    let t = s.finish();
    let order: Vec<&String> = t.iter().filter(|l| ["a", "b", "c"].contains(&l.as_str())).collect();
    assert_eq!(order, ["a", "b", "c"], "{t:?}\n{}", show(&s));
}

#[test]
fn fetch_and_xhr_in_a_worker_use_the_workers_own_origin() {
    let mut s = Sim::new(
        "const w = new Worker('net.js'); w.onmessage = (e) => { console.log('T ' + e.data); if (e.data.startsWith('xhr')) console.log('T done'); };",
        vec![
            file("http://www.milk.test/net.js", "fetch('data.json').then(r => r.json()).then(j => postMessage('fetch ' + j.a));
                fetch('http://api.test/x', {mode: 'cors'}).catch(e => postMessage('cors ' + e.name));
                const x = new XMLHttpRequest(); x.open('GET', '/x.txt'); x.onload = () => postMessage('xhr ' + x.responseText + ' ' + x.status); x.send();
                try { const y = new XMLHttpRequest(); y.open('GET', '/x', false); postMessage('sync ok'); } catch (e) { postMessage('sync ' + e.name); }"),
            File { url: String::from("http://www.milk.test/data.json"), ctype: String::from("application/json"), body: b"{\"a\":5}".to_vec(), status: 200 },
            File { url: String::from("http://www.milk.test/x.txt"), ctype: String::from("text/plain"), body: b"hello".to_vec(), status: 200 },
        ],
    );
    let t = s.finish();
    assert!(has_line(&t, "fetch 5"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "xhr hello 200"), "{t:?}\n{}", show(&s));
    // The requests are the page's kind, from the worker's address, same-origin credentials.
    let r = s.requests.iter().find(|r| r.url == "http://www.milk.test/data.json").unwrap();
    assert_eq!(r.kind, NetKind::Fetch);
    assert_eq!(r.credentials, CredentialsMode::SameOrigin);
    assert!(s.requests.iter().any(|r| r.url == "http://api.test/x"), "{:?}", s.requests);
}

#[test]
fn message_ports_given_to_a_worker_keep_working_both_ways() {
    let mut s = Sim::new(
        "const w = new Worker('p.js'); const mc = new MessageChannel();
         mc.port1.onmessage = (e) => { console.log('T page got ' + JSON.stringify(e.data)); if (e.data.n < 3) mc.port1.postMessage({n: e.data.n + 1}); else console.log('T done'); };
         w.postMessage({hello: 'x', port: mc.port2}, [mc.port2]);
         mc.port1.postMessage({n: 0});
         try { mc.port2.postMessage(1); console.log('T posted on a sent port'); } catch (e) { console.log('T sent ' + e.name); }
         try { w.postMessage(1, [mc.port2]); } catch (e) { console.log('T twice ' + e.name); }",
        vec![file("http://www.milk.test/p.js", "onmessage = (e) => {
            const p = e.data.port; console.log('T worker ' + e.data.hello + ' ' + e.ports.length + ' ' + (e.ports[0] === p));
            p.onmessage = (m) => p.postMessage({n: m.data.n + 1});
        };")],
    );
    let t = s.finish();
    assert!(has_line(&t, "worker x 1 true"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "page got {\"n\":1}"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "page got {\"n\":3}"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "twice DataCloneError"), "{t:?}");
}

#[test]
fn ports_of_the_page_and_of_the_worker_do_not_share_numbers() {
    // The page's first channel goes over; the worker then makes one of its own and sends it back.
    let mut s = Sim::new(
        "const w = new Worker('n.js'); const mc = new MessageChannel();
         mc.port1.onmessage = (e) => console.log('T from page port ' + e.data);
         w.postMessage('first', [mc.port2]);
         w.onmessage = (e) => { const p = e.ports[0]; p.onmessage = (m) => { console.log('T from worker port ' + m.data); console.log('T done'); }; p.postMessage('to the worker port'); };",
        vec![file("http://www.milk.test/n.js", "onmessage = (e) => { e.ports[0].postMessage('hello from the first'); const mc = new MessageChannel(); mc.port1.onmessage = (m) => mc.port1.postMessage('echo ' + m.data); postMessage('second', [mc.port2]); };")],
    );
    let t = s.finish();
    assert!(has_line(&t, "from page port hello from the first"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "from worker port echo to the worker port"), "{t:?}\n{}", show(&s));
}

#[test]
fn a_port_made_in_a_worker_can_be_sent_to_the_page() {
    let mut s = Sim::new(
        "const w = new Worker('q.js'); w.onmessage = (e) => { const p = e.ports[0]; console.log('T ports ' + e.ports.length + ' ' + e.data); p.onmessage = (m) => { console.log('T page ' + m.data); if (m.data === 'pong 2') console.log('T done'); }; p.postMessage('ping 1'); p.postMessage('ping 2'); };",
        vec![file("http://www.milk.test/q.js", "const mc = new MessageChannel();
            mc.port1.onmessage = (e) => mc.port1.postMessage('pong ' + e.data.split(' ')[1]);
            postMessage('here', [mc.port2]);")],
    );
    let t = s.finish();
    assert!(has_line(&t, "ports 1 here"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "page pong 1"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "page pong 2"), "{t:?}\n{}", show(&s));
}

#[test]
fn a_call_and_answer_pattern_over_ports_like_rpc_libraries_use() {
    // One channel per call, the reply port sent with the request.
    let mut s = Sim::new(
        "const w = new Worker('rpc.js');
         const call = (name, ...args) => new Promise((resolve) => { const { port1, port2 } = new MessageChannel(); port1.onmessage = (e) => { port1.close(); resolve(e.data); }; w.postMessage({name, args}, [port2]); });
         (async () => {
           const r1 = await call('add', 2, 3); const r2 = await call('add', r1, 10); const r3 = await call('shout', 'hi');
           console.log('T results ' + [r1, r2, r3].join());
           console.log('T done');
         })();",
        vec![file("http://www.milk.test/rpc.js", "const api = { add: (a, b) => a + b, shout: (s) => s.toUpperCase() + '!' };
            onmessage = (e) => { const [port] = e.ports; port.postMessage(api[e.data.name](...e.data.args)); port.close(); };")],
    );
    let t = s.finish();
    assert!(has_line(&t, "results 5,15,HI!"), "{t:?}\n{}", show(&s));
}

// ---- hostile input -------------------------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[(self.next() % v.len() as u64) as usize]
    }
}

fn boot(url: &str, kind: WorkerType) -> WorkerBoot {
    WorkerBoot {
        spawn: WorkerSpawn {
            id: 1,
            url: String::from(url),
            kind,
            name: String::new(),
            origin: String::from("http://www.milk.test"),
            source: Some(b"onmessage = (e) => { try { postMessage(e.data, e.ports); } catch (x) {} };".to_vec()),
            secure: false,
        },
        limits: WorkerLimits { stack: 4 << 20, ..WorkerLimits::default() },
        user_agent: String::from("test"),
        tz_offset: 0,
    }
}

#[test]
fn a_worker_global_survives_garbage_from_the_page() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let datas = [
        "", "null", "{", "[1,2", "{\"$t\":\"p\",\"v\":99}", "{\"$t\":\"ref\",\"v\":5}", "{\"$t\":\"w\",\"v\":[\"Uint8Array\",0,0,1]}",
        "{\"$t\":\"t\",\"v\":[\"Nope\",\"zz\"]}", "{\"$t\":\"b\",\"v\":\"0\"}", "[{\"$t\":\"m\",\"v\":[1]}]", "\"str\"", "{\"$t\":\"r\",\"v\":[\"(\",\"g\"]}",
        "{\"$t\":\"d\",\"v\":\"x\"}", "{\"a\":{\"b\":{\"c\":[1,2,{\"$t\":\"u\"}]}}}", "[[[[[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]]]]]",
    ];
    let b = boot("http://www.milk.test/g.js", WorkerType::Classic);
    let mut d = Document::new_worker(&b);
    let mut h = Host::new();
    for _ in 0..50 {
        d.run(&mut h);
        h.now += 5.0;
    }
    for i in 0..400u32 {
        let data = rng.pick(&datas).to_string();
        let mut xfer = Vec::new();
        for _ in 0..(rng.next() % 4) {
            xfer.push(if rng.next() % 2 == 0 { Xfer::Port(rng.next() % 6) } else { Xfer::Buffer(vec![1u8; (rng.next() % 40) as usize]) });
        }
        match rng.next() % 4 {
            0 => {
                d.worker_message(&mut h, WireMsg { data, xfer });
            }
            1 => d.worker_port(rng.next() % 6, Some(WireMsg { data, xfer })),
            2 => d.worker_port(rng.next() % 6, None),
            _ => {
                d.net_event(&mut h, i, NetEvent::Done(vec![1, 2, 3]));
            }
        }
        d.run(&mut h);
        let _ = d.take_worker_out();
        let _ = d.take_console();
        let _ = d.take_net();
        h.now += 5.0;
    }
    assert!(!d.script_stats().stopped);
}

#[test]
fn a_page_survives_garbage_from_its_workers() {
    let mut rng = Rng(0xfeed_beef_0123_4567);
    let mut d = doc("<script>var w = new Worker('w.js'); w.onmessage = (e) => { globalThis.n = (globalThis.n || 0) + 1; }; w.onerror = () => {}; w.onmessageerror = () => {};
        var ch = new MessageChannel(); w.postMessage(1, [ch.port2]);</script>");
    let mut h = Host::new();
    d.run(&mut h);
    let spawn = d.take_workers().into_iter().find_map(|o| if let WorkerOut::Spawn(s) = o { Some(s) } else { None }).unwrap();
    let datas = ["", "null", "{", "{\"$t\":\"p\",\"v\":99}", "\"x\"", "[1,{\"$t\":\"ref\",\"v\":3}]", "{\"$t\":\"w\",\"v\":[\"X\",7,0,0]}"];
    for _ in 0..400 {
        let data = rng.pick(&datas).to_string();
        let mut xfer = Vec::new();
        for _ in 0..(rng.next() % 3) {
            xfer.push(if rng.next() % 2 == 0 { Xfer::Port(rng.next() % 5) } else { Xfer::Buffer(vec![9u8; (rng.next() % 20) as usize]) });
        }
        let ev = match rng.next() % 5 {
            0 => WorkerEvent::Message(WireMsg { data, xfer }),
            1 => WorkerEvent::Error { message: "m".repeat((rng.next() % 50) as usize), filename: String::from("f"), line: rng.next() as u32, column: rng.next() as u32 },
            2 => WorkerEvent::Port { port: rng.next() % 5, msg: Some(WireMsg { data, xfer }) },
            3 => WorkerEvent::Port { port: rng.next() % 5, msg: None },
            _ => continue,
        };
        d.worker_event(&mut h, spawn.id, ev);
        d.run(&mut h);
        let _ = d.take_workers();
        h.now += 5.0;
    }
    // Events for workers that were never made, or already ended.
    d.worker_event(&mut h, 999, WorkerEvent::Message(WireMsg::default()));
    d.worker_event(&mut h, spawn.id, WorkerEvent::Closed);
    d.worker_event(&mut h, spawn.id, WorkerEvent::Message(WireMsg::default()));
    d.worker_event(&mut h, spawn.id, WorkerEvent::Closed);
    assert!(!d.script_stats().stopped);
}

#[test]
fn hostile_calls_from_script_do_not_panic() {
    let mut s = Sim::new(
        "const calls = [
           () => new Worker(null), () => new Worker({toString() { throw 1; }}), () => new Worker('w.js', 5), () => new Worker('w.js', {name: {toString() { throw new Error('n'); }}}),
           () => new Worker('w.js', {type: 'module', name: 'x'.repeat(100000)}), () => Worker.prototype.postMessage.call({}, 1), () => Worker.prototype.terminate.call(5),
           () => { const w = new Worker('w.js'); w.postMessage(); }, () => { const w = new Worker('w.js'); w.postMessage(() => {}); },
           () => { const w = new Worker('w.js'); w.postMessage(1, 5); }, () => { const w = new Worker('w.js'); w.postMessage(1, {transfer: 5}); },
           () => { const w = new Worker('w.js'); w.postMessage(1, [{}]); }, () => { const w = new Worker('w.js'); w.postMessage(Symbol()); },
           () => { const w = new Worker('w.js'); const a = {}; a.a = a; w.postMessage(a); }, () => { const w = new Worker('w.js'); w.postMessage('x'.repeat(5 << 20)); },
           () => { const w = new Worker('w.js'); w.terminate(); w.terminate(); w.postMessage(1); },
           () => { const mc = new MessageChannel(); mc.port1.close(); new Worker('w.js').postMessage(1, [mc.port2, mc.port1]); },
           () => new Worker('w.js', {type: 'classic'}).dispatchEvent(new Event('x')),
           () => { const w = new Worker('w.js'); w.onmessage = 5; w.onerror = null; w.addEventListener('error', {handleEvent() { throw 1; }}); },
         ];
         let n = 0; for (const c of calls) { try { c(); } catch (e) { n++; } } console.log('T threw ' + (n > 5)); console.log('T done');",
        vec![file("http://www.milk.test/w.js", "onmessage = () => {}; postMessage(1);")],
    );
    s.max_live = 100;
    let t = s.finish();
    assert!(has_line(&t, "threw true"), "{t:?}\n{}", show(&s));
    assert!(s.spawns.len() <= 16 + 1, "{}", s.spawns.len());
}

#[test]
fn a_worker_on_a_small_stack_gets_errors_for_what_nests_too_deep_not_a_crash() {
    let src = "// Things that eat native stack: a worker has a thread of its own with a smaller stack than the page's,\n// so the engine's limits must hold (an error in script, never a crash).\nfunction report(group, name, value) { console.log('T ' + group + ' ' + name + ' ' + value); }\nfunction rec(n) { return rec(n + 1) + 1; }\ntry { rec(0); report('deep', 'recursion', 'no error?'); } catch (e) { report('deep', 'recursion', e.name); }\nvar nested = '[' .repeat(4000) + ']'.repeat(4000);\ntry { JSON.parse(nested); report('deep', 'json', 'parsed'); } catch (e) { report('deep', 'json', e.name); }\ntry { eval(nested); report('deep', 'eval', 'ran'); } catch (e) { report('deep', 'eval', e.name); }\ntry { new RegExp('('.repeat(3000) + 'a' + ')'.repeat(3000)).test('a'); report('deep', 'regexp', 'ran'); } catch (e) { report('deep', 'regexp', e.name); }\nvar o = {}; var cur = o; for (var i = 0; i < 20000; i++) { cur.n = {}; cur = cur.n; }\ntry { structuredClone(o); report('deep', 'clone', 'cloned'); } catch (e) { report('deep', 'clone', e.name); }\ntry { JSON.stringify(o); report('deep', 'stringify', 'done'); } catch (e) { report('deep', 'stringify', e.name); }\ntry { postMessage(o); report('deep', 'post', 'sent'); } catch (e) { report('deep', 'post', e.name); }\nvar f = function () {}; for (var j = 0; j < 3000; j++) { f = (function (g) { return function () { return g() + 1; }; })(f); }\ntry { f(); report('deep', 'closures', 'ran'); } catch (e) { report('deep', 'closures', e.name); }\nreport('deep', 'alive', 'yes');\npostMessage('end');\n";
    let mut s = Sim::new(
        "const w = new Worker('deep.js', {name: 'deep'}); w.onmessage = (e) => console.log('T done');",
        vec![file("http://www.milk.test/deep.js", src)],
    );
    s.limits.stack = 2 << 20;
    let t = s.finish();
    assert!(has_line(&t, "deep alive yes"), "{t:?}\n{}", show(&s));
    assert!(has_line(&t, "deep recursion RangeError"), "{t:?}");
    assert!(t.iter().any(|l| l == "deep post DataCloneError" || l == "deep post RangeError"), "{t:?}");
    assert!(s.closed.is_empty(), "the worker is still running: {:?}", s.closed);
}
