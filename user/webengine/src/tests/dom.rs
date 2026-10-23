//! Changing the DOM after parsing (what scripts do): fragment parsing, serialization, the
//! change log, incremental restyling (checked against a fresh parse of the same markup),
//! form state across tree changes, hit testing after changes.

use alloc::string::String;
use alloc::vec::Vec;

use crate::api::*;
use crate::atoms::tag;
use crate::dom::{Dom, NONE, NodeId};
use crate::serialize::{inner_html, outer_html};

fn lay(d: &mut Document) {
    d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
}

fn by_id(dom: &Dom, id: &str) -> NodeId {
    let a = dom.atoms.get(id);
    let mut n = 0;
    while n != NONE {
        if a.is_some() && dom.element(n).is_some_and(|e| e.id == a) {
            return n;
        }
        n = dom.next_in(n, 0);
    }
    NONE
}

/// Parse `html` as the children of a fresh element of `ctx`, and serialize them back.
fn frag(html: &str, ctx: crate::atoms::Atom) -> String {
    let p = crate::html::parse_fragment(html, ctx, &Limits::default(), false);
    let r = p.dom.document_element();
    inner_html(&p.dom, r, false, 1 << 20)
}

#[test]
fn fragment_parsing_follows_the_context() {
    assert_eq!(frag("<p>a<b>b</p>c", tag::DIV), "<p>a<b>b</b></p><b>c</b>");
    // In a tbody the rows go in directly (no table, no extra tbody).
    assert_eq!(
        frag("<tr><td>1<td>2</tr>", tag::TBODY),
        "<tr><td>1</td><td>2</td></tr>"
    );
    // In a div, table parts without a table are dropped.
    assert_eq!(frag("<tr><td>1</td></tr>", tag::DIV), "1");
    // A textarea's content is text.
    assert_eq!(frag("<b>x</b>", tag::TEXTAREA), "&lt;b&gt;x&lt;/b&gt;");
    assert_eq!(frag("<option>a<option>b", tag::SELECT), "<option>a</option><option>b</option>");
    // Scripts come in as elements (they are not run: the bindings mark them started).
    assert_eq!(
        frag("<script>if (a < b) x()</script>", tag::DIV),
        "<script>if (a < b) x()</script>"
    );
}

#[test]
fn serialization_escapes_and_handles_voids() {
    let d = Document::parse_str(
        "<div id=d title='a\"b&amp;c'>x &lt; y &amp; z\u{a0}<br><img src=i.png><!--c--></div>",
        "http://t.example/",
    );
    let dom = &d.page.dom;
    let n = by_id(dom, "d");
    assert_eq!(
        outer_html(dom, n, false, 1 << 20),
        "<div id=\"d\" title=\"a&quot;b&amp;c\">x &lt; y &amp; z&nbsp;<br><img src=\"i.png\"><!--c--></div>"
    );
}

#[test]
fn noscript_follows_the_scripting_flag() {
    let html = "<body><noscript><p id=n>no js</p></noscript><p>x</p>";
    let off = Document::parse_str(html, "http://t.example/");
    assert!(by_id(&off.page.dom, "n") != NONE);
    let on = Document {
        page: crate::page::Page::parse_str_with(html, "http://t.example/", Limits::default(), true),
    };
    // Raw text: no <p id=n> element, and it isn't shown.
    assert_eq!(by_id(&on.page.dom, "n"), NONE);
    let mut on = on;
    lay(&mut on);
    assert!(!on.text().contains("no js"), "{}", on.text());
    assert!(on.text().contains('x'));
}

#[test]
fn changes_are_noted() {
    let mut d = Document::parse_str(
        "<ul id=u><li>a<li>b</ul><p id=p class=x>t</p>",
        "http://t.example/",
    );
    lay(&mut d);
    let dom = &mut d.page.dom;
    assert!(dom.track);
    assert_eq!(dom.take_changes(), Default::default());
    let u = by_id(dom, "u");
    let li = dom.create_element(tag::LI, crate::dom::Ns::Html, Vec::new());
    dom.append(u, li);
    let c = dom.take_changes();
    assert!(c.tree && c.forms && c.any && !c.sheets);
    assert_eq!(c.restyle.len(), 1);
    let p = by_id(dom, "p");
    dom.set_attr(p, "class", "y z");
    let e = dom.element(p).unwrap();
    assert_eq!(e.classes.len(), 2);
    let c = dom.take_changes();
    assert!(!c.tree && c.any);
    // A <style> coming in means the sheets are looked at again.
    let st = dom.create_element(tag::STYLE, crate::dom::Ns::Html, Vec::new());
    dom.append(p, st);
    assert!(dom.take_changes().sheets);
    // Many separate changes: everything is restyled instead.
    for _ in 0..100 {
        let x = dom.create_element(tag::DIV, crate::dom::Ns::Html, Vec::new());
        let y = dom.create_element(tag::SPAN, crate::dom::Ns::Html, Vec::new());
        dom.append(x, y);
        dom.append(p, x);
        dom.set_attr(y, "class", "q");
    }
    let c = dom.take_changes();
    assert!(c.restyle_all && c.restyle.is_empty());
}

