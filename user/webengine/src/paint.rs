//! Painting the box tree into a [`DisplayList`], in the order of CSS 2.1 Appendix E,
//! simplified: for each box its background and border, then (for an inline formatting
//! context) the inline boxes' backgrounds, floats and atomic inlines, then the text with its
//! decorations; positioned boxes are painted after the normal flow, sorted by `z-index`
//! (negative ones before it); `position: fixed` boxes go to the fixed layer. `overflow` other
//! than visible clips descendants to the padding box. Relative offsets are applied here.
//!
//! It also records hit rectangles (for links, controls and titles) and each element's box.

use alloc::rc::Rc;
use alloc::vec::Vec;

use crate::api::*;
use crate::dom::{NONE, NodeId};
use crate::layout::*;
use crate::style::types::*;

pub struct HitRect {
    pub rect: Rect,
    pub node: NodeId,
    pub fixed: bool,
    /// From a text run (an I-beam would fit).
    pub text: bool,
}

pub struct Painted {
    pub dl: DisplayList,
    pub hits: Vec<HitRect>,
    /// Per DOM node: the union of its boxes (document space; fixed boxes in viewport space).
    pub node_rects: Vec<Rect>,
    pub control_rects: Vec<(ControlId, Rect)>,
}

fn px(a: Au) -> i32 {
    (a + AU / 2).div_euclid(AU)
}

fn rect_au(x: Au, y: Au, w: Au, h: Au) -> Rect {
    let x0 = px(x);
    let y0 = px(y);
    Rect::new(x0, y0, px(x + w) - x0, px(y + h) - y0)
}

fn union(a: Rect, b: Rect) -> Rect {
    if a.is_empty() && a.w == 0 && a.h == 0 && a.x == 0 && a.y == 0 {
        return b;
    }
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = a.right().max(b.right());
    let y1 = a.bottom().max(b.bottom());
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

fn fade(c: Color, opacity: f32) -> Color {
    if opacity >= 1.0 {
        return c;
    }
    let a = ((c >> 24) as f32 * opacity) as u32;
    (a.min(255) << 24) | (c & 0x00FF_FFFF)
}

/// What painting needs to know about images (intrinsic sizes, for background tiles).
pub type ImageSize<'a> = &'a dyn Fn(ImageId) -> Option<(u32, u32)>;

struct P<'a> {
    boxes: &'a [LBox],
    cx: &'a Cx<'a>,
    image_size: ImageSize<'a>,
    controls: &'a dyn Fn(NodeId) -> Option<ControlId>,
    text: alloc::string::String,
    clips: Vec<Rect>,
    neg: Vec<Entry>,
    flow: Vec<Entry>,
    pos: Vec<Entry>,
    fixed: Vec<Entry>,
    hits: Vec<HitRect>,
    node_rects: Vec<Rect>,
    control_rects: Vec<(ControlId, Rect)>,
    /// Positioned boxes waiting: (z, order, box, offset, clip, opacity, fixed).
    deferred: Vec<(i32, u32, BoxId, (Au, Au), u16, f32, bool)>,
    order: u32,
}

#[derive(Clone, Copy, PartialEq)]
enum Layer {
    Neg,
    Flow,
    Pos,
    Fixed,
}

