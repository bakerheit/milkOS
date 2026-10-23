//! The tree of frames of a page: which `<iframe>` elements each document has (the *slots*), the
//! documents the host attached to them, and everything that crosses a frame boundary:
//! layout of the children into the parent's display list, hit testing, scrolling, focus, find.
//!
//! The top [`crate::Document`] owns a [`Frames`] (made at the first `<iframe>` it sees). The
//! documents of the frames are boxed inside its slots; they are ordinary documents that know
//! nothing of one another (a child's own `<iframe>`s are slots of the top document's table, with
//! the child as their parent). Operations that cross frames take a document out of its slot
//! while they work on it, so the table and the document can be borrowed together.

use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use crate::api::*;
use crate::atoms::tag;
use crate::dom::{NONE, NodeId};
use crate::page::Page;

/// Where a document sits in the tree (kept in its [`Page`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrameCtx {
    pub id: FrameId,
    pub parent: FrameId,
    pub depth: u32,
}

impl FrameCtx {
    pub const TOP: FrameCtx = FrameCtx {
        id: FrameId::TOP,
        parent: FrameId::TOP,
        depth: 0,
    };
}

/// An `<iframe>` element's attributes, as the slot keeps them.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct SlotAttrs {
    pub src: Option<String>,
    pub srcdoc: Option<String>,
    pub name: String,
    pub sandbox: Option<Sandbox>,
    pub lazy: bool,
    pub allow: String,
    pub allow_fullscreen: bool,
    pub referrer_policy: String,
    pub no_scroll: bool,
}

pub(crate) struct Slot {
    pub id: FrameId,
    pub parent: FrameId,
    pub node: NodeId,
    pub depth: u32,
    /// Position among the parent's iframes (tree order) at the last sync.
    pub order: u32,
    pub generation: u32,
    pub attrs: SlotAttrs,
    pub rect: Rect,
    pub rendered: bool,
    pub blocked: Option<FrameBlock>,
    pub attached: Option<u32>,
    pub scroll: (i32, i32),
    pub content: (i32, i32),
    pub doc: Option<Box<crate::Document>>,
}

impl Slot {
    pub fn info(&self) -> FrameSlot {
        FrameSlot {
            id: self.id,
            parent: self.parent,
            depth: self.depth,
            generation: self.generation,
            src: self.attrs.src.clone(),
            srcdoc: self.attrs.srcdoc.clone(),
            name: self.attrs.name.clone(),
            sandbox: self.attrs.sandbox,
            lazy: self.attrs.lazy,
            allow: self.attrs.allow.clone(),
            allow_fullscreen: self.attrs.allow_fullscreen,
            referrer_policy: self.attrs.referrer_policy.clone(),
            scrolling: if self.attrs.no_scroll {
                FrameScrolling::No
            } else {
                FrameScrolling::Auto
            },
            rect: self.rect,
            rendered: self.rendered,
            blocked: self.blocked,
            attached: self.attached,
            scroll: self.scroll,
            content: self.content,
        }
    }
}

/// Most iframe elements looked at in one document (the rest get no slot).
const MAX_SCAN: usize = 4096;
/// Console lines the table keeps for the host between [`Frames::take_notes`] calls.
const MAX_NOTES: usize = 16;

pub(crate) struct Frames {
    pub slots: Vec<Slot>,
    next_id: u32,
    /// The frame with the keyboard.
    pub focused: FrameId,
    /// Frames whose slot went away since the host last asked.
    closed: Vec<FrameId>,
    /// "Too many frames" was said already.
    overflowed: bool,
    notes: Vec<ConsoleLine>,
    /// Bumped by anything that changes what the frames add to the parent's display list
    /// beyond their own layout (an attach, a detach, a scroll).
    pub version: u64,
}

impl Frames {
    pub fn new() -> Frames {
        Frames {
            slots: Vec::new(),
            next_id: 1,
            focused: FrameId::TOP,
            closed: Vec::new(),
            overflowed: false,
            notes: Vec::new(),
            version: 0,
        }
    }

    pub fn index(&self, id: FrameId) -> Option<usize> {
        self.slots.iter().position(|s| s.id == id)
    }

    pub fn slot(&self, id: FrameId) -> Option<&Slot> {
        self.slots.iter().find(|s| s.id == id)
    }

