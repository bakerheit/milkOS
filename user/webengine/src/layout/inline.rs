//! Inline formatting contexts: turning a block container's inline-level content into line
//! boxes.
//!
//! 1. Items: the inline subtree is flattened into words, spaces, inline-box open/close marks,
//!    atomic inlines, floats, forced breaks (CSS Text 3 §4.1 white-space processing: spaces
//!    collapse across element boundaries for `normal`/`nowrap`/`pre-line`, are kept for
//!    `pre`/`pre-wrap`; tabs advance to 8-space stops; `text-transform` is applied).
//! 2. Line breaking (CSS Text 3 §5, a greedy fit): break opportunities after spaces, after
//!    hyphens, around CJK characters, at U+200B and `<wbr>`, around atomic inlines; a word
//!    wider than the line is split anywhere only with `overflow-wrap: anywhere/break-word`.
//!    Lines are shortened beside floats (CSS 2.1 §9.5).
//! 3. Each line's height and baseline from the strut and every inline box and atomic on it,
//!    with `vertical-align` (CSS 2.1 §10.8.1), then `text-align` (justify spreads the spaces).

use alloc::vec::Vec;

use super::block::{self, Floats};
use super::*;

#[derive(Clone, Debug)]
enum It {
    /// A word or a space: text[start..end].
    Text {
        start: u32,
        end: u32,
        w: Au,
        styled: BoxId,
        font: Font,
        node: NodeId,
        space: bool,
        brk_after: bool,
    },
    Open(BoxId, Au),
    Close(BoxId, Au),
    Atomic(BoxId),
    Float(BoxId),
    Abs(BoxId),
    /// A forced break (`<br>` or a preserved newline).
    Break,
    /// A break opportunity (`<wbr>`).
    Wbr,
}

struct Collect<'a, 'b> {
    cx: &'a Cx<'b>,
    text: String,
    items: Vec<It>,
    /// The previous character was a collapsible space (spaces collapse across boxes).
    prev_space: bool,
    /// Start of the line so far (for leading-space removal).
    at_line_start: bool,
}

pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF | 0x2E80..=0x2FFF | 0x3000..=0x30FF | 0x3100..=0x31FF | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FFFF)
}

impl Collect<'_, '_> {
    fn push_word(&mut self, boxes: &[LBox], b: BoxId, word: &str, brk_after: bool) {
        if word.is_empty() {
            return;
        }
        let st = &boxes[b as usize].style;
        let font = self.cx.font(st);
        let start = self.text.len() as u32;
        match st.text_transform {
            TextTransform::None => self.text.push_str(word),
            TextTransform::Uppercase => self
                .text
                .extend(word.chars().flat_map(|c| c.to_uppercase())),
            TextTransform::Lowercase => self
                .text
                .extend(word.chars().flat_map(|c| c.to_lowercase())),
            TextTransform::Capitalize => {
                let mut first = self.prev_space || self.at_line_start || start == 0;
                for c in word.chars() {
                    if first && c.is_alphabetic() {
                        self.text.extend(c.to_uppercase());
                        first = false;
                    } else {
                        self.text.push(c);
                    }
                }
            }
        }
        let end = self.text.len() as u32;
        let node = boxes[b as usize].node;
        if st.letter_spacing != 0.0 {
            // Letter-spaced text goes out one character per item (and per run), so what is drawn
            // matches what was measured.
            let ls = self.cx.au(st.letter_spacing);
            let mut i = start as usize;
            while i < end as usize {
                let c = self.text[i..].chars().next().unwrap();
                let j = i + c.len_utf8();
                let w = self.cx.text_width(&font, &self.text[i..j]) + ls;
                let last = j == end as usize;
                self.items.push(It::Text {
                    start: i as u32,
                    end: j as u32,
                    w,
                    styled: b,
                    font,
                    node,
                    space: false,
                    brk_after: brk_after && last,
                });
                i = j;
            }
            self.prev_space = false;
            self.at_line_start = false;
            return;
        }
        let s = &self.text[start as usize..end as usize];
        let w = self.cx.text_width(&font, s);
        self.items.push(It::Text {
            start,
            end,
            w,
            styled: b,
            font,
            node,
            space: false,
            brk_after,
        });
        self.prev_space = false;
        self.at_line_start = false;
    }

