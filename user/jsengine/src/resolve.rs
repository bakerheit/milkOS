//! Scope analysis: declares every binding in its scope (hoisting `var` and function
//! declarations), resolves each identifier reference, and marks the bindings that inner
//! functions capture. Captured bindings live in heap environments; the rest are frame
//! locals. Script-level declarations are global (looked up by name at run time).

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::ops::{Index, IndexMut};

use crate::ast::*;
use crate::atoms::{Atom, Atoms, kw};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SK {
    Script,
    /// A module's top level: a function-like scope whose declarations are all lexical.
    Module,
    Function,
    Block,
    Catch,
    Class,
    FuncName,
    /// Stands for the environments around a function that is compiled later (see
    /// [`ChainNode`]).
    Chain,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BK {
    Var,
    Let,
    Const,
    Class,
    FuncDecl,
    Param,
    CatchParam,
    /// A named function expression's own name (assignment is ignored, or a TypeError in
    /// strict code).
    FuncName,
    Arguments,
    /// `import { x } from "m"`: a live view of another module's binding.
    Import,
}

impl BK {
    pub fn is_lexical(self) -> bool {
        matches!(self, BK::Let | BK::Const | BK::Class)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Loc {
    Unassigned,
    Local(u32),
    Env(u32),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Binding {
    pub name: Atom,
    pub kind: BK,
    pub captured: bool,
    pub loc: Loc,
    /// A sloppy-mode block function also assigned to a var of the same name (Annex B).
    pub annex_b: bool,
    /// A parameter of a function with defaults or patterns: in its TDZ until initialized.
    pub tdz: bool,
}

/// The captured bindings of one environment around a function that is compiled on its first
/// call, with the node of the next environment out. Shared by all the functions made in that
/// environment, so a module's thousands of bindings are not copied per function.
pub(crate) struct ChainNode {
    /// Every binding is in the environment: `loc` is `Env(slot)`.
    pub bindings: Vec<Binding>,
    index: Option<BTreeMap<Atom, u32>>,
    pub parent: Option<Rc<ChainNode>>,
}

impl ChainNode {
    pub fn new(bindings: Vec<Binding>, parent: Option<Rc<ChainNode>>) -> ChainNode {
        let index = (bindings.len() > INDEX_AT).then(|| {
            let mut ix = BTreeMap::new();
            for (i, b) in bindings.iter().enumerate() {
                ix.entry(b.name).or_insert(i as u32);
            }
            ix
        });
        ChainNode {
            bindings,
            index,
            parent,
        }
    }

    fn find(&self, a: Atom) -> Option<usize> {
        match &self.index {
            Some(ix) => ix.get(&a).map(|i| *i as usize),
            None => self.bindings.iter().position(|b| b.name == a),
        }
    }
}

/// The scope table, indexed by scope id from `base` (a function that is analysed on its own
/// only needs its own range of ids).
pub(crate) struct Scopes {
    base: u32,
    v: Vec<Scope>,
}

impl Default for Scopes {
    fn default() -> Scopes {
        Scopes::empty()
    }
}

impl Scopes {
    pub fn new(base: u32, count: u32) -> Scopes {
        let mut v = Vec::with_capacity(count as usize);
        for _ in 0..count {
            v.push(blank(SK::Block));
        }
        Scopes { base, v }
    }

    pub fn empty() -> Scopes {
        Scopes {
            base: 0,
            v: Vec::new(),
        }
    }

    /// Makes ids below `end` valid (blank scopes for the new ones).
    pub fn grow(&mut self, end: u32) {
        while self.base + (self.v.len() as u32) < end {
            self.v.push(blank(SK::Block));
        }
    }

    /// Forgets the scopes from id `end` on.
    pub fn truncate(&mut self, end: u32) {
        self.v.truncate(end.saturating_sub(self.base) as usize);
    }
}

impl Index<u32> for Scopes {
    type Output = Scope;
    #[inline]
    fn index(&self, i: u32) -> &Scope {
        &self.v[(i - self.base) as usize]
    }
}

impl IndexMut<u32> for Scopes {
    #[inline]
    fn index_mut(&mut self, i: u32) -> &mut Scope {
        &mut self.v[(i - self.base) as usize]
    }
}

pub(crate) struct Scope {
    pub kind: SK,
    /// For a `Chain` scope: the environment it stands for.
    pub chain: Option<Rc<ChainNode>>,
    pub parent: Option<ScopeId>,
    /// The function (or script) scope this scope belongs to.
    pub func: ScopeId,
    pub bindings: Vec<Binding>,
    pub is_arrow: bool,
    pub is_field_init: bool,
    pub uses_arguments: bool,
    pub strict: bool,
    /// Set by the compiler when it enters the scope.
    pub env_size: u32,
    /// Name -> position, for scopes with many bindings (a module can have tens of
    /// thousands; every identifier reference looks its scope chain up).
    index: Option<alloc::collections::BTreeMap<Atom, u32>>,
}

const INDEX_AT: usize = 24;

impl Scope {
    /// The `i`th binding (an index `find` returned).
    #[inline]
    pub fn bind(&self, i: usize) -> &Binding {
        match &self.chain {
            Some(c) => &c.bindings[i],
            None => &self.bindings[i],
        }
    }

    pub fn find(&self, a: Atom) -> Option<usize> {
        if let Some(c) = &self.chain {
            return c.find(a);
        }
        match &self.index {
            Some(ix) => ix.get(&a).map(|i| *i as usize),
            None => self.bindings.iter().position(|b| b.name == a),
        }
    }

    /// Adds a binding (the names of existing ones stay where they are).
    pub fn add(&mut self, b: Binding) {
        let pos = self.bindings.len() as u32;
        let name = b.name;
        self.bindings.push(b);
        match &mut self.index {
            Some(ix) => {
                ix.entry(name).or_insert(pos);
            }
            None => {
                if self.bindings.len() > INDEX_AT {
                    let mut ix = alloc::collections::BTreeMap::new();
                    for (i, b) in self.bindings.iter().enumerate() {
                        ix.entry(b.name).or_insert(i as u32);
                    }
                    self.index = Some(ix);
                }
            }
        }
    }
}

pub(crate) struct ResolveError {
    pub msg: alloc::string::String,
    pub pos: u32,
}

pub(crate) struct Resolver<'a> {
    pub scopes: Scopes,
    atoms: &'a Atoms,
    cur: ScopeId,
    depth: u32,
    pub stack: Option<crate::stack::StackGuard>,
    /// Analysing one function on its own (for the parser, which then drops its body): names
    /// that nothing inside resolves are collected instead of being taken for globals.
    analyzing: bool,
    free: Vec<Atom>,
    free_priv: Vec<(Atom, u32)>,
}

/// What analysing a function found.
pub(crate) struct Facts {
    pub free: Vec<Atom>,
    pub privs: Vec<(Atom, u32)>,
    pub self_ref: bool,
}

type RResult = Result<(), ResolveError>;

fn blank(kind: SK) -> Scope {
    Scope {
        kind,
        chain: None,
        parent: None,
        func: 0,
        bindings: Vec::new(),
        is_arrow: false,
        is_field_init: false,
        uses_arguments: false,
        strict: false,
        env_size: 0,
        index: None,
    }
}

impl<'a> Resolver<'a> {
    pub fn new(atoms: &'a Atoms, nscopes: u32) -> Resolver<'a> {
        Resolver {
            scopes: Scopes::new(0, nscopes),
            atoms,
            cur: 0,
            depth: 0,
            stack: None,
            analyzing: false,
            free: Vec::new(),
            free_priv: Vec::new(),
        }
    }

    /// A resolver for a function that is compiled later: scope 0 is the global scope, then
    /// one scope per environment around it (outermost first), then the ids the parser hands
    /// out from there. Returns it and the id of the innermost environment's scope.
    pub fn for_lazy(
        atoms: &'a Atoms,
        nscopes: u32,
        chain: &Option<Rc<ChainNode>>,
    ) -> (Resolver<'a>, ScopeId) {
        let mut nodes: Vec<Rc<ChainNode>> = Vec::new();
        let mut n = chain.clone();
        while let Some(c) = n {
            n = c.parent.clone();
            nodes.push(c);
        }
        let mut r = Resolver::new(atoms, nscopes);
        r.scopes[0].kind = SK::Script;
        let mut parent = 0;
        for (i, node) in nodes.into_iter().rev().enumerate() {
            let id = i as u32 + 1;
            let sc = &mut r.scopes[id];
            sc.kind = SK::Chain;
            sc.parent = Some(parent);
            sc.func = id;
            sc.env_size = 1;
            sc.chain = Some(node);
            parent = id;
        }
        (r, parent)
    }

    /// Resolves a function that was parsed on its own (see `for_lazy`) whose enclosing scope
    /// is `parent`.
    pub fn lazy_function(&mut self, f: &Func, parent: ScopeId) -> RResult {
        self.cur = parent;
        self.func(f)
    }

    /// Checks a function on its own (the scopes of its ids `lo..hi`) and reports what it
    /// uses from outside.
    pub fn analyze(
        atoms: &'a Atoms,
        stack: Option<crate::stack::StackGuard>,
        f: &Func,
        lo: ScopeId,
        hi: ScopeId,
    ) -> Result<Facts, ResolveError> {
        let mut r = Resolver {
            scopes: Scopes::new(lo, hi - lo + 1),
            atoms,
            cur: hi,
            depth: 0,
            stack,
            analyzing: true,
            free: Vec::new(),
            free_priv: Vec::new(),
        };
        // `hi` is a dummy scope around the function: nothing is declared in it.
        r.scopes[hi].strict = f.strict;
        r.func(f)?;
        let self_ref = f.name_scope.is_some_and(|ns| {
            r.scopes[ns].bindings.first().is_some_and(|b| b.captured)
        });
        let mut free = core::mem::take(&mut r.free);
        free.sort_unstable();
        free.dedup();
        let mut privs = core::mem::take(&mut r.free_priv);
        privs.sort_unstable();
        privs.dedup_by_key(|p| p.0);
        Ok(Facts {
            free,
            privs,
            self_ref,
        })
    }

    fn err(&self, msg: alloc::string::String, pos: u32) -> RResult {
        Err(ResolveError { msg, pos })
    }

    fn open(&mut self, id: ScopeId, kind: SK, parent: Option<ScopeId>) {
        let func = match kind {
            SK::Function | SK::Script | SK::Module => id,
            _ => parent.map_or(id, |p| self.scopes[p].func),
        };
        let strict = parent.is_some_and(|p| self.scopes[p].strict);
        let s = &mut self.scopes[id];
        s.kind = kind;
        s.parent = parent;
        s.func = func;
        s.strict = strict;
    }

    fn declare(&mut self, scope: ScopeId, name: Atom, kind: BK, pos: u32) -> RResult {
        let s = &mut self.scopes[scope];
        if s.kind == SK::Script {
            // Script-level names are global: only track lexical ones for duplicate checks.
            if let Some(i) = s.find(name) {
                let old = s.bindings[i].kind;
                if kind.is_lexical() || old.is_lexical() {
                    return self.err(
                        alloc::format!(
                            "Identifier '{}' has already been declared",
                            self.atoms.get(name)
                        ),
                        pos,
                    );
                }
                return Ok(());
            }
            s.add(Binding {
                name,
                kind,
                captured: false,
                loc: Loc::Unassigned,
                annex_b: false,
                tdz: false,
            });
            return Ok(());
        }
        if s.kind == SK::Module {
            // Everything at a module's top level is lexical, except that `var`s may repeat.
            if let Some(i) = s.find(name) {
                if s.bindings[i].kind == BK::Var && kind == BK::Var {
                    return Ok(());
                }
                return self.err(
                    alloc::format!(
                        "Identifier '{}' has already been declared",
                        self.atoms.get(name)
                    ),
                    pos,
                );
            }
            s.add(Binding {
                name,
                kind,
                captured: true,
                loc: Loc::Unassigned,
                annex_b: false,
                tdz: false,
            });
            return Ok(());
        }
        if let Some(i) = s.find(name) {
            let old = s.bindings[i].kind;
            let clash = match (old, kind) {
                (BK::Var | BK::Param | BK::Arguments, BK::Var) => false,
                // Duplicate parameters (sloppy, simple lists: the parser rejects the rest).
                (BK::Param, BK::Param) => false,
                (BK::FuncDecl, BK::Var) | (BK::Var, BK::FuncDecl) => false,
                (BK::Param, BK::FuncDecl) => {
                    s.bindings[i].kind = BK::FuncDecl;
                    false
                }
                (BK::FuncDecl, BK::FuncDecl) => s.kind != SK::Function && s.strict,
                (BK::CatchParam, BK::Var) => false,
                (BK::Arguments, _) => {
                    s.bindings[i].kind = kind;
                    false
                }
                _ => true,
            };
            if clash {
                return self.err(
                    alloc::format!(
                        "Identifier '{}' has already been declared",
                        self.atoms.get(name)
                    ),
                    pos,
                );
            }
            if old == BK::Var && kind == BK::FuncDecl {
                s.bindings[i].kind = BK::FuncDecl;
            }
            return Ok(());
        }
        s.add(Binding {
            name,
            kind,
            captured: false,
            loc: Loc::Unassigned,
            annex_b: false,
            tdz: false,
        });
        Ok(())
    }

    /// Resolves a reference from the current scope, marking captures.
    fn reference(&mut self, name: Atom) {
        self.reference_from(name, false);
    }

    /// A reference made from inside a function that is not being resolved here (its body
    /// was dropped): whatever binds the name is captured.
    fn reference_inner(&mut self, name: Atom) {
        self.reference_from(name, true);
    }

    fn reference_from(&mut self, name: Atom, from_inner: bool) {
        let cur_func = self.scopes[self.cur].func;
        let mut s = Some(self.cur);
        while let Some(sid) = s {
            let sc = &self.scopes[sid];
            if sc.kind == SK::Script {
                return;
            }
            if let Some(i) = sc.find(name) {
                let captured = from_inner || sc.func != cur_func;
                if captured && sc.chain.is_none() {
                    self.scopes[sid].bindings[i].captured = true;
                }
                return;
            }
            if name == kw::ARGUMENTS && sc.kind == SK::Function && !sc.is_arrow && !sc.is_field_init
            {
                let captured = from_inner || sid != cur_func;
                let scm = &mut self.scopes[sid];
                scm.uses_arguments = true;
                scm.add(Binding {
                    name,
                    kind: BK::Arguments,
                    captured,
                    loc: Loc::Unassigned,
                    annex_b: false,
                    tdz: false,
                });
                return;
            }
            s = sc.parent;
        }
        if self.analyzing {
            self.free.push(name);
        }
    }

    /// A use of a private name: it has to be declared by an enclosing class.
    fn private_ref(&mut self, name: Atom, pos: u32) -> RResult {
        self.private_ref_from(name, pos, false)
    }

    fn private_ref_from(&mut self, name: Atom, pos: u32, from_inner: bool) -> RResult {
        let mut s = Some(self.cur);
        while let Some(sid) = s {
            let sc = &self.scopes[sid];
            // (A `Chain` scope is a class's environment too when the class declares it.)
            if (sc.kind == SK::Class || sc.kind == SK::Chain) && sc.find(name).is_some() {
                self.reference_from(name, from_inner);
                return Ok(());
            }
            s = sc.parent;
        }
        if self.analyzing {
            self.free_priv.push((name, pos));
            return Ok(());
        }
        self.err(
            alloc::format!(
                "Private field '{}' must be declared in an enclosing class",
                self.atoms.get(name)
            ),
            pos,
        )
    }

    // ---- entry points ----

    /// A resolver for the statements of a script that is checked one at a time: the table so
    /// far, with the script scope (id 0) open in it. (The table is handed to whoever works
    /// on a statement next.)
    pub fn for_script_stmt(atoms: &'a Atoms, scopes: Scopes) -> Resolver<'a> {
        Resolver {
            scopes,
            atoms,
            cur: 0,
            depth: 0,
            stack: None,
            analyzing: false,
            free: Vec::new(),
            free_priv: Vec::new(),
        }
    }

    /// Declares the names of one top-level statement and resolves what it uses. `hi` is one
    /// past the highest scope id the statement's tree has.
    pub fn script_stmt(&mut self, st: &Stmt, strict: bool, hi: ScopeId) -> RResult {
        self.scopes.grow(hi);
        self.scopes[0].strict = strict;
        self.cur = 0;
        self.hoist_stmt(st, 0, true)?;
        self.declare_lexical(core::slice::from_ref(st), 0, true)?;
        self.stmt(st)
    }

    pub fn module(&mut self, m: &ModuleAst) -> RResult {
        self.open(m.scope, SK::Module, None);
        self.scopes[m.scope].strict = true;
        self.cur = m.scope;
        for imp in &m.info.imports {
            let bk = if matches!(imp.name, ImportName::All) {
                BK::Const
            } else {
                BK::Import
            };
            self.declare(m.scope, imp.local, bk, imp.pos)?;
        }
        self.hoist_vars(&m.body, m.scope)?;
        self.declare_lexical(&m.body, m.scope, true)?;
        self.stmts(&m.body)?;
        for le in &m.info.local_exports {
            if self.scopes[m.scope].find(le.local).is_none() {
                return self.err(
                    alloc::format!(
                        "Export '{}' is not defined in module",
                        self.atoms.get(le.local)
                    ),
                    le.pos,
                );
            }
        }
        Ok(())
    }

    // ---- hoisting ----

    /// Declares the `var`s (and top-level function declarations) of a function body.
    fn hoist_vars(&mut self, body: &[Stmt], func: ScopeId) -> RResult {
        for st in body {
            self.hoist_stmt(st, func, true)?;
        }
        Ok(())
    }

    fn hoist_stmt(&mut self, st: &Stmt, func: ScopeId, top: bool) -> RResult {
        match &st.k {
            StmtKind::Var(VarKind::Var, decls) => {
                for d in decls {
                    let mut names = Vec::new();
                    crate::parser::collect_pat_names(&d.pat, &mut names);
                    for n in names {
                        self.declare(func, n, BK::Var, st.pos)?;
                    }
                }
            }
            StmtKind::FuncDecl(f) if top => {
                if let Some(n) = f.name {
                    self.declare(func, n, BK::FuncDecl, st.pos)?;
                }
            }
            StmtKind::FuncDecl(f) => {
                // Annex B: a sloppy block function is also a var (unless that would clash).
                let strict = self.scopes[func].strict || f.strict;
                if !strict
                    && !f.is_async
                    && !f.is_generator
                    && let Some(n) = f.name
                {
                    let sc = &self.scopes[func];
                    let clash = sc.find(n).is_some_and(|i| sc.bindings[i].kind.is_lexical());
                    if !clash {
                        self.declare(func, n, BK::Var, st.pos)?;
                    }
                }
            }
            StmtKind::If(_, a, b) => {
                self.hoist_stmt(a, func, false)?;
                if let Some(b) = b {
                    self.hoist_stmt(b, func, false)?;
                }
            }
            StmtKind::Block(b) => {
                for s in &b.body {
                    self.hoist_stmt(s, func, false)?;
                }
            }
            StmtKind::For { init, body, .. } => {
                if let Some(ForInit::Var(VarKind::Var, decls)) = init {
                    for d in decls {
                        let mut names = Vec::new();
                        crate::parser::collect_pat_names(&d.pat, &mut names);
                        for n in names {
                            self.declare(func, n, BK::Var, st.pos)?;
                        }
                    }
                }
                self.hoist_stmt(body, func, false)?;
            }
            StmtKind::ForIn { left, body, .. } | StmtKind::ForOf { left, body, .. } => {
                if let ForHead::Var(VarKind::Var, p) = left {
                    let mut names = Vec::new();
                    crate::parser::collect_pat_names(p, &mut names);
                    for n in names {
                        self.declare(func, n, BK::Var, st.pos)?;
                    }
                }
                self.hoist_stmt(body, func, false)?;
            }
            StmtKind::While(_, b) | StmtKind::DoWhile(b, _) | StmtKind::Labeled(_, b) => {
                let top_label = top && matches!(st.k, StmtKind::Labeled(..));
                self.hoist_stmt(b, func, top_label)?;
            }
            StmtKind::Try {
                block,
                handler,
                finalizer,
                ..
            } => {
                for s in &block.body {
                    self.hoist_stmt(s, func, false)?;
                }
                if let Some(h) = handler {
                    for s in &h.body {
                        self.hoist_stmt(s, func, false)?;
                    }
                }
                if let Some(f) = finalizer {
                    for s in &f.body {
                        self.hoist_stmt(s, func, false)?;
                    }
                }
            }
            StmtKind::Switch { cases, .. } => {
                for c in cases {
                    for s in &c.body {
                        self.hoist_stmt(s, func, false)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Declares the let/const/class (and, in blocks, function) declarations directly in a
    /// statement list.
    fn declare_lexical(&mut self, body: &[Stmt], scope: ScopeId, is_func_top: bool) -> RResult {
        for st in body {
            let st = if let StmtKind::Labeled(_, inner) = &st.k {
                inner
            } else {
                st
            };
            match &st.k {
                StmtKind::Var(k @ (VarKind::Let | VarKind::Const), decls) => {
                    let bk = if *k == VarKind::Let {
                        BK::Let
                    } else {
                        BK::Const
                    };
                    for d in decls {
                        let mut names = Vec::new();
                        crate::parser::collect_pat_names(&d.pat, &mut names);
                        for n in names {
                            self.declare(scope, n, bk, st.pos)?;
                        }
                    }
                }
                StmtKind::ClassDecl(c) => {
                    if let Some(n) = c.name {
                        self.declare(scope, n, BK::Class, st.pos)?;
                    }
                }
                StmtKind::FuncDecl(f) if !is_func_top => {
                    if let Some(n) = f.name {
                        self.declare(scope, n, BK::FuncDecl, st.pos)?;
                        // Annex B marker (the var was hoisted by hoist_vars).
                        let strict = self.scopes[scope].strict || f.strict;
                        if !strict && !f.is_async && !f.is_generator {
                            let sc = &mut self.scopes[scope];
                            if let Some(i) = sc.find(n) {
                                sc.bindings[i].annex_b = true;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    // ---- functions ----

    fn func(&mut self, f: &Func) -> RResult {
        self.depth += 1;
        let r = self.func_inner(f);
        self.depth -= 1;
        r
    }

    fn func_inner(&mut self, f: &Func) -> RResult {
        let saved = self.cur;
        let mut parent = self.cur;
        if let Some(ns) = f.name_scope {
            self.open(ns, SK::FuncName, Some(parent));
            if let Some(n) = f.name {
                self.declare(ns, n, BK::FuncName, f.start)?;
            }
            parent = ns;
        }
        if let Body::Lazy(l) = &f.body {
            // Not resolved here: its uses of outer names are replayed from where it stands.
            if l.self_ref
                && let Some(ns) = f.name_scope
                && let Some(b) = self.scopes[ns].bindings.first_mut()
            {
                b.captured = true;
            }
            self.open(f.scope, SK::Function, Some(parent));
            self.scopes[f.scope].is_arrow = f.kind == FuncKind::Arrow;
            self.scopes[f.scope].strict = f.strict;
            self.cur = parent;
            for &a in &l.free {
                self.reference_inner(a);
            }
            for &(a, pos) in &l.privs {
                self.private_ref_from(a, pos, true)?;
            }
            self.cur = saved;
            return Ok(());
        }
        self.open(f.scope, SK::Function, Some(parent));
        {
            let s = &mut self.scopes[f.scope];
            s.is_arrow = f.kind == FuncKind::Arrow;
            s.is_field_init = f.kind == FuncKind::FieldInit;
            s.strict = f.strict;
        }
        self.cur = f.scope;
        for p in &f.params {
            let mut names = Vec::new();
            crate::parser::collect_pat_names(&p.pat, &mut names);
            for n in names {
                self.declare(f.scope, n, BK::Param, f.start)?;
            }
        }
        if let Some(r) = &f.rest {
            let mut names = Vec::new();
            crate::parser::collect_pat_names(r, &mut names);
            for n in names {
                self.declare(f.scope, n, BK::Param, f.start)?;
            }
        }
        // With defaults, patterns or a rest parameter, a parameter can't be read before its
        // own initialization runs.
        let simple = f.rest.is_none()
            && f.params
                .iter()
                .all(|p| p.default.is_none() && matches!(p.pat, Pat::Ident(..)));
        if !simple {
            for b in self.scopes[f.scope].bindings.iter_mut() {
                if b.kind == BK::Param {
                    b.tdz = true;
                }
            }
        }
        if let Body::Stmts(body) = &f.body {
            self.hoist_vars(body, f.scope)?;
            self.declare_lexical(body, f.scope, true)?;
        }
        for p in &f.params {
            self.pat(&p.pat)?;
            if let Some(d) = &p.default {
                self.expr(d)?;
            }
        }
        if let Some(r) = &f.rest {
            self.pat(r)?;
        }
        match &f.body {
            Body::Stmts(body) => self.stmts(body)?,
            Body::Expr(e) => self.expr(e)?,
            Body::Lazy(_) => {}
        }
        self.cur = saved;
        Ok(())
    }

    fn class(&mut self, c: &Class) -> RResult {
        let saved = self.cur;
        self.open(c.scope, SK::Class, Some(self.cur));
        self.scopes[c.scope].strict = true;
        if let Some(n) = c.name {
            self.declare(c.scope, n, BK::Const, c.start)?;
        }
        // Private names are bindings of the class scope (their names start with `#`, so they
        // never meet an identifier): the name itself, and for methods and accessors the
        // function that each instance gets.
        for pd in &c.privates {
            if self.scopes[c.scope].find(pd.name).is_none() {
                self.declare(c.scope, pd.name, BK::Const, pd.pos)?;
            }
            if let Some(f) = pd.func {
                self.declare(c.scope, f, BK::Const, pd.pos)?;
            }
        }
        self.cur = c.scope;
        if let Some(h) = &c.heritage {
            self.expr(h)?;
        }
        if let Some(ctor) = &c.ctor {
            self.func(ctor)?;
        }
        for m in &c.members {
            if let PropName::Computed(e) = &m.key {
                self.expr(e)?;
            }
            match &m.kind {
                MemberKind::Method(f) | MemberKind::Get(f) | MemberKind::Set(f) => self.func(f)?,
            }
        }
        for k in &c.field_keys {
            self.expr(k)?;
        }
        if let Some(f) = &c.fields {
            self.func(f)?;
        }
        if let Some(f) = &c.statics {
            self.func(f)?;
        }
        self.cur = saved;
        Ok(())
    }

    // ---- statements ----

    fn stmts(&mut self, body: &[Stmt]) -> RResult {
        for s in body {
            self.stmt(s)?;
        }
        Ok(())
    }

    fn block(&mut self, b: &Block) -> RResult {
        let saved = self.cur;
        self.open(b.scope, SK::Block, Some(self.cur));
        self.cur = b.scope;
        self.declare_lexical(&b.body, b.scope, false)?;
        self.stmts(&b.body)?;
        self.cur = saved;
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt) -> RResult {
        self.depth += 1;
        let r = self.stmt_inner(s);
        self.depth -= 1;
        r
    }

    fn stmt_inner(&mut self, s: &Stmt) -> RResult {
        match &s.k {
            StmtKind::Expr(e) | StmtKind::Throw(e) => self.expr(e),
            StmtKind::Var(_, decls) => {
                for d in decls {
                    self.pat(&d.pat)?;
                    if let Some(i) = &d.init {
                        self.expr(i)?;
                    }
                }
                Ok(())
            }
            StmtKind::FuncDecl(f) => {
                // Annex B: the statement assigns the block binding to the var.
                if let Some(n) = f.name {
                    self.reference(n);
                }
                self.func(f)
            }
            StmtKind::ClassDecl(c) => {
                if let Some(n) = c.name {
                    self.reference(n);
                }
                self.class(c)
            }
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e)?;
                }
                Ok(())
            }
            StmtKind::If(t, a, b) => {
                self.expr(t)?;
                self.sub_stmt(a)?;
                if let Some(b) = b {
                    self.sub_stmt(b)?;
                }
                Ok(())
            }
            StmtKind::Block(b) => self.block(b),
            StmtKind::For {
                init,
                test,
                update,
                body,
                scope,
            } => {
                let saved = self.cur;
                self.open(*scope, SK::Block, Some(self.cur));
                self.cur = *scope;
                if let Some(init) = init {
                    match init {
                        ForInit::Var(k, decls) => {
                            if *k != VarKind::Var {
                                let bk = if *k == VarKind::Let {
                                    BK::Let
                                } else {
                                    BK::Const
                                };
                                for d in decls {
                                    let mut names = Vec::new();
                                    crate::parser::collect_pat_names(&d.pat, &mut names);
                                    for n in names {
                                        self.declare(*scope, n, bk, s.pos)?;
                                    }
                                }
                            }
                            for d in decls {
                                self.pat(&d.pat)?;
                                if let Some(i) = &d.init {
                                    self.expr(i)?;
                                }
                            }
                        }
                        ForInit::Expr(e) => self.expr(e)?,
                    }
                }
                if let Some(t) = test {
                    self.expr(t)?;
                }
                if let Some(u) = update {
                    self.expr(u)?;
                }
                self.sub_stmt(body)?;
                self.cur = saved;
                Ok(())
            }
            StmtKind::ForIn {
                left,
                right,
                body,
                scope,
            }
            | StmtKind::ForOf {
                left,
                right,
                body,
                scope,
                ..
            } => {
                self.expr(right)?;
                let saved = self.cur;
                self.open(*scope, SK::Block, Some(self.cur));
                self.cur = *scope;
                match left {
                    ForHead::Var(k, p) => {
                        if *k != VarKind::Var {
                            let bk = if *k == VarKind::Let {
                                BK::Let
                            } else {
                                BK::Const
                            };
                            let mut names = Vec::new();
                            crate::parser::collect_pat_names(p, &mut names);
                            for n in names {
                                self.declare(*scope, n, bk, s.pos)?;
                            }
                        }
                        self.pat(p)?;
                    }
                    ForHead::Target(p) => self.pat(p)?,
                }
                self.sub_stmt(body)?;
                self.cur = saved;
                Ok(())
            }
            StmtKind::While(t, b) => {
                self.expr(t)?;
                self.sub_stmt(b)
            }
            StmtKind::DoWhile(b, t) => {
                self.sub_stmt(b)?;
                self.expr(t)
            }
            StmtKind::Try {
                block,
                param,
                handler,
                catch_scope,
                finalizer,
            } => {
                self.block(block)?;
                if let Some(h) = handler {
                    let saved = self.cur;
                    self.open(*catch_scope, SK::Catch, Some(self.cur));
                    self.cur = *catch_scope;
                    if let Some(p) = param {
                        let mut names = Vec::new();
                        crate::parser::collect_pat_names(p, &mut names);
                        for n in names {
                            self.declare(*catch_scope, n, BK::CatchParam, s.pos)?;
                        }
                        self.pat(p)?;
                    }
                    // The catch block's own lexical names may not repeat the parameter's.
                    self.open(h.scope, SK::Block, Some(self.cur));
                    self.cur = h.scope;
                    self.declare_lexical(&h.body, h.scope, false)?;
                    for b in &self.scopes[h.scope].bindings {
                        if self.scopes[*catch_scope].find(b.name).is_some()
                            && b.kind.is_lexical()
                        {
                            return self.err(
                                alloc::format!(
                                    "Identifier '{}' has already been declared",
                                    self.atoms.get(b.name)
                                ),
                                s.pos,
                            );
                        }
                    }
                    self.stmts(&h.body)?;
                    self.cur = saved;
                }
                if let Some(f) = finalizer {
                    self.block(f)?;
                }
                Ok(())
            }
            StmtKind::Switch { disc, cases, scope } => {
                self.expr(disc)?;
                let saved = self.cur;
                self.open(*scope, SK::Block, Some(self.cur));
                self.cur = *scope;
                for c in cases {
                    self.declare_lexical(&c.body, *scope, false)?;
                }
                for c in cases {
                    if let Some(t) = &c.test {
                        self.expr(t)?;
                    }
                    self.stmts(&c.body)?;
                }
                self.cur = saved;
                Ok(())
            }
            StmtKind::Labeled(_, b) => self.stmt(b),
            StmtKind::PrivMethods(list) => {
                for i in list {
                    self.reference(i.name);
                    self.reference(i.func);
                }
                Ok(())
            }
            StmtKind::Fields(defs) => {
                for d in defs {
                    if let FieldKey::Name(PropName::Computed(e)) = &d.key {
                        self.expr(e)?;
                    }
                    if let FieldKey::Name(PropName::Private(a)) = &d.key {
                        self.private_ref(*a, d.pos)?;
                    }
                    if let Some(v) = &d.value {
                        self.expr(v)?;
                    }
                }
                Ok(())
            }
            StmtKind::Break(_) | StmtKind::Continue(_) | StmtKind::Empty | StmtKind::Debugger => {
                Ok(())
            }
        }
    }

    /// The body of if/for/while: a lone function declaration there gets its own scope.
    fn sub_stmt(&mut self, s: &Stmt) -> RResult {
        self.stmt(s)
    }

    // ---- patterns and expressions ----

    fn pat(&mut self, p: &Pat) -> RResult {
        match p {
            Pat::Ident(a, _) => {
                self.reference(*a);
                Ok(())
            }
            Pat::Expr(e) => self.expr(e),
            Pat::Array(elems, rest) => {
                for e in elems.iter().flatten() {
                    self.pat(&e.pat)?;
                    if let Some(d) = &e.default {
                        self.expr(d)?;
                    }
                }
                if let Some(r) = rest {
                    self.pat(r)?;
                }
                Ok(())
            }
            Pat::Object(props, rest) => {
                for p in props {
                    if let PropName::Computed(e) = &p.key {
                        self.expr(e)?;
                    }
                    self.pat(&p.pat)?;
                    if let Some(d) = &p.default {
                        self.expr(d)?;
                    }
                }
                if let Some(r) = rest {
                    self.pat(r)?;
                }
                Ok(())
            }
        }
    }

    fn args(&mut self, args: &[Arg]) -> RResult {
        for a in args {
            match a {
                Arg::Item(e) | Arg::Spread(e) => self.expr(e)?,
            }
        }
        Ok(())
    }

    fn expr(&mut self, e: &Expr) -> RResult {
        if self.stack.is_some_and(|s| s.exceeded()) {
            return self.err(
                alloc::string::String::from("the code is nested too deeply"),
                e.pos,
            );
        }
        self.depth += 1;
        let r = self.expr_inner(e);
        self.depth -= 1;
        r
    }

    fn expr_inner(&mut self, e: &Expr) -> RResult {
        match &e.k {
            ExprKind::Ident(a) => {
                self.reference(*a);
                Ok(())
            }
            ExprKind::Num(_)
            | ExprKind::BigInt
            | ExprKind::Str(_)
            | ExprKind::Regex(..)
            | ExprKind::This
            | ExprKind::NewTarget
            | ExprKind::Null
            | ExprKind::Bool(_)
            | ExprKind::NumArray(_)
            | ExprKind::StrArray(_)
            | ExprKind::SuperMember(_) => Ok(()),
            ExprKind::Template(_, exprs) => {
                for x in exprs {
                    self.expr(x)?;
                }
                Ok(())
            }
            ExprKind::Tagged(t, _, exprs) => {
                self.expr(t)?;
                for x in exprs {
                    self.expr(x)?;
                }
                Ok(())
            }
            ExprKind::Array(items) => {
                for it in items {
                    match it {
                        ArrElem::Hole => {}
                        ArrElem::Item(x) | ArrElem::Spread(x) => self.expr(x)?,
                    }
                }
                Ok(())
            }
            ExprKind::Object(props) => {
                for p in props {
                    match p {
                        PropDef::KeyValue(k, v) => {
                            if let PropName::Computed(c) = k {
                                self.expr(c)?;
                            }
                            self.expr(v)?;
                        }
                        PropDef::Shorthand(a, _) => self.reference(*a),
                        PropDef::CoverInit(a, _, d) => {
                            self.reference(*a);
                            self.expr(d)?;
                        }
                        PropDef::Method(k, _, f) => {
                            if let PropName::Computed(c) = k {
                                self.expr(c)?;
                            }
                            self.func(f)?;
                        }
                        PropDef::Spread(x) | PropDef::Proto(x) => self.expr(x)?,
                    }
                }
                Ok(())
            }
            ExprKind::Func(f) => self.func(f),
            ExprKind::Class(c) => self.class(c),
            ExprKind::Unary(_, x)
            | ExprKind::Await(x)
            | ExprKind::OptChain(x)
            | ExprKind::SuperIndex(x)
            | ExprKind::Spread(x) => self.expr(x),
            ExprKind::Update { target, .. } => self.expr(target),
            ExprKind::Bin(_, a, b) | ExprKind::Logic(_, a, b) => {
                self.expr(a)?;
                self.expr(b)
            }
            ExprKind::Assign(_, t, v) => {
                self.pat(t)?;
                self.expr(v)
            }
            ExprKind::Cond(a, b, c) => {
                self.expr(a)?;
                self.expr(b)?;
                self.expr(c)
            }
            ExprKind::Call(c, args, _) | ExprKind::New(c, args) => {
                self.expr(c)?;
                self.args(args)
            }
            ExprKind::SuperCall(args) => self.args(args),
            ExprKind::Member(o, _, _) => self.expr(o),
            ExprKind::Index(o, i, _) => {
                self.expr(o)?;
                self.expr(i)
            }
            ExprKind::Seq(items) => {
                for x in items {
                    self.expr(x)?;
                }
                Ok(())
            }
            ExprKind::Yield(a, _) => {
                if let Some(a) = a {
                    self.expr(a)?;
                }
                Ok(())
            }
            ExprKind::ImportCall(a, b) => {
                self.expr(a)?;
                if let Some(b) = b {
                    self.expr(b)?;
                }
                Ok(())
            }
            ExprKind::ImportMeta => Ok(()),
            ExprKind::PrivMember(o, name, _) => {
                self.expr(o)?;
                self.private_ref(*name, e.pos)
            }
            ExprKind::PrivName(name) => self.private_ref(*name, e.pos),
        }
    }
}
