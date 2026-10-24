//! What an uncaught error tells a person: where it happened, the source line with a caret,
//! and names for the functions on the stack (including the ones a script never named).

use crate::*;
use alloc::format;
use alloc::string::String;

fn report_of(src: &str, file: &str) -> ErrorReport {
    let mut r = Realm::new(Options::default());
    let e = r.eval(src, file).unwrap_err();
    r.report(&e)
}

#[test]
fn the_report_shows_the_source_line_and_a_caret() {
    let rep = report_of("var a = 1;\n  var o = {};\n  o.method(1, 2);\nvar z;", "s.js");
    assert_eq!(rep.name, "TypeError");
    assert_eq!((rep.file.as_str(), rep.line), ("s.js", 3));
    // The indentation is removed, and the position is a column within what is left.
    assert_eq!(rep.snippet, "o.method(1, 2);");
    // The position is the method's name.
    assert_eq!((rep.column, rep.snippet_col), (5, 2), "{rep:?}");
    let text = format!("{rep}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "TypeError: o.method is not a function", "{text}");
    assert_eq!(lines[lines.len() - 2], "    3 | o.method(1, 2);", "{text}");
    assert_eq!(lines[lines.len() - 1], "      |   ^", "{text}");
}

#[test]
fn the_caret_is_under_the_failing_call() {
    let rep = report_of("var f = 1;\nvar x = 5 + f(3);", "c.js");
    assert_eq!(rep.snippet, "var x = 5 + f(3);");
    // `rep.column` counts the line's characters (1-based); the caret agrees with it.
    assert_eq!(rep.column, 13, "{rep:?}");
    assert_eq!(rep.snippet_col, 12, "{rep:?}");
    let text = format!("{rep}");
    assert!(text.ends_with("    2 | var x = 5 + f(3);\n      |             ^"), "{text}");
}

#[test]
fn a_long_minified_line_is_cut_around_the_column() {
    // 300 KB on one line, the failing call in the middle.
    let mut src = String::new();
    for i in 0..30_000 {
        src.push_str(&format!("a{i}=1;"));
        if i % 1000 == 999 {
            src.push_str("var g0=0;");
        }
    }
    let at = src.len();
    src.push_str("notAFunction();");
    for i in 0..30_000 {
        src.push_str(&format!("b{i}=2;"));
    }
    let rep = report_of(&src, "min.js");
    assert_eq!(rep.name, "ReferenceError");
    assert_eq!(rep.line, 1);
    assert_eq!(rep.column as usize, at + 1);
    assert!(rep.snippet.len() <= 130, "{} bytes", rep.snippet.len());
    assert!(rep.snippet.starts_with("...") && rep.snippet.ends_with("..."), "{}", rep.snippet);
    assert!(rep.snippet.contains("notAFunction();"), "{}", rep.snippet);
    // The caret points at the name.
    let col = rep.snippet_col as usize;
    assert!(rep.snippet[col..].starts_with("notAFunction"), "{} / {col}", rep.snippet);
    // The whole report stays small too.
    assert!(format!("{rep}").len() < 600);
}

#[test]
fn non_ascii_text_keeps_the_caret_in_place() {
    let rep = report_of("var s = 'h\u{e9}llo \u{4e16}\u{754c}'; missing(s);", "u.js");
    assert_eq!(rep.name, "ReferenceError");
    let col = rep.snippet_col as usize;
    let chars: String = rep.snippet.chars().skip(col).collect();
    assert!(chars.starts_with("missing"), "{} / {col}", rep.snippet);
    // And a cut that would fall inside a multi-byte character doesn't make a mess.
    let mut src = String::new();
    for _ in 0..200 {
        src.push('\u{4e16}');
    }
    src.push_str(";missing();");
    for _ in 0..200 {
        src.push('\u{754c}');
    }
    let rep = report_of(&format!("var s='{}'; xyz{};", src, ""), "w.js");
    assert!(!rep.snippet.contains('\u{fffd}'), "{}", rep.snippet);
}

#[test]
fn control_characters_in_the_line_become_spaces() {
    let rep = report_of("var t\t=\t1;\tnull.x;", "t.js");
    assert!(!rep.snippet.contains('\t'), "{:?}", rep.snippet);
    let rep = report_of("var s = 1; \u{2028} nope();", "t.js");
    assert_eq!(rep.line, 2, "{rep:?}");
    assert_eq!(rep.snippet, "nope();");
}

#[test]
fn syntax_errors_show_their_line() {
    let rep = report_of("var ok = 1;\nvar = 2;\n", "bad.js");
    assert_eq!(rep.name, "SyntaxError");
    assert_eq!(rep.line, 2);
    assert_eq!(rep.snippet, "var = 2;");
    let mut r = Realm::new(Options::default());
    let e = r
        .eval_with(
            "  x = 1;\n  y = ;",
            &ScriptOptions { file: "page.html", first_line: 30, first_column: 9 },
        )
        .unwrap_err();
    let rep = r.report(&e);
    assert_eq!((rep.line, rep.snippet.as_str()), (31, "y = ;"), "{rep:?}");
}

#[test]
fn thrown_values_that_are_not_errors_have_the_line_too() {
    let rep = report_of("\n  throw 'plain';", "p.js");
    assert_eq!(rep.name, "Uncaught");
    assert_eq!(rep.snippet, "throw 'plain';");
    assert_eq!(rep.snippet_col, 0);
}

#[test]
fn a_native_error_with_no_script_running_has_no_source_line() {
    let mut r = Realm::new(Options::default());
    let e = r.new_error(ErrorKind::TypeError, "from the host");
    let rep = r.report_value(e);
    assert_eq!(rep.snippet, "");
    assert_eq!(format!("{rep}"), "TypeError: from the host");
}

#[test]
fn a_rejected_promise_reports_its_line_too() {
    let mut r = Realm::new(Options::default());
    r.eval("Promise.resolve().then(function () {\n  null.nothing;\n});", "rej.js")
        .unwrap();
    let hits = alloc::rc::Rc::new(core::cell::RefCell::new(alloc::vec::Vec::<String>::new()));
    let h2 = hits.clone();
    r.set_rejection_hook(Some(Box::new(move |r: &mut Realm, ev: PromiseRejection| {
        if let PromiseRejection::Unhandled { reason, .. } = ev {
            h2.borrow_mut().push(format!("{}", r.report_value(reason)));
        }
    })));
    r.run_jobs().unwrap();
    let hits = hits.borrow();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].starts_with("TypeError: Cannot read properties of null"), "{}", hits[0]);
    assert!(hits[0].contains("2 | null.nothing;"), "{}", hits[0]);
}

