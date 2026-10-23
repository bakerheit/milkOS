//! The DOM: an arena of nodes linked as a tree (parent, first/last child, siblings), indexed by
//! [`NodeId`]. Node 0 is the Document.
//!
//! The tree builder uses the plain linking calls. Scripts change the tree through the same
//! calls once [`Dom::track`] is on: every change is then noted in [`Dom::changes`] (which
//! subtrees need restyling, whether forms, images or stylesheets need another look), so the
//! page restyles and relays out once per frame, not once per change. Nodes a script no longer
//! needs can be given back with [`Dom::free_subtree`]; their ids are reused.

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
    /// (name, public id, system id), boxed (there is at most one).
    Doctype(Box<[String; 3]>),
    /// Boxed: half the nodes are text, which shouldn't pay for an element's fields.
    Element(Box<Element>),
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

/// What changed since the page last caught up (see [`Dom::take_changes`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Elements whose subtrees need restyling (a changed node's parent, so sibling selectors
    /// are covered). Capped: past [`MAX_RESTYLE_ROOTS`] `restyle_all` is set instead.
    pub restyle: Vec<NodeId>,
    pub restyle_all: bool,
    /// Some child list changed.
    pub tree: bool,
    /// Text changed (layout, and maybe a `<style>`'s sheet or a `<title>`).
    pub text: bool,
    /// A `<style>` or `<link>` came or went or changed.
    pub sheets: bool,
    /// Form controls (or their attributes) may have changed.
    pub forms: bool,
    /// An image source may have changed.
    pub images: bool,
    /// An `<iframe>` came, went or changed (its `src`, `srcdoc`, `name`, ...).
    pub frames: bool,
    /// `<iframe>` elements given back with [`Dom::free_subtree`] (their ids will be reused).
    pub freed_frames: Vec<NodeId>,
    /// Anything at all (layout is needed).
    pub any: bool,
}

/// Past this many separate restyle roots in one batch, everything is restyled.
pub const MAX_RESTYLE_ROOTS: usize = 64;

pub struct Dom {
    pub nodes: Vec<Node>,
    pub atoms: Interner,
    pub quirks: QuirksMode,
    /// Note changes in [`Dom::changes`] (off while the parser builds the tree).
    pub track: bool,
    pub changes: Changes,
    /// Ids of freed nodes, for reuse.
    free: Vec<NodeId>,
    /// Bumped by every tracked change (tree, attributes, text): live collections cache by it.
    pub generation: u64,
    /// Bytes of text and attribute values made, roughly (freed nodes give theirs back): the
    /// bindings refuse to grow a page past [`MAX_DOM_BYTES`].
    pub bytes: usize,
    /// The checkboxes, radio buttons and options that are checked / selected now (what
    /// `:checked` matches), once the page keeps it; None: the `checked` / `selected`
    /// attributes say.
    pub checked: Option<alloc::collections::BTreeSet<NodeId>>,
}

/// The most text and attribute bytes scripts may grow a page to.
pub const MAX_DOM_BYTES: usize = 256 << 20;

