//! Methods, header grammar, forbidden and safelisted headers, the Headers guards.

use super::error::NetError;
use super::headers::*;

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

#[test]
fn method_normalization() {
    let rows = [
        ("get", "GET"),
        ("GET", "GET"),
        ("gEt", "GET"),
        ("post", "POST"),
        ("put", "PUT"),
        ("delete", "DELETE"),
        ("head", "HEAD"),
        ("options", "OPTIONS"),
        // not normalized: the standard leaves every other method as written
        ("patch", "patch"),
        ("PATCH", "PATCH"),
        ("Patch", "Patch"),
        ("purge", "purge"),
        ("m-search", "m-search"),
        ("X_CUSTOM", "X_CUSTOM"),
    ];
    for (input, want) in rows {
        assert_eq!(Method::parse(input).unwrap().as_str(), want, "{input}");
        assert_eq!(
            Method::parse_script(input).unwrap().as_str(),
            want,
            "{input}"
        );
    }
}

#[test]
fn method_rejections() {
    for bad in [
        "", " ", "GE T", "GET\n", "G\0T", "GET;", "GET/1", "é", "a:b", "(", "a,b", "a b", "\"GET\"",
    ] {
        assert!(
            matches!(Method::parse(bad), Err(NetError::BadMethod(_))),
            "{bad:?}"
        );
    }
    assert!(matches!(
        Method::parse(&"a".repeat(33)),
        Err(NetError::BadMethod(_))
    ));
    assert!(Method::parse(&"a".repeat(32)).is_ok());
    for m in ["CONNECT", "connect", "Trace", "TRACE", "track", "TrAcK"] {
        assert!(
            matches!(Method::parse_script(m), Err(NetError::ForbiddenMethod(_))),
            "{m}"
        );
        assert!(Method::parse(m).is_ok(), "parse (internal) allows {m}");
        assert!(is_forbidden_method(m));
    }
    assert!(!is_forbidden_method("PATCH"));
    assert!(!is_forbidden_method("TRACEX"));
}

#[test]
fn method_classes() {
    let rows = [
        ("GET", true, true, true),
        ("HEAD", false, true, true),
        ("POST", false, false, true),
        ("PUT", false, false, false),
        ("DELETE", false, false, false),
        ("OPTIONS", false, false, false),
        ("PATCH", false, false, false),
        ("patch", false, false, false),
    ];
    for (m, get, get_or_head, safelisted) in rows {
        let m = Method::parse(m).unwrap();
        assert_eq!(m.is_get(), get, "{}", m.as_str());
        assert_eq!(m.is_get_or_head(), get_or_head, "{}", m.as_str());
        assert_eq!(m.is_cors_safelisted(), safelisted, "{}", m.as_str());
    }
}

#[test]
fn header_names() {
    for ok in [
        "a",
        "Accept",
        "X-Custom-Header",
        "x_y",
        "!#$%&'*+-.^_`|~",
        "A1",
        "0",
        "content-type",
    ] {
        assert!(valid_header_name(ok), "{ok}");
    }
    for bad in [
        "", " ", "a b", "a:b", "a\n", "a\0", "é", "a(b", "a)b", "a,b", "a/b", "a;b", "a=b", "a@b",
        "a[b", "a\\b", "a\"b", "a{b", "a<b", "a?b", " a", "a ", "a\t",
    ] {
        assert!(!valid_header_name(bad), "{bad:?}");
    }
    assert!(!valid_header_name(&"a".repeat(MAX_NAME + 1)));
    assert!(valid_header_name(&"a".repeat(MAX_NAME)));
}

#[test]
fn header_values() {
    let ok = [
        "",
        "x",
        "a b",
        "a\tb",
        "text/html; charset=utf-8",
        "!~",
        "bytes=0-99",
        "a\"b",
    ];
    for v in ok {
        assert!(valid_header_value(v), "{v:?}");
    }
    for bad in [
        "a\nb",
        "a\rb",
        "a\0b",
        "a\x01b",
        "a\x1fb",
        "a\x7fb",
        "\u{e9}",
        "caf\u{e9}",
        "a\u{2028}b",
        "a\x0bb",
        "a\x0cb",
        "\u{1F600}",
    ] {
        assert!(!valid_header_value(bad), "{bad:?}");
    }
    assert!(!valid_header_value(&"a".repeat(MAX_VALUE + 1)));
    assert!(valid_header_value(&"a".repeat(MAX_VALUE)));
    assert_eq!(trim_header_value("  \t x y \r\n"), "x y");
    assert_eq!(trim_header_value("\n\n"), "");
    assert_eq!(trim_header_value("a\nb"), "a\nb");
}

