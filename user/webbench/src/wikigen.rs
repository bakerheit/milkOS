//! A deterministic, Wikipedia-like test page of about the size asked for: a big stylesheet
//! (custom properties, media queries, descendant/child/attribute selectors), a header with a
//! search form, a sidebar of links, an article of sections with paragraphs full of links and
//! inline markup, infobox and data tables, lists, figures, a long reference list and a footer.
//! Shared by the host benchmark (webengine's tests) and the guest one (`webbench`).

use alloc::string::String;
use core::fmt::Write;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const WORDS: &[&str] = &[
    "milk",
    "dairy",
    "the",
    "of",
    "and",
    "in",
    "is",
    "a",
    "to",
    "was",
    "for",
    "as",
    "with",
    "by",
    "on",
    "that",
    "from",
    "lactose",
    "protein",
    "calcium",
    "pasteurization",
    "butter",
    "cheese",
    "yogurt",
    "cattle",
    "goats",
    "sheep",
    "production",
    "consumption",
    "history",
    "century",
    "region",
    "industry",
    "nutrition",
    "fat",
    "content",
    "processing",
    "homogenized",
    "fermented",
    "culture",
    "species",
    "mammals",
    "market",
    "global",
    "average",
    "annual",
    "estimated",
    "traditional",
    "modern",
    "agricultural",
    "European",
    "Asian",
    "farm",
    "quality",
    "temperature",
    "bacteria",
    "enzyme",
    "whey",
    "casein",
    "cream",
    "skimmed",
    "powdered",
    "condensed",
    "evaporated",
    "organic",
    "breed",
    "Holstein",
    "Jersey",
    "lactation",
    "yield",
    "litres",
    "tonnes",
    "percent",
    "studies",
    "research",
    "health",
    "diet",
    "children",
    "adults",
    "population",
];

fn words(r: &mut Rng, out: &mut String, n: usize) {
    for i in 0..n {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(WORDS[r.below(WORDS.len())]);
    }
}

fn sentence(r: &mut Rng, out: &mut String, links: &mut usize) {
    let n = 8 + r.below(14);
    let mut first = true;
    let mut i = 0;
    while i < n {
        if !first {
            out.push(' ');
        }
        let w = WORDS[r.below(WORDS.len())];
        match r.below(14) {
            0 | 1 => {
                *links += 1;
                let _ = write!(
                    out,
                    "<a href=\"/wiki/{}_{}\" title=\"{}\">{}</a>",
                    w,
                    *links % 997,
                    w,
                    w
                );
            }
            2 => {
                let _ = write!(out, "<b>{w}</b>");
            }
            3 => {
                let _ = write!(out, "<i>{w}</i>");
            }
            4 if i + 2 < n => {
                out.push_str("<span class=\"nowrap\">");
                words(r, out, 2);
                out.push_str("</span>");
                i += 1;
            }
            _ => {
                if first {
                    let mut c = w.chars();
                    if let Some(f) = c.next() {
                        out.extend(f.to_uppercase());
                        out.push_str(c.as_str());
                    }
                } else {
                    out.push_str(w);
                }
            }
        }
        first = false;
        i += 1;
    }
    out.push('.');
    if r.below(5) == 0 {
        let k = 1 + r.below(400);
        let _ = write!(
            out,
            "<sup class=\"reference\" id=\"cite_ref-{k}\"><a href=\"#cite_note-{k}\">[{k}]</a></sup>"
        );
    }
}

