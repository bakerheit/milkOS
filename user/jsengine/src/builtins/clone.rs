//! `structuredClone(value, { transfer })`: the structured clone algorithm for the types the
//! engine has: primitives (not symbols), Boolean / Number / String wrappers, Date, RegExp,
//! ArrayBuffer, typed arrays, DataView, Map, Set, Array, Error objects and plain objects,
//! with shared references and cycles preserved. Functions, symbols, promises, weak
//! collections and host objects are a `DataCloneError`. Own enumerable string-keyed
//! properties are copied (getters run); prototypes other than the builtin ones are lost.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::names::N;
use crate::object::{ObjKind, WC};
use crate::realm::{Realm, Res};
use crate::value::{ObjId, Val};

use super::arg;
use super::encoding::dom_exception;

struct Cx {
    memo: BTreeMap<(u32, u32), Val>,
    transfer: Vec<ObjId>,
    count: usize,
}

impl Realm {
    fn clone_fail(&mut self, v: Val) -> crate::api::Exception {
        let d = self.short_display(v);
        dom_exception(
            self,
            "DataCloneError",
            &alloc::format!("{d} could not be cloned."),
        )
    }

    /// Registers a new object so the collector can't take it while script runs (getters).
    fn clone_root(&mut self, cx: &mut Cx, src: ObjId, dst: ObjId) -> ObjId {
        self.temps.push(Val::Obj(dst));
        cx.memo.insert((src.idx, src.g), Val::Obj(dst));
        cx.count += 1;
        dst
    }

    fn clone_value(&mut self, cx: &mut Cx, v: Val) -> Res<Val> {
        let Val::Obj(o) = v else {
            if let Val::Sym(_) = v {
                return Err(self.clone_fail(v));
            }
            return Ok(v.or_undef());
        };
        if let Some(c) = cx.memo.get(&(o.idx, o.g)) {
            return Ok(*c);
        }
        self.check_stack()?;
        if cx.count & 255 == 255 {
            self.check_mem()?;
        }
        self.heap.work += 1;
        let out: ObjId = match &self.o(o).kind {
            ObjKind::BoolBox(b) => {
                let (b, p) = (*b, self.intr.boolean_proto);
                let n = self.alloc(Some(p), ObjKind::BoolBox(b));
                self.clone_root(cx, o, n)
            }
            ObjKind::NumBox(x) => {
                let (x, p) = (*x, self.intr.number_proto);
                let n = self.alloc(Some(p), ObjKind::NumBox(x));
                self.clone_root(cx, o, n)
            }
            ObjKind::StrBox(s) => {
                let (s, p) = (*s, self.intr.string_proto);
                let n = self.alloc(Some(p), ObjKind::StrBox(s));
                self.clone_root(cx, o, n)
            }
            ObjKind::Date(t) => {
                let (t, p) = (*t, self.intr.date_proto);
                let n = self.alloc(Some(p), ObjKind::Date(t));
                self.clone_root(cx, o, n)
            }
            ObjKind::RegExp(_) => {
                let n = self.regexp_create(v, Val::Undef, Val::Undef)?;
                self.clone_root(cx, o, n)
            }
            ObjKind::ArrayBuffer(b) => {
                if b.detached {
                    return Err(dom_exception(
                        self,
                        "DataCloneError",
                        "An ArrayBuffer is detached and could not be cloned.",
                    ));
                }
                let len = b.bytes.len();
                let bytes = if cx.transfer.contains(&o) {
                    self.detach_buffer(o)
                } else {
                    self.check_buffer_size(len)?;
                    self.buf_data(o)
                        .map(|d| d.bytes.clone())
                        .unwrap_or_default()
                };
                self.heap.work += (bytes.len() >> 6) as u64;
                let n = self.buffer_from_vec(bytes);
                self.clone_root(cx, o, n)
            }
            ObjKind::Typed(t) => {
                let (kind, buf, off, len) = (t.kind, t.buf, t.offset, t.len);
                // (a buffer detached by this very transfer is fine: its views follow it)
                if self.buf_detached(buf) && !cx.memo.contains_key(&(buf.idx, buf.g)) {
                    return Err(dom_exception(
                        self,
                        "DataCloneError",
                        "An ArrayBuffer is detached and could not be cloned.",
                    ));
                }
                let nb = self.clone_value(cx, Val::Obj(buf))?;
                let Val::Obj(nb) = nb else {
                    return Err(self.clone_fail(v));
                };
                let n = self.typed_new(kind, nb, off, len, None);
                self.clone_root(cx, o, n)
            }
            ObjKind::DataView(d) => {
                let (buf, off, len) = (d.buf, d.offset, d.len);
                if self.buf_detached(buf) && !cx.memo.contains_key(&(buf.idx, buf.g)) {
                    return Err(dom_exception(
                        self,
                        "DataCloneError",
                        "An ArrayBuffer is detached and could not be cloned.",
                    ));
                }
                let nb = self.clone_value(cx, Val::Obj(buf))?;
                let Val::Obj(nb) = nb else {
                    return Err(self.clone_fail(v));
                };
                let p = self.intr.data_view_proto;
                let n = self.alloc(
                    Some(p),
                    ObjKind::DataView(alloc::boxed::Box::new(crate::typed::ViewData {
                        buf: nb,
                        offset: off,
                        len,
                    })),
                );
                self.clone_root(cx, o, n)
            }
            ObjKind::Map(_) | ObjKind::Set(_) => {
                let is_set = matches!(self.o(o).kind, ObjKind::Set(_));
                let (kind, p) = if is_set {
                    (
                        ObjKind::Set(alloc::boxed::Box::new(super::map::MapData::new())),
                        self.intr.set_proto,
                    )
                } else {
                    (
                        ObjKind::Map(alloc::boxed::Box::new(super::map::MapData::new())),
                        self.intr.map_proto,
                    )
                };
                let n = self.alloc(Some(p), kind);
                self.clone_root(cx, o, n);
                // Snapshot the entries, then clone them (cloning may run getters).
                let it = self.alloc(
                    None,
                    ObjKind::MapIter(alloc::boxed::Box::new(super::map::MapIterData {
                        coll: o,
                        pos: 0,
                        kind: 2,
                        done: false,
                    })),
                );
                self.temps.push(Val::Obj(it));
                let mut ents = Vec::new();
                while let Some((k, val, _)) = self.map_iter_next(it) {
                    self.temps.push(k);
                    self.temps.push(val);
                    ents.push((k, val));
                }
                for (k, val) in ents {
                    let k2 = self.clone_value(cx, k)?;
                    self.temps.push(k2);
                    // (a Set keeps each value as both key and value)
                    let v2 = if is_set {
                        k2
                    } else {
                        self.clone_value(cx, val)?
                    };
                    self.temps.push(v2);
                    self.map_set(n, k2, v2);
                }
                n
            }
            ObjKind::Array(_) => {
                let n = self.new_array_from(Vec::new());
                self.clone_root(cx, o, n);
                let len = self.array_len(o);
                self.array_set_length(n, len);
                self.clone_props(cx, o, n)?;
                n
            }
            ObjKind::Error(_) => {
                let n = self.clone_error(cx, o)?;
                self.clone_root(cx, o, n);
                n
            }
            ObjKind::Ordinary | ObjKind::Arguments => {
                let n = self.new_plain();
                self.clone_root(cx, o, n);
                self.clone_props(cx, o, n)?;
                n
            }
            _ => return Err(self.clone_fail(v)),
        };
        Ok(Val::Obj(out))
    }

