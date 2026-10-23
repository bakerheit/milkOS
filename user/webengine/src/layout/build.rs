//! Box generation (CSS 2.1 §9.2, CSS Display 3): one box per rendered element, text boxes,
//! `::before`/`::after`, list markers, replaced elements, then the fix-ups:
//! - anonymous block boxes around inline runs in a block container that has block children
//!   (CSS 2.1 §9.2.1.1); an inline box that ends up holding a block-level box becomes a block
//!   box itself (a simplification of "block-in-inline" splitting that keeps the content and
//!   its order, losing only the inline's own decorations around the block);
//! - flex items: in-flow children of a flex container, text runs wrapped (Flexbox §4);
//! - tables: missing rows / cells / tables wrapped in anonymous ones (CSS 2.1 §17.2.1).

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::*;
use crate::api::{ControlKind, ImageId};
use crate::atoms::tag;
use crate::dom::{NONE, NodeData, Ns};

/// What layout knows about an image.
#[derive(Clone, Debug, Default)]
pub struct ImageState {
    pub url: String,
    pub size: Option<(u32, u32)>,
    pub failed: bool,
}

pub struct ImageLookup<'a> {
    /// `<img src>` / `<input type=image>` element → its image.
    pub of_node: &'a BTreeMap<NodeId, ImageId>,
    pub images: &'a [ImageState],
}

struct ListCx {
    next: i32,
    step: i32,
}

struct B<'a, 'b> {
    inp: &'a Inputs<'b>,
    boxes: Vec<LBox>,
    anon: BTreeMap<(usize, u8), Rc<Style>>,
    lists: Vec<ListCx>,
    /// CSS counters: name → nested instances (innermost last).
    counters: BTreeMap<Rc<str>, Vec<i32>>,
    /// The counter instances created, in order (popped when their parent ends).
    counter_scope: Vec<Rc<str>>,
}

fn display_code(d: Display) -> u8 {
    d as u8
}

impl B<'_, '_> {
    fn new_box(&mut self, kind: Kind, style: Rc<Style>, node: NodeId, parent: BoxId) -> BoxId {
        let id = self.boxes.len() as BoxId;
        let mut b = LBox::new(kind, style, node);
        b.block_level = match b.kind {
            Kind::Text(_) | Kind::Break | Kind::Wbr | Kind::Inline | Kind::Marker(_) => false,
            _ => !b.style.display.is_inline_level(),
        };
        // Text and markers share their element's style; only element boxes leave the flow.
        b.out_of_flow = !matches!(
            b.kind,
            Kind::Text(_) | Kind::Break | Kind::Wbr | Kind::Marker(_)
        ) && (b.style.float != Float::None || b.style.is_out_of_flow_positioned());
        b.parent = parent;
        self.boxes.push(b);
        if parent != NO_BOX {
            self.boxes[parent as usize].children.push(id);
        }
        id
    }

    /// The style of an anonymous box of `display` inside a box styled `parent`.
    fn anon_style(&mut self, parent: &Rc<Style>, display: Display) -> Rc<Style> {
        let key = (Rc::as_ptr(parent) as usize, display_code(display));
        if let Some(s) = self.anon.get(&key) {
            return s.clone();
        }
        let mut s = Style::inherit(parent, &Style::initial());
        s.display = display;
        s.original_display = display;
        let s = Rc::new(s);
        self.anon.insert(key, s.clone());
        s
    }

    fn element_style(&self, n: NodeId) -> Option<Rc<Style>> {
        self.inp.styles.of.get(n as usize).and_then(|s| s.clone())
    }

