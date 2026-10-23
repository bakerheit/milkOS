//! The CORS algorithm's pure parts: the check of a response against the page's origin, the
//! preflight request and its answer, the preflight cache, response filtering (what a page
//! may see of a cross-origin response) and the Cross-Origin-Resource-Policy check.
//!
//! Strictness notes (see also the `netpolicy` docs):
//! * `Access-Control-Allow-Origin` must be present exactly once and equal the serialized
//!   origin byte for byte (or `*` without credentials); a list is a failure;
//! * a present but invalid `Access-Control-Max-Age` means "don't cache" (the standard says 5
//!   seconds); every max-age is capped by [`super::limits::Limits::preflight_max_age`];
//! * a failed CORS check also means the response's `Set-Cookie` is not stored.

use alloc::string::String;
use alloc::vec::Vec;

use milkweb::url::Url;

use super::error::{CorsError, NetError};
use super::headers::{
    Method, combined_value, cors_unsafe_request_header_names, is_forbidden_response_header,
    list_values,
};
use super::origin::Origin;

/// What kind of response a page is handed (`response.type`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResponseType {
    /// Same-origin (or `data:`): everything but Set-Cookie.
    Basic,
    /// Cross-origin, CORS passed: status and body readable, headers filtered.
    Cors,
    /// Cross-origin `no-cors`: nothing readable (status 0, no headers, no body).
    Opaque,
    /// `redirect: "manual"` and the server redirected: nothing readable.
    OpaqueRedirect,
}

impl ResponseType {
    pub fn as_str(self) -> &'static str {
        match self {
            ResponseType::Basic => "basic",
            ResponseType::Cors => "cors",
            ResponseType::Opaque => "opaque",
            ResponseType::OpaqueRedirect => "opaqueredirect",
        }
    }
}

/// Response headers a page may read from a CORS response without the server naming them.
pub const CORS_SAFELISTED_RESPONSE_HEADERS: [&str; 7] = [
    "cache-control",
    "content-language",
    "content-length",
    "content-type",
    "expires",
    "last-modified",
    "pragma",
];

/// The CORS check: may a page of `origin` read this response? `include_credentials`: the
/// request carried credentials (cookies).
pub fn cors_check(
    origin: &Origin,
    include_credentials: bool,
    headers: &[(String, String)],
) -> Result<(), CorsError> {
    let mut allow = headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("access-control-allow-origin"));
    let Some((_, value)) = allow.next() else {
        return Err(CorsError::MissingAllowOrigin);
    };
    if allow.next().is_some() || value.contains(',') {
        return Err(CorsError::MultipleAllowOrigin);
    }
    if value == "*" {
        if include_credentials {
            return Err(CorsError::WildcardWithCredentials);
        }
        return Ok(());
    }
    if *value != origin.serialize() {
        return Err(CorsError::OriginMismatch {
            allowed: value.clone(),
        });
    }
    if include_credentials {
        let creds = combined_value(headers, "access-control-allow-credentials");
        if creds.as_deref() != Some("true") {
            return Err(CorsError::CredentialsNotAllowed);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Preflight

/// The OPTIONS request to send before a non-simple one. Sent without credentials, no body.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PreflightRequest {
    pub url: Url,
    /// Lower-case names. Includes `access-control-request-method`, and
    /// `access-control-request-headers` when there are unsafe headers; the caller adds the
    /// transport's own (User-Agent, Host).
    pub headers: Vec<(String, String)>,
}

/// Build the preflight for a request to `url`. `origin_header` and `referer` are the values
/// the actual request would carry (None: leave the header out).
pub fn build_preflight(
    url: &Url,
    method: &Method,
    unsafe_names: &[String],
    origin_header: Option<&str>,
    referer: Option<&str>,
) -> PreflightRequest {
    let mut headers: Vec<(String, String)> = Vec::new();
    headers.push((String::from("accept"), String::from("*/*")));
    headers.push((
        String::from("access-control-request-method"),
        String::from(method.as_str()),
    ));
    if !unsafe_names.is_empty() {
        headers.push((
            String::from("access-control-request-headers"),
            unsafe_names.join(","),
        ));
    }
    if let Some(o) = origin_header {
        headers.push((String::from("origin"), String::from(o)));
    }
    if let Some(r) = referer {
        headers.push((String::from("referer"), String::from(r)));
    }
    PreflightRequest {
        url: url.clone(),
        headers,
    }
}

/// What a successful preflight allows.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PreflightGrant {
    /// Case-sensitive method names listed (a `*` only counts without credentials).
    pub methods: Vec<String>,
    /// Lower-case header names listed.
    pub headers: Vec<String>,
    pub methods_wildcard: bool,
    pub headers_wildcard: bool,
    /// Seconds the answer may be kept (before the Browser's cap).
    pub max_age: u64,
}

impl PreflightGrant {
    /// Does this grant cover a request with `method`, whose non-safelisted headers are
    /// `unsafe_names` (lower case) and which carries `authorization` if it has that header?
    pub fn allows(
        &self,
        method: &Method,
        unsafe_names: &[String],
        credentials: bool,
    ) -> Result<(), CorsError> {
        let wild_methods = self.methods_wildcard && !credentials;
        if !method.is_cors_safelisted()
            && !self.methods.iter().any(|m| m == method.as_str())
            && !wild_methods
        {
            return Err(CorsError::MethodNotAllowed {
                method: String::from(method.as_str()),
                wildcard_ignored: self.methods_wildcard && credentials,
            });
        }
        let wild_headers = self.headers_wildcard && !credentials;
        for name in unsafe_names {
            let listed = self.headers.iter().any(|h| h == name);
            // Authorization is never covered by a wildcard.
            let by_wildcard = wild_headers && name != "authorization";
            if !listed && !by_wildcard {
                return Err(CorsError::HeaderNotAllowed {
                    name: name.clone(),
                    wildcard_ignored: self.headers_wildcard
                        && (credentials || name == "authorization"),
                });
            }
        }
        Ok(())
    }
}

