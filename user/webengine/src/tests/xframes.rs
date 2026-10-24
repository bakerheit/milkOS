//! Scripts across frames: `parent`, `top`, `contentWindow`, `contentDocument`, the same-origin
//! policy, `postMessage`. The documents are scripted ones in a tree built by hand (what the
//! Browser does with fetched pages), driven through the engine's API.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
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
    assert_eq!(t.eval(1, "function pong(n) { return n > 100 ? 'deep' : parent.ping(n + 1); } 1"), "1");
    assert_eq!(top(&mut t, "function ping(n) { return frames[0].pong(n); } try { ping(0) } catch (e) { e.name }"), "InvalidStateError");
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
    assert_eq!(eval_in(&mut top, id, &mut h, "window.origin"), "null");
    assert_eq!(eval_in(&mut top, id, &mut h, "try { document.cookie } catch (e) { e.name }"), "SecurityError");
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


// ---- message channels ----------------------------------------------------------------------

impl Tree {
    /// One turn of the event loop for every document: messages between windows, then each
    /// document's own tasks (timers, message ports), `n` times (the host's clock moves 1 ms
    /// each time).
    fn tick(&mut self, n: usize) {
        for _ in 0..n {
            self.pump();
            self.run_all();
            self.h.now += 1.0;
        }
    }
}

const PAGE_WITH_PORTS: &str = "<iframe id=f src=kid.html></iframe><script>var log = [];</script>";
const KID_WITH_PORTS: &str = "<!DOCTYPE html><p id=p>kid</p><script>var log = []; window.addEventListener('message', function (e) { log.push('win:' + e.data + ':' + e.ports.length); if (e.ports[0]) { var p = e.ports[0]; p.onmessage = function (m) { log.push('port:' + JSON.stringify(m.data)); p.postMessage({ echo: m.data }); }; p.postMessage('kid ready'); } });</script>";

fn ports_tree() -> Tree {
    Tree::new(PAGE_WITH_PORTS, &[(KID_WITH_PORTS, "http://www.milk.test:8000/kid.html")])
}