    fn gen_children(&mut self, parent_box: BoxId, el: NodeId, el_style: &Rc<Style>) {
        let dom = self.inp.dom;
        let is_list = dom.element(el).is_some_and(|e| {
            e.ns == Ns::Html && matches!(e.name, tag::OL | tag::UL | tag::MENU | tag::DIR)
        });
        if is_list {
            let e = dom.element(el).unwrap();
            let reversed = e.is(tag::OL) && e.has_attr("reversed");
            let count = dom
                .children(el)
                .filter(|&c| dom.is_html(c, tag::LI))
                .count() as i32;
            let start = e
                .attr("start")
                .and_then(|s| s.trim().parse::<i32>().ok())
                .unwrap_or(if reversed { count } else { 1 });
            self.lists.push(ListCx {
                next: start,
                step: if reversed { -1 } else { 1 },
            });
        }
        // Counters reset by the children stay in scope for their later siblings (CSS 2.1
        // §12.4.1), so they are dropped when the parent is done.
        let mark = self.counter_scope.len();
        let mut c = dom.node(el).first_child;
        while c != NONE {
            self.gen_node(parent_box, c, el_style);
            c = dom.node(c).next;
        }
        for name in self.counter_scope.drain(mark..).collect::<Vec<_>>() {
            if let Some(v) = self.counters.get_mut(&name) {
                v.pop();
            }
        }
        if is_list {
            self.lists.pop();
        }
    }

    fn gen_node(&mut self, parent_box: BoxId, n: NodeId, parent_style: &Rc<Style>) {
        let dom = self.inp.dom;
        match &dom.node(n).data {
            NodeData::Text(t) => {
                if !t.is_empty() {
                    self.new_box(
                        Kind::Text(Rc::from(t.as_str())),
                        parent_style.clone(),
                        n,
                        parent_box,
                    );
                }
            }
            NodeData::Element(_) => self.gen_element(parent_box, n),
            _ => {}
        }
    }

