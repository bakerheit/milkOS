//! Hostile input: random and mutated addresses, headers, bodies of header lists and whole
//! exchanges driven by an adversarial server. Nothing may panic (debug builds trap integer
//! overflow here too), and every invariant of the policy must hold on whatever comes out.
//!
//! `NETPOLICY_FUZZ=N` runs N times as many iterations; `NETPOLICY_FUZZ_SEED=S` changes the
//! seed.

use super::cors::*;
use super::error::NetError;
use super::exchange::*;
use super::headers::*;
use super::limits::*;
use super::origin::*;
use super::referrer::*;
use milkweb::url::Url;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
    fn pick<'a, T: ?Sized>(&mut self, items: &[&'a T]) -> &'a T {
        items[self.below(items.len())]
    }
}

fn scale() -> usize {
    std::env::var("NETPOLICY_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1)
}

fn rng(salt: u64) -> Rng {
    let seed: u64 = std::env::var("NETPOLICY_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    Rng(0x9E37_79B9_7F4A_7C15
        ^ seed.wrapping_mul(0x1000_0000_01B3)
        ^ salt.wrapping_mul(0xD6E8_FEB8_6659_FD93)
        | 1)
}

const URL_PIECES: &[&str] = &[
    "http://",
    "https://",
    "//",
    "/",
    "?",
    "#",
    "@",
    ":",
    "\\",
    ".",
    "..",
    "%",
    "%2f",
    "%2e",
    "%00",
    "%zz",
    "[",
    "]",
    "::1",
    "a",
    "b.test",
    "example.com",
    "localhost",
    "127.0.0.1",
    "10.0.0.1",
    "0x7f",
    "2130706433",
    "1.2.3.4",
    "user",
    "pw",
    "80",
    "443",
    "8080",
    "22",
    "25",
    "65535",
    "65536",
    "0",
    "-1",
    "javascript:",
    "data:",
    "file:",
    "blob:",
    "ftp://",
    "ws://",
    "about:",
    " ",
    "\t",
    "\n",
    "\r",
    "\u{0}",
    "\u{7f}",
    "\u{e9}",
    "\u{202e}",
    "\u{1F600}",
    "=",
    "&",
    ";",
    ",",
    "'",
    "\"",
    "<",
    ">",
    "{",
    "}",
    "|",
    "^",
    "`",
    "text/plain",
    "base64",
    ";base64,",
    ",",
    "x",
    "xn--a",
    "..",
    "./",
    "../",
    "a/b/c",
    "%41",
    "+",
    "*",
    "!",
    "~",
    "_",
    "-",
];

fn random_text(r: &mut Rng, pieces: &[&str], max_pieces: usize) -> String {
    let n = r.below(max_pieces + 1);
    let mut s = String::new();
    for _ in 0..n {
        s.push_str(pieces[r.below(pieces.len())]);
    }
    s
}

fn random_chars(r: &mut Rng, max: usize) -> String {
    let n = r.below(max + 1);
    (0..n)
        .map(|_| match r.below(5) {
            0 => char::from_u32(r.below(0x80) as u32).unwrap_or('a'),
            1 => char::from_u32(0x20 + r.below(0x5f) as u32).unwrap_or('a'),
            2 => char::from_u32(r.below(0x3000) as u32).unwrap_or('a'),
            3 => char::from_u32(0x1F300 + r.below(0x200) as u32).unwrap_or('a'),
            _ => (b'a' + r.below(26) as u8) as char,
        })
        .collect()
}

fn mutate(r: &mut Rng, s: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    for _ in 0..1 + r.below(4) {
        match r.below(4) {
            0 if !chars.is_empty() => {
                let i = r.below(chars.len());
                chars.remove(i);
            }
            1 => {
                let i = r.below(chars.len() + 1);
                let c = random_text(r, URL_PIECES, 1);
                for (k, ch) in c.chars().enumerate() {
                    chars.insert((i + k).min(chars.len()), ch);
                }
            }
            2 if !chars.is_empty() => {
                let i = r.below(chars.len());
                chars[i] = char::from_u32(r.below(0x250) as u32).unwrap_or('x');
            }
            _ if chars.len() > 1 => {
                let (i, j) = (r.below(chars.len()), r.below(chars.len()));
                chars.swap(i, j);
            }
            _ => {}
        }
    }
    chars.into_iter().collect()
}

const SEED_URLS: &[&str] = &[
    "https://example.com/a/b?c=d",
    "http://user@example.com/",
    "http://[::1]:8080/x",
    "//cdn.example.com/lib.js",
    "../up/../../x?y#z",
    "data:text/plain;base64,aGVsbG8=",
    "data:,hello%20world",
    "blob:https://example.com/1234",
    "http://127.0.0.1:22/",
    "HTTP://EXAMPLE.COM:80/%7Efoo",
    "javascript:alert(1)",
    "http://a.test\\@b.test/",
];

fn base() -> Url {
    Url::parse("https://app.test/dir/page?q=1").unwrap()
}

fn check_http_url(u: &Url) {
    assert!(!u.host.is_empty(), "{u:?}");
    assert!(host_ok(&u.host), "{u:?}");
    assert!(!is_bad_port(u.port), "{u:?}");
    assert!(u.port > 0);
    assert!(u.path.starts_with('/'), "{u:?}");
    assert!(
        !u.path.bytes().any(|b| b <= b' ' || b >= 0x7f || b == b'\\'),
        "{u:?}"
    );
    let o = Origin::of(u).serialize();
    assert!(
        !o.contains('@')
            && !o.contains(' ')
            && !o.contains('\\')
            && !o.chars().any(|c| c.is_control()),
        "{o}"
    );
    // writing it out and reading it back gives the same address
    let text = u.to_text();
    match resolve_url(&base(), &text, &Limits::default()) {
        Ok(Target::Http(again)) => assert_eq!(&again, u, "{text}"),
        other => panic!("{text} did not survive a round trip: {other:?}"),
    }
}

fn check_resolved(input: &str) {
    let lim = Limits::default();
    match resolve_url(&base(), input, &lim) {
        Ok(Target::Http(u)) => check_http_url(&u),
        Ok(Target::Data(d)) => assert!(d.body.len() <= lim.max_response_body),
        Ok(Target::Blob(b)) => assert!(
            b.starts_with("blob:")
                || b.starts_with("BLOB:")
                || b.to_ascii_lowercase().starts_with("blob:")
        ),
        Err(_) => {}
    }
}

#[test]
fn fuzz_resolve_url() {
    let mut r = rng(1);
    for _ in 0..30_000 * scale() {
        let input = match r.below(4) {
            0 => random_text(&mut r, URL_PIECES, 12),
            1 => random_chars(&mut r, 40),
            2 => {
                let s = r.pick(SEED_URLS);
                mutate(&mut r, s)
            }
            _ => {
                let s = r.pick(SEED_URLS);
                let m = mutate(&mut r, s);
                mutate(&mut r, &m)
            }
        };
        check_resolved(&input);
    }
}

#[test]
fn resolve_url_exhaustive_short_strings() {
    // every string of up to 4 pieces from a small hostile alphabet
    let alphabet = [
        "h", "http:", "//", "/", "@", "\\", ":", ".", "1", "%", "[", "]", "?", "#", " ", "data:",
        ",",
    ];
    let mut count = 0usize;
    fn go(prefix: &mut String, depth: usize, alphabet: &[&str], count: &mut usize) {
        check_resolved(prefix);
        *count += 1;
        if depth == 0 {
            return;
        }
        for a in alphabet {
            let n = prefix.len();
            prefix.push_str(a);
            go(prefix, depth - 1, alphabet, count);
            prefix.truncate(n);
        }
    }
    go(&mut String::new(), 4, &alphabet, &mut count);
    assert!(count > 80_000);
}

#[test]
fn fuzz_resolve_url_giant_inputs() {
    let lim = Limits::default();
    for input in [
        "a".repeat(10_000_000),
        format!("http://{}/", "a.".repeat(500_000)),
        format!("http://h/{}", "%".repeat(1_000_000)),
        format!("data:;base64,{}", "A".repeat(3_000_000)),
        format!("data:,{}", "a".repeat(3_000_000)),
        "../".repeat(1_000_000),
        format!("http://h/{}", "../".repeat(100_000)),
        "\u{1F600}".repeat(1_000_000),
        format!("[{}", ":".repeat(1_000_000)),
    ] {
        let r = resolve_url(&base(), &input, &lim);
        if let Ok(Target::Http(u)) = &r {
            check_http_url(u);
        }
    }
}

#[test]
fn fuzz_parsers_never_panic() {
    let mut r = rng(2);
    for _ in 0..40_000 * scale() {
        let s = match r.below(3) {
            0 => random_chars(&mut r, 30),
            1 => random_text(&mut r, URL_PIECES, 8),
            _ => random_text(
                &mut r,
                &[
                    ":", "::", "1", "ff", "g", ".", "255", "256", "0", "%", "a", "[", "]", "-", " ",
                ],
                14,
            ),
        };
        let _ = parse_ipv4(&s);
        let _ = parse_ipv6(&s);
        let _ = address_space_of_host(&s);
        let _ = is_loopback_host(&s);
        let _ = host_ok(&s);
        let _ = mime_essence(&s);
        let _ = valid_header_name(&s);
        let _ = valid_header_value(&s);
        let _ = trim_header_value(&s);
        let _ = Method::parse(&s);
        let _ = Method::parse_script(&s);
        let _ = ReferrerPolicy::parse(&s);
        let _ = ReferrerPolicy::parse_header(&s);
        let _ = is_forbidden_request_header(&s, &s);
        let _ = is_forbidden_response_header(&s);
        let _ = is_cors_safelisted_request_header(&s, &s);
        let _ = base64_decode(s.as_bytes(), 1000);
        let _ = decode_data_url(&s, 1000);
        let _ = decode_data_url(&format!("text/plain;base64,{s}"), 1000);
        let _ = Url::parse(&s).map(|u| {
            let _ = Origin::of(&u).serialize();
            let _ = is_potentially_trustworthy(&u);
        });
    }
}

#[test]
fn ipv6_parse_agrees_with_std() {
    // std's parser is the reference for what an IPv6 literal is
    let mut r = rng(3);
    let pieces = [
        ":",
        "::",
        "0",
        "1",
        "ff",
        "FFFF",
        "10000",
        "g",
        ".",
        "1.2.3.4",
        "256.1.1.1",
        "a",
        "dead",
        "beef",
        "00",
    ];
    for _ in 0..60_000 * scale() {
        let s = random_text(&mut r, &pieces, 11);
        let ours = parse_ipv6(&s);
        let theirs = s.parse::<std::net::Ipv6Addr>().ok().map(|a| a.segments());
        // std accepts a few shapes we refuse on purpose? It shouldn't: they must agree.
        assert_eq!(ours, theirs, "{s:?}");
    }
}

#[test]
fn ipv4_parse_agrees_with_std_on_strict_forms() {
    let mut r = rng(4);
    for _ in 0..40_000 * scale() {
        let s = random_text(
            &mut r,
            &[
                "0", "1", "12", "255", "256", "07", ".", ".", ".", "a", "300",
            ],
            9,
        );
        let ours = parse_ipv4(&s);
        let theirs = s.parse::<std::net::Ipv4Addr>().ok().map(|a| a.octets());
        // std refuses leading zeros too; so both must agree exactly
        assert_eq!(ours, theirs, "{s:?}");
    }
}

#[test]
fn fuzz_headers_object() {
    let mut r = rng(5);
    let names = [
        "a",
        "X-A",
        "x-a",
        "Cookie",
        "Host",
        "Origin",
        "Content-Type",
        "Accept",
        "Authorization",
        "Set-Cookie",
        "Sec-Foo",
        "Proxy-X",
        "X-HTTP-Method-Override",
        "bad name",
        "",
        "é",
        "a:b",
        "Content-Language",
        "Accept-Language",
        "Range",
    ];
    let values = [
        "",
        "1",
        "text/plain",
        "application/json",
        "TRACE",
        "PUT",
        "a\nb",
        "a\u{0}",
        "\u{e9}",
        " x ",
        "bytes=0-1",
        "a(b",
        "*/*",
        "en",
    ];
    let guards = [
        Guard::None,
        Guard::Request,
        Guard::RequestNoCors,
        Guard::Response,
        Guard::Immutable,
    ];
    for _ in 0..3000 * scale() {
        let guard = guards[r.below(guards.len())];
        let mut h = Headers::new(if guard == Guard::Immutable {
            Guard::None
        } else {
            guard
        });
        for _ in 0..r.below(40) {
            let n = if r.chance(20) {
                random_chars(&mut r, 10)
            } else {
                names[r.below(names.len())].to_string()
            };
            let v = if r.chance(20) {
                random_chars(&mut r, 20)
            } else {
                values[r.below(values.len())].to_string()
            };
            let _ = match r.below(6) {
                0 => h.set(&n, &v),
                1 => h.delete(&n),
                2 => h.get(&n).map(|_| ()),
                3 => h.has(&n).map(|_| ()),
                _ => h.append(&n, &v),
            };
            if r.chance(3) {
                h.set_guard(guards[r.below(guards.len())]);
            }
            // invariants
            assert!(h.len() <= MAX_HEADERS);
            for (k, v) in h.raw() {
                assert!(
                    valid_header_name(k) && *k == k.to_ascii_lowercase(),
                    "{k:?}"
                );
                assert!(valid_header_value(v) && v == trim_header_value(v), "{v:?}");
                match h.guard() {
                    Guard::Request => assert!(!is_forbidden_request_header(k, v), "{k}: {v}"),
                    Guard::RequestNoCors => assert!(
                        is_no_cors_safelisted_name(k) && !is_forbidden_request_header(k, v),
                        "{k}: {v}"
                    ),
                    Guard::Response => assert!(!is_forbidden_response_header(k), "{k}"),
                    _ => {}
                }
            }
            let e = h.entries();
            assert!(e.windows(2).all(|w| w[0].0 <= w[1].0));
        }
    }
}

#[test]
fn fuzz_cors_pieces() {
    let mut r = rng(6);
    let origins = [
        "https://app.test",
        "http://app.test:8080",
        "null",
        "*",
        "https://app.test/",
        "https://x.test",
        "",
        "https://app.test, https://x.test",
        "HTTPS://APP.TEST",
    ];
    let hnames = [
        "access-control-allow-origin",
        "access-control-allow-credentials",
        "access-control-allow-methods",
        "access-control-allow-headers",
        "access-control-expose-headers",
        "access-control-max-age",
        "set-cookie",
        "content-type",
        "x-a",
        "ACCESS-CONTROL-ALLOW-ORIGIN",
        "cross-origin-resource-policy",
        "location",
    ];
    let hvals = [
        "true",
        "*",
        "PUT",
        "GET, PUT",
        "x-a, X-B",
        "60",
        "-1",
        "abc",
        "",
        ",,,",
        "authorization",
        "same-origin",
        "https://app.test",
        "null",
        "99999999999999999999999",
    ];
    let page = Origin::of(&Url::parse("https://app.test/").unwrap());
    let url = Url::parse("https://api.test/x").unwrap();
    for _ in 0..30_000 * scale() {
        let mut headers: Vec<(String, String)> = Vec::new();
        for _ in 0..r.below(10) {
            let n = hnames[r.below(hnames.len())].to_string();
            let v = if r.chance(30) {
                origins[r.below(origins.len())].to_string()
            } else {
                hvals[r.below(hvals.len())].to_string()
            };
            headers.push((n, v));
        }
        let creds = r.chance(50);
        let method =
            Method::parse(r.pick(&["GET", "POST", "PUT", "DELETE", "patch", "HEAD"])).unwrap();
        let unsafe_n: Vec<String> = (0..r.below(3))
            .map(|_| {
                r.pick(&["x-a", "authorization", "content-type", "x-b"])
                    .to_string()
            })
            .collect();
        let _ = cors_check(&page, creds, &headers);
        let status = [200u16, 204, 301, 404, 500, 0, 999][r.below(7)];
        let pf = preflight_check(&page, creds, &method, &unsafe_n, status, &headers);
        if let Ok(g) = &pf {
            // a grant that came back really does allow the request
            assert!(g.allows(&method, &unsafe_n, creds).is_ok());
            // and the response passed the plain CORS check
            assert!(cors_check(&page, creds, &headers).is_ok());
        }
        let basic = filter_basic(&headers);
        assert!(basic.iter().all(|(k, _)| !is_forbidden_response_header(k)));
        let cors = filter_cors(&headers, creds);
        assert!(cors.iter().all(|(k, _)| !is_forbidden_response_header(k)));
        assert!(cors.len() <= headers.len());
        let _ = corp_check(&page, &url, &headers);
        let _ = build_preflight(&url, &method, &unsafe_n, Some("null"), None);
    }
}

#[test]
fn fuzz_preflight_cache() {
    let mut r = rng(7);
    let mut c = PreflightCache::new(8, 100);
    let origins = [
        Origin::of(&Url::parse("https://a.test/").unwrap()),
        Origin::of(&Url::parse("https://b.test/").unwrap()),
        Origin::Opaque(0),
    ];
    let urls: Vec<Url> = (0..6)
        .map(|i| Url::parse(&format!("https://api.test/{i}")).unwrap())
        .collect();
    let mut now = 0u64;
    for _ in 0..20_000 * scale() {
        now = now.saturating_add(r.below(40_000) as u64);
        let o = &origins[r.below(3)];
        let u = &urls[r.below(6)];
        let creds = r.chance(30);
        let g = PreflightGrant {
            methods: vec![r.pick(&["PUT", "DELETE", "patch"]).to_string()],
            headers: vec![r.pick(&["x-a", "x-b"]).to_string()],
            methods_wildcard: r.chance(20),
            headers_wildcard: r.chance(20),
            max_age: [0u64, 1, 5, 99, 100, 101, u64::MAX][r.below(7)],
        };
        let method = Method::parse(r.pick(&["PUT", "GET", "DELETE", "patch"])).unwrap();
        let un = vec![r.pick(&["x-a", "x-b", "x-c"]).to_string()];
        if r.chance(50) {
            c.store(o, u, creds, &g, now);
        } else if c.covers(o, u, creds, &method, &un, now) {
            // whatever the cache says it covers must be allowed by *some* stored grant; just
            // exercise the path
        }
        assert!(c.len() <= 8);
        if r.chance(1) {
            c.clear();
        }
    }
}

#[test]
fn fuzz_limiter() {
    let mut r = rng(8);
    for _ in 0..300 * scale() {
        let lim = Limits {
            max_per_tab: 1 + r.below(8),
            max_per_host: 1 + r.below(4),
            max_pending: 1 + r.below(20),
            ..Limits::default()
        };
        let mut l = Limiter::new(&lim);
        let mut live: Vec<u64> = Vec::new();
        for id in 0..200u64 {
            let host = r.pick(&["a:80", "b:80", "c:443"]);
            match r.below(3) {
                0 | 1 => match l.submit(id, host) {
                    Ok(_) => live.push(id),
                    Err(NetError::TooManyRequests { .. }) => {}
                    Err(e) => panic!("{e:?}"),
                },
                _ if !live.is_empty() => {
                    let i = r.below(live.len());
                    let gone = live.swap_remove(i);
                    l.finish(gone);
                }
                _ => {}
            }
            assert!(l.running() <= lim.max_per_tab.max(1));
            assert!(l.running() + l.waiting() <= lim.max_pending.max(1));
            assert_eq!(l.busy(), l.running() + l.waiting() > 0);
        }
    }
}

// ---- whole exchanges against a hostile server ----

const LOCATIONS: &[&str] = &[
    "/next",
    "/a/../b",
    "https://app.test/x",
    "https://api.test/y",
    "https://other.test/z",
    "http://api.test/plain",
    "//api.test/q",
    "?q=1",
    "",
    " ",
    "file:///etc/passwd",
    "javascript:1",
    "data:text/html,x",
    "https://user:pw@app.test/",
    "https://app.test:25/",
    "http://10.0.0.1/",
    "http://localhost/",
    "https://127.1/",
    "https://[::1]/",
    "https://app.test\\@evil.test/",
    "blob:https://app.test/1",
    "https://a b/",
    "https://app.test/%00",
    "http://",
    "x:y",
    "\u{0}",
    "https://app.test/\u{e9}",
    "../..",
    "#frag",
];

fn random_response_headers(r: &mut Rng) -> Vec<(String, String)> {
    let names = [
        "location",
        "access-control-allow-origin",
        "access-control-allow-credentials",
        "access-control-allow-methods",
        "access-control-allow-headers",
        "access-control-expose-headers",
        "access-control-max-age",
        "set-cookie",
        "content-type",
        "content-length",
        "referrer-policy",
        "cross-origin-resource-policy",
        "x-a",
        "LOCATION",
    ];
    let vals = [
        "true",
        "*",
        "https://app.test",
        "null",
        "PUT, DELETE",
        "x-a, authorization",
        "60",
        "no-referrer",
        "unsafe-url",
        "same-origin",
        "text/plain",
        "10",
        "99999999999",
        "x",
        "",
    ];
    (0..r.below(9))
        .map(|_| {
            let n = names[r.below(names.len())];
            let v = if n.eq_ignore_ascii_case("location") {
                LOCATIONS[r.below(LOCATIONS.len())].to_string()
            } else {
                vals[r.below(vals.len())].to_string()
            };
            (n.to_ascii_lowercase(), v)
        })
        .collect()
}

fn check_hop(hop: &Hop, ex: &Exchange, cfg: &Config, page: &Page) {
    let u = &hop.url;
    assert!(matches!(
        u.scheme,
        milkweb::url::Scheme::Http | milkweb::url::Scheme::Https
    ));
    assert!(!is_bad_port(u.port) && host_ok(&u.host), "{u:?}");
    assert_eq!(u, ex.url());
    assert!(ex.redirect_count() <= cfg.limits.max_redirects);
    let mut seen = std::collections::BTreeMap::<&str, usize>::new();
    for (k, v) in &hop.headers {
        assert!(
            valid_header_name(k) && k == &k.to_ascii_lowercase(),
            "{k:?}"
        );
        assert!(valid_header_value(v), "{k}: {v:?}");
        *seen.entry(k.as_str()).or_default() += 1;
        // the browser-owned names appear only when this crate put them there, at most once
        if is_forbidden_request_header(k, v) {
            assert!(
                matches!(k.as_str(), "origin" | "referer") || k.starts_with("sec-fetch-"),
                "forbidden header {k} got through"
            );
            assert_eq!(seen[k.as_str()], 1, "{k} twice");
        }
    }
    for forbidden in [
        "cookie",
        "host",
        "content-length",
        "set-cookie",
        "connection",
        "transfer-encoding",
        "proxy-authorization",
        "access-control-request-method",
    ] {
        assert!(
            !hop.headers.iter().any(|(k, _)| k == forbidden),
            "{forbidden}"
        );
    }
    if let Some(o) = hop.headers.iter().find(|(k, _)| k == "origin") {
        assert!(o.1 == "null" || o.1 == page.origin.serialize(), "{o:?}");
    }
    if let Some(pf) = &hop.preflight {
        assert_eq!(pf.url, hop.url);
        assert!(
            pf.headers
                .iter()
                .any(|(k, _)| k == "access-control-request-method")
        );
        assert!(
            pf.headers
                .iter()
                .all(|(k, _)| k != "cookie" && k != "authorization" && k != "content-length")
        );
    }
    if hop.method.is_get_or_head() {
        assert!(!hop.send_body);
    }
}

fn check_meta(m: &ResponseMeta, raw: &[(String, String)]) {
    assert!(
        m.headers
            .iter()
            .all(|(k, _)| !is_forbidden_response_header(k))
    );
    match m.kind {
        ResponseType::Opaque | ResponseType::OpaqueRedirect => {
            assert!(
                m.headers.is_empty() && m.hide_body && m.status == 0 && m.status_text.is_empty()
            );
            if m.kind == ResponseType::Opaque {
                assert!(m.url.is_empty());
            }
        }
        ResponseType::Basic | ResponseType::Cors => {
            assert!(!m.hide_body && (200..=599).contains(&m.status));
            assert!(
                m.status_text.chars().all(|c| (' '..='~').contains(&c))
                    && m.status_text.len() <= 128
            );
            if m.kind == ResponseType::Cors {
                let exposed = list_values(raw, "access-control-expose-headers");
                for (k, _) in &m.headers {
                    assert!(
                        CORS_SAFELISTED_RESPONSE_HEADERS.contains(&k.as_str())
                            || exposed
                                .iter()
                                .any(|e| e == "*" || e.eq_ignore_ascii_case(k)),
                        "{k} leaked through a CORS response"
                    );
                }
            }
        }
    }
}

#[test]
fn fuzz_exchanges() {
    let mut r = rng(9);
    let pages = [
        "https://app.test/dir/page",
        "http://app.test/",
        "https://app.test:8443/x?y",
        "http://localhost:3000/",
    ];
    let urls = [
        "/x",
        "https://api.test/v1",
        "http://api.test/v1",
        "https://other.test/",
        "data:,hi",
        "blob:https://app.test/1",
        "https://app.test/%7Ea",
        "file:///x",
        "http://10.0.0.1/",
        "https://api.test:25/",
        "https://u@api.test/",
        "//api.test/p",
    ];
    let methods = [
        "GET", "POST", "PUT", "DELETE", "HEAD", "patch", "OPTIONS", "TRACE", "x y",
    ];
    let hnames = [
        "X-A",
        "Authorization",
        "Content-Type",
        "Accept",
        "Cookie",
        "Origin",
        "Referer",
        "Sec-Foo",
        "X-HTTP-Method",
        "Range",
        "Content-Language",
    ];
    let hvals = [
        "1",
        "text/plain",
        "application/json",
        "Bearer t",
        "TRACE",
        "multipart/form-data; boundary=x",
        "*/*",
    ];
    let mut ran = 0usize;
    for round in 0..4000 * scale() {
        let cfg = Config {
            limits: Limits {
                max_redirects: [0usize, 2, 5, 20][r.below(4)],
                ..Limits::default()
            },
            block_private_network: r.chance(70),
        };
        let mut page = Page::new(Url::parse(pages[r.below(pages.len())]).unwrap());
        if r.chance(5) {
            page.origin = Origin::Opaque(1);
        }
        if r.chance(20) {
            page.referrer_policy = [
                ReferrerPolicy::NoReferrer,
                ReferrerPolicy::UnsafeUrl,
                ReferrerPolicy::Origin,
            ][r.below(3)];
        }
        let mut init = RequestInit::new(urls[r.below(urls.len())]);
        init.method =
            Method::parse(methods[r.below(methods.len())]).unwrap_or_else(|_| Method::get());
        init.mode = [Mode::Cors, Mode::NoCors, Mode::SameOrigin, Mode::Navigate][r.below(4)];
        init.credentials = [
            Credentials::Omit,
            Credentials::SameOrigin,
            Credentials::Include,
        ][r.below(3)];
        init.redirect = [
            Redirect::Follow,
            Redirect::Follow,
            Redirect::Error,
            Redirect::Manual,
        ][r.below(4)];
        init.cache = [
            CacheMode::Default,
            CacheMode::NoStore,
            CacheMode::Reload,
            CacheMode::NoCache,
            CacheMode::ForceCache,
            CacheMode::OnlyIfCached,
        ][r.below(6)];
        init.force_preflight = r.chance(10);
        init.keepalive = r.chance(10);
        init.timeout_ms = [None, Some(0), Some(5), Some(u64::MAX)][r.below(4)];
        if r.chance(30) {
            init.referrer = [
                ReferrerInit::NoReferrer,
                ReferrerInit::Url("/p".into()),
                ReferrerInit::Url("https://evil.test/".into()),
            ][r.below(3)]
            .clone();
        }
        let mut hdrs = Headers::new(Guard::None);
        for _ in 0..r.below(6) {
            let _ = hdrs.append(hnames[r.below(hnames.len())], hvals[r.below(hvals.len())]);
        }
        init.headers = hdrs;
        if r.chance(50) {
            init.body = Some(BodyInfo {
                len: [0usize, 10, 70_000, usize::MAX][r.below(4)],
                content_type: [
                    None,
                    Some("application/json".to_string()),
                    Some("text/plain".to_string()),
                    Some("bad\nvalue".to_string()),
                ][r.below(4)]
                .clone(),
            });
        }
        let mut ex = match Exchange::start(&cfg, &page, init) {
            Ok(Begin::Network(ex)) => ex,
            Ok(Begin::Data(m, body)) => {
                check_meta(&m, &[]);
                assert!(body.len() <= cfg.limits.max_response_body);
                continue;
            }
            Ok(Begin::Blob { .. }) | Err(_) => continue,
        };
        ran += 1;
        let mut cache = PreflightCache::new(8, 600);
        let mut now = round as u64;
        'chain: for _ in 0..40 {
            now += 1;
            let hop = match ex.next_hop(&mut cache, now) {
                Ok(h) => h,
                Err(_) => break,
            };
            check_hop(&hop, &ex, &cfg, &page);
            if let Some(pf) = &hop.preflight {
                let _ = pf;
                let status = [200u16, 204, 403, 302][r.below(4)];
                let mut h = random_response_headers(&mut r);
                if r.chance(60) {
                    h.push((
                        "access-control-allow-origin".into(),
                        page.origin.serialize(),
                    ));
                    h.push(("access-control-allow-methods".into(), "*".into()));
                    h.push(("access-control-allow-headers".into(), "*".into()));
                }
                if ex
                    .on_preflight_response(&mut cache, now, status, &h)
                    .is_err()
                {
                    break;
                }
            }
            let status = [200u16, 204, 301, 302, 303, 307, 308, 404, 500, 100, 700, 0][r.below(12)];
            let mut h = random_response_headers(&mut r);
            if r.chance(50) {
                h.push((
                    "access-control-allow-origin".into(),
                    [page.origin.serialize(), "*".into(), "null".into()][r.below(3)].clone(),
                ));
            }
            match ex.on_response(status, "Some Reason \u{e9}", &h) {
                Ok(out) => match out.next {
                    Next::Done(m) => {
                        check_meta(&m, &h);
                        break 'chain;
                    }
                    Next::Redirect { to } => {
                        assert!(matches!(
                            to.scheme,
                            milkweb::url::Scheme::Http | milkweb::url::Scheme::Https
                        ));
                        assert!(
                            ex.redirect_count() >= 1
                                && ex.redirect_count() <= cfg.limits.max_redirects
                        );
                    }
                },
                Err(_) => break,
            }
        }
    }
    assert!(ran > 200, "the fuzz exercised too few exchanges: {ran}");
}

#[test]
fn exchange_and_cache_can_move_between_threads() {
    fn is_send<T: Send>() {}
    is_send::<Exchange>();
    is_send::<PreflightCache>();
    is_send::<Limiter>();
    is_send::<NetError>();
    is_send::<Hop>();
    is_send::<Outcome>();
    is_send::<Headers>();
}
