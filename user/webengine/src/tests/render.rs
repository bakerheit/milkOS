//! Rendering tests: fixture pages through the whole pipeline, checked on the display list.

use alloc::string::String;
use alloc::vec::Vec;

use crate::api::*;

/// A readable dump of a display list (one line per entry).
pub fn dump(dl: &DisplayList) -> String {
    let mut s = String::new();
    for e in dl.entries.iter().chain(dl.fixed.iter()) {
        let line = match &e.item {
            Item::Rect { rect, color } => alloc::format!(
                "rect {},{} {}x{} #{:08x}",
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                color
            ),
            Item::RoundRect {
                rect,
                color,
                radius,
            } => {
                alloc::format!(
                    "rrect {},{} {}x{} r{} #{:08x}",
                    rect.x,
                    rect.y,
                    rect.w,
                    rect.h,
                    radius,
                    color
                )
            }
            Item::Border { rect, widths, .. } => alloc::format!(
                "border {},{} {}x{} {:?}",
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                widths
            ),
            Item::Text(t) => alloc::format!(
                "text {},{} w{} {:?}{} {}{} #{:08x} {:?}",
                t.x,
                t.baseline,
                t.width,
                t.font.family,
                t.font.size,
                t.font.weight,
                if t.font.italic { "i" } else { "" },
                t.color,
                dl.run_text(t)
            ),
            Item::Image { rect, image } => alloc::format!(
                "image {},{} {}x{} #{}",
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                image.0
            ),
            Item::Tiled {
                area, tile, image, ..
            } => alloc::format!(
                "tiled {},{} {}x{} tile {},{} {}x{} #{}",
                area.x,
                area.y,
                area.w,
                area.h,
                tile.x,
                tile.y,
                tile.w,
                tile.h,
                image.0
            ),
            Item::Marker { rect, kind, .. } => alloc::format!(
                "marker {:?} {},{} {}x{}",
                kind,
                rect.x,
                rect.y,
                rect.w,
                rect.h
            ),
            Item::Control { rect, control } => alloc::format!(
                "control {} {},{} {}x{}",
                control.0,
                rect.x,
                rect.y,
                rect.w,
                rect.h
            ),
        };
        s.push_str(&line);
        s.push('\n');
    }
    s
}

pub fn render(html: &str, w: i32) -> (Document, String) {
    let mut d = Document::parse_str(html, "http://test.example/dir/page.html");
    let dl = d.layout(&Viewport::new(w, 600), &ApproxMeasurer);
    let s = dump(dl);
    (d, s)
}

pub fn texts(dl: &DisplayList) -> Vec<(i32, i32, String)> {
    dl.entries
        .iter()
        .filter_map(|e| match &e.item {
            Item::Text(t) => Some((t.x, t.baseline, String::from(dl.run_text(t)))),
            _ => None,
        })
        .collect()
}

#[test]
fn debug_dump() {
    let (_, s) = render(
        "<!doctype html><style>h1{color:red} .x{background:#eee;padding:4px}</style>\
         <h1>Hello</h1><p>Some <b>bold</b> and <a href=/x>a link</a> text that wraps around the line when it gets long enough.</p>\
         <div class=x>box</div><ul><li>one<li>two</ul>",
        300,
    );
    std::println!("{s}");
}

#[test]
fn debug_abs() {
    let (_, s) = render(
        "<div style='position:relative;width:200px;border:1px solid'><span style='position:absolute;top:-8px;right:-8px;font-size:11px;padding:2px 6px;background:red'>new</span><h3>Two</h3></div>",
        400,
    );
    std::println!("{s}");
}

#[test]
fn debug_z() {
    let (_, s) = render("<style>body{margin:0}</style><div style='position:relative;z-index:2;background:#000001;height:10px'></div><div style='position:relative;z-index:1;background:#000002;height:10px'></div>", 400);
    std::println!("{s}");
}