#[test]
fn forbidden_request_headers() {
    let forbidden = [
        "Accept-Charset",
        "Accept-Encoding",
        "Access-Control-Request-Headers",
        "Access-Control-Request-Method",
        "Connection",
        "Content-Length",
        "Cookie",
        "Cookie2",
        "Date",
        "DNT",
        "Expect",
        "Host",
        "Keep-Alive",
        "Origin",
        "Referer",
        "Set-Cookie",
        "TE",
        "Trailer",
        "Transfer-Encoding",
        "Upgrade",
        "Via",
        "Proxy-Authorization",
        "Proxy-Foo",
        "proxy-connection",
        "Sec-Fetch-Mode",
        "Sec-Fetch-Site",
        "Sec-Fetch-Dest",
        "Sec-WebSocket-Key",
        "sec-",
        "SEC-CH-UA",
        "cOOkIe",
        "HOST",
        "oRiGiN",
        "ReFeReR",
    ];
    for n in forbidden {
        assert!(is_forbidden_request_header(n, "x"), "{n}");
    }
    let allowed = [
        "Accept",
        "Accept-Language",
        "Authorization",
        "Content-Type",
        "Content-Language",
        "X-Requested-With",
        "Cache-Control",
        "Pragma",
        "If-None-Match",
        "If-Modified-Since",
        "Range",
        "User-Agent",
        "X-Api-Key",
        "Referrer",
        "Origin2",
        "Hosts",
        "Secure",
        "Proxy",
        "Cookies",
        "Dat",
        "T",
        "Accept-Encodings",
        "X-Sec-Fetch",
        "Content-Encoding",
        "Content-MD5",
        "Authorization2",
        "Forwarded",
        "X-Forwarded-For",
        "Max-Forwards",
        "If-Match",
        "If-Range",
        "Priority",
    ];
    for n in allowed {
        assert!(!is_forbidden_request_header(n, "x"), "{n}");
    }
}

#[test]
fn method_override_headers() {
    for n in [
        "X-HTTP-Method",
        "X-HTTP-Method-Override",
        "X-Method-Override",
        "x-http-method-override",
    ] {
        for v in [
            "CONNECT",
            "trace",
            "Track",
            " TRACE ",
            "get, trace",
            "PUT,CONNECT",
            "TRACK, x",
        ] {
            assert!(is_forbidden_request_header(n, v), "{n}: {v}");
        }
        for v in ["PUT", "DELETE", "patch", "", "TRACEX", "GET, POST"] {
            assert!(!is_forbidden_request_header(n, v), "{n}: {v}");
        }
    }
    assert!(!is_forbidden_request_header("X-Other", "CONNECT"));
}

#[test]
fn forbidden_response_headers() {
    for n in ["Set-Cookie", "set-cookie", "SET-COOKIE", "Set-Cookie2"] {
        assert!(is_forbidden_response_header(n), "{n}");
    }
    for n in ["Cookie", "Content-Type", "Set-Cookies", "X-Set-Cookie", ""] {
        assert!(!is_forbidden_response_header(n), "{n}");
    }
}

#[test]
fn mime_essences() {
    let rows = [
        ("text/plain", Some("text/plain")),
        ("TEXT/PLAIN", Some("text/plain")),
        ("text/plain; charset=utf-8", Some("text/plain")),
        ("  text/plain  ;x", Some("text/plain")),
        (
            "application/x-www-form-urlencoded",
            Some("application/x-www-form-urlencoded"),
        ),
        (
            "multipart/form-data; boundary=----x",
            Some("multipart/form-data"),
        ),
        ("application/vnd.api+json", Some("application/vnd.api+json")),
        ("text", None),
        ("text/", None),
        ("/plain", None),
        ("", None),
        ("text/plain, text/html", None),
        ("te xt/plain", None),
        ("text/pl ain", None),
        ("text//plain", None),
        ("text/plain/x", None),
        ("\"text/plain\"", None),
        (";charset=x", None),
    ];
    for (v, want) in rows {
        assert_eq!(mime_essence(v).as_deref(), want, "{v:?}");
    }
}

