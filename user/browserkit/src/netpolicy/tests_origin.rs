//! Origins, sites, address classes, ports and address resolution.

use super::error::NetError;
use super::limits::Limits;
use super::origin::*;
use milkweb::url::Url;

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn lim() -> Limits {
    Limits::default()
}

fn resolve(base: &str, input: &str) -> Result<Target, NetError> {
    resolve_url(&u(base), input, &lim())
}

fn http_text(base: &str, input: &str) -> String {
    match resolve(base, input) {
        Ok(Target::Http(url)) => url.to_text(),
        other => panic!("{input:?} against {base}: expected an http address, got {other:?}"),
    }
}

fn err(base: &str, input: &str) -> NetError {
    match resolve(base, input) {
        Err(e) => e,
        Ok(t) => panic!("{input:?} against {base}: expected an error, got {t:?}"),
    }
}

#[test]
fn origin_serialization() {
    let rows = [
        ("http://example.com/", "http://example.com"),
        ("https://example.com/x?y=1", "https://example.com"),
        ("http://example.com:80/", "http://example.com"),
        ("https://example.com:443/", "https://example.com"),
        ("http://example.com:8080/", "http://example.com:8080"),
        ("https://example.com:80/", "https://example.com:80"),
        ("http://EXAMPLE.com/", "http://example.com"),
        ("http://[::1]:3000/", "http://[::1]:3000"),
        ("http://[::1]/", "http://[::1]"),
        ("http://example.com./", "http://example.com"),
        ("http://user:pw\u{40}example.com/", "http://example.com"),
        ("http://localhost:1234/a/b", "http://localhost:1234"),
    ];
    for (url, want) in rows {
        assert_eq!(Origin::of(&u(url)).serialize(), want, "{url}");
    }
    assert_eq!(Origin::Opaque(7).serialize(), "null");
}

#[test]
fn same_origin_table() {
    let rows = [
        ("http://a.test/", "http://a.test/other", true),
        ("http://a.test/", "http://a.test:80/", true),
        ("http://a.test/", "http://A.TEST/", true),
        ("http://a.test/", "https://a.test/", false),
        ("http://a.test/", "http://a.test:8080/", false),
        ("https://a.test/", "https://a.test:443/", true),
        ("http://a.test/", "http://b.test/", false),
        ("http://a.test/", "http://www.a.test/", false),
        ("http://a.test/", "http://a.test.evil/", false),
        ("http://a.test/", "http://a.test.evil.test/", false),
        ("http://[::1]/", "http://[0:0:0:0:0:0:0:1]/", false), // textual compare: stricter
        ("http://localhost/", "http://127.0.0.1/", false),
        ("http://1.2.3.4/", "http://1.2.3.4:80/", true),
    ];
    for (a, b, want) in rows {
        let (oa, ob) = (Origin::of(&u(a)), Origin::of(&u(b)));
        assert_eq!(oa.same_origin(&ob), want, "{a} vs {b}");
        assert_eq!(ob.same_origin(&oa), want, "{b} vs {a}");
    }
}

#[test]
fn opaque_origins_match_only_themselves() {
    let t = Origin::of(&u("http://a.test/"));
    assert!(Origin::Opaque(1).same_origin(&Origin::Opaque(1)));
    assert!(!Origin::Opaque(1).same_origin(&Origin::Opaque(2)));
    assert!(!Origin::Opaque(1).same_origin(&t));
    assert!(!t.same_origin(&Origin::Opaque(1)));
    assert!(!Origin::Opaque(1).same_site(&t));
    assert!(Origin::Opaque(3).same_site(&Origin::Opaque(3)));
    assert_eq!(Origin::Opaque(1).site(), None);
    assert_eq!(Origin::Opaque(1).address_space(), AddressSpace::Public);
}

