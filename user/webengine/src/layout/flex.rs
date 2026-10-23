//! Flex layout, CSS Flexible Box Layout 1 §9 "Flex Layout Algorithm": flex base sizes and
//! hypothetical main sizes (§9.2), line collection (§9.3), resolving flexible lengths with the
//! freeze loop (§9.7), cross sizes and `stretch` (§9.4), main-axis alignment with auto margins
//! and `justify-content` (§9.5), cross-axis alignment and `align-content` (§9.6), gaps, `order`
//! and the reversed directions.

use alloc::vec::Vec;

use super::block;
use super::*;

struct Item {
    b: BoxId,
    /// Margins (main start, main end, cross start, cross end), None = auto.
    m: [Option<Au>; 4],
    /// Border + padding along main / cross.
    bp_main: Au,
    bp_cross: Au,
    base: Au,
    hyp: Au,
    min: Au,
    max: Au,
    grow: f32,
    shrink: f32,
    target: Au,
    frozen: bool,
    cross: Au,
}

impl Item {
    fn outer_hyp(&self) -> Au {
        self.hyp + self.bp_main + self.m[0].unwrap_or(0) + self.m[1].unwrap_or(0)
    }
    fn outer_target(&self) -> Au {
        self.target + self.bp_main + self.m[0].unwrap_or(0) + self.m[1].unwrap_or(0)
    }
    fn outer_cross(&self) -> Au {
        self.cross + self.bp_cross + self.m[2].unwrap_or(0) + self.m[3].unwrap_or(0)
    }
}

fn is_row(s: &Style) -> bool {
    matches!(
        s.flex_direction,
        FlexDirection::Row | FlexDirection::RowReverse
    )
}

/// The in-flow items in `order` order (stable).
fn items_of(boxes: &[LBox], f: BoxId) -> Vec<BoxId> {
    let mut v: Vec<BoxId> = boxes[f as usize]
        .children
        .iter()
        .copied()
        .filter(|&c| !boxes[c as usize].style.is_out_of_flow_positioned())
        .collect();
    v.sort_by_key(|&c| boxes[c as usize].style.order);
    v
}

/// (min-content, max-content) of a flex container's content.
pub fn intrinsic(boxes: &[LBox], cx: &Cx, f: BoxId) -> (Au, Au) {
    let s = &boxes[f as usize].style;
    let items = items_of(boxes, f);
    let gap = cx.lp(&s.column_gap, 0);
    let mut mn = 0;
    let mut mx = 0;
    if is_row(s) {
        let n = items.len() as Au;
        for &c in &items {
            let (a, z) = intrinsic::outer_widths(boxes, cx, c);
            if s.flex_wrap == FlexWrap::NoWrap {
                mn += a;
            } else {
                mn = mn.max(a);
            }
            mx += z;
        }
        if n > 1 {
            mx += gap * (n - 1);
            if s.flex_wrap == FlexWrap::NoWrap {
                mn += gap * (n - 1);
            }
        }
    } else {
        for &c in &items {
            let (a, z) = intrinsic::outer_widths(boxes, cx, c);
            mn = mn.max(a);
            mx = mx.max(z);
        }
    }
    (mn, mx.max(mn))
}

/// The content height of an item laid out at content width `w`.
fn height_at(boxes: &mut Vec<LBox>, cx: &Cx, b: BoxId, cb_w: Au, w: Au, cb_h: Option<Au>) -> Au {
    if let Some(h) = cx.heights.borrow().get(&(b, w, cb_h)) {
        return *h;
    }
    block::layout_sized(boxes, cx, b, cb_w, w, None, cb_h);
    let bx = &boxes[b as usize];
    let h = bx.h - bx.border[0] - bx.border[2] - bx.padding[0] - bx.padding[2];
    cx.heights.borrow_mut().insert((b, w, cb_h), h);
    h
}

