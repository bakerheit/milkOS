//! Frames: slots, attach/detach, and (below) layout, painting, hit testing, scrolling, focus
//! and find across them.

use alloc::string::String;
use alloc::vec::Vec;

use super::script::{Host, with};
use crate::api::*;

fn top(html: &str) -> Document {
    Document::parse_str(html, "http://top.example/dir/page.html")
}

fn child(html: &str) -> Document {
    Document::parse_str(html, "http://child.example/f.html")
}

#[test]
fn slots_are_found_with_their_attributes() {
    let mut d = top(
        "<p>x<iframe src='a.html' name=one sandbox='allow-scripts allow-forms' loading=lazy \
         allow='camera' allowfullscreen referrerpolicy=NO-referrer scrolling=no></iframe>\
         <iframe srcdoc='<b>hi</b>'></iframe><iframe></iframe>",
    );
    let s = d.frame_slots();
    assert_eq!(s.len(), 3);
    assert_eq!(s[0].id, FrameId(1));
    assert_eq!(s[0].parent, FrameId::TOP);
    assert_eq!(s[0].depth, 1);
    assert_eq!(s[0].src.as_deref(), Some("http://top.example/dir/a.html"));
    assert_eq!(s[0].name, "one");
    let sb = s[0].sandbox.unwrap();
    assert!(sb.scripts && sb.forms && !sb.same_origin && !sb.popups);
    assert!(s[0].lazy && s[0].allow_fullscreen);
    assert_eq!(s[0].allow, "camera");
    assert_eq!(s[0].referrer_policy, "no-referrer");
    assert_eq!(s[0].scrolling, FrameScrolling::No);
    assert_eq!(s[1].srcdoc.as_deref(), Some("<b>hi</b>"));
    assert_eq!(s[1].src, None);
    assert_eq!(s[2].src, None);
    assert!(s.iter().all(|x| x.wants_load() && x.blocked.is_none()));
    // Stable on a second look.
    assert_eq!(d.frame_slots(), s);
}

#[test]
fn sandbox_attribute_reading() {
    assert_eq!(Sandbox::parse(""), Sandbox::default());
    let s = Sandbox::parse(" ALLOW-SAME-ORIGIN  allow-popups\nbogus allow-top-navigation");
    assert!(s.same_origin && s.popups && s.top_navigation && !s.scripts);
}

#[test]
fn attach_and_detach() {
    let mut d = top("<iframe src=a.html></iframe><iframe src=b.html name=b></iframe>");
    let s = d.frame_slots();
    let (a, b) = (s[0].id, s[1].id);
    assert!(d.attach_frame(a, child("<p>A"), Some(s[0].generation)));
    assert!(!d.frame_slots()[0].wants_load());
    assert!(d.frame_slots()[1].wants_load());
    assert_eq!(d.frame_ids(), alloc::vec![a]);
    assert_eq!(d.frame_doc(a).unwrap().frame_info().id, a);
    assert_eq!(
        d.frame_doc(a).unwrap().frame_info().parent,
        Some(FrameId::TOP)
    );
    // A stale generation, an unknown id and the top are refused.
    assert!(!d.attach_frame(b, child("x"), Some(99)));
    assert!(!d.attach_frame(FrameId(77), child("x"), None));
    assert!(!d.attach_frame(FrameId::TOP, child("x"), None));
    assert!(d.attach_frame(b, child("<p>B"), None));
    // window.name starts as the element's name, and a replacement keeps what the frame had.
    assert_eq!(d.frame_doc(b).unwrap().window_name(), "b");
    d.frame_doc_mut(b).unwrap().set_window_name("renamed");
    assert!(d.attach_frame(b, child("<p>B2"), None));
    assert_eq!(d.frame_doc(b).unwrap().window_name(), "renamed");
    assert_eq!(d.frames_named("renamed"), alloc::vec![b]);
    let gone = d.detach_frame(a).unwrap();
    assert_eq!(gone.frame_info().id, FrameId::TOP);
    assert!(d.frame_slots()[0].wants_load());
    assert!(d.detach_frame(a).is_none());
}

#[test]
fn nested_slots_belong_to_the_top_table() {
    let mut d = top("<iframe src=a.html></iframe>");
    let a = d.frame_slots()[0].id;
    assert!(d.attach_frame(
        a,
        child("<p>in<iframe src=g.html></iframe><iframe src=h.html></iframe>"),
        None
    ));
    let s = d.frame_slots();
    assert_eq!(s.len(), 3);
    assert_eq!((s[1].parent, s[1].depth), (a, 2));
    assert_eq!(s[1].src.as_deref(), Some("http://child.example/g.html"));
    assert_eq!(d.child_frames(a), alloc::vec![s[1].id, s[2].id]);
    assert_eq!(d.child_frames(FrameId::TOP), alloc::vec![a]);
    // Replacing the child drops its frames' slots, and says so.
    assert!(d.attach_frame(a, child("<p>other"), None));
    assert_eq!(d.frame_slots().len(), 1);
    let closed = d.take_closed_frames();
    assert_eq!(closed, alloc::vec![s[1].id, s[2].id]);
    assert!(d.take_closed_frames().is_empty());
}

#[test]
fn caps_on_count_and_depth() {
    let mut html = String::new();
    for _ in 0..40 {
        html.push_str("<iframe src=x></iframe>");
    }
    let mut d = top(&html);
    assert_eq!(d.frame_slots().len(), 32);
    // Depth: the page is 0; frames at depth 6 are blocked.
    let mut d = top("<iframe></iframe>");
    let mut id = d.frame_slots()[0].id;
    for depth in 1..=6u32 {
        let s = d.frame_slots();
        let me = s.iter().find(|x| x.id == id).unwrap().clone();
        assert_eq!(me.depth, depth);
        if depth <= 5 {
            assert!(me.blocked.is_none(), "depth {depth}");
            assert!(d.attach_frame(id, child("<iframe></iframe>"), None));
            id = d
                .frame_slots()
                .into_iter()
                .find(|x| x.parent == id)
                .unwrap()
                .id;
        } else {
            assert_eq!(me.blocked, Some(FrameBlock::TooDeep));
            assert!(!me.wants_load());
            assert!(!d.attach_frame(id, child("x"), None));
        }
    }
}

#[test]
fn script_made_iframes_are_noticed() {
    let mut d = top("<div id=box></div>");
    assert!(d.frame_slots().is_empty());
    let mut d = Document::parse_scripted(
        b"<div id=box></div><script>var f = document.createElement('iframe'); f.src='/s.html'; f.setAttribute('name','made'); document.getElementById('box').appendChild(f); window.f = f;</script>",
        "http://top.example/",
        None,
        Limits::default(),
        Some(ScriptLimits::default()),
    );
    struct H;
    impl ScriptHost for H {
        fn measurer(&self) -> &dyn TextMeasurer {
            &ApproxMeasurer
        }
        fn viewport(&self) -> Viewport {
            Viewport::new(800, 600)
        }
        fn scroll(&self) -> (i32, i32) {
            (0, 0)
        }
        fn now(&self) -> f64 {
            0.0
        }
        fn time(&self) -> f64 {
            0.0
        }
    }
    d.run(&mut H);
    let s = d.frame_slots();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].src.as_deref(), Some("http://top.example/s.html"));
    assert_eq!(s[0].name, "made");
    let _: Vec<u8> = Vec::new();
}

