//! Server names: parsing what the user typed ([`ServerName::parse`]), matching it against a
//! certificate's subjectAltName (RFC 6125, with the CA/Browser Forum's wildcard rules), and
//! name constraints (RFC 5280 4.2.1.10).
//!
//! Matching rules:
//! * DNS names compare ASCII case-insensitively; a trailing dot on the reference name is
//!   ignored. Internationalised names must already be A-labels (`xn--...`).
//! * A wildcard is only `*` as the whole leftmost label (`*.example.com`), matching exactly
//!   one non-empty label: not `example.com` itself, not `a.b.example.com`, never part of a
//!   label (`f*.example.com` matches nothing), and never with fewer than two labels after it
//!   (`*.com` matches nothing).
//! * IP addresses match only iPAddress entries, byte for byte; never DNS entries.
//! * There is no fallback to the subject's common name.

use alloc::string::String;
use alloc::vec::Vec;

use crate::x509::{self, GeneralName};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerName {
    /// Lowercase, no trailing dot.
    Dns(String),
    /// 4 or 16 bytes.
    Ip(Vec<u8>),
}

impl ServerName {
    /// A DNS name or an IP address literal (IPv6 with or without brackets). None if it's
    /// neither.
    pub fn parse(s: &str) -> Option<ServerName> {
        let t = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')).unwrap_or(s);
        if let Some(ip) = parse_ipv4(t) {
            return Some(ServerName::Ip(ip.to_vec()));
        }
        if t.contains(':') {
            return parse_ipv6(t).map(|ip| ServerName::Ip(ip.to_vec()));
        }
        let name = s.strip_suffix('.').unwrap_or(s);
        valid_dns_name(name.as_bytes()).then(|| ServerName::Dns(name.to_ascii_lowercase()))
    }

    /// The name to send in SNI (RFC 6066: DNS names only, never IP literals).
    pub fn sni(&self) -> Option<&str> {
        match self {
            ServerName::Dns(d) => Some(d),
            ServerName::Ip(_) => None,
        }
    }
}

/// A reference DNS name we're willing to look up and check: 1..=253 bytes, labels of
/// 1..=63 letters, digits, '-' or '_' (some real hosts have underscores), not starting or
/// ending with '-', not all digits in the last label (that would be an IPv4-ish typo).
pub fn valid_dns_name(n: &[u8]) -> bool {
    if n.is_empty() || n.len() > 253 {
        return false;
    }
    let mut last: &[u8] = &[];
    for label in n.split(|&c| c == b'.') {
        if label.is_empty() || label.len() > 63 {
            return false;
        }
        if !label.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') {
            return false;
        }
        if label[0] == b'-' || label[label.len() - 1] == b'-' {
            return false;
        }
        last = label;
    }
    !last.iter().all(|c| c.is_ascii_digit())
}

pub fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = s.split('.');
    for b in out.iter_mut() {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        if p.len() > 1 && p.starts_with('0') {
            return None; // no octal-looking forms
        }
        *b = p.parse::<u16>().ok().filter(|&v| v <= 255)? as u8;
    }
    parts.next().is_none().then_some(out)
}

/// RFC 4291 2.2 text forms: eight groups, one `::`, an optional trailing dotted IPv4.
pub fn parse_ipv6(s: &str) -> Option<[u8; 16]> {
    if s.len() > 45 {
        return None;
    }
    let (head, tail) = match s.find("::") {
        Some(i) => (&s[..i], Some(&s[i + 2..])),
        None => (s, None),
    };
    if tail.is_some_and(|t| t.contains("::")) {
        return None;
    }
    fn groups(part: &str, allow_v4_end: bool) -> Option<Vec<u16>> {
        let mut out = Vec::new();
        if part.is_empty() {
            return Some(out);
        }
        let pieces: Vec<&str> = part.split(':').collect();
        for (i, p) in pieces.iter().enumerate() {
            if allow_v4_end && i == pieces.len() - 1 && p.contains('.') {
                let v4 = parse_ipv4(p)?;
                out.push(u16::from_be_bytes([v4[0], v4[1]]));
                out.push(u16::from_be_bytes([v4[2], v4[3]]));
            } else {
                if p.is_empty() || p.len() > 4 || !p.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return None;
                }
                out.push(u16::from_str_radix(p, 16).ok()?);
            }
        }
        Some(out)
    }
    let h = groups(head, tail.is_none())?;
    let t = match tail {
        Some(t) => groups(t, true)?,
        None => Vec::new(),
    };
    let n = h.len() + t.len();
    let all: Vec<u16> = if tail.is_some() {
        if n > 7 {
            return None;
        }
        let mut v = h;
        v.resize(8 - t.len(), 0);
        v.extend_from_slice(&t);
        v
    } else {
        if n != 8 {
            return None;
        }
        h
    };
    let mut out = [0u8; 16];
    for (i, g) in all.iter().enumerate() {
        out[2 * i..2 * i + 2].copy_from_slice(&g.to_be_bytes());
    }
    Some(out)
}

