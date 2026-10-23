//! Whole exchanges: a request from start to its last hop, the way the Browser's networking
//! will drive it.

use super::cors::{PreflightCache, ResponseType};
use super::error::{CorsError, NetError};
use super::exchange::*;
use super::headers::{Guard, Headers, Method};
use super::limits::{Config, Limits};
use super::origin::{AddressSpace, Origin};
use super::referrer::{ReferrerInit, ReferrerPolicy};
use crate::cookies::Context;
use milkweb::url::Url;

type Hdrs = Vec<(String, String)>;

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn h(v: &[(&str, &str)]) -> Hdrs {
    v.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn page(url: &str) -> Page {
    Page::new(u(url))
}

fn cfg() -> Config {
    Config::default()
}

fn init(url: &str) -> RequestInit {
    RequestInit::new(url)
}

fn with_headers(mut i: RequestInit, v: &[(&str, &str)]) -> RequestInit {
    for (n, x) in v {
        i.headers.append(n, x).unwrap();
    }
    i
}

fn with_method(mut i: RequestInit, m: &str) -> RequestInit {
    i.method = Method::parse(m).unwrap();
    i
}

fn body(len: usize, ct: Option<&str>) -> Option<BodyInfo> {
    Some(BodyInfo {
        len,
        content_type: ct.map(String::from),
    })
}

fn net(page: &Page, i: RequestInit) -> Exchange {
    match Exchange::start(&cfg(), page, i) {
        Ok(Begin::Network(ex)) => ex,
        other => panic!("expected a network exchange, got {other:?}"),
    }
}

fn start_err(page: &Page, i: RequestInit) -> NetError {
    match Exchange::start(&cfg(), page, i) {
        Err(e) => e,
        Ok(b) => panic!("expected an error, got {b:?}"),
    }
}

fn go(ex: &mut Exchange) -> Hop {
    ex.next_hop(&mut PreflightCache::new(64, 600), 0).unwrap()
}

fn hdr<'a>(h: &'a Hop, name: &str) -> Option<&'a str> {
    h.headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn done(o: Outcome) -> ResponseMeta {
    match o.next {
        Next::Done(m) => m,
        other => panic!("expected a final response, got {other:?}"),
    }
}

fn redirected_to(o: &Outcome) -> String {
    match &o.next {
        Next::Redirect { to } => to.to_text(),
        other => panic!("expected a redirect, got {other:?}"),
    }
}

fn is_cors(e: &NetError, want: &CorsError) -> bool {
    matches!(e, NetError::Cors { kind, .. } if kind == want)
}

const ACAO: &str = "access-control-allow-origin";

// ---------------------------------------------------------------------------------------
// Same-origin

#[test]
fn same_origin_get() {
    let p = page("https://app.test/dir/page?x=1");
    let mut ex = net(&p, init("/api/items?id=2"));
    let hop = go(&mut ex);
    assert_eq!(hop.method.as_str(), "GET");
    assert_eq!(hop.url.to_text(), "https://app.test/api/items?id=2");
    assert!(hop.preflight.is_none());
    assert!(!hop.send_body);
    assert_eq!(hdr(&hop, "accept"), Some("*/*"));
    assert_eq!(
        hdr(&hop, "origin"),
        None,
        "same-origin GET carries no Origin"
    );
    assert_eq!(hdr(&hop, "referer"), Some("https://app.test/dir/page?x=1"));
    assert_eq!(hdr(&hop, "sec-fetch-site"), Some("same-origin"));
    assert_eq!(hdr(&hop, "sec-fetch-mode"), Some("cors"));
    assert_eq!(hdr(&hop, "sec-fetch-dest"), Some("empty"));
    assert_eq!(
        hop.cookies,
        Some(Context::Subresource {
            page_site: "app.test".into()
        })
    );
    assert_eq!(hop.timeout_ms, 30_000);
    let out = ex
        .on_response(
            200,
            "OK",
            &h(&[
                ("content-type", "application/json"),
                ("set-cookie", "a=b"),
                ("x-a", "1"),
            ]),
        )
        .unwrap();
    assert_eq!(
        out.store_cookies,
        Some(Context::Subresource {
            page_site: "app.test".into()
        })
    );
    let m = done(out);
    assert_eq!(m.kind, ResponseType::Basic);
    assert_eq!((m.status, m.status_text.as_str()), (200, "OK"));
    assert!(m.ok() && !m.redirected && !m.hide_body);
    assert_eq!(m.url, "https://app.test/api/items?id=2");
    assert_eq!(m.header("X-A").as_deref(), Some("1"));
    assert!(
        m.header("set-cookie").is_none(),
        "Set-Cookie never reaches the page"
    );
}

#[test]
fn same_origin_post_sends_origin() {
    let p = page("http://app.test:8080/");
    let mut i = with_method(init("/submit"), "POST");
    i.body = body(10, Some("text/plain;charset=UTF-8"));
    let mut ex = net(&p, i);
    let hop = go(&mut ex);
    assert!(hop.send_body);
    assert_eq!(hdr(&hop, "origin"), Some("http://app.test:8080"));
    assert_eq!(hdr(&hop, "content-type"), Some("text/plain;charset=UTF-8"));
    assert!(hop.preflight.is_none());
}

#[test]
fn same_origin_unsafe_requests_need_no_preflight() {
    let p = page("https://app.test/");
    for m in ["PUT", "DELETE", "PATCH", "OPTIONS"] {
        let i = with_headers(
            with_method(init("/x"), m),
            &[
                ("X-Custom", "1"),
                ("Content-Type", "application/json"),
                ("Authorization", "t"),
            ],
        );
        let mut ex = net(&p, i);
        let hop = go(&mut ex);
        assert!(hop.preflight.is_none(), "{m}");
        assert_eq!(hdr(&hop, "x-custom"), Some("1"));
        assert_eq!(hdr(&hop, "authorization"), Some("t"));
    }
}

#[test]
fn page_cannot_set_headers_the_browser_owns() {
    let p = page("https://app.test/");
    let mut raw = Headers::new(Guard::None); // a buggy caller passes an unguarded list
    for (n, v) in [
        ("Cookie", "stolen=1"),
        ("Host", "evil.test"),
        ("Origin", "https://evil.test"),
        ("Referer", "https://evil.test/"),
        ("Sec-Fetch-Site", "same-origin"),
        ("Proxy-Authorization", "x"),
        ("Content-Length", "0"),
        ("X-Mine", "ok"),
        ("X-HTTP-Method-Override", "TRACE"),
    ] {
        raw.append(n, v).unwrap();
    }
    let mut i = init("https://other.test/x");
    i.headers = raw;
    let mut ex = net(&p, i);
    let hop = go(&mut ex);
    assert_eq!(hdr(&hop, "cookie"), None);
    assert_eq!(hdr(&hop, "host"), None);
    assert_eq!(hdr(&hop, "origin"), Some("https://app.test"));
    assert_eq!(hdr(&hop, "referer"), Some("https://app.test/"));
    assert_eq!(hdr(&hop, "sec-fetch-site"), Some("cross-site"));
    assert_eq!(hdr(&hop, "proxy-authorization"), None);
    assert_eq!(hdr(&hop, "content-length"), None);
    assert_eq!(hdr(&hop, "x-http-method-override"), None);
    assert_eq!(hdr(&hop, "x-mine"), Some("ok"));
    // the only Origin / Referer / Sec-Fetch-* are the ones the policy made
    for name in [
        "origin",
        "referer",
        "sec-fetch-site",
        "sec-fetch-mode",
        "sec-fetch-dest",
    ] {
        assert_eq!(
            hop.headers.iter().filter(|(k, _)| k == name).count(),
            1,
            "{name}"
        );
    }
    // x-mine makes it a preflighted request
    assert!(hop.preflight.is_some());
}

// ---------------------------------------------------------------------------------------
// Simple cross-origin requests

#[test]
fn cross_origin_simple_get() {
    let p = page("https://app.test/dir/page?x=1");
    let mut ex = net(&p, init("https://api.test/v1/list"));
    let hop = go(&mut ex);
    assert!(hop.preflight.is_none());
    assert_eq!(hdr(&hop, "origin"), Some("https://app.test"));
    assert_eq!(
        hdr(&hop, "referer"),
        Some("https://app.test/"),
        "cross-origin: origin only"
    );
    assert_eq!(hdr(&hop, "sec-fetch-site"), Some("cross-site"));
    assert_eq!(
        hop.cookies, None,
        "credentials 'same-origin' sends no cookies cross-origin"
    );
    let out = ex
        .on_response(
            200,
            "OK",
            &h(&[
                (ACAO, "https://app.test"),
                ("content-type", "application/json"),
                ("x-private", "1"),
                ("set-cookie", "a=b"),
                ("content-length", "2"),
            ]),
        )
        .unwrap();
    assert_eq!(out.store_cookies, None);
    let m = done(out);
    assert_eq!(m.kind, ResponseType::Cors);
    assert_eq!(m.status, 200);
    assert_eq!(m.url, "https://api.test/v1/list");
    let names: Vec<&str> = m.headers.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        ["content-type", "content-length"],
        "only CORS-safelisted headers"
    );
    assert!(!m.hide_body);
}

#[test]
fn cross_origin_get_blocked_without_allow_origin() {
    let p = page("https://app.test/");
    let mut ex = net(&p, init("https://api.test/v1/list"));
    go(&mut ex);
    let e = ex
        .on_response(200, "OK", &h(&[("content-type", "text/plain")]))
        .unwrap_err();
    assert!(is_cors(&e, &CorsError::MissingAllowOrigin), "{e:?}");
    assert!(
        e.to_string().contains("Blocked by CORS")
            && e.to_string().contains("https://api.test/v1/list")
    );
    assert_eq!(e.script_message(), "Failed to fetch");
    // the exchange is over
    assert!(matches!(
        ex.on_response(200, "OK", &[]),
        Err(NetError::OutOfOrder(_))
    ));
    assert!(matches!(
        ex.next_hop(&mut PreflightCache::new(4, 60), 0),
        Err(NetError::OutOfOrder(_))
    ));
}

#[test]
fn cross_origin_error_statuses_also_need_cors() {
    let p = page("https://app.test/");
    for status in [301u16, 400, 404, 500, 503] {
        let mut ex = net(&p, init("https://api.test/x"));
        go(&mut ex);
        let hdrs = if status == 301 {
            h(&[("location", "/y")])
        } else {
            vec![]
        };
        assert!(ex.on_response(status, "x", &hdrs).is_err(), "{status}");
        let mut ex = net(&p, init("https://api.test/x"));
        go(&mut ex);
        if status != 301 {
            let m = done(ex.on_response(status, "x", &h(&[(ACAO, "*")])).unwrap());
            assert_eq!(m.status, status);
            assert!(!m.ok());
        }
    }
}

