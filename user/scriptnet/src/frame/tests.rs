//! Host tests of the frame rules: table tests for every parser and decision, and a fuzz of
//! the header parsers and the history against hostile input.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use browserkit::cookies::Context;
use browserkit::netpolicy::ReferrerPolicy;
use milkweb::url::Url;

use super::ancestors::*;
use super::history::*;
use super::load::*;
use super::nav::*;
use super::sandbox::*;

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn hs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn anc(list: &[&str]) -> Vec<Option<Url>> {
    list.iter().map(|s| Some(u(s))).collect()
}

// ---- sandbox ----

#[test]
fn sandbox_attribute_values() {
    // (attribute, scripts, opaque origin, forms, popups, modals, downloads)
    let rows: &[(&str, bool, bool, bool, bool, bool, bool)] = &[
        ("", true, true, true, true, true, true),
        ("   ", true, true, true, true, true, true),
        ("allow-scripts", false, true, true, true, true, true),
        ("allow-same-origin", true, false, true, true, true, true),
        ("allow-scripts allow-same-origin", false, false, true, true, true, true),
        ("ALLOW-FORMS", true, true, false, true, true, true),
        ("allow-popups", true, true, true, false, true, true),
        ("allow-modals", true, true, true, true, false, true),
        ("allow-downloads", true, true, true, true, true, false),
        ("allow-scripts\tallow-forms\nallow-popups  allow-modals", false, true, false, false, false, true),
        ("allow-nonsense allow-scripts", false, true, true, true, true, true),
    ];
    for (attr, scripts, origin, forms, popups, modals, downloads) in rows {
        let (s, _) = Sandbox::parse(attr);
        assert!(s.is_sandboxed(), "{attr:?}");
        assert_eq!(s.no_scripts(), *scripts, "scripts {attr:?}");
        assert_eq!(s.opaque_origin(), *origin, "origin {attr:?}");
        assert_eq!(s.no_forms(), *forms, "forms {attr:?}");
        assert_eq!(s.no_popups(), *popups, "popups {attr:?}");
        assert_eq!(s.no_modals(), *modals, "modals {attr:?}");
        assert_eq!(s.no_downloads(), *downloads, "downloads {attr:?}");
    }
}

#[test]
fn sandbox_absent_attribute_is_no_sandbox() {
    let s = from_attribute(None);
    assert!(!s.is_sandboxed());
    assert!(!s.no_scripts() && !s.opaque_origin() && !s.no_forms() && !s.no_popups());
    assert!(s.top_navigation_allowed(false));
    assert!(from_attribute(Some("")).is_sandboxed());
}

#[test]
fn sandbox_unknown_tokens_are_reported() {
    let (_, unknown) = Sandbox::parse("allow-scripts allow-teleport Allow-Forms bogus");
    assert_eq!(unknown, vec!["allow-teleport".to_string(), "bogus".to_string()]);
}

#[test]
fn sandbox_top_navigation_tokens() {
    let (s, _) = Sandbox::parse("");
    assert!(!s.top_navigation_allowed(true));
    assert!(!s.top_navigation_allowed(false));
    let (s, _) = Sandbox::parse("allow-top-navigation");
    assert!(s.top_navigation_allowed(false));
    assert!(s.top_navigation_allowed(true));
    let (s, _) = Sandbox::parse("allow-top-navigation-by-user-activation");
    assert!(!s.top_navigation_allowed(false));
    assert!(s.top_navigation_allowed(true));
    // Both: the full one wins.
    let (s, _) = Sandbox::parse("allow-top-navigation-by-user-activation allow-top-navigation");
    assert!(s.top_navigation_allowed(false));
}

#[test]
fn sandbox_merge_is_the_union_of_restrictions() {
    let (a, _) = Sandbox::parse("allow-scripts allow-forms allow-top-navigation");
    let (b, _) = Sandbox::parse("allow-scripts allow-same-origin");
    let m = a.merge(b);
    assert!(!m.no_scripts()); // both allow
    assert!(m.no_forms()); // b forbids
    assert!(!m.opaque_origin() || a.opaque_origin()); // a forbids same-origin
    assert!(m.opaque_origin());
    assert!(!m.top_navigation_allowed(true)); // b forbids top navigation
    // none merged with something is that something
    assert_eq!(Sandbox::NONE.merge(a), a);
    assert_eq!(a.merge(Sandbox::NONE), a);
    // by-activation vs free: by-activation
    let (c, _) = Sandbox::parse("allow-top-navigation-by-user-activation");
    let (d, _) = Sandbox::parse("allow-top-navigation");
    let m = c.merge(d);
    assert!(!m.top_navigation_allowed(false));
    assert!(m.top_navigation_allowed(true));
    assert_eq!(c.merge(c), c);
    assert_eq!(a.merge(a), a);
}

#[test]
fn sandbox_escape_warning() {
    let (s, _) = Sandbox::parse("allow-scripts allow-same-origin");
    assert_eq!(s.warnings().len(), 1);
    let (s, _) = Sandbox::parse("allow-scripts");
    assert!(s.warnings().is_empty());
    assert!(Sandbox::NONE.warnings().is_empty());
}

// ---- X-Frame-Options ----

fn xfo(v: &str) -> XfoReading {
    parse_xfo(&hs(&[("x-frame-options", v)]))
}

#[test]
fn xfo_values() {
    assert_eq!(xfo("DENY").xfo, Xfo::Deny);
    assert_eq!(xfo("deny").xfo, Xfo::Deny);
    assert_eq!(xfo("  SameOrigin ").xfo, Xfo::SameOrigin);
    assert_eq!(xfo("ALLOWALL").xfo, Xfo::None);
    assert_eq!(xfo("").xfo, Xfo::None);
    assert_eq!(xfo("sameorigin, sameorigin").xfo, Xfo::SameOrigin);
    // Obsolete and unknown values are ignored and noted.
    let r = xfo("ALLOW-FROM https://a.test/");
    assert_eq!(r.xfo, Xfo::None);
    assert_eq!(r.notes.len(), 1);
    let r = xfo("whatever");
    assert_eq!(r.xfo, Xfo::None);
    assert_eq!(r.notes.len(), 1);
    // Conflicts fall back to DENY.
    assert_eq!(xfo("DENY, SAMEORIGIN").xfo, Xfo::Deny);
    assert_eq!(xfo("SAMEORIGIN, bogus").xfo, Xfo::Deny);
    let two = parse_xfo(&hs(&[("X-Frame-Options", "SAMEORIGIN"), ("x-frame-options", "DENY")]));
    assert_eq!(two.xfo, Xfo::Deny);
    assert!(!two.notes.is_empty());
    let same = parse_xfo(&hs(&[("X-Frame-Options", "DENY"), ("x-frame-options", "deny")]));
    assert_eq!(same.xfo, Xfo::Deny);
    assert!(same.notes.is_empty());
    assert_eq!(parse_xfo(&[]).xfo, Xfo::None);
}