/// Does the presented SAN dNSName `pattern` match the reference name `host` (lowercase,
/// valid)? See the module docs for the rules.
pub fn dns_matches(pattern: &[u8], host: &[u8]) -> bool {
    let pattern = pattern.strip_suffix(b".").unwrap_or(pattern);
    if pattern.is_empty() || !pattern.is_ascii() {
        return false;
    }
    if let Some(rest) = pattern.strip_prefix(b"*.") {
        if rest.contains(&b'*') || !rest.contains(&b'.') || rest.split(|&c| c == b'.').any(|l| l.is_empty()) {
            return false;
        }
        let Some(dot) = host.iter().position(|&c| c == b'.') else {
            return false;
        };
        dot > 0 && host[dot + 1..].eq_ignore_ascii_case(rest)
    } else {
        !pattern.contains(&b'*') && pattern.eq_ignore_ascii_case(host)
    }
}

/// Does the certificate's SAN name the server?
pub fn san_matches(cert: &x509::Certificate, name: &ServerName) -> bool {
    cert.san_names().any(|g| match (g, name) {
        (GeneralName::Dns(p), ServerName::Dns(h)) => dns_matches(p, h.as_bytes()),
        (GeneralName::Ip(a), ServerName::Ip(b)) => a == &b[..],
        _ => false,
    })
}

/// Is DNS name `name` (presented in a certificate, possibly a wildcard) within the dNSName
/// constraint `base`? `base` "example.com" covers example.com and its subdomains; ".example.com"
/// only the subdomains; "" everything.
fn dns_within(name: &[u8], base: &[u8]) -> bool {
    if base.is_empty() {
        return true;
    }
    let name = name.strip_suffix(b".").unwrap_or(name);
    if let Some(sub) = base.strip_prefix(b".") {
        return name.len() > sub.len() + 1
            && name[name.len() - sub.len()..].eq_ignore_ascii_case(sub)
            && name[name.len() - sub.len() - 1] == b'.';
    }
    if name.eq_ignore_ascii_case(base) {
        return true;
    }
    name.len() > base.len() + 1
        && name[name.len() - base.len()..].eq_ignore_ascii_case(base)
        && name[name.len() - base.len() - 1] == b'.'
}

/// Could a name covered by `name` (a wildcard covers every single label in its place) fall
/// inside the excluded subtree `base`? For exclusions we must be pessimistic: `*.example.com`
/// is excluded by `bad.example.com` because it would match it.
fn dns_may_hit(name: &[u8], base: &[u8]) -> bool {
    let name = name.strip_suffix(b".").unwrap_or(name);
    if dns_within(name, base) {
        return true;
    }
    if let Some(rest) = name.strip_prefix(b"*.") {
        // The wildcard label could be anything: the base is hit if it is exactly one label
        // in front of `rest`, or `rest` itself is inside the base.
        let b = base.strip_prefix(b".").unwrap_or(base);
        if let Some(dot) = b.iter().position(|&c| c == b'.') {
            return b[dot + 1..].eq_ignore_ascii_case(rest);
        }
    }
    false
}

fn ip_within(addr: &[u8], constraint: &[u8]) -> bool {
    let n = addr.len();
    if constraint.len() != 2 * n {
        return false;
    }
    let (net, mask) = constraint.split_at(n);
    addr.iter().zip(net).zip(mask).all(|((a, b), m)| a & m == b & m)
}

