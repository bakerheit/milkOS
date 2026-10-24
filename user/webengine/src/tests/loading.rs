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
fn a_blocking_script_sees_only_the_page_before_it_as_the_parser_would_have_it() {
    // The parser has got as far as the script: what comes after is not in the page yet. A deferred
    // script and DOMContentLoaded see it all.
    let html = "<!DOCTYPE html><html><head><title>t</title>\
         <script>console.log('T head: body ' + document.body + ', later ' + !!document.getElementById('later') + ', scripts ' + document.scripts.length)</script></head>\
         <body><p id=first>first</p>\
         <script id=b1>var s = document.scripts; console.log('T b1: later ' + !!document.getElementById('later') + ', first ' + !!document.getElementById('first') + ', scripts ' + s.length + ', last is me ' + (s[s.length - 1] === document.currentScript) + ', divs ' + document.querySelectorAll('div').length + ', children ' + document.body.children.length);\
           document.body.appendChild(document.createElement('hr')); console.log('T b1 added: last child ' + document.body.lastElementChild.tagName);</script>\
         <div id=later>later</div>\
         <script defer src=/d.js></script>\
         <script>addEventListener('DOMContentLoaded', () => console.log('T dcl: divs ' + document.querySelectorAll('div').length))</script>\
         </body></html>";
    let l = load(
        html,
        &[("/d.js", "console.log('T defer: later ' + !!document.getElementById('later') + ', scripts ' + document.scripts.length + ', children ' + document.body.children.length + ', order ' + [...document.body.children].map(e => e.tagName).join())", Arrive::At(1))],
        10,
    );
    assert_eq!(
        t(&l),
        [
            "head: body null, later false, scripts 1",
            "b1: later false, first true, scripts 2, last is me true, divs 0, children 2",
            "b1 added: last child HR",
            "defer: later true, scripts 4, children 6, order P,SCRIPT,HR,DIV,SCRIPT,SCRIPT",
            "dcl: divs 1"
        ],
        "{l:?}"
    );
}

#[test]
fn scripts_that_change_the_page_while_it_is_cut_leave_it_whole_afterwards() {
    // A script that moves, removes and adds nodes around the cut; nothing is lost or duplicated.
    let html = "<!DOCTYPE html><html><body><div id=a><p id=a1></p><script id=s>\
           var a = document.getElementById('a'), s = document.currentScript;\
           a.appendChild(document.createElement('i'));\
           a.insertBefore(document.createElement('b'), s);\
           document.body.appendChild(document.createElement('u'));\
           s.parentNode.removeChild(document.getElementById('a1'));\
         </script><p id=a2></p><p id=a3></p></div><div id=z><span id=z1></span></div>\
         <script defer src=/d.js></script>\
         </body></html>";
    let l = load(html, &[("/d.js", "console.log('T ' + [...document.body.querySelectorAll('*')].map(e => e.id || e.tagName).join())", Arrive::At(1))], 10);
    // After the first script: a's children are b, s, i (it added them at the cut), then a2, a3
    // (the rest of a); body: a, u (added at the cut), then z and the deferred script.
    assert_eq!(t(&l), ["a,B,s,I,a2,a3,U,z,z1,SCRIPT"], "{l:?}");
}

#[test]
fn a_script_that_removes_itself_or_its_parent_does_not_lose_the_rest() {
    let html = "<!DOCTYPE html><html><body><div id=x><script>var me = document.currentScript; me.parentNode.removeChild(me); console.log('T removed self')</script><p id=after></p></div>\
         <section id=y><script>document.getElementById('y').remove(); console.log('T removed parent')</script><p id=inside></p></section><p id=last></p>\
         <script defer src=/d.js></script></body></html>";
    let l = load(
        html,
        &[("/d.js", "console.log('T ' + [...document.body.children].map(e => e.id || e.tagName).join() + ' after=' + !!document.getElementById('after') + ' inside=' + !!document.getElementById('inside'))", Arrive::At(1))],
        10,
    );
    let tt = t(&l);
    assert_eq!(&tt[..2], ["removed self", "removed parent"], "{l:?}");
    assert_eq!(tt[2], "x,last,SCRIPT after=true inside=false", "{l:?}");
}