fn embed(h: &[(&str, &str)], url: &str, ancestors: &[&str]) -> Embedding {
    check_embedding(&hs(h), &u(url), &anc(ancestors))
}

#[test]
fn xfo_deny_and_sameorigin_enforced() {
    let page = "http://a.test/page";
    let top = ["http://a.test/"];
    assert!(embed(&[("x-frame-options", "DENY")], page, &top).blocked.is_some());
    assert!(embed(&[("x-frame-options", "SAMEORIGIN")], page, &top).blocked.is_none());
    assert!(embed(&[("x-frame-options", "SAMEORIGIN")], page, &["http://b.test/"]).blocked.is_some());
    // scheme, host and port all count
    assert!(embed(&[("x-frame-options", "SAMEORIGIN")], page, &["https://a.test/"]).blocked.is_some());
    assert!(embed(&[("x-frame-options", "SAMEORIGIN")], page, &["http://a.test:81/"]).blocked.is_some());
    assert!(embed(&[("x-frame-options", "SAMEORIGIN")], page, &["http://www.a.test/"]).blocked.is_some());
    // Every ancestor must match, the top page included.
    assert!(embed(&[("x-frame-options", "SAMEORIGIN")], page, &["http://b.test/", "http://a.test/x"]).blocked.is_some());
    assert!(embed(&[("x-frame-options", "SAMEORIGIN")], page, &["http://a.test/", "http://b.test/x"]).blocked.is_some());
    assert!(embed(&[("x-frame-options", "SAMEORIGIN")], page, &["http://a.test/", "http://a.test/x"]).blocked.is_none());
    // a document with no address in the chain is not the same origin as anything
    let chain = vec![Some(u("http://a.test/")), None];
    assert!(check_embedding(&hs(&[("x-frame-options", "SAMEORIGIN")]), &u(page), &chain).blocked.is_some());
    // nothing said: allowed
    assert!(embed(&[], page, &top).blocked.is_none());
    // a top-level document is never refused
    assert!(check_embedding(&hs(&[("x-frame-options", "DENY")]), &u(page), &[]).blocked.is_none());
    // messages
    let b = embed(&[("x-frame-options", "deny")], page, &top).blocked.unwrap();
    assert!(b.console(&u(page)).contains("X-Frame-Options"));
    assert!(b.reason().contains("frame"));
}

// ---- frame-ancestors ----

fn fa(policy: &str, url: &str, ancestors: &[&str]) -> bool {
    embed(&[("content-security-policy", policy)], url, ancestors)
        .blocked
        .is_none()
}

#[test]
fn csp_frame_ancestors_none_self_any() {
    let me = "http://a.test/page";
    let top = ["http://a.test/"];
    assert!(!fa("frame-ancestors 'none'", me, &top));
    assert!(!fa("frame-ancestors", me, &top));
    assert!(!fa("frame-ancestors ;", me, &top));
    assert!(fa("frame-ancestors 'self'", me, &top));
    assert!(!fa("frame-ancestors 'self'", me, &["http://b.test/"]));
    assert!(fa("frame-ancestors *", me, &["http://b.test/"]));
    assert!(fa("frame-ancestors *", me, &["https://b.test:8443/"]));
    // 'none' with others is ignored
    assert!(fa("frame-ancestors 'none' http://b.test", me, &["http://b.test/"]));
    assert!(!fa("frame-ancestors 'none' http://b.test", me, &["http://c.test/"]));
    // case
    assert!(fa("FRAME-ANCESTORS 'SELF'", me, &top));
    // other directives don't matter
    assert!(fa("default-src 'none'; img-src *", me, &top));
    assert!(fa("script-src 'self'", me, &["http://z.test/"]));
}

#[test]
fn csp_frame_ancestors_hosts_and_schemes() {
    let me = "https://a.test/page";
    let rows: &[(&str, &str, bool)] = &[
        ("frame-ancestors https://b.test", "https://b.test/", true),
        ("frame-ancestors https://b.test", "http://b.test/", false),
        ("frame-ancestors https://b.test", "https://b.test:8443/", false),
        ("frame-ancestors https://b.test:8443", "https://b.test:8443/", true),
        ("frame-ancestors https://b.test:*", "https://b.test:8443/", true),
        ("frame-ancestors https://*.b.test", "https://www.b.test/", true),
        ("frame-ancestors https://*.b.test", "https://x.y.b.test/", true),
        ("frame-ancestors https://*.b.test", "https://b.test/", false),
        ("frame-ancestors https://*.b.test", "https://notb.test/", false),
        ("frame-ancestors *.b.test", "https://www.b.test/", true),
        ("frame-ancestors *.b.test", "http://www.b.test/", false), // the framed page is https
        ("frame-ancestors b.test", "https://b.test/", true),
        ("frame-ancestors b.test", "https://B.TEST/", true),
        ("frame-ancestors https:", "https://q.test/", true),
        ("frame-ancestors https:", "http://q.test/", false),
        ("frame-ancestors http:", "https://q.test/", true),
        ("frame-ancestors http:", "http://q.test/", true),
        ("frame-ancestors http://b.test", "https://b.test/", true), // http covers its https upgrade
        ("frame-ancestors https://*", "https://anything.test/", true),
        ("frame-ancestors 127.0.0.1", "https://127.0.0.1/", true),
        ("frame-ancestors 127.0.0.1", "https://127.0.0.2/", false),
        ("frame-ancestors [::1]", "https://[::1]/", true),
        // A path makes the source invalid: ignored.
        ("frame-ancestors https://b.test/app", "https://b.test/app", false),
        ("frame-ancestors https://b.test/", "https://b.test/", false),
        // Other keywords are ignored.
        ("frame-ancestors 'unsafe-inline' 'nonce-abc' 'sha256-xyz'", "https://b.test/", false),
        ("frame-ancestors 'unsafe-inline' https://b.test", "https://b.test/", true),
        // Junk is ignored without hurting the rest.
        ("frame-ancestors ??? https://b.test", "https://b.test/", true),
        ("frame-ancestors https://b.test:0", "https://b.test/", false),
        ("frame-ancestors https://b.test:99999", "https://b.test/", false),
    ];
    for (pol, anc_url, want) in rows {
        assert_eq!(fa(pol, me, &[anc_url]), *want, "{pol} / ancestor {anc_url}");
    }
}