    /// Copies the own enumerable string-keyed properties.
    fn clone_props(&mut self, cx: &mut Cx, src: ObjId, dst: ObjId) -> Res<()> {
        let keys = self.own_keys_p(src, true, false, true)?;
        for k in keys {
            // (a getter may have removed the property meanwhile)
            if self.get_own(src, k)?.is_none() {
                continue;
            }
            let val = self.get_p(src, k, Val::Obj(src))?;
            self.temps.push(val);
            let c = self.clone_value(cx, val)?;
            self.temps.push(c);
            self.create_data(dst, k, c)?;
        }
        Ok(())
    }

    fn clone_error(&mut self, cx: &mut Cx, o: ObjId) -> Res<ObjId> {
        use crate::api::ErrorKind as K;
        let nk = self.key(N::name);
        let name_v = self.get_p(o, nk, Val::Obj(o))?;
        let name = match name_v {
            Val::Str(s) => self.rust_string(s),
            _ => alloc::string::String::from("Error"),
        };
        let kind = match name.as_str() {
            "EvalError" => K::EvalError,
            "RangeError" => K::RangeError,
            "ReferenceError" => K::ReferenceError,
            "SyntaxError" => K::SyntaxError,
            "TypeError" => K::TypeError,
            "URIError" => K::URIError,
            _ => K::Error,
        };
        let mk = self.key(N::message);
        let msg = match self.get_own_plain(o, mk) {
            Some(crate::object::Own::Data(v, _)) => {
                let s = self.to_str(v)?;
                Some(Val::Str(s))
            }
            _ => None,
        };
        let p = self.error_proto_for(kind);
        let n = self.make_error_with_proto(p, kind.name(), msg);
        let sk = self.key(N::stack);
        if let Some(crate::object::Own::Data(Val::Str(s), _)) = self.get_own_plain(o, sk) {
            self.put_prop(n, sk, Val::Str(s), WC);
        }
        let ck = self.key(N::cause);
        if let Some(crate::object::Own::Data(cv, _)) = self.get_own_plain(o, ck) {
            self.temps.push(Val::Obj(n));
            let c = self.clone_value(cx, cv)?;
            self.put_prop(n, ck, c, WC);
        }
        Ok(n)
    }
}

fn structured_clone(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    if args.is_empty() {
        return Err(r.type_error("structuredClone requires 1 argument"));
    }
    let mut cx = Cx {
        memo: BTreeMap::new(),
        transfer: Vec::new(),
        count: 0,
    };
    let mark = r.temps.len();
    let res: Res<Val> = (|| {
        if let Val::Obj(opts) = arg(args, 1) {
            let tk = r.key_str("transfer");
            let t = r.get_p(opts, tk, Val::Obj(opts))?;
            if !t.is_undef() {
                let items = r.iterate_to_vec(t)?;
                r.temps.extend_from_slice(&items);
                for it in items {
                    let ok = matches!(it, Val::Obj(b)
                        if matches!(r.o(b).kind, ObjKind::ArrayBuffer(ref d) if !d.detached));
                    let Val::Obj(b) = it else {
                        return Err(dom_exception(r, "DataCloneError", "Value not transferable"));
                    };
                    if !ok || cx.transfer.contains(&b) {
                        return Err(dom_exception(r, "DataCloneError", "Value not transferable"));
                    }
                    cx.transfer.push(b);
                }
            }
        } else if !arg(args, 1).is_nullish() {
            return Err(r.type_error("structuredClone: options must be an object"));
        }
        let out = r.clone_value(&mut cx, args[0])?;
        r.temps.push(out);
        // Buffers in the transfer list that the value never reached are still detached.
        for b in cx.transfer.clone() {
            if !r.buf_detached(b) {
                r.detach_buffer(b);
            }
        }
        Ok(out)
    })();
    r.temps.truncate(mark);
    res
}

pub(crate) fn init(r: &mut Realm) {
    let g = r.global;
    r.method(g, "structuredClone", 1, structured_clone);
}