#[test]
fn same_site_table() {
    let rows = [
        ("http://a.milk.test/", "http://b.milk.test/", true),
        ("http://milk.test/", "http://www.milk.test:8080/", true),
        ("https://a.milk.test/", "https://milk.test/", true),
        // schemeful: http and https are different sites
        ("http://milk.test/", "https://milk.test/", false),
        ("http://milk.test/", "http://other.test/", false),
        ("http://a.example.co.uk/", "http://b.example.co.uk/", true),
        ("http://a.example.co.uk/", "http://b.other.co.uk/", false),
        ("http://a.co.uk/", "http://b.co.uk/", false),
        ("http://alice.github.io/", "http://bob.github.io/", false),
        ("http://localhost/", "http://localhost:99/", true),
        ("http://localhost/", "http://127.0.0.1/", false),
        ("http://10.0.0.1/", "http://10.0.0.2/", false),
        ("http://10.0.0.1/", "http://10.0.0.1:81/", true),
    ];
    for (a, b, want) in rows {
        let (oa, ob) = (Origin::of(&u(a)), Origin::of(&u(b)));
        assert_eq!(oa.same_site(&ob), want, "{a} vs {b}");
        assert_eq!(ob.same_site(&oa), want, "{b} vs {a}");
    }
}

#[test]
fn address_spaces() {
    use AddressSpace::*;
    let rows = [
        ("localhost", Loopback),
        ("LOCALHOST", Loopback),
        ("foo.localhost", Loopback),
        ("a.b.localhost", Loopback),
        ("notlocalhost", Private),
        ("localhost.evil.test", Public),
        ("127.0.0.1", Loopback),
        ("127.255.255.254", Loopback),
        ("0.0.0.0", Loopback),
        ("0.1.2.3", Loopback),
        ("::1", Loopback),
        ("::", Loopback),
        ("10.0.2.2", Private),
        ("10.255.255.255", Private),
        ("172.15.0.1", Public),
        ("172.16.0.1", Private),
        ("172.31.255.255", Private),
        ("172.32.0.1", Public),
        ("192.168.0.1", Private),
        ("192.167.0.1", Public),
        ("169.254.169.254", Private),
        ("100.64.0.1", Private),
        ("100.127.255.255", Private),
        ("100.128.0.1", Public),
        ("100.63.255.255", Public),
        ("224.0.0.1", Private),
        ("255.255.255.255", Private),
        ("8.8.8.8", Public),
        ("93.184.216.34", Public),
        ("fe80::1", Private),
        ("febf::1", Private),
        ("fec0::1", Public),
        ("fc00::1", Private),
        ("fd12:3456::1", Private),
        ("fb00::1", Public),
        ("ff02::1", Private),
        ("2001:db8::1", Public),
        ("2606:4700::1111", Public),
        ("::ffff:127.0.0.1", Loopback),
        ("::ffff:10.1.2.3", Private),
        ("::ffff:8.8.8.8", Public),
        ("::127.0.0.1", Loopback),
        ("printer", Private),
        ("router.local", Private),
        ("nas.lan", Private),
        ("thing.internal", Private),
        ("x.home.arpa", Private),
        ("example.com", Public),
        ("www.example.com", Public),
        ("milk.test", Public),
        ("example.com.", Public),
        ("localhost.", Loopback),
        ("not an ip:::", Loopback),
    ];
    for (host, want) in rows {
        assert_eq!(address_space_of_host(host), want, "{host}");
    }
}

#[test]
fn address_space_ordering_and_reach() {
    use AddressSpace::*;
    assert!(Public < Private && Private < Loopback);
    assert!(Public.may_reach(Public));
    assert!(!Public.may_reach(Private));
    assert!(!Public.may_reach(Loopback));
    assert!(Private.may_reach(Public));
    assert!(Private.may_reach(Private));
    assert!(!Private.may_reach(Loopback));
    assert!(Loopback.may_reach(Loopback));
    assert!(Loopback.may_reach(Private));
    assert!(Loopback.may_reach(Public));
}

#[test]
fn ipv4_parsing() {
    let ok = [
        ("1.2.3.4", [1, 2, 3, 4]),
        ("0.0.0.0", [0, 0, 0, 0]),
        ("255.255.255.255", [255, 255, 255, 255]),
        ("127.0.0.1", [127, 0, 0, 1]),
        ("10.0.2.2", [10, 0, 2, 2]),
    ];
    for (s, want) in ok {
        assert_eq!(parse_ipv4(s), Some(want), "{s}");
    }
    let bad = [
        "",
        "1",
        "1.2",
        "1.2.3",
        "1.2.3.4.5",
        "256.1.1.1",
        "1.2.3.256",
        "01.2.3.4",
        "1.2.3.04",
        "1..2.3",
        ".1.2.3",
        "1.2.3.",
        "1.2.3.4 ",
        " 1.2.3.4",
        "a.b.c.d",
        "0x7f.0.0.1",
        "1.2.3.-4",
        "1.2.3.+4",
        "1000.1.1.1",
        "1.2.3.4a",
        "127.1",
        "2130706433",
        "0177.0.0.1",
    ];
    for s in bad {
        assert_eq!(parse_ipv4(s), None, "{s:?}");
    }
}