    fn gen_element(&mut self, parent_box: BoxId, n: NodeId) {
        use tag::*;
        let dom = self.inp.dom;
        let Some(style) = self.element_style(n) else {
            return;
        };
        if style.display == Display::None {
            return;
        }
        let e = dom.element(n).unwrap();
        // CSS counters: reset (a new instance) then increment (CSS Lists 3 §4).
        if let Some((name, v)) = &style.counter_reset {
            self.counters.entry(name.clone()).or_default().push(*v);
            self.counter_scope.push(name.clone());
        }
        if let Some((name, v)) = &style.counter_increment {
            let stack = self.counters.entry(name.clone()).or_default();
            if stack.is_empty() {
                stack.push(0);
                self.counter_scope.push(name.clone());
            }
            if let Some(top) = stack.last_mut() {
                *top = top.saturating_add(*v);
            }
        }
        if style.display == Display::Contents {
            self.pseudo(parent_box, n, &style, false);
            self.gen_children(parent_box, n, &style);
            self.pseudo(parent_box, n, &style, true);
            return;
        }
        let html = e.ns == Ns::Html;
        // Replaced elements and form controls.
        if html || e.name == SVG {
            match e.name {
                BR if html => {
                    self.new_box(Kind::Break, style, n, parent_box);
                    return;
                }
                WBR if html => {
                    self.new_box(Kind::Wbr, style, n, parent_box);
                    return;
                }
                IMG => {
                    let img = self.inp.images.of_node.get(&n).copied();
                    let st = img.and_then(|i| self.inp.images.images.get(i.0 as usize));
                    let failed = img.is_none() || st.is_some_and(|s| s.failed);
                    let alt = e.attr("alt").unwrap_or("");
                    let sized = e.has_attr("width") && e.has_attr("height")
                        || !matches!(style.width, Size::Auto)
                            && !matches!(style.height, Size::Auto);
                    if failed && !sized {
                        // The alt text, inline (HTML §15.4.2).
                        if !alt.is_empty() {
                            let ib = self.new_box(Kind::Inline, style.clone(), n, parent_box);
                            self.boxes[ib as usize].block_level = false;
                            self.new_box(Kind::Text(Rc::from(alt)), style, n, ib);
                        }
                        return;
                    }
                    let size = st.and_then(|s| s.size);
                    self.new_box(
                        Kind::Replaced(Replaced::Image(if failed { None } else { img }, size)),
                        style,
                        n,
                        parent_box,
                    );
                    return;
                }
                INPUT | SELECT | TEXTAREA => {
                    if let Some(&(id, kind)) = self.inp.controls.get(&n) {
                        if kind == ControlKind::ImageSubmit {
                            let img = self.inp.images.of_node.get(&n).copied();
                            let size = img
                                .and_then(|i| self.inp.images.images.get(i.0 as usize))
                                .and_then(|s| s.size);
                            if size.is_some() {
                                self.new_box(
                                    Kind::Replaced(Replaced::Image(img, size)),
                                    style,
                                    n,
                                    parent_box,
                                );
                                return;
                            }
                        }
                        self.new_box(
                            Kind::Replaced(Replaced::Control(id, kind)),
                            style,
                            n,
                            parent_box,
                        );
                    }
                    return;
                }
                IFRAME | VIDEO | CANVAS | EMBED | OBJECT | AUDIO | SVG => {
                    if e.name == AUDIO && !e.has_attr("controls") {
                        return;
                    }
                    let (w, h) = match e.name {
                        AUDIO => (300.0, 54.0),
                        SVG => (
                            e.attr("width")
                                .and_then(|v| v.trim().trim_end_matches("px").parse().ok())
                                .unwrap_or(0.0),
                            e.attr("height")
                                .and_then(|v| v.trim().trim_end_matches("px").parse().ok())
                                .unwrap_or(0.0),
                        ),
                        _ => (300.0, 150.0),
                    };
                    self.new_box(Kind::Replaced(Replaced::Empty(w, h)), style, n, parent_box);
                    return;
                }
                _ => {}
            }
        }
        let kind = match style.display {
            Display::Inline => Kind::Inline,
            Display::Table | Display::InlineTable => Kind::Table,
            Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup => {
                Kind::RowGroup
            }
            Display::TableRow => Kind::Row,
            Display::TableCell => {
                let span = |a: &str, max: u32| {
                    e.attr(a)
                        .and_then(|v| v.trim().parse::<u32>().ok())
                        .unwrap_or(1)
                        .clamp(1, max)
                };
                let rowspan = e
                    .attr("rowspan")
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .unwrap_or(1);
                Kind::Cell {
                    colspan: span("colspan", 1000),
                    rowspan: if rowspan == 0 {
                        65534
                    } else {
                        rowspan.min(65534)
                    },
                }
            }
            Display::TableCaption => Kind::Caption,
            Display::TableColumn | Display::TableColumnGroup => {
                let span = e
                    .attr("span")
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .unwrap_or(1)
                    .clamp(1, 1000);
                Kind::Column { span }
            }
            Display::Flex | Display::InlineFlex => Kind::Flex,
            Display::Grid | Display::InlineGrid => Kind::Grid,
            _ => Kind::Block,
        };
        let is_column = matches!(kind, Kind::Column { .. });
        let b = self.new_box(kind, style.clone(), n, parent_box);
        if is_column {
            // <col> inside <colgroup>: keep as children of the group.
            let mut c = dom.node(n).first_child;
            while c != NONE {
                if dom.element(c).is_some() {
                    self.gen_element(b, c);
                }
                c = dom.node(c).next;
            }
            return;
        }
        if style.display == Display::ListItem {
            self.marker(b, n, &style);
        }
        self.pseudo(b, n, &style, false);
        if !(html && matches!(e.name, TEXTAREA | SELECT | OPTION)) {
            self.gen_children(b, n, &style);
        }
        self.pseudo(b, n, &style, true);
        if html && e.name == FIELDSET {
            // The rendered legend (HTML §15.3.13): the first legend child, not floated or
            // positioned, sits in the top border; it is laid out apart from the flow.
            let kids = self.boxes[b as usize].children.clone();
            if let Some(&l) = kids.iter().find(|&&c| !self.is_collapsible_ws(c)) {
                let lb = &self.boxes[l as usize];
                if dom.is_html(lb.node, LEGEND) && !lb.out_of_flow {
                    self.boxes[l as usize].out_of_flow = true;
                    self.boxes[l as usize].block_level = true;
                    self.boxes[b as usize].legend = l;
                }
            }
        }
        self.fixup(b);
    }

