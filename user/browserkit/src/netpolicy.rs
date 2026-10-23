//! Network policy for scripts: what a page's `fetch` and XMLHttpRequest may do. Pure rules,
//! no I/O and no clock of its own: the Browser's networking asks, does what it is told, and
//! reports back what it saw. The web's same-origin policy and CORS, the way the Fetch standard
//! writes them, plus a few browser-wide limits.
//!
//! # Use
//!
//! The entry point is [`Exchange`], one request hop by hop (the full loop is in its module
//! docs). Everything it is built from is public too: [`origin`] (origins, sites, address
//! classes, `resolve_url`), [`headers`] (methods, the forbidden and safelisted lists, the
//! `Headers` guards), [`referrer`], [`cors`] (the CORS check, preflight, its cache, response
//! filtering), [`limits`] (sizes, the per-tab `Limiter`) and [`error`] (every refusal, with
//! console text and the short text a page gets).
//!
//! * `Exchange::start(cfg, page, init)` validates a [`RequestInit`] and says what the address
//!   is: the network, a `data:` address (answered here, no network) or a `blob:` of the
//!   page's own origin (looked up by the bindings).
//! * `next_hop` says exactly what to put on the wire, `on_response` says what came of the
//!   answer: the final [`ResponseMeta`] (type, filtered headers, whether the body may be
//!   shown) or "redirect, ask again". Any `Err` ends the exchange.
//! * The cookie jar stays the Browser's: a [`Hop`] says whether to ask it (`cookies`), and an
//!   [`Outcome`] says whether a response's `Set-Cookie` may be stored, both with the
//!   `cookies::Context` to use.
//!
//! # What is enforced
//!
//! * **Origins**: scheme, host and port; `==` on [`Origin`] is the same-origin test (an opaque
//!   origin equals only itself). Same-site is *schemeful* (`http://a.test` and
//!   `https://a.test` are different sites) and uses the cookie code's site rule (last two
//!   labels, a short list of two-label suffixes; no full public suffix list: known gap).
//! * **Addresses** ([`resolve_url`]): http, https, `data:` (GET only, decoded here, size
//!   capped) and `blob:` (own origin only). `file:`, `javascript:`, `ftp:`, `ws:` and the rest
//!   are refused. A user name or password, a backslash, control characters, `http:foo`
//!   without `//`, number-like hosts that are not a plain dotted quad (`127.1`, `0x7f.1`,
//!   `2130706433`), non-ASCII hosts, and the web's refused ports (mail, ssh, irc, ...) are
//!   refused. Fragments are dropped. Length limit 8192 characters.
//! * **Modes**: `same-origin` (cross-origin refused, also on a redirect), `cors`, `no-cors`
//!   (GET, HEAD and POST only; only CORS-safelisted headers survive; a cross-origin answer is
//!   opaque: status 0, no headers, no body; a `Cross-Origin-Resource-Policy` that excludes the
//!   page blocks it), `navigate` (refused for scripts). Tainting only goes up along a redirect
//!   chain: once a response needed CORS it always does.
//! * **Credentials**: `omit` (no cookies sent, none stored), `same-origin` (cookies only to the
//!   page's own origin), `include`. Cookies are never sent to or stored from another *site*
//!   (no third-party cookies, whatever `include` says), nor after a redirect chain has touched
//!   another site, nor when the scheme differs from the page's. Set-Cookie is stored only
//!   after the CORS check passed. The cookie jar's own rules (SameSite, Secure, HttpOnly)
//!   apply on top.
//! * **Headers**: names are tokens; values are plain ASCII without control characters;
//!   forbidden request headers (Host, Cookie, Origin, Referer, Sec-*, Proxy-*, Content-Length,
//!   ...) are silently dropped as the standard says and again, defensively, when a request
//!   starts; `Set-Cookie` is never visible in a response. The browser adds `Accept`, `Origin`
//!   (as the standard says: for CORS requests, and for non-GET requests by referrer policy;
//!   `null` once a redirect moves from another origin on to a third), `Referer`, `Sec-Fetch-Dest / Mode
//!   / Site` (only to trustworthy addresses) and cache-mode headers.
//! * **CORS**: simple requests go straight out and the answer is checked (`Access-Control-
//!   Allow-Origin` exactly once and exactly the origin, or `*` without credentials; with
//!   credentials also `Access-Control-Allow-Credentials: true`). Anything else (a method other
//!   than GET, HEAD, POST, a header that is not safelisted, a non-simple `Content-Type`, an XHR
//!   with upload listeners) is preflighted: an OPTIONS request without credentials; status
//!   2xx, the same origin check, `Allow-Methods` and `Allow-Headers` (case-sensitive methods,
//!   case-insensitive headers, `*` only without credentials, `Authorization` always named).
//!   Answers are cached by (origin, address, credentials) for `Max-Age` seconds, capped.
//!   Every hop is checked on its own, redirect responses included. A page sees only the
//!   CORS-safelisted response headers plus the ones `Access-Control-Expose-Headers` names.
//! * **Redirects**: at most 20; 301/302 turn POST into GET, 303 everything but GET/HEAD;
//!   307/308 keep method and body; `Authorization` is dropped when the origin changes; only
//!   http(s) targets; `redirect: error` and `manual` are honoured (manual gives an opaque
//!   redirect); a changed `Referrer-Policy` applies from then on.
//! * **Mixed content**: an https page may not request http, except to this machine
//!   (`localhost`, 127/8, `::1`), where the private-network rule then decides. Checked on every
//!   hop. http pages may request anything.
//! * **Private networks**: a page may reach its own address space or a more public one. A page
//!   on a public host asking for an IP literal or name in a private or loopback range (10/8,
//!   172.16/12, 192.168/16, 169.254/16, 100.64/10, fc00::/7, fe80::/10, `localhost`,
//!   `*.local`, one-label names, ...) is refused ([`Config::block_private_network`], on by
//!   default). A *name* that points inward is caught by the transport: each [`Hop`] carries
//!   `max_address_space`, and `Hop::check_resolved(ip)` is called with the looked-up address.
//! * **Limits** ([`Limits`]): request body 4 MiB (keepalive 64 KiB), response body 8 MiB
//!   (a `Content-Length` over it fails at once; the transport enforces the rest), 64 request
//!   headers of 16 KiB in all, 20 redirects, 30 s for the whole exchange, 16 requests in
//!   flight per tab and 6 per host:port (more wait in a queue, up to 64 pending).
//! * **Errors**: `Display` of [`NetError`] is for the console and names the URLs and the rule;
//!   `script_message()` is what the page gets ("Failed to fetch" for anything the network
//!   rules decided, so a page cannot probe with the error text).
//!
//! # Where this is stricter than the standard or browsers (on purpose)
//!
//! * Request header values must be ASCII (the standard allows Latin-1 bytes).
//! * `Range` is not CORS-safelisted (preflighted).
//! * An invalid `Access-Control-Max-Age` means "do not cache" (standard: 5 s); the cap is
//!   600 s.
//! * A header list with several `Access-Control-Allow-Origin` values, or one holding a list,
//!   fails.
//! * A user name in a redirect target fails (the standard allows it for same-origin).
//! * Same-site is schemeful for cookies, and a redirect chain that touched another site gets no
//!   cookies for the rest of the chain.
//! * No cookies are stored from a response that failed the CORS check.
//! * Number-like hosts, backslashes, `http:foo`, empty labels and ports on the web's refusal
//!   list fail instead of being "fixed".
//! * Public pages cannot reach private-network or loopback addresses (browsers ask the server
//!   to opt in with a preflight; here there is no opt-in).
//! * `X-HTTP-Method*` override headers may not name CONNECT, TRACE or TRACK; `Set-Cookie` may
//!   not be set on a request.
//!
//! # Known gaps
//!
//! * No full public suffix list (see `cookies`): `a.example.com.mx` and `b.example.com.mx` are
//!   one site. Same-site matters for cookies only.
//! * No IDNA: a non-ASCII host is an error (write punycode).
//! * No HTTP cache (`cache` modes only set request headers; `only-if-cached` fails), no CSP,
//!   no `Cross-Origin-Opener/Embedder-Policy`, no Opaque Response Blocking (an opaque response
//!   has no body at all, so nothing leaks).
//! * Redirect and other responses are judged by the status codes we know (301, 302, 303,
//!   307, 308).
//! * Time: the preflight cache wants a monotonic clock in milliseconds.

pub mod cors;
pub mod error;
pub mod exchange;
pub mod headers;
pub mod limits;
pub mod origin;
pub mod referrer;

pub use cors::{
    PreflightCache, PreflightGrant, PreflightRequest, ResponseType, build_preflight, corp_check,
    cors_check, filter_basic, filter_cors, preflight_check,
};
pub use error::{CorsError, ErrorKind, NetError};
pub use exchange::{
    Begin, BodyInfo, CacheMode, Credentials, Exchange, Hop, Mode, Next, Outcome, Page, Redirect,
    RequestInit, ResponseMeta, check_construct,
};
pub use headers::{Guard, Headers, Method};
pub use limits::{Admission, Config, Limiter, Limits};
pub use origin::{
    AddressSpace, DataUrl, Origin, Target, address_space_of_host, address_space_of_ip, resolve_url,
};
pub use referrer::{ReferrerInit, ReferrerPolicy, referrer_header};

#[cfg(test)]
mod tests_cors;
#[cfg(test)]
mod tests_exchange;
#[cfg(test)]
mod tests_fuzz;
#[cfg(test)]
mod tests_headers;
#[cfg(test)]
mod tests_origin;