fn lay(d: &mut Document) {
    d.layout(&Viewport::new(400, 300), &ApproxMeasurer);
}

/// A page whose frames are all filled: `parts` are (parent frame index in slot order, html)
/// for the slots in the order `frame_slots` lists them, attached as they appear.
fn assemble(top_html: &str, kids: &[&str]) -> Document {
    let mut d = top(top_html);
    let mut done = 0;
    for _ in 0..8 {
        let slots = d.frame_slots();
        let mut any = false;
        for s in slots.iter().filter(|s| s.attached.is_none()) {
            if done < kids.len() {
                assert!(d.attach_frame(s.id, child(kids[done]), None));
                done += 1;
                any = true;
            }
        }
        if !any {
            break;
        }
    }
    lay(&mut d);
    d
}

fn texts_of(d: &Document) -> Vec<(u32, i32, i32, String)> {
    let dl = d.display_list();
    dl.entries
        .iter()
        .chain(dl.fixed.iter())
        .filter_map(|e| match &e.item {
            Item::Text(t) => Some((e.frame.0, t.x, t.baseline, String::from(dl.run_text(t)))),
            _ => None,
        })
        .collect()
}

const FLAT: &str = "<body style='margin:0'>";

fn slot(d: &mut Document, i: usize) -> FrameSlot {
    d.frame_slots()[i].clone()
}

#[test]
fn default_size_attributes_and_borders() {
    // 300 x 150 content box inside the UA's 2px inset border.
    let mut d = assemble(&alloc::format!("{FLAT}<iframe></iframe>"), &["<p>x"]);
    let s = slot(&mut d, 0);
    assert_eq!((s.rect.x, s.rect.w, s.rect.h), (2, 300, 150));
    assert!(s.rendered);
    // width/height attributes, CSS over them, frameborder=0.
    let mut d = assemble(
        &alloc::format!(
            "{FLAT}<iframe width=120 height=60 frameborder=0></iframe>\
             <iframe width=120 height=60 style='width:50px;height:20px;border:0'></iframe>\
             <iframe width='50%' height=10 frameborder=no style='display:block'></iframe>"
        ),
        &["a", "b", "c"],
    );
    let s = d.frame_slots();
    assert_eq!((s[0].rect.x, s[0].rect.w, s[0].rect.h), (0, 120, 60));
    assert_eq!((s[1].rect.w, s[1].rect.h), (50, 20));
    assert_eq!((s[2].rect.w, s[2].rect.h), (200, 10));
}

#[test]
fn display_none_has_a_slot_but_no_box() {
    let mut d = assemble(
        &alloc::format!("{FLAT}<iframe style='display:none' src=x></iframe><iframe></iframe>"),
        &["<p>hidden", "<p>shown"],
    );
    let s = d.frame_slots();
    assert!(!s[0].rendered && s[0].rect.is_empty());
    assert!(s[1].rendered);
    let t: Vec<String> = texts_of(&d).into_iter().map(|x| x.3).collect();
    assert_eq!(t, alloc::vec!["shown"]);
    // Showing it lays it out.
    let t = d.frame_doc(s[0].id).unwrap().text();
    assert_eq!(t, "");
}

#[test]
fn the_frame_viewport_is_the_slot_and_it_wraps_there() {
    let mut d = assemble(
        &alloc::format!("{FLAT}<iframe width=100 height=200 style='border:0'></iframe>"),
        &[
            "<body style='margin:0'><p style='margin:0'>aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj</p><div style='width:50vw;height:10px;background:red'>",
        ],
    );
    let id = d.frame_slots()[0].id;
    let vp = d.frame_viewport(id).unwrap();
    assert_eq!((vp.width, vp.height), (100, 200));
    // Wrapped in 100 px: several lines, none wider than the slot; the 50vw box is 50 px wide.
    let t = texts_of(&d);
    let lines: Vec<i32> = t.iter().map(|x| x.2).collect();
    let mut uniq = lines.clone();
    uniq.dedup();
    assert!(uniq.len() >= 3, "{t:?}");
    let dl = d.display_list();
    let red = dl
        .entries
        .iter()
        .find_map(|e| match e.item {
            Item::Rect {
                rect,
                color: 0xFFFF_0000,
            } => Some(rect),
            _ => None,
        })
        .unwrap();
    assert_eq!(red.w, 50);
}

#[test]
fn zoom_and_dark_follow_the_parent() {
    let mut d = top(&alloc::format!(
        "{FLAT}<iframe width=100 height=50 style='border:0'></iframe>"
    ));
    let id = d.frame_slots()[0].id;
    assert!(d.attach_frame(id, child("<p>zz"), None));
    let vp = Viewport {
        width: 400,
        height: 300,
        zoom: 200,
        dark: true,
    };
    d.layout(&vp, &ApproxMeasurer);
    let s = d.frame_slots()[0].clone();
    assert_eq!((s.rect.w, s.rect.h), (200, 100));
    let v = d.frame_viewport(id).unwrap();
    assert_eq!((v.width, v.height, v.zoom, v.dark), (200, 100, 200, true));
    // The text is drawn at twice the size; the frame's canvas is the dark one.
    let dl = d.display_list();
    let t = dl
        .entries
        .iter()
        .find_map(|e| match &e.item {
            Item::Text(t) => Some(*t),
            _ => None,
        })
        .unwrap();
    assert_eq!(t.font.size, 32);
    let canvas = dl
        .entries
        .iter()
        .find_map(|e| match e.item {
            Item::Rect { color, .. } if e.frame == id => Some(color),
            _ => None,
        })
        .unwrap();
    assert_eq!(canvas, crate::page::CANVAS_DARK);
}

#[test]
fn entries_are_clipped_to_the_slot_and_moved_into_place() {
    let d = assemble(
        &alloc::format!(
            "{FLAT}<p style='margin:0'>top</p><iframe width=80 height=30 style='border:0'></iframe>"
        ),
        &[
            "<body style='margin:0'><p style='margin:0;font-size:20px'>one</p><p style='margin:0'>two</p><p style='margin:0'>three</p><p style='margin:0'>four</p>",
        ],
    );
    let slot = Rect::new(0, 19, 80, 30);
    let dl = d.display_list();
    for e in dl.entries.iter().filter(|e| e.frame == FrameId(1)) {
        let c = dl.clips[e.clip as usize];
        assert!(
            c.x >= slot.x
                && c.y >= slot.y
                && c.right() <= slot.right()
                && c.bottom() <= slot.bottom(),
            "{c:?}"
        );
    }
    let t = texts_of(&d);
    let one = t.iter().find(|x| x.3 == "one").unwrap();
    assert_eq!((one.0, one.1), (1, 0));
    assert!(one.2 > 19 && one.2 < 49, "{one:?}");
    // The page's own entries stay unclipped and are frame 0.
    assert!(
        dl.entries
            .iter()
            .filter(|e| e.frame == FrameId::TOP)
            .all(|e| e.clip == 0)
    );
}

