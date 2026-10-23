//! Who may frame a page: `X-Frame-Options` and the `frame-ancestors` directive of
//! `Content-Security-Policy`, read from the framed document's response, plus its CSP `sandbox`
//! directive. Pure: headers and addresses in, a verdict out.
//!
//! # What is enforced
//!
//! * **`X-Frame-Options`**: `DENY` (never framed) and `SAMEORIGIN` (framed only when the
//!   response's origin equals the origin of *every* ancestor, the top page included, as
//!   browsers do). Values are case-insensitive, may be repeated or comma-separated. `ALLOWALL`
//!   and unknown values (including the obsolete `ALLOW-FROM`, which no browser honours any
//!   more) are ignored and noted for the console. Two different values (`DENY` and
//!   `SAMEORIGIN`, or a valid and an invalid one) conflict and count as `DENY`.
//! * **`Content-Security-Policy: frame-ancestors`**: every enforced policy that has the
//!   directive must allow every ancestor. The source list: `'none'` (alone: nothing is
//!   allowed; an empty list is the same), `'self'` (the framed response's own origin), `*`
//!   (any http or https page), a scheme (`https:`; `http:` also covers https), a host with an
//!   optional scheme and port (`example.com`, `https://*.example.com`, `example.com:8080`,
//!   `example.com:*`, an IP address): a source without a scheme matches the framed
//!   response's scheme (or its https upgrade), a source without a port matches the scheme's
//!   default port, `*.` covers sub-domains but not the bare domain. A source with a path is
//!   invalid in `frame-ancestors` and ignored; other keywords and values (`'unsafe-inline'`,
//!   nonces, hashes) are ignored. When a policy has `frame-ancestors`, `X-Frame-Options` is
//!   ignored (CSP wins), as in browsers.
//! * **`Content-Security-Policy: sandbox`**: the framed document gets those restrictions on top
//!   of the iframe's attribute ([`sandbox_of`]).
//!
//! # What is skipped
//!
//! * Every other directive (`frame-src`, `child-src`, `default-src`, `script-src`, ...): the
//!   Browser has no CSP. So a page cannot restrict what *it* embeds.
//! * `Content-Security-Policy-Report-Only` (nothing is reported anywhere), `report-uri`,
//!   `report-to`, and a `<meta http-equiv=Content-Security-Policy>` (which browsers do not
//!   honour for `frame-ancestors` either).
//! * `Cross-Origin-Opener-Policy`, `Cross-Origin-Embedder-Policy` and
//!   `Cross-Origin-Resource-Policy` for documents.
//! * The path of a source (see above) and internationalised host names.
//!
//! "Ancestor" addresses: the caller passes, for each ancestor frame from the top down to the
//! parent, the address its *document* was loaded from; an `about:blank` or `srcdoc` document
//! uses the address of the document that created it, and a `data:` document has none (`None`:
//! no source matches it, and it is not the same origin as anything).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use milkweb::url::{Scheme, Url};

use super::sandbox::Sandbox;

/// What `X-Frame-Options` asks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Xfo {
    /// Absent, or only values that are ignored.
    None,
    Deny,
    SameOrigin,
}

/// The reading of the `X-Frame-Options` header(s), and what to tell the console about it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct XfoReading {
    pub xfo: Xfo,
    /// The value as written (for the message), "" if none.
    pub shown: String,
    pub notes: Vec<String>,
}

fn header_values<'a>(headers: &'a [(String, String)], name: &str) -> Vec<&'a str> {
    headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .collect()
}

