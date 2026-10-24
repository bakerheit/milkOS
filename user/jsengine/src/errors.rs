//! Error objects (with where they were made and a stack), error reports for the embedder, and
//! the console-style value formatter (`inspect`).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::api::{ErrorKind, ErrorReport, Exc, Exception};
use crate::names::N;
use crate::object::{ObjKind, Own, PropKey, WC};
use crate::realm::{Realm, Site};
use crate::value::{ObjId, Val};

pub(crate) struct ErrInfo {
    pub file: String,
    pub line: u32,
    pub col: u32,
    /// "    at ..." lines.
    pub frames: String,
    /// The source text around the position (empty when there is none).
    pub snippet: Snippet,
}

/// The characters shown before and after the position of an error in its source line.
const SNIPPET_BEFORE: usize = 60;
const SNIPPET_AFTER: usize = 60;

/// One line of source around a position, cut to a bounded width, for error reports.
#[derive(Clone, Default)]
pub(crate) struct Snippet {
    /// The text on one line (control characters are spaces). Empty when there is none.
    pub text: String,
    /// The character index of the position within `text`.
    pub caret: u32,
}

/// The line of `src` that holds byte offset `pos`, trimmed to at most 60 characters before the
/// position and 60 after it (a minified line of a megabyte shows just the part around it, with
/// `...` where it was cut), leading indentation removed. Looks at a bounded number of bytes,
/// whatever the line length.
pub(crate) fn source_snippet(src: &crate::bytecode::Source, pos: u32) -> Snippet {
    let t: &[u8] = &src.text;
    let pos = (pos as usize).min(t.len());
    let li = match src.line_starts.binary_search(&(pos as u32)) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    let ls = (src.line_starts.get(li).copied().unwrap_or(0) as usize).min(pos);
    let is_nl = |b: u8| b == b'\n' || b == b'\r';
    let mut from = ls.max(pos.saturating_sub(SNIPPET_BEFORE * 4 + 4));
    while from < pos && (t[from] & 0xc0) == 0x80 {
        from += 1;
    }
    let to = (pos + SNIPPET_AFTER * 4 + 4).min(t.len());
    let mut end = to;
    if let Some(i) = t[pos..to].iter().position(|&b| is_nl(b)) {
        end = pos + i;
    } else {
        while end > pos && end < t.len() && (t[end] & 0xc0) == 0x80 {
            end -= 1;
        }
    }
    // (A line break inside the window ends the line; so does the end of the text.)
    let line_continues = end == to && to < t.len() && !is_nl(t[to]);
    let before: Vec<char> = String::from_utf8_lossy(&t[from..pos]).chars().collect();
    let after: Vec<char> = String::from_utf8_lossy(&t[pos..end]).chars().collect();
    let mut cut_l = from > ls;
    let mut b: &[char] = &before;
    if b.len() > SNIPPET_BEFORE {
        b = &b[b.len() - SNIPPET_BEFORE..];
        cut_l = true;
    }
    let mut cut_r = line_continues;
    let mut a: &[char] = &after;
    if a.len() > SNIPPET_AFTER {
        a = &a[..SNIPPET_AFTER];
        cut_r = true;
    }
    if !cut_l {
        let n = b.iter().take_while(|c| **c == ' ' || **c == '\t').count();
        b = &b[n..];
    }
    let mut text = String::new();
    if cut_l {
        text.push_str("...");
    }
    for c in b.iter().chain(a.iter()) {
        text.push(if c.is_control() || *c == '\u{2028}' || *c == '\u{2029}' { ' ' } else { *c });
    }
    if cut_r {
        text.push_str("...");
    }
    if text.trim().is_empty() {
        return Snippet::default();
    }
    Snippet {
        text,
        caret: (b.len() + if cut_l { 3 } else { 0 }) as u32,
    }
}

impl Realm {
    pub(crate) fn error_proto_for(&self, kind: ErrorKind) -> ObjId {
        let t = &self.intr;
        match kind {
            ErrorKind::Error => t.error_proto,
            ErrorKind::TypeError => t.type_error_proto,
            ErrorKind::RangeError => t.range_error_proto,
            ErrorKind::SyntaxError => t.syntax_error_proto,
            ErrorKind::ReferenceError => t.reference_error_proto,
            ErrorKind::URIError => t.uri_error_proto,
            ErrorKind::EvalError => t.eval_error_proto,
        }
    }

