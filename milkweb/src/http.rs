//! An HTTP/1.1 client for GET, over a byte stream the caller opens.
//!
//! The caller provides a [`Net`]: `connect(host, port)` gives a [`Transport`] (TCP in the
//! kernel and in `rt::web`, a fake one in the host tests), `now_ms()` a clock for the overall
//! time limit, and `secure(host, stream)` wraps a fresh connection for `https://`. That is the
//! TLS hook: today `has_tls()` is false and an `https://` address fails at once with
//! [`Error::SecureUnsupported`]; once the `tls` crate is there, `has_tls` says true and
//! `secure` becomes `tls::Client::connect(host, stream)` (its stream implementing
//! [`Transport`]), and nothing here changes.
//!
//! What it does: a Host header, `Connection: close` (or keep-alive, reused across same-origin
//! redirects, when [`Options::keep_alive`] asks), bodies framed by chunked transfer encoding,
//! Content-Length or the end of the connection, interim 1xx answers skipped, redirects
//! (301/302/303/307/308, at most [`Options::max_redirects`], a loop is an error), gzip and
//! deflate bodies, a User-Agent, an overall deadline and limits on the header and body sizes.
//! Anything malformed is an [`Error`], never a panic.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::gzip;
use crate::url::{Scheme, Url, UrlError};

/// A connected byte stream (TCP, or TLS on top of it).
pub trait Transport {
    /// Some bytes into `buf`; `Ok(0)` at the end of the stream. An implementation gives up
    /// with [`Error::Timeout`] when nothing arrives for a while (its idle limit).
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error>;
    /// Write some of `buf` (at least one byte unless it is empty).
    fn write(&mut self, buf: &[u8]) -> Result<usize, Error>;
}

/// What the client needs from its host.
pub trait Net {
    /// Look `host` up and connect to `port` (within `timeout_ms`).
    fn connect(
        &mut self,
        host: &str,
        port: u16,
        timeout_ms: u64,
    ) -> Result<Box<dyn Transport>, Error>;
    /// Milliseconds on any monotonic clock.
    fn now_ms(&mut self) -> u64;
    /// Can [`Net::secure`] do TLS? (Asked before connecting, so an `https://` address costs no
    /// lookup or connection while the answer is no.)
    fn has_tls(&self) -> bool {
        false
    }
    /// Wrap a new connection to `host` for `https://` (TLS). Until milkOS has TLS this says so.
    fn secure(
        &mut self,
        host: &str,
        stream: Box<dyn Transport>,
    ) -> Result<Box<dyn Transport>, Error> {
        let _ = (host, stream);
        Err(Error::SecureUnsupported)
    }
}

pub const USER_AGENT: &str = "milkOS/0.1";