    pub fn slot_mut(&mut self, id: FrameId) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.id == id)
    }

    pub fn by_node(&self, parent: FrameId, node: NodeId) -> Option<&Slot> {
        self.slots
            .iter()
            .find(|s| s.parent == parent && s.node == node)
    }

    fn note(&mut self, level: ConsoleLevel, text: String) {
        if self.notes.len() < MAX_NOTES {
            self.notes.push(ConsoleLine { level, text });
        }
    }

    pub fn take_notes(&mut self) -> Vec<ConsoleLine> {
        core::mem::take(&mut self.notes)
    }

    pub fn take_closed(&mut self) -> Vec<FrameId> {
        core::mem::take(&mut self.closed)
    }

    /// The ids of the children of `of`, in tree order (not recursive).
    pub fn children(&self, of: FrameId) -> Vec<FrameId> {
        let mut v: Vec<&Slot> = self.slots.iter().filter(|s| s.parent == of).collect();
        v.sort_by_key(|s| s.order);
        v.iter().map(|s| s.id).collect()
    }

    /// Every slot in tree order, parents before their children (a pre-order walk).
    pub fn walk(&self) -> Vec<FrameId> {
        let mut out = Vec::new();
        let mut stack: Vec<FrameId> = Vec::new();
        for c in self.children(FrameId::TOP).into_iter().rev() {
            stack.push(c);
        }
        while let Some(id) = stack.pop() {
            out.push(id);
            if out.len() > self.slots.len() {
                break;
            }
            for c in self.children(id).into_iter().rev() {
                stack.push(c);
            }
        }
        out
    }

    /// Give up slot `id` and everything below it (documents dropped).
    pub fn remove_slot(&mut self, id: FrameId) {
        let mut gone = alloc::vec![id];
        let mut k = 0;
        while k < gone.len() {
            let p = gone[k];
            for s in &self.slots {
                if s.parent == p && !gone.contains(&s.id) {
                    gone.push(s.id);
                }
            }
            k += 1;
        }
        self.slots.retain(|s| !gone.contains(&s.id));
        if gone.contains(&self.focused) {
            self.focused = FrameId::TOP;
        }
        self.closed.extend(gone);
        self.version += 1;
    }

    /// Drop the slots below `id` (not `id`'s own): its document was replaced.
    pub fn remove_below(&mut self, id: FrameId) {
        for c in self.children(id) {
            self.remove_slot(c);
        }
    }

    /// Bring the slots of the document `page` (frame `ctx`) in step with its `<iframe>`
    /// elements: new elements get slots (within the caps), gone ones lose theirs, changed
    /// attributes are noted (a new `src` or `srcdoc` bumps the generation).
    pub fn sync_doc(
        &mut self,
        ctx: FrameCtx,
        page: &mut Page,
        max_frames: usize,
        max_depth: usize,
    ) {
        let nodes = page.iframe_nodes();
        let freed = page.take_freed_iframes();
        let live: BTreeSet<NodeId> = nodes.iter().copied().collect();
        let freed: BTreeSet<NodeId> = freed.into_iter().collect();
        let gone: Vec<FrameId> = self
            .slots
            .iter()
            .filter(|s| s.parent == ctx.id && (!live.contains(&s.node) || freed.contains(&s.node)))
            .map(|s| s.id)
            .collect();
        for g in gone {
            self.remove_slot(g);
        }
        let depth = ctx.depth + 1;
        for (order, &node) in nodes.iter().enumerate() {
            let attrs = page.slot_attrs(node);
            if let Some(i) = self
                .slots
                .iter()
                .position(|s| s.parent == ctx.id && s.node == node)
            {
                let s = &mut self.slots[i];
                s.order = order as u32;
                if s.attrs != attrs {
                    if s.attrs.src != attrs.src || s.attrs.srcdoc != attrs.srcdoc {
                        s.generation = s.generation.wrapping_add(1);
                    }
                    let renamed = s.attrs.name != attrs.name;
                    s.attrs = attrs;
                    if renamed && let Some(d) = s.doc.as_mut() {
                        d.page.window_name = s.attrs.name.clone();
                    }
                    self.version += 1;
                }
                continue;
            }
            if self.slots.len() >= max_frames || self.next_id == u32::MAX {
                if !self.overflowed {
                    self.overflowed = true;
                    self.note(
                        ConsoleLevel::Warn,
                        alloc::format!(
                            "Too many frames: only {max_frames} <iframe>s are used on a page."
                        ),
                    );
                }
                continue;
            }
            let id = FrameId(self.next_id);
            self.next_id += 1;
            let too_deep = depth as usize > max_depth;
            if too_deep {
                self.note(
                    ConsoleLevel::Warn,
                    alloc::format!("Frames nested more than {max_depth} deep are not loaded."),
                );
            }
            self.slots.push(Slot {
                id,
                parent: ctx.id,
                node,
                depth,
                order: order as u32,
                generation: 0,
                attrs,
                rect: Rect::default(),
                rendered: false,
                blocked: if too_deep {
                    Some(FrameBlock::TooDeep)
                } else {
                    None
                },
                attached: None,
                scroll: (0, 0),
                content: (0, 0),
                doc: None,
            });
            self.version += 1;
        }
        page.set_frame_nodes(
            self.slots
                .iter()
                .filter(|s| s.parent == ctx.id)
                .map(|s| (s.node, s.id))
                .collect(),
        );
        page.frames_dirty = false;
    }
}

impl Page {
    /// The `<iframe>` elements of the document in tree order (not inside `<template>`).
    pub(crate) fn iframe_nodes(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut n = 0;
        while n != NONE {
            let Some(e) = self.dom.element(n) else {
                n = self.dom.next_in(n, 0);
                continue;
            };
            if e.is(tag::TEMPLATE) || e.ns != crate::dom::Ns::Html {
                n = self.dom.next_skipping_children(n, 0);
                continue;
            }
            if e.is(tag::IFRAME) {
                out.push(n);
                if out.len() >= MAX_SCAN {
                    break;
                }
                // Its content is text, not elements.
                n = self.dom.next_skipping_children(n, 0);
                continue;
            }
            n = self.dom.next_in(n, 0);
        }
        out
    }