    /// The innermost script position and the stack, for a new error or a throw.
    pub(crate) fn current_site(&mut self) -> Site {
        let frames = self.stack_frames(16);
        let mut site = Site::default();
        if let Some(f) = frames.first() {
            site.file = f.1.clone();
            site.line = f.2;
            site.col = f.3;
        }
        if let Some(f) = self.vm.frames.last() {
            site.snippet = source_snippet(&f.code.source, f.code.pos_at(f.pc.saturating_sub(1)));
        }
        let mut s = String::new();
        for (i, (name, file, line, col)) in frames.iter().enumerate() {
            if i > 0 {
                s.push('\n');
            }
            if name.is_empty() {
                let _ = write!(s, "    at {file}:{line}:{col}");
            } else {
                let _ = write!(s, "    at {name} ({file}:{line}:{col})");
            }
        }
        site.stack = s;
        site
    }

    /// An error object with `proto`, `message` (if any) and a `stack`.
    pub(crate) fn make_error_with_proto(
        &mut self,
        proto: ObjId,
        name: &str,
        msg: Option<Val>,
    ) -> ObjId {
        let site = self.current_site();
        let header = match msg {
            Some(Val::Str(s)) if self.heap.str_len(s) > 0 => {
                let m = self.rust_string(s);
                alloc::format!("{name}: {m}")
            }
            _ => String::from(name),
        };
        let stack = if site.stack.is_empty() {
            header
        } else {
            alloc::format!("{header}\n{}", site.stack)
        };
        let e = self.alloc(
            Some(proto),
            ObjKind::Error(alloc::boxed::Box::new(ErrInfo {
                file: site.file,
                line: site.line,
                col: site.col,
                frames: site.stack,
                snippet: site.snippet,
            })),
        );
        let sk = self.key(N::stack);
        let sv = self.str_val(&stack);
        self.put_prop(e, sk, sv, WC);
        if let Some(m) = msg {
            let mk = self.key(N::message);
            self.put_prop(e, mk, m, WC);
        }
        e
    }

    pub(crate) fn make_error(&mut self, kind: ErrorKind, msg: &str) -> ObjId {
        let proto = self.error_proto_for(kind);
        let m = self.str_val(msg);
        self.make_error_with_proto(proto, kind.name(), Some(m))
    }

    pub(crate) fn throw_kind(&mut self, kind: ErrorKind, msg: &str) -> Exception {
        let e = self.make_error(kind, msg);
        self.last_site = Site::default();
        Exception(Exc::Throw(Val::Obj(e)))
    }

    pub(crate) fn type_error(&mut self, msg: &str) -> Exception {
        self.throw_kind(ErrorKind::TypeError, msg)
    }
    pub(crate) fn range_error(&mut self, msg: &str) -> Exception {
        self.throw_kind(ErrorKind::RangeError, msg)
    }
    pub(crate) fn reference_error(&mut self, msg: &str) -> Exception {
        self.throw_kind(ErrorKind::ReferenceError, msg)
    }
    pub(crate) fn syntax_error(&mut self, msg: &str) -> Exception {
        self.throw_kind(ErrorKind::SyntaxError, msg)
    }

    /// A data property along the prototype chain, without running getters.
    pub(crate) fn peek_prop(&mut self, o: ObjId, k: PropKey) -> Option<Val> {
        let mut cur = Some(o);
        let mut n = 0;
        while let Some(c) = cur {
            if let ObjKind::Host { .. } = self.o(c).kind {
                // Host hooks may run script: don't.
            }
            match self.get_own_plain(c, k) {
                Some(Own::Data(v, _)) => return Some(v),
                Some(Own::Accessor(..)) => return None,
                None => {}
            }
            cur = self.o(c).proto;
            n += 1;
            if n > 1000 {
                return None;
            }
        }
        None
    }

    pub(crate) fn peek_string(&mut self, o: ObjId, n: N) -> Option<String> {
        let k = self.key(n);
        match self.peek_prop(o, k) {
            Some(Val::Str(s)) => Some(self.rust_string(s)),
            _ => None,
        }
    }