    fn pseudo(&mut self, parent_box: BoxId, n: NodeId, _el_style: &Rc<Style>, after: bool) {
        let map = if after {
            &self.inp.styles.after
        } else {
            &self.inp.styles.before
        };
        let Some(ps) = map.get(&n).cloned() else {
            return;
        };
        let Some(items) = ps.content.clone() else {
            return;
        };
        let e = self.inp.dom.element(n);
        let mut text = String::new();
        for it in items.iter() {
            match it {
                ContentItem::Str(s) => text.push_str(s),
                ContentItem::Attr(a) => text.push_str(e.and_then(|e| e.attr(a)).unwrap_or("")),
                ContentItem::OpenQuote | ContentItem::CloseQuote => text.push('"'),
                ContentItem::Counter(name, st) => {
                    let v = self
                        .counters
                        .get(name)
                        .and_then(|v| v.last())
                        .copied()
                        .unwrap_or(0);
                    text.push_str(&format_counter(v, *st));
                }
            }
        }
        if text.is_empty() && ps.display.is_inline_level() {
            return;
        }
        let kind = if ps.display == Display::Inline {
            Kind::Inline
        } else {
            Kind::Block
        };
        let b = self.new_box(kind, ps.clone(), NONE, parent_box);
        if !text.is_empty() {
            self.new_box(Kind::Text(Rc::from(text.as_str())), ps, NONE, b);
        }
        self.fixup(b);
    }

    /// A list item's marker (CSS Lists 3 §3): the counter value or a bullet.
    fn marker(&mut self, li: BoxId, n: NodeId, style: &Rc<Style>) {
        let e = self.inp.dom.element(n).unwrap();
        // The list-item counter (HTML's ol/li numbering).
        let value = if let Some(v) = e.attr("value").and_then(|v| v.trim().parse::<i32>().ok()) {
            if let Some(l) = self.lists.last_mut() {
                l.next = v.saturating_add(l.step);
            }
            v
        } else if let Some(l) = self.lists.last_mut() {
            let v = l.next;
            l.next = l.next.saturating_add(l.step);
            v
        } else {
            1
        };
        if style.list_style_type == ListStyleType::None {
            return;
        }
        let text: String = match style.list_style_type {
            ListStyleType::Disc | ListStyleType::Circle | ListStyleType::Square => String::new(),
            ListStyleType::Str => String::from(style.list_style_string.as_deref().unwrap_or("")),
            t => {
                let mut s = format_counter(value, t);
                s.push_str(". ");
                s
            }
        };
        let ms = self.anon_style(style, Display::Inline);
        let m = self.new_box(Kind::Marker(Rc::from(text.as_str())), ms, NONE, NO_BOX);
        self.boxes[m as usize].parent = li;
        if style.list_inside {
            self.boxes[li as usize].children.push(m);
        } else {
            self.boxes[li as usize].marker = m;
        }
    }

    fn is_collapsible_ws(&self, id: BoxId) -> bool {
        let b = &self.boxes[id as usize];
        match &b.kind {
            Kind::Text(t) => {
                b.style.white_space.collapses_spaces() && t.chars().all(crate::dom::is_html_space)
            }
            _ => false,
        }
    }

    /// Wrap `kids[range]` into a new anonymous box of `kind`/`display` under `parent`.
    fn wrap(&mut self, parent: BoxId, kids: &[BoxId], kind: Kind, display: Display) -> BoxId {
        let ps = self.boxes[parent as usize].style.clone();
        let st = self.anon_style(&ps, display);
        let w = self.new_box(kind, st, NONE, NO_BOX);
        self.boxes[w as usize].parent = parent;
        for &k in kids {
            self.boxes[k as usize].parent = w;
        }
        self.boxes[w as usize].children = kids.to_vec();
        w
    }

