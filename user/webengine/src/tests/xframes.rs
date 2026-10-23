//! Scripts across frames: `parent`, `top`, `contentWindow`, `contentDocument`, the same-origin
//! policy, `postMessage`. The documents are scripted ones in a tree built by hand (what the
//! Browser does with fetched pages), driven through the engine's API.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::script::Host;
use crate::api::*;
use crate::script::hub::FrameSec;

fn scripted(html: &str, url: &str) -> Document {
    let mut d = Document::parse_scripted(
        html.as_bytes(),
        url,
        Some("text/html"),
        Limits::default(),
        Some(ScriptLimits::default()),
    );
    d.layout(&Viewport::new(400, 300), &ApproxMeasurer);
    d
}

/// Evaluate `src` in frame `id`: its result as text, or "ERR name: message".
fn eval_in(top: &mut Document, id: FrameId, h: &mut Host, src: &str) -> String {
    let d = top.frame_doc_mut(id).expect("frame has a document");
    let js = d.js.as_mut().expect("scripts on");
    js.test_entry(&mut d.page, h, |_b, r| match r.eval(src, "test") {
        Ok(v) => r.to_string(v).unwrap_or_else(|_| String::from("?")),
        Err(e) => {
            let rep = r.report(&e);
            format!("ERR {rep}")
        }
    })
    .expect("not stopped")
}

struct Tree {
    top: Box<Document>,
    h: Host,
}

const TOP_URL: &str = "http://www.milk.test:8000/top.html";

impl Tree {
    /// A top page with the given body and one child per `(src, html, url)` attached in order.
    fn new(top_html: &str, kids: &[(&str, &str)]) -> Tree {
        let mut top = Box::new(scripted(top_html, TOP_URL));
        let slots = top.frame_slots();
        assert_eq!(slots.len(), kids.len(), "slots");
        for (s, (html, url)) in slots.iter().zip(kids) {
            let kid = scripted(html, url);
            assert!(top.attach_frame(s.id, kid, None));
        }
        let mut t = Tree { top, h: Host::new() };
        t.run_all();
        t
    }

    /// Run every document's scripts (the inline ones; nothing is fetched here).
    fn run_all(&mut self) {
        let mut ids = alloc::vec![FrameId::TOP];
        ids.extend(self.top.frame_ids());
        for id in ids {
            let d = self.top.frame_doc_mut(id).unwrap();
            d.run(&mut self.h);
        }
    }

    fn eval(&mut self, id: u32, src: &str) -> String {
        eval_in(&mut self.top, FrameId(id), &mut self.h, src)
    }

    /// Take what scripts posted and deliver it (what the Browser's tick does).
    fn pump(&mut self) -> usize {
        let mut n = 0;
        for _ in 0..10 {
            let msgs = self.top.take_all_messages();
            if msgs.is_empty() {
                break;
            }
            for (from, m) in msgs {
                if let Some((to, msg)) = self.top.route_message(from, &m) {
                    let d = self.top.frame_doc_mut(to).unwrap();
                    if d.deliver_message(&mut self.h, &msg) {
                        n += 1;
                    }
                }
            }
        }
        n
    }
}

const KID: &str = "<!DOCTYPE html><title>The kid</title><p id=p>kid text</p><button id=b onclick=\"this.textContent='clicked'\">btn</button>\
<script>var depth = 1; function hello(x) { return 'kid says ' + x; } var got = [];\
window.addEventListener('message', function (e) { got.push(e); });</script>";

#[test]
fn a_page_without_frames_is_its_own_parent() {
    let mut d = Box::new(scripted("<p>x", TOP_URL));
    let mut h = Host::new();
    let r = eval_in(&mut d, FrameId::TOP, &mut h, "[parent === window, top === window, frames.length, length, frameElement, window.frames[0]].join()");
    assert_eq!(r, "true,true,0,0,,");
}