    pub(crate) fn error_report(&mut self, v: Val) -> ErrorReport {
        if let Val::Obj(o) = v
            && let ObjKind::Error(info) = &self.o(o).kind
        {
            let (file, line, column, frames) =
                (info.file.clone(), info.line, info.col, info.frames.clone());
            let snippet = info.snippet.clone();
            let name = self
                .peek_string(o, N::name)
                .unwrap_or_else(|| String::from("Error"));
            let message = self.peek_string(o, N::message).unwrap_or_default();
            return ErrorReport {
                name,
                message,
                file,
                line,
                column,
                stack: frames,
                snippet: snippet.text,
                snippet_col: snippet.caret,
                uncatchable: false,
            };
        }
        let site = self.last_site.clone();
        let message = self.inspect_val(v);
        ErrorReport {
            name: String::from("Uncaught"),
            message,
            file: site.file,
            line: site.line,
            column: site.col,
            stack: site.stack,
            snippet: site.snippet.text,
            snippet_col: site.snippet.caret,
            uncatchable: false,
        }
    }

    // ---- inspect ----

    pub(crate) fn inspect_val(&mut self, v: Val) -> String {
        let mut out = String::new();
        let mut seen: Vec<ObjId> = Vec::new();
        self.inspect_into(&mut out, v, 0, true, &mut seen);
        out
    }

    fn quote_str(&mut self, out: &mut String, s: crate::value::StrId) {
        let r = self.rust_string(s);
        out.push('\'');
        for c in r.chars().take(10_000) {
            match c {
                '\'' => out.push_str("\\'"),
                '\n' => out.push_str("\\n"),
                '\\' => out.push_str("\\\\"),
                c => out.push(c),
            }
        }
        out.push('\'');
    }

    fn ctor_name(&mut self, o: ObjId) -> Option<String> {
        let p = self.o(o).proto?;
        let ck = self.key(N::constructor);
        let Some(Val::Obj(c)) = self.peek_prop(p, ck) else {
            return None;
        };
        let nk = self.key(N::name);
        match self.get_own_plain(c, nk) {
            Some(Own::Data(Val::Str(s), _)) => Some(self.rust_string(s)),
            _ => None,
        }
    }

    pub(crate) fn inspect_into(
        &mut self,
        out: &mut String,
        v: Val,
        depth: u32,
        top: bool,
        seen: &mut Vec<ObjId>,
    ) {
        if out.len() > 100_000 {
            return;
        }
        match v {
            Val::Undef | Val::Empty => out.push_str("undefined"),
            Val::Null => out.push_str("null"),
            Val::Bool(b) => out.push_str(if b { "true" } else { "false" }),
            Val::Int(i) => {
                let _ = write!(out, "{i}");
            }
            Val::Num(n) => {
                if n == 0.0 && n.is_sign_negative() {
                    out.push_str("-0");
                } else {
                    out.push_str(&crate::numconv::number_to_string(n));
                }
            }
            Val::Str(s) => {
                if top {
                    let r = self.rust_string(s);
                    out.push_str(&r);
                } else {
                    self.quote_str(out, s);
                }
            }
            Val::Sym(s) => {
                let d = self.heap.syms.get(s.0 as usize).and_then(|d| d.desc);
                match d {
                    Some(d) => {
                        let r = self.rust_string(d);
                        let _ = write!(out, "Symbol({r})");
                    }
                    None => out.push_str("Symbol()"),
                }
            }
            Val::Obj(o) => self.inspect_obj(out, o, depth, top, seen),
        }
    }

