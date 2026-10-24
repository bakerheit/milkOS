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
    /// (request, bytes used) for every acknowledgement the page sent.
    pub acks: Vec<(u32, u64)>,
    pub pending: usize,
    pub doc: Document,
}

/// Run `js` in a page, answering its requests with `serve`, for up to `ticks` ticks.
pub fn run_with(js: &str, ticks: u32, serve: impl FnMut(&NetRequest) -> Vec<Reply>) -> Run {
    run_limits(js, ticks, ScriptLimits::default(), serve)
}

/// [`run_with`] with the page's own script limits.
pub fn run_limits(js: &str, ticks: u32, limits: ScriptLimits, mut serve: impl FnMut(&NetRequest) -> Vec<Reply>) -> Run {
    let html = alloc::format!("<!DOCTYPE html><body><script>{js}</script></body>");
    let mut d = super::script::doc_with(&html, limits);
    let mut h = Host::new();
    let mut lines: Vec<String> = Vec::new();
    let mut requests = Vec::new();
    let mut cancelled = Vec::new();
    let mut acks: Vec<(u32, u64)> = Vec::new();
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
                    NetOut::Ack { id, consumed } => acks.push((id, consumed)),
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
    Run { lines, requests, cancelled, acks, pending, doc: d }
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


/// What the page logged with `T `, in order.
fn out(js: &str, serve: impl FnMut(&NetRequest) -> Vec<Reply>) -> Vec<String> {
    let r = run(js, serve);
    t_lines(&r).iter().map(|s| String::from(*s)).collect()
}

fn ok_text(req: &NetRequest, body: &str) -> Vec<Reply> {
    reply(req, 200, &[("content-type", "text/plain")], body)
}

/// The test server for pages that only need an answer.
fn echo(req: &NetRequest) -> Vec<Reply> {
    let body = alloc::format!(
        "{} {} [{}] {}",
        req.method,
        req.url,
        req.headers.iter().map(|(k, v)| alloc::format!("{k}={v}")).collect::<Vec<_>>().join(";"),
        String::from_utf8_lossy(&req.body)
    );
    ok_text(req, &body)
}

fn no_answer(_: &NetRequest) -> Vec<Reply> {
    Vec::new()
}

// ---- fetch: requests ----------------------------------------------------------------------------

#[test]
fn fetch_post_json_and_body_types() {
    let r = run(
        "fetch('/api', {method: 'post', body: JSON.stringify({a: 1}), headers: {'Content-Type': 'application/json', 'X-Token': 'abc'}});
         fetch('/s', {method: 'POST', body: 'plain'});
         fetch('/u', {method: 'POST', body: new URLSearchParams({a: '1 2', b: 'x&y'})});
         fetch('/b', {method: 'PUT', body: new Blob(['blobdata'], {type: 'text/csv'})});
         fetch('/ab', {method: 'POST', body: new Uint8Array([1, 2, 3, 255]).buffer});
         fetch('/v', {method: 'POST', body: new Uint16Array([0x4142, 0x4344])});
         fetch('/empty', {method: 'POST'});
         var fd = new FormData(); fd.append('k', 'v1'); fd.append('f', new Blob(['file body'], {type: 'text/plain'}), 'a.txt');
         fetch('/fd', {method: 'POST', body: fd});",
        echo,
    );
    let q = &r.requests;
    assert_eq!(q.len(), 8, "{:?}", r.lines);
    assert_eq!(q[0].method, "POST");
    assert_eq!(q[0].body, br#"{"a":1}"#);
    assert_eq!(header(&q[0], "content-type"), Some("application/json"));
    assert_eq!(header(&q[0], "x-token"), Some("abc"));
    assert_eq!(header(&q[1], "content-type"), Some("text/plain;charset=UTF-8"));
    assert_eq!(header(&q[2], "content-type"), Some("application/x-www-form-urlencoded;charset=UTF-8"));
    assert_eq!(q[2].body, b"a=1+2&b=x%26y");
    assert_eq!(q[3].method, "PUT");
    assert_eq!(header(&q[3], "content-type"), Some("text/csv"));
    assert_eq!(q[3].body, b"blobdata");
    assert_eq!(q[4].body, [1, 2, 3, 255]);
    assert_eq!(header(&q[4], "content-type"), None, "a buffer has no type");
    assert_eq!(q[5].body, [0x42, 0x41, 0x44, 0x43], "a Uint16Array's bytes are its view's (little endian)");
    assert!(q[6].body.is_empty());
    let ct = header(&q[7], "content-type").unwrap();
    assert!(ct.starts_with("multipart/form-data; boundary=----MilkOSFormBoundary"), "{ct}");
    let boundary = ct.split("boundary=").nth(1).unwrap();
    let body = String::from_utf8_lossy(&q[7].body).into_owned();
    assert!(body.starts_with(&alloc::format!("--{boundary}\r\nContent-Disposition: form-data; name=\"k\"\r\n\r\nv1\r\n")), "{body}");
    assert!(body.contains("Content-Disposition: form-data; name=\"f\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nfile body\r\n"), "{body}");
    assert!(body.ends_with(&alloc::format!("--{boundary}--\r\n")), "{body}");
}

#[test]
fn fetch_request_options_reach_the_app() {
    let r = run(
        "fetch('/a', {mode: 'no-cors', credentials: 'include', redirect: 'manual', cache: 'no-store', referrer: '', referrerPolicy: 'origin'});
         fetch('http://other.test/x', {mode: 'cors', credentials: 'omit', redirect: 'error', referrer: 'http://www.milk.test/here'});
         fetch(new Request('/r', {method: 'delete'}));
         fetch('/rel/../up?q=1#frag');",
        no_answer,
    );
    let q = &r.requests;
    assert_eq!(q[0].mode, RequestMode::NoCors);
    assert_eq!(q[0].credentials, CredentialsMode::Include);
    assert_eq!(q[0].redirect, RedirectMode::Manual);
    assert_eq!(q[0].cache, "no-store");
    assert_eq!(q[0].referrer, "");
    assert_eq!(q[0].referrer_policy, "origin");
    assert_eq!(q[1].url, "http://other.test/x");
    assert_eq!(q[1].credentials, CredentialsMode::Omit);
    assert_eq!(q[1].redirect, RedirectMode::Error);
    assert_eq!(q[1].referrer, "http://www.milk.test/here");
    assert_eq!(q[2].method, "DELETE");
    assert_eq!(q[3].url, "http://www.milk.test/up?q=1", "relative, dot segments gone, no fragment on the wire");
    assert_eq!(q[3].kind, NetKind::Fetch);
    assert!(!q[3].force_preflight);
}

#[test]
fn fetch_headers_a_script_may_not_set_never_leave() {
    let r = run(
        "fetch('/x', {headers: {Cookie: 'a=b', Host: 'evil', 'Sec-Fetch-Mode': 'x', 'Proxy-Authorization': 'p', Origin: 'http://evil', Referer: 'x', 'Accept-Encoding': 'br', 'Content-Length': '1', Connection: 'close', 'X-Fine': 'yes', 'User-Agent': 'Mine', 'Set-Cookie': 'z=1', Date: 'x', Via: '1.1 x', TE: 'trailers', Upgrade: 'h2c'}});
         fetch('/y', {mode: 'no-cors', headers: {Accept: 'text/html', 'Content-Type': 'application/json', 'X-Custom': '1', 'Accept-Language': 'en'}});",
        no_answer,
    );
    let names = |i: usize| r.requests[i].headers.iter().map(|(k, _)| k.to_ascii_lowercase()).collect::<Vec<_>>();
    assert_eq!(names(0), ["x-fine", "user-agent"], "{:?}", r.requests[0].headers);
    assert_eq!(names(1), ["accept", "accept-language"], "no-cors keeps only the safelisted ones");
}

#[test]
fn fetch_url_and_request_validation() {
    let l = out(
        "const tries = [
           () => new Request('http://u:p' + String.fromCharCode(64) + 'x.test/'),
           () => new Request('/x', {method: 'TRACE'}),
           () => new Request('/x', {method: 'Con nect'}),
           () => new Request('/x', {method: 'GET', body: 'x'}),
           () => new Request('/x', {mode: 'navigate'}),
           () => new Request('/x', {mode: 'bogus'}),
           () => new Request('/x', {mode: 'no-cors', method: 'PUT'}),
           () => new Request('/x', {cache: 'only-if-cached'}),
           () => new Request('/x', {referrerPolicy: 'sometimes'}),
           () => new Request('http://'),
           () => new Request('/x', {signal: 5}),
           () => new Request('/x', {headers: {'bad name': '1'}}),
           () => new Request('/x', {headers: {'x': 'a\\nb'}}),
         ];
         for (const f of tries) { try { f(); console.log('T no error'); } catch (e) { console.log('T ' + e.name + ': ' + e.message.slice(0, 50)); } }
         const r = new Request('/x', {method: 'patch'}); console.log('T ' + r.method);
         console.log('T ' + new Request('/x', {method: 'put'}).method + ' ' + new Request('/x', {method: 'Options'}).method);
         fetch().catch(e => console.log('T none ' + e.name));
         fetch('ftp://x.test/f').catch(e => console.log('T ftp ' + e.name));
         fetch('javascript:alert(1)').catch(e => console.log('T js ' + e.name));
         fetch('file:///etc/passwd').catch(e => console.log('T file ' + e.name));
         fetch('mailto:a' + String.fromCharCode(64) + 'b').catch(e => console.log('T mailto ' + e.name));",
        no_answer,
    );
    let want = [
        "TypeError: Failed to construct 'Request': Request cannot be",
        "TypeError: Failed to construct 'Request': 'TRACE' HTTP meth",
        "TypeError: Failed to construct 'Request': 'Con nect' is not",
        "TypeError: Failed to construct 'Request': Request with GET/",
        "TypeError: Failed to construct 'Request': Cannot construct ",
        "TypeError: Failed to construct 'Request': The provided valu",
        "TypeError: Failed to construct 'Request': 'PUT' is unsuppor",
        "TypeError: Failed to construct 'Request': 'only-if-cached' ",
        "TypeError: Failed to construct 'Request': The provided valu",
        "TypeError: Failed to construct 'Request': Failed to parse U",
        "TypeError: Failed to construct 'Request': Failed to convert",
        "TypeError: Failed to execute 'append' on 'Headers': Invalid",
        "TypeError: Failed to execute 'append' on 'Headers': Invalid",
        "patch",
        "PUT OPTIONS",
    ];
    assert!(l.len() >= want.len(), "{l:?}");
    for (got, want) in l.iter().zip(want) {
        assert!(got.starts_with(want), "{got:?} vs {want:?}");
    }
    assert!(l.contains(&String::from("none TypeError")), "{l:?}");
    for (k, v) in [("ftp", "TypeError"), ("js", "TypeError"), ("file", "TypeError"), ("mailto", "TypeError")] {
        assert!(l.contains(&alloc::format!("{k} {v}")), "{l:?}");
    }
}

// ---- fetch: responses ---------------------------------------------------------------------------

#[test]
fn fetch_404_resolves_and_network_error_rejects() {
    let r = run(
        "fetch('/missing').then(r => console.log('T 404 ' + r.status + ' ' + r.ok + ' ' + r.statusText));
         fetch('/down').then(() => console.log('T up?'), e => console.log('T rejected ' + (e instanceof TypeError) + ' ' + e.message + ' cause=' + (e.cause && e.cause.message)));
         fetch('/blocked').catch(e => console.log('T blocked ' + e.message));",
        |req| match req.url.as_str() {
            "http://www.milk.test/missing" => reply(req, 404, &[], "gone"),
            "http://www.milk.test/blocked" => fail(NetFailureKind::Blocked, "Blocked by CORS: nope"),
            _ => fail(NetFailureKind::Network, "GET /down failed: connection refused"),
        },
    );
    let t = t_lines(&r);
    assert!(t.contains(&"404 404 false Status"), "{:?}", r.lines);
    assert!(t.contains(&"rejected true Failed to fetch cause=GET /down failed: connection refused"), "{:?}", r.lines);
    assert!(t.contains(&"blocked Failed to fetch"), "the page is only told it failed: {:?}", r.lines);
    // The host's text reaches the console instead.
    assert!(r.lines.iter().any(|l| l == "Blocked by CORS: nope"), "{:?}", r.lines);
    assert!(r.lines.iter().any(|l| l == "GET /down failed: connection refused"));
}

#[test]
fn response_bodies_every_way() {
    let l = out(
        "fetch('/bin', {method: 'POST'}).then(async r => {
           const c = r.clone();
           const ab = await r.arrayBuffer();
           console.log('T ab ' + ab.byteLength + ' ' + new Uint8Array(ab).join(','));
           console.log('T used ' + r.bodyUsed + ' ' + c.bodyUsed);
           const bl = await c.blob();
           console.log('T blob ' + bl.size + ' ' + bl.type);
           try { await r.text(); } catch (e) { console.log('T again ' + e.name); }
           const t = await c.clone().text().catch(e => 'cloned-after-use ' + e.name);
           console.log('T ' + t);
         });
         fetch('/bom').then(r => r.text()).then(t => console.log('T bom [' + t + '] ' + t.length));
         fetch('/bad-utf8').then(r => r.text()).then(t => console.log('T bad ' + t.charCodeAt(0).toString(16)));
         fetch('/form').then(r => r.formData()).then(f => console.log('T form ' + f.get('a') + ' ' + f.getAll('b').join('|')));
         fetch('/mp').then(r => r.formData()).then(f => console.log('T mp ' + f.get('name') + ' ' + f.get('file').name + ' ' + f.get('file').size + ' ' + f.get('file').type));
         fetch('/json-bad').then(r => r.json()).catch(e => console.log('T json ' + e.name));
         fetch('/empty204').then(r => console.log('T 204 ' + r.status + ' ' + (r.body === null)));",
        |req| match req.url.as_str() {
            "http://www.milk.test/bin" => {
                let mut v = reply(req, 200, &[("content-type", "Application/Octet-Stream")], "");
                v[1].ev = NetEvent::Done(alloc::vec![0, 1, 2, 250, 255]);
                v
            }
            "http://www.milk.test/bom" => {
                let mut v = reply(req, 200, &[], "");
                v[1].ev = NetEvent::Done(b"\xEF\xBB\xBFhi".to_vec());
                v
            }
            "http://www.milk.test/bad-utf8" => {
                let mut v = reply(req, 200, &[], "");
                v[1].ev = NetEvent::Done(alloc::vec![0xC3, 0x28]);
                v
            }
            "http://www.milk.test/form" => reply(req, 200, &[("content-type", "application/x-www-form-urlencoded")], "a=1&b=x%20y&b=2+3"),
            "http://www.milk.test/mp" => reply(
                req,
                200,
                &[("content-type", "multipart/form-data; boundary=XX")],
                "--XX\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\nmilk\r\n--XX\r\nContent-Disposition: form-data; name=\"file\"; filename=\"c.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n--XX--\r\n",
            ),
            "http://www.milk.test/json-bad" => ok_text(req, "{not json"),
            "http://www.milk.test/empty204" => reply(req, 204, &[], ""),
            _ => Vec::new(),
        },
    );
    for want in [
        "ab 5 0,1,2,250,255",
        "used true false",
        "blob 5 application/octet-stream",
        "again TypeError",
        "bom [hi] 2",
        "bad fffd",
        "form 1 x y|2 3",
        "mp milk c.txt 5 text/plain",
        "json SyntaxError",
        "204 204 true",
    ] {
        assert!(l.iter().any(|x| x == want), "missing {want:?} in {l:?}");
    }
}