#[test]
fn ipv6_parsing() {
    let ok: [(&str, [u16; 8]); 14] = [
        ("::", [0; 8]),
        ("::1", [0, 0, 0, 0, 0, 0, 0, 1]),
        ("1::", [1, 0, 0, 0, 0, 0, 0, 0]),
        ("1:2:3:4:5:6:7:8", [1, 2, 3, 4, 5, 6, 7, 8]),
        ("1:2:3:4:5:6:7::", [1, 2, 3, 4, 5, 6, 7, 0]),
        ("::2:3:4:5:6:7:8", [0, 2, 3, 4, 5, 6, 7, 8]),
        (
            "2001:db8::ff00:42:8329",
            [0x2001, 0xdb8, 0, 0, 0, 0xff00, 0x42, 0x8329],
        ),
        ("FE80::ABCD", [0xfe80, 0, 0, 0, 0, 0, 0, 0xabcd]),
        ("::ffff:1.2.3.4", [0, 0, 0, 0, 0, 0xffff, 0x0102, 0x0304]),
        ("::1.2.3.4", [0, 0, 0, 0, 0, 0, 0x0102, 0x0304]),
        ("1:2:3:4:5:6:1.2.3.4", [1, 2, 3, 4, 5, 6, 0x0102, 0x0304]),
        ("0:0:0:0:0:0:0:1", [0, 0, 0, 0, 0, 0, 0, 1]),
        ("0001::1", [1, 0, 0, 0, 0, 0, 0, 1]),
        ("a:b:c:d:e:f:0:1", [0xa, 0xb, 0xc, 0xd, 0xe, 0xf, 0, 1]),
    ];
    for (s, want) in ok {
        assert_eq!(parse_ipv6(s), Some(want), "{s}");
    }
    let bad = [
        "",
        ":",
        ":::",
        "1:2:3:4:5:6:7",
        "1:2:3:4:5:6:7:8:9",
        "1::2::3",
        "12345::1",
        "g::1",
        "::1%eth0",
        "1:2:3:4:5:6:7:8::",
        "::1.2.3",
        "::1.2.3.4.5",
        "1.2.3.4",
        "::1.2.3.256",
        "1:2:3:4:5:6:7:1.2.3.4",
        ":1:2:3:4:5:6:7",
        "1:2:3:4:5:6:7:",
        "::1.2.3.4:1",
        "[::1]",
        "::-1",
        "1:2:3:4:5:6:7:8 ",
    ];
    for s in bad {
        assert_eq!(parse_ipv6(s), None, "{s:?}");
    }
}

#[test]
fn trustworthy_and_loopback() {
    let rows = [
        ("https://example.com/", true),
        ("http://example.com/", false),
        ("http://localhost/", true),
        ("http://foo.localhost/", true),
        ("http://127.0.0.1:8080/", true),
        ("http://127.5.5.5/", true),
        ("http://[::1]/", true),
        ("http://10.0.2.2/", false),
        ("http://192.168.0.1/", false),
        ("http://notlocalhost/", false),
        ("http://localhost.evil.test/", false),
        ("http://0.0.0.0/", false),
        ("https://10.0.0.1/", true),
    ];
    for (url, want) in rows {
        assert_eq!(is_potentially_trustworthy(&u(url)), want, "{url}");
    }
    assert!(is_loopback_host("localhost."));
    assert!(!is_loopback_host("::2"));
}