#[test]
fn a_frame_is_opaque_and_painted_in_order() {
    let d = assemble(
        &alloc::format!(
            "{FLAT}<div style='background:#111111;height:20px'></div>\
             <iframe width=50 height=20 style='border:0'></iframe>\
             <div style='background:#222222;height:20px'></div>"
        ),
        &["<body style='background:#333333'>"],
    );
    let dl = d.display_list();
    let order: Vec<(u32, u32)> = dl
        .entries
        .iter()
        .filter_map(|e| match e.item {
            Item::Rect { color, .. } => Some((e.frame.0, color)),
            _ => None,
        })
        .collect();
    assert_eq!(order[0], (0, 0xFF11_1111));
    assert_eq!(order[1], (1, 0xFF33_3333));
    assert!(order.contains(&(0, 0xFF22_2222)));
    let pos = |c: u32| order.iter().position(|x| x.1 == c).unwrap();
    assert!(pos(0xFF11_1111) < pos(0xFF33_3333) && pos(0xFF33_3333) < pos(0xFF22_2222));
}

#[test]
fn nested_frames_clip_to_each_slot() {
    // 100x60 slot holding a page with a 200x200 frame: the grandchild shows through 100x60 only.
    let mut d = assemble(
        &alloc::format!("{FLAT}<iframe width=100 height=60 style='border:0'></iframe>"),
        &[
            "<body style='margin:0'><iframe width=200 height=200 style='border:0'></iframe>",
            "<body style='margin:0'><p style='margin:0'>deep</p>",
        ],
    );
    let dl = d.display_list();
    let deep: Vec<&Entry> = dl
        .entries
        .iter()
        .filter(|e| e.frame == FrameId(2))
        .collect();
    assert!(!deep.is_empty());
    for e in deep {
        let c = dl.clips[e.clip as usize];
        assert!(c.right() <= 100 && c.bottom() <= 60, "{c:?}");
    }
    assert_eq!(d.frame_slots()[1].depth, 2);
    assert_eq!(d.frame_slots()[1].rect, Rect::new(0, 0, 200, 200));
    assert_eq!(texts_of(&d).iter().filter(|x| x.3 == "deep").count(), 1);
}

#[test]
fn scrolling_moves_the_content_and_is_clamped() {
    let mut body = String::from("<body style='margin:0'>");
    for i in 0..30 {
        body.push_str(&alloc::format!(
            "<p style='margin:0;height:20px'>line{i}</p>"
        ));
    }
    let mut d = assemble(
        &alloc::format!("{FLAT}<iframe width=100 height=100 style='border:0'></iframe>"),
        &[&body],
    );
    let id = d.frame_slots()[0].id;
    let s = d.frame_slots()[0].clone();
    assert_eq!(s.content.1, 600);
    let y0 = texts_of(&d).iter().find(|x| x.3 == "line0").unwrap().2;
    assert_eq!(d.set_frame_scroll(id, 0, 50), (0, 50));
    assert!(d.needs_layout());
    lay(&mut d);
    let t = texts_of(&d);
    assert_eq!(
        t.iter().find(|x| x.3 == "line0").map(|x| x.2),
        None,
        "scrolled out"
    );
    let l3 = t.iter().find(|x| x.3 == "line3").unwrap().2;
    assert_eq!(l3, y0 + 3 * 20 - 50);
    // Clamped at the end, and never negative.
    assert_eq!(d.set_frame_scroll(id, 0, 99999), (0, 500));
    assert_eq!(d.set_frame_scroll(id, -5, -5), (0, 0));
    assert_eq!(d.frame_scroll(id), Some((0, 0)));
    // scrolling=no pins it.
    let mut d = assemble(
        &alloc::format!(
            "{FLAT}<iframe scrolling=no width=100 height=100 style='border:0'></iframe>"
        ),
        &[&body],
    );
    let id = d.frame_slots()[0].id;
    d.set_frame_scroll(id, 0, 50);
    lay(&mut d);
    assert_eq!(d.frame_scroll(id), Some((0, 0)));
}

#[test]
fn only_the_visible_part_of_a_scrolled_frame_is_listed() {
    let mut body = String::from("<body style='margin:0'>");
    for i in 0..3000 {
        body.push_str(&alloc::format!(
            "<p style='margin:0;height:20px'>row{i}</p>"
        ));
    }
    let mut d = assemble(
        &alloc::format!("{FLAT}<iframe width=100 height=100 style='border:0'></iframe>"),
        &[&body],
    );
    let n = d.display_list().entries.len();
    assert!(n < 400, "{n} entries for a 100 px window");
    let id = d.frame_slots()[0].id;
    d.set_frame_scroll(id, 0, 40000);
    lay(&mut d);
    let t = texts_of(&d);
    assert!(t.iter().any(|x| x.3 == "row2000"), "{t:?}");
    assert!(d.display_list().entries.len() < 400);
}

#[test]
fn fixed_boxes_in_a_frame_stay_in_the_frame_view() {
    let mut body = String::from(
        "<body style='margin:0'><div style='position:fixed;top:0;left:0;background:#abcdef;width:30px;height:10px'></div>",
    );
    for i in 0..30 {
        body.push_str(&alloc::format!("<p style='margin:0;height:20px'>l{i}</p>"));
    }
    let mut d = assemble(
        &alloc::format!(
            "{FLAT}<p style='margin:0'>t</p><iframe width=100 height=100 style='border:0'></iframe>"
        ),
        &[&body],
    );
    let id = d.frame_slots()[0].id;
    let find = |d: &Document| {
        d.display_list()
            .entries
            .iter()
            .find_map(|e| match e.item {
                Item::Rect {
                    rect,
                    color: 0xFFAB_CDEF,
                } => Some(rect),
                _ => None,
            })
            .unwrap()
    };
    let r0 = find(&d);
    d.set_frame_scroll(id, 0, 100);
    lay(&mut d);
    assert_eq!(find(&d), r0);
    assert_eq!(r0, Rect::new(0, d.frame_slots()[0].rect.y, 30, 10));
}

#[test]
fn positioned_content_stacks_over_a_frame() {
    let mut d = assemble(
        &alloc::format!(
            "{FLAT}<iframe width=100 height=100 style='border:0'></iframe>\
             <div style='position:absolute;left:10px;top:10px;width:20px;height:20px;background:#aa0000'></div>"
        ),
        &["<body style='background:#00aa00'>"],
    );
    let dl = d.display_list();
    let pos = |c: u32| {
        dl.entries
            .iter()
            .position(|e| matches!(e.item, Item::Rect { color, .. } if color == c))
            .unwrap()
    };
    assert!(pos(0xFF00_AA00) < pos(0xFFAA_0000));
    // And the pointer goes to the box above the frame, not into it.
    let h = d.hit_test(15, 15, (0, 0));
    assert_eq!(h.frame, FrameId::TOP);
    assert_eq!(d.hit_test(60, 60, (0, 0)).frame, FrameId(1));
    d.frame_slots();
}

