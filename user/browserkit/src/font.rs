//! Text for pages. Proportional text is the app's: the system fonts, behind [`Text`] (the
//! Browser's is gui::text; tests that have none use Unifont for everything). Monospace
//! (`<pre>`, `<code>`, CSS `monospace`) is always Unifont 8x16 cells (Latin-1; typographic
//! characters folded to ASCII, anything else a box). Sizes snap to the system fonts' three:
//! 13, 26 and 52 px ([`snap`]: scale 1, 2, 4), for both. [`Fonts`] is the page renderer's
//! [`TextMeasurer`], so the engine breaks lines exactly where the glyphs are drawn.

use crate::engine::{Family, Font, FontMetrics, Rect, TextMeasurer};

/// Proportional text in the system fonts: measuring and drawing that agree.
pub trait Text {
    /// The width of `s` (one line) in `f`.
    fn width(&self, f: &Font, s: &str) -> i32;
    /// Draw `s` in `f` into `px` (rows of `stride` 0x00RRGGBB pixels) with the top of its line
    /// box (16 px a scale, the baseline 13 down) at (x, y), only inside `clip`. Returns the end x.
    #[allow(clippy::too_many_arguments)]
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
    ) -> i32;
}

/// The scale (1, 2 or 4: 13, 26 or 52 px) nearest a font size in pixels.
pub fn snap(px: i32) -> i32 {
    if px >= 39 {
        4
    } else if px >= 20 {
        2
    } else {
        1
    }
}

/// Is this font drawn in Unifont (monospace)?
pub fn is_mono(f: &Font) -> bool {
    f.family == Family::Monospace
}

/// The renderer's measurer: proportional text through `text` (None: Unifont), monospace in
/// Unifont.
#[derive(Clone, Copy)]
pub struct Fonts<'a> {
    pub text: Option<&'a dyn Text>,
}

impl Fonts<'_> {
    /// Unifont for everything (host tests, or no system fonts).
    pub const UNIFONT: Fonts<'static> = Fonts { text: None };

    /// The proportional backend for `f`, if it isn't Unifont's.
    pub fn backend(&self, f: &Font) -> Option<&dyn Text> {
        if is_mono(f) { None } else { self.text }
    }

    pub fn width(&self, f: &Font, s: &str) -> i32 {
        match self.backend(f) {
            Some(t) => t.width(f, s),
            None => mono_segments(s, self.text.is_some())
                .map(|(seg, uni)| match (uni, self.text) {
                    (false, Some(t)) => t.width(&proportional(f), seg),
                    _ => width_at(seg, f.size as i32),
                })
                .sum(),
        }
    }
}

/// The proportional font monospace falls back to for characters Unifont lacks (Greek in a
/// `<code>`): the same size and weight, sans-serif.
pub fn proportional(f: &Font) -> Font {
    Font {
        family: Family::SansSerif,
        ..*f
    }
}

/// Can Unifont draw `c` (Latin-1, or folded to it, or nothing to draw)?
pub fn unifont_has(c: char) -> bool {
    let u = c as u32;
    u <= 0xFF || invisible(c) || milkweb::html::fold(c).is_some()
}

/// `s` in runs Unifont draws (true) and runs it can't (false). With `split` false, one run.
pub fn mono_segments(s: &str, split: bool) -> impl Iterator<Item = (&str, bool)> {
    let mut rest = s;
    core::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        if !split {
            let all = rest;
            rest = "";
            return Some((all, true));
        }
        let first = unifont_has(rest.chars().next().unwrap());
        let end = rest
            .char_indices()
            .find(|(_, c)| unifont_has(*c) != first)
            .map_or(rest.len(), |(i, _)| i);
        let (a, b) = rest.split_at(end);
        rest = b;
        Some((a, first))
    })
}

impl TextMeasurer for Fonts<'_> {
    fn text_width(&self, font: &Font, text: &str) -> i32 {
        self.width(font, text)
    }
    fn metrics(&self, font: &Font) -> FontMetrics {
        metrics(font)
    }
}

