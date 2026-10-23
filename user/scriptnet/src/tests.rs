//! scriptnet against a fake web: a handler plays every server, and the requests it saw are
//! kept for the assertions.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use browserkit::cookies::{Context, Jar};
use browserkit::netpolicy::{Config, Page, PreflightCache};
use milkweb::http::{Error, Net, Transport};
use milkweb::url::Url;

use super::*;

/// A request as a fake server saw it.
#[derive(Clone, Debug)]
pub struct Seen {
    pub host: String,
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

type Handler = dyn FnMut(&Seen) -> Vec<u8>;

pub struct World {
    pub handler: Rc<RefCell<Box<Handler>>>,
    pub seen: Rc<RefCell<Vec<Seen>>>,
    pub clock: Rc<RefCell<u64>>,
    /// Hosts that exist, and what they resolve to (a public address unless said).
    pub hosts: Vec<String>,
    pub ips: Vec<(String, Vec<u8>)>,
    /// The host last resolved (the fake connection is to it).
    last: String,
}

struct Conn {
    host: String,
    sent: Vec<u8>,
    out: VecDeque<Vec<u8>>,
    world: (Rc<RefCell<Box<Handler>>>, Rc<RefCell<Vec<Seen>>>),
}

impl Transport for Conn {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        match self.out.front_mut() {
            Some(p) => {
                let n = p.len().min(buf.len());
                buf[..n].copy_from_slice(&p[..n]);
                p.drain(..n);
                if p.is_empty() {
                    self.out.pop_front();
                }
                Ok(n)
            }
            None => Ok(0),
        }
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        self.sent.extend_from_slice(buf);
        let text = String::from_utf8_lossy(&self.sent).into_owned();
        if let Some(end) = text.find("\r\n\r\n") {
            let mut lines = text[..end].lines();
            let first = lines.next().unwrap_or("");
            let mut parts = first.split(' ');
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();
            let headers: Vec<(String, String)> = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect();
            let len = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.parse::<usize>().ok())
                .unwrap_or(0);
            if self.sent.len() >= end + 4 + len {
                let body = self.sent[end + 4..end + 4 + len].to_vec();
                let seen = Seen {
                    host: self.host.clone(),
                    method,
                    path,
                    headers,
                    body,
                };
                let reply = (self.world.0.borrow_mut())(&seen);
                self.world.1.borrow_mut().push(seen);
                // In pieces, so progress is reported.
                for c in reply.chunks(1000) {
                    self.out.push_back(c.to_vec());
                }
                self.sent.clear();
            }
        }
        Ok(buf.len())
    }
}

impl Net for World {
    fn connect(&mut self, host: &str, _port: u16, _t: u64) -> Result<Box<dyn Transport>, Error> {
        if !self.hosts.iter().any(|h| h == host) {
            return Err(Error::NotFound(host.to_string()));
        }
        *self.clock.borrow_mut() += 1;
        Ok(Box::new(Conn {
            host: host.to_string(),
            sent: Vec::new(),
            out: VecDeque::new(),
            world: (self.handler.clone(), self.seen.clone()),
        }))
    }

    fn now_ms(&mut self) -> u64 {
        *self.clock.borrow()
    }
}

impl ScriptNet for World {
    fn resolve(&mut self, host: &str) -> Result<Vec<Vec<u8>>, Error> {
        if let Some((_, ip)) = self.ips.iter().find(|(h, _)| h == host) {
            self.last = host.to_string();
            return Ok(alloc::vec![ip.clone()]);
        }
        let quad: Vec<u8> = host.split('.').filter_map(|p| p.parse().ok()).collect();
        if quad.len() == 4 {
            self.last = host.to_string();
            return Ok(alloc::vec![quad]);
        }
        if !self.hosts.iter().any(|h| h == host) {
            return Err(Error::NotFound(host.to_string()));
        }
        self.last = host.to_string();
        Ok(alloc::vec![alloc::vec![93, 184, 216, 34]])
    }

