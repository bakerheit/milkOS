//! The global object (`window`) and what hangs off it: timers, animation frames,
//! microtasks, the console, location, history, navigator, screen, Web Storage, the dialogs,
//! getComputedStyle and matchMedia; plus the host classes with live keys (element.style,
//! classList, dataset, storage, computed style) and the realm's set-up ([`install`]).

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use jsengine::{Attrs, ErrorKind, HostClass, JsResult, Key, OwnedKey, Pinned, Realm, Value, hooks};

use super::bridge::{Rnd, Bridge, Classes, Protos, arg, arg_num, arg_str, dom_error, type_error};
use super::events_js;
use super::nodes::{self, getter, method, node_of, remove_attribute, set_attribute, wrap};
use crate::api::{ConsoleLevel, PageRequest};
use crate::atoms::{Atom, tag};
use crate::dom::{NONE, NodeId};

// ---- the host classes ----------------------------------------------------------------------

struct NodeClass;
impl HostClass for NodeClass {
    fn name(&self) -> &str {
        "Node"
    }
}

struct EventClass;
impl HostClass for EventClass {
    fn name(&self) -> &str {
        "Event"
    }
}

/// classList: indexed tokens of the class attribute.
struct TokensClass {
    b: Rc<Bridge>,
}
impl HostClass for TokensClass {
    fn name(&self) -> &str {
        "DOMTokenList"
    }
    fn hooks(&self) -> u8 {
        hooks::INDEXED
    }
    fn get(&self, r: &mut Realm, _o: Value, data: u64, key: Key<'_>) -> JsResult<Option<Value>> {
        if let Key::Index(i) = key {
            let t = tokens(&self.b, data as NodeId);
            return Ok(t.get(i as usize).map(|s| r.string(s)));
        }
        Ok(None)
    }
    fn keys(&self, _r: &mut Realm, _o: Value, data: u64) -> JsResult<Vec<OwnedKey>> {
        let n = tokens(&self.b, data as NodeId).len() as u32;
        Ok((0..n).map(OwnedKey::Index).collect())
    }
}

fn tokens(b: &Bridge, n: NodeId) -> Vec<String> {
    let Some(p) = b.page() else {
        return Vec::new();
    };
    let mut v: Vec<String> = Vec::new();
    for w in p
        .dom
        .element(n)
        .and_then(|e| e.attr("class"))
        .unwrap_or("")
        .split(crate::dom::is_html_space)
    {
        if !w.is_empty() && !v.iter().any(|x| x == w) {
            v.push(String::from(w));
        }
    }
    v
}

/// element.style: the style attribute's declarations as properties.
struct StyleClass {
    b: Rc<Bridge>,
}

const STYLE_NAMES: &[&str] = &[
    "cssText",
    "length",
    "item",
    "getPropertyValue",
    "getPropertyPriority",
    "setProperty",
    "removeProperty",
    "parentRule",
    "cssFloat",
    "constructor",
    "toString",
    "hasOwnProperty",
    "valueOf",
    "toJSON",
    "then",
];

impl HostClass for StyleClass {
    fn name(&self) -> &str {
        "CSSStyleDeclaration"
    }
    fn hooks(&self) -> u8 {
        hooks::INDEXED | hooks::NAMED
    }
    fn get(&self, r: &mut Realm, _o: Value, data: u64, key: Key<'_>) -> JsResult<Option<Value>> {
        let decls = style_decls(&self.b, data as NodeId);
        match key {
            Key::Index(i) => Ok(decls.get(i as usize).map(|d| r.string(&d.0))),
            Key::Str(s) if !STYLE_NAMES.contains(&s) => {
                let name = css_name(s);
                let v = decls
                    .iter()
                    .rev()
                    .find(|d| d.0 == name)
                    .map(|d| d.1.clone())
                    .unwrap_or_default();
                Ok(Some(r.string(&v)))
            }
            _ => Ok(None),
        }
    }
    fn set(&self, r: &mut Realm, _o: Value, data: u64, key: Key<'_>, value: Value) -> JsResult<bool> {
        match key {
            Key::Str(s) if !STYLE_NAMES.contains(&s) => {
                let v = if value.is_nullish() {
                    String::new()
                } else {
                    r.to_string(value)?
                };
                set_style_prop(&self.b, r, data as NodeId, &css_name(s), &v, false)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
    fn has(&self, _r: &mut Realm, _o: Value, data: u64, key: Key<'_>) -> JsResult<Option<bool>> {
        match key {
            Key::Index(i) => Ok(Some((i as usize) < style_decls(&self.b, data as NodeId).len())),
            Key::Str(s) if !STYLE_NAMES.contains(&s) => Ok(Some(true)),
            _ => Ok(None),
        }
    }
    fn keys(&self, _r: &mut Realm, _o: Value, data: u64) -> JsResult<Vec<OwnedKey>> {
        let n = style_decls(&self.b, data as NodeId).len() as u32;
        Ok((0..n).map(OwnedKey::Index).collect())
    }
}

/// `backgroundColor` → `background-color`; `cssFloat` → `float`; `--x` stays.
pub fn css_name(s: &str) -> String {
    if s.starts_with("--") {
        return String::from(s);
    }
    if s == "cssFloat" {
        return String::from("float");
    }
    let mut o = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            // webkitTransform → -webkit-transform
            if i == 0 {
                o.push('-');
            } else {
                o.push('-');
            }
            o.push(c.to_ascii_lowercase());
        } else {
            o.push(c);
        }
    }
    if o.starts_with("webkit-") || o.starts_with("moz-") || o.starts_with("ms-") {
        o.insert(0, '-');
    }
    o
}

/// The declarations of a style attribute: (name, value, important), in order.
pub fn parse_decls(s: &str) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    let bytes: Vec<(usize, char)> = s.char_indices().collect();
    let mut pieces = Vec::new();
    for &(i, c) in &bytes {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' => depth += 1,
                ')' => depth -= 1,
                ';' if depth <= 0 => {
                    pieces.push(&s[start..i]);
                    start = i + 1;
                }
                _ => {}
            },
        }
    }
    pieces.push(&s[start..]);
    for p in pieces {
        let Some((n, v)) = p.split_once(':') else {
            continue;
        };
        let n = n.trim();
        if n.is_empty() {
            continue;
        }
        let name = if n.starts_with("--") {
            String::from(n)
        } else {
            n.to_ascii_lowercase()
        };
        let mut v = v.trim();
        let mut imp = false;
        if let Some(i) = v.to_ascii_lowercase().rfind("!important")
            && v[i + 10..].trim().is_empty()
        {
            imp = true;
            v = v[..i].trim();
        }
        out.push((name, String::from(v), imp));
        if out.len() > 4096 {
            break;
        }
    }
    out
}

pub fn serialize_decls(d: &[(String, String, bool)]) -> String {
    let mut s = String::new();
    for (n, v, imp) in d {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(n);
        s.push_str(": ");
        s.push_str(v);
        if *imp {
            s.push_str(" !important");
        }
        s.push(';');
    }
    s
}

fn style_decls(b: &Bridge, n: NodeId) -> Vec<(String, String, bool)> {
    let s = b
        .page()
        .and_then(|p| p.dom.element(n).and_then(|e| e.attr("style")).map(String::from))
        .unwrap_or_default();
    parse_decls(&s)
}

fn set_style_prop(b: &Rc<Bridge>, r: &mut Realm, n: NodeId, name: &str, v: &str, imp: bool) -> JsResult<()> {
    let mut d = style_decls(b, n);
    let v = v.trim();
    d.retain(|x| x.0 != name);
    if !v.is_empty() {
        // A value with a ';' or braces would break out of the declaration: refused.
        if v.contains([';', '{', '}']) && !v.contains('"') && !v.contains('\'') {
            return Ok(());
        }
        d.push((String::from(name), String::from(v), imp));
    }
    let s = serialize_decls(&d);
    if s.is_empty() {
        remove_attribute(b, r, n, "style")
    } else {
        set_attribute(b, r, n, "style", &s)
    }
}

/// element.dataset: `data-*` attributes by their camelCase names.
struct DatasetClass {
    b: Rc<Bridge>,
}

fn data_attr(key: &str) -> Option<String> {
    let mut s = String::from("data-");
    for c in key.chars() {
        if c == '-' {
            return None; // "a-b" isn't a valid dataset name
        }
        if c.is_ascii_uppercase() {
            s.push('-');
            s.push(c.to_ascii_lowercase());
        } else {
            s.push(c);
        }
    }
    Some(s)
}

fn camel(attr: &str) -> String {
    let mut o = String::new();
    let mut up = false;
    for c in attr.chars() {
        if c == '-' {
            up = true;
        } else if up {
            o.push(c.to_ascii_uppercase());
            up = false;
        } else {
            o.push(c);
        }
    }
    o
}

impl HostClass for DatasetClass {
    fn name(&self) -> &str {
        "DOMStringMap"
    }
    fn hooks(&self) -> u8 {
        hooks::NAMED
    }
    fn get(&self, r: &mut Realm, _o: Value, data: u64, key: Key<'_>) -> JsResult<Option<Value>> {
        let Key::Str(s) = key else { return Ok(None) };
        let Some(a) = data_attr(s) else { return Ok(None) };
        let v = self
            .b
            .page()
            .and_then(|p| p.dom.element(data as NodeId)?.attr(&a).map(String::from));
        Ok(v.map(|v| r.string(&v)))
    }
    fn set(&self, r: &mut Realm, _o: Value, data: u64, key: Key<'_>, value: Value) -> JsResult<bool> {
        let Key::Str(s) = key else { return Ok(false) };
        let Some(a) = data_attr(s) else {
            return Err(dom_error(r, "SyntaxError", "bad dataset name"));
        };
        let v = r.to_string(value)?;
        set_attribute(&self.b, r, data as NodeId, &a, &v)?;
        Ok(true)
    }
    fn delete(&self, r: &mut Realm, _o: Value, data: u64, key: Key<'_>) -> JsResult<Option<bool>> {
        let Key::Str(s) = key else { return Ok(None) };
        if let Some(a) = data_attr(s) {
            remove_attribute(&self.b, r, data as NodeId, &a)?;
        }
        Ok(Some(true))
    }
    fn keys(&self, _r: &mut Realm, _o: Value, data: u64) -> JsResult<Vec<OwnedKey>> {
        let Some(p) = self.b.page() else {
            return Ok(Vec::new());
        };
        let Some(e) = p.dom.element(data as NodeId) else {
            return Ok(Vec::new());
        };
        Ok(e
            .attrs
            .iter()
            .filter_map(|a| a.name.strip_prefix("data-"))
            .map(|n| OwnedKey::Str(camel(n)))
            .collect())
    }
}

