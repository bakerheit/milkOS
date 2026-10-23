//! Loading a frame: what a frame's `src` means, whether it may be fetched at all, with which
//! cookies and `Referer`, and whether it may keep what came back. Pure rules for the Browser's
//! frame loader; no I/O.
//!
//! # The rules
//!
//! * **Addresses**: `src` is resolved like a script's address ([`resolve_url`]): `http(s)`
//!   is fetched, `data:` is decoded in place (an opaque-origin document, no network),
//!   `about:blank` (and an empty or missing `src`) is the empty document, `srcdoc` wins over
//!   `src`. `javascript:`, `blob:`, `file:`, `ftp:` and the rest are refused with a line on the
//!   console. The same strictness as `fetch` (user names, backslashes, number-like hosts, the
//!   web's refused ports) applies.
//! * **Limits**: a frame may be at most [`MAX_DEPTH`] levels deep (the top page's frames are
//!   level 1) and a tab holds at most [`MAX_FRAMES`] frames in all. Over either the frame is
//!   not loaded and is left empty with a console line.
//! * **Mixed content**: a plain-http frame inside a document that came over https (the parent
//!   or any ancestor) is blocked; so is any redirect hop that ends up on plain http
//!   ([`check_hop`]). `localhost`, 127/8 and `::1` count as secure, as for `fetch`.
//! * **Cookies**: a frame gets cookies, and can keep them, only when its address is on the
//!   same *site* as every one of its ancestors (the no-third-party-cookies rule, applied along
//!   the whole chain: `a.test` > `b.test` > `a.test` is a third party at the bottom). The
//!   rule is a [`Context`] for the cookie jar, so redirects to another site lose them too. A
//!   document's scripts follow the same rule for `document.cookie` ([`cookies_allowed`]), and
//!   an opaque origin (a sandbox without `allow-same-origin`, or a `data:` document) never
//!   has any.
//! * **Referer**: the parent document's address, cut by the referrer policy ([`referer`]):
//!   the `referrerpolicy` attribute of the `<iframe>`, else the parent's `Referrer-Policy`
//!   header, else strict-origin-when-cross-origin.
//! * **After the answer**: [`super::ancestors::check_embedding`] (X-Frame-Options and CSP
//!   frame-ancestors), the CSP `sandbox` directive, and the type of the answer: HTML, XHTML
//!   and plain text are shown; images are shown as a page holding the picture; anything else
//!   (and `Content-Disposition: attachment`) is a download, which a sandbox without
//!   `allow-downloads` refuses, and otherwise is refused too (a frame is not where a file
//!   should land silently): [`answer_kind`].

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use browserkit::cookies::{Context, site_of};
use browserkit::netpolicy::origin::{DataUrl, Target, is_bad_port, is_potentially_trustworthy};
use browserkit::netpolicy::{Limits, ReferrerPolicy, referrer_header, resolve_url};
use milkweb::url::{Scheme, Url};

use super::sandbox::Sandbox;

/// Levels of nesting allowed: the top page's frames are at level 1.
pub const MAX_DEPTH: usize = 5;
/// Frames one tab may hold.
pub const MAX_FRAMES: usize = 32;
/// The largest `srcdoc` taken (bytes); more is cut.
pub const MAX_SRCDOC: usize = 1 << 20;

/// What a frame's address turned out to be.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Source {
    /// The empty document (`about:blank`, no `src`).
    Blank,
    /// `srcdoc`: the markup itself.
    Srcdoc(String),
    /// Fetch it.
    Http(Url),
    /// Content in the address (an opaque origin).
    Data(DataUrl),
}

/// Why a frame was not loaded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Refusal {
    TooDeep,
    TooMany,
    /// The scheme (`javascript`, `blob`, ...).
    Scheme(String),
    BadUrl(String),
    MixedContent { page: String, url: String },
    BadPort { url: String, port: u16 },
}