fn first_frames(src: &str) -> String {
    let rep = report_of(src, "n.js");
    rep.stack
}

#[test]
fn functions_are_named_by_where_they_are_stored() {
    // `a.b = function`, `var f = function`, an object literal's key, and a path.
    let stack = first_frames(
        "var _ = {}; _.util = {};\n\
         _.util.go = function () { return bad(); };\n\
         var viaVar = function () { return _.util.go(); };\n\
         var o = { key: function () { return viaVar(); } };\n\
         o.key();",
    );
    let names: Vec<&str> = stack.lines().map(|l| l.trim()).collect();
    assert!(names[0].starts_with("at _.util.go (n.js:2:"), "{stack}");
    assert!(names[1].starts_with("at viaVar (n.js:3:"), "{stack}");
    assert!(names[2].starts_with("at key (n.js:4:"), "{stack}");
    assert!(names[3].starts_with("at n.js:5:"), "{stack}");
}

#[test]
fn prototype_and_this_are_left_out_of_inferred_names() {
    let stack = first_frames(
        "function Thing() { this.run = function () { missing(); }; }\n\
         Thing.prototype.start = function () { this.run(); };\n\
         new Thing().start();",
    );
    assert!(stack.contains("at run (n.js:1:"), "{stack}");
    assert!(stack.contains("at Thing.start (n.js:2:"), "{stack}");
}

#[test]
fn inferred_names_do_not_change_the_name_property() {
    let mut r = Realm::new(Options::default());
    let v = r
        .eval("var a = {}; a.b = function () {}; a.b.name + '|' + (a.c = () => 1).name", "x.js")
        .unwrap();
    assert_eq!(r.get_string(v).as_deref(), Some("|"));
}

#[test]
fn big_functions_compiled_late_keep_their_inferred_names() {
    // (Functions over 64 bytes are compiled on their first call.)
    let pad = "var pad = 1; ".repeat(20);
    let src = format!(
        "var ns = {{}};\n\
         ns.outer = function () {{ {pad} return ns.inner(); }};\n\
         ns.inner = function () {{ {pad} return undefinedThing; }};\n\
         ns.outer();"
    );
    let stack = first_frames(&src);
    assert!(stack.contains("at ns.inner (n.js:3:"), "{stack}");
    assert!(stack.contains("at ns.outer (n.js:2:"), "{stack}");
}

#[test]
fn parenthesized_and_arrow_functions_get_inferred_names() {
    let stack = first_frames("var h = {};\nh.arrow = (() => nothing());\nh.arrow();");
    assert!(stack.contains("at h.arrow (n.js:2:"), "{stack}");
}

#[test]
fn a_computed_or_call_base_names_by_the_last_member() {
    let stack = first_frames(
        "function get() { return {}; }\n\
         var t = get();\n\
         get().later = function () { return zzz; };\n\
         var k = get(); k.later = get().later;\n\
         (function () { var q = {}; q.x = function () { return zzz; }; q.x(); })();",
    );
    assert!(stack.contains("at q.x (n.js:5:"), "{stack}");
}

#[test]
fn unnamed_functions_still_say_anonymous() {
    let stack = first_frames("[1].forEach(function () { missing(); });");
    assert!(stack.contains("at <anonymous> (n.js:1:"), "{stack}");
}