#[test]
fn the_response_body_stream() {
    let l = out(
        "fetch('/s').then(async r => {
           console.log('T has ' + (r.body instanceof ReadableStream) + ' locked ' + r.body.locked);
           const rd = r.body.getReader();
           console.log('T locked ' + r.body.locked);
           let n = 0; for (;;) { const {done, value} = await rd.read(); if (done) break; n += value.length; console.log('T chunk ' + value.constructor.name); }
           console.log('T total ' + n + ' used ' + r.bodyUsed);
           try { await r.text(); } catch (e) { console.log('T after ' + e.name); }
           rd.releaseLock();
           console.log('T unlocked ' + r.body.locked);
         });",
        |req| ok_text(req, "stream me"),
    );
    assert_eq!(l, ["has true locked false", "locked true", "chunk Uint8Array", "total 9 used true", "after TypeError", "unlocked false"]);
}

#[test]
fn abort_before_during_and_after() {
    let r = run(
        "const c1 = new AbortController(); c1.abort();
         fetch('/a', {signal: c1.signal}).catch(e => console.log('T pre ' + e.name + ' ' + (e === c1.signal.reason)));
         const c2 = new AbortController();
         fetch('/b', {signal: c2.signal}).catch(e => console.log('T mid ' + e.name));
         setTimeout(() => c2.abort(), 30);
         const c3 = new AbortController();
         fetch('/c', {signal: c3.signal}).then(async r => {
            try { await r.text(); console.log('T no'); } catch (e) { console.log('T body ' + e.name); }
         });
         setTimeout(() => c3.abort(new Error('custom')), 60);
         const c4 = new AbortController();
         fetch('/d', {signal: c4.signal}).then(r => r.text()).then(t => { console.log('T late ' + t); c4.abort(); });
         AbortSignal.timeout(50).addEventListener('abort', function (e) { console.log('T timeout ' + this.reason.name); });
         fetch('/e', {signal: AbortSignal.timeout(40)}).catch(e => console.log('T timed ' + e.name));
         fetch('/f', {signal: AbortSignal.abort('why')}).catch(e => console.log('T why ' + e));",
        |req| match req.url.as_str() {
            "http://www.milk.test/b" => Vec::new(),
            "http://www.milk.test/c" => {
                let mut h = head(200, "OK", &[]);
                h.url = req.url.clone();
                alloc::vec![Reply { after: 0, ev: NetEvent::Head(h) }]
            }
            "http://www.milk.test/d" => ok_text(req, "dd"),
            _ => Vec::new(),
        },
    );
    let t = t_lines(&r);
    for want in ["pre AbortError true", "mid AbortError", "body Error", "late dd", "timeout TimeoutError", "timed TimeoutError", "why why"] {
        assert!(t.contains(&want), "missing {want:?} in {:?}", r.lines);
    }
    assert_eq!(r.cancelled.len(), 3, "b, c and e are cancelled; d had finished: {:?}", r.cancelled);
    assert_eq!(r.pending, 0);
}

// ---- Headers ------------------------------------------------------------------------------------

