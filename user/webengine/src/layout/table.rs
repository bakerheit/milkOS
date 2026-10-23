//! Tables (CSS 2.1 §17): the grid of rows and cells with colspan/rowspan, column widths by
//! the automatic layout algorithm (§17.5.2.2, as browsers implement it: per-column min/max
//! content widths, fixed and percentage widths, spanning cells distributed over their
//! columns, then the table width shared out), row heights from the cells, cell vertical
//! alignment, captions, `border-spacing`, and `border-collapse: collapse` approximated by
//! overlapping adjacent cell borders.

use alloc::vec::Vec;

use super::*;

struct Cell {
    b: BoxId,
    row: usize,
    col: usize,
    colspan: usize,
    rowspan: usize,
}

struct Grid {
    /// Rows in display order (thead, then bodies and loose rows, then tfoot).
    rows: Vec<BoxId>,
    /// The row groups (and which rows they hold) for their boxes.
    groups: Vec<(BoxId, usize, usize)>,
    cells: Vec<Cell>,
    ncols: usize,
    captions: Vec<BoxId>,
    /// Column widths from `<col>`/`<colgroup>`.
    col_styles: Vec<Option<Rc<Style>>>,
}

const MAX_COLS: usize = 1000;
const MAX_ROWS: usize = 100_000;

fn grid(boxes: &[LBox], t: BoxId) -> Grid {
    let mut heads = Vec::new();
    let mut bodies = Vec::new();
    let mut feet = Vec::new();
    let mut captions = Vec::new();
    let mut col_styles: Vec<Option<Rc<Style>>> = Vec::new();
    for &c in &boxes[t as usize].children {
        let cb = &boxes[c as usize];
        match cb.kind {
            Kind::Caption => captions.push(c),
            Kind::Column { span } => {
                if cb.children.is_empty() {
                    for _ in 0..span {
                        col_styles.push(Some(cb.style.clone()));
                    }
                } else {
                    for &cc in &cb.children {
                        if let Kind::Column { span } = boxes[cc as usize].kind {
                            for _ in 0..span {
                                col_styles.push(Some(boxes[cc as usize].style.clone()));
                            }
                        }
                    }
                }
            }
            Kind::RowGroup => match cb.style.display {
                Display::TableHeaderGroup if heads.is_empty() => heads.push(c),
                Display::TableFooterGroup if feet.is_empty() => feet.push(c),
                _ => bodies.push(c),
            },
            Kind::Row => bodies.push(c),
            _ => {}
        }
    }
    let mut rows = Vec::new();
    let mut groups = Vec::new();
    for g in heads.iter().chain(bodies.iter()).chain(feet.iter()) {
        let start = rows.len();
        if matches!(boxes[*g as usize].kind, Kind::Row) {
            rows.push(*g);
        } else {
            for &r in &boxes[*g as usize].children {
                if matches!(boxes[r as usize].kind, Kind::Row) && rows.len() < MAX_ROWS {
                    rows.push(r);
                }
            }
            groups.push((*g, start, rows.len()));
        }
    }
    // Place cells, skipping slots taken by rowspans from above.
    let mut cells = Vec::new();
    let mut taken: Vec<u32> = Vec::new(); // per column: rows still covered by a rowspan
    let mut ncols = 0;
    for (ri, &r) in rows.iter().enumerate() {
        let mut col = 0;
        for &c in &boxes[r as usize].children {
            let Kind::Cell { colspan, rowspan } = boxes[c as usize].kind else {
                continue;
            };
            while col < taken.len() && taken[col] > 0 {
                col += 1;
            }
            if col >= MAX_COLS {
                break;
            }
            let colspan = (colspan as usize).min(MAX_COLS - col);
            let rowspan = (rowspan as usize).min(rows.len() - ri).max(1);
            if taken.len() < col + colspan {
                taken.resize(col + colspan, 0);
            }
            for t in taken.iter_mut().skip(col).take(colspan) {
                *t = rowspan as u32;
            }
            cells.push(Cell {
                b: c,
                row: ri,
                col,
                colspan,
                rowspan,
            });
            col += colspan;
            ncols = ncols.max(col);
        }
        for t in taken.iter_mut() {
            *t = t.saturating_sub(1);
        }
    }
    ncols = ncols.max(col_styles.len().min(MAX_COLS));
    Grid {
        rows,
        groups,
        cells,
        ncols,
        captions,
        col_styles,
    }
}