#[test]
fn csp_frame_ancestors_scheme_defaults() {
    // A source without a scheme matches the framed page's scheme (or its upgrade).
    assert!(fa("frame-ancestors b.test", "http://a.test/", &["http://b.test/"]));
    assert!(fa("frame-ancestors b.test", "http://a.test/", &["https://b.test/"]));
    assert!(!fa("frame-ancestors b.test", "https://a.test/", &["http://b.test/"]));
    // No port: the default one only.
    assert!(!fa("frame-ancestors b.test", "http://a.test/", &["http://b.test:8080/"]));
    assert!(fa("frame-ancestors b.test:8080", "http://a.test/", &["http://b.test:8080/"]));
    // 'self' includes the port.
    assert!(!fa("frame-ancestors 'self'", "http://a.test:8080/", &["http://a.test/"]));
    assert!(fa("frame-ancestors 'self'", "http://a.test:8080/", &["http://a.test:8080/"]));
    // ... and allows the https upgrade of an http page.
    assert!(fa("frame-ancestors 'self'", "http://a.test/", &["https://a.test/"]));
    assert!(!fa("frame-ancestors 'self'", "https://a.test/", &["http://a.test/"]));
}

#[test]
fn csp_frame_ancestors_every_ancestor_and_every_policy() {
    let me = "http://a.test/page";
    assert!(!fa("frame-ancestors http://b.test", me, &["http://b.test/", "http://c.test/"]));
    assert!(fa("frame-ancestors http://b.test http://c.test", me, &["http://b.test/", "http://c.test/"]));
    // Two policies in one header (comma) or in two headers: all must allow.
    let ok = embed(
        &[("content-security-policy", "frame-ancestors http://b.test, frame-ancestors http://b.test http://c.test")],
        me,
        &["http://b.test/"],
    );
    assert!(ok.blocked.is_none());
    let no = embed(
        &[("content-security-policy", "frame-ancestors http://b.test, frame-ancestors http://c.test")],
        me,
        &["http://b.test/"],
    );
    assert!(no.blocked.is_some());
    let no2 = embed(
        &[
            ("content-security-policy", "frame-ancestors http://b.test"),
            ("Content-Security-Policy", "frame-ancestors 'none'"),
        ],
        me,
        &["http://b.test/"],
    );
    assert!(no2.blocked.is_some());
    // The first of two identical directives counts.
    assert!(fa("frame-ancestors http://b.test; frame-ancestors 'none'", me, &["http://b.test/"]));
    // Report-only policies are not enforced.
    let ro = embed(&[("content-security-policy-report-only", "frame-ancestors 'none'")], me, &["http://b.test/"]);
    assert!(ro.blocked.is_none());
}

#[test]
fn csp_frame_ancestors_overrides_x_frame_options() {
    let me = "http://a.test/page";
    let e = embed(
        &[("x-frame-options", "DENY"), ("content-security-policy", "frame-ancestors http://b.test")],
        me,
        &["http://b.test/"],
    );
    assert!(e.blocked.is_none());
    assert!(!e.notes.is_empty());
    let e = embed(
        &[("x-frame-options", "SAMEORIGIN"), ("content-security-policy", "frame-ancestors 'none'")],
        me,
        &["http://a.test/"],
    );
    assert!(matches!(e.blocked, Some(Blocked::FrameAncestors(_))));
    // A CSP without frame-ancestors leaves X-Frame-Options in force.
    let e = embed(
        &[("x-frame-options", "DENY"), ("content-security-policy", "default-src 'self'")],
        me,
        &["http://a.test/"],
    );
    assert!(matches!(e.blocked, Some(Blocked::XFrameOptions(_))));
}

#[test]
fn csp_sandbox_directive() {
    let s = sandbox_of(&hs(&[("content-security-policy", "sandbox allow-scripts")]));
    assert!(s.is_sandboxed());
    assert!(!s.no_scripts());
    assert!(s.opaque_origin());
    let s = sandbox_of(&hs(&[("content-security-policy", "default-src 'self'")]));
    assert!(!s.is_sandboxed());
    let s = sandbox_of(&hs(&[("content-security-policy", "sandbox")]));
    assert!(s.no_scripts());
    // Two policies: the union of restrictions.
    let s = sandbox_of(&hs(&[("content-security-policy", "sandbox allow-scripts allow-forms, sandbox allow-scripts")]));
    assert!(!s.no_scripts());
    assert!(s.no_forms());
}

// ---- Loading ----

fn ask<'a>(src: Option<&'a str>, base: Option<&'a Url>, ancestors: &'a [Option<Url>], n: usize) -> Ask<'a> {
    Ask {
        src,
        srcdoc: None,
        base,
        ancestors,
        frames_now: n,
    }
}

