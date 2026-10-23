//! Web Storage areas (localStorage, sessionStorage) for the page bindings
//! (webengine's `Storage`), and their file format: `~/Library/Browser/storage/<origin>`
//! holds one origin's localStorage, a line per item, `key <TAB> value`, with `\\`, tab,
//! newline and carriage return escaped. A quota per origin (5 MB of keys and values, as
//! other browsers) keeps a page from filling the disk.

use alloc::string::String;
use alloc::vec::Vec;

use crate::engine::Storage;

/// Bytes of keys and values one origin may keep (UTF-8, counted like UTF-16 in spirit:
/// two bytes a character is what browsers charge; here the UTF-8 length).
pub const QUOTA: usize = 5 << 20;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Area {
    items: Vec<(String, String)>,
    bytes: usize,
    pub quota: usize,
    /// Changed since it was last saved.
    pub dirty: bool,
}

impl Area {
    pub fn new() -> Area {
        Area {
            quota: QUOTA,
            ..Area::default()
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn to_file(&self) -> String {
        let mut s = String::new();
        for (k, v) in &self.items {
            escape(k, &mut s);
            s.push('\t');
            escape(v, &mut s);
            s.push('\n');
        }
        s
    }

    pub fn from_file(text: &str) -> Area {
        let mut a = Area::new();
        for line in text.split('\n') {
            if line.is_empty() {
                continue;
            }
            let Some((k, v)) = line.split_once('\t') else {
                continue;
            };
            let (k, v) = (unescape(k), unescape(v));
            if a.bytes + k.len() + v.len() > a.quota {
                break;
            }
            a.set(&k, &v);
        }
        a.dirty = false;
        a
    }
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

impl Storage for Area {
    fn len(&self) -> usize {
        self.items.len()
    }
    fn key(&self, i: usize) -> Option<String> {
        self.items.get(i).map(|(k, _)| k.clone())
    }
    fn get(&self, key: &str) -> Option<String> {
        self.items
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }
    fn set(&mut self, key: &str, value: &str) -> bool {
        match self.items.iter().position(|(k, _)| k == key) {
            Some(i) => {
                let old = self.items[i].1.len();
                if self.bytes - old + value.len() > self.quota {
                    return false;
                }
                if self.items[i].1 != value {
                    self.bytes = self.bytes - old + value.len();
                    self.items[i].1 = String::from(value);
                    self.dirty = true;
                }
            }
            None => {
                if self.bytes + key.len() + value.len() > self.quota {
                    return false;
                }
                self.bytes += key.len() + value.len();
                self.items.push((String::from(key), String::from(value)));
                self.dirty = true;
            }
        }
        true
    }
    fn remove(&mut self, key: &str) {
        if let Some(i) = self.items.iter().position(|(k, _)| k == key) {
            let (k, v) = self.items.remove(i);
            self.bytes -= k.len() + v.len();
            self.dirty = true;
        }
    }
    fn clear(&mut self) {
        if !self.items.is_empty() {
            self.items.clear();
            self.bytes = 0;
            self.dirty = true;
        }
    }
}

/// The file name for an origin's storage: the origin with anything but letters, digits, `.`
/// and `-` as `%XX` (one name per origin, and no origin can name a path outside the folder).
pub fn file_name(origin: &str) -> String {
    let mut s = String::new();
    for b in origin.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'-' => s.push(b as char),
            _ => s.push_str(&alloc::format!("%{b:02X}")),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_quota_and_files() {
        let mut a = Area::new();
        assert!(a.set("k", "v"));
        assert!(a.set("tab\tkey", "line\nbreak\\slash"));
        assert!(a.set("k", "v2"));
        assert_eq!(a.len(), 2);
        assert_eq!(a.key(0).as_deref(), Some("k"));
        assert_eq!(a.get("k").as_deref(), Some("v2"));
        let f = a.to_file();
        assert_eq!(f.lines().count(), 2);
        let b = Area::from_file(&f);
        assert_eq!(b.get("tab\tkey").as_deref(), Some("line\nbreak\\slash"));
        assert!(!b.dirty);
        a.remove("k");
        assert_eq!(a.len(), 1);
        a.quota = 20;
        a.clear();
        assert!(a.set("a", "0123456789"));
        assert!(!a.set("b", "0123456789"));
        assert!(a.set("a", "x"));
        assert!(a.set("b", "0123456789"));
        assert_eq!(a.bytes(), 13);
        assert!(a.dirty);
    }

    #[test]
    fn origin_file_names() {
        assert_eq!(file_name("http://www.milk.test:8080"), "http%3A%2F%2Fwww.milk.test%3A8080");
        assert_eq!(file_name("http://[::1]:80"), "http%3A%2F%2F%5B%3A%3A1%5D%3A80");
        assert!(!file_name("http://../x").contains('/'));
    }
}