    fn connect_ip(&mut self, _ip: &[u8], _port: u16, _t: u64) -> Result<Box<dyn Transport>, Error> {
        *self.clock.borrow_mut() += 1;
        Ok(Box::new(Conn {
            host: self.last.clone(),
            sent: Vec::new(),
            out: VecDeque::new(),
            world: (self.handler.clone(), self.seen.clone()),
        }))
    }
}

pub fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut s = alloc::format!("HTTP/1.1 {status}\r\n");
    for (k, v) in headers {
        s.push_str(&alloc::format!("{k}: {v}\r\n"));
    }
    s.push_str(&alloc::format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut out = s.into_bytes();
    out.extend_from_slice(body);
    out
}

pub fn world(hosts: &[&str], h: impl FnMut(&Seen) -> Vec<u8> + 'static) -> World {
    World {
        handler: Rc::new(RefCell::new(Box::new(h))),
        seen: Rc::new(RefCell::new(Vec::new())),
        clock: Rc::new(RefCell::new(1000)),
        hosts: hosts.iter().map(|h| h.to_string()).collect(),
        ips: Vec::new(),
        last: String::new(),
    }
}

pub struct TestJar(pub RefCell<Jar>);

impl Cookies for TestJar {
    fn header(&self, url: &Url, post: bool, now: i64, ctx: &Context) -> Option<String> {
        self.0.borrow().header(url, post, now, ctx)
    }
    fn store(&self, url: &Url, headers: &[(String, String)], now: i64, ctx: &Context) {
        self.0.borrow_mut().set_all(url, headers, now, ctx);
    }
}

pub struct TestCache(pub RefCell<PreflightCache>);

impl Preflights for TestCache {
    fn with(&self, f: &mut dyn FnMut(&mut PreflightCache)) {
        f(&mut self.0.borrow_mut());
    }
}

/// What the page heard.
#[derive(Default)]
pub struct Heard {
    pub heads: Vec<NetHead>,
    pub progress: Vec<(usize, Option<usize>)>,
    pub cancel_after: Option<usize>,
    pub asked: usize,
}

impl Events for Heard {
    fn head(&mut self, head: &NetHead) {
        self.heads.push(head.clone());
    }
    fn progress(&mut self, got: usize, total: Option<usize>) {
        self.progress.push((got, total));
    }
    fn cancelled(&mut self) -> bool {
        self.asked += 1;
        self.cancel_after.is_some_and(|n| self.asked > n)
    }
}

pub fn req(url: &str) -> NetRequest {
    NetRequest {
        id: 1,
        kind: NetKind::Fetch,
        url: url.to_string(),
        method: "GET".to_string(),
        headers: Vec::new(),
        body: Vec::new(),
        mode: RequestMode::Cors,
        credentials: CredentialsMode::SameOrigin,
        redirect: RedirectMode::Follow,
        referrer: "about:client".to_string(),
        referrer_policy: String::new(),
        cache: "default".to_string(),
        timeout_ms: 0,
        force_preflight: false,
    }
}

pub struct Rig {
    pub cfg: Config,
    pub page: Page,
    pub jar: TestJar,
    pub cache: TestCache,
}

impl Rig {
    pub fn new(page_url: &str) -> Rig {
        let cfg = Config {
            block_private_network: false,
            ..Config::default()
        };
        Rig {
            cache: TestCache(RefCell::new(PreflightCache::new(
                cfg.limits.preflight_cache_entries,
                cfg.limits.preflight_max_age,
            ))),
            cfg,
            page: Page::new(Url::parse(page_url).unwrap()),
            jar: TestJar(RefCell::new(Jar::new())),
        }
    }

    pub fn run(&self, w: &mut World, r: &NetRequest, ev: &mut Heard) -> Result<Done, Failed> {
        let now = || 1_800_000_000i64;
        let env = Env {
            cfg: &self.cfg,
            page: &self.page,
            jar: &self.jar,
            cache: &self.cache,
            now_secs: &now,
        };
        run(w, &env, r, ev)
    }
}