impl Refusal {
    /// The line for the console.
    pub fn console(&self, src: &str) -> String {
        let s = clip(src);
        match self {
            Refusal::TooDeep => format!(
                "Not loading the frame '{s}': frames may be nested at most {MAX_DEPTH} deep."
            ),
            Refusal::TooMany => format!(
                "Not loading the frame '{s}': a page may hold at most {MAX_FRAMES} frames."
            ),
            Refusal::Scheme(sc) => {
                format!("Not loading the frame '{s}': {sc}: addresses can't be shown in a frame.")
            }
            Refusal::BadUrl(why) => format!("Not loading the frame '{s}': {why}."),
            Refusal::MixedContent { page, url } => format!(
                "Mixed Content: The page at '{page}' was loaded over HTTPS, but contains a frame '{url}' that is only available over plain HTTP. The frame was blocked."
            ),
            Refusal::BadPort { url, port } => {
                format!("Not loading the frame '{url}': port {port} is one the web refuses to talk to.")
            }
        }
    }

    /// What the frame says in its place.
    pub fn reason(&self) -> String {
        match self {
            Refusal::TooDeep => String::from("Too many frames inside frames."),
            Refusal::TooMany => String::from("This page has too many frames."),
            Refusal::Scheme(sc) => format!("A {sc}: address can't be shown in a frame."),
            Refusal::BadUrl(_) => String::from("That isn't an address the Browser can show."),
            Refusal::MixedContent { .. } => String::from(
                "This frame was blocked: the page is secure (https) but the frame is not (http).",
            ),
            Refusal::BadPort { .. } => {
                String::from("The Browser doesn't connect to that port from a page.")
            }
        }
    }
}

fn clip(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(100).collect()
}

/// Everything the rules need to know about where a frame is going.
pub struct Ask<'a> {
    /// The `src` attribute as written (None: absent).
    pub src: Option<&'a str>,
    pub srcdoc: Option<&'a str>,
    /// What relative addresses resolve against: the parent document's base address.
    pub base: Option<&'a Url>,
    /// The addresses the ancestors' documents came from, the top page first and the parent
    /// last (None: no address, such as a `data:` document). Its length is the frame's level
    /// minus one.
    pub ancestors: &'a [Option<Url>],
    /// Frames in the tab now (all levels).
    pub frames_now: usize,
}

/// Decide what the frame's `src` / `srcdoc` mean and whether it may load.
pub fn source(ask: &Ask<'_>) -> Result<Source, Refusal> {
    let src = ask.src.map(str::trim).unwrap_or("");
    if ask.ancestors.len() >= MAX_DEPTH {
        return Err(Refusal::TooDeep);
    }
    if ask.frames_now >= MAX_FRAMES {
        return Err(Refusal::TooMany);
    }
    if let Some(doc) = ask.srcdoc {
        let mut d = String::from(doc);
        if d.len() > MAX_SRCDOC {
            let mut n = MAX_SRCDOC;
            while !d.is_char_boundary(n) {
                n -= 1;
            }
            d.truncate(n);
        }
        return Ok(Source::Srcdoc(d));
    }
    if src.is_empty() || src.eq_ignore_ascii_case("about:blank") {
        return Ok(Source::Blank);
    }
    // about:srcdoc and about:anything-else: blank, as browsers do for unknown about: pages
    // would be wrong (they are errors); only about:blank is the empty document.
    let lower = src.to_ascii_lowercase();
    if lower.starts_with("about:") {
        return Err(Refusal::Scheme(String::from("about")));
    }
    let relative_ok = ask.base.is_some();
    let has_scheme = lower.starts_with("http:") || lower.starts_with("https:") || lower.starts_with("data:");
    if !relative_ok && !has_scheme {
        return Err(Refusal::BadUrl(String::from("it is a relative address and the page has none to start from")));
    }
    let dummy;
    let base = match ask.base {
        Some(b) => b,
        None => {
            dummy = Url::parse("http://about.invalid/").map_err(|e| Refusal::BadUrl(format!("{e}")))?;
            &dummy
        }
    };
    match resolve_url(base, src, &Limits::default()) {
        Ok(Target::Http(u)) => {
            check_hop(ask.ancestors, &u)?;
            Ok(Source::Http(u))
        }
        Ok(Target::Data(d)) => Ok(Source::Data(d)),
        Ok(Target::Blob(_)) => Err(Refusal::Scheme(String::from("blob"))),
        Err(browserkit::netpolicy::NetError::UnsupportedScheme(s)) => Err(Refusal::Scheme(s)),
        Err(browserkit::netpolicy::NetError::BlockedPort { url, port }) => {
            Err(Refusal::BadPort { url, port })
        }
        Err(e) => Err(Refusal::BadUrl(format!("{e}"))),
    }
}