#[derive(Clone, Debug)]
pub struct Options {
    /// Redirects to follow (more is [`Error::TooManyRedirects`]).
    pub max_redirects: usize,
    /// The whole fetch, redirects included.
    pub timeout_ms: u64,
    /// Each connection attempt (passed to [`Net::connect`]).
    pub connect_timeout_ms: u64,
    /// Status line plus headers.
    pub max_header: usize,
    /// The body as received and as decompressed.
    pub max_body: usize,
    pub user_agent: String,
    pub accept: String,
    /// Ask for (and undo) gzip / deflate.
    pub decompress: bool,
    /// Keep the connection open for a redirect to the same scheme, host and port.
    pub keep_alive: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            max_redirects: 10,
            timeout_ms: 30_000,
            connect_timeout_ms: 10_000,
            max_header: 32 * 1024,
            max_body: 4 * 1024 * 1024,
            user_agent: String::from(USER_AGENT),
            accept: String::from("text/html, text/plain;q=0.9, */*;q=0.5"),
            decompress: true,
            keep_alive: false,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Error {
    Url(UrlError),
    /// `https://` before milkOS has TLS.
    SecureUnsupported,
    /// The name wasn't looked up because the internet is off (offline mode).
    Offline(String),
    /// There is no such host (the name server said so).
    NotFound(String),
    /// No answer about a name (no DNS server, or it didn't answer).
    NoDns(String),
    /// The connection couldn't be made (refused, unreachable, timed out): the text says.
    Connect(String),
    /// The server stopped sending, or the whole fetch took too long.
    Timeout,
    /// The headers or the body went over the limit (the limit, in bytes).
    TooBig(usize),
    TooManyRedirects(usize),
    /// A redirect came back to an address already visited.
    RedirectLoop(String),
    /// Not a valid HTTP response (what was wrong).
    Malformed(&'static str),
    /// The connection broke (the text says how).
    Io(String),
    /// A gzip / deflate body didn't decompress.
    Decode(&'static str),
}

impl Error {
    /// A short heading for an error page.
    pub fn title(&self) -> &'static str {
        match self {
            Error::Url(_) => "That isn't an address",
            Error::SecureUnsupported => "Secure pages aren't supported yet",
            Error::Offline(_) => "No internet (offline mode)",
            Error::NotFound(_) => "Server not found",
            Error::NoDns(_) => "No internet",
            Error::Connect(_) => "Can't connect to the server",
            Error::Timeout => "The server didn't answer in time",
            Error::TooBig(_) => "The page is too big",
            Error::TooManyRedirects(_) | Error::RedirectLoop(_) => "Too many redirects",
            Error::Malformed(_) | Error::Decode(_) => "The server sent something odd",
            Error::Io(_) => "The connection broke",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Url(e) => write!(f, "{e}"),
            Error::SecureUnsupported => f.write_str(
                "https:// pages need TLS, which milkOS doesn't have yet (try the http:// address)",
            ),
            Error::Offline(h) => write!(
                f,
                "{h}: not looked up, the internet is off (offline mode; numeric addresses still work)"
            ),
            Error::NotFound(h) => write!(f, "{h}: there's no such host"),
            Error::NoDns(m) => f.write_str(m),
            Error::Connect(m) => f.write_str(m),
            Error::Timeout => f.write_str("the server stopped answering (timed out)"),
            Error::TooBig(n) => write!(f, "more than {} KiB", n / 1024),
            Error::TooManyRedirects(n) => write!(f, "more than {n} redirects"),
            Error::RedirectLoop(u) => write!(f, "the redirects go round in a circle (back to {u})"),
            Error::Malformed(m) => write!(f, "not a valid HTTP response: {m}"),
            Error::Io(m) => write!(f, "{m}"),
            Error::Decode(m) => f.write_str(m),
        }
    }
}

impl From<UrlError> for Error {
    fn from(e: UrlError) -> Error {
        Error::Url(e)
    }
}

/// A failed fetch: what went wrong and the address it was at (after any redirects).
#[derive(Clone, Debug)]
pub struct Failure {
    pub error: Error,
    pub url: Url,
    pub redirects: Vec<Url>,
}

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    /// Names in lower case, in the order received.
    pub headers: Vec<(String, String)>,
    /// Decoded: de-chunked and decompressed.
    pub body: Vec<u8>,
    /// Where the body came from (the last address after redirects).
    pub url: Url,
    /// Every address redirected to, in order (the last is `url`).
    pub redirects: Vec<Url>,
    /// Body bytes as they came over the wire (before decompression).
    pub wire_bytes: usize,
    /// "gzip" / "deflate" if the body was compressed.
    pub encoding: Option<&'static str>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn content_type(&self) -> &str {
        self.header("content-type").unwrap_or("")
    }

    /// The raw header block as text (`HTTP/1.1 200 OK` + `Name: value` lines).
    pub fn head_text(&self) -> String {
        let mut s = format!("HTTP/1.1 {} {}\r\n", self.status, self.reason);
        for (k, v) in &self.headers {
            s.push_str(k);
            s.push_str(": ");
            s.push_str(v);
            s.push_str("\r\n");
        }
        s
    }
}

// ---- reading ----------------------------------------------------------------------------------

struct Reader<'a> {
    t: &'a mut dyn Transport,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    deadline: u64,
}

