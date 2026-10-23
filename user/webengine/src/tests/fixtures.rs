//! The fixture pages (tests/fixtures/*.html) through the whole engine at a few widths, with
//! screenshots when WEBENGINE_SHOTS is set.

use super::raster::{SysMeasurer, save, screenshot};
use crate::api::*;

pub const ARTICLE: &str = include_str!("../../tests/fixtures/article.html");
pub const LAYOUT: &str = include_str!("../../tests/fixtures/layout.html");
pub const TABLES: &str = include_str!("../../tests/fixtures/tables.html");
pub const FORMS: &str = include_str!("../../tests/fixtures/forms.html");
pub const TEXT: &str = include_str!("../../tests/fixtures/text.html");

#[test]
fn fixture_screenshots() {
    for (name, html) in [("article", ARTICLE), ("layout", LAYOUT), ("tables", TABLES), ("forms", FORMS), ("text", TEXT)] {
        for w in [1024, 480] {
            let (d, c) = screenshot(html, w, 1400);
            save(&alloc::format!("{name}-{w}"), &c);
            assert!(!d.display_list().entries.is_empty(), "{name}");
        }
    }
    let _ = SysMeasurer;
    let _ = Viewport::new(1, 1);
}
