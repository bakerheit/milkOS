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

use super::cors::{PreflightCache, PreflightRequest, ResponseType};
use super::error::NetError;
use super::headers::{Headers, Method};
use super::limits::Config;
use super::origin::{Origin, Target, resolve_url};
use super::referrer::{ReferrerInit, ReferrerPolicy};
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
    /// The page's referrer policy (`<meta name=referrer>` / header; the default if none).
    pub referrer_policy: ReferrerPolicy,
}

impl Page {
    pub fn new(url: Url) -> Page {
        Page {
            origin: Origin::of(&url),
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

/// One script request in flight (see the module docs).
#[derive(Clone, Debug)]
pub struct Exchange {
    cfg: Config,
    page: Page,
    url: Url,
    method: Method,
    state: State,
    timeout_ms: u64,
}

impl Exchange {
    /// Check the request against every rule that needs no network, and set it up.
    pub fn start(cfg: &Config, page: &Page, init: RequestInit) -> Result<Begin, NetError> {
        // STUB: only same-origin requests, no CORS yet (the full rules replace this).
        let target = resolve_url(&page.url, &init.url, &cfg.limits)?;
        let Target::Http(url) = target else {
            return Err(NetError::OutOfOrder(
                "stub: data and blob addresses are not supported yet",
            ));
        };
        if Origin::of(&url) != page.origin {
            return Err(NetError::SameOriginOnly {
                page: page.origin.serialize(),
                url: url.to_text(),
            });
        }
        Ok(Begin::Network(Exchange {
            cfg: cfg.clone(),
            page: page.clone(),
            url,
            method: init.method,
            state: State::Ready,
            timeout_ms: cfg.effective_timeout(init.timeout_ms),
        }))
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

    /// The next request to send (the first one, or after a redirect).
    pub fn next_hop(&mut self, cache: &mut PreflightCache, now_ms: u64) -> Result<Hop, NetError> {
        let _ = (cache, now_ms);
        if self.state != State::Ready {
            return Err(NetError::OutOfOrder("next_hop called out of order"));
        }
        self.state = State::AwaitResponse;
        Ok(Hop {
            method: self.method.clone(),
            url: self.url.clone(),
            headers: Vec::new(),
            send_body: true,
            cookies: self
                .page
                .origin
                .site()
                .map(|s| cookies::Context::Subresource { page_site: s }),
            preflight: None,
            timeout_ms: self.timeout_ms,
        })
    }

    /// The preflight's answer arrived.
    pub fn on_preflight_response(
        &mut self,
        cache: &mut PreflightCache,
        now_ms: u64,
        status: u16,
        headers: &[(String, String)],
    ) -> Result<(), NetError> {
        let _ = (cache, now_ms, status, headers);
        Err(NetError::OutOfOrder("stub: no preflight yet"))
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
        let _ = &self.cfg;
        Ok(Outcome {
            store_cookies: None,
            next: Next::Done(ResponseMeta {
                kind: ResponseType::Basic,
                status,
                status_text: String::from(reason),
                headers: super::cors::filter_basic(headers),
                url: self.url.to_text(),
                redirected: false,
                hide_body: false,
            }),
        })
    }
}