/// (horizontal, vertical) spacing between cells, Au; negative in the collapsing model (the
/// borders overlap).
fn spacing(boxes: &[LBox], cx: &Cx, t: BoxId, g: &Grid) -> (Au, Au) {
    let s = &boxes[t as usize].style;
    if s.border_collapse {
        let mut h = 0;
        let mut v = 0;
        for c in &g.cells {
            let cs = &boxes[c.b as usize].style;
            h = h.max(cx.au(cs.border_width[1].min(cs.border_width[3])));
            v = v.max(cx.au(cs.border_width[0].min(cs.border_width[2])));
        }
        (-h, -v)
    } else {
        (cx.au(s.border_spacing.0), cx.au(s.border_spacing.1))
    }
}

struct Cols {
    min: Vec<Au>,
    max: Vec<Au>,
    /// A fixed width (from a cell or a column), Au.
    fixed: Vec<Option<Au>>,
    pct: Vec<f32>,
}

fn columns(boxes: &[LBox], cx: &Cx, g: &Grid) -> Cols {
    let n = g.ncols;
    let mut c = Cols {
        min: alloc::vec![0; n],
        max: alloc::vec![0; n],
        fixed: alloc::vec![None; n],
        pct: alloc::vec![0.0; n],
    };
    for (i, cs) in g.col_styles.iter().enumerate().take(n) {
        if let Some(s) = cs
            && let Size::L(l) = s.width
        {
            if l.has_pct() {
                c.pct[i] = c.pct[i].max(l.pct);
            } else {
                let w = cx.lp(&l, 0);
                c.fixed[i] = Some(w);
                c.min[i] = c.min[i].max(0);
                c.max[i] = c.max[i].max(w);
            }
        }
    }
    let cell_widths = |cell: &Cell| -> (Au, Au, Option<Au>, f32) {
        let s = &boxes[cell.b as usize].style;
        let (_, bo, p) = block::edges(cx, s, 0);
        let bp = bo[1] + bo[3] + p[1] + p[3];
        let (mn, mx) = intrinsic::content_widths(boxes, cx, cell.b);
        let mut fixed = None;
        let mut pct = 0.0;
        if let Size::L(l) = s.width {
            if l.has_pct() {
                pct = l.pct;
            } else if let Some(w) = block::size_px(cx, s, &s.width, Some(0), bp) {
                fixed = Some(w + bp);
            }
        }
        let mn = mn + bp;
        let mx = match fixed {
            Some(f) => f.max(mn),
            None => mx + bp,
        };
        (mn, mx.max(mn), fixed, pct)
    };
    let mut spanning = Vec::new();
    for cell in &g.cells {
        let (mn, mx, fixed, pct) = cell_widths(cell);
        if cell.colspan == 1 {
            let i = cell.col;
            c.min[i] = c.min[i].max(mn);
            c.max[i] = c.max[i].max(mx);
            if let Some(f) = fixed {
                c.fixed[i] = Some(c.fixed[i].unwrap_or(0).max(f));
            }
            if pct > 0.0 {
                c.pct[i] = c.pct[i].max(pct);
            }
        } else {
            spanning.push((cell.col, cell.colspan, mn, mx));
        }
    }
    for i in 0..n {
        if let Some(f) = c.fixed[i] {
            c.max[i] = c.max[i].max(f).max(c.min[i]);
        }
        c.max[i] = c.max[i].max(c.min[i]);
    }
    // Spanning cells: spread what their columns lack, by the columns' max widths.
    spanning.sort_by_key(|s| s.1);
    for (col, span, mn, mx) in spanning {
        let r = col..(col + span).min(n);
        let sum_min: Au = c.min[r.clone()].iter().sum();
        let sum_max: Au = c.max[r.clone()].iter().sum();
        let count = r.len().max(1) as i64;
        if mn > sum_min {
            let extra = (mn - sum_min) as i64;
            for i in r.clone() {
                let share = if sum_max > 0 {
                    extra * c.max[i] as i64 / sum_max as i64
                } else {
                    extra / count
                };
                c.min[i] += share as Au;
                c.max[i] = c.max[i].max(c.min[i]);
            }
        }
        let sum_max: Au = c.max[r.clone()].iter().sum();
        if mx > sum_max {
            let extra = (mx - sum_max) as i64;
            for i in r.clone() {
                let share = if sum_max > 0 {
                    extra * c.max[i] as i64 / sum_max as i64
                } else {
                    extra / count
                };
                c.max[i] += share as Au;
            }
        }
    }
    c
}

