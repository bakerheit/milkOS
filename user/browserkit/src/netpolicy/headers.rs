//! Methods, header names and values, the forbidden and CORS-safelisted lists, and the
//! `Headers` object with its guards (what `new Headers()`, `request.headers` and
//! `response.headers` are made of).
//!
//! Choices stricter than the standard, on purpose:
//! * a request header value must be plain ASCII (text, tab, space): no control characters, no
//!   bytes above 0x7e. The standard allows any "byte string" and browsers send Latin-1; our
//!   writer sends UTF-8, so a non-ASCII value would mean different bytes on the wire;
//! * `Range` is not CORS-safelisted (the standard now lists a simple range; it is preflighted
//!   here);
//! * `Set-Cookie` is a forbidden request header too, and the `X-HTTP-Method*` override headers
//!   may not ask for CONNECT, TRACE or TRACK.

use alloc::string::String;
use alloc::vec::Vec;

use super::error::NetError;

pub const MAX_HEADERS: usize = 64;
pub const MAX_HEADERS_BYTES: usize = 16 * 1024;
pub const MAX_VALUE: usize = 8192;
pub const MAX_NAME: usize = 256;
pub const MAX_METHOD: usize = 32;

/// HTTP tab-or-space plus line breaks (what the standard strips from header values).
pub fn is_http_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | ' ')
}

/// RFC 9110 token characters.
pub fn is_token_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

pub fn is_token(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(is_token_byte)
}

// ---------------------------------------------------------------------------------------
// Methods

/// A request method: a token, with the six well-known ones in upper case (and no other
/// method changed: `patch` stays `patch`, as the standard says).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Method(String);

