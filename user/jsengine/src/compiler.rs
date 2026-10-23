//! Bytecode generation from the AST, after scope analysis (`resolve.rs`) has said which
//! bindings are captured. `finally` blocks are compiled inline on every exit path (normal,
//! break/continue/return) and once more as the exception handler; the exception regions
//! are split around the inlined copies so an exception inside a copy goes to the right
//! handler.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use crate::ast::*;
use crate::atoms::{Atom, Atoms, kw};
use crate::bytecode::{Code, CodeKind, Const, ENV_SLOT_BITS, Handler, Op, Source, env_ref};
use crate::realm::Realm;
use crate::resolve::{BK, Loc, SK, Scope};
use crate::value::StrId;

pub(crate) struct CompileError {
    pub msg: String,
    pub pos: u32,
}

type CResult<T> = Result<T, CompileError>;

const MAX_LOCALS: u32 = 60_000;
const MAX_ENV_DEPTH: u32 = 250;

struct Region {
    segs: Vec<(u32, u32)>,
    open: Option<u32>,
    target: u32,
    finally: bool,
}

enum Jc<'a> {
    /// A break/continue target: loops, switches (`plain`: an unlabelled break stops here) and
    /// labelled statements.
    Break {
        labels: Vec<Atom>,
        is_loop: bool,
        plain: bool,
        breaks: Vec<usize>,
        continues: Vec<usize>,
    },
    /// A try statement: its regions are suspended on the way out, and its finally block (if
    /// any) is run.
    Finally {
        block: Option<&'a Block>,
        regions: Vec<usize>,
    },
    IterClose {
        slot: u32,
        region: usize,
    },
    PopEnv,
}

struct FnState<'a> {
    ops: Vec<Op>,
    consts: Vec<Const>,
    str_consts: BTreeMap<Vec<u16>, u32>,
    handlers: Vec<Handler>,
    lines: Vec<(u32, u32)>,
    nlocals: u32,
    nparams: u32,
    scope_chain: Vec<ScopeId>,
    ctx: Vec<Jc<'a>>,
    regions: Vec<Region>,
    kind: CodeKind,
    strict: bool,
    is_async: bool,
    is_generator: bool,
    completion: Option<u32>,
    /// `this` needs a TDZ check (inside a derived constructor, or an arrow in one).
    this_chk: bool,
    keeps_extra_args: bool,
}

pub(crate) struct Compiler<'a> {
    pub realm: &'a mut Realm,
    atoms: &'a Atoms,
    scopes: Vec<Scope>,
    source: Rc<Source>,
    fns: Vec<FnState<'a>>,
    atom_strs: Vec<Option<StrId>>,
    /// The class constructor being compiled next has instance fields to initialize.
    ctor_fields: bool,
    /// Compiling a module: every Code made belongs to it.
    module: Option<crate::api::ModuleId>,
    /// The hidden `*default*` binding's atom (named "default" when it names a function).
    default_atom: Option<Atom>,
}

#[derive(Clone, Copy)]
enum Lookup {
    Local(u32, BK),
    Env(u32, u32, BK),
    Global,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Store {
    /// An assignment (TDZ and const checks).
    Assign,
    /// A declaration's initialization.
    Init,
}

fn is_anon_fn(e: &Expr) -> bool {
    match &e.k {
        ExprKind::Func(f) => f.name.is_none(),
        ExprKind::Class(c) => c.name.is_none(),
        // Parentheses don't hide it: `var f = (function () {})` names the function f.
        ExprKind::Seq(items) if items.len() == 1 => is_anon_fn(&items[0]),
        _ => false,
    }
}

impl<'a> Compiler<'a> {
    pub fn new(
        realm: &'a mut Realm,
        atoms: &'a Atoms,
        scopes: Vec<Scope>,
        source: Rc<Source>,
    ) -> Compiler<'a> {
        Compiler {
            realm,
            atoms,
            scopes,
            source,
            fns: Vec::new(),
            atom_strs: Vec::new(),
            ctor_fields: false,
            module: None,
            default_atom: None,
        }
    }

    fn err<T>(&self, msg: &str, pos: u32) -> CResult<T> {
        Err(CompileError {
            msg: String::from(msg),
            pos,
        })
    }

    // ---- function state helpers ----