#[test]
fn headers_behave_like_the_standard() {
    let l = out(
        "const h = new Headers([['Content-Type', 'text/plain'], ['X-B', ' two '], ['x-a', '1']]);
         h.append('X-A', '2'); h.append('set-cookie', 'a=1'); h.append('Set-Cookie', 'b=2');
         console.log('T ' + JSON.stringify([...h]));
         console.log('T get ' + h.get('x-a') + ' | ' + h.get('X-B') + ' | ' + h.get('nope') + ' | ' + h.has('CONTENT-TYPE'));
         console.log('T cookies ' + JSON.stringify(h.getSetCookie()) + ' get ' + h.get('set-cookie'));
         h.set('x-a', 'only'); h.delete('x-b');
         console.log('T ' + [...h.keys()].join() + ' / ' + [...h.values()].join('|'));
         const seen = []; h.forEach((v, k, o) => seen.push(k + '=' + v + (o === h ? '!' : ''))); console.log('T ' + seen.join(' '));
         console.log('T from object ' + JSON.stringify([...new Headers({B: '1', a: '2'})]));
         console.log('T from headers ' + JSON.stringify([...new Headers(new Headers({z: 'y'}))]));
         const bad = (f) => { try { f(); return 'ok'; } catch (e) { return e.name; } };
         console.log('T bad ' + [bad(() => new Headers({'a b': '1'})), bad(() => new Headers({a: 'x\\r\\ny'})), bad(() => h.append('', '1')), bad(() => new Headers([['a']])), bad(() => new Headers(5)), bad(() => new Headers({a: '\\u0100'})), bad(() => h.get('bad name')), bad(() => h.append('a'))].join());
         console.log('T tag ' + Object.prototype.toString.call(h) + ' ' + (h instanceof Headers));
         console.log('T order ' + JSON.stringify([...new Headers([['B', '1'], ['a', '2'], ['b', '3']])]));
         const it = h[Symbol.iterator](); console.log('T iter ' + (typeof it.next) + ' ' + (it.next().value instanceof Array));",
        no_answer,
    );
    let want = [
        r#"[["content-type","text/plain"],["set-cookie","a=1"],["set-cookie","b=2"],["x-a","1, 2"],["x-b","two"]]"#,
        "get 1, 2 | two | null | true",
        r#"cookies ["a=1","b=2"] get a=1, b=2"#,
        "content-type,set-cookie,set-cookie,x-a / text/plain|a=1|b=2|only",
        "content-type=text/plain! set-cookie=a=1! set-cookie=b=2! x-a=only!",
        r#"from object [["a","2"],["b","1"]]"#,
        r#"from headers [["z","y"]]"#,
        "bad TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError",
        "tag [object Headers] true",
        r#"order [["a","2"],["b","1, 3"]]"#,
        "iter function true",
    ];
    assert_eq!(l, want);
}

#[test]
fn header_guards() {
    let l = out(
        "const r = new Request('/x', {headers: {Cookie: 'a=1', 'X-Ok': '1', 'Proxy-Foo': 'x', 'Sec-Bar': 'y'}});
         console.log('T request ' + JSON.stringify([...r.headers]));
         r.headers.set('Host', 'evil'); r.headers.append('Content-Length', '9'); r.headers.set('X-More', '2');
         console.log('T after ' + JSON.stringify([...r.headers]));
         const nc = new Request('/x', {mode: 'no-cors', headers: {Accept: 'a/b', 'X-Custom': '1', 'Content-Type': 'application/json'}});
         nc.headers.set('Accept-Language', 'de'); nc.headers.set('Content-Type', 'text/plain'); nc.headers.append('Content-Language', 'fr');
         console.log('T no-cors ' + JSON.stringify([...nc.headers]));
         const res = new Response('x', {headers: {'Set-Cookie': 'a=1', 'X-Ok': '1', 'Set-Cookie2': 'z'}});
         console.log('T response ' + JSON.stringify([...res.headers]));
         fetch('/net').then(f => {
           try { f.headers.set('x', '1'); console.log('T mutable?'); } catch (e) { console.log('T immutable ' + e.name); }
           try { f.headers.delete('content-type'); } catch (e) { console.log('T immutable delete ' + e.name); }
           console.log('T net-headers ' + JSON.stringify([...f.headers]));
         });
         const e = Response.error(); try { e.headers.append('a', 'b'); } catch (x) { console.log('T error immutable ' + x.name); }
         const rd = Response.redirect('/there', 301); console.log('T redirect ' + rd.status + ' ' + rd.headers.get('location'));
         try { rd.headers.set('a', 'b'); } catch (x) { console.log('T redirect immutable'); }
         const fresh = new Headers({Cookie: 'x'}); console.log('T plain keeps cookie ' + fresh.get('cookie'));",
        |req| reply(req, 200, &[("content-type", "text/plain"), ("set-cookie", "leak=1"), ("x-a", "1")], "n"),
    );
    let want = [
        r#"request [["x-ok","1"]]"#,
        r#"after [["x-more","2"],["x-ok","1"]]"#,
        r#"no-cors [["accept","a/b"],["accept-language","de"],["content-language","fr"],["content-type","text/plain"]]"#,
        r#"response [["content-type","text/plain;charset=UTF-8"],["x-ok","1"]]"#,
        "error immutable TypeError",
        "redirect 301 http://www.milk.test/there",
        "redirect immutable",
        "plain keeps cookie x",
        "immutable TypeError",
        "immutable delete TypeError",
        r#"net-headers [["content-type","text/plain"],["x-a","1"]]"#,
    ];
    assert_eq!(l, want);
}

// ---- Request and Response -----------------------------------------------------------------------

#[test]
fn request_class() {
    let l = out(
        "const r = new Request('/path?x=1', {method: 'post', body: 'hello', headers: {a: 'b'}});
         console.log('T ' + [r.method, r.url, r.mode, r.credentials, r.cache, r.redirect, r.referrer, JSON.stringify(r.referrerPolicy), r.keepalive, r.destination === '', r.signal.aborted, r.bodyUsed].join(' '));
         const c = r.clone(); const c2 = r.clone();
         r.text().then(t => { console.log('T text ' + t + ' ' + r.bodyUsed + ' ' + c.bodyUsed); return c.text(); }).then(t => {
           console.log('T clone ' + t);
           try { r.clone(); console.log('T clone of used?'); } catch (e) { console.log('T used ' + e.name); }
           try { new Request(r); console.log('T from used?'); } catch (e) { console.log('T from used ' + e.name); }
         });
         const r2 = new Request(c2, {method: 'PUT', headers: {x: 'y'}});
         console.log('T from request ' + r2.method + ' ' + r2.url + ' ' + r2.headers.get('x') + ' ' + r2.headers.get('a') + ' ' + c2.bodyUsed);
         const sig = new AbortController(); const r3 = new Request('/z', {signal: sig.signal}); sig.abort();
         console.log('T signal follows ' + r3.signal.aborted + ' ' + (r3.signal !== sig.signal));
         console.log('T tag ' + Object.prototype.toString.call(r) + ' ' + Object.getOwnPropertyNames(Request.prototype).includes('arrayBuffer'));
         new Request('/j', {method: 'POST', body: JSON.stringify([1,2])}).json().then(j => console.log('T json ' + j));
         new Request('/e').text().then(t => console.log('T empty [' + t + ']'));
         console.log('T body null ' + (new Request('/g').body === null) + ' ' + (r2.body !== null));",
        no_answer,
    );
    let want = [
        "POST http://www.milk.test/path?x=1 cors same-origin default follow about:client \"\" false true false false",
        "from request PUT http://www.milk.test/path?x=1 y null true",
        "signal follows true true",
        "tag [object Request] true",
        "body null true true",
        "text hello true false",
        "json 1,2",
        "empty []",
        "clone hello",
        "used TypeError",
        "from used TypeError",
    ];
    // (Promise callbacks run after the synchronous lines.)
    let mut got = l.clone();
    got.sort();
    let mut want_sorted: Vec<String> = want.iter().map(|s| String::from(*s)).collect();
    want_sorted.sort();
    assert_eq!(got, want_sorted, "{l:?}");
}

#[test]
fn response_class() {
    let l = out(
        "const r = new Response('body', {status: 201, statusText: 'Made', headers: {'X-A': '1'}});
         console.log('T ' + [r.status, r.statusText, r.ok, r.type, JSON.stringify(r.url), r.redirected, r.headers.get('content-type'), r.bodyUsed].join(' '));
         console.log('T default ' + new Response().status + ' [' + new Response().statusText + '] ' + (new Response().body === null) + ' ' + new Response(null).headers.get('content-type'));
         const bad = (f) => { try { f(); return 'ok'; } catch (e) { return e.name; } };
         console.log('T bad ' + [bad(() => new Response('', {status: 199})), bad(() => new Response('', {status: 600})), bad(() => new Response('x', {status: 204})), bad(() => new Response('', {statusText: 'a\\nb'})), bad(() => Response.redirect('/x', 200)), bad(() => Response.json(undefined)), bad(() => Response.redirect('http://'))].join());
         const j = Response.json({a: [1, 2]}, {status: 202});
         j.text().then(t => console.log('T json ' + t + ' ' + j.status + ' ' + j.headers.get('content-type')));
         const e = Response.error(); console.log('T error ' + e.type + ' ' + e.status + ' ' + e.ok + ' ' + e.statusText + '|');
         const u = new Response(new URLSearchParams({q: '1'})); console.log('T usp ' + u.headers.get('content-type'));
         const b = new Response(new Blob(['x'], {type: 'image/png'})); console.log('T blob ' + b.headers.get('content-type'));
         const keep = new Response('x', {headers: {'content-type': 'text/html'}}); console.log('T keep ' + keep.headers.get('content-type'));
         const c = new Response('abc'); const d = c.clone(); c.text().then(a => d.text().then(b => console.log('T clone ' + a + b)));
         console.log('T tag ' + Object.prototype.toString.call(r));
         new Response(new Uint8Array([104, 105])).text().then(t => console.log('T bytes ' + t));
         new Response(new ReadableStream({start(c) { c.enqueue(new Uint8Array([111, 107])); c.close(); }})).text().then(t => console.log('T stream ' + t));
         r.arrayBuffer().then(ab => console.log('T len ' + ab.byteLength + ' ' + r.bodyUsed));
         const red = Response.redirect('http://other.test/a b', 307); console.log('T redirect ' + red.headers.get('location') + ' ' + red.status);",
        no_answer,
    );
    for want in [
        "201 Made true default \"\" false text/plain;charset=UTF-8 false",
        "default 200 [] true null",
        "bad RangeError,RangeError,TypeError,TypeError,RangeError,TypeError,TypeError",
        "json {\"a\":[1,2]} 202 application/json",
        "error error 0 false |",
        "usp application/x-www-form-urlencoded;charset=UTF-8",
        "blob image/png",
        "keep text/html",
        "clone abcabc",
        "tag [object Response]",
        "bytes hi",
        "stream ok",
        "len 4 true",
        "redirect http://other.test/a%20b 307",
    ] {
        assert!(l.iter().any(|x| x == want), "missing {want:?} in {l:#?}");
    }
}

