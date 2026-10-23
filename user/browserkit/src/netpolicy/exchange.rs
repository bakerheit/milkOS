//! One script request from start to finish: [`Exchange`] holds the state of a `fetch` or
//! XHR (the request as the page made it, the redirect chain so far) and answers the
//! questions the Browser's networking asks at each step, so the bindings never apply a
//! rule themselves. The pieces it composes are public too (see the sibling modules).
//!
//! ```text
//!   Exchange::start(cfg, page, init)  -> Begin::Network(ex) | Begin::Data(..) | Begin::Blob(..)
//!   loop {
//!       let hop = ex.next_hop(&mut preflight_cache, now_ms)?;
//!       if let Some(pf) = &hop.preflight {
//!           // send OPTIONS pf.url with pf.headers (no cookies, no body), then:
//!           ex.on_preflight_response(&mut preflight_cache, now_ms, status, &headers)?;
//!       }
//!       // send hop.method hop.url with hop.headers (+ the Cookie header if hop.cookies is Some),
//!       // the body if hop.send_body; then:
//!       let out = ex.on_response(status, &reason, &headers)?;
//!       if let Some(ctx) = out.store_cookies { jar.set_all(&hop.url, &headers, now, &ctx) }
//!       match out.next { Next::Done(meta) => break, Next::Redirect { .. } => continue }
//!   }
//! ```
//! Any `Err` ends the exchange (a network error for the page; the error's `Display` is
//! for the console, `script_message()` for the page).

use alloc::string::String;
use alloc::vec::Vec;

use milkweb::url::Url;

use super::cors::{
    PreflightCache, PreflightRequest, ResponseType, build_preflight, corp_check, cors_check,
    filter_basic, filter_cors, preflight_check,
};
use super::error::NetError;
use super::headers::{
    Guard, Headers, Method, combined_value, cors_unsafe_request_header_names, is_forbidden_method,
};
use super::limits::Config;
use super::origin::{
    AddressSpace, Origin, Target, address_space_of_host, address_space_of_ip, is_loopback_host,
    is_potentially_trustworthy, resolve_url,
};
use super::referrer::{ReferrerInit, ReferrerPolicy, referrer_header};
use crate::cookies;

macro_rules! text_enum {
    ($(#[$m:meta])* $name:ident { $($var:ident = $text:expr),+ $(,)? } default $def:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum $name { $($var),+ }
        impl $name {
            pub fn as_str(self) -> &'static str { match self { $($name::$var => $text),+ } }
            /// The option's exact text (lower case); `None` for anything else.
            pub fn parse(s: &str) -> Option<$name> { match s { $($text => Some($name::$var),)+ _ => None } }
        }
        impl Default for $name { fn default() -> $name { $name::$def } }
    };
}

text_enum! {
    /// The Fetch `mode`.
    Mode { SameOrigin = "same-origin", Cors = "cors", NoCors = "no-cors", Navigate = "navigate" } default Cors
}
text_enum! {
    /// The Fetch `credentials`.
    Credentials { Omit = "omit", SameOrigin = "same-origin", Include = "include" } default SameOrigin
}
text_enum! {
    /// The Fetch `redirect`.
    Redirect { Follow = "follow", Error = "error", Manual = "manual" } default Follow
}
text_enum! {
    /// The Fetch `cache`. There is no HTTP cache: the modes only set request headers, and
    /// `only-if-cached` always fails.
    CacheMode { Default = "default", NoStore = "no-store", Reload = "reload", NoCache = "no-cache",
        ForceCache = "force-cache", OnlyIfCached = "only-if-cached" } default Default
}

/// The page making the request.
#[derive(Clone, Debug)]
pub struct Page {
    pub url: Url,
    pub origin: Origin,
    /// Where the page itself came from, for the private-network rule. [`Page::new`] guesses
    /// from the host text; the Browser sets the real one when it knows the address the page
    /// was fetched from (a page on an intranet name that resolves to 10.x is `Private`).
    pub address_space: AddressSpace,
    /// The page's referrer policy (`<meta name=referrer>` / header; the default if none).
    pub referrer_policy: ReferrerPolicy,
}

impl Page {
    pub fn new(url: Url) -> Page {
        Page {
            origin: Origin::of(&url),
            address_space: address_space_of_host(&url.host),
            url,
            referrer_policy: ReferrerPolicy::DEFAULT,
        }
    }
}

/// What the request body is, as far as the rules care (the bytes stay with the bindings).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BodyInfo {
    pub len: usize,
    /// The Content-Type the body type implies (`text/plain;charset=UTF-8` for a string,
    /// a Blob's type, the form encodings), used if the headers have none.
    pub content_type: Option<String>,
}

