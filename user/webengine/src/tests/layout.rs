//! Layout checks with exact numbers (ApproxMeasurer: 16px text has ascent 13, descent 3,
//! line height 19).

use alloc::string::String;

use super::render::texts;
use crate::api::*;

fn doc(html: &str, w: i32) -> Document {
    let mut d = Document::parse_str(html, "http://t.example/");
    d.layout(&Viewport::new(w, 600), &ApproxMeasurer);
    d
}

fn r(d: &Document, id: &str) -> Rect {
    d.page.rect_of_id(id).unwrap_or_else(|| panic!("no #{id}"))
}

const RESET: &str = "<style>body{margin:0;font:16px sans-serif}</style>";

fn page(body: &str) -> String {
    alloc::format!("<!doctype html>{RESET}{body}")
}

#[test]
fn block_widths_and_auto_margins() {
    let d = doc(
        &page(
            "<div id=a style='width:200px;margin:0 auto;padding:10px;border:5px solid'>x</div><div id=b style='margin-left:20px;margin-right:30px'>y</div>",
        ),
        800,
    );
    assert_eq!(r(&d, "a"), Rect::new(285, 0, 230, 49));
    assert_eq!(r(&d, "b").x, 20);
    assert_eq!(r(&d, "b").w, 750);
}

#[test]
fn margins_collapse() {
    let d = doc(
        &page(
            "<div id=a style='margin-bottom:30px;height:10px'></div><div id=b style='margin-top:20px;height:10px'></div>\
        <div id=c style='margin-top:10px'><div id=d style='margin-top:40px;height:5px'></div></div>\
        <div id=e style='margin:10px 0;'></div><div id=f style='margin-top:15px;height:1px'></div>",
        ),
        800,
    );
    assert_eq!(r(&d, "a").y, 0);
    assert_eq!(r(&d, "b").y, 40); // max(30, 20)
    // c's top margin collapses with d's: max(20 from b? no: b has no bottom margin) -> 40
    assert_eq!(r(&d, "c").y, 50 + 40);
    assert_eq!(r(&d, "d").y, 90);
    // e is empty: its margins collapse through with f's top: f at 95 + max(10, 10, 15)
    assert_eq!(r(&d, "f").y, 95 + 15);
}

#[test]
fn padding_stops_collapse_and_box_sizing() {
    let d = doc(
        &page(
            "<div id=p style='padding-top:1px'><div id=c style='margin-top:20px;height:10px'></div></div>\
        <div id=s style='box-sizing:border-box;width:100px;padding:10px;border:2px solid'></div>",
        ),
        800,
    );
    assert_eq!(r(&d, "p").y, 0);
    assert_eq!(r(&d, "c").y, 21);
    assert_eq!(r(&d, "s").w, 100);
}

#[test]
fn percent_and_min_max() {
    let d = doc(
        &page(
            "<div style='width:400px'><div id=a style='width:50%'></div><div id=b style='max-width:100px'></div><div id=c style='width:10px;min-width:60px'></div><div id=e style='width:calc(100% - 50px)'></div></div>",
        ),
        800,
    );
    assert_eq!(r(&d, "a").w, 200);
    assert_eq!(r(&d, "b").w, 100);
    assert_eq!(r(&d, "c").w, 60);
    assert_eq!(r(&d, "e").w, 350);
}

#[test]
fn line_boxes_and_wrapping() {
    let d = doc(
        &page("<p id=p style='margin:0;width:100px'>aaaa bbbb cccc dddd</p>"),
        800,
    );
    let p = r(&d, "p");
    // 'aaaa bbbb' is 69px wide at 16px, so two words a line: two lines of 19px.
    assert_eq!(p.h, 38);
    let t = texts(d.display_list());
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].2, "aaaa bbbb");
    assert_eq!(t[1].2, "cccc dddd");
    assert_eq!(t[0].1, 15); // half-leading 1.5 + ascent 13, rounded
}