// ---- URL and URLSearchParams --------------------------------------------------------------------

#[test]
fn url_parts_and_setters() {
    let l = out(
        "const AT = String.fromCharCode(64);
         const p = (s, b) => { try { const u = new URL(s, b); return [u.href, u.origin, u.protocol, u.host, u.hostname, u.port, u.pathname, u.search, u.hash, u.username, u.password].join(' '); } catch (e) { return e.name; } };
         for (const [s, b] of [
           ['http://Example.COM:80/a/./b/../c?x=1#h'], ['https://example.com:443/'], ['https://example.com:8443/p'],
           ['/rel', 'http://a.test/dir/file'], ['../up', 'http://a.test/dir/sub/file'], ['?q', 'http://a.test/p?old#h'], ['#only', 'http://a.test/p?q'],
           ['//other.test/x', 'https://a.test/'], ['http://u:p' + AT + 'h.test/'], ['http://[::1]:8080/p'], ['mailto:a' + AT + 'b.test'], ['data:text/plain,hi'],
           ['http://a b.test/'], ['http://'], ['nope'], ['http://a.test:99999/'], ['http://a.test:abc/'], ['http://a.test/a b?c d#e f'], ['http://a.test/%7Efoo'],
           ['HTTP://A.TEST/Path'], ['http:\\\\\\\\a.test\\\\x'], ['file:///etc/hosts'], ['blob:http://a.test/uuid'], ['javascript:void 0'], ['http://a.test/a?b=1&c=%20&d=é'],
         ]) console.log('T ' + s + ' => ' + p(s, b));",
        no_answer,
    );
    let find = |pre: &str| l.iter().find(|x| x.starts_with(pre)).cloned().unwrap_or_default();
    assert_eq!(
        find("http://Example.COM:80/a"),
        "http://Example.COM:80/a/./b/../c?x=1#h => http://example.com/a/c?x=1#h http://example.com http: example.com example.com  /a/c ?x=1 #h  "
    );
    assert!(find("https://example.com:443/ =>").contains("=> https://example.com/ https://example.com https: example.com example.com  / "), "{}", find("https://example.com:443/ =>"));
    assert!(find("https://example.com:8443/p").contains("https://example.com:8443/p https://example.com:8443 https: example.com:8443 example.com 8443 /p"));
    assert!(find("/rel =>").contains("=> http://a.test/rel "), "{}", find("/rel =>"));
    assert!(find("../up").contains("=> http://a.test/dir/up "));
    assert!(find("?q").contains("=> http://a.test/p?q "), "{}", find("?q"));
    assert!(find("#only").contains("=> http://a.test/p?q#only "));
    assert!(find("//other.test/x").contains("=> https://other.test/x "));
    let at = '@';
    let creds = alloc::format!("http://u:p{at}h.test/ =>");
    assert!(find(&creds).ends_with(" u p"), "{}", find(&creds));
    assert!(find("http://[::1]:8080/p").contains("=> http://[::1]:8080/p http://[::1]:8080 http: [::1]:8080 [::1] 8080 /p"), "{}", find("http://[::1]:8080/p"));
    let mail = alloc::format!("mailto:a{at}b.test");
    assert!(find(&mail).contains(&alloc::format!("=> {mail} null mailto: ")), "{}", find(&mail));
    assert!(find("data:text/plain,hi").contains("=> data:text/plain,hi null data:"));
    for bad in ["http://a b.test/", "http://", "nope", "http://a.test:99999/", "http://a.test:abc/"] {
        assert!(find(&alloc::format!("{bad} =>")).ends_with("=> TypeError"), "{}", find(&alloc::format!("{bad} =>")));
    }
    assert!(find("http://a.test/a b?c d#e f").contains("=> http://a.test/a%20b?c%20d#e%20f "), "{}", find("http://a.test/a b?c d#e f"));
    assert!(find("http://a.test/%7Efoo").contains("=> http://a.test/%7Efoo "));
    assert!(find("HTTP://A.TEST/Path").contains("=> http://a.test/Path "), "{}", find("HTTP://A.TEST/Path"));
    assert!(find("file:///etc/hosts").contains("=> file:///etc/hosts null file:"), "{}", find("file:///etc/hosts"));
    assert!(find("blob:http://a.test/uuid").contains("=> blob:http://a.test/uuid http://a.test blob:"), "{}", find("blob:http://a.test/uuid"));
    assert!(find("http://a.test/a?b=1&c=%20&d=é").contains("?b=1&c=%20&d=%C3%A9"), "{}", find("http://a.test/a?b=1&c=%20&d=é"));

    let l = out(
        "const u = new URL('http://user:pw' + String.fromCharCode(64) + 'a.test:8080/p/q?x=1&y=2#frag');
         u.pathname = '/new dir/x'; u.search = 'a=1&b=2'; u.hash = 'top'; u.port = '9'; u.hostname = 'B.test'; u.username = 'me'; u.password = '';
         console.log('T ' + u.href);
         u.port = ''; u.protocol = 'https'; u.host = 'c.test:444'; u.search = ''; u.hash = '';
         console.log('T ' + u.href + ' ' + u.origin);
         u.protocol = 'mailto'; console.log('T keeps ' + u.protocol);
         u.href = 'http://fresh.test/x?z=9'; console.log('T ' + u.searchParams.get('z') + ' ' + u.host);
         u.searchParams.append('w', 'a b'); console.log('T ' + u.href);
         u.search = '?k=v'; console.log('T ' + u.searchParams.get('k') + ' ' + u.searchParams.has('w') + ' ' + u.search);
         u.searchParams.delete('k'); console.log('T ' + u.href + '|');
         console.log('T ' + String(u) + ' ' + JSON.stringify({u}) + ' ' + (u.searchParams === u.searchParams));
         console.log('T can ' + URL.canParse('http://x.test') + URL.canParse('x') + URL.canParse('x', 'http://y.test/'));
         u.pathname = 'noslash'; console.log('T ' + u.pathname);
         try { u.href = 'bad'; } catch (e) { console.log('T href ' + e.name); }
         console.log('T parse ' + (URL.parse('zz') === null) + ' ' + URL.parse('/a', 'http://q.test').href);",
        no_answer,
    );
    assert_eq!(l[0], alloc::format!("http://me{at}b.test:9/new%20dir/x?a=1&b=2#top"));
    assert_eq!(l[1], alloc::format!("https://me{at}c.test:444/new%20dir/x https://c.test:444"));
    assert_eq!(l[2], "keeps https:");
    assert_eq!(l[3], "9 fresh.test");
    assert_eq!(l[4], "http://fresh.test/x?z=9&w=a+b");
    assert_eq!(l[5], "v false ?k=v");
    assert_eq!(l[6], "http://fresh.test/x|");
    assert_eq!(l[7], "http://fresh.test/x {\"u\":\"http://fresh.test/x\"} true");
    assert_eq!(l[8], "can truefalsetrue");
    assert_eq!(l[9], "/noslash");
    assert_eq!(l[10], "href TypeError");
    assert_eq!(l[11], "parse true http://q.test/a");
}

#[test]
fn url_search_params() {
    let l = out(
        "const p = new URLSearchParams('?a=1&b=2&a=3&c&=d&e=%20x+y%2B&f=%E2%82%AC&g=%zz');
         console.log('T ' + JSON.stringify([...p]));
         console.log('T ' + p.get('a') + p.getAll('a') + p.get('c') + p.get('nope') + p.has('c') + p.has('a', '3') + p.has('a', '9') + p.size);
         p.append('h', 'é&='); p.set('a', 'only'); p.delete('b'); p.delete('e', 'zzz');
         console.log('T ' + p.toString());
         p.sort(); console.log('T ' + [...p.keys()].join());
         console.log('T ' + JSON.stringify([...new URLSearchParams({x: '1', y: '2'})]) + JSON.stringify([...new URLSearchParams([['k', 'v'], ['k', 'w']])]) + [...new URLSearchParams(new URLSearchParams('m=n'))].join());
         const bad = (f) => { try { f(); return 'ok'; } catch (e) { return e.name; } };
         console.log('T ' + [bad(() => new URLSearchParams([['a']])), bad(() => new URLSearchParams([5])), bad(() => p.get())].join());
         const seen = []; p.forEach((v, k) => seen.push(k + v)); console.log('T ' + seen.length);
         console.log('T ' + new URLSearchParams('a=b c&d=%u0041').toString() + ' ' + Object.prototype.toString.call(p) + ' ' + new URLSearchParams(null).toString() + new URLSearchParams(undefined).toString() + '|');
         const q = new URLSearchParams('a=1'); for (const [k, v] of q) { if (k === 'a') q.append('b', '2'); if (q.size > 5) break; } console.log('T live ' + q);",
        no_answer,
    );
    let want = [
        r#"[["a","1"],["b","2"],["a","3"],["c",""],["","d"],["e"," x y+"],["f","€"],["g","%zz"]]"#,
        "11,3nulltruetruefalse8",
        "a=only&c=&=d&e=+x+y%2B&f=%E2%82%AC&g=%25zz&h=%C3%A9%26%3D",
        ",a,c,e,f,g,h",
        r#"[["x","1"],["y","2"]][["k","v"],["k","w"]]m,n"#,
        "TypeError,TypeError,TypeError",
        "7",
        "a=b+c&d=%25u0041 [object URLSearchParams] null=|",
        "live a=1&b=2",
    ];
    assert_eq!(l, want);
}

