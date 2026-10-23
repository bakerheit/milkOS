//! The CORS pieces: check, preflight, cache, filtering, CORP; plus referrers, the limiter
//! and the error texts.

use super::cors::*;
use super::error::{CorsError, ErrorKind, NetError};
use super::headers::{Method, cors_unsafe_request_header_names};
use super::limits::*;
use super::origin::Origin;
use super::referrer::*;
use milkweb::url::Url;

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn o(s: &str) -> Origin {
    Origin::of(&u(s))
}

fn h(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn m(s: &str) -> Method {
    Method::parse(s).unwrap()
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn cors_check_table() {
    use CorsError::*;
    let page = o("https://app.test/");
    let mism = |a: &str| {
        Err(OriginMismatch {
            allowed: a.to_string(),
        })
    };
    let rows: Vec<(&str, bool, Vec<(&str, &str)>, Result<(), CorsError>)> = vec![
        (
            "exact",
            false,
            vec![("access-control-allow-origin", "https://app.test")],
            Ok(()),
        ),
        (
            "exact, header name in other case",
            false,
            vec![("Access-Control-Allow-Origin", "https://app.test")],
            Ok(()),
        ),
        (
            "wildcard",
            false,
            vec![("access-control-allow-origin", "*")],
            Ok(()),
        ),
        ("none", false, vec![], Err(MissingAllowOrigin)),
        (
            "unrelated headers",
            false,
            vec![("vary", "origin")],
            Err(MissingAllowOrigin),
        ),
        (
            "other origin",
            false,
            vec![("access-control-allow-origin", "https://other.test")],
            mism("https://other.test"),
        ),
        (
            "trailing slash",
            false,
            vec![("access-control-allow-origin", "https://app.test/")],
            mism("https://app.test/"),
        ),
        (
            "upper case",
            false,
            vec![("access-control-allow-origin", "HTTPS://APP.TEST")],
            mism("HTTPS://APP.TEST"),
        ),
        (
            "http not https",
            false,
            vec![("access-control-allow-origin", "http://app.test")],
            mism("http://app.test"),
        ),
        (
            "explicit port",
            false,
            vec![("access-control-allow-origin", "https://app.test:443")],
            mism("https://app.test:443"),
        ),
        (
            "null",
            false,
            vec![("access-control-allow-origin", "null")],
            mism("null"),
        ),
        (
            "empty",
            false,
            vec![("access-control-allow-origin", "")],
            mism(""),
        ),
        (
            "list",
            false,
            vec![(
                "access-control-allow-origin",
                "https://app.test, https://b.test",
            )],
            Err(MultipleAllowOrigin),
        ),
        (
            "twice",
            false,
            vec![
                ("access-control-allow-origin", "https://app.test"),
                ("access-control-allow-origin", "https://app.test"),
            ],
            Err(MultipleAllowOrigin),
        ),
        (
            "twice wildcard",
            false,
            vec![
                ("access-control-allow-origin", "*"),
                ("access-control-allow-origin", "*"),
            ],
            Err(MultipleAllowOrigin),
        ),
        (
            "wildcard in list",
            false,
            vec![("access-control-allow-origin", "*, https://app.test")],
            Err(MultipleAllowOrigin),
        ),
        (
            "subdomain",
            false,
            vec![("access-control-allow-origin", "https://*.test")],
            mism("https://*.test"),
        ),
        (
            "cred: exact + true",
            true,
            vec![
                ("access-control-allow-origin", "https://app.test"),
                ("access-control-allow-credentials", "true"),
            ],
            Ok(()),
        ),
        (
            "cred: exact, no allow-credentials",
            true,
            vec![("access-control-allow-origin", "https://app.test")],
            Err(CredentialsNotAllowed),
        ),
        (
            "cred: exact + True",
            true,
            vec![
                ("access-control-allow-origin", "https://app.test"),
                ("access-control-allow-credentials", "True"),
            ],
            Err(CredentialsNotAllowed),
        ),
        (
            "cred: exact + TRUE",
            true,
            vec![
                ("access-control-allow-origin", "https://app.test"),
                ("access-control-allow-credentials", "TRUE"),
            ],
            Err(CredentialsNotAllowed),
        ),
        (
            "cred: exact + 1",
            true,
            vec![
                ("access-control-allow-origin", "https://app.test"),
                ("access-control-allow-credentials", "1"),
            ],
            Err(CredentialsNotAllowed),
        ),
        (
            "cred: exact + false",
            true,
            vec![
                ("access-control-allow-origin", "https://app.test"),
                ("access-control-allow-credentials", "false"),
            ],
            Err(CredentialsNotAllowed),
        ),
        (
            "cred: exact + true twice",
            true,
            vec![
                ("access-control-allow-origin", "https://app.test"),
                ("access-control-allow-credentials", "true"),
                ("access-control-allow-credentials", "true"),
            ],
            Err(CredentialsNotAllowed),
        ),
        (
            "cred: wildcard",
            true,
            vec![
                ("access-control-allow-origin", "*"),
                ("access-control-allow-credentials", "true"),
            ],
            Err(WildcardWithCredentials),
        ),
        (
            "cred: wrong origin + true",
            true,
            vec![
                ("access-control-allow-origin", "https://x.test"),
                ("access-control-allow-credentials", "true"),
            ],
            mism("https://x.test"),
        ),
        (
            "no cred: true is ignored",
            false,
            vec![
                ("access-control-allow-origin", "https://app.test"),
                ("access-control-allow-credentials", "nonsense"),
            ],
            Ok(()),
        ),
    ];
    for (label, cred, hdrs, want) in rows {
        assert_eq!(cors_check(&page, cred, &h(&hdrs)), want, "{label}");
    }
}

#[test]
fn cors_check_other_origins() {
    let rows = [
        ("http://localhost:8080/", "http://localhost:8080"),
        ("http://[::1]:3000/", "http://[::1]:3000"),
        ("https://a.b.c.test/x", "https://a.b.c.test"),
        ("http://10.0.2.2:8000/", "http://10.0.2.2:8000"),
    ];
    for (page, ok) in rows {
        let hdr = h(&[("access-control-allow-origin", ok)]);
        assert_eq!(cors_check(&o(page), false, &hdr), Ok(()), "{page}");
        let other = h(&[("access-control-allow-origin", "http://example.test")]);
        assert!(cors_check(&o(page), false, &other).is_err());
    }
    // an opaque page origin is "null"
    let hdr = h(&[("access-control-allow-origin", "null")]);
    assert_eq!(cors_check(&Origin::Opaque(1), false, &hdr), Ok(()));
    assert!(
        cors_check(
            &Origin::Opaque(1),
            false,
            &h(&[("access-control-allow-origin", "https://a.test")])
        )
        .is_err()
    );
}

fn grant_headers(origin: &str, extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut v = vec![(
        "access-control-allow-origin".to_string(),
        origin.to_string(),
    )];
    v.extend(extra.iter().map(|(a, b)| (a.to_string(), b.to_string())));
    v
}

#[test]
fn preflight_check_table() {
    use CorsError::*;
    let page = o("https://app.test/");
    let ok = "https://app.test";
    // (label, credentials, method, unsafe request headers, status, response headers, expectation)
    let rows: Vec<(
        &str,
        bool,
        &str,
        Vec<&str>,
        u16,
        Vec<(&str, &str)>,
        Result<(), CorsError>,
    )> = vec![
        (
            "PUT allowed",
            false,
            "PUT",
            vec![],
            204,
            vec![("access-control-allow-methods", "GET, PUT")],
            Ok(()),
        ),
        (
            "PUT not listed",
            false,
            "PUT",
            vec![],
            204,
            vec![("access-control-allow-methods", "GET, POST")],
            Err(MethodNotAllowed {
                method: "PUT".into(),
                wildcard_ignored: false,
            }),
        ),
        (
            "PUT, no methods header",
            false,
            "PUT",
            vec![],
            200,
            vec![],
            Err(MethodNotAllowed {
                method: "PUT".into(),
                wildcard_ignored: false,
            }),
        ),
        (
            "method case matters",
            false,
            "PUT",
            vec![],
            200,
            vec![("access-control-allow-methods", "put")],
            Err(MethodNotAllowed {
                method: "PUT".into(),
                wildcard_ignored: false,
            }),
        ),
        (
            "patch is case sensitive",
            false,
            "patch",
            vec![],
            200,
            vec![("access-control-allow-methods", "PATCH")],
            Err(MethodNotAllowed {
                method: "patch".into(),
                wildcard_ignored: false,
            }),
        ),
        (
            "patch listed as written",
            false,
            "patch",
            vec![],
            200,
            vec![("access-control-allow-methods", "patch")],
            Ok(()),
        ),
        (
            "POST needs no listing",
            false,
            "POST",
            vec!["x-a"],
            200,
            vec![("access-control-allow-headers", "x-a")],
            Ok(()),
        ),
        (
            "GET needs no listing",
            false,
            "GET",
            vec!["x-a"],
            200,
            vec![("access-control-allow-headers", "X-A")],
            Ok(()),
        ),
        (
            "header missing",
            false,
            "POST",
            vec!["x-a"],
            200,
            vec![],
            Err(HeaderNotAllowed {
                name: "x-a".into(),
                wildcard_ignored: false,
            }),
        ),
        (
            "one of two headers missing",
            false,
            "POST",
            vec!["x-a", "x-b"],
            200,
            vec![("access-control-allow-headers", "x-a")],
            Err(HeaderNotAllowed {
                name: "x-b".into(),
                wildcard_ignored: false,
            }),
        ),
        (
            "headers in two header lines",
            false,
            "POST",
            vec!["x-a", "x-b"],
            200,
            vec![
                ("access-control-allow-headers", "x-a"),
                ("access-control-allow-headers", "x-b"),
            ],
            Ok(()),
        ),
        (
            "method wildcard",
            false,
            "DELETE",
            vec![],
            200,
            vec![("access-control-allow-methods", "*")],
            Ok(()),
        ),
        (
            "method wildcard + credentials",
            true,
            "DELETE",
            vec![],
            200,
            vec![
                ("access-control-allow-methods", "*"),
                ("access-control-allow-credentials", "true"),
            ],
            Err(MethodNotAllowed {
                method: "DELETE".into(),
                wildcard_ignored: true,
            }),
        ),
        (
            "method listed + credentials",
            true,
            "DELETE",
            vec![],
            200,
            vec![
                ("access-control-allow-methods", "DELETE"),
                ("access-control-allow-credentials", "true"),
            ],
            Ok(()),
        ),
        (
            "header wildcard",
            false,
            "POST",
            vec!["x-a", "content-type"],
            200,
            vec![("access-control-allow-headers", "*")],
            Ok(()),
        ),
        (
            "header wildcard + credentials",
            true,
            "POST",
            vec!["x-a"],
            200,
            vec![
                ("access-control-allow-headers", "*"),
                ("access-control-allow-credentials", "true"),
            ],
            Err(HeaderNotAllowed {
                name: "x-a".into(),
                wildcard_ignored: true,
            }),
        ),
        (
            "authorization needs naming",
            false,
            "POST",
            vec!["authorization"],
            200,
            vec![("access-control-allow-headers", "*")],
            Err(HeaderNotAllowed {
                name: "authorization".into(),
                wildcard_ignored: true,
            }),
        ),
        (
            "authorization named",
            false,
            "POST",
            vec!["authorization"],
            200,
            vec![("access-control-allow-headers", "Authorization")],
            Ok(()),
        ),
        (
            "authorization named, plus wildcard",
            false,
            "POST",
            vec!["authorization", "x-a"],
            200,
            vec![("access-control-allow-headers", "authorization, *")],
            Ok(()),
        ),
        (
            "allow-headers case-insensitive",
            false,
            "POST",
            vec!["x-custom-header"],
            200,
            vec![("access-control-allow-headers", "X-CUSTOM-Header")],
            Ok(()),
        ),
        ("status 200", false, "GET", vec![], 200, vec![], Ok(())),
        ("status 204", false, "GET", vec![], 204, vec![], Ok(())),
        ("status 299", false, "GET", vec![], 299, vec![], Ok(())),
        (
            "status 404",
            false,
            "GET",
            vec![],
            404,
            vec![],
            Err(PreflightStatus(404)),
        ),
        (
            "status 500",
            false,
            "GET",
            vec![],
            500,
            vec![],
            Err(PreflightStatus(500)),
        ),
        (
            "status 403",
            false,
            "GET",
            vec![],
            403,
            vec![],
            Err(PreflightStatus(403)),
        ),
        (
            "status 100",
            false,
            "GET",
            vec![],
            100,
            vec![],
            Err(PreflightStatus(100)),
        ),
        (
            "redirect 301",
            false,
            "GET",
            vec![],
            301,
            vec![("location", "https://x.test/")],
            Err(PreflightRedirect),
        ),
        (
            "redirect 307",
            false,
            "GET",
            vec![],
            307,
            vec![],
            Err(PreflightRedirect),
        ),
        (
            "redirect 399",
            false,
            "GET",
            vec![],
            399,
            vec![],
            Err(PreflightRedirect),
        ),
        (
            "credentials need allow-credentials",
            true,
            "GET",
            vec![],
            200,
            vec![],
            Err(CredentialsNotAllowed),
        ),
    ];
    for (label, cred, method, uh, status, extra, want) in rows {
        // The expected error for status-only rows includes the origin check running second.
        let origin_hdr = if cred { ok.to_string() } else { ok.to_string() };
        let hdrs = if label.starts_with("credentials need") {
            grant_headers(&origin_hdr, &[])
        } else {
            grant_headers(&origin_hdr, &extra)
        };
        let got = preflight_check(&page, cred, &m(method), &names(&uh), status, &hdrs).map(|_| ());
        assert_eq!(got, want, "{label}");
    }
    // the origin check applies to the preflight too
    let bad = preflight_check(&page, false, &m("GET"), &[], 200, &h(&[]));
    assert_eq!(bad, Err(MissingAllowOrigin));
    let bad = preflight_check(
        &page,
        false,
        &m("GET"),
        &[],
        200,
        &grant_headers("https://other.test", &[]),
    );
    assert!(matches!(bad, Err(OriginMismatch { .. })));
    let bad = preflight_check(
        &page,
        true,
        &m("GET"),
        &[],
        200,
        &grant_headers("*", &[("access-control-allow-credentials", "true")]),
    );
    assert_eq!(bad, Err(WildcardWithCredentials));
    // the status is checked before anything else
    assert_eq!(
        preflight_check(&page, false, &m("GET"), &[], 500, &h(&[])),
        Err(PreflightStatus(500))
    );
}

#[test]
fn preflight_max_age_parsing() {
    let page = o("https://app.test/");
    let age = |extra: &[(&str, &str)]| {
        preflight_check(
            &page,
            false,
            &m("GET"),
            &[],
            200,
            &grant_headers("https://app.test", extra),
        )
        .unwrap()
        .max_age
    };
    assert_eq!(age(&[]), 5);
    assert_eq!(age(&[("access-control-max-age", "600")]), 600);
    assert_eq!(age(&[("access-control-max-age", " 30 ")]), 30);
    assert_eq!(age(&[("access-control-max-age", "0")]), 0);
    assert_eq!(age(&[("access-control-max-age", "-1")]), 0);
    assert_eq!(age(&[("access-control-max-age", "abc")]), 0);
    assert_eq!(age(&[("access-control-max-age", "")]), 0);
    assert_eq!(age(&[("access-control-max-age", "1.5")]), 0);
    assert_eq!(age(&[("access-control-max-age", "10, 20")]), 0);
    assert_eq!(
        age(&[("access-control-max-age", "99999999999999999999999")]),
        u64::MAX
    );
    assert_eq!(age(&[("access-control-max-age", "86400")]), 86400);
}

#[test]
fn grant_allows_directly() {
    let g = PreflightGrant {
        methods: names(&["PUT", "DELETE"]),
        headers: names(&["x-a"]),
        methods_wildcard: false,
        headers_wildcard: false,
        max_age: 5,
    };
    assert!(g.allows(&m("PUT"), &names(&["x-a"]), false).is_ok());
    assert!(g.allows(&m("GET"), &[], false).is_ok());
    assert!(g.allows(&m("PATCH"), &[], false).is_err());
    assert!(g.allows(&m("PUT"), &names(&["x-b"]), false).is_err());
    assert!(g.allows(&m("PUT"), &names(&["x-a", "x-b"]), true).is_err());
    let w = PreflightGrant {
        methods_wildcard: true,
        headers_wildcard: true,
        ..g.clone()
    };
    assert!(w.allows(&m("PATCH"), &names(&["x-z"]), false).is_ok());
    assert!(w.allows(&m("PATCH"), &[], true).is_err());
    assert!(w.allows(&m("PUT"), &names(&["x-z"]), true).is_err());
    assert!(
        w.allows(&m("PUT"), &names(&["authorization"]), false)
            .is_err()
    );
}

#[test]
fn build_preflight_shape() {
    let url = u("https://api.test/v1/item?id=1");
    let p = build_preflight(
        &url,
        &m("PUT"),
        &names(&["content-type", "x-a"]),
        Some("https://app.test"),
        Some("https://app.test/"),
    );
    assert_eq!(p.url, url);
    let get = |n: &str| {
        p.headers
            .iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(get("access-control-request-method"), Some("PUT"));
    assert_eq!(
        get("access-control-request-headers"),
        Some("content-type,x-a")
    );
    assert_eq!(get("origin"), Some("https://app.test"));
    assert_eq!(get("referer"), Some("https://app.test/"));
    assert_eq!(get("accept"), Some("*/*"));
    // never any cookies or credentials
    for forbidden in ["cookie", "authorization", "content-length", "content-type"] {
        assert!(get(forbidden).is_none(), "{forbidden}");
    }
    let p = build_preflight(&url, &m("POST"), &[], None, None);
    assert!(
        p.headers
            .iter()
            .all(|(k, _)| k != "access-control-request-headers" && k != "origin" && k != "referer")
    );
    assert!(
        p.headers
            .iter()
            .any(|(k, v)| k == "access-control-request-method" && v == "POST")
    );
}

#[test]
fn preflight_cache_behaviour() {
    let page = o("https://app.test/");
    let url = u("https://api.test/x");
    let grant = |age: u64| PreflightGrant {
        methods: names(&["PUT"]),
        headers: names(&["x-a"]),
        methods_wildcard: false,
        headers_wildcard: false,
        max_age: age,
    };
    let mut c = PreflightCache::new(4, 600);
    let put = m("PUT");
    let ua = names(&["x-a"]);
    assert!(!c.covers(&page, &url, false, &put, &ua, 0));
    c.store(&page, &url, false, &grant(10), 1_000);
    assert_eq!(c.len(), 1);
    assert!(c.covers(&page, &url, false, &put, &ua, 1_000));
    assert!(c.covers(&page, &url, false, &put, &ua, 10_999));
    assert!(
        !c.covers(&page, &url, false, &put, &ua, 11_000),
        "expired at exactly max-age"
    );
    assert!(c.is_empty(), "expired entries are dropped");

    // keyed by credentials, origin and url
    c.store(&page, &url, false, &grant(10), 0);
    assert!(!c.covers(&page, &url, true, &put, &ua, 1));
    assert!(!c.covers(&o("https://other.test/"), &url, false, &put, &ua, 1));
    assert!(!c.covers(&page, &u("https://api.test/y"), false, &put, &ua, 1));
    assert!(!c.covers(&page, &u("https://api.test/x?q=1"), false, &put, &ua, 1));
    assert!(!c.covers(&page, &u("http://api.test/x"), false, &put, &ua, 1));
    assert!(c.covers(&page, &url, false, &put, &ua, 1));
    // what the entry doesn't cover
    assert!(!c.covers(&page, &url, false, &m("DELETE"), &ua, 1));
    assert!(!c.covers(&page, &url, false, &put, &names(&["x-b"]), 1));
    assert!(c.covers(&page, &url, false, &put, &[], 1));
    assert!(c.covers(&page, &url, false, &m("GET"), &[], 1));

    // a newer answer replaces the older
    c.store(
        &page,
        &url,
        false,
        &PreflightGrant {
            methods: names(&["DELETE"]),
            ..grant(10)
        },
        0,
    );
    assert_eq!(c.len(), 1);
    assert!(!c.covers(&page, &url, false, &put, &ua, 1));
    assert!(c.covers(&page, &url, false, &m("DELETE"), &ua, 1));

    // max-age 0 stores nothing, and clears the old entry
    c.store(&page, &url, false, &grant(0), 5);
    assert!(c.is_empty());

    // the cap on a server's max-age
    c.store(&page, &url, false, &grant(1_000_000), 0);
    assert!(c.covers(&page, &url, false, &put, &ua, 599_999));
    assert!(!c.covers(&page, &url, false, &put, &ua, 600_000));

    // size limit: the oldest go
    c.clear();
    for i in 0..10 {
        c.store(
            &page,
            &u(&format!("https://api.test/{i}")),
            false,
            &grant(100),
            0,
        );
    }
    assert_eq!(c.len(), 4);
    assert!(!c.covers(&page, &u("https://api.test/0"), false, &put, &ua, 1));
    assert!(c.covers(&page, &u("https://api.test/9"), false, &put, &ua, 1));

    // a wildcard entry doesn't cover credentialed requests
    c.clear();
    let wild = PreflightGrant {
        methods: vec![],
        headers: vec![],
        methods_wildcard: true,
        headers_wildcard: true,
        max_age: 100,
    };
    c.store(&page, &url, false, &wild, 0);
    assert!(c.covers(&page, &url, false, &m("PATCH"), &names(&["x-q"]), 1));
    assert!(!c.covers(&page, &url, true, &m("PATCH"), &names(&["x-q"]), 1));
}

#[test]
fn preflight_cache_time_extremes() {
    let page = o("https://app.test/");
    let url = u("https://api.test/x");
    let mut c = PreflightCache::new(4, u64::MAX);
    let g = PreflightGrant {
        methods: vec![],
        headers: vec![],
        methods_wildcard: false,
        headers_wildcard: false,
        max_age: u64::MAX,
    };
    c.store(&page, &url, false, &g, u64::MAX - 5);
    assert!(c.covers(&page, &url, false, &m("GET"), &[], u64::MAX - 5));
    let mut c = PreflightCache::new(0, 0);
    c.store(&page, &url, false, &g, 0);
    assert!(c.is_empty());
}

#[test]
fn filter_headers() {
    let resp = h(&[
        ("content-type", "text/plain"),
        ("Set-Cookie", "a=b"),
        ("set-cookie2", "c"),
        ("X-Secret", "s"),
        ("X-Shown", "v"),
        ("Cache-Control", "no-store"),
        ("content-length", "5"),
        ("ETag", "\"x\""),
        ("Last-Modified", "now"),
        ("Expires", "never"),
        ("Pragma", "no-cache"),
        ("Content-Language", "en"),
        ("Access-Control-Expose-Headers", "x-shown, SET-COOKIE, etag"),
    ]);
    let basic = filter_basic(&resp);
    let basic_names: Vec<&str> = basic.iter().map(|(k, _)| k.as_str()).collect();
    assert!(!basic_names.contains(&"set-cookie") && !basic_names.contains(&"set-cookie2"));
    assert!(basic_names.contains(&"x-secret") && basic_names.contains(&"etag"));
    assert!(basic.iter().all(|(k, _)| *k == k.to_ascii_lowercase()));

    let cors = filter_cors(&resp, false);
    let cors_names: Vec<&str> = cors.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        cors_names,
        [
            "content-type",
            "x-shown",
            "cache-control",
            "content-length",
            "etag",
            "last-modified",
            "expires",
            "pragma",
            "content-language",
            "access-control-expose-headers"
        ]
        .iter()
        .filter(|n| !["access-control-expose-headers"].contains(n))
        .copied()
        .collect::<Vec<_>>()
    );
    assert!(!cors_names.contains(&"x-secret"));
    assert!(
        !cors_names.contains(&"set-cookie"),
        "even if named in Expose-Headers"
    );

    // wildcard exposes everything but cookies, unless credentials went
    let wild = h(&[
        ("x-a", "1"),
        ("set-cookie", "s"),
        ("access-control-expose-headers", "*"),
    ]);
    let no_cred: Vec<&str> = filter_cors(&wild, false)
        .iter()
        .map(|(k, _)| k.clone())
        .map(|s| Box::leak(s.into_boxed_str()) as &str)
        .collect();
    assert_eq!(no_cred, ["x-a", "access-control-expose-headers"]);
    let with_cred = filter_cors(&wild, true);
    assert!(with_cred.is_empty());
    // expose list across two header lines
    let two = h(&[
        ("x-a", "1"),
        ("x-b", "2"),
        ("x-c", "3"),
        ("access-control-expose-headers", "x-a"),
        ("access-control-expose-headers", "x-c"),
    ]);
    let got: Vec<String> = filter_cors(&two, true)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(got, ["x-a", "x-c"]);
    // nothing exposed, nothing sensitive
    assert!(
        filter_cors(
            &h(&[
                ("x-a", "1"),
                ("server", "x"),
                ("date", "d"),
                ("vary", "origin")
            ]),
            false
        )
        .is_empty()
    );
}

#[test]
fn corp_table() {
    let page = o("https://app.test/");
    let rows = [
        ("https://app.test/x", "same-origin", true),
        ("https://api.test/x", "same-origin", false),
        ("https://app.test:444/x", "same-origin", false),
        ("https://sub.app.test/x", "same-site", true),
        ("https://other.test/x", "same-site", false),
        ("http://app.test/x", "same-site", false),
        ("https://other.test/x", "cross-origin", true),
        ("https://other.test/x", "", true),
        ("https://other.test/x", "bogus", true),
        ("https://other.test/x", "Same-Origin", true), // byte-case-sensitive values: unknown, ignored
        ("https://other.test/x", " same-origin ", false),
    ];
    for (url, policy, ok) in rows {
        let r = corp_check(
            &page,
            &u(url),
            &h(&[("cross-origin-resource-policy", policy)]),
        );
        assert_eq!(r.is_ok(), ok, "{url} {policy:?}: {r:?}");
        if !ok {
            assert!(matches!(r, Err(NetError::BlockedByCorp { .. })));
        }
    }
    assert!(corp_check(&page, &u("https://other.test/"), &[]).is_ok());
}

// ---- referrers ----

#[test]
fn referrer_policy_names() {
    for p in [
        ReferrerPolicy::NoReferrer,
        ReferrerPolicy::NoReferrerWhenDowngrade,
        ReferrerPolicy::SameOrigin,
        ReferrerPolicy::Origin,
        ReferrerPolicy::StrictOrigin,
        ReferrerPolicy::OriginWhenCrossOrigin,
        ReferrerPolicy::StrictOriginWhenCrossOrigin,
        ReferrerPolicy::UnsafeUrl,
    ] {
        assert_eq!(ReferrerPolicy::parse(p.as_str()), Some(p));
        assert_eq!(
            ReferrerPolicy::parse_header(&p.as_str().to_uppercase()),
            Some(p)
        );
    }
    assert_eq!(ReferrerPolicy::parse(""), None);
    assert_eq!(ReferrerPolicy::parse("Origin"), None);
    assert_eq!(ReferrerPolicy::parse("never"), None);
    assert_eq!(
        ReferrerPolicy::DEFAULT,
        ReferrerPolicy::StrictOriginWhenCrossOrigin
    );
    let rows = [
        ("no-referrer", Some(ReferrerPolicy::NoReferrer)),
        ("origin, no-referrer", Some(ReferrerPolicy::NoReferrer)),
        ("unsafe-url, bogus", Some(ReferrerPolicy::UnsafeUrl)),
        ("bogus, origin", Some(ReferrerPolicy::Origin)),
        (" Same-Origin ", Some(ReferrerPolicy::SameOrigin)),
        ("bogus", None),
        ("", None),
        (",,,", None),
        ("origin;no-referrer", None),
    ];
    for (v, want) in rows {
        assert_eq!(ReferrerPolicy::parse_header(v), want, "{v:?}");
    }
    assert_eq!(ReferrerInit::from_text(""), ReferrerInit::NoReferrer);
    assert_eq!(
        ReferrerInit::from_text("about:client"),
        ReferrerInit::Client
    );
    assert_eq!(
        ReferrerInit::from_text("/x"),
        ReferrerInit::Url("/x".into())
    );
}

#[test]
fn referrer_policy_matrix() {
    use ReferrerPolicy::*;
    let page_https = "https://app.test/dir/page?q=1";
    let page_http = "http://app.test/dir/page?q=1";
    // (policy, page, target, expected)
    let same = "https://app.test/api";
    let cross = "https://other.test/api";
    let down = "http://other.test/api";
    let same_down = "http://app.test/api";
    let local = "http://localhost:8080/api";
    let full = "https://app.test/dir/page?q=1";
    let orig = "https://app.test/";
    let rows: Vec<(ReferrerPolicy, &str, &str, Option<&str>)> = vec![
        (NoReferrer, page_https, same, None),
        (NoReferrer, page_https, cross, None),
        (NoReferrer, page_http, cross, None),
        (NoReferrerWhenDowngrade, page_https, same, Some(full)),
        (NoReferrerWhenDowngrade, page_https, cross, Some(full)),
        (NoReferrerWhenDowngrade, page_https, down, None),
        (NoReferrerWhenDowngrade, page_https, same_down, None),
        (NoReferrerWhenDowngrade, page_https, local, Some(full)),
        (
            NoReferrerWhenDowngrade,
            page_http,
            down,
            Some("http://app.test/dir/page?q=1"),
        ),
        (SameOrigin, page_https, same, Some(full)),
        (SameOrigin, page_https, cross, None),
        (SameOrigin, page_https, down, None),
        (
            SameOrigin,
            page_http,
            "http://app.test/z",
            Some("http://app.test/dir/page?q=1"),
        ),
        (SameOrigin, page_https, "https://app.test:8443/z", None),
        (Origin, page_https, same, Some(orig)),
        (Origin, page_https, cross, Some(orig)),
        (Origin, page_https, down, Some(orig)),
        (StrictOrigin, page_https, same, Some(orig)),
        (StrictOrigin, page_https, cross, Some(orig)),
        (StrictOrigin, page_https, down, None),
        (StrictOrigin, page_https, local, Some(orig)),
        (StrictOrigin, page_http, down, Some("http://app.test/")),
        (OriginWhenCrossOrigin, page_https, same, Some(full)),
        (OriginWhenCrossOrigin, page_https, cross, Some(orig)),
        (OriginWhenCrossOrigin, page_https, down, Some(orig)),
        (StrictOriginWhenCrossOrigin, page_https, same, Some(full)),
        (StrictOriginWhenCrossOrigin, page_https, cross, Some(orig)),
        (StrictOriginWhenCrossOrigin, page_https, down, None),
        (StrictOriginWhenCrossOrigin, page_https, same_down, None),
        (StrictOriginWhenCrossOrigin, page_https, local, Some(orig)),
        (
            StrictOriginWhenCrossOrigin,
            page_http,
            down,
            Some("http://app.test/"),
        ),
        (
            StrictOriginWhenCrossOrigin,
            page_http,
            "http://app.test/z",
            Some("http://app.test/dir/page?q=1"),
        ),
        (UnsafeUrl, page_https, same, Some(full)),
        (UnsafeUrl, page_https, cross, Some(full)),
        (UnsafeUrl, page_https, down, Some(full)),
    ];
    for (policy, page, target, want) in rows {
        let got = referrer_header(policy, &u(page), &u(target));
        assert_eq!(
            got.as_deref(),
            want,
            "{} {page} -> {target}",
            policy.as_str()
        );
    }
}

#[test]
fn referrer_origin_forms_and_length() {
    let page = u("http://localhost:8080/a/b?c=d");
    assert_eq!(
        referrer_header(ReferrerPolicy::Origin, &page, &u("http://x.test/")).as_deref(),
        Some("http://localhost:8080/")
    );
    let v6 = u("http://[::1]:3000/a");
    assert_eq!(
        referrer_header(ReferrerPolicy::Origin, &v6, &u("http://x.test/")).as_deref(),
        Some("http://[::1]:3000/")
    );
    // a long query is cut to the origin
    let long = u(&format!("https://app.test/p?q={}", "a".repeat(5000)));
    assert_eq!(
        referrer_header(ReferrerPolicy::UnsafeUrl, &long, &u("https://x.test/")).as_deref(),
        Some("https://app.test/")
    );
    let ok = u(&format!("https://app.test/p?q={}", "a".repeat(4000)));
    assert!(
        referrer_header(ReferrerPolicy::UnsafeUrl, &ok, &u("https://x.test/"))
            .unwrap()
            .len()
            > 4000
    );
}

// ---- the limiter and limits ----

#[test]
fn limiter_admission_and_queue() {
    let lim = Limits {
        max_per_tab: 3,
        max_per_host: 2,
        max_pending: 5,
        ..Limits::default()
    };
    let mut l = Limiter::new(&lim);
    assert!(!l.busy());
    assert_eq!(l.submit(1, "a.test:80"), Ok(Admission::Start));
    assert_eq!(l.submit(2, "a.test:80"), Ok(Admission::Start));
    assert_eq!(
        l.submit(3, "a.test:80"),
        Ok(Admission::Queued),
        "per-host limit"
    );
    assert_eq!(
        l.submit(4, "b.test:80"),
        Ok(Admission::Start),
        "another host isn't held back"
    );
    assert_eq!(
        l.submit(5, "c.test:80"),
        Ok(Admission::Queued),
        "per-tab limit"
    );
    assert_eq!((l.running(), l.waiting()), (3, 2));
    assert_eq!(
        l.submit(6, "d.test:80"),
        Err(NetError::TooManyRequests { max: 5 })
    );
    assert_eq!(
        l.submit(2, "z"),
        Err(NetError::OutOfOrder("request id used twice"))
    );
    assert_eq!(
        l.submit(3, "z"),
        Err(NetError::OutOfOrder("request id used twice"))
    );
    // finishing a.test:80 #1 lets #3 start (host free), not #5 (tab still full)
    assert_eq!(l.finish(1), vec![3]);
    assert_eq!((l.running(), l.waiting()), (3, 1));
    // finishing b.test starts c.test
    assert_eq!(l.finish(4), vec![5]);
    assert_eq!((l.running(), l.waiting()), (3, 0));
    // cancelling something that's running or unknown is harmless
    assert!(l.finish(99).is_empty());
    assert_eq!(l.finish(2), Vec::<u64>::new());
    assert_eq!(l.finish(3), Vec::<u64>::new());
    assert_eq!(l.finish(5), Vec::<u64>::new());
    assert!(!l.busy());
    // after all this, the ids can be reused
    assert_eq!(l.submit(1, "a.test:80"), Ok(Admission::Start));
}

#[test]
fn limiter_cancel_waiting_and_clear() {
    let lim = Limits {
        max_per_tab: 1,
        max_per_host: 1,
        max_pending: 4,
        ..Limits::default()
    };
    let mut l = Limiter::new(&lim);
    assert_eq!(l.submit(1, "a:80"), Ok(Admission::Start));
    assert_eq!(l.submit(2, "a:80"), Ok(Admission::Queued));
    assert_eq!(l.submit(3, "a:80"), Ok(Admission::Queued));
    // a waiting request is cancelled: nothing starts, the order of the rest is kept
    assert!(l.finish(2).is_empty());
    assert_eq!(l.finish(1), vec![3]);
    l.clear();
    assert!(!l.busy());
    assert_eq!((l.running(), l.waiting()), (0, 0));
    // queue order per host
    assert_eq!(l.submit(10, "a:80"), Ok(Admission::Start));
    assert_eq!(l.submit(11, "a:80"), Ok(Admission::Queued));
    assert_eq!(l.submit(12, "a:80"), Ok(Admission::Queued));
    assert_eq!(l.finish(10), vec![11]);
    assert_eq!(l.finish(11), vec![12]);
}

#[test]
fn limiter_zero_limits_are_clamped() {
    let lim = Limits {
        max_per_tab: 0,
        max_per_host: 0,
        max_pending: 0,
        ..Limits::default()
    };
    let mut l = Limiter::new(&lim);
    assert_eq!(l.submit(1, "a:80"), Ok(Admission::Start));
    assert!(matches!(
        l.submit(2, "a:80"),
        Err(NetError::TooManyRequests { max: 1 })
    ));
}

#[test]
fn effective_timeout() {
    let c = Config::default();
    assert_eq!(c.effective_timeout(None), 30_000);
    assert_eq!(c.effective_timeout(Some(0)), 30_000);
    assert_eq!(c.effective_timeout(Some(1)), 1);
    assert_eq!(c.effective_timeout(Some(5_000)), 5_000);
    assert_eq!(c.effective_timeout(Some(30_000)), 30_000);
    assert_eq!(c.effective_timeout(Some(30_001)), 30_000);
    assert_eq!(c.effective_timeout(Some(u64::MAX)), 30_000);
    assert!(c.block_private_network);
}

#[test]
fn default_limits_are_sane() {
    let l = Limits::default();
    assert_eq!(l.max_redirects, 20);
    assert!(l.max_per_host <= l.max_per_tab && l.max_per_tab <= l.max_pending);
    assert_eq!(l.max_keepalive_body, 65536);
    assert!(l.preflight_default_age <= l.preflight_max_age);
    assert!(l.max_url >= 2048);
    assert!(l.max_request_body <= 64 << 20 && l.max_response_body <= 64 << 20);
}

// ---- error texts ----

#[test]
fn error_kinds_and_texts() {
    let cors = NetError::Cors {
        kind: CorsError::MissingAllowOrigin,
        page_origin: "https://app.test".into(),
        url: "https://api.test/x".into(),
    };
    assert_eq!(cors.kind(), ErrorKind::Network);
    let text = cors.to_string();
    assert!(
        text.starts_with(
            "Blocked by CORS: https://api.test/x doesn't allow origin https://app.test"
        ),
        "{text}"
    );
    assert!(text.contains("Access-Control-Allow-Origin"));
    // the page is not told why
    assert_eq!(cors.script_message(), "Failed to fetch");

    let mixed = NetError::MixedContent {
        page: "https://a.test/".into(),
        url: "http://b.test/x".into(),
    };
    assert!(mixed.to_string().starts_with("Mixed content"));
    assert!(mixed.to_string().contains("http://b.test/x"));
    assert_eq!(mixed.script_message(), "Failed to fetch");

    // type errors are the page's own doing and say what's wrong
    for e in [
        NetError::BadUrl("x".into()),
        NetError::UrlHasCredentials,
        NetError::UnsupportedScheme("file".into()),
        NetError::ForbiddenMethod("TRACE".into()),
        NetError::BadHeaderName("a b".into()),
        NetError::NoCorsMethod("PUT".into()),
        NetError::BodyNotAllowed("GET".into()),
        NetError::CacheNeedsSameOrigin,
        NetError::NavigateMode,
    ] {
        assert_eq!(e.kind(), ErrorKind::Type, "{e:?}");
        assert_eq!(e.script_message(), e.to_string());
    }
    assert_eq!(NetError::Timeout { ms: 5 }.kind(), ErrorKind::Timeout);
    assert!(NetError::Timeout { ms: 5 }.to_string().contains("5 ms"));
    assert_eq!(
        NetError::TooManyRedirects { max: 20 }.kind(),
        ErrorKind::Network
    );
    assert!(
        NetError::UnsupportedScheme("file".into())
            .to_string()
            .contains("file:")
    );
    for k in [
        CorsError::MissingAllowOrigin,
        CorsError::MultipleAllowOrigin,
        CorsError::WildcardWithCredentials,
        CorsError::OriginMismatch {
            allowed: "x".into(),
        },
        CorsError::CredentialsNotAllowed,
        CorsError::PreflightStatus(404),
        CorsError::PreflightRedirect,
        CorsError::MethodNotAllowed {
            method: "PUT".into(),
            wildcard_ignored: true,
        },
        CorsError::HeaderNotAllowed {
            name: "x-a".into(),
            wildcard_ignored: true,
        },
    ] {
        assert!(!k.to_string().is_empty());
    }
    assert!(
        CorsError::OriginMismatch {
            allowed: "https://z.test".into()
        }
        .to_string()
        .contains("https://z.test")
    );
    assert!(CorsError::PreflightStatus(404).to_string().contains("404"));
    assert!(
        CorsError::MethodNotAllowed {
            method: "PUT".into(),
            wildcard_ignored: true
        }
        .to_string()
        .contains("*")
    );
    assert!(
        CorsError::HeaderNotAllowed {
            name: "x-a".into(),
            wildcard_ignored: false
        }
        .to_string()
        .contains("x-a")
    );
}

#[test]
fn unsafe_names_helper_matches() {
    let hdrs = h(&[("X-A", "1"), ("Accept", "*/*")]);
    assert_eq!(unsafe_names(&hdrs), cors_unsafe_request_header_names(&hdrs));
}