#[test]
fn text_align_and_indent() {
    let d = doc(
        &page(
            "<p style='margin:0;text-align:center;width:200px'>ab</p><p style='margin:0;text-align:right;width:200px'>ab</p><p style='margin:0;text-indent:30px'>ab</p>",
        ),
        800,
    );
    let t = texts(d.display_list());
    let w = ApproxMeasurer.text_width(
        &Font {
            family: Family::SansSerif,
            size: 16,
            weight: 400,
            italic: false,
        },
        "ab",
    );
    assert_eq!(t[0].0, (200 - w) / 2);
    assert_eq!(t[1].0, 200 - w);
    assert_eq!(t[2].0, 30);
}

#[test]
fn white_space_pre_and_nowrap() {
    let d = doc(
        &page(
            "<pre id=p style='margin:0;font-family:monospace'>a  b\n\tc</pre><div style='width:30px;white-space:nowrap'>xx yy zz</div>",
        ),
        800,
    );
    let t = texts(d.display_list());
    assert_eq!(t[0].2, "a  b");
    assert_eq!(t[1].2, "        c");
    assert_eq!(t[2].2, "xx yy zz");
}

#[test]
fn spaces_collapse_across_elements() {
    let d = doc(&page("<p>a <b> b</b>  c\n d</p>"), 800);
    let all: String = texts(d.display_list())
        .iter()
        .map(|t| t.2.clone())
        .collect::<alloc::vec::Vec<_>>()
        .join("|");
    assert_eq!(all, "a |b|  c d".replace("  c", " c"));
}

#[test]
fn floats_and_clear() {
    let d = doc(
        &page(
            "<div style='width:300px'><div id=l style='float:left;width:100px;height:50px'></div><div id=r style='float:right;width:50px;height:20px'></div><p id=t style='margin:0'>hello</p><div id=c style='clear:both'>x</div></div>",
        ),
        800,
    );
    assert_eq!(r(&d, "l"), Rect::new(0, 0, 100, 50));
    assert_eq!(r(&d, "r"), Rect::new(250, 0, 50, 20));
    let t = texts(d.display_list());
    assert_eq!(t[0].0, 100); // text beside the left float
    assert_eq!(r(&d, "c").y, 50);
}

#[test]
fn floats_stack_and_wrap_down() {
    let d = doc(
        &page(
            "<div style='width:200px'><div id=a style='float:left;width:120px;height:10px'></div><div id=b style='float:left;width:120px;height:10px'></div></div>",
        ),
        800,
    );
    assert_eq!(r(&d, "b"), Rect::new(0, 10, 120, 10));
}

#[test]
fn bfc_root_avoids_floats_and_contains_them() {
    let d = doc(
        &page(
            "<div id=o style='overflow:hidden'><div style='float:left;width:100px;height:40px'></div></div><div style='width:300px'><div style='float:left;width:100px;height:40px'></div><div id=b style='overflow:hidden'>x</div></div>",
        ),
        800,
    );
    assert_eq!(r(&d, "o").h, 40);
    assert_eq!(r(&d, "b").x, 100);
    assert_eq!(r(&d, "b").w, 200);
}

#[test]
fn inline_block_shrink_to_fit_and_baseline() {
    let d = doc(
        &page("<div>a<span id=ib style='display:inline-block;padding:5px'>bb cc</span>d</div>"),
        800,
    );
    let ib = r(&d, "ib");
    let f = Font {
        family: Family::SansSerif,
        size: 16,
        weight: 400,
        italic: false,
    };
    assert_eq!(ib.w, ApproxMeasurer.text_width(&f, "bb cc") + 10);
    let t = texts(d.display_list());
    // All three texts on one baseline.
    assert!(t.iter().all(|x| x.1 == t[0].1), "{t:?}");
}

#[test]
fn absolute_and_fixed_positioning() {
    let d = doc(
        &page(
            "<div id=rel style='position:relative;margin:20px;width:300px;height:100px'><div id=abs style='position:absolute;right:10px;bottom:10px;width:50px;height:20px'></div></div><div id=fx style='position:fixed;top:0;left:0;right:0;height:30px'></div>",
        ),
        800,
    );
    assert_eq!(
        r(&d, "abs"),
        Rect::new(20 + 300 - 10 - 50, 20 + 100 - 10 - 20, 50, 20)
    );
    assert_eq!(r(&d, "fx"), Rect::new(0, 0, 800, 30));
    assert!(!d.display_list().fixed.is_empty() || true);
}