// ---- XMLHttpRequest -----------------------------------------------------------------------------

#[test]
fn xhr_get_event_order_and_accessors() {
    let l = out(
        "const x = new XMLHttpRequest();
         const log = (s) => console.log('T ' + s);
         ['readystatechange', 'loadstart', 'progress', 'load', 'loadend', 'error', 'abort', 'timeout'].forEach(t => x.addEventListener(t, e => log(t + ' ' + x.readyState + ' ' + e.constructor.name + (t === 'readystatechange' ? '' : ' ' + e.loaded + '/' + e.total + (e.lengthComputable ? '!' : '')))));
         log('new ' + x.readyState + ' ' + x.status + ' [' + x.responseText + '] ' + x.responseType + '|' + x.response + '|' + x.getAllResponseHeaders() + '|' + x.getResponseHeader('x'));
         x.open('GET', '/data.txt');
         log('opened ' + x.readyState);
         x.setRequestHeader('X-Test', 'a'); x.setRequestHeader('x-test', 'b');
         x.send();
         log('sent ' + x.readyState);",
        |req| {
            let mut h = head(200, "OK", &[("Content-Type", "text/plain; charset=utf-8"), ("X-Two", "a"), ("x-two", "b"), ("Set-Cookie", "no=1")]);
            h.url = req.url.clone();
            alloc::vec![
                Reply { after: 1, ev: NetEvent::Head(h) },
                Reply { after: 2, ev: NetEvent::Progress { loaded: 5, total: Some(11) } },
                Reply { after: 3, ev: NetEvent::Done(b"hello world".to_vec()) },
            ]
        },
    );
    let want = [
        "new 0 0 [] |||null",
        "readystatechange 1 Event",
        "opened 1",
        "loadstart 1 ProgressEvent 0/0",
        "sent 1",
        "readystatechange 2 Event",
        "readystatechange 3 Event",
        "progress 3 ProgressEvent 5/11!",
        "readystatechange 4 Event",
        "progress 4 ProgressEvent 11/11!",
        "load 4 ProgressEvent 11/11!",
        "loadend 4 ProgressEvent 11/11!",
    ];
    assert_eq!(l, want);
    let r = run(
        "const x = new XMLHttpRequest(); x.open('GET', '/d'); x.send();
         x.onload = () => console.log('T ' + [x.status, x.statusText, x.responseText, x.response, x.responseURL, x.getResponseHeader('X-TWO'), x.getResponseHeader('content-type'), x.getResponseHeader('set-cookie'), JSON.stringify(x.getAllResponseHeaders()), x.responseXML].join(' ; '));",
        |req| {
            let mut v = reply(req, 200, &[("Content-Type", "text/plain; charset=utf-8"), ("X-Two", "a"), ("x-two", "b"), ("Set-Cookie", "no=1")], "hello");
            if let NetEvent::Head(h) = &mut v[0].ev {
                h.url = String::from("http://www.milk.test/final#frag");
                h.status_text = String::from("Fine");
            }
            v
        },
    );
    let t = t_lines(&r);
    assert_eq!(
        t,
        ["200 ; Fine ; hello ; hello ; http://www.milk.test/final ; a, b ; text/plain; charset=utf-8 ;  ; \"content-type: text/plain; charset=utf-8\\r\\nx-two: a, b\\r\\n\" ; "]
    );
    assert_eq!(r.requests[0].kind, NetKind::Xhr);
    assert_eq!(header(&r.requests[0], "x-test"), None);
}

#[test]
fn xhr_requests_headers_bodies_and_credentials() {
    let r = run(
        "const send = (m, u, body, f) => { const x = new XMLHttpRequest(); x.open(m, u); if (f) f(x); x.send(body); };
         send('POST', '/a', 'text body', x => { x.setRequestHeader('X-A', '1'); x.setRequestHeader('X-A', '2'); x.setRequestHeader('Cookie', 'no'); x.setRequestHeader('Content-Type', 'application/json'); });
         send('POST', '/b', 'plain');
         send('PUT', '/c', new Blob(['bb'], {type: 'text/x'}));
         send('POST', '/d', new URLSearchParams({k: 'v'}));
         send('GET', '/e', 'ignored for GET', x => { x.withCredentials = true; x.timeout = 1500; });
         send('DELETE', '/f');
         send('POST', '/g', new Uint8Array([9, 8]));
         const fd = new FormData(); fd.append('a', 'b'); send('POST', '/h', fd);
         send('POST', '/i', null);
         send('patch', '/j', 'p');",
        no_answer,
    );
    let q = &r.requests;
    assert_eq!(q.len(), 10, "{:?}", r.lines);
    assert_eq!(header(&q[0], "x-a"), Some("1, 2"), "setRequestHeader combines");
    assert_eq!(header(&q[0], "cookie"), None);
    assert_eq!(header(&q[0], "content-type"), Some("application/json"));
    assert_eq!(header(&q[1], "content-type"), Some("text/plain;charset=UTF-8"));
    assert_eq!(header(&q[2], "content-type"), Some("text/x"));
    assert_eq!(q[3].body, b"k=v");
    assert!(q[4].body.is_empty(), "no body with GET");
    assert_eq!(q[4].credentials, CredentialsMode::Include);
    assert_eq!(q[4].timeout_ms, 1500);
    assert_eq!(q[0].credentials, CredentialsMode::SameOrigin);
    assert_eq!(q[0].mode, RequestMode::Cors);
    assert_eq!(q[5].method, "DELETE");
    assert_eq!(q[6].body, [9, 8]);
    assert!(header(&q[7], "content-type").unwrap().starts_with("multipart/form-data; boundary="));
    assert!(q[8].body.is_empty());
    assert_eq!(q[9].method, "patch", "an unknown-case method keeps its case");
    assert!(r.lines.iter().any(|l| l.contains("Refused to set unsafe header")), "{:?}", r.lines);
}

#[test]
fn xhr_response_types() {
    let l = out(
        "const get = (u, type, f) => { const x = new XMLHttpRequest(); x.open('GET', u); if (type !== undefined) x.responseType = type; x.onload = () => f(x); x.send(); };
         get('/j', 'json', x => console.log('T json ' + JSON.stringify(x.response) + ' ' + typeof x.response));
         get('/jbad', 'json', x => console.log('T json bad ' + x.response));
         get('/b', 'arraybuffer', x => console.log('T ab ' + x.response.byteLength + ' ' + new Uint8Array(x.response).join() + ' ' + (x.response === x.response)));
         get('/b', 'blob', x => console.log('T blob ' + x.response.size + ' ' + x.response.type + ' ' + (x.response === x.response)));
         get('/t', 'text', x => console.log('T text ' + x.response + ' ' + x.responseText));
         get('/l1', undefined, x => console.log('T latin1 ' + x.responseText.length + ' ' + x.responseText.charCodeAt(1).toString(16)));
         get('/doc', 'document', x => console.log('T doc ' + x.response + ' ' + x.responseXML));
         get('/t', 'json', x => { try { x.responseText; console.log('T no throw'); } catch (e) { console.log('T responseText ' + e.name); } });
         const y = new XMLHttpRequest(); y.open('GET', '/t'); y.responseType = 'bogus'; console.log('T type ' + JSON.stringify(y.responseType));
         y.send(); try { y.responseType = 'text'; y.onreadystatechange = () => { if (y.readyState === 4) { try { y.responseType = 'json'; } catch (e) { console.log('T late ' + e.name); } } }; } catch (e) {}",
        |req| match req.url.as_str() {
            "http://www.milk.test/j" => reply(req, 200, &[("content-type", "application/json")], r#"{"a":[1,2]}"#),
            "http://www.milk.test/jbad" => reply(req, 200, &[], "{oops"),
            "http://www.milk.test/b" => {
                let mut v = reply(req, 200, &[("content-type", "Image/PNG")], "");
                v[1].ev = NetEvent::Done(alloc::vec![137, 80, 78, 71, 0, 255]);
                v
            }
            "http://www.milk.test/l1" => {
                let mut v = reply(req, 200, &[("content-type", "text/plain; charset=ISO-8859-1")], "");
                v[1].ev = NetEvent::Done(alloc::vec![b'a', 0xE9, b'b']);
                v
            }
            _ => ok_text(req, "plain"),
        },
    );
    for want in [
        r#"json {"a":[1,2]} object"#,
        "json bad null",
        "ab 6 137,80,78,71,0,255 true",
        "blob 6 image/png true",
        "text plain plain",
        "latin1 3 e9",
        "doc null null",
        "responseText InvalidStateError",
        "type \"\"",
        "late InvalidStateError",
    ] {
        assert!(l.iter().any(|x| x == want), "missing {want:?} in {l:#?}");
    }
}

#[test]
fn xhr_errors_abort_timeout_and_misuse() {
    let r = run(
        "const mk = (u) => { const x = new XMLHttpRequest(); const log = [];
            ['loadstart', 'load', 'error', 'abort', 'timeout', 'loadend', 'readystatechange'].forEach(t => x.addEventListener(t, () => log.push(t + x.readyState)));
            x.open('GET', u); return {x, log}; };
         const down = mk('/down'); down.x.send();
         const aborted = mk('/slow'); aborted.x.send(); setTimeout(() => { aborted.x.abort(); console.log('T after abort ' + aborted.x.readyState + ' ' + aborted.x.status); }, 30);
         const slow = mk('/slow'); slow.x.timeout = 50; slow.x.send();
         const nf = mk('/404'); nf.x.send();
         const twice = mk('/slow'); twice.x.send(); try { twice.x.send(); } catch (e) { console.log('T twice ' + e.name); }
         const sync = new XMLHttpRequest(); try { sync.open('GET', '/x', false); } catch (e) { console.log('T sync ' + e.name); }
         const un = new XMLHttpRequest(); try { un.send(); } catch (e) { console.log('T unsent ' + e.name); } try { un.setRequestHeader('a', 'b'); } catch (e) { console.log('T hdr ' + e.name); }
         const badm = new XMLHttpRequest(); try { badm.open('TRACE', '/x'); } catch (e) { console.log('T trace ' + e.name); } try { badm.open('bad method', '/x'); } catch (e) { console.log('T method ' + e.name); } try { badm.open('GET', 'http://'); } catch (e) { console.log('T url ' + e.name); }
         const reopen = mk('/slow'); reopen.x.send(); reopen.x.open('GET', '/ok2'); console.log('T reopen ' + reopen.x.readyState);
         const wc = new XMLHttpRequest(); wc.open('GET', '/ok'); wc.send(); try { wc.withCredentials = true; } catch (e) { console.log('T wc ' + e.name); }
         setTimeout(() => { console.log('T down ' + down.log.join()); console.log('T abort ' + aborted.log.join()); console.log('T timeout ' + slow.log.join()); console.log('T 404 ' + nf.log.join() + ' ' + nf.x.status); }, 150);",
        |req| match req.url.as_str() {
            "http://www.milk.test/down" => fail(NetFailureKind::Network, "down"),
            "http://www.milk.test/404" => reply(req, 404, &[], "x"),
            _ => Vec::new(),
        },
    );
    let t = t_lines(&r);
    for want in [
        "after abort 0 0",
        "twice InvalidStateError",
        "sync InvalidAccessError",
        "unsent InvalidStateError",
        "hdr InvalidStateError",
        "trace SecurityError",
        "method SyntaxError",
        "url SyntaxError",
        "reopen 1",
        "wc InvalidStateError",
        "down readystatechange1,loadstart1,readystatechange4,error4,loadend4",
        "abort readystatechange1,loadstart1,readystatechange4,abort4,loadend4",
        "timeout readystatechange1,loadstart1,readystatechange4,timeout4,loadend4",
        "404 readystatechange1,loadstart1,readystatechange2,readystatechange4,load4,loadend4 404",
    ] {
        assert!(t.contains(&want), "missing {want:?} in {:?}", r.lines);
    }
    assert!(r.lines.iter().any(|l| l.contains("Synchronous XMLHttpRequest")), "{:?}", r.lines);
    assert!(r.lines.iter().any(|l| l == "down"), "the host's text reaches the console");
}