#[test]
fn cross_origin_wildcard_and_credentials() {
    let p = page("https://app.test/");
    // credentials 'include' + wildcard: refused
    let mut i = init("https://api.test/x");
    i.credentials = Credentials::Include;
    let mut ex = net(&p, i);
    let hop1 = go(&mut ex);
    assert_eq!(
        hop1.cookies, None,
        "cross-site: no third-party cookies even with include"
    );
    let e = ex.on_response(200, "OK", &h(&[(ACAO, "*")])).unwrap_err();
    assert!(is_cors(&e, &CorsError::WildcardWithCredentials));
    // exact origin without Allow-Credentials: refused
    let mut i = init("https://api.test/x");
    i.credentials = Credentials::Include;
    let mut ex = net(&p, i);
    go(&mut ex);
    let e = ex
        .on_response(200, "OK", &h(&[(ACAO, "https://app.test")]))
        .unwrap_err();
    assert!(is_cors(&e, &CorsError::CredentialsNotAllowed));
    // exact origin and Allow-Credentials: accepted
    let mut i = init("https://api.test/x");
    i.credentials = Credentials::Include;
    let mut ex = net(&p, i);
    go(&mut ex);
    let m = done(
        ex.on_response(
            200,
            "OK",
            &h(&[
                (ACAO, "https://app.test"),
                ("access-control-allow-credentials", "true"),
            ]),
        )
        .unwrap(),
    );
    assert_eq!(m.kind, ResponseType::Cors);
    // credentials 'same-origin' (the default) with a wildcard: accepted
    let mut ex = net(&p, init("https://api.test/x"));
    go(&mut ex);
    assert!(ex.on_response(200, "OK", &h(&[(ACAO, "*")])).is_ok());
    // credentials 'omit' with wildcard: accepted
    let mut i = init("https://api.test/x");
    i.credentials = Credentials::Omit;
    let mut ex = net(&p, i);
    go(&mut ex);
    assert!(ex.on_response(200, "OK", &h(&[(ACAO, "*")])).is_ok());
}

#[test]
fn cookies_follow_credentials_and_site() {
    let ctx = |s: &str| {
        Some(Context::Subresource {
            page_site: s.into(),
        })
    };
    let rows: [(&str, &str, Credentials, Option<&str>); 12] = [
        (
            "https://www.app.test/",
            "https://www.app.test/api",
            Credentials::SameOrigin,
            Some("app.test"),
        ),
        (
            "https://www.app.test/",
            "https://www.app.test/api",
            Credentials::Include,
            Some("app.test"),
        ),
        (
            "https://www.app.test/",
            "https://www.app.test/api",
            Credentials::Omit,
            None,
        ),
        // same site, other origin
        (
            "https://www.app.test/",
            "https://api.app.test/x",
            Credentials::SameOrigin,
            None,
        ),
        (
            "https://www.app.test/",
            "https://api.app.test/x",
            Credentials::Include,
            Some("app.test"),
        ),
        (
            "https://www.app.test/",
            "https://api.app.test/x",
            Credentials::Omit,
            None,
        ),
        (
            "https://www.app.test/",
            "https://api.app.test:8443/x",
            Credentials::Include,
            Some("app.test"),
        ),
        // other site
        (
            "https://app.test/",
            "https://other.test/x",
            Credentials::Include,
            None,
        ),
        (
            "https://app.test/",
            "https://other.test/x",
            Credentials::SameOrigin,
            None,
        ),
        // schemeful: http and https are different sites
        (
            "http://app.test/",
            "https://app.test/x",
            Credentials::Include,
            None,
        ),
        (
            "http://app.test/",
            "http://api.app.test/x",
            Credentials::Include,
            Some("app.test"),
        ),
        (
            "http://localhost:3000/",
            "http://localhost:4000/x",
            Credentials::Include,
            Some("localhost"),
        ),
    ];
    for (pg, url, cred, want) in rows {
        let p = page(pg);
        let mut i = init(url);
        i.credentials = cred;
        let mut ex = net(&p, i);
        let hop = go(&mut ex);
        assert_eq!(hop.cookies, want.and_then(ctx), "{pg} -> {url} {cred:?}");
    }
}

#[test]
fn set_cookie_is_stored_only_after_the_cors_check() {
    let p = page("https://www.app.test/");
    let mut i = init("https://api.app.test/x");
    i.credentials = Credentials::Include;
    let mut ex = net(&p, i.clone());
    go(&mut ex);
    let out = ex
        .on_response(
            200,
            "OK",
            &h(&[
                (ACAO, "https://www.app.test"),
                ("access-control-allow-credentials", "true"),
                ("set-cookie", "a=b"),
            ]),
        )
        .unwrap();
    assert_eq!(
        out.store_cookies,
        Some(Context::Subresource {
            page_site: "app.test".into()
        })
    );
    // a failed check: an Err, so there is nothing to store
    let mut ex = net(&p, i);
    go(&mut ex);
    assert!(
        ex.on_response(200, "OK", &h(&[("set-cookie", "a=b")]))
            .is_err()
    );
}

#[test]
fn cross_origin_credentialed_but_other_site_never_stores() {
    let p = page("https://app.test/");
    let mut i = init("https://other.test/x");
    i.credentials = Credentials::Include;
    let mut ex = net(&p, i);
    go(&mut ex);
    let out = ex
        .on_response(
            200,
            "OK",
            &h(&[
                (ACAO, "https://app.test"),
                ("access-control-allow-credentials", "true"),
                ("set-cookie", "a=b"),
            ]),
        )
        .unwrap();
    assert_eq!(out.store_cookies, None);
}

// ---------------------------------------------------------------------------------------
// Preflight

fn put_request() -> RequestInit {
    let mut i = with_headers(
        with_method(init("https://api.test/item/1"), "PUT"),
        &[("X-Token", "t"), ("Content-Type", "application/json")],
    );
    i.body = body(7, None);
    i
}

fn ok_preflight() -> Hdrs {
    h(&[
        (ACAO, "https://app.test"),
        ("access-control-allow-methods", "GET, PUT, DELETE"),
        ("access-control-allow-headers", "x-token, content-type"),
        ("access-control-max-age", "60"),
    ])
}