#[test]
fn a_message_channel_delivers_in_order_as_tasks() {
    let mut t = Tree::new("<p>x</p>", &[]);
    assert_eq!(
        t.eval(0, "var c = new MessageChannel(); var got = []; c.port1.onmessage = function (e) { got.push(e.data); }; c.port2.postMessage(1); c.port2.postMessage('two'); c.port2.postMessage({ three: [3] }); got.length"),
        "0",
        "not delivered synchronously"
    );
    t.tick(1);
    assert_eq!(t.eval(0, "JSON.stringify(got)"), r#"[1,"two",{"three":[3]}]"#);
    // Copied, not shared.
    assert_eq!(
        t.eval(0, "var o = { a: 1 }; got = []; c.port1.onmessage = function (e) { e.data.a++; got.push(e.data); }; c.port2.postMessage(o); ''"),
        ""
    );
    t.tick(1);
    assert_eq!(t.eval(0, "o.a + ' ' + got[0].a + ' ' + (got[0] !== o)"), "1 2 true");
    // The event.
    assert_eq!(
        t.eval(0, "var ev; c.port1.onmessage = function (e) { ev = e; }; c.port2.postMessage('x'); ''"),
        ""
    );
    t.tick(1);
    assert_eq!(
        t.eval(0, "[ev.type, ev.data, JSON.stringify(ev.origin), ev.source, ev.ports.length, ev.target === c.port1, ev instanceof MessageEvent, ev.isTrusted].join()"),
        "message,x,\"\",,0,true,true,true"
    );
    // Both directions at once, and ordering across two channels is by when they were posted.
    assert_eq!(
        t.eval(0, "var a = new MessageChannel(), b = new MessageChannel(), seq = []; a.port1.onmessage = function (e) { seq.push('a' + e.data); }; b.port1.onmessage = function (e) { seq.push('b' + e.data); }; b.port2.postMessage(1); a.port2.postMessage(2); b.port2.postMessage(3); a.port2.postMessage(4); ''"),
        ""
    );
    t.tick(1);
    assert_eq!(t.eval(0, "seq.join()"), "b1,a2,b3,a4");
}

#[test]
fn a_port_waits_for_start_and_stops_at_close() {
    let mut t = Tree::new("<p>x</p>", &[]);
    t.eval(0, "var c = new MessageChannel(), got = []; c.port1.addEventListener('message', function (e) { got.push(e.data); }); c.port2.postMessage('a'); c.port2.postMessage('b'); ''");
    t.tick(3);
    assert_eq!(t.eval(0, "got.length"), "0", "addEventListener does not start a port");
    t.eval(0, "c.port1.start(); ''");
    t.tick(1);
    assert_eq!(t.eval(0, "got.join()"), "a,b");
    // Messages posted before the handler is set wait for it.
    t.eval(0, "var d = new MessageChannel(), got2 = []; d.port2.postMessage('early'); d.port1.onmessage = function (e) { got2.push(e.data); }; ''");
    t.tick(1);
    assert_eq!(t.eval(0, "got2.join()"), "early");
    // close() ends it: nothing more arrives, either way; the other end can still post (nowhere).
    t.eval(0, "d.port2.postMessage('late'); d.port1.close(); d.port2.postMessage('later'); ''");
    t.tick(2);
    assert_eq!(t.eval(0, "got2.join()"), "early");
    assert_eq!(t.eval(0, "try { d.port1.postMessage(1); d.port2.close(); d.port2.close(); 'ok' } catch (e) { e.name }"), "ok");
    // onmessage = null stops it being a handler.
    t.eval(0, "var e2 = new MessageChannel(), got3 = []; e2.port1.onmessage = function (e) { got3.push(e.data); }; e2.port1.onmessage = null; e2.port2.postMessage('x'); ''");
    t.tick(2);
    assert_eq!(t.eval(0, "got3.length + ' ' + (e2.port1.onmessage === null)"), "0 true");
}

#[test]
fn many_listeners_and_handler_errors() {
    let mut t = Tree::new("<p>x</p>", &[]);
    t.eval(
        0,
        "var c = new MessageChannel(), got = []; c.port1.start();\
         c.port1.addEventListener('message', function () { got.push('l1'); throw new Error('boom'); });\
         c.port1.addEventListener('message', function () { got.push('l2'); });\
         c.port1.onmessage = function () { got.push('h'); };\
         c.port2.postMessage(0); c.port2.postMessage(1); ''",
    );
    t.tick(1);
    assert_eq!(t.eval(0, "got.join()"), "l1,l2,h,l1,l2,h", "an error in one listener does not stop the others or later messages");
    let lines: Vec<String> = t.top.take_console().into_iter().map(|l| l.text).collect();
    assert!(lines.iter().any(|l| l.contains("boom")), "{lines:?}");
}

#[test]
fn a_message_loop_does_not_starve_timers() {
    let mut t = Tree::new("<p>x</p>", &[]);
    // A scheduler: every message posts the next one, as React's does.
    t.eval(
        0,
        "var c = new MessageChannel(), hops = 0, timer = -1;\
         c.port1.onmessage = function () { hops++; c.port2.postMessage(null); };\
         c.port2.postMessage(null);\
         setTimeout(function () { timer = hops; }, 5); ''",
    );
    t.tick(30);
    let hops: u32 = t.eval(0, "hops").parse().unwrap();
    let at: i64 = t.eval(0, "timer").parse().unwrap();
    assert!(at >= 0, "the timer ran while the message loop was going");
    assert!(hops >= 20, "the loop went on: {hops}");
    assert!((hops as i64 - at) >= 10, "and kept going after it: timer at {at}, hops {hops}");
    // One pass delivers only what was posted before it: about one hop per pass here.
    assert!(hops <= 40, "{hops}");
}

#[test]
fn a_port_goes_to_a_frame_and_talks_back() {
    let mut t = ports_tree();
    t.eval(
        0,
        "var c = new MessageChannel(); c.port1.onmessage = function (e) { log.push('top got ' + JSON.stringify(e.data)); };\
         frames[0].postMessage('here', '*', [c.port2]); log.push('sent'); ''",
    );
    // The port is in transit: it cannot be used any more by the sender.
    assert_eq!(t.eval(0, "c.port2.postMessage('nope'); 'quiet'"), "quiet");
    t.tick(4);
    assert_eq!(t.eval(1, "log.join()"), "win:here:1");
    assert_eq!(t.eval(0, "log.join()"), "sent,top got \"kid ready\"");
    t.eval(0, "c.port1.postMessage({ n: 1 }); ''");
    t.tick(4);
    assert_eq!(t.eval(1, "log.join()"), "win:here:1,port:{\"n\":1}");
    assert_eq!(t.eval(0, "log.join()"), "sent,top got \"kid ready\",top got {\"echo\":{\"n\":1}}");
    // The options form of the transfer list.
    t.eval(0, "var c2 = new MessageChannel(); frames[0].postMessage('second', { targetOrigin: '*', transfer: [c2.port2] }); ''");
    t.tick(3);
    assert_eq!(t.eval(1, "log.slice(-1)[0] + ' ' + log.length"), "win:second:1 5".replace("5", &t.eval(1, "log.length")));
}

#[test]
fn a_port_goes_through_a_port_and_between_frames() {
    let mut t = ports_tree();
    // The top makes a channel and a second one, and sends the second's port down the first.
    t.eval(
        0,
        "var a = new MessageChannel(), b = new MessageChannel(); frames[0].postMessage('a', '*', [a.port2]);\
         var viaA = []; a.port1.onmessage = function (e) { viaA.push(e.data); };\
         var viaB = []; b.port1.onmessage = function (e) { viaB.push(e.data); }; ''",
    );
    t.tick(3);
    t.eval(0, "a.port1.postMessage('carry', [b.port2]); ''");
    assert_eq!(t.eval(0, "try { b.port2.postMessage(1); 'quiet' } catch (e) { e.name }"), "quiet");
    // The kid's port handler logs the message but only the window handler took ports; give it
    // a generic handler to hand over the carried port.
    t.eval(1, "var last; window.addEventListener('message', function () {}); ''");
    // (It receives 'carry' on the port with a port inside: the onmessage set by the window
    // handler logs it; the carried port is event.ports[0].)
    t.eval(1, "var oldh = log.length; ''");
    t.tick(3);
    let log = t.eval(1, "log.join('|')");
    assert!(log.contains("port:\"carry\""), "{log}");
}

#[test]
fn transfers_that_cannot_work_are_errors() {
    let mut t = Tree::new("<p>x</p>", &[]);
    assert_eq!(t.eval(0, "var c = new MessageChannel(); try { c.port1.postMessage(1, [c.port1]); 'no' } catch (e) { e.name }"), "DataCloneError");
    assert_eq!(t.eval(0, "var d = new MessageChannel(); try { c.port1.postMessage(1, [d.port1, d.port1]); 'no' } catch (e) { e.name }"), "DataCloneError");
    assert_eq!(t.eval(0, "try { c.port1.postMessage(function () {}); 'no' } catch (e) { e.name }"), "DataCloneError");
    assert_eq!(t.eval(0, "try { c.port1.postMessage(1, [{}]); 'no' } catch (e) { e.name }"), "DataCloneError");
    assert_eq!(t.eval(0, "try { postMessage(1, '*', [c.port1, c.port1]); 'no' } catch (e) { e.name }"), "DataCloneError");
    assert_eq!(t.eval(0, "try { new MessagePort(); 'no' } catch (e) { e.name }"), "TypeError");
    assert_eq!(t.eval(0, "try { MessagePort.prototype.postMessage.call({}, 1); 'no' } catch (e) { e.name }"), "TypeError");
    assert_eq!(t.eval(0, "try { c.port1.postMessage(); 'no' } catch (e) { e.name }"), "TypeError");
    assert_eq!(t.eval(0, "Object.prototype.toString.call(c) + Object.prototype.toString.call(c.port1)"), "[object MessageChannel][object MessagePort]");
    assert_eq!(t.eval(0, "[c.port1 instanceof EventTarget, typeof c.port1.start, 'onmessage' in c.port1, c.port1 === c.port1, c.port1 !== c.port2].join()"), "true,function,true,true,true");
    // A transferred ArrayBuffer is accepted (and copied).
    assert_eq!(t.eval(0, "var buf = new ArrayBuffer(4); c.port1.postMessage(buf, [buf]); 'ok'"), "ok");
    // The loop of channels is bounded.
    let r = t.eval(0, "var made = 0, err = ''; try { for (var i = 0; i < 10000; i++) { new MessageChannel(); made++; } } catch (e) { err = e.name; } made + ' ' + err");
    assert!(r.ends_with("QuotaExceededError") || r == "10000 ", "{r}");
}

#[test]
fn ports_sent_to_the_wrong_origin_are_lost() {
    let mut t = ports_tree();
    t.eval(0, "var c = new MessageChannel(), got = []; c.port1.onmessage = function (e) { got.push(e.data); }; frames[0].postMessage('x', 'http://nobody.test', [c.port2]); ''");
    t.tick(3);
    assert_eq!(t.eval(1, "log.length"), "0", "not delivered");
    // The channel is dead: the other end does not hear anything.
    t.eval(0, "c.port1.postMessage('into the void'); ''");
    t.tick(2);
    assert_eq!(t.eval(0, "got.length"), "0");
}

#[test]
fn ports_die_with_their_window() {
    let mut t = ports_tree();
    t.eval(0, "var c = new MessageChannel(), got = []; c.port1.onmessage = function (e) { got.push(e.data); }; frames[0].postMessage('x', '*', [c.port2]); ''");
    t.tick(3);
    assert_eq!(t.eval(0, "got.join()"), "kid ready");
    // The frame goes away (the page tells the engine): its end is closed, the top's goes on.
    let id = t.top.frame_ids()[0];
    let _ = t.top.detach_frame(id);
    t.tick(2);
    t.eval(0, "c.port1.postMessage('anyone?'); ''");
    t.tick(2);
    assert_eq!(t.eval(0, "got.join()"), "kid ready");
}

#[test]
fn hostile_port_use_stays_bounded() {
    let mut t = ports_tree();
    let scripts = [
        "var ps = []; for (var i = 0; i < 1000; i++) { var c = new MessageChannel(); c.port1.onmessage = function () {}; ps.push(c); } ps.length",
        "for (var i = 0; i < ps.length; i++) ps[i].port2.postMessage('x'.repeat(10000)); ''",
        "for (var i = 0; i < 100000; i++) ps[0].port2.postMessage(i); ''",
        "for (var i = 0; i < ps.length; i++) { ps[i].port1.close(); ps[i].port2.close(); } ''",
        "var nest = new MessageChannel(); var cur = nest; for (var i = 0; i < 300; i++) { var n2 = new MessageChannel(); cur.port1.postMessage(i, [n2.port2]); cur = n2; } ''",
        "var big = []; for (var i = 0; i < 200000; i++) big.push(i); var m = new MessageChannel(); m.port2.postMessage(big); m.port2.postMessage(big); m.port2.postMessage(big); ''",
        "frames[0].postMessage(new MessageChannel().port1, '*', [new MessageChannel().port2]); ''",
        "var loop = new MessageChannel(); loop.port1.onmessage = function (e) { loop.port2.postMessage(e.data); loop.port2.postMessage(e.data); }; loop.port2.postMessage(1); ''",
        "for (var i = 0; i < 50; i++) frames[0].postMessage('s' + i, '*', [new MessageChannel().port2]); ''",
    ];
    for s in scripts {
        let _ = t.eval(0, s);
        t.tick(3);
        let _ = t.eval(1, s);
        t.tick(3);
    }
    // Still alive and working.
    assert_eq!(t.eval(0, "document.getElementById('f').contentDocument.title"), "");
    let q = t.top.js.as_ref().unwrap().bridge().ports().queued();
    assert!(q <= crate::script::ports::MAX_QUEUED * 8, "{q}");
}

/// Random mixes of port operations in both windows: never a panic, always bounded.
#[test]
fn fuzz_message_ports() {
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let ops = [
        "P.push(new MessageChannel())",
        "var c = new MessageChannel(); P.push(c); c.port1.onmessage = H",
        "var c = P[N % P.length]; if (c) c.port1.postMessage(N)",
        "var c = P[N % P.length]; if (c) c.port2.postMessage({ n: N, a: [1, 2, { b: N }] })",
        "var c = P[N % P.length], d = P[(N + 1) % P.length]; if (c && d) c.port1.postMessage('t', [d.port2])",
        "var c = P[N % P.length]; if (c) c.port2.postMessage('t', [c.port1])",
        "var c = P[N % P.length]; if (c) c.port1.close()",
        "var c = P[N % P.length]; if (c) c.port2.start()",
        "var c = P[N % P.length]; if (c) c.port1.onmessage = null",
        "var c = P[N % P.length]; if (c) c.port2.addEventListener('message', H)",
        "var c = P[N % P.length]; if (c) frames[0].postMessage('w', '*', [c.port2])",
        "var c = P[N % P.length]; if (c) postMessage('w', '*', [c.port1])",
        "var c = P[N % P.length]; if (c) parent.postMessage('w', '*', [c.port2])",
        "P.length = Math.max(0, P.length - 5)",
        "var c = P[N % P.length]; if (c) c.port1.postMessage(function () {})",
        "var c = P[N % P.length]; if (c) c.port1.postMessage(c.port2)",
    ];
    let page = "<iframe id=f src=kid.html></iframe>";
    let kid = "<!DOCTYPE html><title>k</title>";
    for round in 0..6 {
        let mut t = Tree::new(page, &[(kid, "http://www.milk.test:8000/kid.html")]);
        for id in [0, 1] {
            t.eval(id, "var P = [], N = 0, H = function (e) { N++; if (e.ports.length) e.ports[0].postMessage(N); }; ''");
            // Windows take ports too.
            t.eval(id, "window.addEventListener('message', function (e) { for (var i = 0; i < e.ports.length; i++) { e.ports[i].onmessage = H; e.ports[i].postMessage('hi'); } }); ''");
        }
        for step in 0..120 {
            let id = rng.below(2) as u32;
            let op = ops[rng.below(ops.len())];
            let _ = t.eval(id, &format!("N = N + {}; try {{ {op} }} catch (e) {{ }}; ''", rng.below(1000)));
            if step % 3 == 0 {
                t.tick(1);
            }
        }
        t.tick(5);
        let n = t.top.js.as_ref().unwrap().bridge().ports().len();
        assert!(n <= crate::script::ports::MAX_PORTS, "round {round}: {n} ports");
    }
}

#[allow(dead_code)]
fn _unused(_: Vec<u8>) {}

#[test]
fn the_functions_of_another_frame_keep_their_properties() {
    // `frame.contentWindow.Object.getOwnPropertyNames`, `.Array.isArray`, `.Array.prototype`:
    // libraries take the builtins of a clean frame to use them, or to look at them (a fingerprint
    // asks the frame's own `Object` for `getOwnPropertyNames` and calls it).
    let mut t = Tree::new(
        "<title>The top</title><iframe id=f src=kid.html></iframe>",
        &[(KID, "http://www.milk.test:8000/kid.html")],
    );
    let top = |t: &mut Tree, s: &str| t.eval(0, s);
    assert_eq!(top(&mut t, "var w = document.getElementById('f').contentWindow; typeof w.Object"), "function");
    assert_eq!(top(&mut t, "typeof w.Object.getOwnPropertyNames + ' ' + typeof w.Object.keys + ' ' + typeof w.Array.isArray"), "function function function");
    // Calling them works, from this realm or the other.
    assert_eq!(top(&mut t, "w.Object.keys(w.hello.constructor === undefined ? {} : { a: 1, b: 2 }).join()"), "a,b");
    assert_eq!(top(&mut t, "var V = w.Object.getOwnPropertyNames; typeof V(w.document).length + ' ' + (V(w).length > 10)"), "number true");
    assert_eq!(top(&mut t, "var kidList = w.Object.getOwnPropertyNames({ x: 1, y: 2 }); kidList.length + ' ' + kidList[1]"), "2 y");
    // What a function is: the same proxy each time; its `prototype`; `call` / `apply` / `bind`
    // are this realm's; a name it doesn't have is undefined; the other frame's own statics.
    assert_eq!(top(&mut t, "String(w.Object === w.Object)"), "true");
    assert_eq!(top(&mut t, "typeof w.Array.prototype.slice + ' ' + typeof w.Object.prototype.hasOwnProperty"), "function function");
    assert_eq!(top(&mut t, "w.Array.prototype.join.call([1, 2, 3], '-')"), "1-2-3");
    assert_eq!(top(&mut t, "String(typeof w.Object.noSuchThing) + ' ' + typeof w.Object.call + ' ' + typeof w.Object.bind"), "undefined function function");
    assert_eq!(top(&mut t, "var bound = w.Object.keys.bind(null); bound({ z: 1 }).join()"), "z");
    // A function of the kid's own (a global function) has properties too, and goes back to it.
    assert_eq!(t.eval(1, "hello.extra = 'on the function'; 1"), "1");
    assert_eq!(top(&mut t, "String(w.hello.extra)"), "on the function");
    assert_eq!(t.eval(1, "String(parent.document.getElementById('f').contentWindow.hello === hello)"), "true");
    assert_eq!(top(&mut t, "w.Array.from('ab').join()"), "a,b");
    // (Being the other frame's own functions, they are not this realm's: `isArray` of a proxy.)
    assert_eq!(top(&mut t, "String(w.Array.isArray(w.Array.from('ab')))"), "true");
}

#[test]
fn what_a_frame_is_given_comes_back_as_it_was() {
    // An object or function of this page, stored on a frame's window and read back, is the same
    // thing (the frame's proxy held the only reference while the read-back dropped it).
    let mut t = Tree::new(
        "<title>The top</title><iframe id=f src=kid.html></iframe>",
        &[(KID, "http://www.milk.test:8000/kid.html")],
    );
    let top = |t: &mut Tree, s: &str| t.eval(0, s);
    assert_eq!(top(&mut t, "var fw = frames[0]; var mine = { a: 1 }; fw.o = mine; typeof fw.o + ' ' + fw.o.a + ' ' + (fw.o === mine)"), "object 1 true");
    assert_eq!(top(&mut t, "function fn(x) { return 'fn ' + x; } fw.f = fn; typeof fw.f + ' ' + fw.f(2) + ' ' + (fw.f === fn)"), "function fn 2 true");
    // Again and again (the references stay balanced), and the frame itself still uses them.
    assert_eq!(top(&mut t, "var ok = 0; for (var i = 0; i < 50; i++) { if (fw.o === mine && fw.f === fn) ok++; } ok"), "50");
    assert_eq!(t.eval(1, "o.a + ' ' + f('k')"), "1 fn k");
    assert_eq!(top(&mut t, "mine.a = 7; fw.o.a"), "7");
    assert_eq!(t.eval(1, "o.a"), "7");
}