/// Read `X-Frame-Options` out of a response's headers.
pub fn parse_xfo(headers: &[(String, String)]) -> XfoReading {
    let mut tokens: Vec<String> = Vec::new();
    for v in header_values(headers, "x-frame-options") {
        for part in v.split(',') {
            let t = part.trim();
            if !t.is_empty() {
                tokens.push(t.to_ascii_lowercase());
            }
        }
    }
    let mut notes = Vec::new();
    if tokens.is_empty() {
        return XfoReading {
            xfo: Xfo::None,
            shown: String::new(),
            notes,
        };
    }
    let first = tokens[0].clone();
    if tokens.iter().any(|t| *t != first) {
        notes.push(format!(
            "Multiple 'X-Frame-Options' headers with conflicting values ('{}') encountered. Falling back to 'DENY'.",
            tokens.join(", ")
        ));
        return XfoReading {
            xfo: Xfo::Deny,
            shown: tokens.join(", "),
            notes,
        };
    }
    let xfo = match first.as_str() {
        "deny" => Xfo::Deny,
        "sameorigin" => Xfo::SameOrigin,
        "allowall" => Xfo::None,
        other => {
            let shown = clip(other);
            if other.starts_with("allow-from") {
                notes.push(format!(
                    "Invalid 'X-Frame-Options' header encountered: '{shown}' is obsolete and ignored. Use 'Content-Security-Policy: frame-ancestors' instead."
                ));
            } else {
                notes.push(format!(
                    "Invalid 'X-Frame-Options' header encountered: '{shown}' is not a recognized directive. The header will be ignored."
                ));
            }
            Xfo::None
        }
    };
    XfoReading {
        xfo,
        shown: first,
        notes,
    }
}

fn clip(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(60).collect()
}

// ---- Content-Security-Policy ----

/// One source of a `frame-ancestors` list that can match something.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AncSource {
    /// `'self'`.
    SelfOrigin,
    /// `*`.
    Any,
    /// `https:`.
    Scheme(String),
    Host {
        scheme: Option<String>,
        /// Lower case; may start with `*.`; `*` alone is [`AncSource::Any`] when no scheme or port
        /// is given and a host wildcard otherwise.
        host: String,
        port: Port,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Port {
    /// No port written: the scheme's default.
    Default,
    Any,
    Number(u16),
}

/// A `frame-ancestors` list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AncestorList {
    /// `'none'` alone, or nothing at all: nothing matches.
    pub none: bool,
    pub sources: Vec<AncSource>,
    /// The directive as written, for messages.
    pub text: String,
}

/// One policy of a `Content-Security-Policy` header, as far as framing goes.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Policy {
    pub frame_ancestors: Option<AncestorList>,
    pub sandbox: Option<Sandbox>,
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c')
}

/// Parse the enforced policies out of a response's `Content-Security-Policy` headers (one
/// header may hold several, comma separated). Policies with nothing about framing come back
/// empty (they are still policies, but have nothing for us).
pub fn parse_csp(headers: &[(String, String)]) -> Vec<Policy> {
    let mut out = Vec::new();
    for v in header_values(headers, "content-security-policy") {
        for pol in v.split(',') {
            let mut p = Policy::default();
            let mut seen: Vec<String> = Vec::new();
            for dir in pol.split(';') {
                let dir = dir.trim_matches(is_ws);
                if dir.is_empty() {
                    continue;
                }
                let mut it = dir.split(is_ws).filter(|s| !s.is_empty());
                let Some(name) = it.next() else { continue };
                if !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')
                {
                    continue;
                }
                let name = name.to_ascii_lowercase();
                if seen.contains(&name) {
                    continue; // the first one counts
                }
                seen.push(name.clone());
                let values: Vec<&str> = it.collect();
                match name.as_str() {
                    "frame-ancestors" => p.frame_ancestors = Some(parse_list(&values)),
                    "sandbox" => p.sandbox = Some(Sandbox::parse(&values.join(" ")).0),
                    _ => {}
                }
            }
            out.push(p);
        }
    }
    out
}

fn parse_list(values: &[&str]) -> AncestorList {
    let text = clip(&values.join(" "));
    let lower: Vec<String> = values.iter().map(|v| v.to_ascii_lowercase()).collect();
    if lower.is_empty() || (lower.len() == 1 && lower[0] == "'none'") {
        return AncestorList {
            none: true,
            sources: Vec::new(),
            text,
        };
    }
    let mut sources = Vec::new();
    for v in &lower {
        if v == "'none'" {
            continue; // ignored when there are other sources
        }
        if v == "'self'" {
            sources.push(AncSource::SelfOrigin);
        } else if v == "*" {
            sources.push(AncSource::Any);
        } else if let Some(s) = parse_source(v) {
            sources.push(s);
        }
    }
    AncestorList {
        none: false,
        sources,
        text,
    }
}