/// localStorage (data 0) and sessionStorage (data 1): items as properties.
struct StorageClass {
    b: Rc<Bridge>,
}

const STORAGE_NAMES: &[&str] = &[
    "length", "key", "getItem", "setItem", "removeItem", "clear", "constructor", "toString",
    "hasOwnProperty", "valueOf", "toJSON", "then",
];

impl HostClass for StorageClass {
    fn name(&self) -> &str {
        "Storage"
    }
    fn hooks(&self) -> u8 {
        hooks::NAMED | hooks::INDEXED
    }
    fn get(&self, r: &mut Realm, _o: Value, data: u64, key: Key<'_>) -> JsResult<Option<Value>> {
        let k = match key {
            Key::Str(s) if !STORAGE_NAMES.contains(&s) => String::from(s),
            Key::Index(i) => format!("{i}"),
            _ => return Ok(None),
        };
        let v = self
            .b
            .host()
            .and_then(|h| h.storage(data == 1))
            .and_then(|s| s.get(&k));
        Ok(v.map(|v| r.string(&v)))
    }
    fn set(&self, r: &mut Realm, _o: Value, data: u64, key: Key<'_>, value: Value) -> JsResult<bool> {
        let k = match key {
            Key::Str(s) if !STORAGE_NAMES.contains(&s) => String::from(s),
            Key::Index(i) => format!("{i}"),
            _ => return Ok(false),
        };
        let v = r.to_string(value)?;
        storage_set(&self.b, r, data == 1, &k, &v)?;
        Ok(true)
    }
    fn delete(&self, _r: &mut Realm, _o: Value, data: u64, key: Key<'_>) -> JsResult<Option<bool>> {
        let k = match key {
            Key::Str(s) if !STORAGE_NAMES.contains(&s) => String::from(s),
            Key::Index(i) => format!("{i}"),
            _ => return Ok(None),
        };
        if let Some(s) = self.b.host().and_then(|h| h.storage(data == 1)) {
            s.remove(&k);
        }
        Ok(Some(true))
    }
    fn keys(&self, _r: &mut Realm, _o: Value, data: u64) -> JsResult<Vec<OwnedKey>> {
        let Some(s) = self.b.host().and_then(|h| h.storage(data == 1)) else {
            return Ok(Vec::new());
        };
        Ok((0..s.len()).filter_map(|i| s.key(i)).map(OwnedKey::Str).collect())
    }
}

fn storage_set(b: &Bridge, r: &mut Realm, session: bool, k: &str, v: &str) -> JsResult<()> {
    let ok = match b.host().and_then(|h| h.storage(session)) {
        Some(s) => s.set(k, v),
        None => return Err(dom_error(r, "SecurityError", "Storage isn't available here")),
    };
    if !ok {
        return Err(dom_error(r, "QuotaExceededError", "The storage quota has been exceeded"));
    }
    Ok(())
}

/// getComputedStyle(el): computed values by property name.
struct ComputedClass {
    b: Rc<Bridge>,
}

impl HostClass for ComputedClass {
    fn name(&self) -> &str {
        "CSSStyleDeclaration"
    }
    fn hooks(&self) -> u8 {
        hooks::NAMED
    }
    fn get(&self, r: &mut Realm, _o: Value, data: u64, key: Key<'_>) -> JsResult<Option<Value>> {
        let Key::Str(s) = key else { return Ok(None) };
        if STYLE_NAMES.contains(&s) {
            return Ok(None);
        }
        let v = computed(&self.b, data as NodeId, &css_name(s));
        Ok(Some(r.string(&v)))
    }
}

fn kebab_debug<T: core::fmt::Debug>(t: T) -> String {
    let d = format!("{t:?}");
    let mut o = String::new();
    for (i, c) in d.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                o.push('-');
            }
            o.push(c.to_ascii_lowercase());
        } else {
            o.push(c);
        }
    }
    o
}

fn rgb(c: u32) -> String {
    let a = c >> 24;
    let (r, g, b) = ((c >> 16) & 255, (c >> 8) & 255, c & 255);
    if a == 255 {
        format!("rgb({r}, {g}, {b})")
    } else {
        let af = a as f32 / 255.0;
        format!("rgba({r}, {g}, {b}, {})", (af as f64 * 100.0).rnd() / 100.0)
    }
}

/// A few computed values as CSS text ("" for ones not known).
fn computed(b: &Bridge, n: NodeId, name: &str) -> String {
    nodes::ensure_layout(b);
    let Some(p) = b.page() else {
        return String::new();
    };
    let st = p
        .styles()
        .and_then(|s| s.of.get(n as usize))
        .and_then(|s| s.clone());
    let Some(s) = st else {
        return String::from(if name == "display" && p.dom.element(n).is_some() {
            "none"
        } else {
            ""
        });
    };
    let zoom = b
        .host()
        .map_or(1.0, |h| h.viewport().zoom.clamp(25, 500) as f64 / 100.0);
    let rect = p.node_rect(n).unwrap_or_default();
    let px = |v: f64| format!("{}px", (v * 100.0).rnd() / 100.0);
    // Percentages resolve against the containing block's width.
    let base = {
        let parent = p.dom.parent(n);
        if parent == crate::dom::NONE {
            0.0
        } else {
            p.node_rect(parent).map_or(0.0, |r| r.w as f64 / zoom)
        }
    };
    let lp = |v: crate::css::values::LP| px(v.px as f64 + v.pct as f64 * base / 100.0);
    let lpa = |v: crate::style::types::LPA| match v {
        crate::style::types::LPA::Auto => String::from("auto"),
        crate::style::types::LPA::L(l) => lp(l),
    };
    let size = |v: crate::style::types::Size| match v {
        crate::style::types::Size::Auto => String::from("auto"),
        crate::style::types::Size::L(l) => lp(l),
        crate::style::types::Size::MinContent => String::from("min-content"),
        crate::style::types::Size::MaxContent => String::from("max-content"),
        crate::style::types::Size::FitContent => String::from("fit-content"),
        crate::style::types::Size::Content => String::from("content"),
    };
    let pad = |i: usize| (s.padding[i].px as f64 + s.padding[i].pct as f64 * base / 100.0).max(0.0);
    let bw = |i: usize| s.border_width[i] as f64;
    let content_box = s.box_sizing == crate::style::types::BoxSizing::ContentBox;
    let border_style = |i: usize| match s.border_style[i] {
        crate::api::BorderStyle::None => "none",
        crate::api::BorderStyle::Solid | crate::api::BorderStyle::Shaded => "solid",
        crate::api::BorderStyle::Dashed => "dashed",
        crate::api::BorderStyle::Dotted => "dotted",
        crate::api::BorderStyle::Double => "double",
    };
    let hidden = s.display == crate::style::types::Display::None;
    let deco = |d: crate::api::Decoration| {
        let mut v: Vec<&str> = Vec::new();
        if d.has(crate::api::Decoration::UNDERLINE) {
            v.push("underline");
        }
        if d.has(crate::api::Decoration::OVERLINE) {
            v.push("overline");
        }
        if d.has(crate::api::Decoration::LINE_THROUGH) {
            v.push("line-through");
        }
        if v.is_empty() { String::from("none") } else { v.join(" ") }
    };
    match name {
        n if n.starts_with("--") => custom_text(&s, n),
        "display" => kebab_debug(s.display),
        "position" => kebab_debug(s.position),
        "visibility" => kebab_debug(s.visibility),
        "overflow" | "overflow-y" => kebab_debug(s.overflow_y),
        "overflow-x" => kebab_debug(s.overflow_x),
        "float" => kebab_debug(s.float),
        "clear" => kebab_debug(s.clear),
        "box-sizing" => kebab_debug(s.box_sizing),
        "color" => rgb(s.color),
        "background-color" => rgb(s.background_color),
        "background-image" => String::from(if s.bg_image.is_some() { "url(\"\")" } else { "none" }),
        "opacity" => format!("{}", s.opacity),
        "font-size" => px(s.font_size as f64),
        "font-weight" => format!("{}", s.font_weight),
        "font-style" => String::from(if s.italic { "italic" } else { "normal" }),
        "font-variant" | "font-variant-caps" => String::from(if s.small_caps { "small-caps" } else { "normal" }),
        "font-family" => String::from(match s.family {
            crate::api::Family::Serif => "serif",
            crate::api::Family::SansSerif => "sans-serif",
            crate::api::Family::Monospace => "monospace",
            crate::api::Family::Cursive => "cursive",
            crate::api::Family::Fantasy => "fantasy",
            crate::api::Family::SystemUi => "system-ui",
        }),
        "line-height" => match s.line_height {
            crate::style::types::LineHeight::Normal => String::from("normal"),
            crate::style::types::LineHeight::Number(v) => px(v as f64 * s.font_size as f64),
            crate::style::types::LineHeight::Px(v) => px(v as f64),
        },
        "text-align" => kebab_debug(s.text_align),
        "text-transform" => kebab_debug(s.text_transform),
        "text-indent" => lp(s.text_indent),
        "text-decoration-line" => deco(s.deco_line),
        "text-decoration" => deco(s.deco_line),
        "white-space" => kebab_debug(s.white_space),
        "letter-spacing" => if s.letter_spacing == 0.0 { String::from("normal") } else { px(s.letter_spacing as f64) },
        "word-spacing" => if s.word_spacing == 0.0 { String::from("normal") } else { px(s.word_spacing as f64) },
        "cursor" => kebab_debug(s.cursor),
        "vertical-align" => match s.vertical_align {
            crate::style::types::VAlign::L(l) => lp(l),
            v => kebab_debug(v),
        },
        "z-index" => s.z_index.map_or(String::from("auto"), |z| format!("{z}")),
        "width" if hidden => String::from("auto"),
        "height" if hidden => String::from("auto"),
        "width" => {
            let w = rect.w as f64 / zoom;
            px(if content_box { (w - pad(1) - pad(3) - bw(1) - bw(3)).max(0.0) } else { w })
        }
        "height" => {
            let h = rect.h as f64 / zoom;
            px(if content_box { (h - pad(0) - pad(2) - bw(0) - bw(2)).max(0.0) } else { h })
        }
        "min-width" => size(s.min_width),
        "min-height" => size(s.min_height),
        "max-width" => s.max_width.map_or(String::from("none"), lp),
        "max-height" => s.max_height.map_or(String::from("none"), lp),
        "top" => lpa(s.inset[0]),
        "right" => lpa(s.inset[1]),
        "bottom" => lpa(s.inset[2]),
        "left" => lpa(s.inset[3]),
        "margin-top" => lpa(s.margin[0]).replace("auto", "0px"),
        "margin-right" => lpa(s.margin[1]).replace("auto", "0px"),
        "margin-bottom" => lpa(s.margin[2]).replace("auto", "0px"),
        "margin-left" => lpa(s.margin[3]).replace("auto", "0px"),
        "padding-top" => px(pad(0)),
        "padding-right" => px(pad(1)),
        "padding-bottom" => px(pad(2)),
        "padding-left" => px(pad(3)),
        "border-top-width" => px(bw(0)),
        "border-right-width" => px(bw(1)),
        "border-bottom-width" => px(bw(2)),
        "border-left-width" => px(bw(3)),
        "border-top-style" => String::from(border_style(0)),
        "border-right-style" => String::from(border_style(1)),
        "border-bottom-style" => String::from(border_style(2)),
        "border-left-style" => String::from(border_style(3)),
        "border-top-color" => rgb(s.border_color[0]),
        "border-right-color" => rgb(s.border_color[1]),
        "border-bottom-color" => rgb(s.border_color[2]),
        "border-left-color" => rgb(s.border_color[3]),
        "border-radius" | "border-top-left-radius" | "border-top-right-radius" | "border-bottom-left-radius"
        | "border-bottom-right-radius" => lp(s.border_radius),
        "flex-direction" => kebab_debug(s.flex_direction),
        "flex-wrap" => String::from(match s.flex_wrap {
            crate::style::types::FlexWrap::NoWrap => "nowrap",
            crate::style::types::FlexWrap::Wrap => "wrap",
            crate::style::types::FlexWrap::WrapReverse => "wrap-reverse",
        }),
        "flex-basis" => size(s.flex_basis),
        "justify-content" => kebab_debug(s.justify_content),
        "align-content" => kebab_debug(s.align_content),
        "align-items" => kebab_debug(s.align_items),
        "align-self" => kebab_debug(s.align_self),
        "row-gap" => lp(s.row_gap),
        "column-gap" => lp(s.column_gap),
        "flex-grow" => format!("{}", s.flex_grow),
        "flex-shrink" => format!("{}", s.flex_shrink),
        "order" => format!("{}", s.order),
        "aspect-ratio" => s.aspect_ratio.map_or(String::from("auto"), |v| format!("{v}")),
        "list-style-type" => kebab_debug(s.list_style_type),
        "color-scheme" => String::from(match s.color_scheme & 3 {
            1 => "light",
            2 => "dark",
            3 => "light dark",
            _ => "normal",
        }),
        "transform" | "filter" | "box-shadow" | "text-shadow" | "animation-name" | "transition-property" | "clip-path"
        | "backdrop-filter" | "mask-image" | "translate" | "rotate" | "scale" => String::from(if name == "transition-property" { "all" } else { "none" }),
        "transition-duration" | "animation-duration" | "transition-delay" | "animation-delay" => String::from("0s"),
        "pointer-events" | "user-select" | "will-change" | "touch-action" | "scroll-behavior" | "overflow-wrap" => String::from("auto"),
        "content" => String::from("normal"),
        "direction" => String::from("ltr"),
        "writing-mode" => String::from("horizontal-tb"),
        "unicode-bidi" => String::from("normal"),
        "outline-style" => String::from("none"),
        "outline-width" => String::from("0px"),
        "background-repeat" => kebab_debug(s.bg_repeat),
        _ => String::new(),
    }
}

