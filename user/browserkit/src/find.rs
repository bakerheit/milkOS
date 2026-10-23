//! Find in page: every place `needle` occurs in a laid-out page's text (ignoring case), as
//! rectangles in page coordinates, in reading order.

use alloc::string::String;
use alloc::vec::Vec;

use crate::engine::{DisplayList, Host, Item, Rect};

fn fold(s: &str) -> Vec<char> {
    s.chars().flat_map(char::to_lowercase).collect()
}

/// The matches of `needle` in `list` (at most `max`).
pub fn find(list: &DisplayList, needle: &str, host: &dyn Host, max: usize) -> Vec<Rect> {
    let n = fold(needle);
    if n.is_empty() || n.len() > 200 {
        return Vec::new();
    }
    let mut out = Vec::new();
    'items: for it in &list.items {
        let Item::Text {
            x, y, text, font, ..
        } = it
        else {
            continue;
        };
        let hay: Vec<char> = text.chars().collect();
        let low: Vec<char> = hay
            .iter()
            .map(|c| c.to_lowercase().next().unwrap_or(*c))
            .collect();
        if low.len() < n.len() {
            continue;
        }
        let mut i = 0;
        while i + n.len() <= low.len() {
            if low[i..i + n.len()] == n[..] {
                let pre: String = hay[..i].iter().collect();
                let m: String = hay[i..i + n.len()].iter().collect();
                let r = Rect::new(
                    x + host.text_width(&pre, *font),
                    *y,
                    host.text_width(&m, *font).max(2),
                    host.line_height(*font),
                );
                out.push(r);
                if out.len() >= max {
                    break 'items;
                }
                i += n.len();
            } else {
                i += 1;
            }
        }
    }
    out.sort_by_key(|r| (r.y, r.x));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::Metrics;

    #[test]
    fn finds_across_words_and_case() {
        let mut d = crate::engine::parse(
            b"<h1>Milk</h1><p>Cows give milk. MILK is good, <b>milk</b>shakes too.</p>",
            "text/html",
            "http://x.test/",
        );
        let none = |_: &str| None;
        let m = Metrics { image_size: &none };
        let l = d.layout(800, &m).clone();
        let r = find(&l, "milk", &m, 100);
        assert_eq!(r.len(), 4);
        assert!(r[0].y < r[1].y);
        assert_eq!(r[0].w, 2 * 4 * 8); // the h1 is drawn twice the size
        assert_eq!(find(&l, "give milk", &m, 100).len(), 1);
        assert_eq!(find(&l, "", &m, 100).len(), 0);
        assert_eq!(find(&l, "milk", &m, 2).len(), 2);
        assert_eq!(find(&l, "nothing", &m, 10).len(), 0);
    }
}