/// A request as the page asked for it.
#[derive(Clone, Debug)]
pub struct RequestInit {
    /// The address text, resolved against the page.
    pub url: String,
    pub method: Method,
    /// The page's headers (any guard; every entry is checked again here).
    pub headers: Headers,
    pub body: Option<BodyInfo>,
    pub mode: Mode,
    pub credentials: Credentials,
    pub redirect: Redirect,
    pub cache: CacheMode,
    pub referrer: ReferrerInit,
    pub referrer_policy: Option<ReferrerPolicy>,
    pub keepalive: bool,
    /// Always preflight (an XHR with `upload` listeners).
    pub force_preflight: bool,
    /// The page's own timeout (XHR `timeout`), if any.
    pub timeout_ms: Option<u64>,
}

impl RequestInit {
    /// A plain `fetch(url)`: GET, cors, same-origin credentials, follow.
    pub fn new(url: &str) -> RequestInit {
        RequestInit {
            url: String::from(url),
            method: Method::get(),
            headers: Headers::new(super::headers::Guard::Request),
            body: None,
            mode: Mode::Cors,
            credentials: Credentials::SameOrigin,
            redirect: Redirect::Follow,
            cache: CacheMode::Default,
            referrer: ReferrerInit::Client,
            referrer_policy: None,
            keepalive: false,
            force_preflight: false,
            timeout_ms: None,
        }
    }
}

impl RequestInit {
    /// `navigator.sendBeacon(url, data)`: a keepalive POST with credentials. `content_type`
    /// is the body's own (None for a string body without type, which the caller gives
    /// `text/plain;charset=UTF-8`).
    pub fn beacon(url: &str, len: usize, content_type: Option<&str>) -> RequestInit {
        let mut i = RequestInit::new(url);
        i.method = Method::post();
        i.credentials = Credentials::Include;
        i.keepalive = true;
        i.body = Some(BodyInfo {
            len,
            content_type: content_type.map(String::from),
        });
        i
    }
}

/// The checks `new Request(...)` makes when the request is built, before any fetch: only
/// the errors that are the page's own doing ([`ErrorKind::Type`](super::error::ErrorKind));
/// a request the network rules would stop later is still a valid `Request`.
pub fn check_construct(cfg: &Config, page: &Page, init: &RequestInit) -> Result<(), NetError> {
    match Exchange::start(cfg, page, init.clone()) {
        Err(e) if e.kind() == super::error::ErrorKind::Type => Err(e),
        _ => Ok(()),
    }
}

/// What the page gets of a response (everything but the body bytes, which the bindings
/// hold: they must drop them when `hide_body` is set).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ResponseMeta {
    pub kind: ResponseType,
    /// 0 for opaque responses.
    pub status: u16,
    pub status_text: String,
    /// Filtered, lower-case names, in the order received.
    pub headers: Vec<(String, String)>,
    /// The final address ("" for an opaque response).
    pub url: String,
    pub redirected: bool,
    /// The page may not see the body (opaque and opaque-redirect responses).
    pub hide_body: bool,
}

impl ResponseMeta {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `getResponseHeader` / `headers.get`: all values of `name` joined with ", ".
    pub fn header(&self, name: &str) -> Option<String> {
        super::headers::combined_value(&self.headers, name)
    }
}

