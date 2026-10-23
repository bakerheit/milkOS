//! Host tests: the HTTP client against scripted fake servers, DNS messages built by hand and
//! a fake name server, gzip, URLs and the HTML subset. Nothing here touches a network.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use std::cell::RefCell;
use std::rc::Rc;

use crate::dns::{self, Addr, Cache, Cached, DnsError, Host, TYPE_A, TYPE_AAAA, TYPE_CNAME, Want};
use crate::html::{self, BlockKind};
use crate::http::{self, Error, Net, Options, Transport};
use crate::url::{Scheme, Url, UrlError};
use crate::{gzip, inflate};

// ---- a fake network -----------------------------------------------------------------------

/// What one connection to a server does: the bytes it answers with, in the pieces it sends
/// them (each `read` returns at most one piece), then end of stream (or a stall).
#[derive(Clone)]
struct Script {
    pieces: Vec<Vec<u8>>,
    stall: bool,
}

fn answer(text: &[u8]) -> Script {
    Script {
        pieces: vec![text.to_vec()],
        stall: false,
    }
}

fn pieces(p: &[&[u8]]) -> Script {
    Script {
        pieces: p.iter().map(|x| x.to_vec()).collect(),
        stall: false,
    }
}

#[derive(Default)]
struct Log {
    /// (host, port) of every connect.
    connects: Vec<(String, u16)>,
    /// Every request, as sent.
    requests: Vec<String>,
}

struct FakeConn {
    pieces: VecDeque<Vec<u8>>,
    stall: bool,
    log: Rc<RefCell<Log>>,
    clock: Rc<RefCell<u64>>,
    sent: Vec<u8>,
}

impl Transport for FakeConn {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        match self.pieces.front_mut() {
            Some(p) => {
                let n = p.len().min(buf.len());
                buf[..n].copy_from_slice(&p[..n]);
                p.drain(..n);
                if p.is_empty() {
                    self.pieces.pop_front();
                }
                Ok(n)
            }
            None if self.stall => {
                *self.clock.borrow_mut() += 5_000;
                Err(Error::Timeout)
            }
            None => Ok(0),
        }
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        self.sent.extend_from_slice(buf);
        // A whole request: the head, and its body if it says it has one.
        let text = String::from_utf8_lossy(&self.sent).into_owned();
        if let Some(end) = text.find("\r\n\r\n") {
            let len = text[..end]
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .and_then(|n| n.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if self.sent.len() >= end + 4 + len {
                self.log.borrow_mut().requests.push(text);
                self.sent.clear();
            }
        }
        Ok(buf.len())
    }
}

/// Servers by host: each connect takes the next script for that host.
struct FakeNet {
    servers: Vec<(String, VecDeque<Script>)>,
    log: Rc<RefCell<Log>>,
    clock: Rc<RefCell<u64>>,
}

impl FakeNet {
    fn new() -> FakeNet {
        FakeNet {
            servers: Vec::new(),
            log: Rc::new(RefCell::new(Log::default())),
            clock: Rc::new(RefCell::new(1000)),
        }
    }

    fn serve(mut self, host: &str, s: Script) -> FakeNet {
        match self.servers.iter_mut().find(|(h, _)| h == host) {
            Some((_, q)) => q.push_back(s),
            None => self.servers.push((host.to_string(), VecDeque::from([s]))),
        }
        self
    }
}

impl Net for FakeNet {
    fn connect(&mut self, host: &str, port: u16, _t: u64) -> Result<Box<dyn Transport>, Error> {
        self.log
            .borrow_mut()
            .connects
            .push((host.to_string(), port));
        let Some((_, q)) = self.servers.iter_mut().find(|(h, _)| h == host) else {
            return Err(Error::NotFound(host.to_string()));
        };
        let Some(s) = q.pop_front() else {
            return Err(Error::Connect(alloc::format!("{host}: connection refused")));
        };
        Ok(Box::new(FakeConn {
            pieces: s.pieces.into(),
            stall: s.stall,
            log: self.log.clone(),
            clock: self.clock.clone(),
            sent: Vec::new(),
        }))
    }

    fn now_ms(&mut self) -> u64 {
        *self.clock.borrow()
    }
}

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn get(net: &mut FakeNet, u: &str) -> Result<http::Response, http::Failure> {
    http::get(net, &url(u), &Options::default())
}

fn gz(data: &[u8]) -> Vec<u8> {
    // A gzip member with stored (uncompressed) deflate blocks: valid, and easy to build here.
    let mut o = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
    o.extend(stored_deflate(data));
    o.extend(inflate::crc32(&[data]).to_le_bytes());
    o.extend((data.len() as u32).to_le_bytes());
    o
}

fn stored_deflate(data: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    let chunks: Vec<&[u8]> = if data.is_empty() {
        vec![&[][..]]
    } else {
        data.chunks(65535).collect()
    };
    for (i, c) in chunks.iter().enumerate() {
        o.push(u8::from(i + 1 == chunks.len()));
        o.extend((c.len() as u16).to_le_bytes());
        o.extend((!(c.len() as u16)).to_le_bytes());
        o.extend_from_slice(c);
    }
    o
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut o = vec![0x78, 0x01];
    o.extend(stored_deflate(data));
    o.extend(inflate::adler32(data).to_be_bytes());
    o
}

// A real gzip (zlib's output for "hello hello hello hello\n", made with
// python3 -c "import gzip; print(gzip.compress(b'hello hello hello hello\n', mtime=0))").
const HELLO_GZ: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57,
    0xc8, 0x40, 0x27, 0xb9, 0x00, 0x00, 0x88, 0x59, 0x0b, 0x18, 0x00, 0x00, 0x00,
];

// ---- HTTP -----------------------------------------------------------------------------------

#[test]
fn http_content_length_and_request() {
    let mut net = FakeNet::new().serve(
        "example.test",
        answer(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\n\r\nhelloEXTRA",
        ),
    );
    let r = get(&mut net, "http://Example.TEST/a?b=1").unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.body, b"hello");
    assert_eq!(r.content_type(), "text/html");
    let log = net.log.borrow();
    assert_eq!(log.connects, vec![("example.test".to_string(), 80)]);
    let req = &log.requests[0];
    assert!(req.starts_with("GET /a?b=1 HTTP/1.1\r\n"), "{req}");
    assert!(req.contains("\r\nHost: example.test\r\n"));
    assert!(req.contains("\r\nUser-Agent: milkOS/0.1\r\n"));
    assert!(req.contains("\r\nAccept-Encoding: gzip, deflate\r\n"));
    assert!(req.contains("\r\nConnection: close\r\n"));
}

#[test]
fn http_host_header_port_and_v6() {
    let mut net = FakeNet::new().serve("::1", answer(b"HTTP/1.0 200 OK\r\n\r\nbody to the end"));
    let r = get(&mut net, "http://[::1]:8080/").unwrap();
    assert_eq!(r.body, b"body to the end");
    assert!(net.log.borrow().requests[0].contains("Host: [::1]:8080\r\n"));
    assert_eq!(net.log.borrow().connects[0], ("::1".to_string(), 8080));
}

#[test]
fn http_chunked_in_pieces() {
    let mut net = FakeNet::new().serve(
        "h.test",
        pieces(&[
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n",
            b"\r\n5;ext=1\r\nhel",
            b"lo\r\n",
            b"7\r\n, world\r\n0\r\nX-Trailer: yes\r\n\r\n",
        ]),
    );
    let r = get(&mut net, "http://h.test/").unwrap();
    assert_eq!(r.body, b"hello, world");
}