/// A custom property's value as text (tokens joined the way they were written, trimmed).
fn custom_text(s: &crate::style::types::Style, name: &str) -> String {
    use crate::css::tokenizer::Tok;
    let Some(map) = &s.custom else { return String::new() };
    let Some(toks) = map.get(name) else { return String::new() };
    let mut out = String::new();
    for t in toks.iter() {
        match t {
            Tok::Ident(v) => out.push_str(v),
            Tok::Function(v) => {
                out.push_str(v);
                out.push('(');
            }
            Tok::AtKeyword(v) => {
                out.push('@');
                out.push_str(v);
            }
            Tok::Hash { value, .. } => {
                out.push('#');
                out.push_str(value);
            }
            Tok::Str(v) => {
                out.push('"');
                out.push_str(v);
                out.push('"');
            }
            Tok::Url(v) => out.push_str(&format!("url({v})")),
            Tok::Delim(c) => out.push(*c),
            Tok::Number { value, .. } => out.push_str(&format!("{value}")),
            Tok::Percentage(v) => out.push_str(&format!("{v}%")),
            Tok::Dimension { value, unit } => out.push_str(&format!("{value}{unit}")),
            Tok::Whitespace => out.push(' '),
            Tok::Colon => out.push(':'),
            Tok::Semicolon => out.push(';'),
            Tok::Comma => out.push(','),
            Tok::Open(c) | Tok::Close(c) => out.push(*c),
            _ => {}
        }
        if out.len() > 65536 {
            break;
        }
    }
    String::from(out.trim())
}

/// The names `getComputedStyle(el)` lists (length, item(i)) and answers.
const COMPUTED_NAMES: &[&str] = &[
    "align-content", "align-items", "align-self", "aspect-ratio", "background-color", "background-image", "background-repeat",
    "border-bottom-color", "border-bottom-style", "border-bottom-width", "border-left-color", "border-left-style",
    "border-left-width", "border-radius", "border-right-color", "border-right-style", "border-right-width",
    "border-top-color", "border-top-style", "border-top-width", "bottom", "box-sizing", "clear", "color", "color-scheme",
    "column-gap", "content", "cursor", "direction", "display", "flex-basis", "flex-direction", "flex-grow", "flex-shrink",
    "flex-wrap", "float", "font-family", "font-size", "font-style", "font-variant", "font-weight", "height",
    "justify-content", "left", "letter-spacing", "line-height", "list-style-type", "margin-bottom", "margin-left",
    "margin-right", "margin-top", "max-height", "max-width", "min-height", "min-width", "opacity", "order", "overflow-x",
    "overflow-y", "padding-bottom", "padding-left", "padding-right", "padding-top", "pointer-events", "position", "right",
    "row-gap", "text-align", "text-decoration-line", "text-indent", "text-transform", "top", "transform",
    "transition-duration", "vertical-align", "visibility", "white-space", "width", "word-spacing", "z-index",
];

// ---- helpers the node bindings use --------------------------------------------------------

/// A per-element helper object (classList, style, dataset), the same one each time.
pub fn cached_object(
    b: &Rc<Bridge>,
    r: &mut Realm,
    what: &'static str,
    n: NodeId,
    class: jsengine::ClassId,
    proto: &Pinned,
) -> JsResult<Value> {
    if let Some(p) = b.st(r)?.helpers.get(&(what, n)) {
        return Ok(r.pinned(p));
    }
    let pv = r.pinned(proto);
    let o = r.new_host_object(class, n as u64, pv);
    let pin = r.pin(o);
    b.st(r)?.helpers.insert((what, n), pin);
    Ok(o)
}

/// Is the page a secure context: https, or a local address?
pub fn is_secure(b: &Bridge) -> bool {
    let Some(p) = b.page() else { return false };
    let u = split_url(p.page_url());
    match u.protocol.as_str() {
        "https:" | "wss:" | "file:" => true,
        "http:" | "ws:" => {
            let h = u.hostname.to_ascii_lowercase();
            h == "localhost" || h.ends_with(".localhost") || h == "127.0.0.1" || h == "[::1]"
        }
        _ => false,
    }
}

/// The scroll offset in CSS pixels.
pub fn scroll_css(b: &Bridge) -> (f64, f64) {
    match b.host() {
        Some(h) => {
            let z = h.viewport().zoom.clamp(25, 500) as f64 / 100.0;
            let (x, y) = h.scroll();
            (x as f64 / z, y as f64 / z)
        }
        None => (0.0, 0.0),
    }
}

/// Ask the app to scroll to CSS position (x, y) (None: where it is).
pub fn scroll_to_css(b: &Bridge, x: Option<f64>, y: Option<f64>) {
    let Some(h) = b.host() else { return };
    let z = h.viewport().zoom.clamp(25, 500) as f64 / 100.0;
    let (sx, sy) = h.scroll();
    let fx = |v: f64| if v.is_finite() { (v * z) as i32 } else { 0 };
    let x = x.map_or(sx, fx).max(0);
    let y = y.map_or(sy, fx).max(0);
    b.request(PageRequest::ScrollTo { x, y });
}

/// The parts of a URL as `location` and `<a>` show them.
#[derive(Default)]
pub struct UrlParts {
    pub protocol: String,
    pub host: String,
    pub hostname: String,
    pub port: String,
    pub pathname: String,
    pub search: String,
    pub hash: String,
    pub origin: String,
}

pub fn split_url(u: &str) -> UrlParts {
    let mut p = UrlParts::default();
    let (rest, hash) = match u.find('#') {
        Some(i) => (&u[..i], &u[i..]),
        None => (u, ""),
    };
    p.hash = if hash.len() > 1 { String::from(hash) } else { String::new() };
    let (rest, search) = match rest.find('?') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    p.search = if search.len() > 1 { String::from(search) } else { String::new() };
    let Some((scheme, after)) = rest.split_once(':') else {
        p.pathname = String::from(rest);
        return p;
    };
    p.protocol = format!("{}:", scheme.to_ascii_lowercase());
    match after.strip_prefix("//") {
        Some(a) => {
            let (auth, path) = match a.find('/') {
                Some(i) => (&a[..i], &a[i..]),
                None => (a, "/"),
            };
            let auth = auth.rsplit('@').next().unwrap_or(auth);
            p.host = String::from(auth);
            let (hn, port) = if auth.starts_with('[') {
                match auth.find(']') {
                    Some(i) => (&auth[..=i], auth[i + 1..].strip_prefix(':').unwrap_or("")),
                    None => (auth, ""),
                }
            } else {
                match auth.rsplit_once(':') {
                    Some((h, po)) => (h, po),
                    None => (auth, ""),
                }
            };
            p.hostname = String::from(hn);
            p.port = String::from(port);
            p.pathname = String::from(path);
            p.origin = format!("{}//{}", p.protocol, p.host);
        }
        None => {
            p.pathname = String::from(after);
            p.origin = String::from("null");
        }
    }
    p
}

