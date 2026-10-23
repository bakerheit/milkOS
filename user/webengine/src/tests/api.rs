//! The app-facing calls: resources, charsets, forms, hit testing, find, fragments.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::api::*;

struct F {
    sheets: BTreeMap<&'static str, &'static str>,
    asked: Vec<String>,
}

impl Fetcher for F {
    fn stylesheet(&mut self, url: &str) -> Option<Vec<u8>> {
        self.asked.push(String::from(url));
        self.sheets.get(url).map(|s| s.as_bytes().to_vec())
    }
    fn image(&mut self, url: &str, _id: ImageId) -> Option<(u32, u32)> {
        self.asked.push(String::from(url));
        if url.ends_with("bg.png") {
            Some((10, 10))
        } else {
            Some((64, 32))
        }
    }
}

fn lay(d: &mut Document) {
    d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
}

#[test]
fn stylesheets_imports_and_background_images() {
    let html = "<link rel=stylesheet href=/a.css><link rel='alternate stylesheet' href=/alt.css><style>@import 'b.css' screen;</style><div id=x class=c>hi</div><img src='pic.png'>";
    let mut d = Document::parse_str(html, "http://site.example/dir/page");
    let mut f = F {
        sheets: BTreeMap::from([
            (
                "http://site.example/a.css",
                "@import url(c.css); .c { height: 33px }",
            ),
            ("http://site.example/dir/b.css", "#x { width: 44px }"),
            (
                "http://site.example/c.css",
                "div { background: url(img/bg.png) }",
            ),
        ]),
        asked: Vec::new(),
    };
    d.load_resources(&mut f);
    assert!(!f.asked.iter().any(|u| u.contains("alt.css")));
    assert!(
        f.asked
            .contains(&String::from("http://site.example/img/bg.png")),
        "{:?}",
        f.asked
    );
    assert!(d.pending_resources().is_empty());
    lay(&mut d);
    let r = d.page.rect_of_id("x").unwrap();
    assert_eq!((r.w, r.h), (44, 33));
    assert!(
        d.display_list()
            .entries
            .iter()
            .any(|e| matches!(e.item, Item::Tiled { .. }))
    );
    assert!(
        d.display_list()
            .entries
            .iter()
            .any(|e| matches!(e.item, Item::Image { .. }))
    );
}

#[test]
fn failed_resources_dont_block() {
    let mut d = Document::parse_str(
        "<link rel=stylesheet href=x.css><img src=y.png alt=why>",
        "http://s.example/",
    );
    for r in d.pending_resources() {
        match r.kind {
            ResourceKind::Stylesheet => d.supply_stylesheet(&r.url, None),
            ResourceKind::Image(id) => d.supply_image(id, None),
        }
    }
    assert!(d.pending_resources().is_empty());
    lay(&mut d);
    assert!(d.text().contains("why"));
}

#[test]
fn charsets() {
    // windows-1252 by meta, by header, UTF-8 by BOM and by validity, UTF-16 by BOM.
    let mut b = b"<meta charset=windows-1252><p>caf\xe9 \x93q\x94".to_vec();
    let mut d = Document::parse(&b, "http://x/", None);
    lay(&mut d);
    assert!(d.text().contains("café \u{201C}q\u{201D}"), "{}", d.text());
    b = b"<p>na\xefve".to_vec();
    let mut d = Document::parse(&b, "http://x/", Some("text/html; charset=ISO-8859-1"));
    lay(&mut d);
    assert!(d.text().contains("naïve"));
    let mut d = Document::parse(
        "\u{FEFF}<p>żółw".as_bytes(),
        "http://x/",
        Some("text/html; charset=latin1"),
    );
    lay(&mut d);
    assert!(d.text().contains("żółw"));
    let mut u16le = alloc::vec![0xFF, 0xFE];
    for u in "<p>hi ☃".encode_utf16() {
        u16le.extend_from_slice(&u.to_le_bytes());
    }
    let mut d = Document::parse(&u16le, "http://x/", None);
    lay(&mut d);
    assert!(d.text().contains("hi ☃"));
    let mut d = Document::parse(
        b"line one\r\n<b>not bold</b>",
        "http://x/f.txt",
        Some("text/plain"),
    );
    lay(&mut d);
    assert!(d.text().contains("<b>not bold</b>"));
}

