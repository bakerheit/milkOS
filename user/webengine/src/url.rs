//! URL resolution: RFC 3986 §5.2 "Relative Resolution" with the WHATWG URL Standard's
//! behaviour for http(s) ("special" schemes): backslashes as slashes, scheme-relative
//! `//host`, dot-segment removal, default paths. Non-special schemes (`data:`, `mailto:`,
//! `javascript:`, `about:`) come back as written.

use alloc::string::String;
use alloc::vec::Vec;

/// Split `scheme:` off, if the text starts with a valid scheme (RFC 3986 §3.1).
fn scheme(s: &str) -> Option<(&str, &str)> {
    let i = s.find(':')?;
    let sc = &s[..i];
    let mut ch = sc.chars();
    let first = ch.next()?;
    if !first.is_ascii_alphabetic()
        || !ch.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    Some((sc, &s[i + 1..]))
}

fn is_special(sc: &str) -> bool {
    matches!(
        sc.to_ascii_lowercase().as_str(),
        "http" | "https" | "ftp" | "ws" | "wss" | "file"
    )
}

struct Parts<'a> {
    scheme: String,
    authority: Option<&'a str>,
    path: &'a str,
    query: Option<&'a str>,
    fragment: Option<&'a str>,
}

/// RFC 3986 appendix B, after the scheme.
fn split(rest: &str) -> (Option<&str>, &str, Option<&str>, Option<&str>) {
    let (rest, fragment) = match rest.find('#') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    let (rest, query) = match rest.find('?') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    if let Some(r) = rest.strip_prefix("//") {
        let end = r.find('/').unwrap_or(r.len());
        (Some(&r[..end]), &r[end..], query, fragment)
    } else {
        (None, rest, query, fragment)
    }
}

