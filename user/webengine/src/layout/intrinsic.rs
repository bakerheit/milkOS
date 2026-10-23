//! Intrinsic widths (CSS Sizing 3 §5): the min-content and max-content widths of a box's
//! content, used by shrink-to-fit (floats, inline-blocks, absolutely positioned boxes),
//! tables and flexbox. Cached per box for one layout.

use super::*;

/// (min-content, max-content) of `b`'s content box.
pub fn content_widths(boxes: &[LBox], cx: &Cx, b: BoxId) -> (Au, Au) {
    if let Some(v) = cx.intrinsic.borrow().get(&b) {
        return *v;
    }
    cx.work.set(cx.work.get() + 1);
    let v = compute(boxes, cx, b);
    cx.intrinsic.borrow_mut().insert(b, v);
    v
}

fn compute(boxes: &[LBox], cx: &Cx, b: BoxId) -> (Au, Au) {
    let bx = &boxes[b as usize];
    match &bx.kind {
        Kind::Table => return table::intrinsic(boxes, cx, b),
        Kind::Flex => return flex::intrinsic(boxes, cx, b),
        Kind::Grid => return grid::intrinsic(boxes, cx, b),
        Kind::Replaced(_) => {
            let w = replaced_width(boxes, cx, b);
            return (w, w);
        }
        _ => {}
    }
    if bx.inline_content || matches!(bx.kind, Kind::Inline) {
        return inline::inline_widths(boxes, cx, b);
    }
    let mut mn = 0;
    let mut mx = 0;
    let mut float_run = 0;
    for &c in &bx.children {
        let cb = &boxes[c as usize];
        if cb.style.is_out_of_flow_positioned() {
            continue;
        }
        let (a, z) = outer_widths(boxes, cx, c);
        mn = mn.max(a);
        if cb.style.float != Float::None {
            // Floats side by side add up.
            float_run += z;
            mx = mx.max(float_run);
        } else {
            float_run = 0;
            mx = mx.max(z);
        }
    }
    (mn, mx.max(mn))
}

/// A replaced box's content width when nothing constrains it.
fn replaced_width(boxes: &[LBox], cx: &Cx, b: BoxId) -> Au {
    let bx = &boxes[b as usize];
    let s = &bx.style;
    if let Size::L(l) = s.width
        && !l.has_pct()
    {
        let (_, bo, p) = block::edges(cx, s, 0);
        let bp = bo[1] + bo[3] + p[1] + p[3];
        return block::size_px(cx, s, &s.width, Some(0), bp).unwrap_or(0);
    }
    // Lay it out against a huge containing block, ignoring percentage max-widths.
    let mut tmp: alloc::vec::Vec<LBox> = alloc::vec::Vec::new();
    tmp.push(bx.clone());
    let mut st = (*bx.style).clone();
    if st.max_width.is_some_and(|m| m.has_pct()) {
        st.max_width = None;
    }
    if matches!(st.width, Size::L(l) if l.has_pct()) {
        st.width = Size::Auto;
    }
    tmp[0].style = alloc::rc::Rc::new(st);
    tmp[0].parent = NO_BOX;
    tmp[0].children.clear();
    tmp[0].laid_out = false;
    tmp[0].marker = NO_BOX;
    block::layout_atomic(&mut tmp, cx, 0, 1 << 28);
    let t = &tmp[0];
    t.w - t.border[1] - t.border[3] - t.padding[1] - t.padding[3]
}

/// A child's contribution: its margin box at min-content and max-content.
pub fn outer_widths(boxes: &[LBox], cx: &Cx, c: BoxId) -> (Au, Au) {
    let cb = &boxes[c as usize];
    let s = &cb.style;
    let (m, bo, p) = block::edges(cx, s, 0);
    let bp = bo[1] + bo[3] + p[1] + p[3];
    let mm = m[1].unwrap_or(0).max(0) + m[3].unwrap_or(0).max(0);
    let fixed = match s.width {
        Size::L(l) if !l.has_pct() => block::size_px(cx, s, &s.width, Some(0), bp),
        _ => None,
    };
    let (mut a, mut z) = match fixed {
        Some(w) => (w, w),
        None => content_widths(boxes, cx, c),
    };
    if let Some(mx) = s.max_width
        && !mx.has_pct()
    {
        let mx = cx.lp(&mx, 0);
        let mx = if s.box_sizing == BoxSizing::BorderBox {
            (mx - bp).max(0)
        } else {
            mx
        };
        a = a.min(mx);
        z = z.min(mx);
    }
    if let Size::L(l) = s.min_width
        && !l.has_pct()
    {
        let mn = cx.lp(&l, 0);
        let mn = if s.box_sizing == BoxSizing::BorderBox {
            (mn - bp).max(0)
        } else {
            mn
        };
        a = a.max(mn);
        z = z.max(mn);
    }
    (a + bp + mm, z + bp + mm)
}
