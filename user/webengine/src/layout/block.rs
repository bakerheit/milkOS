//! Block formatting contexts: block-level boxes stacked vertically with collapsing margins
//! (CSS 2.1 §8.3.1), widths (§10.3.3) and heights (§10.6.3), floats (§9.5, placed by
//! [`Floats`]), clearance (§9.5.2), and the sizing of atomic boxes: replaced elements
//! (§10.3.2, §10.6.2, the §10.4 min/max table), inline-blocks and floats (shrink-to-fit,
//! §10.3.5).
//!
//! Margin collapsing uses a "chain" walk: before a block is placed, the top margins that
//! collapse with it (its own, its first child's, ... while nothing separates them) are
//! combined with the pending bottom margin of the previous sibling, so every box is placed
//! once, at its final position.

use alloc::vec::Vec;

use super::*;
use crate::api::ControlKind;
use crate::atoms::tag;

// ---- floats -------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct FloatBox {
    x: Au,
    y: Au,
    w: Au,
    h: Au,
    left: bool,
}

/// The floats of one block formatting context (margin boxes, document coordinates).
#[derive(Default)]
pub struct Floats {
    list: Vec<FloatBox>,
}

impl Floats {
    pub fn new() -> Floats {
        Floats { list: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The free interval [l, r) between the floats over the band y..y+h, within [l0, r0).
    pub fn band(&self, y: Au, h: Au, l0: Au, r0: Au) -> (Au, Au) {
        let mut l = l0;
        let mut r = r0;
        let h = h.max(1);
        for f in &self.list {
            if f.y < y + h && f.y + f.h > y && f.h > 0 {
                if f.left {
                    l = l.max(f.x + f.w);
                } else {
                    r = r.min(f.x);
                }
            }
        }
        (l, r.max(l))
    }

    /// The lowest float bottom above which `y` is (the next place where the band changes).
    pub fn next_edge_below(&self, y: Au) -> Au {
        let mut best = Au::MAX;
        for f in &self.list {
            let b = f.y + f.h;
            if b > y && b < best {
                best = b;
            }
        }
        if best == Au::MAX { y } else { best }
    }

    /// CSS 2.1 §9.5.1: the highest position at or below `y` where a `w`×`h` float fits.
    pub fn place(&self, w: Au, h: Au, left: bool, y: Au, l0: Au, r0: Au) -> (Au, Au) {
        let mut y = y;
        // A float may not be higher than an earlier float (rule 5).
        if let Some(last) = self.list.last() {
            y = y.max(last.y);
        }
        for _ in 0..1000 {
            let (l, r) = self.band(y, h, l0, r0);
            if r - l >= w || (l == l0 && r == r0) {
                return (if left { l } else { r - w }, y);
            }
            let ny = self.next_edge_below(y);
            if ny <= y {
                return (if left { l } else { r - w }, y);
            }
            y = ny;
        }
        (if left { l0 } else { r0 - w }, y)
    }

    pub fn add(&mut self, x: Au, y: Au, w: Au, h: Au, left: bool) {
        if self.list.len() < 10_000 {
            self.list.push(FloatBox { x, y, w, h, left });
        }
    }

    /// Where a box with `clear` must start (CSS 2.1 §9.5.2).
    pub fn clear_y(&self, c: Clear, y: Au) -> Au {
        let mut out = y;
        for f in &self.list {
            let hit = match c {
                Clear::None => false,
                Clear::Left => f.left,
                Clear::Right => !f.left,
                Clear::Both => true,
            };
            if hit {
                out = out.max(f.y + f.h);
            }
        }
        out
    }

    pub fn bottom(&self) -> Au {
        self.list.iter().map(|f| f.y + f.h).max().unwrap_or(Au::MIN)
    }
}

// ---- margins ------------------------------------------------------------------------------

/// Adjoining margins collapsing: the largest positive and the most negative (CSS 2.1 §8.3.1).
#[derive(Clone, Copy, Debug, Default)]
pub struct Margins {
    pos: Au,
    neg: Au,
}

impl Margins {
    pub fn of(m: Au) -> Margins {
        let mut x = Margins::default();
        x.add(m);
        x
    }
    pub fn add(&mut self, m: Au) {
        if m > 0 {
            self.pos = self.pos.max(m);
        } else {
            self.neg = self.neg.min(m);
        }
    }
    pub fn join(&mut self, o: Margins) {
        self.pos = self.pos.max(o.pos);
        self.neg = self.neg.min(o.neg);
    }
    pub fn value(&self) -> Au {
        self.pos + self.neg
    }
}

// ---- edges and sizes ----------------------------------------------------------------------

/// Margins (None = auto), borders and padding of a box against a containing block width.
pub fn edges(cx: &Cx, s: &Style, cb_w: Au) -> ([Option<Au>; 4], [Au; 4], [Au; 4]) {
    let mut m = [None; 4];
    let mut b = [0; 4];
    let mut p = [0; 4];
    for i in 0..4 {
        m[i] = match s.margin[i] {
            LPA::Auto => None,
            LPA::L(l) => Some(cx.lp(&l, cb_w)),
        };
        b[i] = cx.au(s.border_width[i]);
        // Borders thinner than a device pixel still show (CSS Backgrounds 3 §4.3: round down,
        // but at least 1 device px).
        if s.border_width[i] > 0.0 && b[i] < AU {
            b[i] = AU;
        }
        b[i] = b[i] / AU * AU;
        p[i] = cx.lp(&s.padding[i], cb_w).max(0);
    }
    (m, b, p)
}

/// A width-ish size against a reference, as a content width (box-sizing applied). None for
/// auto / intrinsic keywords (the callers handle those).
pub fn size_px(cx: &Cx, s: &Style, v: &Size, base: Option<Au>, bp: Au) -> Option<Au> {
    match v {
        Size::L(l) => {
            if l.has_pct() && base.is_none() {
                return None;
            }
            let x = cx.lp(l, base.unwrap_or(0));
            Some(if s.box_sizing == BoxSizing::BorderBox {
                (x - bp).max(0)
            } else {
                x.max(0)
            })
        }
        _ => None,
    }
}

fn max_px(cx: &Cx, s: &Style, v: &Option<LP>, base: Option<Au>, bp: Au) -> Option<Au> {
    let l = v.as_ref()?;
    if l.has_pct() && base.is_none() {
        return None;
    }
    let x = cx.lp(l, base.unwrap_or(0));
    Some(if s.box_sizing == BoxSizing::BorderBox {
        (x - bp).max(0)
    } else {
        x.max(0)
    })
}

/// CSS 2.1 §10.4: clamp a content width between min-width and max-width.
pub fn clamp_width(cx: &Cx, s: &Style, w: Au, cb_w: Au, bp: Au, boxes: &[LBox], b: BoxId) -> Au {
    let mut w = w;
    if let Some(mx) = max_px(cx, s, &s.max_width, Some(cb_w), bp) {
        w = w.min(mx);
    }
    let mn = match s.min_width {
        Size::Auto => 0,
        Size::MinContent | Size::FitContent => intrinsic::content_widths(boxes, cx, b).0,
        Size::MaxContent => intrinsic::content_widths(boxes, cx, b).1,
        _ => size_px(cx, s, &s.min_width, Some(cb_w), bp).unwrap_or(0),
    };
    w.max(mn)
}

pub fn clamp_height(cx: &Cx, s: &Style, h: Au, cb_h: Option<Au>, bp: Au) -> Au {
    let mut h = h;
    if let Some(mx) = max_px(cx, s, &s.max_height, cb_h, bp) {
        h = h.min(mx);
    }
    if let Some(mn) = size_px(cx, s, &s.min_height, cb_h, bp) {
        h = h.max(mn);
    }
    h
}

/// The content width a box asks for: a length, an intrinsic keyword, or None for auto.
pub fn specified_width(boxes: &[LBox], cx: &Cx, b: BoxId, cb_w: Au, bp: Au) -> Option<Au> {
    let s = &boxes[b as usize].style;
    match s.width {
        Size::Auto | Size::Content => None,
        Size::MinContent => Some(intrinsic::content_widths(boxes, cx, b).0),
        Size::MaxContent => Some(intrinsic::content_widths(boxes, cx, b).1),
        Size::FitContent => {
            let (mn, mx) = intrinsic::content_widths(boxes, cx, b);
            Some(mx.min(cb_w - bp).max(mn))
        }
        _ => size_px(cx, s, &s.width, Some(cb_w), bp),
    }
}

// ---- moving boxes -------------------------------------------------------------------------

/// Move a laid-out box and everything in it.
pub fn translate(boxes: &mut [LBox], b: BoxId, dx: Au, dy: Au) {
    if dx == 0 && dy == 0 {
        return;
    }
    let mut stack = alloc::vec![b];
    while let Some(id) = stack.pop() {
        let bx = &mut boxes[id as usize];
        bx.x += dx;
        bx.y += dy;
        bx.static_pos.0 += dx;
        bx.static_pos.1 += dy;
        if let Some(fb) = bx.first_baseline.as_mut() {
            *fb += dy;
        }
        if let Some(lb) = bx.last_baseline.as_mut() {
            *lb += dy;
        }
        if let Some(ifc) = bx.ifc.as_mut() {
            for l in ifc.lines.iter_mut() {
                l.y += dy;
                l.baseline += dy;
            }
            for f in ifc.frags.iter_mut() {
                match f {
                    Frag::Text { x, baseline, .. } => {
                        *x += dx;
                        *baseline += dy;
                    }
                    Frag::Inline { x, y, .. } => {
                        *x += dx;
                        *y += dy;
                    }
                }
            }
        }
        if bx.marker != NO_BOX {
            stack.push(bx.marker);
        }
        stack.extend_from_slice(&boxes[id as usize].children);
    }
}

// ---- replaced elements --------------------------------------------------------------------

/// Intrinsic size (CSS px) and ratio of a replaced box, if it has them.
fn intrinsic_replaced(cx: &Cx, b: &LBox) -> (Option<f32>, Option<f32>, Option<f32>) {
    match &b.kind {
        Kind::Replaced(Replaced::Image(_, Some((w, h)))) => {
            let (w, h) = (*w as f32, *h as f32);
            (Some(w), Some(h), if h > 0.0 { Some(w / h) } else { None })
        }
        Kind::Replaced(Replaced::Image(_, None)) => {
            // Not loaded: the width/height attributes give the ratio (HTML maps them to
            // aspect-ratio), nothing else.
            let e = cx.dom.element(b.node);
            let a = |n: &str| {
                e.and_then(|e| e.attr(n))
                    .and_then(|v| v.trim().parse::<f32>().ok())
            };
            match (a("width"), a("height")) {
                (Some(w), Some(h)) if h > 0.0 => (None, None, Some(w / h)),
                _ => (Some(0.0), Some(0.0), None),
            }
        }
        Kind::Replaced(Replaced::Empty(w, h)) => (
            Some(*w),
            Some(*h),
            if *h > 0.0 { Some(*w / *h) } else { None },
        ),
        Kind::Replaced(Replaced::Control(_, k)) => {
            let (w, h) = control_size(cx, b, *k);
            (Some(w), Some(h), None)
        }
        _ => (None, None, None),
    }
}

/// The default content size of a form control (CSS px), from its attributes and font.
fn control_size(cx: &Cx, b: &LBox, k: ControlKind) -> (f32, f32) {
    let s = &b.style;
    let fs = s.font_size;
    let e = cx.dom.element(b.node);
    let attr_u = |n: &str| {
        e.and_then(|e| e.attr(n))
            .and_then(|v| v.trim().parse::<u32>().ok())
    };
    let lh = match s.line_height {
        LineHeight::Normal => cx.metrics(&cx.font(s)).line_height as f32 / cx.zoom,
        LineHeight::Number(n) => n * fs,
        LineHeight::Px(p) => p,
    };
    let text_w = |t: &str| cx.text_width(&cx.font(s), t) as f32 / AU as f32 / cx.zoom;
    match k {
        ControlKind::Text | ControlKind::Password => {
            let size = attr_u("size").filter(|&n| n > 0).unwrap_or(20).min(1000);
            (size as f32 * fs * 0.55, lh)
        }
        ControlKind::TextArea => {
            let cols = attr_u("cols").filter(|&n| n > 0).unwrap_or(20).min(1000);
            let rows = attr_u("rows").filter(|&n| n > 0).unwrap_or(2).min(1000);
            (cols as f32 * fs * 0.55, rows as f32 * lh)
        }
        ControlKind::Checkbox | ControlKind::Radio => (13.0, 13.0),
        ControlKind::Submit
        | ControlKind::Reset
        | ControlKind::Button
        | ControlKind::ImageSubmit => {
            let label = e
                .and_then(|e| e.attr("value"))
                .map(alloc::string::String::from)
                .unwrap_or_else(|| {
                    alloc::string::String::from(match k {
                        ControlKind::Reset => "Reset",
                        ControlKind::Button => "",
                        _ => "Submit",
                    })
                });
            (text_w(&label), lh)
        }
        ControlKind::File => (text_w("Choose File") + text_w("No file chosen") + 8.0, lh),
        ControlKind::Select => {
            let mut w: f32 = 0.0;
            let mut n_opts = 0u32;
            if b.node != crate::dom::NONE {
                let dom = cx.dom;
                let mut c = dom.next_in(b.node, b.node);
                while c != crate::dom::NONE {
                    if dom.is_html(c, tag::OPTION) {
                        n_opts += 1;
                        let label = dom
                            .element(c)
                            .and_then(|o| o.attr("label").map(alloc::string::String::from))
                            .unwrap_or_else(|| dom.text_content(c));
                        let t: alloc::string::String =
                            label.split_whitespace().collect::<Vec<_>>().join(" ");
                        w = w.max(text_w(&t));
                        if n_opts > 500 {
                            break;
                        }
                    }
                    c = dom.next_in(c, b.node);
                }
            }
            let multiple = e.is_some_and(|e| e.has_attr("multiple"));
            let rows = attr_u("size")
                .unwrap_or(if multiple { 4 } else { 1 })
                .clamp(1, 100);
            (w + 20.0, rows as f32 * lh)
        }
    }
}

/// Size a replaced box (CSS 2.1 §10.3.2 / §10.6.2 and the §10.4 constraint table), content
/// box in Au.
fn replaced_size(
    boxes: &[LBox],
    cx: &Cx,
    b: BoxId,
    cb_w: Au,
    cb_h: Option<Au>,
    bp_w: Au,
    bp_h: Au,
) -> (Au, Au) {
    let bx = &boxes[b as usize];
    let s = &bx.style;
    let (iw, ih, ratio) = intrinsic_replaced(cx, bx);
    let ratio = s.aspect_ratio.or(ratio);
    let sw = size_px(cx, s, &s.width, Some(cb_w), bp_w);
    let sh = size_px(cx, s, &s.height, cb_h, bp_h);
    let iw = iw.map(|v| cx.au(v));
    let ih = ih.map(|v| cx.au(v));
    let by_ratio_w = |h: Au| ratio.map(|r| fmath::round_i32(h as f32 * r));
    let by_ratio_h = |w: Au| {
        ratio.map(|r| {
            if r > 0.0 {
                fmath::round_i32(w as f32 / r)
            } else {
                0
            }
        })
    };
    let (mut w, mut h) = match (sw, sh) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, by_ratio_h(w).or(ih).unwrap_or(cx.au(150.0))),
        (None, Some(h)) => (by_ratio_w(h).or(iw).unwrap_or(cx.au(300.0)), h),
        (None, None) => match (iw, ih) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, by_ratio_h(w).unwrap_or(cx.au(150.0))),
            (None, Some(h)) => (by_ratio_w(h).unwrap_or(cx.au(300.0)), h),
            (None, None) => {
                // Only a ratio (an image not loaded yet): fill the width that's there.
                let w = (cb_w - bp_w).max(0);
                (w, by_ratio_h(w).unwrap_or(0))
            }
        },
    };
    // min/max with the ratio kept when one side was derived.
    let w0 = w;
    w = clamp_width(cx, s, w, cb_w, bp_w, boxes, b);
    if w != w0 && sh.is_none() && ratio.is_some() {
        h = by_ratio_h(w).unwrap_or(h);
    }
    let h0 = h;
    h = clamp_height(cx, s, h, cb_h, bp_h);
    if h != h0 && sw.is_none() && ratio.is_some() {
        w = by_ratio_w(h).unwrap_or(w);
    }
    (w.max(0), h.max(0))
}