/// Lay out flex container `f` (x, y, w and edges set). `spec_h` is its definite content
/// height, if any.
pub fn layout_flex(
    boxes: &mut Vec<LBox>,
    cx: &Cx,
    f: BoxId,
    spec_h: Option<Au>,
    _cb_h: Option<Au>,
) {
    let s = boxes[f as usize].style.clone();
    let (fx, fy, fw, _) = boxes[f as usize].content();
    let row = is_row(&s);
    let reverse = matches!(
        s.flex_direction,
        FlexDirection::RowReverse | FlexDirection::ColumnReverse
    );
    let main_gap = cx.lp(if row { &s.column_gap } else { &s.row_gap }, fw);
    let cross_gap = cx.lp(if row { &s.row_gap } else { &s.column_gap }, fw);
    // Available main space (definite for rows; for columns only with a definite height).
    let avail_main: Option<Au> = if row { Some(fw) } else { spec_h };
    let ids = items_of(boxes, f);
    // Absolutely positioned children: static position at the content box origin.
    for &c in &boxes[f as usize].children.clone() {
        if boxes[c as usize].style.is_out_of_flow_positioned() {
            boxes[c as usize].static_pos = (fx, fy);
        }
    }
    let mut items: Vec<Item> = Vec::with_capacity(ids.len());
    for &b in &ids {
        cx.work.set(cx.work.get() + 1);
        let st = boxes[b as usize].style.clone();
        let (m, bo, p) = block::edges(cx, &st, fw);
        let (ms, me, cs, ce) = if row {
            (m[3], m[1], m[0], m[2])
        } else {
            (m[0], m[2], m[3], m[1])
        };
        let bp_w = bo[1] + bo[3] + p[1] + p[3];
        let bp_h = bo[0] + bo[2] + p[0] + p[2];
        let (bp_main, bp_cross) = if row { (bp_w, bp_h) } else { (bp_h, bp_w) };
        let main_size_prop = if row { &st.width } else { &st.height };
        let main_ref = if row { Some(fw) } else { spec_h };
        // §9.2.3 the flex base size.
        let basis = match st.flex_basis {
            Size::L(l) if !(l.has_pct() && main_ref.is_none()) => {
                let x = cx.lp(&l, main_ref.unwrap_or(0));
                Some(if st.box_sizing == BoxSizing::BorderBox {
                    (x - bp_main).max(0)
                } else {
                    x.max(0)
                })
            }
            Size::Auto => block::size_px(cx, &st, main_size_prop, main_ref, bp_main),
            _ => None,
        };
        let content_main = |boxes: &mut Vec<LBox>| -> Au {
            if row {
                intrinsic::content_widths(boxes, cx, b).1
            } else {
                // Column: the height at the item's cross size.
                let cw = match block::size_px(cx, &st, &st.width, Some(fw), bp_w) {
                    Some(w) => w,
                    None => (fw - bp_w - m[1].unwrap_or(0) - m[3].unwrap_or(0)).max(0),
                };
                height_at(boxes, cx, b, fw, cw, spec_h)
            }
        };
        let base = match basis {
            Some(v) => v,
            None => content_main(boxes),
        };
        // min/max main sizes; min auto = the content-based minimum (§4.5).
        let (min_prop, max_prop) = if row {
            (&st.min_width, &st.max_width)
        } else {
            (&st.min_height, &st.max_height)
        };
        let max = match max_prop {
            Some(l) if !(l.has_pct() && main_ref.is_none()) => {
                let x = cx.lp(l, main_ref.unwrap_or(0));
                if st.box_sizing == BoxSizing::BorderBox {
                    (x - bp_main).max(0)
                } else {
                    x
                }
            }
            _ => Au::MAX / 4,
        };
        let min = match min_prop {
            Size::Auto => {
                let clips = if row {
                    st.overflow_x.clips()
                } else {
                    st.overflow_y.clips()
                };
                if clips {
                    0
                } else if row {
                    let mc = intrinsic::content_widths(boxes, cx, b).0;
                    let spec = block::size_px(cx, &st, main_size_prop, main_ref, bp_main);
                    spec.map(|s| s.min(mc)).unwrap_or(mc).min(max)
                } else {
                    0
                }
            }
            v => block::size_px(cx, &st, v, main_ref, bp_main).unwrap_or(0),
        };
        let hyp = base.min(max).max(min);
        items.push(Item {
            b,
            m: [ms, me, cs, ce],
            bp_main,
            bp_cross,
            base,
            hyp,
            min,
            max,
            grow: st.flex_grow,
            shrink: st.flex_shrink,
            target: hyp,
            frozen: false,
            cross: 0,
        });
    }
    // §9.3 collect into lines.
    let mut lines: Vec<(usize, usize)> = Vec::new();
    {
        let mut start = 0;
        let mut used = 0;
        for i in 0..items.len() {
            let o = items[i].outer_hyp();
            let wrap = s.flex_wrap != FlexWrap::NoWrap;
            if wrap && i > start && avail_main.is_some_and(|a| used + main_gap + o > a) {
                lines.push((start, i));
                start = i;
                used = 0;
            }
            used += if i > start { main_gap + o } else { o };
        }
        if start < items.len() || items.is_empty() {
            lines.push((start, items.len()));
        }
    }
    // §9.7 resolve the flexible lengths, line by line.
    for &(a, z) in &lines {
        let gaps = main_gap * (z - a).saturating_sub(1) as Au;
        let Some(avail) = avail_main else {
            for it in &mut items[a..z] {
                it.target = it.hyp;
            }
            continue;
        };
        let sum_hyp: Au = items[a..z].iter().map(|i| i.outer_hyp()).sum::<Au>() + gaps;
        let growing = sum_hyp < avail;
        for it in &mut items[a..z] {
            it.frozen = false;
            it.target = it.hyp;
            let inflexible = if growing {
                it.grow == 0.0
            } else {
                it.shrink == 0.0
            };
            if inflexible || (growing && it.base > it.hyp) || (!growing && it.base < it.hyp) {
                it.frozen = true;
            }
        }
        let initial_free = avail
            - items[a..z]
                .iter()
                .map(|i| {
                    if i.frozen {
                        i.outer_target()
                    } else {
                        i.base + i.bp_main + i.m[0].unwrap_or(0) + i.m[1].unwrap_or(0)
                    }
                })
                .sum::<Au>()
            - gaps;
        for _round in 0..64 {
            if items[a..z].iter().all(|i| i.frozen) {
                break;
            }
            let used: Au = items[a..z]
                .iter()
                .map(|i| {
                    if i.frozen {
                        i.outer_target()
                    } else {
                        i.base + i.bp_main + i.m[0].unwrap_or(0) + i.m[1].unwrap_or(0)
                    }
                })
                .sum::<Au>()
                + gaps;
            let mut free = avail - used;
            let sum_grow: f32 = items[a..z]
                .iter()
                .filter(|i| !i.frozen)
                .map(|i| i.grow)
                .sum();
            let sum_shrink: f32 = items[a..z]
                .iter()
                .filter(|i| !i.frozen)
                .map(|i| i.shrink * i.base as f32)
                .sum();
            if growing && sum_grow < 1.0 {
                let lim = fmath::round_i32(initial_free as f32 * sum_grow);
                if lim.abs() < free.abs() {
                    free = lim;
                }
            }
            for it in &mut items[a..z] {
                if it.frozen {
                    continue;
                }
                it.target = if growing {
                    if sum_grow > 0.0 {
                        it.base + fmath::round_i32(free as f32 * it.grow / sum_grow)
                    } else {
                        it.base
                    }
                } else if sum_shrink > 0.0 {
                    it.base
                        + fmath::round_i32(free as f32 * (it.shrink * it.base as f32) / sum_shrink)
                } else {
                    it.base
                };
            }
            // Clamp and freeze (§9.7 step 4d-e).
            let mut total = 0i64;
            for it in &mut items[a..z] {
                if it.frozen {
                    continue;
                }
                let c = it.target.min(it.max).max(it.min).max(0);
                total += (c - it.target) as i64;
                it.target = c;
            }
            for it in &mut items[a..z] {
                if it.frozen {
                    continue;
                }
                let at_min = it.target == it.min.max(0) && it.min > 0;
                let at_max = it.target == it.max;
                if total == 0 || (total > 0 && at_min) || (total < 0 && at_max) {
                    it.frozen = true;
                }
            }
            if total == 0 {
                break;
            }
        }
    }
    // §9.4 cross sizes: lay each item out at its main size.
    let cross_avail: Option<Au> = if row { spec_h } else { Some(fw) };
    for it in &mut items {
        let st = boxes[it.b as usize].style.clone();
        if row {
            let h = height_at(boxes, cx, it.b, fw, it.target, spec_h);
            it.cross = match block::size_px(cx, &st, &st.height, spec_h, it.bp_cross) {
                Some(h2) => h2,
                None => h,
            };
        } else {
            let w = match block::size_px(cx, &st, &st.width, Some(fw), it.bp_cross) {
                Some(w) => w,
                None => {
                    let stretch = matches!(align_of(&s, &st), Align::Stretch | Align::Normal)
                        && it.m[2].is_some()
                        && it.m[3].is_some();
                    let full =
                        (fw - it.bp_cross - it.m[2].unwrap_or(0) - it.m[3].unwrap_or(0)).max(0);
                    if stretch {
                        full
                    } else {
                        let (mn, mx) = intrinsic::content_widths(boxes, cx, it.b);
                        mx.min(full).max(mn)
                    }
                }
            };
            it.cross = w;
        }
    }
    let single = s.flex_wrap == FlexWrap::NoWrap;
    let mut line_cross: Vec<Au> = lines
        .iter()
        .map(|&(a, z)| {
            items[a..z]
                .iter()
                .map(|i| i.outer_cross())
                .max()
                .unwrap_or(0)
        })
        .collect();
    if single && let (Some(c), Some(l)) = (cross_avail, line_cross.first_mut()) {
        *l = c;
    }
    // §9.6 align-content (multi-line with a definite cross size): extra space to the lines.
    let total_cross: Au =
        line_cross.iter().sum::<Au>() + cross_gap * (lines.len() as Au - 1).max(0);
    let mut line_offsets: Vec<Au> = alloc::vec![0; lines.len()];
    {
        let free = cross_avail.map(|c| c - total_cross).unwrap_or(0);
        let n = lines.len() as Au;
        let (mut pos, between) = if single || free <= 0 || n == 0 {
            (0, 0)
        } else {
            match s.align_content {
                Justify::FlexStart => (0, 0),
                Justify::FlexEnd => (free, 0),
                Justify::Center => (free / 2, 0),
                Justify::SpaceBetween => (0, if n > 1 { free / (n - 1) } else { 0 }),
                Justify::SpaceAround => (free / (2 * n), free / n),
                Justify::SpaceEvenly => (free / (n + 1), free / (n + 1)),
                Justify::Normal | Justify::Stretch => {
                    let per = free / n;
                    for l in line_cross.iter_mut() {
                        *l += per;
                    }
                    (0, 0)
                }
            }
        };
        for (i, off) in line_offsets.iter_mut().enumerate() {
            *off = pos;
            pos += line_cross[i] + cross_gap + between;
        }
    }
    // Final sizes and positions.
    for (li, &(a, z)) in lines.iter().enumerate() {
        let gaps = main_gap * (z - a).saturating_sub(1) as Au;
        let used: Au = items[a..z].iter().map(|i| i.outer_target()).sum::<Au>() + gaps;
        let free = avail_main.map(|m| m - used).unwrap_or(0);
        let auto_margins = items[a..z]
            .iter()
            .map(|i| i.m[0].is_none() as i32 + i.m[1].is_none() as i32)
            .sum::<i32>();
        let n = (z - a) as Au;
        let (mut pos, between) = if auto_margins > 0 || free <= 0 {
            (0, 0)
        } else {
            match s.justify_content {
                Justify::FlexStart | Justify::Normal | Justify::Stretch => (0, 0),
                Justify::FlexEnd => (free, 0),
                Justify::Center => (free / 2, 0),
                Justify::SpaceBetween => (0, if n > 1 { free / (n - 1) } else { 0 }),
                Justify::SpaceAround => (free / (2 * n), free / n),
                Justify::SpaceEvenly => (free / (n + 1), free / (n + 1)),
            }
        };
        if free < 0 && matches!(s.justify_content, Justify::Center) && auto_margins == 0 {
            pos = free / 2;
        }
        let auto_share = if auto_margins > 0 && free > 0 {
            free / auto_margins
        } else {
            0
        };
        let main_len = avail_main.unwrap_or(used);
        for it in &mut items[a..z] {
            let st = boxes[it.b as usize].style.clone();
            let ms = it.m[0].unwrap_or(auto_share);
            let me = it.m[1].unwrap_or(auto_share);
            let start = pos + ms;
            pos = start + it.target + it.bp_main + me + main_gap + between;
            // Cross: stretch or align.
            let lc = line_cross[li];
            let al = align_of(&s, &st);
            let cross_auto = if row {
                matches!(st.height, Size::Auto)
            } else {
                matches!(st.width, Size::Auto)
            };
            let cm_start = it.m[2];
            let cm_end = it.m[3];
            let mut cross = it.cross;
            if matches!(al, Align::Stretch | Align::Normal)
                && cross_auto
                && cm_start.is_some()
                && cm_end.is_some()
            {
                cross = (lc - it.bp_cross - cm_start.unwrap_or(0) - cm_end.unwrap_or(0)).max(0);
            }
            let outer = cross + it.bp_cross + cm_start.unwrap_or(0) + cm_end.unwrap_or(0);
            let room = lc - outer;
            let coff = if cm_start.is_none() && cm_end.is_none() {
                room / 2
            } else if cm_start.is_none() {
                room
            } else if cm_end.is_none() {
                0
            } else {
                match al {
                    Align::FlexEnd => room,
                    Align::Center => room / 2,
                    _ => 0,
                }
            };
            let cross_pos = line_offsets[li] + coff + cm_start.unwrap_or(0);
            // Lay out at the final size.
            let (cw, ch) = if row {
                (it.target, Some(cross))
            } else {
                (cross, Some(it.target))
            };
            let height_forced = if row {
                if cross_auto && cross != it.cross {
                    Some(cross)
                } else {
                    ch.filter(|_| !cross_auto)
                }
            } else {
                ch
            };
            block::layout_sized(boxes, cx, it.b, fw, cw, height_forced, spec_h);
            if row && height_forced.is_none() {
                // Its own height was used (not stretched).
                let bx = &boxes[it.b as usize];
                it.cross = bx.h - it.bp_cross;
            }
            let main_start = if reverse {
                main_len - start - it.target - it.bp_main
            } else {
                start
            };
            let (nx, ny) = if row {
                (fx + main_start, fy + cross_pos)
            } else {
                (fx + cross_pos, fy + main_start)
            };
            let bx = &boxes[it.b as usize];
            let (ox, oy) = (bx.x, bx.y);
            block::translate(boxes, it.b, nx - ox, ny - oy);
        }
    }
    // The container's content height (auto: the lines' cross sizes for rows, the items' main
    // sizes for columns).
    let content_h = if row {
        let used =
            line_offsets.last().copied().unwrap_or(0) + line_cross.last().copied().unwrap_or(0);
        spec_h.unwrap_or(used)
    } else {
        match spec_h {
            Some(h) => h,
            None => lines
                .iter()
                .map(|&(a, z)| {
                    items[a..z].iter().map(|i| i.outer_target()).sum::<Au>()
                        + main_gap * (z - a).saturating_sub(1) as Au
                })
                .max()
                .unwrap_or(0),
        }
    };
    let bo = boxes[f as usize].border;
    let p = boxes[f as usize].padding;
    let bp_h = bo[0] + bo[2] + p[0] + p[2];
    let h = block::clamp_height(cx, &s, content_h, None, bp_h);
    boxes[f as usize].h = h + bp_h;
    // Baseline: the first item's.
    let fb = ids.first().and_then(|&b| boxes[b as usize].first_baseline);
    boxes[f as usize].first_baseline = fb;
    boxes[f as usize].last_baseline = fb;
}

fn align_of(container: &Style, item: &Style) -> Align {
    match item.align_self {
        Align::Auto => container.align_items,
        a => a,
    }
}