#[test]
fn same_origin_get_with_head_progress_and_body() {
    let big = alloc::vec![b'x'; 5000];
    let b2 = big.clone();
    let mut w = world(&["www.milk.test"], move |s| {
        assert_eq!(s.method, "GET");
        response("200 OK", &[("Content-Type", "text/plain"), ("X-A", "1")], &b2)
    });
    let rig = Rig::new("http://www.milk.test/page");
    let mut ev = Heard::default();
    let done = rig.run(&mut w, &req("http://www.milk.test/data"), &mut ev).unwrap();
    assert_eq!(done.head.status, 200);
    assert_eq!(done.head.status_text, "OK");
    assert_eq!(done.body, big);
    assert_eq!(ev.heads.len(), 1, "the head is announced once, before the body");
    assert!(ev.progress.last().is_some_and(|p| p.0 == 5000), "{:?}", ev.progress);
    assert_eq!(done.head.kind, ResponseKind::Basic);
    assert!(done.head.headers.iter().any(|(k, v)| k == "x-a" && v == "1"));
}

#[test]
fn methods_bodies_and_content_type_reach_the_wire() {
    let mut w = world(&["www.milk.test"], |_| response("200 OK", &[], b"ok"));
    let rig = Rig::new("http://www.milk.test/page");
    for (m, body) in [("POST", &b"a=1"[..]), ("PUT", b"{\"x\":1}"), ("PATCH", b"p"), ("DELETE", b"")] {
        let mut r = req("http://www.milk.test/item");
        r.method = m.to_string();
        r.body = body.to_vec();
        r.headers.push(("Content-Type".into(), "application/json".into()));
        r.headers.push(("X-Custom".into(), "yes".into()));
        let mut ev = Heard::default();
        rig.run(&mut w, &r, &mut ev).unwrap();
    }
    let seen = w.seen.borrow();
    assert_eq!(seen.len(), 4);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].body, b"a=1");
    assert_eq!(seen[1].method, "PUT");
    assert_eq!(seen[1].header("content-type"), Some("application/json"));
    assert_eq!(seen[1].body, b"{\"x\":1}");
    assert_eq!(seen[2].method, "PATCH");
    assert_eq!(seen[3].method, "DELETE");
    assert!(seen[3].header("content-length").is_none());
    assert_eq!(seen[3].header("x-custom"), Some("yes"));
}

#[test]
fn head_has_no_body_and_failures_are_plain() {
    let mut w = world(&["www.milk.test"], |s| {
        if s.path == "/missing" {
            response("404 Not Found", &[], b"nope")
        } else {
            response("200 OK", &[("Content-Length-Hint", "x")], b"body")
        }
    });
    let rig = Rig::new("http://www.milk.test/page");
    let mut r = req("http://www.milk.test/a");
    r.method = "HEAD".to_string();
    let done = rig.run(&mut w, &r, &mut Heard::default()).unwrap();
    assert!(done.body.is_empty());
    // 404 is not a failure.
    let done = rig.run(&mut w, &req("http://www.milk.test/missing"), &mut Heard::default()).unwrap();
    assert_eq!(done.head.status, 404);
    assert_eq!(done.body, b"nope");
    // No such server: a network failure that says which request.
    let mut w2 = world(&[], |_| Vec::new());
    let e = rig.run(&mut w2, &req("http://www.milk.test/a"), &mut Heard::default()).unwrap_err();
    assert_eq!(e.failure.kind, NetFailureKind::Network);
    assert!(e.failure.message.contains("http://www.milk.test/a"), "{}", e.failure.message);
}