#[test]
fn same_origin_frames_reach_each_other() {
    let mut t = Tree::new(
        "<title>The top</title><p id=note>top text</p><iframe id=f name=kid src=kid.html></iframe>",
        &[(KID, "http://www.milk.test:8000/kid.html")],
    );
    let top = |t: &mut Tree, s: &str| t.eval(0, s);
    let kid = |t: &mut Tree, s: &str| t.eval(1, s);
    assert_eq!(top(&mut t, "document.getElementById('f').contentDocument.title"), "The kid");
    assert_eq!(top(&mut t, "document.getElementById('f').contentDocument.getElementById('p').textContent"), "kid text");
    assert_eq!(top(&mut t, "document.getElementById('f').contentWindow.hello('x')"), "kid says x");
    assert_eq!(top(&mut t, "document.getElementById('f').contentWindow.depth"), "1");
    assert_eq!(top(&mut t, "frames.length + ' ' + length"), "1 1");
    assert_eq!(top(&mut t, "(frames[0] === document.getElementById('f').contentWindow) + ' ' + (frames.kid === frames[0])"), "true true");
    assert_eq!(top(&mut t, "String(document.getElementById('f').contentWindow.parent === window)"), "true");
    assert_eq!(top(&mut t, "String(document.getElementById('f').contentWindow.top === window)"), "true");
    // The same proxy every time.
    assert_eq!(top(&mut t, "var d1 = document.getElementById('f').contentDocument; String(d1 === document.getElementById('f').contentDocument)"), "true");
    // Writing across.
    assert_eq!(top(&mut t, "var kd = document.getElementById('f').contentDocument; kd.getElementById('p').textContent = 'changed by top'; kd.getElementById('p').textContent"), "changed by top");
    assert_eq!(kid(&mut t, "document.getElementById('p').textContent"), "changed by top");
    assert_eq!(top(&mut t, "var el = kd.createElement('i'); el.id = 'made'; kd.body.appendChild(el); kd.getElementById('made').tagName"), "I");
    assert_eq!(kid(&mut t, "document.getElementById('made').tagName"), "I");
    assert_eq!(top(&mut t, "document.getElementById('f').contentWindow.got = 5; String(document.getElementById('f').contentWindow.got)"), "5");
    assert_eq!(kid(&mut t, "String(got)"), "5");
    // A click made from outside runs the kid's own handler.
    assert_eq!(top(&mut t, "kd.getElementById('b').click(); kd.getElementById('b').textContent"), "clicked");
    // And from inside out.
    assert_eq!(kid(&mut t, "(parent !== window) + ' ' + (top === parent) + ' ' + parent.document.title + ' ' + frameElement.id + ' ' + window.name"), "true true The top f kid");
    assert_eq!(kid(&mut t, "parent.document.getElementById('note').textContent = 'kid was here'; parent.document.getElementById('note').textContent"), "kid was here");
    assert_eq!(top(&mut t, "document.getElementById('note').textContent"), "kid was here");
    assert_eq!(kid(&mut t, "String(parent.frames[0] === window) + ' ' + String(window.frames.length)"), "true 0");
    assert_eq!(kid(&mut t, "String(frameElement === parent.document.getElementById('f'))"), "true");
    // Errors thrown over there arrive here, with their names.
    assert_eq!(top(&mut t, "try { document.getElementById('f').contentWindow.nope(); } catch (e) { e.name }"), "TypeError");
    // Functions given to the other frame run when it calls them.
    assert_eq!(top(&mut t, "var seen = []; document.getElementById('f').contentWindow.hello = function (a) { seen.push(a); return 'top says ' + a; }; ''"), "");
    assert_eq!(kid(&mut t, "hello('y') + ' / ' + parent.seen.join()"), "top says y / y");
    // The parent calls the kid, which calls the parent back: nested, and it works.
    assert_eq!(top(&mut t, "document.getElementById('f').contentWindow.callBack = function () { return 'kid saw ' + parent.seen.length; }; ''"), "");
    assert_eq!(top(&mut t, "frames[0].callBack()"), "kid saw 1");
    // But not for ever.
    assert_eq!(top(&mut t, "function ping(n) { return frames[0].pong(n); } frames[0].pong = function (n) { return n > 100 ? 'deep' : parent.ping(n + 1); }; try { ping(0) } catch (e) { e.name }"), "InvalidStateError");
}