#[test]
fn title_base_favicon_refresh() {
    let d = Document::parse_str(
        "<title>  A\n  title </title><base href='https://cdn.example/root/'><link rel='shortcut icon' href=fav.ico><meta http-equiv=Refresh content=\"3; URL='../next'\">",
        "http://x/",
    );
    assert_eq!(d.title(), "A title");
    assert_eq!(d.base_url(), "https://cdn.example/root/");
    assert_eq!(
        d.favicon().as_deref(),
        Some("https://cdn.example/root/fav.ico")
    );
    assert_eq!(
        d.meta_refresh(),
        Some((3, Some(String::from("https://cdn.example/next"))))
    );
    assert_eq!(d.resolve("a b?c#d"), "https://cdn.example/root/a b?c#d");
}

fn control(d: &Document, name: &str) -> ControlId {
    d.controls()
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("{name}"))
        .id
}

#[test]
fn form_controls_and_submission() {
    let html = include_str!("../../tests/fixtures/forms.html");
    let mut d = Document::parse_str(html, "http://site.example/app/forms.html");
    lay(&mut d);
    let user = control(&d, "user");
    assert_eq!(d.controls()[user.0 as usize].value, "milk");
    assert!(!d.controls()[user.0 as usize].rect.is_empty());
    d.set_value(user, "cow & calf");
    d.set_value(control(&d, "pass"), "s3cret");
    // Radios: checking B unchecks A.
    let b = d
        .controls()
        .iter()
        .find(|c| c.kind == ControlKind::Radio && c.value == "b")
        .unwrap()
        .id;
    d.set_checked(b, true);
    assert!(
        !d.controls()
            .iter()
            .find(|c| c.kind == ControlKind::Radio && c.value == "a")
            .unwrap()
            .checked
    );
    let plan = control(&d, "plan");
    assert_eq!(
        d.controls()[plan.0 as usize]
            .options
            .iter()
            .position(|o| o.selected),
        Some(1)
    );
    d.select_option(plan, 2, true);
    // The <button> submits; "go" isn't the submitter so it's not sent.
    let btn = d.controls().iter().find(|c| c.content_drawn).unwrap().id;
    let s = d.submit(btn, None).unwrap();
    assert_eq!(s.method, Method::Post);
    assert_eq!(s.url, "http://site.example/login");
    let body = String::from_utf8(s.body).unwrap();
    assert_eq!(
        body,
        "user=cow+%26+calf&pass=s3cret&remember=on&mode=b&plan=Team&notes=Hello%0D%0Aworld&csrf=tok+en"
    );
    // Submit via the named input: it's included.
    let go = control(&d, "go");
    let s = d.submit(go, None).unwrap();
    assert!(String::from_utf8(s.body).unwrap().contains("&go=Go"));
    // Reset restores the defaults.
    d.reset(user);
    assert_eq!(d.controls()[user.0 as usize].value, "milk");
    // GET form: the query replaces the action's.
    let q = control(&d, "q");
    let s = d.submit(q, None).unwrap();
    assert_eq!(s.method, Method::Get);
    assert_eq!(s.url, "http://site.example/app/search?q=milk+%26+honey");
}

#[test]
fn maxlength_disabled_readonly() {
    let mut d = Document::parse_str(
        "<form><input name=a maxlength=3><input name=b disabled value=x><input name=c readonly value=y><fieldset disabled><input name=d value=z></fieldset><input type=submit></form>",
        "http://x/",
    );
    lay(&mut d);
    let a = control(&d, "a");
    d.set_value(a, "abcdef");
    assert_eq!(d.controls()[a.0 as usize].value, "abc");
    d.set_value(control(&d, "c"), "changed");
    assert_eq!(d.controls()[control(&d, "c").0 as usize].value, "y");
    let sub = d
        .controls()
        .iter()
        .find(|c| c.kind == ControlKind::Submit)
        .unwrap()
        .id;
    let s = d.submit(sub, None).unwrap();
    assert_eq!(s.url, "http://x/?a=abc&c=y");
}