impl Reader<'_> {
    fn avail(&self) -> &[u8] {
        &self.buf[self.pos..]
    }

    /// Read more; false at the end of the stream.
    fn fill(&mut self, net: &mut dyn Net) -> Result<bool, Error> {
        if self.eof {
            return Ok(false);
        }
        if net.now_ms() > self.deadline {
            return Err(Error::Timeout);
        }
        if self.pos > 0 && self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        } else if self.pos > 64 * 1024 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        let at = self.buf.len();
        self.buf.resize(at + 16 * 1024, 0);
        let r = self.t.read(&mut self.buf[at..]);
        let n = match r {
            Ok(n) => n,
            Err(e) => {
                self.buf.truncate(at);
                return Err(e);
            }
        };
        self.buf.truncate(at + n.min(16 * 1024));
        if n == 0 {
            self.eof = true;
        }
        Ok(n > 0)
    }

    /// One line (without CR LF), at most `max` bytes.
    fn line(&mut self, net: &mut dyn Net, max: usize) -> Result<Vec<u8>, Error> {
        loop {
            if let Some(i) = self.avail().iter().position(|&b| b == b'\n') {
                let mut l = self.avail()[..i].to_vec();
                self.pos += i + 1;
                if l.last() == Some(&b'\r') {
                    l.pop();
                }
                return Ok(l);
            }
            if self.avail().len() > max {
                return Err(Error::TooBig(max));
            }
            if !self.fill(net)? {
                return Err(Error::Malformed(
                    "the connection closed in the middle of a line",
                ));
            }
        }
    }

    /// Exactly `n` bytes appended to `out`.
    fn exact(&mut self, net: &mut dyn Net, n: usize, out: &mut Vec<u8>) -> Result<(), Error> {
        let mut left = n;
        while left > 0 {
            if self.avail().is_empty() && !self.fill(net)? {
                return Err(Error::Malformed("the body is shorter than the server said"));
            }
            let take = left.min(self.avail().len());
            out.extend_from_slice(&self.avail()[..take]);
            self.pos += take;
            left -= take;
        }
        Ok(())
    }

    /// Everything up to the end of the stream, at most `max` bytes in `out`.
    fn rest(&mut self, net: &mut dyn Net, out: &mut Vec<u8>, max: usize) -> Result<(), Error> {
        loop {
            out.extend_from_slice(self.avail());
            self.pos = self.buf.len();
            if out.len() > max {
                return Err(Error::TooBig(max));
            }
            if !self.fill(net)? {
                return Ok(());
            }
        }
    }
}

/// Status, reason, headers (names in lower case).
type Head = (u16, String, Vec<(String, String)>);
/// A head and its (still encoded) body.
type Answer = (Head, Vec<u8>);

/// Status line + headers. Ok(None) for a connection closed before anything came (a dead
/// kept-alive connection).
fn read_head(r: &mut Reader, net: &mut dyn Net, max: usize) -> Result<Option<Head>, Error> {
    if r.avail().is_empty() && !r.fill(net)? {
        return Ok(None);
    }
    let status_line = r.line(net, max)?;
    let sl = String::from_utf8_lossy(&status_line).into_owned();
    let mut parts = sl.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    if !(version == "HTTP/1.1" || version == "HTTP/1.0") {
        return Err(Error::Malformed("no HTTP/1.x status line"));
    }
    let code = parts.next().unwrap_or("");
    let status = match code.parse::<u16>() {
        Ok(s) if code.len() == 3 && (100..600).contains(&s) => s,
        _ => return Err(Error::Malformed("bad status code")),
    };
    let reason = String::from(parts.next().unwrap_or("").trim());
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut total = status_line.len();
    loop {
        let l = r.line(net, max)?;
        total += l.len() + 2;
        if total > max {
            return Err(Error::TooBig(max));
        }
        if l.is_empty() {
            break;
        }
        let text = String::from_utf8_lossy(&l).into_owned();
        if text.starts_with([' ', '\t']) {
            // obs-fold: a continuation of the previous header.
            match headers.last_mut() {
                Some((_, v)) => {
                    v.push(' ');
                    v.push_str(text.trim());
                }
                None => return Err(Error::Malformed("a header continues nothing")),
            }
            continue;
        }
        let Some((k, v)) = text.split_once(':') else {
            return Err(Error::Malformed("a header line without a colon"));
        };
        let k = k.trim();
        if k.is_empty() || k.contains(char::is_whitespace) {
            return Err(Error::Malformed("a bad header name"));
        }
        if headers.len() >= 200 {
            return Err(Error::Malformed("too many headers"));
        }
        headers.push((k.to_ascii_lowercase(), String::from(v.trim())));
    }
    Ok(Some((status, reason, headers)))
}

fn header<'h>(headers: &'h [(String, String)], name: &str) -> Option<&'h str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// Does a comma-separated header list contain `token` (case-insensitive)?
fn has_token(headers: &[(String, String)], name: &str, token: &str) -> bool {
    headers
        .iter()
        .filter(|(k, _)| k == name)
        .flat_map(|(_, v)| v.split(','))
        .any(|t| t.trim().eq_ignore_ascii_case(token))
}