#[test]
fn frame_source_kinds() {
    let base = u("http://a.test/dir/page.html");
    let top = anc(&["http://a.test/dir/page.html"]);
    assert_eq!(source(&ask(None, Some(&base), &top, 0)), Ok(Source::Blank));
    assert_eq!(source(&ask(Some(""), Some(&base), &top, 0)), Ok(Source::Blank));
    assert_eq!(source(&ask(Some("  about:blank "), Some(&base), &top, 0)), Ok(Source::Blank));
    assert_eq!(source(&ask(Some("ABOUT:BLANK"), Some(&base), &top, 0)), Ok(Source::Blank));
    assert_eq!(
        source(&ask(Some("inner.html"), Some(&base), &top, 0)),
        Ok(Source::Http(u("http://a.test/dir/inner.html")))
    );
    assert_eq!(
        source(&ask(Some("/x?y=1"), Some(&base), &top, 0)),
        Ok(Source::Http(u("http://a.test/x?y=1")))
    );
    assert_eq!(
        source(&ask(Some("https://b.test/z"), Some(&base), &top, 0)),
        Ok(Source::Http(u("https://b.test/z")))
    );
    match source(&ask(Some("data:text/html,<p>hi"), Some(&base), &top, 0)) {
        Ok(Source::Data(d)) => {
            assert!(d.mime.starts_with("text/html"));
            assert_eq!(d.body, b"<p>hi");
        }
        other => panic!("{other:?}"),
    }
    // srcdoc wins over src
    let mut a = ask(Some("inner.html"), Some(&base), &top, 0);
    a.srcdoc = Some("<b>x</b>");
    assert_eq!(source(&a), Ok(Source::Srcdoc("<b>x</b>".to_string())));
    // Refused schemes.
    for bad in ["javascript:alert(1)", "blob:http://a.test/1", "file:///etc/passwd", "ftp://a.test/", "about:srcdoc", "chrome://x"] {
        assert!(source(&ask(Some(bad), Some(&base), &top, 0)).is_err(), "{bad}");
    }
    // Strict like fetch.
    for bad in [concat!("http://user:pw", "@", "a.test/"), "http://a.test\\x", "http://127.1/", "http://a.test:22/"] {
        assert!(source(&ask(Some(bad), Some(&base), &top, 0)).is_err(), "{bad}");
    }
    assert_eq!(
        source(&ask(Some("http://a.test:25/"), Some(&base), &top, 0)),
        Err(Refusal::BadPort { url: "http://a.test:25/".to_string(), port: 25 })
    );
    // No base (the parent is a data: document): only absolute addresses.
    assert!(source(&ask(Some("rel.html"), None, &top, 0)).is_err());
    assert!(source(&ask(Some("http://b.test/"), None, &top, 0)).is_ok());
}

#[test]
fn frame_srcdoc_is_capped() {
    let big = "x".repeat(MAX_SRCDOC + 100);
    let top = anc(&["http://a.test/"]);
    let mut a = ask(None, None, &top, 0);
    a.srcdoc = Some(&big);
    match source(&a) {
        Ok(Source::Srcdoc(d)) => assert_eq!(d.len(), MAX_SRCDOC),
        other => panic!("{other:?}"),
    }
    // ... on a character boundary
    let multi = "é".repeat(MAX_SRCDOC);
    let mut a = ask(None, None, &top, 0);
    a.srcdoc = Some(&multi);
    match source(&a) {
        Ok(Source::Srcdoc(d)) => assert!(d.len() <= MAX_SRCDOC && d.chars().all(|c| c == 'é')),
        other => panic!("{other:?}"),
    }
}

#[test]
fn frame_depth_and_count_limits() {
    let base = u("http://a.test/");
    let mut chain = Vec::new();
    for _ in 0..MAX_DEPTH {
        chain.push(Some(base.clone()));
    }
    // Level MAX_DEPTH + 1 (MAX_DEPTH + 1 ancestors with the top page): refused.
    chain.push(Some(base.clone()));
    assert_eq!(source(&ask(Some("/x"), Some(&base), &chain, 3)), Err(Refusal::TooDeep));
    // Level MAX_DEPTH: fine.
    assert!(source(&ask(Some("/x"), Some(&base), &chain[..MAX_DEPTH], 3)).is_ok());
    // Count.
    let top = anc(&["http://a.test/"]);
    assert!(source(&ask(Some("/x"), Some(&base), &top, MAX_FRAMES - 1)).is_ok());
    assert_eq!(source(&ask(Some("/x"), Some(&base), &top, MAX_FRAMES)), Err(Refusal::TooMany));
    // about:blank and srcdoc frames count too.
    assert_eq!(source(&ask(None, Some(&base), &top, MAX_FRAMES)), Err(Refusal::TooMany));
    // Messages exist.
    assert!(Refusal::TooDeep.console("/x").contains("deep"));
    assert!(Refusal::TooMany.console("/x").contains("frames"));
    assert!(!Refusal::TooMany.reason().is_empty());
}

#[test]
fn mixed_content_in_frames() {
    let https_top = anc(&["https://a.test/"]);
    let http_top = anc(&["http://a.test/"]);
    assert!(matches!(
        check_hop(&https_top, &u("http://b.test/")),
        Err(Refusal::MixedContent { .. })
    ));
    assert!(check_hop(&https_top, &u("https://b.test/")).is_ok());
    assert!(check_hop(&http_top, &u("http://b.test/")).is_ok());
    assert!(check_hop(&http_top, &u("https://b.test/")).is_ok());
    // This machine is exempt, as for fetch.
    assert!(check_hop(&https_top, &u("http://localhost:8000/")).is_ok());
    assert!(check_hop(&https_top, &u("http://127.0.0.1:8000/")).is_ok());
    // Any secure ancestor counts, not just the parent.
    let chain = anc(&["https://a.test/", "http://b.test/"]);
    assert!(check_hop(&chain, &u("http://c.test/")).is_err());
    let chain = anc(&["http://a.test/", "https://b.test/"]);
    assert!(check_hop(&chain, &u("http://c.test/")).is_err());
    // A top-level page is no frame.
    assert!(check_hop(&[], &u("http://c.test/")).is_ok());
    // And through source().
    let base = u("https://a.test/");
    assert!(matches!(
        source(&ask(Some("http://b.test/"), Some(&base), &https_top, 0)),
        Err(Refusal::MixedContent { .. })
    ));
    let m = Refusal::MixedContent { page: "https://a.test/".into(), url: "http://b.test/".into() };
    assert!(m.console("x").contains("Mixed Content"));
    // Bad ports apply to redirect hops too.
    assert!(matches!(check_hop(&http_top, &u("http://b.test:6667/")), Err(Refusal::BadPort { .. })));
}

