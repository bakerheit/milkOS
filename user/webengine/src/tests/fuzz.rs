//! Hostile input: mutated fixture pages, random tag and CSS soup, and pathological shapes
//! (deep nesting, huge spans, giant numbers). Nothing may panic (debug builds also trap
//! integer overflow here), and every page must finish quickly.

use alloc::string::String;
use alloc::vec::Vec;

use super::fixtures::*;
use crate::api::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// How many times the default iteration counts to run (WEBENGINE_FUZZ=N for longer runs).
fn scale() -> usize {
    std::env::var("WEBENGINE_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1)
}

fn seed() -> u64 {
    std::env::var("WEBENGINE_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn run(html: &[u8], widths: &[i32]) {
    let mut d = Document::parse(html, "http://fuzz.example/a/b", None);
    for (i, &w) in widths.iter().enumerate() {
        let vp = Viewport {
            width: w,
            height: 300,
            zoom: [100, 37, 250, 500][i % 4],
            dark: i % 2 == 1,
        };
        d.layout(&vp, &ApproxMeasurer);
        let _ = d.hit_test(w / 2, 50, (0, 0));
        let _ = d.find("a", false);
    }
    let _ = d.title();
    let _ = d.text();
    let _ = d.main_content();
    let _ = d.fragment_y("x");
    for c in 0..d.controls().len() {
        let id = ControlId(c as u32);
        d.set_value(id, "v");
        d.set_checked(id, true);
        d.select_option(id, 0, true);
        let _ = d.submit(id, Some((1, 1)));
    }
}

const SOUP: &[&str] = &[
    "<div>",
    "</div>",
    "<p>",
    "</p>",
    "<table>",
    "<tr>",
    "<td>",
    "</td>",
    "</table>",
    "<b>",
    "</b>",
    "<i>",
    "<a href=x>",
    "</a>",
    "<ul>",
    "<li>",
    "<ol>",
    "<select>",
    "<option>",
    "<textarea>",
    "<input type=text>",
    "<input type=checkbox>",
    "<button>",
    "<span style='display:inline-block'>",
    "<span>",
    "</span>",
    "<img src=a>",
    "<br>",
    "<pre>",
    "<svg>",
    "<math>",
    "<table><caption>",
    "<form>",
    "</form>",
    "<template>",
    "</template>",
    "<frameset>",
    "<style>p{}</style>",
    "<!--",
    "-->",
    "<![CDATA[",
    "&amp;",
    "&#x110000;",
    "&notit;",
    "text ",
    "word",
    "  ",
    "\n",
    "\t",
    "<h1>",
    "<hr>",
    "<center>",
    "<nobr>",
    "<marquee>",
    "<dl><dt><dd>",
    "<div style='float:left;width:30%'>",
    "<div style='position:absolute;top:-1e9px'>",
    "<div style='display:flex'>",
    "<div style='display:table-cell'>",
    "<div style='width:99999999999px'>",
    "<div style='margin:-500px'>",
    "<div style='font-size:0'>",
    "<div style='font-size:99999px'>",
    "<div style='line-height:-3'>",
    "<div style='letter-spacing:9999px'>",
    "<td colspan=999999 rowspan=0>",
    "<li value=-2147483648>",
    "<ol start=2147483647>",
    "<div style='column-count:3'>",
    "<span style='vertical-align:-1e10px'>",
    "<div style='padding:100%'>",
    "<p style='text-indent:-1e30px'>",
    "<div style='height:calc(100% / 0)'>",
    "<div style='flex:1e30 1e30 0'>",
    "<x-custom>",
    "<body bgcolor=red>",
    "<style>*{display:flex}</style>",
    "<style>*{float:left}</style>",
    "<style>*{position:relative;top:1px}</style>",
    "<style>@media (min-width:1px){*{display:table}}</style>",
    "<style>:root{--a:var(--b);--b:var(--a)}div{width:var(--a)}</style>",
    "<div style='--x:var(--x,var(--x));width:var(--x)'>",
    "\u{0}",
    "\u{FFFD}",
    "é",
    "日本語",
    "<q>",
    "<sub>",
    "<sup>",
    "<wbr>",
    "<fieldset><legend>",
    "<details><summary>",
    "<video>",
    "<iframe>",
    "<object>",
    "<base href='//x'>",
];

fn soup(r: &mut Rng, n: usize) -> String {
    let mut s = String::new();
    for _ in 0..n {
        s.push_str(SOUP[r.below(SOUP.len())]);
    }
    s
}

#[test]
fn tag_soup() {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15 ^ seed());
    for _ in 0..300 * scale() {
        let n = 1 + r.below(120);
        let s = soup(&mut r, n);
        run(s.as_bytes(), &[400, 37]);
    }
}

#[test]
fn mutated_fixtures() {
    let mut r = Rng(12345 ^ seed());
    for base in [ARTICLE, LAYOUT, TABLES, FORMS, TEXT] {
        for _ in 0..40 * scale() {
            let mut b: Vec<u8> = base.as_bytes().to_vec();
            for _ in 0..(1 + r.below(30)) {
                let i = r.below(b.len());
                match r.below(4) {
                    0 => b[i] = (r.next() & 0xFF) as u8,
                    1 => {
                        b.remove(i);
                    }
                    2 => b.insert(i, b"<>/\"'=&;{}:()"[r.below(13)]),
                    _ => {
                        let j = r.below(b.len());
                        let (a, z) = (i.min(j), i.max(j).min(a_max(i, j, b.len())));
                        let chunk: Vec<u8> = b[a..z].to_vec();
                        let k = r.below(b.len());
                        for (o, c) in chunk.into_iter().enumerate().take(400) {
                            b.insert((k + o).min(b.len()), c);
                        }
                    }
                }
            }
            run(&b, &[1024, 320]);
        }
    }
}

fn a_max(i: usize, j: usize, len: usize) -> usize {
    (i.min(j) + 400).min(len)
}

#[test]
fn css_soup() {
    let atoms = [
        "{",
        "}",
        ";",
        ":",
        "(",
        ")",
        "[",
        "]",
        ",",
        ">",
        "+",
        "~",
        "*",
        ".a",
        "#b",
        "div",
        "@media",
        "@supports",
        "@import",
        "@font-face",
        "@keyframes",
        "!important",
        "calc(",
        "var(",
        "--x",
        "1e999px",
        "-0",
        "50%",
        "url(",
        "\"",
        "'",
        "\\",
        "/*",
        "*/",
        "rgb(",
        "hsl(",
        "#fff",
        "nth-child(",
        "2n+1",
        ":not(",
        ":is(",
        "::before",
        "content",
        "attr(",
        "display",
        "flex",
        "grid",
        "table",
        "float",
        "left",
        "width",
        "min(",
        "max(",
        "clamp(",
        "em",
        "rem",
        "vw",
        "and",
        "or",
        "not",
        "(max-width:",
        "100px",
        "inherit",
        "unset",
        "initial",
        "\n",
        " ",
        "position",
        "absolute",
        "z-index",
        "-2147483649",
    ];
    let mut r = Rng(777 ^ seed());
    for _ in 0..300 * scale() {
        let mut css = String::new();
        for _ in 0..(1 + r.below(80)) {
            css.push_str(atoms[r.below(atoms.len())]);
            if r.below(3) == 0 {
                css.push(' ');
            }
        }
        let html = alloc::format!(
            "<style>{css}</style><div class=a id=b style=\"{}\"><p>text <span>more</span></p></div>",
            css.replace('"', "'")
        );
        run(html.as_bytes(), &[500]);
    }
}

#[test]
fn pathological_shapes() {
    // Deep nesting of every kind.
    for open in [
        "<div>",
        "<span>",
        "<b>",
        "<table><tr><td>",
        "<ul><li>",
        "<div style='display:flex'>",
        "<div style='float:left'>",
        "<q>",
        "<sup>",
        "<span style='display:inline-block'>",
        "<div style='position:relative'>",
    ] {
        let mut s = String::new();
        for _ in 0..3000 {
            s.push_str(open);
        }
        s.push('x');
        let t = std::time::Instant::now();
        run(s.as_bytes(), &[800]);
        std::println!("nesting {open}: {:?}", t.elapsed());
        assert!(t.elapsed().as_secs() < 20, "{open}: {:?}", t.elapsed());
    }
    // One enormous word, one enormous line, many tiny elements.
    let word: String = core::iter::repeat_n('m', 200_000).collect();
    run(
        alloc::format!("<p style='overflow-wrap:anywhere'>{word}</p><p>{word}</p>").as_bytes(),
        &[300],
    );
    let line: String = core::iter::repeat_n("ab ", 60_000).collect();
    run(
        alloc::format!("<pre style='white-space:pre-wrap'>{line}</pre>").as_bytes(),
        &[300],
    );
    let many: String = core::iter::repeat_n("<i>a</i>", 40_000).collect();
    run(many.as_bytes(), &[600]);
    // A huge table.
    let mut t = String::from("<table>");
    for i in 0..400 {
        t.push_str("<tr>");
        for j in 0..30 {
            t.push_str(&alloc::format!(
                "<td colspan={}>{i}.{j}</td>",
                1 + (i * j) % 3
            ));
        }
    }
    run(t.as_bytes(), &[1000]);
    // Selector explosion: many descendant combinators against a deep tree.
    let mut s = String::from("<style>");
    for _ in 0..50 {
        s.push_str("div div div div div div div div div div span{color:red}");
    }
    s.push_str("</style>");
    for _ in 0..200 {
        s.push_str("<div>");
    }
    s.push_str("<span>x</span>");
    let t = std::time::Instant::now();
    run(s.as_bytes(), &[800]);
    assert!(t.elapsed().as_secs() < 20, "selectors: {:?}", t.elapsed());
}

#[test]
fn limits_cut_inputs() {
    let big: String = core::iter::repeat_n("<p>x</p>", 10_000).collect();
    let limits = Limits {
        max_input: 4096,
        ..Limits::default()
    };
    let d = Document::parse_with(big.as_bytes(), "http://x/", None, limits);
    assert!(d.truncated());
    let limits = Limits {
        max_nodes: 500,
        ..Limits::default()
    };
    let d = Document::parse_with(big.as_bytes(), "http://x/", None, limits);
    assert!(d.truncated());
    assert!(d.stats().nodes <= 520);
}

#[test]
fn random_bytes() {
    let mut r = Rng(4242 ^ seed());
    for _ in 0..200 * scale() {
        let n = r.below(4000);
        let b: Vec<u8> = (0..n).map(|_| (r.next() & 0xFF) as u8).collect();
        run(&b, &[200]);
        // Also as other charsets.
        let _ = Document::parse(&b, "http://x/", Some("text/html; charset=utf-16le"));
        let _ = Document::parse(&b, "http://x/", Some("text/html; charset=windows-1252"));
        let _ = Document::parse(&b, "http://x/", Some("text/plain"));
    }
}
