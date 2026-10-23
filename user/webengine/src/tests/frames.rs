//! Frames: slots, attach/detach, and (below) layout, painting, hit testing, scrolling, focus
//! and find across them.

use alloc::string::String;
use alloc::vec::Vec;

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