#[test]
fn frame_cookie_context_follows_the_whole_chain() {
    let site = |c: Context| match c {
        Context::Subresource { page_site } => page_site,
        _ => panic!("not a subresource context"),
    };
    let same = cookie_context(&anc(&["http://www.milk.test/"]), &u("http://api.milk.test/f"));
    assert_eq!(site(same), "milk.test");
    // A cross-site frame gets a context that matches no site.
    let cross = site(cookie_context(&anc(&["http://www.milk.test/"]), &u("http://other.test/f")));
    for host in ["other.test", "milk.test", "www.milk.test", ""] {
        assert_ne!(cross, browserkit::cookies::site_of(host), "{host}");
    }
    // a.test > b.test > a.test: a third party at the bottom.
    let sneaky = site(cookie_context(&anc(&["http://a.test/", "http://b.test/f"]), &u("http://a.test/g")));
    assert_ne!(sneaky, "a.test");
    // same site all the way down
    let deep = site(cookie_context(&anc(&["http://a.test/", "http://x.a.test/f"]), &u("http://y.a.test/g")));
    assert_eq!(deep, "a.test");
    // schemeful: https page, http frame (not blocked here) is another site
    let mixed = site(cookie_context(&anc(&["https://a.test/"]), &u("http://a.test/")));
    assert_ne!(mixed, "a.test");
    // an ancestor with no address
    let chain = vec![Some(u("http://a.test/")), None];
    assert_ne!(site(cookie_context(&chain, &u("http://a.test/"))), "a.test");
}

#[test]
fn document_cookie_in_frames() {
    let top = anc(&["http://a.test/"]);
    assert!(cookies_allowed(&top, &u("http://a.test/f"), Sandbox::NONE));
    assert!(cookies_allowed(&[], &u("http://a.test/"), Sandbox::NONE));
    assert!(!cookies_allowed(&top, &u("http://b.test/f"), Sandbox::NONE));
    let (sb, _) = Sandbox::parse("allow-scripts");
    assert!(!cookies_allowed(&top, &u("http://a.test/f"), sb)); // opaque origin
    let (sb, _) = Sandbox::parse("allow-scripts allow-same-origin");
    assert!(cookies_allowed(&top, &u("http://a.test/f"), sb));
}

#[test]
fn frame_referer() {
    let parent = u("https://a.test/dir/p?q=1");
    let same = u("https://a.test/inner");
    let cross = u("https://b.test/inner");
    let down = u("http://b.test/inner");
    let d = ReferrerPolicy::DEFAULT;
    assert_eq!(referer(Some(&parent), &same, None, d).as_deref(), Some("https://a.test/dir/p?q=1"));
    assert_eq!(referer(Some(&parent), &cross, None, d).as_deref(), Some("https://a.test/"));
    assert_eq!(referer(Some(&parent), &down, None, d), None);
    // The iframe's referrerpolicy wins over the parent's policy.
    assert_eq!(referer(Some(&parent), &same, Some("no-referrer"), d), None);
    assert_eq!(referer(Some(&parent), &cross, Some("unsafe-url"), d).as_deref(), Some("https://a.test/dir/p?q=1"));
    assert_eq!(referer(Some(&parent), &cross, Some("origin"), d).as_deref(), Some("https://a.test/"));
    // An unknown attribute value falls back to the parent's policy.
    assert_eq!(referer(Some(&parent), &cross, Some("bogus"), ReferrerPolicy::NoReferrer), None);
    assert_eq!(referer(Some(&parent), &cross, Some(" NO-REFERRER "), ReferrerPolicy::UnsafeUrl), None);
    // No parent address (a data: parent): none.
    assert_eq!(referer(None, &same, None, d), None);
}

#[test]
fn what_an_answer_is() {
    assert_eq!(answer_kind("text/html; charset=utf-8", None), Answer::Page);
    assert_eq!(answer_kind("TEXT/HTML", None), Answer::Page);
    assert_eq!(answer_kind("application/xhtml+xml", None), Answer::Page);
    assert_eq!(answer_kind("text/plain", None), Answer::Page);
    assert_eq!(answer_kind("", None), Answer::Page);
    assert_eq!(answer_kind("image/png", None), Answer::Image);
    assert_eq!(answer_kind("image/jpeg", None), Answer::Image);
    assert_eq!(answer_kind("application/pdf", None), Answer::Download);
    assert_eq!(answer_kind("application/octet-stream", None), Answer::Download);
    assert_eq!(answer_kind("text/html", Some("attachment; filename=x.html")), Answer::Download);
    assert_eq!(answer_kind("text/html", Some("inline")), Answer::Page);
}

#[test]
fn judge_combines_the_headers() {
    let h = hs(&[("x-frame-options", "DENY"), ("content-security-policy", "sandbox allow-forms")]);
    let (inherited, _) = Sandbox::parse("allow-scripts");
    let v = judge(&h, &u("http://a.test/f"), &anc(&["http://a.test/"]), inherited);
    assert!(v.blocked.is_some());
    assert!(v.sandbox.no_scripts()); // the CSP sandbox restricts scripts again
    assert!(v.sandbox.no_forms()); // and the parent's restrictions stay
}

// ---- Navigation ----