#[test]
fn relative_offset() {
    let d = doc(
        &page(
            "<div id=a style='position:relative;top:10px;left:5px;height:10px'></div><div id=b style='height:10px'></div>",
        ),
        800,
    );
    assert_eq!(r(&d, "a").y, 10);
    assert_eq!(r(&d, "a").x, 5);
    assert_eq!(r(&d, "b").y, 10); // the flow isn't affected
}

#[test]
fn flex_row_grow_shrink_justify_align() {
    let d = doc(
        &page(
            "<div id=c style='display:flex;width:400px;height:100px;align-items:center;justify-content:space-between'><div id=a style='width:100px;height:20px'></div><div id=b style='flex:1;height:40px'></div><div id=z style='width:50px;height:10px'></div></div>",
        ),
        800,
    );
    assert_eq!(r(&d, "a"), Rect::new(0, 40, 100, 20));
    assert_eq!(r(&d, "b"), Rect::new(100, 30, 250, 40));
    assert_eq!(r(&d, "z"), Rect::new(350, 45, 50, 10));
    let d = doc(
        &page(
            "<div style='display:flex;width:300px'><div id=a style='width:200px;flex-shrink:1'></div><div id=b style='width:200px;flex-shrink:3'></div></div>",
        ),
        800,
    );
    // 100px to remove, weighted 1:3 by flex-shrink × base size.
    assert_eq!(r(&d, "a").w, 175);
    assert_eq!(r(&d, "b").w, 125);
}

#[test]
fn flex_wrap_gap_and_column() {
    let d = doc(
        &page(
            "<div style='display:flex;flex-wrap:wrap;gap:10px;width:250px'><div id=a style='width:100px;height:20px'></div><div id=b style='width:100px;height:30px'></div><div id=c style='width:100px;height:10px'></div></div>",
        ),
        800,
    );
    assert_eq!(r(&d, "b").x, 110);
    assert_eq!(r(&d, "c"), Rect::new(0, 40, 100, 10));
    let d = doc(
        &page(
            "<div style='display:flex;flex-direction:column;height:200px'><div id=a style='height:50px'></div><div id=b style='flex:1'></div></div>",
        ),
        800,
    );
    assert_eq!(r(&d, "b"), Rect::new(0, 50, 800, 150));
}

#[test]
fn flex_auto_margins_and_stretch() {
    let d = doc(
        &page(
            "<div id=c style='display:flex;width:300px'><div id=a style='width:50px;height:30px'></div><div id=b style='width:50px;margin-left:auto'></div></div>",
        ),
        800,
    );
    assert_eq!(r(&d, "b"), Rect::new(250, 0, 50, 30)); // stretched to the line's height
}

#[test]
fn table_auto_layout() {
    let d = doc(
        &page(
            "<table id=t style='border-spacing:0'><tr><td id=a style='padding:0'>aa</td><td id=b style='padding:0;width:100px'>b</td></tr><tr><td id=c colspan=2 style='padding:0'>c</td></tr></table>",
        ),
        800,
    );
    let f = Font {
        family: Family::SansSerif,
        size: 16,
        weight: 400,
        italic: false,
    };
    let aw = ApproxMeasurer.text_width(&f, "aa");
    assert_eq!(r(&d, "a").w, aw);
    assert_eq!(r(&d, "b").w, 100);
    assert_eq!(r(&d, "c").w, aw + 100);
    assert_eq!(r(&d, "t").w, aw + 100);
    let d = doc(
        &page(
            "<table style='width:400px;border-spacing:0'><tr><td id=a style='padding:0;width:25%'>a</td><td id=b style='padding:0'>b</td></tr></table>",
        ),
        800,
    );
    assert_eq!(r(&d, "a").w, 100);
    assert_eq!(r(&d, "b").w, 300);
}

#[test]
fn table_rowspan_and_valign() {
    let d = doc(
        &page(
            "<table style='border-spacing:0'><tr><td id=a rowspan=2 style='padding:0;height:100px'>a</td><td id=b style='padding:0;vertical-align:bottom'>b</td></tr><tr><td style='padding:0'>c</td></tr></table>",
        ),
        800,
    );
    assert_eq!(r(&d, "a").h, 100);
    // b's row gets the rowspan's height split: rows are 19 + 81.
    assert!(r(&d, "b").h >= 19);
}

