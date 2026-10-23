//! Scope analysis: declares every binding in its scope (hoisting `var` and function
//! declarations), resolves each identifier reference, and marks the bindings that inner
//! functions capture. Captured bindings live in heap environments; the rest are frame
//! locals. Script-level declarations are global (looked up by name at run time).

use alloc::vec::Vec;

use crate::ast::*;
use crate::atoms::{Atom, Atoms, kw};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SK {
    Script,
    Function,
    Block,
    Catch,
    Class,
    FuncName,
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
}

pub(crate) struct Scope {
    pub kind: SK,
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
}

impl Scope {
    pub fn find(&self, a: Atom) -> Option<usize> {
        self.bindings.iter().position(|b| b.name == a)
    }
}

pub(crate) struct ResolveError {
    pub msg: alloc::string::String,
    pub pos: u32,
}

pub(crate) struct Resolver<'a> {
    pub scopes: Vec<Scope>,
    atoms: &'a Atoms,
    cur: ScopeId,
    depth: u32,
    pub stack: Option<crate::stack::StackGuard>,
}

type RResult = Result<(), ResolveError>;

fn blank(kind: SK) -> Scope {
    Scope {
        kind,
        parent: None,
        func: 0,
        bindings: Vec::new(),
        is_arrow: false,
        is_field_init: false,
        uses_arguments: false,
        strict: false,
        env_size: 0,
    }
}

impl<'a> Resolver<'a> {
    pub fn new(atoms: &'a Atoms, nscopes: u32) -> Resolver<'a> {
        let mut scopes = Vec::with_capacity(nscopes as usize);
        for _ in 0..nscopes {
            scopes.push(blank(SK::Block));
        }
        Resolver {
            scopes,
            atoms,
            cur: 0,
            depth: 0,
            stack: None,
        }
    }

    fn err(&self, msg: alloc::string::String, pos: u32) -> RResult {
        Err(ResolveError { msg, pos })
    }

    fn open(&mut self, id: ScopeId, kind: SK, parent: Option<ScopeId>) {
        let func = match kind {
            SK::Function | SK::Script => id,
            _ => parent.map_or(id, |p| self.scopes[p as usize].func),
        };
        let strict = parent.is_some_and(|p| self.scopes[p as usize].strict);
        let s = &mut self.scopes[id as usize];
        s.kind = kind;
        s.parent = parent;
        s.func = func;
        s.strict = strict;
    }

    fn declare(&mut self, scope: ScopeId, name: Atom, kind: BK, pos: u32) -> RResult {
        let s = &mut self.scopes[scope as usize];
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
            s.bindings.push(Binding {
                name,
                kind,
                captured: false,
                loc: Loc::Unassigned,
                annex_b: false,
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
        s.bindings.push(Binding {
            name,
            kind,
            captured: false,
            loc: Loc::Unassigned,
            annex_b: false,
        });
        Ok(())
    }

    /// Resolves a reference from the current scope, marking captures.
    fn reference(&mut self, name: Atom) {
        let cur_func = self.scopes[self.cur as usize].func;
        let mut s = Some(self.cur);
        while let Some(sid) = s {
            let sc = &self.scopes[sid as usize];
            if sc.kind == SK::Script {
                return;
            }
            if let Some(i) = sc.find(name) {
                let captured = sc.func != cur_func;
                if captured {
                    self.scopes[sid as usize].bindings[i].captured = true;
                }
                return;
            }
            if name == kw::ARGUMENTS && sc.kind == SK::Function && !sc.is_arrow && !sc.is_field_init
            {
                let captured = sid != cur_func;
                let scm = &mut self.scopes[sid as usize];
                scm.uses_arguments = true;
                scm.bindings.push(Binding {
                    name,
                    kind: BK::Arguments,
                    captured,
                    loc: Loc::Unassigned,
                    annex_b: false,
                });
                return;
            }
            s = sc.parent;
        }
    }

    // ---- entry points ----

    pub fn script(&mut self, s: &Script) -> RResult {
        self.open(s.scope, SK::Script, None);
        self.scopes[s.scope as usize].strict = s.strict;
        self.cur = s.scope;
        self.hoist_vars(&s.body, s.scope)?;
        self.declare_lexical(&s.body, s.scope, true)?;
        self.stmts(&s.body)
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
                let strict = self.scopes[func as usize].strict || f.strict;
                if !strict
                    && !f.is_async
                    && !f.is_generator
                    && let Some(n) = f.name
                {
                    let sc = &self.scopes[func as usize];
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
                        let strict = self.scopes[scope as usize].strict || f.strict;
                        if !strict && !f.is_async && !f.is_generator {
                            let sc = &mut self.scopes[scope as usize];
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
        self.open(f.scope, SK::Function, Some(parent));
        {
            let s = &mut self.scopes[f.scope as usize];
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
        }
        self.cur = saved;
        Ok(())
    }

    fn class(&mut self, c: &Class) -> RResult {
        let saved = self.cur;
        self.open(c.scope, SK::Class, Some(self.cur));
        self.scopes[c.scope as usize].strict = true;
        if let Some(n) = c.name {
            self.declare(c.scope, n, BK::Const, c.start)?;
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
                    for b in &self.scopes[h.scope as usize].bindings {
                        if self.scopes[*catch_scope as usize].find(b.name).is_some()
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
            StmtKind::Fields(defs) => {
                for d in defs {
                    if let FieldKey::Name(PropName::Computed(e)) = &d.key {
                        self.expr(e)?;
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
            | ExprKind::Str(_)
            | ExprKind::Regex(..)
            | ExprKind::This
            | ExprKind::NewTarget
            | ExprKind::Null
            | ExprKind::Bool(_)
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
        }
    }
}