#[test]
fn listeners_across_frames_fire_when_events_happen() {
    let mut t = Tree::new(
        "<iframe id=f src=kid.html></iframe><script>var log = [];</script>",
        &[(KID, "http://www.milk.test:8000/kid.html")],
    );
    assert_eq!(
        t.eval(0, "var kd = document.getElementById('f').contentDocument; kd.getElementById('b').addEventListener('click', function (e) { log.push('top heard ' + e.type); }); ''"),
        ""
    );
    // The kid's button is clicked on its own (no one is up the stack).
    assert_eq!(t.eval(1, "document.getElementById('b').click(); document.getElementById('b').textContent"), "clicked");
    let c: Vec<String> = t.top.frame_doc_mut(FrameId(1)).unwrap().take_console().into_iter().map(|l| l.text).collect();
    assert_eq!(t.eval(0, "log.join()"), "top heard click", "{c:?}");
}

#[test]
fn other_origins_see_almost_nothing() {
    let mut t = Tree::new(
        "<iframe id=f src=kid.html></iframe><p id=note>mine</p>",
        &[(KID, "http://other.test:8000/kid.html")],
    );
    let top = |t: &mut Tree, s: &str| t.eval(0, s);
    assert_eq!(top(&mut t, "String(document.getElementById('f').contentDocument)"), "null");
    let w = "document.getElementById('f').contentWindow";
    assert_eq!(top(&mut t, &format!("try {{ {w}.document; 'no error' }} catch (e) {{ e.name }}")), "SecurityError");
    assert_eq!(top(&mut t, &format!("try {{ {w}.location.href; 'no error' }} catch (e) {{ e.name }}")), "SecurityError");
    assert_eq!(top(&mut t, &format!("try {{ {w}.hello('x'); 'no error' }} catch (e) {{ e.name }}")), "SecurityError");
    assert_eq!(top(&mut t, &format!("try {{ {w}.depth = 2; 'no error' }} catch (e) {{ e.name }}")), "SecurityError");
    assert_eq!(top(&mut t, &format!("try {{ {w}.name; 'no error' }} catch (e) {{ e.name }}")), "SecurityError");
    assert_eq!(top(&mut t, &format!("try {{ {w}.got; 'no error' }} catch (e) {{ e.name }}")), "SecurityError");
    // What is allowed.
    assert_eq!(top(&mut t, &format!("[typeof {w}.postMessage, {w}.closed, {w}.parent === window, {w}.top === window, {w}.length, typeof {w}.focus, {w}.window === {w}, {w}.self === {w}].join()")), "function,false,true,true,0,function,true,true");
    assert_eq!(top(&mut t, &format!("typeof {w}.location.replace")), "function");
    assert_eq!(top(&mut t, &format!("'depth' in {w}")), "false");
    // The other way.
    assert_eq!(t.eval(1, "try { parent.document; 'no error' } catch (e) { e.name }"), "SecurityError");
    assert_eq!(t.eval(1, "try { parent.document.body; 'no error' } catch (e) { e.name }"), "SecurityError");
    assert_eq!(t.eval(1, "(parent !== window) + ' ' + String(frameElement)"), "true null");
    assert_eq!(t.eval(1, "String(top === parent)"), "true");
    assert_eq!(t.eval(1, "try { top.location.href } catch (e) { e.name }"), "SecurityError");
    // Setting location from outside is a request in the caller's queue, for the host to judge.
    assert_eq!(top(&mut t, &format!("{w}.location = '/elsewhere.html'; 'ok'")), "ok");
    let reqs = t.top.take_requests();
    assert!(
        reqs.iter().any(|r| matches!(r, PageRequest::NavigateOther { target, url, .. } if *target == FrameId(1) && url == "http://www.milk.test:8000/elsewhere.html")),
        "{reqs:?}"
    );
    assert_eq!(t.eval(1, "top.location.href = 'http://www.milk.test:8000/away.html'; 'ok'"), "ok");
    let reqs = t.top.frame_doc_mut(FrameId(1)).unwrap().take_requests();
    assert!(
        reqs.iter().any(|r| matches!(r, PageRequest::NavigateOther { target, .. } if target.is_top())),
        "{reqs:?}"
    );
}

