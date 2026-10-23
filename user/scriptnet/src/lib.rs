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

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use browserkit::cookies::Context;
use browserkit::netpolicy::{
    Begin, Config, Credentials, CacheMode, ErrorKind, Exchange, Guard, Headers, Method, Mode,
    NetError, Next, Page, PreflightCache, Redirect, ReferrerInit, ReferrerPolicy, RequestInit,
    ResponseMeta, ResponseType,
};
use browserkit::netpolicy::exchange::BodyInfo;
use milkweb::http::{self, Net};
use milkweb::url::Url;
use webengine::{
    CredentialsMode, NetFailure, NetFailureKind, NetHead, NetKind, NetRequest, RedirectMode,
    RequestMode, ResponseKind,
};

#[cfg(test)]
mod tests;

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
    /// Empty for an opaque response and for HEAD.
    pub body: Vec<u8>,
    /// Requests made on the wire (the preflight and the redirects count).
    pub hops: u32,
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
    init.body = (!req.body.is_empty()).then(|| BodyInfo {
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

    fn progress(&mut self, got: usize, total: Option<usize>) {
        if self.final_hop {
            self.events.progress(got, total);
        }
    }

    fn stop(&mut self) -> bool {
        if self.error.is_some() {
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
    net: &mut dyn Net,
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
            return Ok(Done { head, body, hops: 0 });
        }
        Begin::Blob { url } => {
            return Err(Failed::net(
                NetFailureKind::Network,
                format!("Can't fetch {url}: blob addresses are served by the page"),
            ));
        }
    };
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
            let resp = match http::fetch(net, &wreq, &opts, &mut nh) {
                Ok(r) => r,
                Err(f) => {
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
        };
        hops += 1;
        let result = http::fetch(net, &wreq, &opts, &mut hooks);
        let (meta, redirect, error, stopped) = (hooks.meta.take(), hooks.redirect, hooks.error.take(), hooks.stopped);
        drop(hooks);
        if let Some(e) = error {
            return Err(Failed::policy(&e));
        }
        let resp = match result {
            Ok(r) => r,
            Err(f) => return Err(network_failure(&f, hop.method.as_str(), stopped, total_ms)),
        };
        if redirect {
            continue;
        }
        let Some(meta) = meta else {
            return Err(Failed::net(
                NetFailureKind::Network,
                format!("{} {} failed: no answer", hop.method.as_str(), hop.url.to_text()),
            ));
        };
        let head = head_of(&meta);
        let body = if meta.hide_body || hop.method.as_str() == "HEAD" {
            Vec::new()
        } else {
            resp.body
        };
        return Ok(Done { head, body, hops });
    }
}