#[test]
fn cancelling_stops_quietly_and_cookies_ride_along() {
    let mut w = world(&["www.milk.test"], |s| {
        if s.path == "/login" {
            response("200 OK", &[("Set-Cookie", "sid=7; Path=/")], b"in")
        } else {
            let c = s.header("cookie").unwrap_or("none").to_string();
            response("200 OK", &[], c.as_bytes())
        }
    });
    let rig = Rig::new("http://www.milk.test/page");
    rig.run(&mut w, &req("http://www.milk.test/login"), &mut Heard::default()).unwrap();
    let mut r = req("http://www.milk.test/who");
    let d = rig.run(&mut w, &r, &mut Heard::default()).unwrap();
    assert_eq!(d.body, b"sid=7", "same-origin credentials include the cookie");
    r.credentials = CredentialsMode::Omit;
    let d = rig.run(&mut w, &r, &mut Heard::default()).unwrap();
    assert_eq!(d.body, b"none", "credentials: omit sends none");
    let mut ev = Heard { cancel_after: Some(0), ..Heard::default() };
    let e = rig.run(&mut w, &req("http://www.milk.test/who"), &mut ev).unwrap_err();
    assert!(e.cancelled);
}

// ---- cross-origin: CORS, preflight, credentials, redirects -------------------------------------

fn cors_ok(origin: &str, extra: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut h = alloc::vec![("Access-Control-Allow-Origin", origin), ("Content-Type", "text/plain")];
    h.extend_from_slice(extra);
    response("200 OK", &h, body)
}

fn run_page(rig: &Rig, w: &mut World, r: &NetRequest) -> (Result<Done, Failed>, Heard) {
    let mut ev = Heard::default();
    let out = rig.run(w, r, &mut ev);
    (out, ev)
}

#[test]
fn cross_origin_get_needs_allow_origin() {
    let mut w = world(&["www.milk.test", "api.other.test"], |s| {
        if s.path.starts_with("/ok") {
            cors_ok("http://www.milk.test", &[("X-Secret", "1"), ("X-Plain", "2")], b"data")
        } else if s.path.starts_with("/star") {
            cors_ok("*", &[], b"star")
        } else if s.path.starts_with("/wrong") {
            cors_ok("http://evil.test", &[], b"no")
        } else {
            response("200 OK", &[("Content-Type", "text/plain")], b"no header")
        }
    });
    let rig = Rig::new("http://www.milk.test/page");
    let (out, _) = run_page(&rig, &mut w, &req("http://api.other.test/ok"));
    let d = out.unwrap();
    assert_eq!(d.head.kind, ResponseKind::Cors);
    assert_eq!(d.body, b"data");
    // Only the safelisted response headers are shown.
    assert!(d.head.headers.iter().any(|(k, _)| k == "content-type"));
    assert!(!d.head.headers.iter().any(|(k, _)| k == "x-secret" || k == "x-plain"), "{:?}", d.head.headers);
    let seen = w.seen.borrow();
    assert_eq!(seen[0].header("origin"), Some("http://www.milk.test"), "{:?}", seen[0].headers);
    assert!(seen[0].header("referer").is_some_and(|r| r == "http://www.milk.test/"), "{:?}", seen[0].headers);
    drop(seen);
    let (out, _) = run_page(&rig, &mut w, &req("http://api.other.test/star"));
    assert_eq!(out.unwrap().body, b"star");
    for p in ["/wrong", "/none"] {
        let (out, ev) = run_page(&rig, &mut w, &req(&alloc::format!("http://api.other.test{p}")));
        let e = out.unwrap_err();
        assert_eq!(e.failure.kind, NetFailureKind::Blocked, "{p}");
        assert!(e.failure.message.contains("CORS") || e.failure.message.contains("Access-Control"), "{}", e.failure.message);
        assert!(ev.heads.is_empty(), "the page never sees a head it can't read");
    }
}