    fn inspect_obj(
        &mut self,
        out: &mut String,
        o: ObjId,
        depth: u32,
        top: bool,
        seen: &mut Vec<ObjId>,
    ) {
        if seen.contains(&o) {
            out.push_str("[Circular]");
            return;
        }
        if let Some(s) = self.inspect_special(o, depth, top) {
            out.push_str(&s);
            return;
        }
        if self.callable(Val::Obj(o)) {
            let n = self.fn_name(o);
            if self.is_class_ctor(o) {
                let _ = write!(
                    out,
                    "class {}",
                    if n.is_empty() { "(anonymous)" } else { &n }
                );
            } else {
                let _ = write!(out, "ƒ {n}()");
            }
            return;
        }
        if let ObjKind::Error(_) = self.o(o).kind {
            let k = self.key(N::stack);
            if let Some(Val::Str(s)) = self.peek_prop(o, k) {
                let r = self.rust_string(s);
                out.push_str(&r);
            } else {
                let name = self
                    .peek_string(o, N::name)
                    .unwrap_or_else(|| String::from("Error"));
                let msg = self.peek_string(o, N::message).unwrap_or_default();
                if msg.is_empty() {
                    out.push_str(&name);
                } else {
                    let _ = write!(out, "{name}: {msg}");
                }
            }
            return;
        }
        let is_array = matches!(self.o(o).kind, ObjKind::Array(_));
        if depth > 2 {
            out.push_str(if is_array { "[…]" } else { "{…}" });
            return;
        }
        seen.push(o);
        let keys = self.own_keys_p(o, true, true, true).unwrap_or_default();
        if is_array {
            let len = self.array_len(o);
            out.push('[');
            let mut first = true;
            let mut holes = 0u32;
            let shown = len.min(100);
            for i in 0..shown {
                match self.get_own_plain(o, PropKey::Idx(i)) {
                    Some(own) => {
                        if holes > 0 {
                            if !first {
                                out.push_str(", ");
                            }
                            let _ = write!(out, "empty × {holes}");
                            holes = 0;
                            first = false;
                        }
                        if !first {
                            out.push_str(", ");
                        }
                        first = false;
                        match own {
                            Own::Data(v, _) => self.inspect_into(out, v, depth + 1, false, seen),
                            Own::Accessor(..) => out.push_str("[Getter/Setter]"),
                        }
                    }
                    None => holes += 1,
                }
            }
            if holes > 0 {
                if !first {
                    out.push_str(", ");
                }
                let _ = write!(out, "empty × {holes}");
                first = false;
            }
            if len > shown {
                if !first {
                    out.push_str(", ");
                }
                let _ = write!(out, "… {} more", len - shown);
                first = false;
            }
            for k in keys {
                if let PropKey::Idx(_) = k {
                    continue;
                }
                if !first {
                    out.push_str(", ");
                }
                first = false;
                self.inspect_prop(out, o, k, depth, seen);
            }
            out.push(']');
        } else {
            let tag = match self.o(o).kind {
                ObjKind::Host { class, .. } => self
                    .classes
                    .get(class as usize)
                    .map(|c| String::from(c.class.name())),
                _ => None,
            };
            let name = tag.or_else(|| self.ctor_name(o));
            match name.as_deref() {
                Some("Object") | None => {}
                Some(n) => {
                    out.push_str(n);
                    out.push(' ');
                }
            }
            if let Some(prefix) = self.inspect_box(o) {
                out.push_str(&prefix);
                if keys.is_empty() {
                    seen.pop();
                    return;
                }
                out.push(' ');
            }
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                if i >= 50 {
                    out.push('…');
                    break;
                }
                self.inspect_prop(out, o, *k, depth, seen);
            }
            out.push('}');
        }
        seen.pop();
    }

    fn inspect_box(&mut self, o: ObjId) -> Option<String> {
        match self.o(o).kind {
            ObjKind::NumBox(n) => Some(alloc::format!(
                "[Number: {}]",
                crate::numconv::number_to_string(n)
            )),
            ObjKind::BoolBox(b) => Some(alloc::format!("[Boolean: {b}]")),
            ObjKind::StrBox(s) => {
                let r = self.rust_string(s);
                Some(alloc::format!("[String: '{r}']"))
            }
            _ => None,
        }
    }

    fn inspect_prop(
        &mut self,
        out: &mut String,
        o: ObjId,
        k: PropKey,
        depth: u32,
        seen: &mut Vec<ObjId>,
    ) {
        match k {
            PropKey::Str(s) => {
                let r = self.rust_string(s);
                let ident = !r.is_empty()
                    && r.chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
                    && !r.starts_with(|c: char| c.is_ascii_digit());
                if ident {
                    out.push_str(&r);
                } else {
                    self.quote_str(out, s);
                }
            }
            PropKey::Idx(i) => {
                let _ = write!(out, "{i}");
            }
            PropKey::Sym(_) => {
                out.push('[');
                let kv = self.key_to_val(k);
                self.inspect_into(out, kv, depth, false, seen);
                out.push(']');
            }
        }
        out.push_str(": ");
        match self.get_own_plain(o, k) {
            Some(Own::Data(v, _)) => self.inspect_into(out, v, depth + 1, false, seen),
            Some(Own::Accessor(g, s, _)) => out.push_str(match (g.is_undef(), s.is_undef()) {
                (false, false) => "[Getter/Setter]",
                (false, true) => "[Getter]",
                _ => "[Setter]",
            }),
            None => {
                // A host hook's live property: don't run it from a formatter.
                out.push('…');
            }
        }
    }
}