#[test]
fn hit_testing_goes_into_the_frame() {
    let mut d = assemble(
        &alloc::format!(
            "{FLAT}<p style='margin:0'><a href='/up'>up</a></p><iframe width=120 height=60 style='border:0'></iframe>"
        ),
        &[
            "<body style='margin:0'><p style='margin:0'><a href='go.html' target=x title=tip>link</a></p><form><input name=q size=5></form>",
        ],
    );
    let slot = d.frame_slots()[0].clone();
    // The page's own link.
    let h = d.hit_test(3, 5, (0, 0));
    assert_eq!(h.frame, FrameId::TOP);
    assert_eq!(h.link.unwrap().url, "http://top.example/up");
    // The frame's link: the URL is the frame document's, the point is in its viewport.
    let h = d.hit_test(3, slot.rect.y + 5, (0, 0));
    assert_eq!(h.frame, slot.id);
    assert_eq!(h.point, (3, 5));
    assert_eq!(h.scroll, (0, 0));
    let l = h.link.unwrap();
    assert_eq!(l.url, "http://child.example/go.html");
    assert_eq!(l.target.as_deref(), Some("x"));
    assert_eq!(h.title.as_deref(), Some("tip"));
    // The control, whose id is the frame's.
    let c = d.frame_doc(slot.id).unwrap().controls()[0].rect;
    let h = d.hit_test(c.x + 2, slot.rect.y + c.y + 2, (0, 0));
    assert_eq!(h.frame, slot.id);
    assert_eq!(h.control, Some(ControlId(0)));
    // The page scrolled: viewport points move, the frame's do not.
    let h = d.hit_test(3, slot.rect.y + 5 - 7, (0, 7));
    assert_eq!(h.frame, slot.id);
    assert_eq!(h.point, (3, 5));
    // Outside the frame: the page.
    assert_eq!(d.hit_test(300, slot.rect.y + 5, (0, 0)).frame, FrameId::TOP);
    assert_eq!(d.frame_at(3, slot.rect.y + 5, (0, 0)), slot.id);
}

#[test]
fn hits_in_a_scrolled_frame_and_in_the_border() {
    let mut body = String::from("<body style='margin:0'>");
    for i in 0..30 {
        body.push_str(&alloc::format!(
            "<p style='margin:0;height:20px'><a href='/l{i}'>l{i}</a></p>"
        ));
    }
    let mut d = assemble(
        &alloc::format!(
            "{FLAT}<iframe width=100 height=100 style='border:5px solid red'></iframe>"
        ),
        &[&body],
    );
    let slot = d.frame_slots()[0].clone();
    assert_eq!(slot.rect.x, 5);
    let id = slot.id;
    d.set_frame_scroll(id, 0, 60);
    lay(&mut d);
    // y=0 of the viewport is document y 60: line 3.
    let h = d.hit_test(slot.rect.x + 2, slot.rect.y + 5, (0, 0));
    assert_eq!(h.link.as_ref().unwrap().url, "http://child.example/l3");
    assert_eq!((h.point, h.scroll), ((2, 5), (0, 60)));
    // On the border it is the iframe element: no link, and the page's frame.
    let h = d.hit_test(2, 20, (0, 0));
    assert_eq!(h.frame, FrameId::TOP);
    assert!(h.link.is_none());
}

#[test]
fn nested_hits_report_the_innermost_frame() {
    let mut d = assemble(
        &alloc::format!(
            "{FLAT}<div style='height:10px'></div><iframe width=100 height=60 style='border:0'></iframe>"
        ),
        &[
            "<body style='margin:0'><div style='height:20px'></div><iframe width=80 height=30 style='border:0'></iframe>",
            "<body style='margin:0'><a href='/in'>inner</a>",
        ],
    );
    let s = d.frame_slots();
    assert_eq!(s.len(), 2);
    let h = d.hit_test(3, 10 + 20 + 4, (0, 0));
    assert_eq!(h.frame, s[1].id);
    assert_eq!(h.point, (3, 4));
    assert_eq!(h.link.unwrap().url, "http://child.example/in");
    // Between the two: the first frame.
    let h = d.hit_test(3, 10 + 5, (0, 0));
    assert_eq!(h.frame, s[0].id);
}

#[test]
fn the_wheel_chains_outwards() {
    let mut inner = String::from("<body style='margin:0'>");
    for i in 0..10 {
        inner.push_str(&alloc::format!("<p style='margin:0;height:20px'>i{i}</p>"));
    }
    let mut outer = String::from("<body style='margin:0'>");
    for i in 0..10 {
        outer.push_str(&alloc::format!("<p style='margin:0;height:20px'>o{i}</p>"));
    }
    outer.push_str("<iframe width=100 height=50 style='border:0'></iframe>");
    let mut d = assemble(
        &alloc::format!("{FLAT}<iframe width=100 height=100 style='border:0'></iframe>"),
        &[&outer, &inner],
    );
    let s = d.frame_slots();
    let (o, i) = (s[0].id, s[1].id);
    // 200 px of lines and the inner frame on a line of its own (with the descender gap).
    assert_eq!(s[0].content.1, 255);
    // The inner frame is at outer y=200; the outer frame shows 100 px, so scroll it to the end.
    assert_eq!(d.set_frame_scroll(o, 0, 9999), (0, 155));
    lay(&mut d);
    let y = d.frame_slots()[1].rect.y - 155;
    assert!((0..100).contains(&y));
    // 30 px of wheel over the inner frame: all of it used there.
    assert_eq!(d.scroll_frames_at(5, y + 5, (0, 0), 0, 30), (0, 0));
    assert_eq!(d.frame_scroll(i), Some((0, 30)));
    // The inner has 150 px to go (200 - 50): 200 px more leaves 80 for the outer; the outer
    // is at its end, so the page gets what remains.
    assert_eq!(d.scroll_frames_at(5, y + 5, (0, 0), 0, 200), (0, 80));
    assert_eq!(d.frame_scroll(i), Some((0, 150)));
    // Outside any frame: all of it is the page's.
    assert_eq!(d.scroll_frames_at(300, 5, (0, 0), 3, 40), (3, 40));
    // Over the outer frame's own area (not the inner): the outer scrolls up.
    assert_eq!(d.scroll_frames_at(5, 5, (0, 0), 0, -20), (0, 0));
    assert_eq!(d.frame_scroll(o), Some((0, 135)));
}

#[test]
fn reveal_scrolls_each_frame_and_returns_page_space() {
    let mut inner = String::from("<body style='margin:0'>");
    for i in 0..40 {
        inner.push_str(&alloc::format!("<p style='margin:0;height:20px'>i{i}</p>"));
    }
    let mut d = assemble(
        &alloc::format!(
            "{FLAT}<div style='height:100px'></div><iframe width=100 height=100 style='border:0'></iframe>"
        ),
        &[&inner],
    );
    let id = d.frame_slots()[0].id;
    // Row 30 is at y=600 in the frame's document.
    let r = d.reveal(id, Rect::new(0, 600, 40, 20));
    let sc = d.frame_scroll(id).unwrap();
    // It is centred: the frame's offset puts it in the middle of its 100 px.
    assert_eq!(sc, (0, 600 + 10 - 50));
    // In the page: 100 (the spacer) + (600 - 560).
    assert_eq!(r, Rect::new(0, 100 + 40, 40, 20));
    // Already in view: nothing moves.
    let r = d.reveal(id, Rect::new(0, 570, 10, 10));
    assert_eq!(d.frame_scroll(id), Some((0, 560)));
    assert_eq!(r.y, 110);
    // A fragment.
    let mut d = assemble(
        &alloc::format!("{FLAT}<iframe width=100 height=50 style='border:0'></iframe>"),
        &[
            "<body style='margin:0'><div style='height:300px'></div><h2 id=here style='margin:0'>here</h2><div style='height:300px'></div>",
        ],
    );
    let id = d.frame_slots()[0].id;
    assert!(d.frame_fragment(id, "here"));
    assert_eq!(d.frame_scroll(id).unwrap().1, 300);
    assert!(!d.frame_fragment(id, "nothere"));
    assert!(d.frame_fragment(id, "top"));
    assert_eq!(d.frame_scroll(id).unwrap().1, 0);
}