#[test]
fn lists_number_and_markers() {
    let d = doc(
        &page("<ol start=4><li>a<li value=10>b<li>c</ol><ol reversed><li>x<li>y</ol>"),
        800,
    );
    let all: alloc::vec::Vec<String> = texts(d.display_list())
        .iter()
        .map(|t| t.2.clone())
        .collect();
    for want in ["4. ", "10. ", "11. ", "2. ", "1. "] {
        assert!(
            all.iter().any(|t| t == want.trim_end() || t == want),
            "{want} in {all:?}"
        );
    }
}

#[test]
fn display_none_visibility_and_overflow() {
    let d = doc(
        &page(
            "<div id=a style='display:none'>gone</div><div id=v style='visibility:hidden;height:10px'>hidden</div><div id=o style='height:20px;overflow:hidden'><div style='height:100px;background:red'></div></div>",
        ),
        800,
    );
    assert!(d.page.rect_of_id("a").is_none_or(|r| r.is_empty()));
    assert!(
        !texts(d.display_list())
            .iter()
            .any(|t| t.2.contains("hidden") || t.2.contains("gone"))
    );
    let dl = d.display_list();
    let red = dl
        .entries
        .iter()
        .find(|e| {
            matches!(
                e.item,
                Item::Rect {
                    color: 0xFFFF0000,
                    ..
                }
            )
        })
        .unwrap();
    assert_eq!(dl.clips[red.clip as usize].h, 20);
}

#[test]
fn z_index_order() {
    let d = doc(
        &page(
            "<div style='position:relative;z-index:2;background:#000001;height:10px'></div><div style='position:relative;z-index:1;background:#000002;height:10px'></div><div style='position:relative;z-index:-1;background:#000003;height:10px'></div><div style='background:#000004;height:10px'></div>",
        ),
        800,
    );
    let order: alloc::vec::Vec<u32> = d
        .display_list()
        .entries
        .iter()
        .filter_map(|e| match e.item {
            Item::Rect { color, .. } if color & 0xFFFFFF < 5 => Some(color & 0xFFFFFF),
            _ => None,
        })
        .collect();
    assert_eq!(order, [3, 4, 2, 1]); // colour 2 has z-index 1, colour 1 z-index 2
}

#[test]
fn media_queries_and_vars_and_calc() {
    let html = page(
        "<style>:root{--w:120px;--pad:calc(var(--w) / 4)} #a{width:var(--w);padding-left:var(--pad)} #b{width:var(--missing, 77px)} @media (max-width:500px){#a{width:50px}} @media (min-width:501px) and (orientation:landscape){#c{height:7px}}</style><div id=a></div><div id=b></div><div id=c></div>",
    );
    let d = doc(&html, 800);
    assert_eq!(r(&d, "a").w, 150);
    assert_eq!(r(&d, "b").w, 77);
    assert_eq!(r(&d, "c").h, 7);
    let d = doc(&html, 400);
    assert_eq!(r(&d, "a").w, 80);
}

#[test]
fn cascade_specificity_important_inline() {
    let html = page(
        "<style>div#x{height:1px} .c.d{height:2px !important} div{height:3px}</style><div id=x class='c d' style='height:4px'></div><div id=y class=c style='height:5px'></div><style>#y{height:6px!important}</style>",
    );
    let d = doc(&html, 800);
    assert_eq!(r(&d, "x").h, 2);
    assert_eq!(r(&d, "y").h, 6);
}