#[test]
fn bad_ports_table() {
    for p in BAD_PORTS {
        assert!(is_bad_port(*p), "{p}");
    }
    for p in [
        80u16,
        443,
        8080,
        8443,
        3000,
        5000,
        8000,
        9000,
        5432,
        6379,
        27017,
        6666 + 100,
        65535,
    ] {
        assert!(!is_bad_port(p), "{p}");
    }
    for p in [
        22, 25, 110, 143, 465, 587, 993, 995, 6000, 6667, 10080, 2049, 5060, 5061, 4045,
    ] {
        assert!(is_bad_port(p), "{p}");
    }
    // the list is strictly sorted, so a typo shows up
    assert!(BAD_PORTS.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn host_ok_table() {
    let ok = [
        "example.com",
        "a.b.c",
        "a-b.c-d.e",
        "localhost",
        "1.2.3.4",
        "::1",
        "2001:db8::1",
        "a1.b2",
        "1a.com",
        "x-y.test",
        "123abc.test",
        "a.1b",
        "my_host.test",
    ];
    for h in ok {
        assert!(host_ok(h), "{h}");
    }
    let bad = [
        "127.1",
        "0x7f.1",
        "0x7f.0.0.1",
        "2130706433",
        "1.2.3",
        "a.1",
        "a.0x1",
        "010.0.0.1",
        "1.2.3.4.5",
        "1.2.3.256",
        ":::",
        "g::1",
        "a.123",
        "a.0X10",
        "-",
        "a-.b",
        "-a.b",
        "a.b-",
        "a..b",
        ".a",
        &"x".repeat(64),
    ];
    for h in bad {
        assert!(!host_ok(h), "{h}");
    }
}

#[test]
fn resolve_absolute_and_relative() {
    let base = "http://a.test/dir/page.html?q=1";
    let rows = [
        ("http://b.test/x", "http://b.test/x"),
        ("https://b.test/x?y=2", "https://b.test/x?y=2"),
        ("HTTP://B.TEST/X", "http://b.test/X"),
        ("//c.test/p", "http://c.test/p"),
        ("/root", "http://a.test/root"),
        ("sibling", "http://a.test/dir/sibling"),
        ("../up", "http://a.test/up"),
        ("./here", "http://a.test/dir/here"),
        ("?z=9", "http://a.test/dir/page.html?z=9"),
        ("#frag", "http://a.test/dir/page.html?q=1"),
        ("", "http://a.test/dir/page.html?q=1"),
        ("  /trim  ", "http://a.test/trim"),
        ("/a b", "http://a.test/a%20b"),
        ("/\u{e9}", "http://a.test/%C3%A9"),
        ("/a\tb\nc\rd", "http://a.test/abcd"),
        ("http://b.test:8080/x#frag", "http://b.test:8080/x"),
        ("http://b.test", "http://b.test/"),
        ("http://[::1]:3000/x", "http://[::1]:3000/x"),
        ("http://b.test/a/../b", "http://b.test/b"),
        ("http://B.test./x", "http://b.test/x"),
        ("/path:with:colons", "http://a.test/path:with:colons"),
        ("1a:b", "http://a.test/dir/1a:b"),
        ("./a:b", "http://a.test/dir/a:b"),
        ("/x@y", "http://a.test/x@y"),
        ("?q=a@b", "http://a.test/dir/page.html?q=a@b"),
        (
            "http://b.test/?u=http://c.test\u{40}d.test",
            "http://b.test/?u=http://c.test\u{40}d.test",
        ),
        ("http://b.test?x=@", "http://b.test/?x=@"),
        ("http://b.test#@x", "http://b.test/"),
        ("//b.test?x=@", "http://b.test/?x=@"),
    ];
    for (input, want) in rows {
        assert_eq!(http_text(base, input), want, "{input:?}");
    }
    // https page: a scheme-relative address keeps https
    assert_eq!(
        http_text("https://a.test/", "//c.test/p"),
        "https://c.test/p"
    );
}

#[test]
fn resolve_refuses_credentials_backslashes_and_odd_schemes() {
    let base = "http://a.test/";
    for bad in [
        "http://user\u{40}b.test/",
        "http://user:pw\u{40}b.test/",
        "https://:pw\u{40}b.test/",
        "http://@b.test/",
        "//user\u{40}b.test/",
        "//u:p\u{40}b.test/x",
        "HTTP://user\u{40}b.test/",
        "http://a.test:80\u{40}evil.test/",
        "http://good.test\u{40}evil.test/",
    ] {
        assert_eq!(err(base, bad), NetError::UrlHasCredentials, "{bad}");
    }
    for bad in [
        "http://a.test\\@evil.test/",
        "http:\\\\evil.test/",
        "\\\\evil.test/x",
        "/a\\b",
        "http://a.test/a\\b",
        "http:/\\evil.test",
    ] {
        assert!(matches!(err(base, bad), NetError::BadUrl(_)), "{bad}");
    }
    for (bad, scheme) in [
        ("file:///etc/passwd", "file"),
        ("javascript:alert(1)", "javascript"),
        ("JavaScript:alert(1)", "javascript"),
        ("ftp://x.test/", "ftp"),
        ("ws://x.test/", "ws"),
        ("wss://x.test/", "wss"),
        ("mailto:a\u{40}b.test", "mailto"),
        ("about:blank", "about"),
        ("chrome://settings", "chrome"),
        ("milk:history", "milk"),
        ("localhost:8080/x", "localhost"),
        ("a.b:foo", "a.b"),
        ("x-y+z.q:foo", "x-y+z.q"),
        ("view-source:http://a.test/", "view-source"),
        ("filesystem:http://a.test/x", "filesystem"),
        ("tel:123", "tel"),
    ] {
        assert_eq!(
            err(base, bad),
            NetError::UnsupportedScheme(scheme.into()),
            "{bad}"
        );
    }
    // http: with no // is refused rather than guessed at
    for bad in [
        "http:foo",
        "https:foo",
        "http:/foo",
        "https:",
        "http:",
        "http:?x",
    ] {
        assert!(matches!(err(base, bad), NetError::BadUrl(_)), "{bad}");
    }
    // not schemes: these are paths
    for ok in [
        "1:2",
        "-a:b",
        "/a:b",
        "./javascript:x",
        "?javascript:x",
        "#javascript:x",
    ] {
        assert!(resolve(base, ok).is_ok(), "{ok}");
    }
}

#[test]
fn resolve_refuses_odd_hosts_and_ports() {
    let base = "http://a.test/";
    for bad in [
        "http://127.1/",
        "http://0x7f.0.0.1/",
        "http://2130706433/",
        "http://010.0.0.1/",
        "http://1.2.3/",
        "http://256.1.1.1/",
        "http://[::g]/",
        "http://[1:2:3]/",
        "http://[::1/",
        "http://[::1]x/",
        "http://b.test:99999/",
        "http://b.test:0/",
        "http://b.test:-1/",
        "http://b.test:abc/",
        "http:///x",
        "http://",
        "http://b..test/",
        "http://.b.test/",
        "http://b.test%2f@x/",
        "http://b.test%00/",
        "http://caf\u{e9}.test/",
        "http://b t.test/",
        "http://b.test:8o/",
        "http://-/",
        "//",
        "///x",
    ] {
        assert!(
            matches!(
                err(base, bad),
                NetError::BadUrl(_) | NetError::UrlHasCredentials
            ),
            "{bad}: {:?}",
            resolve(base, bad)
        );
    }
    for p in [22u16, 25, 110, 6000, 6667, 10080, 1, 5060] {
        let e = err(base, &format!("http://b.test:{p}/x"));
        assert!(
            matches!(e, NetError::BlockedPort { port, .. } if port == p),
            "{p}: {e:?}"
        );
    }
    let e = err(base, "https://b.test:22/");
    assert!(matches!(e, NetError::BlockedPort { port: 22, .. }));
    assert!(resolve(base, "http://b.test:8080/").is_ok());
    assert!(resolve(base, "http://b.test:65535/").is_ok());
    // default port of the scheme given, even if it's on the list for the other
    assert!(resolve(base, "http://b.test:80/").is_ok());
}

#[test]
fn resolve_controls_and_lengths() {
    let base = "http://a.test/";
    assert!(matches!(err(base, "/a\u{1}b"), NetError::BadUrl(_)));
    assert!(matches!(err(base, "/a\u{7f}b"), NetError::BadUrl(_)));
    assert!(matches!(err(base, "/a\u{0}b"), NetError::BadUrl(_)));
    assert!(matches!(err(base, "/a\u{1b}[31m"), NetError::BadUrl(_)));
    // tab/newline are dropped, leading/trailing controls and spaces trimmed
    assert_eq!(http_text(base, "\u{1}\u{2} /x \u{3}"), "http://a.test/x");
    assert_eq!(http_text(base, "ht\ntp://b.test/"), "http://b.test/");
    assert_eq!(http_text(base, "http://b.te\tst/"), "http://b.test/");
    let long = format!("/{}", "a".repeat(8192));
    assert!(matches!(err(base, &long), NetError::UrlTooLong { .. }));
    let ok = format!("/{}", "a".repeat(8000));
    assert!(resolve(base, &ok).is_ok());
    let huge = format!("http://b.test/{}", "a".repeat(2_000_000));
    assert!(matches!(err(base, &huge), NetError::UrlTooLong { .. }));
    // multi-byte characters are counted as characters, not bytes
    let wide = format!("/{}", "\u{e9}".repeat(4000));
    assert!(resolve(base, &wide).is_ok());
}

#[test]
fn resolve_blob_and_data_kinds() {
    let base = "http://a.test/";
    assert_eq!(
        resolve(base, "blob:http://a.test/1234"),
        Ok(Target::Blob("blob:http://a.test/1234".into()))
    );
    match resolve(base, "data:text/plain,hi") {
        Ok(Target::Data(d)) => {
            assert_eq!(d.body, b"hi");
            assert_eq!(d.mime, "text/plain");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn data_urls() {
    let max = 1 << 20;
    let d = |s: &str| decode_data_url(s, max);
    let ok = d("text/plain,hello%20world").unwrap();
    assert_eq!(
        (ok.mime.as_str(), ok.body.as_slice()),
        ("text/plain", b"hello world".as_slice())
    );
    let ok = d(",plain").unwrap();
    assert_eq!(ok.mime, "text/plain;charset=US-ASCII");
    assert_eq!(ok.body, b"plain");
    let ok = d(";charset=utf-8,x").unwrap();
    assert_eq!(ok.mime, "text/plain;charset=utf-8");
    let ok = d("application/json;base64,eyJhIjoxfQ==").unwrap();
    assert_eq!(
        (ok.mime.as_str(), ok.body.as_slice()),
        ("application/json", br#"{"a":1}"#.as_slice())
    );
    let ok = d("text/plain;BASE64,aGk").unwrap();
    assert_eq!(ok.body, b"hi");
    let ok = d("text/plain; base64,aG k=").unwrap();
    assert_eq!(ok.body, b"hi");
    let ok = d("text/html,<b>x</b>#frag").unwrap();
    assert_eq!(ok.body, b"<b>x</b>");
    let ok = d("text/plain,%zz%4").unwrap();
    assert_eq!(ok.body, b"%zz%4");
    let ok = d("text/plain,a%2Cb").unwrap();
    assert_eq!(ok.body, b"a,b");
    let ok = d("text/plain,").unwrap();
    assert!(ok.body.is_empty());
    let ok = d("not a mime,x").unwrap();
    assert_eq!(ok.mime, "text/plain;charset=US-ASCII");
    let ok = d("text/plain;x=\u{1},x").unwrap();
    assert_eq!(ok.mime, "text/plain;charset=US-ASCII");
    for bad in [
        "text/plain",
        "text/plain;base64,@@@",
        "text/plain;base64,a",
        "text/plain;base64,aaaaa",
        "text/plain;base64,a=aa",
        "text/plain;base64,====",
        "text/plain;base64,_-_-",
    ] {
        assert!(matches!(d(bad), Err(NetError::BadUrl(_))), "{bad}");
    }
    // the size limit applies to the decoded body
    let big = format!("text/plain,{}", "a".repeat(100));
    assert!(matches!(
        decode_data_url(&big, 50),
        Err(NetError::ResponseTooLarge { max: 50 })
    ));
    assert!(decode_data_url(&big, 100).is_ok());
    let b64 = format!("text/plain;base64,{}", "QUJD".repeat(100));
    assert!(matches!(
        decode_data_url(&b64, 100),
        Err(NetError::BadUrl(_) | NetError::ResponseTooLarge { .. })
    ));
    assert_eq!(decode_data_url(&b64, 300).unwrap().body.len(), 300);
}

#[test]
fn base64_table() {
    let rows: [(&str, Option<&[u8]>); 18] = [
        ("", Some(b"")),
        ("QQ==", Some(b"A")),
        ("QQ", Some(b"A")),
        ("QUI=", Some(b"AB")),
        ("QUI", Some(b"AB")),
        ("QUJD", Some(b"ABC")),
        ("QUJDRA==", Some(b"ABCD")),
        ("Q U\nJ\tD", Some(b"ABC")),
        ("+/+/", Some(&[0xfb, 0xff, 0xbf])),
        ("Q", None),
        ("QQ=", None),
        ("QQ===", None),
        ("=QQ", None),
        ("QQ==QQ==", None),
        ("QUJD!", None),
        ("-_-_", None),
        ("QU=D", None),
        ("é", None),
    ];
    for (s, want) in rows {
        assert_eq!(
            base64_decode(s.as_bytes(), 1 << 20).as_deref(),
            want,
            "{s:?}"
        );
    }
    assert_eq!(base64_decode(b"QUJD", 2), None);
    assert_eq!(base64_decode(b"QUJD", 3).unwrap(), b"ABC");
}

#[test]
fn page_cannot_pick_the_origin_with_userinfo_tricks() {
    // Every one of these must resolve to a host we can name, never the one after a trick.
    let base = "http://a.test/";
    for (input, host) in [
        ("http://b.test/@c.test", "b.test"),
        ("http://b.test?@c.test", "b.test"),
        ("http://b.test#@c.test", "b.test"),
        ("//b.test/@c.test", "b.test"),
        ("http://b.test:80/@c.test", "b.test"),
        ("/@c.test", "a.test"),
    ] {
        match resolve(base, input) {
            Ok(Target::Http(url)) => assert_eq!(url.host, host, "{input}"),
            other => panic!("{input}: {other:?}"),
        }
    }
}

#[test]
fn address_spaces_of_embedded_ipv4() {
    use AddressSpace::*;
    let rows = [
        ("64:ff9b::7f00:1", Loopback),
        ("64:ff9b::a00:1", Private),
        ("64:ff9b::808:808", Public),
        ("2002:7f00:1::", Loopback),
        ("2002:c0a8:101::1", Private),
        ("2002:808:808::", Public),
        ("64:ff9b:1::7f00:1", Public),
        ("::ffff:7f00:1", Loopback),
        ("::ffff:c0a8:1", Private),
    ];
    for (host, want) in rows {
        assert_eq!(address_space_of_host(host), want, "{host}");
    }
}

#[test]
fn address_space_of_resolved_bytes() {
    use AddressSpace::*;
    assert_eq!(address_space_of_ip(&[127, 0, 0, 1]), Some(Loopback));
    assert_eq!(address_space_of_ip(&[10, 0, 2, 2]), Some(Private));
    assert_eq!(address_space_of_ip(&[8, 8, 8, 8]), Some(Public));
    let mut v6 = [0u8; 16];
    v6[15] = 1;
    assert_eq!(address_space_of_ip(&v6), Some(Loopback));
    let mut mapped = [0u8; 16];
    mapped[10] = 0xff;
    mapped[11] = 0xff;
    mapped[12..].copy_from_slice(&[192, 168, 1, 1]);
    assert_eq!(address_space_of_ip(&mapped), Some(Private));
    let mut ula = [0u8; 16];
    ula[0] = 0xfd;
    assert_eq!(address_space_of_ip(&ula), Some(Private));
    let mut public = [0u8; 16];
    public[0] = 0x26;
    public[1] = 0x06;
    assert_eq!(address_space_of_ip(&public), Some(Public));
    for len in [0usize, 1, 3, 5, 8, 15, 17, 100] {
        assert_eq!(address_space_of_ip(&vec![1u8; len]), None, "{len}");
    }
}

#[test]
fn blob_with_a_user_name_is_refused() {
    let base = "http://a.test/";
    for bad in [
        "blob:http://u\u{40}a.test/1",
        "blob:http://a.test\u{40}evil.test/1",
        "blob:https://u:p\u{40}a.test/1",
    ] {
        assert_eq!(err(base, bad), NetError::UrlHasCredentials, "{bad}");
    }
}