#[test]
fn hit_testing() {
    let html = "<style>body{margin:0} .fix{position:fixed;top:500px;left:0;width:100px;height:20px;background:#eee}</style>\
        <p style='margin:0;height:40px'><a href='/x' title=Tip target=_blank>link text</a> plain</p>\
        <label>Name <input name=n></label><div style='height:2000px'></div><div class=fix><a href=/top>T</a></div>";
    let mut d = Document::parse_str(html, "http://h.example/");
    lay(&mut d);
    let h = d.hit_test(150, 30, (0, 0));
    assert!(h.link.is_none());
    let h = d.hit_test(5, 10, (0, 0));
    let l = h.link.expect("link");
    assert_eq!(l.url, "http://h.example/x");
    assert_eq!(l.target.as_deref(), Some("_blank"));
    assert_eq!(h.title.as_deref(), Some("Tip"));
    // The label points at its control.
    let n = control(&d, "n");
    let r = d.controls()[n.0 as usize].rect;
    assert_eq!(d.hit_test(r.x + 2, r.y + 2, (0, 0)).control, Some(n));
    assert_eq!(d.hit_test(5, r.y + 5, (0, 0)).control, Some(n));
    // The fixed box is hit where it's drawn, however far we scrolled.
    let h = d.hit_test(3, 510, (0, 1500));
    assert_eq!(
        h.link.map(|l| l.url).as_deref(),
        Some("http://h.example/top")
    );
}

#[test]
fn find_and_fragments() {
    let html = "<p>The <b>Milk</b>y Way and milk.</p><div style='height:500px'></div><h2 id=s2>Second</h2><a name=old></a><p>more milk</p>";
    let mut d = Document::parse_str(html, "http://f.example/");
    lay(&mut d);
    let m = d.find("milk", false);
    assert_eq!(m.len(), 3);
    assert_eq!(m[0].rects.len(), 1);
    assert!(d.find("Milk", true).len() == 1);
    assert_eq!(d.find("milky way", false).len(), 1);
    assert!(d.find("zebra", false).is_empty());
    let y = d.fragment_y("s2").unwrap();
    assert!(y > 500, "{y}");
    assert!(d.fragment_y("old").unwrap() >= y);
    assert_eq!(d.fragment_y("top"), Some(0));
    assert_eq!(d.fragment_y("nope"), None);
}

#[test]
fn main_content_heuristic() {
    let mut d = Document::parse_str(
        "<nav style='height:100px'>n</nav><div role=main id=m>main</div>",
        "http://m/",
    );
    lay(&mut d);
    assert_eq!(d.main_content().map(|r| r.y), Some(100 + 8));
    let mut d = Document::parse_str(
        "<div style='height:50px'></div><article>a</article>",
        "http://m/",
    );
    lay(&mut d);
    assert!(d.main_content().is_some());
    let mut d = Document::parse_str("<p>nothing</p>", "http://m/");
    lay(&mut d);
    assert!(d.main_content().is_none());
}

#[test]
fn display_list_culling() {
    let html: String = (0..400)
        .map(|i| alloc::format!("<p>line {i}</p>"))
        .collect();
    let mut d = Document::parse_str(&html, "http://c/");
    lay(&mut d);
    let dl = d.display_list();
    let area = Rect::new(0, 3000, 800, 600);
    let vis = dl.visible(area);
    let brute: Vec<&Entry> = dl
        .entries
        .iter()
        .filter(|e| entry_rect(&e.item).intersects(&area))
        .collect();
    assert_eq!(vis.len(), brute.len());
    assert!(vis.len() < dl.entries.len() / 4);
}