/// directoryName constraint: the subject's RDNs start with the constraint's (bytewise).
fn dir_within(subject: &[u8], base: &[u8]) -> bool {
    match (x509::name_rdns(subject), x509::name_rdns(base)) {
        (Some(s), Some(b)) => b.len() <= s.len() && s[..b.len()] == b[..],
        _ => false,
    }
}

fn subtrees(s: Option<&[u8]>) -> Vec<GeneralName<'_>> {
    let mut out = Vec::new();
    if let Some(s) = s {
        let mut st = crate::der::Der::new(s);
        while let Some(mut sub) = st.sequence() {
            if let Some((t, v, _)) = sub.any()
                && let Some(g) = x509::general_names_one(t, v)
            {
                out.push(g);
            }
        }
    }
    out
}

/// Apply an issuer's NameConstraints (`nc`, the SEQUENCE contents) to a certificate below it.
/// Checked forms: dNSName, iPAddress, directoryName (the subject and SAN entries). Names of
/// other forms are left alone, except that a constraint on a form we can't check, when the
/// certificate has a name of that form, fails closed.
///
/// `budget` counts name-against-subtree comparisons across a whole chain; when it runs out
/// the answer is false (a hostile chain with thousands of names and subtrees can't make us
/// spin).
pub fn within_constraints(nc: &[u8], cert: &x509::Certificate, budget: &mut usize) -> bool {
    let mut d = crate::der::Der::new(nc);
    let permitted = d.optional(crate::der::ctx_cons(0)).flatten();
    let excluded = d.optional(crate::der::ctx_cons(1)).flatten();
    let perm = subtrees(permitted);
    let excl = subtrees(excluded);

    // The names to check: SAN entries, and the subject as a directoryName if not empty.
    let mut names: Vec<GeneralName> = cert.san_names().collect();
    let subject_empty = x509::name_rdns(cert.subject).is_some_and(|r| r.is_empty());
    if !subject_empty {
        names.push(GeneralName::Directory(cert.subject));
    }

    let cost = names.len().saturating_mul(perm.len() + excl.len());
    if cost > *budget {
        *budget = 0;
        return false;
    }
    *budget -= cost;
    for n in &names {
        // Excluded: no match allowed.
        for e in &excl {
            let hit = match (n, e) {
                (GeneralName::Dns(a), GeneralName::Dns(b)) => dns_may_hit(a, b),
                (GeneralName::Ip(a), GeneralName::Ip(b)) => ip_within(a, b),
                (GeneralName::Directory(a), GeneralName::Directory(b)) => dir_within(a, b),
                (GeneralName::Other(ta, _), GeneralName::Other(tb, _)) => ta == tb, // can't check: fail closed
                _ => false,
            };
            if hit {
                return false;
            }
        }
        // Permitted: if there are permitted subtrees of this name's form, it must be in one.
        let same_form: Vec<&GeneralName> = perm.iter().filter(|p| same_kind(n, p)).collect();
        if same_form.is_empty() {
            continue;
        }
        let ok = same_form.iter().any(|p| match (n, *p) {
            (GeneralName::Dns(a), GeneralName::Dns(b)) => dns_within(a, b),
            (GeneralName::Ip(a), GeneralName::Ip(b)) => ip_within(a, b),
            (GeneralName::Directory(a), GeneralName::Directory(b)) => dir_within(a, b),
            _ => false, // a form we can't check: fail closed
        });
        if !ok {
            return false;
        }
    }
    true
}

