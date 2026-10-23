//! The builtins. (Stub: the intrinsic objects exist, with the Error constructors; the rest
//! arrive with the engine.)

pub(crate) mod function;

use alloc::vec::Vec;

use crate::api::{ErrorKind, ErrorReport, ScriptOptions};
use crate::names::N;
use crate::object::{ArrayData, ObjKind, WC};
use crate::realm::{Realm, Res};
use crate::value::{ObjId, StrId, Val};

fn noop(_r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    Ok(Val::Undef)
}

pub(crate) fn init(r: &mut Realm) {
    let op = r.alloc(None, ObjKind::Ordinary);
    r.intr.object_proto = op;
    let fp = r.new_builtin(Val::Str(r.nstr(N::empty)), 0, noop);
    r.om(fp).proto = Some(op);
    r.intr.function_proto = fp;
    let ap = r.alloc(
        Some(op),
        ObjKind::Array(ArrayData {
            dense: Vec::new(),
            len: 0,
        }),
    );
    r.intr.array_proto = ap;
    macro_rules! plain {
        ($($f:ident),*) => { $( r.intr.$f = r.alloc(Some(op), ObjKind::Ordinary); )* };
    }
    plain!(
        string_proto,
        number_proto,
        boolean_proto,
        symbol_proto,
        error_proto,
        iterator_proto,
        async_iterator_proto,
        array_iterator_proto,
        map_iterator_proto,
        set_iterator_proto,
        string_iterator_proto,
        regexp_string_iterator_proto,
        generator_proto,
        generator_function_proto,
        async_function_proto,
        async_generator_proto,
        async_generator_function_proto,
        async_from_sync_iterator_proto,
        promise_proto,
        regexp_proto,
        date_proto,
        map_proto,
        set_proto,
        weakmap_proto,
        weakset_proto
    );
    let ep = r.intr.error_proto;
    macro_rules! sub {
        ($($f:ident),*) => { $( r.intr.$f = r.alloc(Some(ep), ObjKind::Ordinary); )* };
    }
    sub!(
        type_error_proto,
        range_error_proto,
        syntax_error_proto,
        reference_error_proto,
        uri_error_proto,
        eval_error_proto,
        aggregate_error_proto
    );
    macro_rules! fns {
        ($($f:ident),*) => { $( r.intr.$f = r.new_builtin(Val::Str(r.nstr(N::empty)), 0, noop); )* };
    }
    fns!(
        object_ctor,
        function_ctor,
        array_ctor,
        promise_ctor,
        error_ctor,
        regexp_ctor,
        array_values,
        array_for_each,
        throw_type_error,
        eval_fn,
        promise_then,
        generator_function_ctor,
        async_function_ctor
    );
    let g = r.alloc(Some(op), ObjKind::Ordinary);
    r.global = g;
    for (kind, proto) in [
        (ErrorKind::Error, r.intr.error_proto),
        (ErrorKind::TypeError, r.intr.type_error_proto),
        (ErrorKind::RangeError, r.intr.range_error_proto),
        (ErrorKind::SyntaxError, r.intr.syntax_error_proto),
        (ErrorKind::ReferenceError, r.intr.reference_error_proto),
        (ErrorKind::URIError, r.intr.uri_error_proto),
        (ErrorKind::EvalError, r.intr.eval_error_proto),
    ] {
        let nk = r.key(N::name);
        let nv = r.str_val(kind.name());
        r.put_prop(proto, nk, nv, WC);
        let mk = r.key(N::message);
        let ev = Val::Str(r.nstr(N::empty));
        r.put_prop(proto, mk, ev, WC);
        let gk = r.key_str(kind.name());
        let ctor = r.new_builtin(nv, 1, noop);
        let pk = r.key(N::prototype);
        r.put_prop(ctor, pk, Val::Obj(proto), 0);
        let ck = r.key(N::constructor);
        r.put_prop(proto, ck, Val::Obj(ctor), WC);
        r.put_prop(g, gk, Val::Obj(ctor), WC);
    }
    let gt = r.key(N::globalThis);
    r.put_prop(g, gt, Val::Obj(g), WC);
}

impl Realm {
    pub(crate) fn eval_script(&mut self, _source: &str, _opts: &ScriptOptions<'_>) -> Res<Val> {
        Err(self.throw_kind(
            ErrorKind::Error,
            "the JavaScript engine is not built yet (api stub)",
        ))
    }
    pub(crate) fn syntax_check(&mut self, _source: &str, _file: &str) -> Result<(), ErrorReport> {
        Ok(())
    }
    pub(crate) fn make_function_from_source(
        &mut self,
        _params: &[&str],
        _body: &str,
        _file: &str,
    ) -> Res<Val> {
        Err(self.throw_kind(
            ErrorKind::Error,
            "the JavaScript engine is not built yet (api stub)",
        ))
    }
    pub(crate) fn json_parse_units(&mut self, _u: &[u16]) -> Res<Val> {
        Err(self.throw_kind(ErrorKind::Error, "JSON is not built yet (api stub)"))
    }
    pub(crate) fn json_stringify_val(
        &mut self,
        _v: Val,
        _replacer: Val,
        _space: Val,
    ) -> Res<Option<StrId>> {
        Err(self.throw_kind(ErrorKind::Error, "JSON is not built yet (api stub)"))
    }
    pub(crate) fn promise_with_resolvers(&mut self) -> (ObjId, Val, Val) {
        let p = self.new_plain();
        let f = self.new_builtin(Val::Str(self.nstr(N::empty)), 1, noop);
        (p, Val::Obj(f), Val::Obj(f))
    }
    pub(crate) fn run_reaction_job(&mut self, _reaction: Val, _arg: Val) -> Res<()> {
        Ok(())
    }
    pub(crate) fn run_thenable_job(&mut self, _p: Val, _t: Val, _then: Val) -> Res<()> {
        Ok(())
    }
    pub(crate) fn track_rejections(&mut self) {}
    pub(crate) fn set_integrity(&mut self, o: ObjId, _frozen: bool) -> Res<()> {
        self.om(o).flags &= !crate::object::EXTENSIBLE;
        Ok(())
    }
}
