//! Serializing a subtree back to HTML (HTML §13.3 "Serializing HTML fragments"), for
//! `innerHTML` and `outerHTML`. Iterative (no recursion on hostile depth) and bounded: output
//! stops at `max` bytes.

use alloc::string::String;
use alloc::vec::Vec;

use crate::atoms::tag;
use crate::dom::{Dom, NONE, NodeData, NodeId, Ns};

/// HTML's void elements: no end tag, no children serialized.
pub fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "basefont"
            | "bgsound"
            | "br"
            | "col"
            | "embed"
            | "frame"
            | "hr"
            | "img"
            | "input"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn escape(s: &str, attr: bool, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            '"' if attr => out.push_str("&quot;"),
            '<' if !attr => out.push_str("&lt;"),
            '>' if !attr => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

/// Text under these is written as is.
fn raw_text_parent(dom: &Dom, n: NodeId, scripting: bool) -> bool {
    match dom.element(n) {
        Some(e) if e.ns == Ns::Html => {
            matches!(
                e.name,
                tag::STYLE
                    | tag::SCRIPT
                    | tag::XMP
                    | tag::IFRAME
                    | tag::NOEMBED
                    | tag::NOFRAMES
                    | tag::PLAINTEXT
            ) || (scripting && e.name == tag::NOSCRIPT)
        }
        _ => false,
    }
}

fn open_tag(dom: &Dom, n: NodeId, out: &mut String) {
    let Some(e) = dom.element(n) else { return };
    out.push('<');
    out.push_str(dom.atoms.name(e.name));
    for a in &e.attrs {
        out.push(' ');
        out.push_str(&a.name);
        out.push_str("=\"");
        escape(&a.value, true, out);
        out.push('"');
    }
    out.push('>');
}

fn node_start(dom: &Dom, n: NodeId, scripting: bool, out: &mut String) -> bool {
    match &dom.node(n).data {
        NodeData::Element(e) => {
            open_tag(dom, n, out);
            // Children unless void.
            !(e.ns == Ns::Html && is_void(dom.atoms.name(e.name)))
        }
        NodeData::Text(t) => {
            let p = dom.parent(n);
            if p != NONE && raw_text_parent(dom, p, scripting) {
                out.push_str(t);
            } else {
                escape(t, false, out);
            }
            false
        }
        NodeData::Comment(t) => {
            out.push_str("<!--");
            out.push_str(t);
            out.push_str("-->");
            false
        }
        NodeData::Doctype(d) => {
            out.push_str("<!DOCTYPE ");
            out.push_str(&d[0]);
            out.push('>');
            false
        }
        NodeData::Document => true,
    }
}

fn node_end(dom: &Dom, n: NodeId, out: &mut String) {
    if let Some(e) = dom.element(n) {
        out.push_str("</");
        out.push_str(dom.atoms.name(e.name));
        out.push('>');
    }
}

/// The children of `n`, serialized (`innerHTML`).
pub fn inner_html(dom: &Dom, n: NodeId, scripting: bool, max: usize) -> String {
    let mut out = String::new();
    // (node, entered): entered nodes get their end tag when popped again.
    let mut stack: Vec<(NodeId, bool)> = Vec::new();
    let kids: Vec<NodeId> = dom.children(n).collect();
    for &k in kids.iter().rev() {
        stack.push((k, false));
    }
    while let Some((c, entered)) = stack.pop() {
        if out.len() > max {
            break;
        }
        if entered {
            node_end(dom, c, &mut out);
            continue;
        }
        if node_start(dom, c, scripting, &mut out) {
            stack.push((c, true));
            let kids: Vec<NodeId> = dom.children(c).collect();
            for &k in kids.iter().rev() {
                stack.push((k, false));
            }
        }
    }
    out
}

/// `n` itself and its children (`outerHTML`).
pub fn outer_html(dom: &Dom, n: NodeId, scripting: bool, max: usize) -> String {
    let mut out = String::new();
    if node_start(dom, n, scripting, &mut out) {
        out.push_str(&inner_html(dom, n, scripting, max));
        if !matches!(dom.node(n).data, NodeData::Document) {
            node_end(dom, n, &mut out);
        }
    }
    out
}
