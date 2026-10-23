//! A small rasterizer for the tests: paints a display list into pixels with the system fonts
//! (user/fontkit over the baked kernel/src/sysfont/*.font, scaled from their 13 / 26 px faces) and writes PNG screenshots, and
//! the matching [`SysMeasurer`] so layout and drawing agree. Host tests only.

use alloc::vec::Vec;

use crate::api::*;
static BLOBS: [&[u8]; 5] = [
    include_bytes!("../../../../kernel/src/sysfont/figtree.font"),
    include_bytes!("../../../../kernel/src/sysfont/plex.font"),
    include_bytes!("../../../../kernel/src/sysfont/fira.font"),
    include_bytes!("../../../../kernel/src/sysfont/atkinson.font"),
    include_bytes!("../../../../kernel/src/sysfont/ubuntu.font"),
];

/// Which baked family stands in for a generic one.
fn family_index(f: Family) -> usize {
    match f {
        Family::SansSerif | Family::SystemUi => 0, // Figtree
        Family::Serif => 1,                        // IBM Plex Sans (no serif face is baked)
        Family::Monospace => 2,                    // Fira Sans
        Family::Cursive | Family::Fantasy => 4,
    }
}

struct G {
    w: i32,
    h: i32,
    left: i32,
    top: i32,
    adv: i32,
    /// The pixel size of the face it came from (metrics are in its pixels).
    fs: i32,
    cov: Vec<u8>,
}

/// The baked families (user/fontkit over kernel/src/sysfont/*.font), parsed once.
fn families() -> &'static [fontkit::Family<'static>] {
    use std::sync::OnceLock;
    static F: OnceLock<Vec<fontkit::Family<'static>>> = OnceLock::new();
    F.get_or_init(|| {
        BLOBS
            .iter()
            .map(|b| fontkit::Family::parse(b).expect("a baked font"))
            .collect()
    })
}

fn glyph(f: &Font, c: char) -> Option<G> {
    let fams = families();
    let fam = &fams[family_index(f.family).min(fams.len() - 1)];
    let scale = if f.size <= 19 { 1 } else { 2 };
    let weight = fontkit::Weight::bold(f.weight >= 600);
    let (g, mag) = fam
        .find(c as u32, scale, weight)
        .or_else(|| fam.find(0xFFFD, scale, weight))?;
    let mut cov = alloc::vec![0u8; (g.w * g.h).max(0) as usize];
    g.spans(1, |x, y, len, a| {
        for k in 0..len {
            let i = (y * g.w + x + k) as usize;
            if i < cov.len() {
                cov[i] = a;
            }
        }
    });
    Some(G {
        w: g.w,
        h: g.h,
        left: g.left,
        top: g.top,
        adv: g.adv,
        fs: 13 * scale / mag.max(1),
        cov,
    })
}

/// Measures with the baked fonts' advances, scaled to the font size.
pub struct SysMeasurer;

impl TextMeasurer for SysMeasurer {
    fn text_width(&self, f: &Font, text: &str) -> i32 {
        let w: f32 = text
            .chars()
            .filter_map(|c| glyph(f, c))
            .map(|g| g.adv as f32 * f.size as f32 / g.fs as f32)
            .sum();
        (w + 0.5) as i32
    }
    fn metrics(&self, f: &Font) -> FontMetrics {
        let s = f.size as i32;
        let ascent = (s * 12 + 6) / 13;
        let descent = ((s * 3 + 6) / 13).max(1);
        FontMetrics {
            ascent,
            descent,
            line_height: ((s * 16 + 6) / 13).max(ascent + descent),
        }
    }
}

pub struct Canvas {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u32>,
}

fn blend(dst: u32, c: u32, a: u32) -> u32 {
    let a = a.min(255);
    let mix = |d: u32, s: u32| (s * a + d * (255 - a)) / 255;
    0xFF00_0000
        | (mix((dst >> 16) & 255, (c >> 16) & 255) << 16)
        | (mix((dst >> 8) & 255, (c >> 8) & 255) << 8)
        | mix(dst & 255, c & 255)
}

impl Canvas {
    pub fn new(w: i32, h: i32, bg: u32) -> Canvas {
        Canvas {
            w,
            h,
            px: alloc::vec![bg | 0xFF00_0000; (w * h) as usize],
        }
    }