/// May the frame go to `next` (the first address or a redirect hop)? Mixed content and the
/// refused ports.
pub fn check_hop(ancestors: &[Option<Url>], next: &Url) -> Result<(), Refusal> {
    if is_bad_port(next.port) {
        return Err(Refusal::BadPort {
            url: next.to_text(),
            port: next.port,
        });
    }
    if !is_potentially_trustworthy(next)
        && let Some(secure) = ancestors
            .iter()
            .flatten()
            .find(|a| a.scheme == Scheme::Https)
    {
        return Err(Refusal::MixedContent {
            page: secure.to_text(),
            url: next.to_text(),
        });
    }
    Ok(())
}

/// A site no cookie belongs to: the context of a frame whose chain is not all one site.
const NO_SITE: &str = "\u{0}no-site";

/// Every ancestor is on the same site as `url` (schemeful, like the cookie rules).
fn chain_same_site(ancestors: &[Option<Url>], url: &Url) -> bool {
    ancestors.iter().all(|a| {
        a.as_ref()
            .is_some_and(|a| a.scheme == url.scheme && site_of(&a.host) == site_of(&url.host))
    })
}

/// The cookie jar's context for fetching `url` into a frame under `ancestors`.
pub fn cookie_context(ancestors: &[Option<Url>], url: &Url) -> Context {
    if chain_same_site(ancestors, url) {
        Context::Subresource {
            page_site: site_of(&url.host),
        }
    } else {
        Context::Subresource {
            page_site: String::from(NO_SITE),
        }
    }
}

/// May a document at `url` in a frame under `ancestors` read and write `document.cookie`?
/// `sandbox`: the restrictions it has. (A top-level page has no ancestors.)
pub fn cookies_allowed(ancestors: &[Option<Url>], url: &Url, sandbox: Sandbox) -> bool {
    !sandbox.opaque_origin() && chain_same_site(ancestors, url)
}

/// The `Referer` for the frame's request. `parent`: the parent document's address;
/// `attribute`: the `<iframe referrerpolicy=...>` value; `parent_policy`: the policy the
/// parent document runs under (its `Referrer-Policy` header, else the default).
pub fn referer(
    parent: Option<&Url>,
    target: &Url,
    attribute: Option<&str>,
    parent_policy: ReferrerPolicy,
) -> Option<String> {
    let policy = attribute
        .map(|a| a.trim().to_ascii_lowercase())
        .and_then(|a| ReferrerPolicy::parse(&a))
        .unwrap_or(parent_policy);
    referrer_header(policy, parent?, target)
}

// ---- The answer ----

/// What the frame does with a response.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    /// Parse it as a page.
    Page,
    /// A picture: shown as a page holding it.
    Image,
    /// A file: not for a frame.
    Download,
}

/// Sort a response by its `Content-Type` and `Content-Disposition`.
pub fn answer_kind(content_type: &str, disposition: Option<&str>) -> Answer {
    if let Some(d) = disposition {
        let d = d.trim().to_ascii_lowercase();
        if d.starts_with("attachment") {
            return Answer::Download;
        }
    }
    let ct = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if ct.is_empty() {
        return Answer::Page;
    }
    match ct.as_str() {
        "text/html" | "application/xhtml+xml" | "text/plain" | "text/xml" | "application/xml" => {
            Answer::Page
        }
        c if c.starts_with("image/") && c != "image/svg+xml" => Answer::Image,
        c if c.starts_with("text/") => Answer::Page,
        _ => Answer::Download,
    }
}

/// Everything the framed response's headers decide, in one call: the embedding check, the
/// CSP sandbox, and the console notes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Verdict {
    pub blocked: Option<super::ancestors::Blocked>,
    /// The sandbox the document runs under: the iframe's, the parents', and the response's.
    pub sandbox: Sandbox,
    pub notes: Vec<String>,
}

/// `inherited`: the parent document's restrictions merged with the iframe's attribute.
pub fn judge(
    headers: &[(String, String)],
    url: &Url,
    ancestors: &[Option<Url>],
    inherited: Sandbox,
) -> Verdict {
    let e = super::ancestors::check_embedding(headers, url, ancestors);
    Verdict {
        blocked: e.blocked,
        sandbox: inherited.merge(super::ancestors::sandbox_of(headers)),
        notes: e.notes,
    }
}
