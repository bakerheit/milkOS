//! Bookmarks: (title, address) pairs in the order they were added, kept in a text file
//! (`~/Library/Browser/bookmarks`, one `address \t title` a line).

use alloc::string::String;
use alloc::vec::Vec;

pub const MAX: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, Default)]
pub struct Bookmarks(pub Vec<Bookmark>);

impl Bookmarks {
    pub fn contains(&self, url: &str) -> bool {
        self.0.iter().any(|b| b.url == url)
    }

    /// Add `url` (or, if it is there already, take it away). True if it is now bookmarked.
    pub fn toggle(&mut self, url: &str, title: &str) -> bool {
        if self.contains(url) {
            self.0.retain(|b| b.url != url);
            return false;
        }
        if self.0.len() >= MAX || url.is_empty() {
            return false;
        }
        let title = crate::clean_line(title);
        self.0.push(Bookmark {
            url: String::from(url),
            title: if title.trim().is_empty() {
                String::from(url)
            } else {
                title
            },
        });
        true
    }

    pub fn remove(&mut self, url: &str) {
        self.0.retain(|b| b.url != url);
    }

    pub fn to_file(&self) -> String {
        let mut s = String::new();
        for b in &self.0 {
            s.push_str(&b.url);
            s.push('\t');
            s.push_str(&crate::clean_line(&b.title));
            s.push('\n');
        }
        s
    }

    pub fn from_file(text: &str) -> Bookmarks {
        let mut b = Bookmarks::default();
        for l in text.lines() {
            let (u, t) = l.split_once('\t').unwrap_or((l, ""));
            let u = u.trim();
            if !u.is_empty()
                && !u.contains(char::is_whitespace)
                && !b.contains(u)
                && b.0.len() < MAX
            {
                b.0.push(Bookmark {
                    url: String::from(u),
                    title: String::from(if t.is_empty() { u } else { t }),
                });
            }
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_and_file() {
        let mut b = Bookmarks::default();
        assert!(b.toggle("http://a.test/", "A page"));
        assert!(b.toggle("http://b.test/", ""));
        assert!(b.contains("http://a.test/"));
        assert!(!b.toggle("http://a.test/", "A page"));
        assert!(!b.contains("http://a.test/"));
        b.toggle("http://c.test/", "Tabs\tand\nnewlines");
        let back = Bookmarks::from_file(&b.to_file());
        assert_eq!(back.0, b.0);
        assert_eq!(back.0[0].title, "http://b.test/");
        assert_eq!(back.0[1].title, "Tabs and newlines");
        let junk = Bookmarks::from_file("\n \t x\nhttp://dup/\ta\nhttp://dup/\tb\nwith space\tz\n");
        assert_eq!(junk.0.len(), 1);
    }
}
