//! Origins and sites, address classes, the web's list of refused ports, and turning the
//! text a script gives `fetch` into an address we are willing to talk to ([`resolve_url`]).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use milkweb::url::{Scheme, Url};

use super::error::NetError;
use super::limits::Limits;
use crate::cookies::site_of;

/// A web origin: scheme, host and port, or an opaque one (a page with no usable address: a
/// sandboxed or `data:` document). The Browser's pages are http(s), so a page's origin is a
/// tuple unless the bindings make an [`Origin::Opaque`] for a special document.
///
/// `==` is the same-origin test: two opaque origins are the same only if they carry the same
/// number.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Origin {
    Opaque(u64),
    Tuple {
        scheme: Scheme,
        /// Lower case; an IPv6 literal without brackets (as [`Url::host`]).
        host: String,
        port: u16,
    },
}

fn host_port_text(scheme: Scheme, host: &str, port: u16) -> String {
    let h = if host.contains(':') {
        format!("[{host}]")
    } else {
        String::from(host)
    };
    if port == scheme.default_port() {
        h
    } else {
        format!("{h}:{port}")
    }
}

impl Origin {
    pub fn of(url: &Url) -> Origin {
        Origin::Tuple {
            scheme: url.scheme,
            host: url.host.to_ascii_lowercase(),
            port: url.port,
        }
    }

    pub fn is_opaque(&self) -> bool {
        matches!(self, Origin::Opaque(_))
    }

    /// The origin as the `Origin` header and `Access-Control-Allow-Origin` write it:
    /// `https://example.com`, `http://localhost:8080`, `null`.
    pub fn serialize(&self) -> String {
        match self {
            Origin::Opaque(_) => String::from("null"),
            Origin::Tuple { scheme, host, port } => {
                format!(
                    "{}://{}",
                    scheme.name(),
                    host_port_text(*scheme, host, *port)
                )
            }
        }
    }

    pub fn same_origin(&self, other: &Origin) -> bool {
        self == other
    }

    /// The registrable-domain test, with the scheme in it (`http://a.test` and
    /// `https://a.test` are different sites, as in current browsers' cookie rules). Uses the
    /// cookie code's site rule (no full public suffix list: a known gap).
    pub fn same_site(&self, other: &Origin) -> bool {
        match (self, other) {
            (
                Origin::Tuple {
                    scheme: s1,
                    host: h1,
                    ..
                },
                Origin::Tuple {
                    scheme: s2,
                    host: h2,
                    ..
                },
            ) => s1 == s2 && site_of(h1) == site_of(h2),
            _ => self == other,
        }
    }

    /// The site (cookie rule) of a tuple origin, for the cookie jar's `page_site`.
    pub fn site(&self) -> Option<String> {
        match self {
            Origin::Tuple { host, .. } => Some(site_of(host)),
            Origin::Opaque(_) => None,
        }
    }

    pub fn scheme(&self) -> Option<Scheme> {
        match self {
            Origin::Tuple { scheme, .. } => Some(*scheme),
            Origin::Opaque(_) => None,
        }
    }

    pub fn host(&self) -> Option<&str> {
        match self {
            Origin::Tuple { host, .. } => Some(host),
            Origin::Opaque(_) => None,
        }
    }

    /// Public, private network or this machine (opaque origins count as public).
    pub fn address_space(&self) -> AddressSpace {
        match self {
            Origin::Tuple { host, .. } => address_space_of_host(host),
            Origin::Opaque(_) => AddressSpace::Public,
        }
    }
}

/// Where an address sits, from the host text alone (no name lookup is done here: a name that
/// points at a private address is a known gap, see the module docs of `netpolicy`).
/// Ordered by how private: a request may go to the same or a more public space.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum AddressSpace {
    Public,
    /// 10/8, 172.16/12, 192.168/16, 169.254/16, 100.64/10, fc00::/7, fe80::/10, `*.local`,
    /// one-label names and the like.
    Private,
    /// 127/8, 0/8, ::1, `localhost` and `*.localhost`.
    Loopback,
}

impl AddressSpace {
    /// Is `self` (the page) allowed to ask `target` for something without the target's
    /// say-so? Only the same or a more public space.
    pub fn may_reach(self, target: AddressSpace) -> bool {
        target <= self
    }
}

