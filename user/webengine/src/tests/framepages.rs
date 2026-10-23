//! Pages with frames, loaded the way a host does it (`frame_slots` → fetch → `attach_frame`)
//! from the fixtures in tests/fixtures/frames: golden display lists, screenshots
//! (WEBENGINE_SHOTS), and fuzzing of random nested structures.

use alloc::string::String;
use alloc::vec::Vec;

use super::raster::{Canvas, SysMeasurer, save};
use crate::api::*;

pub const PAGE: &str = include_str!("../../tests/fixtures/frames/page.html");
pub const WIDGET: &str = include_str!("../../tests/fixtures/frames/widget.html");
pub const LIST: &str = include_str!("../../tests/fixtures/frames/list.html");
pub const NESTED: &str = include_str!("../../tests/fixtures/frames/nested.html");
pub const LEVEL1: &str = include_str!("../../tests/fixtures/frames/level1.html");
pub const LEVEL2: &str = include_str!("../../tests/fixtures/frames/level2.html");
pub const LAYOUT: &str = include_str!("../../tests/fixtures/frames/layout.html");

const FILES: [(&str, &str); 6] = [
    ("widget.html", WIDGET),
    ("list.html", LIST),
    ("level1.html", LEVEL1),
    ("level2.html", LEVEL2),
    ("nested.html", NESTED),
    ("page.html", PAGE),
];

const BASE: &str = "http://fixture.example/dir/";

/// What a host does: look at the slots, load what they want from `files`, attach it; until
/// nothing more is wanted.
pub fn load(html: &str, files: &[(&str, &str)]) -> Document {
    let mut d = Document::parse_str(html, &alloc::format!("{BASE}top.html"));
    for _ in 0..10 {
        let mut any = false;
        for s in d.frame_slots() {
            if !s.wants_load() {
                continue;
            }
            let doc = if let Some(src) = &s.srcdoc {
                Document::parse_str(src, "about:srcdoc")
            } else if let Some(u) = &s.src {
                let name = u.rsplit('/').next().unwrap_or("");
                match files.iter().find(|f| f.0 == name) {
                    Some(f) => Document::parse_str(f.1, u),
                    None => Document::parse_str("<p>not found", u),
                }
            } else {
                Document::parse_str("", "about:blank")
            };
            if d.attach_frame(s.id, doc, Some(s.generation)) {
                any = true;
            }
        }
        if !any {
            break;
        }
    }
    d
}

/// The display list as lines: frame, clip, item.
pub fn dump_tree(d: &Document) -> String {
    let dl = d.display_list();
    let mut s = String::new();
    for (name, list) in [("", &dl.entries), ("fixed ", &dl.fixed)] {
        for e in list.iter() {
            let one = super::render::dump(&DisplayList {
                entries: alloc::vec![*e],
                text: dl.text.clone(),
                ..DisplayList::default()
            });
            let c = dl.clips[e.clip as usize];
            let clip = if c.w > 1 << 24 {
                String::from("-")
            } else {
                alloc::format!("{},{} {}x{}", c.x, c.y, c.w, c.h)
            };
            s.push_str(&alloc::format!("{name}f{} [{clip}] {one}", e.frame.0));
        }
    }
    s
}

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");