/// Line metrics, the same for Unifont and the system fonts (they share the 16 px line box with
/// the baseline at 13), plus a little leading.
pub fn metrics(font: &Font) -> FontMetrics {
    let s = scale(font);
    FontMetrics {
        ascent: ASCENT * s,
        descent: (CELL_H - ASCENT) * s,
        line_height: (CELL_H + 2) * s,
    }
}

#[allow(dead_code)]
mod data {
    include!("../../../kernel/src/font_data.rs");
}

pub const CELL_W: i32 = 8;
pub const CELL_H: i32 = 16;
/// Rows of a cell above the baseline.
pub const ASCENT: i32 = 13;

/// The glyph scale for a font size in pixels.
pub fn scale_for(size: i32) -> i32 {
    snap(size)
}

pub fn scale(f: &Font) -> i32 {
    scale_for(f.size as i32)
}

/// The glyph bitmap for one drawable character.
fn glyph(c: char) -> &'static [u8; 16] {
    let cp = c as u32;
    if (data::FIRST_CHAR..=data::LAST_CHAR).contains(&cp) {
        &data::GLYPHS[(cp - data::FIRST_CHAR) as usize]
    } else {
        &data::REPLACEMENT
    }
}

/// Characters that take no room (soft hyphen, zero-width space and joiners, combining marks).
fn invisible(c: char) -> bool {
    let u = c as u32;
    u == 0xAD || (0x200B..=0x200F).contains(&u) || u == 0xFEFF || (0x300..0x370).contains(&u)
}

/// Every glyph `s` is drawn with (a folded character may become several).
pub fn glyphs(s: &str) -> impl Iterator<Item = &'static [u8; 16]> + '_ {
    s.chars().filter(|c| !invisible(*c)).flat_map(|c| {
        let cp = c as u32;
        let folded = if cp > 0xFF {
            milkweb::html::fold(c)
        } else {
            None
        };
        let (one, many) = match folded {
            Some(f) => (None, Some(f.chars().map(glyph))),
            None => (
                Some(glyph(if c == '\t' || c == '\u{a0}' { ' ' } else { c })),
                None,
            ),
        };
        one.into_iter().chain(many.into_iter().flatten())
    })
}

/// How many cells `s` takes.
pub fn cells(s: &str) -> i32 {
    glyphs(s).count() as i32
}

/// Width of `s` at size `size`, in pixels.
pub fn width_at(s: &str, size: i32) -> i32 {
    cells(s) * CELL_W * scale_for(size)
}

/// Unifont for everything: [`Fonts::UNIFONT`] as a value.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unifont;

impl TextMeasurer for Unifont {
    fn text_width(&self, font: &Font, text: &str) -> i32 {
        width_at(text, font.size as i32)
    }
    fn metrics(&self, font: &Font) -> FontMetrics {
        metrics(font)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Family;

    fn font(size: u16) -> Font {
        Font {
            family: Family::SansSerif,
            size,
            weight: 400,
            italic: false,
        }
    }

    #[test]
    fn measures() {
        assert_eq!(Unifont.text_width(&font(16), "abc"), 24);
        assert_eq!(Unifont.text_width(&font(32), "abc"), 48);
        assert_eq!(Unifont.metrics(&font(16)).line_height, 18);
        assert_eq!(Unifont.text_width(&font(60), "a"), 32);
        assert_eq!(
            (snap(13), snap(19), snap(20), snap(32), snap(39), snap(80)),
            (1, 1, 2, 2, 4, 4)
        );
        // An em dash folds to "-", an ellipsis to "...", a cow is a box, a soft hyphen nothing.
        assert_eq!(cells("a\u{2014}b"), 3);
        assert_eq!(cells("\u{2026}"), 3);
        assert_eq!(cells("\u{1F404}"), 1);
        assert_eq!(cells("soft\u{ad}ware"), 8);
    }
}
