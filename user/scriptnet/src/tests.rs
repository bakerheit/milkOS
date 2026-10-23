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
use webengine::*;

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
    /// Hosts that exist.
    pub hosts: Vec<String>,
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
