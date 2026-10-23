//! Cookies, RFC 6265 basics with today's browser defaults: `Domain`, `Path`, `Secure`,
//! `HttpOnly`, `Expires` / `Max-Age`, `SameSite` (Lax when a site says nothing), the
//! `__Secure-` and `__Host-` prefixes, and no third-party cookies at all.
//!
//! "Site" is the host's last two labels (`www.milk.test` and `shop.milk.test` are one site),
//! a numeric address or a one-label name (`localhost`) is its own site, and a short list of
//! two-label suffixes (`co.uk`, ...) counts as one label. There is no full public suffix list:
//! a known gap, said in docs/browser.md.
//!
//! What is sent and kept depends on the [`Context`] of the request:
//! * a top-level page (typed, a bookmark, a link, a form): the page's own cookies. A page
//!   reached from another site (a link or a form there) gets `SameSite=Lax` cookies only for a
//!   GET, and `Strict` ones not at all.
//! * a subresource (an image): only when it comes from the page's own site; anything from
//!   another site gets no cookies and can't set any (no third-party cookies).
//!
//! Persistent cookies (Expires / Max-Age) are saved with [`Jar::to_file`]; session cookies
//! live until the Browser quits. Limits: 4096 bytes a cookie, 50 a site, 3000 in all (the
//! oldest go first).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use milkweb::url::{Scheme, Url};

use crate::date;

pub const MAX_COOKIE: usize = 4096;
pub const MAX_PER_SITE: usize = 50;
pub const MAX_TOTAL: usize = 3000;
/// The longest Cookie header we send.
pub const MAX_HEADER: usize = 8192;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SameSite {
    Strict,
    Lax,
    None,
}

impl SameSite {
    pub fn name(self) -> &'static str {
        match self {
            SameSite::Strict => "Strict",
            SameSite::Lax => "Lax",
            SameSite::None => "None",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Lower case, without a leading dot.
    pub domain: String,
    /// No Domain attribute: only this exact host.
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
    /// Unix seconds; None: a session cookie.
    pub expires: Option<i64>,
    /// Creation order (older first when sending, and when evicting).
    pub created: u64,
}

/// Where a request comes from (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Context {
    /// A page in a tab. `from`: the site of the page whose link or form led here (None: typed,
    /// a bookmark, reload, back and forward).
    TopLevel { from: Option<String> },
    /// Something a page loads (an image): `page_site` is that page's site.
    Subresource { page_site: String },
}

/// Two-label public suffixes we know of (a full list is a known gap).
const TWO_LABEL_SUFFIXES: [&str; 14] = [
    "co.uk",
    "org.uk",
    "ac.uk",
    "gov.uk",
    "me.uk",
    "com.au",
    "net.au",
    "org.au",
    "co.nz",
    "co.jp",
    "com.br",
    "co.za",
    "com.cn",
    "github.io",
];

fn is_ip(host: &str) -> bool {
    host.contains(':')
        || (!host.is_empty() && host.bytes().all(|c| c.is_ascii_digit() || c == b'.'))
}

/// Is `domain` a public suffix (one label, or one of the known two-label ones)?
fn is_public_suffix(domain: &str) -> bool {
    !domain.contains('.') || TWO_LABEL_SUFFIXES.contains(&domain)
}

/// The site of `host`: the registrable part (see the module docs).
pub fn site_of(host: &str) -> String {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if is_ip(&host) {
        return host;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() <= 2 {
        return host;
    }
    let last2 = labels[labels.len() - 2..].join(".");
    let keep = if TWO_LABEL_SUFFIXES.contains(&last2.as_str()) {
        3
    } else {
        2
    };
    labels[labels.len().saturating_sub(keep)..].join(".")
}

/// RFC 6265 5.1.3: does `host` domain-match `domain`?
fn domain_match(host: &str, domain: &str) -> bool {
    host == domain
        || (!is_ip(host)
            && host.len() > domain.len()
            && host.ends_with(domain)
            && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
}

/// The request path without its query.
fn uri_path(url: &Url) -> &str {
    url.path.split(['?', '#']).next().unwrap_or("/")
}

/// RFC 6265 5.1.4: the default path, the request path up to its last `/`.
fn default_path(url: &Url) -> String {
    let p = uri_path(url);
    if !p.starts_with('/') {
        return String::from("/");
    }
    match p.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(i) => String::from(&p[..i]),
    }
}

/// RFC 6265 5.1.4: does request path `req` path-match cookie path `cp`?
fn path_match(req: &str, cp: &str) -> bool {
    req == cp
        || (req.starts_with(cp)
            && (cp.ends_with('/') || req.as_bytes().get(cp.len()) == Some(&b'/')))
}

fn has_ctl(s: &str) -> bool {
    s.chars()
        .any(|c| (c as u32) < 0x20 && c != '\t' || c == '\u{7f}')
}

/// Why a Set-Cookie was not kept (for the log; never shown to a page).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    Syntax,
    TooBig,
    ThirdParty,
    Domain,
    NotSecure,
    Prefix,
    SameSiteNone,
    /// A script (document.cookie) tried to set or replace an HttpOnly cookie.
    HttpOnly,
}