#[test]
fn safelisted_request_headers() {
    let rows = [
        ("Accept", "*/*", true),
        ("accept", "text/html,application/xhtml+xml;q=0.9", true),
        ("Accept", "application/json", true),
        ("Accept", "a\"b", false),
        ("Accept", "a(b", false),
        ("Accept", "a:b", false),
        ("Accept", "a<b", false),
        ("Accept", "a?b", false),
        ("Accept", "a@b", false),
        ("Accept", "a[b", false),
        ("Accept", "a\\b", false),
        ("Accept", "a{b", false),
        ("Accept", "a}b", false),
        ("Accept", "a\x7fb", false),
        ("Accept", "a\x01b", false),
        ("Accept", "a\tb", true),
        ("Accept", &"a".repeat(128), true),
        ("Accept", &"a".repeat(129), false),
        ("Accept-Language", "en-US,en;q=0.9", true),
        ("Accept-Language", "*", true),
        ("Accept-Language", "de_DE", false),
        ("Accept-Language", "en/US", false),
        ("Accept-Language", "en\u{e9}", false),
        ("Content-Language", "en", true),
        ("Content-Language", "en-US, de", true),
        ("Content-Language", "en:US", false),
        ("Content-Type", "text/plain", true),
        ("Content-Type", "text/plain;charset=UTF-8", true),
        ("Content-Type", "text/plain; charset=utf-8", true),
        ("Content-Type", "Text/Plain", true),
        ("Content-Type", "application/x-www-form-urlencoded", true),
        (
            "Content-Type",
            "application/x-www-form-urlencoded;charset=UTF-8",
            true,
        ),
        (
            "Content-Type",
            "multipart/form-data; boundary=----WebKitFormBoundaryABC",
            true,
        ),
        ("Content-Type", "application/json", false),
        ("Content-Type", "application/json; charset=utf-8", false),
        ("Content-Type", "text/html", false),
        ("Content-Type", "text/xml", false),
        ("Content-Type", "application/xml", false),
        ("Content-Type", "image/png", false),
        ("Content-Type", "text/plain;charset=\"utf-8\"", false),
        ("Content-Type", "text/plain, text/html", false),
        ("Content-Type", "text/plain\u{1}", false),
        ("Content-Type", "", false),
        ("Content-Type", "plain", false),
        (
            "Content-Type",
            &format!("text/plain;x={}", "a".repeat(120)),
            false,
        ),
        ("Range", "bytes=0-99", false),
        ("Range", "bytes=5-", false),
        ("Authorization", "Bearer x", false),
        ("X-Custom", "1", false),
        ("Cache-Control", "no-cache", false),
        ("If-None-Match", "x", false),
        ("DPR", "2", false),
        ("Save-Data", "on", false),
        ("Content-Length", "0", false),
        ("X-Requested-With", "XMLHttpRequest", false),
        ("Pragma", "no-cache", false),
    ];
    for (n, v, want) in rows {
        assert_eq!(is_cors_safelisted_request_header(n, v), want, "{n}: {v:?}");
    }
}

#[test]
fn no_cors_safelisted() {
    for n in [
        "Accept",
        "accept-language",
        "Content-Language",
        "Content-Type",
    ] {
        assert!(is_no_cors_safelisted_name(n), "{n}");
    }
    for n in ["Range", "Authorization", "X-Foo", "Cache-Control"] {
        assert!(!is_no_cors_safelisted_name(n), "{n}");
    }
    assert!(is_no_cors_safelisted_request_header(
        "Content-Type",
        "text/plain"
    ));
    assert!(!is_no_cors_safelisted_request_header(
        "Content-Type",
        "application/json"
    ));
    assert!(is_no_cors_safelisted_request_header("Accept", "*/*"));
    assert!(!is_no_cors_safelisted_request_header("Accept", "a\"b"));
}