/// `a.b.c.d`, each part 0 to 255 in plain decimal (no leading zeros, no octal or hex forms).
pub fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut n = 0;
    for part in s.split('.') {
        if n >= 4 || part.is_empty() || part.len() > 3 || !part.bytes().all(|c| c.is_ascii_digit())
        {
            return None;
        }
        if part.len() > 1 && part.starts_with('0') {
            return None;
        }
        out[n] = part.parse::<u16>().ok().filter(|v| *v <= 255)? as u8;
        n += 1;
    }
    (n == 4).then_some(out)
}

/// An IPv6 literal without brackets: hex groups, one `::`, an optional dotted-quad tail.
/// No zone ids.
pub fn parse_ipv6(s: &str) -> Option<[u16; 8]> {
    if s.is_empty() || s.len() > 45 || !s.contains(':') {
        return None;
    }
    let (head, tail) = match s.find("::") {
        Some(i) => {
            if s[i + 2..].contains("::") {
                return None;
            }
            (&s[..i], Some(&s[i + 2..]))
        }
        None => (s, None),
    };
    fn groups(part: &str, last_allows_v4: bool) -> Option<Vec<u16>> {
        let mut out = Vec::new();
        if part.is_empty() {
            return Some(out);
        }
        let items: Vec<&str> = part.split(':').collect();
        let n = items.len();
        for (i, g) in items.iter().enumerate() {
            if i + 1 == n && last_allows_v4 && g.contains('.') {
                let v4 = parse_ipv4(g)?;
                out.push(u16::from_be_bytes([v4[0], v4[1]]));
                out.push(u16::from_be_bytes([v4[2], v4[3]]));
            } else {
                if g.is_empty() || g.len() > 4 || !g.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return None;
                }
                out.push(u16::from_str_radix(g, 16).ok()?);
            }
        }
        Some(out)
    }
    let mut v = [0u16; 8];
    match tail {
        None => {
            let g = groups(head, true)?;
            if g.len() != 8 {
                return None;
            }
            v.copy_from_slice(&g);
        }
        Some(t) => {
            let a = groups(head, false)?;
            let b = groups(t, true)?;
            if a.len() + b.len() > 7 {
                return None;
            }
            v[..a.len()].copy_from_slice(&a);
            v[8 - b.len()..].copy_from_slice(&b);
        }
    }
    Some(v)
}

fn space_of_v4(a: [u8; 4]) -> AddressSpace {
    match a {
        [127, ..] | [0, ..] => AddressSpace::Loopback,
        [10, ..] | [192, 168, ..] | [169, 254, ..] => AddressSpace::Private,
        [172, b, ..] if (16..=31).contains(&b) => AddressSpace::Private,
        [100, b, ..] if (64..=127).contains(&b) => AddressSpace::Private,
        // Multicast, reserved and broadcast are never "public".
        [a0, ..] if a0 >= 224 => AddressSpace::Private,
        _ => AddressSpace::Public,
    }
}

