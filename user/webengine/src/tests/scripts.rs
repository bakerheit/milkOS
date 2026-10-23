//! Script text against the js-test fixture pages (tests/js at the top of the repo) and small
//! pages of its own: the engine runs real JavaScript through the bindings. Each test checks
//! the console lines the page prints (`JSTEST ...`), the DOM and what the page asked of the
//! app. Skipped (with a note) while the engine can't run scripts (its stub).

use alloc::string::String;
use alloc::vec::Vec;

use super::script::{Host, doc};
use crate::api::*;

const COMMON: &str = include_str!("../../../../tests/js/common.js");
const EXT: &str = include_str!("../../../../tests/js/ext.js");

/// Can the engine run a script at all?
fn engine_runs() -> bool {
    let mut r = jsengine::Realm::new(jsengine::Options::default());
    let ok = r.eval("1 + 1", "probe").is_ok_and(|v| v.as_number() == Some(2.0));
    if !ok {
        std::println!("(skipped: the JavaScript engine can't run scripts yet)");
    }
    ok
}

/// Parse a fixture, give it its external scripts, run until nothing is due (moving the
/// clock `steps` times by `dt` ms, with a frame each time), and hand back the console.
fn page(html: &str, h: &mut Host, steps: u32, dt: f64) -> (Document, Vec<String>) {
    let mut d = doc(html);
    let mut lines = Vec::new();
    for _ in 0..=steps {
        for u in d.pending_scripts() {
            let body = if u.ends_with("common.js") {
                Some(COMMON.as_bytes())
            } else if u.contains("ext.js") {
                Some(EXT.as_bytes())
            } else {
                None
            };
            d.supply_script(&u, body);
        }
        d.run(h);
        if d.wants_frame() {
            d.frame(h);
        }
        d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
        lines.extend(d.take_console().into_iter().map(|l| l.text));
        h.now += dt;
    }
    (d, lines)
}

fn has(lines: &[String], want: &str) -> bool {
    lines.iter().any(|l| l.contains(want))
}

/// The element `id`'s centre, for a click (viewport = document here: no scroll).
fn centre(d: &Document, id: &str) -> (i32, i32) {
    let r = d.page.rect_of_id(id).expect("laid out");
    (r.x + r.w / 2, r.y + r.h / 2)
}

#[test]
fn tabs_widget() {
    if !engine_runs() {
        return;
    }
    let mut h = Host::new();
    let (mut d, lines) = page(include_str!("../../../../tests/js/tabs.html"), &mut h, 1, 10.0);
    assert!(has(&lines, "JSTEST tabs ready"), "{lines:?}");
    let (x, y) = centre(&d, "t2");
    assert!(d.click(&mut h, x, y, (0, 0), Mods::default()));
    let c: Vec<String> = d.take_console().into_iter().map(|l| l.text).collect();
    assert!(has(&c, "JSTEST tabs shown p2 Cheese is made from milk"), "{c:?}");
    d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
    assert!(d.text().contains("Cheese is made"));
    assert!(!d.text().contains("Milk comes from cows"));
}

#[test]
fn todo_list() {
    if !engine_runs() {
        return;
    }
    let mut h = Host::new();
    let (mut d, lines) = page(include_str!("../../../../tests/js/todo.html"), &mut h, 2, 10.0);
    assert!(has(&lines, "JSTEST todo ready 3"), "{lines:?}");
    let field = d.controls().iter().position(|c| c.kind == ControlKind::Text).unwrap();
    let id = ControlId(field as u32);
    d.set_value(id, "Make cheese");
    d.input(&mut h, id);
    assert!(d.submit_form(&mut h, id, None).is_none(), "the submit is cancelled");
    let c: Vec<String> = d.take_console().into_iter().map(|l| l.text).collect();
    assert!(has(&c, "JSTEST todo items 4 last Make cheese"), "{c:?}");
    d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
    let (x, y) = centre(&d, "item0");
    d.click(&mut h, x, y, (0, 0), Mods::default());
    let c: Vec<String> = d.take_console().into_iter().map(|l| l.text).collect();
    assert!(has(&c, "JSTEST todo toggled 0 done"), "{c:?}");
    assert!(d.take_requests().is_empty());
}

