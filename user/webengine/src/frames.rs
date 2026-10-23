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
    /// `visibility: visible`.
    pub visible: bool,
    pub blocked: Option<FrameBlock>,
    pub attached: Option<u32>,
    pub scroll: (i32, i32),
    pub content: (i32, i32),
    pub doc: Option<Box<crate::Document>>,
    /// The viewport the document was last laid out in.
    pub vp: Option<Viewport>,
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
    /// The caps ([`Limits::max_frames`], [`Limits::max_frame_depth`] of the top document).
    pub max_frames: usize,
    pub max_depth: usize,
}

impl Frames {
    pub fn new() -> Frames {
        Frames {
            max_frames: 32,
            max_depth: 5,
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
    pub fn sync_doc(&mut self, ctx: FrameCtx, page: &mut Page) {
        let (max_frames, max_depth) = (self.max_frames, self.max_depth);
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
                visible: true,
                blocked: if too_deep {
                    Some(FrameBlock::TooDeep)
                } else {
                    None
                },
                attached: None,
                scroll: (0, 0),
                content: (0, 0),
                doc: None,
                vp: None,
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
    doc.merged = None;
    doc.attached = true;
    doc.page.frame = FrameCtx {
        id,
        parent: s.parent,
        depth: s.depth,
    };
    doc.page.window_name = name;
    s.doc = Some(Box::new(doc));
    s.attached = Some(s.generation);
    s.scroll = (0, 0);
    s.content = (0, 0);
    s.vp = None;
    if frames.focused == id {
        frames.focused = FrameId::TOP;
    }
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
    s.vp = None;
    if frames.focused == id {
        frames.focused = FrameId::TOP;
    }
    frames.version += 1;
    let mut doc = *doc;
    doc.attached = false;
    doc.merged = None;
    doc.page.frame = FrameCtx::TOP;
    Some(doc)
}

pub(crate) fn viewport_of(top: &crate::Document, id: FrameId) -> Option<Viewport> {
    let s = top.frames.as_ref()?.slot(id)?;
    Some(s.vp.unwrap_or(Viewport::new(300, 150)))
}

// ---- layout and the merged display list ---------------------------------------------------

/// What a document's display list is with its frames in it, and what it was made from.
pub(crate) struct Merged {
    pub dl: DisplayList,
    pub sig: Sig,
    pub gen_: u64,
}

/// (list number, frame, scroll, content box) of each frame that is merged in, after the
/// page's own list number.
pub(crate) type Sig = Vec<(u64, u32, (i32, i32), Rect)>;

/// A number nobody has had: display lists are numbered with these, so two that differ never
/// share one (a frame's document replaced by another, say).
pub(crate) fn next_gen() -> u64 {
    use core::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl crate::Document {
    /// A number that changes whenever [`crate::Document::display_list`] does.
    pub(crate) fn list_gen(&self) -> u64 {
        self.merged
            .as_ref()
            .map_or(self.page.dl_gen(), |m| m.gen_)
    }
}

/// What the frames below `page` (frame `parent`) look like, as far as the merged list
/// depends on it.
fn signature(frames: &Frames, parent: FrameId, page: &Page) -> Sig {
    let mut sig = Vec::new();
    sig.push((page.dl_gen(), u32::MAX, (0, 0), Rect::default()));
    for mk in page.frame_marks() {
        let Some(s) = frames.by_node(parent, mk.node) else {
            continue;
        };
        let Some(d) = s.doc.as_ref() else { continue };
        sig.push((d.list_gen(), s.id.0, s.scroll, mk.rect));
    }
    sig
}

fn shift_rect(r: Rect, dx: i32, dy: i32) -> Rect {
    Rect::new(r.x.saturating_add(dx), r.y.saturating_add(dy), r.w, r.h)
}

fn shift_item(item: &Item, dx: i32, dy: i32) -> Item {
    match *item {
        Item::Rect { rect, color } => Item::Rect {
            rect: shift_rect(rect, dx, dy),
            color,
        },
        Item::Border {
            rect,
            widths,
            colors,
            styles,
            radius,
        } => Item::Border {
            rect: shift_rect(rect, dx, dy),
            widths,
            colors,
            styles,
            radius,
        },
        Item::RoundRect {
            rect,
            color,
            radius,
        } => Item::RoundRect {
            rect: shift_rect(rect, dx, dy),
            color,
            radius,
        },
        Item::Text(mut t) => {
            t.x = t.x.saturating_add(dx);
            t.baseline = t.baseline.saturating_add(dy);
            Item::Text(t)
        }
        Item::Image { rect, image } => Item::Image {
            rect: shift_rect(rect, dx, dy),
            image,
        },
        Item::Tiled {
            area,
            tile,
            image,
            repeat_x,
            repeat_y,
        } => Item::Tiled {
            area: shift_rect(area, dx, dy),
            tile: shift_rect(tile, dx, dy),
            image,
            repeat_x,
            repeat_y,
        },
        Item::Marker { rect, kind, color } => Item::Marker {
            rect: shift_rect(rect, dx, dy),
            kind,
            color,
        },
        Item::Control { rect, control } => Item::Control {
            rect: shift_rect(rect, dx, dy),
            control,
        },
    }
}

/// The display list being built: the parent's, with the frames' content spliced in.
struct Builder {
    entries: Vec<Entry>,
    fixed: Vec<Entry>,
    clips: Vec<Rect>,
    text: String,
}

impl Builder {
    fn clip(&mut self, r: Rect, fallback: u16) -> u16 {
        if self.clips.len() >= u16::MAX as usize - 1 {
            return fallback;
        }
        self.clips.push(r);
        (self.clips.len() - 1) as u16
    }

    /// Frame `id`'s content in its slot (`mk` is the element's mark, `scroll` the frame's).
    fn emit(
        &mut self,
        id: FrameId,
        doc: &crate::Document,
        mk: &crate::paint::FrameMark,
        scroll: (i32, i32),
    ) {
        let cr = mk.rect;
        let child = doc.display_list();
        let base = self.clips.get(mk.clip as usize).copied().unwrap_or(cr).intersect(&cr);
        let base_idx = self.clip(base, mk.clip);
        let target_fixed = mk.fixed;
        // The canvas: a frame is never see-through.
        let canvas = Entry {
            item: Item::Rect {
                rect: cr,
                color: child.background | 0xFF00_0000,
            },
            clip: base_idx,
            frame: id,
        };
        if target_fixed {
            self.fixed.push(canvas);
        } else {
            self.entries.push(canvas);
        }
        let own_frame = |f: FrameId| if f.is_top() { id } else { f };
        // Scrolling content, then the child's own position: fixed boxes.
        for pass in 0..2 {
            let (dx, dy) = if pass == 0 {
                (cr.x.saturating_sub(scroll.0), cr.y.saturating_sub(scroll.1))
            } else {
                (cr.x, cr.y)
            };
            let list: Vec<&Entry> = if pass == 0 {
                child.visible(Rect::new(scroll.0, scroll.1, cr.w, cr.h))
            } else {
                child.fixed.iter().collect()
            };
            let mut map: Vec<u16> = alloc::vec![u16::MAX; child.clips.len()];
            for e in list {
                let ci = e.clip as usize;
                let clip = match map.get(ci).copied() {
                    Some(v) if v != u16::MAX => v,
                    _ => {
                        let c = child.clips.get(ci).copied().unwrap_or(cr);
                        let c = shift_rect(c, dx, dy).intersect(&base);
                        let v = self.clip(c, base_idx);
                        if let Some(m) = map.get_mut(ci) {
                            *m = v;
                        }
                        v
                    }
                };
                let mut item = shift_item(&e.item, dx, dy);
                if let Item::Text(t) = &mut item {
                    let s = child.run_text(t);
                    t.start = self.text.len() as u32;
                    t.len = s.len() as u32;
                    self.text.push_str(s);
                }
                let out = Entry {
                    item,
                    clip,
                    frame: own_frame(e.frame),
                };
                if target_fixed {
                    self.fixed.push(out);
                } else {
                    self.entries.push(out);
                }
            }
        }
    }
}

/// The page's own display list with every frame's content spliced in at its `<iframe>`.
fn merge(frames: &Frames, parent: FrameId, page: &Page) -> Option<DisplayList> {
    let own = page.display_list();
    let mut todo: Vec<(&crate::paint::FrameMark, &Slot)> = Vec::new();
    for mk in page.frame_marks() {
        if !mk.visible || mk.rect.is_empty() {
            continue;
        }
        if let Some(s) = frames.by_node(parent, mk.node)
            && s.doc.is_some()
        {
            todo.push((mk, s));
        }
    }
    if todo.is_empty() {
        return None;
    }
    let mut b = Builder {
        entries: Vec::with_capacity(own.entries.len() + 64),
        fixed: Vec::with_capacity(own.fixed.len()),
        clips: own.clips.clone(),
        text: own.text.clone(),
    };
    // Marks go in at their index; several at one index in the order they were painted.
    let mut by_index: Vec<usize> = (0..todo.len()).collect();
    by_index.sort_by_key(|&k| (todo[k].0.fixed, todo[k].0.index, k));
    let mut next = 0;
    for i in 0..=own.entries.len() {
        while next < by_index.len() {
            let (mk, s) = todo[by_index[next]];
            if mk.fixed || mk.index > i {
                break;
            }
            if let Some(d) = s.doc.as_deref() {
                b.emit(s.id, d, mk, s.scroll);
            }
            next += 1;
        }
        if let Some(e) = own.entries.get(i) {
            b.entries.push(*e);
        }
    }
    let mut next2 = next;
    for i in 0..=own.fixed.len() {
        while next2 < by_index.len() {
            let (mk, s) = todo[by_index[next2]];
            if mk.index > i {
                break;
            }
            if let Some(d) = s.doc.as_deref() {
                b.emit(s.id, d, mk, s.scroll);
            }
            next2 += 1;
        }
        if let Some(e) = own.fixed.get(i) {
            b.fixed.push(*e);
        }
    }
    let mut dl = DisplayList {
        background: own.background,
        entries: b.entries,
        fixed: b.fixed,
        clips: b.clips,
        text: b.text,
        width: own.width,
        height: own.height,
        bands: Vec::new(),
    };
    crate::paint::build_bands(&mut dl);
    Some(dl)
}

/// Lay out the frames below `page` (frame `parent`, already laid out in `vp`) and give each
/// slot its box. Recurses into the frames' own frames.
fn layout_level(frames: &mut Frames, parent: FrameId, page: &Page, vp: &Viewport, m: &dyn TextMeasurer) {
    let marks: Vec<crate::paint::FrameMark> = page.frame_marks().to_vec();
    let mut seen: Vec<usize> = Vec::new();
    for mk in &marks {
        let Some(si) = frames
            .slots
            .iter()
            .position(|s| s.parent == parent && s.node == mk.node)
        else {
            continue;
        };
        if seen.contains(&si) {
            continue;
        }
        seen.push(si);
        let s = &mut frames.slots[si];
        if s.rect != mk.rect || !s.rendered {
            s.rect = mk.rect;
            s.rendered = true;
        }
        s.visible = mk.visible;
        if s.rect.is_empty() || s.doc.is_none() {
            continue;
        }
        let child_vp = Viewport {
            width: s.rect.w,
            height: s.rect.h,
            zoom: vp.zoom,
            dark: vp.dark,
        };
        let id = s.id;
        let mut doc = s.doc.take().unwrap();
        doc.page.layout(&child_vp, m);
        if doc.page.frames_pending() {
            doc.page.flush();
            frames.sync_doc(doc.page.frame, &mut doc.page);
        }
        layout_level(frames, id, &doc.page, &child_vp, m);
        // Its own frames merged in, if it has any.
        let sig = signature(frames, id, &doc.page);
        let stale = doc.merged.as_ref().is_none_or(|mg| mg.sig != sig);
        if stale {
            doc.merged = merge(frames, id, &doc.page).map(|dl| {
                Box::new(Merged {
                    dl,
                    sig,
                    gen_: next_gen(),
                })
            });
        }
        let (cw, ch) = {
            let l = doc.display_list();
            (l.width, l.height)
        };
        if let Some(s) = frames.slot_mut(id) {
            s.vp = Some(child_vp);
            s.content = (cw, ch);
            let (mx, my) = (
                (cw - child_vp.width).max(0),
                (ch - child_vp.height).max(0),
            );
            let want = if s.attrs.no_scroll { (0, 0) } else { s.scroll };
            let clamped = (want.0.clamp(0, mx), want.1.clamp(0, my));
            if clamped != s.scroll {
                s.scroll = clamped;
                frames.version += 1;
            }
            if let Some(s) = frames.slot_mut(id) {
                s.doc = Some(doc);
            }
        }
    }
    // Elements with no box this time.
    for (i, s) in frames.slots.iter_mut().enumerate() {
        if s.parent == parent && !seen.contains(&i) {
            s.rect = Rect::default();
            s.rendered = false;
        }
    }
}

/// After the top page's own layout: lay out the frames and merge them into its list.
pub(crate) fn layout_frames(top: &mut crate::Document, vp: &Viewport, m: &dyn TextMeasurer) {
    let Some(frames) = top.frames.as_mut() else {
        top.merged = None;
        return;
    };
    let vp = Viewport {
        zoom: vp.zoom.clamp(25, 500),
        width: vp.width.clamp(1, 1 << 16),
        height: vp.height.clamp(1, 1 << 16),
        dark: vp.dark,
    };
    layout_level(frames, FrameId::TOP, &top.page, &vp, m);
    let sig = signature(frames, FrameId::TOP, &top.page);
    let stale = top.merged.as_ref().is_none_or(|mg| mg.sig != sig);
    if stale {
        top.merged = merge(frames, FrameId::TOP, &top.page).map(|dl| {
            Box::new(Merged {
                dl,
                sig,
                gen_: next_gen(),
            })
        });
    }
    top.frames_seen = frames.version;
}

/// Something that needs a layout: the page, a frame's document, or the frames' own state.
pub(crate) fn needs_layout(top: &crate::Document) -> bool {
    if top.page.changed() {
        return true;
    }
    let Some(f) = top.frames.as_ref() else {
        return false;
    };
    f.version != top.frames_seen
        || f.slots
            .iter()
            .any(|s| s.doc.as_ref().is_some_and(|d| d.page.changed()))
}

// ---- pointer, scrolling ---------------------------------------------------------------------

/// The frames under a point of `page`'s viewport, outermost first (not counting `parent`
/// itself), each with the point and scroll in its own viewport.
fn path_at(
    frames: &Frames,
    parent: FrameId,
    page: &Page,
    x: i32,
    y: i32,
    scroll: (i32, i32),
    out: &mut Vec<(FrameId, (i32, i32), (i32, i32))>,
) {
    if out.len() > 32 {
        return;
    }
    let Some((node, fixed)) = page.hit_node(x, y, scroll) else {
        return;
    };
    let Some(s) = frames.by_node(parent, node) else {
        return;
    };
    let Some(d) = s.doc.as_ref() else { return };
    if s.rect.is_empty() || !s.visible {
        return;
    }
    let (px, py) = if fixed {
        (x, y)
    } else {
        (x.saturating_add(scroll.0), y.saturating_add(scroll.1))
    };
    if !s.rect.contains(px, py) {
        return;
    }
    let local = (px - s.rect.x, py - s.rect.y);
    out.push((s.id, local, s.scroll));
    path_at(frames, s.id, &d.page, local.0, local.1, s.scroll, out);
}

pub(crate) fn hit_test(top: &crate::Document, x: i32, y: i32, scroll: (i32, i32)) -> Hit {
    let Some(frames) = top.frames.as_ref() else {
        return top.page.hit_test(x, y, scroll);
    };
    let mut path = Vec::new();
    path_at(frames, FrameId::TOP, &top.page, x, y, scroll, &mut path);
    match path.last() {
        None => top.page.hit_test(x, y, scroll),
        Some(&(id, p, sc)) => match frames.slot(id).and_then(|s| s.doc.as_ref()) {
            Some(d) => d.page.hit_test(p.0, p.1, sc),
            None => top.page.hit_test(x, y, scroll),
        },
    }
}

/// The frame under a point (the page itself if none).
pub(crate) fn frame_at(top: &crate::Document, x: i32, y: i32, scroll: (i32, i32)) -> FrameId {
    let Some(frames) = top.frames.as_ref() else {
        return FrameId::TOP;
    };
    let mut path = Vec::new();
    path_at(frames, FrameId::TOP, &top.page, x, y, scroll, &mut path);
    path.last().map_or(FrameId::TOP, |p| p.0)
}

/// Scroll frame `id` by `dx`, `dy` as far as it goes; what was not used.
fn scroll_by(frames: &mut Frames, id: FrameId, dx: i32, dy: i32) -> (i32, i32) {
    let Some(s) = frames.slot_mut(id) else {
        return (dx, dy);
    };
    if s.attrs.no_scroll {
        return (dx, dy);
    }
    let vp = s.vp.unwrap_or(Viewport::new(s.rect.w, s.rect.h));
    let (mx, my) = (
        (s.content.0 - vp.width).max(0),
        (s.content.1 - vp.height).max(0),
    );
    let nx = s.scroll.0.saturating_add(dx).clamp(0, mx);
    let ny = s.scroll.1.saturating_add(dy).clamp(0, my);
    let (ux, uy) = (nx - s.scroll.0, ny - s.scroll.1);
    if (nx, ny) != s.scroll {
        s.scroll = (nx, ny);
        frames.version += 1;
    }
    (dx - ux, dy - uy)
}

pub(crate) fn set_scroll(top: &mut crate::Document, id: FrameId, x: i32, y: i32) -> (i32, i32) {
    let Some(frames) = top.frames.as_mut() else {
        return (0, 0);
    };
    let Some(s) = frames.slot(id) else {
        return (0, 0);
    };
    let (sx, sy) = s.scroll;
    scroll_by(frames, id, x.saturating_sub(sx), y.saturating_sub(sy));
    frames.slot(id).map_or((0, 0), |s| s.scroll)
}

pub(crate) fn scroll_at(
    top: &mut crate::Document,
    x: i32,
    y: i32,
    scroll: (i32, i32),
    dx: i32,
    dy: i32,
) -> (i32, i32) {
    let Some(frames) = top.frames.as_mut() else {
        return (dx, dy);
    };
    let mut path = Vec::new();
    path_at(frames, FrameId::TOP, &top.page, x, y, scroll, &mut path);
    let (mut dx, mut dy) = (dx, dy);
    for &(id, _, _) in path.iter().rev() {
        if dx == 0 && dy == 0 {
            break;
        }
        (dx, dy) = scroll_by(frames, id, dx, dy);
    }
    (dx, dy)
}

pub(crate) fn reveal(top: &mut crate::Document, id: FrameId, rect: Rect) -> Rect {
    let Some(frames) = top.frames.as_mut() else {
        return rect;
    };
    let mut cur = rect;
    let mut at = id;
    for _ in 0..16 {
        if at.is_top() {
            break;
        }
        let Some(s) = frames.slot(at) else { break };
        let (sc, sr, parent) = (s.scroll, s.rect, s.parent);
        // Centre it when it is not fully in view.
        let fit = |pos: i32, len: i32, view: i32, cur_s: i32| -> i32 {
            if pos >= cur_s && pos.saturating_add(len) <= cur_s.saturating_add(view) {
                cur_s
            } else {
                pos.saturating_add(len / 2).saturating_sub(view / 2)
            }
        };
        let nx = fit(cur.x, cur.w, sr.w, sc.0);
        let ny = fit(cur.y, cur.h, sr.h, sc.1);
        set_slot_scroll(frames, at, nx, ny);
        let sc = frames.slot(at).map_or(sc, |s| s.scroll);
        cur = Rect::new(
            cur.x.saturating_sub(sc.0).saturating_add(sr.x),
            cur.y.saturating_sub(sc.1).saturating_add(sr.y),
            cur.w,
            cur.h,
        );
        at = parent;
    }
    cur
}

fn set_slot_scroll(frames: &mut Frames, id: FrameId, x: i32, y: i32) {
    if let Some(s) = frames.slot(id) {
        let (sx, sy) = s.scroll;
        scroll_by(frames, id, x.saturating_sub(sx), y.saturating_sub(sy));
    }
}

pub(crate) fn fragment(top: &mut crate::Document, id: FrameId, frag: &str) -> bool {
    let Some(y) = top.frame_doc(id).and_then(|d| d.page.fragment_y(frag)) else {
        return false;
    };
    if id.is_top() {
        return true;
    }
    set_scroll(top, id, 0, y);
    true
}

// ---- focus -----------------------------------------------------------------------------------

fn order_in(frames: &Frames, parent: FrameId, page: &Page, out: &mut Vec<(FrameId, ControlId)>) {
    if out.len() > 100_000 {
        return;
    }
    let mut n = 0;
    while n != NONE {
        let Some(e) = page.dom.element(n) else {
            n = page.dom.next_in(n, 0);
            continue;
        };
        if e.is(tag::TEMPLATE) {
            n = page.dom.next_skipping_children(n, 0);
            continue;
        }
        if e.is(tag::IFRAME) {
            if let Some(s) = frames.by_node(parent, n)
                && let Some(d) = s.doc.as_ref()
                && !s.rect.is_empty()
            {
                order_in(frames, s.id, &d.page, out);
            }
            n = page.dom.next_skipping_children(n, 0);
            continue;
        }
        if let Some(id) = page.control_of_node(n)
            && let Some(c) = page.controls().get(id.0 as usize)
            && !c.rect.is_empty()
            && !c.disabled
        {
            out.push((page.frame.id, id));
        }
        n = page.dom.next_in(n, 0);
    }
}

pub(crate) fn focus_order(top: &crate::Document) -> Vec<(FrameId, ControlId)> {
    let mut v = Vec::new();
    match top.frames.as_ref() {
        Some(f) => order_in(f, FrameId::TOP, &top.page, &mut v),
        None => {
            for (i, c) in top.page.controls().iter().enumerate() {
                if !c.rect.is_empty() && !c.disabled {
                    v.push((FrameId::TOP, ControlId(i as u32)));
                }
            }
        }
    }
    v
}

// ---- find and text ---------------------------------------------------------------------------

/// A rectangle of a frame's document moved into its parent's (the slot's box, the frame's
/// scroll), cut to what the slot shows.
fn up(r: Rect, slot: Rect, scroll: (i32, i32)) -> Rect {
    let moved = shift_rect(r, slot.x.saturating_sub(scroll.0), slot.y.saturating_sub(scroll.1));
    moved.intersect(&slot)
}

fn find_in(frames: &Frames, parent: FrameId, page: &Page, q: &str, cs: bool) -> Vec<FindMatch> {
    let own = page.find_at(q, cs);
    let marks = page.frame_marks();
    let mut kids: Vec<(usize, Vec<FindMatch>)> = Vec::new();
    for mk in marks {
        let Some(s) = frames.by_node(parent, mk.node) else {
            continue;
        };
        let Some(d) = s.doc.as_ref() else { continue };
        if s.rect.is_empty() {
            continue;
        }
        let mut ms = find_in(frames, s.id, &d.page, q, cs);
        for m in ms.iter_mut() {
            m.rects = m
                .rects
                .iter()
                .map(|&r| up(r, s.rect, s.scroll))
                .filter(|r| !r.is_empty())
                .collect();
        }
        if !ms.is_empty() {
            kids.push((mk.runs_before, ms));
        }
    }
    if kids.is_empty() {
        return own.into_iter().map(|(m, _)| m).collect();
    }
    let mut out = Vec::new();
    let mut k = 0;
    for (m, run) in own {
        while k < kids.len() && kids[k].0 <= run as usize {
            out.extend(core::mem::take(&mut kids[k].1));
            k += 1;
        }
        out.push(m);
    }
    for (_, ms) in kids.iter_mut().skip(k) {
        out.extend(core::mem::take(ms));
    }
    out
}

pub(crate) fn find(top: &crate::Document, q: &str, cs: bool) -> Vec<FindMatch> {
    match top.frames.as_ref() {
        None => top.page.find(q, cs),
        Some(f) => find_in(f, FrameId::TOP, &top.page, q, cs),
    }
}

fn text_in(frames: &Frames, parent: FrameId, page: &Page) -> String {
    let mut chunks: Vec<(usize, String)> = Vec::new();
    for mk in page.frame_marks() {
        let Some(s) = frames.by_node(parent, mk.node) else {
            continue;
        };
        let Some(d) = s.doc.as_ref() else { continue };
        if s.rect.is_empty() {
            continue;
        }
        let t = text_in(frames, s.id, &d.page);
        if !t.is_empty() {
            chunks.push((mk.runs_before, t));
        }
    }
    page.text_with(&chunks)
}

pub(crate) fn text(top: &crate::Document) -> String {
    match top.frames.as_ref() {
        None => top.page.text(),
        Some(f) => text_in(f, FrameId::TOP, &top.page),
    }
}