#[test]
fn unsafe_header_names() {
    let h = |v: &[(&str, &str)]| cors_unsafe_request_header_names(&pairs(v));
    assert!(h(&[]).is_empty());
    assert!(h(&[("Accept", "*/*"), ("Content-Type", "text/plain")]).is_empty());
    assert_eq!(h(&[("X-B", "1"), ("X-A", "2")]), ["x-a", "x-b"]);
    assert_eq!(h(&[("Content-Type", "application/json")]), ["content-type"]);
    assert_eq!(h(&[("X-A", "1"), ("x-a", "2"), ("X-A", "3")]), ["x-a"]);
    assert_eq!(
        h(&[("Authorization", "t"), ("Accept", "*/*")]),
        ["authorization"]
    );
    assert_eq!(h(&[("Range", "bytes=0-1")]), ["range"]);
    // the 1024-byte rule: safelisted headers add up, and then they all count
    let big = "a".repeat(100);
    let many: Vec<(String, String)> = (0..11)
        .map(|i| {
            (
                if i % 2 == 0 {
                    "Accept"
                } else {
                    "Accept-Language"
                }
                .to_string(),
                big.clone(),
            )
        })
        .collect();
    let names = cors_unsafe_request_header_names(&many);
    assert_eq!(names, ["accept", "accept-language"]);
    let mut many = many;
    many.push(("Content-Language".into(), "en".into()));
    // 11 * 100 = 1100 > 1024: every safelisted-by-name header is unsafe now
    assert_eq!(
        cors_unsafe_request_header_names(&many),
        ["accept", "accept-language", "content-language"]
    );
    // 10 * 100 = 1000 is fine
    let ten: Vec<(String, String)> = (0..10)
        .map(|_| ("Accept".to_string(), big.clone()))
        .collect();
    assert!(cors_unsafe_request_header_names(&ten).is_empty());
}

#[test]
fn response_header_lists() {
    let h = pairs(&[
        ("access-control-allow-headers", "x-a, X-B"),
        ("Access-Control-Allow-Headers", " x-c ,, "),
        ("vary", "origin"),
    ]);
    assert_eq!(
        list_values(&h, "access-control-allow-headers"),
        ["x-a", "X-B", "x-c"]
    );
    assert_eq!(
        combined_value(&h, "access-control-allow-headers").unwrap(),
        "x-a, X-B,  x-c ,, "
    );
    assert_eq!(combined_value(&h, "vary").as_deref(), Some("origin"));
    assert_eq!(combined_value(&h, "missing"), None);
    assert!(list_values(&h, "missing").is_empty());
}

// ---- the Headers object ----

fn names(h: &Headers) -> Vec<(String, String)> {
    h.entries()
}

#[test]
fn headers_none_guard_basics() {
    let mut h = Headers::new(Guard::None);
    h.append("Content-Type", "text/plain").unwrap();
    h.append("X-A", "1").unwrap();
    h.append("x-a", "2").unwrap();
    assert_eq!(h.get("X-A").unwrap().as_deref(), Some("1, 2"));
    assert_eq!(
        h.get("content-type").unwrap().as_deref(),
        Some("text/plain")
    );
    assert_eq!(h.get("nope").unwrap(), None);
    assert!(h.has("X-a").unwrap());
    assert!(!h.has("y").unwrap());
    h.set("X-A", "3").unwrap();
    assert_eq!(h.get("x-a").unwrap().as_deref(), Some("3"));
    h.delete("X-A").unwrap();
    assert!(!h.has("x-a").unwrap());
    assert_eq!(names(&h), pairs(&[("content-type", "text/plain")]));
    // forbidden headers are fine with guard none
    h.append("Cookie", "a=b").unwrap();
    h.append("Set-Cookie", "a=b").unwrap();
    assert!(h.has("cookie").unwrap());
}

