//! Tests with real proportional text: the system fonts (fontkit over the baked blobs in
//! kernel/src/sysfont), as the Browser draws them.

use alloc::vec::Vec;
use std::boxed::Box;

use fontkit::{Family as Fam, Source, Weight};

use crate::engine::Family;
use crate::engine::{Document, Font, Item, Rect, Viewport};
use crate::font::{Fonts, Text, family_index, snap};
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

/// The system fonts with `.1` the desktop's family (mapped as the Browser's gui::text
/// backend does, through font::family_index).
struct Sys(All, usize);

fn sys() -> Sys {
    sys_ui(0)
}

fn sys_ui(ui: usize) -> Sys {
    Sys(
        All(BLOBS
            .iter()
            .map(|b| &*Box::leak(Box::new(Fam::parse(b).unwrap())))
            .collect()),
        ui,
    )
}

impl Sys {
    fn kit(&self, f: &Font) -> fontkit::Font {
        fontkit::Font::new(
            family_index(f.family, self.1),
            snap(f.size as i32),
            Weight::bold(f.weight >= 600),
        )
    }
}

impl Text for Sys {
    fn width(&self, f: &Font, s: &str) -> i32 {
        self.kit(f).width(&self.0, s)
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
        let k = self.kit(f);
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
    paint::paint_list(&mut surf, &list, 0, &mut P(&s), &[], None);
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

/// The first text run of a page laid out with the desktop's family `ui`: its family and width.
fn first_run(html: &str, ui: usize) -> (Family, i32) {
    let s = sys_ui(ui);
    let fonts = Fonts { text: Some(&s) };
    let mut d = Document::parse(html.as_bytes(), "http://x.test/", Some("text/html"));
    let list = d.layout(&Viewport::new(800, 600), &fonts).clone();
    let run = list
        .entries
        .iter()
        .find_map(|e| match e.item {
            Item::Text(t) if !list.run_text(&t).trim().is_empty() => Some(t),
            _ => None,
        })
        .expect("a text run");
    (run.font.family, run.width)
}

#[test]
fn unstyled_pages_follow_the_settings_font() {
    let text = "<p>Unstyled text in whatever the desktop uses</p>";
    // Figtree (0), then Fira Sans (2) as the Settings font: the default family is the UI one.
    let (fam, w0) = first_run(text, 0);
    let (_, w2) = first_run(text, 2);
    assert_eq!(
        fam,
        Family::SansSerif,
        "pages that name no font get sans-serif, not serif"
    );
    assert_eq!((family_index(fam, 0), family_index(fam, 2)), (0, 2));
    assert_ne!(w0, w2, "the text was measured in the switched family");
    // A page that asks for serif gets Plex whatever Settings says; monospace stays Unifont.
    let serif = "<p style='font-family: Georgia, serif'>Serif text</p>";
    let ((fam, ws0), (_, ws2)) = (first_run(serif, 0), first_run(serif, 2));
    assert_eq!((fam, family_index(fam, 2), ws0), (Family::Serif, 1, ws2));
    assert_eq!(first_run("<pre>code</pre>", 2).0, Family::Monospace);
    // An unknown font name ends at the default, the UI family.
    assert_eq!(
        first_run("<p style='font-family: NoSuchFont'>x</p>", 2).0,
        Family::SansSerif
    );
    // The built-in pages (system-ui) follow it too.
    let html = crate::pages::new_tab(&crate::bookmarks::Bookmarks::default(), false);
    let (fam, _) = first_run(&html, 3);
    assert_eq!(family_index(fam, 3), 3, "{fam:?}");
}