/// href's parts on `<a>` (protocol, host, ... read only here).
pub fn url_parts(b: &Rc<Bridge>, r: &mut Realm, o: Value) -> JsResult<()> {
    type Pick = fn(&UrlParts) -> &String;
    let parts: [(&str, Pick); 8] = [
        ("protocol", |u| &u.protocol),
        ("host", |u| &u.host),
        ("hostname", |u| &u.hostname),
        ("port", |u| &u.port),
        ("pathname", |u| &u.pathname),
        ("search", |u| &u.search),
        ("hash", |u| &u.hash),
        ("origin", |u| &u.origin),
    ];
    for (name, pick) in parts {
        let b2 = b.clone();
        r.define_getter(
            o,
            name,
            move |r, t, _| {
                let n = nodes::this_node(&b2, r, t)?;
                let p = b2.page_or(r)?;
                let href = p
                    .dom
                    .element(n)
                    .and_then(|e| e.attr("href"))
                    .map(|h| p.resolve(h))
                    .unwrap_or_default();
                let u = split_url(&href);
                let s = pick(&u).clone();
                Ok(r.string(&s))
            },
            None,
        )?;
    }
    Ok(())
}

/// The HTMLxxxElement interface name of a tag.
pub fn interface_name(t: Atom) -> &'static str {
    use tag::*;
    match t {
        A => "HTMLAnchorElement",
        AREA => "HTMLAreaElement",
        BODY => "HTMLBodyElement",
        BR => "HTMLBRElement",
        BUTTON => "HTMLButtonElement",
        CANVAS => "HTMLCanvasElement",
        DIV => "HTMLDivElement",
        DL => "HTMLDListElement",
        DETAILS => "HTMLDetailsElement",
        DIALOG => "HTMLDialogElement",
        FIELDSET => "HTMLFieldSetElement",
        FORM => "HTMLFormElement",
        H1 | H2 | H3 | H4 | H5 | H6 => "HTMLHeadingElement",
        HEAD => "HTMLHeadElement",
        HR => "HTMLHRElement",
        HTML => "HTMLHtmlElement",
        IFRAME => "HTMLIFrameElement",
        IMG => "HTMLImageElement",
        INPUT => "HTMLInputElement",
        LABEL => "HTMLLabelElement",
        LI => "HTMLLIElement",
        LINK => "HTMLLinkElement",
        META => "HTMLMetaElement",
        OL => "HTMLOListElement",
        OPTION => "HTMLOptionElement",
        P => "HTMLParagraphElement",
        PRE => "HTMLPreElement",
        SCRIPT => "HTMLScriptElement",
        SELECT => "HTMLSelectElement",
        SPAN => "HTMLSpanElement",
        STYLE => "HTMLStyleElement",
        TABLE => "HTMLTableElement",
        TD | TH => "HTMLTableCellElement",
        TR => "HTMLTableRowElement",
        TEMPLATE => "HTMLTemplateElement",
        TEXTAREA => "HTMLTextAreaElement",
        TITLE => "HTMLTitleElement",
        UL => "HTMLUListElement",
        _ => "HTMLUnknownElement",
    }
}

/// The tags with prototypes of their own.
const OWN_PROTO_TAGS: &[Atom] = &[
    tag::A, tag::AREA, tag::BODY, tag::BR, tag::BUTTON, tag::CANVAS, tag::DIV, tag::DL,
    tag::DETAILS, tag::DIALOG, tag::FIELDSET, tag::FORM, tag::H1, tag::H2, tag::H3, tag::H4,
    tag::H5, tag::H6, tag::HEAD, tag::HR, tag::HTML, tag::IFRAME, tag::IMG, tag::INPUT,
    tag::LABEL, tag::LI, tag::LINK, tag::META, tag::OL, tag::OPTION, tag::P, tag::PRE,
    tag::SCRIPT, tag::SELECT, tag::SPAN, tag::STYLE, tag::TABLE, tag::TD, tag::TH, tag::TR,
    tag::TEMPLATE, tag::TEXTAREA, tag::TITLE, tag::UL,
];

// ---- window's natives -----------------------------------------------------------------------

pub(super) fn timer_add(b: &Rc<Bridge>, r: &mut Realm, a: &[Value], repeat: bool) -> JsResult<Value> {
    let f = arg(a, 0);
    let f = if r.is_callable(f) {
        f
    } else {
        // The string form (compiled when it runs).
        let s = r.to_string(f)?;
        r.string(&s)
    };
    let delay = if a.len() > 1 { arg_num(r, a, 1)? } else { 0.0 };
    let extra = if a.len() > 2 {
        Some(r.new_array(&a[2..]))
    } else {
        None
    };
    let now = b.host().map_or(0.0, |h| h.now());
    let h = b.keep_cb(r, f, extra)?;
    let id = b.st(r)?.timers.add(h, delay, repeat, now);
    if id == 0 {
        b.drop_cb(r, h);
    }
    Ok(Value::int(id as i32))
}

pub(super) fn timer_clear(b: &Rc<Bridge>, r: &mut Realm, a: &[Value]) -> JsResult<Value> {
    let id = r.to_int32(arg(a, 0))?;
    if id <= 0 {
        return Ok(Value::UNDEFINED);
    }
    // Find its handle to unpin it.
    let handle = {
        let mut st = b.st(r)?;
        let h = st.timers.handle_of(id as u32);
        st.timers.clear(id as u32);
        h
    };
    if let Some(h) = handle {
        b.drop_cb(r, h);
    }
    Ok(Value::UNDEFINED)
}

/// console.log's formatting: %s %d %i %f %o %O %c substitutions in a first string, the
/// rest inspected and joined with spaces.
pub(super) fn console_text(r: &mut Realm, a: &[Value]) -> String {
    let mut out = String::new();
    let mut rest = a;
    if let Some(first) = a.first()
        && let Some(fmt) = r.get_string(*first)
    {
        rest = &a[1..];
        let mut it = fmt.chars().peekable();
        while let Some(c) = it.next() {
            if c == '%'
                && let Some(&d) = it.peek()
            {
                match d {
                    's' | 'd' | 'i' | 'f' | 'o' | 'O' | 'c' => {
                        it.next();
                        if d == 'c' {
                            rest = rest.get(1..).unwrap_or(&[]);
                            continue;
                        }
                        let Some((&v, tail)) = rest.split_first() else {
                            out.push('%');
                            out.push(d);
                            continue;
                        };
                        rest = tail;
                        match d {
                            's' => out.push_str(&r.get_string(v).unwrap_or_else(|| r.inspect(v))),
                            'd' | 'i' => {
                                let x = r.to_number(v).unwrap_or(f64::NAN);
                                if x.is_finite() {
                                    out.push_str(&format!("{}", x as i64));
                                } else {
                                    out.push_str("NaN");
                                }
                            }
                            'f' => {
                                let x = r.to_number(v).unwrap_or(f64::NAN);
                                out.push_str(&format!("{x}"));
                            }
                            _ => out.push_str(&r.inspect(v)),
                        }
                        continue;
                    }
                    '%' => {
                        it.next();
                        out.push('%');
                        continue;
                    }
                    _ => {}
                }
            }
            out.push(c);
        }
    }
    for &v in rest {
        if !out.is_empty() {
            out.push(' ');
        }
        match r.get_string(v) {
            Some(s) => out.push_str(&s),
            None => out.push_str(&r.inspect(v)),
        }
    }
    out
}

pub(super) fn console_fn(b: &Rc<Bridge>, r: &mut Realm, obj: Value, name: &str, level: ConsoleLevel) -> JsResult<()> {
    let b2 = b.clone();
    r.define_method(obj, name, 0, move |r, _t, a| {
        let s = console_text(r, a);
        b2.console(level, s);
        Ok(Value::UNDEFINED)
    })?;
    Ok(())
}

pub(super) fn location_href(b: &Bridge) -> String {
    if let Some(l) = b.xf.sec.borrow().location.as_ref() {
        return l.clone();
    }
    b.page().map(|p| String::from(p.address())).unwrap_or_default()
}

fn navigate(b: &Bridge, r: &mut Realm, url: &str, replace: bool) -> JsResult<()> {
    let Some(p) = b.page() else { return Ok(()) };
    let abs = p.resolve(url.trim());
    if abs.trim_start().to_ascii_lowercase().starts_with("javascript:") {
        let code = crate::url::percent_decode(&abs.trim_start()[11..]);
        if let Err(e) = r.eval(&code, "javascript:") {
            return Err(e);
        }
        return Ok(());
    }
    b.request(PageRequest::Navigate { url: abs, replace });
    Ok(())
}

fn install_location(b: &Rc<Bridge>, r: &mut Realm, g: Value) -> JsResult<()> {
    let loc = r.new_object();
    type Pick = fn(&UrlParts) -> &String;
    let parts: [(&str, Pick); 8] = [
        ("protocol", |u| &u.protocol),
        ("host", |u| &u.host),
        ("hostname", |u| &u.hostname),
        ("port", |u| &u.port),
        ("pathname", |u| &u.pathname),
        ("search", |u| &u.search),
        ("hash", |u| &u.hash),
        ("origin", |u| &u.origin),
    ];
    for (name, pick) in parts {
        let b2 = b.clone();
        let b3 = b.clone();
        let nm = name;
        let set: Option<Box<jsengine::NativeFn>> = if name == "origin" {
            None
        } else {
            Some(Box::new(move |r: &mut Realm, _t: Value, a: &[Value]| {
                let v = arg_str(r, a, 0)?;
                let mut u = split_url(&location_href(&b3));
                match nm {
                    "hash" => {
                        u.hash = if v.is_empty() || v.starts_with('#') { v } else { format!("#{v}") }
                    }
                    "search" => {
                        u.search = if v.is_empty() || v.starts_with('?') { v } else { format!("?{v}") }
                    }
                    "pathname" => u.pathname = if v.starts_with('/') { v } else { format!("/{v}") },
                    "hostname" => {
                        u.host = if u.port.is_empty() { v } else { format!("{v}:{}", u.port) }
                    }
                    "port" => u.host = format!("{}:{v}", u.hostname),
                    "host" => u.host = v,
                    "protocol" => u.protocol = format!("{}:", v.trim_end_matches(':')),
                    _ => {}
                }
                let url = format!("{}//{}{}{}{}", u.protocol, u.host, u.pathname, u.search, u.hash);
                navigate(&b3, r, &url, false)?;
                Ok(Value::UNDEFINED)
            }))
        };
        r.define_getter(
            loc,
            name,
            move |r, _t, _| {
                let u = split_url(&location_href(&b2));
                let s = pick(&u).clone();
                Ok(r.string(&s))
            },
            set,
        )?;
    }
    let b2 = b.clone();
    let b3 = b.clone();
    r.define_getter(
        loc,
        "href",
        move |r, _t, _| {
            let s = location_href(&b2);
            Ok(r.string(&s))
        },
        Some(Box::new(move |r: &mut Realm, _t: Value, a: &[Value]| {
            let v = arg_str(r, a, 0)?;
            navigate(&b3, r, &v, false)?;
            Ok(Value::UNDEFINED)
        })),
    )?;
    let b2 = b.clone();
    r.define_method(loc, "assign", 1, move |r, _t, a| {
        let v = arg_str(r, a, 0)?;
        navigate(&b2, r, &v, false)?;
        Ok(Value::UNDEFINED)
    })?;
    let b2 = b.clone();
    r.define_method(loc, "replace", 1, move |r, _t, a| {
        let v = arg_str(r, a, 0)?;
        navigate(&b2, r, &v, true)?;
        Ok(Value::UNDEFINED)
    })?;
    let b2 = b.clone();
    r.define_method(loc, "reload", 0, move |_r, _t, _a| {
        b2.request(PageRequest::Reload);
        Ok(Value::UNDEFINED)
    })?;
    let b2 = b.clone();
    r.define_method(loc, "toString", 0, move |r, _t, _a| {
        let s = location_href(&b2);
        Ok(r.string(&s))
    })?;
    // window.location = "..." navigates.
    let b2 = b.clone();
    let lp = r.pin(loc);
    let b3 = b.clone();
    r.define_getter(
        g,
        "location",
        move |r, _t, _| Ok(r.pinned(&lp)),
        Some(Box::new(move |r: &mut Realm, _t: Value, a: &[Value]| {
            let v = arg_str(r, a, 0)?;
            navigate(&b3, r, &v, false)?;
            Ok(Value::UNDEFINED)
        })),
    )?;
    let _ = b2;
    Ok(())
}