    /// The anonymous-box fix-ups for one box, after its children were generated.
    fn fixup(&mut self, b: BoxId) {
        let kind = self.boxes[b as usize].kind.clone();
        match kind {
            Kind::Inline => {
                // Block-in-inline: this inline becomes a block box.
                let has_block = self.boxes[b as usize].children.iter().any(|&c| {
                    self.boxes[c as usize].block_level && !self.boxes[c as usize].out_of_flow
                });
                if has_block {
                    self.boxes[b as usize].kind = Kind::Block;
                    self.boxes[b as usize].block_level = true;
                    self.fix_block_container(b);
                }
            }
            Kind::Block | Kind::Cell { .. } | Kind::Caption => {
                self.wrap_table_parts(b);
                self.fix_block_container(b);
            }
            Kind::Flex | Kind::Grid => self.fix_flex(b),
            Kind::Table => self.fix_table(b),
            Kind::RowGroup => self.fix_row_group(b),
            Kind::Row => self.fix_row(b),
            _ => {}
        }
    }

    /// CSS 2.1 §9.2.1.1: in a block container with block-level children, inline runs go into
    /// anonymous blocks; whitespace-only runs are dropped.
    fn fix_block_container(&mut self, b: BoxId) {
        let kids = core::mem::take(&mut self.boxes[b as usize].children);
        let any_block = kids
            .iter()
            .any(|&c| self.boxes[c as usize].block_level && !self.boxes[c as usize].out_of_flow);
        if !any_block {
            self.boxes[b as usize].children = kids;
            self.boxes[b as usize].inline_content = true;
            return;
        }
        let mut out = Vec::new();
        let mut run: Vec<BoxId> = Vec::new();
        let flush = |this: &mut Self, run: &mut Vec<BoxId>, out: &mut Vec<BoxId>| {
            if run.is_empty() {
                return;
            }
            let in_flow_inline = run.iter().any(|&c| !this.boxes[c as usize].out_of_flow);
            let all_ws = run
                .iter()
                .all(|&c| this.boxes[c as usize].out_of_flow || this.is_collapsible_ws(c));
            if !in_flow_inline || all_ws {
                // Only floats / abs boxes (and collapsible space): they stay block-level
                // siblings.
                for &c in run.iter() {
                    if this.boxes[c as usize].out_of_flow {
                        out.push(c);
                    }
                }
            } else {
                let w = this.wrap(b, run, Kind::Block, Display::Block);
                this.boxes[w as usize].block_level = true;
                this.boxes[w as usize].inline_content = true;
                out.push(w);
            }
            run.clear();
        };
        for c in kids {
            let cb = &self.boxes[c as usize];
            if cb.block_level && !cb.out_of_flow {
                flush(self, &mut run, &mut out);
                out.push(c);
            } else {
                run.push(c);
            }
        }
        flush(self, &mut run, &mut out);
        self.boxes[b as usize].children = out;
        self.boxes[b as usize].inline_content = false;
    }

    /// Flexbox §4: each in-flow child is a flex item; contiguous text is wrapped in an
    /// anonymous item; whitespace-only text is dropped.
    fn fix_flex(&mut self, b: BoxId) {
        let kids = core::mem::take(&mut self.boxes[b as usize].children);
        let mut out = Vec::new();
        let mut run: Vec<BoxId> = Vec::new();
        for c in kids {
            let is_text_like = matches!(
                self.boxes[c as usize].kind,
                Kind::Text(_) | Kind::Break | Kind::Wbr | Kind::Inline | Kind::Marker(_)
            ) && !self.boxes[c as usize].block_level;
            if is_text_like {
                run.push(c);
                continue;
            }
            if !run.is_empty() {
                if !run.iter().all(|&r| self.is_collapsible_ws(r)) {
                    let w = self.wrap(b, &run, Kind::Block, Display::Block);
                    self.boxes[w as usize].block_level = true;
                    self.boxes[w as usize].inline_content = true;
                    out.push(w);
                }
                run.clear();
            }
            self.boxes[c as usize].block_level = true;
            out.push(c);
        }
        if !run.is_empty() && !run.iter().all(|&r| self.is_collapsible_ws(r)) {
            let w = self.wrap(b, &run, Kind::Block, Display::Block);
            self.boxes[w as usize].block_level = true;
            self.boxes[w as usize].inline_content = true;
            out.push(w);
        }
        self.boxes[b as usize].children = out;
    }