impl Method {
    /// Any method token (a response-side or internal use). Scripts use [`Method::parse_script`].
    pub fn parse(s: &str) -> Result<Method, NetError> {
        if !is_token(s) || s.len() > MAX_METHOD {
            return Err(NetError::BadMethod(truncate(s)));
        }
        for known in ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"] {
            if s.eq_ignore_ascii_case(known) {
                return Ok(Method(String::from(known)));
            }
        }
        Ok(Method(String::from(s)))
    }

    /// What a script may ask for: not CONNECT, TRACE or TRACK in any case.
    pub fn parse_script(s: &str) -> Result<Method, NetError> {
        let m = Method::parse(s)?;
        if is_forbidden_method(s) {
            return Err(NetError::ForbiddenMethod(m.0));
        }
        Ok(m)
    }

    pub fn get() -> Method {
        Method(String::from("GET"))
    }

    pub fn post() -> Method {
        Method(String::from("POST"))
    }

    pub fn options() -> Method {
        Method(String::from("OPTIONS"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_get(&self) -> bool {
        self.0 == "GET"
    }

    pub fn is_get_or_head(&self) -> bool {
        self.0 == "GET" || self.0 == "HEAD"
    }

    /// GET, HEAD and POST (exact, upper case).
    pub fn is_cors_safelisted(&self) -> bool {
        matches!(self.0.as_str(), "GET" | "HEAD" | "POST")
    }
}

fn truncate(s: &str) -> String {
    s.chars().take(40).collect()
}

pub fn is_forbidden_method(s: &str) -> bool {
    ["CONNECT", "TRACE", "TRACK"]
        .iter()
        .any(|m| s.eq_ignore_ascii_case(m))
}

// ---------------------------------------------------------------------------------------
// Names and values

pub fn valid_header_name(name: &str) -> bool {
    is_token(name) && name.len() <= MAX_NAME
}

/// The value as the standard stores it: leading and trailing tab, space and line breaks cut.
pub fn trim_header_value(v: &str) -> &str {
    v.trim_matches(is_http_whitespace)
}

/// Is this (already trimmed) value one we are willing to send? See the module docs.
pub fn valid_header_value(v: &str) -> bool {
    v.len() <= MAX_VALUE && v.bytes().all(|c| c == b'\t' || (0x20..=0x7e).contains(&c))
}

fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

// ---------------------------------------------------------------------------------------
// Forbidden headers

const FORBIDDEN_REQUEST: &[&str] = &[
    "accept-charset",
    "accept-encoding",
    "access-control-request-headers",
    "access-control-request-method",
    "connection",
    "content-length",
    "cookie",
    "cookie2",
    "date",
    "dnt",
    "expect",
    "host",
    "keep-alive",
    "origin",
    "referer",
    "set-cookie",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "via",
];

/// May a script not set this header on a request? (The browser sets or owns it.)
/// `value` matters for the method-override headers only.
pub fn is_forbidden_request_header(name: &str, value: &str) -> bool {
    let n = lower(name);
    if FORBIDDEN_REQUEST.contains(&n.as_str()) || n.starts_with("proxy-") || n.starts_with("sec-") {
        return true;
    }
    if matches!(
        n.as_str(),
        "x-http-method" | "x-http-method-override" | "x-method-override"
    ) {
        return value
            .split(',')
            .any(|m| is_forbidden_method(m.trim_matches(is_http_whitespace)));
    }
    false
}

/// Response headers a script never sees.
pub fn is_forbidden_response_header(name: &str) -> bool {
    let n = lower(name);
    n == "set-cookie" || n == "set-cookie2"
}

// ---------------------------------------------------------------------------------------
// CORS-safelisted request headers

/// A byte a safelisted header value may not contain.
fn is_cors_unsafe_byte(c: u8) -> bool {
    (c < 0x20 && c != b'\t')
        || matches!(
            c,
            b'"' | b'('
                | b')'
                | b':'
                | b'<'
                | b'>'
                | b'?'
                | b'@'
                | b'['
                | b'\\'
                | b']'
                | b'{'
                | b'}'
                | 0x7f
        )
}

/// `type/subtype` of a MIME type text, lower case; `None` if it isn't one.
pub fn mime_essence(value: &str) -> Option<String> {
    let v = value.trim_matches(is_http_whitespace);
    let essence = v
        .split(';')
        .next()
        .unwrap_or("")
        .trim_matches(is_http_whitespace);
    let (t, s) = essence.split_once('/')?;
    (is_token(t) && is_token(s)).then(|| lower(essence))
}

/// Is `(name, value)` a request header any page may send without a preflight?
pub fn is_cors_safelisted_request_header(name: &str, value: &str) -> bool {
    if value.len() > 128 {
        return false;
    }
    let n = lower(name);
    match n.as_str() {
        "accept" => !value.bytes().any(is_cors_unsafe_byte),
        "accept-language" | "content-language" => value.bytes().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, b' ' | b'*' | b',' | b'-' | b'.' | b';' | b'=')
        }),
        "content-type" => {
            !value.bytes().any(is_cors_unsafe_byte)
                && mime_essence(value).is_some_and(|e| {
                    matches!(
                        e.as_str(),
                        "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain"
                    )
                })
        }
        _ => false,
    }
}

/// The names `no-cors` requests may carry at all.
pub fn is_no_cors_safelisted_name(name: &str) -> bool {
    matches!(
        lower(name).as_str(),
        "accept" | "accept-language" | "content-language" | "content-type"
    )
}

pub fn is_no_cors_safelisted_request_header(name: &str, value: &str) -> bool {
    is_no_cors_safelisted_name(name) && is_cors_safelisted_request_header(name, value)
}

/// The header names that make a request need a preflight: lower case, sorted, without
/// duplicates. `headers` are (name, value) pairs, in any case, and several entries of one
/// name count as one combined value for the safelist test (as the wire would carry them).
pub fn cors_unsafe_request_header_names(headers: &[(String, String)]) -> Vec<String> {
    let mut unsafe_names: Vec<String> = Vec::new();
    let mut maybe: Vec<String> = Vec::new();
    let mut size = 0usize;
    for (n, v) in headers {
        if is_cors_safelisted_request_header(n, v) {
            maybe.push(lower(n));
            size += v.len();
        } else {
            unsafe_names.push(lower(n));
        }
    }
    if size > 1024 {
        unsafe_names.append(&mut maybe);
    }
    unsafe_names.sort();
    unsafe_names.dedup();
    unsafe_names
}

// ---------------------------------------------------------------------------------------
// Header lists on the wire (responses)