fn install_history(b: &Rc<Bridge>, r: &mut Realm, g: Value) -> JsResult<()> {
    let h = r.new_object();
    let b2 = b.clone();
    r.define_method(h, "back", 0, move |_r, _t, _a| {
        b2.request(PageRequest::History(-1));
        Ok(Value::UNDEFINED)
    })?;
    let b2 = b.clone();
    r.define_method(h, "forward", 0, move |_r, _t, _a| {
        b2.request(PageRequest::History(1));
        Ok(Value::UNDEFINED)
    })?;
    let b2 = b.clone();
    r.define_method(h, "go", 0, move |r, _t, a| {
        let d = r.to_int32(arg(a, 0))?;
        b2.request(if d == 0 {
            PageRequest::Reload
        } else {
            PageRequest::History(d)
        });
        Ok(Value::UNDEFINED)
    })?;
    for (name, replace) in [("pushState", false), ("replaceState", true)] {
        let b2 = b.clone();
        r.define_method(h, name, 2, move |r, _t, a| {
            let sv = arg(a, 0);
            let state = if sv.is_nullish() { None } else { r.json_stringify(sv)? };
            let url = arg(a, 2);
            let cur = location_href(&b2);
            let target = if url.is_nullish() {
                cur.clone()
            } else {
                let s = r.to_string(url)?;
                b2.page().map(|p| p.resolve(&s)).unwrap_or(s)
            };
            if split_url(&target).origin != split_url(&cur).origin {
                return Err(dom_error(r, "SecurityError", "pushState: another origin"));
            }
            b2.st(r)?.history_state = state;
            if let Some(p) = b2.page() {
                p.set_address(&target);
            }
            b2.request(PageRequest::SetAddress { url: target, replace });
            Ok(Value::UNDEFINED)
        })?;
    }
    let b2 = b.clone();
    r.define_getter(
        h,
        "state",
        move |r, _t, _| {
            let s = b2.st(r)?.history_state.clone();
            match s {
                Some(j) => r.json_parse(&j),
                None => Ok(Value::NULL),
            }
        },
        None,
    )?;
    r.define(h, "length", Value::int(1), Attrs::ENUMERABLE)?;
    let sr = r.string("auto");
    r.define(h, "scrollRestoration", sr, Attrs::DEFAULT)?;
    r.define(g, "history", h, Attrs::METHOD)?;
    Ok(())
}

fn install_navigator(b: &Rc<Bridge>, r: &mut Realm, g: Value) -> JsResult<()> {
    let nav = r.new_object();
    let ua = b.host().map(|h| h.user_agent()).unwrap_or_default();
    let uav = r.string(&ua);
    r.define(nav, "userAgent", uav, Attrs::ENUMERABLE)?;
    for (k, v) in [
        ("appName", "Netscape"),
        ("appCodeName", "Mozilla"),
        ("product", "Gecko"),
        ("platform", "milkOS"),
        ("vendor", ""),
        ("language", "en-US"),
    ] {
        let s = r.string(v);
        r.define(nav, k, s, Attrs::ENUMERABLE)?;
    }
    let av = r.string(ua.strip_prefix("Mozilla/").unwrap_or(&ua));
    r.define(nav, "appVersion", av, Attrs::ENUMERABLE)?;
    let en = r.string("en-US");
    let en2 = r.string("en");
    let langs = r.new_array(&[en, en2]);
    r.define(nav, "languages", langs, Attrs::ENUMERABLE)?;
    r.define(nav, "onLine", Value::TRUE, Attrs::ENUMERABLE)?;
    r.define(nav, "cookieEnabled", Value::TRUE, Attrs::ENUMERABLE)?;
    r.define(nav, "javaEnabled", Value::FALSE, Attrs::METHOD)?;
    r.define(nav, "hardwareConcurrency", Value::int(1), Attrs::ENUMERABLE)?;
    r.define(nav, "maxTouchPoints", Value::int(0), Attrs::ENUMERABLE)?;
    r.define(nav, "webdriver", Value::FALSE, Attrs::ENUMERABLE)?;
    r.define(nav, "doNotTrack", Value::NULL, Attrs::ENUMERABLE)?;
    r.define(nav, "pdfViewerEnabled", Value::FALSE, Attrs::ENUMERABLE)?;
    r.define_method(nav, "vibrate", 1, |_r, _t, _a| Ok(Value::FALSE))?;
    r.define_method(nav, "getGamepads", 0, |r, _t, _a| Ok(r.new_array(&[])))?;
    r.define(g, "navigator", nav, Attrs::METHOD)?;
    Ok(())
}

/// Does the media query `q` match the page's viewport and colour scheme?
pub fn media_matches(b: &Bridge, q: &str) -> bool {
    let (w, h) = match b.host() {
        Some(h) => {
            let vp = h.viewport();
            let z = vp.zoom.clamp(25, 500) as f32 / 100.0;
            (vp.width as f32 / z, vp.height as f32 / z)
        }
        None => (1024.0, 768.0),
    };
    let dark = b.host().is_some_and(|h| h.viewport().dark);
    let env = crate::css::sheet::MediaEnv {
        width: w,
        height: h,
        dark,
    };
    let list = crate::css::sheet::parse_media_list(&crate::css::tokenizer::tokenize(q, 4096));
    list.matches(&env)
}

/// Make the realm a window: classes, prototypes, the DOM, events and the globals; then the
/// page's `on*` attributes.
pub fn install(b: &Rc<Bridge>, r: &mut Realm) {
    if let Err(e) = install_all(b, r) {
        super::report(b, r, &e, "setting up the page's scripts");
    }
    super::modrt::install(b, r);
    super::frames::install(b, r);
    // Uncaught errors in jobs and unhandled rejections go through window's events first.
    let b2 = b.clone();
    r.set_rejection_hook(Some(Box::new(move |r: &mut Realm, ev: jsengine::PromiseRejection| match ev {
        jsengine::PromiseRejection::Unhandled { promise, reason } => {
            if !events_js::fire_rejection(&b2, r, promise, Some(reason)) {
                let rep = r.report_value(reason);
                b2.console(ConsoleLevel::Error, format!("Uncaught (in promise) {rep}"));
                if let Ok(mut st) = b2.st.try_borrow_mut() {
                    st.stats.errors += 1;
                }
            }
        }
        jsengine::PromiseRejection::Handled { promise } => {
            events_js::fire_rejection(&b2, r, promise, None);
        }
    })));
    let b2 = b.clone();
    r.set_error_hook(Some(Box::new(move |r: &mut Realm, e: &jsengine::Exception| {
        super::report(&b2, r, e, "");
    })));
    // Wall-clock time for Date, and the slow-script check.
    let b2 = b.clone();
    r.set_clock(Box::new(move || b2.host().map_or(0.0, |h| h.time())));
    let b3 = b.clone();
    r.set_interrupt(Some(Box::new(move || interrupt(&b3))));
}

/// The interrupt callback: past the slow threshold, ask the app every 100 ms.
pub(super) fn interrupt(b: &Bridge) -> bool {
    let Some(h) = b.host() else { return true };
    let now = h.now();
    let (start, asked, slow) = match b.st.try_borrow() {
        Ok(st) => (st.entry_start, st.slow_asked, st.limits.slow_ms as f64),
        Err(_) => return true,
    };
    let ran = now - start;
    if ran < slow || now - asked < 100.0 {
        return true;
    }
    if let Ok(mut st) = b.st.try_borrow_mut() {
        st.slow_asked = now;
    }
    let go_on = h.slow(ran);
    if !go_on && let Ok(mut st) = b.st.try_borrow_mut() {
        st.stopped = true;
        st.stop_reason = String::from("the page was too slow and you stopped it");
    }
    go_on
}

/// The values `install_all` goes on with after the classes and prototypes are made.
pub(super) struct Tables {
    pub et: Value,
    pub node_list: Value,
    pub coll: Value,
    pub tok: Value,
    pub style: Value,
    pub storage: Value,
    pub html_element: Value,
    pub element: Value,
}

