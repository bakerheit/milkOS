//! How a page's scripts load and run, checked against the HTML standard ("The script element",
//! "Scripting" and the end of the parser's tree construction): the order of parser-blocking,
//! `defer` and `async` scripts however their bodies arrive, scripts that throw, scripts that do
//! not compile, `document.readyState` and the events, scripts a page inserts, and the
//! functions an inline script finds (or does not) from the scripts before it.
//!
//! Each page logs `T name value` lines. External scripts arrive when the test says (a tick
//! number; a script may also fail), so the order they come in can differ from the order of the page.

use alloc::string::String;
use alloc::vec::Vec;

use super::script::{Host, doc};
use crate::api::*;

pub enum Arrive {
    /// The body comes at this tick.
    At(u32),
    /// The fetch fails at this tick.
    Fails(u32),
}

/// Run `html` (its page is http://www.milk.test/page.html), supplying external scripts from
/// `files` (path, body, arrival) as their turn comes, for `ticks` ticks. The `T ` lines it logged.
pub fn load(html: &str, files: &[(&str, &str, Arrive)], ticks: u32) -> Vec<String> {
    let mut d = doc(html);
    let mut h = Host::new();
    let mut lines: Vec<String> = Vec::new();
    let mut supplied: Vec<String> = Vec::new();
    for tick in 0..ticks {
        d.run(&mut h);
        for url in d.pending_scripts() {
            if supplied.contains(&url) {
                continue;
            }
            let Some((_, body, arrive)) = files
                .iter()
                .find(|(p, ..)| alloc::format!("http://www.milk.test{p}") == url)
            else {
                // A file the test did not list: it is not there.
                supplied.push(url.clone());
                d.supply_script(&url, None);
                continue;
            };
            let due = match arrive {
                Arrive::At(t) | Arrive::Fails(t) => *t,
            };
            if tick >= due {
                supplied.push(url.clone());
                match arrive {
                    Arrive::At(_) => d.supply_script(&url, Some(body.as_bytes())),
                    Arrive::Fails(_) => d.supply_script(&url, None),
                }
                d.run(&mut h);
            }
        }
        lines.extend(d.take_console().into_iter().map(|l| l.text));
        h.now += 10.0;
    }
    lines
}

fn t(lines: &[String]) -> Vec<&str> {
    lines.iter().filter_map(|l| l.strip_prefix("T ")).collect()
}

fn page(body: &str) -> String {
    alloc::format!("<!DOCTYPE html><html><head><title>t</title></head><body>{body}</body></html>")
}

#[test]
fn blocking_scripts_run_in_page_order_whatever_order_they_arrive_in() {
    let html = page(
        "<script src=/a.js></script><script>console.log('T inline1')</script><script src=/b.js></script>\
         <script>console.log('T inline2 ' + typeof fa + ' ' + typeof fb)</script>",
    );
    // b.js comes first, a.js last: a still runs first.
    let l = load(
        &html,
        &[
            ("/a.js", "function fa() {} console.log('T a');", Arrive::At(6)),
            ("/b.js", "function fb() {} console.log('T b');", Arrive::At(1)),
        ],
        20,
    );
    assert_eq!(t(&l), ["a", "inline1", "b", "inline2 function function"], "{l:?}");
}

#[test]
fn a_script_that_throws_still_lets_the_later_ones_run_and_keeps_what_it_defined() {
    let html = page(
        "<script src=/boom.js></script>\
         <script>console.log('T after ' + typeof before + ' ' + typeof late + ' ' + typeof hoisted + ' ' + typeof hoistedLater + ' ' + (typeof v) + ' ' + (typeof w))</script>\
         <script>throw new Error('inline boom')</script>\
         <script>console.log('T still running')</script>",
    );
    let l = load(
        &html,
        &[(
            "/boom.js",
            "var before = 1; function hoisted() {} console.log('T boom starts'); throw new TypeError('x'); var v = 2; function hoistedLater() {} var late = 3;",
            Arrive::At(2),
        )],
        20,
    );
    let t = t(&l);
    // `before` and the function declarations are defined; what comes after the throw is not
    // executed (`var v` is hoisted, so it exists as undefined; `late` is hoisted too).
    assert_eq!(
        t,
        ["boom starts", "after number undefined function function undefined undefined", "still running"],
        "{l:?}"
    );
    assert!(l.iter().any(|x| x.contains("Uncaught TypeError")), "{l:?}");
    assert!(l.iter().any(|x| x.contains("inline boom")), "{l:?}");
}

