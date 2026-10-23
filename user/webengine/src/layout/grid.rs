//! Grid layout, a subset of CSS Grid Layout 1: explicit track lists (lengths, percentages,
//! `fr`, `auto`, `min-content`, `max-content`, `minmax()`, `fit-content()`, `repeat()` with a
//! count or `auto-fill`/`auto-fit`), implicit tracks (`grid-auto-rows/columns`), line-number
//! and `span` placement, sparse row-major auto-placement (§8.5), gaps, the track sizing
//! algorithm (§11) in simplified form (base sizes from the items' min-content contributions,
//! growth limits from their max-content ones, then free space to the auto tracks, then `fr`
//! tracks share what is left, §11.7), and `align-self`/stretch for items.
//!
//! Not done: named lines and `grid-template-areas` (such items are auto-placed), dense
//! packing, column flow (laid out as row flow), subgrid, baseline alignment.

use alloc::rc::Rc;
use alloc::vec::Vec;

use super::block;
use super::*;

const MAX_TRACKS: usize = 1000;
const MAX_ROWS: usize = 20_000;

struct Item {
    b: BoxId,
    col: usize,
    cspan: usize,
    row: usize,
    rspan: usize,
}

fn gstyle(s: &Style) -> Rc<GridStyle> {
    s.grid
        .clone()
        .unwrap_or_else(|| Rc::new(GridStyle::default()))
}

fn fixed(cx: &Cx, l: &LP, base: Au) -> Au {
    cx.lp(l, base).max(0)
}

/// The explicit tracks with any auto-repeat expanded for an available size (§7.2.3.2).
fn expand(cx: &Cx, list: &TrackList, avail: Option<Au>, gap: Au) -> Vec<Track> {
    let mut out = list.tracks.clone();
    if let Some((_, at, rep)) = &list.auto_repeat {
        let mut n = 1usize;
        if let Some(avail) = avail {
            // Each repetition's size: its tracks' fixed sizes (max, else min), at least 1 px.
            let one: Au = rep
                .iter()
                .map(|t| match (t.max, t.min) {
                    (TMax::Fixed(l), _) => fixed(cx, &l, avail),
                    (_, TMin::Fixed(l)) => fixed(cx, &l, avail),
                    _ => 0,
                })
                .sum::<Au>()
                + gap * rep.len() as Au;
            let others: Au = list
                .tracks
                .iter()
                .map(|t| match t.max {
                    TMax::Fixed(l) => fixed(cx, &l, avail) + gap,
                    _ => gap,
                })
                .sum();
            if one > 0 {
                n = (((avail - others + gap).max(0)) / one).max(1) as usize;
            }
        }
        let n = n.min(MAX_TRACKS / rep.len().max(1));
        let at = (*at).min(out.len());
        let mut ins = Vec::with_capacity(n * rep.len());
        for _ in 0..n {
            ins.extend(rep.iter().copied());
        }
        out.splice(at..at, ins);
    }
    out.truncate(MAX_TRACKS);
    out
}

/// A placement pair resolved against `n` explicit lines: (start index or None for auto, span).
fn resolve(lines: (GridLine, GridLine), n: usize) -> (Option<usize>, usize) {
    let idx = |l: i32| -> i64 {
        if l > 0 {
            l as i64 - 1
        } else {
            n as i64 + 1 + l as i64
        }
    };
    match lines {
        (GridLine::Line(a), GridLine::Line(b)) => {
            let (a, b) = (idx(a), idx(b));
            let (s, e) = if a <= b { (a, b) } else { (b, a) };
            let span = (e - s).max(1) as usize;
            (Some(s.max(0) as usize), span)
        }
        (GridLine::Line(a), GridLine::Span(k)) => (Some(idx(a).max(0) as usize), k as usize),
        (GridLine::Line(a), GridLine::Auto) => (Some(idx(a).max(0) as usize), 1),
        (GridLine::Span(k), GridLine::Line(b)) => {
            let e = idx(b);
            (Some((e - k as i64).max(0) as usize), k as usize)
        }
        (GridLine::Auto, GridLine::Line(b)) => (Some((idx(b) - 1).max(0) as usize), 1),
        (GridLine::Span(k), _) | (_, GridLine::Span(k)) => (None, k as usize),
        _ => (None, 1),
    }
}