    fn is_table_part(&self, c: BoxId) -> bool {
        matches!(
            self.boxes[c as usize].kind,
            Kind::RowGroup | Kind::Row | Kind::Cell { .. } | Kind::Caption | Kind::Column { .. }
        )
    }

    /// §17.2.1 step 3: table parts outside a table get an anonymous table around them.
    fn wrap_table_parts(&mut self, b: BoxId) {
        if !self.boxes[b as usize]
            .children
            .iter()
            .any(|&c| self.is_table_part(c))
        {
            return;
        }
        let kids = core::mem::take(&mut self.boxes[b as usize].children);
        let mut out = Vec::new();
        let mut run: Vec<BoxId> = Vec::new();
        let mut i = 0;
        while i <= kids.len() {
            let part = i < kids.len()
                && (self.is_table_part(kids[i])
                    || (!run.is_empty() && self.is_collapsible_ws(kids[i])));
            if part {
                run.push(kids[i]);
            } else {
                if !run.is_empty() {
                    // In inline content the anonymous table is an inline-table (§17.2.1 3).
                    let inline_ctx = kids.iter().any(|&k| {
                        !self.is_table_part(k)
                            && !self.boxes[k as usize].block_level
                            && !self.is_collapsible_ws(k)
                    }) && !kids
                        .iter()
                        .any(|&k| !self.is_table_part(k) && self.boxes[k as usize].block_level);
                    let disp = if inline_ctx {
                        Display::InlineTable
                    } else {
                        Display::Table
                    };
                    let t = self.wrap(b, &run, Kind::Table, disp);
                    self.boxes[t as usize].block_level = !inline_ctx;
                    self.fix_table(t);
                    out.push(t);
                    run.clear();
                }
                if i < kids.len() {
                    out.push(kids[i]);
                }
            }
            i += 1;
        }
        self.boxes[b as usize].children = out;
    }

    fn fix_table(&mut self, t: BoxId) {
        let kids = core::mem::take(&mut self.boxes[t as usize].children);
        let mut out = Vec::new();
        let mut run: Vec<BoxId> = Vec::new();
        for c in kids {
            let k = &self.boxes[c as usize].kind;
            if matches!(
                k,
                Kind::RowGroup | Kind::Row | Kind::Caption | Kind::Column { .. }
            ) {
                self.flush_cells(t, &mut run, &mut out);
                out.push(c);
            } else if !self.is_collapsible_ws(c) {
                run.push(c);
            }
        }
        self.flush_cells(t, &mut run, &mut out);
        // Captions are block containers; rows directly in the table are fixed already.
        self.boxes[t as usize].children = out;
    }

    /// Wrap stray table content into an anonymous row (whose fix-up makes cells).
    fn flush_cells(&mut self, parent: BoxId, run: &mut Vec<BoxId>, out: &mut Vec<BoxId>) {
        if run.is_empty() {
            return;
        }
        let r = self.wrap(parent, run, Kind::Row, Display::TableRow);
        self.boxes[r as usize].block_level = true;
        self.fix_row(r);
        out.push(r);
        run.clear();
    }

    fn fix_row_group(&mut self, g: BoxId) {
        let kids = core::mem::take(&mut self.boxes[g as usize].children);
        let mut out = Vec::new();
        let mut run: Vec<BoxId> = Vec::new();
        for c in kids {
            if matches!(self.boxes[c as usize].kind, Kind::Row) {
                self.flush_cells(g, &mut run, &mut out);
                out.push(c);
            } else if !self.is_collapsible_ws(c) {
                run.push(c);
            }
        }
        self.flush_cells(g, &mut run, &mut out);
        self.boxes[g as usize].children = out;
    }