fn same_kind(a: &GeneralName, b: &GeneralName) -> bool {
    match (a, b) {
        (GeneralName::Dns(_), GeneralName::Dns(_))
        | (GeneralName::Directory(_), GeneralName::Directory(_)) => true,
        // One form (RFC 5280): an IPv6 name under IPv4-only permitted subtrees is outside them.
        (GeneralName::Ip(_), GeneralName::Ip(_)) => true,
        (GeneralName::Other(x, _), GeneralName::Other(y, _)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, h: &str) -> bool {
        dns_matches(p.as_bytes(), h.as_bytes())
    }

    #[test]
    fn hostname_rules() {
        assert!(m("example.com", "example.com"));
        assert!(m("EXAMPLE.com", "example.com"));
        assert!(m("example.com.", "example.com"));
        assert!(!m("example.com", "www.example.com"));
        assert!(!m("www.example.com", "example.com"));
        assert!(m("*.example.com", "www.example.com"));
        assert!(m("*.Example.COM", "a-b.example.com"));
        assert!(!m("*.example.com", "example.com"));
        assert!(!m("*.example.com", "a.b.example.com"));
        assert!(!m("*.example.com", ".example.com"));
        assert!(!m("*.com", "example.com"));
        assert!(!m("*", "example"));
        assert!(!m("f*.example.com", "foo.example.com"));
        assert!(!m("*oo.example.com", "foo.example.com"));
        assert!(!m("w*w.example.com", "www.example.com"));
        assert!(!m("*.*.example.com", "a.b.example.com"));
        assert!(!m("www.*.example.com", "www.a.example.com"));
        assert!(!m("*.example..com", "a.example..com"));
        assert!(!m("", "example.com"));
        assert!(!m("*.", "a."));
        assert!(m("xn--bcher-kva.example", "xn--bcher-kva.example"));
        assert!(m("*.xn--bcher-kva.example", "www.xn--bcher-kva.example"));
        assert!(!m("ex\u{e4}mple.com", "ex\u{e4}mple.com"));
        assert!(!m("example.com\0.evil.com", "example.com"));
    }

    #[test]
    fn server_names() {
        assert_eq!(
            ServerName::parse("WWW.Example.com."),
            Some(ServerName::Dns("www.example.com".into()))
        );
        assert_eq!(
            ServerName::parse("10.0.2.2"),
            Some(ServerName::Ip(alloc::vec![10, 0, 2, 2]))
        );
        assert_eq!(ServerName::parse("10.0.2.256"), None);
        assert_eq!(ServerName::parse("010.0.2.2"), None);
        assert_eq!(ServerName::parse("1.2.3"), None);
        assert_eq!(
            ServerName::parse("[::1]"),
            Some(ServerName::Ip({
                let mut v = alloc::vec![0u8; 16];
                v[15] = 1;
                v
            }))
        );
        assert!(ServerName::parse("fe80::1:2").is_some());
        assert!(ServerName::parse("::ffff:10.0.2.2").is_some());
        assert_eq!(ServerName::parse("1:2:3:4:5:6:7:8:9"), None);
        assert_eq!(ServerName::parse("1::2::3"), None);
        assert_eq!(ServerName::parse("exa mple.com"), None);
        assert_eq!(ServerName::parse("-bad.com"), None);
        assert_eq!(ServerName::parse(""), None);
        assert_eq!(ServerName::parse("a..b"), None);
        assert_eq!(ServerName::parse("*.example.com"), None);
        assert_eq!(ServerName::parse(&"a".repeat(64)), None);
        assert!(ServerName::parse("_dmarc.example.com").is_some());
        assert_eq!(ServerName::parse("ex\u{e4}mple.com"), None);
        assert_eq!(ServerName::parse("1.2.3.4.5"), None);
        assert_eq!(ServerName::parse("localhost").unwrap().sni(), Some("localhost"));
        assert_eq!(ServerName::parse("127.0.0.1").unwrap().sni(), None);
    }

    #[test]
    fn constraint_subtrees() {
        assert!(dns_within(b"example.com", b"example.com"));
        assert!(dns_within(b"www.example.com", b"example.com"));
        assert!(!dns_within(b"wwwexample.com", b"example.com"));
        assert!(!dns_within(b"example.com", b".example.com"));
        assert!(dns_within(b"a.example.com", b".example.com"));
        assert!(dns_within(b"anything", b""));
        assert!(dns_within(b"*.example.com", b"example.com"));
        assert!(dns_may_hit(b"*.example.com", b"bad.example.com"));
        assert!(!dns_may_hit(b"*.example.com", b"bad.other.com"));
        assert!(dns_may_hit(b"*.example.com", b"example.com"));
        assert!(ip_within(&[10, 1, 2, 3], &[10, 0, 0, 0, 255, 0, 0, 0]));
        assert!(!ip_within(&[11, 1, 2, 3], &[10, 0, 0, 0, 255, 0, 0, 0]));
        assert!(!ip_within(&[10, 1, 2, 3], &[10, 0, 0, 0]));
    }
}