fn stylesheet(out: &mut String, r: &mut Rng) {
    out.push_str(
        ":root{--color-base:#202122;--color-link:#36c;--color-visited:#6b4ba1;--border:#a2a9b1;--bg-subtle:#f8f9fa;--font:sans-serif;--gap:16px}\n\
         html{font-size:100%}body{margin:0;font-family:var(--font);color:var(--color-base);background:#fff;line-height:1.6}\n\
         a{color:var(--color-link);text-decoration:none}a:visited{color:var(--color-visited)}a:hover{text-decoration:underline}\n\
         .mw-header{display:flex;align-items:center;gap:var(--gap);padding:8px 16px;border-bottom:1px solid var(--border);background:var(--bg-subtle)}\n\
         .mw-logo{font:bold 20px Georgia,serif}.mw-search{flex:1;max-width:480px}.mw-search input{width:100%;box-sizing:border-box}\n\
         .mw-page-container{display:flex;max-width:1200px;margin:0 auto}\n\
         .vector-main-menu{width:176px;flex-shrink:0;padding:12px;font-size:0.875rem}\n\
         .vector-main-menu ul{list-style:none;margin:0 0 16px;padding:0}.vector-main-menu li{margin:2px 0}\n\
         .mw-body{flex:1;min-width:0;padding:16px 24px;border-left:1px solid var(--border)}\n\
         h1.firstHeading{font-family:'Linux Libertine','Georgia','Times',serif;font-weight:normal;font-size:1.8em;border-bottom:1px solid var(--border);margin:0 0 4px}\n\
         .mw-parser-output h2{font-family:'Linux Libertine','Georgia','Times',serif;font-weight:normal;font-size:1.5em;border-bottom:1px solid var(--border);margin:1em 0 0.25em}\n\
         .mw-parser-output h3{font-size:1.2em;font-weight:bold;margin:0.3em 0 0}\n\
         .mw-parser-output p{margin:0.5em 0}\n\
         .infobox{float:right;clear:right;width:22em;margin:0 0 1em 1em;border:1px solid var(--border);background:var(--bg-subtle);border-spacing:3px;font-size:88%;line-height:1.5em}\n\
         .infobox th{text-align:left;vertical-align:top;padding-right:0.6em}.infobox caption{font-size:125%;font-weight:bold;padding:0.2em}\n\
         table.wikitable{background:var(--bg-subtle);border:1px solid var(--border);border-collapse:collapse;margin:1em 0}\n\
         table.wikitable>tr>th,table.wikitable>*>tr>th{background:#eaecf0;text-align:center;border:1px solid var(--border);padding:0.2em 0.4em}\n\
         table.wikitable>tr>td,table.wikitable>*>tr>td{border:1px solid var(--border);padding:0.2em 0.4em}\n\
         .wikitable tbody tr:nth-child(2n) td{background:#f2f2f2}\n\
         .thumb{float:right;clear:right;margin:0.5em 0 1.3em 1.4em;border:1px solid #c8ccd1;padding:3px;background:var(--bg-subtle);font-size:94%}\n\
         .thumbcaption{padding:3px;line-height:1.4em}.thumb img{display:block}\n\
         .toc{display:table;border:1px solid var(--border);background:var(--bg-subtle);padding:7px;font-size:95%}\n\
         .toc ul{list-style:none;margin:0.3em 0;padding-left:1.6em}\n\
         .reflist{font-size:90%;column-width:30em}.reflist ol{margin-top:0}\n\
         .nowrap{white-space:nowrap}.reference{font-size:80%;line-height:1;white-space:nowrap}\n\
         .navbox{border:1px solid var(--border);width:100%;margin:1em auto 0;clear:both;font-size:88%;text-align:center;padding:1px}\n\
         .navbox-title{background:#ccf;font-weight:bold;padding:0.25em 1em}.navbox-list{text-align:left;padding:0 0.5em}\n\
         .navbox-list a{white-space:nowrap}.hlist ul{margin:0;padding:0}.hlist li{display:inline;margin:0}\n\
         .hlist li:after{content:\" \\00b7 \";font-weight:bold}.hlist li:last-child:after{content:none}\n\
         .mw-footer{border-top:1px solid var(--border);padding:12px 24px;font-size:0.75em;color:#54595d;clear:both}\n\
         .mw-footer ul{list-style:none;padding:0;margin:0}.mw-footer li{display:inline-block;margin-right:1em}\n\
         @media screen and (max-width:720px){.vector-main-menu{display:none}.mw-body{border-left:none;padding:8px}.infobox,.thumb{float:none;margin:1em auto;width:auto}}\n\
         @media print{.mw-header,.vector-main-menu,.mw-footer{display:none}}\n\
         @supports (display:flex){.mw-page-container{display:flex}}\n",
    );
    // Lots of rules that don't match much, as real skins have.
    for i in 0..600 {
        let w = WORDS[r.below(WORDS.len())];
        let _ = match i % 6 {
            0 => writeln!(
                out,
                ".mw-{w}-{i}{{margin:0 {}px;color:#{:06x}}}",
                i % 9,
                r.next() & 0xFFFFFF
            ),
            1 => writeln!(
                out,
                "#p-{w}-{i} .vector-menu-content li a{{padding:{}px 0;font-size:{}%}}",
                i % 5,
                80 + i % 30
            ),
            2 => writeln!(
                out,
                "div.{w}{i}>ul>li:first-child{{list-style-type:disc;padding-left:{}em}}",
                i % 3
            ),
            3 => writeln!(out, "[data-{w}=\"{i}\"]{{display:none}}"),
            4 => writeln!(
                out,
                "@media (min-width:{}px){{.sk-{w}-{i}{{width:{}%}}}}",
                400 + i,
                10 + i % 80
            ),
            _ => writeln!(
                out,
                ".skin-{w} .mw-{i}:not(.hidden){{border-bottom:1px solid var(--border)}}"
            ),
        };
    }
}

