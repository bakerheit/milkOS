//! A page's script request, run: from the [`NetRequest`] a script made to the answer the page
//! gets, hop by hop, with every rule decided by `browserkit::netpolicy` and every byte moved
//! by milkweb's HTTP client.
//!
//! ```text
//!   NetRequest ──request_init──▶ netpolicy::RequestInit ──Exchange::start──▶ Exchange
//!      loop {  next_hop ──▶ [preflight OPTIONS ──▶ on_preflight_response]
//!              the request itself (milkweb::http::fetch, one hop, redirects not followed)
//!                 └─ head arrives: Exchange::on_response (CORS check, cookies stored,
//!                    redirect or final) ── the page's NetHead goes out at once ──┐
//!              Redirect ─▶ next hop          Done ─▶ the body, to the end ◀───────┘
//!      }
//! ```
//!
//! The caller (the Browser's worker thread, or a test) supplies the [`Net`] (sockets), the
//! cookie jar ([`Cookies`]), the preflight cache ([`Preflights`]), the clock and an
//! [`Events`] sink that hears the head and the progress as they happen and can cancel.
//! Nothing here panics on what a server sends; each failure comes back as a
//! [`Failed`] with the line the console shows.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use browserkit::cookies::Context;
use browserkit::netpolicy::{
    Hop, Begin, Config, Credentials, CacheMode, ErrorKind, Exchange, Guard, Headers, Method, Mode,
    NetError, Next, Page, PreflightCache, Redirect, ReferrerInit, ReferrerPolicy, RequestInit,
    ResponseMeta, ResponseType,
};
use browserkit::netpolicy::exchange::BodyInfo;
use milkweb::http::{self, Net, Transport};
use milkweb::url::Url;
use webengine::{
    CredentialsMode, NetFailure, NetFailureKind, NetHead, NetKind, NetRequest, RedirectMode,
    RequestMode, ResponseKind,
};

pub mod frame;

#[cfg(test)]
mod tests;

/// The network, as script requests need it: a [`Net`] that can also say what a name resolves
/// to and connect to an address, so the policy can look at the address before anything is
/// sent (a public page may not be tricked into reaching the local network by a name that
/// points inward).
pub trait ScriptNet: Net {
    /// The addresses `host` names (4 or 16 bytes each), in the order to try them.
    fn resolve(&mut self, host: &str) -> Result<Vec<Vec<u8>>, http::Error>;
    /// Connect to one of them (within `timeout_ms`).
    fn connect_ip(
        &mut self,
        ip: &[u8],
        port: u16,
        timeout_ms: u64,
    ) -> Result<Box<dyn Transport>, http::Error>;
}

/// [`ScriptNet`] seen as a [`Net`] for one hop: every address is checked by the hop before
/// it is connected to.
struct Guarded<'a> {
    net: &'a mut dyn ScriptNet,
    hop: &'a Hop,
    refused: &'a RefCell<Option<NetError>>,
}

impl Net for Guarded<'_> {
    fn connect(&mut self, host: &str, port: u16, timeout_ms: u64) -> Result<Box<dyn Transport>, http::Error> {
        let addrs = self.net.resolve(host)?;
        let mut last = http::Error::Connect(format!("{host}: no address"));
        let mut refused = None;
        for a in addrs {
            match self.hop.check_resolved(&a) {
                Err(e) => refused = Some(e),
                Ok(()) => match self.net.connect_ip(&a, port, timeout_ms) {
                    Ok(t) => return Ok(t),
                    Err(e) => last = e,
                },
            }
        }
        if let Some(e) = refused {
            // Only the refusal matters if nothing else connected.
            last = http::Error::Connect(format!("{e}"));
            *self.refused.borrow_mut() = Some(e);
        }
        Err(last)
    }

    fn now_ms(&mut self) -> u64 {
        self.net.now_ms()
    }

    fn has_tls(&self) -> bool {
        self.net.has_tls()
    }

    fn secure(&mut self, host: &str, stream: Box<dyn Transport>) -> Result<Box<dyn Transport>, http::Error> {
        self.net.secure(host, stream)
    }
}

