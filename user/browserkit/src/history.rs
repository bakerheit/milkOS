//! Back and forward, per tab: a list of the pages visited and where the tab is in it, with
//! each page's title and scroll position (so Back returns to where you were). Plus the
//! Browser-wide list of visited pages for the History page.

use alloc::string::String;
use alloc::vec::Vec;

/// The most entries a tab keeps (older ones fall off the start).
pub const MAX_ENTRIES: usize = 100;
/// The most pages the History page keeps.
pub const MAX_VISITS: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The address as shown (a web address, or a built-in page like `milk:bookmarks`).
    pub url: String,
    pub title: String,
    /// Where the page was scrolled to when the tab left it.
    pub scroll: i32,
}

#[derive(Clone, Debug, Default)]
pub struct History {
    entries: Vec<Entry>,
    pos: usize,
}

impl History {
    pub fn new() -> History {
        History::default()
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.pos)
    }

    pub fn current_mut(&mut self) -> Option<&mut Entry> {
        self.entries.get_mut(self.pos)
    }

    pub fn can_back(&self) -> bool {
        self.pos > 0
    }

    pub fn can_forward(&self) -> bool {
        self.pos + 1 < self.entries.len()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A new page: everything forward of here is dropped.
    pub fn push(&mut self, url: &str) {
        if !self.entries.is_empty() {
            self.entries.truncate(self.pos + 1);
        }
        self.entries.push(Entry {
            url: String::from(url),
            title: String::new(),
            scroll: 0,
        });
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        self.pos = self.entries.len() - 1;
    }

    /// The current page turned out to be somewhere else (a redirect).
    pub fn replace(&mut self, url: &str) {
        match self.entries.get_mut(self.pos) {
            Some(e) => e.url = String::from(url),
            None => self.push(url),
        }
    }

    /// Step back (`-1`) or forward (`1`), remembering `scroll` for the page left.
    pub fn go(&mut self, delta: isize, scroll: i32) -> Option<&Entry> {
        let to = self.pos as isize + delta;
        if to < 0 || to as usize >= self.entries.len() {
            return None;
        }
        if let Some(e) = self.entries.get_mut(self.pos) {
            e.scroll = scroll;
        }
        self.pos = to as usize;
        self.entries.get(self.pos)
    }
}

/// One visit, for the History page (newest last).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Visit {
    pub url: String,
    pub title: String,
    pub when: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Visits(pub Vec<Visit>);

impl Visits {
    /// Record a visit; visiting the same address again moves it to the end.
    pub fn add(&mut self, url: &str, title: &str, when: i64) {
        self.0.retain(|v| v.url != url);
        self.0.push(Visit {
            url: String::from(url),
            title: String::from(title),
            when,
        });
        if self.0.len() > MAX_VISITS {
            let n = self.0.len() - MAX_VISITS;
            self.0.drain(..n);
        }
    }

    /// The page's title arrived after the visit was recorded.
    pub fn set_title(&mut self, url: &str, title: &str) {
        if let Some(v) = self.0.iter_mut().rev().find(|v| v.url == url) {
            v.title = String::from(title);
        }
    }

    /// `when \t url \t title` lines.
    pub fn to_file(&self) -> String {
        let mut s = String::new();
        for v in &self.0 {
            s.push_str(&alloc::format!(
                "{}\t{}\t{}\n",
                v.when,
                v.url,
                crate::clean_line(&v.title)
            ));
        }
        s
    }

    pub fn from_file(text: &str) -> Visits {
        let mut v = Visits::default();
        for l in text.lines() {
            let mut f = l.splitn(3, '\t');
            if let (Some(w), Some(u), t) = (f.next(), f.next(), f.next())
                && let Ok(when) = w.parse::<i64>()
                && !u.is_empty()
            {
                v.add(u, t.unwrap_or(""), when);
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_forward_and_branching() {
        let mut h = History::new();
        assert!(!h.can_back() && !h.can_forward() && h.current().is_none());
        h.push("a");
        h.push("b");
        h.push("c");
        assert_eq!(h.go(-1, 120).unwrap().url, "b");
        assert_eq!(h.go(-1, 0).unwrap().url, "a");
        assert!(h.go(-1, 0).is_none());
        assert_eq!(h.go(1, 0).unwrap().url, "b");
        assert_eq!(h.go(1, 0).unwrap().url, "c");
        assert_eq!(h.current().unwrap().scroll, 120);
        h.go(-2, 0);
        h.push("d");
        assert!(!h.can_forward());
        assert_eq!(h.len(), 2);
        h.replace("d2");
        assert_eq!(h.current().unwrap().url, "d2");
        for i in 0..150 {
            h.push(&alloc::format!("p{i}"));
        }
        assert_eq!(h.len(), MAX_ENTRIES);
        assert_eq!(h.current().unwrap().url, "p149");
    }

    #[test]
    fn visits() {
        let mut v = Visits::default();
        v.add("http://a/", "A", 1);
        v.add("http://b/", "", 2);
        v.add("http://a/", "A again", 3);
        v.set_title("http://b/", "B\tand\nmore");
        assert_eq!(v.0.len(), 2);
        assert_eq!(v.0[1].url, "http://a/");
        let back = Visits::from_file(&v.to_file());
        assert_eq!(back.0[0].title, "B and more");
        assert_eq!(back.0[1].when, 3);
        assert!(Visits::from_file("junk\nx\ty\n\t\t\n").0.is_empty());
    }
}
