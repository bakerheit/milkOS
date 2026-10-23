//! What the address bar does with what was typed: a web address, a search for bare words,
//! or one of the Browser's own pages (`milk:bookmarks`, ...). Also splitting off the
//! `#fragment` (it never goes to the server, but the page scrolls to it).

use alloc::format;
use alloc::string::String;

use milkweb::search::{self, Engine};
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

/// The search page for `words` on `engine` (see `milkweb::search` for the pages).
pub fn search_url(engine: &Engine, words: &str) -> String {
    engine.url(words)
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

/// What typed `text` means; bare words search with the default engine.
pub fn interpret(text: &str) -> Target {
    interpret_with(text, search::engine(search::DEFAULT))
}

/// What typed `text` means; bare words search with `engine`.
pub fn interpret_with(text: &str, engine: &Engine) -> Target {
    let t = text.trim();
    if t.is_empty() {
        return Target::Bad(String::from("there's no address"));
    }
    if let Some(i) = Internal::parse(t) {
        return Target::Internal(i);
    }
    if url::looks_like_search(t) {
        let s = search_url(engine, t);
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
    fn words_go_to_the_chosen_engine() {
        let at = |key: &str, words: &str| match interpret_with(
            words,
            search::engine(search::find(key).unwrap()),
        ) {
            Target::Search(u, w) => (u.to_text(), w),
            t => panic!("{t:?}"),
        };
        assert_eq!(
            at("brave", "milk & cows"),
            (
                String::from("https://search.brave.com/search?q=milk+%26+cows"),
                String::from("milk & cows")
            )
        );
        assert_eq!(
            at("google", "café 牛乳").0,
            "https://www.google.com/search?q=caf%C3%A9+%E7%89%9B%E4%B9%B3&udm=14"
        );
        assert_eq!(
            at("startpage", "one").0,
            "https://www.startpage.com/sp/search?query=one"
        );
        // Without a choice, the default engine (DuckDuckGo Lite), as before engines were a setting.
        assert_eq!(
            match interpret("milk cows") {
                Target::Search(u, _) => u.to_text(),
                t => panic!("{t:?}"),
            },
            "https://lite.duckduckgo.com/lite/?q=milk+cows"
        );
        // The engine matters only for words: addresses and the Browser's pages are the same
        // whatever is chosen.
        let bing = search::engine(search::find("bing").unwrap());
        assert!(
            matches!(interpret_with("example.com/a", bing), Target::Web(u, None) if u.host == "example.com")
        );
        assert!(matches!(interpret_with("localhost", bing), Target::Web(..)));
        assert!(matches!(
            interpret_with("10.0.2.2:8000", bing),
            Target::Web(..)
        ));
        assert_eq!(
            interpret_with("milk:history", bing),
            Target::Internal(Internal::History)
        );
        assert!(matches!(interpret_with("   ", bing), Target::Bad(_)));
        // Hostile words stay inside the query.
        for w in [
            "x\r\nHost: evil.test",
            "a#frag",
            "?q=1&q=2",
            "//evil.test/",
            "http://evil.test/ x",
        ] {
            match interpret_with(w, bing) {
                Target::Search(u, _) => {
                    assert_eq!(u.host, "www.bing.com", "{w}");
                    assert!(u.path.starts_with("/search?q="), "{w}");
                    assert!(!u.path.contains(['\r', '\n', ' ', '#']), "{w}");
                }
                Target::Web(..) | Target::Bad(_) => {} // an address, or not one: not a search
                t => panic!("{w}: {t:?}"),
            }
        }
        // A 1 MB paste is cut to a sane address.
        match interpret_with(&"milk ".repeat(200_000), bing) {
            Target::Search(u, _) => assert!(u.to_text().len() < 2100),
            t => panic!("{t:?}"),
        }
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