/// The cookie jar, as the requests use it (the Browser's is shared with page loads).
pub trait Cookies {
    /// The `Cookie` header for a request to `url` (None: none goes).
    fn header(&self, url: &Url, post: bool, now_secs: i64, ctx: &Context) -> Option<String>;
    /// A response's Set-Cookie headers.
    fn store(&self, url: &Url, headers: &[(String, String)], now_secs: i64, ctx: &Context);
}

/// The preflight cache, shared by all of a Browser's script requests.
pub trait Preflights {
    fn with(&self, f: &mut dyn FnMut(&mut PreflightCache));
}

/// What the caller hears while a request runs.
pub trait Events {
    /// The final response's head, as soon as it is known (the body is still coming).
    fn head(&mut self, head: &NetHead);
    /// Body bytes so far.
    fn progress(&mut self, got: usize, total: Option<usize>);
    /// Asked before every read: true ends the request quietly (the page cancelled it).
    fn cancelled(&mut self) -> bool;
    /// The page reads the body in pieces ([`Events::chunk`]) instead of getting it whole. Asked
    /// when the final response's head is in.
    fn wants_stream(&mut self) -> bool {
        false
    }
    /// A piece of a streamed body. The caller may block here (the connection waits: that is
    /// the backpressure) and says false when the page has cancelled (the request ends).
    fn chunk(&mut self, data: &[u8]) -> bool {
        let _ = data;
        true
    }
}

/// Everything a request needs besides the request.
pub struct Env<'a> {
    pub cfg: &'a Config,
    pub page: &'a Page,
    pub jar: &'a dyn Cookies,
    pub cache: &'a dyn Preflights,
    /// Seconds since 1970 (cookies' clock).
    pub now_secs: &'a dyn Fn() -> i64,
}

/// A request that finished.
#[derive(Debug)]
pub struct Done {
    pub head: NetHead,
    /// Empty for an opaque response and for HEAD, and for a streamed body (it went to
    /// [`Events::chunk`] as it came).
    pub body: Vec<u8>,
    /// Requests made on the wire (the preflight and the redirects count).
    pub hops: u32,
    /// The body was streamed: how many bytes of it there were.
    pub streamed: Option<usize>,
}

/// A request that did not: why, for the page (kind) and the console (message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failed {
    pub failure: NetFailure,
    /// The page cancelled it (no event goes back).
    pub cancelled: bool,
}

impl Failed {
    fn net(kind: NetFailureKind, message: String) -> Failed {
        Failed {
            failure: NetFailure { kind, message },
            cancelled: false,
        }
    }

    fn policy(e: &NetError) -> Failed {
        let kind = match e.kind() {
            ErrorKind::Timeout => NetFailureKind::Timeout,
            ErrorKind::Type | ErrorKind::Network => NetFailureKind::Blocked,
            ErrorKind::Abort => NetFailureKind::Network,
        };
        Failed::net(kind, format!("{e}"))
    }
}

fn mode_of(m: RequestMode) -> Mode {
    match m {
        RequestMode::SameOrigin => Mode::SameOrigin,
        RequestMode::Cors => Mode::Cors,
        RequestMode::NoCors => Mode::NoCors,
        RequestMode::Navigate => Mode::Navigate,
    }
}

fn credentials_of(c: CredentialsMode) -> Credentials {
    match c {
        CredentialsMode::Omit => Credentials::Omit,
        CredentialsMode::SameOrigin => Credentials::SameOrigin,
        CredentialsMode::Include => Credentials::Include,
    }
}

fn redirect_of(r: RedirectMode) -> Redirect {
    match r {
        RedirectMode::Follow => Redirect::Follow,
        RedirectMode::Error => Redirect::Error,
        RedirectMode::Manual => Redirect::Manual,
    }
}

