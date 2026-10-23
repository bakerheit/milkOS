//! Session history with frames: the tab's list of "where you have been", where an entry is the
//! whole frame tree as it was, as browsers keep it ("joint session history").
//!
//! # The model
//!
//! * An [`Entry`] is the top document's address, title and scroll, plus a tree of [`Node`]s,
//!   one per frame: the address its document was loaded from, a [`DocId`] naming *that
//!   document* (a number the Browser gives every document it loads), the frame's scroll, and
//!   the frames inside it. Frames are told apart by a **key**: the frame's `name`, or `#N` (its
//!   place among its siblings) when it has none or the name is taken ([`key_for`]).
//! * The first load of a frame (the `src` it has when its page is parsed, or when a script
//!   adds it) is part of the page's own entry: no new entry ([`SessionHistory::frame_loaded`]
//!   with `initial`). A later navigation of a frame (a link into it, a form, `location`,
//!   `history.pushState` or a fragment change inside it) adds an entry that is the current one
//!   with that frame's node replaced; the other frames keep the documents they show. A
//!   navigation to the address the frame already shows replaces instead (the standard's rule
//!   for a same-address navigation), and so does `location.replace`.
//! * Back and forward ([`SessionHistory::traverse`]) compare the entry left with the entry
//!   reached, frame by frame: a frame whose [`DocId`] is the same keeps its document (only a
//!   fragment, `pushState` address or scroll position is restored); a frame whose document
//!   differs is **reloaded** from the address in the entry, and the frames the entry has inside
//!   it are restored to *their* addresses when they appear ([`restore_for`]) instead of the
//!   `src` the markup gives. Nothing is kept in a back-forward cache: a reload fetches again.
//! * Entries a script pushes with `history.pushState` in the top page or a frame are
//!   same-document entries like fragment navigations.
//! * At most [`MAX_ENTRIES`] entries; the oldest fall off. A tree holds at most
//!   [`MAX_NODES`] nodes (frames beyond that are not recorded; they still load).
//!
//! Skipped: form POST bodies are not kept, so going back to a page a POST produced fetches it
//! with GET (as the Browser already does for top-level pages), and `window.name` is not part of
//! history.

use alloc::string::String;
use alloc::vec::Vec;

/// A number the Browser gives each document it loads (never reused).
pub type DocId = u64;

/// Entries a tab keeps.
pub const MAX_ENTRIES: usize = 100;
/// Frame nodes one entry may hold (the engine's frame cap with room to spare).
pub const MAX_NODES: usize = 64;

/// The key of a frame among its siblings: its `name` when it has one that no earlier sibling
/// uses, else `#index`.
pub fn key_for(name: &str, index: usize, taken: &[String]) -> String {
    if !name.is_empty() && !name.starts_with('#') && !taken.iter().any(|t| t == name) {
        String::from(name)
    } else {
        alloc::format!("#{index}")
    }
}

/// One frame in an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub key: String,
    pub doc: DocId,
    /// The address as shown (with its `#fragment`).
    pub url: String,
    pub scroll: i32,
    pub children: Vec<Node>,
}

impl Node {
    pub fn new(key: &str, doc: DocId, url: &str) -> Node {
        Node {
            key: String::from(key),
            doc,
            url: String::from(url),
            scroll: 0,
            children: Vec::new(),
        }
    }

    fn count(&self) -> usize {
        1 + self.children.iter().map(Node::count).sum::<usize>()
    }

    fn find(&self, path: &[String]) -> Option<&Node> {
        let Some((k, rest)) = path.split_first() else {
            return Some(self);
        };
        self.children
            .iter()
            .find(|c| c.key == *k)
            .and_then(|c| c.find(rest))
    }

    fn find_mut(&mut self, path: &[String]) -> Option<&mut Node> {
        let Some((k, rest)) = path.split_first() else {
            return Some(self);
        };
        self.children
            .iter_mut()
            .find(|c| c.key == *k)
            .and_then(|c| c.find_mut(rest))
    }
}

