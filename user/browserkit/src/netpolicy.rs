//! Network policy for scripts: what a page's `fetch` and XMLHttpRequest may do. Pure rules,
//! no I/O: the Browser's networking asks, does what it is told, and reports back.
//!
//! The entry point is [`Exchange`] (one request, hop by hop); its pieces are public:
//! [`origin`] (origins, sites, address classes, `resolve_url`), [`headers`] (methods,
//! forbidden and safelisted headers, the `Headers` guards), [`referrer`], [`cors`] (the
//! CORS check, preflight, its cache, response filtering), [`limits`] and [`error`].
//!
//! STATUS: interface commit. `Exchange` is a stub that allows same-origin requests only;
//! the full rules replace its body without changing these signatures.

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
    RequestInit, ResponseMeta,
};
pub use headers::{Guard, Headers, Method};
pub use limits::{Admission, Config, Limiter, Limits};
pub use origin::{AddressSpace, DataUrl, Origin, Target, resolve_url};
pub use referrer::{ReferrerInit, ReferrerPolicy, referrer_header};
