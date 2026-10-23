//! Who may touch whose window: the same-origin policy for frame scripting, as pure functions
//! over origin strings, and the short list of things any window may do to a window of another
//! origin (the HTML standard's "cross-origin properties").
//!
//! An origin here is the text `event.origin` shows: `scheme://host[:port]` (default ports
//! left out), or `"null"` for an opaque one. A document with an opaque origin (a sandbox
//! without `allow-same-origin`, a `data:` document) is the same origin as nothing but itself:
//! two `"null"` origins are *different* origins, so [`Origin::same`] needs the documents'
//! identities too.
//!
//! # What a window of another origin lets you do
//!
//! Reading: `window`, `self`, `frames`, `length`, `top`, `parent`, `opener`, `closed` and
//! index access to its child frames (`w[0]`). Calling: `postMessage`, `focus`, `blur`, `close`.
//! Writing: `location` (assigning its `href`, or calling `location.replace()`), nothing else.
//! Everything else throws a `SecurityError`: `document`, `name`, `frameElement`, any global
//! variable, `location.href` read. Not done: the standard's property-descriptor details (a
//! cross-origin `Object.getOwnPropertyDescriptor`), `document.domain` relaxing, and named
//! access to child frames from another origin.

use alloc::format;
use alloc::string::String;

/// A frame's origin, and the identity that tells two opaque ones apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    /// `"null"` for an opaque origin.
    pub text: String,
    /// The document's identity (a number unique to the document), used only when opaque.
    pub doc: u64,
}

impl Origin {
    pub fn is_opaque(&self) -> bool {
        self.text == "null"
    }

    /// The same-origin test.
    pub fn same(&self, other: &Origin) -> bool {
        if self.is_opaque() || other.is_opaque() {
            return self.is_opaque() && other.is_opaque() && self.doc == other.doc;
        }
        self.text == other.text
    }
}

/// The origin of an address: `scheme://host[:port]` in lower case with a default port left
/// out; `"null"` for anything that is not an http(s) address.
pub fn origin_of(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return String::from("null");
    };
    let scheme = scheme.to_ascii_lowercase();
    let default = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return String::from("null"),
    };
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let auth = &rest[..auth_end];
    let auth = auth.rsplit('@').next().unwrap_or(auth).to_ascii_lowercase();
    if auth.is_empty() {
        return String::from("null");
    }
    let (host, port) = if auth.starts_with('[') {
        match auth.find(']') {
            Some(i) => (&auth[..=i], auth[i + 1..].strip_prefix(':').unwrap_or("")),
            None => (auth.as_str(), ""),
        }
    } else {
        match auth.rsplit_once(':') {
            Some((h, p)) => (h, p),
            None => (auth.as_str(), ""),
        }
    };
    let host = host.trim_end_matches('.');
    match port.parse::<u32>() {
        Ok(p) if p != default && p != 0 => format!("{scheme}://{host}:{p}"),
        _ if port.is_empty() || port.parse::<u32>() == Ok(default) => format!("{scheme}://{host}"),
        _ => String::from("null"),
    }
}

/// `targetOrigin` of `postMessage`: what it asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// `"*"`.
    Any,
    /// `"/"`: the sender's own origin.
    SameAsSender,
    /// An origin (already normalised by [`origin_of`]).
    Origin(String),
}

/// Read a `targetOrigin`. None: a SyntaxError (not `*`, not `/`, not an address).
pub fn parse_target(s: &str) -> Option<Target> {
    match s {
        "*" => Some(Target::Any),
        "/" => Some(Target::SameAsSender),
        _ => {
            let o = origin_of(s);
            // An address that is not http(s) names an opaque origin: it matches nothing, but
            // it is not a syntax error either. Only text that is no address at all is.
            if o != "null" {
                Some(Target::Origin(o))
            } else if s.contains("://") || s.starts_with("null") || s == "null" {
                Some(Target::Origin(String::from("null")))
            } else {
                None
            }
        }
    }
}

impl Target {
    /// Does a window whose origin is `receiver` get a message sent from `sender`?
    pub fn allows(&self, sender: &str, receiver: &Origin) -> bool {
        match self {
            Target::Any => true,
            Target::SameAsSender => !receiver.is_opaque() && receiver.text == sender,
            // An opaque origin is never named by an address.
            Target::Origin(o) => !receiver.is_opaque() && receiver.text == *o,
        }
    }
}

/// How a window property is used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Use {
    Get,
    Set,
    Call,
}

/// May a script of another origin use property `name` of a window? (`Call` is a `Get` of a
/// method that is then called: both must be allowed.)
pub fn window_allows(name: &str, how: Use) -> bool {
    match how {
        Use::Get => matches!(
            name,
            "window" | "self" | "location" | "close" | "closed" | "focus" | "blur" | "frames"
                | "length" | "top" | "opener" | "parent" | "postMessage"
        ),
        Use::Set => name == "location",
        Use::Call => matches!(name, "close" | "focus" | "blur" | "postMessage"),
    }
}