#[test]
fn focus_order_runs_through_frames() {
    let d = assemble(
        &alloc::format!(
            "{FLAT}<input name=a><iframe width=100 height=40 style='border:0'></iframe><input name=b>\
             <iframe width=100 height=40 style='border:0'></iframe><input name=c disabled><input name=d>"
        ),
        &[
            "<body style='margin:0'><input name=f1><input name=f2 disabled><input name=f3>",
            "<body style='margin:0'><input name=g1>",
        ],
    );
    let order = d.focus_order();
    let named: Vec<(u32, String)> = order
        .iter()
        .map(|&(f, c)| {
            (
                f.0,
                d.frame_doc(f).unwrap().controls()[c.0 as usize]
                    .name
                    .clone(),
            )
        })
        .collect();
    let want: Vec<(u32, &str)> = alloc::vec![
        (0, "a"),
        (1, "f1"),
        (1, "f3"),
        (0, "b"),
        (2, "g1"),
        (0, "d")
    ];
    assert_eq!(
        named
            .iter()
            .map(|(f, n)| (*f, n.as_str()))
            .collect::<Vec<_>>(),
        want
    );
}

#[test]
fn the_keyboard_frame_is_tracked() {
    let mut d = assemble(&alloc::format!("{FLAT}<iframe></iframe>"), &["<p>x"]);
    let id = d.frame_slots()[0].id;
    assert_eq!(d.focused_frame(), FrameId::TOP);
    d.set_focused_frame(id);
    assert_eq!(d.focused_frame(), id);
    d.set_focused_frame(FrameId(40));
    assert_eq!(d.focused_frame(), id);
    d.detach_frame(id);
    assert_eq!(d.focused_frame(), FrameId::TOP);
}

#[test]
fn find_and_text_include_frames_in_order() {
    let mut d = assemble(
        &alloc::format!(
            "{FLAT}<p style='margin:0'>needle one</p><iframe width=200 height=40 style='border:0'></iframe>\
             <p style='margin:0'>needle two</p><iframe width=200 height=40 style='border:0'></iframe><p style='margin:0'>needle five</p>"
        ),
        &[
            "<body style='margin:0'><p style='margin:0'>a needle three</p>",
            "<body style='margin:0'><p style='margin:0'>needle four</p>",
        ],
    );
    let m = d.find("needle", false);
    assert_eq!(m.len(), 5);
    let frames: Vec<u32> = m.iter().map(|x| x.frame.0).collect();
    assert_eq!(frames, alloc::vec![0, 1, 0, 2, 0]);
    // In page space: the frame's match is inside its slot.
    let s = d.frame_slots();
    assert!(s[0].rect.contains(m[1].rects[0].x + 1, m[1].rects[0].y + 1));
    // The page text has the frames' text where they are.
    let t = d.text();
    let pos = |w: &str| t.find(w).unwrap();
    assert!(
        pos("one") < pos("three")
            && pos("three") < pos("two")
            && pos("two") < pos("four")
            && pos("four") < pos("five"),
        "{t:?}"
    );
    let _ = &mut d;
}

#[test]
fn a_match_scrolled_out_of_its_frame_can_be_revealed() {
    let mut body = String::from("<body style='margin:0'>");
    for i in 0..40 {
        body.push_str(&alloc::format!(
            "<p style='margin:0;height:20px'>{}</p>",
            if i == 30 { "needle" } else { "hay" }
        ));
    }
    let mut d = assemble(
        &alloc::format!("{FLAT}<iframe width=100 height=100 style='border:0'></iframe>"),
        &[&body],
    );
    let m = d.find("needle", true);
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].frame, FrameId(1));
    assert!(
        m[0].rects.is_empty(),
        "outside the slot, so not drawn there"
    );
    assert_eq!(m[0].frame_rects.len(), 1);
    let r = m[0].frame_rects[0];
    assert!(r.y >= 600 && r.y < 620, "{r:?}");
    let page_rect = d.reveal(m[0].frame, r);
    lay(&mut d);
    let m = d.find("needle", true);
    assert_eq!(m[0].rects.len(), 1);
    assert!(Rect::new(0, 0, 100, 100).contains(m[0].rects[0].x, m[0].rects[0].y));
    assert_eq!(m[0].rects[0].y, page_rect.y);
}

#[test]
fn replacing_and_removing_documents() {
    let mut d = assemble(&alloc::format!("{FLAT}<iframe></iframe>"), &["<p>old"]);
    let id = d.frame_slots()[0].id;
    d.set_frame_scroll(id, 0, 3);
    assert!(d.attach_frame(id, child("<p>new"), None));
    lay(&mut d);
    assert_eq!(d.frame_scroll(id), Some((0, 0)));
    let t: Vec<String> = texts_of(&d).into_iter().map(|x| x.3).collect();
    assert_eq!(t, alloc::vec!["new"]);
    let doc = d.detach_frame(id).unwrap();
    assert_eq!(doc.title(), "");
    lay(&mut d);
    assert!(texts_of(&d).is_empty());
    assert!(d.needs_layout() || !d.needs_layout());
}

#[test]
fn removing_an_iframe_element_closes_its_frame() {
    let mut d = Document::parse_scripted(
        b"<div id=box><iframe id=a></iframe><iframe id=b></iframe></div>",
        "http://top.example/",
        None,
        Limits::default(),
        Some(ScriptLimits::default()),
    );
    let s = d.frame_slots();
    assert!(d.attach_frame(s[0].id, child("<p>a"), None));
    assert!(d.attach_frame(s[1].id, child("<p>b"), None));
    run_script(
        &mut d,
        "var b = document.getElementById('b'); b.parentNode.removeChild(b);",
    );
    let s2 = d.frame_slots();
    assert_eq!(s2.len(), 1);
    assert_eq!(s2[0].id, s[0].id);
    assert_eq!(d.take_closed_frames(), alloc::vec![s[1].id]);
    assert_eq!(d.frame_ids(), alloc::vec![s[0].id]);
    // A new element is a new frame, with a new id.
    run_script(
        &mut d,
        "document.getElementById('box').appendChild(document.createElement('iframe'));",
    );
    let s3 = d.frame_slots();
    assert_eq!(s3.len(), 2);
    assert!(s3[1].id.0 > s[1].id.0);
    assert!(s3[1].wants_load());
    // Changing src starts a new generation.
    let g = s3[1].generation;
    run_script(
        &mut d,
        "document.querySelectorAll('iframe')[1].setAttribute('src', '/z.html');",
    );
    let s4 = d.frame_slots();
    assert_eq!(s4[1].generation, g + 1);
    assert_eq!(s4[1].src.as_deref(), Some("http://top.example/z.html"));
    // A document loaded for the old generation is refused.
    assert!(!d.attach_frame(s4[1].id, child("<p>late"), Some(g)));
    assert!(d.attach_frame(s4[1].id, child("<p>ok"), Some(g + 1)));
}

#[test]
fn a_freed_iframe_does_not_leave_its_frame_to_a_new_element() {
    let mut d = Document::parse_scripted(
        b"<div id=box><iframe id=a></iframe></div>",
        "http://top.example/",
        None,
        Limits::default(),
        Some(ScriptLimits::default()),
    );
    let s = d.frame_slots();
    assert!(d.attach_frame(s[0].id, child("<p>a"), None));
    // Free the element and make another in one go: the ids are reused.
    run_script(
        &mut d,
        "var box = document.getElementById('box'); box.innerHTML = ''; box.appendChild(document.createElement('iframe'));",
    );
    let s2 = d.frame_slots();
    assert_eq!(s2.len(), 1);
    assert_ne!(s2[0].id, s[0].id);
    assert!(s2[0].wants_load());
}