#[test]
fn exposed_headers_and_credentials() {
    let mut w = world(&["www.milk.test", "api.milk.test", "api.other.test"], |s| {
        let cred = s.header("cookie").unwrap_or("-").to_string();
        match s.path.as_str() {
            "/exposed" => cors_ok("http://www.milk.test", &[("Access-Control-Expose-Headers", "X-Secret"), ("X-Secret", "shown"), ("X-Hidden", "no")], cred.as_bytes()),
            "/cred" => cors_ok("http://www.milk.test", &[("Access-Control-Allow-Credentials", "true"), ("Set-Cookie", "t=1; Path=/")], cred.as_bytes()),
            "/credstar" => cors_ok("*", &[("Access-Control-Allow-Credentials", "true")], b"x"),
            "/nocredflag" => cors_ok("http://www.milk.test", &[], b"x"),
            _ => response("404 Not Found", &[], b""),
        }
    });
    let rig = Rig::new("http://www.milk.test/page");
    let (out, _) = run_page(&rig, &mut w, &req("http://api.milk.test/exposed"));
    let d = out.unwrap();
    assert!(d.head.headers.iter().any(|(k, v)| k == "x-secret" && v == "shown"));
    assert!(!d.head.headers.iter().any(|(k, _)| k == "x-hidden"));
    // Cookies: same site (milk.test), so credentials: include sends one that is already there.
    rig.jar.0.borrow_mut().set(&Url::parse("http://api.milk.test/").unwrap(), "pre=1; Path=/", 1_800_000_000, &Context::TopLevel { from: None }).unwrap();
    let mut r = req("http://api.milk.test/cred");
    r.credentials = CredentialsMode::Include;
    let (out, _) = run_page(&rig, &mut w, &r);
    let d = out.unwrap();
    assert_eq!(d.body, b"pre=1", "{:?}", w.seen.borrow().last());
    // ... not with same-origin credentials (a different origin) or omit.
    let (out, _) = run_page(&rig, &mut w, &req("http://api.milk.test/cred"));
    assert_eq!(out.unwrap().body, b"-", "same-origin credentials don't cover another origin");
    // The wildcard with credentials fails; so does a missing Allow-Credentials.
    for p in ["/credstar", "/nocredflag"] {
        let mut r = req(&alloc::format!("http://api.milk.test{p}"));
        r.credentials = CredentialsMode::Include;
        assert!(run_page(&rig, &mut w, &r).0.is_err(), "{p}");
    }
}

#[test]
fn preflight_for_put_and_custom_headers_is_cached() {
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let l2 = log.clone();
    let mut w = world(&["www.milk.test", "api.other.test"], move |s| {
        l2.borrow_mut().push(alloc::format!("{} {}", s.method, s.path));
        if s.method == "OPTIONS" {
            assert!(s.header("cookie").is_none(), "no credentials on a preflight");
            if s.path == "/deny" {
                return response("403 Forbidden", &[], b"");
            }
            response(
                "204 No Content",
                &[
                    ("Access-Control-Allow-Origin", "http://www.milk.test"),
                    ("Access-Control-Allow-Methods", "PUT, DELETE"),
                    ("Access-Control-Allow-Headers", "x-token, content-type"),
                    ("Access-Control-Max-Age", "60"),
                ],
                b"",
            )
        } else {
            cors_ok("http://www.milk.test", &[], b"done")
        }
    });
    let rig = Rig::new("http://www.milk.test/page");
    let mut r = req("http://api.other.test/item");
    r.method = "PUT".into();
    r.body = b"{}".to_vec();
    r.headers.push(("Content-Type".into(), "application/json".into()));
    r.headers.push(("X-Token".into(), "t".into()));
    let (out, _) = run_page(&rig, &mut w, &r);
    assert_eq!(out.unwrap().body, b"done");
    assert_eq!(*log.borrow(), ["OPTIONS /item", "PUT /item"]);
    {
        let seen = w.seen.borrow();
        let pf = &seen[0];
        assert_eq!(pf.header("access-control-request-headers"), Some("content-type,x-token"), "{:?}", pf.headers);
        assert_eq!(pf.header("origin"), Some("http://www.milk.test"));
        assert_eq!(seen[1].body, b"{}");
        assert_eq!(seen[1].header("x-token"), Some("t"));
    }
    // The second time the answer is cached: no OPTIONS.
    log.borrow_mut().clear();
    let (out, _) = run_page(&rig, &mut w, &r);
    assert!(out.is_ok());
    assert_eq!(*log.borrow(), ["PUT /item"], "the preflight is cached");
    // A header the preflight didn't allow, a method it didn't, a refusing preflight.
    log.borrow_mut().clear();
    let mut bad = r.clone();
    bad.headers.push(("X-Other".into(), "1".into()));
    assert!(run_page(&rig, &mut w, &bad).0.is_err());
    assert!(!log.borrow().iter().any(|l| l.starts_with("PUT")), "nothing is sent after a failed preflight: {:?}", log.borrow());
    let mut patch = req("http://api.other.test/item");
    patch.method = "PATCH".into();
    assert!(run_page(&rig, &mut w, &patch).0.is_err());
    let mut deny = r.clone();
    deny.url = "http://api.other.test/deny".into();
    let e = run_page(&rig, &mut w, &deny).0.unwrap_err();
    assert!(e.failure.message.contains("403") || e.failure.message.to_lowercase().contains("preflight"), "{}", e.failure.message);
}

