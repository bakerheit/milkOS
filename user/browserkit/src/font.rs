//! Text for pages and the Browser's chrome, in the Unifont 8x16 cells the rest of user space
//! uses (Latin-1; typographic characters folded to ASCII, anything else a box). Fonts of 24
//! px and up are drawn at twice the size, 48 and up three times. [`Unifont`] is the page
//! renderer's [`TextMeasurer`], so the engine breaks lines exactly where the glyphs are drawn.
//! When user space gets the system's proportional fonts, this is the file that changes.

use crate::engine::{Font, FontMetrics, TextMeasurer};

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
    if size >= 48 {
        3
    } else if size >= 24 {
        2
    } else {
        1
    }
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

/// The page renderer's measurer over Unifont.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unifont;

impl TextMeasurer for Unifont {
    fn text_width(&self, font: &Font, text: &str) -> i32 {
        width_at(text, font.size as i32)
    }
    fn metrics(&self, font: &Font) -> FontMetrics {
        let s = scale(font);
        FontMetrics {
            ascent: ASCENT * s,
            descent: (CELL_H - ASCENT) * s,
            line_height: CELL_H * s + 4,
        }
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
        assert_eq!(Unifont.metrics(&font(16)).line_height, 20);
        // An em dash folds to "-", an ellipsis to "...", a cow is a box, a soft hyphen nothing.
        assert_eq!(cells("a\u{2014}b"), 3);
        assert_eq!(cells("\u{2026}"), 3);
        assert_eq!(cells("\u{1F404}"), 1);
        assert_eq!(cells("soft\u{ad}ware"), 8);
    }
}
