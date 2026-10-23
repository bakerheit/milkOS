//! Form controls (HTML §4.10): collecting them from the DOM, their state, and form submission
//! (§4.10.21.3 "form submission algorithm", §4.10.21.4 "constructing the entry list", encoded
//! as application/x-www-form-urlencoded).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use crate::api::*;
use crate::atoms::tag;
use crate::dom::{Dom, NONE, NodeId};

/// A control that is submitted but never drawn (`<input type=hidden>`).
#[derive(Clone, Debug)]
pub struct Hidden {
    pub node: NodeId,
    /// Its place in tree order among the controls (node ids aren't, once scripts move nodes).
    pub seq: u32,
    pub name: String,
    pub value: String,
    pub form: Option<u32>,
}

pub struct Forms {
    pub controls: Vec<FormControl>,
    /// The element of each control.
    pub nodes: Vec<NodeId>,
    /// Each control's place in tree order (shared numbering with [`Hidden::seq`]).
    pub seqs: Vec<u32>,
    pub by_node: BTreeMap<NodeId, (ControlId, ControlKind)>,
    pub hidden: Vec<Hidden>,
    /// `<form>` elements in tree order.
    pub forms: Vec<NodeId>,
    /// The initial state, for reset.
    initial: Vec<FormControl>,
    /// A `<select>`'s option elements, per control (empty for the other kinds).
    pub option_nodes: Vec<Vec<NodeId>>,
    /// Controls whose value / checkedness / selectedness was changed by the user or a script
    /// (HTML's "dirty" flags): recollecting after the DOM changes keeps their state instead of
    /// taking it from the attributes again.
    dirty_value: BTreeSet<NodeId>,
    dirty_checked: BTreeSet<NodeId>,
    dirty_select: BTreeSet<NodeId>,
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    for w in s.split(crate::dom::is_html_space) {
        if w.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

fn default_font() -> Font {
    Font {
        family: Family::SansSerif,
        size: 13,
        weight: 400,
        italic: false,
    }
}

/// The form owner of `n` (HTML §4.10.17.3): the `form` attribute's form, else the nearest
/// form ancestor.
fn owner(dom: &Dom, n: NodeId, forms: &[NodeId]) -> Option<u32> {
    let e = dom.element(n)?;
    if let Some(fid) = e.attr("form") {
        let a = dom.atoms.get(fid)?;
        return forms
            .iter()
            .position(|&f| dom.element(f).is_some_and(|fe| fe.id == Some(a)))
            .map(|i| i as u32);
    }
    let mut p = dom.parent(n);
    while p != NONE {
        if dom.is_html(p, tag::FORM) {
            return forms.iter().position(|&f| f == p).map(|i| i as u32);
        }
        p = dom.parent(p);
    }
    None
}

pub fn collect(dom: &Dom, max: usize) -> Forms {
    let mut forms = Vec::new();
    let mut n = 0;
    while n != NONE {
        if dom.is_html(n, tag::FORM) {
            forms.push(n);
        }
        n = dom.next_in(n, 0);
    }
    let mut f = Forms {
        controls: Vec::new(),
        nodes: Vec::new(),
        by_node: BTreeMap::new(),
        hidden: Vec::new(),
        seqs: Vec::new(),
        forms,
        initial: Vec::new(),
        option_nodes: Vec::new(),
        dirty_value: BTreeSet::new(),
        dirty_checked: BTreeSet::new(),
        dirty_select: BTreeSet::new(),
    };
    let mut n = 0;
    while n != NONE {
        let next = dom.next_in(n, 0);
        let Some(e) = dom.element(n) else {
            n = next;
            continue;
        };
        if e.is(tag::TEMPLATE) {
            n = dom.next_skipping_children(n, 0);
            continue;
        }
        if f.controls.len() >= max {
            break;
        }
        let name = String::from(e.attr("name").unwrap_or(""));
        let disabled = e.has_attr("disabled");
        let base = |kind: ControlKind, input_type: &str| FormControl {
            id: ControlId(0),
            kind,
            input_type: String::from(input_type),
            name: name.clone(),
            value: String::new(),
            checked: false,
            disabled,
            readonly: e.has_attr("readonly"),
            required: e.has_attr("required"),
            placeholder: collapse_ws(e.attr("placeholder").unwrap_or("")),
            label: String::new(),
            options: Vec::new(),
            multiple: e.has_attr("multiple"),
            max_length: e.attr("maxlength").and_then(|v| v.trim().parse().ok()),
            rows: 0,
            cols: 0,
            image: None,
            rect: Rect::default(),
            content_drawn: false,
            font: default_font(),
            color: 0xFF00_0000,
            form: owner(dom, n, &f.forms),
        };
        let mut opt_nodes = Vec::new();
        let ctl = if e.is(tag::INPUT) {
            let t = e.attr("type").unwrap_or("text").trim().to_ascii_lowercase();
            let value = String::from(e.attr("value").unwrap_or(""));
            match t.as_str() {
                "hidden" => {
                    f.hidden.push(Hidden {
                        node: n,
                        seq: (f.nodes.len() + f.hidden.len()) as u32,
                        name: name.clone(),
                        value,
                        form: owner(dom, n, &f.forms),
                    });
                    None
                }
                "checkbox" | "radio" => {
                    let mut c = base(
                        if t == "checkbox" {
                            ControlKind::Checkbox
                        } else {
                            ControlKind::Radio
                        },
                        &t,
                    );
                    c.value = if e.has_attr("value") {
                        value
                    } else {
                        String::from("on")
                    };
                    c.checked = e.has_attr("checked");
                    Some(c)
                }
                "submit" | "reset" | "button" | "image" => {
                    let kind = match t.as_str() {
                        "submit" => ControlKind::Submit,
                        "reset" => ControlKind::Reset,
                        "button" => ControlKind::Button,
                        _ => ControlKind::ImageSubmit,
                    };
                    let mut c = base(kind, &t);
                    c.label = if e.has_attr("value") {
                        value.clone()
                    } else {
                        String::from(match kind {
                            ControlKind::Reset => "Reset",
                            ControlKind::Button => "",
                            ControlKind::ImageSubmit => e.attr("alt").unwrap_or("Submit"),
                            _ => "Submit",
                        })
                    };
                    c.value = value;
                    Some(c)
                }
                "file" => Some(base(ControlKind::File, &t)),
                "password" => {
                    let mut c = base(ControlKind::Password, &t);
                    c.value = value;
                    Some(c)
                }
                _ => {
                    let mut c = base(ControlKind::Text, &t);
                    c.value = value.replace(['\n', '\r'], "");
                    Some(c)
                }
            }
        } else if e.is(tag::TEXTAREA) {
            let mut c = base(ControlKind::TextArea, "textarea");
            c.value = dom.text_content(n);
            c.rows = e
                .attr("rows")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(2);
            c.cols = e
                .attr("cols")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(20);
            Some(c)
        } else if e.is(tag::SELECT) {
            let mut c = base(ControlKind::Select, "select");
            let mut o = dom.next_in(n, n);
            while o != NONE {
                if let Some(oe) = dom.element(o)
                    && oe.is(tag::OPTION)
                {
                    let text = collapse_ws(&dom.text_content(o));
                    let in_disabled_group = dom
                        .element(dom.parent(o))
                        .is_some_and(|p| p.is(tag::OPTGROUP) && p.has_attr("disabled"));
                    opt_nodes.push(o);
                    c.options.push(SelectOption {
                        label: oe
                            .attr("label")
                            .map(collapse_ws)
                            .unwrap_or_else(|| text.clone()),
                        value: oe.attr("value").map(String::from).unwrap_or(text),
                        selected: oe.has_attr("selected"),
                        disabled: oe.has_attr("disabled") || in_disabled_group,
                    });
                    if c.options.len() >= 10_000 {
                        break;
                    }
                }
                o = dom.next_in(o, n);
            }
            c.rows = e
                .attr("size")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            // HTML §4.10.7 the selectedness setting algorithm: one selected option for a
            // single select (the last one marked, else the first enabled one).
            if !c.multiple {
                let last = c.options.iter().rposition(|o| o.selected);
                for (i, o) in c.options.iter_mut().enumerate() {
                    o.selected = Some(i) == last;
                }
                if last.is_none()
                    && c.rows <= 1
                    && let Some(i) = c.options.iter().position(|o| !o.disabled)
                {
                    c.options[i].selected = true;
                }
            }
            Some(c)
        } else if e.is(tag::BUTTON) {
            let t = e
                .attr("type")
                .unwrap_or("submit")
                .trim()
                .to_ascii_lowercase();
            let kind = match t.as_str() {
                "reset" => ControlKind::Reset,
                "button" => ControlKind::Button,
                _ => ControlKind::Submit,
            };
            let mut c = base(kind, &t);
            c.value = String::from(e.attr("value").unwrap_or(""));
            c.label = collapse_ws(&dom.text_content(n));
            c.content_drawn = true;
            Some(c)
        } else {
            None
        };
        if let Some(mut c) = ctl {
            let id = ControlId(f.controls.len() as u32);
            c.id = id;
            // Inside a disabled fieldset (HTML §4.10.19.2).
            let mut p = dom.parent(n);
            while p != NONE {
                if let Some(pe) = dom.element(p)
                    && pe.is(tag::FIELDSET)
                    && pe.has_attr("disabled")
                {
                    c.disabled = true;
                }
                p = dom.parent(p);
            }
            f.by_node.insert(n, (id, c.kind));
            f.seqs.push((f.nodes.len() + f.hidden.len()) as u32);
            f.nodes.push(n);
            f.controls.push(c);
            f.option_nodes.push(opt_nodes);
        }
        // Don't look inside controls (options belong to their select).
        n = if e.is(tag::SELECT) || e.is(tag::TEXTAREA) {
            dom.next_skipping_children(n, 0)
        } else {
            next
        };
    }
    f.initial = f.controls.clone();
    f
}

/// Collect the controls again after the DOM changed, keeping what the user (or a script)
/// changed: a dirty value, checkedness or selectedness stays; rects, fonts and colours stay
/// until the next layout.
pub fn recollect(dom: &Dom, old: &Forms, max: usize) -> Forms {
    let mut f = collect(dom, max);
    f.dirty_value = old.dirty_value.clone();
    f.dirty_checked = old.dirty_checked.clone();
    f.dirty_select = old.dirty_select.clone();
    for i in 0..f.controls.len() {
        let node = f.nodes[i];
        let Some(&(oid, okind)) = old.by_node.get(&node) else {
            continue;
        };
        let o = &old.controls[oid.0 as usize];
        let c = &mut f.controls[i];
        c.rect = o.rect;
        c.font = o.font;
        c.color = o.color;
        if okind != c.kind {
            continue;
        }
        if f.dirty_value.contains(&node) {
            c.value = o.value.clone();
        }
        if f.dirty_checked.contains(&node) {
            c.checked = o.checked;
        }
        if f.dirty_select.contains(&node) {
            let sel: Vec<NodeId> = old.option_nodes[oid.0 as usize]
                .iter()
                .zip(o.options.iter())
                .filter(|(_, op)| op.selected)
                .map(|(n, _)| *n)
                .collect();
            for (k, op) in c.options.iter_mut().enumerate() {
                op.selected = sel.contains(&f.option_nodes[i][k]);
            }
            if !c.multiple && !c.options.iter().any(|o| o.selected) && c.rows <= 1 {
                if let Some(k) = c.options.iter().position(|o| !o.disabled) {
                    c.options[k].selected = true;
                }
            }
        }
    }
    // Forget nodes that are no longer controls.
    let live: BTreeSet<NodeId> = f.nodes.iter().copied().collect();
    f.dirty_value.retain(|n| live.contains(n));
    f.dirty_checked.retain(|n| live.contains(n));
    f.dirty_select.retain(|n| live.contains(n));
    f
}

impl Forms {
    pub fn empty() -> Forms {
        Forms {
            controls: Vec::new(),
            nodes: Vec::new(),
            by_node: BTreeMap::new(),
            hidden: Vec::new(),
            seqs: Vec::new(),
            forms: Vec::new(),
            initial: Vec::new(),
            option_nodes: Vec::new(),
            dirty_value: BTreeSet::new(),
            dirty_checked: BTreeSet::new(),
            dirty_select: BTreeSet::new(),
        }
    }

    pub fn set_value(&mut self, id: ControlId, v: &str) {
        self.set_value_as(id, v, false)
    }

    /// `script`: a script's `value =` (disabled and read-only controls take it too, and
    /// `maxlength` isn't applied).
    pub fn set_value_as(&mut self, id: ControlId, v: &str, script: bool) {
        let Some(c) = self.controls.get_mut(id.0 as usize) else {
            return;
        };
        if !script && (c.disabled || c.readonly) {
            return;
        }
        if !matches!(
            c.kind,
            ControlKind::Text | ControlKind::Password | ControlKind::TextArea
        ) {
            return;
        }
        let mut v: String = if c.kind == ControlKind::TextArea {
            String::from(v)
        } else {
            v.replace(['\n', '\r'], "")
        };
        if !script && let Some(max) = c.max_length {
            v = v.chars().take(max as usize).collect();
        }
        c.value = v;
        self.dirty_value.insert(self.nodes[id.0 as usize]);
    }

    pub fn set_checked(&mut self, id: ControlId, on: bool) {
        self.set_checked_as(id, on, false)
    }

    pub fn set_checked_as(&mut self, id: ControlId, on: bool, script: bool) {
        let Some(c) = self.controls.get(id.0 as usize) else {
            return;
        };
        if c.disabled && !script {
            return;
        }
        match c.kind {
            ControlKind::Checkbox => self.controls[id.0 as usize].checked = on,
            ControlKind::Radio => {
                let (name, form) = (c.name.clone(), c.form);
                if on && !name.is_empty() {
                    // The radio button group (HTML §4.10.5.1.15): same name, same form owner.
                    for (i, o) in self.controls.iter_mut().enumerate() {
                        if o.kind == ControlKind::Radio && o.name == name && o.form == form {
                            o.checked = false;
                            self.dirty_checked.insert(self.nodes[i]);
                        }
                    }
                }
                self.controls[id.0 as usize].checked = on;
            }
            _ => return,
        }
        self.dirty_checked.insert(self.nodes[id.0 as usize]);
    }

    pub fn select_option(&mut self, id: ControlId, index: usize, on: bool) {
        self.select_option_as(id, index, on, false)
    }

    pub fn select_option_as(&mut self, id: ControlId, index: usize, on: bool, script: bool) {
        let Some(c) = self.controls.get_mut(id.0 as usize) else {
            return;
        };
        if c.kind != ControlKind::Select
            || (c.disabled && !script)
            || index >= c.options.len()
            || (c.options[index].disabled && !script)
        {
            return;
        }
        self.dirty_select.insert(self.nodes[id.0 as usize]);
        if c.multiple {
            c.options[index].selected = on;
        } else {
            for (i, o) in c.options.iter_mut().enumerate() {
                o.selected = i == index;
            }
        }
    }

    /// No option of a select selected (a script set a value no option has).
    pub fn deselect_all(&mut self, id: ControlId) {
        if let Some(c) = self.controls.get_mut(id.0 as usize)
            && c.kind == ControlKind::Select
        {
            for o in c.options.iter_mut() {
                o.selected = false;
            }
            self.dirty_select.insert(self.nodes[id.0 as usize]);
        }
    }

    pub fn reset(&mut self, id: ControlId) {
        let Some(form) = self.controls.get(id.0 as usize).and_then(|c| c.form) else {
            return;
        };
        self.reset_form(form);
    }

    /// Reset form number `form` (its controls back to their attributes).
    pub fn reset_form(&mut self, form: u32) {
        for i in 0..self.controls.len() {
            if self.controls[i].form == Some(form) {
                let node = self.nodes[i];
                self.dirty_value.remove(&node);
                self.dirty_checked.remove(&node);
                self.dirty_select.remove(&node);
                let rect = self.controls[i].rect;
                let font = self.controls[i].font;
                let color = self.controls[i].color;
                self.controls[i] = self.initial[i].clone();
                self.controls[i].rect = rect;
                self.controls[i].font = font;
                self.controls[i].color = color;
            }
        }
    }

    /// The form submission for activating `id`.
    pub fn submit(
        &self,
        dom: &Dom,
        doc_url: &str,
        base: &str,
        id: ControlId,
        click: Option<(i32, i32)>,
    ) -> Option<Submission> {
        let c = self.controls.get(id.0 as usize)?;
        let form_idx = c.form?;
        let submitter =
            matches!(c.kind, ControlKind::Submit | ControlKind::ImageSubmit).then_some(id);
        if c.kind == ControlKind::Reset || c.kind == ControlKind::Button {
            return None;
        }
        self.submit_form(dom, doc_url, base, form_idx, submitter, click)
    }

    /// The submission of form number `form_idx` (`form.submit()` has no submitter).
    pub fn submit_form(
        &self,
        dom: &Dom,
        doc_url: &str,
        base: &str,
        form_idx: u32,
        submitter: Option<ControlId>,
        click: Option<(i32, i32)>,
    ) -> Option<Submission> {
        let form_node = *self.forms.get(form_idx as usize)?;
        let fe = dom.element(form_node)?;
        let sub_el = submitter.and_then(|s| dom.element(self.nodes[s.0 as usize]));
        let pick = |a: &str, fa: &str| -> Option<String> {
            sub_el
                .and_then(|e| e.attr(a))
                .or_else(|| fe.attr(fa))
                .map(String::from)
        };
        let method = match pick("formmethod", "method")
            .map(|m| m.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("post") => Method::Post,
            Some("dialog") => return None,
            _ => Method::Get,
        };
        let action = pick("formaction", "action")
            .map(|a| String::from(a.trim()))
            .unwrap_or_default();
        let action = if action.is_empty() {
            String::from(doc_url)
        } else {
            crate::url::resolve(base, &action)
        };
        let target = pick("formtarget", "target");
        // §4.10.21.4 the entry list, in tree order.
        let mut entries: Vec<(String, String)> = Vec::new();
        let mut order: Vec<(u32, NodeId, usize, bool)> = Vec::new();
        for (i, ctl) in self.controls.iter().enumerate() {
            if ctl.form == Some(form_idx) {
                order.push((self.seqs[i], self.nodes[i], i, false));
            }
        }
        for (i, h) in self.hidden.iter().enumerate() {
            if h.form == Some(form_idx) {
                order.push((h.seq, h.node, i, true));
            }
        }
        order.sort_by_key(|o| o.0);
        for (_, node, i, hidden) in order {
            if hidden {
                let h = &self.hidden[i];
                if h.name.is_empty() || dom.element(node).is_some_and(|e| e.has_attr("disabled")) {
                    continue;
                }
                let v = if h.name.eq_ignore_ascii_case("_charset_") {
                    String::from("UTF-8")
                } else {
                    h.value.clone()
                };
                entries.push((h.name.clone(), v));
                continue;
            }
            let ctl = &self.controls[i];
            if ctl.disabled {
                continue;
            }
            match ctl.kind {
                ControlKind::Submit | ControlKind::ImageSubmit => {
                    if Some(ctl.id) != submitter {
                        continue;
                    }
                    if ctl.kind == ControlKind::ImageSubmit {
                        let (x, y) = click.unwrap_or((0, 0));
                        let prefix = if ctl.name.is_empty() {
                            String::new()
                        } else {
                            alloc::format!("{}.", ctl.name)
                        };
                        entries.push((alloc::format!("{prefix}x"), alloc::format!("{x}")));
                        entries.push((alloc::format!("{prefix}y"), alloc::format!("{y}")));
                        continue;
                    }
                    if !ctl.name.is_empty() {
                        entries.push((ctl.name.clone(), ctl.value.clone()));
                    }
                }
                ControlKind::Reset | ControlKind::Button | ControlKind::File => {}
                ControlKind::Checkbox | ControlKind::Radio => {
                    if ctl.checked && !ctl.name.is_empty() {
                        entries.push((ctl.name.clone(), ctl.value.clone()));
                    }
                }
                ControlKind::Select => {
                    if ctl.name.is_empty() {
                        continue;
                    }
                    for o in &ctl.options {
                        if o.selected && !o.disabled {
                            entries.push((ctl.name.clone(), o.value.clone()));
                        }
                    }
                }
                ControlKind::TextArea => {
                    if !ctl.name.is_empty() {
                        // Newlines are CR LF in submissions.
                        let v = ctl
                            .value
                            .replace("\r\n", "\n")
                            .replace('\r', "\n")
                            .replace('\n', "\r\n");
                        entries.push((ctl.name.clone(), v));
                    }
                }
                ControlKind::Text | ControlKind::Password => {
                    if !ctl.name.is_empty() {
                        entries.push((ctl.name.clone(), ctl.value.clone()));
                    }
                }
            }
        }
        let mut q = String::new();
        for (i, (k, v)) in entries.iter().enumerate() {
            if i > 0 {
                q.push('&');
            }
            crate::url::form_urlencode(k, &mut q);
            q.push('=');
            crate::url::form_urlencode(v, &mut q);
        }
        Some(match method {
            Method::Get => {
                // Replace the action's query, keep its fragment (§4.10.21.3 "mutate action URL").
                let (no_frag, frag) = match action.find('#') {
                    Some(i) => (&action[..i], Some(&action[i..])),
                    None => (action.as_str(), None),
                };
                let base = no_frag.split('?').next().unwrap_or(no_frag);
                let mut url = String::from(base);
                url.push('?');
                url.push_str(&q);
                if let Some(f) = frag {
                    url.push_str(f);
                }
                Submission {
                    method,
                    url,
                    body: Vec::new(),
                    content_type: "application/x-www-form-urlencoded",
                    target,
                }
            }
            Method::Post => Submission {
                method,
                url: action,
                body: q.into_bytes(),
                content_type: "application/x-www-form-urlencoded",
                target,
            },
        })
    }
}