#[test]
fn preflighted_put_with_custom_header() {
    let p = page("https://app.test/dir/");
    let mut cache = PreflightCache::new(16, 600);
    let mut ex = net(&p, put_request());
    let hop = ex.next_hop(&mut cache, 1_000).unwrap();
    let pf = hop.preflight.clone().expect("a preflight is needed");
    assert_eq!(pf.url.to_text(), "https://api.test/item/1");
    let get = |n: &str| {
        pf.headers
            .iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(get("access-control-request-method"), Some("PUT"));
    assert_eq!(
        get("access-control-request-headers"),
        Some("content-type,x-token")
    );
    assert_eq!(get("origin"), Some("https://app.test"));
    assert_eq!(get("referer"), Some("https://app.test/"));
    assert_eq!(get("cookie"), None);
    assert_eq!(get("x-token"), None);
    // the real request, as it will go
    assert_eq!(hop.method.as_str(), "PUT");
    assert_eq!(hdr(&hop, "x-token"), Some("t"));
    assert_eq!(hdr(&hop, "content-type"), Some("application/json"));
    assert!(hop.send_body);
    // the request can't be answered before the preflight is
    assert!(matches!(
        ex.on_response(200, "OK", &[]),
        Err(NetError::OutOfOrder(_))
    ));
    let mut ex = net(&p, put_request());
    ex.next_hop(&mut cache, 1_000).unwrap();
    ex.on_preflight_response(&mut cache, 1_000, 204, &ok_preflight())
        .unwrap();
    assert_eq!(cache.len(), 1);
    let m = done(
        ex.on_response(200, "OK", &h(&[(ACAO, "https://app.test")]))
            .unwrap(),
    );
    assert_eq!(m.kind, ResponseType::Cors);
    // a second request within max-age skips the preflight
    let mut ex2 = net(&p, put_request());
    let hop2 = ex2.next_hop(&mut cache, 30_000).unwrap();
    assert!(hop2.preflight.is_none());
    // and after it expires, it's back
    let mut ex3 = net(&p, put_request());
    let hop3 = ex3.next_hop(&mut cache, 61_001).unwrap();
    assert!(hop3.preflight.is_some());
}

#[test]
fn preflight_failures_end_the_exchange() {
    let p = page("https://app.test/");
    let cases: Vec<(&str, u16, Hdrs, CorsError)> = vec![
        ("404", 404, ok_preflight(), CorsError::PreflightStatus(404)),
        ("500", 500, ok_preflight(), CorsError::PreflightStatus(500)),
        (
            "redirect",
            302,
            h(&[("location", "https://api.test/y")]),
            CorsError::PreflightRedirect,
        ),
        (
            "no ACAO",
            200,
            h(&[
                ("access-control-allow-methods", "PUT"),
                ("access-control-allow-headers", "x-token, content-type"),
            ]),
            CorsError::MissingAllowOrigin,
        ),
        (
            "method missing",
            200,
            h(&[
                (ACAO, "https://app.test"),
                ("access-control-allow-methods", "GET"),
                ("access-control-allow-headers", "x-token, content-type"),
            ]),
            CorsError::MethodNotAllowed {
                method: "PUT".into(),
                wildcard_ignored: false,
            },
        ),
        (
            "header missing",
            200,
            h(&[
                (ACAO, "https://app.test"),
                ("access-control-allow-methods", "PUT"),
                ("access-control-allow-headers", "x-token"),
            ]),
            CorsError::HeaderNotAllowed {
                name: "content-type".into(),
                wildcard_ignored: false,
            },
        ),
        (
            "wrong origin",
            200,
            h(&[
                (ACAO, "https://evil.test"),
                ("access-control-allow-methods", "PUT"),
                ("access-control-allow-headers", "x-token, content-type"),
            ]),
            CorsError::OriginMismatch {
                allowed: "https://evil.test".into(),
            },
        ),
    ];
    for (label, status, hdrs, want) in cases {
        let mut cache = PreflightCache::new(16, 600);
        let mut ex = net(&p, put_request());
        ex.next_hop(&mut cache, 0).unwrap();
        let e = ex
            .on_preflight_response(&mut cache, 0, status, &hdrs)
            .unwrap_err();
        assert!(is_cors(&e, &want), "{label}: {e:?}");
        assert!(
            cache.is_empty(),
            "{label}: a failed preflight is not cached"
        );
        assert!(
            matches!(
                ex.on_response(200, "OK", &h(&[(ACAO, "https://app.test")])),
                Err(NetError::OutOfOrder(_))
            ),
            "{label}: the request is never sent"
        );
    }
}

#[test]
fn preflight_not_needed_for_simple_requests() {
    let p = page("https://app.test/");
    let simple: Vec<RequestInit> = vec![
        init("https://api.test/x"),
        with_method(init("https://api.test/x"), "HEAD"),
        with_headers(
            init("https://api.test/x"),
            &[("Accept", "application/json"), ("Accept-Language", "en")],
        ),
        {
            let mut i = with_method(init("https://api.test/x"), "POST");
            i.body = body(3, Some("text/plain;charset=UTF-8"));
            i
        },
        {
            let mut i = with_method(init("https://api.test/x"), "POST");
            i.body = body(3, Some("application/x-www-form-urlencoded;charset=UTF-8"));
            i
        },
        {
            let mut i = with_method(init("https://api.test/x"), "POST");
            i = with_headers(i, &[("Content-Type", "multipart/form-data; boundary=x")]);
            i.body = body(3, None);
            i
        },
        {
            let mut i = with_method(init("https://api.test/x"), "POST");
            i.body = body(0, None); // a Blob with no type adds no Content-Type
            i
        },
    ];
    for (n, i) in simple.into_iter().enumerate() {
        let mut ex = net(&p, i);
        assert!(go(&mut ex).preflight.is_none(), "case {n}");
    }
}

#[test]
fn preflight_needed_for() {
    let p = page("https://app.test/");
    let mut cases: Vec<(&str, RequestInit)> = vec![
        ("PUT", with_method(init("https://api.test/x"), "PUT")),
        ("DELETE", with_method(init("https://api.test/x"), "DELETE")),
        ("PATCH", with_method(init("https://api.test/x"), "PATCH")),
        (
            "patch (not normalised)",
            with_method(init("https://api.test/x"), "patch"),
        ),
        (
            "OPTIONS",
            with_method(init("https://api.test/x"), "OPTIONS"),
        ),
        (
            "custom header",
            with_headers(init("https://api.test/x"), &[("X-A", "1")]),
        ),
        (
            "authorization",
            with_headers(init("https://api.test/x"), &[("Authorization", "t")]),
        ),
        (
            "range",
            with_headers(init("https://api.test/x"), &[("Range", "bytes=0-9")]),
        ),
        (
            "json content type",
            with_headers(
                with_method(init("https://api.test/x"), "POST"),
                &[("Content-Type", "application/json")],
            ),
        ),
        (
            "cache-control",
            with_headers(init("https://api.test/x"), &[("Cache-Control", "no-cache")]),
        ),
        (
            "accept with unsafe byte",
            with_headers(init("https://api.test/x"), &[("Accept", "a(b")]),
        ),
        (
            "long accept",
            with_headers(init("https://api.test/x"), &[("Accept", &"a".repeat(129))]),
        ),
    ];
    let mut forced = init("https://api.test/x");
    forced.force_preflight = true;
    cases.push(("forced (upload listener)", forced));
    let mut json = with_method(init("https://api.test/x"), "POST");
    json.body = body(5, Some("application/json"));
    cases.push(("body type application/json", json));
    for (label, i) in cases {
        let mut ex = net(&p, i);
        assert!(go(&mut ex).preflight.is_some(), "{label}");
    }
}

#[test]
fn preflight_lists_the_default_content_type() {
    let p = page("https://app.test/");
    let mut i = with_method(init("https://api.test/x"), "POST");
    i.body = body(5, Some("application/json"));
    let mut ex = net(&p, i);
    let hop = go(&mut ex);
    let pf = hop.preflight.clone().unwrap();
    assert!(pf.headers.contains(&(
        "access-control-request-headers".to_string(),
        "content-type".to_string()
    )));
    assert!(pf.headers.contains(&(
        "access-control-request-method".to_string(),
        "POST".to_string()
    )));
    assert_eq!(hdr(&hop, "content-type"), Some("application/json"));
    // a Content-Type the page set wins over the body's
    let mut i = with_headers(
        with_method(init("https://api.test/x"), "POST"),
        &[("Content-Type", "text/plain")],
    );
    i.body = body(5, Some("application/json"));
    let mut ex = net(&p, i);
    let hop = go(&mut ex);
    assert!(hop.preflight.is_none());
    assert_eq!(hdr(&hop, "content-type"), Some("text/plain"));
}

#[test]
fn forced_preflight_on_a_simple_request() {
    let p = page("https://app.test/");
    let mut i = init("https://api.test/x");
    i.force_preflight = true;
    let mut cache = PreflightCache::new(4, 600);
    let mut ex = net(&p, i.clone());
    let hop = ex.next_hop(&mut cache, 0).unwrap();
    let pf = hop.preflight.unwrap();
    assert!(pf.headers.contains(&(
        "access-control-request-method".to_string(),
        "GET".to_string()
    )));
    assert!(
        pf.headers
            .iter()
            .all(|(k, _)| k != "access-control-request-headers")
    );
    // GET needs no Allow-Methods listing; a bare 204 with ACAO does
    ex.on_preflight_response(&mut cache, 0, 204, &h(&[(ACAO, "https://app.test")]))
        .unwrap();
    assert!(
        ex.on_response(200, "OK", &h(&[(ACAO, "https://app.test")]))
            .is_ok()
    );
    // same-origin requests are never preflighted, forced or not
    let mut i = init("/x");
    i.force_preflight = true;
    let mut ex = net(&p, i);
    assert!(go(&mut ex).preflight.is_none());
}

#[test]
fn preflight_with_credentials() {
    let p = page("https://app.test/");
    let mk = || {
        let mut i = with_method(init("https://api.test/x"), "DELETE");
        i.credentials = Credentials::Include;
        i
    };
    let mut cache = PreflightCache::new(4, 600);
    // wildcard method is not enough with credentials
    let mut ex = net(&p, mk());
    ex.next_hop(&mut cache, 0).unwrap();
    let e = ex
        .on_preflight_response(
            &mut cache,
            0,
            200,
            &h(&[
                (ACAO, "https://app.test"),
                ("access-control-allow-credentials", "true"),
                ("access-control-allow-methods", "*"),
            ]),
        )
        .unwrap_err();
    assert!(is_cors(
        &e,
        &CorsError::MethodNotAllowed {
            method: "DELETE".into(),
            wildcard_ignored: true
        }
    ));
    // preflight needs Allow-Credentials too
    let mut ex = net(&p, mk());
    ex.next_hop(&mut cache, 0).unwrap();
    let e = ex
        .on_preflight_response(
            &mut cache,
            0,
            200,
            &h(&[
                (ACAO, "https://app.test"),
                ("access-control-allow-methods", "DELETE"),
            ]),
        )
        .unwrap_err();
    assert!(is_cors(&e, &CorsError::CredentialsNotAllowed));
    // success is cached for credentialed requests only
    let mut ex = net(&p, mk());
    ex.next_hop(&mut cache, 0).unwrap();
    ex.on_preflight_response(
        &mut cache,
        0,
        200,
        &h(&[
            (ACAO, "https://app.test"),
            ("access-control-allow-credentials", "true"),
            ("access-control-allow-methods", "DELETE"),
        ]),
    )
    .unwrap();
    let mut ex = net(&p, mk());
    assert!(ex.next_hop(&mut cache, 1).unwrap().preflight.is_none());
    let mut plain = with_method(init("https://api.test/x"), "DELETE");
    plain.credentials = Credentials::SameOrigin;
    let mut ex = net(&p, plain);
    assert!(ex.next_hop(&mut cache, 1).unwrap().preflight.is_some());
}

#[test]
fn preflight_wildcards_without_credentials() {
    let p = page("https://app.test/");
    let mut cache = PreflightCache::new(4, 600);
    let mut ex = net(
        &p,
        with_headers(
            with_method(init("https://api.test/x"), "PATCH"),
            &[("X-A", "1"), ("X-B", "2")],
        ),
    );
    ex.next_hop(&mut cache, 0).unwrap();
    ex.on_preflight_response(
        &mut cache,
        0,
        200,
        &h(&[
            (ACAO, "*"),
            ("access-control-allow-methods", "*"),
            ("access-control-allow-headers", "*"),
        ]),
    )
    .unwrap();
    // authorization needs naming even then
    let mut ex = net(
        &p,
        with_headers(init("https://api.test/y"), &[("Authorization", "t")]),
    );
    ex.next_hop(&mut cache, 0).unwrap();
    let e = ex
        .on_preflight_response(
            &mut cache,
            0,
            200,
            &h(&[(ACAO, "*"), ("access-control-allow-headers", "*")]),
        )
        .unwrap_err();
    assert!(is_cors(
        &e,
        &CorsError::HeaderNotAllowed {
            name: "authorization".into(),
            wildcard_ignored: true
        }
    ));
}

#[test]
fn preflight_state_misuse() {
    let p = page("https://app.test/");
    let mut cache = PreflightCache::new(4, 600);
    // no preflight pending
    let mut ex = net(&p, init("https://api.test/x"));
    ex.next_hop(&mut cache, 0).unwrap();
    assert!(matches!(
        ex.on_preflight_response(&mut cache, 0, 200, &[]),
        Err(NetError::OutOfOrder(_))
    ));
    // before next_hop
    let mut ex = net(&p, put_request());
    assert!(matches!(
        ex.on_preflight_response(&mut cache, 0, 200, &ok_preflight()),
        Err(NetError::OutOfOrder(_))
    ));
    assert!(matches!(
        ex.on_response(200, "OK", &[]),
        Err(NetError::OutOfOrder(_))
    ));
    // next_hop twice
    ex.next_hop(&mut cache, 0).unwrap();
    assert!(matches!(
        ex.next_hop(&mut cache, 0),
        Err(NetError::OutOfOrder(_))
    ));
    // preflight twice
    ex.on_preflight_response(&mut cache, 0, 200, &ok_preflight())
        .unwrap();
    assert!(matches!(
        ex.on_preflight_response(&mut cache, 0, 200, &ok_preflight()),
        Err(NetError::OutOfOrder(_))
    ));
}

// ---------------------------------------------------------------------------------------
// no-cors, same-origin mode

#[test]
fn no_cors_cross_origin_is_opaque() {
    let p = page("https://app.test/");
    let mut i = init("https://other.test/pixel");
    i.mode = Mode::NoCors;
    i = with_headers(
        i,
        &[
            ("Content-Type", "application/json"),
            ("X-Custom", "1"),
            ("Accept", "image/*"),
        ],
    );
    let mut ex = net(&p, i);
    let hop = go(&mut ex);
    assert!(hop.preflight.is_none());
    assert_eq!(
        hdr(&hop, "content-type"),
        None,
        "unsafe headers are dropped"
    );
    assert_eq!(hdr(&hop, "x-custom"), None);
    assert_eq!(hdr(&hop, "accept"), Some("image/*"));
    assert_eq!(
        hdr(&hop, "origin"),
        None,
        "a GET without CORS carries no Origin"
    );
    assert_eq!(hdr(&hop, "sec-fetch-mode"), Some("no-cors"));
    let out = ex
        .on_response(
            200,
            "OK",
            &h(&[
                ("content-type", "image/gif"),
                ("secret", "x"),
                ("set-cookie", "a=b"),
            ]),
        )
        .unwrap();
    let m = done(out);
    assert_eq!(m.kind, ResponseType::Opaque);
    assert_eq!(
        (m.status, m.status_text.as_str(), m.url.as_str()),
        (0, "", "")
    );
    assert!(m.headers.is_empty() && m.hide_body && !m.ok());
}

#[test]
fn no_cors_hides_even_errors_and_ignores_missing_acao() {
    let p = page("https://app.test/");
    for status in [200u16, 404, 500] {
        let mut i = init("https://other.test/x");
        i.mode = Mode::NoCors;
        let mut ex = net(&p, i);
        go(&mut ex);
        let m = done(ex.on_response(status, "x", &[]).unwrap());
        assert_eq!(
            (m.kind, m.status, m.hide_body),
            (ResponseType::Opaque, 0, true)
        );
    }
}

#[test]
fn no_cors_same_origin_is_readable() {
    let p = page("https://app.test/");
    let mut i = init("/x");
    i.mode = Mode::NoCors;
    let mut ex = net(&p, i);
    go(&mut ex);
    let m = done(ex.on_response(200, "OK", &h(&[("x-a", "1")])).unwrap());
    assert_eq!(m.kind, ResponseType::Basic);
    assert_eq!(m.status, 200);
    assert_eq!(m.header("x-a").as_deref(), Some("1"));
}

#[test]
fn no_cors_methods() {
    let p = page("https://app.test/");
    for (m, ok) in [
        ("GET", true),
        ("HEAD", true),
        ("POST", true),
        ("PUT", false),
        ("DELETE", false),
        ("PATCH", false),
        ("patch", false),
        ("OPTIONS", false),
    ] {
        let mut i = with_method(init("https://other.test/x"), m);
        i.mode = Mode::NoCors;
        let r = Exchange::start(&cfg(), &p, i);
        assert_eq!(r.is_ok(), ok, "{m}");
        if !ok {
            assert!(matches!(r, Err(NetError::NoCorsMethod(_))));
        }
    }
}

#[test]
fn no_cors_post_origin_header_depends_on_policy() {
    // (page, target, policy, Origin sent?)
    let rows = [
        (
            "https://app.test/",
            "https://other.test/p",
            ReferrerPolicy::StrictOriginWhenCrossOrigin,
            true,
        ),
        (
            "https://app.test/",
            "https://other.test/p",
            ReferrerPolicy::NoReferrer,
            false,
        ),
        (
            "https://app.test/",
            "https://other.test/p",
            ReferrerPolicy::SameOrigin,
            false,
        ),
        (
            "https://app.test/",
            "https://app.test/p",
            ReferrerPolicy::SameOrigin,
            true,
        ),
        (
            "https://app.test/",
            "https://other.test/p",
            ReferrerPolicy::Origin,
            true,
        ),
        (
            "https://app.test/",
            "https://other.test/p",
            ReferrerPolicy::UnsafeUrl,
            true,
        ),
        (
            "https://app.test/",
            "https://other.test/p",
            ReferrerPolicy::StrictOrigin,
            true,
        ),
        (
            "https://app.test/",
            "https://other.test/p",
            ReferrerPolicy::NoReferrerWhenDowngrade,
            true,
        ),
        (
            "http://app.test/",
            "http://other.test/p",
            ReferrerPolicy::StrictOriginWhenCrossOrigin,
            true,
        ),
        (
            "http://app.test/",
            "http://other.test/p",
            ReferrerPolicy::NoReferrer,
            false,
        ),
    ];
    for (pg, url, policy, sent) in rows {
        let p = page(pg);
        let mut i = with_method(init(url), "POST");
        i.mode = Mode::NoCors;
        i.referrer_policy = Some(policy);
        i.body = body(1, Some("text/plain"));
        let mut ex = net(&p, i);
        let hop = go(&mut ex);
        assert_eq!(
            hdr(&hop, "origin").is_some(),
            sent,
            "{pg} -> {url} {}",
            policy.as_str()
        );
    }
}

#[test]
fn same_origin_mode() {
    let p = page("https://app.test/");
    let mut i = init("https://api.test/x");
    i.mode = Mode::SameOrigin;
    assert!(matches!(start_err(&p, i), NetError::SameOriginOnly { .. }));
    let mut i = init("https://app.test:444/x");
    i.mode = Mode::SameOrigin;
    assert!(matches!(start_err(&p, i), NetError::SameOriginOnly { .. }));
    let mut i = init("/x");
    i.mode = Mode::SameOrigin;
    let mut ex = net(&p, i);
    assert_eq!(hdr(&go(&mut ex), "sec-fetch-mode"), Some("same-origin"));
    // data: is allowed in this mode (it carries its own content)
    let mut i = init("data:text/plain,hi");
    i.mode = Mode::SameOrigin;
    assert!(matches!(
        Exchange::start(&cfg(), &p, i),
        Ok(Begin::Data(..))
    ));
    // a redirect to another origin is refused
    let mut i = init("/x");
    i.mode = Mode::SameOrigin;
    let mut ex = net(&p, i);
    go(&mut ex);
    let e = ex
        .on_response(302, "Found", &h(&[("location", "https://api.test/y")]))
        .unwrap_err();
    assert!(matches!(e, NetError::SameOriginOnly { .. }), "{e:?}");
    // ... but a same-origin redirect is fine
    let mut i = init("/x");
    i.mode = Mode::SameOrigin;
    let mut ex = net(&p, i);
    go(&mut ex);
    assert_eq!(
        redirected_to(
            &ex.on_response(302, "Found", &h(&[("location", "/y")]))
                .unwrap()
        ),
        "https://app.test/y"
    );
}

// ---------------------------------------------------------------------------------------
// Where requests may go

#[test]
fn mixed_content() {
    let https = page("https://app.test/");
    let e = start_err(&https, init("http://api.test/x"));
    assert!(
        matches!(&e, NetError::MixedContent { page, url } if page == "https://app.test/" && url == "http://api.test/x"),
        "{e:?}"
    );
    assert!(e.to_string().starts_with("Mixed content"));
    assert_eq!(e.script_message(), "Failed to fetch");
    for ok in [
        "https://api.test/x",
        "/x",
        "//api.test/x",
        "data:text/plain,x",
    ] {
        assert!(Exchange::start(&cfg(), &https, init(ok)).is_ok(), "{ok}");
    }
    // no-cors and same-origin modes don't change it
    for mode in [Mode::NoCors, Mode::Cors] {
        let mut i = init("http://api.test/x");
        i.mode = mode;
        assert!(matches!(
            start_err(&https, i),
            NetError::MixedContent { .. }
        ));
    }
    // an http page may ask for http or https
    let http = page("http://app.test/");
    for ok in ["http://api.test/x", "https://api.test/x", "/x"] {
        assert!(Exchange::start(&cfg(), &http, init(ok)).is_ok(), "{ok}");
    }
    // this machine is exempt from the mixed-content rule (a loopback page may use it)
    let local_https = page("https://localhost:8443/");
    assert!(Exchange::start(&cfg(), &local_https, init("http://localhost:8080/x")).is_ok());
    assert!(Exchange::start(&cfg(), &local_https, init("http://127.0.0.1:8080/x")).is_ok());
    assert!(Exchange::start(&cfg(), &local_https, init("http://[::1]:8080/x")).is_ok());
    // ... but a public https page still can't reach it (private network rule)
    assert!(matches!(
        start_err(&https, init("http://localhost:8080/x")),
        NetError::PrivateNetwork { .. }
    ));
}

#[test]
fn private_network_rule() {
    let rows = [
        // (page, target, allowed)
        ("https://app.test/", "https://api.test/", true),
        ("https://app.test/", "https://127.0.0.1/", false),
        ("https://app.test/", "https://10.0.2.2/", false),
        ("https://app.test/", "https://192.168.1.1/", false),
        ("https://app.test/", "https://[::1]/", false),
        ("https://app.test/", "https://[fe80::1]/", false),
        ("https://app.test/", "https://[::ffff:10.0.0.1]/", false),
        ("https://app.test/", "https://localhost/", false),
        ("https://app.test/", "https://x.localhost/", false),
        ("https://app.test/", "https://printer/", false),
        ("https://app.test/", "https://nas.local/", false),
        ("https://app.test/", "https://8.8.8.8/", true),
        ("https://app.test/", "https://172.32.0.1/", true),
        ("http://192.168.1.5/", "http://192.168.1.6/x", true),
        ("http://192.168.1.5/", "http://10.0.0.1/x", true),
        ("http://192.168.1.5/", "https://example.com/", true),
        ("http://192.168.1.5/", "http://127.0.0.1/", false),
        ("http://192.168.1.5/", "http://localhost:3000/", false),
        ("http://localhost:3000/", "http://localhost:4000/", true),
        ("http://localhost:3000/", "http://127.0.0.1:4000/", true),
        ("http://localhost:3000/", "http://10.0.0.1/", true),
        ("http://localhost:3000/", "https://example.com/", true),
        ("http://printer/", "http://router/", true),
        ("http://printer/", "http://localhost/", false),
    ];
    for (pg, target, ok) in rows {
        let r = Exchange::start(&cfg(), &page(pg), init(target));
        assert_eq!(r.is_ok(), ok, "{pg} -> {target}: {r:?}");
        if !ok {
            assert!(
                matches!(
                    r,
                    Err(NetError::PrivateNetwork { .. }) | Err(NetError::MixedContent { .. })
                ),
                "{pg} -> {target}"
            );
        }
    }
    // switched off for test setups
    let off = Config {
        block_private_network: false,
        ..cfg()
    };
    assert!(
        Exchange::start(
            &off,
            &page("http://app.test/"),
            init("http://10.0.2.2:8080/x")
        )
        .is_ok()
    );
    // the error says what happened
    let e = start_err(&page("http://app.test/"), init("http://10.0.2.2:8080/x"));
    assert!(e.to_string().contains("private") && e.to_string().contains("10.0.2.2"));
    assert_eq!(e.script_message(), "Failed to fetch");
}

#[test]
fn private_network_is_checked_on_redirects() {
    let p = page("http://app.test/");
    let mut ex = net(&p, init("http://api.test/x"));
    go(&mut ex);
    let e = ex
        .on_response(
            302,
            "Found",
            &h(&[("location", "http://10.0.0.1/admin"), (ACAO, "*")]),
        )
        .unwrap_err();
    assert!(matches!(e, NetError::PrivateNetwork { .. }), "{e:?}");
    let mut ex = net(&p, init("http://api.test/x"));
    go(&mut ex);
    let e = ex
        .on_response(
            302,
            "Found",
            &h(&[("location", "http://localhost/admin"), (ACAO, "*")]),
        )
        .unwrap_err();
    assert!(matches!(e, NetError::PrivateNetwork { .. }), "{e:?}");
}

#[test]
fn blocked_ports() {
    let p = page("http://app.test/");
    for port in [22, 25, 110, 143, 465, 587, 993, 6667] {
        let e = start_err(&p, init(&format!("http://api.test:{port}/x")));
        assert!(
            matches!(e, NetError::BlockedPort { port: q, .. } if q == port),
            "{port}"
        );
        assert_eq!(e.script_message(), "Failed to fetch");
    }
    assert!(Exchange::start(&cfg(), &p, init("http://api.test:8080/x")).is_ok());
    // also as a redirect target
    let mut ex = net(&p, init("/x"));
    go(&mut ex);
    let e = ex
        .on_response(302, "Found", &h(&[("location", "http://app.test:25/")]))
        .unwrap_err();
    assert!(matches!(e, NetError::BlockedPort { port: 25, .. }), "{e:?}");
}

#[test]
fn schemes() {
    let p = page("https://app.test/");
    for (bad, scheme) in [
        ("file:///etc/passwd", "file"),
        ("javascript:alert(1)", "javascript"),
        ("ftp://x.test/", "ftp"),
        ("ws://x.test/", "ws"),
        ("about:blank", "about"),
    ] {
        assert_eq!(
            start_err(&p, init(bad)),
            NetError::UnsupportedScheme(scheme.into())
        );
    }
    assert!(matches!(
        start_err(&p, init("https://user:pw\u{40}api.test/")),
        NetError::UrlHasCredentials
    ));
    assert!(matches!(
        start_err(&p, init("https://api.test\\x")),
        NetError::BadUrl(_)
    ));
}

#[test]
fn data_urls_are_answered_locally() {
    let p = page("https://app.test/");
    match Exchange::start(&cfg(), &p, init("data:application/json,%7B%22a%22%3A1%7D")).unwrap() {
        Begin::Data(meta, bytes) => {
            assert_eq!(bytes, br#"{"a":1}"#);
            assert_eq!(
                (meta.kind, meta.status, meta.status_text.as_str()),
                (ResponseType::Basic, 200, "OK")
            );
            assert_eq!(
                meta.header("content-type").as_deref(),
                Some("application/json")
            );
            assert!(!meta.hide_body && meta.url.starts_with("data:"));
        }
        other => panic!("{other:?}"),
    }
    // no-cors and cors both give a readable response
    for mode in [Mode::Cors, Mode::NoCors] {
        let mut i = init("data:,x");
        i.mode = mode;
        match Exchange::start(&cfg(), &p, i).unwrap() {
            Begin::Data(meta, _) => assert_eq!(meta.kind, ResponseType::Basic),
            other => panic!("{other:?}"),
        }
    }
    // GET only
    let e = start_err(&p, with_method(init("data:,x"), "POST"));
    assert!(matches!(e, NetError::SchemeMethod { .. }));
    // too big
    let small = Config {
        limits: Limits {
            max_response_body: 10,
            ..Limits::default()
        },
        ..cfg()
    };
    assert!(matches!(
        Exchange::start(&small, &p, init("data:,0123456789abc")),
        Err(NetError::ResponseTooLarge { max: 10 })
    ));
    // never from a redirect
    let mut ex = net(&p, init("/x"));
    go(&mut ex);
    let e = ex
        .on_response(302, "Found", &h(&[("location", "data:text/plain,hi")]))
        .unwrap_err();
    assert!(
        matches!(&e, NetError::RedirectScheme { scheme, .. } if scheme == "data"),
        "{e:?}"
    );
}

#[test]
fn blob_urls_must_be_the_pages_own() {
    let p = page("https://app.test/");
    match Exchange::start(&cfg(), &p, init("blob:https://app.test/1111-2222")).unwrap() {
        Begin::Blob { url } => assert_eq!(url, "blob:https://app.test/1111-2222"),
        other => panic!("{other:?}"),
    }
    for bad in [
        "blob:https://other.test/1111",
        "blob:http://app.test/1111",
        "blob:null/1111",
        "blob:",
        "blob:app.test/x",
        "blob:https://app.test:8443/1",
    ] {
        assert!(Exchange::start(&cfg(), &p, init(bad)).is_err(), "{bad}");
    }
    assert!(matches!(
        start_err(&p, with_method(init("blob:https://app.test/1"), "POST")),
        NetError::SchemeMethod { .. }
    ));
    let mut ex = net(&p, init("/x"));
    go(&mut ex);
    let e = ex
        .on_response(302, "Found", &h(&[("location", "blob:https://app.test/1")]))
        .unwrap_err();
    assert!(
        matches!(&e, NetError::RedirectScheme { scheme, .. } if scheme == "blob"),
        "{e:?}"
    );
}

// ---------------------------------------------------------------------------------------
// Validating the request

#[test]
fn start_validation() {
    let p = page("https://app.test/");
    let mut i = init("/x");
    i.mode = Mode::Navigate;
    assert_eq!(start_err(&p, i), NetError::NavigateMode);
    let mut i = init("https://api.test/x");
    i.cache = CacheMode::OnlyIfCached;
    assert_eq!(start_err(&p, i), NetError::CacheNeedsSameOrigin);
    let mut i = init("/x");
    i.cache = CacheMode::OnlyIfCached;
    i.mode = Mode::SameOrigin;
    assert!(matches!(start_err(&p, i), NetError::NotCached { .. }));
    let mut i = init("/x");
    i.body = body(1, None);
    assert!(matches!(start_err(&p, i), NetError::BodyNotAllowed(m) if m == "GET"));
    let mut i = with_method(init("/x"), "HEAD");
    i.body = body(1, None);
    assert!(matches!(start_err(&p, i), NetError::BodyNotAllowed(m) if m == "HEAD"));
    let mut i = with_method(init("/x"), "POST");
    i.body = body(cfg().limits.max_request_body + 1, None);
    assert!(matches!(
        start_err(&p, i),
        NetError::RequestBodyTooLarge { .. }
    ));
    let mut i = with_method(init("/x"), "POST");
    i.body = body(cfg().limits.max_request_body, None);
    assert!(Exchange::start(&cfg(), &p, i).is_ok());
    let mut i = with_method(init("/x"), "POST");
    i.keepalive = true;
    i.body = body(65537, None);
    assert!(matches!(
        start_err(&p, i),
        NetError::KeepaliveBodyTooLarge { .. }
    ));
    let mut i = with_method(init("/x"), "POST");
    i.keepalive = true;
    i.body = body(65536, None);
    assert!(Exchange::start(&cfg(), &p, i).is_ok());
    // forbidden methods, however they got in
    for m in ["CONNECT", "TRACE", "track"] {
        let mut i = init("/x");
        i.method = Method::parse(m).unwrap();
        assert!(
            matches!(start_err(&p, i), NetError::ForbiddenMethod(_)),
            "{m}"
        );
    }
    // a bad header in a list the caller didn't guard
    let mut bad = Headers::new(Guard::None);
    bad.append("X-A", "1").unwrap();
    let mut i = init("/x");
    i.headers = bad;
    assert!(Exchange::start(&cfg(), &p, i).is_ok());
}

#[test]
fn header_limits_are_checked_against_the_config() {
    let p = page("https://app.test/");
    let small = Config {
        limits: Limits {
            max_headers: 2,
            max_headers_bytes: 200,
            max_header_value: 20,
            ..Limits::default()
        },
        ..cfg()
    };
    let three = with_headers(init("/x"), &[("a", "1"), ("b", "2"), ("c", "3")]);
    assert!(matches!(
        Exchange::start(&small, &p, three),
        Err(NetError::TooManyHeaders { max: 2 })
    ));
    let long = with_headers(init("/x"), &[("a", &"v".repeat(21))]);
    assert!(matches!(
        Exchange::start(&small, &p, long),
        Err(NetError::HeadersTooLarge { .. })
    ));
    let fat = with_headers(
        init("/x"),
        &[("a", &"v".repeat(20)), ("b", &"v".repeat(20)), ("c", "")],
    );
    assert!(Exchange::start(&small, &p, fat.clone()).is_err());
}

#[test]
fn referrer_options() {
    let p = page("https://app.test/dir/page?x=1");
    let referer = |i: RequestInit| {
        let mut ex = net(&p, i);
        go(&mut ex)
            .headers
            .iter()
            .find(|(k, _)| k == "referer")
            .map(|(_, v)| v.clone())
    };
    let mut i = init("https://api.test/x");
    assert_eq!(referer(i.clone()).as_deref(), Some("https://app.test/"));
    i.referrer = ReferrerInit::NoReferrer;
    assert_eq!(referer(i.clone()), None);
    i.referrer = ReferrerInit::Url("/other/place?z=2".into());
    assert_eq!(referer(i.clone()).as_deref(), Some("https://app.test/"));
    i.url = "/x".into();
    assert_eq!(
        referer(i.clone()).as_deref(),
        Some("https://app.test/other/place?z=2")
    );
    i.referrer_policy = Some(ReferrerPolicy::NoReferrer);
    assert_eq!(referer(i.clone()), None);
    i.referrer_policy = Some(ReferrerPolicy::UnsafeUrl);
    i.url = "https://api.test/x".into();
    assert_eq!(
        referer(i.clone()).as_deref(),
        Some("https://app.test/other/place?z=2")
    );
    // another origin's address as referrer: refused
    for bad in [
        "https://evil.test/",
        "http://app.test/",
        "https://app.test:444/",
        "javascript:1",
        "data:,x",
        "file:///x",
    ] {
        let mut i = init("/x");
        i.referrer = ReferrerInit::Url(bad.into());
        assert!(
            matches!(start_err(&p, i), NetError::BadReferrer(_)),
            "{bad}"
        );
    }
    // the page's policy applies, unless the request has its own
    let mut pol = page("https://app.test/dir/page?x=1");
    pol.referrer_policy = ReferrerPolicy::NoReferrer;
    let mut ex = net(&pol, init("/x"));
    assert!(go(&mut ex).headers.iter().all(|(k, _)| k != "referer"));
    let mut i = init("/x");
    i.referrer_policy = Some(ReferrerPolicy::Origin);
    let mut ex = net(&pol, i);
    assert_eq!(hdr(&go(&mut ex), "referer"), Some("https://app.test/"));
}

#[test]
fn referrer_downgrade_is_never_sent() {
    // https page -> http target is blocked outright (mixed content), so the interesting case
    // is localhost, which counts as secure: no downgrade there.
    let p = page("https://localhost:8443/p");
    let mut ex = net(&p, init("http://localhost:8080/x"));
    assert_eq!(
        hdr(&go(&mut ex), "referer"),
        Some("https://localhost:8443/")
    );
}

#[test]
fn cache_modes_set_request_headers() {
    let p = page("https://app.test/");
    let cases = [
        (CacheMode::Default, None, None),
        (CacheMode::ForceCache, None, None),
        (CacheMode::NoStore, Some("no-cache"), Some("no-cache")),
        (CacheMode::Reload, Some("no-cache"), Some("no-cache")),
        (CacheMode::NoCache, None, Some("max-age=0")),
    ];
    for (mode, pragma, cc) in cases {
        let mut i = init("/x");
        i.cache = mode;
        let mut ex = net(&p, i);
        let hop = go(&mut ex);
        assert_eq!(hdr(&hop, "pragma"), pragma, "{mode:?}");
        assert_eq!(hdr(&hop, "cache-control"), cc, "{mode:?}");
        assert!(hop.preflight.is_none());
    }
    // the page's own Cache-Control is kept, and these headers never cause a preflight
    let mut i = with_headers(init("https://api.test/x"), &[]);
    i.cache = CacheMode::NoStore;
    let mut ex = net(&p, i);
    let hop = go(&mut ex);
    assert!(hop.preflight.is_none());
    assert_eq!(hdr(&hop, "cache-control"), Some("no-cache"));
}

#[test]
fn timeouts() {
    let p = page("https://app.test/");
    let mut i = init("/x");
    let mut ex = net(&p, i.clone());
    assert_eq!(ex.timeout_ms(), 30_000);
    assert_eq!(go(&mut ex).timeout_ms, 30_000);
    i.timeout_ms = Some(1500);
    assert_eq!(net(&p, i.clone()).timeout_ms(), 1500);
    i.timeout_ms = Some(10_000_000);
    assert_eq!(net(&p, i.clone()).timeout_ms(), 30_000);
    i.timeout_ms = Some(0);
    assert_eq!(net(&p, i).timeout_ms(), 30_000);
}

#[test]
fn opaque_page_origin() {
    let mut p = page("https://app.test/");
    p.origin = Origin::Opaque(9);
    let mut i = init("https://api.test/x");
    i.credentials = Credentials::Include;
    let mut ex = net(&p, i);
    let hop = go(&mut ex);
    assert_eq!(hdr(&hop, "origin"), Some("null"));
    assert_eq!(
        hop.cookies, None,
        "an opaque origin has no site, so no cookies"
    );
    assert!(
        ex.on_response(
            200,
            "OK",
            &h(&[
                (ACAO, "https://app.test"),
                ("access-control-allow-credentials", "true")
            ])
        )
        .is_err()
    );
    let mut ex = net(&p, init("https://api.test/x"));
    go(&mut ex);
    assert!(ex.on_response(200, "OK", &h(&[(ACAO, "null")])).is_ok());
    // even the page's own address is cross-origin to an opaque origin
    let mut ex = net(&p, init("https://app.test/x"));
    let hop = go(&mut ex);
    assert_eq!(hdr(&hop, "sec-fetch-site"), Some("cross-site"));
    assert_eq!(hdr(&hop, "origin"), Some("null"));
}

// ---------------------------------------------------------------------------------------
// Responses

#[test]
fn response_size_and_status_checks() {
    let p = page("https://app.test/");
    let small = Config {
        limits: Limits {
            max_response_body: 1000,
            ..Limits::default()
        },
        ..cfg()
    };
    let mk = |len: &str| {
        let mut ex = match Exchange::start(&small, &p, init("/x")).unwrap() {
            Begin::Network(e) => e,
            _ => unreachable!(),
        };
        go(&mut ex);
        ex.on_response(200, "OK", &h(&[("content-length", len)]))
    };
    assert!(mk("1000").is_ok());
    assert!(matches!(
        mk("1001"),
        Err(NetError::ResponseTooLarge { max: 1000 })
    ));
    assert!(
        mk("99999999999999999999999999").is_ok(),
        "a length we can't parse is left to the transport's own cap"
    );
    assert!(mk("abc").is_ok());
    for status in [0u16, 99, 100, 101, 199, 600, 999, 65535] {
        let mut ex = net(&p, init("/x"));
        go(&mut ex);
        assert!(
            matches!(
                ex.on_response(status, "x", &[]),
                Err(NetError::BadResponse(_))
            ),
            "{status}"
        );
    }
    for status in [200u16, 204, 206, 304, 400, 404, 418, 500, 503, 599] {
        let mut ex = net(&p, init("/x"));
        go(&mut ex);
        assert_eq!(
            done(ex.on_response(status, "x", &[]).unwrap()).status,
            status
        );
    }
}

#[test]
fn status_text_is_cleaned() {
    let p = page("https://app.test/");
    let mut ex = net(&p, init("/x"));
    go(&mut ex);
    let m = done(ex.on_response(200, "O\u{0}K\r\n\u{e9}!", &[]).unwrap());
    assert_eq!(m.status_text, "OK!");
    let mut ex = net(&p, init("/x"));
    go(&mut ex);
    let m = done(ex.on_response(200, &"x".repeat(1000), &[]).unwrap());
    assert_eq!(m.status_text.len(), 128);
}

#[test]
fn cors_exposed_headers_in_full_exchange() {
    let p = page("https://app.test/");
    let mut ex = net(&p, init("https://api.test/x"));
    go(&mut ex);
    let m = done(
        ex.on_response(
            200,
            "OK",
            &h(&[
                (ACAO, "*"),
                ("x-total", "9"),
                ("x-hidden", "1"),
                ("set-cookie", "a=b"),
                ("access-control-expose-headers", "X-Total, Set-Cookie"),
            ]),
        )
        .unwrap(),
    );
    assert_eq!(m.header("x-total").as_deref(), Some("9"));
    assert!(m.header("x-hidden").is_none() && m.header("set-cookie").is_none());
}

#[test]
fn corp_blocks_no_cors_reads() {
    let p = page("https://app.test/");
    let mk = |v: &str| {
        let mut i = init("https://other.test/x");
        i.mode = Mode::NoCors;
        let mut ex = net(&p, i);
        go(&mut ex);
        ex.on_response(200, "OK", &h(&[("cross-origin-resource-policy", v)]))
    };
    assert!(matches!(
        mk("same-origin"),
        Err(NetError::BlockedByCorp { .. })
    ));
    assert!(matches!(
        mk("same-site"),
        Err(NetError::BlockedByCorp { .. })
    ));
    assert!(mk("cross-origin").is_ok());
    assert!(mk("").is_ok());
}

// ---------------------------------------------------------------------------------------
// Redirects

fn redirecting(
    p: &Page,
    i: RequestInit,
    status: u16,
    location: &str,
    response: &[(&str, &str)],
) -> (Exchange, Result<Outcome, NetError>) {
    let mut ex = net(p, i);
    go(&mut ex);
    let mut hdrs = h(response);
    hdrs.push(("location".into(), location.into()));
    let r = ex.on_response(status, "Redirect", &hdrs);
    (ex, r)
}

#[test]
fn redirect_methods_and_bodies() {
    let p = page("https://app.test/");
    // (status, method, expected method after, body kept)
    let rows = [
        (301u16, "GET", "GET", false),
        (301, "POST", "GET", false),
        (302, "POST", "GET", false),
        (303, "POST", "GET", false),
        (303, "PUT", "GET", false),
        (303, "DELETE", "GET", false),
        (303, "PATCH", "GET", false),
        (303, "GET", "GET", false),
        (303, "HEAD", "HEAD", false),
        (301, "PUT", "PUT", true),
        (302, "DELETE", "DELETE", true),
        (302, "PATCH", "PATCH", true),
        (307, "POST", "POST", true),
        (307, "PUT", "PUT", true),
        (308, "POST", "POST", true),
        (308, "DELETE", "DELETE", true),
    ];
    for (status, method, after, kept) in rows {
        let mut i = with_headers(
            with_method(init("/start"), method),
            &[("Content-Type", "application/json"), ("X-A", "1")],
        );
        if method != "GET" && method != "HEAD" {
            i.body = body(5, None);
        }
        let (mut ex, r) = redirecting(&p, i, status, "/next", &[]);
        assert_eq!(
            redirected_to(&r.unwrap()),
            "https://app.test/next",
            "{status} {method}"
        );
        let hop = go(&mut ex);
        assert_eq!(hop.method.as_str(), after, "{status} {method}");
        let had_body = method != "GET" && method != "HEAD";
        assert_eq!(hop.send_body, had_body && kept, "{status} {method}");
        assert_eq!(
            hdr(&hop, "content-type").is_some(),
            kept || method == "GET" || method == "HEAD",
            "{status} {method}"
        );
        assert_eq!(hdr(&hop, "x-a"), Some("1"));
        assert_eq!(ex.redirect_count(), 1);
    }
}

#[test]
fn redirect_without_location_is_the_response() {
    let p = page("https://app.test/");
    let mut ex = net(&p, init("/x"));
    go(&mut ex);
    let m = done(ex.on_response(302, "Found", &h(&[("x-a", "1")])).unwrap());
    assert_eq!(m.status, 302);
    assert!(!m.redirected);
    // 300, 304 and 305 are not redirects we follow
    for status in [300u16, 304, 305, 306] {
        let mut ex = net(&p, init("/x"));
        go(&mut ex);
        let m = done(
            ex.on_response(status, "x", &h(&[("location", "/y")]))
                .unwrap(),
        );
        assert_eq!(m.status, status);
    }
}

#[test]
fn redirect_follow_then_final() {
    let p = page("https://app.test/a/");
    let (mut ex, r) = redirecting(&p, init("one"), 302, "two?x=1", &[("set-cookie", "s=1")]);
    let r = r.unwrap();
    assert_eq!(
        r.store_cookies,
        Some(Context::Subresource {
            page_site: "app.test".into()
        }),
        "a redirect's Set-Cookie counts"
    );
    assert_eq!(redirected_to(&r), "https://app.test/a/two?x=1");
    let hop2 = go(&mut ex);
    assert_eq!(hop2.url.to_text(), "https://app.test/a/two?x=1");
    assert_eq!(
        hop2.cookies,
        Some(Context::Subresource {
            page_site: "app.test".into()
        })
    );
    let m = done(ex.on_response(200, "OK", &[]).unwrap());
    assert!(m.redirected);
    assert_eq!(m.url, "https://app.test/a/two?x=1");
    assert_eq!(ex.redirect_count(), 1);
    // an absolute Location, and a protocol-relative one
    let (_, r) = redirecting(&p, init("one"), 301, "https://app.test/abs", &[]);
    assert_eq!(redirected_to(&r.unwrap()), "https://app.test/abs");
    let (_, r) = redirecting(&p, init("one"), 301, "//app.test/rel", &[]);
    assert_eq!(redirected_to(&r.unwrap()), "https://app.test/rel");
}

#[test]
fn redirect_modes() {
    let p = page("https://app.test/");
    let mut i = init("/x");
    i.redirect = Redirect::Error;
    let (mut ex, r) = redirecting(&p, i, 302, "/y", &[]);
    assert!(matches!(r, Err(NetError::RedirectRefused { .. })));
    assert!(matches!(
        ex.next_hop(&mut PreflightCache::new(1, 1), 0),
        Err(NetError::OutOfOrder(_))
    ));
    // 'error' applies to a redirect status even with no Location
    let mut i = init("/x");
    i.redirect = Redirect::Error;
    let mut ex = net(&p, i);
    go(&mut ex);
    assert!(matches!(
        ex.on_response(302, "Found", &[]),
        Err(NetError::RedirectRefused { .. })
    ));
    // manual: an opaque redirect, whatever the Location says
    let mut i = init("/x");
    i.redirect = Redirect::Manual;
    let (_, r) = redirecting(&p, i, 307, "javascript:alert(1)", &[("set-cookie", "a=b")]);
    let r = r.unwrap();
    assert!(r.store_cookies.is_some());
    let m = done(r);
    assert_eq!(
        (m.kind, m.status, m.hide_body),
        (ResponseType::OpaqueRedirect, 0, true)
    );
    assert!(m.headers.is_empty() && m.status_text.is_empty());
    assert_eq!(m.url, "https://app.test/x");
    // manual on a cross-origin request still needs the CORS check first
    let mut i = init("https://api.test/x");
    i.redirect = Redirect::Manual;
    let (_, r) = redirecting(&p, i.clone(), 302, "https://api.test/y", &[]);
    assert!(r.is_err());
    let (_, r) = redirecting(&p, i, 302, "https://api.test/y", &[(ACAO, "*")]);
    assert_eq!(done(r.unwrap()).kind, ResponseType::OpaqueRedirect);
    // manual with a non-redirect response is just the response
    let mut i = init("/x");
    i.redirect = Redirect::Manual;
    let mut ex = net(&p, i);
    go(&mut ex);
    assert_eq!(
        done(ex.on_response(200, "OK", &[]).unwrap()).kind,
        ResponseType::Basic
    );
}

#[test]
fn redirect_limit() {
    let p = page("https://app.test/");
    for max in [0usize, 1, 3, 20] {
        let c = Config {
            limits: Limits {
                max_redirects: max,
                ..Limits::default()
            },
            ..cfg()
        };
        let mut ex = match Exchange::start(&c, &p, init("/loop")).unwrap() {
            Begin::Network(e) => e,
            _ => unreachable!(),
        };
        let mut followed = 0;
        loop {
            go(&mut ex);
            match ex.on_response(302, "Found", &h(&[("location", "/loop")])) {
                Ok(o) => {
                    assert!(matches!(o.next, Next::Redirect { .. }));
                    followed += 1;
                    assert!(followed <= max, "followed {followed} with a limit of {max}");
                }
                Err(e) => {
                    assert_eq!(e, NetError::TooManyRedirects { max });
                    break;
                }
            }
        }
        assert_eq!(followed, max);
    }
    // an empty Location is a redirect to the same address: bounded too
    let mut ex = net(&p, init("/loop"));
    let mut n = 0;
    loop {
        go(&mut ex);
        match ex.on_response(302, "Found", &h(&[("location", "")])) {
            Ok(_) => n += 1,
            Err(NetError::TooManyRedirects { .. }) => break,
            Err(e) => panic!("{e:?}"),
        }
        assert!(n <= 20);
    }
    assert_eq!(n, 20);
}

#[test]
#[allow(clippy::type_complexity)]
fn redirect_targets_that_are_refused() {
    let p = page("https://app.test/");
    let rows: [(&str, fn(&NetError) -> bool); 14] = [
        (
            "file:///etc/passwd",
            |e| matches!(e, NetError::RedirectScheme { scheme, .. } if scheme == "file"),
        ),
        (
            "javascript:alert(1)",
            |e| matches!(e, NetError::RedirectScheme { scheme, .. } if scheme == "javascript"),
        ),
        (
            "ftp://x.test/",
            |e| matches!(e, NetError::RedirectScheme { scheme, .. } if scheme == "ftp"),
        ),
        ("data:text/html,x", |e| {
            matches!(e, NetError::RedirectScheme { .. })
        }),
        ("https://user:pw\u{40}app.test/x", |e| {
            matches!(e, NetError::RedirectWithCredentials { .. })
        }),
        ("//user\u{40}app.test/x", |e| {
            matches!(e, NetError::RedirectWithCredentials { .. })
        }),
        ("http://app.test/x", |e| {
            matches!(e, NetError::MixedContent { .. })
        }),
        ("https://app.test:25/x", |e| {
            matches!(e, NetError::BlockedPort { port: 25, .. })
        }),
        ("http://", |e| matches!(e, NetError::BadRedirect { .. })),
        ("https://[::1/", |e| {
            matches!(e, NetError::BadRedirect { .. })
        }),
        ("https://a b.test/", |e| {
            matches!(e, NetError::BadRedirect { .. })
        }),
        ("https://app.test\\@evil.test/", |e| {
            matches!(e, NetError::BadRedirect { .. })
        }),
        ("https://127.1/", |e| {
            matches!(e, NetError::BadRedirect { .. })
        }),
        ("https://10.0.0.1/", |e| {
            matches!(e, NetError::PrivateNetwork { .. })
        }),
    ];
    for (loc, check) in rows {
        let (_, r) = redirecting(&p, init("/x"), 302, loc, &[]);
        let e = r.unwrap_err();
        assert!(check(&e), "{loc}: {e:?}");
        assert_eq!(e.script_message(), "Failed to fetch");
    }
}

#[test]
fn cross_origin_redirects_are_checked_at_every_hop() {
    let p = page("https://app.test/");
    // same-origin first hop, redirected to another origin: the target must allow us
    let (mut ex, r) = redirecting(&p, init("/x"), 302, "https://api.test/y", &[]);
    assert_eq!(redirected_to(&r.unwrap()), "https://api.test/y");
    let hop2 = go(&mut ex);
    assert_eq!(
        hdr(&hop2, "origin"),
        Some("https://app.test"),
        "CORS tainting: Origin is sent now"
    );
    assert_eq!(hdr(&hop2, "sec-fetch-site"), Some("cross-site"));
    assert_eq!(
        hop2.cookies, None,
        "credentials 'same-origin' stop at the origin border"
    );
    assert!(
        ex.on_response(200, "OK", &[]).is_err(),
        "no Access-Control-Allow-Origin on the final response"
    );
    let (mut ex, _) = redirecting(&p, init("/x"), 302, "https://api.test/y", &[]);
    go(&mut ex);
    let m = done(
        ex.on_response(200, "OK", &h(&[(ACAO, "https://app.test"), ("x-a", "1")]))
            .unwrap(),
    );
    assert_eq!(m.kind, ResponseType::Cors);
    assert!(m.header("x-a").is_none());
    assert!(m.redirected);
    // a cross-origin first hop whose redirect response lacks the header fails right there
    let (_, r) = redirecting(
        &p,
        init("https://api.test/x"),
        302,
        "https://api.test/y",
        &[],
    );
    assert!(is_cors(&r.unwrap_err(), &CorsError::MissingAllowOrigin));
    let (mut ex, r) = redirecting(
        &p,
        init("https://api.test/x"),
        302,
        "https://api.test/y",
        &[(ACAO, "https://app.test")],
    );
    assert!(r.is_ok());
    go(&mut ex);
    assert!(ex.on_response(200, "OK", &[]).is_err());
}

#[test]
fn origin_header_is_null_after_a_hop_through_another_origin() {
    let p = page("https://app.test/");
    // app -> api1 -> api2 : at api2 the Origin is null, so ACAO must say null (or *)
    let (mut ex, r) = redirecting(
        &p,
        init("https://api1.test/x"),
        302,
        "https://api2.test/y",
        &[(ACAO, "https://app.test")],
    );
    r.unwrap();
    let hop2 = go(&mut ex);
    assert_eq!(hdr(&hop2, "origin"), Some("null"));
    let e = ex
        .on_response(200, "OK", &h(&[(ACAO, "https://app.test")]))
        .unwrap_err();
    assert!(is_cors(
        &e,
        &CorsError::OriginMismatch {
            allowed: "https://app.test".into()
        }
    ));
    let (mut ex, _) = redirecting(
        &p,
        init("https://api1.test/x"),
        302,
        "https://api2.test/y",
        &[(ACAO, "https://app.test")],
    );
    go(&mut ex);
    assert!(ex.on_response(200, "OK", &h(&[(ACAO, "null")])).is_ok());
    let (mut ex, _) = redirecting(
        &p,
        init("https://api1.test/x"),
        302,
        "https://api2.test/y",
        &[(ACAO, "https://app.test")],
    );
    go(&mut ex);
    assert!(ex.on_response(200, "OK", &h(&[(ACAO, "*")])).is_ok());
    // app -> api -> app: back home. The origin is no secret from itself, so Origin stays, but
    // CORS still applies to the final response (the chain was cross-origin)
    let (mut ex, r) = redirecting(
        &p,
        init("https://api.test/x"),
        302,
        "https://app.test/home",
        &[(ACAO, "https://app.test")],
    );
    r.unwrap();
    let hop2 = go(&mut ex);
    assert_eq!(hdr(&hop2, "origin"), Some("https://app.test"));
    assert_eq!(
        hdr(&hop2, "sec-fetch-site"),
        Some("cross-site"),
        "the chain's worst class sticks"
    );
    assert!(ex.on_response(200, "OK", &[]).is_err());
    // app -> app -> api: leaving from the page's own origin doesn't blank the Origin
    let (mut ex, _) = redirecting(&p, init("/a"), 302, "/b", &[]);
    go(&mut ex);
    let o = ex
        .on_response(302, "Found", &h(&[("location", "https://api.test/c")]))
        .unwrap();
    assert!(matches!(o.next, Next::Redirect { .. }));
    assert_eq!(hdr(&go(&mut ex), "origin"), Some("https://app.test"));
}

#[test]
fn authorization_does_not_cross_origins_on_redirect() {
    let p = page("https://app.test/");
    let mk = || {
        with_headers(
            init("/x"),
            &[("Authorization", "Bearer t"), ("Accept-Language", "en")],
        )
    };
    let (mut ex, _) = redirecting(&p, mk(), 302, "/same", &[]);
    let hop2 = go(&mut ex);
    assert_eq!(hdr(&hop2, "authorization"), Some("Bearer t"));
    let (mut ex, _) = redirecting(&p, mk(), 302, "https://api.test/other", &[]);
    let hop2 = go(&mut ex);
    assert_eq!(hdr(&hop2, "authorization"), None);
    assert_eq!(hdr(&hop2, "accept-language"), Some("en"));
    assert!(
        hop2.preflight.is_none(),
        "without Authorization nothing needs a preflight"
    );
    // and it stays gone if the chain comes back
    let o = ex.on_response(
        302,
        "Found",
        &h(&[
            (ACAO, "https://app.test"),
            ("location", "https://app.test/back"),
        ]),
    );
    assert!(o.is_ok(), "{o:?}");
    assert_eq!(hdr(&go(&mut ex), "authorization"), None);
    // same for a POST redirected with 307
    let i = with_headers(with_method(init("/x"), "POST"), &[("Authorization", "t")]);
    let (mut ex, r) = redirecting(&p, i, 307, "https://api.test/y", &[]);
    r.unwrap();
    let hop2 = go(&mut ex);
    assert!(hop2.preflight.is_none());
    assert_eq!(hdr(&hop2, "authorization"), None);
}

#[test]
fn a_redirect_hop_is_preflighted_on_its_own() {
    let p = page("https://app.test/");
    let mut cache = PreflightCache::new(8, 600);
    let mut i = with_headers(with_method(init("/api"), "PUT"), &[("X-A", "1")]);
    i.body = body(2, None);
    let mut ex = net(&p, i);
    let first = ex.next_hop(&mut cache, 0).unwrap();
    assert!(first.preflight.is_none(), "same-origin: no preflight");
    let o = ex
        .on_response(
            307,
            "Temporary Redirect",
            &h(&[("location", "https://api.test/v2")]),
        )
        .unwrap();
    assert!(matches!(o.next, Next::Redirect { .. }));
    let second = ex.next_hop(&mut cache, 0).unwrap();
    assert_eq!(second.method.as_str(), "PUT");
    assert!(second.send_body);
    let pf = second.preflight.expect("now it's cross-origin");
    assert_eq!(pf.url.to_text(), "https://api.test/v2");
    let rq = pf
        .headers
        .iter()
        .find(|(k, _)| k == "access-control-request-headers")
        .unwrap();
    assert_eq!(rq.1, "x-a");
    ex.on_preflight_response(
        &mut cache,
        0,
        200,
        &h(&[
            (ACAO, "https://app.test"),
            ("access-control-allow-methods", "PUT"),
            ("access-control-allow-headers", "x-a"),
        ]),
    )
    .unwrap();
    assert!(
        ex.on_response(200, "OK", &h(&[(ACAO, "https://app.test")]))
            .is_ok()
    );
}

#[test]
fn credentials_modes_across_redirects() {
    let p = page("https://www.app.test/");
    // credentials 'same-origin': cookies on the first (same-origin) hop only
    let (mut ex, _) = redirecting(&p, init("/x"), 302, "https://api.app.test/y", &[]);
    assert_eq!(go(&mut ex).cookies, None);
    // 'include': kept along a same-site chain
    let mut i = init("/x");
    i.credentials = Credentials::Include;
    let (mut ex, _) = redirecting(&p, i.clone(), 302, "https://api.app.test/y", &[]);
    assert_eq!(
        go(&mut ex).cookies,
        Some(Context::Subresource {
            page_site: "app.test".into()
        })
    );
    // 'include' through another site and back: no cookies after the detour, and the
    // detour's own Set-Cookie is not stored
    let mut ex = net(&p, i);
    go(&mut ex);
    let o = ex
        .on_response(302, "Found", &h(&[("location", "https://other.test/y")]))
        .unwrap();
    assert!(o.store_cookies.is_some(), "the first hop was same-site");
    let hop2 = go(&mut ex);
    assert_eq!(hop2.cookies, None);
    let o = ex
        .on_response(
            302,
            "Found",
            &h(&[
                ("location", "https://www.app.test/back"),
                (ACAO, "https://www.app.test"),
                ("access-control-allow-credentials", "true"),
                ("set-cookie", "x=1"),
            ]),
        )
        .unwrap();
    assert_eq!(o.store_cookies, None);
    let hop3 = go(&mut ex);
    assert_eq!(hop3.cookies, None, "the chain has been cross-site");
    assert_eq!(hdr(&hop3, "sec-fetch-site"), Some("cross-site"));
    let o = ex.on_response(
        200,
        "OK",
        &h(&[(ACAO, "null"), ("access-control-allow-credentials", "true")]),
    );
    assert!(o.is_err() || o.unwrap().store_cookies.is_none());
}

#[test]
fn referrer_policy_header_on_a_redirect_applies_next() {
    let p = page("https://app.test/dir/page?q=1");
    let (mut ex, _) = redirecting(
        &p,
        init("https://api.test/x"),
        302,
        "https://api.test/y",
        &[(ACAO, "*"), ("referrer-policy", "no-referrer")],
    );
    let hop2 = go(&mut ex);
    assert_eq!(hdr(&hop2, "referer"), None);
    let (mut ex, _) = redirecting(
        &p,
        init("https://api.test/x"),
        302,
        "https://api.test/y",
        &[(ACAO, "*"), ("referrer-policy", "bogus")],
    );
    assert_eq!(hdr(&go(&mut ex), "referer"), Some("https://app.test/"));
    let (mut ex, _) = redirecting(
        &p,
        init("https://api.test/x"),
        302,
        "https://api.test/y",
        &[(ACAO, "*"), ("referrer-policy", "no-referrer, unsafe-url")],
    );
    assert_eq!(
        hdr(&go(&mut ex), "referer"),
        Some("https://app.test/dir/page?q=1")
    );
}

#[test]
fn sec_fetch_site_classes() {
    let rows = [
        (
            "https://www.app.test/",
            "https://www.app.test/x",
            "same-origin",
        ),
        (
            "https://www.app.test/",
            "https://api.app.test/x",
            "same-site",
        ),
        ("https://www.app.test/", "https://app.test/x", "same-site"),
        (
            "https://www.app.test/",
            "https://other.test/x",
            "cross-site",
        ),
        (
            "https://www.app.test/",
            "https://www.app.test:8443/x",
            "same-site",
        ),
        (
            "http://localhost:3000/",
            "http://localhost:4000/x",
            "same-site",
        ),
    ];
    for (pg, url, want) in rows {
        let mut ex = net(&page(pg), init(url));
        assert_eq!(
            hdr(&go(&mut ex), "sec-fetch-site"),
            Some(want),
            "{pg} -> {url}"
        );
    }
    // http targets that aren't trustworthy get no Sec-Fetch-* at all
    let mut ex = net(&page("http://app.test/"), init("http://api.test/x"));
    let hop = go(&mut ex);
    assert!(hop.headers.iter().all(|(k, _)| !k.starts_with("sec-")));
    // but https and localhost do
    let mut ex = net(
        &page("http://localhost:3000/"),
        init("http://localhost:4000/x"),
    );
    assert!(hdr(&go(&mut ex), "sec-fetch-mode").is_some());
}

#[test]
fn page_can_not_slip_a_second_origin_into_headers() {
    // Header values holding line breaks never get in: the Headers object refuses them, and an
    // unguarded one can't be built with them either.
    let mut hdrs = Headers::new(Guard::None);
    assert!(
        hdrs.append("X-A", "1\r\nOrigin: https://evil.test")
            .is_err()
    );
    assert!(hdrs.append("X-A\r\nOrigin", "https://evil.test").is_err());
}

#[test]
fn xhr_style_request() {
    // new XMLHttpRequest(); open('POST', url); withCredentials = true; setRequestHeader(...)
    let p = page("https://www.app.test/");
    let mut hdrs = Headers::new(Guard::Request);
    hdrs.append("X-Requested-With", "XMLHttpRequest").unwrap();
    hdrs.append(
        "Content-Type",
        "application/x-www-form-urlencoded; charset=UTF-8",
    )
    .unwrap();
    hdrs.append("Cookie", "no=way").unwrap();
    let mut i = with_method(init("https://api.app.test/rpc"), "POST");
    i.headers = hdrs;
    i.credentials = Credentials::Include;
    i.body = body(12, Some("text/plain;charset=UTF-8"));
    let mut cache = PreflightCache::new(8, 600);
    let mut ex = net(&p, i);
    let hop = ex.next_hop(&mut cache, 0).unwrap();
    let pf = hop
        .preflight
        .clone()
        .expect("X-Requested-With is not safelisted");
    assert!(pf.headers.contains(&(
        "access-control-request-headers".into(),
        "x-requested-with".into()
    )));
    assert_eq!(
        hdr(&hop, "content-type"),
        Some("application/x-www-form-urlencoded; charset=UTF-8")
    );
    assert_eq!(
        hop.cookies,
        Some(Context::Subresource {
            page_site: "app.test".into()
        })
    );
    ex.on_preflight_response(
        &mut cache,
        0,
        204,
        &h(&[
            (ACAO, "https://www.app.test"),
            ("access-control-allow-credentials", "true"),
            ("access-control-allow-headers", "x-requested-with"),
        ]),
    )
    .unwrap();
    let o = ex
        .on_response(
            200,
            "OK",
            &h(&[
                (ACAO, "https://www.app.test"),
                ("access-control-allow-credentials", "true"),
                ("set-cookie", "sid=1"),
                ("content-type", "application/json"),
            ]),
        )
        .unwrap();
    assert_eq!(
        o.store_cookies,
        Some(Context::Subresource {
            page_site: "app.test".into()
        })
    );
    let m = done(o);
    assert_eq!(
        m.header("content-type").as_deref(),
        Some("application/json")
    );
}

// ---------------------------------------------------------------------------------------
// Address spaces after name lookup, convenience constructors

#[test]
fn hop_checks_resolved_addresses() {
    // a public page: a name that resolves to something private is a rebinding attempt
    let p = page("https://app.test/");
    let mut ex = net(&p, init("https://api.test/x"));
    let hop = go(&mut ex);
    assert_eq!(hop.max_address_space, AddressSpace::Public);
    assert!(hop.check_resolved(&[93, 184, 216, 34]).is_ok());
    for bad in [
        [127u8, 0, 0, 1],
        [10, 0, 0, 1],
        [192, 168, 1, 1],
        [169, 254, 169, 254],
    ] {
        let e = hop.check_resolved(&bad).unwrap_err();
        assert!(matches!(e, NetError::PrivateNetwork { .. }), "{bad:?}");
    }
    assert!(
        hop.check_resolved(&[1, 2, 3]).is_err(),
        "an address we can't read is refused"
    );
    let mut v6 = [0u8; 16];
    v6[15] = 1;
    assert!(hop.check_resolved(&v6).is_err());
    // a page that is itself on the private network may reach it
    let mut intranet = page("http://wiki.example.test/");
    intranet.address_space = AddressSpace::Private;
    let mut ex = net(&intranet, init("/x"));
    let hop = go(&mut ex);
    assert_eq!(hop.max_address_space, AddressSpace::Private);
    assert!(hop.check_resolved(&[10, 1, 2, 3]).is_ok());
    assert!(hop.check_resolved(&[8, 8, 8, 8]).is_ok());
    assert!(hop.check_resolved(&[127, 0, 0, 1]).is_err());
    // with the rule off, anything goes
    let off = Config {
        block_private_network: false,
        ..cfg()
    };
    let mut ex = match Exchange::start(&off, &p, init("https://api.test/x")).unwrap() {
        Begin::Network(e) => e,
        _ => unreachable!(),
    };
    let hop = go(&mut ex);
    assert!(hop.check_resolved(&[127, 0, 0, 1]).is_ok());
    assert!(hop.page_origin == "https://app.test");
}

#[test]
fn page_address_space_decides_the_private_network_rule() {
    let mut intranet = page("http://wiki.example.test/");
    assert_eq!(
        intranet.address_space,
        AddressSpace::Public,
        "guessed from the name"
    );
    assert!(matches!(
        start_err(&intranet, init("http://10.0.0.5/x")),
        NetError::PrivateNetwork { .. }
    ));
    intranet.address_space = AddressSpace::Private;
    assert!(Exchange::start(&cfg(), &intranet, init("http://10.0.0.5/x")).is_ok());
    assert!(matches!(
        start_err(&intranet, init("http://localhost/x")),
        NetError::PrivateNetwork { .. }
    ));
    // and a page that came from a loopback address may reach anything
    let mut local = page("http://dev.example.test/");
    local.address_space = AddressSpace::Loopback;
    assert!(Exchange::start(&cfg(), &local, init("http://localhost:3009/x")).is_ok());
    // the guess for the common cases
    for (url, want) in [
        ("http://localhost:3000/", AddressSpace::Loopback),
        ("http://10.0.2.2:8080/", AddressSpace::Private),
        ("https://example.com/", AddressSpace::Public),
        ("http://printer/", AddressSpace::Private),
    ] {
        assert_eq!(page(url).address_space, want, "{url}");
    }
}

#[test]
fn beacon_request() {
    let p = page("https://app.test/");
    let i = RequestInit::beacon(
        "https://stats.test/b",
        100,
        Some("text/plain;charset=UTF-8"),
    );
    assert_eq!(i.method.as_str(), "POST");
    assert!(i.keepalive);
    assert_eq!(i.credentials, Credentials::Include);
    let mut ex = net(&p, i);
    let hop = go(&mut ex);
    assert!(hop.preflight.is_none(), "text/plain is simple");
    assert!(hop.send_body);
    assert_eq!(hdr(&hop, "origin"), Some("https://app.test"));
    // a typed Blob makes it preflighted
    let mut ex = net(
        &p,
        RequestInit::beacon("https://stats.test/b", 100, Some("application/json")),
    );
    assert!(go(&mut ex).preflight.is_some());
    // too big for keepalive
    assert!(matches!(
        start_err(
            &p,
            RequestInit::beacon("https://stats.test/b", 70_000, None)
        ),
        NetError::KeepaliveBodyTooLarge { .. }
    ));
}

#[test]
fn check_construct_reports_only_type_errors() {
    let p = page("https://app.test/");
    let c = cfg();
    assert!(check_construct(&c, &p, &init("/x")).is_ok());
    assert!(check_construct(&c, &p, &init("https://elsewhere.test/")).is_ok());
    // stopped later by network rules, but a valid Request
    assert!(check_construct(&c, &p, &init("http://plain.test/")).is_ok());
    assert!(check_construct(&c, &p, &init("http://127.0.0.1/")).is_ok());
    assert!(check_construct(&c, &p, &init("https://app.test:25/")).is_ok());
    // the page's own mistakes
    assert!(check_construct(&c, &p, &init("file:///x")).is_err());
    assert!(check_construct(&c, &p, &init("https://u\u{40}app.test/")).is_err());
    let mut i = init("/x");
    i.body = body(1, None);
    assert!(check_construct(&c, &p, &i).is_err());
    let mut i = with_method(init("https://api.test/"), "PUT");
    i.mode = Mode::NoCors;
    assert!(check_construct(&c, &p, &i).is_err());
}
