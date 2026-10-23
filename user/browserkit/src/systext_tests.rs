//! Tests with real proportional text: the system fonts (fontkit over the baked blobs in
//! kernel/src/sysfont), as the Browser draws them.

use alloc::vec::Vec;
use std::boxed::Box;

use fontkit::{Family as Fam, Source, Weight};

use crate::engine::{Document, Font, Item, Rect, Viewport};
use crate::font::{Fonts, Text, snap};
use crate::paint::{self, Painter, Surface};

static BLOBS: [&[u8]; 6] = [
    include_bytes!("../../../kernel/src/sysfont/figtree.font"),
    include_bytes!("../../../kernel/src/sysfont/plex.font"),
    include_bytes!("../../../kernel/src/sysfont/fira.font"),
    include_bytes!("../../../kernel/src/sysfont/atkinson.font"),
    include_bytes!("../../../kernel/src/sysfont/ubuntu.font"),
    include_bytes!("../../../kernel/src/sysfont/symbols.font"),
];

struct All(Vec<&'static Fam<'static>>);

impl Source<'static> for All {
    fn family(&self, i: usize) -> Option<&'static Fam<'static>> {
        self.0.get(i).copied()
    }
}

struct Sys(All);

fn sys() -> Sys {
    Sys(All(BLOBS
        .iter()
        .map(|b| &*Box::leak(Box::new(Fam::parse(b).unwrap())))
        .collect()))
}

fn kit(f: &Font) -> fontkit::Font {
    fontkit::Font::new(0, snap(f.size as i32), Weight::bold(f.weight >= 600))
}

impl Text for Sys {
    fn width(&self, f: &Font, s: &str) -> i32 {
        kit(f).width(&self.0, s)
    }
    fn draw(
        &self,
        px: &mut [u32],
        stride: usize,
        clip: Rect,
        x: i32,
        y: i32,
        f: &Font,
        s: &str,
        c: u32,
    ) -> i32 {
        let k = kit(f);
        let base = y + k.baseline();
        k.layout(&self.0, x, s, |pen, g, mag| {
            g.spans(mag, |sx, sy, len, _a| {
                let (gx, gy) = (pen + g.left * mag + sx, base - g.top * mag + sy);
                for xx in gx.max(clip.x)..(gx + len).min(clip.x + clip.w) {
                    if gy >= clip.y && gy < clip.y + clip.h && (xx as usize) < stride {
                        let i = gy as usize * stride + xx as usize;
                        if i < px.len() {
                            px[i] = c;
                        }
                    }
                }
            });
        })
    }
}

struct P<'a>(&'a Sys);

impl Painter for P<'_> {
    fn image(&self, _: crate::engine::ImageId) -> Option<&imgdec::Image> {
        None
    }
    fn control(&mut self, _: &mut Surface, _: Rect, _: crate::engine::ControlId) {}
    fn text(&self) -> Option<&dyn Text> {
        Some(self.0)
    }
}

#[test]
fn find_highlights_fit_their_text() {
    let s = sys();
    let fonts = Fonts { text: Some(&s) };
    let mut d = Document::parse(
        b"<p>Paragraph 250 of the long page: milk, cream, butter and cheese.</p><h1>A heading with milk</h1>",
        "http://x.test/",
        Some("text/html"),
    );
    let list = d.layout(&Viewport::new(800, 600), &fonts).clone();
    for (q, want) in [("Paragraph 250", false), ("milk", false), ("heading", true)] {
        let m = d.find(q, false);
        assert!(!m.is_empty(), "{q}");
        // The run the match is in, and its font: the highlight is as wide as the text.
        let r = m[0].rects[0];
        let run = list
            .entries
            .iter()
            .find_map(|e| match e.item {
                Item::Text(t)
                    if t.baseline > r.y
                        && t.baseline <= r.y + r.h
                        && list.run_text(&t).contains(q) =>
                {
                    Some(t)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no run for {q}"));
        let w = s.width(&run.font, q);
        assert!(
            (r.w - w).abs() <= 2,
            "{q}: highlight {} wide, text {w}",
            r.w
        );
        assert_eq!(run.font.size >= 24, want);
    }
}

#[test]
fn greek_and_cyrillic_draw_without_boxes() {
    let s = sys();
    let fonts = Fonts { text: Some(&s) };
    let mut d = Document::parse(
        "<p>Το γάλα — Молоко — Łódź “quotes”</p><p><code>λ x → x</code></p>".as_bytes(),
        "http://x.test/",
        Some("text/html; charset=utf-8"),
    );
    let list = d.layout(&Viewport::new(800, 600), &fonts).clone();
    let mut surf = Surface::new(800, 200, 0);
    paint::paint_list(&mut surf, &list, 0, &mut P(&s), &[], None, 0x4F64A0);
    let ink = surf.px.iter().filter(|&&p| p != surf.px[0]).count();
    assert!(ink > 500, "text was drawn ({ink} pixels)");
    for e in &list.entries {
        if let Item::Text(t) = e.item {
            let text = list.run_text(&t);
            for c in text.chars().filter(|c| !c.is_whitespace()) {
                // Some family has it (the monospace runs' Latin-1 is Unifont's).
                let found = (c as u32) <= 0xFF
                    || s.0
                        .0
                        .iter()
                        .any(|f| f.find(c as u32, 1, Weight::Regular).is_some())
                    || fontkit::fold(c).is_some();
                assert!(found, "{c:?} in {text:?} would be a box");
                let one = alloc::string::String::from(c);
                assert!(fonts.width(&t.font, &one) > 0, "{c:?} takes no room");
            }
        }
    }
}