// ---- atomic boxes (inline-blocks, replaced, floats, abs) ----------------------------------

/// Size and lay out an atomic box (inline-block, inline-table/flex, replaced, float, marker)
/// with its margin box at (0, 0), in a containing block `cb_w` wide. The caller moves it.
pub fn layout_atomic(boxes: &mut Vec<LBox>, cx: &Cx, b: BoxId, cb_w: Au) {
    if boxes[b as usize].laid_out {
        return;
    }
    boxes[b as usize].laid_out = true;
    let s = boxes[b as usize].style.clone();
    let (m, bo, p) = edges(cx, &s, cb_w);
    let mg = [
        m[0].unwrap_or(0),
        m[1].unwrap_or(0),
        m[2].unwrap_or(0),
        m[3].unwrap_or(0),
    ];
    let bp_w = bo[1] + bo[3] + p[1] + p[3];
    let bp_h = bo[0] + bo[2] + p[0] + p[2];
    {
        let bx = &mut boxes[b as usize];
        bx.margin = mg;
        bx.border = bo;
        bx.padding = p;
    }
    match boxes[b as usize].kind.clone() {
        Kind::Marker(t) => {
            let f = cx.font(&s);
            let fm = cx.metrics(&f);
            let w = if t.is_empty() {
                cx.au(s.font_size * 0.9)
            } else {
                cx.text_width(&f, &t)
            };
            let bx = &mut boxes[b as usize];
            bx.margin = [0; 4];
            bx.border = [0; 4];
            bx.padding = [0; 4];
            bx.x = 0;
            bx.y = 0;
            bx.w = w;
            bx.h = (fm.ascent + fm.descent) * AU;
            bx.first_baseline = Some(fm.ascent * AU);
            bx.last_baseline = bx.first_baseline;
        }
        Kind::Replaced(_) => {
            let (w, h) = replaced_size(boxes, cx, b, cb_w, None, bp_w, bp_h);
            let bx = &mut boxes[b as usize];
            bx.x = mg[3];
            bx.y = mg[0];
            bx.w = w + bp_w;
            bx.h = h + bp_h;
        }
        _ => {
            // Shrink-to-fit (CSS 2.1 §10.3.5) unless the width is given.
            let avail = (cb_w - mg[1] - mg[3] - bp_w).max(0);
            let w = match specified_width(boxes, cx, b, cb_w, bp_w) {
                Some(w) => w,
                None => {
                    let (mn, mx) = intrinsic::content_widths(boxes, cx, b);
                    mx.min(avail).max(mn)
                }
            };
            let w = clamp_width(cx, &s, w, cb_w, bp_w, boxes, b);
            {
                let bx = &mut boxes[b as usize];
                bx.x = mg[3];
                bx.y = mg[0];
                bx.w = w + bp_w;
            }
            let cb_h = None;
            layout_insides(boxes, cx, b, cb_h, true);
        }
    }
}