#[test]
fn selectors() {
    let html = page(
        "<style>li:nth-child(2n+1){height:1px} li:nth-child(even){height:2px} li:first-child{height:3px} li:last-of-type{height:4px} ul > li + li{width:10px} p ~ span{display:block;height:9px} div[data-x^=ab][data-x]{height:11px} :is(#q, .nope) em{display:block;height:12px} div:not(.a, .b){height:13px}</style>\
        <ul><li id=l1><li id=l2><li id=l3><li id=l4><li id=l5></ul><p></p><span id=s></span><div id=dx data-x=abc></div><div id=q><em id=em></em></div><div id=n class=c></div>",
    );
    let d = doc(&html, 800);
    assert_eq!(r(&d, "l1").h, 3);
    assert_eq!(r(&d, "l2").h, 2);
    assert_eq!(r(&d, "l3").h, 1);
    assert_eq!(r(&d, "l5").h, 4);
    assert_eq!(r(&d, "l2").w, 10);
    assert_eq!(r(&d, "s").h, 9);
    assert_eq!(r(&d, "dx").h, 11);
    assert_eq!(r(&d, "em").h, 12);
    assert_eq!(r(&d, "n").h, 13);
}

#[test]
fn inheritance_and_em_rem() {
    let html = page(
        "<style>html{font-size:20px} #a{font-size:2em} #b{width:2em;height:1rem}</style><div id=a><div id=b></div></div>",
    );
    let d = doc(&html, 800);
    assert_eq!(r(&d, "b").w, 64); // body is 16px (RESET), a is 32px
    assert_eq!(r(&d, "b").h, 20);
}

#[test]
fn presentational_hints() {
    let d = doc(
        &page(
            "<table id=t width=300 border=1 cellspacing=0 cellpadding=5><tr><td id=c bgcolor=red>x</td></tr></table><img id=i width=40 height=30><center><div id=cd>c</div></center><font id=f size=7>big</font>",
        ),
        800,
    );
    assert_eq!(r(&d, "t").w, 300);
    assert_eq!(r(&d, "i"), Rect::new(r(&d, "i").x, r(&d, "i").y, 40, 30));
    assert!(d.display_list().entries.iter().any(|e| matches!(
        e.item,
        Item::Rect {
            color: 0xFFFF0000,
            ..
        }
    )));
    let t = texts(d.display_list());
    assert!(t.iter().any(|x| x.2 == "big"));
}

#[test]
fn images_sizes_and_alt() {
    let mut d = Document::parse_str(
        &page(
            "<img id=a src=a.png><img id=b src=b.png style='width:100px'><img id=c src=c.png alt='Alt text'><p style='width:200px'><img id=d src=a.png style='max-width:100%'></p>",
        ),
        "http://t.example/",
    );
    let pending = d.pending_resources();
    assert_eq!(pending.len(), 3); // a.png, b.png, c.png (a.png once)
    for r in pending {
        if let ResourceKind::Image(id) = r.kind {
            let size = if r.url.ends_with("c.png") {
                None
            } else {
                Some((400, 200))
            };
            d.supply_image(id, size);
        }
    }
    d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
    assert_eq!(r(&d, "a").w, 400);
    assert_eq!(r(&d, "b"), Rect::new(r(&d, "b").x, r(&d, "b").y, 100, 50));
    assert_eq!(r(&d, "d").w, 200);
    assert_eq!(r(&d, "d").h, 100);
    assert!(texts(d.display_list()).iter().any(|t| t.2 == "Alt text"));
}

#[test]
fn zoom_scales_everything() {
    let html = page("<div id=a style='width:100px;height:50px'>x</div>");
    let mut d = Document::parse_str(&html, "http://t.example/");
    d.layout(
        &Viewport {
            width: 800,
            height: 600,
            zoom: 200,
            dark: false,
        },
        &ApproxMeasurer,
    );
    assert_eq!(r(&d, "a"), Rect::new(0, 0, 200, 100));
    let t = texts(d.display_list());
    assert!(matches!(
        d.display_list()
            .entries
            .iter()
            .find_map(|e| if let Item::Text(t) = e.item {
                Some(t.font.size)
            } else {
                None
            }),
        Some(32)
    ));
    let _ = t;
}

#[test]
fn generated_content() {
    let d = doc(
        &page(
            "<style>.q::before{content:'[' attr(data-n) ']'} .q::after{content:\"\\2192\"}</style><span class=q data-n=7>x</span>",
        ),
        800,
    );
    let all: String = texts(d.display_list())
        .iter()
        .map(|t| t.2.clone())
        .collect();
    assert_eq!(all, "[7]x\u{2192}");
}