#[test]
fn forced_preflight_for_upload_listeners() {
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let l2 = log.clone();
    let mut w = world(&["www.milk.test", "api.other.test"], move |s| {
        l2.borrow_mut().push(s.method.clone());
        if s.method == "OPTIONS" {
            response("204 No Content", &[("Access-Control-Allow-Origin", "http://www.milk.test"), ("Access-Control-Allow-Methods", "GET")], b"")
        } else {
            cors_ok("http://www.milk.test", &[], b"")
        }
    });
    let rig = Rig::new("http://www.milk.test/page");
    let mut r = req("http://api.other.test/x");
    r.force_preflight = true;
    assert!(run_page(&rig, &mut w, &r).0.is_ok());
    assert_eq!(*log.borrow(), ["OPTIONS", "GET"]);
}

#[test]
fn no_cors_gives_an_opaque_response() {
    let mut w = world(&["www.milk.test", "api.other.test"], |_| response("200 OK", &[("Content-Type", "text/plain"), ("X-A", "1")], b"secret"));
    let rig = Rig::new("http://www.milk.test/page");
    let mut r = req("http://api.other.test/x");
    r.mode = RequestMode::NoCors;
    let (out, ev) = run_page(&rig, &mut w, &r);
    let d = out.unwrap();
    assert_eq!(d.head.kind, ResponseKind::Opaque);
    assert_eq!(d.head.status, 0);
    assert!(d.head.headers.is_empty());
    assert!(d.body.is_empty(), "the page never gets the bytes");
    assert_eq!(ev.heads.len(), 1);
    // A same-origin mode request to another origin never goes out.
    let mut r = req("http://api.other.test/x");
    r.mode = RequestMode::SameOrigin;
    let before = w.seen.borrow().len();
    assert!(run_page(&rig, &mut w, &r).0.is_err());
    assert_eq!(w.seen.borrow().len(), before);
}