    fn push_space(&mut self, boxes: &[LBox], b: BoxId, n: usize, wraps: bool) {
        let st = &boxes[b as usize].style;
        let font = self.cx.font(st);
        let start = self.text.len() as u32;
        for _ in 0..n {
            self.text.push(' ');
        }
        let end = self.text.len() as u32;
        let w = (self.cx.space_width(&font) + self.cx.au(st.word_spacing + st.letter_spacing))
            * n as i32;
        let node = boxes[b as usize].node;
        self.items.push(It::Text {
            start,
            end,
            w,
            styled: b,
            font,
            node,
            space: true,
            brk_after: wraps,
        });
    }

    /// One text box's content (CSS Text 3 §4.1.1 phase I).
    fn text_box(&mut self, boxes: &[LBox], b: BoxId, t: &str) {
        let ws = boxes[b as usize].style.white_space;
        let wraps = ws.wraps();
        let collapse = ws.collapses_spaces();
        let keep_nl = ws.keeps_newlines();
        let mut word = String::new();
        let mut col = 0usize; // for tabs in pre
        let chars: Vec<char> = t.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\n' && keep_nl {
                self.flush_word(boxes, b, &mut word, false);
                if collapse {
                    // pre-line: spaces before the newline go.
                    if let Some(It::Text { space: true, .. }) = self.items.last() {
                        self.items.pop();
                    }
                }
                self.items.push(It::Break);
                self.prev_space = collapse;
                self.at_line_start = true;
                col = 0;
                i += 1;
                continue;
            }
            if c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\x0c' {
                self.flush_word(boxes, b, &mut word, false);
                if collapse {
                    if !self.prev_space && !self.at_line_start {
                        self.push_space(boxes, b, 1, wraps);
                    }
                    self.prev_space = true;
                } else if c == '\t' {
                    let n = 8 - col % 8;
                    self.push_space(boxes, b, n, wraps);
                    col += n;
                } else {
                    // Preserved spaces: one item per run.
                    let mut n = 1;
                    while i + 1 < chars.len() && chars[i + 1] == ' ' {
                        n += 1;
                        i += 1;
                    }
                    self.push_space(boxes, b, n, wraps);
                    col += n;
                    self.prev_space = false;
                    self.at_line_start = false;
                }
                i += 1;
                continue;
            }
            match c {
                '\u{200B}' => {
                    self.flush_word(boxes, b, &mut word, wraps);
                }
                '\u{00AD}' => {
                    // A soft hyphen: a break opportunity (no hyphen is drawn).
                    self.flush_word(boxes, b, &mut word, wraps);
                }
                '-' | '\u{2010}' | '\u{2013}' | '\u{2014}' => {
                    word.push(c);
                    let next_alnum = chars.get(i + 1).is_some_and(|n| n.is_alphanumeric());
                    let prev_alnum = i > 0 && chars[i - 1].is_alphanumeric();
                    self.flush_word(boxes, b, &mut word, wraps && next_alnum && prev_alnum);
                }
                c if is_cjk(c) => {
                    self.flush_word(boxes, b, &mut word, wraps);
                    word.push(c);
                    self.flush_word(boxes, b, &mut word, wraps);
                }
                c => {
                    word.push(c);
                    if word.len() > 256 {
                        // Very long "words": cut into pieces (break only with break-anywhere).
                        let ba = boxes[b as usize].style.break_anywhere && wraps;
                        self.flush_word(boxes, b, &mut word, ba);
                    }
                }
            }
            col += 1;
            i += 1;
        }
        self.flush_word(boxes, b, &mut word, false);
    }

    fn flush_word(&mut self, boxes: &[LBox], b: BoxId, word: &mut String, brk_after: bool) {
        if !word.is_empty() {
            let w = core::mem::take(word);
            self.push_word(boxes, b, &w, brk_after);
        } else if brk_after {
            // A break opportunity after the previous word.
            if let Some(It::Text {
                brk_after: ba,
                space: false,
                ..
            }) = self.items.last_mut()
            {
                *ba = true;
            }
        }
    }

    fn walk(&mut self, boxes: &[LBox], b: BoxId, depth: u32) {
        for &c in &boxes[b as usize].children {
            let cb = &boxes[c as usize];
            if cb.out_of_flow {
                if cb.style.float != Float::None {
                    self.items.push(It::Float(c));
                } else {
                    self.items.push(It::Abs(c));
                }
                continue;
            }
            match &cb.kind {
                Kind::Text(t) => {
                    let t = t.clone();
                    self.text_box(boxes, c, &t);
                }
                Kind::Break => {
                    self.items.push(It::Break);
                    self.prev_space = true;
                    self.at_line_start = true;
                }
                Kind::Wbr => self.items.push(It::Wbr),
                Kind::Marker(t) => {
                    let t = t.clone();
                    if t.is_empty() {
                        // An inside bullet: a text-sized atomic drawn as a marker.
                        self.items.push(It::Atomic(c));
                    } else {
                        self.push_word(boxes, c, t.trim_end(), false);
                        self.push_space(boxes, c, 1, true);
                        self.prev_space = true;
                    }
                }
                Kind::Inline if depth < 200 => {
                    let (l, r) = inline_edges(self.cx, cb);
                    self.items.push(It::Open(c, l));
                    self.walk(boxes, c, depth + 1);
                    self.items.push(It::Close(c, r));
                }
                _ => {
                    self.items.push(It::Atomic(c));
                    self.prev_space = false;
                    self.at_line_start = false;
                }
            }
        }
    }
}

