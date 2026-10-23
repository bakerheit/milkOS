//! Absolutely and fixed positioned boxes (CSS 2.1 §10.3.7 and §10.6.4), laid out after the
//! normal flow in tree order (so a box's containing block is already placed), and the
//! document's scrollable extent.

use alloc::vec::Vec;

use super::block;
use super::*;

/// The absolutely positioned boxes in tree order.
fn abs_boxes(boxes: &[LBox], root: BoxId) -> Vec<BoxId> {
    let mut out = Vec::new();
    let mut stack = alloc::vec![root];
    while let Some(b) = stack.pop() {
        let bx = &boxes[b as usize];
        if b != root && bx.style.is_out_of_flow_positioned() {
            out.push(b);
        }
        for &c in bx.children.iter().rev() {
            stack.push(c);
        }
    }
    out
}

/// The containing block of an absolutely positioned box: the padding box of the nearest
/// positioned ancestor, else the initial containing block (CSS 2.1 §10.1). Fixed boxes use
/// the viewport.
fn containing_block(boxes: &[LBox], cx: &Cx, b: BoxId) -> (Au, Au, Au, Au) {
    if boxes[b as usize].style.position == Position::Fixed {
        return (0, 0, cx.vw, cx.vh);
    }
    let mut p = boxes[b as usize].parent;
    while p != NO_BOX {
        let pb = &boxes[p as usize];
        if pb.style.is_positioned() && pb.laid_out {
            if matches!(pb.kind, Kind::Inline) {
                // An inline containing block: the first fragment's box (approximation).
                return pb.padding_box();
            }
            return pb.padding_box();
        }
        p = pb.parent;
    }
    (0, 0, cx.vw, cx.vh)
}

pub fn layout_abs(boxes: &mut Vec<LBox>, cx: &Cx) {
    if boxes.is_empty() {
        return;
    }
    // Repeat a few times: a box laid out late can be the containing block of one before it
    // (only for odd trees); one pass in tree order is right for the normal case.
    let list = abs_boxes(boxes, 0);
    for b in list {
        cx.work.set(cx.work.get() + 1);
        layout_one(boxes, cx, b);
    }
}

fn layout_one(boxes: &mut Vec<LBox>, cx: &Cx, b: BoxId) {
    let (cbx, cby, cbw, cbh) = containing_block(boxes, cx, b);
    let s = boxes[b as usize].style.clone();
    let (m, bo, p) = block::edges(cx, &s, cbw);
    let bp_w = bo[1] + bo[3] + p[1] + p[3];
    let bp_h = bo[0] + bo[2] + p[0] + p[2];
    let inset = |i: usize, base: Au| match s.inset[i] {
        LPA::Auto => None,
        LPA::L(l) => Some(cx.lp(&l, base)),
    };
    let (top, right, bottom, left) = (inset(0, cbh), inset(1, cbw), inset(2, cbh), inset(3, cbw));
    let (sx, sy) = boxes[b as usize].static_pos;
    let is_replaced = matches!(boxes[b as usize].kind, Kind::Replaced(_));
    // Width (§10.3.7).
    let spec_w = if is_replaced {
        None
    } else {
        block::specified_width(boxes, cx, b, cbw, bp_w)
    };
    let ml0 = m[3].unwrap_or(0);
    let mr0 = m[1].unwrap_or(0);
    let shrink = |boxes: &[LBox], avail: Au| {
        let (mn, mx) = intrinsic::content_widths(boxes, cx, b);
        mx.min(avail.max(0)).max(mn)
    };
    let w = match (left, spec_w, right) {
        (_, Some(w), _) => w,
        (Some(l), None, Some(r)) => (cbw - l - r - ml0 - mr0 - bp_w).max(0),
        (l, None, r) => {
            let avail = cbw - l.unwrap_or(0) - r.unwrap_or(0) - ml0 - mr0 - bp_w;
            shrink(boxes, avail)
        }
    };
    let w = block::clamp_width(cx, &s, w, cbw, bp_w, boxes, b);
    // Horizontal position (auto margins center when both insets are set).
    let x = match (left, right) {
        (Some(l), Some(r)) if m[3].is_none() || m[1].is_none() => {
            let rest = cbw - l - r - w - bp_w;
            let (ml, _) = match (m[3], m[1]) {
                (None, None) => (rest.max(0) / 2, 0),
                (None, Some(mr)) => (rest - mr, mr),
                (Some(ml), _) => (ml, 0),
            };
            cbx + l + ml
        }
        (Some(l), _) => cbx + l + ml0,
        (None, Some(r)) => cbx + cbw - r - mr0 - w - bp_w,
        (None, None) => sx + ml0,
    };
    // Height (§10.6.4): fixed, or from the insets, or the content.
    let spec_h = block::size_px(cx, &s, &s.height, Some(cbh), bp_h);
    let from_insets = match (top, bottom, spec_h) {
        (Some(t), Some(bt), None) => {
            Some((cbh - t - bt - m[0].unwrap_or(0) - m[2].unwrap_or(0) - bp_h).max(0))
        }
        _ => None,
    };
    let forced = spec_h.or(from_insets);
    if is_replaced {
        block::layout_atomic(boxes, cx, b, cbw);
    } else {
        block::layout_sized(boxes, cx, b, cbw, w, forced, Some(cbh));
    }
    let h = boxes[b as usize].h;
    let mt = m[0].unwrap_or(0);
    let mb = m[2].unwrap_or(0);
    let y = match (top, bottom) {
        (Some(t), Some(bt)) if m[0].is_none() && m[2].is_none() && forced.is_some() => {
            let rest = cbh - t - bt - h;
            cby + t + rest.max(0) / 2
        }
        (Some(t), _) => cby + t + mt,
        (None, Some(bt)) => cby + cbh - bt - mb - h,
        (None, None) => sy + mt,
    };
    let bx = &boxes[b as usize];
    let (ox, oy) = (bx.x, bx.y);
    block::translate(boxes, b, x - ox, y - oy);
    boxes[b as usize].laid_out = true;
}

/// The scrollable extent of the document: the furthest box edges, not counting boxes inside
/// clipping (overflow: hidden) ancestors or fixed boxes.
pub fn extent(boxes: &[LBox], root: BoxId) -> (Au, Au) {
    let mut w = 0;
    let mut h = 0;
    let mut stack = alloc::vec![(root, false)];
    while let Some((b, clipped)) = stack.pop() {
        let bx = &boxes[b as usize];
        if bx.style.position == Position::Fixed {
            continue;
        }
        if !clipped && bx.laid_out {
            w = w.max(bx.x + bx.w + bx.margin[1].max(0));
            h = h.max(bx.y + bx.h + bx.margin[2].max(0));
            if let Some(ifc) = &bx.ifc
                && let Some(l) = ifc.lines.last()
            {
                h = h.max(l.y + l.h);
            }
        }
        let clips =
            clipped || (b != root && (bx.style.overflow_x.clips() || bx.style.overflow_y.clips()));
        for &c in &bx.children {
            stack.push((c, clips));
        }
    }
    (w, h)
}