struct T {
    // (parent, name)
    n: Vec<(Option<u32>, &'static str)>,
}

impl FrameTree for T {
    fn top(&self) -> FrameId {
        0
    }
    fn parent(&self, f: FrameId) -> Option<FrameId> {
        self.n.get(f as usize).and_then(|x| x.0)
    }
    fn children(&self, f: FrameId) -> Vec<FrameId> {
        (0..self.n.len() as u32).filter(|c| self.n[*c as usize].0 == Some(f)).collect()
    }
    fn name(&self, f: FrameId) -> String {
        self.n.get(f as usize).map(|x| x.1.to_string()).unwrap_or_default()
    }
}

/// 0 top; 1 "left" (child of 0); 2 "right" (child of 0); 3 "inner" (child of 1); 4 "deep" (child of 3).
fn tree() -> T {
    T {
        n: vec![(None, ""), (Some(0), "left"), (Some(0), "right"), (Some(1), "inner"), (Some(3), "deep")],
    }
}

#[test]
fn target_keywords() {
    let t = tree();
    assert_eq!(choose(&t, 3, ""), Choice::Frame(3));
    assert_eq!(choose(&t, 3, "_self"), Choice::Frame(3));
    assert_eq!(choose(&t, 3, "_SELF"), Choice::Frame(3));
    assert_eq!(choose(&t, 3, "_parent"), Choice::Frame(1));
    assert_eq!(choose(&t, 0, "_parent"), Choice::Frame(0));
    assert_eq!(choose(&t, 4, "_top"), Choice::Frame(0));
    assert_eq!(choose(&t, 4, "_blank"), Choice::NewTab);
    assert_eq!(choose(&t, 4, "_Blank"), Choice::NewTab);
    assert_eq!(choose(&t, 4, "_weird"), Choice::NewTab);
}

#[test]
fn target_names() {
    let t = tree();
    assert_eq!(choose(&t, 0, "left"), Choice::Frame(1));
    assert_eq!(choose(&t, 0, "inner"), Choice::Frame(3));
    assert_eq!(choose(&t, 0, "deep"), Choice::Frame(4));
    // from inside: siblings and cousins through the ancestors
    assert_eq!(choose(&t, 1, "right"), Choice::Frame(2));
    assert_eq!(choose(&t, 4, "right"), Choice::Frame(2));
    assert_eq!(choose(&t, 2, "inner"), Choice::Frame(3));
    // its own name
    assert_eq!(choose(&t, 3, "inner"), Choice::Frame(3));
    // nobody has it: a new tab
    assert_eq!(choose(&t, 3, "nobody"), Choice::NewTab);
    // the nearest one wins
    let t2 = T {
        n: vec![(None, ""), (Some(0), "a"), (Some(0), "b"), (Some(1), "x"), (Some(2), "x")],
    };
    assert_eq!(choose(&t2, 1, "x"), Choice::Frame(3));
    assert_eq!(choose(&t2, 2, "x"), Choice::Frame(4));
}

#[test]
fn target_names_survive_hostile_trees() {
    // A cycle in the parent links must not hang the lookup.
    struct Loop;
    impl FrameTree for Loop {
        fn top(&self) -> FrameId {
            0
        }
        fn parent(&self, f: FrameId) -> Option<FrameId> {
            Some((f + 1) % 3)
        }
        fn children(&self, f: FrameId) -> Vec<FrameId> {
            vec![(f + 1) % 3]
        }
        fn name(&self, _: FrameId) -> String {
            String::from("n")
        }
    }
    assert_eq!(choose(&Loop, 0, "zzz"), Choice::NewTab);
    let _ = relation(&Loop, 0, 5);
    let _ = allowed(&Loop, 0, &Choice::Frame(2), Sandbox::FULL, false);
}

#[test]
fn relations_between_frames() {
    let t = tree();
    assert_eq!(relation(&t, 1, 1), Relation::Same);
    assert_eq!(relation(&t, 1, 4), Relation::Descendant);
    assert_eq!(relation(&t, 4, 1), Relation::Ancestor);
    assert_eq!(relation(&t, 4, 0), Relation::Top);
    assert_eq!(relation(&t, 0, 4), Relation::Descendant);
    assert_eq!(relation(&t, 1, 2), Relation::Other);
    assert_eq!(relation(&t, 4, 2), Relation::Other);
}

#[test]
fn sandboxed_navigation_rules() {
    let t = tree();
    let (full, _) = Sandbox::parse("");
    let (scripts, _) = Sandbox::parse("allow-scripts allow-top-navigation");
    let (by_act, _) = Sandbox::parse("allow-scripts allow-top-navigation-by-user-activation");
    let (popups, _) = Sandbox::parse("allow-popups");
    let go = |sb: Sandbox, from: u32, to: Choice, act: bool| allowed(&t, from, &to, sb, act);
    // Itself and its descendants: always.
    assert!(go(full, 1, Choice::Frame(1), false).is_ok());
    assert!(go(full, 1, Choice::Frame(4), false).is_ok());
    // Top page.
    assert_eq!(go(full, 3, Choice::Frame(0), true), Err(Denied::TopNavigation));
    assert!(go(scripts, 3, Choice::Frame(0), false).is_ok());
    assert_eq!(go(by_act, 3, Choice::Frame(0), false), Err(Denied::TopNavigation));
    assert!(go(by_act, 3, Choice::Frame(0), true).is_ok());
    // A non-top ancestor.
    assert!(go(full, 4, Choice::Frame(1), false).is_ok());
    // Anything else.
    assert_eq!(go(full, 1, Choice::Frame(2), false), Err(Denied::Navigation));
    assert_eq!(go(scripts, 1, Choice::Frame(2), false), Err(Denied::Navigation));
    // Unsandboxed frames navigate anywhere in the tab.
    assert!(go(Sandbox::NONE, 1, Choice::Frame(2), false).is_ok());
    assert!(go(Sandbox::NONE, 1, Choice::Frame(0), false).is_ok());
    // New tabs need allow-popups.
    assert_eq!(go(full, 1, Choice::NewTab, true), Err(Denied::Popups));
    assert!(go(popups, 1, Choice::NewTab, true).is_ok());
    assert!(go(Sandbox::NONE, 1, Choice::NewTab, false).is_ok());
    for d in [Denied::TopNavigation, Denied::Navigation, Denied::Popups] {
        assert!(!d.console().is_empty());
    }
}

// ---- History ----

fn p(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|k| k.to_string()).collect()
}

/// A page (doc 1) with frames a (doc 2) and b (doc 3), a has a frame c (doc 4).
fn page() -> SessionHistory {
    let mut h = SessionHistory::new();
    h.push_doc("http://t.test/", 1);
    h.frame_loaded(&p(&["a"]), 2, "http://t.test/a1", true, false);
    h.frame_loaded(&p(&["b"]), 3, "http://t.test/b1", true, false);
    h.frame_loaded(&p(&["a", "c"]), 4, "http://t.test/c1", true, false);
    h
}

#[test]
fn history_initial_frame_loads_do_not_add_entries() {
    let h = page();
    assert_eq!(h.len(), 1);
    let e = h.current().unwrap();
    assert_eq!(e.frames.len(), 2);
    assert_eq!(h.node(&p(&["a", "c"])).unwrap().url, "http://t.test/c1");
    assert!(!h.can_back() && !h.can_forward());
}

#[test]
fn history_frame_navigation_adds_an_entry_and_back_reloads_just_that_frame() {
    let mut h = page();
    h.frame_loaded(&p(&["b"]), 5, "http://t.test/b2", false, false);
    assert_eq!(h.len(), 2);
    assert!(h.can_back());
    // The other frames are as they were.
    assert_eq!(h.node(&p(&["a"])).unwrap().url, "http://t.test/a1");
    assert_eq!(h.node(&p(&["b"])).unwrap().url, "http://t.test/b2");
    let t = h.traverse(-1, 40).unwrap();
    assert!(!t.top_reload);
    assert_eq!(t.reloads.len(), 1);
    assert_eq!(t.reloads[0].path, p(&["b"]));
    assert_eq!(t.reloads[0].node.url, "http://t.test/b1");
    assert_eq!(h.current().unwrap().url, "http://t.test/");
    // The top page's scroll was saved in the entry left.
    assert_eq!(h.traverse(1, 0).unwrap().entry.scroll, 40);
    h.traverse(-1, 7).unwrap();
}

