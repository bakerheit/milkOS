//! The search engines the Browser's address bar can use for bare words, and the address each
//! one wants. Shared by the Browser (which turns words into an address) and Settings (which
//! lists the engines and saves the choice as `search = KEY` in `~/.config/milk/settings`).
//!
//! The page picked for each engine is its plainest results page, the one that needs the
//! fewest scripts, because the Browser's engine is small. A GET address with the words in a
//! query parameter, https only. Nothing here talks to the network.
//!
//! | key | address | why that page |
//! | --- | --- | --- |
//! | `duckduckgo-lite` (default) | `https://lite.duckduckgo.com/lite/?q=` | DuckDuckGo's text-only page: no scripts, no images, a few KB |
//! | `duckduckgo-html` | `https://html.duckduckgo.com/html/?q=` | DuckDuckGo's no-script page with snippets and a little styling |
//! | `brave` | `https://search.brave.com/search?q=` | Brave has no lite page; its results come in the HTML itself (the scripts only add extras) |
//! | `startpage` | `https://www.startpage.com/sp/search?query=` | the address its own search form uses (it has no lite page) |
//! | `bing` | `https://www.bing.com/search?q=` | results are in the HTML; the scripts only add extras |
//! | `google` | `https://www.google.com/search?q=...&udm=14` | `udm=14` is Google's "Web" results: links and snippets, without the AI box and widgets |
//!
//! Brave, Startpage, Bing and Google may show a check page ("verify you're human") instead of
//! results when they don't like a client. The Browser doesn't work around that; it is the
//! engine's decision, and DuckDuckGo's two pages are the ones that tend to just answer.

use alloc::string::String;

use crate::url::query_encode;

/// How much of the typed words go into the address (bytes). Search engines read only the
/// first part of a long query, and servers refuse addresses much longer than this.
pub const MAX_QUERY: usize = 2000;

/// One search engine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Engine {
    /// The name in the settings file (`search = brave`).
    pub key: &'static str,
    /// The name people see (Settings, the new tab page).
    pub name: &'static str,
    /// A few words about the page (Settings).
    pub note: &'static str,
    /// The address up to where the words go, and what follows them.
    before: &'static str,
    after: &'static str,
}

impl Engine {
    /// The address that searches for `words`. Any run of white space (including line breaks
    /// from a paste) becomes one space; the words are cut at [`MAX_QUERY`] bytes; everything
    /// but letters, digits and `-_.~` is percent-encoded (a space is `+`, a real `+` is `%2B`).
    pub fn url(&self, words: &str) -> String {
        let mut q = String::new();
        for w in words.split_whitespace() {
            if !q.is_empty() {
                q.push(' ');
            }
            q.push_str(w);
            if q.len() > MAX_QUERY {
                let mut cut = MAX_QUERY;
                while !q.is_char_boundary(cut) {
                    cut -= 1;
                }
                q.truncate(cut);
                q.truncate(q.trim_end().len());
                break;
            }
        }
        let mut u = String::from(self.before);
        u.push_str(&query_encode(&q));
        u.push_str(self.after);
        u
    }
}

/// The engines, in the order Settings lists them. The first is the default.
pub const ENGINES: [Engine; 6] = [
    Engine {
        key: "duckduckgo-lite",
        name: "DuckDuckGo (Lite)",
        note: "Smallest page, text only",
        before: "https://lite.duckduckgo.com/lite/?q=",
        after: "",
    },
    Engine {
        key: "duckduckgo-html",
        name: "DuckDuckGo (HTML)",
        note: "Plain page, no scripts",
        before: "https://html.duckduckgo.com/html/?q=",
        after: "",
    },
    Engine {
        key: "brave",
        name: "Brave Search",
        note: "Its own index",
        before: "https://search.brave.com/search?q=",
        after: "",
    },
    Engine {
        key: "startpage",
        name: "Startpage",
        note: "Private, web results",
        before: "https://www.startpage.com/sp/search?query=",
        after: "",
    },
    Engine {
        key: "bing",
        name: "Bing",
        note: "Microsoft's search",
        before: "https://www.bing.com/search?q=",
        after: "",
    },
    Engine {
        key: "google",
        name: "Google",
        note: "Web results only",
        before: "https://www.google.com/search?q=",
        after: "&udm=14",
    },
];

/// The index of the default engine in [`ENGINES`] (DuckDuckGo Lite, as before engines were
/// a choice).
pub const DEFAULT: usize = 0;

