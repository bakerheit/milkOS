//! The host side of DOMParser and `document.implementation.createHTMLDocument`: markup parsed
//! into nodes that belong to the page's DOM but hang under a detached holder, so the script
//! side (`js/dom.js`) can present them as a document. Scripts in the markup never run.

use alloc::rc::Rc;
use alloc::string::String;

use jsengine::{JsResult, Realm, Value};

use super::bridge::{Bridge, arg, arg_str};
use super::nodes;
use crate::dom::NodeData;

/// Most bytes of markup one parse takes.
const MAX_MARKUP: usize = 32 << 20;

pub fn define_natives(b: &Rc<Bridge>, r: &mut Realm, n: Value) -> JsResult<()> {
    // parseMarkup(html, asDocument) -> [holder, doctypeName | null]: `holder` is a detached
    // fragment whose children are the parsed nodes (for a document: its <html> element).
    let b2 = b.clone();
    r.define_method(n, "parseMarkup", 2, move |r, _t, a| {
        let html = arg_str(r, a, 0)?;
        let as_doc = r.to_boolean(arg(a, 1));
        if html.len() > MAX_MARKUP {
            return Err(r.throw_error(jsengine::ErrorKind::RangeError, "the markup is too big"));
        }
        let (holder, doctype, tops) = {
            let p = b2.page_or(r)?;
            nodes::check_room(p, r)?;
            let holder = nodes::new_fragment(p);
            let parsed = if as_doc {
                crate::html::parse_document_with(&html, p.limits(), false)
            } else {
                crate::html::parse_fragment(&html, crate::atoms::tag::DIV, p.limits(), false)
            };
            let doctype: Option<String> = if as_doc {
                parsed
                    .dom
                    .children(0)
                    .find_map(|c| match &parsed.dom.node(c).data {
                        NodeData::Doctype(d) => Some(d[0].clone()),
                        _ => None,
                    })
            } else {
                None
            };
            let room = p.limits().max_nodes.saturating_sub(p.dom.live());
            let from = if as_doc { 0 } else { parsed.dom.document_element() };
            let tops = p.dom.import(&parsed.dom, from, holder, crate::dom::NONE, room);
            (holder, doctype, tops)
        };
        nodes::mark_scripts_started(&b2, &tops);
        let h = nodes::wrap(&b2, r, holder);
        let d = match doctype {
            Some(s) => r.string(&s),
            None => Value::NULL,
        };
        Ok(r.new_array(&[h, d]))
    })?;
    Ok(())
}