#[test]
fn headers_value_normalization_and_errors() {
    let mut h = Headers::new(Guard::None);
    h.append("X-A", "  v \t").unwrap();
    assert_eq!(h.get("x-a").unwrap().as_deref(), Some("v"));
    assert!(matches!(
        h.append("bad name", "v"),
        Err(NetError::BadHeaderName(_))
    ));
    assert!(matches!(h.append("", "v"), Err(NetError::BadHeaderName(_))));
    assert!(matches!(
        h.append("x", "a\nb"),
        Err(NetError::BadHeaderValue(_))
    ));
    assert!(matches!(
        h.append("x", "a\0"),
        Err(NetError::BadHeaderValue(_))
    ));
    assert!(matches!(
        h.append("x", "\u{e9}"),
        Err(NetError::BadHeaderValue(_))
    ));
    assert!(matches!(
        h.set("bad name", "v"),
        Err(NetError::BadHeaderName(_))
    ));
    assert!(matches!(
        h.set("x", "a\rb"),
        Err(NetError::BadHeaderValue(_))
    ));
    assert!(matches!(h.get("bad name"), Err(NetError::BadHeaderName(_))));
    assert!(matches!(h.has("bad name"), Err(NetError::BadHeaderName(_))));
    assert!(matches!(
        h.delete("bad name"),
        Err(NetError::BadHeaderName(_))
    ));
    // leading/trailing line breaks are trimmed, not rejected
    h.append("y", "\r\nv\r\n").unwrap();
    assert_eq!(h.get("y").unwrap().as_deref(), Some("v"));
    assert_eq!(h.len(), 2);
}

#[test]
fn headers_request_guard_drops_forbidden_silently() {
    let mut h = Headers::new(Guard::Request);
    for n in [
        "Cookie",
        "Host",
        "Origin",
        "Referer",
        "Sec-Fetch-Mode",
        "Proxy-Authorization",
        "Content-Length",
        "Set-Cookie",
        "Connection",
        "Via",
        "Date",
    ] {
        h.append(n, "x").unwrap();
        h.set(n, "y").unwrap();
        assert!(!h.has(n).unwrap(), "{n}");
    }
    h.append("X-HTTP-Method-Override", "TRACE").unwrap();
    assert!(h.is_empty());
    h.append("X-HTTP-Method-Override", "PUT").unwrap();
    assert!(h.has("x-http-method-override").unwrap());
    h.append("Authorization", "Bearer t").unwrap();
    h.append("Content-Type", "application/json").unwrap();
    assert_eq!(h.len(), 3);
    // delete of a forbidden name does nothing
    h.delete("Cookie").unwrap();
    h.delete("authorization").unwrap();
    assert!(!h.has("authorization").unwrap());
}

#[test]
fn headers_no_cors_guard_keeps_only_safelisted() {
    let mut h = Headers::new(Guard::RequestNoCors);
    h.append("Accept", "*/*").unwrap();
    h.append("Content-Type", "text/plain").unwrap();
    h.append("Content-Type", "application/json").unwrap(); // combined value is not safelisted: dropped
    h.append("Content-Language", "en").unwrap();
    h.append("Accept-Language", "en").unwrap();
    h.append("Authorization", "x").unwrap();
    h.append("X-Custom", "x").unwrap();
    h.append("Range", "bytes=0-1").unwrap();
    assert_eq!(
        names(&h),
        pairs(&[
            ("accept", "*/*"),
            ("accept-language", "en"),
            ("content-language", "en"),
            ("content-type", "text/plain")
        ])
    );
    h.set("Content-Type", "application/json").unwrap(); // not safelisted: ignored
    assert_eq!(
        h.get("content-type").unwrap().as_deref(),
        Some("text/plain")
    );
    h.set("Content-Type", "multipart/form-data").unwrap();
    assert_eq!(
        h.get("content-type").unwrap().as_deref(),
        Some("multipart/form-data")
    );
    h.delete("X-Custom").unwrap();
    h.delete("Content-Type").unwrap();
    assert!(!h.has("content-type").unwrap());
    // an Accept with an unsafe byte is dropped
    h.set("Accept", "a(b").unwrap();
    assert_eq!(h.get("accept").unwrap().as_deref(), Some("*/*"));
}