/// A place in the frame tree: the keys from the top page down (empty: the top page itself).
pub type Path = Vec<String>;

/// One entry of the session history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The top document's address as shown (a web address, or a built-in page like
    /// `milk:bookmarks`).
    pub url: String,
    pub title: String,
    /// Where the top page was scrolled to when the tab left the entry.
    pub scroll: i32,
    pub doc: DocId,
    /// The frames in the top document.
    pub frames: Vec<Node>,
}

impl Entry {
    fn root_node(&self) -> Node {
        Node {
            key: String::new(),
            doc: self.doc,
            url: self.url.clone(),
            scroll: self.scroll,
            children: self.frames.clone(),
        }
    }

    fn find(&self, path: &[String]) -> Option<&Node> {
        let (k, rest) = path.split_first()?;
        self.frames.iter().find(|c| c.key == *k).and_then(|c| c.find(rest))
    }

    fn find_mut(&mut self, path: &[String]) -> Option<&mut Node> {
        let (k, rest) = path.split_first()?;
        self.frames
            .iter_mut()
            .find(|c| c.key == *k)
            .and_then(|c| c.find_mut(rest))
    }

    fn nodes(&self) -> usize {
        self.frames.iter().map(Node::count).sum()
    }
}

/// What to do to reach an entry: how its frames differ from the ones on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Traversal {
    /// The entry reached.
    pub entry: Entry,
    /// The top document is another one: load `entry.url` and restore `entry.frames` as they
    /// appear.
    pub top_reload: bool,
    /// Frames whose documents differ (outermost ones only): load `node.url`, restore
    /// `node.children` as they appear.
    pub reloads: Vec<Reload>,
    /// Frames (and the top page, path empty) that keep their document: the address (a
    /// fragment or `pushState` address may differ) and scroll to restore.
    pub restores: Vec<Restore>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reload {
    pub path: Path,
    pub node: Node,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restore {
    pub path: Path,
    pub url: String,
    pub scroll: i32,
    /// The address differs from the one on screen (a same-document navigation).
    pub url_changed: bool,
}

/// The node a frame created at `path` should load, if a traversal says what it showed: the
/// outermost reload that contains `path` knows the subtree.
pub fn restore_for<'a>(reloads: &'a [Reload], path: &[String]) -> Option<&'a Node> {
    for r in reloads {
        if path.len() >= r.path.len() && path[..r.path.len()] == r.path[..] && path.len() > r.path.len() {
            return r.node.find(&path[r.path.len()..]);
        }
    }
    None
}

/// The tab's history.
#[derive(Clone, Debug, Default)]
pub struct SessionHistory {
    entries: Vec<Entry>,
    pos: usize,
}

impl SessionHistory {
    pub fn new() -> SessionHistory {
        SessionHistory::default()
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

    pub fn position(&self) -> usize {
        self.pos
    }

    /// A new top-level page: everything forward of here is dropped. `doc` names its document
    /// (0 if it is not a web document: a built-in page).
    pub fn push(&mut self, url: &str) {
        self.push_doc(url, 0);
    }

    pub fn push_doc(&mut self, url: &str, doc: DocId) {
        if !self.entries.is_empty() {
            self.entries.truncate(self.pos + 1);
        }
        self.entries.push(Entry {
            url: String::from(url),
            title: String::new(),
            scroll: 0,
            doc,
            frames: Vec::new(),
        });
        self.trim();
        self.pos = self.entries.len() - 1;
    }

    fn trim(&mut self) {
        if self.entries.len() > MAX_ENTRIES {
            let n = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..n);
        }
    }

    /// The current page turned out to be somewhere else (a redirect), or is a new load of the
    /// same entry (reload): its frames are gone with the old document.
    pub fn replace(&mut self, url: &str) {
        self.replace_doc(url, 0);
    }

    pub fn replace_doc(&mut self, url: &str, doc: DocId) {
        match self.entries.get_mut(self.pos) {
            Some(e) => {
                e.url = String::from(url);
                e.doc = doc;
                e.frames.clear();
            }
            None => self.push_doc(url, doc),
        }
    }

