//! The `Referer` a script request carries: referrer policies, the `referrer` option, and
//! the `Referrer-Policy` response header.

use alloc::format;
use alloc::string::String;

use milkweb::url::Url;

use super::origin::{Origin, is_potentially_trustworthy};

/// Longest `Referer` sent whole; a longer one is cut to the origin.
pub const MAX_REFERRER: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReferrerPolicy {
    NoReferrer,
    NoReferrerWhenDowngrade,
    SameOrigin,
    Origin,
    StrictOrigin,
    OriginWhenCrossOrigin,
    StrictOriginWhenCrossOrigin,
    UnsafeUrl,
}

impl ReferrerPolicy {
    /// What browsers use when a page says nothing.
    pub const DEFAULT: ReferrerPolicy = ReferrerPolicy::StrictOriginWhenCrossOrigin;

    pub fn as_str(self) -> &'static str {
        match self {
            ReferrerPolicy::NoReferrer => "no-referrer",
            ReferrerPolicy::NoReferrerWhenDowngrade => "no-referrer-when-downgrade",
            ReferrerPolicy::SameOrigin => "same-origin",
            ReferrerPolicy::Origin => "origin",
            ReferrerPolicy::StrictOrigin => "strict-origin",
            ReferrerPolicy::OriginWhenCrossOrigin => "origin-when-cross-origin",
            ReferrerPolicy::StrictOriginWhenCrossOrigin => "strict-origin-when-cross-origin",
            ReferrerPolicy::UnsafeUrl => "unsafe-url",
        }
    }

    /// One policy name, exactly as the `referrerPolicy` option spells it (lower case);
    /// `None` for `""` and anything else.
    pub fn parse(s: &str) -> Option<ReferrerPolicy> {
        Some(match s {
            "no-referrer" => ReferrerPolicy::NoReferrer,
            "no-referrer-when-downgrade" => ReferrerPolicy::NoReferrerWhenDowngrade,
            "same-origin" => ReferrerPolicy::SameOrigin,
            "origin" => ReferrerPolicy::Origin,
            "strict-origin" => ReferrerPolicy::StrictOrigin,
            "origin-when-cross-origin" => ReferrerPolicy::OriginWhenCrossOrigin,
            "strict-origin-when-cross-origin" => ReferrerPolicy::StrictOriginWhenCrossOrigin,
            "unsafe-url" => ReferrerPolicy::UnsafeUrl,
            _ => return None,
        })
    }

    /// A `Referrer-Policy` header (or `<meta name=referrer>`) value: a comma-separated list,
    /// any case; the last name we know wins (so a page can list a fallback first).
    pub fn parse_header(value: &str) -> Option<ReferrerPolicy> {
        let mut found = None;
        for piece in value.split(',') {
            let p = piece
                .trim_matches(|c: char| c == ' ' || c == '\t')
                .to_ascii_lowercase();
            if let Some(pol) = ReferrerPolicy::parse(&p) {
                found = Some(pol);
            }
        }
        found
    }
}

/// The `referrer` option of a request.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ReferrerInit {
    /// `"about:client"` (the default): the page.
    Client,
    /// `""`.
    NoReferrer,
    /// An address (resolved against the page; it must be of the page's own origin).
    Url(String),
}

impl ReferrerInit {
    /// From the option's text.
    pub fn from_text(s: &str) -> ReferrerInit {
        match s {
            "" => ReferrerInit::NoReferrer,
            "about:client" => ReferrerInit::Client,
            other => ReferrerInit::Url(String::from(other)),
        }
    }
}

fn origin_form(url: &Url) -> String {
    format!("{}/", Origin::of(url).serialize())
}

/// The `Referer` value for a request to `target` that comes from `referrer` (the page's
/// address, or a same-origin address it chose), under `policy`. `None`: send none.
/// The address is sent without fragment (our `Url` has none) or user name, and cut to its
/// origin when over [`MAX_REFERRER`] bytes.
pub fn referrer_header(policy: ReferrerPolicy, referrer: &Url, target: &Url) -> Option<String> {
    let full = {
        let t = referrer.to_text();
        if t.len() > MAX_REFERRER {
            origin_form(referrer)
        } else {
            t
        }
    };
    let origin = origin_form(referrer);
    let same = Origin::of(referrer) == Origin::of(target);
    let downgrade = is_potentially_trustworthy(referrer) && !is_potentially_trustworthy(target);
    match policy {
        ReferrerPolicy::NoReferrer => None,
        ReferrerPolicy::NoReferrerWhenDowngrade => (!downgrade).then_some(full),
        ReferrerPolicy::SameOrigin => same.then_some(full),
        ReferrerPolicy::Origin => Some(origin),
        ReferrerPolicy::StrictOrigin => (!downgrade).then_some(origin),
        ReferrerPolicy::OriginWhenCrossOrigin => Some(if same { full } else { origin }),
        ReferrerPolicy::StrictOriginWhenCrossOrigin => {
            if same {
                Some(full)
            } else if downgrade {
                None
            } else {
                Some(origin)
            }
        }
        ReferrerPolicy::UnsafeUrl => Some(full),
    }
}