/// Register the host classes and make the prototypes (a window's, and a worker's too: the
/// bindings expect them to exist).
pub(super) fn make_tables(b: &Rc<Bridge>, r: &mut Realm) -> Tables {
    let classes = Classes {
        node: r.register_class(NodeClass),
        event: r.register_class(EventClass),
        list: r.register_class(nodes::ListClass { b: b.clone() }),
        tokens: r.register_class(TokensClass { b: b.clone() }),
        style: r.register_class(StyleClass { b: b.clone() }),
        dataset: r.register_class(DatasetClass { b: b.clone() }),
        storage: r.register_class(StorageClass { b: b.clone() }),
        computed: r.register_class(ComputedClass { b: b.clone() }),
    };
    let _ = b.classes.set(classes);
    let objp = r.intrinsic(jsengine::Intrinsic::ObjectPrototype);
    let mk = |r: &mut Realm, parent: Value| -> (Value, Pinned) {
        let o = r.new_object_with_proto(parent);
        let p = r.pin(o);
        (o, p)
    };
    let (et, p_et) = mk(r, objp);
    let (node, p_node) = mk(r, et);
    let (document, p_document) = mk(r, node);
    let (element, p_element) = mk(r, node);
    let (html_element, p_html_element) = mk(r, element);
    let (cdata, p_cdata) = mk(r, node);
    let (_text, p_text) = mk(r, cdata);
    let (_comment, p_comment) = mk(r, cdata);
    let (_doctype, p_doctype) = mk(r, node);
    let (_fragment, p_fragment) = mk(r, node);
    let mut by_tag = Vec::new();
    for &t in OWN_PROTO_TAGS {
        let (_, p) = mk(r, html_element);
        by_tag.push((t, p));
    }
    let (event, p_event) = mk(r, objp);
    let (ui, p_ui) = mk(r, event);
    let (_mouse, p_mouse) = mk(r, ui);
    let (_key, p_key) = mk(r, ui);
    let (_focus, p_focus) = mk(r, ui);
    let (_custom, p_custom) = mk(r, event);
    let (_input, p_input) = mk(r, ui);
    let (_submit, p_submit) = mk(r, event);
    let (_errev, p_errev) = mk(r, event);
    let (_rejev, p_rejev) = mk(r, event);
    let (node_list, p_node_list) = mk(r, objp);
    let (coll, p_coll) = mk(r, objp);
    let (tok, p_tok) = mk(r, objp);
    let (style, p_style) = mk(r, objp);
    let (_smap, p_smap) = mk(r, objp);
    let (storage, p_storage) = mk(r, objp);
    let (_rect, p_rect) = mk(r, objp);
    let protos = Protos {
        event_target: p_et,
        node: p_node,
        document: p_document,
        element: p_element,
        html_element: p_html_element,
        character_data: p_cdata,
        text: p_text,
        comment: p_comment,
        doc_type: p_doctype,
        fragment: p_fragment,
        by_tag,
        event: p_event,
        ui_event: p_ui,
        mouse_event: p_mouse,
        keyboard_event: p_key,
        focus_event: p_focus,
        custom_event: p_custom,
        input_event: p_input,
        submit_event: p_submit,
        error_event: p_errev,
        rejection_event: p_rejev,
        node_list: p_node_list,
        html_collection: p_coll,
        token_list: p_tok,
        style_decl: p_style,
        string_map: p_smap,
        storage: p_storage,
        dom_rect: p_rect,
    };
    let _ = b.protos.set(protos);
    let _ = document;
    Tables { et, node_list, coll, tok, style, storage, html_element, element }
}