fn data_bytes(d: &NodeData) -> usize {
    match d {
        NodeData::Text(t) | NodeData::Comment(t) => t.len(),
        NodeData::Element(e) => e.attrs.iter().map(|a| a.name.len() + a.value.len()).sum(),
        _ => 0,
    }
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
            track: false,
            changes: Changes::default(),
            free: Vec::new(),
            generation: 0,
            bytes: 0,
            checked: None,
        }
    }

    /// Nodes in use (not counting freed ones).
    pub fn live(&self) -> usize {
        self.nodes.len() - self.free.len()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    fn push(&mut self, data: NodeData) -> NodeId {
        self.bytes += data_bytes(&data);
        if let Some(id) = self.free.pop() {
            self.nodes[id as usize] = Node {
                parent: NONE,
                first_child: NONE,
                last_child: NONE,
                prev: NONE,
                next: NONE,
                data,
            };
            return id;
        }
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
        self.push(NodeData::Element(Box::new(Element {
            name,
            ns,
            attrs,
            id,
            classes,
        })))
    }

    pub fn create_text(&mut self, s: String) -> NodeId {
        self.push(NodeData::Text(s))
    }

    pub fn create_comment(&mut self, s: String) -> NodeId {
        self.push(NodeData::Comment(s))
    }

    pub fn create_doctype(&mut self, name: String, public_id: String, system_id: String) -> NodeId {
        self.push(NodeData::Doctype(Box::new([name, public_id, system_id])))
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
        if self.track {
            self.note_tree(p, n);
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
        if self.track {
            self.note_tree(parent, child);
        }
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
        if self.track {
            self.note_tree(parent, child);
        }
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

    // ---- changes (scripts) -----------------------------------------------------------------

    /// Hand over what changed since the last call, and start afresh.
    pub fn take_changes(&mut self) -> Changes {
        core::mem::take(&mut self.changes)
    }

    fn note_restyle(&mut self, n: NodeId) {
        self.generation += 1;
        let c = &mut self.changes;
        c.any = true;
        if c.restyle_all {
            return;
        }
        if !c.restyle.contains(&n) {
            if c.restyle.len() >= MAX_RESTYLE_ROOTS {
                c.restyle_all = true;
                c.restyle.clear();
            } else {
                c.restyle.push(n);
            }
        }
    }

    /// `child` is being linked into or out of `parent`.
    fn note_tree(&mut self, parent: NodeId, child: NodeId) {
        self.generation += 1;
        self.changes.tree = true;
        self.changes.forms = true;
        self.changes.images = true;
        if self.subtree_has_iframe(child) {
            self.changes.frames = true;
        }
        // The parent's parent: :empty, :nth-child and sibling combinators around the parent.
        let gp = self.nodes[parent as usize].parent;
        self.note_restyle(if gp == NONE || gp == 0 { parent } else { gp });
        if self.is_html(parent, tag::STYLE) || self.subtree_has_sheet(child) {
            self.changes.sheets = true;
        }
    }

    /// Is there an `<iframe>` in the subtree of `n` (bounded, so a hostile tree costs little)?
    fn subtree_has_iframe(&self, n: NodeId) -> bool {
        let mut c = n;
        let mut k = 0;
        while c != NONE {
            if self.is_html(c, tag::IFRAME) {
                return true;
            }
            k += 1;
            if k > 100_000 {
                return true;
            }
            c = self.next_in(c, n);
        }
        false
    }

    fn subtree_has_sheet(&self, n: NodeId) -> bool {
        let mut c = n;
        let mut k = 0;
        while c != NONE {
            if self.is_html(c, tag::STYLE) || self.is_html(c, tag::LINK) {
                return true;
            }
            k += 1;
            if k > 100_000 {
                return true;
            }
            c = self.next_in(c, n);
        }
        false
    }

    /// `n`'s state (checked, selected) changed: restyle around it.
    pub fn note_state(&mut self, n: NodeId) {
        let p = self.nodes[n as usize].parent;
        self.note_restyle(if p == NONE || p == 0 { n } else { p });
    }

    /// Is `n` in the document (an ancestor chain up to node 0)?
    pub fn is_connected(&self, mut n: NodeId) -> bool {
        let mut k = 0;
        while n != NONE {
            if n == 0 {
                return true;
            }
            n = self.nodes[n as usize].parent;
            k += 1;
            if k > 1 << 20 {
                return false;
            }
        }
        false
    }

    /// Is `n` `a` or inside it?
    pub fn contains(&self, a: NodeId, mut n: NodeId) -> bool {
        while n != NONE {
            if n == a {
                return true;
            }
            n = self.nodes[n as usize].parent;
        }
        false
    }

    /// Set (or add) an attribute. `name` as given (callers lower-case HTML names). Keeps the
    /// element's interned id and classes in step.
    pub fn set_attr(&mut self, n: NodeId, name: &str, value: &str) {
        let Some(NodeData::Element(e)) = self.nodes.get_mut(n as usize).map(|x| &mut x.data) else {
            return;
        };
        match e.attrs.iter_mut().find(|a| &*a.name == name) {
            Some(a) => {
                if &*a.value == value {
                    return;
                }
                self.bytes = (self.bytes + value.len()).saturating_sub(a.value.len());
                a.value = value.into();
            }
            None => {
                self.bytes += name.len() + value.len();
                e.attrs.push(Attr {
                    name: name.into(),
                    value: value.into(),
                })
            }
        }
        self.attr_changed(n, name);
    }

    pub fn remove_attr(&mut self, n: NodeId, name: &str) {
        let Some(e) = self.element_mut(n) else {
            return;
        };
        let before = e.attrs.len();
        e.attrs.retain(|a| &*a.name != name);
        if e.attrs.len() != before {
            self.attr_changed(n, name);
        }
    }

    fn attr_changed(&mut self, n: NodeId, name: &str) {
        self.generation += 1;
        if name == "id" || name == "class" {
            let (id, cls) = {
                let e = self.element(n).unwrap();
                (
                    e.attr("id").map(String::from),
                    e.attr("class").map(String::from),
                )
            };
            let id = id.filter(|v| !v.is_empty()).map(|v| self.atoms.intern(&v));
            let mut classes = Vec::new();
            if let Some(c) = cls {
                for w in c.split(is_html_space) {
                    if !w.is_empty() {
                        let a = self.atoms.intern(w);
                        if !classes.contains(&a) {
                            classes.push(a);
                        }
                    }
                }
            }
            let e = self.element_mut(n).unwrap();
            e.id = id;
            e.classes = classes;
        }
        if !self.track {
            return;
        }
        let p = self.nodes[n as usize].parent;
        self.note_restyle(if p == NONE || p == 0 { n } else { p });
        let t = self.html_tag(n);
        if matches!(
            t,
            tag::INPUT
                | tag::SELECT
                | tag::OPTION
                | tag::OPTGROUP
                | tag::TEXTAREA
                | tag::BUTTON
                | tag::FORM
                | tag::FIELDSET
                | tag::LABEL
        ) || name == "id"
            || name == "form"
        {
            self.changes.forms = true;
        }
        if matches!(t, tag::IMG | tag::INPUT) && matches!(name, "src" | "srcset" | "type") {
            self.changes.images = true;
        }
        if t == tag::IFRAME
            && matches!(
                name,
                "src"
                    | "srcdoc"
                    | "name"
                    | "sandbox"
                    | "loading"
                    | "allow"
                    | "allowfullscreen"
                    | "referrerpolicy"
                    | "scrolling"
                    | "csp"
            )
        {
            self.changes.frames = true;
        }
        if matches!(t, tag::LINK | tag::STYLE) {
            self.changes.sheets = true;
        }
    }

    /// Replace a text or comment node's data.
    pub fn set_data(&mut self, n: NodeId, s: &str) {
        match self.nodes.get_mut(n as usize).map(|x| &mut x.data) {
            Some(NodeData::Text(t)) | Some(NodeData::Comment(t)) => {
                if t == s {
                    return;
                }
                self.bytes = (self.bytes + s.len()).saturating_sub(t.len());
                t.clear();
                t.push_str(s);
            }
            _ => return,
        }
        self.generation += 1;
        if self.track && matches!(self.nodes[n as usize].data, NodeData::Text(_)) {
            self.changes.text = true;
            self.changes.any = true;
            let p = self.nodes[n as usize].parent;
            match self.html_tag(p) {
                tag::STYLE => self.changes.sheets = true,
                tag::TEXTAREA | tag::OPTION | tag::BUTTON => self.changes.forms = true,
                _ if p != NONE && self.is_html(self.nodes[p as usize].parent, tag::SELECT) => {
                    self.changes.forms = true
                }
                _ => {}
            }
            // Text can be what :empty looks at.
            if p != NONE {
                self.note_restyle(p);
            }
        }
    }

    /// Remove all children of `n` and put `s` there as one text node (none if empty): the
    /// `textContent` setter. The old children are detached, not freed.
    pub fn set_text_content(&mut self, n: NodeId, s: &str) -> Vec<NodeId> {
        let old: Vec<NodeId> = self.children(n).collect();
        for &c in &old {
            self.detach(c);
        }
        if !s.is_empty() {
            let t = self.create_text(String::from(s));
            self.append(n, t);
        }
        old
    }

    /// A copy of `n` (and with `deep`, of its descendants), detached. At most `budget` nodes are
    /// made (a deep copy stops there).
    pub fn clone_node(&mut self, n: NodeId, deep: bool, budget: usize) -> NodeId {
        let mut made = 0usize;
        let root = self.clone_one(n);
        made += 1;
        if !deep {
            return root;
        }
        // Iterative: (source, copy) pairs whose children still need copying.
        let mut stack = alloc::vec![(n, root)];
        while let Some((src, dst)) = stack.pop() {
            let kids: Vec<NodeId> = self.children(src).collect();
            for k in kids {
                if made >= budget {
                    return root;
                }
                let c = self.clone_one(k);
                made += 1;
                let was = self.track;
                self.track = false;
                self.append(dst, c);
                self.track = was;
                stack.push((k, c));
            }
        }
        root
    }

    fn clone_one(&mut self, n: NodeId) -> NodeId {
        let data = match &self.nodes[n as usize].data {
            NodeData::Document => NodeData::Comment(String::new()),
            d => d.clone(),
        };
        self.push(data)
    }

    /// Give back `n` and everything under it (detaching it first). The caller makes sure
    /// nothing refers to these ids any more (no script object, no saved focus).
    pub fn free_subtree(&mut self, n: NodeId) {
        if n == 0 || n as usize >= self.nodes.len() {
            return;
        }
        self.detach(n);
        let mut all = Vec::new();
        let mut c = n;
        while c != NONE {
            all.push(c);
            c = self.next_in(c, n);
        }
        for c in all {
            if self.track && self.is_html(c, tag::IFRAME) {
                self.changes.freed_frames.push(c);
                self.changes.frames = true;
            }
            let b = data_bytes(&self.nodes[c as usize].data);
            self.bytes = self.bytes.saturating_sub(b);
            let x = &mut self.nodes[c as usize];
            *x = Node {
                parent: NONE,
                first_child: NONE,
                last_child: NONE,
                prev: NONE,
                next: NONE,
                data: NodeData::Comment(String::new()),
            };
            self.free.push(c);
        }
    }

    /// Copy the children of `from_root` in another DOM (a parsed fragment) into this one,
    /// under `parent` before `before` (NONE: at the end), re-interning names. At most
    /// `budget` nodes are copied. Returns the top-level nodes added.
    pub fn import(
        &mut self,
        from: &Dom,
        from_root: NodeId,
        parent: NodeId,
        before: NodeId,
        budget: usize,
    ) -> Vec<NodeId> {
        let mut made = 0usize;
        let mut top = Vec::new();
        let was = self.track;
        self.track = false;
        // Build each top-level node's subtree detached, then link the tops in (tracked).
        // (source child, destination parent; NONE for a top-level node)
        let mut stack: Vec<(NodeId, NodeId)> = Vec::new();
        let kids: Vec<NodeId> = from.children(from_root).collect();
        for &k in kids.iter().rev() {
            stack.push((k, NONE));
        }
        while let Some((src, dst)) = stack.pop() {
            if made >= budget {
                break;
            }
            let n = match &from.nodes[src as usize].data {
                NodeData::Element(e) => {
                    let name = self.atoms.intern(from.atoms.name(e.name));
                    self.create_element(name, e.ns, e.attrs.clone())
                }
                NodeData::Text(t) => self.create_text(t.clone()),
                NodeData::Comment(t) => self.create_comment(t.clone()),
                NodeData::Doctype(_) | NodeData::Document => continue,
            };
            made += 1;
            if dst == NONE {
                top.push(n);
            } else {
                self.append(dst, n);
            }
            let kids: Vec<NodeId> = from.children(src).collect();
            for &k in kids.iter().rev() {
                stack.push((k, n));
            }
        }
        self.track = was;
        for &n in &top {
            self.insert_before(parent, n, before);
        }
        top
    }

    /// The position of `n` among its parent's children (0-based), NONE-parent: 0.
    pub fn index_in_parent(&self, n: NodeId) -> usize {
        let mut i = 0;
        let mut c = self.nodes[n as usize].prev;
        while c != NONE {
            i += 1;
            c = self.nodes[c as usize].prev;
        }
        i
    }

    /// The topmost ancestor of `n` (n itself if detached and parentless).
    pub fn root_of(&self, mut n: NodeId) -> NodeId {
        while self.nodes[n as usize].parent != NONE {
            n = self.nodes[n as usize].parent;
        }
        n
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