#[test]
fn an_opaque_origin_is_the_same_as_nobody() {
    let mut top = Box::new(scripted("<iframe id=f src=kid.html></iframe>", TOP_URL));
    let id = top.frame_slots()[0].id;
    let mut kid = scripted(KID, "http://www.milk.test:8000/kid.html");
    kid.set_frame_security(FrameSec {
        opaque: true,
        ..FrameSec::default()
    });
    assert!(top.attach_frame(id, kid, None));
    let mut h = Host::new();
    // Same address as the top page, but a sandbox without allow-same-origin: not the same origin.
    assert_eq!(eval_in(&mut top, FrameId::TOP, &mut h, "String(document.getElementById('f').contentDocument)"), "null");
    assert_eq!(eval_in(&mut top, FrameId::TOP, &mut h, "try { document.getElementById('f').contentWindow.depth } catch (e) { e.name }"), "SecurityError");
    assert_eq!(eval_in(&mut top, id, &mut h, "window.origin"), "http://www.milk.test:8000");
    assert_eq!(top.frame_origin(id).as_deref(), Some("null"));
    assert_eq!(top.frame_origin(FrameId::TOP).as_deref(), Some("http://www.milk.test:8000"));
}

fn post_page() -> (Tree, ) {
    (Tree::new(
        "<iframe id=f src=kid.html></iframe><script>var got = []; window.addEventListener('message', function (e) { got.push(e); });</script>",
        &[(KID, "http://www.milk.test:8000/kid.html")],
    ),)
}

#[test]
fn post_message_is_asynchronous_and_carries_origin_and_source() {
    let (mut t,) = post_page();
    assert_eq!(t.eval(0, "document.getElementById('f').contentWindow.postMessage('hi', '*'); String(frames[0].got.length)"), "0");
    assert_eq!(t.pump(), 1);
    assert_eq!(t.eval(1, "got.length + ' ' + got[0].data + ' ' + got[0].origin + ' ' + (got[0].source === parent) + ' ' + got[0].type + ' ' + got[0].isTrusted + ' ' + got[0].lastEventId + '|' + got[0].ports.length"), "1 hi http://www.milk.test:8000 true message true |0");
    // The other way, answered by the kid with the event's own source.
    assert_eq!(t.eval(1, "window.addEventListener('message', function (e) { e.source.postMessage('echo ' + e.data, e.origin); }); ''"), "");
    t.eval(0, "frames[0].postMessage('ping', '/')");
    t.pump();
    assert_eq!(t.eval(0, "got.map(function (e) { return e.data; }).join()"), "echo ping");
    assert_eq!(t.eval(0, "String(got[0].source === frames[0])"), "true");
}

#[test]
fn post_message_checks_the_target_origin() {
    let (mut t,) = post_page();
    for target in ["'http://nobody.test:8000'", "'https://www.milk.test:8000'", "'http://www.milk.test:8001'", "'http://www.milk.test'"] {
        t.eval(0, &format!("frames[0].postMessage('wrong', {target})"));
    }
    assert_eq!(t.pump(), 0);
    for target in ["'*'", "'/'", "'http://www.milk.test:8000'", "'http://WWW.milk.test:8000/some/path'", "{ targetOrigin: 'http://www.milk.test:8000' }", "{ targetOrigin: '*' }"] {
        t.eval(0, &format!("frames[0].postMessage('right', {target})"));
    }
    assert_eq!(t.pump(), 6);
    assert_eq!(t.eval(1, "got.length"), "6");
    // Bad target origins are a SyntaxError in the sender.
    assert_eq!(t.eval(0, "try { frames[0].postMessage('x', 'not an origin') } catch (e) { e.name }"), "SyntaxError");
    // A message to nothing is dropped.
    t.eval(0, "window.postMessage('to myself', '*')");
    assert_eq!(t.pump(), 1);
    assert_eq!(t.eval(0, "got[got.length - 1].data + ' ' + (got[got.length - 1].source === window)"), "to myself true");
}

