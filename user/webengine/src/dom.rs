//! The DOM: an arena of nodes linked as a tree (parent, first/last child, siblings), indexed by
//! [`NodeId`]. Node 0 is the Document. Only what the parser, style system and layout need; no
//! live mutation API beyond what the tree builder uses.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::atoms::{Atom, Interner, tag};

pub type NodeId = u32;
/// "No node".
pub const NONE: NodeId = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ns {
    Html,
    Svg,
    MathMl,
}

#[derive(Clone, Debug)]
pub struct Attr {
    /// Lower-cased (HTML) or as adjusted (foreign content).
    pub name: Box<str>,
    pub value: Box<str>,
}

#[derive(Clone, Debug)]
pub struct Element {
    pub name: Atom,
    pub ns: Ns,
    pub attrs: Vec<Attr>,
    /// The `id` attribute, interned (None if absent or empty).
    pub id: Option<Atom>,
    /// The `class` attribute's tokens, interned.
    pub classes: Vec<Atom>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| &*a.name == name)
            .map(|a| &*a.value)
    }
    pub fn has_attr(&self, name: &str) -> bool {
        self.attrs.iter().any(|a| &*a.name == name)
    }
    pub fn is(&self, t: Atom) -> bool {
        self.ns == Ns::Html && self.name == t
    }
}

#[derive(Clone, Debug)]
pub enum NodeData {
    Document,
    Doctype {
        name: String,
        public_id: String,
        system_id: String,
    },
    Element(Element),
    Text(String),
    Comment(String),
}

#[derive(Clone, Debug)]
pub struct Node {
    pub parent: NodeId,
    pub first_child: NodeId,
    pub last_child: NodeId,
    pub prev: NodeId,
    pub next: NodeId,
    pub data: NodeData,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuirksMode {
    NoQuirks,
    LimitedQuirks,
    Quirks,
}

pub struct Dom {
    pub nodes: Vec<Node>,
    pub atoms: Interner,
    pub quirks: QuirksMode,
}

impl Default for Dom {
    fn default() -> Self {
        Self::new()
    }
}

impl Dom {
    pub fn new() -> Dom {
        Dom {
            nodes: alloc::vec![Node {
                parent: NONE,
                first_child: NONE,
                last_child: NONE,
                prev: NONE,
                next: NONE,
                data: NodeData::Document,
            }],
            atoms: Interner::new(),
            quirks: QuirksMode::NoQuirks,
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    fn push(&mut self, data: NodeData) -> NodeId {
        self.nodes.push(Node {
            parent: NONE,
            first_child: NONE,
            last_child: NONE,
            prev: NONE,
            next: NONE,
            data,
        });
        (self.nodes.len() - 1) as NodeId
    }

    /// A new, detached element. `attrs` names are as given (lower-cased by the tokenizer).
    pub fn create_element(&mut self, name: Atom, ns: Ns, attrs: Vec<Attr>) -> NodeId {
        let mut id = None;
        let mut classes = Vec::new();
        for a in &attrs {
            if &*a.name == "id" && !a.value.is_empty() && id.is_none() {
                id = Some(self.atoms.intern(&a.value));
            } else if &*a.name == "class" && classes.is_empty() {
                for c in a.value.split(is_html_space) {
                    if !c.is_empty() {
                        classes.push(self.atoms.intern(c));
                    }
                }
            }
        }
        self.push(NodeData::Element(Element {
            name,
            ns,
            attrs,
            id,
            classes,
        }))
    }

    pub fn create_text(&mut self, s: String) -> NodeId {
        self.push(NodeData::Text(s))
    }

    pub fn create_comment(&mut self, s: String) -> NodeId {
        self.push(NodeData::Comment(s))
    }

    pub fn create_doctype(&mut self, name: String, public_id: String, system_id: String) -> NodeId {
        self.push(NodeData::Doctype {
            name,
            public_id,
            system_id,
        })
    }

    pub fn node(&self, n: NodeId) -> &Node {
        &self.nodes[n as usize]
    }

    pub fn element(&self, n: NodeId) -> Option<&Element> {
        match self.nodes.get(n as usize).map(|x| &x.data) {
            Some(NodeData::Element(e)) => Some(e),
            _ => None,
        }
    }

    pub fn element_mut(&mut self, n: NodeId) -> Option<&mut Element> {
        match self.nodes.get_mut(n as usize).map(|x| &mut x.data) {
            Some(NodeData::Element(e)) => Some(e),
            _ => None,
        }
    }

    /// The node's HTML tag (EMPTY for non-elements and foreign elements).
    pub fn html_tag(&self, n: NodeId) -> Atom {
        match self.element(n) {
            Some(e) if e.ns == Ns::Html => e.name,
            _ => tag::EMPTY,
        }
    }

    pub fn is_html(&self, n: NodeId, t: Atom) -> bool {
        self.element(n).is_some_and(|e| e.is(t))
    }

    pub fn parent(&self, n: NodeId) -> NodeId {
        self.nodes[n as usize].parent
    }

    pub fn detach(&mut self, n: NodeId) {
        let (p, prev, next) = {
            let x = &self.nodes[n as usize];
            (x.parent, x.prev, x.next)
        };
        if p == NONE {
            return;
        }
        if prev != NONE {
            self.nodes[prev as usize].next = next;
        } else {
            self.nodes[p as usize].first_child = next;
        }
        if next != NONE {
            self.nodes[next as usize].prev = prev;
        } else {
            self.nodes[p as usize].last_child = prev;
        }
        let x = &mut self.nodes[n as usize];
        x.parent = NONE;
        x.prev = NONE;
        x.next = NONE;
    }

    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        self.detach(child);
        let last = self.nodes[parent as usize].last_child;
        {
            let c = &mut self.nodes[child as usize];
            c.parent = parent;
            c.prev = last;
            c.next = NONE;
        }
        if last != NONE {
            self.nodes[last as usize].next = child;
        } else {
            self.nodes[parent as usize].first_child = child;
        }
        self.nodes[parent as usize].last_child = child;
    }

    /// Insert `child` into `parent` before `before` (NONE = append).
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: NodeId) {
        if before == NONE {
            return self.append(parent, child);
        }
        self.detach(child);
        let prev = self.nodes[before as usize].prev;
        {
            let c = &mut self.nodes[child as usize];
            c.parent = parent;
            c.prev = prev;
            c.next = before;
        }
        self.nodes[before as usize].prev = child;
        if prev != NONE {
            self.nodes[prev as usize].next = child;
        } else {
            self.nodes[parent as usize].first_child = child;
        }
    }

