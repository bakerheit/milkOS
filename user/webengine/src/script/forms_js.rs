//! Form controls in script (HTML §4.10): HTMLInputElement, HTMLSelectElement,
//! HTMLOptionElement, HTMLTextAreaElement, HTMLButtonElement, HTMLFormElement and
//! HTMLLabelElement over the page's form state (values, checkedness and selectedness live in
//! the engine's form controls, so what script sets is what the app draws and submits), and
//! constraint validation (required, type, length, range, pattern, custom messages), which
//! also stops a submission from the user ([`validate_form`]).

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use jsengine::{Attrs, JsResult, Realm, Value};

use super::bridge::{Bridge, ListKind, arg, arg_bool, arg_str, dom_error, type_error};
use super::events_js;
use super::nodes::{self, getter, method, node_of, set_attribute, remove_attribute, this_node, wrap};
use crate::api::{ControlId, ControlKind, FormControl, PageRequest};
use crate::atoms::tag;
use crate::dom::{NONE, NodeId};
use crate::page::Page;

/// The control state of element `n` (after catching up with DOM changes).
fn ctl(b: &Bridge, n: NodeId) -> Option<(ControlId, FormControl)> {
    let p = b.page()?;
    p.sync_forms();
    let id = p.control_of_node(n)?;
    Some((id, p.controls()[id.0 as usize].clone()))
}

fn attr(b: &Bridge, n: NodeId, a: &str) -> Option<String> {
    b.page()?.dom.element(n)?.attr(a).map(String::from)
}

fn input_type(b: &Bridge, n: NodeId) -> String {
    let t = attr(b, n, "type").unwrap_or_default().trim().to_ascii_lowercase();
    const KNOWN: &[&str] = &[
        "text", "search", "tel", "url", "email", "password", "date", "month", "week", "time",
        "datetime-local", "number", "range", "color", "checkbox", "radio", "file", "submit",
        "image", "reset", "button", "hidden",
    ];
    if KNOWN.contains(&t.as_str()) {
        t
    } else {
        String::from("text")
    }
}

fn is_text_kind(k: ControlKind) -> bool {
    matches!(
        k,
        ControlKind::Text | ControlKind::Password | ControlKind::TextArea
    )
}

// ---- value, checked, ... -------------------------------------------------------------------

fn value_get(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let p = b.page_or(r)?;
    if p.dom.is_html(n, tag::OPTION) {
        let v = option_value(p, n);
        return Ok(r.string(&v));
    }
    let v = match ctl(b, n) {
        Some((_, c)) if is_text_kind(c.kind) => c.value,
        Some((_, c)) if c.kind == ControlKind::Select => c
            .options
            .iter()
            .find(|o| o.selected)
            .map(|o| o.value.clone())
            .unwrap_or_default(),
        Some((_, c)) if matches!(c.kind, ControlKind::Checkbox | ControlKind::Radio) => {
            attr(b, n, "value").unwrap_or_else(|| String::from("on"))
        }
        _ => attr(b, n, "value").unwrap_or_default(),
    };
    Ok(r.string(&v))
}

fn value_set(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let v = if arg(a, 0).is_null() { String::new() } else { arg_str(r, a, 0)? };
    if b.page_or(r)?.dom.is_html(n, tag::OPTION) {
        set_attribute(b, r, n, "value", &v)?;
        return Ok(Value::UNDEFINED);
    }
    match ctl(b, n) {
        Some((id, c)) if is_text_kind(c.kind) => {
            if let Some(p) = b.page() {
                p.forms_mut().set_value_as(id, &v, true);
                p.mark_forms_changed();
            }
        }
        Some((id, c)) if c.kind == ControlKind::Select => {
            let idx = c.options.iter().position(|o| o.value == v);
            if let Some(p) = b.page() {
                match idx {
                    Some(i) => p.forms_mut().select_option_as(id, i, true, true),
                    None => p.forms_mut().deselect_all(id),
                }
                p.mark_forms_changed();
            }
        }
        _ => set_attribute(b, r, n, "value", &v)?,
    }
    Ok(Value::UNDEFINED)
}

fn checked_get(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    Ok(Value::bool(ctl(b, n).is_some_and(|(_, c)| c.checked)))
}

fn checked_set(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let on = arg_bool(r, a, 0);
    if let Some((id, _)) = ctl(b, n)
        && let Some(p) = b.page()
    {
        p.forms_mut().set_checked_as(id, on, true);
        p.mark_forms_changed();
    }
    Ok(Value::UNDEFINED)
}

fn disabled_get(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    Ok(Value::bool(attr(b, n, "disabled").is_some()))
}