#[test]
fn form_validation_blocks_submit() {
    if !engine_runs() {
        return;
    }
    let mut h = Host::new();
    let (mut d, lines) = page(include_str!("../../../../tests/js/form.html"), &mut h, 0, 0.0);
    assert!(has(&lines, "JSTEST form ready"), "{lines:?}");
    let id = ControlId(0);
    d.set_value(id, "nope");
    assert!(d.submit_form(&mut h, id, None).is_none());
    let c: Vec<String> = d.take_console().into_iter().map(|l| l.text).collect();
    assert!(has(&c, "JSTEST form blocked 1 'nope'"), "{c:?}");
    // (Made at run time: no e-mail-shaped string in the repo.)
    let addr = alloc::format!("milk{}cow.test", '@');
    d.set_value(id, &addr);
    let sub = d.submit_form(&mut h, id, None).expect("sent");
    assert_eq!(sub.url, "http://www.milk.test/thanks?email=milk%40cow.test");
}

#[test]
fn timers_and_microtasks_in_order() {
    if !engine_runs() {
        return;
    }
    let mut h = Host::new();
    let (_d, lines) = page(include_str!("../../../../tests/js/timer.html"), &mut h, 80, 10.0);
    let got: Vec<&String> = lines.iter().filter(|l| l.contains("JSTEST timer")).collect();
    assert_eq!(
        got,
        [
            "JSTEST timer microtask first",
            "JSTEST timer queued microtask",
            "JSTEST timer timeout milk!",
            "JSTEST timer five in-time"
        ],
        "{lines:?}"
    );
}

#[test]
fn animation_frames() {
    if !engine_runs() {
        return;
    }
    let mut h = Host::new();
    let (_d, lines) = page(include_str!("../../../../tests/js/raf.html"), &mut h, 60, 16.0);
    assert!(has(&lines, "JSTEST raf done 50 moved yes"), "{lines:?}");
}

#[test]
fn inner_html_and_noscript() {
    if !engine_runs() {
        return;
    }
    let mut h = Host::new();
    let (d, lines) = page(include_str!("../../../../tests/js/innerhtml.html"), &mut h, 0, 0.0);
    assert!(has(&lines, "JSTEST innerhtml rows 4 cells 6 total 16 ns hidden"), "{lines:?}");
    assert!(has(&lines, "JSTEST innerhtml outer <p id=\"after\">Added after.</p>"), "{lines:?}");
    assert!(d.text().contains("Cheese"));
    assert!(!d.text().contains("Scripts are off"));
}

#[test]
fn script_order() {
    if !engine_runs() {
        return;
    }
    let mut h = Host::new();
    let (_d, lines) = page(include_str!("../../../../tests/js/order.html"), &mut h, 2, 10.0);
    assert!(
        has(
            &lines,
            "JSTEST order inline1 loading, ext b loading, inline2, ext a interactive, DOMContentLoaded, load complete | written yes"
        ),
        "{lines:?}"
    );
    assert!(has(&lines, "module"), "{lines:?}");
}

#[test]
fn storage_and_cookies() {
    if !engine_runs() {
        return;
    }
    let mut h = Host::new();
    h.cookies = String::from("secret-not-really=0");
    let (_d, lines) = page(include_str!("../../../../tests/js/storage.html"), &mut h, 0, 0.0);
    assert!(has(&lines, "JSTEST storage visits 1 cookie set"), "{lines:?}");
    let (_d, lines) = page(include_str!("../../../../tests/js/storage.html"), &mut h, 0, 0.0);
    assert!(has(&lines, "JSTEST storage visits 2"), "{lines:?}");
}

