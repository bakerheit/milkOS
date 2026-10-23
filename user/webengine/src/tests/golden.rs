//! Golden tests: each fixture page rendered (ApproxMeasurer, 800 px) to a display-list dump
//! and compared with tests/golden/NAME.txt. After an intended change, regenerate with
//!   WEBENGINE_BLESS=1 cargo test -p webengine --lib --target aarch64-apple-darwin golden
//! and review the diff.

use super::fixtures::*;
use super::render::dump;
use crate::api::*;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");

fn check(name: &str, html: &str) -> Option<alloc::string::String> {
    let mut d = Document::parse_str(html, "http://fixture.example/dir/page.html");
    let got = dump(d.layout(&Viewport::new(800, 600), &ApproxMeasurer));
    let path = alloc::format!("{DIR}/{name}.txt");
    if std::env::var("WEBENGINE_BLESS").is_ok() {
        std::fs::create_dir_all(DIR).unwrap();
        std::fs::write(&path, &got).unwrap();
        return None;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    if got == want {
        return None;
    }
    // The first differing line, for the message.
    let (i, (g, w)) = got
        .lines()
        .zip(want.lines().chain(core::iter::repeat("<end>")))
        .enumerate()
        .find(|(_, (g, w))| g != w)
        .unwrap_or((got.lines().count(), ("<end>", "<more>")));
    Some(alloc::format!(
        "{name}.txt line {}:\n  got  {g}\n  want {w}",
        i + 1
    ))
}

#[test]
fn golden_display_lists() {
    let mut errs = alloc::vec::Vec::new();
    for (name, html) in [
        ("article", ARTICLE),
        ("layout", LAYOUT),
        ("tables", TABLES),
        ("forms", FORMS),
        ("text", TEXT),
    ] {
        if let Some(e) = check(name, html) {
            errs.push(e);
        }
    }
    assert!(
        errs.is_empty(),
        "golden mismatch (WEBENGINE_BLESS=1 to update):\n{}",
        errs.join("\n")
    );
}