/// What to put on the wire next.
#[derive(Clone, Debug)]
pub struct Hop {
    pub method: Method,
    pub url: Url,
    /// Lower-case names: the page's headers plus Accept, Origin, Referer, Sec-Fetch-* and
    /// the cache headers. Not included (the transport adds them): Host, Content-Length,
    /// User-Agent, Accept-Encoding, Connection, and Cookie (see `cookies`).
    pub headers: Vec<(String, String)>,
    /// Send the request body (false after a redirect that turned the request into a GET).
    pub send_body: bool,
    /// `Some(ctx)`: ask the jar for the Cookie header with `jar.header(&url, !method.is_get(),
    /// now, &ctx)`. `None`: send no cookies.
    pub cookies: Option<cookies::Context>,
    /// Send this OPTIONS request first (and call `on_preflight_response`).
    pub preflight: Option<PreflightRequest>,
    /// The whole exchange's time limit in ms (preflight and redirects included).
    pub timeout_ms: u64,
    /// The most private address space this hop's host may resolve to: the page's own (or
    /// anything, when `block_private_network` is off). A name that resolves to something
    /// more private is a DNS-rebinding attempt: the transport's connect step calls
    /// [`Hop::check_resolved`] with the address it got and refuses on `Err`.
    pub max_address_space: AddressSpace,
    /// The page's origin, for the error text.
    pub page_origin: String,
}

impl Hop {
    /// Is it all right to connect to `ip` (4 or 16 bytes, as looked up for this hop's host)?
    pub fn check_resolved(&self, ip: &[u8]) -> Result<(), NetError> {
        match address_space_of_ip(ip) {
            Some(space) if self.max_address_space.may_reach(space) => Ok(()),
            _ => Err(NetError::PrivateNetwork {
                page: self.page_origin.clone(),
                url: self.url.to_text(),
            }),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Next {
    /// The final response.
    Done(ResponseMeta),
    /// Follow the redirect: call `next_hop` again. (`to` is for the log.)
    Redirect { to: Url },
}

#[derive(Clone, Debug)]
pub struct Outcome {
    /// `Some(ctx)`: feed this response's Set-Cookie headers to the jar with this context.
    pub store_cookies: Option<cookies::Context>,
    pub next: Next,
}

/// What `start` found the address to be.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Begin {
    Network(Exchange),
    /// A `data:` address: the response, and the decoded bytes. No network.
    Data(ResponseMeta, Vec<u8>),
    /// A `blob:` address of this page's own origin: the bindings look it up. GET only.
    Blob {
        url: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// A hop is to be asked for.
    Ready,
    /// The preflight is out.
    AwaitPreflight,
    /// The request is out.
    AwaitResponse,
    Done,
}

/// How far from the page every address so far in the redirect chain was (the
/// `Sec-Fetch-Site` values).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum SiteClass {
    SameOrigin,
    SameSite,
    CrossSite,
}

impl SiteClass {
    fn name(self) -> &'static str {
        match self {
            SiteClass::SameOrigin => "same-origin",
            SiteClass::SameSite => "same-site",
            SiteClass::CrossSite => "cross-site",
        }
    }
}

/// One script request in flight (see the module docs).
#[derive(Clone, Debug)]
pub struct Exchange {
    cfg: Config,
    page: Page,
    method: Method,
    /// The page's headers as checked (lower-case names); the headers the browser adds are
    /// put on in `next_hop`.
    user_headers: Vec<(String, String)>,
    has_body: bool,
    send_body: bool,
    mode: Mode,
    credentials: Credentials,
    redirect: Redirect,
    cache: CacheMode,
    /// The address the `Referer` is made from (None: no referrer).
    referrer: Option<Url>,
    policy: ReferrerPolicy,
    force_preflight: bool,
    timeout_ms: u64,