    /// An `<iframe>` element's attributes as a slot keeps them.
    pub(crate) fn slot_attrs(&self, node: NodeId) -> SlotAttrs {
        let Some(e) = self.dom.element(node) else {
            return SlotAttrs::default();
        };
        let src = e
            .attr("src")
            .map(|s| s.trim_matches(|c: char| c.is_ascii_whitespace()))
            .filter(|s| !s.is_empty())
            .map(|s| self.resolve(s));
        SlotAttrs {
            src,
            srcdoc: e.attr("srcdoc").map(String::from),
            name: e.attr("name").map(String::from).unwrap_or_default(),
            sandbox: e.attr("sandbox").map(Sandbox::parse),
            lazy: e
                .attr("loading")
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("lazy")),
            allow: e.attr("allow").map(String::from).unwrap_or_default(),
            allow_fullscreen: e.has_attr("allowfullscreen"),
            referrer_policy: e
                .attr("referrerpolicy")
                .map(|v| v.trim().to_ascii_lowercase())
                .unwrap_or_default(),
            no_scroll: e.attr("scrolling").is_some_and(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "no" | "false" | "off"
                )
            }),
        }
    }
}

impl Page {
    /// The `<iframe>` elements may have changed since the table last looked.
    pub(crate) fn frames_pending(&self) -> bool {
        self.frames_dirty
            || self.dom.changes.frames
            || !self.dom.changes.freed_frames.is_empty()
            || !self.freed_iframes_is_empty()
    }
}

/// Put `doc` into slot `id` (see [`crate::Document::attach_frame`]).
pub(crate) fn attach(
    frames: &mut Frames,
    id: FrameId,
    mut doc: crate::Document,
    gen_: Option<u32>,
) -> bool {
    let Some(i) = frames.index(id) else {
        return false;
    };
    {
        let s = &frames.slots[i];
        if s.blocked.is_some() || gen_.is_some_and(|g| g != s.generation) {
            return false;
        }
    }
    // The new document brings its own frames; the old one's go.
    frames.remove_below(id);
    let s = &mut frames.slots[i];
    let name = match s.doc.as_ref() {
        Some(old) => old.page.window_name.clone(),
        None => s.attrs.name.clone(),
    };
    doc.frames = None;
    doc.attached = true;
    doc.page.frame = FrameCtx {
        id,
        parent: s.parent,
        depth: s.depth,
    };
    doc.page.window_name = name;
    s.doc = Some(alloc::boxed::Box::new(doc));
    s.attached = Some(s.generation);
    s.scroll = (0, 0);
    s.content = (0, 0);
    frames.version += 1;
    true
}

pub(crate) fn detach(frames: &mut Frames, id: FrameId) -> Option<crate::Document> {
    frames.index(id)?;
    frames.remove_below(id);
    let s = frames.slot_mut(id)?;
    let doc = s.doc.take()?;
    s.attached = None;
    s.scroll = (0, 0);
    s.content = (0, 0);
    if frames.focused == id {
        frames.focused = FrameId::TOP;
    }
    frames.version += 1;
    let mut doc = *doc;
    doc.attached = false;
    doc.page.frame = FrameCtx::TOP;
    Some(doc)
}

pub(crate) fn viewport_of(top: &crate::Document, id: FrameId) -> Option<Viewport> {
    let s = top.frames.as_ref()?.slot(id)?;
    Some(Viewport {
        width: if s.rect.w > 0 { s.rect.w } else { 300 },
        height: if s.rect.h > 0 { s.rect.h } else { 150 },
        zoom: 100,
        dark: false,
    })
}

pub(crate) fn set_scroll(top: &mut crate::Document, id: FrameId, x: i32, y: i32) -> (i32, i32) {
    let Some(s) = top.frames.as_mut().and_then(|f| f.slot_mut(id)) else {
        return (0, 0);
    };
    s.scroll = (x.max(0), y.max(0));
    s.scroll
}

pub(crate) fn scroll_at(
    _top: &mut crate::Document,
    _x: i32,
    _y: i32,
    _scroll: (i32, i32),
    dx: i32,
    dy: i32,
) -> (i32, i32) {
    (dx, dy)
}

pub(crate) fn reveal(_top: &mut crate::Document, _id: FrameId, rect: Rect) -> Rect {
    rect
}

pub(crate) fn fragment(top: &mut crate::Document, id: FrameId, frag: &str) -> bool {
    let Some(y) = top.frame_doc(id).and_then(|d| d.page.fragment_y(frag)) else {
        return false;
    };
    set_scroll(top, id, 0, y);
    true
}

pub(crate) fn focus_order(top: &crate::Document) -> Vec<(FrameId, ControlId)> {
    let mut v = Vec::new();
    for (i, c) in top.page.controls().iter().enumerate() {
        if !c.rect.is_empty() && !c.disabled {
            v.push((FrameId::TOP, ControlId(i as u32)));
        }
    }
    v
}