/// Lay out the inside of `b` (its x, y, w and edges are set): children by kind, then the
/// height (auto: from the content; floats included if it is a BFC root).
pub fn layout_insides(
    boxes: &mut Vec<LBox>,
    cx: &Cx,
    b: BoxId,
    cb_h: Option<Au>,
    bfc_root: bool,
) -> Margins {
    layout_insides_h(boxes, cx, b, cb_h, bfc_root, None)
}

/// [`layout_insides`] with the content height forced (flex items, stretched boxes).
pub fn layout_insides_h(
    boxes: &mut Vec<LBox>,
    cx: &Cx,
    b: BoxId,
    cb_h: Option<Au>,
    bfc_root: bool,
    forced_h: Option<Au>,
) -> Margins {
    let s = boxes[b as usize].style.clone();
    let (bo, p) = (boxes[b as usize].border, boxes[b as usize].padding);
    let bp_h = bo[0] + bo[2] + p[0] + p[2];
    let spec_h = forced_h.or_else(|| size_px(cx, &s, &s.height, cb_h, bp_h));
    match boxes[b as usize].kind {
        Kind::Table => {
            table::layout_table(boxes, cx, b, spec_h);
            return Margins::default();
        }
        Kind::Flex => {
            flex::layout_flex(boxes, cx, b, spec_h, cb_h);
            return Margins::default();
        }
        _ => {}
    }
    let mut own = Floats::new();
    let mut floats_tmp = Floats::new();
    let floats: &mut Floats = if bfc_root { &mut own } else { &mut floats_tmp };
    let (out, _) = layout_block_children(boxes, cx, b, spec_h, floats, false, bfc_root);
    if let Some(h) = forced_h {
        boxes[b as usize].h = h + bp_h;
    }
    out
}