/// The request as the policy wants to see it.
pub fn request_init(req: &NetRequest) -> Result<RequestInit, NetError> {
    let method = Method::parse_script(&req.method)?;
    let guard = if req.mode == RequestMode::NoCors {
        Guard::RequestNoCors
    } else {
        Guard::Request
    };
    let mut headers = Headers::new(guard);
    for (k, v) in &req.headers {
        headers.append(k, v)?;
    }
    let mut init = RequestInit::new(&req.url);
    init.method = method;
    init.headers = headers;
    init.body = (!req.body.is_empty()).then_some(BodyInfo {
        len: req.body.len(),
        content_type: None,
    });
    init.mode = mode_of(req.mode);
    init.credentials = credentials_of(req.credentials);
    init.redirect = redirect_of(req.redirect);
    init.cache = CacheMode::parse(&req.cache).unwrap_or_default();
    init.referrer = ReferrerInit::from_text(&req.referrer);
    init.referrer_policy = ReferrerPolicy::parse(&req.referrer_policy);
    init.keepalive = req.kind == NetKind::Beacon;
    init.force_preflight = req.force_preflight;
    init.timeout_ms = (req.timeout_ms > 0).then_some(req.timeout_ms as u64);
    Ok(init)
}

fn head_of(m: &ResponseMeta) -> NetHead {
    NetHead {
        status: m.status,
        status_text: m.status_text.clone(),
        headers: m.headers.clone(),
        url: m.url.clone(),
        redirected: m.redirected,
        kind: match m.kind {
            ResponseType::Basic => ResponseKind::Basic,
            ResponseType::Cors => ResponseKind::Cors,
            ResponseType::Opaque => ResponseKind::Opaque,
            ResponseType::OpaqueRedirect => ResponseKind::OpaqueRedirect,
        },
    }
}

/// The hooks for one hop: the policy hears the head, the page hears the progress.
struct Hooks<'a> {
    ex: &'a mut Exchange,
    jar: &'a dyn Cookies,
    now_secs: &'a dyn Fn() -> i64,
    events: &'a mut dyn Events,
    url: Url,
    cookie: Option<String>,
    max_body: usize,
    /// What the head turned out to be.
    meta: Option<ResponseMeta>,
    redirect: bool,
    error: Option<NetError>,
    /// The hop is the one whose body the page gets (progress counts).
    final_hop: bool,
    stopped: bool,
    /// A module script's request: the final response must be a success with a module's type.
    module: bool,
    /// ... or a classic worker script's: a success with a JavaScript type.
    script: bool,
    /// Why the final response is no module (the request then ends there).
    module_error: Option<String>,
    /// Bytes handed to [`Events::chunk`].
    streamed: usize,
    /// The page reads the body in pieces (and may: a fetch).
    stream: bool,
}

impl http::Hooks for Hooks<'_> {
    fn cookie(&mut self, _url: &Url, _m: http::Method) -> Option<String> {
        self.cookie.clone()
    }

    fn response_head(
        &mut self,
        _url: &Url,
        status: u16,
        reason: &str,
        headers: &[(String, String)],
    ) -> Option<usize> {
        match self.ex.on_response(status, reason, headers) {
            Ok(out) => {
                if let Some(ctx) = &out.store_cookies {
                    self.jar.store(&self.url, headers, (self.now_secs)(), ctx);
                }
                match out.next {
                    Next::Done(meta) => {
                        self.final_hop = true;
                        if self.module {
                            // Strict checks for module scripts (HTML "fetch a single module
                            // script"): a success status and a JavaScript (or JSON) type. The
                            // page is told nothing; the body is not read.
                            let ctype = headers
                                .iter()
                                .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                                .map(|(_, v)| v.as_str());
                            let verdict = if self.script {
                                webengine::script::modules::check_script_head(meta.status, reason, ctype)
                            } else {
                                webengine::script::modules::check_module_head(meta.status, reason, ctype)
                            };
                            if let Err(why) = verdict {
                                self.module_error = Some(why);
                                return Some(0);
                            }
                        }
                        self.events.head(&head_of(&meta));
                        self.meta = Some(meta);
                    }
                    Next::Redirect { .. } => self.redirect = true,
                }
            }
            Err(e) => self.error = Some(e),
        }
        Some(self.max_body)
    }

    fn stream(&mut self, _url: &Url, _status: u16, _headers: &[(String, String)]) -> bool {
        self.final_hop
            && !self.redirect
            && !self.module
            && self.error.is_none()
            && self.module_error.is_none()
            && self.stream
            && self.meta.as_ref().is_some_and(|m| !m.hide_body)
    }

    fn body(&mut self, data: &[u8]) -> bool {
        self.streamed += data.len();
        if self.events.chunk(data) {
            true
        } else {
            self.stopped = true;
            false
        }
    }

    fn progress(&mut self, got: usize, total: Option<usize>) {
        if self.final_hop {
            self.events.progress(got, total);
        }
    }

    fn stop(&mut self) -> bool {
        if self.error.is_some() || self.module_error.is_some() {
            return true;
        }
        if self.events.cancelled() {
            self.stopped = true;
            return true;
        }
        false
    }
}