/// The horizontal margin+border+padding at the start and end of an inline box.
fn inline_edges(cx: &Cx, b: &LBox) -> (Au, Au) {
    let s = &b.style;
    let m = |i: usize| match s.margin[i] {
        LPA::L(l) => cx.lp(&l, 0),
        LPA::Auto => 0,
    };
    let l = m(3) + cx.au(s.border_width[3]) + cx.lp(&s.padding[3], 0);
    let r = m(1) + cx.au(s.border_width[1]) + cx.lp(&s.padding[1], 0);
    (l, r)
}

/// Line-box contributions of an inline box (CSS 2.1 §10.8.1), relative to the root baseline.
#[derive(Clone, Copy)]
struct VBox {
    /// Baseline shift from the root baseline (down = positive).
    shift: Au,
    ascent: Au,
    descent: Au,
    half_leading: Au,
}

fn vbox(cx: &Cx, s: &Style) -> VBox {
    let f = cx.font(s);
    let m = cx.metrics(&f);
    let a = m.ascent * AU;
    let d = m.descent * AU;
    let lh = cx.line_height(s);
    VBox {
        shift: 0,
        ascent: a,
        descent: d,
        half_leading: (lh - (a + d)) / 2,
    }
}

/// The baseline shift of a box styled `s` inside a parent styled `p` (CSS 2.1 §10.8.1
/// `vertical-align`), relative to the parent's baseline. None for top/bottom (aligned to the
/// line box afterwards).
fn shift_for(cx: &Cx, s: &Style, p: &Style, own: &VBox, parent: &VBox) -> Option<Au> {
    let pfs = cx.au(p.font_size);
    Some(match s.vertical_align {
        VAlign::Baseline => 0,
        VAlign::Sub => pfs / 5,
        VAlign::Super => -(pfs * 2 / 5),
        VAlign::L(l) => -cx.lp(&l, 0),
        // The box's middle at the parent's baseline plus half an x-height (~0.25 em).
        VAlign::Middle => -(pfs / 4) - (own.descent - own.ascent) / 2,
        VAlign::TextTop => own.ascent + own.half_leading - parent.ascent,
        VAlign::TextBottom => parent.descent - own.descent - own.half_leading,
        VAlign::Top | VAlign::Bottom => return None,
    })
}

#[derive(Clone, Copy)]
struct Placed {
    it: usize,
    x: Au,
    w: Au,
}

fn has_content(items: &[It], placed: &[Placed]) -> bool {
    placed
        .iter()
        .any(|p| matches!(items[p.it], It::Text { space: false, .. } | It::Atomic(_)))
}