/// The values of every header called `name` (lower case) in a response, split on commas and
/// trimmed (empty pieces dropped).
pub fn list_values(headers: &[(String, String)], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (k, v) in headers {
        if k.eq_ignore_ascii_case(name) {
            for piece in v.split(',') {
                let p = piece.trim_matches(is_http_whitespace);
                if !p.is_empty() {
                    out.push(String::from(p));
                }
            }
        }
    }
    out
}

/// The value of header `name` in a response: several entries joined with ", " (None: absent).
pub fn combined_value(headers: &[(String, String)], name: &str) -> Option<String> {
    let mut out: Option<String> = None;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case(name) {
            match &mut out {
                Some(s) => {
                    s.push_str(", ");
                    s.push_str(v);
                }
                None => out = Some(v.clone()),
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// The Headers object

/// How strictly a [`Headers`] guards what goes in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Guard {
    /// `new Headers()`: anything valid.
    None,
    /// Nothing may change (a Response from the network).
    Immutable,
    /// A Request's: forbidden request headers are silently ignored.
    Request,
    /// A `no-cors` Request's: only the no-CORS-safelisted headers stay.
    RequestNoCors,
    /// A Response's: forbidden response headers (Set-Cookie) are silently ignored.
    Response,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Headers {
    guard: Guard,
    /// (lower-case name, value) in the order added; several entries may share a name.
    list: Vec<(String, String)>,
}

impl Headers {
    pub fn new(guard: Guard) -> Headers {
        Headers {
            guard,
            list: Vec::new(),
        }
    }

    /// Build from pairs, applying the guard to each (the `Headers` constructor with a list).
    pub fn from_pairs(guard: Guard, pairs: &[(&str, &str)]) -> Result<Headers, NetError> {
        let mut h = Headers::new(Guard::None);
        for (n, v) in pairs {
            h.append(n, v)?;
        }
        h.guard = guard;
        // Apply the guard's silent drops to what is already in.
        h.list = h.filtered_for(guard);
        Ok(h)
    }

    fn filtered_for(&self, guard: Guard) -> Vec<(String, String)> {
        self.list
            .iter()
            .filter(|(n, v)| match guard {
                Guard::Request => !is_forbidden_request_header(n, v),
                Guard::RequestNoCors => {
                    !is_forbidden_request_header(n, v) && is_no_cors_safelisted_request_header(n, v)
                }
                Guard::Response => !is_forbidden_response_header(n),
                _ => true,
            })
            .cloned()
            .collect()
    }

    pub fn guard(&self) -> Guard {
        self.guard
    }

    /// Change the guard (`Immutable` once a Response is built, `RequestNoCors` when a
    /// Request is made with that mode, which also drops entries that mode doesn't allow).
    pub fn set_guard(&mut self, guard: Guard) {
        self.list = self.filtered_for(guard);
        self.guard = guard;
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The entries as they would be sent: lower-case names, insertion order.
    pub fn raw(&self) -> &[(String, String)] {
        &self.list
    }

    fn check_name(name: &str) -> Result<String, NetError> {
        if valid_header_name(name) {
            Ok(lower(name))
        } else {
            Err(NetError::BadHeaderName(truncate(name)))
        }
    }

    fn check_value(name: &str, value: &str) -> Result<String, NetError> {
        let v = trim_header_value(value);
        if valid_header_value(v) {
            Ok(String::from(v))
        } else {
            Err(NetError::BadHeaderValue(truncate(name)))
        }
    }

    fn total_bytes(&self) -> usize {
        self.list.iter().map(|(n, v)| n.len() + v.len() + 4).sum()
    }

    /// Is the guard going to silently drop `(name, value)`?
    fn ignored(&self, name: &str, combined: &str, value: &str) -> bool {
        match self.guard {
            Guard::Request => is_forbidden_request_header(name, value),
            Guard::RequestNoCors => {
                is_forbidden_request_header(name, value)
                    || !is_no_cors_safelisted_request_header(name, combined)
            }
            Guard::Response => is_forbidden_response_header(name),
            _ => false,
        }
    }

    fn room_for(&self, extra: usize, new_entry: bool) -> Result<(), NetError> {
        if new_entry && self.list.len() >= MAX_HEADERS {
            return Err(NetError::TooManyHeaders { max: MAX_HEADERS });
        }
        if self.total_bytes() + extra > MAX_HEADERS_BYTES {
            return Err(NetError::HeadersTooLarge {
                max: MAX_HEADERS_BYTES,
            });
        }
        Ok(())
    }

    /// `headers.append(name, value)`.
    pub fn append(&mut self, name: &str, value: &str) -> Result<(), NetError> {
        let n = Self::check_name(name)?;
        let v = Self::check_value(name, value)?;
        if self.guard == Guard::Immutable {
            return Err(NetError::HeadersImmutable);
        }
        let combined = match self.get_lower(&n) {
            Some(old) => {
                let mut c = old;
                c.push_str(", ");
                c.push_str(&v);
                c
            }
            None => v.clone(),
        };
        if self.ignored(&n, &combined, &v) {
            return Ok(());
        }
        self.room_for(n.len() + v.len() + 4, true)?;
        self.list.push((n, v));
        Ok(())
    }

    /// `headers.set(name, value)`.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), NetError> {
        let n = Self::check_name(name)?;
        let v = Self::check_value(name, value)?;
        if self.guard == Guard::Immutable {
            return Err(NetError::HeadersImmutable);
        }
        if self.ignored(&n, &v, &v) {
            return Ok(());
        }
        let before = self.list.clone();
        self.list.retain(|(k, _)| *k != n);
        if let Err(e) = self.room_for(n.len() + v.len() + 4, true) {
            self.list = before;
            return Err(e);
        }
        self.list.push((n, v));
        Ok(())
    }

    /// `headers.delete(name)`.
    pub fn delete(&mut self, name: &str) -> Result<(), NetError> {
        let n = Self::check_name(name)?;
        if self.guard == Guard::Immutable {
            return Err(NetError::HeadersImmutable);
        }
        let skip = match self.guard {
            Guard::Request => is_forbidden_request_header(&n, ""),
            Guard::RequestNoCors => {
                is_forbidden_request_header(&n, "") || !is_no_cors_safelisted_name(&n)
            }
            Guard::Response => is_forbidden_response_header(&n),
            _ => false,
        };
        if !skip {
            self.list.retain(|(k, _)| *k != n);
        }
        Ok(())
    }

    fn get_lower(&self, n: &str) -> Option<String> {
        let mut out: Option<String> = None;
        for (k, v) in &self.list {
            if k == n {
                match &mut out {
                    Some(s) => {
                        s.push_str(", ");
                        s.push_str(v);
                    }
                    None => out = Some(v.clone()),
                }
            }
        }
        out
    }

    /// `headers.get(name)`: all values of that name joined with ", ".
    pub fn get(&self, name: &str) -> Result<Option<String>, NetError> {
        let n = Self::check_name(name)?;
        Ok(self.get_lower(&n))
    }

    pub fn has(&self, name: &str) -> Result<bool, NetError> {
        let n = Self::check_name(name)?;
        Ok(self.list.iter().any(|(k, _)| *k == n))
    }

    /// `headers.getSetCookie()`: each Set-Cookie value on its own.
    pub fn get_set_cookie(&self) -> Vec<String> {
        self.list
            .iter()
            .filter(|(k, _)| k == "set-cookie")
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// What iteration (`entries()`, `forEach`, `for...of`) gives: sorted by name, one entry
    /// per name with the values joined by ", " (each Set-Cookie separate).
    pub fn entries(&self) -> Vec<(String, String)> {
        let mut names: Vec<&String> = self.list.iter().map(|(k, _)| k).collect();
        names.sort();
        names.dedup();
        let mut out = Vec::new();
        for n in names {
            if n == "set-cookie" {
                for (k, v) in &self.list {
                    if k == n {
                        out.push((k.clone(), v.clone()));
                    }
                }
            } else if let Some(v) = self.get_lower(n) {
                out.push((n.clone(), v));
            }
        }
        out
    }
}

impl core::fmt::Display for Guard {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.write_str(match self {
            Guard::None => "none",
            Guard::Immutable => "immutable",
            Guard::Request => "request",
            Guard::RequestNoCors => "request-no-cors",
            Guard::Response => "response",
        })
    }
}