fn run_script(d: &mut Document, src: &str) {
    let mut h = Host::new();
    d.run(&mut h);
    with(d, &mut h, |r, _, _| {
        if let Err(e) = r.eval(src, "test") {
            panic!("{}", r.report(&e));
        }
    });
}

fn eval_str(d: &mut Document, src: &str) -> String {
    let mut h = Host::new();
    with(d, &mut h, |r, _, _| match r.eval(src, "test") {
        Ok(v) => r.to_string(v).unwrap_or_default(),
        Err(e) => alloc::format!("threw {}", r.report(&e)),
    })
}

fn scripted(html: &str) -> Document {
    let mut d = Document::parse_scripted(
        html.as_bytes(),
        "http://top.example/",
        None,
        Limits::default(),
        Some(ScriptLimits::default()),
    );
    d.run(&mut Host::new());
    d
}

#[test]
fn post_message_queues_and_deliver_message_fires() {
    let mut d = scripted("<p>x");
    let r = eval_str(
        &mut d,
        "window.got = []; window.addEventListener('message', function (e) { \
           got.push(JSON.stringify([e.data, e.origin, e.source === window, e.type, e instanceof MessageEvent, e instanceof Event, e.lastEventId, e.ports.length])); }); \
         window.onmessage = function (e) { got.push('on:' + e.data); }; \
         window.postMessage({ a: [1, 2], b: 'x' }, '*'); window.postMessage('plain', { targetOrigin: 'http://a.example' }); window.postMessage(undefined, '*'); 'ok'",
    );
    assert_eq!(r, "ok");
    let q = d.take_messages();
    assert_eq!(q.len(), 3);
    assert_eq!(q[0].target, FrameId::TOP);
    assert_eq!(q[0].target_origin, "*");
    assert_eq!(q[0].data, "{\"a\":[1,2],\"b\":\"x\"}");
    assert_eq!(q[1].target_origin, "http://a.example");
    assert_eq!(q[1].data, "\"plain\"");
    assert_eq!(q[2].data, "");
    assert!(d.take_messages().is_empty());
    // The host delivers them.
    let mut h = Host::new();
    for m in &q {
        assert!(d.deliver_message(
            &mut h,
            &Message {
                data: m.data.clone(),
                origin: String::from("http://top.example"),
                source: FrameId::TOP
            }
        ));
    }
    let got = eval_str(&mut d, "got.join('\\n')");
    let lines: Vec<&str> = got.lines().collect();
    assert_eq!(lines.len(), 6, "{got}");
    assert_eq!(
        lines[0],
        "[{\"a\":[1,2],\"b\":\"x\"},\"http://top.example\",true,\"message\",true,true,\"\",0]"
    );
    assert_eq!(lines[1], "on:[object Object]");
    assert_eq!(lines[2].matches("plain").count(), 1);
    // Bad data is refused; things JSON can't hold throw.
    assert!(
        eval_str(
            &mut d,
            "var c = {}; c.c = c; try { postMessage(c, '*'); 'no' } catch (e) { e.name }"
        )
        .contains("DataCloneError")
    );
    assert!(
        eval_str(&mut d, "try { postMessage(); 'no' } catch (e) { e.name }").contains("TypeError")
    );
    assert_eq!(d.take_messages().len(), 0);
}

#[test]
fn messages_go_to_a_frames_own_document() {
    let mut d = assemble(&alloc::format!("{FLAT}<iframe></iframe>"), &["<p>x"]);
    let id = d.frame_slots()[0].id;
    let mut h = Host::new();
    {
        let c = d.frame_doc_mut(id).unwrap();
        // A frame document without scripts takes no messages.
        assert!(!c.deliver_message(
            &mut h,
            &Message {
                data: String::new(),
                origin: String::new(),
                source: FrameId::TOP
            }
        ));
    }
    let mut c = scripted("<p>y");
    eval_str(
        &mut c,
        "window.n = 0; onmessage = function (e) { n += e.data; };",
    );
    assert!(d.attach_frame(id, c, None));
    let c = d.frame_doc_mut(id).unwrap();
    assert_eq!(c.frame_info().id, id);
    for _ in 0..3 {
        assert!(c.deliver_message(
            &mut h,
            &Message {
                data: String::from("2"),
                origin: String::from("http://top.example"),
                source: FrameId::TOP
            }
        ));
    }
    assert_eq!(eval_str(c, "n"), "6");
    // Its own postMessage names its own frame.
    eval_str(c, "postMessage('hi', '*')");
    let q = c.take_messages();
    assert_eq!(q[0].target, id);
}

/// A host that finds a second host for any other frame (the Browser's would be per frame).
struct Hosts {
    me: Host,
    next: Option<alloc::boxed::Box<Hosts>>,
    deny: bool,
}

impl Hosts {
    fn chain(n: usize) -> Hosts {
        Hosts {
            me: Host::new(),
            next: (n > 0).then(|| alloc::boxed::Box::new(Hosts::chain(n - 1))),
            deny: false,
        }
    }
}

impl ScriptHost for Hosts {
    fn measurer(&self) -> &dyn TextMeasurer {
        &ApproxMeasurer
    }
    fn viewport(&self) -> Viewport {
        Viewport::new(800, 600)
    }
    fn scroll(&self) -> (i32, i32) {
        (0, 0)
    }
    fn now(&self) -> f64 {
        self.me.now()
    }
    fn time(&self) -> f64 {
        self.me.time()
    }
    fn frame_host(&mut self, _id: FrameId) -> Option<&mut dyn ScriptHost> {
        if self.deny {
            return None;
        }
        self.next.as_deref_mut().map(|h| h as &mut dyn ScriptHost)
    }
}

/// Install `peer(id, source)` in the document's realm: evaluates `source` in frame `id`'s
/// realm (through `with_peer`) and gives back its string, or "refused".
fn install_peer(d: &mut Document, h: &mut dyn ScriptHost) {
    d.arm();
    let js = d.js.as_mut().expect("scripts on");
    js.test_entry(&mut d.page, h, |b, r| {
        let b2 = b.clone();
        let f = r.new_function("peer", 2, move |r, _t, a| {
            let id = r.to_number(a[0])? as u32;
            let src = r.to_string(a[1])?;
            let out = crate::script::frames::with_peer(&b2, FrameId(id), |doc, host| {
                doc.armed(|doc| {
                    doc.js
                        .as_mut()
                        .and_then(|j| {
                            j.test_entry(&mut doc.page, host, |_b, r2| {
                                match r2.eval(&src, "peer") {
                                    Ok(v) => r2.to_string(v).unwrap_or_default(),
                                    Err(e) => alloc::format!("threw {}", r2.report(&e)),
                                }
                            })
                        })
                        .unwrap_or_else(|| String::from("stopped"))
                })
            });
            Ok(r.string(&out.unwrap_or_else(|| String::from("refused"))))
        });
        let g = r.global();
        r.set(g, "peer", f).unwrap();
    });
    d.disarm();
}