/// Lay out the IFC of block container `b` whose content box starts at (`x`, `y`) and is `w`
/// wide. Returns the height of its line boxes. Fills the box's `ifc` and baselines, places
/// atomic inlines (translating their subtrees) and floats.
pub fn layout_ifc(
    boxes: &mut Vec<LBox>,
    cx: &Cx,
    b: BoxId,
    x: Au,
    y: Au,
    w: Au,
    floats: &mut Floats,
) -> Au {
    let mut col = Collect {
        cx,
        text: String::new(),
        items: Vec::new(),
        prev_space: false,
        at_line_start: true,
    };
    col.walk(boxes, b, 0);
    let Collect {
        text, mut items, ..
    } = col;
    let bs = boxes[b as usize].style.clone();
    for it in &items {
        if let It::Atomic(a) = it {
            block::layout_atomic(boxes, cx, *a, w);
        }
    }
    let root_vb = vbox(cx, &bs);
    let strut_h = cx.line_height(&bs);
    let indent = cx.lp(&bs.text_indent, w);
    let wraps_root = bs.white_space.wraps();
    let mut frags: Vec<Frag> = Vec::new();
    let mut lines: Vec<Line> = Vec::new();
    let mut open: Vec<BoxId> = Vec::new();
    let mut floats_done: Vec<bool> = alloc::vec![false; items.len()];
    let mut pending: Vec<BoxId> = Vec::new();
    let mut i = 0;
    let mut cy = y;
    let mut first_line = true;
    let mut guard = 0u32;
    loop {
        guard += 1;
        if guard > 1_000_000 {
            break;
        }
        for f in core::mem::take(&mut pending) {
            block::place_float(boxes, cx, f, x, w, cy, floats);
        }
        while i < items.len() {
            match &items[i] {
                It::Text {
                    space: true,
                    styled,
                    ..
                } if boxes[*styled as usize].style.white_space.collapses_spaces() => i += 1,
                _ => break,
            }
        }
        if i >= items.len() {
            break;
        }
        let (mut left, mut right) = floats.band(cy, strut_h, x, x + w);
        let x0 = left + if first_line { indent } else { 0 };
        let mut cur = x0;
        let mut placed: Vec<Placed> = Vec::new();
        let mut last_break: Option<(usize, usize)> = None;
        let mut forced = false;
        let mut j = i;
        while j < items.len() {
            match items[j] {
                It::Break => {
                    j += 1;
                    forced = true;
                    break;
                }
                It::Float(f) => {
                    if !floats_done[j] {
                        floats_done[j] = true;
                        block::layout_atomic(boxes, cx, f, w);
                        let fw = boxes[f as usize].margin_box_w();
                        if fw <= right - cur || !has_content(&items, &placed) {
                            block::place_float(boxes, cx, f, x, w, cy, floats);
                            let (l2, r2) = floats.band(cy, strut_h, x, x + w);
                            let dx = l2 - left;
                            if dx != 0 {
                                for p in placed.iter_mut() {
                                    p.x += dx;
                                }
                                cur += dx;
                            }
                            left = l2;
                            right = r2;
                        } else {
                            pending.push(f);
                        }
                    }
                    j += 1;
                    continue;
                }
                It::Abs(a) => {
                    boxes[a as usize].static_pos = (cur, cy);
                    j += 1;
                    continue;
                }
                It::Wbr => {
                    last_break = Some((j + 1, placed.len()));
                    j += 1;
                    continue;
                }
                _ => {}
            }
            let mut iw = match &items[j] {
                It::Text { w, .. } => *w,
                It::Open(_, e) | It::Close(_, e) => *e,
                It::Atomic(a) => boxes[*a as usize].margin_box_w(),
                _ => 0,
            };
            let wraps_here = match &items[j] {
                It::Text { styled, .. } => boxes[*styled as usize].style.white_space.wraps(),
                _ => wraps_root,
            };
            let is_space = matches!(items[j], It::Text { space: true, .. });
            if !is_space && cur + iw > right && has_content(&items, &placed) {
                if let Some((after, plen)) = last_break {
                    placed.truncate(plen);
                    j = after;
                    break;
                }
                if matches!(items[j], It::Atomic(_)) && wraps_here {
                    break;
                }
            }
            // overflow-wrap: anywhere / break-word: split a word too wide for an empty line.
            if let It::Text {
                start,
                end,
                styled,
                font,
                node,
                space: false,
                brk_after,
                ..
            } = items[j]
                && cur + iw > right
                && !has_content(&items, &placed)
                && boxes[styled as usize].style.break_anywhere
                && end - start > 1
            {
                let s = &text[start as usize..end as usize];
                let mut fit = 0;
                let mut acc = 0;
                for (k, ch) in s.char_indices() {
                    let cw = cx.text_width(&font, &s[k..k + ch.len_utf8()]);
                    if cur + acc + cw > right && fit > 0 {
                        break;
                    }
                    acc += cw;
                    fit = k + ch.len_utf8();
                }
                if fit < s.len() {
                    let mid = start + fit as u32;
                    let tail_w = cx.text_width(&font, &text[mid as usize..end as usize]);
                    items[j] = It::Text {
                        start,
                        end: mid,
                        w: acc,
                        styled,
                        node,
                        font,
                        space: false,
                        brk_after: true,
                    };
                    items.insert(
                        j + 1,
                        It::Text {
                            start: mid,
                            end,
                            w: tail_w,
                            styled,
                            node,
                            font,
                            space: false,
                            brk_after,
                        },
                    );
                    floats_done.insert(j + 1, false);
                    iw = acc;
                }
            }
            placed.push(Placed {
                it: j,
                x: cur,
                w: iw,
            });
            cur += iw;
            let brk = match &items[j] {
                It::Text { brk_after, .. } => *brk_after,
                It::Atomic(_) => wraps_here,
                _ => false,
            };
            if brk {
                last_break = Some((j + 1, placed.len()));
            }
            if j + 1 < items.len()
                && matches!(items[j + 1], It::Atomic(_))
                && wraps_here
                && !matches!(items[j], It::Open(..))
            {
                last_break = Some((j + 1, placed.len()));
            }
            j += 1;
        }
        if !forced {
            // Close marks right after the break end this line's boxes.
            while j < items.len() && matches!(items[j], It::Close(..)) {
                let It::Close(_, e) = items[j] else {
                    unreachable!()
                };
                placed.push(Placed {
                    it: j,
                    x: cur,
                    w: e,
                });
                cur += e;
                j += 1;
            }
        }
        let last_line = j >= items.len();
        let l = finish_line(
            boxes, cx, &bs, &root_vb, &items, &text, &placed, &mut open, cy, left, right, forced,
            last_line, &mut frags,
        );
        if l.h > 0 {
            cy = l.y + l.h;
            lines.push(l);
        }
        first_line = false;
        if j == i {
            // No progress (nothing fits beside the floats): go below the next float edge.
            let ny = floats.next_edge_below(cy);
            if ny > cy {
                cy = ny;
                continue;
            }
            j = i + 1;
        }
        i = j;
    }
    for f in core::mem::take(&mut pending) {
        block::place_float(boxes, cx, f, x, w, cy, floats);
    }
    let bx = &mut boxes[b as usize];
    bx.first_baseline = lines.first().map(|l| l.baseline);
    bx.last_baseline = lines.last().map(|l| l.baseline);
    bx.ifc = Some(alloc::boxed::Box::new(Ifc { text, lines, frags }));
    cy - y
}