impl Realm {
    /// Console renderings of the builtin kinds that aren't plain objects.
    pub(crate) fn inspect_special(&mut self, o: ObjId, depth: u32, _top: bool) -> Option<String> {
        let mut out = String::new();
        match &self.o(o).kind {
            ObjKind::Date(t) => {
                let t = *t;
                return Some(if t.is_nan() {
                    String::from("Invalid Date")
                } else {
                    self.date_to_string(t)
                });
            }
            ObjKind::RegExp(d) => {
                let (s, f) = (d.source, d.flags);
                let s = self.rust_string(s);
                let f = self.rust_string(f);
                return Some(alloc::format!("/{s}/{f}"));
            }
            ObjKind::Promise(p) => {
                let (st, v) = (p.state, p.result);
                out.push_str("Promise {");
                match st {
                    crate::builtins::promise::PState::Pending => out.push_str("<pending>"),
                    crate::builtins::promise::PState::Fulfilled => {
                        let mut seen = Vec::new();
                        self.inspect_into(&mut out, v, depth + 1, false, &mut seen);
                    }
                    crate::builtins::promise::PState::Rejected => {
                        out.push_str("<rejected> ");
                        let mut seen = Vec::new();
                        self.inspect_into(&mut out, v, depth + 1, false, &mut seen);
                    }
                }
                out.push('}');
                return Some(out);
            }
            ObjKind::Map(_) | ObjKind::Set(_) => {
                let is_set = matches!(self.o(o).kind, ObjKind::Set(_));
                let it = self.alloc(
                    None,
                    ObjKind::MapIter(alloc::boxed::Box::new(crate::builtins::map::MapIterData {
                        coll: o,
                        pos: 0,
                        kind: 2,
                        done: false,
                    })),
                );
                let mut items = Vec::new();
                while let Some((k, v, _)) = self.map_iter_next(it) {
                    items.push((k, v));
                    if items.len() > 100 {
                        break;
                    }
                }
                let _ = write!(
                    out,
                    "{}({}) {{",
                    if is_set { "Set" } else { "Map" },
                    items.len()
                );
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    let mut seen = Vec::new();
                    if depth > 1 {
                        out.push('…');
                        break;
                    }
                    self.inspect_into(&mut out, *k, depth + 1, false, &mut seen);
                    if !is_set {
                        out.push_str(" => ");
                        self.inspect_into(&mut out, *v, depth + 1, false, &mut seen);
                    }
                }
                out.push('}');
                return Some(out);
            }
            ObjKind::Typed(t) => {
                let (name, total) = (t.kind.name(), t.len);
                let len = self.typed_len(o);
                let shown = len.min(100);
                let _ = write!(out, "{name}({total}) [");
                for i in 0..shown {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    if let Some(v) = self.typed_get(o, i) {
                        let mut seen = Vec::new();
                        self.inspect_into(&mut out, v, depth + 1, false, &mut seen);
                    }
                }
                if len > shown {
                    let _ = write!(out, ", … {} more", len - shown);
                }
                out.push(']');
                return Some(out);
            }
            ObjKind::ArrayBuffer(b) => {
                let n = b.bytes.len();
                return Some(alloc::format!("ArrayBuffer {{ byteLength: {n} }}"));
            }
            ObjKind::DataView(d) => {
                let (n, off) = (d.len, d.offset);
                return Some(alloc::format!(
                    "DataView {{ byteLength: {n}, byteOffset: {off} }}"
                ));
            }
            ObjKind::SymBox(s) => {
                let s = *s;
                let d = self.symbol_descriptive(Val::Sym(s));
                return Some(alloc::format!("[Symbol: {d}]"));
            }
            ObjKind::Coroutine(_) => return Some(String::from("Object [Generator] {}")),
            ObjKind::Namespace(ns) => {
                let entries: Vec<(PropKey, Val)> = ns
                    .order
                    .iter()
                    .zip(ns.targets.iter())
                    .map(|(k, t)| (*k, self.ns_value(t)))
                    .collect();
                out.push_str("[Module: null prototype] {");
                if depth > 2 {
                    out.push_str(" … }");
                    return Some(out);
                }
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    if i >= 50 {
                        out.push_str(" …");
                        break;
                    }
                    out.push(' ');
                    let name = self.key_display(*k);
                    out.push_str(&name);
                    out.push_str(": ");
                    if v.is_empty() {
                        out.push_str("<uninitialized>");
                    } else {
                        let mut seen = Vec::new();
                        self.inspect_into(&mut out, *v, depth + 1, false, &mut seen);
                    }
                }
                out.push_str(if entries.is_empty() { "}" } else { " }" });
                return Some(out);
            }
            _ => {}
        }
        None
    }
}