#[derive(Clone, Debug, Default)]
pub struct Jar {
    cookies: Vec<Cookie>,
    seq: u64,
}

impl Jar {
    pub fn new() -> Jar {
        Jar::default()
    }

    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    pub fn all(&self) -> &[Cookie] {
        &self.cookies
    }

    pub fn clear(&mut self) {
        self.cookies.clear();
    }

    /// Forget every cookie of `site`.
    pub fn clear_site(&mut self, site: &str) {
        self.cookies.retain(|c| site_of(&c.domain) != site);
    }

    /// Drop the expired ones.
    pub fn expire(&mut self, now: i64) {
        self.cookies.retain(|c| c.expires.is_none_or(|e| e > now));
    }

    /// May a request to `url` in `ctx` use cookies at all? (Third-party ones never do.)
    fn first_party(url: &Url, ctx: &Context) -> bool {
        match ctx {
            Context::TopLevel { .. } => true,
            Context::Subresource { page_site } => site_of(&url.host) == *page_site,
        }
    }

    /// Is this request cross-site (a top-level one from another site's link or form)?
    fn cross_site(url: &Url, ctx: &Context) -> bool {
        match ctx {
            Context::TopLevel { from: Some(f) } => site_of(&url.host) != *f,
            _ => false,
        }
    }

    /// One `Set-Cookie` header from a response for `url`.
    pub fn set(&mut self, url: &Url, header: &str, now: i64, ctx: &Context) -> Result<(), Refused> {
        self.set_as(url, header, now, ctx, false)
    }

    /// `document.cookie = line` on a page at `url`: like a Set-Cookie header, except that a
    /// script can't set an HttpOnly cookie or replace one.
    pub fn set_from_script(&mut self, url: &Url, line: &str, now: i64) -> Result<(), Refused> {
        self.set_as(url, line, now, &Context::TopLevel { from: None }, true)
    }