/// Clear the "laid out" marks of a subtree (before laying it out again at another size).
pub fn reset_subtree(boxes: &mut [LBox], b: BoxId) {
    let mut stack = alloc::vec![b];
    while let Some(id) = stack.pop() {
        boxes[id as usize].laid_out = false;
        stack.extend_from_slice(&boxes[id as usize].children);
        if boxes[id as usize].marker != NO_BOX {
            stack.push(boxes[id as usize].marker);
        }
    }
}

/// Lay out `b` with a given content width (and optionally content height), its margin box at
/// (0, 0), as a formatting-context root (flex items, absolutely positioned boxes).
pub fn layout_sized(
    boxes: &mut Vec<LBox>,
    cx: &Cx,
    b: BoxId,
    cb_w: Au,
    cw: Au,
    ch: Option<Au>,
    cb_h: Option<Au>,
) {
    reset_subtree(boxes, b);
    boxes[b as usize].laid_out = true;
    let s = boxes[b as usize].style.clone();
    let (m, bo, p) = edges(cx, &s, cb_w);
    let mg = [
        m[0].unwrap_or(0),
        m[1].unwrap_or(0),
        m[2].unwrap_or(0),
        m[3].unwrap_or(0),
    ];
    let bp_w = bo[1] + bo[3] + p[1] + p[3];
    let bp_h = bo[0] + bo[2] + p[0] + p[2];
    {
        let bx = &mut boxes[b as usize];
        bx.margin = mg;
        bx.border = bo;
        bx.padding = p;
        bx.x = mg[3];
        bx.y = mg[0];
        bx.w = cw.max(0) + bp_w;
    }
    if matches!(boxes[b as usize].kind, Kind::Replaced(_)) {
        let h = match ch {
            Some(h) => h,
            None => {
                let (iw, ih, ratio) = intrinsic_replaced(cx, &boxes[b as usize]);
                let ratio = s.aspect_ratio.or(ratio);
                match (ratio, ih, iw) {
                    (Some(r), _, _) if r > 0.0 => fmath::round_i32(cw as f32 / r),
                    (_, Some(h), _) => cx.au(h),
                    _ => 0,
                }
            }
        };
        boxes[b as usize].h = h.max(0) + bp_h;
        return;
    }
    if matches!(boxes[b as usize].kind, Kind::Marker(_)) {
        layout_atomic(boxes, cx, b, cb_w);
        return;
    }
    layout_insides_h(boxes, cx, b, cb_h, true, ch);
}