fn space_of_v6(v: [u16; 8]) -> AddressSpace {
    if v == [0; 8] || v == [0, 0, 0, 0, 0, 0, 0, 1] {
        return AddressSpace::Loopback;
    }
    let v4 = |hi: u16, lo: u16| [(hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8];
    // ::ffff:a.b.c.d (mapped) and ::a.b.c.d (the old compatible form)
    if v[..5] == [0, 0, 0, 0, 0] && (v[5] == 0xffff || v[5] == 0) {
        return space_of_v4(v4(v[6], v[7]));
    }
    // 64:ff9b::a.b.c.d (NAT64) and 2002:a.b.c.d:: (6to4) carry an IPv4 address too
    if v[0] == 0x64 && v[1] == 0xff9b && v[2..6] == [0, 0, 0, 0] {
        return space_of_v4(v4(v[6], v[7]));
    }
    if v[0] == 0x2002 {
        return space_of_v4(v4(v[1], v[2]));
    }
    match v[0] {
        x if x & 0xfe00 == 0xfc00 => AddressSpace::Private,
        x if x & 0xffc0 == 0xfe80 => AddressSpace::Private,
        x if x & 0xff00 == 0xff00 => AddressSpace::Private,
        _ => AddressSpace::Public,
    }
}

/// Classify a host (as in [`Url::host`]).
pub fn address_space_of_host(host: &str) -> AddressSpace {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.contains(':') {
        // Not a valid literal: treat as the most private (never reachable from outside).
        return parse_ipv6(&host).map_or(AddressSpace::Loopback, space_of_v6);
    }
    if let Some(a) = parse_ipv4(&host) {
        return space_of_v4(a);
    }
    if host == "localhost" || host.ends_with(".localhost") {
        return AddressSpace::Loopback;
    }
    if !host.contains('.') {
        return AddressSpace::Private;
    }
    for suffix in [
        ".local",
        ".internal",
        ".lan",
        ".home.arpa",
        ".intranet",
        ".corp",
        ".home",
    ] {
        if host.ends_with(suffix) {
            return AddressSpace::Private;
        }
    }
    AddressSpace::Public
}

/// Classify a resolved address: 4 bytes (IPv4) or 16 bytes (IPv6), as a name lookup gives
/// them. `None` for any other length.
pub fn address_space_of_ip(ip: &[u8]) -> Option<AddressSpace> {
    match ip.len() {
        4 => Some(space_of_v4([ip[0], ip[1], ip[2], ip[3]])),
        16 => {
            let mut v = [0u16; 8];
            for (i, w) in v.iter_mut().enumerate() {
                *w = u16::from_be_bytes([ip[2 * i], ip[2 * i + 1]]);
            }
            Some(space_of_v6(v))
        }
        _ => None,
    }
}

/// "Potentially trustworthy" (secure-contexts): https, or http to this machine.
pub fn is_potentially_trustworthy(url: &Url) -> bool {
    url.scheme == Scheme::Https || is_loopback_host(&url.host)
}

/// `localhost`, `*.localhost`, 127/8, ::1 (what mixed-content and referrer rules exempt).
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.contains(':') {
        return parse_ipv6(&host) == Some([0, 0, 0, 0, 0, 0, 0, 1]);
    }
    if let Some([127, ..]) = parse_ipv4(&host) {
        return true;
    }
    host == "localhost" || host.ends_with(".localhost")
}

/// The ports the web refuses to talk to (the Fetch standard's "bad ports": mail, ssh, ftp,
/// printers, irc, and other services a hostile page could poke).
pub const BAD_PORTS: &[u16] = &[
    1, 7, 9, 11, 13, 15, 17, 19, 20, 21, 22, 23, 25, 37, 42, 43, 53, 69, 77, 79, 87, 95, 101, 102,
    103, 104, 109, 110, 111, 113, 115, 117, 119, 123, 135, 137, 139, 143, 161, 179, 389, 427, 465,
    512, 513, 514, 515, 526, 530, 531, 532, 540, 548, 554, 556, 563, 587, 601, 636, 989, 990, 993,
    995, 1719, 1720, 1723, 2049, 3659, 4045, 4190, 5060, 5061, 6000, 6566, 6665, 6666, 6667, 6668,
    6669, 6679, 6697, 10080,
];

pub fn is_bad_port(port: u16) -> bool {
    BAD_PORTS.contains(&port)
}

// ---------------------------------------------------------------------------------------
// Addresses a script gives us

/// A decoded `data:` address.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DataUrl {
    /// The `Content-Type` to report.
    pub mime: String,
    pub body: Vec<u8>,
}

/// What an address from a script names.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Target {
    /// Go to the network (the policy's checks come next).
    Http(Url),
    /// Content in the address itself: no network, same-origin treatment (as the standard
    /// does), GET only, still under the size limit.
    Data(DataUrl),
    /// `blob:` (an object URL the bindings made): the whole address, for them to look up.
    /// Only a blob made by this origin is acceptable (checked by the Exchange).
    Blob(String),
}