fn check(name: &str, got: &str) -> Option<String> {
    let path = alloc::format!("{DIR}/{name}.txt");
    if std::env::var("WEBENGINE_BLESS").is_ok() {
        std::fs::create_dir_all(DIR).unwrap();
        std::fs::write(&path, got).unwrap();
        return None;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    if got == want {
        return None;
    }
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

fn lay(d: &mut Document, w: i32, m: &dyn TextMeasurer) {
    d.layout(&Viewport::new(w, 600), m);
}

#[test]
fn golden_frame_display_lists() {
    let mut errs = Vec::new();
    for (name, html) in [
        ("frames-page", PAGE),
        ("frames-nested", NESTED),
        ("frames-layout", LAYOUT),
    ] {
        let mut d = load(html, &FILES);
        lay(&mut d, 800, &ApproxMeasurer);
        if let Some(e) = check(name, &dump_tree(&d)) {
            errs.push(e);
        }
    }
    // The page with its frames scrolled.
    let mut d = load(PAGE, &FILES);
    lay(&mut d, 800, &ApproxMeasurer);
    let list = d
        .frame_slots()
        .iter()
        .find(|s| s.name == "list")
        .unwrap()
        .id;
    d.set_frame_scroll(list, 0, 300);
    lay(&mut d, 800, &ApproxMeasurer);
    if let Some(e) = check("frames-page-scrolled", &dump_tree(&d)) {
        errs.push(e);
    }
    assert!(
        errs.is_empty(),
        "golden mismatch (WEBENGINE_BLESS=1 to update):\n{}",
        errs.join("\n")
    );
}

fn shot(name: &str, mut d: Document, w: i32, h: i32) -> Document {
    d.layout(&Viewport::new(w, h), &SysMeasurer);
    let full = d.display_list().height.clamp(1, h);
    let mut c = Canvas::new(w, full, d.display_list().background);
    let dl = d.display_list().clone();
    c.paint(&dl, &d, 0);
    save(name, &c);
    d
}

#[test]
fn frame_screenshots() {
    let d = shot("frames-page", load(PAGE, &FILES), 760, 760);
    assert!(d.frame_ids().len() == 3);
    let mut d = load(PAGE, &FILES);
    d.layout(&Viewport::new(760, 760), &SysMeasurer);
    let list = d
        .frame_slots()
        .iter()
        .find(|s| s.name == "list")
        .unwrap()
        .id;
    d.set_frame_scroll(list, 0, 260);
    shot("frames-page-scrolled", d, 760, 760);
    let d = shot("frames-nested", load(NESTED, &FILES), 520, 480);
    assert_eq!(d.frame_ids().len(), 2);
    let mut d = load(NESTED, &FILES);
    d.layout(&Viewport::new(520, 480), &SysMeasurer);
    let s = d.frame_slots();
    d.set_frame_scroll(s[1].id, 0, 40);
    shot("frames-nested-scrolled", d, 520, 480);
    shot("frames-layout", load(LAYOUT, &FILES), 520, 600);
    // Zoomed and dark.
    let mut d = load(PAGE, &FILES);
    let vp = Viewport {
        width: 760,
        height: 700,
        zoom: 150,
        dark: true,
    };
    d.layout(&vp, &SysMeasurer);
    let mut c = Canvas::new(760, 700, d.display_list().background);
    let dl = d.display_list().clone();
    c.paint(&dl, &d, 0);
    save("frames-page-zoom", &c);
    // A page that leaves its colours to the browser, in a dark desktop, with a frame that does
    // the same: both dark, each with its own canvas.
    let mut d = load(
        "<h3>Dark desktop</h3><p>A plain page with a plain frame.</p><iframe width=300 height=110 srcdoc='<p>Plain frame text</p><input value=field>'></iframe><p>after</p>",
        &[],
    );
    let vp = Viewport {
        width: 420,
        height: 260,
        zoom: 100,
        dark: true,
    };
    d.layout(&vp, &SysMeasurer);
    let mut c = Canvas::new(420, 260, d.display_list().background);
    let dl = d.display_list().clone();
    c.paint(&dl, &d, 0);
    save("frames-plain-dark", &c);
    let canvas = dl
        .entries
        .iter()
        .find_map(|e| match e.item {
            Item::Rect { color, .. } if !e.frame.is_top() => Some(color),
            _ => None,
        })
        .unwrap();
    assert_eq!(canvas, crate::page::CANVAS_DARK);
    assert_eq!(dl.background, crate::page::CANVAS_DARK);
}

// ---- hostile shapes -------------------------------------------------------------------------

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

fn scale() -> usize {
    std::env::var("WEBENGINE_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1)
}

/// A random little page: some text, boxes of random sizes and styles, maybe iframes.
fn random_page(r: &mut Rng, frames: usize) -> String {
    let mut s = String::from("<body style='margin:");
    s.push_str(&alloc::format!("{}px'>", r.below(20)));
    for _ in 0..r.below(8) {
        match r.below(7) {
            0 => s.push_str("<p>some text that goes on for a while to wrap in narrow frames</p>"),
            1 => s.push_str(&alloc::format!(
                "<div style='height:{}px;width:{}px;background:#{:06x}'></div>",
                r.below(400),
                r.below(500),
                r.below(0xFFFFFF)
            )),
            2 => s.push_str("<input name=a><a href=/x>link</a><select><option>1<option>2</select>"),
            3 => s.push_str(&alloc::format!(
                "<div style='position:{};left:{}px;top:{}px;width:50px;height:50px;background:#abc;overflow:hidden'>x</div>",
                ["absolute", "fixed", "relative"][r.below(3)],
                r.below(100),
                r.below(100)
            )),
            4 => s.push_str("<h1>needle</h1><ul><li>a<li>b</ul>"),
            _ => s.push_str(&alloc::format!("<span id=s{}>x</span>", r.below(5))),
        }
    }
    for _ in 0..frames {
        let styles = [
            "",
            "width:100%;height:100%",
            "display:block;width:0;height:0",
            "position:absolute;left:5px;top:5px;width:80px;height:60px",
            "position:fixed;right:0;bottom:0;width:40px;height:40px",
            "float:right;width:60px;height:60px;border:5px solid red",
            "visibility:hidden",
            "display:none",
            "width:70000px;height:70000px",
            "width:1px;height:1px",
            "border-width:30px;width:10px;height:10px",
        ];
        s.push_str(&alloc::format!(
            "<iframe style='{}' width={} height={} {}></iframe>",
            styles[r.below(styles.len())],
            r.below(300),
            r.below(300),
            ["", "scrolling=no", "srcdoc='<p>hi'", "name=n"][r.below(4)]
        ));
    }
    s
}

/// Build a random tree of frames of `n` slots and run the whole API over it.
fn tree_case(seed: u64, ops: usize) {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let top_frames = 1 + r.below(4);
    let mut d = Document::parse_str(&random_page(&mut r, top_frames), "http://fuzz.example/a/b");
    let m = ApproxMeasurer;
    let mut attached = 0usize;
    for step in 0..ops {
        match r.below(10) {
            0..=2 => {
                // Fill a random wanting slot with a random page.
                let slots = d.frame_slots();
                let wanting: Vec<&FrameSlot> =
                    slots.iter().filter(|s| s.blocked.is_none()).collect();
                if !wanting.is_empty() {
                    let s = wanting[r.below(wanting.len())];
                    let n = r.below(3);
                    let doc = Document::parse_str(&random_page(&mut r, n), "http://fuzz.example/f");
                    if d.attach_frame(s.id, doc, None) {
                        attached += 1;
                    }
                }
            }
            3 => {
                let vp = Viewport {
                    width: 1 + r.below(900) as i32,
                    height: 1 + r.below(700) as i32,
                    zoom: [100, 25, 500, 37, 150][r.below(5)],
                    dark: r.below(2) == 0,
                };
                d.layout(&vp, &m);
            }
            4 => {
                let ids = d.frame_ids();
                if !ids.is_empty() {
                    let id = ids[r.below(ids.len())];
                    d.set_frame_scroll(id, r.below(1000) as i32 - 100, r.below(3000) as i32 - 100);
                }
            }
            5 => {
                let _ = d.hit_test(
                    r.below(900) as i32 - 50,
                    r.below(700) as i32 - 50,
                    (r.below(50) as i32, r.below(500) as i32),
                );
            }
            6 => {
                let _ = d.scroll_frames_at(
                    r.below(900) as i32,
                    r.below(700) as i32,
                    (0, r.below(100) as i32),
                    r.below(200) as i32 - 100,
                    r.below(400) as i32 - 200,
                );
            }
            7 => {
                let ms = d.find("needle", false);
                if let Some(mm) = ms.get(r.below(ms.len().max(1)))
                    && let Some(rc) = mm.frame_rects.first()
                {
                    d.reveal(mm.frame, *rc);
                }
                let _ = d.text();
                let _ = d.focus_order();
            }
            8 => {
                let ids = d.frame_ids();
                if !ids.is_empty() && r.below(3) == 0 {
                    let id = ids[r.below(ids.len())];
                    let _ = d.detach_frame(id);
                }
                let _ = d.take_closed_frames();
            }
            _ => {
                let _ = d.frame_at(r.below(900) as i32, r.below(700) as i32, (0, 0));
                let _ = d.frame_viewport(FrameId(r.below(10) as u32));
                d.set_focused_frame(FrameId(r.below(10) as u32));
            }
        }
        // Invariants: whatever happened, the list is well-formed.
        if step % 5 == 0 {
            d.layout(&Viewport::new(400, 300), &m);
            let slots = d.frame_slots();
            let dl = d.display_list();
            for e in dl.entries.iter().chain(dl.fixed.iter()) {
                assert!(
                    (e.clip as usize) < dl.clips.len(),
                    "seed {seed}: clip out of range"
                );
                if let Item::Text(t) = e.item {
                    assert!(
                        (t.start as usize + t.len as usize) <= dl.text.len(),
                        "seed {seed}: text out of range"
                    );
                }
            }
            assert!(slots.len() <= 32, "seed {seed}: {} slots", slots.len());
            assert!(slots.iter().all(|s| s.depth <= 6));
            for e in &dl.entries {
                assert!(
                    e.frame.is_top() || slots.iter().any(|s| s.id == e.frame),
                    "seed {seed}: entry of an unknown frame"
                );
            }
        }
    }
    let _ = attached;
}

#[test]
fn random_nested_structures_never_break() {
    let n = 60 * scale() as u64;
    for seed in 0..n {
        tree_case(seed, 60);
    }
}

#[test]
fn frame_bomb_shapes_stay_bounded() {
    let m = ApproxMeasurer;
    // Thousands of iframes in one page.
    let mut html = String::from("<body>");
    for _ in 0..5000 {
        html.push_str("<iframe src=x width=10 height=10></iframe>");
    }
    let mut d = Document::parse_str(&html, "http://bomb.example/");
    d.layout(&Viewport::new(800, 600), &m);
    let slots = d.frame_slots();
    assert_eq!(slots.len(), 32);
    for s in &slots {
        d.attach_frame(
            s.id,
            Document::parse_str("<p>x", "http://bomb.example/f"),
            None,
        );
    }
    d.layout(&Viewport::new(800, 600), &m);
    assert!(
        d.display_list().entries.len() < 5200,
        "{}",
        d.display_list().entries.len()
    );
    let console = d.take_console();
    assert!(
        console.iter().any(|l| l.text.contains("Too many frames")),
        "{console:?}"
    );
    // A chain of frames that each contain the next, to the cap and beyond.
    let mut d = Document::parse_str("<iframe></iframe>", "http://bomb.example/");
    for _ in 0..40 {
        let slots = d.frame_slots();
        let Some(s) = slots.iter().find(|s| s.wants_load()) else {
            break;
        };
        d.attach_frame(
            s.id,
            Document::parse_str(
                "<iframe style='width:90%;height:90%'></iframe>",
                "http://bomb.example/n",
            ),
            None,
        );
    }
    d.layout(&Viewport::new(800, 600), &m);
    let slots = d.frame_slots();
    assert_eq!(slots.iter().map(|s| s.depth).max(), Some(6));
    assert_eq!(slots.iter().filter(|s| s.blocked.is_some()).count(), 1);
    assert_eq!(slots.len(), 6);
    // Frames of 70000 px each, many of them, with big content: the list stays small.
    let mut html = String::from("<body style='margin:0'>");
    for _ in 0..8 {
        html.push_str("<iframe style='width:70000px;height:70000px;border:0'></iframe>");
    }
    let mut d = Document::parse_str(&html, "http://bomb.example/");
    for s in d.frame_slots() {
        let mut big = String::from("<body>");
        for i in 0..2000 {
            big.push_str(&alloc::format!("<p>line {i}</p>"));
        }
        d.attach_frame(
            s.id,
            Document::parse_str(&big, "http://bomb.example/big"),
            None,
        );
    }
    d.layout(&Viewport::new(800, 600), &m);
    assert!(
        d.display_list().entries.len() < 60_000,
        "{}",
        d.display_list().entries.len()
    );
}

// ---- scripts that make, move and remove frames ----------------------------------------------

const CHURN: [&str; 10] = [
    "var f = document.createElement('iframe'); f.setAttribute('src', '/a' + (Math.random() * 5 | 0)); document.body.appendChild(f);",
    "var l = document.querySelectorAll('iframe'); if (l.length) l[0].parentNode.removeChild(l[0]);",
    "var l = document.querySelectorAll('iframe'); if (l.length) l[l.length - 1].setAttribute('src', '/b' + (Math.random() * 1e6 | 0));",
    "document.body.insertAdjacentHTML('beforeend', '<iframe name=n srcdoc=\"<p>x\" width=50 height=20></iframe>');",
    "var l = document.querySelectorAll('iframe'); if (l.length > 1) document.body.appendChild(l[0]);",
    "document.body.innerHTML = '<p>cleared</p><iframe></iframe>';",
    "var l = document.querySelectorAll('iframe'); for (var i = 0; i < l.length; i++) l[i].setAttribute('style', 'width:' + (Math.random() * 300 | 0) + 'px;height:' + (Math.random() * 200 | 0) + 'px');",
    "var l = document.querySelectorAll('iframe'); if (l.length) l[0].setAttribute('name', 'q' + (Math.random() * 3 | 0));",
    "var l = document.querySelectorAll('iframe'); if (l.length) { var d = document.createElement('div'); d.appendChild(l[0]); }",
    "for (var i = 0; i < 40; i++) document.body.appendChild(document.createElement('iframe'));",
];

fn scripted(html: &str) -> Document {
    Document::parse_scripted(
        html.as_bytes(),
        "http://churn.example/",
        None,
        Limits::default(),
        Some(ScriptLimits::default()),
    )
}

#[test]
fn scripts_that_churn_frames_keep_the_tree_sound() {
    use super::script::{Host, with};
    let m = ApproxMeasurer;
    let mut biggest = 0;
    let mut attached_total = 0;
    for seed in 0..(25 * scale() as u64) {
        let mut r = Rng(seed.wrapping_mul(0xA24B_AED4_963E_E407) | 1);
        let mut d = scripted("<body><iframe></iframe><iframe srcdoc='<p>x'></iframe>");
        let mut h = Host::new();
        d.run(&mut h);
        let mut seen_ids: Vec<u32> = Vec::new();
        for _ in 0..50 {
            // A script in the top or in one of the frames.
            let ids = d.frame_ids();
            let who = if ids.is_empty() || r.below(2) == 0 {
                FrameId::TOP
            } else {
                ids[r.below(ids.len())]
            };
            let snippet = CHURN[r.below(CHURN.len())];
            d.with_frame(who, |doc| {
                let mut hh = Host::new();
                with(doc, &mut hh, |rl, _, _| {
                    let _ = rl.eval(snippet, "churn");
                });
                doc.run(&mut hh);
            });
            // The host's part.
            for s in d.frame_slots() {
                if s.wants_load() && r.below(3) != 0 {
                    let doc = if r.below(2) == 0 {
                        scripted(&random_page(&mut r, 1))
                    } else {
                        scripted("<body><iframe></iframe><p>child")
                    };
                    let _ = d.attach_frame(s.id, doc, Some(s.generation));
                }
            }
            d.layout(&Viewport::new(500, 400), &m);
            let slots = d.frame_slots();
            assert!(slots.len() <= 32, "seed {seed}: {}", slots.len());
            for s in &slots {
                assert!(s.depth >= 1 && s.depth <= 6);
                if s.parent != FrameId::TOP {
                    assert!(
                        slots.iter().any(|p| p.id == s.parent),
                        "seed {seed}: orphan slot"
                    );
                }
                seen_ids.push(s.id.0);
            }
            // Ids are never given twice to different elements: closed ones are gone for good.
            for c in d.take_closed_frames() {
                assert!(
                    slots.iter().all(|s| s.id != c),
                    "seed {seed}: closed frame {c:?} still has a slot"
                );
            }
            let dl = d.display_list();
            assert!(
                dl.entries
                    .iter()
                    .all(|e| (e.clip as usize) < dl.clips.len())
            );
            let _ = d.hit_test(r.below(500) as i32, r.below(400) as i32, (0, 0));
            let _ = d.find("child", false);
        }
        biggest = biggest.max(seen_ids.iter().copied().max().unwrap_or(0));
        attached_total += d.frame_ids().len();
        // Everything that was attached is reachable and consistent.
        for id in d.frame_ids() {
            let fi = d.frame_doc(id).unwrap().frame_info();
            assert_eq!(fi.id, id);
        }
    }
    // The scripts really did make frames, and the host really did fill them.
    assert!(biggest > 30, "highest frame id {biggest}");
    assert!(attached_total > 20, "{attached_total}");
}