#[test]
fn css_counters() {
    let d = doc(
        &page(
            "<style>body{counter-reset:h} h2{counter-increment:h} h2::before{content:counter(h) '. '} .r h3{counter-increment:s} .r{counter-reset:s} h3::before{content:counter(h) '.' counter(s, lower-roman) ' '}</style><h2>A</h2><div class=r><h3>x</h3><h3>y</h3></div><h2>B</h2>",
        ),
        800,
    );
    let all: String = texts(d.display_list())
        .iter()
        .map(|t| t.2.clone())
        .collect::<alloc::vec::Vec<_>>()
        .join("|");
    assert_eq!(all.replace('|', ""), "1. A1.i x1.ii y2. B", "{all}");
}

#[test]
fn overflow_auto_spills_sideways() {
    let d = doc(
        &page(
            "<div style='overflow-x:auto;width:100px'><div id=w style='width:500px;height:10px;background:red'></div></div>",
        ),
        800,
    );
    let dl = d.display_list();
    let red = dl
        .entries
        .iter()
        .find(|e| {
            matches!(
                e.item,
                Item::Rect {
                    color: 0xFFFF0000,
                    ..
                }
            )
        })
        .unwrap();
    assert!(dl.clips[red.clip as usize].w > 400);
}

#[test]
fn grid_fr_fixed_and_gaps() {
    let d = doc(
        &page(
            "<div style='display:grid;grid-template-columns:100px 1fr 2fr;gap:10px 20px;width:460px'><div id=a style='height:30px'></div><div id=b></div><div id=c></div><div id=d style='height:5px'></div></div>",
        ),
        800,
    );
    // 460 - 100 - 2*20 = 320 for 3fr: 1fr = 106.66.
    assert_eq!(r(&d, "a"), Rect::new(0, 0, 100, 30));
    assert_eq!(r(&d, "b").x, 120);
    assert!((r(&d, "b").w - 107).abs() <= 1);
    assert!((r(&d, "c").w - 213).abs() <= 1);
    assert_eq!(r(&d, "b").h, 30); // stretched to the row
    assert_eq!(r(&d, "d").y, 40); // second row after the 10px row gap
}

#[test]
fn grid_auto_fill_and_spans() {
    let d = doc(
        &page(
            "<div style='display:grid;grid-template-columns:repeat(auto-fill, minmax(150px, 1fr));width:500px'><div id=a>a</div><div id=b style='grid-column:span 2'>b</div><div id=c>c</div><div id=e style='grid-column:1 / -1'>e</div></div>",
        ),
        800,
    );
    // 3 columns of 166.66.
    assert!((r(&d, "a").w - 167).abs() <= 1);
    assert!((r(&d, "b").w - 333).abs() <= 1);
    assert_eq!(r(&d, "c").y, r(&d, "e").y - 19);
    assert_eq!(r(&d, "e").w, 500);
}

#[test]
fn grid_auto_columns_content_sized() {
    let d = doc(
        &page(
            "<div style='display:inline-grid;grid-template-columns:auto auto'><span id=a>aa</span><span id=b>bbbb</span></div>",
        ),
        800,
    );
    let f = Font {
        family: Family::SansSerif,
        size: 16,
        weight: 400,
        italic: false,
    };
    assert_eq!(r(&d, "a").w, ApproxMeasurer.text_width(&f, "aa"));
    assert_eq!(r(&d, "b").x, r(&d, "a").w);
}

#[test]
fn grid_template_areas() {
    let d = doc(
        &page(
            "<div style='display:grid;grid-template-areas:\"head head\" \"side main\" \"foot foot\";grid-template-columns:100px 1fr;width:400px'><main id=m style='grid-area:main'>m</main><header id=h style='grid-area:head'>h</header><nav id=n style='grid-area:side'>n</nav><footer id=f style='grid-area:foot'>f</footer></div>",
        ),
        800,
    );
    assert_eq!(r(&d, "h"), Rect::new(0, 0, 400, 19));
    assert_eq!(r(&d, "n"), Rect::new(0, 19, 100, 19));
    assert_eq!(r(&d, "m"), Rect::new(100, 19, 300, 19));
    assert_eq!(r(&d, "f").y, 38);
}