/// A chunked body (RFC 9112 7.1): sizes in hex (extensions ignored), then trailers.
fn read_chunked(r: &mut Reader, net: &mut dyn Net, max: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    loop {
        let l = r.line(net, 1024)?;
        let text = String::from_utf8_lossy(&l);
        let hex = text.split(';').next().unwrap_or("").trim();
        if hex.is_empty() || hex.len() > 15 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Malformed("bad chunk size"));
        }
        let n = usize::from_str_radix(hex, 16).map_err(|_| Error::Malformed("bad chunk size"))?;
        if n == 0 {
            // Trailers, up to the empty line.
            for _ in 0..100 {
                if r.line(net, 8192)?.is_empty() {
                    return Ok(out);
                }
            }
            return Err(Error::Malformed("too many trailers"));
        }
        if out.len() + n > max {
            return Err(Error::TooBig(max));
        }
        r.exact(net, n, &mut out)?;
        if !r.line(net, 2)?.is_empty() {
            return Err(Error::Malformed("a chunk is longer than its size"));
        }
    }
}

/// What to do with the connection after a response.
#[derive(PartialEq, Eq)]
enum After {
    Reuse,
    Close,
}

/// The body of a response whose head was just read.
fn read_body(
    r: &mut Reader,
    net: &mut dyn Net,
    status: u16,
    headers: &[(String, String)],
    max: usize,
) -> Result<(Vec<u8>, After), Error> {
    let close = has_token(headers, "connection", "close");
    if status == 204 || status == 304 || (100..200).contains(&status) {
        return Ok((Vec::new(), if close { After::Close } else { After::Reuse }));
    }
    let te = headers
        .iter()
        .filter(|(k, _)| k == "transfer-encoding")
        .flat_map(|(_, v)| v.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>();
    if let Some(last) = te.last() {
        if last != "chunked" {
            // Some other coding with no chunking: the body runs to the close.
            if te.iter().any(|t| t != "identity") {
                return Err(Error::Malformed("an unknown transfer encoding"));
            }
        } else {
            if te.len() > 1 {
                return Err(Error::Malformed("an unknown transfer encoding"));
            }
            let b = read_chunked(r, net, max)?;
            return Ok((b, if close { After::Close } else { After::Reuse }));
        }
    }
    let lens: Vec<&str> = headers
        .iter()
        .filter(|(k, _)| k == "content-length")
        .flat_map(|(_, v)| v.split(','))
        .map(str::trim)
        .collect();
    if let Some(first) = lens.first() {
        if lens.iter().any(|l| l != first) {
            return Err(Error::Malformed("two different Content-Lengths"));
        }
        if first.is_empty() || !first.bytes().all(|c| c.is_ascii_digit()) || first.len() > 15 {
            return Err(Error::Malformed("a bad Content-Length"));
        }
        let n: usize = first
            .parse()
            .map_err(|_| Error::Malformed("a bad Content-Length"))?;
        if n > max {
            return Err(Error::TooBig(max));
        }
        let mut out = Vec::with_capacity(n.min(1 << 20));
        r.exact(net, n, &mut out)?;
        return Ok((out, if close { After::Close } else { After::Reuse }));
    }
    let mut out = Vec::new();
    r.rest(net, &mut out, max)?;
    Ok((out, After::Close))
}

fn decode_body(
    body: Vec<u8>,
    headers: &[(String, String)],
    max: usize,
) -> Result<(Vec<u8>, Option<&'static str>), Error> {
    let enc = header(headers, "content-encoding")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let r = match enc.as_str() {
        "" | "identity" => return Ok((body, None)),
        "gzip" | "x-gzip" => gzip::gunzip(&body, max).map(|b| (b, Some("gzip"))),
        "deflate" => gzip::inflate_http(&body, max).map(|b| (b, Some("deflate"))),
        _ => {
            return Err(Error::Decode(
                "the page is compressed in a way milkOS can't undo",
            ));
        }
    };
    r.map_err(|e| match e {
        gzip::GzipError::TooBig => Error::TooBig(max),
        e => Error::Decode(e.message()),
    })
}

// ---- a fetch ----------------------------------------------------------------------------------

/// The request for `url`.
pub fn request_text(url: &Url, o: &Options) -> String {
    let mut s = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nAccept: {}\r\n",
        url.path,
        url.host_port(),
        o.user_agent,
        o.accept
    );
    if o.decompress {
        s.push_str("Accept-Encoding: gzip, deflate\r\n");
    }
    s.push_str(if o.keep_alive {
        "Connection: keep-alive\r\n\r\n"
    } else {
        "Connection: close\r\n\r\n"
    });
    s
}

fn write_all(t: &mut dyn Transport, mut b: &[u8]) -> Result<(), Error> {
    while !b.is_empty() {
        let n = t.write(b)?;
        if n == 0 {
            return Err(Error::Io(String::from(
                "the connection closed while sending",
            )));
        }
        b = &b[n.min(b.len())..];
    }
    Ok(())
}