/// Place float `f` (already sized by [`layout_atomic`]) in the containing block
/// (`cb_x`, `cb_w`) at or below `y`, and record it.
pub fn place_float(
    boxes: &mut [LBox],
    cx: &Cx,
    f: BoxId,
    cb_x: Au,
    cb_w: Au,
    y: Au,
    floats: &mut Floats,
) {
    let _ = cx;
    let bx = &boxes[f as usize];
    let left = bx.style.float != Float::Right;
    let mw = bx.margin_box_w();
    let mh = bx.margin_box_h();
    let y = floats.clear_y(bx.style.clear, y);
    let (fx, fy) = floats.place(mw, mh, left, y, cb_x, cb_x + cb_w);
    let (ox, oy) = (bx.x - bx.margin[3], bx.y - bx.margin[0]);
    translate(boxes, f, fx - ox, fy - oy);
    floats.add(fx, fy, mw, mh, left);
}

// ---- block flow ---------------------------------------------------------------------------

/// Whether a block's top margin adjoins its first child's (CSS 2.1 §8.3.1).
fn top_collapses(boxes: &[LBox], cx: &Cx, b: BoxId) -> bool {
    let bx = &boxes[b as usize];
    let s = &bx.style;
    bx.parent != NO_BOX
        && matches!(bx.kind, Kind::Block)
        && !bx.inline_content
        && !s.establishes_bfc()
        && s.border_width[0] == 0.0
        && cx.lp(&s.padding[0], 0) == 0
        && s.padding[0].pct == 0.0
}