#[test]
fn freed_nodes_are_reused() {
    let mut d = Document::parse_str("<div id=a><p>1</p><p>2</p></div>", "http://t.example/");
    let dom = &mut d.page.dom;
    let a = by_id(dom, "a");
    let before = dom.len();
    let first = dom.node(a).first_child;
    dom.free_subtree(first);
    assert_eq!(dom.live(), before - 2);
    let n = dom.create_text(String::from("new"));
    assert!((n as usize) < before);
    assert_eq!(dom.len(), before);
    assert_eq!(inner_html(dom, a, false, 1000), "<p>2</p>");
}

/// The page after a script's changes must look exactly like a fresh parse of the same
/// markup: incremental restyling may skip work, never change the result.
fn same_as_fresh(d: &mut Document) {
    lay(d);
    let html = outer_html(&d.page.dom, d.page.dom.document_element(), false, 1 << 22);
    let mut fresh = Document::parse_str(&html, "http://t.example/");
    lay(&mut fresh);
    let a = d.display_list();
    let b = fresh.display_list();
    assert_eq!(a.entries, b.entries, "markup: {html}");
    assert_eq!(a.text, b.text);
    assert_eq!((a.width, a.height), (b.width, b.height));
}

const STYLED: &str = "<!DOCTYPE html><html><head><style>
  .tab { color: #333; padding: 4px }
  .tab.on { color: #c00; font-weight: bold }
  .panel { display: none }
  .panel.on { display: block; border: 1px solid #888 }
  li:first-child { color: green }
  li:last-child { color: blue }
  li:nth-child(2n) { background: #eee }
  .x + .y { margin-left: 20px }
  .x ~ .z { text-decoration: underline }
  ul:empty { height: 30px; background: #f00 }
  #box > span { font-size: 20px }
</style></head><body>
<div id=tabs><span class='tab on' id=t1>One</span><span class=tab id=t2>Two</span></div>
<div class='panel on' id=p1>First panel</div><div class=panel id=p2>Second panel</div>
<ul id=list><li>a</li><li>b</li><li>c</li></ul><ul id=empty></ul>
<p class=x id=px>x</p><p id=py>y</p><p id=pz>z</p>
<div id=box><em>e</em></div>
</body></html>";

#[test]
fn incremental_restyle_matches_a_fresh_parse() {
    let mut d = Document::parse_str(STYLED, "http://t.example/");
    lay(&mut d);
    let full0 = d.page.restyles.1;
    // Switch tabs.
    {
        let dom = &mut d.page.dom;
        let (t1, t2, p1, p2) = (by_id(dom, "t1"), by_id(dom, "t2"), by_id(dom, "p1"), by_id(dom, "p2"));
        dom.set_attr(t1, "class", "tab");
        dom.set_attr(t2, "class", "tab on");
        dom.set_attr(p1, "class", "panel");
        dom.set_attr(p2, "class", "panel on");
    }
    same_as_fresh(&mut d);
    assert!(d.page.restyles.0 >= 1, "incremental restyle used");
    assert_eq!(d.page.restyles.1, full0, "no full restyle");
    // A new first list item: :first-child and :nth-child move.
    {
        let dom = &mut d.page.dom;
        let list = by_id(dom, "list");
        let li = dom.create_element(tag::LI, crate::dom::Ns::Html, Vec::new());
        let t = dom.create_text(String::from("new"));
        dom.append(li, t);
        let first = dom.node(list).first_child;
        dom.insert_before(list, li, first);
    }
    same_as_fresh(&mut d);
    // Sibling combinators: .y gets the class x's neighbour has.
    {
        let dom = &mut d.page.dom;
        let (py, pz) = (by_id(dom, "py"), by_id(dom, "pz"));
        dom.set_attr(py, "class", "y");
        dom.set_attr(pz, "class", "z");
    }
    same_as_fresh(&mut d);
    // :empty stops matching; a span under #box gets its size.
    {
        let dom = &mut d.page.dom;
        let e = by_id(dom, "empty");
        let li = dom.create_element(tag::LI, crate::dom::Ns::Html, Vec::new());
        dom.append(e, li);
        let bx = by_id(dom, "box");
        let sp = dom.create_element(tag::SPAN, crate::dom::Ns::Html, Vec::new());
        let t = dom.create_text(String::from("big"));
        dom.append(sp, t);
        dom.append(bx, sp);
    }
    same_as_fresh(&mut d);
    // Removing and inline styles.
    {
        let dom = &mut d.page.dom;
        let list = by_id(dom, "list");
        let last = dom.node(list).last_child;
        dom.detach(last);
        let px = by_id(dom, "px");
        dom.set_attr(px, "style", "color: purple; padding-left: 3em");
        let t2 = by_id(dom, "t2");
        let tx = dom.node(t2).first_child;
        dom.set_data(tx, "Two (changed)");
    }
    same_as_fresh(&mut d);
    // A script adds a stylesheet.
    {
        let dom = &mut d.page.dom;
        let body = dom.body();
        let st = dom.create_element(tag::STYLE, crate::dom::Ns::Html, Vec::new());
        let t = dom.create_text(String::from("p { margin: 30px }"));
        dom.append(st, t);
        dom.append(body, st);
    }
    same_as_fresh(&mut d);
    assert!(d.page.restyles.1 > full0, "a new sheet restyles everything");
}

/// The same DOM restyled from scratch must paint the same.
fn same_as_full_restyle(d: &mut Document) {
    lay(d);
    let a = d.display_list().clone();
    d.page.force_full_restyle();
    lay(d);
    let b = d.display_list();
    assert_eq!(a.entries, b.entries);
    assert_eq!(a.text, b.text);
}

#[test]
fn incremental_restyle_random_changes() {
    // A small deterministic fuzz: random class changes and moves, each batch checked.
    let mut d = Document::parse_str(STYLED, "http://t.example/");
    lay(&mut d);
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut rnd = |n: usize| -> usize {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    let classes = ["tab", "tab on", "panel", "panel on", "x", "y", "z", ""];
    for _round in 0..12 {
        let dom = &mut d.page.dom;
        let body = dom.body();
        let mut els = Vec::new();
        let mut n = dom.next_in(body, body);
        while n != NONE {
            if dom.element(n).is_some() {
                els.push(n);
            }
            n = dom.next_in(n, body);
        }
        for _ in 0..4 {
            let e = els[rnd(els.len())];
            match rnd(3) {
                0 => dom.set_attr(e, "class", classes[rnd(classes.len())]),
                1 => {
                    let to = els[rnd(els.len())];
                    if !dom.contains(e, to) {
                        dom.append(to, e);
                    }
                }
                _ => {
                    let x = dom.create_element(tag::P, crate::dom::Ns::Html, Vec::new());
                    let t = dom.create_text(String::from("added"));
                    dom.append(x, t);
                    dom.insert_before(dom.parent(e), x, e);
                }
            }
        }
        // (Not a fresh parse: random moves make nesting the parser would undo.)
        same_as_full_restyle(&mut d);
    }
}

#[test]
fn form_state_survives_tree_changes() {
    let mut d = Document::parse_str(
        "<form id=f><input id=a name=a value=one><input type=checkbox id=c name=c><select id=s name=s><option>x<option>y</select></form>",
        "http://t.example/",
    );
    lay(&mut d);
    d.set_value(ControlId(0), "typed");
    d.set_checked(ControlId(1), true);
    d.select_option(ControlId(2), 1, true);
    {
        let dom = &mut d.page.dom;
        let f = by_id(dom, "f");
        let first = dom.node(f).first_child;
        let x = dom.create_element(
            tag::INPUT,
            crate::dom::Ns::Html,
            alloc::vec![crate::dom::Attr { name: "name".into(), value: "new".into() }],
        );
        dom.insert_before(f, x, first);
        // A new option before the selected one.
        let s = by_id(dom, "s");
        let o = dom.create_element(tag::OPTION, crate::dom::Ns::Html, Vec::new());
        let t = dom.create_text(String::from("w"));
        dom.append(o, t);
        let so = dom.node(s).first_child;
        dom.insert_before(s, o, so);
        // The value attribute changes, but the user's value is dirty: it stays.
        let a = by_id(dom, "a");
        dom.set_attr(a, "value", "attr");
    }
    lay(&mut d);
    let c = d.controls();
    assert_eq!(c.len(), 4);
    assert_eq!(c[0].name, "new");
    assert_eq!(c[1].value, "typed");
    assert!(c[2].checked);
    let sel: Vec<&str> = c[3]
        .options
        .iter()
        .filter(|o| o.selected)
        .map(|o| o.value.as_str())
        .collect();
    assert_eq!(sel, ["y"]);
    let sub = d.submit(ControlId(1), None).unwrap();
    assert_eq!(sub.url, "http://t.example/?new=&a=typed&c=on&s=y");
    // Reset goes back to the attributes (the changed one, too).
    d.reset(ControlId(1));
    assert_eq!(d.controls()[1].value, "attr");
    assert!(!d.controls()[2].checked);
}

#[test]
fn hit_testing_follows_changes() {
    let mut d = Document::parse_str(
        "<body style='margin:0'><div id=d style='height:50px'>top</div><a id=l href=/x>link</a>",
        "http://t.example/",
    );
    lay(&mut d);
    let link_y = 60;
    assert!(d.hit_test(5, link_y, (0, 0)).link.is_some());
    {
        let dom = &mut d.page.dom;
        let dd = by_id(dom, "d");
        dom.set_attr(dd, "style", "height:200px");
    }
    assert!(d.page.changed());
    lay(&mut d);
    assert!(d.hit_test(5, link_y, (0, 0)).link.is_none());
    assert!(d.hit_test(5, 210, (0, 0)).link.is_some());
}