    fn put(&mut self, x: i32, y: i32, c: u32, a: u32, clip: &Rect) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || !clip.contains(x, y) {
            return;
        }
        let i = (y * self.w + x) as usize;
        self.px[i] = blend(self.px[i], c, a * (c >> 24) / 255);
    }

    fn fill(&mut self, r: Rect, c: u32, clip: &Rect) {
        let r = r
            .intersect(clip)
            .intersect(&Rect::new(0, 0, self.w, self.h));
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                self.put(x, y, c, 255, clip);
            }
        }
    }

    fn text(&mut self, dl: &DisplayList, t: &TextRun, clip: &Rect) {
        let mut pen = t.x as f32;
        for c in dl.run_text(t).chars() {
            let Some(g) = glyph(&t.font, c) else { continue };
            let k = t.font.size as f32 / g.fs as f32;
            let gx = pen + g.left as f32 * k;
            let gy = t.baseline as f32 - g.top as f32 * k;
            let tw = (g.w as f32 * k).ceil() as i32;
            let th = (g.h as f32 * k).ceil() as i32;
            for yy in 0..th {
                for xx in 0..tw {
                    let sx = ((xx as f32) / k) as i32;
                    let sy = ((yy as f32) / k) as i32;
                    if sx < g.w && sy < g.h {
                        let a = g.cov[(sy * g.w + sx) as usize] as u32;
                        if a > 0 {
                            self.put(gx as i32 + xx, gy as i32 + yy, t.color, a, clip);
                        }
                    }
                }
            }
            pen += g.adv as f32 * k;
        }
    }

    /// Paint `dl` scrolled by `scroll_y`.
    pub fn paint(&mut self, dl: &DisplayList, doc: &Document, scroll_y: i32) {
        let view = Rect::new(0, scroll_y, self.w, self.h);
        for (fixed, list) in [(false, &dl.entries), (true, &dl.fixed)] {
            for e in list.iter() {
                let dy = if fixed { 0 } else { -scroll_y };
                let mut clip = dl.clips[e.clip as usize];
                clip.y = clip.y.saturating_add(dy);
                if !fixed && !entry_rect(&e.item).intersects(&view) {
                    continue;
                }
                let mv = |r: Rect| Rect::new(r.x, r.y + dy, r.w, r.h);
                match e.item {
                    Item::Rect { rect, color } | Item::RoundRect { rect, color, .. } => {
                        self.fill(mv(rect), color, &clip)
                    }
                    Item::Border {
                        rect,
                        widths,
                        colors,
                        ..
                    } => {
                        let r = mv(rect);
                        self.fill(Rect::new(r.x, r.y, r.w, widths[0]), colors[0], &clip);
                        self.fill(
                            Rect::new(r.right() - widths[1], r.y, widths[1], r.h),
                            colors[1],
                            &clip,
                        );
                        self.fill(
                            Rect::new(r.x, r.bottom() - widths[2], r.w, widths[2]),
                            colors[2],
                            &clip,
                        );
                        self.fill(Rect::new(r.x, r.y, widths[3], r.h), colors[3], &clip);
                    }
                    Item::Text(t) => {
                        let mut t = t;
                        t.baseline += dy;
                        self.text(dl, &t, &clip);
                    }
                    Item::Image { rect, .. } => {
                        // A placeholder: a soft blue with a cross.
                        let r = mv(rect);
                        self.fill(r, 0xFFB8C8E0, &clip);
                        for i in 0..r.w.min(r.h) {
                            let fx = i * r.w / r.h.max(1);
                            self.put(r.x + fx.min(r.w - 1), r.y + i, 0xFF6080A0, 255, &clip);
                        }
                    }
                    Item::Tiled { area, .. } => self.fill(mv(area), 0xFFD8E4F0, &clip),
                    Item::Marker { rect, color, .. } => self.fill(mv(rect), color, &clip),
                    Item::Control { rect, control } => {
                        let r = mv(rect);
                        // A control of a frame is in that frame's document.
                        let owner = doc.frame_doc(e.frame).unwrap_or(doc);
                        let c = &owner.controls()[control.0 as usize];
                        let font = c.font;
                        let (text, col) = match c.kind {
                            ControlKind::Checkbox | ControlKind::Radio => {
                                if c.checked {
                                    self.fill(
                                        Rect::new(r.x + 3, r.y + 3, r.w - 6, r.h - 6),
                                        0xFF2060C0,
                                        &clip,
                                    );
                                }
                                continue;
                            }
                            ControlKind::Submit | ControlKind::Reset | ControlKind::Button
                                if !c.content_drawn =>
                            {
                                (c.label.clone(), 0xFF000000)
                            }
                            ControlKind::Select => (
                                c.options
                                    .iter()
                                    .find(|o| o.selected)
                                    .map(|o| o.label.clone())
                                    .unwrap_or_default(),
                                0xFF000000,
                            ),
                            ControlKind::Text | ControlKind::TextArea => {
                                if c.value.is_empty() {
                                    (c.placeholder.clone(), 0xFF808080)
                                } else {
                                    (c.value.clone(), 0xFF000000)
                                }
                            }
                            ControlKind::Password => {
                                (c.value.chars().map(|_| '*').collect(), 0xFF000000)
                            }
                            _ => continue,
                        };
                        let m = SysMeasurer.metrics(&font);
                        let start = dl.text.len();
                        let mut dl2 = DisplayList {
                            text: dl.text.clone(),
                            ..DisplayList::default()
                        };
                        dl2.text.push_str(&text);
                        let run = TextRun {
                            x: r.x + 1,
                            baseline: r.y + (r.h + m.ascent - m.descent) / 2,
                            width: r.w,
                            font,
                            color: col,
                            decoration: Decoration::NONE,
                            start: start as u32,
                            len: text.len() as u32,
                        };
                        self.text(&dl2, &run, &clip.intersect(&r));
                    }
                }
            }
        }
    }

    pub fn png(&self) -> Vec<u8> {
        crate::kernel_png::encode(&self.px, self.w as usize, self.h as usize, self.w as usize)
    }
}

/// Lay out `html` at `w` px with the system-font measurer and paint the top `h` px.
pub fn screenshot(html: &str, w: i32, h: i32) -> (Document, Canvas) {
    let mut d = Document::parse_str(html, "http://test.example/dir/page.html");
    let full_h;
    let bg;
    {
        let dl = d.layout(&Viewport::new(w, h), &SysMeasurer);
        full_h = dl.height.clamp(1, h);
        bg = dl.background;
    }
    let mut c = Canvas::new(w, full_h, bg);
    let dl = d.display_list().clone();
    c.paint(&dl, &d, 0);
    (d, c)
}

/// Write a screenshot when WEBENGINE_SHOTS names a directory.
pub fn save(name: &str, c: &Canvas) {
    if let Ok(dir) = std::env::var("WEBENGINE_SHOTS") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(alloc::format!("{dir}/{name}.png"), c.png());
    }
}