/// `scheme:` at the start of `s`, per the URL standard (a letter, then letters, digits and
/// `+-.`).
fn scheme_prefix(s: &str) -> Option<(String, &str)> {
    let i = s.find(':')?;
    let name = &s[..i];
    let mut it = name.bytes();
    let first = it.next()?;
    if !first.is_ascii_alphabetic()
        || !it.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
    {
        return None;
    }
    Some((name.to_ascii_lowercase(), &s[i + 1..]))
}

/// The authority part of what follows `//`.
fn authority_of(after_slashes: &str) -> &str {
    let end = after_slashes
        .find(['/', '?', '#'])
        .unwrap_or(after_slashes.len());
    &after_slashes[..end]
}

/// A host the network layer may be handed: a name or a strict IPv4 or IPv6 literal.
/// A host whose last label is a number must be a plain dotted quad (browsers read `127.1`,
/// `0x7f.1` and `2130706433` as 127.0.0.1; we refuse them, so the address classes above
/// can't be dodged).
pub fn host_ok(host: &str) -> bool {
    if host.contains(':') {
        return parse_ipv6(host).is_some();
    }
    // Name syntax: labels of 1 to 63 letters, digits, `-` and `_`, not starting or ending
    // with a hyphen.
    for label in host.split('.') {
        let b = label.as_bytes();
        if b.is_empty() || b.len() > 63 || b[0] == b'-' || b[b.len() - 1] == b'-' {
            return false;
        }
    }
    let last = host.rsplit('.').next().unwrap_or("");
    let numeric = !last.is_empty() && last.bytes().all(|c| c.is_ascii_digit());
    let hex = last.len() >= 2 && (last.starts_with("0x") || last.starts_with("0X"));
    if numeric || hex {
        return parse_ipv4(host).is_some();
    }
    true
}

fn url_error(e: milkweb::url::UrlError) -> NetError {
    match e {
        milkweb::url::UrlError::Scheme(s) => NetError::UnsupportedScheme(s),
        other => NetError::BadUrl(other.to_string()),
    }
}

/// Resolve `input` (what a script passed to `fetch`, `open`, `new Request`) against the page
/// `base`. Stricter than the URL standard on purpose:
/// * a user name or password in the address is refused (the standard does too, for fetch);
/// * a backslash anywhere is refused (browsers read it as a slash, other parsers don't);
/// * `http:foo` and `https:foo` (no `//`) are refused;
/// * a host that ends in a number must be a plain dotted quad ([`host_ok`]);
/// * a host with non-ASCII letters is refused (no IDNA here: write it in punycode);
/// * bad ports (mail, ssh...) are refused;
/// * only `http:`, `https:`, `data:` and `blob:` exist; the rest ([`NetError::UnsupportedScheme`]).
///
/// Tabs and line breaks anywhere in the text are dropped and surrounding blanks trimmed, as
/// the standard says; other control characters are refused. Never panics.
pub fn resolve_url(base: &Url, input: &str, limits: &Limits) -> Result<Target, NetError> {
    // Cheap bound before any copying: data: addresses may be long, others not.
    let looks_data = input
        .trim_start_matches(|c: char| c <= ' ')
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("data:"));
    let cap = if looks_data {
        limits
            .max_response_body
            .saturating_mul(2)
            .saturating_add(256)
    } else {
        limits.max_url.saturating_mul(2)
    };
    if input.len() > cap {
        return Err(NetError::UrlTooLong {
            max: limits.max_url,
        });
    }
    let text: String = input
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    if text.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}') {
        return Err(NetError::BadUrl(String::from("it has a control character")));
    }
    if text.contains('\\') {
        return Err(NetError::BadUrl(String::from("it has a backslash")));
    }
    if let Some((scheme, rest)) = scheme_prefix(&text) {
        return match scheme.as_str() {
            "http" | "https" => {
                if text.chars().count() > limits.max_url {
                    return Err(NetError::UrlTooLong {
                        max: limits.max_url,
                    });
                }
                let Some(after) = rest.strip_prefix("//") else {
                    return Err(NetError::BadUrl(String::from(
                        "it has no // after the scheme",
                    )));
                };
                if authority_of(after).contains('@') {
                    return Err(NetError::UrlHasCredentials);
                }
                finish_http(Url::parse(&text).map_err(url_error)?)
            }
            "data" => {
                if text.len() > cap {
                    return Err(NetError::UrlTooLong {
                        max: limits.max_url,
                    });
                }
                decode_data_url(rest, limits.max_response_body).map(Target::Data)
            }
            "blob" => {
                if text.chars().count() > limits.max_url {
                    return Err(NetError::UrlTooLong {
                        max: limits.max_url,
                    });
                }
                // blob:https://host/uuid names the origin that made it: no user name in it.
                if let Some((_, after)) = rest.split_once("://")
                    && authority_of(after).contains('@')
                {
                    return Err(NetError::UrlHasCredentials);
                }
                Ok(Target::Blob(text))
            }
            _ => Err(NetError::UnsupportedScheme(scheme)),
        };
    }
    if text.chars().count() > limits.max_url {
        return Err(NetError::UrlTooLong {
            max: limits.max_url,
        });
    }
    if let Some(after) = text.strip_prefix("//")
        && authority_of(after).contains('@')
    {
        return Err(NetError::UrlHasCredentials);
    }
    finish_http(base.join(&text).map_err(url_error)?)
}