#[test]
fn scripts_of_one_frame_can_run_code_in_another() {
    let mut root = scripted("<title>top title</title><iframe></iframe><iframe></iframe>");
    let ids: Vec<FrameId> = root.frame_slots().iter().map(|s| s.id).collect();
    let a = scripted("<title>a title</title><p id=pa>in a");
    let b = scripted("<title>b title</title>");
    assert!(root.attach_frame(ids[0], a, None));
    assert!(root.attach_frame(ids[1], b, None));
    let mut hs = Hosts::chain(4);
    install_peer(&mut root, &mut hs);
    for id in &ids {
        root.with_frame(*id, |d| install_peer(d, &mut hs)).unwrap();
    }
    let ev = |root: &mut Document, from: FrameId, src: &str, hs: &mut Hosts| -> String {
        root.with_frame(from, |d| {
            d.arm();
            let js = d.js.as_mut().unwrap();
            let out = js.test_entry(&mut d.page, hs, |_b, r| match r.eval(src, "t") {
                Ok(v) => r.to_string(v).unwrap_or_default(),
                Err(e) => alloc::format!("threw {}", r.report(&e)),
            });
            d.disarm();
            out.unwrap_or_default()
        })
        .unwrap_or_else(|| String::from("no frame"))
    };
    // The top reaches a frame, a frame the top, a frame its sibling.
    assert_eq!(
        ev(
            &mut root,
            FrameId::TOP,
            "peer(1, 'document.title')",
            &mut hs
        ),
        "a title"
    );
    assert_eq!(
        ev(&mut root, ids[0], "peer(0, 'document.title')", &mut hs),
        "top title"
    );
    assert_eq!(
        ev(&mut root, ids[0], "peer(2, 'document.title')", &mut hs),
        "b title"
    );
    // State survives between calls: it is the same realm.
    assert_eq!(
        ev(
            &mut root,
            FrameId::TOP,
            "peer(2, 'window.v = 7; v * 2')",
            &mut hs
        ),
        "14"
    );
    assert_eq!(ev(&mut root, ids[1], "String(window.v)", &mut hs), "7");
    // Errors in the other realm come back as text, not as a panic.
    assert!(ev(&mut root, FrameId::TOP, "peer(1, 'nope()')", &mut hs).starts_with("threw"));
    // Unknown frames and a host with no frame host are refused.
    assert_eq!(
        ev(&mut root, FrameId::TOP, "peer(9, '1')", &mut hs),
        "refused"
    );
    // A frame that called into the top while the top is calling into it: the top is busy.
    assert_eq!(
        ev(
            &mut root,
            FrameId::TOP,
            "peer(1, \"peer(0, '1')\")",
            &mut hs
        ),
        "refused"
    );
    // A frame reaching a frame reaching the top is fine; back to the first frame is not (it
    // is the one running).
    assert_eq!(
        ev(
            &mut root,
            ids[0],
            "peer(2, \"peer(0, 'document.title')\")",
            &mut hs
        ),
        "top title"
    );
    assert_eq!(
        ev(
            &mut root,
            ids[0],
            "peer(2, \"peer(1, 'document.title')\")",
            &mut hs
        ),
        "refused"
    );
    hs.deny = true;
    assert_eq!(
        ev(&mut root, FrameId::TOP, "peer(1, '1')", &mut hs),
        "refused"
    );
}

#[test]
fn window_name_is_the_browsing_contexts() {
    let mut d = scripted("<iframe name=kid></iframe>");
    let id = d.frame_slots()[0].id;
    assert_eq!(eval_str(&mut d, "name"), "");
    eval_str(&mut d, "name = 'top-name'");
    assert_eq!(d.window_name(), "top-name");
    d.set_window_name("again");
    assert_eq!(eval_str(&mut d, "window.name"), "again");
    assert!(d.attach_frame(id, scripted("<p>x"), None));
    let c = d.frame_doc_mut(id).unwrap();
    assert_eq!(eval_str(c, "name"), "kid");
    eval_str(c, "name = 'renamed'");
    // Kept across a navigation of the frame; target= finds it by it.
    assert!(d.attach_frame(id, scripted("<p>y"), None));
    assert_eq!(eval_str(d.frame_doc_mut(id).unwrap(), "name"), "renamed");
    assert_eq!(d.frames_named("renamed"), alloc::vec![id]);
    assert_eq!(d.frames_named("again"), alloc::vec![FrameId::TOP]);
    assert!(d.frames_named("nobody").is_empty());
    // Changing the element's name attribute renames the context.
    eval_str(
        &mut d,
        "document.querySelector('iframe').setAttribute('name', 'attr')",
    );
    d.frame_slots();
    assert_eq!(d.frames_named("attr"), alloc::vec![id]);
}

#[test]
fn extreme_numbers_do_not_overflow() {
    let mut d = assemble(
        &alloc::format!("{FLAT}<iframe width=100 height=100 style='border:0'></iframe>"),
        &["<p>x</p><p style='height:3000px'>y"],
    );
    let id = d.frame_slots()[0].id;
    for v in [i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX - 1, i32::MAX] {
        for w in [i32::MIN, 0, i32::MAX] {
            let _ = d.hit_test(v, w, (v, w));
            let _ = d.hit_test(5, 5, (v, w));
            let _ = d.frame_at(v, w, (w, v));
            let _ = d.scroll_frames_at(5, 5, (0, 0), v, w);
            let _ = d.scroll_frames_at(v, w, (w, v), w, v);
            d.set_frame_scroll(id, v, w);
            let r = d.reveal(id, Rect::new(v, w, v, w));
            let _ = r;
            d.reveal(id, Rect::new(0, w, 0, 0));
        }
    }
    lay(&mut d);
    assert!(d.frame_scroll(id).is_some());
}

#[test]
fn the_window_can_tell_where_it_sits() {
    use crate::script::frames::*;
    let mut root = scripted("<p>x</p><iframe id=a></iframe><div><iframe id=b></iframe></div>");
    let ids: Vec<FrameId> = root.frame_slots().iter().map(|s| s.id).collect();
    assert!(root.attach_frame(ids[0], scripted("<p>a</p><iframe id=c></iframe>"), None));
    root.frame_slots();
    let mut h = Host::new();
    root.arm();
    let top_node = root.page.dom.find_tag(crate::atoms::tag::IFRAME);
    let js = root.js.as_mut().unwrap();
    js.test_entry(&mut root.page, &mut h, |b, _r| {
        assert_eq!(own_info(b).id, FrameId::TOP);
        assert_eq!(own_info(b).parent, None);
        assert_eq!(child_frames(b), alloc::vec![ids[0], ids[1]]);
        assert_eq!(frame_of_element(b, top_node), Some(ids[0]));
        assert_eq!(frame_of_element(b, 0), None);
        assert!(frame_element(b).is_none());
    });
    root.disarm();
    let a = root.frame_doc_mut(ids[0]).unwrap();
    a.arm();
    let js = a.js.as_mut().unwrap();
    js.test_entry(&mut a.page, &mut h, |b, _r| {
        let i = own_info(b);
        assert_eq!((i.id, i.parent, i.depth), (ids[0], Some(FrameId::TOP), 1));
        assert_eq!(child_frames(b).len(), 1);
        let (pf, node) = frame_element(b).unwrap();
        assert_eq!(pf, FrameId::TOP);
        assert_eq!(node, top_node);
    });
}