/// One line: drop hanging spaces, align, compute the line height and baseline, emit the
/// fragments and move the atomics into place.
#[allow(clippy::too_many_arguments)]
fn finish_line(
    boxes: &mut Vec<LBox>,
    cx: &Cx,
    bs: &Style,
    root_vb: &VBox,
    items: &[It],
    text: &str,
    placed: &[Placed],
    open: &mut Vec<BoxId>,
    y: Au,
    left: Au,
    right: Au,
    forced: bool,
    last_line: bool,
    frags: &mut Vec<Frag>,
) -> Line {
    let mut placed: Vec<Placed> = placed.to_vec();
    // Trailing collapsible spaces hang (CSS Text 3 §4.1.2), also before closing inline boxes.
    let mut k = placed.len();
    while k > 0 {
        k -= 1;
        match &items[placed[k].it] {
            It::Close(..) => continue,
            It::Text {
                space: true,
                styled,
                ..
            } if boxes[*styled as usize].style.white_space.collapses_spaces() => {
                let w = placed[k].w;
                placed.remove(k);
                for p in placed.iter_mut().skip(k) {
                    p.x -= w;
                }
            }
            _ => break,
        }
    }
    let content = has_content(items, &placed)
        || placed
            .iter()
            .any(|p| matches!(items[p.it], It::Open(_, e) | It::Close(_, e) if e > 0));
    if !content && !forced {
        for p in &placed {
            match items[p.it] {
                It::Open(ib, _) => open.push(ib),
                It::Close(..) => {
                    open.pop();
                }
                _ => {}
            }
        }
        return Line {
            y,
            h: 0,
            baseline: y,
        };
    }
    let end_x = placed.last().map(|p| p.x + p.w).unwrap_or(left);
    // Horizontal alignment (CSS Text 3 §6.1, §6.4 justify).
    let slack = (right - end_x).max(0);
    let justify = bs.text_align == TextAlign::Justify && !last_line && !forced && slack > 0;
    let dx = match bs.text_align {
        TextAlign::Left | TextAlign::Justify => 0,
        TextAlign::Right => slack,
        TextAlign::Center => slack / 2,
    };
    let mut xs: Vec<Au> = placed.iter().map(|p| p.x + dx).collect();
    if justify {
        let spaces = placed
            .iter()
            .filter(|p| matches!(items[p.it], It::Text { space: true, .. }))
            .count();
        if spaces > 0 {
            let per = slack / spaces as i32;
            let mut rem = slack - per * spaces as i32;
            let mut extra = 0;
            for (k, p) in placed.iter().enumerate() {
                xs[k] += extra;
                if matches!(items[p.it], It::Text { space: true, .. }) {
                    extra += per;
                    if rem > 0 {
                        extra += 1;
                        rem -= 1;
                    }
                }
            }
        }
    }
    // Vertical metrics: the strut, the inline boxes (continued ones first) and the atomics.
    let mut top = -(root_vb.ascent + root_vb.half_leading);
    let mut bottom = root_vb.descent + root_vb.half_leading;
    let mut stack: Vec<(BoxId, VBox)> = Vec::new();
    let mut box_shift: Vec<(BoxId, Au)> = Vec::new();
    let push_box = |boxes: &[LBox],
                    stack: &mut Vec<(BoxId, VBox)>,
                    ib: BoxId,
                    top: &mut Au,
                    bottom: &mut Au|
     -> Au {
        let cur = stack.last().map(|s| s.1).unwrap_or(*root_vb);
        let s = &boxes[ib as usize].style;
        let ps = &boxes[boxes[ib as usize].parent as usize].style;
        let mut v = vbox(cx, s);
        v.shift = cur.shift + shift_for(cx, s, ps, &v, &cur).unwrap_or(0);
        *top = (*top).min(v.shift - v.ascent - v.half_leading);
        *bottom = (*bottom).max(v.shift + v.descent + v.half_leading);
        stack.push((ib, v));
        v.shift
    };
    for &ob in open.iter() {
        let sh = push_box(boxes, &mut stack, ob, &mut top, &mut bottom);
        box_shift.push((ob, sh));
    }
    let mut shifts: Vec<Au> = alloc::vec![0; placed.len()];
    let mut edges: Vec<Option<bool>> = alloc::vec![None; placed.len()];
    let mut tall = 0;
    for (k, p) in placed.iter().enumerate() {
        let cur = stack.last().map(|s| s.1).unwrap_or(*root_vb);
        match items[p.it] {
            It::Open(ib, _) => {
                let sh = push_box(boxes, &mut stack, ib, &mut top, &mut bottom);
                box_shift.push((ib, sh));
                shifts[k] = sh;
            }
            It::Close(..) => {
                shifts[k] = cur.shift;
                stack.pop();
            }
            It::Text { .. } => shifts[k] = cur.shift,
            It::Atomic(a) => {
                let ab = &boxes[a as usize];
                let h = ab.margin_box_h();
                let s = &ab.style;
                let ps = if ab.parent == NO_BOX {
                    s
                } else {
                    &boxes[ab.parent as usize].style
                };
                let base = atomic_baseline(ab);
                let own = VBox {
                    shift: 0,
                    ascent: base,
                    descent: h - base,
                    half_leading: 0,
                };
                match shift_for(cx, s, ps, &own, &cur) {
                    Some(sh) => {
                        let t = cur.shift + sh - base;
                        shifts[k] = t;
                        top = top.min(t);
                        bottom = bottom.max(t + h);
                    }
                    None => {
                        edges[k] = Some(s.vertical_align == VAlign::Top);
                        tall = tall.max(h);
                    }
                }
            }
            _ => {}
        }
    }
    let height = (bottom - top).max(tall);
    let baseline = y - top;
    let line_bottom = y + height;
    let shift_of = |b: BoxId| {
        box_shift
            .iter()
            .rev()
            .find(|(x, _)| *x == b)
            .map(|x| x.1)
            .unwrap_or(0)
    };
    // Fragments.
    let mut starts: Vec<(BoxId, Au, bool)> = open
        .iter()
        .map(|&ob| (ob, xs.first().copied().unwrap_or(left), false))
        .collect();
    let mut k = 0;
    while k < placed.len() {
        match items[placed[k].it] {
            It::Text {
                start,
                end,
                styled,
                font,
                node,
                ..
            } => {
                // A run: consecutive text items with the same style box and shift.
                let mut kk = k + 1;
                let mut run_end = end;
                let mut run_w = placed[k].w;
                if !justify && boxes[styled as usize].style.letter_spacing == 0.0 {
                    while kk < placed.len() {
                        match items[placed[kk].it] {
                            It::Text {
                                start: s2,
                                end: e2,
                                styled: st2,
                                ..
                            } if st2 == styled && shifts[kk] == shifts[k] && s2 == run_end => {
                                run_end = e2;
                                run_w += placed[kk].w;
                                kk += 1;
                            }
                            _ => break,
                        }
                    }
                }
                let mut te = run_end;
                let mut tw = run_w;
                // Preserved trailing spaces at the end of the line don't need drawing.
                if kk == placed.len() {
                    while te > start && text.as_bytes()[te as usize - 1] == b' ' {
                        te -= 1;
                        tw -= cx.space_width(&font);
                    }
                }
                if te > start {
                    frags.push(Frag::Text {
                        start,
                        end: te,
                        styled,
                        font,
                        x: xs[k],
                        baseline: baseline + shifts[k],
                        w: tw.max(0),
                        node,
                    });
                }
                k = kk;
                continue;
            }
            It::Open(ib, _) => starts.push((ib, xs[k], true)),
            It::Close(ib, e) => {
                if let Some(pos) = starts.iter().rposition(|s| s.0 == ib) {
                    let (bb, sx, first) = starts.remove(pos);
                    inline_frag(
                        boxes,
                        cx,
                        frags,
                        bb,
                        sx,
                        xs[k] + e - sx,
                        baseline,
                        shift_of(bb),
                        first,
                        true,
                    );
                }
            }
            It::Atomic(a) => {
                let ab = &boxes[a as usize];
                let top_edge = match edges[k] {
                    Some(true) => y,
                    Some(false) => line_bottom - ab.margin_box_h(),
                    None => baseline + shifts[k],
                };
                let nx = xs[k] + ab.margin[3];
                let ny = top_edge + ab.margin[0];
                let (ox, oy) = (ab.x, ab.y);
                block::translate(boxes, a, nx - ox, ny - oy);
            }
            _ => {}
        }
        k += 1;
    }
    let line_end = placed
        .last()
        .map(|p| xs[placed.len() - 1] + p.w)
        .unwrap_or(left);
    for (bb, sx, first) in starts {
        inline_frag(
            boxes,
            cx,
            frags,
            bb,
            sx,
            line_end - sx,
            baseline,
            shift_of(bb),
            first,
            false,
        );
    }
    for p in &placed {
        match items[p.it] {
            It::Open(ib, _) => open.push(ib),
            It::Close(..) => {
                open.pop();
            }
            _ => {}
        }
    }
    Line {
        y,
        h: height.max(0),
        baseline,
    }
}

