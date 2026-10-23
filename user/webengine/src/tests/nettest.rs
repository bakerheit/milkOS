//! fetch, XMLHttpRequest and the classes around them (script/net.rs and script/js/*.js),
//! over a fake network: the page's requests come out of `Document::take_net`, a closure plays the
//! server, and its answers go back with `Document::net_event`, one task each, the way the app does.

use alloc::string::String;
use alloc::vec::Vec;

use super::script::{Host, doc};
use crate::api::*;

/// One answer, delivered after `after` ticks (20 ms of the page's clock each).
pub struct Reply {
    pub after: u32,
    pub ev: NetEvent,
}

pub fn head(status: u16, text: &str, headers: &[(&str, &str)]) -> NetHead {
    NetHead {
        status,
        status_text: String::from(text),
        headers: headers.iter().map(|(k, v)| (String::from(*k), String::from(*v))).collect(),
        url: String::new(),
        redirected: false,
        kind: ResponseKind::Basic,
    }
}

/// Head and body at once.
pub fn reply(req: &NetRequest, status: u16, headers: &[(&str, &str)], body: &str) -> Vec<Reply> {
    let mut h = head(status, if status == 200 { "OK" } else { "Status" }, headers);
    h.url = req.url.clone();
    alloc::vec![
        Reply { after: 0, ev: NetEvent::Head(h) },
        Reply { after: 0, ev: NetEvent::Done(body.as_bytes().to_vec()) },
    ]
}

pub fn fail(kind: NetFailureKind, msg: &str) -> Vec<Reply> {
    alloc::vec![Reply { after: 0, ev: NetEvent::Failed(NetFailure { kind, message: String::from(msg) }) }]
}

pub struct Run {
    pub lines: Vec<String>,
    pub requests: Vec<NetRequest>,
    pub cancelled: Vec<u32>,
    pub pending: usize,
    pub doc: Document,
}

/// Run `js` in a page, answering its requests with `serve`, for up to `ticks` ticks.
pub fn run_with(js: &str, ticks: u32, mut serve: impl FnMut(&NetRequest) -> Vec<Reply>) -> Run {
    let html = alloc::format!("<!DOCTYPE html><body><script>{js}</script></body>");
    let mut d = doc(&html);
    let mut h = Host::new();
    let mut lines: Vec<String> = Vec::new();
    let mut requests = Vec::new();
    let mut cancelled = Vec::new();
    // (due tick, id, event)
    let mut queue: Vec<(u32, u32, NetEvent)> = Vec::new();
    for tick in 0..ticks {
        d.run(&mut h);
        loop {
            let outs = d.take_net();
            let mut got = false;
            for o in outs {
                got = true;
                match o {
                    NetOut::Start(req) => {
                        for r in serve(&req) {
                            queue.push((tick + r.after, req.id, r.ev));
                        }
                        requests.push(req);
                    }
                    NetOut::Cancel(id) => {
                        cancelled.push(id);
                        queue.retain(|q| q.1 != id);
                    }
                }
            }
            // Deliver what is due, in order.
            let mut i = 0;
            while i < queue.len() {
                if queue[i].0 <= tick {
                    let (_, id, ev) = queue.remove(i);
                    d.net_event(&mut h, id, ev);
                    got = true;
                } else {
                    i += 1;
                }
            }
            if !got {
                break;
            }
        }
        lines.extend(d.take_console().into_iter().map(|l| l.text));
        h.now += 20.0;
    }
    let pending = d.net_pending();
    Run { lines, requests, cancelled, pending, doc: d }
}

pub fn run(js: &str, serve: impl FnMut(&NetRequest) -> Vec<Reply>) -> Run {
    run_with(js, 40, serve)
}

/// The lines starting with `T ` (what the test page logs), without it.
pub fn t_lines(r: &Run) -> Vec<&str> {
    r.lines.iter().filter_map(|l| l.strip_prefix("T ")).collect()
}

fn header<'a>(req: &'a NetRequest, name: &str) -> Option<&'a str> {
    req.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

#[test]
fn fetch_text_and_json() {
    let r = run(
        "fetch('/api/hello').then(r => { console.log('T status ' + r.status + ' ' + r.ok + ' ' + r.statusText);
            console.log('T type ' + r.type + ' url ' + r.url + ' ct ' + r.headers.get('Content-Type'));
            return r.text(); }).then(t => console.log('T text ' + t));
         fetch('/api/data', {headers: {Accept: 'application/json'}}).then(r => r.json()).then(j => console.log('T json ' + j.a + ' ' + j.b.join()));",
        |req| match req.url.as_str() {
            "http://www.milk.test/api/hello" => reply(req, 200, &[("content-type", "text/plain")], "hello"),
            "http://www.milk.test/api/data" => reply(req, 200, &[("content-type", "application/json")], r#"{"a":1,"b":[2,3]}"#),
            _ => fail(NetFailureKind::Network, "no route"),
        },
    );
    let t = t_lines(&r);
    assert!(t.contains(&"status 200 true OK"), "{:?}", r.lines);
    assert!(t.contains(&"type basic url http://www.milk.test/api/hello ct text/plain"), "{:?}", r.lines);
    assert!(t.contains(&"text hello"), "{:?}", r.lines);
    assert!(t.contains(&"json 1 2,3"), "{:?}", r.lines);
    assert_eq!(r.requests.len(), 2);
    assert_eq!(r.requests[1].method, "GET");
    assert_eq!(header(&r.requests[1], "accept"), Some("application/json"));
    assert_eq!(r.requests[0].mode, RequestMode::Cors);
    assert_eq!(r.requests[0].credentials, CredentialsMode::SameOrigin);
    assert_eq!(r.pending, 0);
}

// Scratch (removed before the final commit): run the script file SCRATCH_JS and print the console.
#[test]
fn scratch() {
    let Ok(path) = std::env::var("SCRATCH_JS") else { return };
    let js = std::fs::read_to_string(path).unwrap();
    let r = run(&js, |req| match req.url.as_str() {
        u if u.ends_with("/ok") => reply(req, 200, &[("content-type", "text/plain")], "fine"),
        _ => fail(NetFailureKind::Network, "no route"),
    });
    for l in &r.lines {
        std::println!("{l}");
    }
}