    /// Append text at a position: merged into the previous sibling if that is a text node
    /// (HTML "insert a character").
    pub fn insert_text(&mut self, parent: NodeId, before: NodeId, s: &str) {
        let prev = if before == NONE {
            self.nodes[parent as usize].last_child
        } else {
            self.nodes[before as usize].prev
        };
        if prev != NONE
            && let NodeData::Text(t) = &mut self.nodes[prev as usize].data
        {
            t.push_str(s);
            return;
        }
        let t = self.create_text(String::from(s));
        self.insert_before(parent, t, before);
    }

    /// Move all children of `from` to the end of `to`.
    pub fn reparent_children(&mut self, from: NodeId, to: NodeId) {
        let mut c = self.nodes[from as usize].first_child;
        while c != NONE {
            let next = self.nodes[c as usize].next;
            self.append(to, c);
            c = next;
        }
    }

    pub fn children(&self, n: NodeId) -> Children<'_> {
        Children {
            dom: self,
            next: self.nodes[n as usize].first_child,
        }
    }

    /// The next node in tree order after `n` within the subtree rooted at `root`
    /// (pre-order); NONE at the end.
    pub fn next_in(&self, n: NodeId, root: NodeId) -> NodeId {
        let x = &self.nodes[n as usize];
        if x.first_child != NONE {
            return x.first_child;
        }
        self.next_skipping_children(n, root)
    }

    /// Like [`Dom::next_in`] but not into `n`'s children.
    pub fn next_skipping_children(&self, mut n: NodeId, root: NodeId) -> NodeId {
        loop {
            if n == root {
                return NONE;
            }
            let x = &self.nodes[n as usize];
            if x.next != NONE {
                return x.next;
            }
            n = x.parent;
            if n == NONE {
                return NONE;
            }
        }
    }

    /// The concatenated text of all text descendants.
    pub fn text_content(&self, n: NodeId) -> String {
        let mut s = String::new();
        let mut c = self.next_in(n, n);
        while c != NONE {
            if let NodeData::Text(t) = &self.nodes[c as usize].data {
                s.push_str(t);
            }
            c = self.next_in(c, n);
        }
        s
    }

    /// The `<html>` element (first element child of the document).
    pub fn document_element(&self) -> NodeId {
        self.children(0)
            .find(|&c| self.element(c).is_some())
            .unwrap_or(NONE)
    }

    /// First HTML element with tag `t` in tree order.
    pub fn find_tag(&self, t: Atom) -> NodeId {
        let mut c = 0;
        while c != NONE {
            if self.is_html(c, t) {
                return c;
            }
            c = self.next_in(c, 0);
        }
        NONE
    }

    pub fn body(&self) -> NodeId {
        let html = self.document_element();
        if html == NONE {
            return NONE;
        }
        self.children(html)
            .find(|&c| self.is_html(c, tag::BODY) || self.is_html(c, tag::FRAMESET))
            .unwrap_or(NONE)
    }

    /// Give back the slack of the parser's growing buffers (text nodes, attribute lists).
    pub fn shrink(&mut self) {
        self.nodes.shrink_to_fit();
        for n in self.nodes.iter_mut() {
            match &mut n.data {
                NodeData::Text(t) | NodeData::Comment(t) => t.shrink_to_fit(),
                NodeData::Element(e) => {
                    e.attrs.shrink_to_fit();
                    e.classes.shrink_to_fit();
                }
                _ => {}
            }
        }
    }

    pub fn approx_bytes(&self) -> usize {
        let mut b = self.nodes.capacity() * core::mem::size_of::<Node>();
        for n in &self.nodes {
            b += match &n.data {
                NodeData::Element(e) => {
                    e.attrs
                        .iter()
                        .map(|a| a.name.len() + a.value.len() + 32)
                        .sum::<usize>()
                        + e.classes.capacity() * 4
                }
                NodeData::Text(t) | NodeData::Comment(t) => t.capacity(),
                _ => 0,
            };
        }
        b + self.atoms.approx_bytes()
    }
}

pub struct Children<'a> {
    dom: &'a Dom,
    next: NodeId,
}

impl Iterator for Children<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        if self.next == NONE {
            return None;
        }
        let n = self.next;
        self.next = self.dom.nodes[n as usize].next;
        Some(n)
    }
}

/// HTML's "ASCII whitespace": tab, LF, FF, CR, space.
pub fn is_html_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' ')
}