#[test]
fn history_back_then_a_new_navigation_drops_the_forward_entries() {
    let mut h = page();
    h.frame_loaded(&p(&["b"]), 5, "http://t.test/b2", false, false);
    h.frame_loaded(&p(&["b"]), 6, "http://t.test/b3", false, false);
    assert_eq!(h.len(), 3);
    h.traverse(-2, 0).unwrap();
    h.frame_loaded(&p(&["a"]), 7, "http://t.test/a2", false, false);
    assert_eq!(h.len(), 2);
    assert!(!h.can_forward());
    assert_eq!(h.node(&p(&["b"])).unwrap().url, "http://t.test/b1");
}

#[test]
fn history_same_address_and_replace_navigations_replace() {
    let mut h = page();
    h.frame_loaded(&p(&["b"]), 5, "http://t.test/b1", false, false); // same address
    assert_eq!(h.len(), 1);
    assert_eq!(h.node(&p(&["b"])).unwrap().doc, 5);
    h.frame_loaded(&p(&["b"]), 6, "http://t.test/b9", false, true); // location.replace
    assert_eq!(h.len(), 1);
    assert_eq!(h.node(&p(&["b"])).unwrap().url, "http://t.test/b9");
    // a navigation in a frame inside a frame
    h.frame_loaded(&p(&["a", "c"]), 7, "http://t.test/c2", false, false);
    assert_eq!(h.len(), 2);
}

#[test]
fn history_navigating_a_frame_drops_what_was_inside_it() {
    let mut h = page();
    h.frame_loaded(&p(&["a"]), 8, "http://t.test/a2", false, false);
    assert!(h.node(&p(&["a"])).unwrap().children.is_empty());
    // back: a is reloaded and c comes back with it
    let t = h.traverse(-1, 0).unwrap();
    assert_eq!(t.reloads.len(), 1);
    assert_eq!(t.reloads[0].path, p(&["a"]));
    let c = restore_for(&t.reloads, &p(&["a", "c"])).unwrap();
    assert_eq!(c.url, "http://t.test/c1");
    assert!(restore_for(&t.reloads, &p(&["a", "zzz"])).is_none());
    assert!(restore_for(&t.reloads, &p(&["b"])).is_none());
    assert!(restore_for(&t.reloads, &p(&["a"])).is_none()); // the frame itself is not "inside" itself
}

#[test]
fn history_top_page_navigation_reloads_everything() {
    let mut h = page();
    h.push_doc("http://t.test/other", 9);
    assert_eq!(h.len(), 2);
    assert!(h.current().unwrap().frames.is_empty());
    let t = h.traverse(-1, 0).unwrap();
    assert!(t.top_reload);
    assert_eq!(t.entry.frames.len(), 2);
    // the top is loaded again as doc 10: entries naming doc 1 are rebound
    h.rebind(1, 10);
    assert_eq!(h.current().unwrap().doc, 10);
    let t = h.traverse(1, 0).unwrap();
    assert!(t.top_reload);
    h.rebind(9, 11);
    let t = h.traverse(-1, 0).unwrap();
    assert!(t.top_reload);
}

#[test]
fn history_same_document_entries_keep_their_documents() {
    let mut h = page();
    h.top_same_doc("http://t.test/#x", false);
    h.frame_same_doc(&p(&["b"]), "http://t.test/b1#y", false);
    assert_eq!(h.len(), 3);
    let t = h.traverse(-1, 0).unwrap();
    assert!(!t.top_reload && t.reloads.is_empty());
    let b = t.restores.iter().find(|r| r.path == p(&["b"])).unwrap();
    assert_eq!(b.url, "http://t.test/b1");
    assert!(b.url_changed);
    let t = h.traverse(-1, 0).unwrap();
    assert!(!t.top_reload && t.reloads.is_empty());
    let top = t.restores.iter().find(|r| r.path.is_empty()).unwrap();
    assert_eq!(top.url, "http://t.test/");
    assert!(top.url_changed);
    // forward all the way
    let t = h.traverse(2, 0).unwrap();
    assert!(t.reloads.is_empty());
    // replace variants change in place
    h.top_same_doc("http://t.test/#z", true);
    h.frame_same_doc(&p(&["b"]), "http://t.test/b1#w", true);
    assert_eq!(h.len(), 3);
}

#[test]
fn history_mixed_entries_reload_only_what_differs() {
    // top fragment entry between two frame navigations
    let mut h = page();
    h.frame_loaded(&p(&["a"]), 20, "http://t.test/a2", false, false); // entry 1
    h.top_same_doc("http://t.test/#f", false); // entry 2
    h.frame_loaded(&p(&["b"]), 21, "http://t.test/b2", false, false); // entry 3
    assert_eq!(h.len(), 4);
    let t = h.traverse(-3, 0).unwrap();
    // entry 0: a is doc 2 (now 20), b is doc 3 (now 21)
    assert!(!t.top_reload);
    let paths: Vec<_> = t.reloads.iter().map(|r| r.path.clone()).collect();
    assert_eq!(paths, vec![p(&["a"]), p(&["b"])]);
    // after loading them again, rebinding keeps the series straight
    h.rebind(2, 30);
    h.rebind(3, 31);
    let t = h.traverse(3, 0).unwrap();
    let paths: Vec<_> = t.reloads.iter().map(|r| r.path.clone()).collect();
    // a: 30 -> 20 ... entry 3 still names doc 20 for a and 21 for b: both reload again
    assert_eq!(paths, vec![p(&["a"]), p(&["b"])]);
}

#[test]
fn history_traversal_bounds() {
    let mut h = page();
    assert!(h.traverse(-1, 0).is_none());
    assert!(h.traverse(1, 0).is_none());
    assert!(h.traverse(0, 0).is_some());
    let mut e = SessionHistory::new();
    assert!(e.traverse(0, 0).is_none());
    assert!(e.current().is_none());
    e.frame_loaded(&p(&["a"]), 1, "x", true, false); // no entry yet: nothing happens
    assert!(e.is_empty());
}