#[test]
fn frames_in_flex_tables_floats_and_clipped_boxes() {
    let mut d = assemble(
        &alloc::format!(
            "{FLAT}<div style='display:flex;gap:10px'><iframe width=60 height=40 style='border:0'></iframe><iframe width=60 height=40 style='border:0'></iframe></div>\\
             <table><tr><td><iframe width=70 height=30 style='border:0'></iframe></td></tr></table>\\
             <div style='float:right;width:90px'><iframe width=80 height=20 style='border:0'></iframe></div>\\
             <div style='width:50px;height:30px;overflow:hidden'><iframe width=100 height=100 style='border:0'></iframe></div>"
        ),
        &[
            "<body style='background:#111111'>",
            "<body style='background:#222222'>",
            "<body style='background:#333333'>",
            "<body style='background:#444444'>",
            "<body style='background:#555555'>",
        ],
    );
    let s = d.frame_slots();
    assert_eq!(s.len(), 5);
    // Side by side in the flex row.
    assert_eq!(s[1].rect.x - s[0].rect.right(), 10);
    assert_eq!(s[0].rect.y, s[1].rect.y);
    // Each one's canvas is where its slot is, and the clipped one shows only 50 x 30.
    let dl = d.display_list();
    for slot in &s {
        let canvas = dl
            .entries
            .iter()
            .find(|e| e.frame == slot.id && matches!(e.item, Item::Rect { .. }))
            .unwrap();
        let Item::Rect { rect, .. } = canvas.item else {
            unreachable!()
        };
        assert_eq!(rect, slot.rect);
        let clip = dl.clips[canvas.clip as usize];
        if slot.id == s[4].id {
            assert_eq!((clip.w, clip.h), (50, 30), "{clip:?}");
        } else {
            assert_eq!(clip, slot.rect);
        }
    }
    // The pointer: the visible corner of the clipped frame is the frame's; the part of the
    // frame outside the box still has the page's hit rectangle, as any overflow does.
    let r = s[4].rect;
    assert_eq!(d.hit_test(r.x + 5, r.y + 5, (0, 0)).frame, s[4].id);
    d.frame_slots();
}

#[test]
fn a_frame_in_a_fixed_box_stays_put_when_the_page_scrolls() {
    let mut d = assemble(
        &alloc::format!(
            "{FLAT}<div style='position:fixed;left:10px;top:20px'><iframe width=100 height=50 style='border:0'></iframe></div><div style='height:3000px'></div>"
        ),
        &[
            "<body style='margin:0;background:#123456'><p style='margin:0'>fixed frame</p><input name=q>",
        ],
    );
    let dl = d.display_list();
    assert!(
        dl.entries.iter().all(|e| e.frame.is_top()),
        "all of it is in the fixed layer"
    );
    let fx: Vec<&Entry> = dl.fixed.iter().filter(|e| e.frame == FrameId(1)).collect();
    assert!(fx.len() >= 3);
    for e in &fx {
        let c = dl.clips[e.clip as usize];
        assert!(
            c.x >= 10 && c.y >= 20 && c.right() <= 110 && c.bottom() <= 70,
            "{c:?}"
        );
    }
    let s = d.frame_slots()[0].clone();
    assert_eq!(s.rect, Rect::new(10, 20, 100, 50));
    // Hit in viewport space whatever the page's scroll.
    for sy in [0, 500] {
        let h = d.hit_test(15, 25, (0, sy));
        assert_eq!(h.frame, s.id, "scrolled {sy}");
        assert_eq!(h.point, (5, 5));
    }
    assert_eq!(d.hit_test(15, 25 + 100, (0, 0)).frame, FrameId::TOP);
    // The wheel over it goes to the frame first.
    assert_eq!(
        d.scroll_frames_at(15, 25, (0, 500), 0, 10),
        (0, 10),
        "nothing to scroll in the frame"
    );
}

#[test]
fn layout_settles_with_frames_of_every_kind() {
    let mut d = assemble(
        &alloc::format!(
            "{FLAT}<iframe style='display:none'></iframe><iframe width=0 height=0></iframe>\
             <iframe style='visibility:hidden' width=50 height=50></iframe><iframe width=50 height=50></iframe>"
        ),
        &["<p>a", "<p>b<iframe></iframe>", "<p>c", "<p>d<iframe width=10 height=10></iframe>"],
    );
    assert!(!d.needs_layout());
    lay(&mut d);
    assert!(!d.needs_layout());
    // A change in a hidden frame doesn't ask for layouts nobody can see, and one in a shown
    // frame does, once.
    let ids = d.frame_ids();
    let doc = d.frame_doc_mut(ids[0]).unwrap();
    doc.supply_stylesheet("x.css", None);
    let shown = *ids.last().unwrap();
    d.frame_doc_mut(shown).unwrap().set_window_name("x");
    d.frame_doc_mut(shown).unwrap().page.mark_forms_changed();
    assert!(d.needs_layout());
    lay(&mut d);
    assert!(!d.needs_layout());
}

#[test]
fn the_iframe_element_hears_load_and_error() {
    let mut d = scripted(
        "<iframe id=a onload='window.log.push(\"attr\")'></iframe><iframe id=b></iframe>",
    );
    eval_str(
        &mut d,
        "window.log = []; document.getElementById('b').addEventListener('load', function (e) { log.push('b:' + e.type + ':' + (e.target.id) + ':' + e.bubbles); }); \
         document.getElementById('b').addEventListener('error', function (e) { log.push('b:' + e.type); });",
    );
    let ids: Vec<FrameId> = d.frame_slots().iter().map(|s| s.id).collect();
    let mut h = Host::new();
    assert!(d.frame_load_event(ids[1], &mut h, false));
    assert!(d.frame_load_event(ids[1], &mut h, true));
    assert!(d.frame_load_event(ids[0], &mut h, false));
    assert_eq!(eval_str(&mut d, "log.join()"), "b:load:b:false,b:error,attr");
    assert!(!d.frame_load_event(FrameId(9), &mut h, false));
    // From inside a frame: the element is in the parent's document, so it is the parent's realm.
    let mut outer = scripted("<iframe id=f></iframe>");
    eval_str(&mut outer, "window.n = 0; document.getElementById('f').onload = function () { n++; };");
    let id = outer.frame_slots()[0].id;
    assert!(outer.attach_frame(id, scripted("<iframe id=g></iframe>"), None));
    outer.frame_slots();
    let g = outer.frame_slots()[1].id;
    eval_str(outer.frame_doc_mut(id).unwrap(), "window.m = 0; document.getElementById('g').onload = function () { m++; };");
    assert!(outer.frame_load_event(g, &mut h, false));
    assert!(outer.frame_load_event(id, &mut h, false));
    assert_eq!(eval_str(&mut outer, "n"), "1");
    assert_eq!(eval_str(outer.frame_doc_mut(id).unwrap(), "m"), "1");
}

#[test]
fn loaded_says_when_a_document_is_done() {
    let mut d = Document::parse_scripted(
        b"<img src=a.png><script>var x = 1;</script>",
        "http://top.example/",
        None,
        Limits::default(),
        Some(ScriptLimits::default()),
    );
    let mut h = Host::new();
    assert!(!d.loaded());
    d.run(&mut h);
    // The page's load waits for its image.
    assert!(!d.loaded());
    let id = d.pending_resources()[0].kind;
    if let ResourceKind::Image(i) = id {
        d.supply_image(i, Some((1, 1)));
    }
    d.run(&mut h);
    assert!(d.loaded());
    let plain = Document::parse_str("<p>x", "http://top.example/");
    assert!(plain.loaded());
    let plain = Document::parse_str("<img src=a.png>", "http://top.example/");
    assert!(!plain.loaded());
}