#[test]
fn xhr_upload_events_and_preflight_flag() {
    let r = run(
        "const x = new XMLHttpRequest(); x.open('POST', '/up'); const log = [];
         x.upload.onloadstart = () => log.push('up-loadstart'); x.upload.onprogress = e => log.push('up-progress ' + e.loaded + '/' + e.total); x.upload.onload = () => log.push('up-load'); x.upload.onloadend = () => log.push('up-loadend');
         x.onload = () => { console.log('T ' + log.join()); console.log('T ' + (x.upload instanceof EventTarget) + ' ' + Object.prototype.toString.call(x.upload)); };
         x.send('12345');
         const y = new XMLHttpRequest(); y.open('GET', '/plain'); y.send();",
        |req| ok_text(req, "ok"),
    );
    assert_eq!(
        t_lines(&r),
        ["up-loadstart,up-progress 5/5,up-load,up-loadend", "true [object XMLHttpRequestUpload]"]
    );
    assert!(r.requests[0].force_preflight, "listeners on upload force a preflight");
    assert!(!r.requests[1].force_preflight);
}

// ---- Blob, File, FormData, FileReader -----------------------------------------------------------

#[test]
fn blob_file_and_formdata() {
    let l = out(
        "const b = new Blob(['ab', new Uint8Array([99, 100]), new Blob(['ef']), 12], {type: 'TEXT/Plain'});
         console.log('T ' + b.size + ' ' + b.type + ' ' + Object.prototype.toString.call(b));
         b.slice(1, 5).text().then(t => console.log('T slice ' + t));
         b.slice(-3).text().then(t => console.log('T neg ' + t));
         b.slice(2, -1, 'X/Y').text().then(t => console.log('T mid ' + t + ' ' + b.slice(0, 1, 'X/Y').type));
         b.slice(5, 2).text().then(t => console.log('T empty [' + t + ']'));
         new Blob(['é€😀']).arrayBuffer().then(ab => console.log('T utf8 ' + ab.byteLength));
         new Blob().text().then(t => console.log('T none [' + t + ']'));
         const bad = (f) => { try { f(); return 'ok'; } catch (e) { return e.name; } };
         console.log('T bad ' + [bad(() => new Blob(5)), bad(() => new Blob('str')), bad(() => new Blob([], 5)), bad(() => new File(['x']))].join());
         const f = new File(['hello'], 'a.txt', {type: 'text/plain', lastModified: 1234});
         console.log('T file ' + [f.name, f.size, f.type, f.lastModified, f instanceof Blob, Object.prototype.toString.call(f)].join(' '));
         const fd = new FormData();
         fd.append('a', '1'); fd.append('a', '2'); fd.append('b', f); fd.append('c', new Blob(['zz']), 'z.bin'); fd.append('d', new Blob(['yy']));
         console.log('T fd ' + fd.getAll('a') + ' ' + (fd.get('b') === f) + ' ' + fd.get('c').name + ' ' + (fd.get('c') instanceof File) + ' ' + fd.get('d').name + ' ' + fd.has('x') + fd.has('a'));
         fd.set('a', 'only'); fd.delete('c'); console.log('T ' + [...fd.keys()].join() + ' ' + [...fd].length);
         const names = []; fd.forEach((v, k) => names.push(k + (typeof v === 'string' ? '=' + v : ':' + v.name))); console.log('T ' + names.join(' '));
         console.log('T ' + bad(() => fd.append('x', 'y', 'name')) + bad(() => fd.append('x')) + bad(() => new FormData(5)));
         URL.revokeObjectURL(URL.createObjectURL(b));",
        no_answer,
    );
    for want in [
        "8 text/plain [object Blob]",
        "slice bcde",
        "neg f12",
        "mid cdef1 x/y",
        "empty []",
        "utf8 9",
        "none []",
        "bad TypeError,TypeError,TypeError,TypeError",
        "file a.txt 5 text/plain 1234 true [object File]",
        "fd 1,2 true z.bin true blob falsetrue",
        "a,b,d 3",
        "a=only b:a.txt d:blob",
        "TypeErrorTypeErrorTypeError",
    ] {
        assert!(l.iter().any(|x| x == want), "missing {want:?} in {l:#?}");
    }
}

