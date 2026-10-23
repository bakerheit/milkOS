//! Web addresses: `http://host[:port]/path?query` and `https://...` (RFC 3986, the parts a
//! browser needs). Hosts are lower-cased; an IPv6 literal is kept without its brackets.
//! The fragment (`#...`) is dropped: it never goes to the server.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scheme {
    Http,
    Https,
}

impl Scheme {
    pub fn name(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Scheme::Http => 80,
            Scheme::Https => 443,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Url {
    pub scheme: Scheme,
    /// Lower case; an IPv6 literal without brackets.
    pub host: String,
    pub port: u16,
    /// Path and query, always starting with `/`, percent-encoded where it had to be.
    pub path: String,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum UrlError {
    Empty,
    /// A scheme other than http / https (`ftp`, `mailto`, ...).
    Scheme(String),
    NoHost,
    BadHost(String),
    BadPort(String),
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            UrlError::Empty => f.write_str("there's no address"),
            UrlError::Scheme(s) => write!(
                f,
                "{s}: addresses can't be opened here (only http:// and https://)"
            ),
            UrlError::NoHost => f.write_str("the address has no host name"),
            UrlError::BadHost(h) => write!(f, "'{h}' isn't a host name"),
            UrlError::BadPort(p) => write!(f, "'{p}' isn't a port number"),
        }
    }
}

/// `scheme:` at the start of `s` (letters, digits, `+-.`, starting with a letter), lower case.
fn scheme_of(s: &str) -> Option<(String, &str)> {
    let i = s.find(':')?;
    let name = &s[..i];
    let ok = !name.is_empty()
        && name.as_bytes()[0].is_ascii_alphabetic()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'));
    ok.then(|| (name.to_ascii_lowercase(), &s[i + 1..]))
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_'))
        && !h.starts_with('.')
        && !h.contains("..")
}

fn valid_v6(h: &str) -> bool {
    h.contains(':')
        && h.bytes()
            .all(|c| c.is_ascii_hexdigit() || c == b':' || c == b'.')
}

/// Percent-encode what may not appear raw in a request line (spaces, controls, non-ASCII,
/// `"<>\^`{|}`). An existing `%XX` is kept.
fn encode_path(p: &str) -> String {
    let mut o = String::with_capacity(p.len());
    for &c in p.as_bytes() {
        if c <= b' '
            || c >= 0x7f
            || matches!(
                c,
                b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'
            )
        {
            o.push_str(&format!("%{c:02X}"));
        } else {
            o.push(c as char);
        }
    }
    o
}

/// RFC 3986 5.2.4: remove `.` and `..` segments from a path (the query is left alone).
fn remove_dots(path_query: &str) -> String {
    let (path, query) = match path_query.find('?') {
        Some(i) => (&path_query[..i], &path_query[i..]),
        None => (path_query, ""),
    };
    let mut out: Vec<&str> = Vec::new();
    let segs: Vec<&str> = path.split('/').collect();
    let n = segs.len();
    for (k, seg) in segs.iter().enumerate() {
        let last = k + 1 == n;
        match *seg {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let mut p = out.join("/");
    if !p.starts_with('/') {
        p.insert(0, '/');
    }
    p + query
}

impl Url {
    /// Parse an address. No scheme means http (`example.com/x`); `//host/x` too.
    pub fn parse(s: &str) -> Result<Url, UrlError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(UrlError::Empty);
        }
        let (scheme, rest) = match scheme_of(s) {
            Some((name, rest)) if rest.starts_with("//") => match name.as_str() {
                "http" => (Scheme::Http, &rest[2..]),
                "https" => (Scheme::Https, &rest[2..]),
                _ => return Err(UrlError::Scheme(name)),
            },
            // `host:port/...` looks like a scheme but isn't one (the "scheme" is the host).
            Some((name, rest))
                if !rest.starts_with(|c: char| c.is_ascii_digit()) && !name.contains('.') =>
            {
                if name == "localhost" {
                    (Scheme::Http, s)
                } else {
                    return Err(UrlError::Scheme(name));
                }
            }
            _ => (Scheme::Http, s.strip_prefix("//").unwrap_or(s)),
        };
        Self::with_scheme(scheme, rest)
    }

    /// `rest` is `authority[/path][?query][#fragment]`.
    fn with_scheme(scheme: Scheme, rest: &str) -> Result<Url, UrlError> {
        let rest = rest.split('#').next().unwrap_or("");
        let end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (auth, path) = rest.split_at(end);
        // user:pass@ is dropped (never sent).
        let auth = auth.rsplit('@').next().unwrap_or(auth);
        let (host, port) = if let Some(r) = auth.strip_prefix('[') {
            let close = r
                .find(']')
                .ok_or_else(|| UrlError::BadHost(String::from(auth)))?;
            let h = &r[..close];
            if !valid_v6(h) {
                return Err(UrlError::BadHost(String::from(h)));
            }
            let after = &r[close + 1..];
            let port = match after.strip_prefix(':') {
                Some(p) => Some(p),
                None if after.is_empty() => None,
                None => return Err(UrlError::BadHost(String::from(auth))),
            };
            (h, port)
        } else {
            match auth.rsplit_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (auth, None),
            }
        };
        if host.is_empty() {
            return Err(UrlError::NoHost);
        }
        let host = host.to_ascii_lowercase();
        let host = host.strip_suffix('.').map(String::from).unwrap_or(host);
        if !host.contains(':') && !valid_host(&host) {
            return Err(UrlError::BadHost(host));
        }
        let port = match port {
            None | Some("") => scheme.default_port(),
            Some(p) => match p.parse::<u16>() {
                Ok(n) if n > 0 => n,
                _ => return Err(UrlError::BadPort(String::from(p))),
            },
        };
        let path = if path.is_empty() {
            String::from("/")
        } else if path.starts_with('?') {
            format!("/{path}")
        } else {
            remove_dots(path)
        };
        Ok(Url {
            scheme,
            host,
            port,
            path: encode_path(&path),
        })
    }

