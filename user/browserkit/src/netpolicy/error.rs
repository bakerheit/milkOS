//! Every way a script's request can be refused, with two texts for each: the plain-English
//! one for the console ([`NetError`]'s `Display`: it names URLs and says why) and the short
//! one the page itself gets ([`NetError::script_message`]: like browsers, a page learns
//! "it failed" and not which security rule fired, so a page can't use the error to probe the
//! network).

use alloc::format;
use alloc::string::String;
use core::fmt;

/// How a failure reaches the page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    /// The request was malformed: `fetch` / `new Request` throws or rejects with a TypeError,
    /// XHR throws a SyntaxError or SecurityError. Nothing was sent.
    Type,
    /// A rule stopped it before or after it was sent (CORS, mixed content, blocked port, ...):
    /// `fetch` rejects with `TypeError: Failed to fetch`, XHR fires `error`.
    Network,
    /// The overall time limit ran out.
    Timeout,
    /// The page's AbortSignal fired (the bindings produce this; the policy never does).
    Abort,
}

/// Why a CORS check failed (see [`NetError::Cors`] for the URLs).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CorsError {
    /// No `Access-Control-Allow-Origin` header.
    MissingAllowOrigin,
    /// More than one of them, or one holding a list.
    MultipleAllowOrigin,
    /// `Access-Control-Allow-Origin: *` on a request that carries credentials.
    WildcardWithCredentials,
    /// The header names another origin (or something that isn't an origin).
    OriginMismatch { allowed: String },
    /// A credentialed request, and the response lacks `Access-Control-Allow-Credentials: true`.
    CredentialsNotAllowed,
    /// The preflight answered with this status (it must be 2xx).
    PreflightStatus(u16),
    /// The preflight answered with a redirect (not allowed).
    PreflightRedirect,
    /// `Access-Control-Allow-Methods` doesn't list the method.
    MethodNotAllowed {
        method: String,
        wildcard_ignored: bool,
    },
    /// `Access-Control-Allow-Headers` doesn't list the header.
    HeaderNotAllowed {
        name: String,
        wildcard_ignored: bool,
    },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NetError {
    // ---- the request itself (ErrorKind::Type) ----
    /// The address text can't be parsed (what was wrong).
    BadUrl(String),
    /// `https://user:pass@host/` (a request address may not carry credentials).
    UrlHasCredentials,
    /// `file:`, `javascript:`, `ftp:` and so on (the scheme).
    UnsupportedScheme(String),
    UrlTooLong {
        max: usize,
    },
    /// Not a method token.
    BadMethod(String),
    /// CONNECT, TRACE or TRACK.
    ForbiddenMethod(String),
    BadHeaderName(String),
    /// The header's name (its value has a control character, a line break, or isn't ASCII).
    BadHeaderValue(String),
    TooManyHeaders {
        max: usize,
    },
    HeadersTooLarge {
        max: usize,
    },
    /// The Headers object is immutable (a Response from `fetch`, ...).
    HeadersImmutable,
    /// GET and HEAD can't carry a body.
    BodyNotAllowed(String),
    /// `mode: "no-cors"` takes only GET, HEAD and POST.
    NoCorsMethod(String),
    /// `cache: "only-if-cached"` needs `mode: "same-origin"`.
    CacheNeedsSameOrigin,
    /// `mode: "navigate"` can't be set by a script.
    NavigateMode,
    /// The `referrer` is another origin's address, or not an address.
    BadReferrer(String),
    RequestBodyTooLarge {
        len: usize,
        max: usize,
    },
    KeepaliveBodyTooLarge {
        len: usize,
        max: usize,
    },

    // ---- stopped by a rule (ErrorKind::Network) ----
    /// An http:// address asked for by an https:// page.
    MixedContent {
        page: String,
        url: String,
    },
    /// One of the ports browsers refuse to talk to (mail, ssh, ...).
    BlockedPort {
        url: String,
        port: u16,
    },
    /// A public page asking for a private-network or loopback address.
    PrivateNetwork {
        page: String,
        url: String,
    },
    /// `mode: "same-origin"` and the address is another origin's.
    SameOriginOnly {
        page: String,
        url: String,
    },
    Cors {
        kind: CorsError,
        page_origin: String,
        url: String,
    },
    /// `redirect: "error"` and the server redirected.
    RedirectRefused {
        url: String,
    },
    /// A redirect with a Location that isn't an address we can use (what was wrong).
    BadRedirect {
        url: String,
        why: String,
    },
    /// A redirect (to another origin) to an address with a user name in it.
    RedirectWithCredentials {
        url: String,
    },
    /// A redirect to something that isn't http or https.
    RedirectScheme {
        url: String,
        scheme: String,
    },
    TooManyRedirects {
        max: usize,
    },
    ResponseTooLarge {
        max: usize,
    },
    /// `Cross-Origin-Resource-Policy` refuses this reader.
    BlockedByCorp {
        url: String,
        policy: String,
    },
    /// `cache: "only-if-cached"`, and there's no cache.
    NotCached {
        url: String,
    },
    /// Too many requests at once for the tab.
    TooManyRequests {
        max: usize,
    },
    /// The Exchange was used out of order (a bug in the caller, never a page's doing).
    OutOfOrder(&'static str),

    // ---- ErrorKind::Timeout ----
    Timeout {
        ms: u64,
    },
}

impl NetError {
    pub fn kind(&self) -> ErrorKind {
        use NetError::*;
        match self {
            BadUrl(_)
            | UrlHasCredentials
            | UnsupportedScheme(_)
            | UrlTooLong { .. }
            | BadMethod(_)
            | ForbiddenMethod(_)
            | BadHeaderName(_)
            | BadHeaderValue(_)
            | TooManyHeaders { .. }
            | HeadersTooLarge { .. }
            | HeadersImmutable
            | BodyNotAllowed(_)
            | NoCorsMethod(_)
            | CacheNeedsSameOrigin
            | NavigateMode
            | BadReferrer(_)
            | RequestBodyTooLarge { .. }
            | KeepaliveBodyTooLarge { .. } => ErrorKind::Type,
            Timeout { .. } => ErrorKind::Timeout,
            _ => ErrorKind::Network,
        }
    }

    /// What the page is told. Never says which rule fired for a network-class failure.
    pub fn script_message(&self) -> String {
        match self.kind() {
            ErrorKind::Network => String::from("Failed to fetch"),
            ErrorKind::Timeout => String::from("The request timed out"),
            ErrorKind::Abort => String::from("The operation was aborted"),
            ErrorKind::Type => format!("{self}"),
        }
    }
}

impl fmt::Display for CorsError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CorsError::MissingAllowOrigin => {
                f.write_str("the response has no Access-Control-Allow-Origin header")
            }
            CorsError::MultipleAllowOrigin => f.write_str(
                "the response has more than one Access-Control-Allow-Origin value (it must have exactly one)",
            ),
            CorsError::WildcardWithCredentials => f.write_str(
                "the response says Access-Control-Allow-Origin: * but the request sends credentials, so it must name the origin",
            ),
            CorsError::OriginMismatch { allowed } => {
                write!(f, "the Access-Control-Allow-Origin header is '{allowed}'")
            }
            CorsError::CredentialsNotAllowed => f.write_str(
                "the request sends credentials but the response lacks Access-Control-Allow-Credentials: true",
            ),
            CorsError::PreflightStatus(s) => write!(
                f,
                "the preflight (OPTIONS) request answered with status {s}, not 2xx"
            ),
            CorsError::PreflightRedirect => {
                f.write_str("the preflight (OPTIONS) request was redirected, which isn't allowed")
            }
            CorsError::MethodNotAllowed {
                method,
                wildcard_ignored,
            } => {
                write!(
                    f,
                    "method {method} isn't listed in the preflight's Access-Control-Allow-Methods"
                )?;
                if *wildcard_ignored {
                    f.write_str(" (a * doesn't count when credentials are sent)")?;
                }
                Ok(())
            }
            CorsError::HeaderNotAllowed {
                name,
                wildcard_ignored,
            } => {
                write!(
                    f,
                    "request header '{name}' isn't listed in the preflight's Access-Control-Allow-Headers"
                )?;
                if *wildcard_ignored {
                    f.write_str(" (a * doesn't count when credentials are sent, and never covers Authorization)")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use NetError::*;
        match self {
            BadUrl(why) => write!(f, "Invalid URL: {why}"),
            UrlHasCredentials => {
                f.write_str("Invalid URL: a request address can't contain a user name or password")
            }
            UnsupportedScheme(s) => write!(
                f,
                "Unsupported URL scheme '{s}:' (requests can use http:, https: and data: only)"
            ),
            UrlTooLong { max } => write!(f, "Invalid URL: longer than {max} characters"),
            BadMethod(m) => write!(f, "'{m}' isn't a valid HTTP method"),
            ForbiddenMethod(m) => write!(f, "The method {m} is not allowed"),
            BadHeaderName(n) => write!(f, "'{n}' isn't a valid header name"),
            BadHeaderValue(n) => write!(
                f,
                "The value of header '{n}' isn't valid (no control characters, line breaks or non-ASCII text)"
            ),
            TooManyHeaders { max } => write!(f, "More than {max} request headers"),
            HeadersTooLarge { max } => write!(f, "Request headers are larger than {max} bytes"),
            HeadersImmutable => f.write_str("These headers can't be changed (immutable)"),
            BodyNotAllowed(m) => write!(f, "A {m} request can't have a body"),
            NoCorsMethod(m) => write!(f, "mode 'no-cors' allows GET, HEAD and POST only, not {m}"),
            CacheNeedsSameOrigin => {
                f.write_str("cache mode 'only-if-cached' can only be used with mode 'same-origin'")
            }
            NavigateMode => f.write_str("mode 'navigate' can't be used by a script"),
            BadReferrer(r) => write!(
                f,
                "The referrer '{r}' isn't an address of this page's origin"
            ),
            RequestBodyTooLarge { len, max } => {
                write!(
                    f,
                    "The request body ({len} bytes) is over the {max} byte limit"
                )
            }
            KeepaliveBodyTooLarge { len, max } => write!(
                f,
                "A keepalive request body ({len} bytes) is over the {max} byte limit"
            ),
            MixedContent { page, url } => write!(
                f,
                "Mixed content: the page at {page} is secure (https) but asked for {url}, which isn't. The request was blocked; use https:// for it"
            ),
            BlockedPort { url, port } => write!(
                f,
                "Blocked: {url} uses port {port}, which browsers refuse to connect to"
            ),
            PrivateNetwork { page, url } => write!(
                f,
                "Blocked: the page at {page} isn't on a private network but asked for {url}, which is (a private, loopback or local address)"
            ),
            SameOriginOnly { page, url } => write!(
                f,
                "Blocked: the request is set to mode 'same-origin' but {url} is not the same origin as {page}"
            ),
            Cors {
                kind,
                page_origin,
                url,
            } => write!(
                f,
                "Blocked by CORS: {url} doesn't allow origin {page_origin}: {kind}"
            ),
            RedirectRefused { url } => write!(
                f,
                "The server redirected {url}, but the request is set to redirect: 'error'"
            ),
            BadRedirect { url, why } => write!(f, "Bad redirect from {url}: {why}"),
            RedirectWithCredentials { url } => write!(
                f,
                "Blocked: the redirect from {url} points to an address with a user name or password in it"
            ),
            RedirectScheme { url, scheme } => write!(
                f,
                "Blocked: the redirect from {url} points to a '{scheme}:' address (only http and https redirects are followed)"
            ),
            TooManyRedirects { max } => write!(f, "Too many redirects (more than {max})"),
            ResponseTooLarge { max } => {
                write!(f, "The response is larger than the {max} byte limit")
            }
            BlockedByCorp { url, policy } => write!(
                f,
                "Blocked: {url} sent Cross-Origin-Resource-Policy: {policy}, which doesn't allow this page to read it"
            ),
            NotCached { url } => write!(
                f,
                "{url} isn't in the cache and the request is set to cache: 'only-if-cached'"
            ),
            TooManyRequests { max } => {
                write!(f, "Too many requests at once (this tab allows {max})")
            }
            OutOfOrder(what) => write!(f, "Internal error: {what}"),
            Timeout { ms } => write!(f, "The request took longer than {ms} ms and was stopped"),
        }
    }
}