#[test]
fn formdata_from_a_form() {
    let html = r#"<!DOCTYPE html><body><form id=f action=/x><input name=a value="1 2"><input type=checkbox name=c value=on checked><input type=checkbox name=d value=off>
        <select name=s><option value=x>X<option value=y selected>Y</select><textarea name=t>line1
line2</textarea><input type=submit name=go value=Go><input name=off disabled value=no></form><script>
        const fd = new FormData(document.getElementById('f'));
        console.log('T ' + JSON.stringify([...fd]));
        </script>"#;
    let mut d = doc(html);
    let mut h = Host::new();
    d.run(&mut h);
    let lines: Vec<String> = d.take_console().into_iter().map(|l| l.text).collect();
    assert!(
        lines.iter().any(|l| l == r#"T [["a","1 2"],["c","on"],["s","y"],["t","line1\r\nline2"]]"#),
        "{lines:?}"
    );
}

#[test]
fn file_reader_reads_blobs() {
    let l = out(
        "const b = new Blob(['hé'], {type: 'text/plain'});
         const r1 = new FileReader(); r1.onload = () => console.log('T text ' + r1.result + ' ' + r1.readyState); r1.onloadend = () => console.log('T end'); r1.readAsText(b);
         const r2 = new FileReader(); r2.onload = () => console.log('T url ' + r2.result); r2.readAsDataURL(b);
         const r3 = new FileReader(); r3.onload = () => console.log('T buf ' + r3.result.byteLength); r3.readAsArrayBuffer(b);
         const r4 = new FileReader(); r4.onload = () => console.log('T bin ' + r4.result.length); r4.readAsBinaryString(b);
         const r5 = new FileReader(); r5.onload = () => console.log('T never'); r5.onabort = () => console.log('T abort'); r5.readAsText(b); r5.abort();
         const r6 = new FileReader(); r6.readAsText(b); try { r6.readAsText(b); } catch (e) { console.log('T busy ' + e.name); }
         try { new FileReader().readAsText('x'); } catch (e) { console.log('T arg ' + e.name); }
         console.log('T state ' + r1.readyState + ' ' + r1.result + ' ' + FileReader.DONE);",
        no_answer,
    );
    for want in [
        "text hé 2",
        "end",
        "url data:text/plain;base64,aMOp",
        "buf 3",
        "bin 3",
        "abort",
        "busy InvalidStateError",
        "arg TypeError",
        "state 1 null 2",
    ] {
        assert!(l.iter().any(|x| x == want), "missing {want:?} in {l:#?}");
    }
    assert!(!l.iter().any(|x| x == "never"));
}

// ---- AbortSignal, DOMException, ReadableStream, events ------------------------------------------

#[test]
fn abort_signal_any_timeout_and_listeners() {
    let l = out(
        "const a = new AbortController(), b = new AbortController();
         const any = AbortSignal.any([a.signal, b.signal]);
         any.onabort = () => console.log('T any aborted: ' + any.reason);
         console.log('T before ' + any.aborted);
         b.abort('b-reason');
         console.log('T after ' + any.aborted + ' ' + any.reason + ' ' + a.signal.aborted);
         const done = AbortSignal.any([a.signal, AbortSignal.abort('pre')]); console.log('T pre ' + done.aborted + ' ' + done.reason);
         const s = AbortSignal.abort(); console.log('T default ' + s.reason.name + ' ' + s.reason.code + ' ' + (s.reason instanceof DOMException) + ' ' + (s.reason instanceof Error));
         try { s.throwIfAborted(); } catch (e) { console.log('T throws ' + e.name); }
         const c = new AbortController(); const order = [];
         c.signal.addEventListener('abort', () => order.push('one')); c.signal.addEventListener('abort', () => order.push('two'), {once: true}); c.signal.onabort = () => order.push('handler');
         c.signal.addEventListener('abort', function f() { order.push('three'); c.signal.removeEventListener('abort', f); });
         c.abort(); c.abort(); console.log('T order ' + order.join());
         const gone = new AbortController(); const ac2 = new AbortController(); gone.signal.addEventListener('abort', () => console.log('T no'), {signal: ac2.signal}); ac2.abort(); gone.abort();
         const t = AbortSignal.timeout(20); t.addEventListener('abort', () => console.log('T timeout ' + t.reason.name + ' ' + t.reason.code));
         console.log('T new ' + bad(() => new AbortSignal()) + ' ' + (new AbortController().signal instanceof EventTarget) + ' ' + Object.prototype.toString.call(a));
         function bad(f) { try { f(); return 'ok'; } catch (e) { return e.name; } }
         const ev = []; const sig = new AbortController(); sig.signal.addEventListener('abort', e => ev.push(e.type + ':' + e.target.constructor.name + ':' + (e.currentTarget === sig.signal) + ':' + e.isTrusted + ':' + e.eventPhase)); sig.abort(); console.log('T ev ' + ev);
         const target = sig.signal; let calls = 0; target.addEventListener('abort', { handleEvent(e) { calls++; } }); target.dispatchEvent(new Event('abort')); console.log('T handleEvent ' + calls);",
        no_answer,
    );
    for want in [
        "before false",
        "after true b-reason false",
        "pre true pre",
        "default AbortError 20 true true",
        "throws AbortError",
        "order one,two,handler,three",
        "new TypeError true [object AbortController]",
        "ev abort:AbortSignal:true:false:2",
        "handleEvent 1",
        "any aborted: b-reason",
        "timeout TimeoutError 23",
    ] {
        assert!(l.iter().any(|x| x == want), "missing {want:?} in {l:#?}");
    }
    assert!(!l.iter().any(|x| x == "no"));
}

#[test]
fn dom_exception_and_progress_event() {
    let l = out(
        "const e = new DOMException('msg', 'NotFoundError');
         console.log('T ' + [e.name, e.message, e.code, e instanceof Error, e instanceof DOMException, String(e), DOMException.NOT_FOUND_ERR, Object.prototype.toString.call(e)].join(' '));
         const d = new DOMException(); console.log('T ' + d.name + ' ' + d.code + ' [' + d.message + ']');
         console.log('T ' + new DOMException('x', 'WeirdError').code + ' ' + (typeof e.stack));
         const p = new ProgressEvent('progress', {lengthComputable: true, loaded: 3, total: 10});
         console.log('T ' + [p.type, p.loaded, p.total, p.lengthComputable, p instanceof Event, p.bubbles, Object.prototype.toString.call(p)].join(' '));
         const q = new ProgressEvent('x'); console.log('T ' + [q.loaded, q.total, q.lengthComputable].join());",
        no_answer,
    );
    assert_eq!(
        l,
        [
            "NotFoundError msg 8 true true NotFoundError: msg 8 [object DOMException]",
            "Error 0 []",
            "0 string",
            "progress 3 10 true true false [object ProgressEvent]",
            "0,0,false",
        ]
    );
}

#[test]
fn readable_stream_basics() {
    let l = out(
        "const log = [];
         const s = new ReadableStream({
           start(c) { log.push('start'); c.enqueue('a'); },
           pull(c) { log.push('pull'); c.enqueue('b'); c.close(); },
           cancel(r) { log.push('cancel ' + r); },
         });
         (async () => {
           const rd = s.getReader();
           const out = [];
           for (;;) { const {value, done} = await rd.read(); if (done) break; out.push(value); }
           console.log('T ' + out.join() + ' ' + log.join());
           await rd.closed; console.log('T closed');
           const s2 = new ReadableStream({ start(c) { c.error(new Error('boom')); } });
           try { await s2.getReader().read(); } catch (e) { console.log('T errored ' + e.message); }
           const s3 = new ReadableStream({ cancel(r) { console.log('T cancelled ' + r); } });
           await s3.cancel('because'); console.log('T locked? ' + s3.locked);
           const s4 = new ReadableStream({ start(c) { c.enqueue(1); c.enqueue(2); c.close(); } });
           const [x, y] = s4.tee(); const r1 = await x.getReader().read(); const r2 = await y.getReader().read(); console.log('T tee ' + r1.value + r2.value);
           const s5 = new ReadableStream({ start(c) { c.enqueue('p'); c.enqueue('q'); c.close(); } }); const got = [];
           for await (const v of s5) got.push(v); console.log('T iter ' + got.join());
           const s6 = new ReadableStream(); s6.getReader(); try { s6.getReader(); } catch (e) { console.log('T twice ' + e.name); }
         })();",
        no_answer,
    );
    assert_eq!(
        l,
        [
            "a,b start,pull",
            "closed",
            "errored boom",
            "cancelled because",
            "locked? false",
            "tee 11",
            "iter p,q",
            "twice TypeError",
        ]
    );
}

#[test]
fn send_beacon_and_lazy_globals() {
    let r = run(
        "console.log('T ' + typeof navigator.sendBeacon + ' ' + ('fetch' in window) + ' ' + (typeof fetch) + ' ' + (window.fetch === fetch) + ' ' + Object.getOwnPropertyDescriptor(window, 'fetch').writable);
         console.log('T ' + navigator.sendBeacon('/log', 'event=1') + ' ' + navigator.sendBeacon('/log2', new Blob(['x'], {type: 'text/x'})) + ' ' + navigator.sendBeacon('/log3'));
         console.log('T big ' + navigator.sendBeacon('/log4', new Uint8Array(70000)));
         try { navigator.sendBeacon('ftp://x/'); } catch (e) { console.log('T ftp ' + e.name); }
         try { navigator.sendBeacon(); } catch (e) { console.log('T none ' + e.name); }
         const mine = () => 'mine'; window.fetch = mine; console.log('T replaced ' + (fetch === mine) + ' ' + fetch());
         window.Headers = 5; console.log('T assigned ' + Headers);
         delete window.URL; console.log('T deleted ' + typeof URL);
         console.log('T others ' + [typeof XMLHttpRequest, typeof Request, typeof AbortController, typeof FormData, typeof DOMException, typeof URLSearchParams].join());",
        no_answer,
    );
    let t = t_lines(&r);
    assert_eq!(t[0], "function true function true true");
    assert_eq!(t[1], "true true true");
    assert_eq!(t[2], "big false");
    for want in ["ftp TypeError", "none TypeError", "replaced true mine", "assigned 5", "deleted undefined", "others function,function,function,function,function,function"] {
        assert!(t.contains(&want), "missing {want:?} in {:?}", r.lines);
    }
    let q = &r.requests;
    assert_eq!(q.len(), 3);
    assert_eq!(q[0].kind, NetKind::Beacon);
    assert_eq!(q[0].method, "POST");
    assert_eq!(q[0].body, b"event=1");
    assert_eq!(header(&q[0], "content-type"), Some("text/plain;charset=UTF-8"));
    assert_eq!(header(&q[1], "content-type"), Some("text/x"));
    assert_eq!(q[0].credentials, CredentialsMode::Include);
}

// ---- hostile pages and hostile answers ----------------------------------------------------------

#[test]
fn limits_hold_and_nothing_panics() {
    let r = run(
        "let refused = 0, started = 0;
         for (let i = 0; i < 400; i++) fetch('/n' + i).catch(() => {});
         console.log('T pending-capped');
         const part = new Uint8Array(20 << 20);
         fetch('/big', {method: 'POST', body: new Blob([part, part])}).catch(e => console.log('T big ' + e.message));
         const longUrl = '/' + 'a'.repeat(70000); fetch(longUrl).catch(e => console.log('T long ' + e.message));
         const hs = {}; for (let i = 0; i < 400; i++) hs['x-' + i] = 'v'.repeat(300); fetch('/h', {headers: hs}).catch(() => {});
         fetch('/ok', {method: 'bad method'}).catch(e => console.log('T method ' + e.name));
         new XMLHttpRequest().abort(); new XMLHttpRequest().getAllResponseHeaders();
         try { new Blob([new ArrayBuffer(1 << 20)]).slice(0, 1 << 30).size; } catch (e) {}
         try { for (let i = 0; i < 80; i++) new Blob([new Uint8Array(1 << 20)]); } catch (e) { console.log('T quota ' + e.name); }
         console.log('T done');",
        no_answer,
    );
    let started = r.requests.len();
    assert!(started <= 256 + 1, "at most {} outstanding: {started}", 256);
    assert!(r.lines.iter().any(|l| l == "T done"), "{:?}", &r.lines[..r.lines.len().min(5)]);
    assert!(r.lines.iter().any(|l| l.starts_with("T big Failed to fetch")), "{:?}", r.lines.iter().filter(|l| l.starts_with("T")).collect::<Vec<_>>());
    assert!(r.lines.iter().any(|l| l.contains("The request's body is too big")));
    assert!(r.lines.iter().any(|l| l.contains("Too many requests are outstanding")));
}

#[test]
fn answers_out_of_order_twice_or_for_nobody_are_ignored() {
    let mut r = run(
        "fetch('/a').then(r => r.text()).then(t => console.log('T got ' + t), e => console.log('T err ' + e.message));
         const x = new XMLHttpRequest(); x.open('GET', '/b'); x.onload = () => console.log('T xhr ' + x.responseText); x.send();",
        no_answer,
    );
    let mut h = Host::new();
    let ids: Vec<u32> = r.requests.iter().map(|q| q.id).collect();
    let mut hd = head(200, "OK", &[]);
    hd.url = String::from("http://www.milk.test/a");
    let d = &mut r.doc;
    // For an id nobody made, a Done before a Head, a second Head, a Progress after Done.
    assert!(!d.net_event(&mut h, 9999, NetEvent::Done(alloc::vec![1])));
    d.net_event(&mut h, ids[0], NetEvent::Progress { loaded: 1, total: None });
    d.net_event(&mut h, ids[0], NetEvent::Head(hd.clone()));
    d.net_event(&mut h, ids[0], NetEvent::Head(hd.clone()));
    d.net_event(&mut h, ids[0], NetEvent::Done(b"one".to_vec()));
    d.net_event(&mut h, ids[0], NetEvent::Done(b"two".to_vec()));
    d.net_event(&mut h, ids[0], NetEvent::Failed(NetFailure { kind: NetFailureKind::Network, message: String::from("late") }));
    // The XHR: a Done with no Head at all.
    d.net_event(&mut h, ids[1], NetEvent::Done(b"direct".to_vec()));
    d.net_event(&mut h, ids[1], NetEvent::Head(hd));
    let lines: Vec<String> = d.take_console().into_iter().map(|l| l.text).collect();
    assert!(lines.iter().any(|l| l == "T got one"), "{lines:?}");
    assert!(lines.iter().any(|l| l == "T xhr direct"), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("two") || l.contains("late")), "{lines:?}");
    assert_eq!(d.net_pending(), 0);
}