/// Place the items (§8.5, sparse, row by row).
fn place(boxes: &[LBox], ids: &[BoxId], ncols_explicit: usize) -> (Vec<Item>, usize, usize) {
    let mut ncols = ncols_explicit.max(1);
    // Columns implied by definite placements widen the grid first.
    let mut pre: Vec<(BoxId, Option<usize>, usize, Option<usize>, usize)> = Vec::new();
    for &b in ids {
        let g = gstyle(&boxes[b as usize].style);
        let (c, cs) = resolve(g.col, ncols_explicit);
        let (r, rs) = resolve(g.row, 0);
        let cs = cs.clamp(1, MAX_TRACKS);
        let rs = rs.clamp(1, 1000);
        if let Some(c) = c {
            ncols = ncols.max((c + cs).min(MAX_TRACKS));
        } else {
            ncols = ncols.max(cs.min(MAX_TRACKS));
        }
        pre.push((b, c, cs, r, rs));
    }
    let mut occ: Vec<Vec<bool>> = Vec::new();
    let free = |occ: &Vec<Vec<bool>>, r: usize, c: usize, rs: usize, cs: usize| -> bool {
        if c + cs > ncols {
            return false;
        }
        (r..r + rs).all(|rr| occ.get(rr).is_none_or(|row| (c..c + cs).all(|cc| !row[cc])))
    };
    let mark = |occ: &mut Vec<Vec<bool>>, r: usize, c: usize, rs: usize, cs: usize| {
        while occ.len() < r + rs {
            occ.push(alloc::vec![false; ncols]);
        }
        for row in occ.iter_mut().skip(r).take(rs) {
            for x in row.iter_mut().skip(c).take(cs) {
                *x = true;
            }
        }
    };
    let mut items: Vec<Option<Item>> = (0..pre.len()).map(|_| None).collect();
    // 1. Items with a definite row and column.
    for (i, &(b, c, cs, r, rs)) in pre.iter().enumerate() {
        if let (Some(c), Some(r)) = (c, r) {
            let r = r.min(MAX_ROWS);
            mark(&mut occ, r, c, rs, cs);
            items[i] = Some(Item {
                b,
                col: c,
                cspan: cs,
                row: r,
                rspan: rs,
            });
        }
    }
    // 2. The rest, in order, with a cursor.
    let (mut cr, mut cc) = (0usize, 0usize);
    for (i, &(b, c, cs, r, rs)) in pre.iter().enumerate() {
        if items[i].is_some() {
            continue;
        }
        let mut placed = None;
        if let Some(r) = r {
            // A definite row: the first free column in it.
            let r = r.min(MAX_ROWS);
            for c0 in 0..=ncols.saturating_sub(cs) {
                if free(&occ, r, c0, rs, cs) {
                    placed = Some((r, c0));
                    break;
                }
            }
            placed = placed.or(Some((r, 0)));
        } else {
            let mut guard = 0;
            loop {
                guard += 1;
                if guard > MAX_ROWS * 4 || cr >= MAX_ROWS {
                    placed = Some((cr.min(MAX_ROWS), 0));
                    break;
                }
                let c0 = match c {
                    Some(c) => {
                        if cc > c {
                            cr += 1;
                        }
                        c
                    }
                    None => cc,
                };
                if c0 + cs <= ncols && free(&occ, cr, c0, rs, cs) {
                    placed = Some((cr, c0));
                    break;
                }
                if c.is_some() {
                    cr += 1;
                    cc = 0;
                } else {
                    cc += 1;
                    if cc + cs > ncols {
                        cc = 0;
                        cr += 1;
                    }
                }
            }
        }
        let (r0, c0) = placed.unwrap();
        mark(&mut occ, r0, c0, rs, cs);
        if r.is_none() {
            cr = r0;
            cc = c0 + cs;
        }
        items[i] = Some(Item {
            b,
            col: c0,
            cspan: cs,
            row: r0,
            rspan: rs,
        });
    }
    let items: Vec<Item> = items.into_iter().flatten().collect();
    let nrows = items.iter().map(|i| i.row + i.rspan).max().unwrap_or(0);
    (items, ncols, nrows)
}