#[test]
fn redirects_are_judged_hop_by_hop() {
    let mut w = world(&["www.milk.test", "api.other.test"], |s| match (s.host.as_str(), s.path.as_str()) {
        ("www.milk.test", "/r1") => response("302 Found", &[("Location", "/r2")], b""),
        ("www.milk.test", "/r2") => response("200 OK", &[("Content-Type", "text/plain")], b"landed"),
        ("www.milk.test", "/to-other") => response("302 Found", &[("Location", "http://api.other.test/target")], b""),
        ("www.milk.test", "/to-other-307") => response("307 Temporary", &[("Location", "http://api.other.test/target")], b""),
        ("www.milk.test", "/to-bad") => response("302 Found", &[("Location", "ftp://x.test/f")], b""),
        ("www.milk.test", "/loop") => response("302 Found", &[("Location", "/loop")], b""),
        ("api.other.test", "/target") => cors_ok("http://www.milk.test", &[], b"cross"),
        ("api.other.test", "/target2") => response("200 OK", &[], b"no cors header"),
        _ => response("404 Not Found", &[], b""),
    });
    let rig = Rig::new("http://www.milk.test/page");
    let d = run_page(&rig, &mut w, &req("http://www.milk.test/r1")).0.unwrap();
    assert_eq!(d.body, b"landed");
    assert!(d.head.redirected);
    assert_eq!(d.head.url, "http://www.milk.test/r2");
    // Redirect mode manual: an opaque redirect, nothing followed.
    let mut r = req("http://www.milk.test/r1");
    r.redirect = RedirectMode::Manual;
    let d = run_page(&rig, &mut w, &r).0.unwrap();
    assert_eq!(d.head.kind, ResponseKind::OpaqueRedirect);
    assert_eq!(d.head.status, 0);
    // Redirect mode error.
    r.redirect = RedirectMode::Error;
    assert!(run_page(&rig, &mut w, &r).0.is_err());
    // Same-origin to cross-origin (with CORS headers there): fine, and the page sees "cors".
    let d = run_page(&rig, &mut w, &req("http://www.milk.test/to-other")).0.unwrap();
    assert_eq!(d.body, b"cross");
    assert_eq!(d.head.kind, ResponseKind::Cors);
    // A POST that is redirected with 307 keeps its body (and a cross-origin target preflights
    // or fails the CORS check: here the target's answer has the header, but a PUT would need a
    // preflight, so use POST with a simple content type).
    let mut p = req("http://www.milk.test/to-other-307");
    p.method = "POST".into();
    p.body = b"x=1".to_vec();
    p.headers.push(("Content-Type".into(), "application/x-www-form-urlencoded".into()));
    let before = w.seen.borrow().len();
    let d = run_page(&rig, &mut w, &p).0.unwrap();
    assert_eq!(d.body, b"cross");
    let seen = w.seen.borrow();
    assert_eq!(seen[before + 1].method, "POST");
    assert_eq!(seen[before + 1].body, b"x=1");
    drop(seen);
    // To a scheme we don't do; a loop.
    let e = run_page(&rig, &mut w, &req("http://www.milk.test/to-bad")).0.unwrap_err();
    assert!(e.failure.message.contains("ftp") || e.failure.message.contains("redirect"), "{}", e.failure.message);
    assert!(run_page(&rig, &mut w, &req("http://www.milk.test/loop")).0.is_err());
}

#[test]
fn private_network_and_rebinding() {
    let mut w = world(&["www.milk.test", "inward.milk.test"], |_| response("200 OK", &[("Access-Control-Allow-Origin", "*")], b"inside"));
    w.ips.push(("inward.milk.test".into(), alloc::vec![192, 168, 1, 5]));
    let mut rig = Rig::new("http://www.milk.test/page");
    rig.cfg.block_private_network = true;
    // A public page asking for a private address literal: refused before anything is sent.
    let e = run_page(&rig, &mut w, &req("http://192.168.1.9/x")).0.unwrap_err();
    assert_eq!(e.failure.kind, NetFailureKind::Blocked, "{}", e.failure.message);
    assert!(w.seen.borrow().is_empty());
    // A name that points inward (rebinding): caught when the name is looked up.
    let e = run_page(&rig, &mut w, &req("http://inward.milk.test/x")).0.unwrap_err();
    assert_eq!(e.failure.kind, NetFailureKind::Blocked);
    assert!(e.failure.message.to_lowercase().contains("private") || e.failure.message.to_lowercase().contains("local"), "{}", e.failure.message);
    assert!(w.seen.borrow().is_empty(), "nothing was connected to");
    // A private page may reach private addresses.
    let mut rig2 = Rig::new("http://192.168.1.2/page");
    rig2.cfg.block_private_network = true;
    assert!(run_page(&rig2, &mut w, &req("http://192.168.1.9/x")).0.is_ok());
}