/// The same for the `location` of a window of another origin: only `href` may be set, and
/// `replace` called.
pub fn location_allows(name: &str, how: Use) -> bool {
    match how {
        Use::Get => name == "replace",
        Use::Set => name == "href",
        Use::Call => name == "replace",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(t: &str, d: u64) -> Origin {
        Origin {
            text: t.into(),
            doc: d,
        }
    }

    #[test]
    fn origins_of_addresses() {
        let rows = [
            ("http://a.test/x", "http://a.test"),
            ("HTTP://A.Test:80/x?y#z", "http://a.test"),
            ("https://a.test:443/", "https://a.test"),
            ("https://a.test:8443/", "https://a.test:8443"),
            ("http://a.test:443/", "http://a.test:443"),
            (concat!("http://user:pw", "@", "a.test:81/"), "http://a.test:81"),
            ("http://[::1]:8000/", "http://[::1]:8000"),
            ("http://a.test./", "http://a.test"),
            ("http://a.test", "http://a.test"),
            ("http://a.test:/", "http://a.test"),
            ("http://a.test:bad/", "null"),
            ("data:text/html,x", "null"),
            ("about:blank", "null"),
            ("javascript:1", "null"),
            ("file:///etc/hosts", "null"),
            ("http:///x", "null"),
            ("", "null"),
        ];
        for (u, want) in rows {
            assert_eq!(origin_of(u), want, "{u}");
        }
    }

    #[test]
    fn same_origin() {
        assert!(o("http://a.test", 1).same(&o("http://a.test", 2)));
        assert!(!o("http://a.test", 1).same(&o("https://a.test", 1)));
        assert!(!o("http://a.test", 1).same(&o("http://a.test:81", 1)));
        assert!(!o("http://a.test", 1).same(&o("http://www.a.test", 1)));
        // Opaque origins are only the same as themselves.
        assert!(o("null", 5).same(&o("null", 5)));
        assert!(!o("null", 5).same(&o("null", 6)));
        assert!(!o("null", 5).same(&o("http://a.test", 5)));
        assert!(!o("http://a.test", 5).same(&o("null", 5)));
    }

    #[test]
    fn target_origins() {
        assert_eq!(parse_target("*"), Some(Target::Any));
        assert_eq!(parse_target("/"), Some(Target::SameAsSender));
        assert_eq!(
            parse_target("http://A.test:80/path"),
            Some(Target::Origin("http://a.test".into()))
        );
        assert_eq!(parse_target("not a url"), None);
        assert_eq!(parse_target(""), None);
        assert_eq!(parse_target("a.test"), None);
        assert_eq!(parse_target("null"), Some(Target::Origin("null".into())));
        assert_eq!(parse_target("data:text/html,x"), None);
        assert_eq!(parse_target("ftp://a.test/"), Some(Target::Origin("null".into())));
    }

    #[test]
    fn target_matching() {
        let a = o("http://a.test", 1);
        let opaque = o("null", 2);
        assert!(Target::Any.allows("http://b.test", &a));
        assert!(Target::Any.allows("http://b.test", &opaque));
        assert!(Target::SameAsSender.allows("http://a.test", &a));
        assert!(!Target::SameAsSender.allows("http://b.test", &a));
        assert!(!Target::SameAsSender.allows("null", &opaque));
        assert!(Target::Origin("http://a.test".into()).allows("x", &a));
        assert!(!Target::Origin("http://b.test".into()).allows("x", &a));
        assert!(!Target::Origin("null".into()).allows("x", &opaque));
        assert!(!Target::Origin("http://a.test".into()).allows("x", &opaque));
    }

    #[test]
    fn cross_origin_properties() {
        for p in ["window", "self", "frames", "length", "top", "parent", "opener", "closed", "location", "postMessage"] {
            assert!(window_allows(p, Use::Get), "{p}");
        }
        for p in ["document", "name", "frameElement", "localStorage", "alert", "eval", "constructor", "toString", "then", "foo"] {
            assert!(!window_allows(p, Use::Get), "{p}");
            assert!(!window_allows(p, Use::Set), "{p}");
        }
        assert!(window_allows("location", Use::Set));
        assert!(!window_allows("top", Use::Set));
        assert!(!window_allows("parent", Use::Set));
        assert!(window_allows("postMessage", Use::Call));
        assert!(!window_allows("location", Use::Call));
        assert!(location_allows("href", Use::Set));
        assert!(!location_allows("href", Use::Get));
        assert!(location_allows("replace", Use::Call));
        for p in ["pathname", "search", "hash", "origin", "assign", "reload", "host", "toString"] {
            assert!(!location_allows(p, Use::Get), "{p}");
            assert!(!location_allows(p, Use::Set), "{p}");
        }
    }

    #[test]
    fn hostile_origin_text_does_not_panic() {
        for s in ["://", "http://", "http://@", "http://[", "http://[]:1", "http://a:99999999999/", "\0", "http://\u{e9}.test/", "http://a/\u{1F600}"] {
            let _ = origin_of(s);
            let _ = parse_target(s);
        }
    }
}