fn parse_source(v: &str) -> Option<AncSource> {
    if v.starts_with('\'') {
        return None; // 'unsafe-inline', 'nonce-..', 'sha256-..' and the like
    }
    // A scheme source: `https:`
    if let Some(s) = v.strip_suffix(':')
        && !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
    {
        return Some(AncSource::Scheme(String::from(s)));
    }
    let (scheme, rest) = match v.find("://") {
        Some(i) => {
            let s = &v[..i];
            if s.is_empty()
                || !s.chars().next().unwrap().is_ascii_alphabetic()
                || !s
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            {
                return None;
            }
            (Some(String::from(s)), &v[i + 3..])
        }
        None => (None, v),
    };
    if rest.contains('/') || rest.contains('?') || rest.contains('#') {
        return None; // a path: invalid in frame-ancestors
    }
    let (host, port) = if let Some(r) = rest.strip_prefix('[') {
        // [::1] or [::1]:8080
        let close = r.find(']')?;
        let h = &r[..close];
        let after = &r[close + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p),
            None if after.is_empty() => None,
            None => return None,
        };
        (String::from(h), port)
    } else {
        match rest.rsplit_once(':') {
            Some((h, p)) => (String::from(h), Some(p)),
            None => (String::from(rest), None),
        }
    };
    let port = match port {
        None => Port::Default,
        Some("*") => Port::Any,
        Some(p) => Port::Number(p.parse::<u16>().ok().filter(|n| *n > 0)?),
    };
    if host.is_empty() {
        return None;
    }
    // Hosts: letters, digits, `-`, `.`, `_`; a leading `*.` wildcard; `*` alone; IPv6.
    let body = host.strip_prefix("*.").unwrap_or(&host);
    let ok = body == "*" && host == "*"
        || (!body.is_empty()
            && body
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':')));
    if !ok {
        return None;
    }
    if host == "*" && scheme.is_none() && port == Port::Default {
        return Some(AncSource::Any);
    }
    Some(AncSource::Host { scheme, host, port })
}

/// Does `scheme` (a source's) cover the address's scheme? `http` covers https too (upgrade).
fn scheme_covers(source: &str, url: &Url) -> bool {
    match url.scheme {
        Scheme::Http => source == "http",
        Scheme::Https => source == "https" || source == "http",
    }
}

fn matches_host(host_src: &str, host: &str) -> bool {
    if host_src == "*" {
        return !host.is_empty();
    }
    if let Some(suffix) = host_src.strip_prefix("*.") {
        return host.len() > suffix.len() + 1
            && host.ends_with(suffix)
            && host.as_bytes()[host.len() - suffix.len() - 1] == b'.';
    }
    host_src == host
}

impl AncSource {
    /// Does this source cover `ancestor`? `me`: the framed response's own address (for
    /// `'self'`, and for a source that names no scheme).
    pub fn matches(&self, ancestor: &Url, me: &Url) -> bool {
        match self {
            AncSource::SelfOrigin => same_origin_upgrade(ancestor, me),
            AncSource::Any => true, // http(s) only: that is all an Url can be
            AncSource::Scheme(s) => scheme_covers(&s.to_ascii_lowercase(), ancestor),
            AncSource::Host { scheme, host, port } => {
                match scheme {
                    Some(s) => {
                        if !scheme_covers(s, ancestor) {
                            return false;
                        }
                    }
                    None => {
                        // The scheme of the framed response, or its https upgrade.
                        let ok = match me.scheme {
                            Scheme::Http => true,
                            Scheme::Https => ancestor.scheme == Scheme::Https,
                        };
                        if !ok {
                            return false;
                        }
                    }
                }
                if !matches_host(host, &ancestor.host) {
                    return false;
                }
                let upgraded = scheme.as_deref() == Some("http") && ancestor.scheme == Scheme::Https
                    || (scheme.is_none() && me.scheme == Scheme::Http && ancestor.scheme == Scheme::Https);
                match port {
                    Port::Any => true,
                    Port::Default => ancestor.port == ancestor.scheme.default_port(),
                    Port::Number(n) => {
                        ancestor.port == *n
                            || (upgraded && *n == 80 && ancestor.port == 443)
                    }
                }
            }
        }
    }
}

/// `'self'`: the same host and port, and the same scheme or an https upgrade of an http one.
fn same_origin_upgrade(a: &Url, me: &Url) -> bool {
    if !a.host.eq_ignore_ascii_case(&me.host) {
        return false;
    }
    if a.scheme == me.scheme {
        return a.port == me.port;
    }
    // http -> https with the default ports.
    me.scheme == Scheme::Http && a.scheme == Scheme::Https && me.port == 80 && a.port == 443
}

