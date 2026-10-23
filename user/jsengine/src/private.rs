//! Private class members (`#x`, `#m() {}`, `get #a() {}`, `static #s`, `#x in obj`).
//!
//! Each evaluation of a class makes a fresh private name for every `#name` it declares: a
//! private symbol, kept in a binding of the class scope. A private field, method or accessor
//! is a property of the object under that symbol, so the object's property map is also its
//! brand: reading `obj.#x` on an object without it is a TypeError, `#x in obj` asks whether
//! it has it. Script never holds the symbol, and own-key listings, `Object.freeze` and the
//! like skip private symbols.

use crate::api::Exception;
use crate::object::{ACCESSOR, ObjKind, Prop, PropKey, W};
use crate::realm::{Realm, Res};
use crate::value::{ObjId, Val};

impl Realm {
    /// The private symbol and its readable name (`#x`).
    fn private_key(&mut self, k: Val) -> Res<(crate::value::SymId, alloc::string::String)> {
        match k {
            Val::Sym(s) => {
                let d = self.heap.syms.get(s.0 as usize).and_then(|d| d.desc);
                let name = match d {
                    Some(d) => self.rust_string(d),
                    None => alloc::string::String::from("#?"),
                };
                Ok((s, name))
            }
            _ => Err(self.type_error("internal error: not a private name")),
        }
    }

    pub(crate) fn new_private_name(&mut self, desc: crate::value::StrId) -> Val {
        Val::Sym(self.heap.new_private_sym(desc))
    }

    fn private_slot(&self, o: Val, s: crate::value::SymId) -> Option<(ObjId, Prop)> {
        let Val::Obj(id) = o else { return None };
        self.o(id).props.get(PropKey::Sym(s)).map(|p| (id, *p))
    }

    fn accessor_halves(&self, p: Prop) -> (Val, Val) {
        match p.v {
            Val::Obj(a) => match self.o(a).kind {
                ObjKind::Accessor(g, s) => (g, s),
                _ => (Val::Undef, Val::Undef),
            },
            _ => (Val::Undef, Val::Undef),
        }
    }

    /// `obj.#x`.
    pub(crate) fn private_get(&mut self, o: Val, k: Val) -> Res<Val> {
        let (s, name) = self.private_key(k)?;
        let Some((_, p)) = self.private_slot(o, s) else {
            return Err(self.type_error(&alloc::format!(
                "Cannot read private member {name} from an object whose class did not declare it"
            )));
        };
        if p.f & ACCESSOR != 0 {
            let (g, _) = self.accessor_halves(p);
            if g.is_undef() {
                return Err(self.type_error(&alloc::format!("'{name}' was defined without a getter")));
            }
            return self.call_v(g, o, &[]);
        }
        Ok(p.v)
    }

    /// `obj.#x = v`.
    pub(crate) fn private_set(&mut self, o: Val, k: Val, v: Val) -> Res<()> {
        let (s, name) = self.private_key(k)?;
        let Some((id, p)) = self.private_slot(o, s) else {
            return Err(self.type_error(&alloc::format!(
                "Cannot write private member {name} to an object whose class did not declare it"
            )));
        };
        if p.f & ACCESSOR != 0 {
            let (_, st) = self.accessor_halves(p);
            if st.is_undef() {
                return Err(self.type_error(&alloc::format!("'{name}' was defined without a setter")));
            }
            self.call_v(st, o, &[v])?;
            return Ok(());
        }
        if p.f & W == 0 {
            return Err(self.type_error(&alloc::format!("Private method '{name}' is not writable")));
        }
        if let Some(pos) = self.o(id).props.find(PropKey::Sym(s)) {
            self.om(id).props.ents[pos].1.v = v;
        }
        Ok(())
    }

    /// `#x in obj`.
    pub(crate) fn private_in(&mut self, k: Val, o: Val) -> Res<bool> {
        let (s, name) = self.private_key(k)?;
        if !matches!(o, Val::Obj(_)) {
            let d = self.short_display(o);
            return Err(self.type_error(&alloc::format!(
                "Cannot use 'in' operator to search for '{name}' in {d}"
            )));
        }
        Ok(self.private_slot(o, s).is_some())
    }

    /// A private field is added to `o` (an instance, or a class for a static field).
    pub(crate) fn private_add_field(&mut self, o: Val, k: Val, v: Val) -> Res<()> {
        let (s, name) = self.private_key(k)?;
        let Val::Obj(id) = o else {
            return Err(self.type_error("Cannot define a private field on a non-object"));
        };
        if self.o(id).props.find(PropKey::Sym(s)).is_some() {
            return Err(self.type_error(&alloc::format!(
                "Cannot initialize {name} twice on the same object"
            )));
        }
        self.heap.charge(32);
        self.om(id).props.insert(PropKey::Sym(s), Prop { v, f: W });
        Ok(())
    }

    /// A private method (0), getter (1) or setter (2) is added to `o`.
    pub(crate) fn private_add_method(&mut self, o: Val, k: Val, f: Val, kind: u32) -> Res<()> {
        let (s, name) = self.private_key(k)?;
        let Val::Obj(id) = o else {
            return Err(self.type_error("Cannot define a private method on a non-object"));
        };
        let key = PropKey::Sym(s);
        let existing = self.o(id).props.get(key).copied();
        self.heap.charge(48);
        if kind == 0 {
            if existing.is_some() {
                return Err(self.type_error(&alloc::format!(
                    "Cannot initialize private methods of class twice on the same object ({name})"
                )));
            }
            self.om(id).props.insert(key, Prop { v: f, f: 0 });
            return Ok(());
        }
        match existing {
            Some(p) if p.f & ACCESSOR != 0 => {
                let (g, st) = self.accessor_halves(p);
                let (g, st) = if kind == 1 {
                    if !g.is_undef() {
                        return Err(self.type_error(&alloc::format!(
                            "Cannot initialize private methods of class twice on the same object ({name})"
                        )));
                    }
                    (f, st)
                } else {
                    if !st.is_undef() {
                        return Err(self.type_error(&alloc::format!(
                            "Cannot initialize private methods of class twice on the same object ({name})"
                        )));
                    }
                    (g, f)
                };
                if let Val::Obj(a) = p.v {
                    self.om(a).kind = ObjKind::Accessor(g, st);
                }
                Ok(())
            }
            Some(_) => Err(self.type_error(&alloc::format!(
                "Cannot initialize private methods of class twice on the same object ({name})"
            ))),
            None => {
                let (g, st) = if kind == 1 {
                    (f, Val::Undef)
                } else {
                    (Val::Undef, f)
                };
                let a = self.alloc(None, ObjKind::Accessor(g, st));
                self.om(id).props.insert(
                    key,
                    Prop {
                        v: Val::Obj(a),
                        f: ACCESSOR,
                    },
                );
                Ok(())
            }
        }
    }
}

#[allow(dead_code)]
fn _unused(_: Exception) {}