#[test]
fn a_script_that_does_not_compile_defines_nothing_and_the_rest_run() {
    // A syntax error anywhere in a script stops all of it, even the function declarations
    // above the error and the statements before it.
    let html = page(
        "<script src=/bad.js></script>\
         <script>console.log('T after ' + typeof good + ' ' + typeof x + ' ' + (typeof side))</script>\
         <script>var ok = 1; function broken( { }</script>\
         <script>console.log('T next ' + typeof ok)</script>",
    );
    let l = load(
        &html,
        &[("/bad.js", "function good() {}\nvar x = 1;\nside = 1;\nvar = ;\n", Arrive::At(1))],
        20,
    );
    assert_eq!(t(&l), ["after undefined undefined undefined", "next undefined"], "{l:?}");
    assert!(l.iter().any(|x| x.contains("SyntaxError") && x.contains("bad.js")), "{l:?}");
}

#[test]
fn a_script_that_fails_to_load_does_not_stop_the_page() {
    let html = page(
        "<script src=/gone.js onerror=\"console.log('T onerror ' + typeof window.gone)\" onload=\"console.log('T onload?!')\"></script>\
         <script>console.log('T next')</script><script src=/ok.js onload=\"console.log('T ok loaded ' + window.fine)\"></script>",
    );
    let l = load(
        &html,
        &[("/gone.js", "", Arrive::Fails(3)), ("/ok.js", "var fine = 'yes';", Arrive::At(1))],
        20,
    );
    assert_eq!(t(&l), ["onerror undefined", "next", "ok loaded yes"], "{l:?}");
}

#[test]
fn defer_scripts_run_in_order_after_the_parser_and_before_domcontentloaded() {
    let html = page(
        "<script defer src=/d1.js></script><script defer src=/d2.js></script>\
         <script>console.log('T inline ' + document.readyState + ' ' + typeof d1)</script>\
         <script>document.addEventListener('DOMContentLoaded', () => console.log('T DCL ' + document.readyState + ' ' + typeof d1 + typeof d2));\
                 document.addEventListener('readystatechange', () => console.log('T rsc ' + document.readyState));\
                 addEventListener('load', () => console.log('T load ' + document.readyState));</script>",
    );
    // d2 arrives first.
    let l = load(
        &html,
        &[
            ("/d1.js", "var d1 = 1; console.log('T d1 ' + document.readyState);", Arrive::At(8)),
            ("/d2.js", "var d2 = 2; console.log('T d2 ' + document.readyState + ' ' + typeof d1);", Arrive::At(2)),
        ],
        30,
    );
    assert_eq!(
        t(&l),
        [
            "inline loading undefined",
            "rsc interactive",
            "d1 interactive",
            "d2 interactive number",
            "DCL interactive numbernumber",
            "rsc complete",
            "load complete"
        ],
        "{l:?}"
    );
}

#[test]
fn async_scripts_run_when_they_are_ready_and_do_not_hold_up_the_parser_or_domcontentloaded() {
    let html = page(
        "<script async src=/a1.js></script><script async src=/a2.js></script>\
         <script defer src=/d.js></script>\
         <script>document.addEventListener('DOMContentLoaded', () => console.log('T DCL'));\
                 addEventListener('load', () => console.log('T load'));\
                 console.log('T inline');</script>",
    );
    // a2 comes first and runs first; a1 arrives late, after DOMContentLoaded but before load.
    let l = load(
        &html,
        &[
            ("/a1.js", "console.log('T a1');", Arrive::At(10)),
            ("/a2.js", "console.log('T a2');", Arrive::At(1)),
            ("/d.js", "console.log('T d');", Arrive::At(3)),
        ],
        40,
    );
    let t = t(&l);
    assert_eq!(t.first(), Some(&"inline"), "{l:?}");
    let pos = |w: &str| t.iter().position(|x| *x == w).unwrap_or_else(|| panic!("{w} missing in {l:?}"));
    assert!(pos("a2") < pos("a1"), "{l:?}");
    assert!(pos("d") < pos("DCL"), "{l:?}");
    assert!(pos("DCL") < pos("a1"), "async a1 arrived after DOMContentLoaded: {l:?}");
    assert!(pos("a1") < pos("load"), "load waits for async scripts: {l:?}");
}

#[test]
fn an_inline_script_calling_what_a_later_kind_of_script_defines_gets_a_reference_error() {
    // The shape of a page whose inline script runs before a script it depends on: the function
    // comes from a deferred script, an async one that is not in yet, and a blocking one before it.
    let html = page(
        "<script src=/blocking.js></script>\
         <script defer src=/deferred.js></script>\
         <script async src=/asynced.js></script>\
         <script nonce=\"abc123\">\
           var r = [];\
           for (const n of ['fromBlocking', 'fromDeferred', 'fromAsync']) { try { (0, eval)(n + '()'); r.push(n + ':ok'); } catch (e) { r.push(n + ':' + e.name); } }\
           console.log('T ' + r.join(' '));\
           try { solveSimpleChallenge(); } catch (e) { console.log('T solve ' + e.name + ': ' + e.message); }\
         </script>\
         <script nonce=\"abc123\">addEventListener('load', () => { try { fromDeferred(); fromAsync(); console.log('T at load: both defined'); } catch (e) { console.log('T at load ' + e.message); } });</script>",
    );
    let l = load(
        &html,
        &[
            ("/blocking.js", "function fromBlocking() {}", Arrive::At(1)),
            ("/deferred.js", "function fromDeferred() {}", Arrive::At(1)),
            ("/asynced.js", "function fromAsync() {}", Arrive::At(6)),
        ],
        30,
    );
    assert_eq!(
        t(&l),
        [
            "fromBlocking:ok fromDeferred:ReferenceError fromAsync:ReferenceError",
            "solve ReferenceError: solveSimpleChallenge is not defined",
            "at load: both defined"
        ],
        "{l:?}"
    );
}