/// RFC 3986 §5.2.4 "Remove Dot Segments".
fn remove_dots(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let absolute = path.starts_with('/');
    let segs: Vec<&str> = path.split('/').collect();
    let n = segs.len();
    for (i, seg) in segs.iter().enumerate() {
        if i == 0 && absolute {
            continue;
        }
        let last = i + 1 == n;
        match *seg {
            "." | "%2e" | "%2E" => {
                if last {
                    out.push("");
                }
            }
            ".." | ".%2e" | "%2e." | "%2e%2e" | ".%2E" | "%2E." | "%2E%2E" => {
                out.pop();
                if last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let mut s = String::new();
    if absolute {
        s.push('/');
    }
    s.push_str(&out.join("/"));
    s
}

fn parts(s: &str) -> Option<Parts<'_>> {
    let (sc, rest) = scheme(s)?;
    let (authority, path, query, fragment) = split(rest);
    Some(Parts {
        scheme: sc.to_ascii_lowercase(),
        authority,
        path,
        query,
        fragment,
    })
}

fn compose(p: &Parts, path: &str) -> String {
    let mut s = String::new();
    s.push_str(&p.scheme);
    s.push(':');
    if let Some(a) = p.authority {
        s.push_str("//");
        s.push_str(&a.to_ascii_lowercase());
    }
    if path.is_empty() && p.authority.is_some() && is_special(&p.scheme) {
        s.push('/');
    } else {
        s.push_str(path);
    }
    if let Some(q) = p.query {
        s.push('?');
        s.push_str(q);
    }
    if let Some(f) = p.fragment {
        s.push('#');
        s.push_str(f);
    }
    s
}

/// Resolve `href` against `base` (both as written). Leading/trailing C0 controls and spaces
/// are trimmed and tabs/newlines removed (URL Standard §4.4).
pub fn resolve(base: &str, href: &str) -> String {
    let href: String = href
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|&c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let base_special = scheme(base).is_some_and(|(sc, _)| is_special(sc));
    let href = if base_special || scheme(&href).is_some_and(|(sc, _)| is_special(sc)) {
        // Backslashes count as slashes before the query in special URLs.
        let q = href.find(['?', '#']).unwrap_or(href.len());
        let mut h = href[..q].replace('\\', "/");
        h.push_str(&href[q..]);
        h
    } else {
        href
    };
    if let Some((sc, rest)) = scheme(&href) {
        if !is_special(sc) {
            return href;
        }
        // "http:foo" relative to an http base is relative (URL Standard special-relative).
        let same = scheme(base).is_some_and(|(b, _)| b.eq_ignore_ascii_case(sc));
        if !rest.starts_with('/') && same {
            return resolve(base, rest);
        }
        let Some(p) = parts(&href) else { return href };
        let path = remove_dots(p.path);
        return compose(&p, &path);
    }
    let Some(b) = parts(base) else { return href };
    if href.is_empty() {
        let p = Parts {
            fragment: None,
            ..b
        };
        return compose(&p, p.path);
    }
    if let Some(frag) = href.strip_prefix('#') {
        let p = Parts {
            fragment: Some(frag),
            ..b
        };
        return compose(&p, p.path);
    }
    if href.starts_with("//") {
        let full = alloc::format!("{}:{}", b.scheme, href);
        return resolve(&full, "");
    }
    let (_, rpath, rquery, rfrag) = split(&href);
    let path = if rpath.is_empty() {
        String::from(b.path)
    } else if rpath.starts_with('/') {
        remove_dots(rpath)
    } else {
        // Merge (RFC 3986 §5.2.3).
        let dir = match b.path.rfind('/') {
            Some(i) => &b.path[..=i],
            None => {
                if b.authority.is_some() {
                    "/"
                } else {
                    ""
                }
            }
        };
        remove_dots(&alloc::format!("{dir}{rpath}"))
    };
    let query = if rpath.is_empty() && rquery.is_none() {
        b.query
    } else {
        rquery
    };
    let p = Parts {
        scheme: b.scheme.clone(),
        authority: b.authority,
        path: "",
        query,
        fragment: rfrag,
    };
    compose(&p, &path)
}

/// application/x-www-form-urlencoded byte serializer (URL Standard §5.2).
pub fn form_urlencode(s: &str, out: &mut String) {
    for &b in s.as_bytes() {
        match b {
            b' ' => out.push('+'),
            b'*' | b'-' | b'.' | b'_' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => {
                out.push(b as char)
            }
            _ => {
                const H: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(H[(b >> 4) as usize] as char);
                out.push(H[(b & 15) as usize] as char);
            }
        }
    }
}

/// Percent-decode (for URL fragments).
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = |c: u8| (c as char).to_digit(16);
            if let (Some(x), Some(y)) = (
                b.get(i + 1).and_then(|&c| h(c)),
                b.get(i + 2).and_then(|&c| h(c)),
            ) {
                out.push((x * 16 + y) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::resolve;

    #[test]
    fn rfc3986_examples() {
        // RFC 3986 §5.4.1 normal examples.
        let base = "http://a/b/c/d;p?q";
        for (r, want) in [
            ("g:h", "g:h"),
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g/"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g#s", "http://a/b/c/g#s"),
            ("g?y#s", "http://a/b/c/g?y#s"),
            (";x", "http://a/b/c/;x"),
            ("g;x", "http://a/b/c/g;x"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../", "http://a/"),
            ("../../g", "http://a/g"),
            ("../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            ("g..", "http://a/b/c/g.."),
            ("./../g", "http://a/b/g"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
        ] {
            assert_eq!(resolve(base, r), want, "{r}");
        }
        assert_eq!(resolve("https://x.org", "a b"), "https://x.org/a b");
        assert_eq!(
            resolve("https://x.org/p/", "\\\\y.org\\z"),
            "https://y.org/z"
        );
        assert_eq!(
            resolve("https://x.org/p", "javascript:void(0)"),
            "javascript:void(0)"
        );
        assert_eq!(resolve("http://X.org/p", "http:q"), "http://x.org/q");
    }
}