fn finish_http(url: Url) -> Result<Target, NetError> {
    if !host_ok(&url.host) {
        return Err(NetError::BadUrl(format!(
            "'{}' isn't a host name or a plain IP address",
            url.host
        )));
    }
    if is_bad_port(url.port) {
        return Err(NetError::BlockedPort {
            url: url.to_text(),
            port: url.port,
        });
    }
    Ok(Target::Http(url))
}

// ---------------------------------------------------------------------------------------
// data: addresses

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// The standard's "forgiving base64": spaces are ignored, padding is optional, anything else
/// outside the alphabet is an error. `None` on a bad input or one that decodes past `max`.
pub fn base64_decode(input: &[u8], max: usize) -> Option<Vec<u8>> {
    let mut data: Vec<u8> = input
        .iter()
        .copied()
        .filter(|c| !matches!(c, b' ' | b'\t' | b'\n' | 0x0c | b'\r'))
        .collect();
    if data.len() % 4 == 0 {
        for _ in 0..2 {
            if data.last() == Some(&b'=') {
                data.pop();
            }
        }
    }
    if data.len() % 4 == 1 || data.contains(&b'=') {
        return None;
    }
    let mut out = Vec::with_capacity((data.len() / 4 + 1) * 3);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in data {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
            if out.len() > max {
                return None;
            }
        }
    }
    Some(out)
}

/// The text after `data:`: `[mediatype][;base64],data`.
pub fn decode_data_url(rest: &str, max_body: usize) -> Result<DataUrl, NetError> {
    // The address's own fragment is not part of the data.
    let rest = rest.split('#').next().unwrap_or("");
    let Some((meta, data)) = rest.split_once(',') else {
        return Err(NetError::BadUrl(String::from(
            "a data: address needs a comma",
        )));
    };
    let mut meta = meta.trim_matches(|c: char| c == ' ');
    let mut base64 = false;
    if let Some(i) = meta.rfind(';')
        && meta[i + 1..].trim().eq_ignore_ascii_case("base64")
    {
        base64 = true;
        meta = &meta[..i];
    }
    let bytes = percent_decode(data);
    let body = if base64 {
        base64_decode(&bytes, max_body)
            .ok_or_else(|| NetError::BadUrl(String::from("the base64 data is not valid")))?
    } else {
        bytes
    };
    if body.len() > max_body {
        return Err(NetError::ResponseTooLarge { max: max_body });
    }
    let fallback = || String::from("text/plain;charset=US-ASCII");
    let meta = meta.trim();
    let mime = if meta.is_empty() {
        fallback()
    } else if meta.starts_with(';') {
        // ";charset=x" alone means text/plain with those parameters.
        if meta.bytes().all(|c| c >= 0x20 && c != 0x7f) && meta.len() <= 200 {
            format!("text/plain{meta}")
        } else {
            fallback()
        }
    } else if meta.bytes().all(|c| c >= 0x20 && c != 0x7f)
        && super::headers::mime_essence(meta).is_some()
        && meta.len() <= 200
    {
        String::from(meta)
    } else {
        fallback()
    };
    Ok(DataUrl { mime, body })
}