/// The track sizes along one axis (§11.4-11.7, simplified). `contrib(item)` gives an item's
/// (min, max) outer contribution; `avail` the space if definite.
fn size_tracks(
    cx: &Cx,
    tracks: &[Track],
    spans: &[(usize, usize, Au, Au)],
    avail: Option<Au>,
    gap: Au,
    stretch: bool,
) -> Vec<Au> {
    let n = tracks.len();
    let base_ref = avail.unwrap_or(0);
    let mut base = alloc::vec![0 as Au; n];
    let mut limit = alloc::vec![0 as Au; n];
    let mut flex = alloc::vec![0f32; n];
    for (i, t) in tracks.iter().enumerate() {
        base[i] = match t.min {
            TMin::Fixed(l) => fixed(cx, &l, base_ref),
            _ => 0,
        };
        limit[i] = match t.max {
            TMax::Fixed(l) => fixed(cx, &l, base_ref),
            TMax::Fr(f) => {
                flex[i] = f;
                0
            }
            _ => 0,
        };
    }
    // Single-span items first, then spanning ones (§11.5).
    for &(start, _, mn, mx) in spans.iter().filter(|s| s.1 == 1) {
        let t = &tracks[start];
        if !matches!(t.min, TMin::Fixed(_)) {
            base[start] = base[start].max(if t.min == TMin::MaxContent { mx } else { mn });
        }
        match t.max {
            TMax::Auto | TMax::MaxContent | TMax::Fr(_) => limit[start] = limit[start].max(mx),
            TMax::MinContent => limit[start] = limit[start].max(mn),
            TMax::FitContent(l) => {
                limit[start] = limit[start].max(mx.min(fixed(cx, &l, base_ref)).max(mn))
            }
            TMax::Fixed(_) => {}
        }
    }
    for &(start, span, mn, mx) in spans.iter().filter(|s| s.1 > 1) {
        let end = (start + span).min(n);
        let r = start..end;
        let gaps = gap * (r.len() as Au - 1).max(0);
        let growable: Vec<usize> = r
            .clone()
            .filter(|&i| !matches!(tracks[i].min, TMin::Fixed(_)))
            .collect();
        let have: Au = base[r.clone()].iter().sum::<Au>() + gaps;
        if mn > have && !growable.is_empty() {
            let per = (mn - have) / growable.len() as Au;
            for &i in &growable {
                base[i] += per;
            }
        }
        let have: Au = limit[r.clone()]
            .iter()
            .zip(base[r.clone()].iter())
            .map(|(l, b)| *l.max(b))
            .sum::<Au>()
            + gaps;
        let flexless: Vec<usize> = r
            .filter(|&i| flex[i] == 0.0 && !matches!(tracks[i].max, TMax::Fixed(_)))
            .collect();
        if mx > have && !flexless.is_empty() {
            let per = (mx - have) / flexless.len() as Au;
            for &i in &flexless {
                limit[i] += per;
            }
        }
    }
    for i in 0..n {
        limit[i] = limit[i].max(base[i]);
    }
    let gaps = gap * (n as Au - 1).max(0);
    let mut size = base.clone();
    let any_flex = flex.iter().any(|&f| f > 0.0);
    match avail {
        Some(avail) => {
            // §11.6 maximize tracks: grow the non-flexible ones toward their limits.
            let mut free = avail - gaps - size.iter().sum::<Au>();
            for _ in 0..8 {
                let open: Vec<usize> = (0..n)
                    .filter(|&i| flex[i] == 0.0 && size[i] < limit[i])
                    .collect();
                if free <= 0 || open.is_empty() {
                    break;
                }
                let per = (free / open.len() as Au).max(1);
                for &i in &open {
                    let g = per.min(limit[i] - size[i]).min(free);
                    size[i] += g;
                    free -= g;
                }
            }
            if any_flex {
                // §11.7 expand flexible tracks: the fr size, skipping tracks whose base is bigger.
                let mut inflexible = alloc::vec![false; n];
                for _ in 0..n.max(1) {
                    let used: Au = (0..n)
                        .filter(|&i| flex[i] == 0.0 || inflexible[i])
                        .map(|i| size[i])
                        .sum();
                    let sum_f: f32 = (0..n)
                        .filter(|&i| flex[i] > 0.0 && !inflexible[i])
                        .map(|i| flex[i])
                        .sum();
                    let left = (avail - gaps - used).max(0);
                    let fr = if sum_f > 0.0 {
                        left as f32 / sum_f.max(1.0)
                    } else {
                        0.0
                    };
                    let mut changed = false;
                    for i in 0..n {
                        if flex[i] > 0.0 && !inflexible[i] && (fr * flex[i]) < base[i] as f32 {
                            inflexible[i] = true;
                            changed = true;
                        }
                    }
                    if !changed {
                        for i in 0..n {
                            if flex[i] > 0.0 && !inflexible[i] {
                                size[i] = (fr * flex[i]) as Au;
                            }
                        }
                        break;
                    }
                }
            } else if stretch && free > 0 {
                // §11.8 stretch auto tracks.
                let autos: Vec<usize> = (0..n)
                    .filter(|&i| matches!(tracks[i].max, TMax::Auto))
                    .collect();
                if !autos.is_empty() {
                    let per = free / autos.len() as Au;
                    for &i in &autos {
                        size[i] += per;
                    }
                }
            }
        }
        None => {
            // Indefinite: flexible tracks at their max-content (as at max-content size).
            for i in 0..n {
                size[i] = limit[i];
            }
        }
    }
    size
}