#[test]
fn history_frame_keys() {
    assert_eq!(key_for("main", 0, &[]), "main");
    assert_eq!(key_for("", 2, &[]), "#2");
    assert_eq!(key_for("main", 1, &["main".to_string()]), "#1");
    assert_eq!(key_for("#7", 3, &[]), "#3");
}

#[test]
fn history_is_bounded() {
    let mut h = SessionHistory::new();
    for i in 0..(MAX_ENTRIES + 50) {
        h.push_doc(&alloc::format!("http://t.test/{i}"), i as u64 + 1);
    }
    assert_eq!(h.len(), MAX_ENTRIES);
    assert_eq!(h.position(), MAX_ENTRIES - 1);
    // frames per entry are bounded
    let mut h = SessionHistory::new();
    h.push_doc("http://t.test/", 1);
    for i in 0..(MAX_NODES * 2) {
        h.frame_loaded(&p(&[&alloc::format!("f{i}")]), 100 + i as u64, "http://t.test/x", true, false);
    }
    assert!(h.current().unwrap().frames.len() <= MAX_NODES);
}

#[test]
fn history_removed_frames_are_forgotten() {
    let mut h = page();
    h.frame_removed(&p(&["a", "c"]));
    assert!(h.node(&p(&["a", "c"])).is_none());
    h.frame_removed(&p(&["b"]));
    assert_eq!(h.current().unwrap().frames.len(), 1);
    h.frame_removed(&[]);
    h.frame_removed(&p(&["nope", "x"]));
}

// ---- Hostile input ----

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[(self.next() as usize) % xs.len()]
    }
}

#[test]
fn fuzz_header_parsers_never_panic() {
    let bits = [
        "frame-ancestors", "'none'", "'self'", "*", "https:", "http://", "://", "a.test", "*.a.test", ":*", ":8080", ":0",
        ":99999999", "[::1]", "[", "]", ";", ",", " ", "\t", "\u{e9}", "sandbox", "allow-scripts", "deny", "sameorigin",
        "DENY", "'unsafe-inline'", "/", "/path", "?", "#", "\0", "\r\n", "%41", "http://a.test:80", "'nonce-x'", "..",
    ];
    let mut rng = Rng(7);
    for _ in 0..3000 {
        let mut v = String::new();
        for _ in 0..(rng.next() % 12) {
            v.push_str(rng.pick(&bits));
            if rng.next() % 3 == 0 {
                v.push(' ');
            }
        }
        let h = hs(&[
            ("content-security-policy", &v),
            ("x-frame-options", &v),
            ("X-Frame-Options", rng.pick(&bits)),
        ]);
        let chain = anc(&["http://a.test/", "https://x.a.test:8443/f"]);
        let _ = check_embedding(&h, &u("https://a.test/z"), &chain);
        let _ = check_embedding(&h, &u("http://127.0.0.1:81/"), &chain[..1]);
        let _ = sandbox_of(&h);
        let _ = Sandbox::parse(&v);
        let _ = answer_kind(&v, Some(&v));
    }
}

#[test]
fn fuzz_sources_never_panic() {
    let bits = [
        "http", "https", "data", "javascript", "blob", "about", ":", "//", "a.test", "/", "../", "?", "#", "\\", " ", "\t", "\n",
        "%", "é", "[::1]", "0x7f.1", "127.1", "user@", ":22", ":99999", "text/html,", "<b>", "base64,", ";",
    ];
    let mut rng = Rng(11);
    let base = u("http://a.test/dir/page");
    let top = anc(&["http://a.test/dir/page"]);
    for _ in 0..3000 {
        let mut s = String::new();
        for _ in 0..(rng.next() % 10) {
            s.push_str(rng.pick(&bits));
        }
        let _ = source(&Ask {
            src: Some(&s),
            srcdoc: None,
            base: Some(&base),
            ancestors: &top,
            frames_now: (rng.next() % 40) as usize,
        });
        let _ = source(&Ask {
            src: Some(&s),
            srcdoc: None,
            base: None,
            ancestors: &top[..0],
            frames_now: 0,
        });
        if let Ok(x) = Url::parse(&s) {
            let _ = check_hop(&top, &x);
            let _ = cookie_context(&top, &x);
            let _ = referer(Some(&base), &x, Some(&s), ReferrerPolicy::DEFAULT);
        }
    }
}

#[test]
fn fuzz_history_never_panics_and_stays_bounded() {
    let keys = ["a", "b", "c", "#0", "#1", "d"];
    let urls = ["http://t.test/1", "http://t.test/2", "http://t.test/3#x", "milk:newtab"];
    let mut rng = Rng(99);
    let mut h = SessionHistory::new();
    h.push_doc("http://t.test/", 1);
    let mut next_doc = 100;
    for _ in 0..20000 {
        let depth = 1 + (rng.next() % 3) as usize;
        let path: Vec<String> = (0..depth).map(|_| rng.pick(&keys).to_string()).collect();
        let url = rng.pick(&urls);
        next_doc += 1;
        match rng.next() % 12 {
            0 => h.push_doc(url, next_doc),
            1 => h.replace_doc(url, next_doc),
            2..=4 => h.frame_loaded(&path, next_doc, url, rng.next() % 3 == 0, rng.next() % 5 == 0),
            5 => h.frame_same_doc(&path, url, rng.next() % 2 == 0),
            6 => h.top_same_doc(url, rng.next() % 2 == 0),
            7 => h.frame_removed(&path),
            8 => {
                let d = (rng.next() % 5) as isize - 2;
                if let Some(t) = h.traverse(d, 3) {
                    for r in &t.reloads {
                        let _ = restore_for(&t.reloads, &r.path);
                        h.rebind(r.node.doc, next_doc);
                    }
                    if t.top_reload {
                        h.rebind(t.entry.doc, next_doc);
                    }
                }
            }
            9 => h.save_scroll(&path, 5),
            10 => h.rebind(rng.next() % 200, next_doc),
            _ => h.set_top_doc(next_doc),
        }
        assert!(h.len() <= MAX_ENTRIES);
        if let Some(e) = h.current() {
            fn count(n: &Node) -> usize {
                1 + n.children.iter().map(count).sum::<usize>()
            }
            // Adding past the cap is refused; a navigation clones an entry that is within it.
            assert!(e.frames.iter().map(count).sum::<usize>() <= MAX_NODES * 4);
        }
    }
}