#[test]
fn mixed_content_and_limits() {
    let mut w = world(&["secure.milk.test", "plain.other.test"], |s| {
        if s.path == "/big" {
            response("200 OK", &[], &alloc::vec![b'x'; 300_000])
        } else {
            response("200 OK", &[("Access-Control-Allow-Origin", "*")], b"ok")
        }
    });
    let mut rig = Rig::new("https://secure.milk.test/page");
    let e = run_page(&rig, &mut w, &req("http://plain.other.test/x")).0.unwrap_err();
    assert_eq!(e.failure.kind, NetFailureKind::Blocked);
    assert!(e.failure.message.to_lowercase().contains("mixed"), "{}", e.failure.message);
    assert!(w.seen.borrow().is_empty());
    // The response size limit.
    let rig2 = Rig::new("http://plain.other.test/page");
    let mut rig2 = rig2;
    rig2.cfg.limits.max_response_body = 100_000;
    let e = run_page(&rig2, &mut w, &req("http://plain.other.test/big")).0.unwrap_err();
    assert!(matches!(e.failure.kind, NetFailureKind::TooBig | NetFailureKind::Blocked), "{:?} {}", e.failure.kind, e.failure.message);
    // A request body over the limit never goes.
    rig.cfg.limits.max_request_body = 10;
    let mut r = req("https://secure.milk.test/x");
    r.method = "POST".into();
    r.body = alloc::vec![0; 100];
    let e = run_page(&rig, &mut w, &r).0.unwrap_err();
    assert_eq!(e.failure.kind, NetFailureKind::Blocked);
}

#[test]
fn timeouts_and_stalls() {
    // A server that never answers (the fake connection ends the stream; a stalled read is the
    // client's timeout): here the policy's own limit is tiny and the clock moves.
    let mut w = world(&["www.milk.test"], |_| Vec::new());
    let mut rig = Rig::new("http://www.milk.test/page");
    rig.cfg.limits.timeout_ms = 50;
    let e = run_page(&rig, &mut w, &req("http://www.milk.test/x")).0.unwrap_err();
    assert!(matches!(e.failure.kind, NetFailureKind::Network | NetFailureKind::Timeout), "{:?} {}", e.failure.kind, e.failure.message);
}

#[test]
fn set_cookie_follows_the_cors_check() {
    let mut w = world(&["www.milk.test", "api.milk.test", "tracker.other.test"], |s| match s.host.as_str() {
        "api.milk.test" if s.path == "/ok" => cors_ok("http://www.milk.test", &[("Access-Control-Allow-Credentials", "true"), ("Set-Cookie", "good=1; Path=/")], b""),
        "api.milk.test" => response("200 OK", &[("Set-Cookie", "bad=1; Path=/")], b""),
        _ => cors_ok("*", &[("Set-Cookie", "tracker=1; Path=/")], b""),
    });
    let rig = Rig::new("http://www.milk.test/page");
    let jar_has = |name: &str| rig.jar.0.borrow().all().iter().any(|c| c.name == name);
    let mut r = req("http://api.milk.test/ok");
    r.credentials = CredentialsMode::Include;
    run_page(&rig, &mut w, &r).0.unwrap();
    assert!(jar_has("good"), "a CORS-approved response from the same site may set a cookie");
    // Without the CORS header the request fails and its cookie is not kept.
    let mut r = req("http://api.milk.test/nocors");
    r.credentials = CredentialsMode::Include;
    assert!(run_page(&rig, &mut w, &r).0.is_err());
    assert!(!jar_has("bad"));
    // Another site: never, credentials or not.
    let mut r = req("http://tracker.other.test/x");
    r.credentials = CredentialsMode::Include;
    let _ = run_page(&rig, &mut w, &r);
    assert!(!jar_has("tracker"), "no third-party cookies");
}
