//! What the address bar does with what was typed: a web address, a search for bare words,
//! or one of the Browser's own pages (`milk:bookmarks`, ...). Also splitting off the
//! `#fragment` (it never goes to the server, but the page scrolls to it).

use alloc::format;
use alloc::string::String;

use milkweb::url::{self, Url, UrlError};

/// The Browser's own pages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Internal {
    /// A new tab: the bookmarks and a search box hint.
    NewTab,
    Bookmarks,
    History,
    Downloads,
    /// The cookies the Browser keeps, by site.
    Cookies,
}

impl Internal {
    pub const ALL: [Internal; 5] = [
        Internal::NewTab,
        Internal::Bookmarks,
        Internal::History,
        Internal::Downloads,
        Internal::Cookies,
    ];

    pub fn address(self) -> &'static str {
        match self {
            Internal::NewTab => "milk:newtab",
            Internal::Bookmarks => "milk:bookmarks",
            Internal::History => "milk:history",
            Internal::Downloads => "milk:downloads",
            Internal::Cookies => "milk:cookies",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Internal::NewTab => "New Tab",
            Internal::Bookmarks => "Bookmarks",
            Internal::History => "History",
            Internal::Downloads => "Downloads",
            Internal::Cookies => "Cookies",
        }
    }

    pub fn parse(s: &str) -> Option<Internal> {
        let s = s.trim().to_ascii_lowercase();
        let name = s
            .strip_prefix("milk:")
            .or_else(|| s.strip_prefix("about:"))?;
        Some(match name.trim_start_matches('/') {
            "" | "newtab" | "blank" | "home" => Internal::NewTab,
            "bookmarks" => Internal::Bookmarks,
            "history" => Internal::History,
            "downloads" => Internal::Downloads,
            "cookies" => Internal::Cookies,
            _ => return None,
        })
    }
}

/// Where the address bar (or a link) leads.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Target {
    /// A web address and its `#fragment` (without the `#`), if any.
    Web(Url, Option<String>),
    /// Bare words: the search engine's address for them (needs the internet).
    Search(Url, String),
    Internal(Internal),
    /// Not something the Browser can open (why, in words).
    Bad(String),
}

/// The search page for `words` (DuckDuckGo's lite, text-only one).
pub fn search_url(words: &str) -> String {
    format!(
        "https://lite.duckduckgo.com/lite/?q={}",
        url::query_encode(words)
    )
}

/// Split `text` at its first `#`: the address and the fragment.
pub fn split_fragment(text: &str) -> (&str, Option<&str>) {
    match text.split_once('#') {
        Some((a, f)) => (a, Some(f)),
        None => (text, None),
    }
}

fn web(u: Result<Url, UrlError>, frag: Option<&str>) -> Target {
    match u {
        Ok(u) => Target::Web(u, frag.filter(|f| !f.is_empty()).map(String::from)),
        Err(e) => Target::Bad(format!("{e}")),
    }
}

/// What typed `text` means.
pub fn interpret(text: &str) -> Target {
    let t = text.trim();
    if t.is_empty() {
        return Target::Bad(String::from("there's no address"));
    }
    if let Some(i) = Internal::parse(t) {
        return Target::Internal(i);
    }
    if url::looks_like_search(t) {
        let s = search_url(t);
        return match Url::parse(&s) {
            Ok(u) => Target::Search(u, String::from(t)),
            Err(e) => Target::Bad(format!("{e}")),
        };
    }
    let (a, frag) = split_fragment(t);
    web(Url::parse(a), frag)
}

/// Where a link or form `href` on page `base` leads.
pub fn resolve(base: Option<&Url>, href: &str) -> Target {
    let h = href.trim();
    if let Some(i) = Internal::parse(h) {
        return Target::Internal(i);
    }
    if let Some(f) = h.strip_prefix('#')
        && let Some(b) = base
    {
        return Target::Web(b.clone(), Some(String::from(f)));
    }
    let (a, frag) = split_fragment(h);
    let lower = a.to_ascii_lowercase();
    for bad in [
        "javascript:",
        "data:",
        "file:",
        "mailto:",
        "ftp:",
        "tel:",
        "blob:",
    ] {
        if lower.starts_with(bad) {
            return Target::Bad(format!(
                "{}: links like this can't be opened here (only http:// and https://)",
                bad.trim_end_matches(':')
            ));
        }
    }
    match base {
        Some(b) => web(b.join(a), frag),
        None => web(Url::parse(a), frag),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed() {
        assert!(
            matches!(interpret("milk cows"), Target::Search(u, _) if u.host == "lite.duckduckgo.com" && u.path == "/lite/?q=milk+cows")
        );
        assert!(matches!(interpret("cows"), Target::Search(..)));
        assert_eq!(
            interpret("milk:bookmarks"),
            Target::Internal(Internal::Bookmarks)
        );
        assert_eq!(interpret("about:blank"), Target::Internal(Internal::NewTab));
        match interpret("10.0.2.2:8000/a#top") {
            Target::Web(u, f) => {
                assert_eq!(u.to_text(), "http://10.0.2.2:8000/a");
                assert_eq!(f.as_deref(), Some("top"));
            }
            t => panic!("{t:?}"),
        }
        assert!(matches!(interpret("https://milk.test/"), Target::Web(u, None) if u.port == 443));
        assert!(matches!(interpret("ftp://x.test/"), Target::Bad(_)));
        assert!(matches!(interpret("   "), Target::Bad(_)));
        assert!(matches!(
            interpret("milk:nope"),
            Target::Bad(_) | Target::Search(..)
        ));
    }

    #[test]
    fn links() {
        let base = Url::parse("http://milk.test/a/b?x").unwrap();
        assert!(
            matches!(resolve(Some(&base), "c"), Target::Web(u, None) if u.to_text() == "http://milk.test/a/c")
        );
        assert!(
            matches!(resolve(Some(&base), "#sec"), Target::Web(u, Some(f)) if u == base && f == "sec")
        );
        assert!(
            matches!(resolve(Some(&base), "/d#e"), Target::Web(u, Some(f)) if u.path == "/d" && f == "e")
        );
        assert!(matches!(
            resolve(Some(&base), "JavaScript:alert(1)"),
            Target::Bad(_)
        ));
        assert!(matches!(resolve(Some(&base), "mailto:a@b"), Target::Bad(_)));
        assert!(matches!(
            resolve(Some(&base), "milk:history"),
            Target::Internal(Internal::History)
        ));
        assert!(matches!(resolve(None, "http://x.test/"), Target::Web(..)));
    }
}