fn bottom_collapses(boxes: &[LBox], cx: &Cx, b: BoxId) -> bool {
    let bx = &boxes[b as usize];
    let s = &bx.style;
    bx.parent != NO_BOX
        && matches!(bx.kind, Kind::Block)
        && !bx.inline_content
        && !s.establishes_bfc()
        && s.border_width[2] == 0.0
        && cx.lp(&s.padding[2], 0) == 0
        && s.padding[2].pct == 0.0
        && matches!(s.height, Size::Auto)
        && matches!(s.min_height, Size::Auto)
}

/// The top margins that collapse together starting at `b` (its own and its first in-flow
/// children's, down the chain).
fn top_chain(boxes: &[LBox], cx: &Cx, b: BoxId, cb_w: Au, depth: u32) -> Margins {
    let s = &boxes[b as usize].style;
    let mt = match s.margin[0] {
        LPA::L(l) => cx.lp(&l, cb_w),
        LPA::Auto => 0,
    };
    let mut m = Margins::of(mt);
    if depth < 64 && top_collapses(boxes, cx, b) {
        let w = content_width_estimate(boxes, cx, b, cb_w);
        if let Some(&fc) = boxes[b as usize]
            .children
            .iter()
            .find(|&&c| !boxes[c as usize].out_of_flow)
        {
            if boxes[fc as usize].style.clear == Clear::None {
                m.join(top_chain(boxes, cx, fc, w, depth + 1));
            }
        }
    }
    m
}

/// A block's content width as [`layout_block`] will compute it (for percentages in margins).
fn content_width_estimate(boxes: &[LBox], cx: &Cx, b: BoxId, cb_w: Au) -> Au {
    let s = &boxes[b as usize].style;
    let (m, bo, p) = edges(cx, s, cb_w);
    let bp = bo[1] + bo[3] + p[1] + p[3];
    match specified_width(boxes, cx, b, cb_w, bp) {
        Some(w) => w,
        None => (cb_w - m[1].unwrap_or(0) - m[3].unwrap_or(0) - bp).max(0),
    }
}