#[test]
fn stopped_scripts_cancel_everything() {
    let mut r = run("for (let i = 0; i < 5; i++) fetch('/w' + i); const x = new XMLHttpRequest(); x.open('GET', '/x'); x.send();", no_answer);
    assert_eq!(r.pending, 6);
    r.doc.stop_scripts();
    let outs = r.doc.take_net();
    let cancels = outs.iter().filter(|o| matches!(o, NetOut::Cancel(_))).count();
    assert_eq!(cancels, 6, "{outs:?}");
    assert_eq!(r.doc.net_pending(), 0);
    let mut h = Host::new();
    assert!(!r.doc.net_event(&mut h, 1, NetEvent::Done(alloc::vec![])), "answers after the stop are dropped");
}

#[test]
fn garbage_in_garbage_out_is_still_bounded() {
    // A deterministic fuzz of the page-facing parsers from script: whatever goes in, only
    // exceptions come out.
    let r = run(
        "let seed = 12345; const rnd = () => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
         const chars = ['a', 'b', ':', '/', '?', '#', '@', '[', ']', '%', '%4', '%zz', ' ', '\\\\', '.', '..', '\\t', '\\u0000', 'é', '\\ud83d', '\\u2028', '=', '&', ';', ',', 'http', 'https://', '//', '0', '65536', 'x'.repeat(100)];
         const word = () => { let s = ''; const n = Math.floor(rnd() * 12); for (let i = 0; i < n; i++) s += chars[Math.floor(rnd() * chars.length)]; return s; };
         let ok = 0, threw = 0;
         const t = (f) => { try { f(); ok++; } catch (e) { threw++; if (!(e instanceof Error)) throw e; } };
         for (let i = 0; i < 600; i++) {
           const a = word(), b = word();
           t(() => new URL(a)); t(() => new URL(a, 'http://base.test/p')); t(() => new URL(a, b));
           t(() => { const u = new URL('http://x.test/p?q#h'); u.pathname = a; u.search = a; u.hash = b; u.host = a; u.hostname = b; u.port = a; u.protocol = b; u.username = a; u.password = b; u.searchParams.append(a, b); String(u); });
           t(() => new URLSearchParams(a).toString()); t(() => new Headers({[a]: b})); t(() => new Headers().append(a, b));
           t(() => new Request(a, {method: b})); t(() => new Request('/x', {headers: {[a]: b}})); t(() => new Response(a, {status: i, statusText: b}));
           t(() => new Blob([a, b], {type: a}).slice(i - 300, i).size); t(() => new File([a], b)); t(() => { const f = new FormData(); f.append(a, b); f.set(a, b); f.get(b); f.delete(a); });
           t(() => fetch(a).catch(() => {})); t(() => { const x = new XMLHttpRequest(); x.open(b, a); x.setRequestHeader(a, b); x.send(a); x.abort(); });
           t(() => navigator.sendBeacon(a, b)); t(() => URL.createObjectURL(a)); t(() => fetch('data:' + a).catch(() => {})); t(() => fetch('blob:' + a).catch(() => {}));
         }
         console.log('T fuzz ' + (ok > 1000) + ' ' + (threw > 100));",
        no_answer,
    );
    assert!(r.lines.iter().any(|l| l == "T fuzz true true"), "{:?}", r.lines.iter().take(5).collect::<Vec<_>>());
}

#[test]
fn url_parser_never_panics() {
    use crate::script::net_for_tests::parse_url;
    let mut seed = 0x1234_5678u64;
    let mut next = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as u32
    };
    let pieces = ["http", "https", "://", ":", "/", "//", "?", "#", "@", "[", "]", "%", "%2e", "..", ".", "\\", " ", "é", "😀", "a", "0", "65535", "99999", "\t", "\n", "::1", "user:pw@", "x.test", "-", "_", "~", "\u{0}"];
    for _ in 0..20_000 {
        let n = (next() % 14) as usize;
        let mut s = String::new();
        for _ in 0..n {
            s.push_str(pieces[(next() as usize) % pieces.len()]);
        }
        let base = if next() % 2 == 0 { Some("http://a.test/dir/page?q#f") } else { None };
        if let Some(u) = parse_url(&s, base) {
            // What it gives parses back to the same thing.
            let again = parse_url(&u.href(), None).expect("a serialized address parses");
            assert_eq!(again.href(), u.href(), "{s:?}");
        }
    }
}

// ---- window.onerror, unhandledrejection, memory -------------------------------------------------

#[test]
fn onerror_and_unhandled_rejections() {
    let r = run(
        "window.addEventListener('error', e => console.log('T listener ' + e.message + ' ' + e.lineno + ' ' + (e.error && e.error.message) + ' ' + (e instanceof ErrorEvent)));
         window.onerror = function (msg, src, line, col, err) { console.log('T onerror ' + msg + ' ' + (line > 0) + ' ' + err.name); return true; };
         window.addEventListener('unhandledrejection', e => { console.log('T unhandled ' + e.reason.message + ' ' + (e.promise instanceof Promise) + ' ' + (e instanceof PromiseRejectionEvent)); e.preventDefault(); });
         window.addEventListener('rejectionhandled', e => console.log('T handled later'));
         Promise.reject(new Error('nope'));
         const late = Promise.reject(new Error('late'));
         setTimeout(() => late.catch(() => {}), 5);
         setTimeout(function () { throw new TypeError('in timer'); }, 5);
         queueMicrotask(() => { throw new Error('in microtask'); });
         throw new Error('top');",
        no_answer,
    );
    let t = t_lines(&r);
    for want in [
        "onerror Uncaught Error: top true Error",
        "onerror Uncaught Error: in microtask true Error",
        "onerror Uncaught TypeError: in timer true TypeError",
        "unhandled nope true true",
        "unhandled late true true",
        "handled later",
    ] {
        assert!(t.contains(&want), "missing {want:?} in {:?}", r.lines);
    }
    assert!(t.iter().any(|l| l.starts_with("listener Uncaught Error: top")), "{:?}", r.lines);
    // A handled error leaves the console quiet.
    assert!(!r.lines.iter().any(|l| l.starts_with("Uncaught")), "{:?}", r.lines);
    // Without handlers they are reported.
    let r = run("Promise.reject(new Error('x')); throw new RangeError('y');", no_answer);
    assert!(r.lines.iter().any(|l| l.starts_with("Uncaught RangeError: y")), "{:?}", r.lines);
    assert!(r.lines.iter().any(|l| l.starts_with("Uncaught (in promise) Error: x")), "{:?}", r.lines);
}

#[test]
fn byte_buffers_do_not_pile_up() {
    // 120 responses of 2 MB read as text and dropped, 240 MB in all, through the byte store
    // (96 MB); the collector gives the dead ones back between tasks.
    let r = run_with(
        "let n = 0, errors = 0;
         function next() {
           if (n >= 120) { console.log('T finished ' + n + ' errors ' + errors); return; }
           n++;
           fetch('/chunk' + n).then(r => r.text()).then(t => { if (t.length !== 2000000) errors++; next(); }, () => { errors++; next(); });
         }
         next();
         let blobs = 0;
         const iv = setInterval(() => { try { new Blob([new Uint8Array(1 << 20)]); } catch (e) { errors++; } if (++blobs >= 150) clearInterval(iv); }, 1);",
        400,
        |req| {
            let mut v = reply(req, 200, &[], "");
            v[1].ev = NetEvent::Done(alloc::vec![b'a'; 2_000_000]);
            v
        },
    );
    assert!(r.lines.iter().any(|l| l == "T finished 120 errors 0"), "{:?}", r.lines.iter().rev().take(4).collect::<Vec<_>>());
}