/// The table's (min, max) content width: columns plus spacing, or its captions'.
pub fn intrinsic(boxes: &[LBox], cx: &Cx, t: BoxId) -> (Au, Au) {
    let g = grid(boxes, t);
    let c = columns(boxes, cx, &g);
    let (hs, _) = spacing(boxes, cx, t, &g);
    let sp = if g.ncols > 0 {
        hs * (g.ncols as Au + 1)
    } else {
        0
    };
    let sp = sp.max(0);
    let mut mn: Au = c.min.iter().sum::<Au>() + sp;
    let mut mx: Au = c.max.iter().sum::<Au>() + sp;
    for &cap in &g.captions {
        let (a, z) = intrinsic::outer_widths(boxes, cx, cap);
        mn = mn.max(a);
        mx = mx.max(z.min(a.max(mx)));
    }
    (mn, mx.max(mn))
}

/// Share the table width `w` out to the columns.
fn distribute(c: &Cols, w: Au) -> Vec<Au> {
    let n = c.min.len();
    let mut out = alloc::vec![0; n];
    if n == 0 {
        return out;
    }
    // Percentage columns first.
    let mut rest = w;
    let mut auto_cols = Vec::new();
    for i in 0..n {
        if c.pct[i] > 0.0 {
            out[i] = fmath::round_i32(w as f32 * c.pct[i] / 100.0).max(c.min[i]);
            rest -= out[i];
        } else {
            auto_cols.push(i);
        }
    }
    if auto_cols.is_empty() {
        if rest > 0 {
            // Grow the percentage columns proportionally.
            let total: Au = out.iter().sum::<Au>().max(1);
            for o in out.iter_mut() {
                *o += (rest as i64 * *o as i64 / total as i64) as Au;
            }
        }
        return out;
    }
    let sum_min: Au = auto_cols.iter().map(|&i| c.min[i]).sum();
    let sum_max: Au = auto_cols.iter().map(|&i| c.max[i]).sum();
    if rest >= sum_max {
        let extra = (rest - sum_max) as i64;
        // Extra space to auto (non-fixed) columns, by their max widths.
        let growable: Vec<usize> = auto_cols
            .iter()
            .copied()
            .filter(|&i| c.fixed[i].is_none())
            .collect();
        let targets = if growable.is_empty() {
            &auto_cols
        } else {
            &growable
        };
        let tmax: i64 = targets.iter().map(|&i| c.max[i] as i64).sum();
        for &i in &auto_cols {
            out[i] = c.max[i];
        }
        let mut given = 0i64;
        for (k, &i) in targets.iter().enumerate() {
            let share = if k + 1 == targets.len() {
                extra - given
            } else if tmax > 0 {
                extra * c.max[i] as i64 / tmax
            } else {
                extra / targets.len() as i64
            };
            given += share;
            out[i] += share as Au;
        }
    } else if rest >= sum_min && sum_max > sum_min {
        let num = (rest - sum_min) as i64;
        let den = (sum_max - sum_min) as i64;
        for &i in &auto_cols {
            out[i] = c.min[i] + ((c.max[i] - c.min[i]) as i64 * num / den) as Au;
        }
    } else {
        for &i in &auto_cols {
            out[i] = c.min[i];
        }
    }
    out
}