impl P<'_> {
    fn push(&mut self, layer: Layer, item: Item, clip: u16) {
        let e = Entry { item, clip };
        match layer {
            Layer::Neg => self.neg.push(e),
            Layer::Flow => self.flow.push(e),
            Layer::Pos => self.pos.push(e),
            Layer::Fixed => self.fixed.push(e),
        }
    }

    fn note_node(&mut self, node: NodeId, r: Rect) {
        if node != NONE && (node as usize) < self.node_rects.len() {
            let cur = self.node_rects[node as usize];
            self.node_rects[node as usize] = union(cur, r);
        }
    }

    fn rel_offset(&self, b: &LBox) -> (Au, Au) {
        if !matches!(b.style.position, Position::Relative | Position::Sticky) {
            return (0, 0);
        }
        let s = &b.style;
        let cbw = if b.parent != NO_BOX {
            self.boxes[b.parent as usize].content().2
        } else {
            self.cx.vw
        };
        let cbh = if b.parent != NO_BOX {
            self.boxes[b.parent as usize].content().3
        } else {
            self.cx.vh
        };
        let v = |i: usize, base: Au| match s.inset[i] {
            LPA::Auto => None,
            LPA::L(l) => Some(self.cx.lp(&l, base)),
        };
        let dx = match (v(3, cbw), v(1, cbw)) {
            (Some(l), _) => l,
            (None, Some(r)) => -r,
            _ => 0,
        };
        let dy = if s.position == Position::Sticky {
            0
        } else {
            match (v(0, cbh), v(2, cbh)) {
                (Some(t), _) => t,
                (None, Some(bt)) => -bt,
                _ => 0,
            }
        };
        (dx, dy)
    }

    fn clip_with(&mut self, clip: u16, r: Rect) -> u16 {
        let base = self.clips[clip as usize];
        let c = base.intersect(&r);
        if self.clips.len() >= u16::MAX as usize - 1 {
            return clip;
        }
        self.clips.push(c);
        (self.clips.len() - 1) as u16
    }

    /// Background and borders of a box's border box.
    fn background(&mut self, layer: Layer, b: &LBox, off: (Au, Au), clip: u16, op: f32) {
        let s = &b.style;
        let r = rect_au(b.x + off.0, b.y + off.1, b.w, b.h);
        let radius = px(self.cx.lp(&s.border_radius, b.w.min(b.h)))
            .min(r.w / 2)
            .min(r.h / 2)
            .max(0);
        if s.background_color >> 24 != 0 {
            let color = fade(s.background_color, op);
            if radius > 0 {
                self.push(
                    layer,
                    Item::RoundRect {
                        rect: r,
                        color,
                        radius,
                    },
                    clip,
                );
            } else {
                self.push(layer, Item::Rect { rect: r, color }, clip);
            }
        }
        if let Some(img) = s.bg_image {
            self.bg_image(layer, b, off, clip, img);
        }
        self.border(layer, b, r, clip, op, radius);
    }

    fn border(&mut self, layer: Layer, b: &LBox, r: Rect, clip: u16, op: f32, radius: i32) {
        let s = &b.style;
        if b.border.iter().all(|&w| w == 0) {
            return;
        }
        let widths = [
            px(b.border[0]),
            px(b.border[1]),
            px(b.border[2]),
            px(b.border[3]),
        ];
        let mut colors = [0u32; 4];
        let mut styles = s.border_style;
        for i in 0..4 {
            let mut c = fade(s.border_color[i], op);
            if styles[i] == BorderStyle::Shaded && (i == 0 || i == 3) {
                // groove/ridge/inset/outset, drawn inset-style: the top and left darker.
                c = shade(c, 0.6);
            }
            colors[i] = c;
        }
        let _ = &mut styles;
        self.push(
            layer,
            Item::Border {
                rect: r,
                widths,
                colors,
                styles,
                radius,
            },
            clip,
        );
    }

    fn bg_image(&mut self, layer: Layer, b: &LBox, off: (Au, Au), clip: u16, img: ImageId) {
        let s = &b.style;
        let Some((iw, ih)) = (self.image_size)(img) else {
            return;
        };
        if iw == 0 || ih == 0 {
            return;
        }
        let (px_, py_, pw, ph) = b.padding_box();
        let area = rect_au(b.x + off.0, b.y + off.1, b.w, b.h);
        let iw = self.cx.au(iw as f32);
        let ih = self.cx.au(ih as f32);
        let (tw, th) = match s.bg_size {
            BgSize::Auto => (iw, ih),
            BgSize::Cover | BgSize::Contain => {
                let sx = pw as f32 / iw as f32;
                let sy = ph as f32 / ih as f32;
                let k = if s.bg_size == BgSize::Cover {
                    sx.max(sy)
                } else {
                    sx.min(sy)
                };
                (
                    crate::fmath::round_i32(iw as f32 * k),
                    crate::fmath::round_i32(ih as f32 * k),
                )
            }
            BgSize::L(w, h) => {
                let w2 = match w {
                    LPA::L(l) => Some(self.cx.lp(&l, pw)),
                    LPA::Auto => None,
                };
                let h2 = match h {
                    LPA::L(l) => Some(self.cx.lp(&l, ph)),
                    LPA::Auto => None,
                };
                match (w2, h2) {
                    (Some(w), Some(h)) => (w, h),
                    (Some(w), None) => (w, (ih as i64 * w as i64 / iw.max(1) as i64) as Au),
                    (None, Some(h)) => ((iw as i64 * h as i64 / ih.max(1) as i64) as Au, h),
                    (None, None) => (iw, ih),
                }
            }
        };
        if tw <= 0 || th <= 0 {
            return;
        }
        let ox = px_ + self.cx.lp(&s.bg_position.0, pw - tw) + off.0;
        let oy = py_ + self.cx.lp(&s.bg_position.1, ph - th) + off.1;
        let tile = rect_au(ox, oy, tw, th);
        let (rx, ry) = match s.bg_repeat {
            BgRepeat::Repeat => (true, true),
            BgRepeat::RepeatX => (true, false),
            BgRepeat::RepeatY => (false, true),
            BgRepeat::NoRepeat => (false, false),
        };
        if !rx && !ry {
            let c = self.clip_with(clip, area);
            self.push(
                layer,
                Item::Image {
                    rect: tile,
                    image: img,
                },
                c,
            );
        } else {
            self.push(
                layer,
                Item::Tiled {
                    area,
                    tile,
                    image: img,
                    repeat_x: rx,
                    repeat_y: ry,
                },
                clip,
            );
        }
    }

    fn hit(&mut self, r: Rect, node: NodeId, layer: Layer) {
        self.hit_t(r, node, layer, false);
    }

    fn hit_t(&mut self, r: Rect, node: NodeId, layer: Layer, text: bool) {
        if node != NONE && !r.is_empty() {
            self.hits.push(HitRect {
                rect: r,
                node,
                fixed: layer == Layer::Fixed,
                text,
            });
        }
    }

    /// Paint box `b` and its subtree. `deferring`: positioned descendants go to the
    /// deferred list instead of being painted inline.
    fn paint(&mut self, id: BoxId, off: (Au, Au), clip: u16, op: f32, layer: Layer, depth: u32) {
        if depth > 400 {
            return;
        }
        let boxes = self.boxes;
        let b = &boxes[id as usize];
        let s: Rc<Style> = b.style.clone();
        let op = op * s.opacity;
        if op <= 0.0 {
            return;
        }
        let ro = self.rel_offset(b);
        let off = (off.0 + ro.0, off.1 + ro.1);
        let visible = s.visibility == Visibility::Visible;
        let r = rect_au(b.x + off.0, b.y + off.1, b.w, b.h);
        match &b.kind {
            Kind::Text(_) | Kind::Break | Kind::Wbr => return,
            Kind::Inline => {
                // Drawn through the IFC fragments; descend for atomics and floats.
                for &c in &b.children {
                    self.child(c, off, clip, op, layer, depth);
                }
                return;
            }
            Kind::Marker(t) => {
                if visible {
                    self.marker(layer, b, t, off, clip, op);
                }
                return;
            }
            _ => {}
        }
        self.note_node(b.node, r);
        if visible {
            self.background(layer, b, off, clip, op);
        }
        self.hit(r, b.node, layer);
        if let Kind::Replaced(rep) = &b.kind {
            let (cx_, cy_, cw, ch) = b.content();
            let cr = rect_au(cx_ + off.0, cy_ + off.1, cw, ch);
            if visible {
                match rep {
                    Replaced::Image(Some(img), _) => {
                        let c = self.clip_with(clip, cr);
                        self.push(
                            layer,
                            Item::Image {
                                rect: cr,
                                image: *img,
                            },
                            c,
                        );
                        if let Some(ctl) = (self.controls)(b.node) {
                            self.control_rects.push((ctl, cr));
                            self.push(
                                layer,
                                Item::Control {
                                    rect: cr,
                                    control: ctl,
                                },
                                clip,
                            );
                        }
                    }
                    Replaced::Control(ctl, _) => {
                        self.control_rects.push((*ctl, cr));
                        self.push(
                            layer,
                            Item::Control {
                                rect: cr,
                                control: *ctl,
                            },
                            clip,
                        );
                    }
                    _ => {}
                }
            }
            return;
        }
        // <button>: the control item first, then its content on top.
        if let Some(ctl) = (self.controls)(b.node) {
            let (px_, py_, pw, ph) = b.padding_box();
            let cr = rect_au(px_ + off.0, py_ + off.1, pw, ph);
            self.control_rects.push((ctl, cr));
            if visible {
                self.push(
                    layer,
                    Item::Control {
                        rect: cr,
                        control: ctl,
                    },
                    clip,
                );
            }
        }
        if b.marker != NO_BOX {
            self.paint(b.marker, off, clip, op, layer, depth + 1);
        }
        let clip = if s.overflow_x.clips() || s.overflow_y.clips() {
            let (px_, py_, pw, ph) = b.padding_box();
            let pr = rect_au(px_ + off.0, py_ + off.1, pw, ph);
            // overflow-x: hidden alone still clips both axes here (no scrolling boxes).
            self.clip_with(clip, pr)
        } else {
            clip
        };
        if let Some(ifc) = &b.ifc {
            // Inline boxes' backgrounds and borders.
            for f in &ifc.frags {
                if let Frag::Inline {
                    b: ib,
                    x,
                    y,
                    w,
                    h,
                    first,
                    last,
                } = *f
                {
                    let ibx = &boxes[ib as usize];
                    let ist = &ibx.style;
                    let fr = rect_au(x + off.0, y + off.1, w, h);
                    self.note_node(ibx.node, fr);
                    self.hit(fr, ibx.node, layer);
                    if ist.visibility != Visibility::Visible {
                        continue;
                    }
                    if ist.background_color >> 24 != 0 {
                        self.push(
                            layer,
                            Item::Rect {
                                rect: fr,
                                color: fade(ist.background_color, op),
                            },
                            clip,
                        );
                    }
                    if ist.border_width.iter().any(|&w| w > 0.0) {
                        let mut widths = [
                            px(self.cx.au(ist.border_width[0])),
                            px(self.cx.au(ist.border_width[1])),
                            px(self.cx.au(ist.border_width[2])),
                            px(self.cx.au(ist.border_width[3])),
                        ];
                        if !first {
                            widths[3] = 0;
                        }
                        if !last {
                            widths[1] = 0;
                        }
                        let colors = [
                            fade(ist.border_color[0], op),
                            fade(ist.border_color[1], op),
                            fade(ist.border_color[2], op),
                            fade(ist.border_color[3], op),
                        ];
                        self.push(
                            layer,
                            Item::Border {
                                rect: fr,
                                widths,
                                colors,
                                styles: ist.border_style,
                                radius: 0,
                            },
                            clip,
                        );
                    }
                }
            }
            // Floats and atomic inlines.
            for &c in &b.children {
                self.child(c, off, clip, op, layer, depth);
            }
            // Text.
            for f in &ifc.frags {
                if let Frag::Text {
                    start,
                    end,
                    styled,
                    font,
                    x,
                    baseline,
                    w,
                    node,
                } = *f
                {
                    let ts = &boxes[styled as usize].style;
                    let s = &ifc.text[start as usize..end as usize];
                    let run_rect = {
                        let m = self.cx.metrics(&font);
                        rect_au(
                            x + off.0,
                            baseline + off.1 - m.ascent * AU,
                            w,
                            (m.ascent + m.descent) * AU,
                        )
                    };
                    let hnode = if node != NONE {
                        self.cx.dom.parent(node)
                    } else {
                        boxes[styled as usize].node
                    };
                    self.hit_t(run_rect, hnode, layer, true);
                    self.note_node(hnode, run_rect);
                    if ts.visibility != Visibility::Visible {
                        continue;
                    }
                    let tstart = self.text.len() as u32;
                    self.text.push_str(s);
                    let color = fade(ts.color, op);
                    let deco = ts.deco_in_effect;
                    let run = TextRun {
                        x: px(x + off.0),
                        baseline: px(baseline + off.1),
                        width: px(w).max(0),
                        font,
                        color,
                        decoration: deco,
                        start: tstart,
                        len: s.len() as u32,
                    };
                    self.push(layer, Item::Text(run), clip);
                    if deco.0 != 0 {
                        self.decorations(layer, run, ts, op, clip);
                    }
                }
            }
        } else {
            for &c in &b.children {
                self.child(c, off, clip, op, layer, depth);
            }
        }
    }

    /// A child: painted now, or deferred if positioned.
    fn child(&mut self, c: BoxId, off: (Au, Au), clip: u16, op: f32, layer: Layer, depth: u32) {
        let cb = &self.boxes[c as usize];
        let s = &cb.style;
        if s.position == Position::Fixed && layer != Layer::Fixed {
            self.order += 1;
            self.deferred
                .push((s.z_index.unwrap_or(0), self.order, c, (0, 0), 0, op, true));
            return;
        }
        if s.is_positioned() && !matches!(cb.kind, Kind::Text(_)) {
            self.order += 1;
            let z = s.z_index.unwrap_or(0);
            self.deferred
                .push((z, self.order, c, off, clip, op, layer == Layer::Fixed));
            return;
        }
        self.paint(c, off, clip, op, layer, depth + 1);
    }

    fn marker(&mut self, layer: Layer, b: &LBox, t: &str, off: (Au, Au), clip: u16, op: f32) {
        let s = &b.style;
        let color = fade(s.color, op);
        if t.is_empty() {
            // A bullet: a square or disc about a third of an em, on the x-height.
            let f = self.cx.font(s);
            let m = self.cx.metrics(&f);
            let size = (s.font_size * self.cx.zoom * 0.36).max(3.0) as i32;
            let baseline = px(b.y + off.1 + m.ascent * AU);
            let x = px(b.x + off.0) + (px(b.w) - size) / 2 - px(b.w) / 6;
            let y = baseline - (m.ascent as f32 * 0.32) as i32 - size / 2 - 1;
            let kind = match s.list_style_type {
                ListStyleType::Circle => MarkerKind::Circle,
                ListStyleType::Square => MarkerKind::Square,
                _ => MarkerKind::Disc,
            };
            self.push(
                layer,
                Item::Marker {
                    rect: Rect::new(x, y, size, size),
                    kind,
                    color,
                },
                clip,
            );
        } else {
            let f = self.cx.font(s);
            let m = self.cx.metrics(&f);
            let tstart = self.text.len() as u32;
            self.text.push_str(t);
            let run = TextRun {
                x: px(b.x + off.0),
                baseline: px(b.y + off.1 + m.ascent * AU),
                width: px(b.w),
                font: f,
                color,
                decoration: Decoration::NONE,
                start: tstart,
                len: t.len() as u32,
            };
            self.push(layer, Item::Text(run), clip);
        }
    }

    /// Underline / overline / line-through as rectangles (CSS Text Decoration 3 §2).
    fn decorations(&mut self, layer: Layer, run: TextRun, s: &Style, op: f32, clip: u16) {
        let fs = run.font.size as i32;
        let th = (fs / 14).max(1);
        let color = fade(s.deco_color_in_effect, op);
        let m = self.cx.metrics(&run.font);
        let mut add = |y: i32| {
            self.push(
                layer,
                Item::Rect {
                    rect: Rect::new(run.x, y, run.width.max(1), th),
                    color,
                },
                clip,
            );
        };
        if run.decoration.has(Decoration::UNDERLINE) {
            add(run.baseline + (m.descent / 2).max(1));
        }
        if run.decoration.has(Decoration::OVERLINE) {
            add(run.baseline - m.ascent);
        }
        if run.decoration.has(Decoration::LINE_THROUGH) {
            add(run.baseline - m.ascent * 3 / 10 - th / 2);
        }
    }
}