#[test]
fn headers_response_guard_and_immutable() {
    let mut h = Headers::new(Guard::Response);
    h.append("Set-Cookie", "a=b").unwrap();
    h.append("Set-Cookie2", "a=b").unwrap();
    h.append("Cookie", "kept").unwrap(); // a response may hold this name
    h.append("X-A", "1").unwrap();
    assert_eq!(names(&h), pairs(&[("cookie", "kept"), ("x-a", "1")]));
    assert!(h.get_set_cookie().is_empty());
    h.set_guard(Guard::Immutable);
    assert_eq!(h.guard(), Guard::Immutable);
    assert_eq!(h.append("x", "1"), Err(NetError::HeadersImmutable));
    assert_eq!(h.set("x", "1"), Err(NetError::HeadersImmutable));
    assert_eq!(h.delete("x-a"), Err(NetError::HeadersImmutable));
    assert_eq!(h.get("x-a").unwrap().as_deref(), Some("1"));
    // errors in the name come first? either way it's an error
    assert!(h.append("bad name", "1").is_err());
}

#[test]
fn headers_set_guard_prunes() {
    let mut h = Headers::new(Guard::None);
    h.append("Cookie", "a").unwrap();
    h.append("X-A", "1").unwrap();
    h.append("Accept", "*/*").unwrap();
    h.set_guard(Guard::Request);
    assert_eq!(h.len(), 2);
    h.set_guard(Guard::RequestNoCors);
    assert_eq!(names(&h), pairs(&[("accept", "*/*")]));
}

#[test]
fn headers_entries_sorted_and_combined() {
    let mut h = Headers::new(Guard::None);
    h.append("Zed", "1").unwrap();
    h.append("alpha", "a").unwrap();
    h.append("Zed", "2").unwrap();
    h.append("Set-Cookie", "a=1").unwrap();
    h.append("Set-Cookie", "b=2").unwrap();
    h.append("mid", "m").unwrap();
    assert_eq!(
        names(&h),
        pairs(&[
            ("alpha", "a"),
            ("mid", "m"),
            ("set-cookie", "a=1"),
            ("set-cookie", "b=2"),
            ("zed", "1, 2")
        ])
    );
    assert_eq!(h.get_set_cookie(), ["a=1", "b=2"]);
    assert_eq!(h.get("set-cookie").unwrap().as_deref(), Some("a=1, b=2"));
    assert_eq!(h.raw().len(), 6);
}

#[test]
fn headers_from_pairs_applies_guard() {
    let h = Headers::from_pairs(
        Guard::Request,
        &[("Cookie", "x"), ("X-A", "1"), ("Host", "h")],
    )
    .unwrap();
    assert_eq!(names(&h), pairs(&[("x-a", "1")]));
    assert_eq!(h.guard(), Guard::Request);
    assert!(Headers::from_pairs(Guard::None, &[("bad name", "x")]).is_err());
    let h = Headers::from_pairs(Guard::Response, &[("Set-Cookie", "a=b"), ("A", "1")]).unwrap();
    assert_eq!(names(&h), pairs(&[("a", "1")]));
}

#[test]
fn headers_limits() {
    let mut h = Headers::new(Guard::None);
    for i in 0..MAX_HEADERS {
        h.append(&format!("x-{i}"), "v").unwrap();
    }
    assert_eq!(
        h.append("x-extra", "v"),
        Err(NetError::TooManyHeaders { max: MAX_HEADERS })
    );
    // repeating a name still counts as an entry
    assert!(h.append("x-1", "w").is_err());
    // set of an existing name needs no more room than the entry it replaces
    h.set("x-1", "w").unwrap();
    assert_eq!(h.len(), MAX_HEADERS);
    let mut h = Headers::new(Guard::None);
    let big = "a".repeat(8000);
    h.append("a", &big).unwrap();
    h.append("b", &big).unwrap();
    assert!(matches!(
        h.append("c", &big),
        Err(NetError::HeadersTooLarge { .. })
    ));
    assert_eq!(h.len(), 2);
    // a failed set leaves the object as it was
    let before = h.clone();
    assert!(h.set("a", &format!("{big}")).is_ok());
    assert_eq!(h.len(), before.len());
}

#[test]
fn headers_names_are_stored_lower_case() {
    let mut h = Headers::new(Guard::None);
    h.append("X-MiXeD", "1").unwrap();
    assert_eq!(h.raw()[0].0, "x-mixed");
}