    fn f(&mut self) -> &mut FnState<'a> {
        let n = self.fns.len() - 1;
        &mut self.fns[n]
    }

    fn fr(&self) -> &FnState<'a> {
        &self.fns[self.fns.len() - 1]
    }

    fn emit(&mut self, op: Op) -> usize {
        let f = self.f();
        f.ops.push(op);
        f.ops.len() - 1
    }

    fn here(&self) -> u32 {
        self.fr().ops.len() as u32
    }

    fn mark(&mut self, pos: u32) {
        let pc = self.here();
        let f = self.f();
        if let Some(last) = f.lines.last_mut()
            && last.0 == pc
        {
            last.1 = pos;
            return;
        }
        f.lines.push((pc, pos));
    }

    fn patch(&mut self, at: usize, target: u32) {
        let op = &mut self.f().ops[at];
        *op = match *op {
            Op::Jump(_) => Op::Jump(target),
            Op::JumpIfFalse(_) => Op::JumpIfFalse(target),
            Op::JumpIfTrue(_) => Op::JumpIfTrue(target),
            Op::JumpIfFalseKeep(_) => Op::JumpIfFalseKeep(target),
            Op::JumpIfTrueKeep(_) => Op::JumpIfTrueKeep(target),
            Op::JumpIfNotNullishKeep(_) => Op::JumpIfNotNullishKeep(target),
            Op::JumpIfNullish(_) => Op::JumpIfNullish(target),
            Op::JumpIfNotUndef(_) => Op::JumpIfNotUndef(target),
            Op::IterStep(s, _) => Op::IterStep(s, target),
            Op::ForInNext(s, _) => Op::ForInNext(s, target),
            Op::AsyncIterStep(s, _) => Op::AsyncIterStep(s, target),
            other => other,
        };
    }

    fn patch_here(&mut self, at: usize) {
        let h = self.here();
        self.patch(at, h);
    }

    fn alloc_local(&mut self, pos: u32) -> CResult<u32> {
        let f = self.f();
        let l = f.nlocals;
        f.nlocals += 1;
        if f.nlocals > MAX_LOCALS {
            return self.err("too many local variables", pos);
        }
        Ok(l)
    }

    fn atom_str(&mut self, a: Atom) -> StrId {
        let i = a as usize;
        if i >= self.atom_strs.len() {
            self.atom_strs.resize(i + 1, None);
        }
        if let Some(s) = self.atom_strs[i] {
            return s;
        }
        let text = if Some(a) == self.default_atom {
            "default"
        } else {
            self.atoms.get(a)
        };
        let s = self.realm.heap.intern_str(text);
        self.atom_strs[i] = Some(s);
        s
    }

    fn const_units(&mut self, u: &[u16]) -> u32 {
        if let Some(&i) = self.fr().str_consts.get(u) {
            return i;
        }
        let s = self.realm.heap.intern(u);
        let f = self.f();
        let i = f.consts.len() as u32;
        f.consts.push(Const::Str(s));
        f.str_consts.insert(u.to_vec(), i);
        i
    }

    fn const_atom(&mut self, a: Atom) -> u32 {
        let u: Vec<u16> = self.atoms.get(a).encode_utf16().collect();
        self.const_units(&u)
    }

    fn const_str(&mut self, s: &str) -> u32 {
        let u: Vec<u16> = s.encode_utf16().collect();
        self.const_units(&u)
    }

    fn const_num(&mut self, n: f64) -> u32 {
        let f = self.f();
        for (i, c) in f.consts.iter().enumerate() {
            if let Const::Num(m) = c
                && m.to_bits() == n.to_bits()
            {
                return i as u32;
            }
        }
        f.consts.push(Const::Num(n));
        f.consts.len() as u32 - 1
    }

    fn add_const(&mut self, c: Const) -> u32 {
        let f = self.f();
        f.consts.push(c);
        f.consts.len() as u32 - 1
    }

    fn push_num(&mut self, n: f64) {
        if n >= i32::MIN as f64
            && n <= i32::MAX as f64
            && (n as i32) as f64 == n
            && !(n == 0.0 && n.is_sign_negative())
        {
            self.emit(Op::Int(n as i32));
        } else {
            let c = self.const_num(n);
            self.emit(Op::Const(c));
        }
    }

    fn prop_name_units(&self, k: &PropName) -> Option<Vec<u16>> {
        match k {
            PropName::Ident(a) => Some(self.atoms.get(*a).encode_utf16().collect()),
            PropName::Str(s) => Some(s.clone()),
            PropName::Num(n) => Some(crate::string::utf16(&crate::numconv::number_to_string(*n))),
            PropName::Computed(_) => None,
            PropName::Private(a) => Some(self.atoms.get(*a).encode_utf16().collect()),
        }
    }

    // ---- regions (exception handlers) ----

    fn open_region(&mut self, finally: bool) -> usize {
        let h = self.here();
        let f = self.f();
        f.regions.push(Region {
            segs: Vec::new(),
            open: Some(h),
            target: 0,
            finally,
        });
        f.regions.len() - 1
    }

    fn suspend_region(&mut self, r: usize) {
        let h = self.here();
        let reg = &mut self.f().regions[r];
        if let Some(s) = reg.open.take()
            && h > s
        {
            reg.segs.push((s, h));
        }
    }

    fn resume_region(&mut self, r: usize) {
        let h = self.here();
        let reg = &mut self.f().regions[r];
        if reg.open.is_none() {
            reg.open = Some(h);
        }
    }

    fn close_region(&mut self, r: usize) {
        self.suspend_region(r);
    }

    fn finish_region(&mut self, r: usize, target: u32) {
        self.suspend_region(r);
        let f = self.f();
        let reg = &mut f.regions[r];
        reg.target = target;
        let fin = reg.finally;
        let segs = core::mem::take(&mut reg.segs);
        for (s, e) in segs {
            f.handlers.push(Handler {
                start: s,
                end: e,
                target,
                depth: 0,
                finally: fin,
            });
        }
    }

    // ---- scopes ----

    fn cur_scope(&self) -> ScopeId {
        *self.fr().scope_chain.last().unwrap_or(&0)
    }

    /// Assigns locations to a scope's bindings and emits its entry (environment, TDZ marks).
    fn enter_scope(&mut self, sid: ScopeId, pos: u32) -> CResult<()> {
        let n = self.scopes[sid as usize].bindings.len();
        let mut env = 0u32;
        for i in 0..n {
            let b = self.scopes[sid as usize].bindings[i];
            let loc = if b.captured {
                env += 1;
                Loc::Env(env - 1)
            } else {
                Loc::Local(self.alloc_local(pos)?)
            };
            self.scopes[sid as usize].bindings[i].loc = loc;
        }
        self.scopes[sid as usize].env_size = env;
        if env > 0 {
            self.emit(Op::PushEnv(env));
        }
        self.f().scope_chain.push(sid);
        self.tdz_init(sid);
        Ok(())
    }

    fn tdz_init(&mut self, sid: ScopeId) {
        let n = self.scopes[sid as usize].bindings.len();
        for i in 0..n {
            let b = self.scopes[sid as usize].bindings[i];
            if b.kind.is_lexical() {
                self.emit(Op::Empty);
                match b.loc {
                    Loc::Local(l) => {
                        self.emit(Op::PutLocal(l));
                    }
                    Loc::Env(e) => {
                        self.emit(Op::PutEnv(env_ref(0, e)));
                    }
                    Loc::Unassigned => {
                        self.emit(Op::Pop);
                    }
                }
            }
        }
    }

    fn leave_scope(&mut self, sid: ScopeId) {
        if self.scopes[sid as usize].env_size > 0 {
            self.emit(Op::PopEnv);
        }
        self.f().scope_chain.pop();
    }

    fn lookup(&self, name: Atom) -> Lookup {
        let mut sid = Some(self.cur_scope());
        let mut depth = 0u32;
        while let Some(s) = sid {
            let sc = &self.scopes[s as usize];
            if sc.kind == SK::Script {
                return Lookup::Global;
            }
            if let Some(i) = sc.find(name) {
                let b = sc.bindings[i];
                // A parameter that is still in its TDZ is checked like a `let`.
                let kind = if b.tdz && b.kind == BK::Param {
                    BK::Let
                } else {
                    b.kind
                };
                return match b.loc {
                    Loc::Local(l) => Lookup::Local(l, kind),
                    Loc::Env(e) => Lookup::Env(depth, e, kind),
                    Loc::Unassigned => Lookup::Global,
                };
            }
            if sc.env_size > 0 {
                depth += 1;
            }
            sid = sc.parent;
        }
        Lookup::Global
    }

    fn load_name(&mut self, a: Atom, pos: u32) -> CResult<()> {
        match self.lookup(a) {
            Lookup::Local(l, k) => {
                if k.is_lexical() {
                    let n = self.const_atom(a);
                    self.emit(Op::GetLocalChk(l, n));
                } else {
                    self.emit(Op::GetLocal(l));
                }
            }
            Lookup::Env(d, s, k) => {
                if d > MAX_ENV_DEPTH {
                    return self.err("scopes nested too deeply", pos);
                }
                if k == BK::Import {
                    let n = self.const_atom(a);
                    self.mark(pos);
                    self.emit(Op::GetImport(env_ref(d, s), n));
                } else if k.is_lexical() {
                    let n = self.const_atom(a);
                    self.emit(Op::GetEnvChk(env_ref(d, s), n));
                } else {
                    self.emit(Op::GetEnv(env_ref(d, s)));
                }
            }
            Lookup::Global => {
                if a == kw::UNDEFINED {
                    // Fast path: `undefined` is a non-writable global.
                    self.emit(Op::Undef);
                    return Ok(());
                }
                self.mark(pos);
                let n = self.const_atom(a);
                self.emit(Op::GetGlobal(n));
            }
        }
        Ok(())
    }

    /// Stores the top of stack to a binding; `keep` leaves the value on the stack.
    fn store_name(&mut self, a: Atom, mode: Store, keep: bool, pos: u32) -> CResult<()> {
        let strict = self.fr().strict;
        match self.lookup(a) {
            Lookup::Local(l, k) => {
                if mode == Store::Assign && matches!(k, BK::Const | BK::FuncName) {
                    if k == BK::FuncName && !strict {
                        if !keep {
                            self.emit(Op::Pop);
                        }
                        return Ok(());
                    }
                    let n = self.const_atom(a);
                    self.mark(pos);
                    if k == BK::Const {
                        // A TDZ read comes first (`x = 1` before `const x`).
                        self.emit(Op::GetLocalChk(l, n));
                        self.emit(Op::Pop);
                    }
                    self.emit(Op::ThrowConst(n));
                    return Ok(());
                }
                if mode == Store::Assign && k.is_lexical() {
                    let n = self.const_atom(a);
                    self.mark(pos);
                    self.emit(Op::SetLocalChk(l, n));
                    if !keep {
                        self.emit(Op::Pop);
                    }
                } else if keep {
                    self.emit(Op::SetLocal(l));
                } else {
                    self.emit(Op::PutLocal(l));
                }
            }
            Lookup::Env(d, s, k) => {
                if d > MAX_ENV_DEPTH {
                    return self.err("scopes nested too deeply", pos);
                }
                let r = env_ref(d, s);
                if k == BK::Import {
                    // Imports are read-only views: any store throws.
                    let n = self.const_atom(a);
                    self.mark(pos);
                    self.emit(Op::GetImport(r, n));
                    self.emit(Op::Pop);
                    self.emit(Op::ThrowConst(n));
                    return Ok(());
                }
                if mode == Store::Assign && matches!(k, BK::Const | BK::FuncName) {
                    if k == BK::FuncName && !strict {
                        if !keep {
                            self.emit(Op::Pop);
                        }
                        return Ok(());
                    }
                    let n = self.const_atom(a);
                    self.mark(pos);
                    if k == BK::Const {
                        self.emit(Op::GetEnvChk(r, n));
                        self.emit(Op::Pop);
                    }
                    self.emit(Op::ThrowConst(n));
                    return Ok(());
                }
                if mode == Store::Assign && k.is_lexical() {
                    let n = self.const_atom(a);
                    self.mark(pos);
                    self.emit(Op::SetEnvChk(r, n));
                    if !keep {
                        self.emit(Op::Pop);
                    }
                } else if keep {
                    self.emit(Op::SetEnv(r));
                } else {
                    self.emit(Op::PutEnv(r));
                }
            }
            Lookup::Global => {
                let n = self.const_atom(a);
                self.mark(pos);
                let is_lex_init = mode == Store::Init && self.script_lexical(a);
                if is_lex_init {
                    if keep {
                        self.emit(Op::Dup);
                    }
                    self.emit(Op::InitLex(n));
                } else {
                    self.emit(Op::SetGlobal(n));
                    if !keep {
                        self.emit(Op::Pop);
                    }
                }
            }
        }
        Ok(())
    }

    /// Whether `a` is a let/const/class declared at script level (of this script).
    fn script_lexical(&self, a: Atom) -> bool {
        let mut sid = Some(self.cur_scope());
        while let Some(s) = sid {
            let sc = &self.scopes[s as usize];
            if sc.kind == SK::Script {
                return sc.find(a).is_some_and(|i| sc.bindings[i].kind.is_lexical());
            }
            sid = sc.parent;
        }
        false
    }

    // ---- entry: scripts ----

    pub fn compile_script(&mut self, s: &'a Script) -> CResult<Rc<Code>> {
        self.fns.push(new_fn(CodeKind::Script, s.strict));
        self.f().scope_chain.push(s.scope);
        let c = self.alloc_local(0)?;
        self.f().completion = Some(c);
        // Declarations: vars and functions on the global object, lexical names in the
        // global lexical environment.
        let n = self.scopes[s.scope as usize].bindings.len();
        for i in 0..n {
            let b = self.scopes[s.scope as usize].bindings[i];
            let name = self.const_atom(b.name);
            match b.kind {
                BK::Let | BK::Class => {
                    self.emit(Op::DeclareLex(name, 0));
                }
                BK::Const => {
                    self.emit(Op::DeclareLex(name, 1));
                }
                _ => {
                    self.emit(Op::DeclareVar(name));
                }
            }
        }
        self.hoist_functions(&s.body)?;
        self.stmts(&s.body)?;
        self.emit(Op::GetLocal(c));
        self.emit(Op::Return);
        let st = self
            .fns
            .pop()
            .unwrap_or_else(|| new_fn(CodeKind::Script, false));
        Ok(self.finish(st, None, 0, (0, self.source.text.len() as u32)))
    }

    /// Compiles a module into two codes that run in the module's own environment (made by the
    /// linker, with one slot per top-level binding, in binding order): the first creates the
    /// closures of the function declarations (it runs at link time), the second is the body
    /// (an async function when the module uses top-level await). Returns them and the
    /// environment size.
    pub fn compile_module(
        &mut self,
        m: &'a ModuleAst,
        id: crate::api::ModuleId,
    ) -> CResult<(Rc<Code>, Rc<Code>, u32)> {
        self.module = Some(id);
        self.default_atom = Some(m.info.default_atom);
        let sid = m.scope;
        let n = self.scopes[sid as usize].bindings.len();
        if n as u32 >= (1 << ENV_SLOT_BITS) {
            return self.err("too many top-level bindings", 0);
        }
        for i in 0..n {
            self.scopes[sid as usize].bindings[i].loc = Loc::Env(i as u32);
        }
        self.scopes[sid as usize].env_size = n as u32;
        let span = (0, self.source.text.len() as u32);
        // Link-time code: the hoisted functions.
        self.fns.push(new_fn(CodeKind::Normal, true));
        self.f().scope_chain.push(sid);
        self.hoist_functions(&m.body)?;
        self.emit(Op::ReturnUndef);
        let st = self.fns.pop().unwrap_or_else(|| new_fn(CodeKind::Normal, true));
        let init = self.finish(st, None, 0, span);
        // The body.
        let mut st = new_fn(CodeKind::Normal, true);
        st.is_async = m.info.has_tla;
        self.fns.push(st);
        self.f().scope_chain.push(sid);
        self.stmts(&m.body)?;
        self.emit(Op::ReturnUndef);
        let st = self.fns.pop().unwrap_or_else(|| new_fn(CodeKind::Normal, true));
        let body = self.finish(st, None, 0, span);
        Ok((init, body, n as u32))
    }

    /// Creates the closures of the function declarations directly in `body` (at scope entry).
    fn hoist_functions(&mut self, body: &'a [Stmt]) -> CResult<()> {
        for st in body {
            let st = if let StmtKind::Labeled(_, inner) = &st.k {
                inner
            } else {
                st
            };
            if let StmtKind::FuncDecl(f) = &st.k {
                let Some(name) = f.name else { continue };
                let code = self.compile_function(f, None)?;
                let ci = self.add_const(Const::Code(code));
                self.emit(Op::Closure(ci));
                self.store_name(name, Store::Init, false, st.pos)?;
            }
        }
        Ok(())
    }

    fn finish(
        &mut self,
        st: FnState<'a>,
        name: Option<StrId>,
        length: u32,
        span: (u32, u32),
    ) -> Rc<Code> {
        let keys = st
            .consts
            .iter()
            .map(|c| match c {
                Const::Str(s) => self.realm.key_from_str(*s),
                _ => crate::object::PropKey::Idx(u32::MAX),
            })
            .collect();
        let mut ops = st.ops;
        crate::bytecode::fuse(&mut ops);
        Rc::new(Code {
            ops,
            consts: st.consts,
            keys,
            handlers: st.handlers,
            lines: st.lines,
            nparams: st.nparams,
            nlocals: st.nlocals,
            length,
            name,
            kind: st.kind,
            strict: st.strict,
            is_async: st.is_async,
            is_generator: st.is_generator,
            keeps_extra_args: st.keeps_extra_args,
            source: self.source.clone(),
            span,
            marked: Cell::new(0),
            module: self.module,
        })
    }

    // ---- functions ----

    pub fn compile_function(&mut self, f: &'a Func, name_hint: Option<StrId>) -> CResult<Rc<Code>> {
        if self.fns.len() > 200 {
            return self.err("functions nested too deeply", f.start);
        }
        let kind = match f.kind {
            FuncKind::Normal => CodeKind::Normal,
            FuncKind::Arrow => CodeKind::Arrow,
            FuncKind::Method | FuncKind::Getter | FuncKind::Setter => CodeKind::Method,
            FuncKind::ClassCtor => CodeKind::ClassCtor,
            FuncKind::DerivedCtor => CodeKind::DerivedCtor,
            FuncKind::FieldInit => CodeKind::FieldInit,
        };
        let outer_this_chk = self.fns.last().is_some_and(|s| s.this_chk);
        let init_fields = kind == CodeKind::ClassCtor && core::mem::take(&mut self.ctor_fields);
        let mut st = new_fn(kind, f.strict);
        st.is_async = f.is_async;
        st.is_generator = f.is_generator;
        st.this_chk = kind == CodeKind::DerivedCtor || (kind == CodeKind::Arrow && outer_this_chk);
        let nparams = f.params.len() as u32;
        st.nparams = nparams;
        st.nlocals = nparams;
        st.keeps_extra_args = f.rest.is_some() || self.scopes[f.scope as usize].uses_arguments;
        self.fns.push(st);
        // A named function expression's own name.
        if let Some(ns) = f.name_scope {
            let has = self.scopes[ns as usize]
                .bindings
                .first()
                .is_some_and(|b| b.captured);
            if has {
                self.scopes[ns as usize].bindings[0].loc = Loc::Env(0);
                self.scopes[ns as usize].env_size = 1;
            } else {
                self.scopes[ns as usize].env_size = 0;
            }
        }
        let r = self.function_body(f, init_fields);
        let st = self.fns.pop().unwrap_or_else(|| new_fn(kind, false));
        r?;
        let name = match f.name {
            Some(a) => Some(self.atom_str(a)),
            None => name_hint,
        };
        let length = f.params.iter().take_while(|p| p.default.is_none()).count() as u32;
        let name = match f.kind {
            FuncKind::Getter | FuncKind::Setter | FuncKind::FieldInit => name,
            _ => name,
        };
        Ok(self.finish(st, name, length, (f.start, f.end)))
    }

    fn function_body(&mut self, f: &'a Func, init_fields: bool) -> CResult<()> {
        let sid = f.scope;
        // Parameter slots: simple uncaptured identifier parameters live in their argument slot.
        let n = self.scopes[sid as usize].bindings.len();
        let mut param_local: Vec<(usize, u32)> = Vec::new();
        for (i, p) in f.params.iter().enumerate() {
            if let Pat::Ident(a, _) = p.pat
                && let Some(bi) = self.scopes[sid as usize].find(a)
            {
                let b = self.scopes[sid as usize].bindings[bi];
                if !b.captured && b.kind == BK::Param && !b.tdz {
                    // A repeated name binds the last parameter with it.
                    param_local.retain(|(x, _)| *x != bi);
                    param_local.push((bi, i as u32));
                }
            }
        }
        let mut env = 0u32;
        for i in 0..n {
            let b = self.scopes[sid as usize].bindings[i];
            let loc = if b.captured {
                env += 1;
                Loc::Env(env - 1)
            } else if let Some((_, slot)) = param_local.iter().find(|(x, _)| *x == i) {
                Loc::Local(*slot)
            } else {
                Loc::Local(self.alloc_local(f.start)?)
            };
            self.scopes[sid as usize].bindings[i].loc = loc;
        }
        self.scopes[sid as usize].env_size = env;
        if env > 0 {
            self.emit(Op::PushEnv(env));
        }
        self.f().scope_chain.push(sid);
        // arguments
        if self.scopes[sid as usize].uses_arguments {
            self.emit(Op::ArgumentsObj);
            self.store_name(kw::ARGUMENTS, Store::Init, false, f.start)?;
        }
        // Parameters with defaults or patterns start out uninitialized.
        for i in 0..n {
            let b = self.scopes[sid as usize].bindings[i];
            if b.tdz && b.kind == BK::Param {
                self.emit(Op::Empty);
                match b.loc {
                    Loc::Local(l) => self.emit(Op::PutLocal(l)),
                    Loc::Env(e) => self.emit(Op::PutEnv(env_ref(0, e))),
                    Loc::Unassigned => self.emit(Op::Pop),
                };
            }
        }
        // Parameters: defaults and patterns, in order.
        for (i, p) in f.params.iter().enumerate() {
            let i = i as u32;
            match (&p.pat, &p.default) {
                (Pat::Ident(a, pos), None) => {
                    let repeated_later = f.params[i as usize + 1..]
                        .iter()
                        .any(|q| matches!(q.pat, Pat::Ident(b, _) if b == *a));
                    if !repeated_later && !matches!(self.lookup(*a), Lookup::Local(l, _) if l == i)
                    {
                        self.emit(Op::GetLocal(i));
                        self.store_name(*a, Store::Init, false, *pos)?;
                    }
                }
                (pat, default) => {
                    self.emit(Op::GetLocal(i));
                    if let Some(d) = default {
                        self.default_value(d, pat)?;
                    }
                    self.destructure(pat, Store::Init)?;
                }
            }
        }
        if let Some(r) = &f.rest {
            let np = f.params.len() as u32;
            self.emit(Op::RestArgs(np));
            self.destructure(r, Store::Init)?;
        }
        // TDZ marks for the body's lexical declarations; hoisted functions.
        for i in 0..n {
            let b = self.scopes[sid as usize].bindings[i];
            if b.kind.is_lexical() {
                self.emit(Op::Empty);
                match b.loc {
                    Loc::Local(l) => self.emit(Op::PutLocal(l)),
                    Loc::Env(e) => self.emit(Op::PutEnv(env_ref(0, e))),
                    Loc::Unassigned => self.emit(Op::Pop),
                };
            }
        }
        if init_fields {
            self.emit(Op::InitFields);
        }
        match &f.body {
            Body::Stmts(body) => {
                self.hoist_functions(body)?;
                if f.is_generator {
                    self.emit(Op::GenStart);
                }
                self.stmts(body)?;
                self.emit(Op::ReturnUndef);
            }
            Body::Expr(e) => {
                self.expr(e)?;
                self.emit(Op::Return);
            }
        }
        self.f().scope_chain.pop();
        Ok(())
    }

    /// A function expression (or arrow) as a value.
    fn func_expr(&mut self, f: &'a Func, name_hint: Option<StrId>) -> CResult<()> {
        if let Some(ns) = f.name_scope {
            self.scopes[ns as usize].parent = Some(self.cur_scope());
        }
        let code = self.compile_function(f, name_hint)?;
        let ci = self.add_const(Const::Code(code));
        let name_env = f
            .name_scope
            .is_some_and(|ns| self.scopes[ns as usize].env_size > 0);
        if name_env {
            self.emit(Op::PushEnv(1));
            self.emit(Op::Closure(ci));
            self.emit(Op::Dup);
            self.emit(Op::PutEnv(env_ref(0, 0)));
            self.emit(Op::PopEnv);
        } else {
            self.emit(Op::Closure(ci));
        }
        Ok(())
    }

    /// Compiles an expression, naming it if it's an anonymous function or class.
    fn named_expr(&mut self, e: &'a Expr, name: Option<Atom>) -> CResult<()> {
        if let Some(n) = name
            && is_anon_fn(e)
        {
            let s = self.atom_str(n);
            return self.named_expr_str(e, s);
        }
        self.expr(e)
    }

    fn named_expr_str(&mut self, e: &'a Expr, s: StrId) -> CResult<()> {
        match &e.k {
            ExprKind::Func(f) if f.name.is_none() => self.func_expr(f, Some(s)),
            ExprKind::Class(c) if c.name.is_none() => self.class(c, Some(s)),
            ExprKind::Seq(items) if items.len() == 1 && is_anon_fn(&items[0]) => {
                self.named_expr_str(&items[0], s)
            }
            _ => self.expr(e),
        }
    }

    // ---- classes ----

    fn class(&mut self, c: &'a Class, name_hint: Option<StrId>) -> CResult<()> {
        let pos = c.start;
        self.scopes[c.scope as usize].parent = Some(self.cur_scope());
        self.enter_scope(c.scope, pos)?;
        let name = match c.name {
            Some(a) => Some(self.atom_str(a)),
            None => name_hint,
        };
        let has_heritage = c.heritage.is_some();
        // Fresh private names for this evaluation of the class.
        let mut made: Vec<Atom> = Vec::new();
        for pd in &c.privates {
            if made.contains(&pd.name) {
                continue;
            }
            made.push(pd.name);
            let desc = self.const_atom(pd.name);
            self.emit(Op::NewPrivName(desc));
            self.store_name(pd.name, Store::Init, false, pos)?;
        }
        if let Some(h) = &c.heritage {
            self.expr(h)?;
        }
        // The constructor.
        self.ctor_fields = c.fields.is_some() && !has_heritage;
        let ctor_code = match &c.ctor {
            Some(f) => {
                let mut code = self.compile_function(f, name)?;
                if let Some(cm) = Rc::get_mut(&mut code) {
                    cm.span = (c.start, c.end);
                    cm.name = name;
                }
                code
            }
            None => {
                let fields = core::mem::take(&mut self.ctor_fields);
                self.default_ctor(has_heritage, fields, name, (c.start, c.end))
            }
        };
        let ci = self.add_const(Const::Code(ctor_code));
        self.mark(pos);
        self.emit(Op::MakeClass(ci, has_heritage as u32));
        // [F proto]
        let nkeys = c.field_keys.len();
        let (ik, sk) = if nkeys > 0 {
            let ik = self.alloc_local(pos)?;
            let sk = self.alloc_local(pos)?;
            self.emit(Op::NewArray(0));
            self.emit(Op::PutLocal(ik));
            self.emit(Op::NewArray(0));
            self.emit(Op::PutLocal(sk));
            (Some(ik), Some(sk))
        } else {
            (None, None)
        };
        let mut next_key = 0usize;
        for (mi, m) in c.members.iter().enumerate() {
            while next_key < nkeys && c.field_key_at[next_key] as usize == mi {
                self.field_key(c, next_key, ik, sk)?;
                next_key += 1;
            }
            let (f, kind) = match &m.kind {
                MemberKind::Method(f) => (f, 0u32),
                MemberKind::Get(f) => (f, 1),
                MemberKind::Set(f) => (f, 2),
            };
            if m.is_static {
                self.emit(Op::Swap);
            }
            match &m.key {
                PropName::Private(pn) => {
                    let ns = self.atom_str(*pn);
                    let code = self.compile_function(f, Some(ns))?;
                    let fc = self.add_const(Const::Code(code));
                    if m.is_static {
                        // [proto F]: the function goes straight onto the class.
                        self.load_name(*pn, pos)?;
                        self.emit(Op::Closure(fc));
                        self.emit(Op::DefinePrivMethod(kind));
                    } else {
                        // Each instance gets it when it is made: keep it for the field
                        // initializer.
                        let want = match kind {
                            1 => PrivKind::Getter,
                            2 => PrivKind::Setter,
                            _ => PrivKind::Method,
                        };
                        let fb = c
                            .privates
                            .iter()
                            .find(|p| p.name == *pn && p.kind == want && !p.is_static)
                            .and_then(|p| p.func);
                        self.emit(Op::Closure(fc));
                        match fb {
                            Some(fb) => self.store_name(fb, Store::Init, false, pos)?,
                            None => {
                                self.emit(Op::Pop);
                            }
                        }
                    }
                }
                PropName::Computed(k) => {
                    self.expr(k)?;
                    self.emit(Op::ToPropKey);
                    let code = self.compile_function(f, None)?;
                    let fc = self.add_const(Const::Code(code));
                    self.emit(Op::Closure(fc));
                    self.emit(Op::InitMethodElem(kind));
                }
                key => {
                    let u = self.prop_name_units(key).unwrap_or_default();
                    let ns = self.realm.heap.intern(&u);
                    let code = self.compile_function(f, Some(ns))?;
                    let fc = self.add_const(Const::Code(code));
                    self.emit(Op::Closure(fc));
                    let kc = self.const_units(&u);
                    self.emit(Op::InitMethod(kc, kind));
                }
            }
            if m.is_static {
                self.emit(Op::Swap);
            }
        }
        while next_key < nkeys {
            self.field_key(c, next_key, ik, sk)?;
            next_key += 1;
        }
        self.emit(Op::Pop); // proto
        // [F]
        if let Some(fi) = &c.fields {
            self.scopes[fi.scope as usize].parent = Some(self.cur_scope());
            let code = self.compile_function(fi, None)?;
            let fc = self.add_const(Const::Code(code));
            match ik {
                Some(ik) => self.emit(Op::GetLocal(ik)),
                None => self.emit(Op::NewArray(0)),
            };
            self.emit(Op::SetFieldInit(fc));
        }
        // The class's own name binding, before static initializers run.
        if let Some(a) = c.name {
            self.emit(Op::Dup);
            self.store_name(a, Store::Init, false, pos)?;
        }
        if let Some(si) = &c.statics {
            self.scopes[si.scope as usize].parent = Some(self.cur_scope());
            let code = self.compile_function(si, None)?;
            let fc = self.add_const(Const::Code(code));
            match sk {
                Some(sk) => self.emit(Op::GetLocal(sk)),
                None => self.emit(Op::NewArray(0)),
            };
            self.mark(pos);
            self.emit(Op::RunStaticInit(fc));
        }
        self.leave_scope(c.scope);
        Ok(())
    }

    fn field_key(
        &mut self,
        c: &'a Class,
        i: usize,
        ik: Option<u32>,
        sk: Option<u32>,
    ) -> CResult<()> {
        let local = if c.field_key_static[i] { sk } else { ik };
        let Some(local) = local else { return Ok(()) };
        self.emit(Op::GetLocal(local));
        self.expr(&c.field_keys[i])?;
        self.emit(Op::ToPropKey);
        self.emit(Op::ArrPush);
        self.emit(Op::Pop);
        Ok(())
    }

    fn default_ctor(
        &mut self,
        derived: bool,
        fields: bool,
        name: Option<StrId>,
        span: (u32, u32),
    ) -> Rc<Code> {
        let kind = if derived {
            CodeKind::DerivedCtor
        } else {
            CodeKind::ClassCtor
        };
        let mut st = new_fn(kind, true);
        if fields && !derived {
            st.ops.push(Op::InitFields);
        }
        if derived {
            st.keeps_extra_args = true;
            st.this_chk = true;
            st.ops.push(Op::RestArgs(0));
            st.ops.push(Op::SuperCallSpread);
            st.ops.push(Op::Pop);
        }
        st.ops.push(Op::ReturnUndef);
        st.lines.push((0, span.0));
        self.finish(st, name, 0, span)
    }

    // ---- statements ----

    fn stmts(&mut self, body: &'a [Stmt]) -> CResult<()> {
        for s in body {
            self.stmt(s)?;
        }
        Ok(())
    }

    fn block(&mut self, b: &'a Block) -> CResult<()> {
        self.scopes[b.scope as usize].parent = Some(self.cur_scope());
        self.enter_scope(b.scope, 0)?;
        self.hoist_block_functions(&b.body)?;
        self.stmts(&b.body)?;
        self.leave_scope(b.scope);
        Ok(())
    }

    /// Block-level function declarations: their closures exist from the block's start.
    fn hoist_block_functions(&mut self, body: &'a [Stmt]) -> CResult<()> {
        for st in body {
            let st = if let StmtKind::Labeled(_, inner) = &st.k {
                inner
            } else {
                st
            };
            if let StmtKind::FuncDecl(f) = &st.k {
                let Some(name) = f.name else { continue };
                self.scopes[f.scope as usize].parent = Some(self.cur_scope());
                let code = self.compile_function(f, None)?;
                let ci = self.add_const(Const::Code(code));
                self.emit(Op::Closure(ci));
                self.store_name(name, Store::Init, false, st.pos)?;
            }
        }
        Ok(())
    }

    fn stmt(&mut self, s: &'a Stmt) -> CResult<()> {
        if self.realm.stack.exceeded() {
            return self.err("the code is nested too deeply", s.pos);
        }
        self.mark(s.pos);
        match &s.k {
            StmtKind::Expr(e) => {
                self.expr(e)?;
                match self.fr().completion {
                    Some(c) => {
                        self.emit(Op::PutLocal(c));
                    }
                    None => {
                        self.emit(Op::Pop);
                    }
                }
            }
            StmtKind::Var(kind, decls) => {
                for d in decls {
                    match &d.init {
                        Some(init) => {
                            self.named_expr(init, pat_name(&d.pat))?;
                            let mode = if *kind == VarKind::Var {
                                Store::Assign
                            } else {
                                Store::Init
                            };
                            self.destructure_mode(&d.pat, mode, *kind == VarKind::Var)?;
                        }
                        None => {
                            if *kind != VarKind::Var {
                                self.emit(Op::Undef);
                                self.destructure(&d.pat, Store::Init)?;
                            }
                        }
                    }
                }
            }
            StmtKind::FuncDecl(f) => {
                // Hoisted already when the binding is in the current scope; a lone function
                // in a sloppy if-body (no block) is created here.
                let Some(name) = f.name else { return Ok(()) };
                let sid = self.cur_scope();
                let in_scope = self.scopes[sid as usize].find(name).is_some();
                if !in_scope {
                    self.scopes[f.scope as usize].parent = Some(sid);
                    let code = self.compile_function(f, None)?;
                    let ci = self.add_const(Const::Code(code));
                    self.emit(Op::Closure(ci));
                    self.store_name(name, Store::Assign, false, s.pos)?;
                } else {
                    let sc = &self.scopes[sid as usize];
                    let annex = sc.find(name).is_some_and(|i| sc.bindings[i].annex_b)
                        && sc.kind == SK::Block;
                    if annex {
                        // Annex B: copy the block binding to the function-level var.
                        self.load_name(name, s.pos)?;
                        self.store_to_func_var(name, s.pos)?;
                    }
                }
            }
            StmtKind::ClassDecl(c) => {
                self.class(c, None)?;
                if let Some(n) = c.name {
                    self.store_name(n, Store::Init, false, s.pos)?;
                }
            }
            StmtKind::Return(e) => {
                match e {
                    Some(e) => self.expr(e)?,
                    None => {
                        self.emit(Op::Undef);
                    }
                }
                self.emit_return(s.pos)?;
            }
            StmtKind::If(t, a, b) => {
                self.expr(t)?;
                let jf = self.emit(Op::JumpIfFalse(0));
                self.sub_stmt(a)?;
                match b {
                    Some(b) => {
                        let je = self.emit(Op::Jump(0));
                        self.patch_here(jf);
                        self.sub_stmt(b)?;
                        self.patch_here(je);
                    }
                    None => self.patch_here(jf),
                }
            }
            StmtKind::Block(b) => self.block(b)?,
            StmtKind::For {
                init,
                test,
                update,
                body,
                scope,
            } => self.for_loop(init, test, update, body, *scope, Vec::new(), s.pos)?,
            StmtKind::ForIn {
                left,
                right,
                body,
                scope,
            } => self.for_in(left, right, body, *scope, Vec::new(), s.pos)?,
            StmtKind::ForOf {
                left,
                right,
                body,
                scope,
                is_await,
            } => self.for_of(left, right, body, *scope, *is_await, Vec::new(), s.pos)?,
            StmtKind::While(t, b) => self.while_loop(t, b, Vec::new())?,
            StmtKind::DoWhile(b, t) => self.do_while(b, t, Vec::new())?,
            StmtKind::Break(l) => self.emit_jump(*l, false, s.pos)?,
            StmtKind::Continue(l) => self.emit_jump(*l, true, s.pos)?,
            StmtKind::Throw(e) => {
                self.expr(e)?;
                self.mark(s.pos);
                self.emit(Op::Throw);
            }
            StmtKind::Try {
                block,
                param,
                handler,
                catch_scope,
                finalizer,
            } => self.try_stmt(block, param, handler, *catch_scope, finalizer, s.pos)?,
            StmtKind::Switch { disc, cases, scope } => {
                self.switch(disc, cases, *scope, Vec::new(), s.pos)?
            }
            StmtKind::Labeled(l, b) => self.labeled(alloc::vec![*l], b)?,
            StmtKind::Empty => {}
            StmtKind::Debugger => {
                self.emit(Op::Nop);
            }
            StmtKind::PrivMethods(list) => {
                for i in list {
                    self.emit(Op::This);
                    self.load_name(i.name, s.pos)?;
                    self.load_name(i.func, s.pos)?;
                    self.mark(s.pos);
                    self.emit(Op::DefinePrivMethod(i.kind as u32));
                    self.emit(Op::Pop);
                }
            }
            StmtKind::Fields(defs) => {
                for d in defs {
                    self.mark(d.pos);
                    self.emit(Op::This);
                    match &d.key {
                        FieldKey::Name(PropName::Private(pn)) => {
                            self.load_name(*pn, d.pos)?;
                            match &d.value {
                                Some(v) => {
                                    if is_anon_fn(v) {
                                        let ns = self.atom_str(*pn);
                                        self.named_expr_str(v, ns)?;
                                    } else {
                                        self.expr(v)?;
                                    }
                                }
                                None => {
                                    self.emit(Op::Undef);
                                }
                            }
                            self.mark(d.pos);
                            self.emit(Op::DefinePrivField);
                        }
                        FieldKey::Computed(i) => {
                            self.emit(Op::FieldKey(*i));
                            match &d.value {
                                Some(v) => {
                                    self.expr(v)?;
                                    if is_anon_fn(v) {
                                        self.emit(Op::SetFnName);
                                    }
                                }
                                None => {
                                    self.emit(Op::Undef);
                                }
                            }
                            self.emit(Op::DefineFieldElem);
                        }
                        FieldKey::Name(k) => {
                            let u = self.prop_name_units(k).unwrap_or_default();
                            match &d.value {
                                Some(v) => {
                                    if is_anon_fn(v) {
                                        let ns = self.realm.heap.intern(&u);
                                        self.named_expr_str(v, ns)?;
                                    } else {
                                        self.expr(v)?;
                                    }
                                }
                                None => {
                                    self.emit(Op::Undef);
                                }
                            }
                            let kc = self.const_units(&u);
                            self.emit(Op::DefineField(kc));
                        }
                    }
                    self.emit(Op::Pop);
                }
            }
        }
        Ok(())
    }

    fn store_to_func_var(&mut self, name: Atom, pos: u32) -> CResult<()> {
        // Resolve from the function scope (skipping the block's own binding).
        let fscope = self.scopes[self.cur_scope() as usize].func;
        let saved = core::mem::take(&mut self.f().scope_chain);
        self.f().scope_chain = alloc::vec![fscope];
        // Count the environments between the current scope and the function scope.
        let mut extra = 0;
        let mut s = Some(*saved.last().unwrap_or(&fscope));
        while let Some(x) = s {
            if x == fscope {
                break;
            }
            if self.scopes[x as usize].env_size > 0 {
                extra += 1;
            }
            s = self.scopes[x as usize].parent;
        }
        let r = match self.lookup(name) {
            Lookup::Local(l, _) => {
                self.emit(Op::PutLocal(l));
                Ok(())
            }
            Lookup::Env(d, sl, _) => {
                self.emit(Op::PutEnv(env_ref(d + extra, sl)));
                Ok(())
            }
            Lookup::Global => {
                let n = self.const_atom(name);
                self.mark(pos);
                self.emit(Op::SetGlobal(n));
                self.emit(Op::Pop);
                Ok(())
            }
        };
        self.f().scope_chain = saved;
        r
    }

    fn sub_stmt(&mut self, s: &'a Stmt) -> CResult<()> {
        self.stmt(s)
    }

    fn labeled(&mut self, mut labels: Vec<Atom>, body: &'a Stmt) -> CResult<()> {
        match &body.k {
            StmtKind::Labeled(l, b) => {
                labels.push(*l);
                self.labeled(labels, b)
            }
            StmtKind::For {
                init,
                test,
                update,
                body: b,
                scope,
            } => self.for_loop(init, test, update, b, *scope, labels, body.pos),
            StmtKind::ForIn {
                left,
                right,
                body: b,
                scope,
            } => self.for_in(left, right, b, *scope, labels, body.pos),
            StmtKind::ForOf {
                left,
                right,
                body: b,
                scope,
                is_await,
            } => self.for_of(left, right, b, *scope, *is_await, labels, body.pos),
            StmtKind::While(t, b) => self.while_loop(t, b, labels),
            StmtKind::DoWhile(b, t) => self.do_while(b, t, labels),
            StmtKind::Switch { disc, cases, scope } => {
                self.switch(disc, cases, *scope, labels, body.pos)
            }
            _ => {
                self.f().ctx.push(Jc::Break {
                    labels,
                    is_loop: false,
                    plain: false,
                    breaks: Vec::new(),
                    continues: Vec::new(),
                });
                self.stmt(body)?;
                self.pop_break_ctx(0);
                Ok(())
            }
        }
    }

    /// Pops a Break context, patching its breaks to here and its continues to `cont`.
    fn pop_break_ctx(&mut self, cont: u32) {
        if let Some(Jc::Break {
            breaks, continues, ..
        }) = self.f().ctx.pop()
        {
            for b in breaks {
                self.patch_here(b);
            }
            for c in continues {
                self.patch(c, cont);
            }
        }
    }

    fn while_loop(&mut self, t: &'a Expr, b: &'a Stmt, labels: Vec<Atom>) -> CResult<()> {
        let top = self.here();
        self.expr(t)?;
        let jf = self.emit(Op::JumpIfFalse(0));
        self.f().ctx.push(Jc::Break {
            labels,
            is_loop: true,
            plain: true,
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        self.sub_stmt(b)?;
        self.emit(Op::Jump(top));
        self.patch_here(jf);
        self.pop_break_ctx(top);
        Ok(())
    }

    fn do_while(&mut self, b: &'a Stmt, t: &'a Expr, labels: Vec<Atom>) -> CResult<()> {
        let top = self.here();
        self.f().ctx.push(Jc::Break {
            labels,
            is_loop: true,
            plain: true,
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        self.sub_stmt(b)?;
        let cont = self.here();
        self.expr(t)?;
        self.emit(Op::JumpIfTrue(top));
        // continues go to the test
        if let Some(Jc::Break { continues, .. }) = self.f().ctx.last_mut() {
            let cs = core::mem::take(continues);
            for c in cs {
                self.patch(c, cont);
            }
        }
        self.pop_break_ctx(cont);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn for_loop(
        &mut self,
        init: &'a Option<ForInit>,
        test: &'a Option<Expr>,
        update: &'a Option<Expr>,
        body: &'a Stmt,
        scope: ScopeId,
        labels: Vec<Atom>,
        pos: u32,
    ) -> CResult<()> {
        self.scopes[scope as usize].parent = Some(self.cur_scope());
        self.enter_scope(scope, pos)?;
        let per_iter = self.scopes[scope as usize].env_size > 0;
        if let Some(init) = init {
            match init {
                ForInit::Var(kind, decls) => {
                    for d in decls {
                        match &d.init {
                            Some(e) => {
                                self.named_expr(e, pat_name(&d.pat))?;
                                let mode = if *kind == VarKind::Var {
                                    Store::Assign
                                } else {
                                    Store::Init
                                };
                                self.destructure_mode(&d.pat, mode, *kind == VarKind::Var)?;
                            }
                            None => {
                                if *kind != VarKind::Var {
                                    self.emit(Op::Undef);
                                    self.destructure(&d.pat, Store::Init)?;
                                }
                            }
                        }
                    }
                }
                ForInit::Expr(e) => {
                    self.expr(e)?;
                    self.emit(Op::Pop);
                }
            }
        }
        if per_iter {
            self.emit(Op::CopyEnv);
        }
        let top = self.here();
        let jf = match test {
            Some(t) => {
                self.expr(t)?;
                Some(self.emit(Op::JumpIfFalse(0)))
            }
            None => None,
        };
        self.f().ctx.push(Jc::Break {
            labels,
            is_loop: true,
            plain: true,
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        self.sub_stmt(body)?;
        let cont = self.here();
        if per_iter {
            self.emit(Op::CopyEnv);
        }
        if let Some(u) = update {
            self.expr(u)?;
            self.emit(Op::Pop);
        }
        self.emit(Op::Jump(top));
        if let Some(jf) = jf {
            self.patch_here(jf);
        }
        self.pop_break_ctx(cont);
        self.leave_scope(scope);
        Ok(())
    }

    /// Binds the per-iteration value (on the stack) to a for-in/of head.
    fn bind_for_head(&mut self, left: &'a ForHead) -> CResult<()> {
        match left {
            ForHead::Var(VarKind::Var, p) => self.destructure_mode(p, Store::Assign, true),
            ForHead::Var(_, p) => self.destructure(p, Store::Init),
            ForHead::Target(p) => self.destructure(p, Store::Assign),
        }
    }

    fn for_in(
        &mut self,
        left: &'a ForHead,
        right: &'a Expr,
        body: &'a Stmt,
        scope: ScopeId,
        labels: Vec<Atom>,
        pos: u32,
    ) -> CResult<()> {
        self.expr(right)?;
        let it = self.alloc_local(pos)?;
        self.mark(pos);
        self.emit(Op::ForInStart(it));
        self.scopes[scope as usize].parent = Some(self.cur_scope());
        let top = self.here();
        let jn = self.emit(Op::ForInNext(it, 0));
        self.enter_scope(scope, pos)?;
        let has_env = self.scopes[scope as usize].env_size > 0;
        self.bind_for_head(left)?;
        self.f().ctx.push(Jc::Break {
            labels,
            is_loop: true,
            plain: true,
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        if has_env {
            self.f().ctx.push(Jc::PopEnv);
        }
        self.sub_stmt(body)?;
        if has_env {
            self.f().ctx.pop();
        }
        self.leave_scope(scope);
        self.emit(Op::Jump(top));
        self.patch_here(jn);
        self.pop_break_ctx(top);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn for_of(
        &mut self,
        left: &'a ForHead,
        right: &'a Expr,
        body: &'a Stmt,
        scope: ScopeId,
        is_await: bool,
        labels: Vec<Atom>,
        pos: u32,
    ) -> CResult<()> {
        self.expr(right)?;
        let it = self.alloc_local(pos)?;
        self.alloc_local(pos)?;
        self.alloc_local(pos)?;
        let exc = self.alloc_local(pos)?;
        let env_save = self.alloc_local(pos)?;
        self.mark(pos);
        if is_await {
            self.emit(Op::GetAsyncIter(it));
        } else {
            self.emit(Op::GetIter(it));
        }
        self.emit(Op::SaveEnv(env_save));
        let region = self.open_region(true);
        self.f().ctx.push(Jc::IterClose { slot: it, region });
        self.scopes[scope as usize].parent = Some(self.cur_scope());
        let top = self.here();
        let step = if is_await {
            self.emit(Op::AsyncIterStep(it, u32::MAX));
            self.emit(Op::Await);
            self.emit(Op::AsyncIterStep(it, 0))
        } else {
            self.emit(Op::IterStep(it, 0))
        };
        self.enter_scope(scope, pos)?;
        let has_env = self.scopes[scope as usize].env_size > 0;
        self.bind_for_head(left)?;
        self.f().ctx.push(Jc::Break {
            labels,
            is_loop: true,
            plain: true,
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        if has_env {
            self.f().ctx.push(Jc::PopEnv);
        }
        self.sub_stmt(body)?;
        if has_env {
            self.f().ctx.pop();
        }
        self.leave_scope(scope);
        self.emit(Op::Jump(top));
        // done / break: close (a no-op when done)
        self.patch_here(step);
        self.pop_break_ctx(top);
        self.close_region(region);
        self.f().ctx.pop(); // IterClose
        self.emit(Op::IterClose(it));
        let skip = self.emit(Op::Jump(0));
        // exception: close quietly and rethrow
        let handler = self.here();
        self.emit(Op::PutLocal(exc));
        self.emit(Op::RestoreEnv(env_save));
        self.emit(Op::IterCloseQuiet(it));
        self.emit(Op::Rethrow(exc));
        self.finish_region(region, handler);
        self.patch_here(skip);
        Ok(())
    }

    fn switch(
        &mut self,
        disc: &'a Expr,
        cases: &'a [Case],
        scope: ScopeId,
        labels: Vec<Atom>,
        pos: u32,
    ) -> CResult<()> {
        self.expr(disc)?;
        let d = self.alloc_local(pos)?;
        self.emit(Op::PutLocal(d));
        self.scopes[scope as usize].parent = Some(self.cur_scope());
        self.enter_scope(scope, pos)?;
        for c in cases {
            self.hoist_block_functions(&c.body)?;
        }
        let mut jumps = Vec::new();
        for c in cases {
            if let Some(t) = &c.test {
                self.emit(Op::GetLocal(d));
                self.expr(t)?;
                self.emit(Op::StrictEq);
                jumps.push(Some(self.emit(Op::JumpIfTrue(0))));
            } else {
                jumps.push(None);
            }
        }
        let to_default = self.emit(Op::Jump(0));
        self.f().ctx.push(Jc::Break {
            labels,
            is_loop: false,
            plain: true,
            breaks: Vec::new(),
            continues: Vec::new(),
        });
        let mut default_at = None;
        for (i, c) in cases.iter().enumerate() {
            match jumps[i] {
                Some(j) => self.patch_here(j),
                None => default_at = Some(self.here()),
            }
            self.stmts(&c.body)?;
        }
        match default_at {
            Some(t) => self.patch(to_default, t),
            None => self.patch_here(to_default),
        }
        self.pop_break_ctx(0);
        self.leave_scope(scope);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn try_stmt(
        &mut self,
        block: &'a Block,
        param: &'a Option<Pat>,
        handler: &'a Option<Block>,
        catch_scope: ScopeId,
        finalizer: &'a Option<Block>,
        pos: u32,
    ) -> CResult<()> {
        let env_save = self.alloc_local(pos)?;
        let exc = self.alloc_local(pos)?;
        self.emit(Op::SaveEnv(env_save));
        let fin_region = finalizer.as_ref().map(|_| self.open_region(true));
        let catch_region = handler.as_ref().map(|_| self.open_region(false));
        {
            let mut regions = alloc::vec![];
            if let Some(r) = fin_region {
                regions.push(r);
            }
            if let Some(r) = catch_region {
                regions.push(r);
            }
            self.f().ctx.push(Jc::Finally {
                block: finalizer.as_ref(),
                regions,
            });
        }
        self.block(block)?;
        if let Some(r) = catch_region {
            self.close_region(r);
            // A break out of the catch body must not suspend (and so reopen) the closed catch
            // region.
            if let Some(Jc::Finally { regions, .. }) = self.f().ctx.last_mut() {
                regions.retain(|x| *x != r);
            }
        }
        if finalizer.is_none() {
            self.f().ctx.pop();
        }
        let mut to_end = Vec::new();
        // Normal completion of the try block.
        if let Some(fin) = finalizer {
            let r = fin_region.unwrap_or(0);
            self.suspend_region(r);
            let saved = self.f().ctx.pop();
            self.block(fin)?;
            if let Some(s) = saved {
                self.f().ctx.push(s);
            }
            to_end.push(self.emit(Op::Jump(0)));
            self.resume_region(r);
        } else {
            to_end.push(self.emit(Op::Jump(0)));
        }
        // The catch clause.
        if let (Some(h), Some(cr)) = (handler, catch_region) {
            let target = self.here();
            self.finish_region(cr, target);
            self.emit(Op::RestoreEnv(env_save));
            self.scopes[catch_scope as usize].parent = Some(self.cur_scope());
            self.enter_scope(catch_scope, pos)?;
            match param {
                Some(p) => self.destructure(p, Store::Init)?,
                None => {
                    self.emit(Op::Pop);
                }
            }
            self.block(h)?;
            self.leave_scope(catch_scope);
            if let Some(fin) = finalizer {
                let r = fin_region.unwrap_or(0);
                self.suspend_region(r);
                let saved = self.f().ctx.pop();
                self.block(fin)?;
                if let Some(s) = saved {
                    self.f().ctx.push(s);
                }
                to_end.push(self.emit(Op::Jump(0)));
                self.resume_region(r);
            } else {
                to_end.push(self.emit(Op::Jump(0)));
            }
        }
        // The finally handler for exceptions (and generator returns).
        if let (Some(fin), Some(fr)) = (finalizer, fin_region) {
            self.f().ctx.pop();
            let target = self.here();
            self.finish_region(fr, target);
            self.emit(Op::PutLocal(exc));
            self.emit(Op::RestoreEnv(env_save));
            self.block(fin)?;
            self.emit(Op::Rethrow(exc));
        }
        for j in to_end {
            self.patch_here(j);
        }
        Ok(())
    }

    /// break / continue: runs the exit actions (finally blocks, iterator closes, environment
    /// pops) between here and the target.
    fn emit_jump(&mut self, label: Option<Atom>, is_continue: bool, pos: u32) -> CResult<()> {
        let n = self.fr().ctx.len();
        let mut target = None;
        for i in (0..n).rev() {
            if let Jc::Break {
                labels,
                is_loop,
                plain,
                ..
            } = &self.fr().ctx[i]
            {
                let matches = match label {
                    Some(l) => labels.contains(&l) && (!is_continue || *is_loop),
                    None => {
                        if is_continue {
                            *is_loop
                        } else {
                            *plain
                        }
                    }
                };
                if matches {
                    target = Some(i);
                    break;
                }
            }
        }
        let Some(t) = target else {
            return self.err(
                if is_continue {
                    "Illegal continue statement"
                } else {
                    "Illegal break statement"
                },
                pos,
            );
        };
        let suspended = self.exit_actions(t + 1, false)?;
        let j = self.emit(Op::Jump(0));
        if let Jc::Break {
            breaks, continues, ..
        } = &mut self.f().ctx[t]
        {
            if is_continue {
                continues.push(j);
            } else {
                breaks.push(j);
            }
        }
        for r in suspended {
            self.resume_region(r);
        }
        Ok(())
    }

    /// Emits the exit actions of ctx[from..] (innermost first). Returns the regions it
    /// suspended (the caller resumes them after its jump).
    fn exit_actions(&mut self, from: usize, is_return: bool) -> CResult<Vec<usize>> {
        let mut suspended = Vec::new();
        let n = self.fr().ctx.len();
        let mut i = n;
        while i > from {
            i -= 1;
            let action = match &self.fr().ctx[i] {
                Jc::Finally { block, regions } => Some((*block, regions.clone(), None)),
                Jc::IterClose { slot, region } => Some((None, alloc::vec![*region], Some(*slot))),
                Jc::PopEnv => {
                    if !is_return {
                        self.emit(Op::PopEnv);
                    }
                    None
                }
                Jc::Break { .. } => None,
            };
            if let Some((block, regions, slot)) = action {
                for r in &regions {
                    self.suspend_region(*r);
                    suspended.push(*r);
                }
                if let Some(s) = slot {
                    self.emit(Op::IterClose(s));
                }
                if let Some(b) = block {
                    // Compile the finally body with the contexts outside it.
                    let saved: Vec<Jc<'a>> = self.f().ctx.drain(i..).collect();
                    let r = self.block(b);
                    self.f().ctx.extend(saved);
                    r?;
                }
            }
        }
        Ok(suspended)
    }

    fn emit_return(&mut self, pos: u32) -> CResult<()> {
        let has_actions = self
            .fr()
            .ctx
            .iter()
            .any(|c| matches!(c, Jc::Finally { .. } | Jc::IterClose { .. }));
        if has_actions {
            let t = self.alloc_local(pos)?;
            self.emit(Op::PutLocal(t));
            let suspended = self.exit_actions(0, true)?;
            self.emit(Op::GetLocal(t));
            self.emit(Op::Return);
            for r in suspended {
                self.resume_region(r);
            }
        } else {
            self.emit(Op::Return);
        }
        Ok(())
    }

    // ---- destructuring ----

    fn destructure(&mut self, p: &'a Pat, mode: Store) -> CResult<()> {
        self.destructure_mode(p, mode, false)
    }

    /// Consumes the value on the stack into the pattern. `var_init` marks a `var`
    /// declaration's initializer (assignments to vars, no TDZ).
    fn destructure_mode(&mut self, p: &'a Pat, mode: Store, var_init: bool) -> CResult<()> {
        match p {
            Pat::Ident(a, pos) => {
                let m = if var_init { Store::Init } else { mode };
                self.store_name(*a, m, false, *pos)
            }
            Pat::Expr(e) => self.store_to_target(e),
            Pat::Array(elems, rest) => {
                let it = self.alloc_local(0)?;
                self.alloc_local(0)?;
                self.alloc_local(0)?;
                self.emit(Op::GetIter(it));
                for el in elems {
                    match el {
                        None => {
                            self.emit(Op::IterValue(it));
                            self.emit(Op::Pop);
                        }
                        Some(PatElem { pat, default }) => {
                            self.emit(Op::IterValue(it));
                            if let Some(d) = default {
                                self.default_value(d, pat)?;
                            }
                            self.destructure_mode(pat, mode, var_init)?;
                        }
                    }
                }
                match rest {
                    Some(r) => {
                        self.emit(Op::IterRest(it));
                        self.destructure_mode(r, mode, var_init)?;
                    }
                    None => {
                        self.emit(Op::IterClose(it));
                    }
                }
                Ok(())
            }
            Pat::Object(props, rest) => {
                self.emit(Op::CheckCoercible);
                let keys = match rest {
                    Some(_) => {
                        let k = self.alloc_local(0)?;
                        self.emit(Op::NewArray(0));
                        self.emit(Op::PutLocal(k));
                        Some(k)
                    }
                    None => None,
                };
                for pp in props {
                    self.emit(Op::Dup);
                    match &pp.key {
                        PropName::Computed(e) => {
                            self.expr(e)?;
                            self.emit(Op::ToPropKey);
                            if let Some(k) = keys {
                                // remember the key for the rest object
                                let t = self.alloc_local(0)?;
                                self.emit(Op::SetLocal(t));
                                self.emit(Op::GetLocal(k));
                                self.emit(Op::GetLocal(t));
                                self.emit(Op::ArrPush);
                                self.emit(Op::Pop);
                            }
                            self.emit(Op::GetElem);
                        }
                        key => {
                            let u = self.prop_name_units(key).unwrap_or_default();
                            let c = self.const_units(&u);
                            if let Some(k) = keys {
                                self.emit(Op::GetLocal(k));
                                self.emit(Op::Const(c));
                                self.emit(Op::ArrPush);
                                self.emit(Op::Pop);
                            }
                            self.emit(Op::GetProp(c));
                        }
                    }
                    if let Some(d) = &pp.default {
                        self.default_value(d, &pp.pat)?;
                    }
                    self.destructure_mode(&pp.pat, mode, var_init)?;
                }
                match (rest, keys) {
                    (Some(r), Some(k)) => {
                        self.emit(Op::GetLocal(k));
                        self.emit(Op::ObjRest);
                        self.destructure_mode(r, mode, var_init)?;
                    }
                    _ => {
                        self.emit(Op::Pop);
                    }
                }
                Ok(())
            }
        }
    }

    /// [v] → [v or default]
    fn default_value(&mut self, d: &'a Expr, target: &Pat) -> CResult<()> {
        self.emit(Op::Dup);
        let j = self.emit(Op::JumpIfNotUndef(0));
        self.emit(Op::Pop);
        self.named_expr(d, pat_name(target))?;
        self.patch_here(j);
        Ok(())
    }

    /// [v] → [] : stores into a member expression target.
    fn store_to_target(&mut self, e: &'a Expr) -> CResult<()> {
        match &e.k {
            ExprKind::Member(o, name, _) => {
                self.expr(o)?;
                self.emit(Op::Swap);
                let c = self.const_atom(*name);
                self.mark(e.pos);
                self.emit(Op::SetProp(c));
                self.emit(Op::Pop);
            }
            ExprKind::Index(o, i, _) => {
                self.expr(o)?;
                self.expr(i)?;
                self.emit(Op::Rot3);
                self.emit(Op::Rot3);
                self.mark(e.pos);
                self.emit(Op::SetElem);
                self.emit(Op::Pop);
            }
            ExprKind::PrivMember(o, pn, _) => {
                self.expr(o)?;
                self.load_name(*pn, e.pos)?;
                self.emit(Op::Rot3);
                self.emit(Op::Rot3);
                self.mark(e.pos);
                self.emit(Op::SetPriv);
                self.emit(Op::Pop);
            }
            ExprKind::SuperMember(name) => {
                let c = self.const_atom(*name);
                self.emit(Op::SetSuper(c));
                self.emit(Op::Pop);
            }
            ExprKind::SuperIndex(i) => {
                self.expr(i)?;
                self.emit(Op::Swap);
                self.emit(Op::SetSuperElem);
                self.emit(Op::Pop);
            }
            ExprKind::Seq(items) if items.len() == 1 => return self.store_to_target(&items[0]),
            _ => return self.err("Invalid assignment target", e.pos),
        }
        Ok(())
    }

    // ---- expressions ----

    fn expr(&mut self, e: &'a Expr) -> CResult<()> {
        if self.realm.stack.exceeded() {
            return self.err("the code is nested too deeply", e.pos);
        }
        match &e.k {
            ExprKind::Num(n) => self.push_num(*n),
            ExprKind::Str(s) => {
                let c = self.const_units(s);
                self.emit(Op::Const(c));
            }
            ExprKind::Template(parts, exprs) => {
                let mut n = 0;
                for (i, q) in parts.cooked.iter().enumerate() {
                    let q = q.as_deref().unwrap_or(&[]);
                    if !q.is_empty() || (i == 0 && exprs.is_empty()) {
                        let c = self.const_units(q);
                        self.emit(Op::Const(c));
                        n += 1;
                    }
                    if let Some(x) = exprs.get(i) {
                        self.expr(x)?;
                        self.mark(x.pos);
                        self.emit(Op::ToStr);
                        n += 1;
                    }
                }
                if n == 0 {
                    let c = self.const_units(&[]);
                    self.emit(Op::Const(c));
                } else if n > 1 {
                    self.emit(Op::Concat(n));
                } else if exprs.is_empty() {
                    // a single literal chunk: already a string
                } else if parts
                    .cooked
                    .iter()
                    .all(|q| q.as_ref().is_none_or(|v| v.is_empty()))
                {
                    // `${x}` alone: ToStr already made a string
                }
            }
            ExprKind::Tagged(tag, parts, exprs) => {
                self.callee(tag)?;
                let cooked: Vec<Option<StrId>> = parts
                    .cooked
                    .iter()
                    .map(|c| c.as_ref().map(|u| self.realm.heap.intern(u)))
                    .collect();
                let raw: Vec<StrId> = parts
                    .raw
                    .iter()
                    .map(|u| self.realm.heap.intern(u))
                    .collect();
                let c = self.add_const(Const::Template(cooked, raw, RefCell::new(None)));
                self.emit(Op::TemplateObj(c));
                for x in exprs {
                    self.expr(x)?;
                }
                self.mark(e.pos);
                self.emit(Op::Call(1 + exprs.len() as u32));
            }
            ExprKind::Regex(p, f) => {
                let ps = self.realm.heap.intern(p);
                let fs = self.realm.heap.intern(f);
                // Validate now: a bad pattern is an early SyntaxError.
                if let Err(m) = crate::regexp::validate(p, f) {
                    return self.err(
                        &alloc::format!(
                            "Invalid regular expression: /{}/: {m}",
                            crate::string::to_rust(p)
                        ),
                        e.pos,
                    );
                }
                let c = self.add_const(Const::Regex(ps, fs));
                self.emit(Op::RegExp(c));
            }
            ExprKind::Ident(a) => self.load_name(*a, e.pos)?,
            ExprKind::This => {
                if self.fr().this_chk {
                    self.mark(e.pos);
                    self.emit(Op::ThisChk);
                } else {
                    self.emit(Op::This);
                }
            }
            ExprKind::NewTarget => {
                self.emit(Op::NewTarget);
            }
            ExprKind::Null => {
                self.emit(Op::Null);
            }
            ExprKind::Bool(b) => {
                self.emit(if *b { Op::True } else { Op::False });
            }
            ExprKind::Array(items) => {
                self.emit(Op::NewArray(items.len().min(1 << 16) as u32));
                for it in items {
                    match it {
                        ArrElem::Hole => {
                            self.emit(Op::ArrHole);
                        }
                        ArrElem::Item(x) => {
                            self.expr(x)?;
                            self.emit(Op::ArrPush);
                        }
                        ArrElem::Spread(x) => {
                            self.expr(x)?;
                            self.mark(x.pos);
                            self.emit(Op::ArrSpread);
                        }
                    }
                }
            }
            ExprKind::Object(props) => self.object_literal(props)?,
            ExprKind::Func(f) => self.func_expr(f, None)?,
            ExprKind::Class(c) => self.class(c, None)?,
            ExprKind::Unary(op, x) => self.unary(*op, x, e.pos)?,
            ExprKind::Update {
                inc,
                prefix,
                target,
            } => self.update(*inc, *prefix, target, e.pos)?,
            ExprKind::Bin(BinOp::In, a, b) if matches!(a.k, ExprKind::PrivName(_)) => {
                let ExprKind::PrivName(pn) = a.k else {
                    return self.err("Unexpected private name", e.pos);
                };
                self.load_name(pn, e.pos)?;
                self.expr(b)?;
                self.mark(e.pos);
                self.emit(Op::PrivIn);
            }
            ExprKind::PrivName(_) => return self.err("Unexpected private name", e.pos),
            ExprKind::PrivMember(o, pn, _) => {
                self.expr(o)?;
                self.load_name(*pn, e.pos)?;
                self.mark(e.pos);
                self.emit(Op::GetPriv);
            }
            ExprKind::Bin(op, a, b) => {
                self.expr(a)?;
                self.expr(b)?;
                self.mark(e.pos);
                self.emit(bin_op(*op));
            }
            ExprKind::Logic(op, a, b) => {
                self.expr(a)?;
                let j = self.emit(match op {
                    LogicOp::And => Op::JumpIfFalseKeep(0),
                    LogicOp::Or => Op::JumpIfTrueKeep(0),
                    LogicOp::Nullish => Op::JumpIfNotNullishKeep(0),
                });
                self.expr(b)?;
                self.patch_here(j);
            }
            ExprKind::Assign(op, target, value) => self.assign(*op, target, value, e.pos)?,
            ExprKind::Cond(t, a, b) => {
                self.expr(t)?;
                let jf = self.emit(Op::JumpIfFalse(0));
                self.expr(a)?;
                let je = self.emit(Op::Jump(0));
                self.patch_here(jf);
                self.expr(b)?;
                self.patch_here(je);
            }
            ExprKind::Call(callee, args, _) => {
                self.callee(callee)?;
                self.call_args(args, e.pos, false)?;
            }
            ExprKind::New(callee, args) => {
                self.emit(Op::Undef);
                self.expr(callee)?;
                self.call_args(args, e.pos, true)?;
            }
            ExprKind::Member(o, name, _) => {
                self.expr(o)?;
                let c = self.const_atom(*name);
                self.mark(e.pos);
                self.emit(Op::GetProp(c));
            }
            ExprKind::Index(o, i, _) => {
                self.expr(o)?;
                self.expr(i)?;
                self.mark(e.pos);
                self.emit(Op::GetElem);
            }
            ExprKind::OptChain(inner) => {
                let mut exits = Vec::new();
                self.chain(inner, false, &mut exits)?;
                self.chain_landings(exits, Op::Undef);
            }
            ExprKind::Seq(items) => {
                for (i, x) in items.iter().enumerate() {
                    self.expr(x)?;
                    if i + 1 < items.len() {
                        self.emit(Op::Pop);
                    }
                }
            }
            ExprKind::ImportCall(spec, opts) => {
                self.expr(spec)?;
                match opts {
                    Some(o) => self.expr(o)?,
                    None => {
                        self.emit(Op::Undef);
                    }
                }
                self.mark(e.pos);
                self.emit(Op::ImportCall);
            }
            ExprKind::ImportMeta => {
                self.mark(e.pos);
                self.emit(Op::ImportMeta);
            }
            ExprKind::Yield(arg, delegate) => {
                match arg {
                    Some(a) => self.expr(a)?,
                    None => {
                        self.emit(Op::Undef);
                    }
                }
                self.mark(e.pos);
                if *delegate {
                    let it = self.alloc_local(e.pos)?;
                    self.alloc_local(e.pos)?;
                    self.alloc_local(e.pos)?;
                    if self.fr().is_async {
                        self.emit(Op::GetAsyncIter(it));
                    } else {
                        self.emit(Op::GetIter(it));
                    }
                    self.emit(Op::Undef);
                    let top = self.here();
                    self.emit(Op::YieldStar(it));
                    self.emit(Op::Jump(top));
                } else {
                    if self.fr().is_async {
                        // In an async generator, `yield v` awaits v first.
                        self.emit(Op::Await);
                    }
                    self.emit(Op::Yield);
                }
            }
            ExprKind::Await(x) => {
                self.expr(x)?;
                self.mark(e.pos);
                self.emit(Op::Await);
            }
            ExprKind::SuperCall(args) => {
                self.mark(e.pos);
                if args.iter().any(|a| matches!(a, Arg::Spread(_))) {
                    self.spread_array(args)?;
                    self.mark(e.pos);
                    self.emit(Op::SuperCallSpread);
                } else {
                    for a in args {
                        if let Arg::Item(x) = a {
                            self.expr(x)?;
                        }
                    }
                    self.mark(e.pos);
                    self.emit(Op::SuperCall(args.len() as u32));
                }
            }
            ExprKind::SuperMember(name) => {
                let c = self.const_atom(*name);
                self.mark(e.pos);
                self.emit(Op::GetSuper(c));
            }
            ExprKind::SuperIndex(i) => {
                self.expr(i)?;
                self.mark(e.pos);
                self.emit(Op::GetSuperElem);
            }
            ExprKind::Spread(_) => return self.err("Unexpected token '...'", e.pos),
        }
        Ok(())
    }

    fn object_literal(&mut self, props: &'a [PropDef]) -> CResult<()> {
        self.emit(Op::NewObject);
        for p in props {
            match p {
                PropDef::KeyValue(PropName::Computed(k), v) => {
                    self.expr(k)?;
                    self.emit(Op::ToPropKey);
                    self.expr(v)?;
                    if is_anon_fn(v) {
                        self.emit(Op::SetFnName);
                    }
                    self.emit(Op::InitElem);
                }
                PropDef::KeyValue(key, v) => {
                    let u = self.prop_name_units(key).unwrap_or_default();
                    if is_anon_fn(v) {
                        let ns = self.realm.heap.intern(&u);
                        self.named_expr_str(v, ns)?;
                    } else {
                        self.expr(v)?;
                    }
                    let c = self.const_units(&u);
                    self.emit(Op::InitProp(c));
                }
                PropDef::Shorthand(a, pos) => {
                    self.load_name(*a, *pos)?;
                    let c = self.const_atom(*a);
                    self.emit(Op::InitProp(c));
                }
                PropDef::CoverInit(_, pos, _) => {
                    return self.err("Invalid shorthand property initializer", *pos);
                }
                PropDef::Method(key, kind, f) => {
                    let k = match kind {
                        MethodKind::Method => 0,
                        MethodKind::Get => 1,
                        MethodKind::Set => 2,
                    } | 4;
                    match key {
                        PropName::Computed(ke) => {
                            self.expr(ke)?;
                            self.emit(Op::ToPropKey);
                            let code = self.compile_function(f, None)?;
                            let fc = self.add_const(Const::Code(code));
                            self.emit(Op::Closure(fc));
                            self.emit(Op::InitMethodElem(k));
                        }
                        key => {
                            let u = self.prop_name_units(key).unwrap_or_default();
                            let ns = self.realm.heap.intern(&u);
                            let code = self.compile_function(f, Some(ns))?;
                            let fc = self.add_const(Const::Code(code));
                            self.emit(Op::Closure(fc));
                            let c = self.const_units(&u);
                            self.emit(Op::InitMethod(c, k));
                        }
                    }
                }
                PropDef::Spread(x) => {
                    self.expr(x)?;
                    self.emit(Op::CopyProps);
                }
                PropDef::Proto(x) => {
                    self.expr(x)?;
                    self.emit(Op::SetProtoLit);
                }
            }
        }
        Ok(())
    }

    fn unary(&mut self, op: UnOp, x: &'a Expr, pos: u32) -> CResult<()> {
        match op {
            UnOp::Typeof => {
                if let ExprKind::Ident(a) = x.k
                    && let Lookup::Global = self.lookup(a)
                {
                    let n = self.const_atom(a);
                    self.mark(pos);
                    self.emit(Op::TypeofGlobal(n));
                    return Ok(());
                }
                self.expr(x)?;
                self.emit(Op::Typeof);
            }
            UnOp::Delete => self.delete(x, pos)?,
            UnOp::Void => {
                self.expr(x)?;
                self.emit(Op::Pop);
                self.emit(Op::Undef);
            }
            _ => {
                self.expr(x)?;
                self.mark(pos);
                self.emit(match op {
                    UnOp::Neg => Op::Neg,
                    UnOp::Plus => Op::Plus,
                    UnOp::Not => Op::Not,
                    _ => Op::BitNot,
                });
            }
        }
        Ok(())
    }

    fn delete(&mut self, x: &'a Expr, pos: u32) -> CResult<()> {
        match &x.k {
            ExprKind::Member(o, name, _) => {
                self.expr(o)?;
                let c = self.const_atom(*name);
                self.mark(pos);
                self.emit(Op::DelProp(c));
            }
            ExprKind::Index(o, i, _) => {
                self.expr(o)?;
                self.expr(i)?;
                self.mark(pos);
                self.emit(Op::DelElem);
            }
            ExprKind::Ident(a) => match self.lookup(*a) {
                Lookup::Global => {
                    // Deleting a global property (an implicit global): delete globalThis[name].
                    let g = self.const_str("globalThis");
                    self.emit(Op::GetGlobal(g));
                    let c = self.const_atom(*a);
                    self.emit(Op::DelProp(c));
                }
                _ => {
                    self.emit(Op::False);
                }
            },
            ExprKind::OptChain(inner) => {
                let mut exits = Vec::new();
                match &inner.k {
                    ExprKind::Member(o, name, opt) => {
                        self.chain(o, false, &mut exits)?;
                        if *opt {
                            exits.push((self.emit(Op::JumpIfNullish(0)), 1));
                        }
                        let c = self.const_atom(*name);
                        self.emit(Op::DelProp(c));
                    }
                    ExprKind::Index(o, i, opt) => {
                        self.chain(o, false, &mut exits)?;
                        if *opt {
                            exits.push((self.emit(Op::JumpIfNullish(0)), 1));
                        }
                        self.expr(i)?;
                        self.emit(Op::DelElem);
                    }
                    _ => {
                        self.chain(inner, false, &mut exits)?;
                        self.emit(Op::Pop);
                        self.emit(Op::True);
                    }
                }
                self.chain_landings(exits, Op::True);
            }
            ExprKind::Seq(items) if items.len() == 1 => return self.delete(&items[0], pos),
            ExprKind::SuperMember(_) | ExprKind::SuperIndex(_) => {
                let c = self.const_str("Unsupported reference to 'super'");
                self.emit(Op::ThrowRef(c));
                self.emit(Op::Undef);
            }
            _ => {
                self.expr(x)?;
                self.emit(Op::Pop);
                self.emit(Op::True);
            }
        }
        Ok(())
    }

    /// Pushes `this` and the function for a call.
    fn callee(&mut self, c: &'a Expr) -> CResult<()> {
        match &c.k {
            ExprKind::Member(o, name, false) => {
                self.expr(o)?;
                let k = self.const_atom(*name);
                self.mark(c.pos);
                self.emit(Op::GetPropKeep(k));
            }
            ExprKind::Index(o, i, false) => {
                self.expr(o)?;
                self.expr(i)?;
                self.mark(c.pos);
                self.emit(Op::GetElemKeep);
            }
            ExprKind::PrivMember(o, pn, false) => {
                self.expr(o)?;
                self.load_name(*pn, c.pos)?;
                self.mark(c.pos);
                self.emit(Op::GetPrivKeep);
            }
            ExprKind::SuperMember(name) => {
                if self.fr().this_chk {
                    self.emit(Op::ThisChk);
                } else {
                    self.emit(Op::This);
                }
                let k = self.const_atom(*name);
                self.mark(c.pos);
                self.emit(Op::GetSuper(k));
            }
            ExprKind::SuperIndex(i) => {
                if self.fr().this_chk {
                    self.emit(Op::ThisChk);
                } else {
                    self.emit(Op::This);
                }
                self.expr(i)?;
                self.mark(c.pos);
                self.emit(Op::GetSuperElem);
            }
            ExprKind::Seq(items)
                if items.len() == 1
                    && matches!(
                        items[0].k,
                        ExprKind::Member(..) | ExprKind::Index(..) | ExprKind::PrivMember(..)
                    ) =>
            {
                return self.callee(&items[0]);
            }
            ExprKind::Seq(items)
                if items.len() == 1 && matches!(items[0].k, ExprKind::OptChain(_)) =>
            {
                return self.callee(&items[0]);
            }
            ExprKind::OptChain(inner) => {
                // (a?.b)() : the chain yields this and the function together.
                let mut exits = Vec::new();
                self.chain_callee(inner, &mut exits)?;
                if !exits.is_empty() {
                    let mut ends = alloc::vec![self.emit(Op::Jump(0))];
                    for (j, depth) in exits {
                        self.patch_here(j);
                        for _ in 0..depth {
                            self.emit(Op::Pop);
                        }
                        self.emit(Op::Undef);
                        self.emit(Op::Undef);
                        ends.push(self.emit(Op::Jump(0)));
                    }
                    for e in ends {
                        self.patch_here(e);
                    }
                }
            }
            _ => {
                self.emit(Op::Undef);
                self.expr(c)?;
            }
        }
        Ok(())
    }

    fn spread_array(&mut self, args: &'a [Arg]) -> CResult<()> {
        self.emit(Op::NewArray(args.len() as u32));
        for a in args {
            match a {
                Arg::Item(x) => {
                    self.expr(x)?;
                    self.emit(Op::ArrPush);
                }
                Arg::Spread(x) => {
                    self.expr(x)?;
                    self.mark(x.pos);
                    self.emit(Op::ArrSpread);
                }
            }
        }
        Ok(())
    }

    fn call_args(&mut self, args: &'a [Arg], pos: u32, is_new: bool) -> CResult<()> {
        if args.iter().any(|a| matches!(a, Arg::Spread(_))) {
            self.spread_array(args)?;
            self.mark(pos);
            self.emit(if is_new {
                Op::NewSpread
            } else {
                Op::CallSpread
            });
        } else {
            for a in args {
                if let Arg::Item(x) = a {
                    self.expr(x)?;
                }
            }
            self.mark(pos);
            let n = args.len() as u32;
            self.emit(if is_new { Op::New(n) } else { Op::Call(n) });
        }
        Ok(())
    }

    /// An optional chain link: leaves the value; records where nullish values exit, with how
    /// many stack slots to drop there.
    fn chain(&mut self, e: &'a Expr, _keep: bool, exits: &mut Vec<(usize, u32)>) -> CResult<()> {
        match &e.k {
            ExprKind::Member(o, name, opt) => {
                self.chain(o, false, exits)?;
                if *opt {
                    exits.push((self.emit(Op::JumpIfNullish(0)), 1));
                }
                let c = self.const_atom(*name);
                self.mark(e.pos);
                self.emit(Op::GetProp(c));
            }
            ExprKind::Index(o, i, opt) => {
                self.chain(o, false, exits)?;
                if *opt {
                    exits.push((self.emit(Op::JumpIfNullish(0)), 1));
                }
                self.expr(i)?;
                self.mark(e.pos);
                self.emit(Op::GetElem);
            }
            ExprKind::PrivMember(o, pn, opt) => {
                self.chain(o, false, exits)?;
                if *opt {
                    exits.push((self.emit(Op::JumpIfNullish(0)), 1));
                }
                self.load_name(*pn, e.pos)?;
                self.mark(e.pos);
                self.emit(Op::GetPriv);
            }
            ExprKind::Call(callee, args, opt) => {
                self.chain_callee(callee, exits)?;
                if *opt {
                    exits.push((self.emit(Op::JumpIfNullish(0)), 2));
                }
                self.call_args(args, e.pos, false)?;
            }
            _ => self.expr(e)?,
        }
        Ok(())
    }

    /// [this f] for a call inside a chain.
    fn chain_callee(&mut self, c: &'a Expr, exits: &mut Vec<(usize, u32)>) -> CResult<()> {
        match &c.k {
            ExprKind::Member(o, name, opt) => {
                self.chain(o, false, exits)?;
                if *opt {
                    exits.push((self.emit(Op::JumpIfNullish(0)), 1));
                }
                let k = self.const_atom(*name);
                self.mark(c.pos);
                self.emit(Op::GetPropKeep(k));
            }
            ExprKind::Index(o, i, opt) => {
                self.chain(o, false, exits)?;
                if *opt {
                    exits.push((self.emit(Op::JumpIfNullish(0)), 1));
                }
                self.expr(i)?;
                self.mark(c.pos);
                self.emit(Op::GetElemKeep);
            }
            ExprKind::PrivMember(o, pn, opt) => {
                self.chain(o, false, exits)?;
                if *opt {
                    exits.push((self.emit(Op::JumpIfNullish(0)), 1));
                }
                self.load_name(*pn, c.pos)?;
                self.mark(c.pos);
                self.emit(Op::GetPrivKeep);
            }
            ExprKind::Call(..) => {
                self.chain(c, false, exits)?;
                self.emit(Op::Undef);
                self.emit(Op::Swap);
            }
            _ => self.callee(c)?,
        }
        Ok(())
    }

    fn chain_landings(&mut self, exits: Vec<(usize, u32)>, value: Op) {
        if exits.is_empty() {
            return;
        }
        let end = self.emit(Op::Jump(0));
        let mut ends = alloc::vec![end];
        for (j, depth) in exits {
            self.patch_here(j);
            for _ in 0..depth {
                self.emit(Op::Pop);
            }
            self.emit(value);
            ends.push(self.emit(Op::Jump(0)));
        }
        for e in ends {
            self.patch_here(e);
        }
    }

    fn update(&mut self, inc: bool, prefix: bool, target: &'a Expr, pos: u32) -> CResult<()> {
        let op = if inc { Op::Inc } else { Op::Dec };
        match &target.k {
            ExprKind::Ident(a) => {
                self.load_name(*a, target.pos)?;
                self.emit(Op::ToNumeric);
                if !prefix {
                    self.emit(Op::Dup);
                }
                self.emit(op);
                self.store_name(*a, Store::Assign, prefix, pos)?;
            }
            ExprKind::Member(o, name, _) => {
                self.expr(o)?;
                self.emit(Op::Dup);
                let c = self.const_atom(*name);
                self.mark(pos);
                self.emit(Op::GetProp(c));
                self.emit(Op::ToNumeric);
                if prefix {
                    self.emit(op);
                    self.emit(Op::SetProp(c));
                } else {
                    // [obj old] → [old obj new] → SetProp → [old new] → [old]
                    self.emit(Op::Dup);
                    self.emit(Op::Rot3);
                    self.emit(op);
                    self.emit(Op::SetProp(c));
                    self.emit(Op::Pop);
                }
            }
            ExprKind::Index(o, i, _) => {
                self.expr(o)?;
                self.expr(i)?;
                self.emit(Op::ToPropKey);
                self.emit(Op::Dup2);
                self.mark(pos);
                self.emit(Op::GetElem);
                self.emit(Op::ToNumeric);
                if prefix {
                    self.emit(op);
                    self.emit(Op::SetElem);
                } else {
                    self.emit(Op::Dup);
                    self.emit(Op::Rot4);
                    self.emit(op);
                    self.emit(Op::SetElem);
                    self.emit(Op::Pop);
                }
            }
            ExprKind::PrivMember(o, pn, _) => {
                self.expr(o)?;
                self.load_name(*pn, target.pos)?;
                self.emit(Op::Dup2);
                self.mark(pos);
                self.emit(Op::GetPriv);
                self.emit(Op::ToNumeric);
                if prefix {
                    self.emit(op);
                    self.emit(Op::SetPriv);
                } else {
                    self.emit(Op::Dup);
                    self.emit(Op::Rot4);
                    self.emit(op);
                    self.emit(Op::SetPriv);
                    self.emit(Op::Pop);
                }
            }
            ExprKind::SuperMember(name) => {
                let c = self.const_atom(*name);
                self.emit(Op::GetSuper(c));
                self.emit(Op::ToNumeric);
                if !prefix {
                    self.emit(Op::Dup);
                }
                self.emit(op);
                self.emit(Op::SetSuper(c));
                if !prefix {
                    self.emit(Op::Pop);
                }
            }
            ExprKind::Seq(items) if items.len() == 1 => {
                return self.update(inc, prefix, &items[0], pos);
            }
            _ => {
                return self.err(
                    "Invalid left-hand side expression in postfix operation",
                    pos,
                );
            }
        }
        Ok(())
    }

    fn assign(&mut self, op: AssignOp, target: &'a Pat, value: &'a Expr, pos: u32) -> CResult<()> {
        match target {
            Pat::Ident(a, ipos) => match op {
                AssignOp::Assign => {
                    self.named_expr(value, Some(*a))?;
                    self.store_name(*a, Store::Assign, true, *ipos)?;
                }
                AssignOp::Op(b) => {
                    self.load_name(*a, *ipos)?;
                    self.expr(value)?;
                    self.mark(pos);
                    self.emit(bin_op(b));
                    self.store_name(*a, Store::Assign, true, *ipos)?;
                }
                AssignOp::Logic(l) => {
                    self.load_name(*a, *ipos)?;
                    let j = self.emit(logic_jump(l));
                    self.named_expr(value, Some(*a))?;
                    self.store_name(*a, Store::Assign, true, *ipos)?;
                    self.patch_here(j);
                }
            },
            Pat::Expr(t) => self.assign_member(op, t, value, pos)?,
            pat => {
                // Destructuring assignment: the value is the result.
                self.expr(value)?;
                self.emit(Op::Dup);
                self.destructure(pat, Store::Assign)?;
            }
        }
        Ok(())
    }

    fn assign_member(
        &mut self,
        op: AssignOp,
        t: &'a Expr,
        value: &'a Expr,
        pos: u32,
    ) -> CResult<()> {
        match &t.k {
            ExprKind::Member(o, name, _) => {
                let c = self.const_atom(*name);
                self.expr(o)?;
                match op {
                    AssignOp::Assign => {
                        self.expr(value)?;
                        self.mark(pos);
                        self.emit(Op::SetProp(c));
                    }
                    AssignOp::Op(b) => {
                        self.emit(Op::Dup);
                        self.mark(t.pos);
                        self.emit(Op::GetProp(c));
                        self.expr(value)?;
                        self.mark(pos);
                        self.emit(bin_op(b));
                        self.emit(Op::SetProp(c));
                    }
                    AssignOp::Logic(l) => {
                        self.emit(Op::Dup);
                        self.mark(t.pos);
                        self.emit(Op::GetProp(c));
                        // [obj old]
                        let short = self.emit(logic_jump(l));
                        self.expr(value)?;
                        self.emit(Op::SetProp(c));
                        let end = self.emit(Op::Jump(0));
                        self.patch_here(short);
                        self.emit(Op::Swap);
                        self.emit(Op::Pop);
                        self.patch_here(end);
                    }
                }
            }
            ExprKind::Index(o, i, _) => {
                self.expr(o)?;
                self.expr(i)?;
                match op {
                    AssignOp::Assign => {
                        self.expr(value)?;
                        self.mark(pos);
                        self.emit(Op::SetElem);
                    }
                    AssignOp::Op(b) => {
                        self.emit(Op::ToPropKey);
                        self.emit(Op::Dup2);
                        self.mark(t.pos);
                        self.emit(Op::GetElem);
                        self.expr(value)?;
                        self.mark(pos);
                        self.emit(bin_op(b));
                        self.emit(Op::SetElem);
                    }
                    AssignOp::Logic(l) => {
                        self.emit(Op::ToPropKey);
                        self.emit(Op::Dup2);
                        self.mark(t.pos);
                        self.emit(Op::GetElem);
                        // [obj key old]
                        let short = self.emit(logic_jump(l));
                        self.expr(value)?;
                        self.emit(Op::SetElem);
                        let end = self.emit(Op::Jump(0));
                        self.patch_here(short);
                        // [obj key old] → [old]
                        self.emit(Op::Rot3);
                        self.emit(Op::Pop);
                        self.emit(Op::Pop);
                        self.patch_here(end);
                    }
                }
            }
            ExprKind::PrivMember(o, pn, _) => {
                self.expr(o)?;
                self.load_name(*pn, t.pos)?;
                match op {
                    AssignOp::Assign => {
                        self.expr(value)?;
                        self.mark(pos);
                        self.emit(Op::SetPriv);
                    }
                    AssignOp::Op(b) => {
                        self.emit(Op::Dup2);
                        self.mark(t.pos);
                        self.emit(Op::GetPriv);
                        self.expr(value)?;
                        self.mark(pos);
                        self.emit(bin_op(b));
                        self.emit(Op::SetPriv);
                    }
                    AssignOp::Logic(l) => {
                        self.emit(Op::Dup2);
                        self.mark(t.pos);
                        self.emit(Op::GetPriv);
                        // [obj name old]
                        let short = self.emit(logic_jump(l));
                        self.expr(value)?;
                        self.emit(Op::SetPriv);
                        let end = self.emit(Op::Jump(0));
                        self.patch_here(short);
                        // [obj name old] → [old]
                        self.emit(Op::Rot3);
                        self.emit(Op::Pop);
                        self.emit(Op::Pop);
                        self.patch_here(end);
                    }
                }
            }
            ExprKind::SuperMember(name) => {
                let c = self.const_atom(*name);
                match op {
                    AssignOp::Assign => {
                        self.expr(value)?;
                        self.emit(Op::SetSuper(c));
                    }
                    AssignOp::Op(b) => {
                        self.emit(Op::GetSuper(c));
                        self.expr(value)?;
                        self.emit(bin_op(b));
                        self.emit(Op::SetSuper(c));
                    }
                    AssignOp::Logic(l) => {
                        self.emit(Op::GetSuper(c));
                        let j = self.emit(logic_jump(l));
                        self.expr(value)?;
                        self.emit(Op::SetSuper(c));
                        self.patch_here(j);
                    }
                }
            }
            ExprKind::SuperIndex(i) => {
                self.expr(i)?;
                self.emit(Op::ToPropKey);
                match op {
                    AssignOp::Assign => {
                        self.expr(value)?;
                        self.emit(Op::SetSuperElem);
                    }
                    _ => {
                        self.emit(Op::Dup);
                        self.emit(Op::GetSuperElem);
                        self.expr(value)?;
                        if let AssignOp::Op(b) = op {
                            self.emit(bin_op(b));
                        }
                        self.emit(Op::SetSuperElem);
                    }
                }
            }
            ExprKind::Seq(items) if items.len() == 1 => {
                return self.assign_member(op, &items[0], value, pos);
            }
            _ => return self.err("Invalid left-hand side in assignment", t.pos),
        }
        Ok(())
    }
}

fn logic_jump(l: LogicOp) -> Op {
    match l {
        LogicOp::And => Op::JumpIfFalseKeep(0),
        LogicOp::Or => Op::JumpIfTrueKeep(0),
        LogicOp::Nullish => Op::JumpIfNotNullishKeep(0),
    }
}

fn bin_op(op: BinOp) -> Op {
    match op {
        BinOp::Add => Op::Add,
        BinOp::Sub => Op::Sub,
        BinOp::Mul => Op::Mul,
        BinOp::Div => Op::Div,
        BinOp::Mod => Op::Mod,
        BinOp::Exp => Op::Exp,
        BinOp::Shl => Op::Shl,
        BinOp::Shr => Op::Shr,
        BinOp::UShr => Op::UShr,
        BinOp::BitAnd => Op::BitAnd,
        BinOp::BitOr => Op::BitOr,
        BinOp::BitXor => Op::BitXor,
        BinOp::Eq => Op::Eq,
        BinOp::Ne => Op::Ne,
        BinOp::StrictEq => Op::StrictEq,
        BinOp::StrictNe => Op::StrictNe,
        BinOp::Lt => Op::Lt,
        BinOp::Gt => Op::Gt,
        BinOp::Le => Op::Le,
        BinOp::Ge => Op::Ge,
        BinOp::In => Op::In,
        BinOp::InstanceOf => Op::InstanceOf,
    }
}

fn pat_name(p: &Pat) -> Option<Atom> {
    match p {
        Pat::Ident(a, _) => Some(*a),
        _ => None,
    }
}

fn new_fn<'a>(kind: CodeKind, strict: bool) -> FnState<'a> {
    FnState {
        ops: Vec::new(),
        consts: Vec::new(),
        str_consts: BTreeMap::new(),
        handlers: Vec::new(),
        lines: Vec::new(),
        nlocals: 0,
        nparams: 0,
        scope_chain: Vec::new(),
        ctx: Vec::new(),
        regions: Vec::new(),
        kind,
        strict,
        is_async: false,
        is_generator: false,
        completion: None,
        this_chk: false,
        keeps_extra_args: false,
    }
}