fn implicit(tracks: &[Track], auto: Track, n: usize) -> Vec<Track> {
    let mut v = tracks.to_vec();
    while v.len() < n {
        v.push(auto);
    }
    v
}

/// Item contributions along columns: (start, span, min-content, max-content) outer widths.
fn col_spans(boxes: &[LBox], cx: &Cx, items: &[Item]) -> Vec<(usize, usize, Au, Au)> {
    items
        .iter()
        .map(|it| {
            let (mn, mx) = intrinsic::outer_widths(boxes, cx, it.b);
            (it.col, it.cspan, mn, mx)
        })
        .collect()
}

/// (min-content, max-content) width of a grid container's content.
pub fn intrinsic(boxes: &[LBox], cx: &Cx, g: BoxId) -> (Au, Au) {
    let s = &boxes[g as usize].style;
    let gs = gstyle(s);
    let gap = cx.lp(&s.column_gap, 0);
    let ids: Vec<BoxId> = boxes[g as usize]
        .children
        .iter()
        .copied()
        .filter(|&c| !boxes[c as usize].out_of_flow)
        .collect();
    let cols = expand(cx, &gs.columns, None, gap);
    let (items, ncols, _) = place(boxes, &ids, cols.len());
    let tracks = implicit(&cols, gs.auto_columns, ncols);
    let spans = col_spans(boxes, cx, &items);
    let mx = size_tracks(cx, &tracks, &spans, None, gap, false);
    // Min-content: the tracks' base sizes.
    let mins: Vec<(usize, usize, Au, Au)> =
        spans.iter().map(|&(a, b, mn, _)| (a, b, mn, mn)).collect();
    let mn = size_tracks(cx, &tracks, &mins, None, gap, false);
    let gaps = gap * (tracks.len() as Au - 1).max(0);
    let a = mn.iter().sum::<Au>() + gaps;
    let z = mx.iter().sum::<Au>() + gaps;
    (a, z.max(a))
}