#[test]
fn runaway_script_is_stopped() {
    if !engine_runs() {
        return;
    }
    let mut h = Host::new();
    h.tick = 5.0; // every read of the clock moves it 5 ms
    h.stop_after = Some(3);
    let (mut d, lines) = page(include_str!("../../../../tests/js/runaway.html"), &mut h, 40, 20.0);
    assert!(has(&lines, "JSTEST runaway starting"), "{lines:?}");
    assert!(h.slow_calls >= 3, "asked {} times", h.slow_calls);
    assert!(!d.scripting());
    assert!(d.script_stats().stopped);
    assert!(has(&lines, "The page's scripts were stopped"), "{lines:?}");
    assert!(!has(&lines, "JSTEST runaway never"));
    // Nothing runs any more.
    h.now += 10_000.0;
    assert!(!d.run(&mut h));
}

#[test]
fn errors_handlers_and_events_from_script() {
    if !engine_runs() {
        return;
    }
    let mut h = Host::new();
    let (mut d, lines) = page(
        "<button id=b onclick=\"this.textContent = 'clicked ' + event.type; console.log('handler', this.id)\">B</button>
         <div id=log></div>
         <script>
           nosuchthing();
         </script>
         <script>
           var out = [];
           var d = document.getElementById('log');
           d.addEventListener('ping', function (e) { out.push('bubble ' + e.detail.n); });
           document.addEventListener('ping', function (e) { out.push('doc ' + e.eventPhase); }, true);
           d.dispatchEvent(new CustomEvent('ping', { bubbles: true, detail: { n: 7 } }));
           class Thing extends Array { get last() { return this[this.length - 1]; } }
           var t = new Thing(); t.push(1, 2, 3);
           out.push('class ' + t.last);
           var p = document.createElement('p');
           p.setAttribute('data-x', '1');
           p.dataset.yValue = '2';
           out.push(p.outerHTML);
           out.push(JSON.stringify({ a: [1, 'b'], c: null }));
           out.push(typeof window.setTimeout + ' ' + (window === globalThis) + ' ' + (document.defaultView === window));
           out.push(String(document.querySelectorAll('button').length));
           console.log(out.join(' | '));
           Promise.reject(new Error('nobody handles this'));
         </script>",
        &mut h,
        1,
        10.0,
    );
    assert!(has(&lines, "Uncaught ReferenceError"), "{lines:?}");
    assert!(
        has(&lines, "doc 1 | bubble 7 | class 3 | <p data-x=\"1\" data-y-value=\"2\"></p> | {\"a\":[1,\"b\"],\"c\":null} | function true true | 1"),
        "{lines:?}"
    );
    assert!(has(&lines, "Uncaught (in promise)"), "{lines:?}");
    let (x, y) = centre(&d, "b");
    d.click(&mut h, x, y, (0, 0), Mods::default());
    let c: Vec<String> = d.take_console().into_iter().map(|l| l.text).collect();
    assert!(has(&c, "handler b"), "{c:?}");
    d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
    assert!(d.text().contains("clicked click"), "{}", d.text());
}

#[test]
fn hostile_scripts_stay_bounded() {
    if !engine_runs() {
        return;
    }
    let mut h = Host::new();
    // Deep recursion, a heap bomb, a DOM bomb, a listener storm, timers flood: each one is
    // refused or stopped, the page goes on (or stops), nothing panics.
    let (d, lines) = page(
        "<div id=x></div><script>
           function f(n) { return f(n + 1) + 1; }
           try { f(0); } catch (e) { console.log('deep ' + (e instanceof RangeError)); }
           try { for (var i = 0; i < 2000000; i++) document.body.appendChild(document.createElement('i')); }
           catch (e) { console.log('dom ' + e.name); }
           for (var j = 0; j < 20000; j++) setTimeout(function () {}, 1);
           console.log('timers queued');
         </script>
         <script>
           var a = []; while (true) a.push(new Array(100000).join('x'));
         </script>
         <script>console.log('after the bomb');</script>",
        &mut h,
        3,
        10.0,
    );
    assert!(has(&lines, "deep true"), "{lines:?}");
    assert!(has(&lines, "timers queued"), "{lines:?}");
    assert!(d.page.dom.live() <= Limits::default().max_nodes + 10);
    // The heap bomb ends the page's scripts.
    assert!(has(&lines, "out of memory") || has(&lines, "stopped"), "{lines:?}");
}