/// The fragment of inline box `b` on one line: its content area (the font's ascent and
/// descent around the baseline) plus vertical padding and borders (CSS 2.1 §10.6.1).
#[allow(clippy::too_many_arguments)]
fn inline_frag(
    boxes: &[LBox],
    cx: &Cx,
    frags: &mut Vec<Frag>,
    b: BoxId,
    x: Au,
    w: Au,
    baseline: Au,
    shift: Au,
    first: bool,
    last: bool,
) {
    let s = &boxes[b as usize].style;
    let f = cx.font(s);
    let m = cx.metrics(&f);
    let pt = cx.lp(&s.padding[0], 0) + cx.au(s.border_width[0]);
    let pb = cx.lp(&s.padding[2], 0) + cx.au(s.border_width[2]);
    let top = baseline + shift - m.ascent * AU - pt;
    let h = (m.ascent + m.descent) * AU + pt + pb;
    let margin = |i: usize| match s.margin[i] {
        LPA::L(l) => cx.lp(&l, 0),
        LPA::Auto => 0,
    };
    let ml = if first { margin(3) } else { 0 };
    let mr = if last { margin(1) } else { 0 };
    frags.push(Frag::Inline {
        b,
        x: x + ml,
        y: top,
        w: (w - ml - mr).max(0),
        h,
        first,
        last,
    });
}