fn message_of(src: &str) -> (String, u32) {
    let rep = report_of(src, "m.js");
    (rep.message, rep.column)
}

#[test]
fn not_a_function_says_which_link_of_a_chain_failed() {
    // The call that fails is the outer one: the callee is `V(...).concat`, not `V`.
    let (m, col) = message_of("var V = function () { return {}; };\nvar r = V(1).concat(V(2));");
    assert_eq!(m, "V(...).concat is not a function");
    assert_eq!(col, 14, "the position is the property's name");
    // The inner call that fails.
    let (m, col) = message_of("var V = 5;\nvar r = V(1).concat(2);");
    assert_eq!(m, "V is not a function");
    assert_eq!(col, 9);
    // A call of a call's result, an index, a longer path.
    let (m, _) = message_of("function f() { return 1; }\nf()();");
    assert_eq!(m, "f(...) is not a function");
    let (m, _) = message_of("var o = { a: [1] }, k = 'a';\no[k][0]();");
    assert_eq!(m, "o[k][0] is not a function");
    let (m, _) = message_of("var o = { a: { b: {} } };\no.a.b.c.d();");
    assert!(m.starts_with("Cannot read properties of undefined (reading 'd')"), "{m}");
    let (m, _) = message_of("var o = { a: { b: {} } };\no.a.b.c();");
    assert_eq!(m, "o.a.b.c is not a function");
    let (m, _) = message_of("var o = { a: { b: {} } };\no.a.b\n  .c(1,\n 2);");
    assert_eq!(m, "o.a.b.c is not a function");
    let (m, _) = message_of("var o = {};\no.f(function () { return 1; }, [2, (3)]).g();");
    assert_eq!(m, "o.f is not a function");
    let (m, _) = message_of("var o = { f: function () { return {}; } };\no.f(function () { return 1; }, [2, (3)]).g('a)');");
    assert_eq!(m, "o.f(...).g is not a function");
    // What it can't word falls back to what the value is.
    let (m, _) = message_of("(1, 2)();");
    assert!(m.contains("is not a function"), "{m}");
    let (m, _) = message_of("var a = [1]; [a][0][0]();");
    assert!(m.contains("is not a function"), "{m}");
}

#[test]
fn each_link_of_a_chain_has_its_own_position() {
    // The line and column of the property that failed, on the line it is on.
    let rep = report_of("var o = { a: {} };\nvar x = o\n  .a\n  .b\n  .c;", "p.js");
    assert_eq!(rep.message, "Cannot read properties of undefined (reading 'c')");
    assert_eq!((rep.line, rep.column), (5, 4));
    let rep = report_of("var o = { a: {} };\nvar x = o.a['b'].c;", "p.js");
    assert_eq!((rep.line, rep.snippet.as_str()), (2, "var x = o.a['b'].c;"));
    // The frames of a stack are at the call, not at the start of the statement.
    let rep = report_of(
        "var q = { go: function () { return missing(); } };\nvar z = 1; q\n.go();",
        "p.js",
    );
    assert!(rep.stack.contains("at go (p.js:1:"), "{}", rep.stack);
    assert!(rep.stack.lines().last().unwrap().contains("p.js:3:2"), "{}", rep.stack);
}

#[test]
fn hostile_text_never_breaks_the_callee_words_or_the_snippet() {
    // Random soup of names, dots, groups, quotes, line breaks and multi-byte text: the
    // callee reader and the snippet look at any position of any text without panicking, and
    // what they say stays short.
    let pool = [
        "a", "b1", "$x", "_", ".", "?.", "(", ")", "[", "]", "{", "}", "'", "\"", "`", "\\", " ", "\n", "\r\n", "\t",
        "new ", "function", "é", "日本", "\u{1F600}", "\u{2028}", ",", ";", "0", "12", "...", "=>",
    ];
    let mut x = 0x1234_5678_9abc_def0u64;
    let mut rnd = |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    for _ in 0..4000 {
        let mut t = String::new();
        for _ in 0..rnd(120) {
            t.push_str(pool[rnd(pool.len() as u64) as usize]);
            if rnd(60) == 0 {
                t.push_str(&"q".repeat(rnd(1500) as usize));
            }
        }
        let b = t.as_bytes();
        for _ in 0..12 {
            let pos = rnd(b.len() as u64 + 3) as usize;
            if let Some(s) = crate::interp::callee_text_at(b, pos) {
                assert!(s.len() <= 100, "{s:?} from {t:?} at {pos}");
            }
        }
        // As a script that fails somewhere (a syntax error most of the time, or a throw).
        let rep = report_of(&format!("{t}\n;null.x"), "h.js");
        assert!(rep.snippet.chars().count() <= 130, "{:?}", rep.snippet);
        assert!(rep.snippet_col as usize <= rep.snippet.chars().count() + 1);
    }
}