/// One request on the wire (a hop or a preflight): milkweb's request from the policy's.
fn wire_request(
    method: &Method,
    url: &Url,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> (http::Request, Option<String>, Option<String>) {
    let mut accept = None;
    let mut agent = None;
    let mut content_type = String::new();
    let mut rest: Vec<(String, String)> = Vec::new();
    for (k, v) in headers {
        match k.as_str() {
            "accept" => accept = Some(v.clone()),
            "user-agent" => agent = Some(v.clone()),
            "content-type" => content_type = v.clone(),
            _ => rest.push((k.clone(), v.clone())),
        }
    }
    let mut req = http::Request::get(url);
    req.headers = rest;
    match method.as_str() {
        "GET" => {}
        "POST" => req.method = http::Method::Post,
        other => req.verb = Some(String::from(other)),
    }
    if let Some(b) = body {
        req.body = b.to_vec();
        req.content_type = content_type;
    }
    (req, accept, agent)
}

fn options(cfg: &Config, remaining_ms: u64, accept: Option<String>, agent: Option<String>) -> http::Options {
    let mut o = http::Options {
        timeout_ms: remaining_ms.max(1),
        connect_timeout_ms: 10_000.min(remaining_ms.max(1)),
        max_header: cfg.limits.max_response_head,
        max_body: cfg.limits.max_response_body,
        follow_redirects: false,
        ..http::Options::default()
    };
    o.accept = accept.unwrap_or_else(|| String::from("*/*"));
    if let Some(a) = agent {
        o.user_agent = a;
    }
    o
}

fn network_failure(f: &http::Failure, method: &str, cancelled: bool, timeout_ms: u64) -> Failed {
    use http::Error as E;
    let (kind, why) = match &f.error {
        E::Timeout => (
            NetFailureKind::Timeout,
            format!("the request took longer than {} s", timeout_ms / 1000),
        ),
        E::TooBig(n) => (
            NetFailureKind::TooBig,
            format!("the response is bigger than the limit ({} KiB)", n / 1024),
        ),
        E::Cancelled => (NetFailureKind::Network, String::from("the request was cancelled")),
        e => (NetFailureKind::Network, format!("{e}")),
    };
    Failed {
        failure: NetFailure {
            kind,
            message: format!("{method} {} failed: {why}", f.url.to_text()),
        },
        cancelled,
    }
}

/// Run `req` to its end. `events` hears the head and the progress on the way.
pub fn run(
    net: &mut dyn ScriptNet,
    env: &Env<'_>,
    req: &NetRequest,
    events: &mut dyn Events,
) -> Result<Done, Failed> {
    let init = request_init(req).map_err(|e| Failed::policy(&e))?;
    let method = init.method.clone();
    let begin = Exchange::start(env.cfg, env.page, init).map_err(|e| Failed::policy(&e))?;
    let mut ex = match begin {
        Begin::Network(ex) => ex,
        Begin::Data(meta, body) => {
            let head = head_of(&meta);
            events.head(&head);
            return Ok(Done { head, body, hops: 0, streamed: None });
        }
        Begin::Blob { url } => {
            return Err(Failed::net(
                NetFailureKind::Network,
                format!("Can't fetch {url}: blob addresses are served by the page"),
            ));
        }
    };
    // A fetch whose page reads in pieces is not held to the limit on a whole response.
    let stream = matches!(req.kind, NetKind::Fetch) && events.wants_stream();
    ex.set_streaming(stream);
    let started = net.now_ms();
    let total_ms = ex.timeout_ms();
    let deadline = started.saturating_add(total_ms);
    let mut hops = 0u32;
    loop {
        let now = net.now_ms();
        if now >= deadline {
            return Err(Failed::net(
                NetFailureKind::Timeout,
                format!("{} {} failed: the request took longer than {} s", method.as_str(), ex.url().to_text(), total_ms / 1000),
            ));
        }
        let mut hop_result = None;
        env.cache.with(&mut |c| hop_result = Some(ex.next_hop(c, now)));
        let hop = hop_result.unwrap_or(Err(NetError::OutOfOrder("no cache")))
            .map_err(|e| Failed::policy(&e))?;
        // The preflight, if the policy wants one.
        if let Some(pf) = &hop.preflight {
            let (wreq, accept, agent) = wire_request(&Method::options(), &pf.url, &pf.headers, None);
            let mut wreq = wreq;
            wreq.verb = Some(String::from("OPTIONS"));
            let left = deadline.saturating_sub(net.now_ms());
            let opts = options(env.cfg, left, accept, agent);
            let mut nh = http::NoHooks;
            hops += 1;
            let refused = RefCell::new(None);
            let resp = match http::fetch(&mut Guarded { net: &mut *net, hop: &hop, refused: &refused }, &wreq, &opts, &mut nh) {
                Ok(r) => r,
                Err(f) => {
                    if let Some(e) = refused.into_inner() {
                        return Err(Failed::policy(&e));
                    }
                    return Err(network_failure(&f, "OPTIONS", false, total_ms));
                }
            };
            let now = net.now_ms();
            let mut res = Ok(());
            env.cache.with(&mut |c| {
                res = ex.on_preflight_response(c, now, resp.status, &resp.headers);
            });
            res.map_err(|e| Failed::policy(&e))?;
        }
        // The request itself.
        let body = hop.send_body.then_some(req.body.as_slice());
        let (wreq, accept, agent) = wire_request(&hop.method, &hop.url, &hop.headers, body);
        let cookie = hop.cookies.as_ref().and_then(|ctx| {
            env.jar
                .header(&hop.url, !hop.method.is_get(), (env.now_secs)(), ctx)
        });
        let left = deadline.saturating_sub(net.now_ms());
        let opts = options(env.cfg, left, accept, agent);
        let mut hooks = Hooks {
            ex: &mut ex,
            jar: env.jar,
            now_secs: env.now_secs,
            events: &mut *events,
            url: hop.url.clone(),
            cookie,
            max_body: env.cfg.limits.max_response_body,
            meta: None,
            redirect: false,
            error: None,
            final_hop: false,
            stopped: false,
            module: matches!(req.kind, NetKind::Module | NetKind::Script),
            script: req.kind == NetKind::Script,
            module_error: None,
            streamed: 0,
            stream,
        };
        hops += 1;
        let refused = RefCell::new(None);
        let result = http::fetch(&mut Guarded { net: &mut *net, hop: &hop, refused: &refused }, &wreq, &opts, &mut hooks);
        let (meta, redirect, error, stopped) = (hooks.meta.take(), hooks.redirect, hooks.error.take(), hooks.stopped);
        let module_error = hooks.module_error.take();
        let streamed_bytes = hooks.streamed;
        drop(hooks);
        if let Some(e) = error {
            return Err(Failed::policy(&e));
        }
        if let Some(why) = module_error {
            // (The page's console says "Failed to load module script <url>: " before this.)
            return Err(Failed::net(NetFailureKind::Network, why));
        }
        let resp = match result {
            Ok(r) => r,
            Err(f) => {
                if let Some(e) = refused.into_inner() {
                    return Err(Failed::policy(&e));
                }
                return Err(network_failure(&f, hop.method.as_str(), stopped, total_ms));
            }
        };
        if redirect {
            continue;
        }
        // The page cancelled while its body was coming.
        if resp.streamed && resp.cut && stopped {
            return Err(Failed {
                failure: NetFailure {
                    kind: NetFailureKind::Network,
                    message: format!("{} {} failed: the request was cancelled", hop.method.as_str(), hop.url.to_text()),
                },
                cancelled: true,
            });
        }
        let Some(meta) = meta else {
            return Err(Failed::net(
                NetFailureKind::Network,
                format!("{} {} failed: no answer", hop.method.as_str(), hop.url.to_text()),
            ));
        };
        let head = head_of(&meta);
        let streamed = resp.streamed.then_some(streamed_bytes);
        let body = if meta.hide_body || hop.method.as_str() == "HEAD" {
            Vec::new()
        } else {
            resp.body
        };
        return Ok(Done { head, body, hops, streamed });
    }
}