#[test]
fn current_script_and_ready_state_in_scripts() {
    let html = page(
        "<script id=s1>console.log('T s1 ' + (document.currentScript && document.currentScript.id) + ' ' + document.readyState)</script>\
         <script id=s2 src=/ext.js></script>\
         <script id=s3 defer src=/def.js></script>\
         <script>setTimeout(() => console.log('T timer ' + String(document.currentScript)), 1)</script>",
    );
    let l = load(
        &html,
        &[
            ("/ext.js", "console.log('T ext ' + document.currentScript.id + ' ' + document.currentScript.getAttribute('src'));", Arrive::At(1)),
            ("/def.js", "console.log('T def ' + document.currentScript.id + ' ' + document.readyState);", Arrive::At(1)),
        ],
        20,
    );
    assert_eq!(t(&l), ["s1 s1 loading", "ext s2 /ext.js", "def s3 interactive", "timer null"], "{l:?}");
}

#[test]
fn scripts_a_page_inserts() {
    let html = page(
        "<script>\
           var log = (s) => console.log('T ' + s);\
           /* an inline one runs at once, inside the insertion */\
           var i = document.createElement('script'); i.textContent = \"log('inserted inline runs')\"; document.body.appendChild(i); log('after inline insert');\
           /* external ones are async unless async=false */\
           var a = document.createElement('script'); a.src = '/slow.js'; document.body.appendChild(a);\
           var b = document.createElement('script'); b.src = '/fast.js'; document.body.appendChild(b);\
           var c = document.createElement('script'); c.src = '/o1.js'; c.async = false; document.body.appendChild(c);\
           var d = document.createElement('script'); d.src = '/o2.js'; d.async = false; document.body.appendChild(d);\
           /* a script with no text and no src is not run until it has some */\
           var e = document.createElement('script'); document.body.appendChild(e); e.textContent = \"log('late text runs once')\";\
           log('end of the inserting script');\
         </script>",
    );
    let l = load(
        &html,
        &[
            ("/slow.js", "console.log('T slow');", Arrive::At(8)),
            ("/fast.js", "console.log('T fast');", Arrive::At(2)),
            ("/o1.js", "console.log('T o1');", Arrive::At(9)),
            ("/o2.js", "console.log('T o2');", Arrive::At(3)),
        ],
        40,
    );
    let t = t(&l);
    assert_eq!(&t[..3], ["inserted inline runs", "after inline insert", "end of the inserting script"], "{l:?}");
    let pos = |w: &str| t.iter().position(|x| *x == w).unwrap_or_else(|| panic!("{w} missing in {l:?}"));
    assert!(pos("fast") < pos("slow"), "async ones run as they come: {l:?}");
    assert!(pos("o1") < pos("o2"), "async=false ones keep their order though o2 came first: {l:?}");
}

#[test]
fn a_failing_dynamic_script_fires_error_and_nothing_else_stops() {
    let html = page(
        "<script>\
           var s = document.createElement('script'); s.src = '/missing.js';\
           s.onerror = () => console.log('T error event');\
           s.onload = () => console.log('T load event?!');\
           document.body.appendChild(s);\
           var g = document.createElement('script'); g.src = '/good.js'; g.onload = () => console.log('T good load event ' + window.good);\
           document.body.appendChild(g);\
         </script>",
    );
    let l = load(
        &html,
        &[("/missing.js", "", Arrive::Fails(2)), ("/good.js", "var good = 1;", Arrive::At(3))],
        20,
    );
    assert_eq!(t(&l), ["error event", "good load event 1"], "{l:?}");
}

#[test]
fn errors_name_the_script_that_threw_and_its_position() {
    let html = page("<script src=/lines.js></script><script>\n\nnull.inline;</script>");
    let l = load(&html, &[("/lines.js", "// one\nvar x = 1;\n  undefinedFunction();\n", Arrive::At(1))], 10);
    assert!(
        l.iter().any(|x| x.contains("Uncaught ReferenceError: undefinedFunction is not defined") && x.contains("lines.js:3:3")),
        "{l:?}"
    );
    assert!(l.iter().any(|x| x.contains("TypeError") && x.contains("page.html:3")), "{l:?}");
}