#[test]
fn http_chunked_errors() {
    for bad in [
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nhello\r\n0\r\n\r\n"[..],
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nhello\r\n0\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel",
    ] {
        let mut net = FakeNet::new().serve("h.test", answer(bad));
        let e = get(&mut net, "http://h.test/").unwrap_err().error;
        assert!(matches!(e, Error::Malformed(_)), "{e:?}");
    }
}

#[test]
fn http_chunked_gzip() {
    let body = gz(b"<p>Zipped and chunked</p>");
    let (a, b) = body.split_at(10);
    let mut resp =
        b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    resp.extend(alloc::format!("{:x}\r\n", a.len()).bytes());
    resp.extend(a);
    resp.extend(alloc::format!("\r\n{:X}\r\n", b.len()).bytes());
    resp.extend(b);
    resp.extend(b"\r\n0\r\n\r\n");
    let mut net = FakeNet::new().serve("h.test", answer(&resp));
    let r = get(&mut net, "http://h.test/").unwrap();
    assert_eq!(r.body, b"<p>Zipped and chunked</p>");
    assert_eq!(r.encoding, Some("gzip"));
    assert_eq!(r.wire_bytes, body.len());
}

#[test]
fn http_real_gzip_and_deflate() {
    let mut resp = alloc::format!(
        "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
        HELLO_GZ.len()
    )
    .into_bytes();
    resp.extend(HELLO_GZ);
    let mut net = FakeNet::new().serve("h.test", answer(&resp));
    assert_eq!(
        get(&mut net, "http://h.test/").unwrap().body,
        b"hello hello hello hello\n"
    );
    // deflate: zlib-wrapped, and raw.
    for enc in [zlib(b"deflated"), stored_deflate(b"deflated")] {
        let mut resp = b"HTTP/1.1 200 OK\r\nContent-Encoding: deflate\r\n\r\n".to_vec();
        resp.extend(enc);
        let mut net = FakeNet::new().serve("h.test", answer(&resp));
        let r = get(&mut net, "http://h.test/").unwrap();
        assert_eq!(r.body, b"deflated");
        assert_eq!(r.encoding, Some("deflate"));
    }
}

#[test]
fn http_bad_gzip() {
    let mut body = gz(b"some text");
    let n = body.len();
    body[n - 8] ^= 1; // CRC
    let mut resp = b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\n".to_vec();
    resp.extend(body);
    let mut net = FakeNet::new().serve("h.test", answer(&resp));
    let e = get(&mut net, "http://h.test/").unwrap_err().error;
    assert!(matches!(e, Error::Decode(_)), "{e:?}");
    let mut resp = b"HTTP/1.1 200 OK\r\nContent-Encoding: br\r\n\r\nxx".to_vec();
    resp.truncate(resp.len());
    let mut net = FakeNet::new().serve("h.test", answer(&resp));
    assert!(matches!(
        get(&mut net, "http://h.test/").unwrap_err().error,
        Error::Decode(_)
    ));
}

