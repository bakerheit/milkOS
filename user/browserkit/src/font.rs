//! Text for pages and the Browser's chrome, in the Unifont 8x16 cells the rest of user space
//! uses (Latin-1; typographic characters folded to ASCII, anything else a box). Sizes of 24
//! px and up are drawn at twice the size. When user space gets the system's proportional
//! fonts, this is the file that changes ([`Metrics`] is the renderer's [`Host`] half).

use crate::engine::{Font, Host};

#[allow(dead_code)]
mod data {
    include!("../../../kernel/src/font_data.rs");
}

pub const CELL_W: i32 = 8;
pub const CELL_H: i32 = 16;

/// The glyph scale for a font size.
pub fn scale(f: Font) -> i32 {
    if f.size >= 24 { 2 } else { 1 }
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

/// Every glyph `s` is drawn with (folded characters may become several).
pub fn glyphs(s: &str) -> impl Iterator<Item = &'static [u8; 16]> + '_ {
    s.chars().flat_map(|c| {
        let cp = c as u32;
        let folded = if cp > 0xFF {
            milkweb::html::fold(c)
        } else {
            None
        };
        let (one, many) = match folded {
            Some(f) => (None, Some(f.chars().map(glyph))),
            None => (Some(glyph(if c == '\t' { ' ' } else { c })), None),
        };
        one.into_iter().chain(many.into_iter().flatten())
    })
}

/// How many cells `s` takes.
pub fn cells(s: &str) -> i32 {
    glyphs(s).count() as i32
}

/// Width of `s` in `f`, in pixels.
pub fn width(s: &str, f: Font) -> i32 {
    cells(s) * CELL_W * scale(f)
}

/// Line height of `f`.
pub fn line_height(f: Font) -> i32 {
    CELL_H * scale(f) + 4
}

/// The renderer's measuring half of [`Host`] over Unifont, plus an image size lookup.
pub struct Metrics<'a> {
    pub image_size: &'a dyn Fn(&str) -> Option<(i32, i32)>,
}

impl Host for Metrics<'_> {
    fn text_width(&self, text: &str, font: Font) -> i32 {
        width(text, font)
    }
    fn line_height(&self, font: Font) -> i32 {
        line_height(font)
    }
    fn image_size(&self, url: &str) -> Option<(i32, i32)> {
        (self.image_size)(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures() {
        assert_eq!(width("abc", Font::BODY), 24);
        assert_eq!(
            width(
                "abc",
                Font {
                    size: 32,
                    ..Font::BODY
                }
            ),
            48
        );
        // An em dash folds to "-", an ellipsis to "...", a cow is a box.
        assert_eq!(cells("a\u{2014}b"), 3);
        assert_eq!(cells("\u{2026}"), 3);
        assert_eq!(cells("\u{1F404}"), 1);
        assert_eq!(line_height(Font::BODY), 20);
    }
}