/// Lay out the in-flow children of block container `b` (x, y, w, edges set). Sets its
/// height. Returns the bottom margin that collapses out of it (CSS 2.1 §8.3.1) and whether it
/// is "empty" for margin collapsing.
pub fn layout_block_children(
    boxes: &mut Vec<LBox>,
    cx: &Cx,
    b: BoxId,
    spec_h: Option<Au>,
    floats: &mut Floats,
    skip_first_chain: bool,
    bfc_root: bool,
) -> (Margins, bool) {
    let s = boxes[b as usize].style.clone();
    let (cxx, cyy, cw, _) = boxes[b as usize].content();
    let cb_h_children = spec_h;
    let mut cursor = cyy;
    let mut pending = Margins::default();
    let mut content_seen = false;
    let mut first = true;
    let is_ifc = boxes[b as usize].inline_content;
    let mut height;
    let mut outgoing = Margins::default();
    let collapses_bottom = bottom_collapses(boxes, cx, b) && !bfc_root;
    if is_ifc {
        let h = inline::layout_ifc(boxes, cx, b, cxx, cyy, cw, floats);
        height = h;
        content_seen = h > 0;
    } else {
        let kids = boxes[b as usize].children.clone();
        for c in kids {
            cx.work.set(cx.work.get() + 1);
            if boxes[c as usize].out_of_flow {
                if boxes[c as usize].style.float != Float::None {
                    layout_atomic(boxes, cx, c, cw);
                    let y = cursor + pending.value();
                    place_float(boxes, cx, c, cxx, cw, y, floats);
                } else {
                    boxes[c as usize].static_pos = (cxx, cursor + pending.value());
                }
                continue;
            }
            // The margins that collapse into this child's top.
            let chain = if first && skip_first_chain {
                Margins::default()
            } else {
                top_chain(boxes, cx, c, cw, 0)
            };
            let mut m = pending;
            m.join(chain);
            let mut y = cursor + m.value();
            let clear = boxes[c as usize].style.clear;
            if clear != Clear::None {
                let cy2 = floats.clear_y(clear, cursor);
                if cy2 > y {
                    y = cy2;
                }
            }
            let (out, empty) = layout_block(boxes, cx, c, cxx, cw, cb_h_children, y, floats, true);
            if empty && clear == Clear::None {
                // Its margins collapse through it (CSS 2.1 §8.3.1, "collapsing through").
                pending = m;
                pending.join(out);
            } else {
                cursor = boxes[c as usize].y + boxes[c as usize].h;
                pending = out;
                content_seen = true;
            }
            first = false;
        }
        if collapses_bottom {
            height = cursor - cyy;
            outgoing = pending;
        } else {
            height = cursor + pending.value() - cyy;
        }
    }
    if bfc_root && !floats.is_empty() {
        // CSS 2.1 §10.6.7: a BFC root's auto height includes its floats.
        let fb = floats.bottom();
        if fb > cyy + height {
            height = fb - cyy;
        }
    }
    let bo = boxes[b as usize].border;
    let p = boxes[b as usize].padding;
    let bp_h = bo[0] + bo[2] + p[0] + p[2];
    let h = match spec_h {
        Some(h) => h,
        None => height.max(0),
    };
    let h = clamp_height(cx, &s, h, cb_h_children, bp_h);
    boxes[b as usize].h = h + bp_h;
    // Last baseline of the block (for inline-blocks and tables): its last line, or the last
    // in-flow child that has one.
    if !is_ifc {
        let mut fb = None;
        let mut lb = None;
        for &c in &boxes[b as usize].children {
            let cb = &boxes[c as usize];
            if cb.out_of_flow {
                continue;
            }
            if fb.is_none() {
                fb = cb.first_baseline;
            }
            if cb.last_baseline.is_some() {
                lb = cb.last_baseline;
            }
        }
        boxes[b as usize].first_baseline = fb;
        boxes[b as usize].last_baseline = lb;
    }
    let empty = !content_seen && h == 0 && bp_h == 0;
    (outgoing, empty)
}