#[test]
fn post_message_to_another_origin_and_from_it() {
    let mut t = Tree::new(
        "<iframe id=f src=kid.html></iframe><script>var got = []; window.addEventListener('message', function (e) { got.push(e.data + '@' + e.origin); });</script>",
        &[(KID, "http://other.test:8000/kid.html")],
    );
    t.eval(0, "frames[0].postMessage('secret', 'http://www.milk.test:8000')"); // wrong target
    t.eval(0, "frames[0].postMessage('hello', 'http://other.test:8000')");
    assert_eq!(t.pump(), 1);
    assert_eq!(t.eval(1, "got[0].data + '@' + got[0].origin"), "hello@http://www.milk.test:8000");
    t.eval(1, "parent.postMessage('from other', '*')");
    t.eval(1, "parent.postMessage('refused', 'http://other.test:8000')");
    assert_eq!(t.pump(), 1);
    assert_eq!(t.eval(0, "got.join()"), "from other@http://other.test:8000");
    // A sandboxed (opaque) sender says "null"; only "*" reaches an opaque receiver.
    let mut top = Box::new(scripted("<iframe id=f src=kid.html></iframe><script>var got = []; window.addEventListener('message', function (e) { got.push(e.data + '@' + e.origin); });</script>", TOP_URL));
    let id = top.frame_slots()[0].id;
    let mut kid = scripted(KID, "http://www.milk.test:8000/kid.html");
    kid.set_frame_security(FrameSec { opaque: true, ..FrameSec::default() });
    top.attach_frame(id, kid, None);
    let mut t = Tree { top, h: Host::new() };
    t.run_all();
    t.eval(1, "parent.postMessage('from sandbox', '*')");
    t.eval(0, "frames[0].postMessage('to sandbox, named', 'http://www.milk.test:8000')");
    t.eval(0, "frames[0].postMessage('to sandbox, star', '*')");
    assert_eq!(t.pump(), 2);
    assert_eq!(t.eval(0, "got.join()"), "from sandbox@null");
    assert_eq!(t.eval(1, "got.map(function (e) { return e.data; }).join()"), "to sandbox, star");
}

#[test]
fn post_message_copies_what_structured_clone_copies() {
    let (mut t,) = post_page();
    t.eval(
        0,
        "var cyc = { name: 'c' }; cyc.self = cyc; var shared = [1]; \
         frames[0].postMessage({ n: 42, s: 'str', arr: [1, 'two', [3]], nested: { a: { b: true } }, nul: null, undef: undefined, \
         d: new Date(86400000), re: /a+b/gi, m: new Map([['k', 1]]), st: new Set([1, 2]), buf: new Uint8Array([1, 2, 3]).buffer, \
         nan: NaN, inf: -Infinity, negz: -0, big: 1.5e300, u8: new Uint8Array([9, 8]), err: new TypeError('boom'), \
         a1: shared, a2: shared, cyc: cyc }, '*')",
    );
    assert_eq!(t.pump(), 1);
    let check = |t: &mut Tree, s: &str| t.eval(1, &format!("var d = got[0].data; {s}"));
    assert_eq!(check(&mut t, "d.n + d.s + JSON.stringify(d.arr) + JSON.stringify(d.nested) + d.nul + ('undef' in d) + d.undef"), "42str[1,\"two\",[3]]{\"a\":{\"b\":true}}nulltrueundefined");
    assert_eq!(check(&mut t, "(d.d instanceof Date) + ':' + d.d.getTime() + ' ' + (d.re instanceof RegExp) + ':' + d.re.source + '/' + d.re.flags"), "true:86400000 true:a+b/gi");
    assert_eq!(check(&mut t, "(d.m instanceof Map) + ':' + d.m.get('k') + ' ' + (d.st instanceof Set) + ':' + d.st.size + ' ' + d.st.has(2)"), "true:1 true:2 true");
    assert_eq!(check(&mut t, "(d.buf instanceof ArrayBuffer) + ':' + d.buf.byteLength + ':' + new Uint8Array(d.buf)[2]"), "true:3:3");
    assert_eq!(check(&mut t, "isNaN(d.nan) + ' ' + d.inf + ' ' + (1 / d.negz) + ' ' + d.big"), "true -Infinity -Infinity 1.5e+300");
    assert_eq!(check(&mut t, "(d.u8 instanceof Uint8Array) + ':' + d.u8.join()"), "true:9,8");
    assert_eq!(check(&mut t, "d.err.name + ':' + d.err.message"), "TypeError:boom");
    assert_eq!(check(&mut t, "(d.a1 === d.a2) + ' ' + (d.cyc.self === d.cyc) + ' ' + d.cyc.name"), "true true c");
    // What cannot be copied throws in the sender.
    for (what, name) in [
        ("function () {}", "DataCloneError"),
        ("{ f: function () {} }", "DataCloneError"),
        ("Symbol('s')", "DataCloneError"),
        ("document.body", "DataCloneError"),
        ("window", "DataCloneError"),
        ("frames[0]", "DataCloneError"),
    ] {
        assert_eq!(t.eval(0, &format!("try {{ frames[0].postMessage({what}, '*'); 'sent' }} catch (e) {{ e.name }}")), name, "{what}");
    }
    // Too deep.
    assert_eq!(t.eval(0, "var deep = []; for (var i = 0; i < 300; i++) deep = [deep]; try { frames[0].postMessage(deep, '*'); 'sent' } catch (e) { e.name }"), "DataCloneError");
    // The queue is bounded.
    t.eval(0, "for (var i = 0; i < 5000; i++) frames[0].postMessage(i, '*')");
    let n = t.top.take_all_messages().len();
    assert!(n <= 2000, "{n}");
}