/// Lay out grid container `g` (x, y, w and edges set). `spec_h`: definite content height.
pub fn layout_grid(boxes: &mut Vec<LBox>, cx: &Cx, g: BoxId, spec_h: Option<Au>) {
    let s = boxes[g as usize].style.clone();
    let gs = gstyle(&s);
    let (gx, gy, gw, _) = boxes[g as usize].content();
    let col_gap = cx.lp(&s.column_gap, gw);
    let row_gap = cx.lp(&s.row_gap, spec_h.unwrap_or(0));
    for &c in &boxes[g as usize].children.clone() {
        if boxes[c as usize].out_of_flow {
            boxes[c as usize].static_pos = (gx, gy);
        }
    }
    let mut ids: Vec<BoxId> = boxes[g as usize]
        .children
        .iter()
        .copied()
        .filter(|&c| !boxes[c as usize].out_of_flow)
        .collect();
    ids.sort_by_key(|&c| boxes[c as usize].style.order);
    let cols = expand(cx, &gs.columns, Some(gw), col_gap);
    let (items, ncols, nrows) = place(boxes, &ids, cols.len());
    let col_tracks = implicit(&cols, gs.auto_columns, ncols);
    let spans = col_spans(boxes, cx, &items);
    let mut widths = size_tracks(cx, &col_tracks, &spans, Some(gw), col_gap, true);
    // auto-fit: empty repeated tracks collapse (§7.2.3.2).
    if let Some((AutoRepeat::Fit, at, rep)) = &gs.columns.auto_repeat {
        let rep_n = cols.len() - gs.columns.tracks.len();
        let _ = rep;
        for (i, w) in widths.iter_mut().enumerate().skip(*at).take(rep_n) {
            if !items.iter().any(|it| it.col <= i && i < it.col + it.cspan) {
                *w = 0;
            }
        }
    }
    let mut col_x = alloc::vec![gx; ncols + 1];
    for i in 0..ncols {
        col_x[i + 1] = col_x[i] + widths[i] + if i + 1 < ncols { col_gap } else { 0 };
    }
    let area_w = |it: &Item| -> Au {
        let e = (it.col + it.cspan).min(ncols);
        (col_x[e] - col_x[it.col] - if e < ncols { col_gap } else { 0 }).max(0)
    };
    // Item heights at their area widths.
    let mut heights: Vec<Au> = Vec::with_capacity(items.len());
    let mut cws: Vec<Au> = Vec::with_capacity(items.len());
    for it in &items {
        cx.work.set(cx.work.get() + 1);
        let st = boxes[it.b as usize].style.clone();
        let aw = area_w(it);
        let (m, bo, p) = block::edges(cx, &st, aw);
        let bp = bo[1] + bo[3] + p[1] + p[3];
        let room = (aw - m[1].unwrap_or(0) - m[3].unwrap_or(0) - bp).max(0);
        let cw = match block::specified_width(boxes, cx, it.b, aw, bp) {
            Some(w) => w,
            None if matches!(boxes[it.b as usize].kind, Kind::Replaced(_)) => {
                let (_, mx) = intrinsic::content_widths(boxes, cx, it.b);
                mx.min(room)
            }
            None => room,
        };
        let cw = block::clamp_width(cx, &st, cw, aw, bp, boxes, it.b);
        block::layout_sized(boxes, cx, it.b, aw, cw, None, None);
        let bx = &boxes[it.b as usize];
        heights.push(bx.margin_box_h());
        cws.push(cw);
    }
    let row_tracks = implicit(&expand(cx, &gs.rows, spec_h, row_gap), gs.auto_rows, nrows);
    let rspans: Vec<(usize, usize, Au, Au)> = items
        .iter()
        .zip(heights.iter())
        .map(|(it, &h)| (it.row, it.rspan, h, h))
        .collect();
    let row_h = size_tracks(cx, &row_tracks, &rspans, spec_h, row_gap, true);
    let nr = row_tracks.len();
    let mut row_y = alloc::vec![gy; nr + 1];
    for i in 0..nr {
        row_y[i + 1] = row_y[i] + row_h[i] + if i + 1 < nr { row_gap } else { 0 };
    }
    // Final placement, with align-self (stretch by default for auto heights).
    for (k, it) in items.iter().enumerate() {
        let st = boxes[it.b as usize].style.clone();
        let e = (it.row + it.rspan).min(nr);
        let ah = (row_y[e] - row_y[it.row] - if e < nr { row_gap } else { 0 }).max(0);
        let aw = area_w(it);
        let (m, bo, p) = block::edges(cx, &st, aw);
        let bp_h = bo[0] + bo[2] + p[0] + p[2];
        let al = match st.align_self {
            Align::Auto => s.align_items,
            a => a,
        };
        let stretch = matches!(al, Align::Stretch | Align::Normal)
            && matches!(st.height, Size::Auto)
            && m[0].is_some()
            && m[2].is_some()
            && !matches!(boxes[it.b as usize].kind, Kind::Replaced(_));
        if stretch {
            let ch = (ah - m[0].unwrap_or(0) - m[2].unwrap_or(0) - bp_h).max(0);
            block::layout_sized(boxes, cx, it.b, aw, cws[k], Some(ch), None);
        } else {
            block::layout_sized(boxes, cx, it.b, aw, cws[k], None, None);
        }
        let h = boxes[it.b as usize].margin_box_h();
        let dy = match al {
            Align::Center => (ah - h) / 2,
            Align::FlexEnd => ah - h,
            _ => 0,
        }
        .max(0);
        let w = boxes[it.b as usize].margin_box_w();
        let dx = if m[3].is_none() && m[1].is_none() {
            ((aw - w) / 2).max(0)
        } else if m[3].is_none() {
            (aw - w).max(0)
        } else {
            0
        };
        let bx = &boxes[it.b as usize];
        let (nx, ny) = (
            col_x[it.col] + dx + bx.margin[3],
            row_y[it.row] + dy + bx.margin[0],
        );
        let (ox, oy) = (bx.x, bx.y);
        block::translate(boxes, it.b, nx - ox, ny - oy);
    }
    let content_h = spec_h.unwrap_or(row_y[nr] - gy);
    let bo = boxes[g as usize].border;
    let p = boxes[g as usize].padding;
    let bp_h = bo[0] + bo[2] + p[0] + p[2];
    let h = block::clamp_height(cx, &s, content_h, None, bp_h);
    boxes[g as usize].h = h + bp_h;
    let fb = items
        .iter()
        .find(|i| i.row == 0)
        .and_then(|i| boxes[i.b as usize].first_baseline);
    boxes[g as usize].first_baseline = fb;
    boxes[g as usize].last_baseline = fb;
}