    /// `host` or `host:port` (when not the default), with brackets for IPv6: the Host header.
    pub fn host_port(&self) -> String {
        let h = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == self.scheme.default_port() {
            h
        } else {
            format!("{h}:{}", self.port)
        }
    }

    /// The address as text (`http://example.com/`).
    pub fn to_text(&self) -> String {
        format!("{}://{}{}", self.scheme.name(), self.host_port(), self.path)
    }

    /// Is the host a numeric address (IPv4 dotted quad or IPv6)?
    pub fn is_ip_literal(&self) -> bool {
        self.host.contains(':') || is_dotted_quad(&self.host)
    }

    /// Resolve a link (`href`) found on this page (RFC 3986 5.2).
    pub fn join(&self, href: &str) -> Result<Url, UrlError> {
        let href = href.trim();
        let href = href.split('#').next().unwrap_or("");
        if href.is_empty() {
            return Ok(self.clone());
        }
        if let Some((name, rest)) = scheme_of(href) {
            match name.as_str() {
                "http" | "https" if rest.starts_with("//") => return Url::parse(href),
                // `http:path` (no //) is relative in practice: drop the scheme.
                "http" | "https" => return self.join(rest),
                _ if !name.contains('.') => return Err(UrlError::Scheme(name)),
                _ => {}
            }
        }
        if let Some(r) = href.strip_prefix("//") {
            return Self::with_scheme(self.scheme, r);
        }
        let mut u = self.clone();
        let path = if href.starts_with('/') {
            remove_dots(href)
        } else if href.starts_with('?') {
            let base = self.path.split('?').next().unwrap_or("/");
            format!("{base}{href}")
        } else {
            let base = self.path.split('?').next().unwrap_or("/");
            let dir = match base.rfind('/') {
                Some(i) => &base[..=i],
                None => "/",
            };
            remove_dots(&format!("{dir}{href}"))
        };
        u.path = encode_path(&path);
        Ok(u)
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.to_text())
    }
}

pub fn is_dotted_quad(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
}

/// What the Browser's address bar should do with what was typed: an address, or a search
/// (a bare word or words that can't be a host: no dot, no colon, no slash, not localhost).
pub fn looks_like_search(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if t.contains(char::is_whitespace) && !t.contains("://") {
        return true;
    }
    !(t.contains('.') || t.contains(':') || t.contains('/') || t.eq_ignore_ascii_case("localhost"))
}

/// `q=` query text: letters, digits and `-_.~` as they are, spaces as `+`, the rest `%XX`.
pub fn query_encode(s: &str) -> String {
    let mut o = String::new();
    for &c in s.trim().as_bytes() {
        match c {
            b' ' => o.push('+'),
            c if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') => {
                o.push(c as char)
            }
            c => o.push_str(&format!("%{c:02X}")),
        }
    }
    o
}

/// The search address for a bare word (DuckDuckGo's lite, text-only page).
pub fn search_url(words: &str) -> String {
    format!("http://lite.duckduckgo.com/lite/?q={}", query_encode(words))
}

impl From<UrlError> for String {
    fn from(e: UrlError) -> String {
        e.to_string()
    }
}