/// An atomic inline's baseline, from its margin-box top (CSS 2.1 §10.8.1: an inline-block's
/// last line box, unless it has none or its overflow isn't visible; else the bottom margin
/// edge).
fn atomic_baseline(b: &LBox) -> Au {
    let mbt = b.y - b.margin[0];
    let bottom = b.margin_box_h();
    match &b.kind {
        Kind::Replaced(Replaced::Control(_, k))
            if !matches!(k, ControlKind::Checkbox | ControlKind::Radio) =>
        {
            // A field's text sits on a baseline about 3/4 down its content box.
            let (_, cy, _, ch) = b.content();
            cy - mbt + ch * 3 / 4
        }
        Kind::Replaced(_) | Kind::Marker(_) => bottom,
        _ if b.style.overflow_y.clips() => bottom,
        _ => match b.last_baseline {
            Some(bl) => bl - mbt,
            None => bottom,
        },
    }
}

/// (min-content, max-content) of an inline formatting context (CSS Sizing 3 §5.1): the widest
/// unbreakable piece, and the widest line with only forced breaks.
pub fn inline_widths(boxes: &[LBox], cx: &Cx, b: BoxId) -> (Au, Au) {
    let mut col = Collect {
        cx,
        text: String::new(),
        items: Vec::new(),
        prev_space: false,
        at_line_start: true,
    };
    col.walk(boxes, b, 0);
    let indent = cx.lp(&boxes[b as usize].style.text_indent, 0).max(0);
    let mut mn = 0;
    let mut mx = 0;
    let mut line = indent;
    let mut word = indent;
    let mut trailing_space = 0;
    for it in &col.items {
        match it {
            It::Text {
                w,
                space,
                brk_after,
                styled,
                ..
            } => {
                if *space {
                    if boxes[*styled as usize].style.white_space.wraps() {
                        mn = mn.max(word);
                        word = 0;
                    } else {
                        word += w;
                    }
                    line += w;
                    trailing_space = *w;
                } else {
                    word += w;
                    line += w;
                    trailing_space = 0;
                    if *brk_after {
                        mn = mn.max(word);
                        word = 0;
                    }
                }
            }
            It::Open(_, e) | It::Close(_, e) => {
                word += e;
                line += e;
            }
            It::Atomic(a) => {
                let (amn, amx) = if matches!(boxes[*a as usize].kind, Kind::Marker(_)) {
                    let f = cx.font(&boxes[*a as usize].style);
                    let w = cx.au(boxes[*a as usize].style.font_size * 0.9);
                    let _ = f;
                    (w, w)
                } else {
                    intrinsic::outer_widths(boxes, cx, *a)
                };
                mn = mn.max(word);
                word = 0;
                mn = mn.max(amn);
                line += amx;
                trailing_space = 0;
            }
            It::Float(f) => {
                let (fmn, fmx) = intrinsic::outer_widths(boxes, cx, *f);
                mn = mn.max(fmn);
                line += fmx;
            }
            It::Break => {
                mx = mx.max(line - trailing_space);
                mn = mn.max(word);
                line = 0;
                word = 0;
                trailing_space = 0;
            }
            It::Wbr => {
                mn = mn.max(word);
                word = 0;
            }
            It::Abs(_) => {}
        }
    }
    mx = mx.max(line - trailing_space);
    mn = mn.max(word);
    (mn, mx.max(mn))
}