fn disabled_set(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    if arg_bool(r, a, 0) {
        set_attribute(b, r, n, "disabled", "")?;
    } else {
        remove_attribute(b, r, n, "disabled")?;
    }
    Ok(Value::UNDEFINED)
}

fn type_get(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let p = b.page_or(r)?;
    let s = if p.dom.is_html(n, tag::INPUT) {
        input_type(b, n)
    } else if p.dom.is_html(n, tag::BUTTON) {
        let t = attr(b, n, "type").unwrap_or_default().to_ascii_lowercase();
        String::from(match t.as_str() {
            "reset" => "reset",
            "button" => "button",
            _ => "submit",
        })
    } else if p.dom.is_html(n, tag::SELECT) {
        String::from(if attr(b, n, "multiple").is_some() {
            "select-multiple"
        } else {
            "select-one"
        })
    } else if p.dom.is_html(n, tag::TEXTAREA) {
        String::from("textarea")
    } else {
        attr(b, n, "type").unwrap_or_default()
    };
    Ok(r.string(&s))
}

fn type_set(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let v = arg_str(r, a, 0)?;
    set_attribute(b, r, n, "type", &v)?;
    Ok(Value::UNDEFINED)
}

fn default_value_get(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let p = b.page_or(r)?;
    let v = if p.dom.is_html(n, tag::TEXTAREA) {
        p.dom.text_content(n)
    } else {
        attr(b, n, "value").unwrap_or_default()
    };
    Ok(r.string(&v))
}

fn default_value_set(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let v = arg_str(r, a, 0)?;
    if b.page_or(r)?.dom.is_html(n, tag::TEXTAREA) {
        b.page_or(r)?.dom.set_text_content(n, &v);
    } else {
        set_attribute(b, r, n, "value", &v)?;
    }
    Ok(Value::UNDEFINED)
}

fn form_get(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let f = form_of(b, n);
    Ok(wrap(b, r, f))
}

/// The form owner of an element (a control, a hidden input, an option's select's).
fn form_of(b: &Bridge, n: NodeId) -> NodeId {
    let Some(p) = b.page() else { return NONE };
    p.sync_forms();
    if let Some(id) = p.control_of_node(n) {
        return p.form_node_of(id).unwrap_or(NONE);
    }
    if let Some(h) = p.forms().hidden.iter().find(|h| h.node == n) {
        return h
            .form
            .and_then(|f| p.forms().forms.get(f as usize).copied())
            .unwrap_or(NONE);
    }
    let mut c = p.dom.parent(n);
    while c != NONE {
        if p.dom.is_html(c, tag::FORM) {
            return c;
        }
        if p.dom.is_html(c, tag::SELECT) {
            return form_of(b, c);
        }
        c = p.dom.parent(c);
    }
    NONE
}

fn selection_len(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let len = ctl(b, n).map_or(0, |(_, c)| c.value.encode_utf16().count());
    Ok(Value::int(len as i32))
}

fn value_as_number(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let v = ctl(b, n).map(|(_, c)| c.value).unwrap_or_default();
    Ok(Value::number(v.trim().parse::<f64>().unwrap_or(f64::NAN)))
}

fn value_as_number_set(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let x = r.to_number(arg(a, 0))?;
    let s = if x.is_finite() { format!("{x}") } else { String::new() };
    let sv = r.string(&s);
    value_set(b, r, t, &[sv])
}

// ---- select and option ---------------------------------------------------------------------

/// An option's value: its `value` attribute, else its text (whitespace collapsed).
fn option_value(p: &Page, n: NodeId) -> String {
    match p.dom.element(n).and_then(|e| e.attr("value")) {
        Some(v) => String::from(v),
        None => collapse(&p.dom.text_content(n)),
    }
}