/// Check a preflight's answer. `unsafe_names`: lower-case, from
/// [`cors_unsafe_request_header_names`] over the request's headers.
pub fn preflight_check(
    origin: &Origin,
    credentials: bool,
    method: &Method,
    unsafe_names: &[String],
    status: u16,
    headers: &[(String, String)],
) -> Result<PreflightGrant, CorsError> {
    if (300..400).contains(&status) {
        return Err(CorsError::PreflightRedirect);
    }
    if !(200..300).contains(&status) {
        return Err(CorsError::PreflightStatus(status));
    }
    cors_check(origin, credentials, headers)?;
    let raw_methods = list_values(headers, "access-control-allow-methods");
    let raw_headers = list_values(headers, "access-control-allow-headers");
    let methods_wildcard = raw_methods.iter().any(|m| m == "*");
    let headers_wildcard = raw_headers.iter().any(|h| h == "*");
    let grant = PreflightGrant {
        methods: raw_methods.into_iter().filter(|m| m != "*").collect(),
        headers: raw_headers
            .into_iter()
            .filter(|h| h != "*")
            .map(|h| h.to_ascii_lowercase())
            .collect(),
        methods_wildcard,
        headers_wildcard,
        max_age: match combined_value(headers, "access-control-max-age") {
            None => 5,
            Some(v) => {
                let v = v.trim();
                if !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()) {
                    v.parse::<u64>().unwrap_or(u64::MAX)
                } else {
                    0
                }
            }
        },
    };
    grant.allows(method, unsafe_names, credentials)?;
    Ok(grant)
}

/// The preflight cache (one per Browser; keep it behind the lock the bindings already have).
/// Keyed by origin, request address and whether credentials go.
#[derive(Clone, Debug)]
pub struct PreflightCache {
    entries: Vec<CacheEntry>,
    max_entries: usize,
    cap_secs: u64,
}

#[derive(Clone, Debug)]
struct CacheEntry {
    origin: String,
    url: String,
    credentials: bool,
    expires_ms: u64,
    grant: PreflightGrant,
}

impl PreflightCache {
    pub fn new(max_entries: usize, cap_secs: u64) -> PreflightCache {
        PreflightCache {
            entries: Vec::new(),
            max_entries: max_entries.max(1),
            cap_secs,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Is a still-valid answer on file that covers this request?
    pub fn covers(
        &mut self,
        origin: &Origin,
        url: &Url,
        credentials: bool,
        method: &Method,
        unsafe_names: &[String],
        now_ms: u64,
    ) -> bool {
        self.entries.retain(|e| e.expires_ms > now_ms);
        let (o, u) = (origin.serialize(), url.to_text());
        self.entries.iter().any(|e| {
            e.origin == o
                && e.url == u
                && e.credentials == credentials
                && e.grant.allows(method, unsafe_names, credentials).is_ok()
        })
    }

    /// Remember an answer (not if its age is 0).
    pub fn store(
        &mut self,
        origin: &Origin,
        url: &Url,
        credentials: bool,
        grant: &PreflightGrant,
        now_ms: u64,
    ) {
        let (o, u) = (origin.serialize(), url.to_text());
        self.entries
            .retain(|e| !(e.origin == o && e.url == u && e.credentials == credentials));
        let age = grant.max_age.min(self.cap_secs);
        if age == 0 {
            return;
        }
        while self.entries.len() >= self.max_entries {
            self.entries.remove(0);
        }
        self.entries.push(CacheEntry {
            origin: o,
            url: u,
            credentials,
            expires_ms: now_ms.saturating_add(age.saturating_mul(1000)),
            grant: grant.clone(),
        });
    }
}

// ---------------------------------------------------------------------------------------
// Filtering responses

/// Headers of a same-origin response: all but the ones a page never sees.
pub fn filter_basic(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(k, _)| !is_forbidden_response_header(k))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect()
}

/// Headers of a CORS response: the safelisted ones plus the ones `Access-Control-Expose-Headers`
/// names (`*` for all, unless credentials went); never Set-Cookie.
pub fn filter_cors(
    headers: &[(String, String)],
    include_credentials: bool,
) -> Vec<(String, String)> {
    let exposed = list_values(headers, "access-control-expose-headers");
    let wildcard = !include_credentials && exposed.iter().any(|e| e == "*");
    headers
        .iter()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            !is_forbidden_response_header(&k)
                && (wildcard
                    || CORS_SAFELISTED_RESPONSE_HEADERS.contains(&k.as_str())
                    || exposed.iter().any(|e| e.eq_ignore_ascii_case(&k)))
        })
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect()
}

/// `Cross-Origin-Resource-Policy` for a `no-cors` request from `origin` to `url`.
pub fn corp_check(
    origin: &Origin,
    url: &Url,
    headers: &[(String, String)],
) -> Result<(), NetError> {
    let Some(v) = combined_value(headers, "cross-origin-resource-policy") else {
        return Ok(());
    };
    let target = Origin::of(url);
    let blocked = match v.trim() {
        "same-origin" => !origin.same_origin(&target),
        "same-site" => !origin.same_site(&target),
        _ => false,
    };
    if blocked {
        return Err(NetError::BlockedByCorp {
            url: url.to_text(),
            policy: String::from(v.trim()),
        });
    }
    Ok(())
}

/// Convenience for callers building the unsafe-name list from a plain header list.
pub fn unsafe_names(headers: &[(String, String)]) -> Vec<String> {
    cors_unsafe_request_header_names(headers)
}
