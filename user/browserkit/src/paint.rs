//! Pixels: a [`Surface`] (an offscreen 0x00RRGGBB picture with a clip rectangle) with the
//! drawing the Browser needs, painting a [`DisplayList`] into one at a scroll offset, and
//! [`scale_into`] for zoom (nearest pixel for whole-number zooms, so text stays crisp, else
//! bilinear).

use alloc::vec;
use alloc::vec::Vec;

use imgdec::Image;

use webengine::{BorderStyle, MarkerKind, TextRun};

use crate::engine::{Color, ControlId, DisplayList, Entry, ImageId, Item, Rect};
use crate::font::{self, Fonts, Text};

/// How glyphs are drawn: the scale (1, 2, 3 times the 8x16 cell), faux bold, a slant.
#[derive(Clone, Copy, Debug, Default)]
pub struct Glyphs {
    pub scale: i32,
    pub bold: bool,
    pub italic: bool,
}

/// `src` over `dst` at `a` (0..=255).
pub fn blend(dst: Color, src: Color, a: u32) -> Color {
    let ia = 255 - a;
    let ch = |s: u32| {
        let (d, x) = ((dst >> s) & 0xFF, (src >> s) & 0xFF);
        ((x * a + d * ia + 127) / 255) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// The overlap of two rectangles; an empty one is (0, 0, 0, 0), so it can be used as a range.
pub fn intersect(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.x.saturating_add(a.w).min(b.x.saturating_add(b.w));
    let y1 = a.y.saturating_add(a.h).min(b.y.saturating_add(b.h));
    if x1 <= x0 || y1 <= y0 {
        return Rect::default();
    }
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

pub struct Surface {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u32>,
    pub clip: Rect,
    /// Added to every coordinate drawn (the page's scroll).
    pub dx: i32,
    pub dy: i32,
}

impl Surface {
    /// The most pixels a surface may have.
    pub const MAX_PIXELS: usize = 4096 * 4096;

    pub fn new(w: i32, h: i32, c: Color) -> Surface {
        let (w, h) = (w.max(1), h.max(1));
        let n = (w as usize * h as usize).min(Self::MAX_PIXELS);
        let h = (n / w as usize) as i32;
        Surface {
            w,
            h,
            px: vec![c; n],
            clip: Rect::new(0, 0, w, h),
            dx: 0,
            dy: 0,
        }
    }

    /// Make it `w` x `h` (contents undefined afterwards).
    pub fn resize(&mut self, w: i32, h: i32) {
        if (w, h) != (self.w, self.h) {
            *self = Surface::new(w, h, 0);
        }
        self.clip = Rect::new(0, 0, self.w, self.h);
        self.dx = 0;
        self.dy = 0;
    }

    pub fn clear(&mut self, c: Color) {
        self.px.fill(c);
    }

    /// The visible part of `r` (after the offset), in surface pixels.
    fn vis(&self, r: Rect) -> Rect {
        intersect(
            Rect::new(
                r.x.saturating_add(self.dx),
                r.y.saturating_add(self.dy),
                r.w,
                r.h,
            ),
            self.clip,
        )
    }

    pub fn fill(&mut self, r: Rect, c: Color) {
        let v = self.vis(r);
        for y in v.y..v.y + v.h {
            let row = (y * self.w) as usize;
            self.px[row + v.x as usize..row + (v.x + v.w) as usize].fill(c);
        }
    }

    pub fn fill_alpha(&mut self, r: Rect, c: Color, a: u32) {
        let v = self.vis(r);
        for y in v.y..v.y + v.h {
            let row = (y * self.w) as usize;
            for p in &mut self.px[row + v.x as usize..row + (v.x + v.w) as usize] {
                *p = blend(*p, c, a);
            }
        }
    }

    fn put(&mut self, x: i32, y: i32, c: Color, a: u32) {
        let (x, y) = (x + self.dx, y + self.dy);
        if self.clip.contains(x, y) {
            let p = &mut self.px[(y * self.w + x) as usize];
            *p = if a >= 255 { c } else { blend(*p, c, a) };
        }
    }

    /// A rounded rectangle (radius `r`) with soft corners.
    pub fn rrect(&mut self, rc: Rect, r: i32, c: Color) {
        let r = r.min(rc.w / 2).min(rc.h / 2).max(0);
        if r == 0 {
            return self.fill(rc, c);
        }
        self.fill(Rect::new(rc.x, rc.y + r, rc.w, rc.h - 2 * r), c);
        self.fill(Rect::new(rc.x + r, rc.y, rc.w - 2 * r, r), c);
        self.fill(Rect::new(rc.x + r, rc.y + rc.h - r, rc.w - 2 * r, r), c);
        for (cx, cy, x0, y0) in [
            (rc.x + r, rc.y + r, rc.x, rc.y),
            (rc.x + rc.w - r - 1, rc.y + r, rc.x + rc.w - r, rc.y),
            (rc.x + r, rc.y + rc.h - r - 1, rc.x, rc.y + rc.h - r),
            (
                rc.x + rc.w - r - 1,
                rc.y + rc.h - r - 1,
                rc.x + rc.w - r,
                rc.y + rc.h - r,
            ),
        ] {
            for y in y0..y0 + r {
                for x in x0..x0 + r {
                    let mut n = 0;
                    for sy in 0..4 {
                        for sx in 0..4 {
                            let dx = (x - cx) * 4 + sx - 2 + 1;
                            let dy = (y - cy) * 4 + sy - 2 + 1;
                            if dx * dx + dy * dy <= 16 * r * r {
                                n += 1;
                            }
                        }
                    }
                    if n > 0 {
                        self.put(x, y, c, n * 255 / 16);
                    }
                }
            }
        }
    }

    /// A filled circle.
    pub fn disc(&mut self, cx: i32, cy: i32, r: i32, c: Color) {
        self.rrect(Rect::new(cx - r, cy - r, 2 * r + 1, 2 * r + 1), r, c);
    }

    /// A `width`-pixel frame just inside `r`.
    pub fn border(&mut self, r: Rect, width: i32, c: Color) {
        let t = width.max(1).min(r.w / 2 + 1).min(r.h / 2 + 1);
        self.fill(Rect::new(r.x, r.y, r.w, t), c);
        self.fill(Rect::new(r.x, r.y + r.h - t, r.w, t), c);
        self.fill(Rect::new(r.x, r.y + t, t, r.h - 2 * t), c);
        self.fill(Rect::new(r.x + r.w - t, r.y + t, t, r.h - 2 * t), c);
    }

    /// Text in the UI font (body size, or `big` for twice the size) with its top left at
    /// (x, y); returns the x where it ends.
    pub fn text(&mut self, x: i32, y: i32, s: &str, bold: bool, c: Color) -> i32 {
        self.glyphs(
            x,
            y,
            s,
            Glyphs {
                scale: 1,
                bold,
                italic: false,
            },
            c,
        )
    }

    /// Unifont glyphs with the cell's top left at (x, y).
    pub fn glyphs(&mut self, x: i32, y: i32, s: &str, f: Glyphs, c: Color) -> i32 {
        let sc = f.scale.max(1);
        let step = font::CELL_W * sc;
        // Nothing of it can show: skip the glyph loop.
        if y + self.dy >= self.clip.bottom() || y + self.dy + font::CELL_H * sc <= self.clip.y {
            return x + font::cells(s) * step;
        }
        let mut cx = x;
        for g in font::glyphs(s) {
            if cx + self.dx >= self.clip.x + self.clip.w {
                cx += step;
                continue;
            }
            if cx + step + self.dx > self.clip.x {
                for (row, bits) in g.iter().enumerate() {
                    if *bits == 0 {
                        continue;
                    }
                    // Italic: a slant of one pixel every four rows.
                    let slant = if f.italic {
                        ((15 - row as i32) / 4 - 1) * sc
                    } else {
                        0
                    };
                    for col in 0..8 {
                        if bits & (0x80 >> col) == 0 {
                            continue;
                        }
                        for sy in 0..sc {
                            for sx in 0..sc {
                                let px = cx + col * sc + sx + slant;
                                let py = y + row as i32 * sc + sy;
                                self.put(px, py, c, 255);
                                if f.bold {
                                    self.put(px + 1, py, c, 255);
                                }
                            }
                        }
                    }
                }
            }
            cx += step;
        }
        cx
    }

    /// `img` (straight alpha ARGB) scaled into `r` (nearest pixel), blended over what is there.
    pub fn image(&mut self, r: Rect, img: &Image) {
        if img.w == 0 || img.h == 0 || r.w <= 0 || r.h <= 0 {
            return;
        }
        let v = self.vis(r);
        if v.w <= 0 || v.h <= 0 {
            return;
        }
        let (ox, oy) = (r.x + self.dx, r.y + self.dy);
        let cols: Vec<usize> = (v.x..v.x + v.w)
            .map(|x| (((x - ox) as i64 * img.w as i64 / r.w as i64) as usize).min(img.w - 1))
            .collect();
        for y in v.y..v.y + v.h {
            let sy = (((y - oy) as i64 * img.h as i64 / r.h as i64) as usize).min(img.h - 1);
            let src = &img.px[sy * img.w..sy * img.w + img.w];
            let row = (y * self.w) as usize;
            for (k, &sx) in cols.iter().enumerate() {
                let p = src[sx];
                let a = p >> 24;
                if a == 0 {
                    continue;
                }
                let d = &mut self.px[row + v.x as usize + k];
                *d = if a == 255 {
                    p & 0xFF_FFFF
                } else {
                    blend(*d, p & 0xFF_FFFF, a)
                };
            }
        }
    }
}

/// What [`paint_list`] needs from the app besides the list.
pub trait Painter {
    /// A loaded image of this page.
    fn image(&self, id: ImageId) -> Option<&Image>;
    /// Draw form control `id` into its content box (the app knows its state, focus and
    /// caret; the engine already painted the control's background and border).
    fn control(&mut self, s: &mut Surface, rect: Rect, id: ControlId);
    /// The system fonts (None: everything in Unifont).
    fn text(&self) -> Option<&dyn Text> {
        None
    }
}

fn alpha(c: Color) -> u32 {
    c >> 24
}

impl Surface {
    /// A fill in an engine colour (0xAARRGGBB).
    fn fill_argb(&mut self, r: Rect, c: Color) {
        match alpha(c) {
            0 => {}
            255 => self.fill(r, c & 0xFF_FFFF),
            a => self.fill_alpha(r, c & 0xFF_FFFF, a),
        }
    }

    /// A rounded rectangle in an engine colour.
    fn rrect_argb(&mut self, r: Rect, radius: i32, c: Color) {
        match alpha(c) {
            0 => {}
            255 => self.rrect(r, radius, c & 0xFF_FFFF),
            // Translucent and rounded: the corners square (rare, and cheap this way).
            a => self.fill_alpha(r, c & 0xFF_FFFF, a),
        }
    }

    /// A rounded frame `w` wide (one colour, solid): straight sides, and quarter rings at
    /// the corners.
    fn rounded_border(&mut self, r: Rect, radius: i32, w: i32, c: Color) {
        let rad = radius.min(r.w / 2).min(r.h / 2).max(0);
        let w = w.max(1).min(rad.max(1));
        if alpha(c) == 0 || rad == 0 {
            return self.border(r, w, c & 0xFF_FFFF);
        }
        let c = c & 0xFF_FFFF;
        self.fill(Rect::new(r.x + rad, r.y, r.w - 2 * rad, w), c);
        self.fill(Rect::new(r.x + rad, r.y + r.h - w, r.w - 2 * rad, w), c);
        self.fill(Rect::new(r.x, r.y + rad, w, r.h - 2 * rad), c);
        self.fill(Rect::new(r.x + r.w - w, r.y + rad, w, r.h - 2 * rad), c);
        let (o, i) = ((rad * 4) as i64, ((rad - w).max(0) * 4) as i64);
        for (cx, cy, x0, y0) in [
            (r.x + rad, r.y + rad, r.x, r.y),
            (r.x + r.w - rad, r.y + rad, r.x + r.w - rad, r.y),
            (r.x + rad, r.y + r.h - rad, r.x, r.y + r.h - rad),
            (
                r.x + r.w - rad,
                r.y + r.h - rad,
                r.x + r.w - rad,
                r.y + r.h - rad,
            ),
        ] {
            for y in y0..y0 + rad {
                for x in x0..x0 + rad {
                    // 4x4 samples: inside the outer circle and outside the inner one.
                    let mut n = 0;
                    for sy in 0..4 {
                        for sx in 0..4 {
                            let dx = ((x - cx) * 4 + sx) as i64 + if x < cx { 0 } else { 1 } - 2;
                            let dy = ((y - cy) * 4 + sy) as i64 + if y < cy { 0 } else { 1 } - 2;
                            let d = dx * dx + dy * dy;
                            if d <= o * o && d >= i * i {
                                n += 1;
                            }
                        }
                    }
                    if n > 0 {
                        self.put(x, y, c, n * 255 / 16);
                    }
                }
            }
        }
    }

    /// A circle's outline.
    fn ring(&mut self, cx: i32, cy: i32, r: i32, c: Color) {
        for y in -r - 1..=r + 1 {
            for x in -r - 1..=r + 1 {
                let d2 = x * x + y * y;
                if d2 <= r * r && d2 >= (r - 1) * (r - 1) {
                    self.put(cx + x, cy + y, c, 255);
                }
            }
        }
    }

    /// One side of a border: solid, dashed, dotted or double.
    fn side(&mut self, r: Rect, horizontal: bool, style: BorderStyle, c: Color) {
        let c2 = c & 0xFF_FFFF;
        let a = alpha(c);
        if a == 0 || r.w <= 0 || r.h <= 0 {
            return;
        }
        match style {
            BorderStyle::None => {}
            BorderStyle::Dashed | BorderStyle::Dotted => {
                let t = if horizontal { r.h } else { r.w }.max(1);
                let (on, off) = if style == BorderStyle::Dotted {
                    (t, t)
                } else {
                    (3 * t, 2 * t)
                };
                let len = if horizontal { r.w } else { r.h };
                let mut k = 0;
                while k < len {
                    let n = on.min(len - k);
                    let seg = if horizontal {
                        Rect::new(r.x + k, r.y, n, r.h)
                    } else {
                        Rect::new(r.x, r.y + k, r.w, n)
                    };
                    self.fill_argb(seg, c);
                    k += on + off;
                }
            }
            BorderStyle::Double if (if horizontal { r.h } else { r.w }) >= 3 => {
                let t = if horizontal { r.h } else { r.w };
                let third = (t / 3).max(1);
                let (a1, a2) = if horizontal {
                    (
                        Rect::new(r.x, r.y, r.w, third),
                        Rect::new(r.x, r.y + t - third, r.w, third),
                    )
                } else {
                    (
                        Rect::new(r.x, r.y, third, r.h),
                        Rect::new(r.x + t - third, r.y, third, r.h),
                    )
                };
                self.fill_argb(a1, c);
                self.fill_argb(a2, c);
            }
            _ => {
                if a == 255 {
                    self.fill(r, c2)
                } else {
                    self.fill_alpha(r, c2, a)
                }
            }
        }
    }

    /// A text run as the engine laid it out (pen at x, on the baseline). Its decoration
    /// lines are separate rectangles in the list (the engine's), not drawn here.
    pub fn run(&mut self, run: &TextRun, text: &str, fonts: Fonts) {
        if alpha(run.color) == 0 {
            return;
        }
        let c = run.color & 0xFF_FFFF;
        self.text_at(
            fonts,
            &run.font,
            run.x,
            run.baseline - font::ASCENT * font::scale(&run.font),
            text,
            c,
        );
    }

    /// Text in `f` with the top of its line box at (x, y): the system font through `fonts`,
    /// or Unifont for monospace. Returns the end x.
    pub fn text_at(
        &mut self,
        fonts: Fonts,
        f: &webengine::Font,
        x: i32,
        y: i32,
        text: &str,
        c: Color,
    ) -> i32 {
        match fonts.backend(f) {
            Some(t) => {
                let (w, clip) = (self.w as usize, self.clip);
                t.draw(&mut self.px, w, clip, x + self.dx, y + self.dy, f, text, c) - self.dx
            }
            None => {
                // Unifont, with what it lacks in the system font (when there is one).
                let g = Glyphs {
                    scale: font::scale(f),
                    bold: f.weight >= 600,
                    italic: f.italic,
                };
                let mut cx = x;
                for (seg, uni) in font::mono_segments(text, fonts.text.is_some()) {
                    cx = match (uni, fonts.text) {
                        (false, Some(t)) => {
                            let (w, clip) = (self.w as usize, self.clip);
                            let pf = font::proportional(f);
                            t.draw(
                                &mut self.px,
                                w,
                                clip,
                                cx + self.dx,
                                y + self.dy,
                                &pf,
                                seg,
                                c,
                            ) - self.dx
                        }
                        _ => self.glyphs(cx, y, seg, g, c),
                    };
                }
                cx
            }
        }
    }
}

/// Paint one display entry (already clipped by the caller).
fn entry(s: &mut Surface, list: &DisplayList, it: &Item, p: &mut dyn Painter) {
    match *it {
        Item::Rect { rect, color } => s.fill_argb(rect, color),
        Item::RoundRect {
            rect,
            color,
            radius,
        } => s.rrect_argb(rect, radius, color),
        Item::Border {
            rect,
            widths,
            colors,
            styles,
            radius,
        } if radius > 0
            && widths.iter().all(|w| *w == widths[0])
            && colors.iter().all(|c| *c == colors[0])
            && styles.iter().all(|st| *st == BorderStyle::Solid) =>
        {
            s.rounded_border(rect, radius, widths[0], colors[0]);
        }
        Item::Border {
            rect,
            widths,
            colors,
            styles,
            ..
        } => {
            let [t, r, b, l] = widths;
            s.side(
                Rect::new(rect.x, rect.y, rect.w, t),
                true,
                styles[0],
                colors[0],
            );
            s.side(
                Rect::new(rect.x + rect.w - r, rect.y + t, r, rect.h - t - b),
                false,
                styles[1],
                colors[1],
            );
            s.side(
                Rect::new(rect.x, rect.y + rect.h - b, rect.w, b),
                true,
                styles[2],
                colors[2],
            );
            s.side(
                Rect::new(rect.x, rect.y + t, l, rect.h - t - b),
                false,
                styles[3],
                colors[3],
            );
        }
        Item::Text(run) => s.run(&run, list.run_text(&run), Fonts { text: p.text() }),
        Item::Image { rect, image } => match p.image(image) {
            Some(img) => s.image(rect, img),
            None => {
                // Not here (yet): a quiet frame.
                s.fill(rect, 0xF3_EC_DF);
                s.border(rect, 1, 0xE2_D5_BD);
            }
        },
        Item::Tiled {
            area,
            tile,
            image,
            repeat_x,
            repeat_y,
        } => {
            let Some(img) = p.image(image) else {
                return;
            };
            if tile.w <= 0 || tile.h <= 0 {
                return;
            }
            let old = s.clip;
            s.clip = intersect(Rect::new(area.x + s.dx, area.y + s.dy, area.w, area.h), old);
            // Only the tiles that can show (and never more than 4096 of them).
            let vis = Rect::new(s.clip.x - s.dx, s.clip.y - s.dy, s.clip.w, s.clip.h);
            let span = |o: i32, size: i32, v0: i32, v1: i32, rep: bool| -> (i32, i32) {
                if !rep {
                    return (0, 0);
                }
                let first = (v0 - o).div_euclid(size);
                let last = (v1 - o).div_euclid(size);
                (first, last)
            };
            let (x0, x1) = span(tile.x, tile.w, vis.x, vis.x + vis.w, repeat_x);
            let (y0, y1) = span(tile.y, tile.h, vis.y, vis.y + vis.h, repeat_y);
            let mut n = 0;
            'tiles: for ty in y0..=y1 {
                for tx in x0..=x1 {
                    n += 1;
                    if n > 4096 {
                        break 'tiles;
                    }
                    s.image(
                        Rect::new(tile.x + tx * tile.w, tile.y + ty * tile.h, tile.w, tile.h),
                        img,
                    );
                }
            }
            s.clip = old;
        }
        Item::Marker { rect, kind, color } => {
            let c = color & 0xFF_FFFF;
            let r = (rect.w.min(rect.h) / 2).max(1);
            let (cx, cy) = (rect.x + rect.w / 2, rect.y + rect.h / 2);
            match kind {
                MarkerKind::Disc => s.disc(cx, cy, r, c),
                MarkerKind::Circle => s.ring(cx, cy, r, c),
                MarkerKind::Square => s.fill(Rect::new(cx - r, cy - r, 2 * r, 2 * r), c),
            }
        }
        Item::Control { rect, control } => p.control(s, rect, control),
    }
}

/// The engine's built-in link colour (its UA stylesheet's `:link`).
pub const UA_LINK: Color = 0xFF_00_00_EE;

/// Paint the part of `list` from page y = `scroll` down into `s` (cleared to the page's
/// background first), then the `position: fixed` boxes over it. `marks` are find-in-page
/// highlights (page coordinates; the `current` one stronger). `link`: the colour for links
/// the page didn't colour itself (the theme's link token, instead of the engine's default
/// blue).
pub fn paint_list(
    s: &mut Surface,
    list: &DisplayList,
    scroll: i32,
    p: &mut dyn Painter,
    marks: &[Rect],
    current: Option<usize>,
    link: Color,
) {
    let bg = if alpha(list.background) == 0 {
        0xFF_FF_FF
    } else {
        list.background & 0xFF_FFFF
    };
    s.clear(bg);
    let base = Rect::new(0, 0, s.w, s.h);
    for (entries, dy, fixed) in [(&list.entries, -scroll, false), (&list.fixed, 0, true)] {
        s.dx = 0;
        s.dy = dy;
        let area = Rect::new(0, -dy, s.w, s.h);
        let picked: alloc::vec::Vec<&Entry> = if fixed {
            entries.iter().collect()
        } else {
            list.visible(area)
        };
        for e in picked {
            let clip = list.clips.get(e.clip as usize).copied().unwrap_or(base);
            s.clip = if e.clip == 0 {
                base
            } else {
                intersect(Rect::new(clip.x, clip.y + dy, clip.w, clip.h), base)
            };
            if s.clip.w > 0 && s.clip.h > 0 {
                match e.item {
                    Item::Text(mut run) if run.color == UA_LINK => {
                        run.color = 0xFF00_0000 | link;
                        s.run(&run, list.run_text(&run), Fonts { text: p.text() });
                    }
                    // A link's underline, drawn by the engine as a thin rectangle.
                    Item::Rect { rect, color } if color == UA_LINK => s.fill(rect, link),
                    ref it => entry(s, list, it, p),
                }
            }
        }
        if !fixed {
            s.clip = base;
            for (k, m) in marks.iter().enumerate() {
                let strong = Some(k) == current;
                s.fill_alpha(
                    Rect::new(m.x - 1, m.y, m.w + 2, m.h),
                    0xEB_B6_50,
                    if strong { 150 } else { 90 },
                );
            }
        }
    }
    s.clip = base;
    s.dx = 0;
    s.dy = 0;
}

/// Copy `src` into `dst` (stride `dst_w`) at (`x`, `y`), scaled to `w` x `h`: nearest pixel
/// when the scale is a whole number (or 1), bilinear otherwise.
#[allow(clippy::too_many_arguments)]
pub fn scale_into(
    src: &Surface,
    dst: &mut [u32],
    dst_w: i32,
    dst_h: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) {
    if w <= 0 || h <= 0 || src.w <= 0 || src.h <= 0 {
        return;
    }
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(dst_w);
    let y1 = (y + h).min(dst_h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    if (w, h) == (src.w, src.h) {
        for yy in y0..y1 {
            let s = ((yy - y) * src.w + (x0 - x)) as usize;
            let d = (yy * dst_w + x0) as usize;
            let n = (x1 - x0) as usize;
            dst[d..d + n].copy_from_slice(&src.px[s..s + n]);
        }
        return;
    }
    let whole = w % src.w == 0 && h % src.h == 0;
    if whole {
        let (fx, fy) = (w / src.w, h / src.h);
        for yy in y0..y1 {
            let sy = ((yy - y) / fy).min(src.h - 1);
            for xx in x0..x1 {
                let sx = ((xx - x) / fx).min(src.w - 1);
                dst[(yy * dst_w + xx) as usize] = src.px[(sy * src.w + sx) as usize];
            }
        }
        return;
    }
    // Bilinear, in 8.8 fixed point.
    let map = |d: i32, dn: i32, sn: i32| -> (usize, usize, u32) {
        let f = ((2 * d + 1) as i64 * sn as i64 * 256 / (2 * dn as i64) - 128).max(0);
        let i = (f >> 8) as i32;
        let i0 = i.min(sn - 1) as usize;
        let i1 = (i + 1).min(sn - 1) as usize;
        (i0, i1, (f & 255) as u32)
    };
    let cols: Vec<(usize, usize, u32)> = (x0..x1).map(|xx| map(xx - x, w, src.w)).collect();
    for yy in y0..y1 {
        let (r0, r1, fy) = map(yy - y, h, src.h);
        let (a, b) = (
            &src.px[r0 * src.w as usize..][..src.w as usize],
            &src.px[r1 * src.w as usize..][..src.w as usize],
        );
        let drow = (yy * dst_w) as usize;
        for (k, &(c0, c1, fx)) in cols.iter().enumerate() {
            let top = blend(a[c0], a[c1], fx);
            let bot = blend(b[c0], b[c1], fx);
            dst[drow + x0 as usize + k] = blend(top, bot, fy);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::Unifont;
    use webengine::Viewport;

    struct P(Option<Image>, usize);
    impl Painter for P {
        fn image(&self, _: ImageId) -> Option<&Image> {
            self.0.as_ref()
        }
        fn control(&mut self, s: &mut Surface, r: Rect, _id: ControlId) {
            self.1 += 1;
            s.fill(r, 0x123456);
        }
    }

    #[test]
    fn fills_clip_and_text() {
        let mut s = Surface::new(40, 30, 0);
        s.fill(Rect::new(-10, -10, 20, 20), 0xFF0000);
        assert_eq!(s.px[0], 0xFF0000);
        assert_eq!(s.px[10], 0);
        s.fill(Rect::new(35, 25, 100, 100), 0x00FF00);
        assert_eq!(*s.px.last().unwrap(), 0x00FF00);
        let end = s.text(0, 10, "Hi", false, 0xFFFFFF);
        assert_eq!(end, 16);
        assert!(s.px[(10 * 40)..(26 * 40)].contains(&0xFFFFFF));
        // Way off the surface: nothing happens, nothing panics.
        s.text(-1000, -1000, "x", true, 1);
        s.text(i32::MAX / 4, 5, "x", false, 1);
        s.rrect(Rect::new(5, 5, 20, 10), 50, 0x0000FF);
        s.disc(20, 15, 4, 0x00FFFF);
        s.border(Rect::new(0, 0, 40, 30), 2, 0xABCDEF);
        assert_eq!(s.px[0], 0xABCDEF);
    }

    #[test]
    fn images_blend_and_scale() {
        let mut img = Image::new(2, 1).unwrap();
        img.px = alloc::vec![0xFF_FF0000, 0x80_0000FF];
        let mut s = Surface::new(4, 2, 0xFFFFFF);
        s.image(Rect::new(0, 0, 4, 2), &img);
        assert_eq!(s.px[0], 0xFF0000);
        assert_eq!(s.px[1], 0xFF0000);
        let b = s.px[3];
        assert!((b & 0xFF) == 0xFF && (b >> 16) < 0x90, "{b:x}");
    }

    fn page(html: &str) -> webengine::Document {
        webengine::Document::parse(html.as_bytes(), "http://x.test/", Some("text/html"))
    }

    #[test]
    fn display_list_paint() {
        let mut d = page(
            "<body style='background:#102030'><p style='color:#ff0000'>hello <u>there</u></p><p><input name=a><img src=x.png alt=x width=20 height=20></p><div style='border:3px dashed #00ff00;height:20px'></div><ul><li>one</ul>",
        );
        let list = d.layout(&Viewport::new(300, 200), &Unifont).clone();
        let mut s = Surface::new(300, 200, 0);
        let mut p = P(None, 0);
        paint_list(
            &mut s,
            &list,
            0,
            &mut p,
            &[Rect::new(20, 20, 10, 10)],
            Some(0),
            0x4F64A0,
        );
        assert_eq!(p.1, 1);
        assert!(s.px.contains(&0x123456), "the control was drawn");
        // The find highlight shows at the top of the page too (scroll 0).
        let hl = blend(s.px[0], 0xEB_B6_50, 150);
        assert!(
            s.px[25 * 300 + 25] == hl || s.px.contains(&hl),
            "the highlight"
        );
        assert!(s.px.contains(&0xFF0000), "red text");
        assert!(s.px.contains(&0x00FF00), "the dashed border");
        let mut d1 = page(
            "<div style='border:2px solid #00ff00;border-radius:8px;height:30px;width:60px'></div>",
        );
        let list1 = d1.layout(&Viewport::new(300, 200), &Unifont).clone();
        let mut s2 = Surface::new(300, 200, 0);
        paint_list(&mut s2, &list1, 0, &mut p, &[], None, 0x4F64A0);
        assert!(s2.px.contains(&0x00FF00), "a rounded border");
        assert_eq!(s.px[0], 0x102030, "the background");
        // A link the page didn't colour gets the theme's colour, not the engine's blue.
        let mut d2 = page("<p><a href=/x>a link</a></p>");
        let list2 = d2.layout(&Viewport::new(300, 200), &Unifont).clone();
        let mut s3 = Surface::new(300, 200, 0);
        paint_list(&mut s3, &list2, 0, &mut p, &[], None, 0x4F64A0);
        assert!(s3.px.contains(&0x4F64A0));
        assert!(!s3.px.contains(&0x0000EE));
        // Scrolled past everything: just the background.
        paint_list(&mut s, &list, 100_000, &mut p, &[], None, 0x4F64A0);
        assert!(s.px.iter().all(|&c| c == 0x102030));
    }

    #[test]
    fn fixed_boxes_stay_put() {
        let mut d = page(
            "<div style='position:fixed;top:0;left:0;width:50px;height:10px;background:#0000ff'></div><div style='height:3000px'></div>",
        );
        let list = d.layout(&Viewport::new(300, 200), &Unifont).clone();
        let mut s = Surface::new(300, 200, 0);
        paint_list(&mut s, &list, 1000, &mut P(None, 0), &[], None, 0x4F64A0);
        assert_eq!(s.px[5 * 300 + 5], 0x0000FF);
    }

    #[test]
    fn hostile_pages_paint() {
        let mut seed = 99u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let parts = [
            "<div style='border:9999px solid red'>",
            "<p>",
            "x ",
            "<img src=a width=99999>",
            "<table><td>",
            "<pre>",
            "\u{1F404}",
            "</div>",
            "<b>",
            "<input>",
            "<select><option>a",
            "<ul><li>",
            "<span style='font-size:999px'>",
            "<div style='position:fixed;top:-99999px'>",
        ];
        for _ in 0..60 {
            let mut h = alloc::string::String::new();
            for _ in 0..(rnd() % 80) {
                h.push_str(parts[rnd() as usize % parts.len()]);
            }
            let mut d = page(&h);
            let list = d
                .layout(&Viewport::new((rnd() % 900) as i32 + 1, 300), &Unifont)
                .clone();
            let mut s = Surface::new(200, 150, 0);
            paint_list(
                &mut s,
                &list,
                (rnd() % 2000) as i32,
                &mut P(None, 0),
                &[],
                None,
                0x4F64A0,
            );
        }
    }

    #[test]
    fn scaling() {
        let mut src = Surface::new(2, 2, 0);
        src.px = alloc::vec![0x000000, 0xFFFFFF, 0xFFFFFF, 0x000000];
        let mut dst = alloc::vec![7u32; 16];
        scale_into(&src, &mut dst, 4, 4, 0, 0, 4, 4);
        assert_eq!(&dst[..4], &[0, 0, 0xFFFFFF, 0xFFFFFF]);
        let mut dst = alloc::vec![7u32; 9];
        scale_into(&src, &mut dst, 3, 3, 0, 0, 3, 3);
        assert!(dst.iter().all(|&c| c != 7));
        let mut same = alloc::vec![0u32; 4];
        scale_into(&src, &mut same, 2, 2, 0, 0, 2, 2);
        assert_eq!(same, src.px);
        let mut d = alloc::vec![7u32; 4];
        scale_into(&src, &mut d, 2, 2, 1, 1, 3, 3);
        assert_eq!(d[0], 7);
        scale_into(&src, &mut d, 2, 2, -5, -5, 3, 3);
    }
}