    url: Url,
    redirects: usize,
    /// Basic, Cors or Opaque; only ever goes up.
    tainting: ResponseType,
    /// Set when the chain went through another origin: the `Origin` header becomes `null`.
    tainted_origin: bool,
    /// Some address in the chain is not (schemeful) same-site with the page: no cookies
    /// go or are kept for the rest of the exchange.
    chain_cross_site: bool,
    class: SiteClass,
    state: State,
    hop_cookies: Option<cookies::Context>,
}

fn class_of(page: &Origin, url: &Url) -> SiteClass {
    let o = Origin::of(url);
    if page.same_origin(&o) {
        SiteClass::SameOrigin
    } else if page.same_site(&o) {
        SiteClass::SameSite
    } else {
        SiteClass::CrossSite
    }
}

/// The rules about one address a request is about to go to (the first, and each redirect).
fn check_target(cfg: &Config, page: &Page, mode: Mode, url: &Url) -> Result<(), NetError> {
    // Mixed content: a secure page may not ask for plain http (this machine excepted, which
    // the private-network rule below then covers).
    if page.url.scheme == milkweb::url::Scheme::Https
        && url.scheme == milkweb::url::Scheme::Http
        && !is_loopback_host(&url.host)
    {
        return Err(NetError::MixedContent {
            page: page.url.to_text(),
            url: url.to_text(),
        });
    }
    if mode == Mode::SameOrigin && !page.origin.same_origin(&Origin::of(url)) {
        return Err(NetError::SameOriginOnly {
            page: page.origin.serialize(),
            url: url.to_text(),
        });
    }
    if cfg.block_private_network
        && !page
            .address_space
            .may_reach(address_space_of_host(&url.host))
    {
        return Err(NetError::PrivateNetwork {
            page: page.url.to_text(),
            url: url.to_text(),
        });
    }
    Ok(())
}

/// Printable ASCII only, and short: a status text a page can safely show.
fn clean_reason(r: &str) -> String {
    r.chars()
        .filter(|c| (' '..='~').contains(c))
        .take(128)
        .collect()
}

impl Exchange {
    /// Check the request against every rule that needs no network, and set it up.
    pub fn start(cfg: &Config, page: &Page, init: RequestInit) -> Result<Begin, NetError> {
        let lim = &cfg.limits;
        if init.mode == Mode::Navigate {
            return Err(NetError::NavigateMode);
        }
        if init.cache == CacheMode::OnlyIfCached && init.mode != Mode::SameOrigin {
            return Err(NetError::CacheNeedsSameOrigin);
        }
        let method = init.method.clone();
        if is_forbidden_method(method.as_str()) {
            return Err(NetError::ForbiddenMethod(String::from(method.as_str())));
        }
        if init.mode == Mode::NoCors && !method.is_cors_safelisted() {
            return Err(NetError::NoCorsMethod(String::from(method.as_str())));
        }
        if let Some(b) = &init.body {
            if method.is_get_or_head() {
                return Err(NetError::BodyNotAllowed(String::from(method.as_str())));
            }
            if b.len > lim.max_request_body {
                return Err(NetError::RequestBodyTooLarge {
                    len: b.len,
                    max: lim.max_request_body,
                });
            }
            if init.keepalive && b.len > lim.max_keepalive_body {
                return Err(NetError::KeepaliveBodyTooLarge {
                    len: b.len,
                    max: lim.max_keepalive_body,
                });
            }
        }

        // The headers, checked again whatever guard they came with.
        let guard = if init.mode == Mode::NoCors {
            Guard::RequestNoCors
        } else {
            Guard::Request
        };
        let mut h = Headers::new(guard);
        for (n, v) in init.headers.raw() {
            h.append(n, v)?;
        }
        if let Some(b) = &init.body
            && let Some(ct) = &b.content_type
            && !h.has("content-type").unwrap_or(false)
        {
            // An unusable default (or one no-cors drops) is simply left out.
            let _ = h.append("content-type", ct);
        }
        if h.len() > lim.max_headers {
            return Err(NetError::TooManyHeaders {
                max: lim.max_headers,
            });
        }
        let total: usize = h.raw().iter().map(|(n, v)| n.len() + v.len() + 4).sum();
        if total > lim.max_headers_bytes {
            return Err(NetError::HeadersTooLarge {
                max: lim.max_headers_bytes,
            });
        }
        if h.raw().iter().any(|(_, v)| v.len() > lim.max_header_value) {
            return Err(NetError::HeadersTooLarge {
                max: lim.max_header_value,
            });
        }

        // The address.
        let target = resolve_url(&page.url, &init.url, lim)?;
        let url = match target {
            Target::Http(u) => u,
            Target::Data(d) => {
                if !method.is_get() {
                    return Err(NetError::SchemeMethod {
                        scheme: String::from("data"),
                        method: String::from(method.as_str()),
                    });
                }
                let text: String = init.url.trim().chars().take(lim.max_url).collect();
                return Ok(Begin::Data(
                    ResponseMeta {
                        kind: ResponseType::Basic,
                        status: 200,
                        status_text: String::from("OK"),
                        headers: alloc::vec![(String::from("content-type"), d.mime)],
                        url: text,
                        redirected: false,
                        hide_body: false,
                    },
                    d.body,
                ));
            }
            Target::Blob(text) => {
                if !method.is_get() {
                    return Err(NetError::SchemeMethod {
                        scheme: String::from("blob"),
                        method: String::from(method.as_str()),
                    });
                }
                // blob:https://host/uuid: the part after "blob:" names the origin that made it.
                let inner = text.get(5..).unwrap_or("");
                let same = Url::parse(inner)
                    .map(|u| page.origin.same_origin(&Origin::of(&u)) && inner.contains("://"))
                    .unwrap_or(false);
                if !same {
                    return Err(NetError::SameOriginOnly {
                        page: page.origin.serialize(),
                        url: text,
                    });
                }
                return Ok(Begin::Blob { url: text });
            }
        };
        check_target(cfg, page, init.mode, &url)?;
        if init.cache == CacheMode::OnlyIfCached {
            return Err(NetError::NotCached { url: url.to_text() });
        }

        // The referrer.
        let policy = init.referrer_policy.unwrap_or(page.referrer_policy);
        let referrer = match &init.referrer {
            ReferrerInit::NoReferrer => None,
            ReferrerInit::Client => Some(page.url.clone()),
            ReferrerInit::Url(s) => match resolve_url(&page.url, s, lim) {
                Ok(Target::Http(u)) if page.origin.same_origin(&Origin::of(&u)) => Some(u),
                _ => return Err(NetError::BadReferrer(s.chars().take(100).collect())),
            },
        };

        let mut ex = Exchange {
            cfg: cfg.clone(),
            page: page.clone(),
            method,
            user_headers: h.raw().to_vec(),
            has_body: init.body.is_some(),
            send_body: init.body.is_some(),
            mode: init.mode,
            credentials: init.credentials,
            redirect: init.redirect,
            cache: init.cache,
            referrer,
            policy,
            force_preflight: init.force_preflight,
            timeout_ms: cfg.effective_timeout(init.timeout_ms),
            url: url.clone(),
            redirects: 0,
            tainting: ResponseType::Basic,
            tainted_origin: false,
            chain_cross_site: false,
            class: SiteClass::SameOrigin,
            state: State::Ready,
            hop_cookies: None,
        };
        ex.enter(&url);
        Ok(Begin::Network(ex))
    }