impl AncestorList {
    /// Does this list allow `ancestor`?
    pub fn allows(&self, ancestor: Option<&Url>, me: &Url) -> bool {
        if self.none {
            return false;
        }
        let Some(a) = ancestor else { return false };
        self.sources.iter().any(|s| s.matches(a, me))
    }
}

// ---- The verdict ----

/// Why a document may not be shown in a frame.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Blocked {
    /// `X-Frame-Options` said so: the header's value.
    XFrameOptions(String),
    /// A `frame-ancestors` directive did not allow an ancestor.
    FrameAncestors(String),
}

impl Blocked {
    /// The line for the console.
    pub fn console(&self, url: &Url) -> String {
        match self {
            Blocked::XFrameOptions(v) => format!(
                "Refused to display '{}' in a frame because it set 'X-Frame-Options' to '{}'.",
                url.to_text(),
                v
            ),
            Blocked::FrameAncestors(d) => format!(
                "Refused to frame '{}' because an ancestor violates the following Content Security Policy directive: \"frame-ancestors {}\".",
                url.to_text(),
                d
            ),
        }
    }

    /// The short reason an error page in the frame gives.
    pub fn reason(&self) -> String {
        match self {
            Blocked::XFrameOptions(v) => {
                format!("This site doesn't allow itself to be shown in a frame here (it sent X-Frame-Options: {v}).")
            }
            Blocked::FrameAncestors(_) => String::from(
                "This site doesn't allow itself to be shown in a frame on this page (its Content-Security-Policy says which pages may frame it).",
            ),
        }
    }
}

/// The result of reading the framing headers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Embedding {
    pub blocked: Option<Blocked>,
    /// Lines for the console about headers that were ignored.
    pub notes: Vec<String>,
}

/// May the response `resp` (address `url`) be shown in a frame whose ancestors, top page
/// first, were loaded from `ancestors`? A top-level document (no ancestors) is never refused.
pub fn check_embedding(
    headers: &[(String, String)],
    url: &Url,
    ancestors: &[Option<Url>],
) -> Embedding {
    let mut notes = Vec::new();
    if ancestors.is_empty() {
        return Embedding {
            blocked: None,
            notes,
        };
    }
    let policies = parse_csp(headers);
    let mut any_fa = false;
    for p in &policies {
        let Some(list) = &p.frame_ancestors else { continue };
        any_fa = true;
        for a in ancestors {
            if !list.allows(a.as_ref(), url) {
                return Embedding {
                    blocked: Some(Blocked::FrameAncestors(list.text.clone())),
                    notes,
                };
            }
        }
    }
    let xfo = parse_xfo(headers);
    if any_fa {
        // CSP wins; X-Frame-Options is not even read for its verdict.
        if xfo.xfo != Xfo::None {
            notes.push(String::from(
                "'X-Frame-Options' is ignored because the Content-Security-Policy has a 'frame-ancestors' directive.",
            ));
        }
        return Embedding {
            blocked: None,
            notes,
        };
    }
    notes.extend(xfo.notes.iter().cloned());
    let blocked = match xfo.xfo {
        Xfo::None => None,
        Xfo::Deny => Some(Blocked::XFrameOptions(xfo.shown.to_ascii_uppercase())),
        Xfo::SameOrigin => {
            let same = ancestors.iter().all(|a| {
                a.as_ref().is_some_and(|a| {
                    a.scheme == url.scheme
                        && a.host.eq_ignore_ascii_case(&url.host)
                        && a.port == url.port
                })
            });
            (!same).then(|| Blocked::XFrameOptions(xfo.shown.to_ascii_uppercase()))
        }
    };
    Embedding { blocked, notes }
}

/// The restrictions a response's CSP `sandbox` directives put on its document.
pub fn sandbox_of(headers: &[(String, String)]) -> Sandbox {
    let mut s = Sandbox::NONE;
    for p in parse_csp(headers) {
        if let Some(sb) = p.sandbox {
            s = s.merge(sb);
        }
    }
    s
}

#[allow(dead_code)]
fn _unused(_: &dyn ToString) {}