fn shade(c: Color, k: f32) -> Color {
    let r = (((c >> 16) & 255) as f32 * k) as u32;
    let g = (((c >> 8) & 255) as f32 * k) as u32;
    let b = ((c & 255) as f32 * k) as u32;
    (c & 0xFF00_0000) | (r << 16) | (g << 8) | b
}

/// Paint a laid-out document.
pub fn paint(
    laid: &Laid,
    cx: &Cx,
    image_size: ImageSize,
    controls: &dyn Fn(NodeId) -> Option<ControlId>,
    nodes: usize,
    canvas: Color,
) -> Painted {
    let mut p = P {
        boxes: &laid.boxes,
        cx,
        image_size,
        controls,
        text: alloc::string::String::new(),
        clips: alloc::vec![Rect::new(
            i32::MIN / 4,
            i32::MIN / 4,
            i32::MAX / 2,
            i32::MAX / 2
        )],
        neg: Vec::new(),
        flow: Vec::new(),
        pos: Vec::new(),
        fixed: Vec::new(),
        hits: Vec::new(),
        node_rects: alloc::vec![Rect::default(); nodes],
        control_rects: Vec::new(),
        deferred: Vec::new(),
        order: 0,
    };
    if laid.root != NO_BOX {
        p.paint(laid.root, (0, 0), 0, 1.0, Layer::Flow, 0);
        // Positioned boxes, by z-index then tree order; their own positioned descendants are
        // deferred again and painted after them (z-index contexts are flattened).
        let mut rounds = 0;
        while !p.deferred.is_empty() && rounds < 64 {
            rounds += 1;
            let mut list = core::mem::take(&mut p.deferred);
            list.sort_by_key(|d| (d.0, d.1));
            for (z, _, b, off, clip, op, fixed) in list {
                let layer = if fixed {
                    Layer::Fixed
                } else if z < 0 {
                    Layer::Neg
                } else {
                    Layer::Pos
                };
                p.paint(b, off, clip, op, layer, 0);
            }
        }
    }
    let mut entries = p.neg;
    entries.extend(p.flow);
    entries.extend(p.pos);
    let width = px(laid.width);
    let height = px(laid.height);
    let mut dl = DisplayList {
        background: canvas,
        entries,
        fixed: p.fixed,
        clips: p.clips,
        text: p.text,
        width,
        height,
        bands: Vec::new(),
    };
    build_bands(&mut dl);
    Painted {
        dl,
        hits: p.hits,
        node_rects: p.node_rects,
        control_rects: p.control_rects,
    }
}

/// The culling index of [`DisplayList::visible`].
fn build_bands(dl: &mut DisplayList) {
    let nb = ((dl.height.max(1) + BAND - 1) / BAND).clamp(1, 1 << 16) as usize;
    let mut bands: Vec<Vec<u32>> = alloc::vec![Vec::new(); nb];
    for (i, e) in dl.entries.iter().enumerate() {
        let r = entry_rect(&e.item).intersect(&dl.clips[e.clip as usize]);
        if r.is_empty() {
            continue;
        }
        let b0 = (r.y.max(0) / BAND) as usize;
        let b1 = ((r.bottom() - 1).max(0) / BAND) as usize;
        for b in bands
            .iter_mut()
            .take(b1.min(nb - 1) + 1)
            .skip(b0.min(nb - 1))
        {
            b.push(i as u32);
        }
    }
    dl.bands = bands;
}