#[test]
fn frames_that_go_away_are_dead_but_do_not_crash_anything() {
    let mut t = Tree::new(
        "<iframe id=f src=kid.html></iframe>",
        &[(KID, "http://www.milk.test:8000/kid.html")],
    );
    t.eval(0, "var w = frames[0]; var d = w.document; var p = d.getElementById('p'); ''");
    let id = FrameId(1);
    assert!(t.top.detach_frame(id).is_some());
    assert_eq!(t.eval(0, "try { p.textContent } catch (e) { e.name }"), "InvalidAccessError");
    assert_eq!(t.eval(0, "String(w.closed) + ' ' + w.length + ' ' + String(w.parent)"), "true 0 null");
    assert_eq!(t.eval(0, "String(document.getElementById('f').contentWindow)"), "null");
    // A new document in the slot is a new realm: the old proxies stay dead.
    let kid = scripted(KID, "http://www.milk.test:8000/kid2.html");
    assert!(t.top.attach_frame(id, kid, None));
    assert_eq!(t.eval(0, "try { p.textContent } catch (e) { e.name }"), "InvalidAccessError");
    assert_eq!(t.eval(0, "frames[0].document.title"), "The kid");
    // postMessage to a dead window is dropped.
    t.eval(0, "w.postMessage('to the dead', '*')");
    assert_eq!(t.pump(), 0);
}

#[test]
fn hostile_scripts_across_frames_do_not_panic() {
    let mut t = Tree::new(
        "<iframe id=f src=kid.html></iframe><iframe id=g src=kid.html></iframe>",
        &[(KID, "http://www.milk.test:8000/kid.html"), (KID, "http://other.test:8000/kid.html")],
    );
    let scripts = [
        "for (var k in frames[0]) {} ''",
        "Object.keys(frames[0]).length > 0",
        "JSON.stringify(frames[0].document.body.children.length)",
        "frames[0].document.body.appendChild(frames[0].document.createElement('b')); ''",
        "frames[0].document.body.appendChild(document.createElement('b'))",
        "frames[1].document",
        "frames[1].location.href",
        "delete frames[0].got; ''",
        "frames[0].got = frames[0]; ''",
        "frames[0].hello.call(frames[1], 1)",
        "frames[0].hello.apply(null, new Array(100000))",
        "new frames[0].Date().getTime() > 0",
        "frames[0].eval('1+1')",
        "frames[0].setTimeout('1', 0)",
        "[frames[0], frames[1], parent, top, self].map(String).join()",
        "Object.getOwnPropertyNames(frames[1]).length",
        "frames[0].frames[0]",
        "frames[5]",
        "frames['nope']",
        "frames[0][Symbol.iterator]",
        "for (var i = 0; i < 3000; i++) { frames[0].document.getElementById('p'); } ''",
        "frames[0].postMessage(frames[0].document, '*')",
        "frames[0].postMessage()",
    ];
    for s in scripts {
        let _ = t.eval(0, s);
        let _ = t.eval(1, s);
        let _ = t.eval(2, s);
    }
    t.pump();
    // Still alive.
    assert_eq!(t.eval(0, "document.getElementById('f').contentDocument.title"), "The kid");
}

#[allow(dead_code)]
fn _unused(_: Vec<u8>) {}