    /// Take `url` into the chain: raise the tainting and the site class.
    fn enter(&mut self, url: &Url) {
        if self.tainting == ResponseType::Basic && !self.page.origin.same_origin(&Origin::of(url)) {
            self.tainting = if self.mode == Mode::NoCors {
                ResponseType::Opaque
            } else {
                ResponseType::Cors
            };
        }
        let c = class_of(&self.page.origin, url);
        if c > self.class {
            self.class = c;
        }
        if c == SiteClass::CrossSite {
            self.chain_cross_site = true;
        }
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn method(&self) -> &Method {
        &self.method
    }

    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    pub fn redirect_count(&self) -> usize {
        self.redirects
    }

    /// `Basic` (same origin), `Cors` or `Opaque` (no-cors, cross-origin) so far.
    pub fn tainting(&self) -> ResponseType {
        self.tainting
    }

    /// Does the request's credentials mode say "include"? (The CORS rules use the mode, not
    /// whether cookies actually go.)
    fn credentials_mode_include(&self) -> bool {
        self.credentials == Credentials::Include
    }

    fn cookie_context(&self) -> Option<cookies::Context> {
        let include = match self.credentials {
            Credentials::Include => true,
            Credentials::SameOrigin => self.tainting == ResponseType::Basic,
            Credentials::Omit => false,
        };
        if !include || self.chain_cross_site {
            return None;
        }
        Some(cookies::Context::Subresource {
            page_site: self.page.origin.site()?,
        })
    }

    fn origin_header(&self) -> Option<String> {
        let ser = if self.tainted_origin {
            String::from("null")
        } else {
            self.page.origin.serialize()
        };
        if self.tainting == ResponseType::Cors {
            return Some(ser);
        }
        if self.method.is_get_or_head() {
            return None;
        }
        match self.policy {
            ReferrerPolicy::NoReferrer => None,
            ReferrerPolicy::NoReferrerWhenDowngrade
            | ReferrerPolicy::StrictOrigin
            | ReferrerPolicy::StrictOriginWhenCrossOrigin => {
                let https_page = self.page.origin.scheme() == Some(milkweb::url::Scheme::Https);
                if https_page && self.url.scheme != milkweb::url::Scheme::Https {
                    None
                } else {
                    Some(ser)
                }
            }
            ReferrerPolicy::SameOrigin => self
                .page
                .origin
                .same_origin(&Origin::of(&self.url))
                .then_some(ser),
            _ => Some(ser),
        }
    }

    /// The origin the `Origin` header names right now: the page's, or `null` once the
    /// redirect chain has left it. The CORS checks compare against this.
    fn request_origin(&self) -> Origin {
        if self.tainted_origin {
            Origin::Opaque(0)
        } else {
            self.page.origin.clone()
        }
    }

    fn unsafe_names(&self) -> Vec<String> {
        cors_unsafe_request_header_names(&self.user_headers)
    }

    fn has_header(&self, name: &str) -> bool {
        self.user_headers.iter().any(|(k, _)| k == name)
    }

    /// The next request to send (the first one, or after a redirect).
    pub fn next_hop(&mut self, cache: &mut PreflightCache, now_ms: u64) -> Result<Hop, NetError> {
        if self.state != State::Ready {
            return Err(NetError::OutOfOrder("next_hop called out of order"));
        }
        let url = self.url.clone();
        let mut headers = self.user_headers.clone();
        if !self.has_header("accept") {
            headers.push((String::from("accept"), String::from("*/*")));
        }
        match self.cache {
            CacheMode::NoStore | CacheMode::Reload => {
                if !self.has_header("pragma") {
                    headers.push((String::from("pragma"), String::from("no-cache")));
                }
                if !self.has_header("cache-control") {
                    headers.push((String::from("cache-control"), String::from("no-cache")));
                }
            }
            CacheMode::NoCache if !self.has_header("cache-control") => {
                headers.push((String::from("cache-control"), String::from("max-age=0")));
            }
            _ => {}
        }
        let origin_hdr = self.origin_header();
        if let Some(o) = &origin_hdr {
            headers.push((String::from("origin"), o.clone()));
        }
        let referer = self
            .referrer
            .as_ref()
            .and_then(|r| referrer_header(self.policy, r, &url));
        if let Some(r) = &referer {
            headers.push((String::from("referer"), r.clone()));
        }
        if is_potentially_trustworthy(&url) {
            headers.push((String::from("sec-fetch-dest"), String::from("empty")));
            headers.push((
                String::from("sec-fetch-mode"),
                String::from(self.mode.as_str()),
            ));
            headers.push((
                String::from("sec-fetch-site"),
                String::from(self.class.name()),
            ));
        }

        let unsafe_names = self.unsafe_names();
        let need_preflight = self.tainting == ResponseType::Cors
            && (self.force_preflight
                || !self.method.is_cors_safelisted()
                || !unsafe_names.is_empty());
        let preflight = if need_preflight
            && !cache.covers(
                &self.request_origin(),
                &url,
                self.credentials_mode_include(),
                &self.method,
                &unsafe_names,
                now_ms,
            ) {
            Some(build_preflight(
                &url,
                &self.method,
                &unsafe_names,
                origin_hdr.as_deref(),
                referer.as_deref(),
            ))
        } else {
            None
        };
        self.state = if preflight.is_some() {
            State::AwaitPreflight
        } else {
            State::AwaitResponse
        };
        self.hop_cookies = self.cookie_context();
        Ok(Hop {
            method: self.method.clone(),
            url,
            headers,
            send_body: self.send_body && self.has_body,
            cookies: self.hop_cookies.clone(),
            preflight,
            timeout_ms: self.timeout_ms,
            max_address_space: if self.cfg.block_private_network {
                self.page.address_space
            } else {
                AddressSpace::Loopback
            },
            page_origin: self.page.origin.serialize(),
        })
    }

    fn cors_error(&self, kind: super::error::CorsError) -> NetError {
        NetError::Cors {
            kind,
            page_origin: self.page.origin.serialize(),
            url: self.url.to_text(),
        }
    }

    /// The preflight's answer arrived.
    pub fn on_preflight_response(
        &mut self,
        cache: &mut PreflightCache,
        now_ms: u64,
        status: u16,
        headers: &[(String, String)],
    ) -> Result<(), NetError> {
        if self.state != State::AwaitPreflight {
            return Err(NetError::OutOfOrder(
                "on_preflight_response called out of order",
            ));
        }
        self.state = State::Done;
        let include = self.credentials_mode_include();
        let grant = preflight_check(
            &self.request_origin(),
            include,
            &self.method,
            &self.unsafe_names(),
            status,
            headers,
        )
        .map_err(|k| self.cors_error(k))?;
        cache.store(&self.request_origin(), &self.url, include, &grant, now_ms);
        self.state = State::AwaitResponse;
        Ok(())
    }

    /// The response's head arrived (`headers`: lower-case names as milkweb gives them).
    pub fn on_response(
        &mut self,
        status: u16,
        reason: &str,
        headers: &[(String, String)],
    ) -> Result<Outcome, NetError> {
        if self.state != State::AwaitResponse {
            return Err(NetError::OutOfOrder("on_response called out of order"));
        }
        self.state = State::Done;
        let lim = self.cfg.limits.clone();
        if !(200..=599).contains(&status) {
            return Err(NetError::BadResponse(alloc::format!("status {status}")));
        }
        if let Some(cl) = combined_value(headers, "content-length")
            && let Ok(n) = cl.trim().parse::<usize>()
            && n > lim.max_response_body
        {
            return Err(NetError::ResponseTooLarge {
                max: lim.max_response_body,
            });
        }
        if self.tainting == ResponseType::Cors {
            cors_check(
                &self.request_origin(),
                self.credentials_mode_include(),
                headers,
            )
            .map_err(|k| self.cors_error(k))?;
        }
        if self.tainting == ResponseType::Opaque {
            corp_check(&self.page.origin, &self.url, headers)?;
        }
        let store_cookies = self.hop_cookies.take();

        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            match self.redirect {
                Redirect::Error => {
                    return Err(NetError::RedirectRefused {
                        url: self.url.to_text(),
                    });
                }
                Redirect::Manual => {
                    return Ok(Outcome {
                        store_cookies,
                        next: Next::Done(ResponseMeta {
                            kind: ResponseType::OpaqueRedirect,
                            status: 0,
                            status_text: String::new(),
                            headers: Vec::new(),
                            url: self.url.to_text(),
                            redirected: self.redirects > 0,
                            hide_body: true,
                        }),
                    });
                }
                Redirect::Follow => {}
            }
            if let Some(loc) = combined_first(headers, "location") {
                let to = self.follow(status, &loc, headers)?;
                self.state = State::Ready;
                return Ok(Outcome {
                    store_cookies,
                    next: Next::Redirect { to },
                });
            }
        }

        let meta = match self.tainting {
            ResponseType::Opaque => ResponseMeta {
                kind: ResponseType::Opaque,
                status: 0,
                status_text: String::new(),
                headers: Vec::new(),
                url: String::new(),
                redirected: self.redirects > 0,
                hide_body: true,
            },
            t => ResponseMeta {
                kind: if t == ResponseType::Cors {
                    ResponseType::Cors
                } else {
                    ResponseType::Basic
                },
                status,
                status_text: clean_reason(reason),
                headers: if t == ResponseType::Cors {
                    filter_cors(headers, self.credentials_mode_include())
                } else {
                    filter_basic(headers)
                },
                url: self.url.to_text(),
                redirected: self.redirects > 0,
                hide_body: false,
            },
        };
        Ok(Outcome {
            store_cookies,
            next: Next::Done(meta),
        })
    }