#[test]
fn http_redirect_chain() {
    let mut net = FakeNet::new()
        .serve("a.test", answer(b"HTTP/1.1 301 Moved\r\nLocation: http://b.test/one\r\nContent-Length: 0\r\n\r\n"))
        .serve("b.test", answer(b"HTTP/1.1 302 Found\r\nLocation: two?x=1\r\n\r\n"))
        .serve("b.test", answer(b"HTTP/1.1 303 See Other\r\nLocation: //c.test:8080/three\r\nContent-Length: 3\r\n\r\nabc"))
        .serve("c.test", answer(b"HTTP/1.1 307 Temporary\r\nLocation: /four#frag\r\n\r\n"))
        .serve("c.test", answer(b"HTTP/1.1 308 Permanent\r\nLocation: ../five\r\n\r\n"))
        .serve("c.test", answer(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndone"));
    let r = get(&mut net, "http://a.test/").unwrap();
    assert_eq!(r.body, b"done");
    assert_eq!(r.url.to_text(), "http://c.test:8080/five");
    let shown: Vec<String> = r.redirects.iter().map(Url::to_text).collect();
    assert_eq!(
        shown,
        [
            "http://b.test/one",
            "http://b.test/two?x=1",
            "http://c.test:8080/three",
            "http://c.test:8080/four",
            "http://c.test:8080/five"
        ]
    );
    let reqs = &net.log.borrow().requests;
    assert!(reqs[4].starts_with("GET /four HTTP/1.1\r\nHost: c.test:8080\r\n"));
}

#[test]
fn http_redirect_limits() {
    // A loop.
    let mut net = FakeNet::new()
        .serve("a.test", answer(b"HTTP/1.1 302 x\r\nLocation: /b\r\n\r\n"))
        .serve("a.test", answer(b"HTTP/1.1 302 x\r\nLocation: /\r\n\r\n"));
    let f = get(&mut net, "http://a.test/").unwrap_err();
    assert_eq!(f.error, Error::RedirectLoop("http://a.test/".into()));
    assert_eq!(f.url.to_text(), "http://a.test/b");
    // Eleven distinct redirects: one too many.
    let mut net = FakeNet::new();
    for k in 0..12 {
        let r = alloc::format!("HTTP/1.1 301 x\r\nLocation: /p{}\r\n\r\n", k + 1);
        net = net.serve("a.test", answer(r.as_bytes()));
    }
    let f = get(&mut net, "http://a.test/p0").unwrap_err();
    assert_eq!(f.error, Error::TooManyRedirects(10));
    assert_eq!(net.log.borrow().connects.len(), 11);
    // Ten is fine.
    let mut net = FakeNet::new();
    for k in 0..10 {
        let r = alloc::format!("HTTP/1.1 301 x\r\nLocation: /p{}\r\n\r\n", k + 1);
        net = net.serve("a.test", answer(r.as_bytes()));
    }
    net = net.serve("a.test", answer(b"HTTP/1.1 200 OK\r\n\r\nok"));
    assert_eq!(
        get(&mut net, "http://a.test/p0").unwrap().redirects.len(),
        10
    );
    // A 3xx without Location is just a response.
    let mut net = FakeNet::new().serve("a.test", answer(b"HTTP/1.1 304 Not Modified\r\n\r\n"));
    assert_eq!(get(&mut net, "http://a.test/").unwrap().status, 304);
}

#[test]
fn http_https_hook() {
    // No TLS: refused before any connection.
    let mut net = FakeNet::new().serve("a.test", answer(b"HTTP/1.1 200 OK\r\n\r\n"));
    let f = get(&mut net, "https://a.test/").unwrap_err();
    assert_eq!(f.error, Error::SecureUnsupported);
    assert!(net.log.borrow().connects.is_empty());
    // A redirect to https: the failure says where it was going.
    let mut net = FakeNet::new().serve(
        "a.test",
        answer(b"HTTP/1.1 301 x\r\nLocation: https://a.test/\r\n\r\n"),
    );
    let f = get(&mut net, "http://a.test/").unwrap_err();
    assert_eq!(f.error, Error::SecureUnsupported);
    assert_eq!(f.url.to_text(), "https://a.test/");
    assert_eq!(f.url.scheme, Scheme::Https);
    assert!(f.error.title().contains("Secure pages"));

    // With a TLS layer (here a pass-through), https goes through `secure` on port 443.
    struct Tls(FakeNet, Rc<RefCell<Vec<String>>>);
    impl Net for Tls {
        fn connect(&mut self, h: &str, p: u16, t: u64) -> Result<Box<dyn Transport>, Error> {
            self.0.connect(h, p, t)
        }
        fn now_ms(&mut self) -> u64 {
            self.0.now_ms()
        }
        fn has_tls(&self) -> bool {
            true
        }
        fn secure(
            &mut self,
            host: &str,
            s: Box<dyn Transport>,
        ) -> Result<Box<dyn Transport>, Error> {
            self.1.borrow_mut().push(host.to_string());
            Ok(s)
        }
    }
    let wrapped = Rc::new(RefCell::new(Vec::new()));
    let mut t = Tls(
        FakeNet::new().serve(
            "s.test",
            answer(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecret"),
        ),
        wrapped.clone(),
    );
    let r = http::get(&mut t, &url("https://s.test/"), &Options::default()).unwrap();
    assert_eq!(r.body, b"secret");
    assert_eq!(*wrapped.borrow(), ["s.test"]);
    assert_eq!(t.0.log.borrow().connects[0], ("s.test".to_string(), 443));
}

#[test]
fn http_limits() {
    let o = Options {
        max_body: 10,
        max_header: 100,
        ..Options::default()
    };
    let mut net = FakeNet::new().serve(
        "a.test",
        answer(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nhello world"),
    );
    assert_eq!(
        http::get(&mut net, &url("http://a.test/"), &o)
            .unwrap_err()
            .error,
        Error::TooBig(10)
    );
    let mut net = FakeNet::new().serve(
        "a.test",
        answer(b"HTTP/1.1 200 OK\r\n\r\nhello world, to the end"),
    );
    assert_eq!(
        http::get(&mut net, &url("http://a.test/"), &o)
            .unwrap_err()
            .error,
        Error::TooBig(10)
    );
    let mut net = FakeNet::new().serve(
        "a.test",
        answer(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nhello \r\n6\r\nworld!\r\n0\r\n\r\n"),
    );
    assert_eq!(
        http::get(&mut net, &url("http://a.test/"), &o)
            .unwrap_err()
            .error,
        Error::TooBig(10)
    );
    let big = alloc::format!("HTTP/1.1 200 OK\r\nX-Big: {}\r\n\r\n", "x".repeat(200));
    let mut net = FakeNet::new().serve("a.test", answer(big.as_bytes()));
    assert_eq!(
        http::get(&mut net, &url("http://a.test/"), &o)
            .unwrap_err()
            .error,
        Error::TooBig(100)
    );
    // A gzip bomb: small on the wire, over the limit unpacked.
    let mut resp = b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\n".to_vec();
    resp.extend(gz(&[b'a'; 64]));
    let o2 = Options {
        max_body: 70,
        ..Options::default()
    };
    let mut net = FakeNet::new().serve("a.test", answer(&resp));
    // 64 bytes of data in 64 + 18 + 5 bytes of gzip: over 70 on the wire already.
    assert!(matches!(
        http::get(&mut net, &url("http://a.test/"), &o2)
            .unwrap_err()
            .error,
        Error::TooBig(_)
    ));
    let mut resp = b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\n".to_vec();
    resp.extend(HELLO_GZ);
    let o3 = Options {
        max_body: 20,
        ..Options::default()
    };
    let mut net = FakeNet::new().serve("a.test", answer(&resp));
    assert_eq!(
        http::get(&mut net, &url("http://a.test/"), &o3)
            .unwrap_err()
            .error,
        Error::TooBig(20)
    );
}

#[test]
fn http_timeout() {
    // The server sends the head, then nothing: the transport times out.
    let mut net = FakeNet::new().serve(
        "a.test",
        Script {
            pieces: vec![b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nsome".to_vec()],
            stall: true,
        },
    );
    assert_eq!(
        get(&mut net, "http://a.test/").unwrap_err().error,
        Error::Timeout
    );
    // The overall deadline: a server that dribbles one byte per read while the clock runs.
    struct Slow(FakeNet);
    impl Net for Slow {
        fn connect(&mut self, h: &str, p: u16, t: u64) -> Result<Box<dyn Transport>, Error> {
            self.0.connect(h, p, t)
        }
        fn now_ms(&mut self) -> u64 {
            *self.0.clock.borrow_mut() += 1000;
            self.0.now_ms()
        }
    }
    let body: Vec<&[u8]> = core::iter::once(&b"HTTP/1.1 200 OK\r\n\r\n"[..])
        .chain(core::iter::repeat_n(&b"x"[..], 100))
        .collect();
    let mut s = Slow(FakeNet::new().serve("a.test", pieces(&body)));
    let o = Options {
        timeout_ms: 20_000,
        ..Options::default()
    };
    assert_eq!(
        http::get(&mut s, &url("http://a.test/"), &o)
            .unwrap_err()
            .error,
        Error::Timeout
    );
}

#[test]
fn http_malformed() {
    for (bad, what) in [
        (&b"SSH-2.0-OpenSSH\r\n\r\n"[..], "status line"),
        (b"HTTP/1.1 2000 OK\r\n\r\n", "status code"),
        (b"HTTP/1.1 abc OK\r\n\r\n", "status code"),
        (b"HTTP/1.1 200 OK\r\nno colon here\r\n\r\n", "colon"),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\nhello!",
            "Content-Length",
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
            "Content-Length",
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort",
            "shorter",
        ),
        (b"HTTP/1.1 200 OK\r\nContent-Le", "middle of a line"),
        (b"", "without answering"),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked, br\r\n\r\n",
            "transfer encoding",
        ),
    ] {
        let mut net = FakeNet::new().serve("a.test", answer(bad));
        let e = get(&mut net, "http://a.test/").unwrap_err().error;
        match &e {
            Error::Malformed(m) => assert!(m.contains(what), "{what}: {m}"),
            e => panic!("{what}: {e:?}"),
        }
    }
    // Lenient where it is harmless: obs-fold, 100 Continue first, bare LF line ends.
    let mut net = FakeNet::new().serve(
        "a.test",
        answer(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\nX-Long: one\n  two\nContent-Length: 2\n\nok"),
    );
    let r = get(&mut net, "http://a.test/").unwrap();
    assert_eq!(r.body, b"ok");
    assert_eq!(r.header("x-long"), Some("one two"));
}

#[test]
fn http_keep_alive_redirect_reuses() {
    let o = Options {
        keep_alive: true,
        ..Options::default()
    };
    let mut net = FakeNet::new().serve(
        "a.test",
        answer(b"HTTP/1.1 301 x\r\nLocation: /next\r\nContent-Length: 0\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"),
    );
    let r = http::get(&mut net, &url("http://a.test/"), &o).unwrap();
    assert_eq!(r.body, b"ok");
    assert_eq!(net.log.borrow().connects.len(), 1);
    assert!(net.log.borrow().requests[1].contains("Connection: keep-alive"));
}

#[test]
fn http_errors_read_well() {
    assert_eq!(
        Error::NotFound("nope.test".into()).title(),
        "Server not found"
    );
    assert!(
        Error::Offline("example.com".into())
            .to_string()
            .contains("offline mode")
    );
    let mut net = FakeNet::new();
    let f = get(&mut net, "http://missing.test/").unwrap_err();
    assert_eq!(f.error, Error::NotFound("missing.test".into()));
}

// ---- URLs -------------------------------------------------------------------------------------

#[test]
fn urls_parse() {
    let u = url("example.com");
    assert_eq!(
        (u.scheme, u.host.as_str(), u.port, u.path.as_str()),
        (Scheme::Http, "example.com", 80, "/")
    );
    assert_eq!(
        url("HTTPS://Example.COM./x#top").to_text(),
        "https://example.com/x"
    );
    assert_eq!(url("localhost:8080").to_text(), "http://localhost:8080/");
    assert_eq!(
        url("10.0.2.2:8000/a b").to_text(),
        "http://10.0.2.2:8000/a%20b"
    );
    assert_eq!(url(concat!("http://u:p", "@", "h.test/")).host, "h.test");
    assert_eq!(url("http://h.test?q=1").path, "/?q=1");
    assert_eq!(url("http://[fe80::1]:81/").host, "fe80::1");
    assert_eq!(url("http://h.test/a/./b/../c").path, "/a/c");
    assert_eq!(url("http://h.test:443/").to_text(), "http://h.test:443/");
    assert!(
        url("10.0.2.2").is_ip_literal()
            && url("[::1]").is_ip_literal()
            && !url("a.test").is_ip_literal()
    );
    assert_eq!(Url::parse("ftp://x/"), Err(UrlError::Scheme("ftp".into())));
    assert_eq!(
        Url::parse(concat!("mailto:me", "@", "x.test")),
        Err(UrlError::Scheme("mailto".into()))
    );
    assert_eq!(
        Url::parse("http://h.test:99999/"),
        Err(UrlError::BadPort("99999".into()))
    );
    assert_eq!(Url::parse("http:///x"), Err(UrlError::NoHost));
    assert!(matches!(
        Url::parse("http://bad host/"),
        Err(UrlError::BadHost(_))
    ));
    assert_eq!(Url::parse("  "), Err(UrlError::Empty));
}

#[test]
fn urls_join() {
    let base = url("http://h.test/dir/page.html?x=1");
    let j = |s: &str| base.join(s).unwrap().to_text();
    assert_eq!(j("other.html"), "http://h.test/dir/other.html");
    assert_eq!(j("../up.html"), "http://h.test/up.html");
    assert_eq!(j("../../../root"), "http://h.test/root");
    assert_eq!(j("/abs"), "http://h.test/abs");
    assert_eq!(j("?y=2"), "http://h.test/dir/page.html?y=2");
    assert_eq!(j("#frag"), "http://h.test/dir/page.html?x=1");
    assert_eq!(j("//other.test/p"), "http://other.test/p");
    assert_eq!(j("https://s.test/"), "https://s.test/");
    assert_eq!(j("./"), "http://h.test/dir/");
    assert_eq!(j("sub/"), "http://h.test/dir/sub/");
    assert!(base.join("javascript:void(0)").is_err());
    assert!(base.join("mailto:x@y").is_err());
}

#[test]
fn urls_search() {
    assert!(html_search("milk"));
    assert!(html_search("how are cows"));
    assert!(!html_search("example.com"));
    assert!(!html_search("localhost"));
    assert!(!html_search("10.0.2.2:8000"));
    assert!(!html_search("http://x/"));
    assert_eq!(
        crate::url::search_url("milk & cookies"),
        "http://lite.duckduckgo.com/lite/?q=milk+%26+cookies"
    );
}

fn html_search(s: &str) -> bool {
    crate::url::looks_like_search(s)
}

// ---- gzip -------------------------------------------------------------------------------------

#[test]
fn gzip_members_and_headers() {
    let mut two = gz(b"one ");
    two.extend(gz(b"two"));
    assert_eq!(gzip::gunzip(&two, 100).unwrap(), b"one two");
    // FEXTRA + FNAME + FCOMMENT + FHCRC.
    let mut g = vec![
        0x1f,
        0x8b,
        8,
        4 | 8 | 16 | 2,
        0,
        0,
        0,
        0,
        0,
        3,
        3,
        0,
        b'a',
        b'b',
        b'c',
    ];
    g.extend(b"name.txt\0comment\0");
    g.extend([0, 0]);
    g.extend(stored_deflate(b"payload"));
    g.extend(inflate::crc32(&[b"payload"]).to_le_bytes());
    g.extend(7u32.to_le_bytes());
    assert_eq!(gzip::gunzip(&g, 100).unwrap(), b"payload");
    assert_eq!(
        gzip::gunzip(b"\x1f\x8c........", 100),
        Err(gzip::GzipError::NotGzip)
    );
    let full = gz(b"truncate me");
    assert_eq!(
        gzip::gunzip(&full[..full.len() - 3], 100),
        Err(gzip::GzipError::Truncated)
    );
    let mut bad = gz(b"abc");
    let n = bad.len();
    bad[n - 1] ^= 1; // ISIZE
    assert_eq!(gzip::gunzip(&bad, 100), Err(gzip::GzipError::Checksum));
}

// ---- DNS --------------------------------------------------------------------------------------

/// Build an answer: header, the question, then records (name as a pointer to the question
/// or spelled out).
struct Msg {
    b: Vec<u8>,
    an: u16,
    ns: u16,
}

fn name_bytes(n: &str) -> Vec<u8> {
    let mut o = Vec::new();
    for l in n.split('.') {
        o.push(l.len() as u8);
        o.extend(l.as_bytes());
    }
    o.push(0);
    o
}

impl Msg {
    fn reply(id: u16, qname: &str, qtype: u16, flags: u16) -> Msg {
        let mut b = Vec::new();
        b.extend(id.to_be_bytes());
        b.extend((0x8180u16 | flags).to_be_bytes());
        b.extend([0, 1, 0, 0, 0, 0, 0, 0]);
        b.extend(name_bytes(qname));
        b.extend(qtype.to_be_bytes());
        b.extend(1u16.to_be_bytes());
        Msg { b, an: 0, ns: 0 }
    }

    fn rr(mut self, name: Option<&str>, ty: u16, ttl: u32, rdata: &[u8]) -> Msg {
        match name {
            None => self.b.extend([0xc0, 12]),
            Some(n) => self.b.extend(name_bytes(n)),
        }
        self.b.extend(ty.to_be_bytes());
        self.b.extend(1u16.to_be_bytes());
        self.b.extend(ttl.to_be_bytes());
        self.b.extend((rdata.len() as u16).to_be_bytes());
        self.b.extend(rdata);
        self.an += 1;
        self
    }

    fn soa(mut self, ttl: u32, minimum: u32) -> Msg {
        self.b.extend([0xc0, 12]);
        self.b.extend(6u16.to_be_bytes());
        self.b.extend(1u16.to_be_bytes());
        self.b.extend(ttl.to_be_bytes());
        let mut rd = name_bytes("ns.test");
        rd.extend(name_bytes("admin.test"));
        for v in [1u32, 2, 3, 4, minimum] {
            rd.extend(v.to_be_bytes());
        }
        self.b.extend((rd.len() as u16).to_be_bytes());
        self.b.extend(rd);
        self.ns += 1;
        self
    }

    fn done(mut self) -> Vec<u8> {
        self.b[6..8].copy_from_slice(&self.an.to_be_bytes());
        self.b[8..10].copy_from_slice(&self.ns.to_be_bytes());
        self.b
    }
}

#[test]
fn dns_query_bytes() {
    let q = dns::query(0x1234, "WWW.Example.com.", TYPE_AAAA).unwrap();
    let mut want = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    want.extend(name_bytes("www.example.com"));
    want.extend([0, 28, 0, 1]);
    assert_eq!(q, want);
    for bad in [
        "",
        "a..b",
        "-",
        &"x".repeat(64),
        "sp ace.test",
        &"a.".repeat(130),
    ] {
        if bad == "-" {
            continue; // a lone hyphen label is allowed by the LDH check here
        }
        assert_eq!(dns::query(1, bad, TYPE_A), Err(DnsError::BadName), "{bad}");
    }
}

#[test]
fn dns_parse_records_compression_ttl() {
    let mut cname_rd = vec![3, b'w', b'e', b'b'];
    cname_rd.extend([0xc0, 12 + 4]); // "web" + pointer to "example.test" inside the question
    let m = Msg::reply(7, "www.example.test", TYPE_A, 0)
        .rr(None, TYPE_CNAME, 300, &cname_rd)
        .rr(Some("web.example.test"), TYPE_A, 60, &[93, 184, 216, 34])
        .rr(
            Some("web.example.test"),
            TYPE_A,
            0x8000_0001,
            &[93, 184, 216, 35],
        )
        .done();
    let p = dns::parse(&m).unwrap();
    assert!(p.response && !p.truncated && p.rcode == 0);
    assert_eq!(p.question, Some(("www.example.test".into(), TYPE_A)));
    assert_eq!(
        p.answers[0].data,
        dns::RData::Cname("web.example.test".into())
    );
    assert_eq!(
        p.answers[2].ttl, 0,
        "a TTL with the top bit set counts as 0"
    );
    let c = dns::follow(&p, "www.example.test", TYPE_A).unwrap();
    assert_eq!(
        c.addrs,
        vec![Addr::V4([93, 184, 216, 34]), Addr::V4([93, 184, 216, 35])]
    );
    assert_eq!(
        (c.canonical.as_str(), c.hops, c.ttl),
        ("web.example.test", 1, 0)
    );
    let six = Msg::reply(8, "v6.test", TYPE_AAAA, 0)
        .rr(
            None,
            TYPE_AAAA,
            120,
            &[0x20, 1, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
        )
        .done();
    let c = dns::follow(&dns::parse(&six).unwrap(), "v6.test", TYPE_AAAA).unwrap();
    assert_eq!(c.addrs[0].to_string(), "2001:db8::1");
    assert_eq!(c.ttl, 120);
}

#[test]
fn dns_parse_hostile() {
    let base = Msg::reply(1, "a.test", TYPE_A, 0)
        .rr(None, TYPE_A, 5, &[1, 2, 3, 4])
        .done();
    // Every truncation is an error, never a panic.
    for n in 0..base.len() {
        assert!(dns::parse(&base[..n]).is_err(), "cut at {n}");
    }
    // A pointer to itself, and one pointing forward.
    let mut looped = base.clone();
    looped[12] = 0xc0;
    looped[13] = 12;
    assert_eq!(
        dns::parse(&looped).unwrap_err(),
        DnsError::Malformed("a compression pointer loops")
    );
    let mut fwd = base.clone();
    fwd[12] = 0xc0;
    fwd[13] = 40;
    assert!(dns::parse(&fwd).is_err());
    // Reserved label bits; a 3-byte A record.
    let mut res = base.clone();
    res[12] = 0x80;
    assert!(dns::parse(&res).is_err());
    let short_a = Msg::reply(1, "a.test", TYPE_A, 0)
        .rr(None, TYPE_A, 5, &[1, 2, 3])
        .done();
    assert!(dns::parse(&short_a).is_err());
    // Random bytes don't panic.
    let mut x: u32 = 12345;
    for _ in 0..2000 {
        let mut m = base.clone();
        for _ in 0..3 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let i = (x >> 8) as usize % m.len();
            m[i] = (x >> 20) as u8;
        }
        let _ = dns::parse(&m);
    }
}

/// A fake name server: answers by (name, type) from a table, with a UDP size limit (TC if
/// over), counting questions; plus the cache and a settable clock.
struct FakeDns {
    zone: Vec<(String, u16, Vec<u8>)>,
    udp_limit: usize,
    udp_asks: usize,
    tcp_asks: usize,
    now: u64,
    cache: Cache,
    next_id: u16,
    drop_first: usize,
}

impl FakeDns {
    fn new() -> FakeDns {
        FakeDns {
            zone: Vec::new(),
            udp_limit: 512,
            udp_asks: 0,
            tcp_asks: 0,
            now: 100,
            cache: Cache::new(16),
            next_id: 40,
            drop_first: 0,
        }
    }

    /// The answer to `q` (an answer built by `make(id)`), by name and type.
    fn on(mut self, name: &str, ty: u16, make: impl Fn(u16) -> Vec<u8>) -> FakeDns {
        // Built for id 0; the id is patched in when answering.
        self.zone.push((name.into(), ty, make(0)));
        self
    }

    fn answer(&self, q: &[u8]) -> Vec<u8> {
        let p = dns::parse(q).unwrap();
        let (n, t) = p.question.unwrap();
        let mut r = match self.zone.iter().find(|(zn, zt, _)| *zn == n && *zt == t) {
            Some((_, _, r)) => r.clone(),
            None => Msg::reply(0, &n, t, 3).done(), // NXDOMAIN
        };
        r[0..2].copy_from_slice(&q[0..2]);
        r
    }
}

impl Host for FakeDns {
    fn udp(&mut self, q: &[u8], ok: &mut dyn FnMut(&[u8]) -> bool) -> Result<Vec<u8>, DnsError> {
        self.udp_asks += 1;
        if self.drop_first > 0 {
            self.drop_first -= 1;
            return Err(DnsError::Timeout);
        }
        // A stray datagram first (wrong id): must be ignored.
        let mut stray = self.answer(q);
        stray[0] ^= 0xff;
        assert!(!ok(&stray));
        let mut r = self.answer(q);
        if r.len() > self.udp_limit {
            r.truncate(12 + q.len() - 12);
            r[2] |= 0x02; // TC
            r[6..12].fill(0);
        }
        assert!(ok(&r));
        Ok(r)
    }

    fn tcp(&mut self, q: &[u8]) -> Result<Vec<u8>, DnsError> {
        self.tcp_asks += 1;
        Ok(self.answer(q))
    }

    fn now_secs(&mut self) -> u64 {
        self.now
    }

    fn new_id(&mut self) -> u16 {
        self.next_id += 1;
        self.next_id
    }

    fn cache_get(&mut self, n: &str, t: u16, now: u64) -> Option<Cached> {
        self.cache.get(n, t, now)
    }

    fn cache_put(&mut self, n: &str, t: u16, v: Cached, ttl: u32, now: u64) {
        self.cache.put(n, t, v, ttl, now)
    }
}

#[test]
fn dns_lookup_cache_and_ttl() {
    let mut h = FakeDns::new()
        .on("a.test", TYPE_A, |id| {
            Msg::reply(id, "a.test", TYPE_A, 0)
                .rr(None, TYPE_A, 30, &[10, 0, 2, 2])
                .done()
        })
        .on("a.test", TYPE_AAAA, |id| {
            Msg::reply(id, "a.test", TYPE_AAAA, 0).soa(600, 20).done()
        });
    assert_eq!(
        dns::lookup(&mut h, "A.test.", TYPE_A).unwrap(),
        vec![Addr::V4([10, 0, 2, 2])]
    );
    assert_eq!(h.udp_asks, 1);
    h.now += 29;
    assert!(dns::lookup(&mut h, "a.test", TYPE_A).is_ok());
    assert_eq!(h.udp_asks, 1, "cached within the TTL");
    h.now += 1;
    assert!(dns::lookup(&mut h, "a.test", TYPE_A).is_ok());
    assert_eq!(h.udp_asks, 2, "asked again once the TTL ran out");
    // No AAAA: NODATA, cached for the SOA minimum (20 s, under its own 600).
    assert_eq!(
        dns::lookup(&mut h, "a.test", TYPE_AAAA),
        Err(DnsError::NoData)
    );
    assert_eq!(
        dns::lookup(&mut h, "a.test", TYPE_AAAA),
        Err(DnsError::NoData)
    );
    assert_eq!(h.udp_asks, 3);
    h.now += 20;
    let _ = dns::lookup(&mut h, "a.test", TYPE_AAAA);
    assert_eq!(h.udp_asks, 4);
    // resolve(): both families; the AAAA miss doesn't spoil the A.
    assert_eq!(
        dns::resolve(&mut h, "a.test", Want::Any).unwrap(),
        vec![Addr::V4([10, 0, 2, 2])]
    );
    assert_eq!(
        dns::resolve(&mut h, "a.test", Want::V6),
        Err(DnsError::NoData)
    );
    // NXDOMAIN, cached (no SOA: the default negative TTL).
    assert_eq!(
        dns::resolve(&mut h, "nope.test", Want::Any),
        Err(DnsError::NotFound)
    );
    let asks = h.udp_asks;
    assert_eq!(
        dns::lookup(&mut h, "nope.test", TYPE_A),
        Err(DnsError::NotFound)
    );
    assert_eq!(h.udp_asks, asks);
    // TTL clamping: a 0 TTL is kept MIN_TTL seconds.
    let mut h = FakeDns::new().on("z.test", TYPE_A, |id| {
        Msg::reply(id, "z.test", TYPE_A, 0)
            .rr(None, TYPE_A, 0, &[1, 1, 1, 1])
            .done()
    });
    dns::lookup(&mut h, "z.test", TYPE_A).unwrap();
    h.now += dns::MIN_TTL as u64 - 1;
    dns::lookup(&mut h, "z.test", TYPE_A).unwrap();
    assert_eq!(h.udp_asks, 1);
}

#[test]
fn dns_cname_across_questions() {
    // www -> cdn (no address in this answer) -> asked separately -> edge -> address.
    let mut h = FakeDns::new()
        .on("www.test", TYPE_A, |id| {
            Msg::reply(id, "www.test", TYPE_A, 0)
                .rr(None, TYPE_CNAME, 300, &name_bytes("cdn.test"))
                .done()
        })
        .on("cdn.test", TYPE_A, |id| {
            Msg::reply(id, "cdn.test", TYPE_A, 0)
                .rr(None, TYPE_CNAME, 50, &name_bytes("edge.test"))
                .rr(Some("edge.test"), TYPE_A, 40, &[5, 6, 7, 8])
                .done()
        });
    assert_eq!(
        dns::lookup(&mut h, "www.test", TYPE_A).unwrap(),
        vec![Addr::V4([5, 6, 7, 8])]
    );
    assert_eq!(h.udp_asks, 2);
    dns::lookup(&mut h, "www.test", TYPE_A).unwrap();
    dns::lookup(&mut h, "edge.test", TYPE_A).unwrap();
    assert_eq!(h.udp_asks, 2, "the chain's names are cached");
    // A loop across questions.
    let mut h = FakeDns::new()
        .on("x.test", TYPE_A, |id| {
            Msg::reply(id, "x.test", TYPE_A, 0)
                .rr(None, TYPE_CNAME, 9, &name_bytes("y.test"))
                .done()
        })
        .on("y.test", TYPE_A, |id| {
            Msg::reply(id, "y.test", TYPE_A, 0)
                .rr(None, TYPE_CNAME, 9, &name_bytes("x.test"))
                .done()
        });
    assert_eq!(
        dns::lookup(&mut h, "x.test", TYPE_A),
        Err(DnsError::CnameLoop)
    );
    // A loop inside one answer.
    let mut h = FakeDns::new().on("l.test", TYPE_A, |id| {
        Msg::reply(id, "l.test", TYPE_A, 0)
            .rr(None, TYPE_CNAME, 9, &name_bytes("m.test"))
            .rr(Some("m.test"), TYPE_CNAME, 9, &name_bytes("l.test"))
            .done()
    });
    assert_eq!(
        dns::lookup(&mut h, "l.test", TYPE_A),
        Err(DnsError::CnameLoop)
    );
}

#[test]
fn dns_truncated_goes_to_tcp() {
    let make = |id| {
        let mut m = Msg::reply(id, "many.test", TYPE_A, 0);
        for k in 0..40u8 {
            m = m.rr(None, TYPE_A, 300, &[10, 0, 0, k]);
        }
        m.done()
    };
    let mut h = FakeDns::new().on("many.test", TYPE_A, make);
    let a = dns::lookup(&mut h, "many.test", TYPE_A).unwrap();
    assert_eq!(a.len(), 40);
    assert_eq!((h.udp_asks, h.tcp_asks), (1, 1));
}

#[test]
fn dns_errors() {
    let mut h = FakeDns::new().on("fail.test", TYPE_A, |id| {
        Msg::reply(id, "fail.test", TYPE_A, 2).done()
    });
    assert_eq!(
        dns::lookup(&mut h, "fail.test", TYPE_A),
        Err(DnsError::Server(2))
    );
    h.drop_first = 1;
    assert_eq!(
        dns::lookup(&mut h, "fail.test", TYPE_A),
        Err(DnsError::Timeout)
    );
    assert_eq!(
        dns::lookup(&mut h, "bad name", TYPE_A),
        Err(DnsError::BadName)
    );
}

#[test]
fn dns_cache_capacity() {
    let mut c = Cache::new(2);
    c.put("a", TYPE_A, Cached::NotFound, 100, 0);
    c.put("b", TYPE_A, Cached::NotFound, 10, 0);
    c.put("c", TYPE_A, Cached::NotFound, 50, 0);
    assert_eq!(c.len(), 2);
    assert!(
        c.get("b", TYPE_A, 1).is_none(),
        "the soonest to expire went first"
    );
    assert!(c.get("a", TYPE_A, 1).is_some() && c.get("c", TYPE_A, 1).is_some());
    assert!(c.get("c", TYPE_A, 50).is_none());
    assert_eq!(c.list(1).len(), 1);
}

#[test]
fn dns_addr_text() {
    assert_eq!(Addr::V6([0; 16]).to_string(), "::");
    let mut a = [0u8; 16];
    a[15] = 1;
    assert_eq!(Addr::V6(a).to_string(), "::1");
    let b = [
        0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0x50, 0x54, 0, 0xff, 0xfe, 0x12, 0x34, 0x56,
    ];
    assert_eq!(Addr::V6(b).to_string(), "fe80::5054:ff:fe12:3456");
    let c = [0, 1, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 3];
    assert_eq!(Addr::V6(c).to_string(), "1:0:0:2::3");
}

// ---- HTML -------------------------------------------------------------------------------------

#[test]
fn html_entities() {
    assert_eq!(
        html::decode_entities("a &amp; b &lt;c&gt; &quot;q&quot; &#39;s"),
        "a & b <c> \"q\" 's"
    );
    assert_eq!(
        html::decode_entities("caf&eacute; &Ouml; &ntilde; &AElig; &yuml; &szlig;"),
        "caf\u{e9} \u{d6} \u{f1} \u{c6} \u{ff} \u{df}"
    );
    assert_eq!(
        html::decode_entities("&#233;&#xE9;&#x2014;&mdash;&hellip;&rsquo;"),
        "\u{e9}\u{e9}--...'"
    );
    assert_eq!(html::decode_entities("&#150;"), "-", "cp1252 numeric");
    assert_eq!(
        html::decode_entities("AT&T &amp &copy 2026 &bogus; & alone"),
        "AT&T & \u{a9} 2026 &bogus; & alone"
    );
    assert_eq!(html::decode_entities("&nbsp;x&euro;"), " xEUR");
}

#[test]
fn html_charsets() {
    assert_eq!(
        html::decode_bytes(b"caf\xe9", "text/html; charset=ISO-8859-1"),
        "caf\u{e9}"
    );
    assert_eq!(
        html::decode_bytes(b"caf\xc3\xa9", "text/html; charset=utf-8"),
        "caf\u{e9}"
    );
    assert_eq!(
        html::decode_bytes(b"<meta charset=\"windows-1252\">\x93hi\x94", "text/html"),
        "<meta charset=\"windows-1252\">\u{201c}hi\u{201d}"
    );
    assert_eq!(
        html::decode_bytes(
            b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=iso-8859-1\">\xe9",
            ""
        ),
        "<meta http-equiv=\"Content-Type\" content=\"text/html; charset=iso-8859-1\">\u{e9}"
    );
    assert_eq!(
        html::decode_bytes(b"\xef\xbb\xbfplain \xc3\xa9", ""),
        "plain \u{e9}"
    );
    assert_eq!(html::decode_bytes(b"not utf8 \xe9", ""), "not utf8 \u{e9}");
}

#[test]
fn html_blocks_and_runs() {
    let d = html::parse(concat!(
        "<!DOCTYPE html><html><head><title>The  Page</title><style>p { color: red }</style>",
        "<script>if (a < b && c > d) { document.write('<p>no</p>') }</script></head>",
        "<body><h1>Head</h1><div>One <span>two</span> <b>bold</b> <a href=\"/x?a=1&amp;b=2\">link</a></div>",
        "<p>Para<br>line two</p><ul><li>first<li>second</ul>",
        "<pre>  code\n    indented</pre><blockquote>quoted</blockquote><hr>",
        "<table><tr><th>Name</th><th>Size</th></tr><tr><td>milk</td><td>1 L</td></tr></table>",
        "<p>An <img src=x.png alt=\"cow picture\"> and <img src=y.png></p>",
        "<noscript>no JS here</noscript><svg><text>drop</text></svg><!-- gone -->",
        "<p>a < b, 5 > 3</p></body></html>"
    ));
    assert_eq!(d.title, "The Page");
    let kinds: Vec<BlockKind> = d.blocks.iter().map(|b| b.kind).collect();
    use BlockKind::*;
    assert_eq!(kinds, [H1, P, P, Li, Li, Pre, Quote, Rule, P, P, P, P, P]);
    let t: Vec<String> = d.blocks.iter().map(|b| b.text()).collect();
    assert_eq!(t[0], "Head");
    assert_eq!(t[1], "One two bold link");
    assert_eq!(t[2], "Para\nline two");
    assert_eq!(t[5], "  code\n    indented");
    assert_eq!(t[8], "Name | Size");
    assert_eq!(t[9], "milk | 1 L");
    assert_eq!(t[10], "An [cow picture] and");
    assert_eq!(t[11], "no JS here");
    assert_eq!(t[12], "a < b, 5 > 3");
    let runs = &d.blocks[1].runs;
    assert!(runs.iter().any(|r| r.bold && r.text == "bold"));
    assert!(
        runs.iter()
            .any(|r| r.link.as_deref() == Some("/x?a=1&b=2") && r.text == "link")
    );
    assert!(
        d.blocks[10]
            .runs
            .iter()
            .any(|r| r.italic && r.text.contains("[cow picture]"))
    );
    assert!(
        !d.text().contains("document.write")
            && !d.text().contains("color")
            && !d.text().contains("drop")
    );
}

#[test]
fn html_odd_input() {
    // Unclosed things, attributes with '>' in quotes, stray '<', no panics on junk.
    let d = html::parse("<p title=\"a>b\">x<b>never closed<p>next<a href='q'>open link");
    assert_eq!(d.blocks.len(), 2);
    assert_eq!(d.blocks[0].text(), "xnever closed");
    assert!(
        d.blocks[1]
            .runs
            .iter()
            .any(|r| r.link.as_deref() == Some("q"))
    );
    let d = html::parse("<script>never closed");
    assert!(d.blocks.is_empty());
    for junk in [
        "<",
        "<<>>",
        "&",
        "&#",
        "&#x;",
        "<a href=",
        "<!--",
        "</>",
        "<p",
        "\u{1F600}<b>\u{e9}",
    ] {
        let _ = html::parse(junk);
    }
    assert_eq!(
        html::parse("<P>Upper <B>case</B></P>").blocks[0].runs[1].text,
        "case"
    );
    assert_eq!(
        html::attr("a  HREF = \"x y\" class=z", "href").as_deref(),
        Some("x y")
    );
    assert_eq!(html::attr("img alt=one two", "alt").as_deref(), Some("one"));
    assert!(
        html::is_html("text/html; charset=utf-8", "")
            && html::is_html("", "  <p>")
            && !html::is_html("text/plain", "<p>")
    );
    let p = html::plain("a\tb\r\n\u{2014}");
    assert_eq!(p.blocks[0].text(), "a    b\n-");
}

// ---- requests with a body, and the hooks (the Browser's fetch) --------------------------------

#[derive(Default)]
struct Rec {
    cookies_asked: Vec<String>,
    seen: Vec<(String, u16, Vec<String>)>,
    progress: Vec<(usize, Option<usize>)>,
    stop_after: Option<usize>,
    reads: usize,
    raise: Option<usize>,
}

impl http::Hooks for Rec {
    fn cookie(&mut self, url: &Url, _m: http::Method) -> Option<String> {
        self.cookies_asked.push(url.to_text());
        (url.host == "a.test").then(|| String::from("sid=1"))
    }
    fn response(&mut self, url: &Url, status: u16, headers: &[(String, String)]) -> Option<usize> {
        let sc = headers
            .iter()
            .filter(|(k, _)| k == "set-cookie")
            .map(|(_, v)| v.clone())
            .collect();
        self.seen.push((url.to_text(), status, sc));
        self.raise
    }
    fn progress(&mut self, got: usize, total: Option<usize>) {
        self.progress.push((got, total));
    }
    fn stop(&mut self) -> bool {
        self.reads += 1;
        self.stop_after.is_some_and(|n| self.reads > n)
    }
}

#[test]
fn http_post_body_and_post_redirect_get() {
    let mut net = FakeNet::new()
        .serve(
            "a.test",
            answer(b"HTTP/1.1 303 See Other\r\nLocation: /form\r\nSet-Cookie: sid=2\r\nContent-Length: 0\r\n\r\n"),
        )
        .serve("a.test", answer(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"));
    let req = http::Request::post_form(&url("http://a.test/form"), b"name=milk&q=a%26b".to_vec());
    let mut h = Rec::default();
    let r = http::fetch(&mut net, &req, &Options::default(), &mut h).unwrap();
    assert_eq!(r.body, b"ok");
    let reqs = &net.log.borrow().requests;
    assert!(
        reqs[0].starts_with("POST /form HTTP/1.1\r\n"),
        "{}",
        reqs[0]
    );
    assert!(reqs[0].contains("\r\nContent-Type: application/x-www-form-urlencoded\r\n"));
    assert!(reqs[0].contains("\r\nContent-Length: 17\r\n"));
    assert!(reqs[0].contains("\r\nCookie: sid=1\r\n"));
    assert!(reqs[0].ends_with("\r\n\r\nname=milk&q=a%26b"));
    // 303 to its own address: a GET, no body, not a loop.
    assert!(reqs[1].starts_with("GET /form HTTP/1.1\r\n"), "{}", reqs[1]);
    assert!(!reqs[1].contains("Content-Length"));
    assert_eq!(
        h.seen[0],
        (
            "http://a.test/form".to_string(),
            303,
            vec!["sid=2".to_string()]
        )
    );
    assert_eq!(h.seen[1].1, 200);
}

#[test]
fn http_307_repeats_the_post_and_cookies_are_per_hop() {
    let mut net = FakeNet::new()
        .serve(
            "a.test",
            answer(b"HTTP/1.1 307 Again\r\nLocation: http://b.test/x\r\n\r\n"),
        )
        .serve(
            "b.test",
            answer(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"),
        );
    let req = http::Request::post_form(&url("http://a.test/f"), b"a=1".to_vec());
    let mut h = Rec::default();
    http::fetch(&mut net, &req, &Options::default(), &mut h).unwrap();
    let reqs = &net.log.borrow().requests;
    assert!(
        reqs[1].starts_with("POST /x HTTP/1.1\r\nHost: b.test\r\n"),
        "{}",
        reqs[1]
    );
    assert!(reqs[1].ends_with("\r\n\r\na=1"));
    // a.test's cookie stays on a.test.
    assert!(reqs[0].contains("Cookie: sid=1"));
    assert!(!reqs[1].contains("Cookie"));
    assert_eq!(h.cookies_asked, ["http://a.test/f", "http://b.test/x"]);
}

#[test]
fn http_extra_headers_cannot_smuggle() {
    let mut net = FakeNet::new().serve(
        "a.test",
        answer(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"),
    );
    let mut req = http::Request::get(&url("http://a.test/"));
    req.headers
        .push(("Referer".into(), "http://a.test/prev".into()));
    req.headers
        .push(("X-Evil".into(), "a\r\nHost: evil.test".into()));
    req.headers.push(("Host".into(), "evil.test".into()));
    req.headers.push(("Bad Name:".into(), "x".into()));
    http::fetch(&mut net, &req, &Options::default(), &mut http::NoHooks).unwrap();
    let r = &net.log.borrow().requests[0];
    assert!(r.contains("\r\nReferer: http://a.test/prev\r\n"));
    assert!(!r.contains("evil"), "{r}");
    assert_eq!(r.matches("Host:").count(), 1);
}

#[test]
fn http_progress_stop_and_bigger_limit() {
    let body = vec![b'x'; 40_000];
    let mut resp = b"HTTP/1.1 200 OK\r\nContent-Length: 40000\r\n\r\n".to_vec();
    resp.extend_from_slice(&body);
    let script = Script {
        pieces: resp.chunks(5000).map(|c| c.to_vec()).collect(),
        stall: false,
    };
    // Progress is reported with the total.
    let mut net = FakeNet::new().serve("a.test", script.clone());
    let mut h = Rec::default();
    let r = http::fetch(
        &mut net,
        &http::Request::get(&url("http://a.test/f")),
        &Options::default(),
        &mut h,
    )
    .unwrap();
    assert_eq!(r.body.len(), 40_000);
    assert_eq!(h.progress.last(), Some(&(40_000, Some(40_000))));
    assert!(h.progress.len() > 2);
    // Stop.
    let mut net = FakeNet::new().serve("a.test", script.clone());
    let mut h = Rec {
        stop_after: Some(3),
        ..Rec::default()
    };
    let e = http::fetch(
        &mut net,
        &http::Request::get(&url("http://a.test/f")),
        &Options::default(),
        &mut h,
    )
    .unwrap_err();
    assert_eq!(e.error, Error::Cancelled);
    assert_eq!(e.error.title(), "Stopped");
    // Over the limit, unless the hook raises it for this response (a download).
    let small = Options {
        max_body: 10_000,
        ..Options::default()
    };
    let mut net = FakeNet::new().serve("a.test", script.clone());
    let e = http::fetch(
        &mut net,
        &http::Request::get(&url("http://a.test/f")),
        &small,
        &mut Rec::default(),
    )
    .unwrap_err();
    assert_eq!(e.error, Error::TooBig(10_000));
    let mut net = FakeNet::new().serve("a.test", script);
    let mut h = Rec {
        raise: Some(100_000),
        ..Rec::default()
    };
    let r = http::fetch(
        &mut net,
        &http::Request::get(&url("http://a.test/f")),
        &small,
        &mut h,
    )
    .unwrap();
    assert_eq!(r.body.len(), 40_000);
}

// ---- other methods, no-follow redirects and the reason in the head (script requests) ---------

#[test]
fn http_put_with_body_and_head_without() {
    let mut net = FakeNet::new()
        .serve("a.test", answer(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"))
        .serve(
            "a.test",
            answer(b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\nContent-Type: text/plain\r\n\r\n"),
        )
        .serve("a.test", answer(b"HTTP/1.1 204 No Content\r\n\r\n"));
    let mut put = http::Request::get(&url("http://a.test/item/1"));
    put.verb = Some("PUT".into());
    put.body = b"{\"a\":1}".to_vec();
    put.content_type = "application/json".into();
    let r = http::fetch(&mut net, &put, &Options::default(), &mut http::NoHooks).unwrap();
    assert_eq!(r.body, b"ok");
    let mut head = http::Request::get(&url("http://a.test/item/1"));
    head.verb = Some("HEAD".into());
    let r = http::fetch(&mut net, &head, &Options::default(), &mut http::NoHooks).unwrap();
    assert_eq!(r.status, 200);
    assert!(r.body.is_empty(), "a HEAD answer has no body, whatever Content-Length says");
    // DELETE without a body: no Content-Length; PATCH without one: Content-Length 0.
    let mut del = http::Request::get(&url("http://a.test/item/1"));
    del.verb = Some("DELETE".into());
    http::fetch(&mut net, &del, &Options::default(), &mut http::NoHooks).unwrap();
    let reqs = &net.log.borrow().requests;
    assert!(reqs[0].starts_with("PUT /item/1 HTTP/1.1\r\n"), "{}", reqs[0]);
    assert!(reqs[0].contains("\r\nContent-Type: application/json\r\n"));
    assert!(reqs[0].contains("\r\nContent-Length: 7\r\n"));
    assert!(reqs[0].ends_with("\r\n\r\n{\"a\":1}"));
    assert!(reqs[1].starts_with("HEAD /item/1 HTTP/1.1\r\n"));
    assert!(reqs[2].starts_with("DELETE /item/1 HTTP/1.1\r\n"));
    assert!(!reqs[2].contains("Content-Length"), "{}", reqs[2]);
    let mut patch = http::Request::get(&url("http://a.test/p"));
    patch.verb = Some("PATCH".into());
    assert!(http::request_head(&patch, None, &Options::default()).contains("Content-Length: 0\r\n"));
}

#[test]
fn http_redirects_can_be_left_to_the_caller() {
    let mut net = FakeNet::new().serve(
        "a.test",
        answer(b"HTTP/1.1 302 Found\r\nLocation: http://b.test/x\r\nSet-Cookie: s=1\r\nContent-Length: 0\r\n\r\n"),
    );
    let o = Options {
        follow_redirects: false,
        ..Options::default()
    };
    let mut h = Rec::default();
    let r = http::fetch(&mut net, &http::Request::get(&url("http://a.test/go")), &o, &mut h).unwrap();
    assert_eq!(r.status, 302);
    assert_eq!(r.header("location"), Some("http://b.test/x"));
    assert!(r.redirects.is_empty());
    assert_eq!(net.log.borrow().requests.len(), 1, "nothing was followed");
    assert_eq!(h.seen.len(), 1);
}

#[test]
fn http_response_head_hook_sees_the_reason_and_can_set_the_limit() {
    struct R {
        heads: Vec<(u16, String)>,
    }
    impl http::Hooks for R {
        fn response_head(
            &mut self,
            _u: &Url,
            status: u16,
            reason: &str,
            _h: &[(String, String)],
        ) -> Option<usize> {
            self.heads.push((status, reason.to_string()));
            Some(3)
        }
    }
    let mut net = FakeNet::new().serve(
        "a.test",
        answer(b"HTTP/1.1 418 I'm a teapot\r\nContent-Length: 10\r\n\r\n0123456789"),
    );
    let mut h = R { heads: Vec::new() };
    let r = http::fetch(
        &mut net,
        &http::Request::get(&url("http://a.test/t")),
        &Options::default(),
        &mut h,
    );
    assert_eq!(h.heads, [(418, "I'm a teapot".to_string())]);
    assert!(matches!(r, Err(f) if matches!(f.error, http::Error::TooBig(3))));
}
