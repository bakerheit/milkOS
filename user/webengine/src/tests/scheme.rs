//! The UA's colours: milk and espresso by default, and the dark defaults only for a page that
//! opts into a dark colour scheme or leaves every colour to the browser, while the desktop is
//! dark.

use crate::api::*;

const MILK: Color = 0xFFFF_FDF8;
const ESPRESSO: Color = 0xFF3A_2E28;
const LINK: Color = 0xFF4F_64A0;
const DARK: Color = 0xFF2E_2520;
const DARK_TEXT: Color = 0xFFF4_EBE0;
const DARK_LINK: Color = 0xFFA9_B8E8;

fn render(html: &str, dark: bool) -> DisplayList {
    let mut d = Document::parse_str(html, "http://x.test/");
    let vp = Viewport {
        dark,
        ..Viewport::new(600, 400)
    };
    d.layout(&vp, &ApproxMeasurer).clone()
}

/// The colours of the text runs, in order.
fn text_colors(l: &DisplayList) -> alloc::vec::Vec<Color> {
    l.entries
        .iter()
        .filter_map(|e| match e.item {
            Item::Text(t) if !l.run_text(&t).trim().is_empty() => Some(t.color),
            _ => None,
        })
        .collect()
}

#[test]
fn light_defaults_are_milk_and_espresso() {
    for dark in [false, true] {
        // (A page with a style of its own stays light even on a dark desktop.)
        let l = render(
            "<style>p{margin:4px}</style><h1>Head</h1><p>text <a href=/x>link</a></p>",
            dark,
        );
        assert_eq!(l.background, MILK, "dark={dark}");
        assert_eq!(text_colors(&l), [ESPRESSO, ESPRESSO, LINK]);
    }
}

#[test]
fn plain_pages_follow_the_desktop() {
    let html = "<h1>Head</h1><p>text <a href=/x>link</a></p>";
    assert_eq!(render(html, false).background, MILK);
    let l = render(html, true);
    assert_eq!(l.background, DARK);
    assert_eq!(text_colors(&l), [DARK_TEXT, DARK_TEXT, DARK_LINK]);
}

#[test]
fn a_page_that_sets_colours_but_no_background_stays_light() {
    for html in [
        "<p style='color:#333'>grey text</p>",
        "<body text=#333333><p>old-style text colour</p>",
        "<p><font color=navy>navy</font></p>",
    ] {
        let l = render(html, true);
        assert_eq!(l.background, MILK, "{html}");
    }
}

#[test]
fn opting_in_to_dark() {
    for html in [
        "<meta name=color-scheme content='light dark'><style>p{margin:0}</style><p>x</p>",
        "<style>:root{color-scheme: light dark}</style><p>x</p>",
        "<html style='color-scheme: dark'><p>x</p>",
    ] {
        let l = render(html, true);
        assert_eq!(l.background, DARK, "{html}");
        assert_eq!(text_colors(&l), [DARK_TEXT], "{html}");
        assert_eq!(
            render(html, false).background,
            MILK,
            "light desktop: {html}"
        );
    }
    // `light` only: never dark.
    let l = render("<meta name=color-scheme content=light><p>x</p>", true);
    assert_eq!(l.background, MILK);
    // An opted-in page's own colours still win.
    let l = render(
        "<style>:root{color-scheme:dark} body{background:#ff0000} p{color:#00ff00}</style><p>x</p>",
        true,
    );
    assert_eq!(l.background, 0xFFFF_0000);
    assert_eq!(text_colors(&l), [0xFF00_FF00]);
}

#[test]
fn prefers_color_scheme_media_query() {
    let html = "<style>p{color:#111111} @media (prefers-color-scheme: dark){p{color:#eeeeee}}</style><p>x</p>";
    assert_eq!(text_colors(&render(html, false)), [0xFF11_1111]);
    assert_eq!(text_colors(&render(html, true)), [0xFFEE_EEEE]);
}