    fn fix_row(&mut self, r: BoxId) {
        let kids = core::mem::take(&mut self.boxes[r as usize].children);
        let mut out = Vec::new();
        let mut run: Vec<BoxId> = Vec::new();
        let flush = |this: &mut Self, run: &mut Vec<BoxId>, out: &mut Vec<BoxId>| {
            if run.is_empty() {
                return;
            }
            let c = this.wrap(
                r,
                run,
                Kind::Cell {
                    colspan: 1,
                    rowspan: 1,
                },
                Display::TableCell,
            );
            this.boxes[c as usize].block_level = true;
            this.fix_block_container(c);
            out.push(c);
            run.clear();
        };
        for c in kids {
            if matches!(self.boxes[c as usize].kind, Kind::Cell { .. }) {
                flush(self, &mut run, &mut out);
                out.push(c);
            } else if !self.is_collapsible_ws(c) {
                run.push(c);
            }
        }
        flush(self, &mut run, &mut out);
        self.boxes[r as usize].children = out;
    }
}

/// A list counter in a list-style-type (CSS Counter Styles 3 §6 predefined styles).
pub fn format_counter(v: i32, t: ListStyleType) -> String {
    match t {
        ListStyleType::DecimalLeadingZero => {
            if (0..10).contains(&v) {
                alloc::format!("0{v}")
            } else {
                alloc::format!("{v}")
            }
        }
        ListStyleType::LowerAlpha | ListStyleType::UpperAlpha | ListStyleType::LowerGreek => {
            if v <= 0 {
                return alloc::format!("{v}");
            }
            let (base, first): (u32, u32) = match t {
                ListStyleType::LowerGreek => (24, 0x3B1),
                ListStyleType::UpperAlpha => (26, 'A' as u32),
                _ => (26, 'a' as u32),
            };
            let mut n = v as u32;
            let mut out = Vec::new();
            while n > 0 {
                n -= 1;
                let mut c = first + n % base;
                if t == ListStyleType::LowerGreek && c >= 0x3C2 {
                    c += 1; // skip final sigma
                }
                out.push(char::from_u32(c).unwrap_or('?'));
                n /= base;
            }
            out.iter().rev().collect()
        }
        ListStyleType::LowerRoman | ListStyleType::UpperRoman => {
            if !(1..4000).contains(&v) {
                return alloc::format!("{v}");
            }
            let table = [
                (1000, "m"),
                (900, "cm"),
                (500, "d"),
                (400, "cd"),
                (100, "c"),
                (90, "xc"),
                (50, "l"),
                (40, "xl"),
                (10, "x"),
                (9, "ix"),
                (5, "v"),
                (4, "iv"),
                (1, "i"),
            ];
            let mut n = v;
            let mut s = String::new();
            for (k, r) in table {
                while n >= k {
                    s.push_str(r);
                    n -= k;
                }
            }
            if t == ListStyleType::UpperRoman {
                s.to_ascii_uppercase()
            } else {
                s
            }
        }
        _ => alloc::format!("{v}"),
    }
}

/// Generate the box tree. Box 0 is the root element's box.
pub fn build(inp: &Inputs) -> Vec<LBox> {
    let mut b = B {
        inp,
        boxes: Vec::new(),
        anon: BTreeMap::new(),
        lists: Vec::new(),
        counters: BTreeMap::new(),
        counter_scope: Vec::new(),
    };
    let root = inp.dom.document_element();
    if root == NONE {
        return b.boxes;
    }
    let Some(style) = b.element_style(root) else {
        return b.boxes;
    };
    if style.display == Display::None {
        return b.boxes;
    }
    let kind = match style.display {
        Display::Flex | Display::InlineFlex => Kind::Flex,
        Display::Grid | Display::InlineGrid => Kind::Grid,
        Display::Table | Display::InlineTable => Kind::Table,
        _ => Kind::Block,
    };
    let r = b.new_box(kind, style.clone(), root, NO_BOX);
    b.boxes[r as usize].block_level = true;
    b.boxes[r as usize].out_of_flow = false;
    b.pseudo(r, root, &style, false);
    b.gen_children(r, root, &style);
    b.pseudo(r, root, &style, true);
    b.fixup(r);
    b.boxes
}