/// The index of the engine called `key` (ASCII case and surrounding spaces don't matter;
/// `duckduckgo` alone means the Lite page).
pub fn find(key: &str) -> Option<usize> {
    let k = key.trim();
    if k.eq_ignore_ascii_case("duckduckgo") {
        return Some(DEFAULT);
    }
    ENGINES.iter().position(|e| e.key.eq_ignore_ascii_case(k))
}

/// The engine at `i`, or the default one if there is no such index.
pub fn engine(i: usize) -> &'static Engine {
    ENGINES.get(i).unwrap_or(&ENGINES[DEFAULT])
}

/// The engine a settings file picks: its `search = KEY` line (the last one counts), or the
/// default if there is none or the key is unknown. Reads at most the first 64 KB.
pub fn from_settings(text: &str) -> usize {
    let mut at = DEFAULT;
    let mut end = text.len().min(64 << 10);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    for line in text[..end].lines() {
        if let Some((k, v)) = line.split_once('=')
            && k.trim() == "search"
        {
            at = find(v).unwrap_or(DEFAULT);
        }
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    fn url(key: &str, words: &str) -> String {
        engine(find(key).unwrap()).url(words)
    }

    #[test]
    fn words_in_each_engine() {
        assert_eq!(
            url("duckduckgo-lite", "milk cows"),
            "https://lite.duckduckgo.com/lite/?q=milk+cows"
        );
        assert_eq!(
            url("duckduckgo-html", "milk cows"),
            "https://html.duckduckgo.com/html/?q=milk+cows"
        );
        assert_eq!(
            url("brave", "milk cows"),
            "https://search.brave.com/search?q=milk+cows"
        );
        assert_eq!(
            url("startpage", "milk cows"),
            "https://www.startpage.com/sp/search?query=milk+cows"
        );
        assert_eq!(
            url("bing", "milk cows"),
            "https://www.bing.com/search?q=milk+cows"
        );
        assert_eq!(
            url("google", "milk cows"),
            "https://www.google.com/search?q=milk+cows&udm=14"
        );
    }

    #[test]
    fn table_is_sane() {
        assert_eq!(ENGINES[DEFAULT].key, "duckduckgo-lite");
        for (i, e) in ENGINES.iter().enumerate() {
            assert!(e.before.starts_with("https://"), "{}", e.key);
            assert!(!e.name.is_empty() && !e.note.is_empty());
            // Keys and names are unique, and a key finds its own engine.
            assert_eq!(find(e.key), Some(i));
            assert_eq!(ENGINES.iter().filter(|o| o.name == e.name).count(), 1);
            // Settings file lines are `key = value`: no characters that break one.
            assert!(
                e.key
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'-' || b.is_ascii_digit())
            );
            // The address, once the words are in, is a well-formed https URL.
            let u = e.url("a b");
            assert!(crate::url::Url::parse(&u).is_ok(), "{u}");
        }
    }

    #[test]
    fn unusual_input_is_encoded() {
        for e in &ENGINES {
            let enc = |w: &str| e.url(w)[e.before.len()..e.url(w).len() - e.after.len()].to_owned();
            assert_eq!(enc("a+b"), "a%2Bb");
            assert_eq!(enc("a&b=c"), "a%26b%3Dc");
            assert_eq!(enc("C# and F#"), "C%23+and+F%23");
            assert_eq!(enc("100% milk?"), "100%25+milk%3F");
            assert_eq!(enc("a/b\\c"), "a%2Fb%5Cc");
            assert_eq!(enc("\"quoted\" 'x'"), "%22quoted%22+%27x%27");
            // Unicode goes out as UTF-8 bytes.
            assert_eq!(enc("café"), "caf%C3%A9");
            assert_eq!(enc("γάλα"), "%CE%B3%CE%AC%CE%BB%CE%B1");
            assert_eq!(enc("牛乳 🐄"), "%E7%89%9B%E4%B9%B3+%F0%9F%90%84");
            // White space of every kind, including pasted line breaks, is one space.
            assert_eq!(enc("  milk \t\n cows\r\n "), "milk+cows");
            assert_eq!(enc("a\u{a0}b\u{2003}c"), "a+b+c");
            // Other control characters can't reach the address as they are.
            assert_eq!(enc("a\u{0}b"), "a%00b");
            // Nothing typed, nothing to search for: the address is just the engine's.
            assert_eq!(enc("   "), "");
            // Only unreserved characters stay as they are.
            assert_eq!(enc("a-b_c.d~e"), "a-b_c.d~e");
        }
        // Whatever the engine, the address never has a raw space, '#', '&' or '"' from the words.
        let u = url("google", "x y#z&w \"q\"");
        assert!(u.ends_with("&udm=14"));
        assert_eq!(u.matches('#').count(), 0);
        assert_eq!(u.matches('&').count(), 1);
        assert!(!u.contains(' ') && !u.contains('"'));
    }

    #[test]
    fn very_long_input_is_cut() {
        for e in &ENGINES {
            let long: String = "milk ".repeat(100_000);
            let u = e.url(&long);
            assert!(
                u.len() <= e.before.len() + e.after.len() + MAX_QUERY,
                "{}",
                u.len()
            );
            assert!(u.starts_with(e.before) && u.ends_with(e.after));
            // One huge word.
            let u = e.url(&"x".repeat(1_000_000));
            assert_eq!(u.len(), e.before.len() + e.after.len() + MAX_QUERY);
            // The cut never splits a multi-byte character (each is 3 bytes: 9 as %XX).
            let u = e.url(&"é".repeat(5000));
            let q = &u[e.before.len()..u.len() - e.after.len()];
            assert_eq!(q.len() % 6, 0, "whole characters only");
            assert!(q.len() <= MAX_QUERY * 3);
            let u = e.url(&"牛".repeat(5000));
            let q = &u[e.before.len()..u.len() - e.after.len()];
            assert_eq!(q.len() % 9, 0, "whole characters only");
            // Still a valid address.
            assert!(crate::url::Url::parse(&u).is_ok());
        }
        // The cut may fall inside a word, and never leaves a space at the end.
        let u = engine(DEFAULT).url(&format!("{} tail", "w".repeat(MAX_QUERY - 2)));
        assert!(u.ends_with(&format!("{}+t", "w".repeat(MAX_QUERY - 2))));
        let u = engine(DEFAULT).url(&format!("{} tail", "w".repeat(MAX_QUERY - 1)));
        assert!(u.ends_with(&"w".repeat(MAX_QUERY - 1)));
    }

    #[test]
    fn keys_and_the_settings_file() {
        assert_eq!(find("brave"), Some(2));
        assert_eq!(find("  Brave "), Some(2));
        assert_eq!(find("GOOGLE"), Some(5));
        assert_eq!(find("duckduckgo"), Some(DEFAULT));
        assert_eq!(find("DuckDuckGo-HTML"), Some(1));
        assert_eq!(find("altavista"), None);
        assert_eq!(find(""), None);
        assert_eq!(find("brave search"), None);
        // No file, no line, an unknown or empty value: the default.
        assert_eq!(from_settings(""), DEFAULT);
        assert_eq!(from_settings("theme = whole\nfont = figtree\n"), DEFAULT);
        assert_eq!(from_settings("search = altavista\n"), DEFAULT);
        assert_eq!(from_settings("search =\n"), DEFAULT);
        assert_eq!(from_settings("search\n"), DEFAULT);
        assert_eq!(from_settings("research = brave\n"), DEFAULT);
        assert_eq!(from_settings("# search = brave\n"), DEFAULT);
        // A line among others, spaces optional, either line ending.
        assert_eq!(
            from_settings("theme = whole\nsearch = brave\ntrash = 30\n"),
            2
        );
        assert_eq!(from_settings("search=bing"), 4);
        assert_eq!(
            from_settings("  search   =   startpage  \r\ntheme = whole\r\n"),
            3
        );
        // The last line wins; a later bad one goes back to the default (as Settings reads it).
        assert_eq!(from_settings("search = bing\nsearch = google\n"), 5);
        assert_eq!(from_settings("search = bing\nsearch = nope\n"), DEFAULT);
        // Every engine round-trips through its own key.
        for (i, e) in ENGINES.iter().enumerate() {
            assert_eq!(from_settings(&format!("search = {}\n", e.key)), i);
        }
        // An out-of-range index is the default engine.
        assert_eq!(engine(99).key, ENGINES[DEFAULT].key);
    }

    #[test]
    fn hostile_settings_text() {
        // A huge file: only the first 64 KB are read (a line past it is ignored), no panic.
        let mut t = "x".repeat(70_000);
        t.push_str("\nsearch = google\n");
        assert_eq!(from_settings(&t), DEFAULT);
        let mut t = String::from("search = bing\n");
        t.push_str(&"é".repeat(70_000)); // the cut falls inside a character
        assert_eq!(from_settings(&t), 4);
        assert_eq!(from_settings(&"=".repeat(100_000)), DEFAULT);
        assert_eq!(from_settings("search = \u{0}\n"), DEFAULT);
        assert_eq!(from_settings("search = brave\u{0}\n"), DEFAULT);
    }
}