/// About `target` bytes of HTML (at least one full article skeleton).
pub fn wiki_page(target: usize, seed: u64) -> String {
    let mut r = Rng(seed | 1);
    let mut out = String::with_capacity(target + 64 * 1024);
    let mut links = 0usize;
    out.push_str("<!DOCTYPE html>\n<html class=\"client-nojs\" lang=\"en\" dir=\"ltr\">\n<head>\n<meta charset=\"UTF-8\">\n<title>Milk - Wikimilk</title>\n<style>\n");
    stylesheet(&mut out, &mut r);
    out.push_str("</style>\n</head>\n<body class=\"skin-vector mediawiki ltr\">\n");
    out.push_str("<header class=\"mw-header\"><a class=\"mw-logo\" href=\"/\">Wikimilk</a><form class=\"mw-search\" action=\"/w/index.php\"><input type=\"search\" name=\"search\" placeholder=\"Search Wikimilk\"></form><a href=\"/login\">Log in</a></header>\n");
    out.push_str(
        "<div class=\"mw-page-container\">\n<nav class=\"vector-main-menu\" id=\"p-navigation\">\n",
    );
    for g in 0..6 {
        let _ = write!(out, "<h3>Menu {g}</h3><ul>");
        for k in 0..12 {
            let w = WORDS[r.below(WORDS.len())];
            let _ = write!(
                out,
                "<li id=\"n-{g}-{k}\"><a href=\"/wiki/Special:{w}{k}\">{w} {k}</a></li>"
            );
        }
        out.push_str("</ul>\n");
    }
    out.push_str("</nav>\n<main id=\"content\" class=\"mw-body\" role=\"main\">\n<h1 id=\"firstHeading\" class=\"firstHeading\">Milk</h1>\n<div id=\"siteSub\">From Wikimilk, the free encyclopedia</div>\n<div class=\"mw-parser-output\">\n");
    // Infobox.
    out.push_str("<table class=\"infobox\"><caption>Milk</caption><tbody><tr><td colspan=\"2\" style=\"text-align:center\"><img src=\"/upload/Milk_glass.jpg\" width=\"220\" height=\"165\" alt=\"Milk\"></td></tr>");
    for k in 0..14 {
        let _ = write!(
            out,
            "<tr><th scope=\"row\">{}</th><td>",
            WORDS[k % WORDS.len()]
        );
        words(&mut r, &mut out, 3);
        out.push_str("</td></tr>");
    }
    out.push_str("</tbody></table>\n");
    let _ = write!(out, "<p>");
    for _ in 0..6 {
        sentence(&mut r, &mut out, &mut links);
        out.push(' ');
    }
    out.push_str("</p>\n<div id=\"toc\" class=\"toc\" role=\"navigation\"><div class=\"toctitle\"><h2 id=\"mw-toc-heading\">Contents</h2></div><ul>");
    for s in 0..20 {
        let _ = write!(
            out,
            "<li class=\"toclevel-1\"><a href=\"#Section_{s}\"><span class=\"tocnumber\">{}</span> <span class=\"toctext\">Section {s}</span></a></li>",
            s + 1
        );
    }
    out.push_str("</ul></div>\n");
    // Sections until the target size, leaving room for references and footer.
    let mut s = 0;
    while out.len() + 90_000 < target {
        let _ = write!(
            out,
            "<h2><span class=\"mw-headline\" id=\"Section_{s}\">Section {s}: "
        );
        words(&mut r, &mut out, 2);
        out.push_str("</span></h2>\n");
        if s % 3 == 1 {
            let _ = write!(
                out,
                "<div class=\"thumb tright\"><div class=\"thumbinner\" style=\"width:222px\"><img src=\"/upload/Figure_{s}.png\" width=\"220\" height=\"{}\" alt=\"\"><div class=\"thumbcaption\">",
                120 + s % 80
            );
            words(&mut r, &mut out, 9);
            out.push_str("</div></div></div>\n");
        }
        for _ in 0..(3 + r.below(4)) {
            out.push_str("<p>");
            for _ in 0..(3 + r.below(5)) {
                sentence(&mut r, &mut out, &mut links);
                out.push(' ');
            }
            out.push_str("</p>\n");
        }
        if s % 4 == 2 {
            out.push_str("<ul>");
            for _ in 0..(4 + r.below(6)) {
                out.push_str("<li>");
                sentence(&mut r, &mut out, &mut links);
                out.push_str("</li>");
            }
            out.push_str("</ul>\n");
        }
        if s % 5 == 3 {
            out.push_str("<h3>Data</h3><table class=\"wikitable sortable\"><thead><tr><th>Country</th><th>Cattle</th><th>Production</th><th>Change</th></tr></thead><tbody>");
            for k in 0..(10 + r.below(20)) {
                let _ = write!(
                    out,
                    "<tr><td><a href=\"/wiki/Country_{k}\">Country {k}</a></td><td style=\"text-align:right\">{}.{}</td><td style=\"text-align:right\">{}</td><td>{}%</td></tr>",
                    r.below(900),
                    r.below(10),
                    r.below(100_000),
                    r.below(40)
                );
            }
            out.push_str("</tbody></table>\n");
        }
        s += 1;
    }
    // References.
    out.push_str("<h2><span class=\"mw-headline\" id=\"References\">References</span></h2><div class=\"reflist\"><ol class=\"references\">");
    let mut k = 1;
    while out.len() + 12_000 < target || k < 40 {
        let _ = write!(
            out,
            "<li id=\"cite_note-{k}\"><b><a href=\"#cite_ref-{k}\">^</a></b> <cite class=\"citation\">"
        );
        words(&mut r, &mut out, 6);
        let _ = write!(
            out,
            " ({}). <a class=\"external\" href=\"https://example.org/ref/{k}\">",
            1900 + r.below(125)
        );
        words(&mut r, &mut out, 4);
        out.push_str("</a>.</cite></li>\n");
        k += 1;
        if k > 100_000 {
            break;
        }
    }
    out.push_str("</ol></div>\n");
    out.push_str("<div class=\"navbox\"><div class=\"navbox-title\">Dairy products</div><div class=\"navbox-list hlist\"><ul>");
    for k in 0..40 {
        let _ = write!(
            out,
            "<li><a href=\"/wiki/Product_{k}\">{}</a></li>",
            WORDS[k % WORDS.len()]
        );
    }
    out.push_str("</ul></div></div>\n</div>\n</main>\n</div>\n<footer class=\"mw-footer\"><ul><li>This page was last edited on 23 October 2026.</li><li><a href=\"/privacy\">Privacy policy</a></li><li><a href=\"/about\">About</a></li></ul></footer>\n</body>\n</html>\n");
    out
}