fn collapse(s: &str) -> String {
    s.split(crate::dom::is_html_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The select an option is in, and its index there.
fn option_place(b: &Bridge, n: NodeId) -> Option<(ControlId, usize)> {
    let p = b.page()?;
    p.sync_forms();
    let mut s = p.dom.parent(n);
    while s != NONE && !p.dom.is_html(s, tag::SELECT) {
        s = p.dom.parent(s);
    }
    let id = p.control_of_node(s)?;
    let i = p.forms().option_nodes[id.0 as usize]
        .iter()
        .position(|&o| o == n)?;
    Some((id, i))
}

fn selected_index_get(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let i = ctl(b, n)
        .and_then(|(_, c)| c.options.iter().position(|o| o.selected))
        .map_or(-1, |i| i as i32);
    Ok(Value::int(i))
}

fn selected_index_set(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let i = r.to_int32(arg(a, 0))?;
    if let Some((id, c)) = ctl(b, n)
        && let Some(p) = b.page()
    {
        if i >= 0 && (i as usize) < c.options.len() {
            if !c.multiple {
                p.forms_mut().select_option_as(id, i as usize, true, true);
            } else {
                p.forms_mut().deselect_all(id);
                p.forms_mut().select_option_as(id, i as usize, true, true);
            }
        } else {
            p.forms_mut().deselect_all(id);
        }
        p.mark_forms_changed();
    }
    Ok(Value::UNDEFINED)
}

fn options_get(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    nodes::live_list(b, r, ListKind::Options(n), true)
}

fn selected_options(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let sel: Vec<NodeId> = match (ctl(b, n), b.page()) {
        (Some((id, c)), Some(p)) => c
            .options
            .iter()
            .enumerate()
            .filter(|(_, o)| o.selected)
            .filter_map(|(i, _)| p.forms().option_nodes[id.0 as usize].get(i).copied())
            .collect(),
        _ => Vec::new(),
    };
    Ok(nodes::static_list(b, r, &sel))
}

fn select_length(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let k = ctl(b, n).map_or(0, |(_, c)| c.options.len());
    Ok(Value::int(k as i32))
}

fn select_add(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let o = node_of(b, r, arg(a, 0)).ok_or_else(|| type_error(r, "add: not an option"))?;
    let before = match arg(a, 1) {
        v if v.is_nullish() => NONE,
        v => match node_of(b, r, v) {
            Some(x) => x,
            None => {
                let i = r.to_int32(v)?;
                let opts = nodes::list_nodes(b.page_or(r)?, &ListKind::Options(n));
                opts.get(i.max(0) as usize).copied().unwrap_or(NONE)
            }
        },
    };
    let parent = if before == NONE { n } else { b.page_or(r)?.dom.parent(before) };
    nodes::insert(b, r, parent, o, before)?;
    Ok(Value::UNDEFINED)
}

fn select_remove(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    if a.is_empty() {
        // Element.remove().
        b.page_or(r)?.dom.detach(n);
        return Ok(Value::UNDEFINED);
    }
    let i = r.to_int32(arg(a, 0))?;
    let opts = nodes::list_nodes(b.page_or(r)?, &ListKind::Options(n));
    if i >= 0
        && let Some(&o) = opts.get(i as usize)
    {
        b.page_or(r)?.dom.detach(o);
    }
    Ok(Value::UNDEFINED)
}

fn select_item(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let i = r.to_int32(arg(a, 0))?;
    let opts = nodes::list_nodes(b.page_or(r)?, &ListKind::Options(n));
    match opts.get(i.max(0) as usize) {
        Some(&o) if i >= 0 => Ok(wrap(b, r, o)),
        _ => Ok(Value::NULL),
    }
}

fn option_selected_get(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let sel = match option_place(b, n) {
        Some((id, i)) => b
            .page()
            .is_some_and(|p| p.controls()[id.0 as usize].options[i].selected),
        None => attr(b, n, "selected").is_some(),
    };
    Ok(Value::bool(sel))
}

fn option_selected_set(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let on = arg_bool(r, a, 0);
    if let Some((id, i)) = option_place(b, n)
        && let Some(p) = b.page()
    {
        let multiple = p.controls()[id.0 as usize].multiple;
        if on || multiple {
            p.forms_mut().select_option_as(id, i, on, true);
        } else {
            p.forms_mut().deselect_all(id);
        }
        p.mark_forms_changed();
    }
    Ok(Value::UNDEFINED)
}

fn option_index(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    Ok(Value::int(option_place(b, n).map_or(0, |(_, i)| i as i32)))
}

fn option_text_get(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let s = collapse(&b.page_or(r)?.dom.text_content(n));
    Ok(r.string(&s))
}

fn option_label_get(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let s = attr(b, n, "label").unwrap_or_else(|| {
        b.page()
            .map(|p| collapse(&p.dom.text_content(n)))
            .unwrap_or_default()
    });
    Ok(r.string(&s))
}

// ---- forms ---------------------------------------------------------------------------------

fn this_form(b: &Bridge, r: &mut Realm, t: Value) -> JsResult<NodeId> {
    let n = this_node(b, r, t)?;
    if b.page_or(r)?.dom.is_html(n, tag::FORM) {
        Ok(n)
    } else {
        Err(type_error(r, "Illegal invocation (not a form)"))
    }
}

fn form_elements(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_form(b, r, t)?;
    nodes::live_list(b, r, ListKind::FormElements(n), true)
}

fn form_length(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_form(b, r, t)?;
    let p = b.page_or(r)?;
    p.sync_forms();
    let k = nodes::list_nodes(p, &ListKind::FormElements(n)).len();
    Ok(Value::int(k as i32))
}

fn form_action(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_form(b, r, t)?;
    let p = b.page_or(r)?;
    let s = match attr(b, n, "action").filter(|a| !a.trim().is_empty()) {
        Some(a) => p.resolve(a.trim()),
        None => String::from(p.page_url()),
    };
    Ok(r.string(&s))
}

fn form_method(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_form(b, r, t)?;
    let m = attr(b, n, "method").unwrap_or_default().trim().to_ascii_lowercase();
    let m = match m.as_str() {
        "post" => "post",
        "dialog" => "dialog",
        _ => "get",
    };
    Ok(r.string(m))
}

fn form_submit(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_form(b, r, t)?;
    // form.submit(): no submit event, no validation.
    if let Some(sub) = b.page().and_then(|p| {
        p.sync_forms();
        p.submit_form_node(n, None)
    }) {
        b.request(PageRequest::Submit(sub));
    }
    Ok(Value::UNDEFINED)
}

fn form_request_submit(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_form(b, r, t)?;
    let submitter = match node_of(b, r, arg(a, 0)) {
        Some(s) => {
            let ok = ctl(b, s).is_some_and(|(id, c)| {
                matches!(c.kind, ControlKind::Submit | ControlKind::ImageSubmit)
                    && b.page().and_then(|p| p.form_node_of(id)) == Some(n)
            });
            if !ok {
                return Err(dom_error(r, "NotFoundError", "requestSubmit: not a submit button of this form"));
            }
            s
        }
        None => NONE,
    };
    submit_with_events(b, r, n, submitter);
    Ok(Value::UNDEFINED)
}

fn form_reset(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_form(b, r, t)?;
    if events_js::fire_simple(b, r, n, "reset", true, true)
        && let Some(p) = b.page()
    {
        p.sync_forms();
        if let Some(fi) = p.forms().forms.iter().position(|&f| f == n) {
            p.forms_mut().reset_form(fi as u32);
            p.mark_forms_changed();
        }
    }
    Ok(Value::UNDEFINED)
}

/// A submit button's click (or requestSubmit): validation, the submit event, then the
/// submission for the app.
pub fn submit_from(b: &Rc<Bridge>, r: &mut Realm, id: ControlId, _events: bool) {
    let (form, node) = match b.page() {
        Some(p) => (p.form_node_of(id), p.control_node(id)),
        None => return,
    };
    if let (Some(f), Some(n)) = (form, node) {
        submit_with_events(b, r, f, n);
    }
}

fn submit_with_events(b: &Rc<Bridge>, r: &mut Realm, form: NodeId, submitter: NodeId) {
    if !validate_form(b, r, form, submitter) {
        return;
    }
    if !events_js::fire_submit(b, r, form, submitter) {
        return;
    }
    let sub = b.page().and_then(|p| {
        p.sync_forms();
        let sid = if submitter == NONE {
            None
        } else {
            p.control_of_node(submitter)
        };
        p.submit_form_node(form, sid)
    });
    if let Some(s) = sub {
        b.request(PageRequest::Submit(s));
    }
}

// ---- validation ----------------------------------------------------------------------------

/// The checks that need no script: custom messages are added by the bindings.
pub fn basic_invalid(p: &mut Page, n: NodeId) -> Option<String> {
    p.sync_forms();
    let id = p.control_of_node(n)?;
    let c = p.controls()[id.0 as usize].clone();
    let e = p.dom.element(n)?;
    if c.disabled || c.readonly || matches!(c.kind, ControlKind::Submit | ControlKind::Reset | ControlKind::Button | ControlKind::ImageSubmit) {
        return None;
    }
    // Inside a <datalist>: barred.
    let required = e.has_attr("required");
    match c.kind {
        ControlKind::Checkbox if required && !c.checked => {
            return Some(String::from("Please tick this box if you want to go on."));
        }
        ControlKind::Radio if required => {
            let any = p.controls().iter().any(|o| {
                o.kind == ControlKind::Radio && o.name == c.name && o.form == c.form && o.checked
            });
            if !any {
                return Some(String::from("Please choose one of these options."));
            }
            return None;
        }
        ControlKind::Select if required => {
            let sel = c.options.iter().position(|o| o.selected);
            let empty = match sel {
                None => true,
                Some(i) => c.options[i].value.is_empty(),
            };
            if empty {
                return Some(String::from("Please choose an item in the list."));
            }
            return None;
        }
        ControlKind::File if required => {
            return Some(String::from("Please choose a file."));
        }
        _ => {}
    }
    if !is_text_kind(c.kind) {
        return None;
    }
    let v = &c.value;
    if required && v.is_empty() {
        return Some(String::from("Please fill in this field."));
    }
    if v.is_empty() {
        return None;
    }
    let len = v.encode_utf16().count();
    if let Some(min) = e.attr("minlength").and_then(|m| m.trim().parse::<usize>().ok())
        && len < min
    {
        return Some(format!(
            "Please use at least {min} characters (you are using {len})."
        ));
    }
    match c.input_type.as_str() {
        "email" => {
            let ok = |a: &str| {
                let mut parts = a.trim().splitn(2, '@');
                let (l, d) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                !l.is_empty()
                    && !d.is_empty()
                    && !d.starts_with('.')
                    && !d.ends_with('.')
                    && !a.contains(char::is_whitespace)
                    && !d.contains('@')
            };
            let all_ok = if e.has_attr("multiple") {
                v.split(',').all(ok)
            } else {
                ok(v)
            };
            if !all_ok {
                return Some(String::from("Please enter an email address."));
            }
        }
        "url" => {
            let t = v.trim();
            let ok = t
                .split_once(':')
                .is_some_and(|(s, rest)| !s.is_empty() && s.chars().all(|ch| ch.is_ascii_alphanumeric() || "+-.".contains(ch)) && !rest.is_empty());
            if !ok {
                return Some(String::from("Please enter a URL."));
            }
        }
        "number" | "range" => {
            let Ok(x) = v.trim().parse::<f64>() else {
                return Some(String::from("Please enter a number."));
            };
            if let Some(min) = e.attr("min").and_then(|m| m.trim().parse::<f64>().ok())
                && x < min
            {
                return Some(format!("Value must be greater than or equal to {min}."));
            }
            if let Some(max) = e.attr("max").and_then(|m| m.trim().parse::<f64>().ok())
                && x > max
            {
                return Some(format!("Value must be less than or equal to {max}."));
            }
        }
        _ => {}
    }
    None
}

/// The pattern attribute, checked with the page's RegExp (`^(?:pattern)$`, the `v` flag
/// approximated with `u`).
fn pattern_mismatch(b: &Bridge, r: &mut Realm, n: NodeId) -> bool {
    let Some(pat) = attr(b, n, "pattern") else {
        return false;
    };
    let Some((_, c)) = ctl(b, n) else { return false };
    if !matches!(c.kind, ControlKind::Text | ControlKind::Password) || c.value.is_empty() {
        return false;
    }
    let g = r.global();
    let Ok(re) = r.get(g, "RegExp") else { return false };
    let src = r.string(&format!("^(?:{pat})$"));
    let flags = r.string("u");
    let Ok(rx) = r.construct(re, &[src, flags]) else {
        return false; // an invalid pattern is ignored
    };
    let val = r.string(&c.value);
    match r.call_method(rx, "test", &[val]) {
        Ok(v) => v.as_bool() == Some(false),
        Err(_) => false,
    }
}

fn custom_message(b: &Bridge, n: NodeId) -> Option<String> {
    b.st.borrow().custom_validity.get(&n).cloned().filter(|s| !s.is_empty())
}

/// The full check for one control: custom message, the basic checks, the pattern.
pub fn check(b: &Bridge, r: &mut Realm, n: NodeId) -> Option<String> {
    if let Some(m) = custom_message(b, n) {
        return Some(m);
    }
    if let Some(p) = b.page()
        && let Some(m) = basic_invalid(p, n)
    {
        return Some(m);
    }
    if pattern_mismatch(b, r, n) {
        let title = attr(b, n, "title").filter(|t| !t.is_empty());
        return Some(match title {
            Some(t) => format!("Please match the requested format: {t}"),
            None => String::from("Please match the requested format."),
        });
    }
    None
}

/// Interactive validation of `form` before a submission: `invalid` at each invalid control,
/// the focus to the first, its message for the app. True if the form may be submitted.
pub fn validate_form(b: &Rc<Bridge>, r: &mut Realm, form: NodeId, submitter: NodeId) -> bool {
    let skip = attr(b, form, "novalidate").is_some()
        || (submitter != NONE && attr(b, submitter, "formnovalidate").is_some());
    if skip {
        return true;
    }
    let controls = match b.page() {
        Some(p) => {
            p.sync_forms();
            nodes::list_nodes(p, &ListKind::FormElements(form))
        }
        None => return true,
    };
    let mut first: Option<(NodeId, String)> = None;
    for n in controls {
        if let Some(m) = check(b, r, n) {
            let e = events_js::new_event(
                b,
                r,
                &b.protos().event,
                "invalid",
                events_js::TRUSTED | events_js::CANCELABLE,
            );
            let shown = events_js::dispatch(b, r, n, e);
            if first.is_none() && shown {
                first = Some((n, m));
            } else if first.is_none() {
                first = Some((n, String::new()));
            }
        }
    }
    match first {
        None => true,
        Some((n, m)) => {
            events_js::move_focus(b, r, n);
            if !m.is_empty() {
                b.request(PageRequest::Invalid(m));
            }
            false
        }
    }
}

fn validity_get(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let custom = custom_message(b, n).is_some();
    let basic = b.page().and_then(|p| basic_invalid(p, n));
    let pattern = pattern_mismatch(b, r, n);
    let o = r.new_object();
    let m = basic.clone().unwrap_or_default();
    let missing = m.starts_with("Please fill") || m.starts_with("Please tick") || m.starts_with("Please choose");
    let mismatch = m.starts_with("Please enter an email") || m.starts_with("Please enter a URL");
    let short = m.starts_with("Please use at least");
    let under = m.starts_with("Value must be greater");
    let over = m.starts_with("Value must be less");
    let bad = m.starts_with("Please enter a number");
    for (k, v) in [
        ("valueMissing", missing),
        ("typeMismatch", mismatch),
        ("patternMismatch", pattern),
        ("tooLong", false),
        ("tooShort", short),
        ("rangeUnderflow", under),
        ("rangeOverflow", over),
        ("stepMismatch", false),
        ("badInput", bad),
        ("customError", custom),
        ("valid", basic.is_none() && !pattern && !custom),
    ] {
        r.define(o, k, Value::bool(v), Attrs::ENUMERABLE)?;
    }
    Ok(o)
}

fn validation_message(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let m = check(b, r, n).unwrap_or_default();
    Ok(r.string(&m))
}

fn set_custom_validity(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let m = arg_str(r, a, 0)?;
    let mut st = b.st(r)?;
    if m.is_empty() {
        st.custom_validity.remove(&n);
    } else if st.custom_validity.len() < 10_000 {
        st.custom_validity.insert(n, m);
    }
    Ok(Value::UNDEFINED)
}

fn check_validity(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    if b.page_or(r)?.dom.is_html(n, tag::FORM) {
        let controls = {
            let p = b.page_or(r)?;
            p.sync_forms();
            nodes::list_nodes(p, &ListKind::FormElements(n))
        };
        let mut ok = true;
        for c in controls {
            if check(b, r, c).is_some() {
                ok = false;
                events_js::fire_simple(b, r, c, "invalid", false, true);
            }
        }
        return Ok(Value::bool(ok));
    }
    if check(b, r, n).is_some() {
        events_js::fire_simple(b, r, n, "invalid", false, true);
        return Ok(Value::FALSE);
    }
    Ok(Value::TRUE)
}

fn report_validity(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    if b.page_or(r)?.dom.is_html(n, tag::FORM) {
        return Ok(Value::bool(validate_form(b, r, n, NONE)));
    }
    let ok = check_validity(b, r, t, a)?;
    if ok.as_bool() == Some(false)
        && let Some(m) = check(b, r, n)
    {
        events_js::move_focus(b, r, n);
        b.request(PageRequest::Invalid(m));
    }
    Ok(ok)
}

fn will_validate(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let ok = ctl(b, n).is_some_and(|(_, c)| {
        !c.disabled
            && !c.readonly
            && !matches!(c.kind, ControlKind::Reset | ControlKind::Button)
    });
    Ok(Value::bool(ok))
}

fn label_control(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let c = {
        let p = b.page_or(r)?;
        p.sync_forms();
        nodes::label_control(p, n).unwrap_or(NONE)
    };
    Ok(wrap(b, r, c))
}

fn noop(_: &Rc<Bridge>, _: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    Ok(Value::UNDEFINED)
}

/// The members of the form element prototypes.
pub fn install(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<()> {
    let pr = b.protos();
    let find = |t| pr.by_tag.iter().find(|(x, _)| *x == t).map(|(_, p)| p);
    let validation = |b: &Rc<Bridge>, r: &mut Realm, o: Value| -> JsResult<()> {
        getter(b, r, o, "validity", validity_get, None)?;
        getter(b, r, o, "validationMessage", validation_message, None)?;
        getter(b, r, o, "willValidate", will_validate, None)?;
        method(b, r, o, "checkValidity", 0, check_validity)?;
        method(b, r, o, "reportValidity", 0, report_validity)?;
        method(b, r, o, "setCustomValidity", 1, set_custom_validity)?;
        getter(b, r, o, "form", form_get, None)?;
        getter(b, r, o, "disabled", disabled_get, Some(disabled_set))?;
        Ok(())
    };
    let str_attr = |b: &Rc<Bridge>, r: &mut Realm, o: Value, prop: &'static str, at: &'static str| -> JsResult<()> {
        let b2 = b.clone();
        let b3 = b.clone();
        let set: alloc::boxed::Box<jsengine::NativeFn> =
            alloc::boxed::Box::new(move |r: &mut Realm, t: Value, a: &[Value]| {
                let n = this_node(&b3, r, t)?;
                let v = arg_str(r, a, 0)?;
                set_attribute(&b3, r, n, at, &v)?;
                Ok(Value::UNDEFINED)
            });
        r.define_getter(
            o,
            prop,
            move |r, t, _| {
                let n = this_node(&b2, r, t)?;
                let v = attr(&b2, n, at).unwrap_or_default();
                Ok(r.string(&v))
            },
            Some(set),
        )
    };
    let bool_attr = |b: &Rc<Bridge>, r: &mut Realm, o: Value, prop: &'static str, at: &'static str| -> JsResult<()> {
        let b2 = b.clone();
        let b3 = b.clone();
        let set: alloc::boxed::Box<jsengine::NativeFn> =
            alloc::boxed::Box::new(move |r: &mut Realm, t: Value, a: &[Value]| {
                let n = this_node(&b3, r, t)?;
                if arg_bool(r, a, 0) {
                    set_attribute(&b3, r, n, at, "")?;
                } else {
                    remove_attribute(&b3, r, n, at)?;
                }
                Ok(Value::UNDEFINED)
            });
        r.define_getter(
            o,
            prop,
            move |r, t, _| {
                let n = this_node(&b2, r, t)?;
                Ok(Value::bool(attr(&b2, n, at).is_some()))
            },
            Some(set),
        )
    };
    let int_attr = |b: &Rc<Bridge>, r: &mut Realm, o: Value, prop: &'static str, at: &'static str, def: i32| -> JsResult<()> {
        let b2 = b.clone();
        let b3 = b.clone();
        let set: alloc::boxed::Box<jsengine::NativeFn> =
            alloc::boxed::Box::new(move |r: &mut Realm, t: Value, a: &[Value]| {
                let n = this_node(&b3, r, t)?;
                let v = r.to_int32(arg(a, 0))?;
                set_attribute(&b3, r, n, at, &format!("{v}"))?;
                Ok(Value::UNDEFINED)
            });
        r.define_getter(
            o,
            prop,
            move |r, t, _| {
                let n = this_node(&b2, r, t)?;
                let v = attr(&b2, n, at).and_then(|v| v.trim().parse().ok()).unwrap_or(def);
                Ok(Value::int(v))
            },
            Some(set),
        )
    };

    if let Some(p) = find(tag::INPUT) {
        let o = r.pinned(p);
        validation(b, r, o)?;
        getter(b, r, o, "value", value_get, Some(value_set))?;
        getter(b, r, o, "defaultValue", default_value_get, Some(default_value_set))?;
        getter(b, r, o, "checked", checked_get, Some(checked_set))?;
        bool_attr(b, r, o, "defaultChecked", "checked")?;
        getter(b, r, o, "type", type_get, Some(type_set))?;
        getter(b, r, o, "valueAsNumber", value_as_number, Some(value_as_number_set))?;
        getter(b, r, o, "selectionStart", selection_len, Some(noop))?;
        getter(b, r, o, "selectionEnd", selection_len, Some(noop))?;
        getter(b, r, o, "files", |_, _, _, _| Ok(Value::NULL), None)?;
        getter(b, r, o, "list", |_, _, _, _| Ok(Value::NULL), None)?;
        for (prop, at) in [
            ("name", "name"),
            ("placeholder", "placeholder"),
            ("min", "min"),
            ("max", "max"),
            ("step", "step"),
            ("pattern", "pattern"),
            ("autocomplete", "autocomplete"),
            ("accept", "accept"),
            ("alt", "alt"),
            ("inputMode", "inputmode"),
        ] {
            str_attr(b, r, o, prop, at)?;
        }
        for (prop, at) in [
            ("required", "required"),
            ("readOnly", "readonly"),
            ("multiple", "multiple"),
            ("autofocus", "autofocus"),
            ("formNoValidate", "formnovalidate"),
        ] {
            bool_attr(b, r, o, prop, at)?;
        }
        int_attr(b, r, o, "maxLength", "maxlength", -1)?;
        int_attr(b, r, o, "minLength", "minlength", -1)?;
        int_attr(b, r, o, "size", "size", 20)?;
        method(b, r, o, "select", 0, noop)?;
        method(b, r, o, "setSelectionRange", 2, noop)?;
        method(b, r, o, "stepUp", 0, noop)?;
        method(b, r, o, "stepDown", 0, noop)?;
    }
    if let Some(p) = find(tag::TEXTAREA) {
        let o = r.pinned(p);
        validation(b, r, o)?;
        getter(b, r, o, "value", value_get, Some(value_set))?;
        getter(b, r, o, "defaultValue", default_value_get, Some(default_value_set))?;
        getter(b, r, o, "type", type_get, None)?;
        getter(b, r, o, "textLength", selection_len, None)?;
        getter(b, r, o, "selectionStart", selection_len, Some(noop))?;
        getter(b, r, o, "selectionEnd", selection_len, Some(noop))?;
        str_attr(b, r, o, "name", "name")?;
        str_attr(b, r, o, "placeholder", "placeholder")?;
        bool_attr(b, r, o, "required", "required")?;
        bool_attr(b, r, o, "readOnly", "readonly")?;
        int_attr(b, r, o, "rows", "rows", 2)?;
        int_attr(b, r, o, "cols", "cols", 20)?;
        int_attr(b, r, o, "maxLength", "maxlength", -1)?;
        int_attr(b, r, o, "minLength", "minlength", -1)?;
        method(b, r, o, "select", 0, noop)?;
        method(b, r, o, "setSelectionRange", 2, noop)?;
    }
    if let Some(p) = find(tag::SELECT) {
        let o = r.pinned(p);
        validation(b, r, o)?;
        getter(b, r, o, "value", value_get, Some(value_set))?;
        getter(b, r, o, "selectedIndex", selected_index_get, Some(selected_index_set))?;
        getter(b, r, o, "options", options_get, None)?;
        getter(b, r, o, "selectedOptions", selected_options, None)?;
        getter(b, r, o, "length", select_length, None)?;
        getter(b, r, o, "type", type_get, None)?;
        str_attr(b, r, o, "name", "name")?;
        bool_attr(b, r, o, "multiple", "multiple")?;
        bool_attr(b, r, o, "required", "required")?;
        int_attr(b, r, o, "size", "size", 0)?;
        method(b, r, o, "add", 2, select_add)?;
        method(b, r, o, "remove", 1, select_remove)?;
        method(b, r, o, "item", 1, select_item)?;
    }
    if let Some(p) = find(tag::OPTION) {
        let o = r.pinned(p);
        getter(b, r, o, "value", value_get, Some(value_set))?;
        getter(b, r, o, "selected", option_selected_get, Some(option_selected_set))?;
        bool_attr(b, r, o, "defaultSelected", "selected")?;
        bool_attr(b, r, o, "disabled", "disabled")?;
        getter(b, r, o, "index", option_index, None)?;
        getter(b, r, o, "text", option_text_get, Some(|b, r, t, a| {
            let n = this_node(b, r, t)?;
            let s = arg_str(r, a, 0)?;
            b.page_or(r)?.dom.set_text_content(n, &s);
            Ok(Value::UNDEFINED)
        }))?;
        getter(b, r, o, "label", option_label_get, None)?;
        getter(b, r, o, "form", form_get, None)?;
    }
    if let Some(p) = find(tag::BUTTON) {
        let o = r.pinned(p);
        validation(b, r, o)?;
        getter(b, r, o, "type", type_get, Some(type_set))?;
        getter(b, r, o, "value", value_get, Some(value_set))?;
        str_attr(b, r, o, "name", "name")?;
        bool_attr(b, r, o, "formNoValidate", "formnovalidate")?;
    }
    if let Some(p) = find(tag::FORM) {
        let o = r.pinned(p);
        getter(b, r, o, "elements", form_elements, None)?;
        getter(b, r, o, "length", form_length, None)?;
        getter(b, r, o, "action", form_action, Some(|b, r, t, a| {
            let n = this_node(b, r, t)?;
            let v = arg_str(r, a, 0)?;
            set_attribute(b, r, n, "action", &v)?;
            Ok(Value::UNDEFINED)
        }))?;
        getter(b, r, o, "method", form_method, Some(|b, r, t, a| {
            let n = this_node(b, r, t)?;
            let v = arg_str(r, a, 0)?;
            set_attribute(b, r, n, "method", &v)?;
            Ok(Value::UNDEFINED)
        }))?;
        str_attr(b, r, o, "name", "name")?;
        str_attr(b, r, o, "target", "target")?;
        str_attr(b, r, o, "enctype", "enctype")?;
        bool_attr(b, r, o, "noValidate", "novalidate")?;
        method(b, r, o, "submit", 0, form_submit)?;
        method(b, r, o, "requestSubmit", 0, form_request_submit)?;
        method(b, r, o, "reset", 0, form_reset)?;
        method(b, r, o, "checkValidity", 0, check_validity)?;
        method(b, r, o, "reportValidity", 0, report_validity)?;
    }
    if let Some(p) = find(tag::LABEL) {
        let o = r.pinned(p);
        str_attr(b, r, o, "htmlFor", "for")?;
        getter(b, r, o, "control", label_control, None)?;
        getter(b, r, o, "form", form_get, None)?;
    }
    if let Some(p) = find(tag::FIELDSET) {
        let o = r.pinned(p);
        getter(b, r, o, "disabled", disabled_get, Some(disabled_set))?;
        str_attr(b, r, o, "name", "name")?;
    }
    Ok(())
}