    /// The top document in the current entry is now `doc` (the entry's address is kept).
    pub fn set_top_doc(&mut self, doc: DocId) {
        if let Some(e) = self.entries.get_mut(self.pos) {
            e.doc = doc;
        }
    }

    /// A document that was loaded again (back, forward, reload) is now `new`: every entry that
    /// named `old` (the entries of one document, a fragment or `pushState` apart) names `new`.
    /// Frames the entries held inside it stay as they are, to be restored as they load.
    pub fn rebind(&mut self, old: DocId, new: DocId) {
        if old == 0 || old == new {
            return;
        }
        fn walk(n: &mut Node, old: DocId, new: DocId) {
            if n.doc == old {
                n.doc = new;
            }
            for c in &mut n.children {
                walk(c, old, new);
            }
        }
        for e in &mut self.entries {
            if e.doc == old {
                e.doc = new;
            }
            for f in &mut e.frames {
                walk(f, old, new);
            }
        }
    }

    /// Save the scroll position of a frame in the current entry (path empty: the top page).
    pub fn save_scroll(&mut self, path: &[String], scroll: i32) {
        let Some(e) = self.entries.get_mut(self.pos) else {
            return;
        };
        if path.is_empty() {
            e.scroll = scroll;
        } else if let Some(n) = e.find_mut(path) {
            n.scroll = scroll;
        }
    }

    /// The node at `path` in the current entry.
    pub fn node(&self, path: &[String]) -> Option<&Node> {
        self.current()?.find(path)
    }

    /// A frame's document loaded. `initial`: the frame's first load (its `src`, or what a
    /// traversal asked for): recorded in the current entry, no new one. Otherwise it is a
    /// navigation: a new entry (the current one with this frame changed), or, when `replace`
    /// is set or the address is the one the frame already shows, the current entry changed in
    /// place. A frame that is not in the entry yet is added (under its parent's path).
    pub fn frame_loaded(
        &mut self,
        path: &[String],
        doc: DocId,
        url: &str,
        initial: bool,
        replace: bool,
    ) {
        if path.is_empty() || self.entries.is_empty() {
            return;
        }
        let Some(cur) = self.entries.get(self.pos) else {
            return;
        };
        let exists = cur.find(path).is_some();
        let same_url = cur.find(path).is_some_and(|n| n.url == url);
        if !exists || initial || replace || same_url {
            self.put(path, doc, url);
            return;
        }
        // A navigation: an entry of its own.
        let mut next = cur.clone();
        let (key, parent) = path.split_last().unwrap();
        let node = Node::new(key, doc, url);
        let slot = if parent.is_empty() {
            Some(&mut next.frames)
        } else {
            next.find_mut(parent).map(|n| &mut n.children)
        };
        if let Some(v) = slot
            && let Some(i) = v.iter().position(|n| n.key == *key)
        {
            v[i] = node;
        }
        self.entries.truncate(self.pos + 1);
        self.entries.push(next);
        self.trim();
        self.pos = self.entries.len() - 1;
    }

    /// Record `path` = (doc, url) in the current entry, adding the node (and dropping what was
    /// inside the frame before).
    fn put(&mut self, path: &[String], doc: DocId, url: &str) {
        let Some(e) = self.entries.get_mut(self.pos) else {
            return;
        };
        let (key, parent) = path.split_last().unwrap();
        let had_nodes = e.nodes();
        let node = Node::new(key, doc, url);
        let slot = if parent.is_empty() {
            Some(&mut e.frames)
        } else {
            e.find_mut(parent).map(|n| &mut n.children)
        };
        let Some(v) = slot else { return };
        match v.iter().position(|n| n.key == *key) {
            Some(i) => {
                // Keep the scroll of a document that only changed address in place? No: a new
                // document starts at the top.
                v[i] = node;
            }
            None => {
                if had_nodes < MAX_NODES {
                    v.push(node);
                }
            }
        }
    }