    fn set_as(
        &mut self,
        url: &Url,
        header: &str,
        now: i64,
        ctx: &Context,
        script: bool,
    ) -> Result<(), Refused> {
        if !Self::first_party(url, ctx) {
            return Err(Refused::ThirdParty);
        }
        if header.len() > MAX_COOKIE {
            return Err(Refused::TooBig);
        }
        let mut parts = header.split(';');
        let pair = parts.next().unwrap_or("");
        let (name, value) = pair.split_once('=').ok_or(Refused::Syntax)?;
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() || has_ctl(name) || has_ctl(value) || name.contains([' ', '\t']) {
            return Err(Refused::Syntax);
        }
        let host = url.host.to_ascii_lowercase();
        let https = url.scheme == Scheme::Https;
        let mut c = Cookie {
            name: name.to_string(),
            value: value.to_string(),
            domain: host.clone(),
            host_only: true,
            path: default_path(url),
            secure: false,
            http_only: false,
            same_site: SameSite::Lax,
            expires: None,
            created: 0,
        };
        let (mut max_age, mut expires) = (None::<i64>, None::<i64>);
        let mut domain_attr = None::<String>;
        let mut path_attr = false;
        for attr in parts {
            let (k, v) = match attr.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => (attr.trim(), ""),
            };
            match k.to_ascii_lowercase().as_str() {
                "expires" => {
                    if let Some(t) = date::parse_cookie_date(v) {
                        expires = Some(t);
                    }
                }
                "max-age" => {
                    let ok = !v.is_empty()
                        && v.bytes()
                            .enumerate()
                            .all(|(i, b)| b.is_ascii_digit() || (i == 0 && b == b'-'));
                    if ok {
                        // Huge values are clamped, not overflowed.
                        let n = v.parse::<i64>().unwrap_or(if v.starts_with('-') {
                            i64::MIN / 2
                        } else {
                            i64::MAX / 2
                        });
                        max_age = Some(n);
                    }
                }
                "domain" if !v.is_empty() => {
                    domain_attr = Some(v.trim_start_matches('.').to_ascii_lowercase());
                }
                "path" => {
                    if v.starts_with('/') {
                        c.path = v.to_string();
                        path_attr = true;
                    }
                }
                "secure" => c.secure = true,
                "httponly" => c.http_only = true,
                "samesite" => {
                    c.same_site = match v.to_ascii_lowercase().as_str() {
                        "strict" => SameSite::Strict,
                        "none" => SameSite::None,
                        _ => SameSite::Lax,
                    }
                }
                _ => {}
            }
        }
        if let Some(d) = domain_attr {
            if is_public_suffix(&d) {
                // A public suffix may only name the host itself (then it is host-only).
                if d != host {
                    return Err(Refused::Domain);
                }
            } else {
                if !domain_match(&host, &d) {
                    return Err(Refused::Domain);
                }
                c.domain = d;
                c.host_only = false;
            }
        }
        // Secure cookies come only from secure pages, and SameSite=None needs Secure.
        if c.secure && !https {
            return Err(Refused::NotSecure);
        }
        if c.same_site == SameSite::None && !c.secure {
            return Err(Refused::SameSiteNone);
        }
        if (c.name.starts_with("__Secure-") && !c.secure)
            || (c.name.starts_with("__Host-")
                && (!c.secure || !c.host_only || !path_attr || c.path != "/"))
        {
            return Err(Refused::Prefix);
        }
        c.expires = match (max_age, expires) {
            (Some(n), _) => Some(if n <= 0 {
                i64::MIN
            } else {
                now.saturating_add(n)
            }),
            (None, Some(t)) => Some(t),
            (None, None) => None,
        };
        // A plain-http page can't replace (or shadow) a secure cookie.
        let same = |o: &Cookie| o.name == c.name && o.domain == c.domain && o.path == c.path;
        if !https
            && self.cookies.iter().any(|o| {
                o.secure
                    && o.name == c.name
                    && (domain_match(&c.domain, &o.domain) || domain_match(&o.domain, &c.domain))
                    && path_match(&c.path, &o.path)
            })
        {
            return Err(Refused::NotSecure);
        }
        let old = self.cookies.iter().position(same);
        if script && (c.http_only || old.is_some_and(|i| self.cookies[i].http_only)) {
            return Err(Refused::HttpOnly);
        }
        if c.expires.is_some_and(|e| e <= now) {
            // Expired: a deletion.
            if let Some(i) = old {
                self.cookies.remove(i);
            }
            return Ok(());
        }
        match old {
            Some(i) => {
                c.created = self.cookies[i].created;
                self.cookies[i] = c;
            }
            None => {
                self.seq += 1;
                c.created = self.seq;
                self.cookies.push(c);
                self.enforce_limits(now);
            }
        }
        Ok(())
    }

    fn enforce_limits(&mut self, now: i64) {
        self.expire(now);
        let Some(newest) = self.cookies.last() else {
            return;
        };
        let site = site_of(&newest.domain);
        let mut of_site: Vec<(u64, usize)> = self
            .cookies
            .iter()
            .enumerate()
            .filter(|(_, c)| site_of(&c.domain) == site)
            .map(|(i, c)| (c.created, i))
            .collect();
        if of_site.len() > MAX_PER_SITE {
            of_site.sort();
            let drop: Vec<u64> = of_site[..of_site.len() - MAX_PER_SITE]
                .iter()
                .map(|(c, _)| *c)
                .collect();
            self.cookies.retain(|c| !drop.contains(&c.created));
        }
        if self.cookies.len() > MAX_TOTAL {
            self.cookies.sort_by_key(|c| c.created);
            let n = self.cookies.len() - MAX_TOTAL;
            self.cookies.drain(..n);
        }
    }

    /// Every `Set-Cookie` header of a response.
    pub fn set_all(&mut self, url: &Url, headers: &[(String, String)], now: i64, ctx: &Context) {
        for (_, v) in headers.iter().filter(|(k, _)| k == "set-cookie") {
            let _ = self.set(url, v, now, ctx);
        }
    }

    /// The `Cookie` header for a request to `url` (None: no cookies go).
    pub fn header(&self, url: &Url, post: bool, now: i64, ctx: &Context) -> Option<String> {
        self.header_as(url, post, now, ctx, false)
    }

    /// `document.cookie` for a page at `url`: what a request would carry, less the HttpOnly
    /// cookies ("" for none).
    pub fn for_script(&self, url: &Url, now: i64) -> String {
        self.header_as(url, false, now, &Context::TopLevel { from: None }, true)
            .unwrap_or_default()
    }

    fn header_as(
        &self,
        url: &Url,
        post: bool,
        now: i64,
        ctx: &Context,
        script: bool,
    ) -> Option<String> {
        if !Self::first_party(url, ctx) {
            return None;
        }
        let cross = Self::cross_site(url, ctx);
        let host = url.host.to_ascii_lowercase();
        let path = uri_path(url);
        let https = url.scheme == Scheme::Https;
        let mut pick: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| {
                let host_ok = if c.host_only {
                    host == c.domain
                } else {
                    domain_match(&host, &c.domain)
                };
                let same_site_ok = !cross
                    || match c.same_site {
                        SameSite::None => true,
                        SameSite::Lax => !post,
                        SameSite::Strict => false,
                    };
                host_ok
                    && !(script && c.http_only)
                    && path_match(path, &c.path)
                    && (https || !c.secure)
                    && c.expires.is_none_or(|e| e > now)
                    && same_site_ok
            })
            .collect();
        // Longer paths first, then older first (RFC 6265 5.4).
        pick.sort_by(|a, b| {
            b.path
                .len()
                .cmp(&a.path.len())
                .then(a.created.cmp(&b.created))
        });
        let mut out = String::new();
        for c in pick {
            let one = format!("{}={}", c.name, c.value);
            if out.len() + one.len() + 2 > MAX_HEADER {
                break;
            }
            if !out.is_empty() {
                out.push_str("; ");
            }
            out.push_str(&one);
        }
        (!out.is_empty()).then_some(out)
    }

    /// The persistent cookies, one a line (tab separated), for the cookies file.
    pub fn to_file(&self, now: i64) -> String {
        let mut s = String::from(
            "# milkOS Browser cookies: domain host_only path secure http_only same_site expires name value\n",
        );
        for c in &self.cookies {
            let Some(e) = c.expires.filter(|&e| e > now) else {
                continue;
            };
            s.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                c.domain,
                u8::from(c.host_only),
                c.path,
                u8::from(c.secure),
                u8::from(c.http_only),
                c.same_site.name(),
                e,
                c.name,
                c.value
            ));
        }
        s
    }

    /// The cookies file back (bad lines and expired cookies are skipped).
    pub fn from_file(text: &str, now: i64) -> Jar {
        let mut j = Jar::new();
        for l in text.lines() {
            if l.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = l.splitn(9, '\t').collect();
            if f.len() != 9 || j.cookies.len() >= MAX_TOTAL {
                continue;
            }
            let Ok(expires) = f[6].parse::<i64>() else {
                continue;
            };
            let same_site = match f[5] {
                "Strict" => SameSite::Strict,
                "None" => SameSite::None,
                _ => SameSite::Lax,
            };
            let c = Cookie {
                domain: f[0].to_ascii_lowercase(),
                host_only: f[1] == "1",
                path: String::from(f[2]),
                secure: f[3] == "1",
                http_only: f[4] == "1",
                same_site,
                expires: Some(expires),
                name: String::from(f[7]),
                value: String::from(f[8]),
                created: 0,
            };
            if expires <= now
                || c.name.is_empty()
                || c.domain.is_empty()
                || !c.path.starts_with('/')
                || has_ctl(&c.name)
                || has_ctl(&c.value)
                || c.name.len() + c.value.len() > MAX_COOKIE
            {
                continue;
            }
            j.seq += 1;
            j.cookies.push(Cookie {
                created: j.seq,
                ..c
            });
        }
        j
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }
    const TOP: Context = Context::TopLevel { from: None };
    const NOW: i64 = 1_800_000_000;

    fn sub(site: &str) -> Context {
        Context::Subresource {
            page_site: site.to_string(),
        }
    }

    #[test]
    fn sites() {
        assert_eq!(site_of("www.milk.test"), "milk.test");
        assert_eq!(site_of("a.b.shop.example.co.uk"), "example.co.uk");
        assert_eq!(site_of("localhost"), "localhost");
        assert_eq!(site_of("10.0.2.2"), "10.0.2.2");
        assert_eq!(site_of("::1"), "::1");
        assert_eq!(site_of("Milk.Test."), "milk.test");
    }

    #[test]
    fn scripts_never_see_or_touch_http_only() {
        let mut j = Jar::new();
        let page = u("http://www.milk.test/app");
        j.set(&page, "sid=secret; HttpOnly; Path=/", NOW, &TOP).unwrap();
        j.set(&page, "theme=dark; Path=/", NOW, &TOP).unwrap();
        assert_eq!(j.for_script(&page, NOW), "theme=dark");
        assert_eq!(j.header(&page, false, NOW, &TOP).unwrap(), "sid=secret; theme=dark");
        assert_eq!(
            j.set_from_script(&page, "sid=stolen; Path=/", NOW),
            Err(Refused::HttpOnly)
        );
        assert_eq!(
            j.set_from_script(&page, "x=1; HttpOnly", NOW),
            Err(Refused::HttpOnly)
        );
        j.set_from_script(&page, "lang=en; Path=/", NOW).unwrap();
        assert_eq!(j.for_script(&page, NOW), "theme=dark; lang=en");
        assert!(j.header(&page, false, NOW, &TOP).unwrap().contains("sid=secret"));
    }

    #[test]
    fn host_only_domain_and_path() {
        let mut j = Jar::new();
        let page = u("http://www.milk.test/shop/cart?x=1");
        j.set(&page, "a=1", NOW, &TOP).unwrap();
        j.set(&page, "b=2; Domain=.milk.test; Path=/", NOW, &TOP)
            .unwrap();
        j.set(&page, "c=3; Path=/shop/cart", NOW, &TOP).unwrap();
        assert_eq!(j.all()[0].path, "/shop");
        assert!(j.all()[0].host_only && !j.all()[1].host_only);
        // Longer path first, then older.
        assert_eq!(
            j.header(&u("http://www.milk.test/shop/cart"), false, NOW, &TOP)
                .as_deref(),
            Some("c=3; a=1; b=2")
        );
        assert_eq!(
            j.header(&u("http://www.milk.test/shopping"), false, NOW, &TOP)
                .as_deref(),
            Some("b=2")
        );
        assert_eq!(
            j.header(&u("http://web.milk.test/shop/x"), false, NOW, &TOP)
                .as_deref(),
            Some("b=2")
        );
        assert_eq!(
            j.header(&u("http://milk.test/"), false, NOW, &TOP)
                .as_deref(),
            Some("b=2")
        );
        assert_eq!(
            j.header(&u("http://evilmilk.test/"), false, NOW, &TOP),
            None
        );
        assert_eq!(j.header(&u("http://other.test/"), false, NOW, &TOP), None);
    }

    #[test]
    fn domain_rules() {
        let mut j = Jar::new();
        let page = u("http://www.milk.test/");
        assert_eq!(
            j.set(&page, "a=1; Domain=other.test", NOW, &TOP),
            Err(Refused::Domain)
        );
        assert_eq!(
            j.set(&page, "a=1; Domain=test", NOW, &TOP),
            Err(Refused::Domain)
        );
        assert_eq!(
            j.set(&page, "a=1; Domain=ilk.test", NOW, &TOP),
            Err(Refused::Domain)
        );
        assert_eq!(
            j.set(
                &u("http://shop.example.co.uk/"),
                "a=1; Domain=co.uk",
                NOW,
                &TOP
            ),
            Err(Refused::Domain)
        );
        // A one-label host naming itself: host-only.
        j.set(
            &u("http://localhost:8080/"),
            "l=1; Domain=localhost",
            NOW,
            &TOP,
        )
        .unwrap();
        assert!(j.all()[0].host_only);
        // IP addresses only match exactly.
        j.set(&u("http://10.0.2.2/"), "ip=1", NOW, &TOP).unwrap();
        assert_eq!(
            j.header(&u("http://10.0.2.2/x"), false, NOW, &TOP)
                .as_deref(),
            Some("ip=1")
        );
        assert_eq!(j.header(&u("http://110.0.2.2/x"), false, NOW, &TOP), None);
    }

    #[test]
    fn secure_and_prefixes() {
        let mut j = Jar::new();
        let http = u("http://milk.test/");
        let https = u("https://milk.test/");
        assert_eq!(
            j.set(&http, "s=1; Secure", NOW, &TOP),
            Err(Refused::NotSecure)
        );
        j.set(&https, "s=1; Secure", NOW, &TOP).unwrap();
        assert_eq!(j.header(&http, false, NOW, &TOP), None);
        assert_eq!(j.header(&https, false, NOW, &TOP).as_deref(), Some("s=1"));
        // http can't overwrite it.
        assert_eq!(j.set(&http, "s=evil", NOW, &TOP), Err(Refused::NotSecure));
        assert_eq!(
            j.set(&https, "__Secure-x=1", NOW, &TOP),
            Err(Refused::Prefix)
        );
        j.set(&https, "__Secure-x=1; Secure", NOW, &TOP).unwrap();
        assert_eq!(
            j.set(&https, "__Host-x=1; Secure", NOW, &TOP),
            Err(Refused::Prefix)
        );
        assert_eq!(
            j.set(
                &https,
                "__Host-x=1; Secure; Path=/; Domain=milk.test",
                NOW,
                &TOP
            ),
            Err(Refused::Prefix)
        );
        j.set(&https, "__Host-x=1; Secure; Path=/", NOW, &TOP)
            .unwrap();
        assert_eq!(
            j.set(&https, "n=1; SameSite=None", NOW, &TOP),
            Err(Refused::SameSiteNone)
        );
    }

    #[test]
    fn expiry_and_deletion() {
        let mut j = Jar::new();
        let p = u("http://milk.test/");
        j.set(&p, "a=1; Max-Age=60", NOW, &TOP).unwrap();
        j.set(&p, "b=1; Expires=Wed, 21 Oct 2015 07:28:00 GMT", NOW, &TOP)
            .unwrap();
        j.set(
            &p,
            "c=1; Expires=Wed, 21 Oct 2099 07:28:00 GMT; Max-Age=10",
            NOW,
            &TOP,
        )
        .unwrap();
        j.set(&p, "d=1", NOW, &TOP).unwrap();
        assert_eq!(
            j.header(&p, false, NOW, &TOP).as_deref(),
            Some("a=1; c=1; d=1")
        );
        assert_eq!(
            j.header(&p, false, NOW + 30, &TOP).as_deref(),
            Some("a=1; d=1")
        );
        j.set(&p, "a=gone; Max-Age=0", NOW, &TOP).unwrap();
        j.set(&p, "d=; Expires=Thu, 01 Jan 1970 00:00:00 GMT", NOW, &TOP)
            .unwrap();
        assert_eq!(j.header(&p, false, NOW, &TOP).as_deref(), Some("c=1"));
        // Replacing keeps the creation order.
        j.set(&p, "x=1", NOW, &TOP).unwrap();
        j.set(&p, "c=2; Max-Age=999", NOW, &TOP).unwrap();
        assert_eq!(j.header(&p, false, NOW, &TOP).as_deref(), Some("c=2; x=1"));
        // Huge Max-Age doesn't overflow.
        j.set(&p, "big=1; Max-Age=99999999999999999999999", NOW, &TOP)
            .unwrap();
        assert!(j.header(&p, false, NOW, &TOP).unwrap().contains("big=1"));
    }

    #[test]
    fn no_third_party_cookies() {
        let mut j = Jar::new();
        let img = u("http://tracker.test/pixel.png");
        assert_eq!(
            j.set(&img, "t=1", NOW, &sub("milk.test")),
            Err(Refused::ThirdParty)
        );
        j.set(&img, "t=1", NOW, &TOP).unwrap();
        assert_eq!(j.header(&img, false, NOW, &sub("milk.test")), None);
        assert_eq!(
            j.header(&img, false, NOW, &sub("tracker.test")).as_deref(),
            Some("t=1")
        );
        // A same-site subresource is fine.
        let own = u("http://img.milk.test/a.png");
        j.set(&own, "i=1; Domain=milk.test", NOW, &sub("milk.test"))
            .unwrap();
        assert_eq!(
            j.header(&own, false, NOW, &sub("milk.test")).as_deref(),
            Some("i=1")
        );
    }

    #[test]
    fn same_site_on_cross_site_navigation() {
        let mut j = Jar::new();
        let p = u("https://milk.test/");
        j.set(&p, "lax=1", NOW, &TOP).unwrap();
        j.set(&p, "strict=1; SameSite=Strict", NOW, &TOP).unwrap();
        j.set(&p, "none=1; SameSite=None; Secure", NOW, &TOP)
            .unwrap();
        let from_other = Context::TopLevel {
            from: Some("other.test".into()),
        };
        let from_self = Context::TopLevel {
            from: Some("milk.test".into()),
        };
        assert_eq!(
            j.header(&p, false, NOW, &TOP).as_deref(),
            Some("lax=1; strict=1; none=1")
        );
        assert_eq!(
            j.header(&p, true, NOW, &from_self).as_deref(),
            Some("lax=1; strict=1; none=1")
        );
        assert_eq!(
            j.header(&p, false, NOW, &from_other).as_deref(),
            Some("lax=1; none=1")
        );
        assert_eq!(
            j.header(&p, true, NOW, &from_other).as_deref(),
            Some("none=1")
        );
    }

    #[test]
    fn hostile_headers() {
        let mut j = Jar::new();
        let p = u("http://milk.test/");
        for bad in ["", "novalue", "=v", " =v", "a\u{1}=b", "a=b\u{0}", "a b=c"] {
            assert!(j.set(&p, bad, NOW, &TOP).is_err(), "{bad:?}");
        }
        let long = alloc::format!("a={}", "x".repeat(MAX_COOKIE));
        assert_eq!(j.set(&p, &long, NOW, &TOP), Err(Refused::TooBig));
        // Odd attributes are ignored, not fatal.
        j.set(
            &p,
            "a=1;;; =;Path=relative;Max-Age=abc;Expires=never;Domain=;SameSite=what",
            NOW,
            &TOP,
        )
        .unwrap();
        let c = &j.all()[0];
        assert_eq!(
            (c.path.as_str(), c.expires, c.same_site, c.host_only),
            ("/", None, SameSite::Lax, true)
        );
        assert!(!j.is_empty());
    }

    #[test]
    fn limits() {
        let mut j = Jar::new();
        let p = u("http://milk.test/");
        for i in 0..60 {
            j.set(&p, &alloc::format!("c{i}=1"), NOW, &TOP).unwrap();
        }
        assert_eq!(j.len(), MAX_PER_SITE);
        assert!(j.all().iter().all(|c| c.name != "c0" && c.name != "c9"));
        assert!(j.all().iter().any(|c| c.name == "c59"));
    }

    #[test]
    fn file_round_trip() {
        let mut j = Jar::new();
        let p = u("https://www.milk.test/a/b");
        j.set(
            &p,
            "keep=1; Max-Age=3600; Secure; HttpOnly; SameSite=Strict",
            NOW,
            &TOP,
        )
        .unwrap();
        j.set(&p, "session=1", NOW, &TOP).unwrap();
        j.set(&p, "d=x y; Domain=milk.test; Path=/; Max-Age=60", NOW, &TOP)
            .unwrap();
        let text = j.to_file(NOW);
        assert!(!text.contains("session"));
        let back = Jar::from_file(&text, NOW);
        assert_eq!(back.len(), 2);
        assert_eq!(back.all()[0].same_site, SameSite::Strict);
        assert!(back.all()[0].secure && back.all()[0].http_only && back.all()[0].host_only);
        assert_eq!(
            back.header(&p, false, NOW, &TOP).as_deref(),
            Some("keep=1; d=x y")
        );
        // Later, they have expired.
        assert!(Jar::from_file(&text, NOW + 7200).is_empty());
        // Garbage lines are skipped.
        let junk = "x\ty\n\t\t\t\t\t\t\t\t\nmilk.test\t1\t/\t0\t0\tLax\tsoon\ta\tb\nmilk.test\t1\tnoslash\t0\t0\tLax\t9999999999\ta\tb\n";
        assert!(Jar::from_file(junk, NOW).is_empty());
    }
}