    /// HTTP-redirect fetch: validate the Location and move the request to it.
    fn follow(
        &mut self,
        status: u16,
        loc: &str,
        headers: &[(String, String)],
    ) -> Result<Url, NetError> {
        let from = self.url.to_text();
        let next = match resolve_url(&self.url, loc, &self.cfg.limits) {
            Ok(Target::Http(u)) => u,
            Ok(Target::Data(_)) => {
                return Err(NetError::RedirectScheme {
                    url: from,
                    scheme: String::from("data"),
                });
            }
            Ok(Target::Blob(_)) => {
                return Err(NetError::RedirectScheme {
                    url: from,
                    scheme: String::from("blob"),
                });
            }
            Err(NetError::UnsupportedScheme(s)) => {
                return Err(NetError::RedirectScheme {
                    url: from,
                    scheme: s,
                });
            }
            Err(NetError::UrlHasCredentials) => {
                return Err(NetError::RedirectWithCredentials { url: from });
            }
            Err(e @ NetError::BlockedPort { .. }) => return Err(e),
            Err(e) => {
                return Err(NetError::BadRedirect {
                    url: from,
                    why: alloc::format!("{e}"),
                });
            }
        };
        if self.redirects >= self.cfg.limits.max_redirects {
            return Err(NetError::TooManyRedirects {
                max: self.cfg.limits.max_redirects,
            });
        }
        check_target(&self.cfg, &self.page, self.mode, &next)?;

        // The Origin header becomes `null` once the chain moves from another origin on to a
        // third one (so that one never learns the page's origin).
        let here = Origin::of(&self.url);
        let there = Origin::of(&next);
        if !self.page.origin.same_origin(&here) && !self.page.origin.same_origin(&there) {
            self.tainted_origin = true;
        }
        // Credentials in a header don't follow a redirect to another origin.
        if !here.same_origin(&there) {
            self.user_headers.retain(|(k, _)| k != "authorization");
        }
        // 301/302 turn a POST into a GET; 303 turns everything but GET/HEAD into a GET.
        let to_get = match status {
            301 | 302 => self.method.as_str() == "POST",
            303 => !self.method.is_get_or_head(),
            _ => false,
        };
        if to_get {
            self.method = Method::get();
            self.send_body = false;
            self.user_headers.retain(|(k, _)| {
                !matches!(
                    k.as_str(),
                    "content-encoding" | "content-language" | "content-location" | "content-type"
                )
            });
        }
        // A Referrer-Policy on the redirect applies from here on.
        if let Some(v) = combined_value(headers, "referrer-policy")
            && let Some(p) = ReferrerPolicy::parse_header(&v)
        {
            self.policy = p;
        }
        self.redirects += 1;
        self.enter(&next);
        self.url = next.clone();
        Ok(next)
    }
}

fn combined_first(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}