#[test]
fn window_onerror_hears_each_error_with_its_file_and_line_and_the_rest_runs() {
    let html = page(
        "<script>onerror = (m, f, l, c, e) => { console.log('T onerror ' + m + ' | ' + f.replace('http://www.milk.test', '') + ':' + l + ':' + c + ' | ' + (e && e.name)); return true; };</script>\
         <script src=/err.js></script><script>console.log('T next')</script>",
    );
    let l = load(&html, &[("/err.js", "\n\nthrow new RangeError('far');", Arrive::At(1))], 10);
    assert_eq!(t(&l), ["onerror Uncaught RangeError: far | /err.js:3:7 | RangeError", "next"], "{l:?}");
}

#[test]
fn scripts_see_the_whole_page_even_the_part_after_them() {
    // Known difference from a browser: the page is parsed whole before any script runs, so an
    // inline script sees elements that come after it (a browser's parser has not got there).
    // This test records what is, so a change either way is noticed.
    let html = page("<script>console.log('T later ' + (document.getElementById('later') ? 'visible' : 'not there yet'))</script><div id=later></div>");
    let l = load(&html, &[], 5);
    assert_eq!(t(&l), ["later visible"], "{l:?}");
}

#[test]
fn markup_written_by_a_script_runs_after_it_and_before_the_next_script() {
    // document.write of scripts (the way tag managers and old ad code load more code): the written
    // scripts run after the script that wrote them finishes, in order, before the page's next script.
    let html = page(
        "<script>document.write('<script src=\"/w.js\"><\\/script><script>console.log(\"T written inline\")<\\/script>'); console.log('T after write')</script>\
         <script>console.log('T next ' + typeof w)</script>",
    );
    let l = load(&html, &[("/w.js", "var w = 1; console.log('T w.js');", Arrive::At(3))], 20);
    assert_eq!(t(&l), ["after write", "w.js", "written inline", "next number"], "{l:?}");
}

#[test]
fn the_same_script_twice_runs_twice_and_a_failed_defer_does_not_hold_up_the_next() {
    let html = page(
        "<script src=/twice.js></script><script src=/twice.js></script>\
         <script defer src=/lost.js></script><script defer src=/d2.js></script>\
         <script>addEventListener('DOMContentLoaded', () => console.log('T dcl'))</script>",
    );
    let l = load(
        &html,
        &[
            ("/twice.js", "console.log('T twice');", Arrive::At(2)),
            ("/lost.js", "", Arrive::Fails(1)),
            ("/d2.js", "console.log('T d2');", Arrive::At(4)),
        ],
        20,
    );
    assert_eq!(t(&l), ["twice", "twice", "d2", "dcl"], "{l:?}");
}

#[test]
fn src_wins_over_text_async_means_nothing_inline_and_an_empty_src_is_an_error() {
    let html = page(
        "<script src=/e.js>console.log('T text of a script with a src: never')</script>\
         <script async>console.log('T inline async runs in order')</script>\
         <script src=\"\" onerror=\"console.log('T empty src errors')\">console.log('T never either')</script>\
         <script>console.log('T last')</script>",
    );
    let l = load(&html, &[("/e.js", "console.log('T e.js');", Arrive::At(1))], 10);
    assert_eq!(t(&l), ["e.js", "inline async runs in order", "empty src errors", "last"], "{l:?}");
}

#[test]
fn domcontentloaded_reaches_window_and_document_listeners_once() {
    let html = page(
        "<script>let n = 0; addEventListener('DOMContentLoaded', () => console.log('T window ' + (++n))); document.addEventListener('DOMContentLoaded', () => console.log('T document ' + (++n)));\
         document.onreadystatechange = () => console.log('T state ' + document.readyState);</script>",
    );
    let l = load(&html, &[], 10);
    assert_eq!(t(&l), ["state interactive", "document 1", "window 2", "state complete"], "{l:?}");
}

#[test]
fn an_error_in_a_deferred_script_does_not_keep_domcontentloaded_or_load_from_firing() {
    let html = page(
        "<script defer src=/bad.js></script><script defer src=/good.js></script>\
         <script>addEventListener('DOMContentLoaded', () => console.log('T dcl')); addEventListener('load', () => console.log('T load'));</script>",
    );
    let l = load(
        &html,
        &[("/bad.js", "throw new Error('deferred boom');", Arrive::At(1)), ("/good.js", "console.log('T good');", Arrive::At(1))],
        20,
    );
    assert_eq!(t(&l), ["good", "dcl", "load"], "{l:?}");
    assert!(l.iter().any(|x| x.contains("deferred boom")), "{l:?}");
}