/// Lay out table `t` (x, y, w and edges set). `spec_h`: a definite content height.
pub fn layout_table(boxes: &mut Vec<LBox>, cx: &Cx, t: BoxId, spec_h: Option<Au>) {
    let g = grid(boxes, t);
    let (tx, ty, tw, _) = boxes[t as usize].content();
    let (hs, vs) = spacing(boxes, cx, t, &g);
    let mut y = ty;
    let caption_bottom = boxes[t as usize].style.caption_bottom;
    let layout_caption = |boxes: &mut Vec<LBox>, y: &mut Au, cap: BoxId| {
        let mut fl = block::Floats::new();
        let (m, _) = block::layout_block(boxes, cx, cap, tx, tw, None, *y, &mut fl, false);
        let top = boxes[cap as usize].margin[0];
        block::translate(boxes, cap, 0, top);
        *y = boxes[cap as usize].y + boxes[cap as usize].h + m.value().max(0);
    };
    if !caption_bottom {
        for &cap in &g.captions {
            layout_caption(boxes, &mut y, cap);
        }
    }
    let n = g.ncols;
    let cols = columns(boxes, cx, &g);
    let edge_sp = hs.max(0);
    let inner = (tw - edge_sp * 2 - hs * (n as Au - 1).max(0)).max(0);
    let widths = distribute(&cols, inner);
    let mut col_x = alloc::vec![0; n + 1];
    col_x[0] = tx + edge_sp;
    for i in 0..n {
        col_x[i + 1] = col_x[i] + widths[i] + hs;
    }
    let grid_top = y + vs.max(0);
    // Lay out the cells at their widths; row heights from the cells (CSS 2.1 §17.5.3).
    let nrows = g.rows.len();
    let mut row_h = alloc::vec![0 as Au; nrows];
    for (ri, &r) in g.rows.iter().enumerate() {
        let rs = &boxes[r as usize].style;
        if let Size::L(l) = rs.height
            && !l.has_pct()
        {
            row_h[ri] = cx.lp(&l, 0);
        }
    }
    let mut cell_base: Vec<Option<Au>> = Vec::with_capacity(g.cells.len());
    let mut cell_need: Vec<Au> = Vec::with_capacity(g.cells.len());
    for cell in &g.cells {
        let x0 = col_x[cell.col];
        let x1 = col_x[(cell.col + cell.colspan).min(n)] - hs;
        let w = (x1 - x0).max(0);
        layout_cell(boxes, cx, cell.b, x0, grid_top, w);
        cell_base.push(
            boxes[cell.b as usize]
                .first_baseline
                .map(|b| b - boxes[cell.b as usize].y),
        );
        // A cell needs its content height, or its specified height if taller (§17.5.3).
        let mut h = boxes[cell.b as usize].h;
        let s = &boxes[cell.b as usize].style;
        if let Size::L(l) = s.height
            && !l.has_pct()
        {
            let (_, bo, p) = block::edges(cx, s, 0);
            let bp = bo[0] + bo[2] + p[0] + p[2];
            h = h.max(block::size_px(cx, s, &s.height, None, bp).unwrap_or(0) + bp);
        }
        cell_need.push(h);
        if cell.rowspan == 1 {
            row_h[cell.row] = row_h[cell.row].max(h);
        }
    }
    // Baseline alignment: cells with vertical-align: baseline line their first lines up.
    let mut row_base = alloc::vec![0 as Au; nrows];
    for (k, cell) in g.cells.iter().enumerate() {
        if cell.rowspan == 1 && boxes[cell.b as usize].style.vertical_align == VAlign::Baseline {
            let b = cell_base[k].unwrap_or(boxes[cell.b as usize].h);
            row_base[cell.row] = row_base[cell.row].max(b);
        }
    }
    for (k, cell) in g.cells.iter().enumerate() {
        if cell.rowspan == 1 && boxes[cell.b as usize].style.vertical_align == VAlign::Baseline {
            let b = cell_base[k].unwrap_or(boxes[cell.b as usize].h);
            let need = boxes[cell.b as usize].h + (row_base[cell.row] - b);
            row_h[cell.row] = row_h[cell.row].max(need);
        }
    }
    // Row-spanning cells grow their last row if needed.
    for (k, cell) in g.cells.iter().enumerate() {
        if cell.rowspan > 1 {
            let last = (cell.row + cell.rowspan - 1).min(nrows - 1);
            let have: Au = row_h[cell.row..=last].iter().sum::<Au>() + vs * (last - cell.row) as Au;
            let need = cell_need[k];
            if need > have {
                row_h[last] += need - have;
            }
        }
    }
    // A taller specified table height goes to the rows.
    let grid_h: Au = row_h.iter().sum::<Au>() + vs * (nrows as Au - 1).max(0) + vs.max(0) * 2;
    if let Some(sh) = spec_h {
        let caps: Au = y - ty;
        let want = sh - caps;
        if want > grid_h && nrows > 0 {
            let extra = want - grid_h;
            let per = extra / nrows as Au;
            for h in row_h.iter_mut() {
                *h += per;
            }
            row_h[nrows - 1] += extra - per * nrows as Au;
        }
    }
    let mut row_y = alloc::vec![0; nrows + 1];
    row_y[0] = grid_top;
    for i in 0..nrows {
        row_y[i + 1] = row_y[i] + row_h[i] + vs;
    }
    // Final cell boxes: stretched to their rows, content aligned (CSS 2.1 §17.5.4).
    for (k, cell) in g.cells.iter().enumerate() {
        let last = (cell.row + cell.rowspan - 1).min(nrows.saturating_sub(1));
        let top = row_y[cell.row];
        let h = row_y[last + 1] - vs - top;
        let b = cell.b;
        let content_h = boxes[b as usize].h;
        let dy_box = top - boxes[b as usize].y;
        block::translate(boxes, b, 0, dy_box);
        let off = match boxes[b as usize].style.vertical_align {
            VAlign::Middle => (h - content_h) / 2,
            VAlign::Bottom => h - content_h,
            VAlign::Baseline if cell.rowspan == 1 => {
                let bl = cell_base[k].unwrap_or(content_h);
                row_base[cell.row] - bl
            }
            _ => 0,
        }
        .max(0);
        if off > 0 {
            let kids = boxes[b as usize].children.clone();
            for c in kids {
                block::translate(boxes, c, 0, off);
            }
            if let Some(ifc) = boxes[b as usize].ifc.as_mut() {
                for l in ifc.lines.iter_mut() {
                    l.y += off;
                    l.baseline += off;
                }
                for f in ifc.frags.iter_mut() {
                    match f {
                        Frag::Text { baseline, .. } => *baseline += off,
                        Frag::Inline { y, .. } => *y += off,
                    }
                }
            }
        }
        boxes[b as usize].h = h.max(content_h);
    }
    // Row and row-group boxes (for their backgrounds).
    for (ri, &r) in g.rows.iter().enumerate() {
        let bx = &mut boxes[r as usize];
        bx.x = tx + edge_sp;
        bx.y = row_y[ri];
        bx.w = (col_x[n] - hs - bx.x).max(0);
        bx.h = row_h[ri];
        bx.laid_out = true;
    }
    for &(gb, a, z) in &g.groups {
        let bx = &mut boxes[gb as usize];
        bx.x = tx + edge_sp;
        bx.w = (col_x[n] - hs - bx.x).max(0);
        if a < z {
            bx.y = row_y[a];
            bx.h = row_y[z] - vs - row_y[a];
        } else {
            bx.y = grid_top;
            bx.h = 0;
        }
        bx.laid_out = true;
    }
    y = if nrows > 0 {
        row_y[nrows] - vs + vs.max(0)
    } else {
        grid_top
    };
    if caption_bottom {
        for &cap in &g.captions {
            layout_caption(boxes, &mut y, cap);
        }
    }
    let bo = boxes[t as usize].border;
    let p = boxes[t as usize].padding;
    let bp_h = bo[0] + bo[2] + p[0] + p[2];
    let content_h = y - ty;
    let h = spec_h.map(|s| s.max(content_h)).unwrap_or(content_h);
    let tb = &mut boxes[t as usize];
    tb.h = h + bp_h;
    // A table's baseline is its first row's (CSS 2.1 §17.5.1... §10.8.1).
    let fb = g
        .cells
        .iter()
        .find(|c| c.row == 0)
        .and_then(|c| boxes[c.b as usize].first_baseline);
    boxes[t as usize].first_baseline = fb;
    boxes[t as usize].last_baseline = fb;
}

/// Lay out one cell as a block formatting context `w` wide (border box) at (x, y).
fn layout_cell(boxes: &mut Vec<LBox>, cx: &Cx, c: BoxId, x: Au, y: Au, w: Au) {
    let s = boxes[c as usize].style.clone();
    let (_, bo, p) = block::edges(cx, &s, w);
    {
        let bx = &mut boxes[c as usize];
        bx.margin = [0; 4];
        bx.border = bo;
        bx.padding = p;
        bx.x = x;
        bx.y = y;
        bx.w = w;
        bx.laid_out = true;
    }
    let mut fl = block::Floats::new();
    block::layout_block_children(boxes, cx, c, None, &mut fl, false, true);
}
