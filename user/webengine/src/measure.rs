//! [`ApproxMeasurer`]: text measuring without font data (Helvetica-like advance widths from
//! the standard Helvetica AFM, in 1/1000 em; monospace is 600 everywhere).

use crate::api::{ApproxMeasurer, Family, Font, FontMetrics, TextMeasurer};

const ASCII: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, // ' '../
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, // 0-9
    278, 278, 584, 584, 584, 556, 1015, // :;<=>?@
    667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667,
    611, 722, 667, 944, 667, 667, 611, // A-Z
    278, 278, 278, 469, 556, 333, // [\]^_`
    556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500,
    278, 556, 500, 722, 500, 500, 500, // a-z
    334, 260, 334, 584, // {|}~
];

fn advance(c: char, mono: bool) -> u32 {
    if mono {
        return if crate::layout::inline::is_cjk(c) {
            1000
        } else {
            600
        };
    }
    let u = c as u32;
    if (0x20..0x7F).contains(&u) {
        ASCII[(u - 0x20) as usize] as u32
    } else if crate::layout::inline::is_cjk(c) {
        1000
    } else if u < 0x20 || (0x300..0x370).contains(&u) || u == 0x200B || u == 0xAD {
        0
    } else {
        556
    }
}

impl TextMeasurer for ApproxMeasurer {
    fn text_width(&self, font: &Font, text: &str) -> i32 {
        let mono = font.family == Family::Monospace;
        let mut units: u64 = text.chars().map(|c| advance(c, mono) as u64).sum();
        if font.weight >= 600 && !mono {
            units = units * 107 / 100;
        }
        ((units * font.size as u64 + 500) / 1000) as i32
    }
    fn metrics(&self, font: &Font) -> FontMetrics {
        let s = font.size as i32;
        let ascent = (s * 8 + 5) / 10;
        let descent = ((s * 2 + 5) / 10).max(1);
        FontMetrics {
            ascent,
            descent,
            line_height: ((s * 12 + 5) / 10).max(ascent + descent),
        }
    }
}