/// Lay out in-flow block-level box `b` with its border-box top at `y` in the containing
/// block (`cb_x`, `cb_w`, `cb_h`). Its top margin has already been applied (through the chain
/// walk) when `top_done`. Returns its outgoing bottom margin and whether it is empty.
#[allow(clippy::too_many_arguments)]
pub fn layout_block(
    boxes: &mut Vec<LBox>,
    cx: &Cx,
    b: BoxId,
    cb_x: Au,
    cb_w: Au,
    cb_h: Option<Au>,
    y: Au,
    floats: &mut Floats,
    top_done: bool,
) -> (Margins, bool) {
    let _ = top_done;
    let s = boxes[b as usize].style.clone();
    boxes[b as usize].laid_out = true;
    let (m, bo, p) = edges(cx, &s, cb_w);
    let bp_w = bo[1] + bo[3] + p[1] + p[3];
    let bp_h = bo[0] + bo[2] + p[0] + p[2];
    let is_replaced = matches!(boxes[b as usize].kind, Kind::Replaced(_));
    let bfc_root = s.establishes_bfc()
        || matches!(boxes[b as usize].kind, Kind::Table | Kind::Flex)
        || is_replaced
        || boxes[b as usize].parent == NO_BOX;
    // CSS 2.1 §9.5: a BFC root (or table / replaced box) doesn't overlap floats: it goes in the
    // space beside them.
    let (mut lx, mut rx) = (cb_x, cb_x + cb_w);
    if bfc_root && !floats.is_empty() {
        let (l, r) = floats.band(y, AU, cb_x, cb_x + cb_w);
        lx = l;
        rx = r;
    }
    let avail = rx - lx;
    // Width (CSS 2.1 §10.3.3 for blocks, §10.3.4 replaced, tables shrink to fit).
    let mut ml = m[3];
    let mut mr = m[1];
    let w = if is_replaced {
        replaced_size(boxes, cx, b, cb_w, cb_h, bp_w, bp_h).0
    } else {
        match specified_width(boxes, cx, b, cb_w, bp_w) {
            Some(w) => clamp_width(cx, &s, w, cb_w, bp_w, boxes, b),
            None => {
                if matches!(boxes[b as usize].kind, Kind::Table) {
                    // Auto table width: shrink-to-fit (CSS 2.1 §17.5.2.2).
                    let (mn, mx) = intrinsic::content_widths(boxes, cx, b);
                    let a = (avail - ml.unwrap_or(0) - mr.unwrap_or(0) - bp_w).max(0);
                    clamp_width(cx, &s, mx.min(a).max(mn), cb_w, bp_w, boxes, b)
                } else {
                    let w = (avail - ml.unwrap_or(0) - mr.unwrap_or(0) - bp_w).max(0);
                    let cw = clamp_width(cx, &s, w, cb_w, bp_w, boxes, b);
                    if cw != w {
                        // max-width kicked in: auto margins now center it.
                        cw
                    } else {
                        ml = Some(ml.unwrap_or(0));
                        mr = Some(mr.unwrap_or(0));
                        w
                    }
                }
            }
        }
    };
    // Auto margins take up the rest (centering when both are auto).
    let rest = avail - w - bp_w;
    let (ml, mr) = match (ml, mr) {
        (None, None) => {
            let h = (rest / 2).max(0);
            (h, rest - h)
        }
        (None, Some(r)) => ((rest - r).max(0), r),
        (Some(l), None) => (l, rest - l),
        (Some(l), Some(r)) => (l, r),
    };
    let mt = m[0].unwrap_or(0);
    let mb = m[2].unwrap_or(0);
    {
        let bx = &mut boxes[b as usize];
        bx.margin = [mt, mr, mb, ml];
        bx.border = bo;
        bx.padding = p;
        bx.x = lx + ml;
        bx.y = y;
        bx.w = w + bp_w;
        bx.h = bp_h;
    }
    // Lists: the outside marker sits left of the first line.
    let (out, empty) = if is_replaced {
        let h = replaced_size(boxes, cx, b, cb_w, cb_h, bp_w, bp_h).1;
        boxes[b as usize].h = h + bp_h;
        (Margins::of(mb), false)
    } else {
        let cbh_for_children = size_px(cx, &s, &s.height, cb_h, bp_h);
        match boxes[b as usize].kind {
            Kind::Table | Kind::Flex => {
                layout_insides(boxes, cx, b, cb_h, true);
                (Margins::of(mb), false)
            }
            _ => {
                let collapse_top = top_collapses(boxes, cx, b);
                let (mut out, empty) = if bfc_root {
                    let mut own = Floats::new();
                    layout_block_children(boxes, cx, b, cbh_for_children, &mut own, false, true)
                } else {
                    layout_block_children(
                        boxes,
                        cx,
                        b,
                        cbh_for_children,
                        floats,
                        collapse_top,
                        false,
                    )
                };
                out.add(mb);
                (out, empty)
            }
        }
    };
    let mk = boxes[b as usize].marker;
    if mk != NO_BOX {
        place_marker(boxes, cx, b, mk);
    }
    let empty = empty && mk == NO_BOX;
    // An empty block's top margin also collapses with its bottom (returned in `out`).
    let out = if empty {
        let mut o = out;
        o.add(mt);
        o
    } else {
        out
    };
    (out, empty)
}

/// Put an outside list marker to the left of the list item's first line (CSS Lists 3 §3.1).
fn place_marker(boxes: &mut Vec<LBox>, cx: &Cx, li: BoxId, mk: BoxId) {
    boxes[mk as usize].laid_out = false;
    layout_atomic(boxes, cx, mk, 0);
    let (cxx, cyy, _, _) = boxes[li as usize].content();
    let m = &boxes[mk as usize];
    let f = cx.font(&m.style);
    let fm = cx.metrics(&f);
    let asc = fm.ascent * AU;
    let gap = if matches!(&m.kind, Kind::Marker(t) if t.is_empty()) {
        0
    } else {
        0
    };
    let baseline = boxes[li as usize].first_baseline.unwrap_or(cyy + asc);
    let w = boxes[mk as usize].w;
    let nx = cxx - w - gap;
    let ny = baseline - asc;
    let (ox, oy) = (boxes[mk as usize].x, boxes[mk as usize].y);
    translate(boxes, mk, nx - ox, ny - oy);
}

/// Lay out the root element in the initial containing block (the viewport).
pub fn layout_root(boxes: &mut Vec<LBox>, cx: &Cx, root: BoxId) {
    let mut floats = Floats::new();
    let (_, _) = layout_block(
        boxes,
        cx,
        root,
        0,
        cx.vw,
        Some(cx.vh),
        0,
        &mut floats,
        false,
    );
    // The root's top margin pushes it down (it doesn't collapse with anything).
    let mt = boxes[root as usize].margin[0];
    if mt != 0 {
        translate(boxes, root, 0, mt);
    }
}