/// An open connection and what is buffered on it.
struct Conn {
    t: Box<dyn Transport>,
    scheme: Scheme,
    host: String,
    port: u16,
    buf: Vec<u8>,
}

fn open(net: &mut dyn Net, url: &Url, o: &Options) -> Result<Conn, Error> {
    let secure = url.scheme == Scheme::Https;
    // Refuse https before any lookup or connection while there is no TLS.
    if secure && !net.has_tls() {
        return Err(Error::SecureUnsupported);
    }
    let t = net.connect(&url.host, url.port, o.connect_timeout_ms)?;
    let t = if secure { net.secure(&url.host, t)? } else { t };
    Ok(Conn {
        t,
        scheme: url.scheme,
        host: url.host.clone(),
        port: url.port,
        buf: Vec::new(),
    })
}

/// One request and its response (no redirects followed). The connection comes back if it
/// can carry another request.
fn exchange(
    net: &mut dyn Net,
    conn: &mut Conn,
    url: &Url,
    o: &Options,
    deadline: u64,
) -> Result<(Option<Answer>, After), Error> {
    write_all(conn.t.as_mut(), request_text(url, o).as_bytes())?;
    let mut r = Reader {
        t: conn.t.as_mut(),
        buf: core::mem::take(&mut conn.buf),
        pos: 0,
        eof: false,
        deadline,
    };
    loop {
        let Some((status, reason, headers)) = read_head(&mut r, net, o.max_header)? else {
            return Ok((None, After::Close));
        };
        if (100..200).contains(&status) {
            if status == 101 {
                return Err(Error::Malformed("the server switched protocols"));
            }
            continue; // 100 Continue, 103 Early Hints: the real answer follows
        }
        let (body, after) = read_body(&mut r, net, status, &headers, o.max_body)?;
        let after = if !o.keep_alive || has_token(&headers, "connection", "close") {
            After::Close
        } else {
            after
        };
        conn.buf = r.buf[r.pos..].to_vec();
        return Ok((Some(((status, reason, headers), body)), after));
    }
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// GET `url`, following redirects. Thread context: it blocks in the transport.
pub fn get(net: &mut dyn Net, url: &Url, o: &Options) -> Result<Response, Failure> {
    let deadline = net.now_ms().saturating_add(o.timeout_ms);
    let mut url = url.clone();
    let mut redirects: Vec<Url> = Vec::new();
    let mut seen: Vec<String> = alloc::vec![url.to_text()];
    let mut conn: Option<Conn> = None;
    let fail = |error: Error, url: &Url, redirects: &Vec<Url>| Failure {
        error,
        url: url.clone(),
        redirects: redirects.clone(),
    };
    loop {
        let reuse = conn
            .as_ref()
            .is_some_and(|c| c.scheme == url.scheme && c.host == url.host && c.port == url.port);
        if !reuse {
            match open(net, &url, o) {
                Ok(c) => conn = Some(c),
                Err(e) => return Err(fail(e, &url, &redirects)),
            }
        }
        let c = conn.as_mut().unwrap();
        let (got, after) = match exchange(net, c, &url, o, deadline) {
            Ok(x) => x,
            Err(e) => return Err(fail(e, &url, &redirects)),
        };
        let Some(((status, reason, headers), body)) = got else {
            if reuse {
                // The kept-alive connection had closed: once more on a new one.
                conn = None;
                continue;
            }
            return Err(fail(
                Error::Malformed("the server closed the connection without answering"),
                &url,
                &redirects,
            ));
        };
        if after == After::Close {
            conn = None;
        }
        if is_redirect(status)
            && let Some(loc) = header(&headers, "location")
        {
            let next = match url.join(loc) {
                Ok(n) => n,
                Err(e) => return Err(fail(Error::Url(e), &url, &redirects)),
            };
            let key = next.to_text();
            if seen.contains(&key) {
                return Err(fail(Error::RedirectLoop(key), &url, &redirects));
            }
            if redirects.len() >= o.max_redirects {
                return Err(fail(
                    Error::TooManyRedirects(o.max_redirects),
                    &url,
                    &redirects,
                ));
            }
            seen.push(key);
            redirects.push(next.clone());
            url = next;
            continue;
        }
        let wire_bytes = body.len();
        let (body, encoding) = match decode_body(body, &headers, o.max_body) {
            Ok(x) => x,
            Err(e) => return Err(fail(e, &url, &redirects)),
        };
        return Ok(Response {
            status,
            reason,
            headers,
            body,
            url,
            redirects,
            wire_bytes,
            encoding,
        });
    }
}
