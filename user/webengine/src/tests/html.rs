//! Tree-builder tests in the html5lib-tests "tree dump" format.

use alloc::string::String;

use crate::api::Limits;
use crate::dom::{Dom, NONE, NodeData, NodeId, Ns};
use crate::html::parse_document;

pub fn dump(dom: &Dom) -> String {
    let mut out = String::new();
    fn walk(dom: &Dom, n: NodeId, depth: usize, out: &mut String) {
        let mut c = dom.node(n).first_child;
        while c != NONE {
            out.push('|');
            for _ in 0..depth {
                out.push_str("  ");
            }
            out.push(' ');
            match &dom.node(c).data {
                NodeData::Element(e) => {
                    out.push('<');
                    match e.ns {
                        Ns::Svg => out.push_str("svg "),
                        Ns::MathMl => out.push_str("math "),
                        Ns::Html => {}
                    }
                    out.push_str(dom.atoms.name(e.name));
                    out.push_str(">\n");
                    let mut attrs: alloc::vec::Vec<_> = e.attrs.iter().collect();
                    attrs.sort_by(|a, b| a.name.cmp(&b.name));
                    for a in attrs {
                        out.push('|');
                        for _ in 0..depth + 1 {
                            out.push_str("  ");
                        }
                        out.push_str(&alloc::format!(" {}=\"{}\"\n", a.name, a.value));
                    }
                }
                NodeData::Text(t) => out.push_str(&alloc::format!("\"{t}\"\n")),
                NodeData::Comment(t) => out.push_str(&alloc::format!("<!-- {t} -->\n")),
                NodeData::Doctype { name, .. } => {
                    out.push_str(&alloc::format!("<!DOCTYPE {name}>\n"))
                }
                NodeData::Document => {}
            }
            walk(dom, c, depth + 1, out);
            c = dom.node(c).next;
        }
    }
    walk(dom, 0, 0, &mut out);
    out
}

fn tree(html: &str) -> String {
    dump(&parse_document(html, &Limits::default()).dom)
}

fn check(html: &str, want: &str) {
    let got = tree(html);
    let want: String = want
        .lines()
        .map(|l| l.trim_start())
        .filter(|l| !l.is_empty())
        .map(|l| alloc::format!("{l}\n"))
        .collect();
    assert_eq!(
        got, want,
        "\ninput: {html}\n--- got ---\n{got}--- want ---\n{want}"
    );
}

#[test]
fn implied_html_head_body() {
    check(
        "Test",
        r#"
        | <html>
        |   <head>
        |   <body>
        |     "Test"
    "#,
    );
}

#[test]
fn doctype_and_title() {
    check(
        "<!DOCTYPE html><title>T &amp; t</title><p>One<p>Two",
        r#"
        | <!DOCTYPE html>
        | <html>
        |   <head>
        |     <title>
        |       "T & t"
        |   <body>
        |     <p>
        |       "One"
        |     <p>
        |       "Two"
    "#,
    );
}

#[test]
fn misnested_formatting_adoption_agency() {
    // The classic html5lib case.
    check(
        "<a><p>X<a>Y</a>Z</p></a>",
        r#"
        | <html>
        |   <head>
        |   <body>
        |     <a>
        |     <p>
        |       <a>
        |         "X"
        |       <a>
        |         "Y"
        |       "Z"
    "#,
    );
    check(
        "<b>1<p>2</b>3</p>",
        r#"
        | <html>
        |   <head>
        |   <body>
        |     <b>
        |       "1"
        |     <p>
        |       <b>
        |         "2"
        |       "3"
    "#,
    );
}

#[test]
fn table_foster_parenting() {
    check(
        "<table><tr><td>a</td></tr>x</table>",
        r#"
        | <html>
        |   <head>
        |   <body>
        |     "x"
        |     <table>
        |       <tbody>
        |         <tr>
        |           <td>
        |             "a"
    "#,
    );
}

#[test]
fn lists_and_entities() {
    check(
        "<ul><li>a<li>b &lt;&#65;&#x42;&notit; &copy</ul>",
        r#"
        | <html>
        |   <head>
        |   <body>
        |     <ul>
        |       <li>
        |         "a"
        |       <li>
        |         "b <AB¬it; ©"
    "#,
    );
}

#[test]
fn attributes_and_duplicates() {
    check(
        "<div id=a class='x y' id=b data-x=\"&amp;&copy=\">",
        r#"
        | <html>
        |   <head>
        |   <body>
        |     <div>
        |       class="x y"
        |       data-x="&&copy="
        |       id="a"
    "#,
    );
}

#[test]
fn raw_text_elements() {
    check(
        "<style>p < b {}</style><script>if (a<b) x='</p>'</script><textarea>\n<b>t</textarea>",
        r#"
        | <html>
        |   <head>
        |     <style>
        |       "p < b {}"
        |     <script>
        |       "if (a<b) x='</p>'"
        |   <body>
        |     <textarea>
        |       "<b>t"
    "#,
    );
}

#[test]
fn svg_foreign_content() {
    check(
        "<p><svg><g><p>x",
        r#"
        | <html>
        |   <head>
        |   <body>
        |     <p>
        |       <svg svg>
        |         <svg g>
        |     <p>
        |       "x"
    "#,
    );
}

#[test]
fn select_and_options() {
    check(
        "<select><option>a<option>b</select>c",
        r#"
        | <html>
        |   <head>
        |   <body>
        |     <select>
        |       <option>
        |         "a"
        |       <option>
        |         "b"
        |     "c"
    "#,
    );
}

#[test]
fn comments() {
    check(
        "<!--a--><p><!-- b -- c --!>",
        r#"
        | <!-- a -->
        | <html>
        |   <head>
        |   <body>
        |     <p>
        |       <!--  b -- c  -->
    "#,
    );
}

#[test]
fn deep_nesting_is_bounded() {
    let mut s = String::new();
    for _ in 0..10_000 {
        s.push_str("<div>");
    }
    let p = parse_document(&s, &Limits::default());
    assert!(p.truncated);
    // Depth of the deepest node stays at the limit.
    let mut deepest = 0;
    for i in 0..p.dom.len() as u32 {
        let mut d = 0;
        let mut n = i;
        while n != NONE {
            n = p.dom.parent(n);
            d += 1;
        }
        deepest = deepest.max(d);
    }
    assert!(deepest <= Limits::default().max_depth + 2, "{deepest}");
}

#[test]
fn hostile_formatting_is_bounded() {
    let mut s = String::new();
    for i in 0..5000 {
        s.push_str(&alloc::format!("<b class=c{i}><i><table><td>"));
    }
    let p = parse_document(&s, &Limits::default());
    assert!(p.dom.len() < 200_000);
}