fn install_all(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<()> {
    let Tables { et, node_list, coll, tok, style, storage, html_element, element } = make_tables(b, r);

    let g = r.global();
    // window's prototype chain: Window.prototype → EventTarget.prototype.
    let wp = r.new_object_with_proto(et);
    r.set_prototype(g, wp);
    events_js::install(b, r)?;
    nodes::install(b, r)?;
    nodes::install_handlers(b, r, wp)?;
    let wc = r.new_class("Window", 0, wp, None, None);
    r.define(g, "Window", wc, Attrs::METHOD)?;

    // Lists.
    let values = r.intrinsic(jsengine::Intrinsic::ArrayValues);
    let for_each = r.intrinsic(jsengine::Intrinsic::ArrayForEach);
    let iter = r.well_known_symbol(jsengine::WellKnown::Iterator);
    for (o, name) in [(node_list, "NodeList"), (coll, "HTMLCollection"), (tok, "DOMTokenList")] {
        let b2 = b.clone();
        r.define_getter(
            o,
            "length",
            move |r, t, _| {
                let n = list_len(&b2, r, t)?;
                Ok(Value::int(n as i32))
            },
            None,
        )?;
        let b2 = b.clone();
        r.define_method(o, "item", 1, move |r, t, a| {
            let i = r.to_int32(arg(a, 0))?;
            if i < 0 {
                return Ok(Value::NULL);
            }
            let v = r.get(t, i as u32)?;
            let _ = &b2;
            Ok(if v.is_undefined() { Value::NULL } else { v })
        })?;
        r.define(o, "forEach", for_each, Attrs::METHOD)?;
        r.define(o, "values", values, Attrs::METHOD)?;
        r.define(o, Key::Value(iter), values, Attrs::METHOD)?;
        let k = r.intrinsic(jsengine::Intrinsic::ArrayPrototype);
        for m in ["entries", "keys"] {
            let f = r.get(k, m)?;
            r.define(o, m, f, Attrs::METHOD)?;
        }
        let c = r.new_class(name, 0, o, None, None);
        r.define(g, name, c, Attrs::METHOD)?;
    }
    let b2 = b.clone();
    r.define_method(coll, "namedItem", 1, move |r, t, a| {
        let s = arg_str(r, a, 0)?;
        let v = r.get(t, s.as_str())?;
        let _ = &b2;
        Ok(if v.is_object() { v } else { Value::NULL })
    })?;
    install_tokens(b, r, tok)?;
    install_style(b, r, style)?;
    install_storage(b, r, g, storage)?;

    // window, self, document...
    r.define(g, "window", g, Attrs::ENUMERABLE)?;
    for k in ["self", "frames", "top", "parent", "globalThis"] {
        r.define(g, k, g, Attrs::DEFAULT)?;
    }
    let b2 = b.clone();
    r.define_getter(g, "document", move |r, _t, _| Ok(wrap(&b2, r, 0)), None)?;
    r.define(g, "frameElement", Value::NULL, Attrs::DEFAULT)?;
    r.define(g, "opener", Value::NULL, Attrs::DEFAULT)?;
    r.define(g, "closed", Value::FALSE, Attrs::DEFAULT)?;
    r.define(g, "length", Value::int(0), Attrs::DEFAULT)?;
    let nm = r.string("");
    r.define(g, "name", nm, Attrs::DEFAULT)?;
    let b2 = b.clone();
    r.define_getter(g, "isSecureContext", move |_r, _t, _| Ok(Value::bool(is_secure(&b2))), None)?;
    let b2 = b.clone();
    r.define_getter(
        g,
        "origin",
        move |r, _t, _| {
            let s = b2.origin().text;
            Ok(r.string(&s))
        },
        None,
    )?;
    install_location(b, r, g)?;
    install_history(b, r, g)?;
    install_navigator(b, r, g)?;
    super::net::install(b, r, false)?;
    super::xwin::install(b, r)?;

    // Sizes and scrolling.
    fn vp_css(b: &Bridge, which: u8) -> f64 {
        let (w, h) = match b.host() {
            Some(h) => {
                let vp = h.viewport();
                let z = vp.zoom.clamp(25, 500) as f64 / 100.0;
                (vp.width as f64 / z, vp.height as f64 / z)
            }
            None => (0.0, 0.0),
        };
        if which == 0 { w } else { h }.rnd()
    }
    let w_get: fn(&Rc<Bridge>, &mut Realm, Value, &[Value]) -> JsResult<Value> =
        |b, _r, _t, _a| Ok(Value::number(vp_css(b, 0)));
    let h_get: fn(&Rc<Bridge>, &mut Realm, Value, &[Value]) -> JsResult<Value> =
        |b, _r, _t, _a| Ok(Value::number(vp_css(b, 1)));
    getter(b, r, g, "innerWidth", w_get, None)?;
    getter(b, r, g, "innerHeight", h_get, None)?;
    getter(b, r, g, "outerWidth", w_get, None)?;
    getter(b, r, g, "outerHeight", h_get, None)?;
    getter(b, r, g, "devicePixelRatio", |b, _r, _t, _a| {
        let z = b.host().map_or(100, |h| h.viewport().zoom.clamp(25, 500));
        Ok(Value::number(z as f64 / 100.0))
    }, None)?;
    getter(b, r, g, "scrollX", |b, _r, _t, _a| Ok(Value::number(scroll_css(b).0)), None)?;
    getter(b, r, g, "pageXOffset", |b, _r, _t, _a| Ok(Value::number(scroll_css(b).0)), None)?;
    getter(b, r, g, "scrollY", |b, _r, _t, _a| Ok(Value::number(scroll_css(b).1)), None)?;
    getter(b, r, g, "pageYOffset", |b, _r, _t, _a| Ok(Value::number(scroll_css(b).1)), None)?;
    let scroll_to: fn(&Rc<Bridge>, &mut Realm, Value, &[Value]) -> JsResult<Value> = |b, r, _t, a| {
        let o = arg(a, 0);
        if o.is_object() {
            let x = r.get(o, "left")?;
            let y = r.get(o, "top")?;
            let x = if x.is_undefined() { None } else { Some(r.to_number(x)?) };
            let y = if y.is_undefined() { None } else { Some(r.to_number(y)?) };
            scroll_to_css(b, x, y);
        } else {
            let x = arg_num(r, a, 0)?;
            let y = arg_num(r, a, 1)?;
            scroll_to_css(b, Some(x), Some(y));
        }
        Ok(Value::UNDEFINED)
    };
    method(b, r, g, "scrollTo", 2, scroll_to)?;
    method(b, r, g, "scroll", 2, scroll_to)?;
    method(b, r, g, "scrollBy", 2, |b, r, _t, a| {
        let (sx, sy) = scroll_css(b);
        let o = arg(a, 0);
        let (dx, dy) = if o.is_object() {
            let x = r.get(o, "left")?;
            let y = r.get(o, "top")?;
            (r.to_number(x).unwrap_or(0.0), r.to_number(y).unwrap_or(0.0))
        } else {
            (arg_num(r, a, 0)?, arg_num(r, a, 1)?)
        };
        let dx = if dx.is_finite() { dx } else { 0.0 };
        let dy = if dy.is_finite() { dy } else { 0.0 };
        scroll_to_css(b, Some(sx + dx), Some(sy + dy));
        Ok(Value::UNDEFINED)
    })?;
    let screen = r.new_object();
    for (k, v) in [
        ("width", 1920),
        ("height", 1080),
        ("availWidth", 1920),
        ("availHeight", 1080),
        ("colorDepth", 24),
        ("pixelDepth", 24),
        ("availLeft", 0),
        ("availTop", 0),
        ("left", 0),
        ("top", 0),
    ] {
        r.define(screen, k, Value::int(v), Attrs::ENUMERABLE)?;
    }
    r.define(screen, "isExtended", Value::FALSE, Attrs::ENUMERABLE)?;
    for k in ["screenX", "screenY", "screenLeft", "screenTop"] {
        r.define(g, k, Value::int(0), Attrs::DEFAULT)?;
    }
    r.define(g, "crossOriginIsolated", Value::FALSE, Attrs::DEFAULT)?;
    r.define(g, "screen", screen, Attrs::METHOD)?;

    // Timers, frames, microtasks.
    method(b, r, g, "setTimeout", 2, |b, r, _t, a| timer_add(b, r, a, false))?;
    method(b, r, g, "setInterval", 2, |b, r, _t, a| timer_add(b, r, a, true))?;
    method(b, r, g, "clearTimeout", 1, |b, r, _t, a| timer_clear(b, r, a))?;
    method(b, r, g, "clearInterval", 1, |b, r, _t, a| timer_clear(b, r, a))?;
    method(b, r, g, "requestAnimationFrame", 1, |b, r, _t, a| {
        let f = arg(a, 0);
        if !r.is_callable(f) {
            return Err(type_error(r, "requestAnimationFrame: not a function"));
        }
        let h = b.keep_cb(r, f, None)?;
        let id = b.st(r)?.timers.request_frame(h);
        if id == 0 {
            b.drop_cb(r, h);
        }
        Ok(Value::int(id as i32))
    })?;
    method(b, r, g, "cancelAnimationFrame", 1, |b, r, _t, a| {
        let id = r.to_int32(arg(a, 0))?;
        let h = {
            let mut st = b.st(r)?;
            let h = st.timers.frame_handle(id as u32);
            st.timers.cancel_frame(id as u32);
            h
        };
        if let Some(h) = h {
            b.drop_cb(r, h);
        }
        Ok(Value::UNDEFINED)
    })?;
    r.define_method(g, "queueMicrotask", 1, |r, _t, a| {
        let f = arg(a, 0);
        if !r.is_callable(f) {
            return Err(r.throw_error(ErrorKind::TypeError, "queueMicrotask: not a function"));
        }
        r.enqueue_job(f, &[]);
        Ok(Value::UNDEFINED)
    })?;

    // console.
    let con = r.new_object();
    console_fn(b, r, con, "log", ConsoleLevel::Log)?;
    console_fn(b, r, con, "info", ConsoleLevel::Info)?;
    console_fn(b, r, con, "warn", ConsoleLevel::Warn)?;
    console_fn(b, r, con, "error", ConsoleLevel::Error)?;
    console_fn(b, r, con, "debug", ConsoleLevel::Debug)?;
    console_fn(b, r, con, "trace", ConsoleLevel::Debug)?;
    console_fn(b, r, con, "dir", ConsoleLevel::Log)?;
    console_fn(b, r, con, "table", ConsoleLevel::Log)?;
    let b2 = b.clone();
    r.define_method(con, "assert", 0, move |r, _t, a| {
        if !r.to_boolean(arg(a, 0)) {
            let s = console_text(r, a.get(1..).unwrap_or(&[]));
            b2.console(ConsoleLevel::Error, format!("Assertion failed: {s}"));
        }
        Ok(Value::UNDEFINED)
    })?;
    for m in ["group", "groupCollapsed", "groupEnd", "time", "timeEnd", "timeLog", "count", "countReset", "clear"] {
        r.define_method(con, m, 0, |_r, _t, _a| Ok(Value::UNDEFINED))?;
    }
    r.define(g, "console", con, Attrs::METHOD)?;

    // performance.
    let perf = r.new_object();
    let start = b.host().map_or(0.0, |h| h.now());
    let origin = b.host().map_or(0.0, |h| h.time());
    if let Ok(mut st) = b.st.try_borrow_mut() {
        st.perf_origin = start;
        st.time_origin = origin;
    }
    let b2 = b.clone();
    r.define_method(perf, "now", 0, move |_r, _t, _a| {
        let now = b2.host().map_or(start, |h| h.now());
        Ok(Value::number(now - start))
    })?;
    r.define(perf, "timeOrigin", Value::number(origin), Attrs::ENUMERABLE)?;
    r.define(g, "performance", perf, Attrs::METHOD)?;

    // Dialogs.
    method(b, r, g, "alert", 1, |b, r, _t, a| {
        let m = if a.is_empty() { String::new() } else { arg_str(r, a, 0)? };
        b.console(ConsoleLevel::Info, format!("alert: {m}"));
        if let Some(h) = b.host() {
            h.alert(&m);
        }
        Ok(Value::UNDEFINED)
    })?;
    method(b, r, g, "confirm", 1, |b, r, _t, a| {
        let m = if a.is_empty() { String::new() } else { arg_str(r, a, 0)? };
        let ok = b.host().is_some_and(|h| h.confirm(&m));
        Ok(Value::bool(ok))
    })?;
    method(b, r, g, "prompt", 2, |b, r, _t, a| {
        let m = if a.is_empty() { String::new() } else { arg_str(r, a, 0)? };
        let d = if a.len() > 1 { arg_str(r, a, 1)? } else { String::new() };
        match b.host().and_then(|h| h.prompt(&m, &d)) {
            Some(s) => Ok(r.string(&s)),
            None => Ok(Value::NULL),
        }
    })?;
    method(b, r, g, "open", 1, |b, r, _t, a| {
        let u = if a.is_empty() { String::new() } else { arg_str(r, a, 0)? };
        if b.xf.sec.borrow().no_popups {
            b.console(ConsoleLevel::Error, String::from("Blocked opening a new tab from a sandboxed frame: the sandbox does not have 'allow-popups'."));
            return Ok(Value::NULL);
        }
        if let Some(p) = b.page() {
            let abs = p.resolve(&u);
            b.request(PageRequest::Open(abs));
        }
        Ok(Value::NULL)
    })?;
    for m in ["close", "print", "focus", "blur", "stop", "moveTo", "resizeTo"] {
        r.define_method(g, m, 0, |_r, _t, _a| Ok(Value::UNDEFINED))?;
    }
    method(b, r, g, "getComputedStyle", 1, |b, r, _t, a| {
        let Some(n) = node_of(b, r, arg(a, 0)) else {
            return Err(type_error(r, "getComputedStyle: not an Element"));
        };
        let p = r.pinned(&b.protos().style_decl);
        Ok(r.new_host_object(b.classes().computed, n as u64, p))
    })?;
    // new Image(w, h), new Option(text, value, defaultSelected, selected).
    let b2 = b.clone();
    let img_proto = b.protos().by_tag.iter().find(|(t, _)| *t == tag::IMG).map(|(_, p)| r.pinned(p)).unwrap_or(html_element);
    let image = r.new_class(
        "Image",
        0,
        img_proto,
        None,
        Some(Box::new(move |r: &mut Realm, _t: Value, a: &[Value]| {
            let n = {
                let p = b2.page_or(r)?;
                p.dom.create_element(tag::IMG, crate::dom::Ns::Html, Vec::new())
            };
            if !a.is_empty() {
                let w = r.to_int32(arg(a, 0))?;
                set_attribute(&b2, r, n, "width", &format!("{w}"))?;
            }
            if a.len() > 1 {
                let h = r.to_int32(arg(a, 1))?;
                set_attribute(&b2, r, n, "height", &format!("{h}"))?;
            }
            Ok(wrap(&b2, r, n))
        })),
    );
    r.define(g, "Image", image, Attrs::METHOD)?;
    let b2 = b.clone();
    let opt_proto = b.protos().by_tag.iter().find(|(t, _)| *t == tag::OPTION).map(|(_, p)| r.pinned(p)).unwrap_or(html_element);
    let option = r.new_class(
        "Option",
        0,
        opt_proto,
        None,
        Some(Box::new(move |r: &mut Realm, _t: Value, a: &[Value]| {
            let text = if a.is_empty() || arg(a, 0).is_undefined() { String::new() } else { arg_str(r, a, 0)? };
            let n = {
                let p = b2.page_or(r)?;
                let n = p.dom.create_element(tag::OPTION, crate::dom::Ns::Html, Vec::new());
                if !text.is_empty() {
                    let t = p.dom.create_text(text);
                    p.dom.append(n, t);
                }
                n
            };
            if a.len() > 1 && !arg(a, 1).is_undefined() {
                let v = arg_str(r, a, 1)?;
                set_attribute(&b2, r, n, "value", &v)?;
            }
            if a.len() > 2 && r.to_boolean(arg(a, 2)) {
                set_attribute(&b2, r, n, "selected", "")?;
            }
            Ok(wrap(&b2, r, n))
        })),
    );
    r.define(g, "Option", option, Attrs::METHOD)?;
    let _ = element;

    super::net::install_members(b, r, false)?;
    super::net::install_locale_methods(b, r)?;
    super::net::install_dom_exceptions(b, r)?;

    // The parsed page's on* attributes.
    nodes::register_handlers(b, r, 0);
    Ok(())
}

fn list_len(b: &Bridge, r: &mut Realm, t: Value) -> JsResult<usize> {
    let cls = b.classes();
    if let Some(id) = r.host_data(t, cls.list) {
        return Ok(nodes::list_now(b, id as u32).len());
    }
    if let Some(n) = r.host_data(t, cls.tokens) {
        return Ok(tokens(b, n as NodeId).len());
    }
    if r.is_array(t) {
        let v = r.get(t, "length")?;
        return Ok(v.as_number().unwrap_or(0.0) as usize);
    }
    Err(type_error(r, "Illegal invocation"))
}

fn tok_this(b: &Bridge, r: &mut Realm, t: Value) -> JsResult<NodeId> {
    r.host_data(t, b.classes().tokens)
        .map(|n| n as NodeId)
        .ok_or_else(|| type_error(r, "Illegal invocation"))
}

fn set_tokens(b: &Rc<Bridge>, r: &mut Realm, n: NodeId, v: &[String]) -> JsResult<()> {
    set_attribute(b, r, n, "class", &v.join(" "))
}

fn check_token(r: &mut Realm, s: &str) -> JsResult<()> {
    if s.is_empty() {
        return Err(dom_error(r, "SyntaxError", "The token is empty"));
    }
    if s.chars().any(crate::dom::is_html_space) {
        return Err(dom_error(r, "InvalidCharacterError", "The token contains whitespace"));
    }
    Ok(())
}

fn install_tokens(b: &Rc<Bridge>, r: &mut Realm, o: Value) -> JsResult<()> {
    method(b, r, o, "contains", 1, |b, r, t, a| {
        let n = tok_this(b, r, t)?;
        let s = arg_str(r, a, 0)?;
        Ok(Value::bool(tokens(b, n).iter().any(|x| *x == s)))
    })?;
    method(b, r, o, "add", 0, |b, r, t, a| {
        let n = tok_this(b, r, t)?;
        let mut v = tokens(b, n);
        for i in 0..a.len() {
            let s = arg_str(r, a, i)?;
            check_token(r, &s)?;
            if !v.contains(&s) {
                v.push(s);
            }
        }
        set_tokens(b, r, n, &v)?;
        Ok(Value::UNDEFINED)
    })?;
    method(b, r, o, "remove", 0, |b, r, t, a| {
        let n = tok_this(b, r, t)?;
        let mut v = tokens(b, n);
        for i in 0..a.len() {
            let s = arg_str(r, a, i)?;
            check_token(r, &s)?;
            v.retain(|x| *x != s);
        }
        set_tokens(b, r, n, &v)?;
        Ok(Value::UNDEFINED)
    })?;
    method(b, r, o, "toggle", 1, |b, r, t, a| {
        let n = tok_this(b, r, t)?;
        let s = arg_str(r, a, 0)?;
        check_token(r, &s)?;
        let mut v = tokens(b, n);
        let has = v.contains(&s);
        let want = if a.len() > 1 && !arg(a, 1).is_undefined() {
            r.to_boolean(arg(a, 1))
        } else {
            !has
        };
        if want && !has {
            v.push(s);
            set_tokens(b, r, n, &v)?;
        } else if !want && has {
            v.retain(|x| *x != s);
            set_tokens(b, r, n, &v)?;
        }
        Ok(Value::bool(want))
    })?;
    method(b, r, o, "replace", 2, |b, r, t, a| {
        let n = tok_this(b, r, t)?;
        let old = arg_str(r, a, 0)?;
        let new = arg_str(r, a, 1)?;
        check_token(r, &old)?;
        check_token(r, &new)?;
        let mut v = tokens(b, n);
        let Some(i) = v.iter().position(|x| *x == old) else {
            return Ok(Value::FALSE);
        };
        if v.contains(&new) {
            v.remove(i);
        } else {
            v[i] = new;
        }
        set_tokens(b, r, n, &v)?;
        Ok(Value::TRUE)
    })?;
    let value_get: fn(&Rc<Bridge>, &mut Realm, Value, &[Value]) -> JsResult<Value> = |b, r, t, _a| {
        let n = tok_this(b, r, t)?;
        let s = b
            .page()
            .and_then(|p| p.dom.element(n).and_then(|e| e.attr("class")).map(String::from))
            .unwrap_or_default();
        Ok(r.string(&s))
    };
    getter(b, r, o, "value", value_get, Some(|b, r, t, a| {
        let n = tok_this(b, r, t)?;
        let s = arg_str(r, a, 0)?;
        set_attribute(b, r, n, "class", &s)?;
        Ok(Value::UNDEFINED)
    }))?;
    method(b, r, o, "toString", 0, value_get)?;
    Ok(())
}

fn style_this(b: &Bridge, r: &mut Realm, t: Value) -> JsResult<NodeId> {
    r.host_data(t, b.classes().style)
        .map(|n| n as NodeId)
        .ok_or_else(|| type_error(r, "Illegal invocation"))
}

/// The element behind an inline style or a computed one (true), for the methods both have.
fn style_any(b: &Bridge, r: &mut Realm, t: Value) -> JsResult<(NodeId, bool)> {
    if let Some(n) = r.host_data(t, b.classes().computed) {
        return Ok((n as NodeId, true));
    }
    style_this(b, r, t).map(|n| (n, false))
}

fn read_only_style(r: &mut Realm) -> jsengine::Exception {
    dom_error(r, "NoModificationAllowedError", "These styles are computed, and therefore read-only.")
}

fn install_style(b: &Rc<Bridge>, r: &mut Realm, o: Value) -> JsResult<()> {
    getter(b, r, o, "cssText", |b, r, t, _a| {
        let (n, comp) = style_any(b, r, t)?;
        if comp {
            return Ok(r.string(""));
        }
        let s = serialize_decls(&style_decls(b, n));
        Ok(r.string(&s))
    }, Some(|b, r, t, a| {
        if style_any(b, r, t)?.1 {
            return Err(read_only_style(r));
        }
        let n = style_this(b, r, t)?;
        let s = arg_str(r, a, 0)?;
        let d = parse_decls(&s);
        let s = serialize_decls(&d);
        if s.is_empty() {
            remove_attribute(b, r, n, "style")?;
        } else {
            set_attribute(b, r, n, "style", &s)?;
        }
        Ok(Value::UNDEFINED)
    }))?;
    getter(b, r, o, "length", |b, r, t, _a| {
        let (n, comp) = style_any(b, r, t)?;
        if comp {
            return Ok(Value::int(COMPUTED_NAMES.len() as i32));
        }
        Ok(Value::int(style_decls(b, n).len() as i32))
    }, None)?;
    method(b, r, o, "item", 1, |b, r, t, a| {
        let (n, comp) = style_any(b, r, t)?;
        let i = r.to_int32(arg(a, 0))?;
        if comp {
            let s = if i >= 0 { COMPUTED_NAMES.get(i as usize).copied().unwrap_or("") } else { "" };
            return Ok(r.string(s));
        }
        let d = style_decls(b, n);
        let s = d.get(i.max(0) as usize).map(|x| x.0.clone()).unwrap_or_default();
        Ok(r.string(&s))
    })?;
    method(b, r, o, "getPropertyValue", 1, |b, r, t, a| {
        let (n, comp) = style_any(b, r, t)?;
        if comp {
            let name = arg_str(r, a, 0)?;
            let name = if name.starts_with("--") { name } else { name.to_ascii_lowercase() };
            let v = computed(b, n, &name);
            return Ok(r.string(&v));
        }
        let name = arg_str(r, a, 0)?.to_ascii_lowercase();
        let v = style_decls(b, n).into_iter().rev().find(|x| x.0 == name).map(|x| x.1).unwrap_or_default();
        Ok(r.string(&v))
    })?;
    method(b, r, o, "getPropertyPriority", 1, |b, r, t, a| {
        let (n, comp) = style_any(b, r, t)?;
        if comp {
            return Ok(r.string(""));
        }
        let name = arg_str(r, a, 0)?.to_ascii_lowercase();
        let imp = style_decls(b, n).into_iter().rev().find(|x| x.0 == name).is_some_and(|x| x.2);
        Ok(r.string(if imp { "important" } else { "" }))
    })?;
    method(b, r, o, "setProperty", 2, |b, r, t, a| {
        if style_any(b, r, t)?.1 {
            return Err(read_only_style(r));
        }
        let n = style_this(b, r, t)?;
        let name = arg_str(r, a, 0)?;
        let name = if name.starts_with("--") { name } else { name.to_ascii_lowercase() };
        let v = if arg(a, 1).is_nullish() { String::new() } else { arg_str(r, a, 1)? };
        let imp = a.len() > 2 && arg_str(r, a, 2)?.eq_ignore_ascii_case("important");
        set_style_prop(b, r, n, &name, &v, imp)?;
        Ok(Value::UNDEFINED)
    })?;
    method(b, r, o, "removeProperty", 1, |b, r, t, a| {
        if style_any(b, r, t)?.1 {
            return Err(read_only_style(r));
        }
        let n = style_this(b, r, t)?;
        let name = arg_str(r, a, 0)?.to_ascii_lowercase();
        let old = style_decls(b, n).into_iter().rev().find(|x| x.0 == name).map(|x| x.1).unwrap_or_default();
        set_style_prop(b, r, n, &name, "", false)?;
        Ok(r.string(&old))
    })?;
    let c = r.new_class("CSSStyleDeclaration", 0, o, None, None);
    let g = r.global();
    r.define(g, "CSSStyleDeclaration", c, Attrs::METHOD)?;
    Ok(())
}

fn storage_this(b: &Bridge, r: &mut Realm, t: Value) -> JsResult<bool> {
    r.host_data(t, b.classes().storage)
        .map(|d| d == 1)
        .ok_or_else(|| type_error(r, "Illegal invocation"))
}

fn install_storage(b: &Rc<Bridge>, r: &mut Realm, g: Value, o: Value) -> JsResult<()> {
    getter(b, r, o, "length", |b, r, t, _a| {
        let s = storage_this(b, r, t)?;
        Ok(Value::int(b.host().and_then(|h| h.storage(s)).map_or(0, |x| x.len()) as i32))
    }, None)?;
    method(b, r, o, "key", 1, |b, r, t, a| {
        let s = storage_this(b, r, t)?;
        let i = r.to_int32(arg(a, 0))?;
        match b.host().and_then(|h| h.storage(s)).and_then(|x| x.key(i.max(0) as usize)) {
            Some(k) if i >= 0 => Ok(r.string(&k)),
            _ => Ok(Value::NULL),
        }
    })?;
    method(b, r, o, "getItem", 1, |b, r, t, a| {
        let s = storage_this(b, r, t)?;
        let k = arg_str(r, a, 0)?;
        match b.host().and_then(|h| h.storage(s)).and_then(|x| x.get(&k)) {
            Some(v) => Ok(r.string(&v)),
            None => Ok(Value::NULL),
        }
    })?;
    method(b, r, o, "setItem", 2, |b, r, t, a| {
        let s = storage_this(b, r, t)?;
        let k = arg_str(r, a, 0)?;
        let v = arg_str(r, a, 1)?;
        storage_set(b, r, s, &k, &v)?;
        Ok(Value::UNDEFINED)
    })?;
    method(b, r, o, "removeItem", 1, |b, r, t, a| {
        let s = storage_this(b, r, t)?;
        let k = arg_str(r, a, 0)?;
        if let Some(x) = b.host().and_then(|h| h.storage(s)) {
            x.remove(&k);
        }
        Ok(Value::UNDEFINED)
    })?;
    method(b, r, o, "clear", 0, |b, r, t, _a| {
        let s = storage_this(b, r, t)?;
        if let Some(x) = b.host().and_then(|h| h.storage(s)) {
            x.clear();
        }
        Ok(Value::UNDEFINED)
    })?;
    let c = r.new_class("Storage", 0, o, None, None);
    r.define(g, "Storage", c, Attrs::METHOD)?;
    for (name, data) in [("localStorage", 0u64), ("sessionStorage", 1u64)] {
        let b2 = b.clone();
        r.define_getter(
            g,
            name,
            move |r, _t, _| {
                // Not available (no host storage): a SecurityError, as for opaque origins.
                if b2.host().and_then(|h| h.storage(data == 1)).is_none() {
                    return Err(dom_error(r, "SecurityError", "Storage isn't available here"));
                }
                let key = if data == 1 { "sessionStorage" } else { "localStorage" };
                if let Some(p) = b2.st(r)?.helpers.get(&(key, NONE)) {
                    return Ok(r.pinned(p));
                }
                let pv = r.pinned(&b2.protos().storage);
                let ob = r.new_host_object(b2.classes().storage, data, pv);
                let pin = r.pin(ob);
                b2.st(r)?.helpers.insert((key, NONE), pin);
                Ok(ob)
            },
            None,
        )?;
    }
    Ok(())
}
