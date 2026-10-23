//! webbench: the web engine on a ~1 MB Wikipedia-like page, in the guest.
//!   webbench [KB]   generate a page of about KB kilobytes (default 1024), then parse it,
//!                   style + lay out + paint it at 1024 px, relayout at 800 px and 2x zoom,
//!                   find and hit-test; then the fixture pages. Prints times (ms) and the
//!                   heap in use, and "webbench: PASS" / "webbench: FAIL ...".

#![no_std]
#![no_main]

extern crate alloc;

mod wikigen;

use rt::{println, sys};
use webengine::{ApproxMeasurer, Document, Viewport};

rt::main!(main);

fn now() -> u64 {
    sys::clock_ns(sys::CLOCK_MONOTONIC)
}

fn ms(a: u64, b: u64) -> u64 {
    (b.saturating_sub(a)) / 1_000_000
}

fn heap_used_kb() -> usize {
    let h = rt::heap::stats();
    (h.end - h.start - h.free) / 1024
}

fn arena_kb() -> usize {
    let h = rt::heap::stats();
    (h.end - h.start) / 1024
}

const FIXTURES: &[(&str, &str)] = &[
    (
        "article",
        include_str!("../../webengine/tests/fixtures/article.html"),
    ),
    (
        "layout",
        include_str!("../../webengine/tests/fixtures/layout.html"),
    ),
    (
        "tables",
        include_str!("../../webengine/tests/fixtures/tables.html"),
    ),
    (
        "forms",
        include_str!("../../webengine/tests/fixtures/forms.html"),
    ),
    (
        "text",
        include_str!("../../webengine/tests/fixtures/text.html"),
    ),
];

fn main(args: &rt::Args) -> i32 {
    let kb: usize = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1024)
        .clamp(1, 16 * 1024);
    let mut fails = 0;
    let base_kb = heap_used_kb();
    let html = wikigen::wiki_page(kb * 1024, 42);
    println!("webbench: page {} bytes ({} KB asked)", html.len(), kb);
    let t0 = now();
    let mut d = Document::parse(
        html.as_bytes(),
        "https://wiki.example/wiki/Milk",
        Some("text/html; charset=utf-8"),
    );
    let t1 = now();
    let parsed_kb = heap_used_kb();
    let (items, height) = {
        let dl = d.layout(&Viewport::new(1024, 768), &ApproxMeasurer);
        (dl.entries.len(), dl.height)
    };
    let t2 = now();
    let peak_kb = arena_kb();
    d.layout(&Viewport::new(800, 768), &ApproxMeasurer);
    let t3 = now();
    d.layout(
        &Viewport {
            width: 1024,
            height: 768,
            zoom: 200,
            dark: false,
        },
        &ApproxMeasurer,
    );
    let t4 = now();
    d.layout(
        &Viewport {
            width: 1024,
            height: 768,
            zoom: 200,
            dark: false,
        },
        &ApproxMeasurer,
    );
    let t5 = now();
    let found = d.find("milk", false).len();
    let t6 = now();
    let _ = d.hit_test(300, 300, (0, 5000));
    let st = d.stats();
    println!("webbench: parse {} ms", ms(t0, t1));
    println!("webbench: style+layout+paint at 1024px {} ms", ms(t1, t2));
    println!("webbench: relayout at 800px {} ms", ms(t2, t3));
    println!("webbench: relayout at 200% zoom {} ms", ms(t3, t4));
    println!("webbench: cached layout {} ms", ms(t4, t5));
    println!(
        "webbench: find 'milk' {} matches in {} ms",
        found,
        ms(t5, t6)
    );
    println!(
        "webbench: {} nodes, {} rules, {} boxes, {} display items, page height {} px",
        st.nodes, st.rules, st.boxes, items, height
    );
    println!(
        "webbench: engine estimate: {} KB kept, {} KB of boxes during layout",
        st.approx_bytes / 1024,
        st.layout_bytes / 1024
    );
    println!(
        "webbench: heap: {} KB for the DOM and styles' inputs after parse, {} KB in use now, arena peak {} KB (source {} KB)",
        parsed_kb.saturating_sub(base_kb),
        heap_used_kb().saturating_sub(base_kb),
        peak_kb,
        html.len() / 1024
    );
    if st.nodes < 1000 || height < 1000 || found == 0 {
        println!("webbench: FAIL the big page looks wrong");
        fails += 1;
    }
    drop(d);
    drop(html);
    for (name, src) in FIXTURES {
        let t = now();
        let mut d = Document::parse_str(src, "http://fixture.example/");
        let n = d
            .layout(&Viewport::new(1024, 768), &ApproxMeasurer)
            .entries
            .len();
        let n2 = d
            .layout(&Viewport::new(360, 640), &ApproxMeasurer)
            .entries
            .len();
        println!(
            "webbench: fixture {name}: {n} items at 1024px, {n2} at 360px, {} ms",
            ms(t, now())
        );
        if n == 0 || n2 == 0 {
            println!("webbench: FAIL fixture {name} drew nothing");
            fails += 1;
        }
    }
    if fails == 0 {
        println!("webbench: PASS");
        0
    } else {
        println!("webbench: FAIL ({fails})");
        1
    }
}