    /// A same-document navigation in a frame (a fragment, `pushState`): the address changes,
    /// the document stays. A new entry unless `replace`.
    pub fn frame_same_doc(&mut self, path: &[String], url: &str, replace: bool) {
        let Some(cur) = self.entries.get(self.pos) else {
            return;
        };
        if path.is_empty() {
            return;
        }
        if cur.find(path).is_none() {
            return;
        }
        if replace {
            if let Some(n) = self.entries[self.pos].find_mut(path) {
                n.url = String::from(url);
            }
            return;
        }
        let mut next = cur.clone();
        if let Some(n) = next.find_mut(path) {
            n.url = String::from(url);
        }
        self.entries.truncate(self.pos + 1);
        self.entries.push(next);
        self.trim();
        self.pos = self.entries.len() - 1;
    }

    /// A same-document navigation of the top page (a fragment, `pushState`): like
    /// [`SessionHistory::push`] but the document (and the frames in it) stay.
    pub fn top_same_doc(&mut self, url: &str, replace: bool) {
        let Some(cur) = self.entries.get(self.pos) else {
            return self.push(url);
        };
        if replace {
            self.entries[self.pos].url = String::from(url);
            return;
        }
        let mut next = cur.clone();
        next.url = String::from(url);
        next.scroll = 0;
        self.entries.truncate(self.pos + 1);
        self.entries.push(next);
        self.trim();
        self.pos = self.entries.len() - 1;
    }

    /// A frame was removed from its page.
    pub fn frame_removed(&mut self, path: &[String]) {
        let Some(e) = self.entries.get_mut(self.pos) else {
            return;
        };
        let Some((key, parent)) = path.split_last() else {
            return;
        };
        let slot = if parent.is_empty() {
            Some(&mut e.frames)
        } else {
            e.find_mut(parent).map(|n| &mut n.children)
        };
        if let Some(v) = slot {
            v.retain(|n| n.key != *key);
        }
    }

    /// Step back (`-1`) or forward (`1`), or any `delta`: the new current entry and what
    /// differs from the one left. `scroll`: the top page's scroll position, saved in the
    /// entry left (frames' scrolls are saved with [`SessionHistory::save_scroll`] before).
    pub fn traverse(&mut self, delta: isize, scroll: i32) -> Option<Traversal> {
        let to = self.pos as isize + delta;
        if to < 0 || to as usize >= self.entries.len() {
            return None;
        }
        if let Some(e) = self.entries.get_mut(self.pos) {
            e.scroll = scroll;
        }
        let from = self.entries[self.pos].root_node();
        self.pos = to as usize;
        let entry = self.entries[self.pos].clone();
        let to_root = entry.root_node();
        let mut t = Traversal {
            top_reload: from.doc != to_root.doc || from.doc == 0,
            reloads: Vec::new(),
            restores: Vec::new(),
            entry,
        };
        if t.top_reload {
            return Some(t);
        }
        let mut path = Vec::new();
        diff(&from, &to_root, &mut path, &mut t);
        Some(t)
    }

    /// Step as [`History::go`](browserkit::history) does: the entry reached, scroll saved.
    pub fn go(&mut self, delta: isize, scroll: i32) -> Option<&Entry> {
        self.traverse(delta, scroll)?;
        self.entries.get(self.pos)
    }
}

fn diff(from: &Node, to: &Node, path: &mut Path, out: &mut Traversal) {
    out.restores.push(Restore {
        path: path.clone(),
        url: to.url.clone(),
        scroll: to.scroll,
        url_changed: from.url != to.url,
    });
    for c in &to.children {
        let Some(old) = from.children.iter().find(|o| o.key == c.key) else {
            continue; // a frame the page no longer has: nothing to restore into
        };
        path.push(c.key.clone());
        if old.doc != c.doc {
            out.reloads.push(Reload {
                path: path.clone(),
                node: c.clone(),
            });
        } else {
            diff(old, c, path, out);
        }
        path.pop();
    }
}
