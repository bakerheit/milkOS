//! A recursive-descent parser for ES2020 scripts (precedence climbing for binary operators, a
//! cover grammar for arrow parameters and destructuring assignment). Nesting is bounded
//! (`MAX_NEST`), so hostile input can't overflow the Rust stack.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ast::*;
use crate::atoms::{Atom, Atoms, is_reserved, is_strict_reserved, kw};
use crate::lexer::{LexError, Lexer, P, Tok, Token};

pub(crate) const MAX_NEST: u32 = 1500;

pub(crate) struct ParseError {
    pub msg: String,
    pub pos: u32,
}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> ParseError {
        ParseError {
            msg: e.msg,
            pos: e.pos,
        }
    }
}

type PResult<T> = Result<T, ParseError>;

#[derive(Clone, Copy, Default)]
struct Ctx {
    strict: bool,
    in_function: bool,
    in_generator: bool,
    in_async: bool,
    allow_super_call: bool,
    allow_super_prop: bool,
    allow_new_target: bool,
    in_class_field: bool,
}

pub(crate) struct Parser<'a> {
    lx: Lexer<'a>,
    pub atoms: Atoms,
    tok: Token,
    prev_end: u32,
    next_scope: u32,
    depth: u32,
    ctx: Ctx,
    labels: Vec<(Atom, bool)>,
    in_loop: u32,
    in_switch: u32,
    /// A `{a = 1}` cover initializer seen and not yet turned into a pattern.
    cover_init_pos: Option<u32>,
    /// The nesting depth at which parse_assign leaves a cover initializer for its caller
    /// (a parenthesized list that may turn out to be arrow parameters).
    defer_cover_at: u32,
    pub stack: Option<crate::stack::StackGuard>,
}

/// Cover for a parenthesized list: `(a, b = 1, ...c)` before we know whether it's an arrow.
struct ParenCover {
    items: Vec<Expr>,
    rest: Option<Expr>,
    trailing_comma: bool,
    pos: u32,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a [u8]) -> Parser<'a> {
        Parser {
            lx: Lexer::new(src),
            atoms: Atoms::new(),
            tok: Token {
                tok: Tok::Eof,
                start: 0,
                end: 0,
                nl_before: false,
            },
            prev_end: 0,
            next_scope: 0,
            depth: 0,
            ctx: Ctx::default(),
            labels: Vec::new(),
            in_loop: 0,
            in_switch: 0,
            cover_init_pos: None,
            defer_cover_at: u32::MAX,
            stack: None,
        }
    }

    pub fn scope_count(&self) -> u32 {
        self.next_scope
    }

    fn new_scope(&mut self) -> ScopeId {
        self.next_scope += 1;
        self.next_scope - 1
    }

    fn err<T>(&self, msg: &str) -> PResult<T> {
        Err(ParseError {
            msg: String::from(msg),
            pos: self.tok.start,
        })
    }

    fn err_at<T>(&self, msg: &str, pos: u32) -> PResult<T> {
        Err(ParseError {
            msg: String::from(msg),
            pos,
        })
    }

    fn unexpected<T>(&self) -> PResult<T> {
        let what = match &self.tok.tok {
            Tok::Eof => String::from("Unexpected end of input"),
            Tok::Name(a, _) => alloc::format!("Unexpected token '{}'", self.atoms.get(*a)),
            Tok::Num(_) => String::from("Unexpected number"),
            Tok::Str(..) => String::from("Unexpected string"),
            Tok::Template(..) => String::from("Unexpected template string"),
            Tok::P(_) => {
                let s = &self.lx.src
                    [self.tok.start as usize..(self.tok.end as usize).min(self.lx.src.len())];
                alloc::format!(
                    "Unexpected token '{}'",
                    core::str::from_utf8(s).unwrap_or("?")
                )
            }
            Tok::Regex(..) => String::from("Unexpected regular expression"),
            Tok::Private(_) => String::from("Private class members (#name) are not supported"),
            Tok::BigInt => String::from("BigInt literals are not supported"),
        };
        Err(ParseError {
            msg: what,
            pos: self.tok.start,
        })
    }

    fn advance(&mut self) -> PResult<()> {
        self.prev_end = self.tok.end;
        self.tok = self.lx.next(&mut self.atoms)?;
        if let Tok::BigInt = self.tok.tok {
            return self.unexpected();
        }
        Ok(())
    }

    /// The token after the current one (without consuming).
    fn peek(&mut self) -> PResult<Token> {
        let save = self.lx.state();
        let t = self.lx.next(&mut self.atoms);
        self.lx.set_state(save);
        Ok(t?)
    }

    #[inline]
    fn is(&self, p: P) -> bool {
        matches!(self.tok.tok, Tok::P(q) if q == p)
    }

    #[inline]
    fn is_name(&self, a: Atom) -> bool {
        matches!(self.tok.tok, Tok::Name(b, false) if b == a)
    }

    fn eat(&mut self, p: P) -> PResult<bool> {
        if self.is(p) {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect(&mut self, p: P) -> PResult<()> {
        if self.is(p) {
            self.advance()
        } else {
            self.unexpected()
        }
    }

    fn eat_name(&mut self, a: Atom) -> PResult<bool> {
        if self.is_name(a) {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn semicolon(&mut self) -> PResult<()> {
        if self.is(P::Semi) {
            return self.advance();
        }
        if self.is(P::RBrace) || matches!(self.tok.tok, Tok::Eof) || self.tok.nl_before {
            return Ok(());
        }
        self.unexpected()
    }

    fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_NEST || self.stack.is_some_and(|s| s.exceeded()) {
            return self.err("the code is nested too deeply");
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    /// An identifier usable as a binding or reference here, or None.
    fn ident_name(&self) -> Option<Atom> {
        match self.tok.tok {
            Tok::Name(a, escaped) => {
                if is_reserved(a) && !escaped {
                    return None;
                }
                if is_reserved(a) {
                    return None;
                }
                if self.ctx.strict && is_strict_reserved(a) {
                    return None;
                }
                if a == kw::YIELD && (self.ctx.in_generator || self.ctx.strict) {
                    return None;
                }
                if a == kw::AWAIT && (self.ctx.in_async || self.ctx.in_class_field) {
                    return None;
                }
                Some(a)
            }
            _ => None,
        }
    }

    fn binding_ident(&mut self) -> PResult<(Atom, u32)> {
        let pos = self.tok.start;
        match self.ident_name() {
            Some(a) => {
                if self.ctx.strict && (a == kw::EVAL || a == kw::ARGUMENTS) {
                    return self.err("Unexpected eval or arguments in strict mode");
                }
                if a == kw::LET && !self.ctx.strict {
                    // `let` can't name a lexical binding; the resolver checks that.
                }
                self.advance()?;
                Ok((a, pos))
            }
            None => self.unexpected(),
        }
    }

    // ---- scripts ----

    pub fn parse_script(&mut self) -> PResult<Script> {
        self.advance()?;
        let scope = self.new_scope();
        let (body, strict) = self.parse_body_with_directives(false)?;
        if !matches!(self.tok.tok, Tok::Eof) {
            return self.unexpected();
        }
        Ok(Script {
            body,
            scope,
            strict,
        })
    }

    /// Statements up to `}` or the end, honouring a "use strict" directive prologue.
    fn parse_body_with_directives(&mut self, in_braces: bool) -> PResult<(Vec<Stmt>, bool)> {
        let mut body = Vec::new();
        let outer_strict = self.ctx.strict;
        let mut directives = true;
        loop {
            if (in_braces && self.is(P::RBrace)) || matches!(self.tok.tok, Tok::Eof) {
                break;
            }
            if directives {
                if let Tok::Str(_, legacy) = &self.tok.tok {
                    let legacy = *legacy;
                    let raw = &self.lx.src
                        [self.tok.start as usize + 1..(self.tok.end as usize).saturating_sub(1)];
                    let is_use_strict = raw == b"use strict";
                    let p = self.peek()?;
                    let ends = matches!(p.tok, Tok::P(P::Semi) | Tok::P(P::RBrace) | Tok::Eof)
                        || p.nl_before;
                    if ends {
                        if is_use_strict {
                            self.ctx.strict = true;
                        }
                        if legacy && self.ctx.strict {
                            return self
                                .err("Octal escape sequences are not allowed in strict mode");
                        }
                    } else {
                        directives = false;
                    }
                } else {
                    directives = false;
                }
            }
            body.push(self.parse_statement_list_item()?);
        }
        let strict = self.ctx.strict;
        self.ctx.strict = outer_strict;
        Ok((body, strict))
    }

    // ---- statements ----

    fn parse_statement_list_item(&mut self) -> PResult<Stmt> {
        let pos = self.tok.start;
        match self.tok.tok {
            Tok::Name(kw::FUNCTION, false) => {
                let f = self.parse_function(true, false, pos)?;
                Ok(Stmt {
                    k: StmtKind::FuncDecl(f),
                    pos,
                })
            }
            Tok::Name(kw::ASYNC, false) => {
                let p = self.peek()?;
                if matches!(p.tok, Tok::Name(kw::FUNCTION, false)) && !p.nl_before {
                    self.advance()?;
                    let f = self.parse_function(true, true, pos)?;
                    return Ok(Stmt {
                        k: StmtKind::FuncDecl(f),
                        pos,
                    });
                }
                self.parse_statement()
            }
            Tok::Name(kw::CLASS, false) => {
                let c = self.parse_class(true)?;
                Ok(Stmt {
                    k: StmtKind::ClassDecl(c),
                    pos,
                })
            }
            Tok::Name(kw::CONST, false) => {
                self.advance()?;
                let d = self.parse_var_decls(VarKind::Const, false)?;
                self.semicolon()?;
                Ok(Stmt {
                    k: StmtKind::Var(VarKind::Const, d),
                    pos,
                })
            }
            Tok::Name(kw::LET, false) => {
                if !self.let_is_decl()? {
                    return self.parse_statement();
                }
                self.advance()?;
                let d = self.parse_var_decls(VarKind::Let, false)?;
                self.semicolon()?;
                Ok(Stmt {
                    k: StmtKind::Var(VarKind::Let, d),
                    pos,
                })
            }
            Tok::Name(kw::IMPORT, false) => {
                let p = self.peek()?;
                if matches!(p.tok, Tok::P(P::LParen) | Tok::P(P::Dot)) {
                    return self.parse_statement();
                }
                self.err("Cannot use import statement outside a module (ES modules are not supported yet)")
            }
            Tok::Name(kw::EXPORT, false) => {
                self.err("Unexpected token 'export' (ES modules are not supported yet)")
            }
            _ => self.parse_statement(),
        }
    }

    fn let_is_decl(&mut self) -> PResult<bool> {
        let p = self.peek()?;
        Ok(match p.tok {
            Tok::P(P::LBracket) | Tok::P(P::LBrace) => true,
            Tok::Name(a, _) => {
                // `let` followed by an identifier on the same line, or a keyword-like name.
                !(a == kw::IN || a == kw::INSTANCEOF) && (!p.nl_before || !is_reserved(a))
            }
            _ => false,
        })
    }

    fn parse_statement(&mut self) -> PResult<Stmt> {
        self.enter()?;
        let r = self.parse_statement_inner();
        self.leave();
        r
    }

    fn parse_statement_inner(&mut self) -> PResult<Stmt> {
        let pos = self.tok.start;
        let k = match self.tok.tok {
            Tok::P(P::LBrace) => StmtKind::Block(self.parse_block()?),
            Tok::P(P::Semi) => {
                self.advance()?;
                StmtKind::Empty
            }
            Tok::Name(a, false) => match a {
                kw::VAR => {
                    self.advance()?;
                    let d = self.parse_var_decls(VarKind::Var, false)?;
                    self.semicolon()?;
                    StmtKind::Var(VarKind::Var, d)
                }
                kw::IF => {
                    self.advance()?;
                    self.expect(P::LParen)?;
                    let test = self.parse_expression(false)?;
                    self.expect(P::RParen)?;
                    let cons = self.parse_substatement()?;
                    let alt = if self.eat_name(kw::ELSE)? {
                        Some(Box::new(self.parse_substatement()?))
                    } else {
                        None
                    };
                    StmtKind::If(test, Box::new(cons), alt)
                }
                kw::FOR => return self.parse_for(pos),
                kw::WHILE => {
                    self.advance()?;
                    self.expect(P::LParen)?;
                    let test = self.parse_expression(false)?;
                    self.expect(P::RParen)?;
                    self.in_loop += 1;
                    let body = self.parse_substatement();
                    self.in_loop -= 1;
                    StmtKind::While(test, Box::new(body?))
                }
                kw::DO => {
                    self.advance()?;
                    self.in_loop += 1;
                    let body = self.parse_substatement();
                    self.in_loop -= 1;
                    let body = body?;
                    if !self.eat_name(kw::WHILE)? {
                        return self.unexpected();
                    }
                    self.expect(P::LParen)?;
                    let test = self.parse_expression(false)?;
                    self.expect(P::RParen)?;
                    // A semicolon after do-while is optional even on the same line.
                    self.eat(P::Semi)?;
                    StmtKind::DoWhile(Box::new(body), test)
                }
                kw::CONTINUE | kw::BREAK => {
                    let is_break = a == kw::BREAK;
                    self.advance()?;
                    let mut label = None;
                    if !self.tok.nl_before
                        && let Some(l) = self.ident_name()
                    {
                        if !self
                            .labels
                            .iter()
                            .any(|(n, is_loop)| *n == l && (is_break || *is_loop))
                        {
                            return self
                                .err(&alloc::format!("Undefined label '{}'", self.atoms.get(l)));
                        }
                        self.advance()?;
                        label = Some(l);
                    }
                    if label.is_none() {
                        if is_break && self.in_loop == 0 && self.in_switch == 0 {
                            return self.err_at("Illegal break statement", pos);
                        }
                        if !is_break && self.in_loop == 0 {
                            return self.err_at(
                                "Illegal continue statement: no surrounding iteration statement",
                                pos,
                            );
                        }
                    }
                    self.semicolon()?;
                    if is_break {
                        StmtKind::Break(label)
                    } else {
                        StmtKind::Continue(label)
                    }
                }
                kw::RETURN => {
                    if !self.ctx.in_function {
                        return self.err("Illegal return statement");
                    }
                    self.advance()?;
                    let arg = if self.is(P::Semi)
                        || self.is(P::RBrace)
                        || matches!(self.tok.tok, Tok::Eof)
                        || self.tok.nl_before
                    {
                        None
                    } else {
                        Some(self.parse_expression(false)?)
                    };
                    self.semicolon()?;
                    StmtKind::Return(arg)
                }
                kw::THROW => {
                    self.advance()?;
                    if self.tok.nl_before {
                        return self.err("Illegal newline after throw");
                    }
                    let e = self.parse_expression(false)?;
                    self.semicolon()?;
                    StmtKind::Throw(e)
                }
                kw::TRY => return self.parse_try(pos),
                kw::SWITCH => return self.parse_switch(pos),
                kw::WITH => return self.err("'with' statements are not supported"),
                kw::DEBUGGER => {
                    self.advance()?;
                    self.semicolon()?;
                    StmtKind::Debugger
                }
                kw::FUNCTION => {
                    // A function declaration as a substatement (sloppy if-bodies, Annex B).
                    if self.ctx.strict {
                        return self.err("In strict mode code, functions can only be declared at top level or inside a block");
                    }
                    let f = self.parse_function(true, false, pos)?;
                    StmtKind::FuncDecl(f)
                }
                kw::CLASS | kw::CONST => return self.unexpected(),
                _ => {
                    // A labelled statement?
                    if let Some(l) = self.ident_name() {
                        let p = self.peek()?;
                        if matches!(p.tok, Tok::P(P::Colon)) {
                            self.advance()?;
                            self.advance()?;
                            if self.labels.iter().any(|(n, _)| *n == l) {
                                return self.err_at("Label has already been declared", pos);
                            }
                            let is_loop = self.label_is_loop()?;
                            self.labels.push((l, is_loop));
                            let body = if self.is_name(kw::FUNCTION) && !self.ctx.strict {
                                let fpos = self.tok.start;
                                let f = self.parse_function(true, false, fpos)?;
                                Ok(Stmt {
                                    k: StmtKind::FuncDecl(f),
                                    pos: fpos,
                                })
                            } else {
                                self.parse_statement()
                            };
                            self.labels.pop();
                            return Ok(Stmt {
                                k: StmtKind::Labeled(l, Box::new(body?)),
                                pos,
                            });
                        }
                    }
                    let e = self.parse_expression(false)?;
                    self.semicolon()?;
                    StmtKind::Expr(e)
                }
            },
            _ => {
                let e = self.parse_expression(false)?;
                self.semicolon()?;
                StmtKind::Expr(e)
            }
        };
        Ok(Stmt { k, pos })
    }

    /// Whether the statement after a label is a loop (looking through further labels).
    fn label_is_loop(&mut self) -> PResult<bool> {
        let saved = (self.lx.state(), self.tok.clone(), self.prev_end);
        let mut r = false;
        for _ in 0..1000 {
            match self.tok.tok {
                Tok::Name(kw::FOR | kw::WHILE | kw::DO, false) => {
                    r = true;
                    break;
                }
                Tok::Name(_, _) => {
                    let p = self.peek()?;
                    if matches!(p.tok, Tok::P(P::Colon)) {
                        self.advance()?;
                        self.advance()?;
                        continue;
                    }
                    break;
                }
                _ => break,
            }
        }
        self.lx.set_state(saved.0);
        self.tok = saved.1;
        self.prev_end = saved.2;
        Ok(r)
    }

    /// The body of if/while/for: a statement, but not a lexical declaration.
    fn parse_substatement(&mut self) -> PResult<Stmt> {
        if self.is_name(kw::LET) {
            let p = self.peek()?;
            if matches!(p.tok, Tok::P(P::LBracket)) {
                return self.err("Lexical declaration cannot appear in a single-statement context");
            }
        }
        self.parse_statement()
    }

    fn parse_block(&mut self) -> PResult<Block> {
        self.expect(P::LBrace)?;
        let scope = self.new_scope();
        let mut body = Vec::new();
        while !self.is(P::RBrace) {
            if matches!(self.tok.tok, Tok::Eof) {
                return self.unexpected();
            }
            body.push(self.parse_statement_list_item()?);
        }
        self.advance()?;
        Ok(Block { body, scope })
    }

    fn parse_var_decls(&mut self, kind: VarKind, no_in: bool) -> PResult<Vec<VarDecl>> {
        let mut out = Vec::new();
        loop {
            let pat = self.parse_binding_target()?;
            if kind != VarKind::Var
                && let Pat::Ident(kw::LET, p) = pat
            {
                return self.err_at("let is disallowed as a lexically bound name", p);
            }
            let init = if self.eat(P::Assign)? {
                Some(self.parse_assign(no_in)?)
            } else {
                None
            };
            out.push(VarDecl { pat, init });
            if !self.eat(P::Comma)? {
                break;
            }
        }
        Ok(out)
    }

    fn parse_for(&mut self, pos: u32) -> PResult<Stmt> {
        self.advance()?;
        let mut is_await = false;
        if self.is_name(kw::AWAIT) {
            if !self.ctx.in_async {
                return self.unexpected();
            }
            self.advance()?;
            is_await = true;
        }
        self.expect(P::LParen)?;
        let scope = self.new_scope();
        let mut init: Option<ForInit> = None;
        if self.is(P::Semi) {
            // no init
        } else {
            let decl_kind = match self.tok.tok {
                Tok::Name(kw::VAR, false) => Some(VarKind::Var),
                Tok::Name(kw::CONST, false) => Some(VarKind::Const),
                Tok::Name(kw::LET, false) => {
                    let p = self.peek()?;
                    match p.tok {
                        Tok::P(P::LBracket) | Tok::P(P::LBrace) => Some(VarKind::Let),
                        Tok::Name(a, _) if a != kw::IN && !(a == kw::OF) => Some(VarKind::Let),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(kind) = decl_kind {
                self.advance()?;
                let decls = self.parse_var_decls(kind, true)?;
                if decls.len() == 1 && (self.is_name(kw::OF) || self.is_name(kw::IN)) {
                    let is_of = self.is_name(kw::OF);
                    let d = decls.into_iter().next();
                    let Some(d) = d else { return self.unexpected() };
                    if d.init.is_some()
                        && (is_of
                            || self.ctx.strict
                            || kind != VarKind::Var
                            || !matches!(d.pat, Pat::Ident(..)))
                    {
                        return self.err(
                            "for-in/of loop variable declaration may not have an initializer",
                        );
                    }
                    self.advance()?;
                    let right = if is_of {
                        self.parse_assign(false)?
                    } else {
                        self.parse_expression(false)?
                    };
                    self.expect(P::RParen)?;
                    let body = self.loop_body()?;
                    let left = ForHead::Var(kind, d.pat);
                    let k = if is_of {
                        StmtKind::ForOf {
                            left,
                            right,
                            body: Box::new(body),
                            scope,
                            is_await,
                        }
                    } else {
                        StmtKind::ForIn {
                            left,
                            right,
                            body: Box::new(body),
                            scope,
                        }
                    };
                    return Ok(Stmt { k, pos });
                }
                if kind == VarKind::Const {
                    for d in &decls {
                        if d.init.is_none() {
                            return self.err("Missing initializer in const declaration");
                        }
                    }
                }
                init = Some(ForInit::Var(kind, decls));
            } else {
                let start = self.tok.start;
                let starts_with_let = self.is_name(kw::LET);
                let starts_with_async = self.is_name(kw::ASYNC);
                let e = self.parse_expression(true)?;
                if self.is_name(kw::OF) || self.is_name(kw::IN) {
                    let is_of = self.is_name(kw::OF);
                    if is_of
                        && (starts_with_let
                            || (starts_with_async && matches!(e.k, ExprKind::Ident(kw::ASYNC))))
                    {
                        return self.err(
                            "The left-hand side of a for-of loop may not be 'let' or 'async'",
                        );
                    }
                    let target = self.expr_to_pattern(e, false)?;
                    self.advance()?;
                    let right = if is_of {
                        self.parse_assign(false)?
                    } else {
                        self.parse_expression(false)?
                    };
                    self.expect(P::RParen)?;
                    let body = self.loop_body()?;
                    let left = ForHead::Target(target);
                    let k = if is_of {
                        StmtKind::ForOf {
                            left,
                            right,
                            body: Box::new(body),
                            scope,
                            is_await,
                        }
                    } else {
                        StmtKind::ForIn {
                            left,
                            right,
                            body: Box::new(body),
                            scope,
                        }
                    };
                    let _ = start;
                    return Ok(Stmt { k, pos });
                }
                self.check_no_cover_init()?;
                init = Some(ForInit::Expr(e));
            }
        }
        if is_await {
            return self.err("for await requires 'of'");
        }
        self.expect(P::Semi)?;
        let test = if self.is(P::Semi) {
            None
        } else {
            Some(self.parse_expression(false)?)
        };
        self.expect(P::Semi)?;
        let update = if self.is(P::RParen) {
            None
        } else {
            Some(self.parse_expression(false)?)
        };
        self.expect(P::RParen)?;
        let body = self.loop_body()?;
        Ok(Stmt {
            k: StmtKind::For {
                init,
                test,
                update,
                body: Box::new(body),
                scope,
            },
            pos,
        })
    }

    fn loop_body(&mut self) -> PResult<Stmt> {
        self.in_loop += 1;
        let b = self.parse_substatement();
        self.in_loop -= 1;
        b
    }

    fn parse_try(&mut self, pos: u32) -> PResult<Stmt> {
        self.advance()?;
        let block = self.parse_block()?;
        let catch_scope = self.new_scope();
        let mut param = None;
        let mut handler = None;
        if self.eat_name(kw::CATCH)? {
            if self.eat(P::LParen)? {
                param = Some(self.parse_binding_target()?);
                self.expect(P::RParen)?;
            }
            handler = Some(self.parse_block()?);
        }
        let finalizer = if self.eat_name(kw::FINALLY)? {
            Some(self.parse_block()?)
        } else {
            None
        };
        if handler.is_none() && finalizer.is_none() {
            return self.err("Missing catch or finally after try");
        }
        Ok(Stmt {
            k: StmtKind::Try {
                block,
                param,
                handler,
                catch_scope,
                finalizer,
            },
            pos,
        })
    }

    fn parse_switch(&mut self, pos: u32) -> PResult<Stmt> {
        self.advance()?;
        self.expect(P::LParen)?;
        let disc = self.parse_expression(false)?;
        self.expect(P::RParen)?;
        self.expect(P::LBrace)?;
        let scope = self.new_scope();
        let mut cases = Vec::new();
        let mut seen_default = false;
        self.in_switch += 1;
        let r: PResult<()> = (|| {
            while !self.is(P::RBrace) {
                let test = if self.eat_name(kw::CASE)? {
                    Some(self.parse_expression(false)?)
                } else if self.eat_name(kw::DEFAULT)? {
                    if seen_default {
                        return self.err("More than one default clause in switch statement");
                    }
                    seen_default = true;
                    None
                } else {
                    return self.unexpected();
                };
                self.expect(P::Colon)?;
                let mut body = Vec::new();
                while !self.is(P::RBrace) && !self.is_name(kw::CASE) && !self.is_name(kw::DEFAULT) {
                    if matches!(self.tok.tok, Tok::Eof) {
                        return self.unexpected();
                    }
                    body.push(self.parse_statement_list_item()?);
                }
                cases.push(Case { test, body });
            }
            Ok(())
        })();
        self.in_switch -= 1;
        r?;
        self.advance()?;
        Ok(Stmt {
            k: StmtKind::Switch { disc, cases, scope },
            pos,
        })
    }

    // ---- functions ----

    /// After `function` (current token) or `async` already consumed.
    fn parse_function(&mut self, is_decl: bool, is_async: bool, start: u32) -> PResult<Box<Func>> {
        self.advance()?; // `function`
        let is_generator = self.eat(P::Star)?;
        let mut name = None;
        if !self.is(P::LParen) {
            // The name of an expression is bound inside it, so `yield`/`await` rules for it
            // are the function's own.
            let saved = self.ctx;
            if !is_decl {
                self.ctx.in_generator = is_generator;
                self.ctx.in_async = is_async;
            }
            let r = self.binding_ident();
            self.ctx = saved;
            name = Some(r?.0);
        } else if is_decl {
            return self.unexpected();
        }
        let name_scope = if !is_decl && name.is_some() {
            Some(self.new_scope())
        } else {
            None
        };
        let mut f = self.parse_function_rest(FuncKind::Normal, is_async, is_generator, start)?;
        f.name = name;
        f.name_scope = name_scope;
        f.is_decl = is_decl;
        if f.strict
            && let Some(n) = name
            && (n == kw::EVAL || n == kw::ARGUMENTS)
        {
            return self.err_at("Unexpected eval or arguments in strict mode", start);
        }
        Ok(f)
    }

    /// Parameters and body (current token `(`).
    fn parse_function_rest(
        &mut self,
        kind: FuncKind,
        is_async: bool,
        is_generator: bool,
        start: u32,
    ) -> PResult<Box<Func>> {
        let saved_ctx = self.ctx;
        let saved_labels = core::mem::take(&mut self.labels);
        let saved_loop = self.in_loop;
        let saved_switch = self.in_switch;
        self.in_loop = 0;
        self.in_switch = 0;
        self.ctx.in_function = true;
        self.ctx.in_generator = is_generator;
        self.ctx.in_async = is_async;
        self.ctx.in_class_field = false;
        self.ctx.allow_new_target = true;
        match kind {
            FuncKind::Method | FuncKind::Getter | FuncKind::Setter | FuncKind::ClassCtor => {
                self.ctx.allow_super_prop = true;
                self.ctx.allow_super_call = false;
            }
            FuncKind::DerivedCtor => {
                self.ctx.allow_super_prop = true;
                self.ctx.allow_super_call = true;
            }
            _ => {
                self.ctx.allow_super_prop = false;
                self.ctx.allow_super_call = false;
            }
        }
        let r = self.parse_function_parts(kind, is_async, is_generator, start);
        self.ctx = saved_ctx;
        self.labels = saved_labels;
        self.in_loop = saved_loop;
        self.in_switch = saved_switch;
        r
    }

    fn parse_function_parts(
        &mut self,
        kind: FuncKind,
        is_async: bool,
        is_generator: bool,
        start: u32,
    ) -> PResult<Box<Func>> {
        let scope = self.new_scope();
        self.expect(P::LParen)?;
        let (params, rest) = self.parse_params()?;
        if kind == FuncKind::Getter && (!params.is_empty() || rest.is_some()) {
            return self.err("Getter must not have any formal parameters");
        }
        if kind == FuncKind::Setter && (params.len() != 1 || rest.is_some()) {
            return self.err("Setter must have exactly one formal parameter");
        }
        self.expect(P::LBrace)?;
        let (body, strict) = self.parse_body_with_directives(true)?;
        let end = self.tok.end;
        self.expect(P::RBrace)?;
        let simple = rest.is_none()
            && params
                .iter()
                .all(|p| p.default.is_none() && matches!(p.pat, Pat::Ident(..)));
        if strict && !self.ctx.strict && !simple {
            return self.err_at(
                "Illegal 'use strict' directive in function with non-simple parameter list",
                start,
            );
        }
        if strict
            || !simple
            || kind == FuncKind::Arrow
            || matches!(
                kind,
                FuncKind::Method
                    | FuncKind::Getter
                    | FuncKind::Setter
                    | FuncKind::ClassCtor
                    | FuncKind::DerivedCtor
            )
        {
            // Duplicate parameters are an error here.
            let mut names: Vec<Atom> = Vec::new();
            for p in &params {
                collect_pat_names(&p.pat, &mut names);
            }
            if let Some(r) = &rest {
                collect_pat_names(r, &mut names);
            }
            for (i, n) in names.iter().enumerate() {
                if names[..i].contains(n) {
                    return self.err_at(
                        "Duplicate parameter name not allowed in this context",
                        start,
                    );
                }
                if strict && (*n == kw::EVAL || *n == kw::ARGUMENTS) {
                    return self.err_at("Unexpected eval or arguments in strict mode", start);
                }
            }
        }
        Ok(Box::new(Func {
            name: None,
            params,
            rest,
            body: Body::Stmts(body),
            kind,
            is_async,
            is_generator,
            strict: strict || self.ctx.strict,
            scope,
            name_scope: None,
            start,
            end,
            is_decl: false,
        }))
    }

    /// After `(`, through `)`.
    fn parse_params(&mut self) -> PResult<(Vec<Param>, Option<Pat>)> {
        let mut params = Vec::new();
        let mut rest = None;
        while !self.is(P::RParen) {
            if self.eat(P::Ellipsis)? {
                rest = Some(self.parse_binding_target()?);
                if !self.is(P::RParen) {
                    return self.err("Rest parameter must be last formal parameter");
                }
                break;
            }
            let pat = self.parse_binding_target()?;
            let default = if self.eat(P::Assign)? {
                Some(self.parse_assign(false)?)
            } else {
                None
            };
            params.push(Param { pat, default });
            if !self.is(P::RParen) {
                self.expect(P::Comma)?;
            }
        }
        self.advance()?;
        Ok((params, rest))
    }

    // ---- binding patterns ----

    fn parse_binding_target(&mut self) -> PResult<Pat> {
        self.enter()?;
        let r = self.parse_binding_target_inner();
        self.leave();
        r
    }

    fn parse_binding_target_inner(&mut self) -> PResult<Pat> {
        if self.is(P::LBracket) {
            self.advance()?;
            let mut elems = Vec::new();
            let mut rest = None;
            loop {
                if self.eat(P::RBracket)? {
                    break;
                }
                if self.is(P::Comma) {
                    self.advance()?;
                    elems.push(None);
                    continue;
                }
                if self.eat(P::Ellipsis)? {
                    rest = Some(Box::new(self.parse_binding_target()?));
                    self.expect(P::RBracket)?;
                    break;
                }
                let pat = self.parse_binding_target()?;
                let default = if self.eat(P::Assign)? {
                    Some(self.parse_assign(false)?)
                } else {
                    None
                };
                elems.push(Some(PatElem { pat, default }));
                if !self.is(P::RBracket) {
                    self.expect(P::Comma)?;
                }
            }
            return Ok(Pat::Array(elems, rest));
        }
        if self.is(P::LBrace) {
            self.advance()?;
            let mut props = Vec::new();
            let mut rest = None;
            loop {
                if self.eat(P::RBrace)? {
                    break;
                }
                if self.eat(P::Ellipsis)? {
                    let (a, p) = self.binding_ident()?;
                    rest = Some(Box::new(Pat::Ident(a, p)));
                    self.expect(P::RBrace)?;
                    break;
                }
                let kpos = self.tok.start;
                let shorthand_name = self.ident_name();
                let key = self.parse_prop_name()?;
                let (pat, default);
                if self.eat(P::Colon)? {
                    pat = self.parse_binding_target()?;
                    default = if self.eat(P::Assign)? {
                        Some(self.parse_assign(false)?)
                    } else {
                        None
                    };
                } else {
                    let Some(a) = shorthand_name else {
                        return self.unexpected();
                    };
                    if !matches!(key, PropName::Ident(_)) {
                        return self.unexpected();
                    }
                    if self.ctx.strict && (a == kw::EVAL || a == kw::ARGUMENTS) {
                        return self.err("Unexpected eval or arguments in strict mode");
                    }
                    pat = Pat::Ident(a, kpos);
                    default = if self.eat(P::Assign)? {
                        Some(self.parse_assign(false)?)
                    } else {
                        None
                    };
                }
                props.push(PatProp { key, pat, default });
                if !self.is(P::RBrace) {
                    self.expect(P::Comma)?;
                }
            }
            return Ok(Pat::Object(props, rest));
        }
        let (a, p) = self.binding_ident()?;
        Ok(Pat::Ident(a, p))
    }

    fn parse_prop_name(&mut self) -> PResult<PropName> {
        let k = match &self.tok.tok {
            Tok::Name(a, _) => PropName::Ident(*a),
            Tok::Str(s, legacy) => {
                if *legacy && self.ctx.strict {
                    return self.err("Octal escape sequences are not allowed in strict mode");
                }
                PropName::Str(s.clone())
            }
            Tok::Num(n) => PropName::Num(*n),
            Tok::P(P::LBracket) => {
                self.advance()?;
                let e = self.parse_assign(false)?;
                self.expect(P::RBracket)?;
                return Ok(PropName::Computed(Box::new(e)));
            }
            Tok::Private(_) => return self.err("Private class members (#name) are not supported"),
            _ => return self.unexpected(),
        };
        self.advance()?;
        Ok(k)
    }

    // ---- classes ----

    fn parse_class(&mut self, is_decl: bool) -> PResult<Box<Class>> {
        let start = self.tok.start;
        self.advance()?; // class
        let saved_strict = self.ctx.strict;
        self.ctx.strict = true;
        let r = self.parse_class_rest(is_decl, start);
        self.ctx.strict = saved_strict;
        r
    }

    fn parse_class_rest(&mut self, is_decl: bool, start: u32) -> PResult<Box<Class>> {
        let mut name = None;
        if !self.is_name(kw::EXTENDS) && !self.is(P::LBrace) {
            name = Some(self.binding_ident()?.0);
        } else if is_decl {
            return self.unexpected();
        }
        let scope = self.new_scope();
        let heritage = if self.eat_name(kw::EXTENDS)? {
            Some(Box::new(self.parse_lhs_expression()?))
        } else {
            None
        };
        self.expect(P::LBrace)?;
        let mut ctor: Option<Box<Func>> = None;
        let mut members = Vec::new();
        let mut inst_fields: Vec<FieldDef> = Vec::new();
        let mut static_body: Vec<Stmt> = Vec::new();
        let mut field_keys: Vec<Expr> = Vec::new();
        let mut field_key_static: Vec<bool> = Vec::new();
        let mut field_key_at: Vec<u32> = Vec::new();
        let derived = heritage.is_some();
        let mut fields_start = 0u32;
        let mut statics_start = 0u32;
        loop {
            if self.eat(P::RBrace)? {
                break;
            }
            if self.eat(P::Semi)? {
                continue;
            }
            let mpos = self.tok.start;
            let mut is_static = false;
            if self.is_name(kw::STATIC) {
                let p = self.peek()?;
                if !matches!(
                    p.tok,
                    Tok::P(P::LParen) | Tok::P(P::Assign) | Tok::P(P::Semi) | Tok::P(P::RBrace)
                ) {
                    self.advance()?;
                    is_static = true;
                    if self.is(P::LBrace) {
                        // A static block: like a method body with this = the class.
                        let saved = self.ctx;
                        self.ctx.in_function = true;
                        self.ctx.in_async = false;
                        self.ctx.in_generator = false;
                        self.ctx.allow_super_prop = true;
                        self.ctx.allow_super_call = false;
                        self.ctx.allow_new_target = true;
                        self.ctx.in_class_field = true;
                        let saved_labels = core::mem::take(&mut self.labels);
                        let b = self.parse_block();
                        self.labels = saved_labels;
                        self.ctx = saved;
                        let b = b?;
                        if statics_start == 0 {
                            statics_start = mpos;
                        }
                        static_body.push(Stmt {
                            k: StmtKind::Block(b),
                            pos: mpos,
                        });
                        continue;
                    }
                }
            }
            let mut is_async = false;
            let mut is_gen = false;
            let mut acc: Option<MethodKind> = None;
            if self.is_name(kw::ASYNC) {
                let p = self.peek()?;
                if !p.nl_before
                    && !matches!(
                        p.tok,
                        Tok::P(P::LParen) | Tok::P(P::Assign) | Tok::P(P::Semi) | Tok::P(P::RBrace)
                    )
                {
                    self.advance()?;
                    is_async = true;
                }
            }
            if self.eat(P::Star)? {
                is_gen = true;
            }
            if !is_async && !is_gen && (self.is_name(kw::GET) || self.is_name(kw::SET)) {
                let p = self.peek()?;
                if !matches!(
                    p.tok,
                    Tok::P(P::LParen) | Tok::P(P::Assign) | Tok::P(P::Semi) | Tok::P(P::RBrace)
                ) {
                    acc = Some(if self.is_name(kw::GET) {
                        MethodKind::Get
                    } else {
                        MethodKind::Set
                    });
                    self.advance()?;
                }
            }
            let key_is_ctor = matches!(self.tok.tok, Tok::Name(kw::CONSTRUCTOR, _))
                || matches!(&self.tok.tok, Tok::Str(s, _) if crate::string::eq_str(s, "constructor"));
            let key = self.parse_prop_name()?;
            if self.is(P::LParen) {
                if key_is_ctor && !is_static {
                    if is_async || is_gen || acc.is_some() {
                        return self.err_at("Class constructor may not be a special method", mpos);
                    }
                    if ctor.is_some() {
                        return self.err_at("A class may only have one constructor", mpos);
                    }
                    let kind = if derived {
                        FuncKind::DerivedCtor
                    } else {
                        FuncKind::ClassCtor
                    };
                    let f = self.parse_function_rest(kind, false, false, start)?;
                    ctor = Some(f);
                    continue;
                }
                let fk = match acc {
                    Some(MethodKind::Get) => FuncKind::Getter,
                    Some(MethodKind::Set) => FuncKind::Setter,
                    _ => FuncKind::Method,
                };
                let f = self.parse_function_rest(fk, is_async, is_gen, mpos)?;
                let kind = match acc {
                    Some(MethodKind::Get) => MemberKind::Get(f),
                    Some(MethodKind::Set) => MemberKind::Set(f),
                    _ => MemberKind::Method(f),
                };
                if is_static && matches!(&key, PropName::Ident(kw::PROTOTYPE)) {
                    return self.err_at(
                        "Classes may not have a static property named 'prototype'",
                        mpos,
                    );
                }
                members.push(ClassMember {
                    is_static,
                    key,
                    kind,
                });
                continue;
            }
            // A field.
            if is_async || is_gen || acc.is_some() || key_is_ctor {
                return self.unexpected();
            }
            let fkey = match key {
                PropName::Computed(e) => {
                    field_keys.push(*e);
                    field_key_static.push(is_static);
                    field_key_at.push(members.len() as u32);
                    FieldKey::Computed(field_keys.len() as u32 - 1)
                }
                k => FieldKey::Name(k),
            };
            let value = if self.eat(P::Assign)? {
                let saved = self.ctx;
                self.ctx.in_function = true;
                self.ctx.in_async = false;
                self.ctx.in_generator = false;
                self.ctx.allow_super_prop = true;
                self.ctx.allow_super_call = false;
                self.ctx.allow_new_target = true;
                self.ctx.in_class_field = true;
                let v = self.parse_assign(false);
                self.ctx = saved;
                Some(v?)
            } else {
                None
            };
            self.semicolon()?;
            let fd = FieldDef {
                key: fkey,
                value,
                pos: mpos,
            };
            if is_static {
                if statics_start == 0 {
                    statics_start = mpos;
                }
                static_body.push(Stmt {
                    k: StmtKind::Fields(alloc::vec![fd]),
                    pos: mpos,
                });
            } else {
                if fields_start == 0 {
                    fields_start = mpos;
                }
                inst_fields.push(fd);
            }
        }
        let end = self.prev_end;
        let mk = |p: &mut Parser, body: Vec<Stmt>, at: u32| -> Box<Func> {
            let scope = p.new_scope();
            Box::new(Func {
                name: None,
                params: Vec::new(),
                rest: None,
                body: Body::Stmts(body),
                kind: FuncKind::FieldInit,
                is_async: false,
                is_generator: false,
                strict: true,
                scope,
                name_scope: None,
                start: at,
                end,
                is_decl: false,
            })
        };
        let fields = if inst_fields.is_empty() {
            None
        } else {
            Some(mk(
                self,
                alloc::vec![Stmt {
                    k: StmtKind::Fields(inst_fields),
                    pos: fields_start
                }],
                fields_start,
            ))
        };
        let statics = if static_body.is_empty() {
            None
        } else {
            Some(mk(self, static_body, statics_start))
        };
        Ok(Box::new(Class {
            name,
            heritage,
            ctor,
            members,
            fields,
            statics,
            field_keys,
            field_key_static,
            field_key_at,
            scope,
            start,
            end,
        }))
    }

    // ---- expressions ----

    pub fn parse_expression(&mut self, no_in: bool) -> PResult<Expr> {
        let pos = self.tok.start;
        let first = self.parse_assign(no_in)?;
        if !self.is(P::Comma) {
            return Ok(first);
        }
        let mut items = alloc::vec![first];
        while self.eat(P::Comma)? {
            items.push(self.parse_assign(no_in)?);
        }
        Ok(Expr {
            k: ExprKind::Seq(items),
            pos,
        })
    }

    fn check_no_cover_init(&mut self) -> PResult<()> {
        if let Some(p) = self.cover_init_pos.take() {
            return self.err_at("Invalid shorthand property initializer", p);
        }
        Ok(())
    }

    pub fn parse_assign(&mut self, no_in: bool) -> PResult<Expr> {
        self.enter()?;
        let r = self.parse_assign_inner(no_in);
        self.leave();
        r
    }

    fn parse_assign_inner(&mut self, no_in: bool) -> PResult<Expr> {
        let pos = self.tok.start;
        // yield
        if self.is_name(kw::YIELD) && self.ctx.in_generator {
            if self.ctx.in_class_field {
                return self.unexpected();
            }
            self.advance()?;
            let delegate = !self.tok.nl_before && self.eat(P::Star)?;
            let has_arg = delegate
                || !(self.tok.nl_before
                    || matches!(
                        self.tok.tok,
                        Tok::P(P::RParen | P::RBracket | P::RBrace | P::Comma | P::Semi | P::Colon)
                            | Tok::Eof
                    )
                    || self.is_name(kw::IN));
            let arg = if has_arg {
                Some(Box::new(self.parse_assign(no_in)?))
            } else {
                None
            };
            return Ok(Expr {
                k: ExprKind::Yield(arg, delegate),
                pos,
            });
        }
        // async arrow: `async x => ...` / `async (..) => ...`
        if self.is_name(kw::ASYNC) {
            let p = self.peek()?;
            if !p.nl_before
                && let Tok::Name(a, _) = p.tok
                && a != kw::FUNCTION
            {
                // async x => body
                let save_pos = self.lx.state();
                let save_tok = self.tok.clone();
                let save_prev = self.prev_end;
                self.advance()?;
                if let Some(param) = self.ident_name() {
                    let ppos = self.tok.start;
                    let p2 = self.peek()?;
                    if matches!(p2.tok, Tok::P(P::Arrow)) && !p2.nl_before {
                        self.advance()?;
                        self.advance()?; // =>
                        let params = alloc::vec![Param {
                            pat: Pat::Ident(param, ppos),
                            default: None
                        }];
                        return self.parse_arrow_body(params, None, true, pos);
                    }
                }
                self.lx.set_state(save_pos);
                self.tok = save_tok;
                self.prev_end = save_prev;
            }
        }
        let start_tok_is_paren = self.is(P::LParen);
        let start_ident = self.ident_name();
        let lhs = self.parse_conditional(no_in)?;
        // Arrow with a single identifier parameter.
        if self.is(P::Arrow) {
            if self.tok.nl_before {
                return self.unexpected();
            }
            if let (Some(a), ExprKind::Ident(b)) = (start_ident, &lhs.k)
                && a == *b
                && !start_tok_is_paren
            {
                if self.ctx.strict && (a == kw::EVAL || a == kw::ARGUMENTS) {
                    return self.err("Unexpected eval or arguments in strict mode");
                }
                self.advance()?;
                let params = alloc::vec![Param {
                    pat: Pat::Ident(a, lhs.pos),
                    default: None
                }];
                return self.parse_arrow_body(params, None, false, pos);
            }
            return self.unexpected();
        }
        let op = match self.tok.tok {
            Tok::P(P::Assign) => AssignOp::Assign,
            Tok::P(P::PlusEq) => AssignOp::Op(BinOp::Add),
            Tok::P(P::MinusEq) => AssignOp::Op(BinOp::Sub),
            Tok::P(P::StarEq) => AssignOp::Op(BinOp::Mul),
            Tok::P(P::SlashEq) => AssignOp::Op(BinOp::Div),
            Tok::P(P::PercentEq) => AssignOp::Op(BinOp::Mod),
            Tok::P(P::StarStarEq) => AssignOp::Op(BinOp::Exp),
            Tok::P(P::ShlEq) => AssignOp::Op(BinOp::Shl),
            Tok::P(P::ShrEq) => AssignOp::Op(BinOp::Shr),
            Tok::P(P::UShrEq) => AssignOp::Op(BinOp::UShr),
            Tok::P(P::AmpEq) => AssignOp::Op(BinOp::BitAnd),
            Tok::P(P::PipeEq) => AssignOp::Op(BinOp::BitOr),
            Tok::P(P::CaretEq) => AssignOp::Op(BinOp::BitXor),
            Tok::P(P::AndAndEq) => AssignOp::Logic(LogicOp::And),
            Tok::P(P::OrOrEq) => AssignOp::Logic(LogicOp::Or),
            Tok::P(P::NullishEq) => AssignOp::Logic(LogicOp::Nullish),
            _ => {
                if self.depth != self.defer_cover_at {
                    self.check_no_cover_init()?;
                }
                return Ok(lhs);
            }
        };
        let target = if op == AssignOp::Assign {
            self.expr_to_pattern(lhs, false)?
        } else {
            match lhs.k {
                ExprKind::Ident(a) => {
                    if self.ctx.strict && (a == kw::EVAL || a == kw::ARGUMENTS) {
                        return self.err("Unexpected eval or arguments in strict mode");
                    }
                    Pat::Ident(a, lhs.pos)
                }
                ExprKind::Member(..)
                | ExprKind::Index(..)
                | ExprKind::SuperMember(_)
                | ExprKind::SuperIndex(_) => Pat::Expr(Box::new(lhs)),
                _ => return self.err_at("Invalid left-hand side in assignment", lhs.pos),
            }
        };
        self.cover_init_pos = None;
        self.advance()?;
        let value = self.parse_assign(no_in)?;
        Ok(Expr {
            k: ExprKind::Assign(op, Box::new(target), Box::new(value)),
            pos,
        })
    }

    fn parse_arrow_body(
        &mut self,
        params: Vec<Param>,
        rest: Option<Pat>,
        is_async: bool,
        start: u32,
    ) -> PResult<Expr> {
        let saved_ctx = self.ctx;
        let saved_labels = core::mem::take(&mut self.labels);
        let saved_loop = self.in_loop;
        let saved_switch = self.in_switch;
        self.in_loop = 0;
        self.in_switch = 0;
        self.ctx.in_function = true;
        self.ctx.in_generator = false;
        self.ctx.in_async = is_async;
        // Arrows keep super/new.target rules of their context.
        let scope = self.new_scope();
        let r: PResult<(Body, bool, u32)> = (|| {
            if self.is(P::LBrace) {
                self.advance()?;
                let (body, strict) = self.parse_body_with_directives(true)?;
                let end = self.tok.end;
                self.expect(P::RBrace)?;
                Ok((Body::Stmts(body), strict, end))
            } else {
                let e = self.parse_assign(false)?;
                let end = self.prev_end;
                Ok((Body::Expr(Box::new(e)), self.ctx.strict, end))
            }
        })();
        let in_class_field = saved_ctx.in_class_field;
        self.ctx = saved_ctx;
        self.labels = saved_labels;
        self.in_loop = saved_loop;
        self.in_switch = saved_switch;
        let (body, strict, end) = r?;
        let _ = in_class_field;
        let simple = rest.is_none()
            && params
                .iter()
                .all(|p| p.default.is_none() && matches!(p.pat, Pat::Ident(..)));
        if strict && !self.ctx.strict && !simple {
            return self.err_at(
                "Illegal 'use strict' directive in function with non-simple parameter list",
                start,
            );
        }
        let mut names: Vec<Atom> = Vec::new();
        for p in &params {
            collect_pat_names(&p.pat, &mut names);
        }
        if let Some(r) = &rest {
            collect_pat_names(r, &mut names);
        }
        for (i, n) in names.iter().enumerate() {
            if names[..i].contains(n) {
                return self.err_at(
                    "Duplicate parameter name not allowed in this context",
                    start,
                );
            }
            if *n == kw::AWAIT && is_async {
                return self.err_at(
                    "'await' is not a valid parameter name in an async function",
                    start,
                );
            }
        }
        let f = Func {
            name: None,
            params,
            rest,
            body,
            kind: FuncKind::Arrow,
            is_async,
            is_generator: false,
            strict: strict || self.ctx.strict,
            scope,
            name_scope: None,
            start,
            end,
            is_decl: false,
        };
        Ok(Expr {
            k: ExprKind::Func(Box::new(f)),
            pos: start,
        })
    }

    fn parse_conditional(&mut self, no_in: bool) -> PResult<Expr> {
        let pos = self.tok.start;
        let test = self.parse_binary(1, no_in)?;
        if !self.is(P::Question) {
            return Ok(test);
        }
        self.check_no_cover_init()?;
        self.advance()?;
        let cons = self.parse_assign(false)?;
        self.expect(P::Colon)?;
        let alt = self.parse_assign(no_in)?;
        Ok(Expr {
            k: ExprKind::Cond(Box::new(test), Box::new(cons), Box::new(alt)),
            pos,
        })
    }

    fn binop(&self, no_in: bool) -> Option<(u8, Result<BinOp, LogicOp>)> {
        let r = match &self.tok.tok {
            Tok::P(p) => match p {
                P::Nullish => (1, Err(LogicOp::Nullish)),
                P::OrOr => (2, Err(LogicOp::Or)),
                P::AndAnd => (3, Err(LogicOp::And)),
                P::Pipe => (4, Ok(BinOp::BitOr)),
                P::Caret => (5, Ok(BinOp::BitXor)),
                P::Amp => (6, Ok(BinOp::BitAnd)),
                P::Eq => (7, Ok(BinOp::Eq)),
                P::Ne => (7, Ok(BinOp::Ne)),
                P::StrictEq => (7, Ok(BinOp::StrictEq)),
                P::StrictNe => (7, Ok(BinOp::StrictNe)),
                P::Lt => (8, Ok(BinOp::Lt)),
                P::Gt => (8, Ok(BinOp::Gt)),
                P::Le => (8, Ok(BinOp::Le)),
                P::Ge => (8, Ok(BinOp::Ge)),
                P::Shl => (9, Ok(BinOp::Shl)),
                P::Shr => (9, Ok(BinOp::Shr)),
                P::UShr => (9, Ok(BinOp::UShr)),
                P::Plus => (10, Ok(BinOp::Add)),
                P::Minus => (10, Ok(BinOp::Sub)),
                P::Star => (11, Ok(BinOp::Mul)),
                P::Slash => (11, Ok(BinOp::Div)),
                P::Percent => (11, Ok(BinOp::Mod)),
                P::StarStar => (12, Ok(BinOp::Exp)),
                _ => return None,
            },
            Tok::Name(kw::INSTANCEOF, false) => (8, Ok(BinOp::InstanceOf)),
            Tok::Name(kw::IN, false) if !no_in => (8, Ok(BinOp::In)),
            _ => return None,
        };
        Some(r)
    }

    /// Precedence climbing over binary and logical operators (`**` is right-associative).
    /// Operators of precedence >= `min` bind here.
    fn parse_binary(&mut self, min: u8, no_in: bool) -> PResult<Expr> {
        let pos = self.tok.start;
        let mut left = self.parse_unary()?;
        while let Some((prec, op)) = self.binop(no_in) {
            if prec < min.max(1) {
                break;
            }
            self.check_no_cover_init()?;
            if prec == 12 && matches!(left.k, ExprKind::Unary(..) | ExprKind::Await(_)) {
                return self.err("Unary operator used immediately before exponentiation expression. Parenthesis must be used to disambiguate operator precedence");
            }
            self.advance()?;
            let right = self.parse_binary(if prec == 12 { 12 } else { prec + 1 }, no_in)?;
            left = match op {
                Ok(b) => Expr {
                    k: ExprKind::Bin(b, Box::new(left), Box::new(right)),
                    pos,
                },
                Err(l) => {
                    let mixes = |e: &Expr, nullish: bool| match &e.k {
                        ExprKind::Logic(LogicOp::Nullish, ..) => !nullish,
                        ExprKind::Logic(..) => nullish,
                        _ => false,
                    };
                    let nullish = l == LogicOp::Nullish;
                    if mixes(&left, nullish) || mixes(&right, nullish) {
                        return self.err("Cannot mix ?? with || or && without parentheses");
                    }
                    Expr {
                        k: ExprKind::Logic(l, Box::new(left), Box::new(right)),
                        pos,
                    }
                }
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        self.enter()?;
        let r = self.parse_unary_inner();
        self.leave();
        r
    }

    fn parse_unary_inner(&mut self) -> PResult<Expr> {
        let pos = self.tok.start;
        let op = match self.tok.tok {
            Tok::P(P::Minus) => Some(UnOp::Neg),
            Tok::P(P::Plus) => Some(UnOp::Plus),
            Tok::P(P::Bang) => Some(UnOp::Not),
            Tok::P(P::Tilde) => Some(UnOp::BitNot),
            Tok::Name(kw::TYPEOF, false) => Some(UnOp::Typeof),
            Tok::Name(kw::VOID, false) => Some(UnOp::Void),
            Tok::Name(kw::DELETE, false) => Some(UnOp::Delete),
            _ => None,
        };
        if let Some(op) = op {
            self.advance()?;
            let arg = self.parse_unary()?;
            if op == UnOp::Delete && self.ctx.strict && matches!(arg.k, ExprKind::Ident(_)) {
                return self.err_at("Delete of an unqualified identifier in strict mode.", pos);
            }
            return Ok(Expr {
                k: ExprKind::Unary(op, Box::new(arg)),
                pos,
            });
        }
        if self.is(P::Inc) || self.is(P::Dec) {
            let inc = self.is(P::Inc);
            self.advance()?;
            let arg = self.parse_unary()?;
            self.check_simple_target(&arg)?;
            return Ok(Expr {
                k: ExprKind::Update {
                    inc,
                    prefix: true,
                    target: Box::new(arg),
                },
                pos,
            });
        }
        if self.is_name(kw::AWAIT) && (self.ctx.in_async) {
            self.advance()?;
            let arg = self.parse_unary()?;
            return Ok(Expr {
                k: ExprKind::Await(Box::new(arg)),
                pos,
            });
        }
        let e = self.parse_postfix()?;
        Ok(e)
    }

    fn check_simple_target(&self, e: &Expr) -> PResult<()> {
        match &e.k {
            ExprKind::Ident(a) => {
                if self.ctx.strict && (*a == kw::EVAL || *a == kw::ARGUMENTS) {
                    return self.err_at("Unexpected eval or arguments in strict mode", e.pos);
                }
                Ok(())
            }
            ExprKind::Member(..)
            | ExprKind::Index(..)
            | ExprKind::SuperMember(_)
            | ExprKind::SuperIndex(_) => Ok(()),
            _ => self.err_at(
                "Invalid left-hand side expression in prefix/postfix operation",
                e.pos,
            ),
        }
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let pos = self.tok.start;
        let e = self.parse_lhs_expression()?;
        if (self.is(P::Inc) || self.is(P::Dec)) && !self.tok.nl_before {
            self.check_simple_target(&e)?;
            let inc = self.is(P::Inc);
            self.advance()?;
            return Ok(Expr {
                k: ExprKind::Update {
                    inc,
                    prefix: false,
                    target: Box::new(e),
                },
                pos,
            });
        }
        Ok(e)
    }

    /// Member / call / new expressions, with optional chains.
    fn parse_lhs_expression(&mut self) -> PResult<Expr> {
        let pos = self.tok.start;
        let mut e = if self.is_name(kw::NEW) {
            self.parse_new()?
        } else if self.is_name(kw::SUPER) {
            self.parse_super()?
        } else if self.is_name(kw::IMPORT) {
            return self.err(
                "import() and import.meta are not supported (ES modules are not supported yet)",
            );
        } else {
            self.parse_primary()?
        };
        let mut in_chain = false;
        loop {
            match self.tok.tok {
                Tok::P(P::Dot) => {
                    self.advance()?;
                    let name = self.member_name()?;
                    e = Expr {
                        k: ExprKind::Member(Box::new(e), name, false),
                        pos,
                    };
                }
                Tok::P(P::QDot) => {
                    self.advance()?;
                    in_chain = true;
                    if self.is(P::LParen) {
                        let args = self.parse_args()?;
                        e = Expr {
                            k: ExprKind::Call(Box::new(e), args, true),
                            pos,
                        };
                    } else if self.is(P::LBracket) {
                        self.advance()?;
                        let idx = self.parse_expression(false)?;
                        self.expect(P::RBracket)?;
                        e = Expr {
                            k: ExprKind::Index(Box::new(e), Box::new(idx), true),
                            pos,
                        };
                    } else if let Tok::Template(..) = self.tok.tok {
                        return self.err("Invalid tagged template on optional chain");
                    } else {
                        let name = self.member_name()?;
                        e = Expr {
                            k: ExprKind::Member(Box::new(e), name, true),
                            pos,
                        };
                    }
                }
                Tok::P(P::LBracket) => {
                    self.advance()?;
                    let idx = self.parse_expression(false)?;
                    self.expect(P::RBracket)?;
                    e = Expr {
                        k: ExprKind::Index(Box::new(e), Box::new(idx), false),
                        pos,
                    };
                }
                Tok::P(P::LParen) => {
                    let args = self.parse_args()?;
                    e = Expr {
                        k: ExprKind::Call(Box::new(e), args, false),
                        pos,
                    };
                }
                Tok::Template(..) => {
                    if in_chain {
                        return self.err("Invalid tagged template on optional chain");
                    }
                    let (parts, exprs) = self.parse_template(true)?;
                    e = Expr {
                        k: ExprKind::Tagged(Box::new(e), parts, exprs),
                        pos,
                    };
                }
                _ => break,
            }
        }
        if in_chain {
            e = Expr {
                k: ExprKind::OptChain(Box::new(e)),
                pos,
            };
        }
        Ok(e)
    }

    fn member_name(&mut self) -> PResult<Atom> {
        match self.tok.tok {
            Tok::Name(a, _) => {
                self.advance()?;
                Ok(a)
            }
            Tok::Private(_) => self.err("Private class members (#name) are not supported"),
            _ => self.unexpected(),
        }
    }

    fn parse_super(&mut self) -> PResult<Expr> {
        let pos = self.tok.start;
        self.advance()?;
        if self.is(P::LParen) {
            if !self.ctx.allow_super_call {
                return self.err_at("'super' keyword unexpected here", pos);
            }
            let args = self.parse_args()?;
            return Ok(Expr {
                k: ExprKind::SuperCall(args),
                pos,
            });
        }
        if !self.ctx.allow_super_prop {
            return self.err_at("'super' keyword unexpected here", pos);
        }
        if self.eat(P::Dot)? {
            let name = self.member_name()?;
            return Ok(Expr {
                k: ExprKind::SuperMember(name),
                pos,
            });
        }
        if self.eat(P::LBracket)? {
            let idx = self.parse_expression(false)?;
            self.expect(P::RBracket)?;
            return Ok(Expr {
                k: ExprKind::SuperIndex(Box::new(idx)),
                pos,
            });
        }
        self.err_at("'super' keyword unexpected here", pos)
    }

    fn parse_new(&mut self) -> PResult<Expr> {
        let pos = self.tok.start;
        self.enter()?;
        let r = (|| {
            self.advance()?; // new
            if self.is(P::Dot) {
                self.advance()?;
                if !self.is_name(kw::TARGET) {
                    return self.unexpected();
                }
                if !self.ctx.allow_new_target && !self.ctx.in_function {
                    return self.err_at("new.target expression is not allowed here", pos);
                }
                self.advance()?;
                return Ok(Expr {
                    k: ExprKind::NewTarget,
                    pos,
                });
            }
            // The callee: a member expression without calls (`new a.b.c(args)`).
            let mut callee = if self.is_name(kw::NEW) {
                self.parse_new()?
            } else if self.is_name(kw::SUPER) {
                self.parse_super()?
            } else if self.is_name(kw::IMPORT) {
                return self.err("import() is not supported");
            } else {
                self.parse_primary()?
            };
            loop {
                match self.tok.tok {
                    Tok::P(P::Dot) => {
                        self.advance()?;
                        let name = self.member_name()?;
                        callee = Expr {
                            k: ExprKind::Member(Box::new(callee), name, false),
                            pos,
                        };
                    }
                    Tok::P(P::LBracket) => {
                        self.advance()?;
                        let idx = self.parse_expression(false)?;
                        self.expect(P::RBracket)?;
                        callee = Expr {
                            k: ExprKind::Index(Box::new(callee), Box::new(idx), false),
                            pos,
                        };
                    }
                    Tok::Template(..) => {
                        let (parts, exprs) = self.parse_template(true)?;
                        callee = Expr {
                            k: ExprKind::Tagged(Box::new(callee), parts, exprs),
                            pos,
                        };
                    }
                    Tok::P(P::QDot) => {
                        return self.err("Invalid optional chain from new expression");
                    }
                    _ => break,
                }
            }
            let args = if self.is(P::LParen) {
                self.parse_args()?
            } else {
                Vec::new()
            };
            Ok(Expr {
                k: ExprKind::New(Box::new(callee), args),
                pos,
            })
        })();
        self.leave();
        r
    }

    fn parse_args(&mut self) -> PResult<Vec<Arg>> {
        self.expect(P::LParen)?;
        let mut args = Vec::new();
        while !self.is(P::RParen) {
            if self.eat(P::Ellipsis)? {
                args.push(Arg::Spread(self.parse_assign(false)?));
            } else {
                args.push(Arg::Item(self.parse_assign(false)?));
            }
            if !self.is(P::RParen) {
                self.expect(P::Comma)?;
            }
        }
        self.advance()?;
        Ok(args)
    }

    fn parse_template(&mut self, tagged: bool) -> PResult<(Box<TemplateParts>, Vec<Expr>)> {
        let mut parts = TemplateParts {
            cooked: Vec::new(),
            raw: Vec::new(),
        };
        let mut exprs = Vec::new();
        loop {
            let Tok::Template(cooked, raw, tail) = core::mem::replace(&mut self.tok.tok, Tok::Eof)
            else {
                return self.unexpected();
            };
            if cooked.is_none() && !tagged {
                return self.err("Invalid escape sequence in template");
            }
            parts.cooked.push(cooked);
            parts.raw.push(raw);
            if tail {
                self.advance()?;
                break;
            }
            self.advance()?;
            exprs.push(self.parse_expression(false)?);
            if !self.is(P::RBrace) {
                return self.unexpected();
            }
            // Resume the template after the `}`.
            self.lx.pos = self.tok.start as usize + 1;
            let start = self.tok.start;
            let t = self.lx.scan_template()?;
            self.tok = Token {
                tok: t,
                start,
                end: self.lx.pos as u32,
                nl_before: false,
            };
        }
        Ok((Box::new(parts), exprs))
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let pos = self.tok.start;
        let k = match &self.tok.tok {
            Tok::Num(n) => {
                let n = *n;
                if self.ctx.strict {
                    let s = &self.lx.src[self.tok.start as usize..self.tok.end as usize];
                    if s.len() > 1 && s[0] == b'0' && s[1].is_ascii_digit() {
                        return self.err("Octal literals are not allowed in strict mode.");
                    }
                }
                self.advance()?;
                ExprKind::Num(n)
            }
            Tok::Str(s, legacy) => {
                if *legacy && self.ctx.strict {
                    return self.err("Octal escape sequences are not allowed in strict mode.");
                }
                let s = s.clone();
                self.advance()?;
                ExprKind::Str(s)
            }
            Tok::Template(..) => {
                let (parts, exprs) = self.parse_template(false)?;
                ExprKind::Template(parts, exprs)
            }
            Tok::P(P::Slash) | Tok::P(P::SlashEq) => {
                let t = self.lx.scan_regex(self.tok.start)?;
                self.tok = Token {
                    tok: t,
                    start: pos,
                    end: self.lx.pos as u32,
                    nl_before: self.tok.nl_before,
                };
                let Tok::Regex(p, f) = core::mem::replace(&mut self.tok.tok, Tok::Eof) else {
                    return self.unexpected();
                };
                self.advance()?;
                ExprKind::Regex(p, f)
            }
            Tok::P(P::LParen) => return self.parse_paren(),
            Tok::P(P::LBracket) => return self.parse_array_literal(),
            Tok::P(P::LBrace) => return self.parse_object_literal(),
            Tok::Name(a, escaped) => {
                let (a, escaped) = (*a, *escaped);
                match a {
                    kw::THIS if !escaped => {
                        self.advance()?;
                        ExprKind::This
                    }
                    kw::NULL if !escaped => {
                        self.advance()?;
                        ExprKind::Null
                    }
                    kw::TRUE if !escaped => {
                        self.advance()?;
                        ExprKind::Bool(true)
                    }
                    kw::FALSE if !escaped => {
                        self.advance()?;
                        ExprKind::Bool(false)
                    }
                    kw::FUNCTION if !escaped => {
                        let f = self.parse_function(false, false, pos)?;
                        ExprKind::Func(f)
                    }
                    kw::CLASS if !escaped => {
                        let c = self.parse_class(false)?;
                        ExprKind::Class(c)
                    }
                    kw::ASYNC if !escaped => {
                        let p = self.peek()?;
                        if matches!(p.tok, Tok::Name(kw::FUNCTION, false)) && !p.nl_before {
                            self.advance()?;
                            let f = self.parse_function(false, true, pos)?;
                            ExprKind::Func(f)
                        } else if matches!(p.tok, Tok::P(P::LParen)) && !p.nl_before {
                            // async(...) — a call, or async arrow parameters.
                            self.advance()?;
                            return self.parse_async_paren(pos);
                        } else {
                            self.advance()?;
                            ExprKind::Ident(kw::ASYNC)
                        }
                    }
                    kw::NEW => return self.parse_new(),
                    _ => match self.ident_name() {
                        Some(a) => {
                            self.advance()?;
                            ExprKind::Ident(a)
                        }
                        None => {
                            if a == kw::AWAIT && self.ctx.in_class_field {
                                return self
                                    .err("'await' is not allowed in class field initializers");
                            }
                            return self.unexpected();
                        }
                    },
                }
            }
            Tok::Private(_) => return self.err("Private class members (#name) are not supported"),
            _ => return self.unexpected(),
        };
        Ok(Expr { k, pos })
    }

    fn parse_array_literal(&mut self) -> PResult<Expr> {
        let pos = self.tok.start;
        self.advance()?;
        let mut items = Vec::new();
        loop {
            if self.eat(P::RBracket)? {
                break;
            }
            if self.is(P::Comma) {
                self.advance()?;
                items.push(ArrElem::Hole);
                continue;
            }
            if self.eat(P::Ellipsis)? {
                items.push(ArrElem::Spread(self.parse_assign(false)?));
            } else {
                items.push(ArrElem::Item(self.parse_assign(false)?));
            }
            if !self.is(P::RBracket) {
                self.expect(P::Comma)?;
            }
        }
        Ok(Expr {
            k: ExprKind::Array(items),
            pos,
        })
    }

    fn parse_object_literal(&mut self) -> PResult<Expr> {
        let pos = self.tok.start;
        self.advance()?;
        let mut props = Vec::new();
        let mut has_proto = false;
        loop {
            if self.eat(P::RBrace)? {
                break;
            }
            if self.eat(P::Ellipsis)? {
                props.push(PropDef::Spread(self.parse_assign(false)?));
            } else {
                let ppos = self.tok.start;
                let mut is_async = false;
                let mut is_gen = false;
                let mut acc: Option<MethodKind> = None;
                if self.is_name(kw::ASYNC) {
                    let p = self.peek()?;
                    if !p.nl_before
                        && !matches!(
                            p.tok,
                            Tok::P(P::LParen)
                                | Tok::P(P::Colon)
                                | Tok::P(P::Comma)
                                | Tok::P(P::RBrace)
                                | Tok::P(P::Assign)
                        )
                    {
                        self.advance()?;
                        is_async = true;
                    }
                }
                if self.eat(P::Star)? {
                    is_gen = true;
                }
                if !is_async && !is_gen && (self.is_name(kw::GET) || self.is_name(kw::SET)) {
                    let p = self.peek()?;
                    if !matches!(
                        p.tok,
                        Tok::P(P::LParen)
                            | Tok::P(P::Colon)
                            | Tok::P(P::Comma)
                            | Tok::P(P::RBrace)
                            | Tok::P(P::Assign)
                    ) {
                        acc = Some(if self.is_name(kw::GET) {
                            MethodKind::Get
                        } else {
                            MethodKind::Set
                        });
                        self.advance()?;
                    }
                }
                let shorthand = self.ident_name();
                let name_tok_is_name = matches!(self.tok.tok, Tok::Name(..));
                let key = self.parse_prop_name()?;
                if self.is(P::LParen) {
                    let fk = match acc {
                        Some(MethodKind::Get) => FuncKind::Getter,
                        Some(MethodKind::Set) => FuncKind::Setter,
                        _ => FuncKind::Method,
                    };
                    let f = self.parse_function_rest(fk, is_async, is_gen, ppos)?;
                    props.push(PropDef::Method(key, acc.unwrap_or(MethodKind::Method), f));
                } else if is_async || is_gen || acc.is_some() {
                    return self.unexpected();
                } else if self.eat(P::Colon)? {
                    let v = self.parse_assign(false)?;
                    let is_proto = match &key {
                        PropName::Ident(kw::PROTO) => true,
                        PropName::Str(s) => crate::string::eq_str(s, "__proto__"),
                        _ => false,
                    };
                    if is_proto {
                        if has_proto {
                            // Duplicate __proto__ is an error unless this becomes a pattern;
                            // keep it simple and report it.
                            return self.err_at(
                                "Duplicate __proto__ fields are not allowed in object literals",
                                ppos,
                            );
                        }
                        has_proto = true;
                        props.push(PropDef::Proto(v));
                    } else {
                        props.push(PropDef::KeyValue(key, v));
                    }
                } else if name_tok_is_name {
                    let Some(a) = shorthand else {
                        return self.unexpected();
                    };
                    if self.is(P::Assign) {
                        self.advance()?;
                        let d = self.parse_assign(false)?;
                        if self.cover_init_pos.is_none() {
                            self.cover_init_pos = Some(ppos);
                        }
                        props.push(PropDef::CoverInit(a, ppos, d));
                    } else {
                        props.push(PropDef::Shorthand(a, ppos));
                    }
                } else {
                    return self.unexpected();
                }
            }
            if !self.is(P::RBrace) {
                self.expect(P::Comma)?;
            }
        }
        Ok(Expr {
            k: ExprKind::Object(props),
            pos,
        })
    }

    /// `( ... )`: a parenthesized expression or arrow parameters.
    fn parse_paren(&mut self) -> PResult<Expr> {
        let pos = self.tok.start;
        let cover = self.parse_paren_cover()?;
        if self.is(P::Arrow) && !self.tok.nl_before {
            self.advance()?;
            let (params, rest) = self.cover_to_params(cover)?;
            return self.parse_arrow_body(params, rest, false, pos);
        }
        if cover.rest.is_some() || cover.items.is_empty() || cover.trailing_comma {
            return self.err_at("Unexpected token ')'", self.prev_end.saturating_sub(1));
        }
        self.check_no_cover_init()?;
        // A one-item Seq marks the parentheses (they end an optional chain, and make an
        // object or array literal a plain expression rather than a pattern).
        Ok(Expr {
            k: ExprKind::Seq(cover.items),
            pos,
        })
    }

    fn parse_paren_cover(&mut self) -> PResult<ParenCover> {
        let pos = self.tok.start;
        self.expect(P::LParen)?;
        let mut items = Vec::new();
        let mut rest = None;
        let mut trailing_comma = false;
        while !self.is(P::RParen) {
            if self.eat(P::Ellipsis)? {
                rest = Some(self.parse_binding_target_as_expr()?);
                if !self.is(P::RParen) {
                    return self.unexpected();
                }
                break;
            }
            let saved_defer = self.defer_cover_at;
            self.defer_cover_at = self.depth + 1;
            let item = self.parse_assign(false);
            self.defer_cover_at = saved_defer;
            items.push(item?);
            if self.is(P::Comma) {
                self.advance()?;
                if self.is(P::RParen) {
                    trailing_comma = true;
                }
            } else if !self.is(P::RParen) {
                return self.unexpected();
            }
        }
        self.advance()?;
        Ok(ParenCover {
            items,
            rest,
            trailing_comma,
            pos,
        })
    }

    /// `...rest` in a cover: parse a binding target, wrapped as an expression.
    fn parse_binding_target_as_expr(&mut self) -> PResult<Expr> {
        let pos = self.tok.start;
        let p = self.parse_binding_target()?;
        Ok(Expr {
            k: ExprKind::Spread(Box::new(pat_to_placeholder(p, pos))),
            pos,
        })
    }

    fn cover_to_params(&mut self, cover: ParenCover) -> PResult<(Vec<Param>, Option<Pat>)> {
        self.cover_init_pos = None;
        let mut params = Vec::new();
        for e in cover.items {
            match e.k {
                ExprKind::Assign(AssignOp::Assign, target, value) => {
                    let pat = self.check_binding_pat(*target)?;
                    params.push(Param {
                        pat,
                        default: Some(*value),
                    });
                }
                _ => {
                    let pat = self.expr_to_pattern(e, true)?;
                    params.push(Param { pat, default: None });
                }
            }
        }
        let rest = match cover.rest {
            Some(Expr {
                k: ExprKind::Spread(inner),
                ..
            }) => Some(placeholder_to_pat(*inner)),
            _ => None,
        };
        let _ = cover.pos;
        Ok((params, rest))
    }

    /// `async (` already at `(`: an async arrow's parameters, or a call of `async`.
    fn parse_async_paren(&mut self, pos: u32) -> PResult<Expr> {
        let saved = self.ctx.in_async;
        let cover = self.parse_paren_cover();
        self.ctx.in_async = saved;
        let cover = cover?;
        if self.is(P::Arrow) && !self.tok.nl_before {
            self.advance()?;
            let (params, rest) = self.cover_to_params(cover)?;
            return self.parse_arrow_body(params, rest, true, pos);
        }
        // A call: async(a, b, ...c)
        self.check_no_cover_init()?;
        let mut args: Vec<Arg> = cover.items.into_iter().map(Arg::Item).collect();
        if let Some(Expr {
            k: ExprKind::Spread(inner),
            pos: rp,
        }) = cover.rest
        {
            let p = placeholder_to_pat(*inner);
            match p {
                Pat::Ident(a, ip) => args.push(Arg::Spread(Expr {
                    k: ExprKind::Ident(a),
                    pos: ip,
                })),
                _ => return self.err_at("Unexpected token", rp),
            }
        }
        let callee = Expr {
            k: ExprKind::Ident(kw::ASYNC),
            pos,
        };
        Ok(Expr {
            k: ExprKind::Call(Box::new(callee), args, false),
            pos,
        })
    }

    fn check_binding_pat(&mut self, p: Pat) -> PResult<Pat> {
        match p {
            Pat::Expr(e) => self.expr_to_pattern(*e, true),
            Pat::Ident(a, pos) => {
                if self.ctx.strict && (a == kw::EVAL || a == kw::ARGUMENTS) {
                    return self.err_at("Unexpected eval or arguments in strict mode", pos);
                }
                Ok(Pat::Ident(a, pos))
            }
            Pat::Array(elems, rest) => {
                let mut out = Vec::new();
                for el in elems {
                    match el {
                        None => out.push(None),
                        Some(PatElem { pat, default }) => out.push(Some(PatElem {
                            pat: self.check_binding_pat(pat)?,
                            default,
                        })),
                    }
                }
                let rest = match rest {
                    Some(r) => Some(Box::new(self.check_binding_pat(*r)?)),
                    None => None,
                };
                Ok(Pat::Array(out, rest))
            }
            Pat::Object(props, rest) => {
                let mut out = Vec::new();
                for p in props {
                    out.push(PatProp {
                        key: p.key,
                        pat: self.check_binding_pat(p.pat)?,
                        default: p.default,
                    });
                }
                let rest = match rest {
                    Some(r) => Some(Box::new(self.check_binding_pat(*r)?)),
                    None => None,
                };
                Ok(Pat::Object(out, rest))
            }
        }
    }

    /// Reinterprets an expression as an assignment target (or, with `binding`, a binding
    /// pattern: only identifiers at the leaves).
    fn expr_to_pattern(&mut self, e: Expr, binding: bool) -> PResult<Pat> {
        self.enter()?;
        let r = self.expr_to_pattern_inner(e, binding);
        self.leave();
        r
    }

    fn expr_to_pattern_inner(&mut self, e: Expr, binding: bool) -> PResult<Pat> {
        let pos = e.pos;
        match e.k {
            ExprKind::Ident(a) => {
                if self.ctx.strict && (a == kw::EVAL || a == kw::ARGUMENTS) {
                    return self.err_at("Unexpected eval or arguments in strict mode", pos);
                }
                Ok(Pat::Ident(a, pos))
            }
            ExprKind::Member(..)
            | ExprKind::Index(..)
            | ExprKind::SuperMember(_)
            | ExprKind::SuperIndex(_)
                if !binding =>
            {
                Ok(Pat::Expr(Box::new(e)))
            }
            ExprKind::Seq(mut items) if items.len() == 1 && !binding => {
                // A parenthesized simple target is fine: `(a) = 1`, `(a.b) = 1`.
                let inner = items.pop().unwrap_or(Expr {
                    k: ExprKind::Null,
                    pos,
                });
                match inner.k {
                    ExprKind::Ident(_) | ExprKind::Member(..) | ExprKind::Index(..) => {
                        self.expr_to_pattern(inner, false)
                    }
                    _ => self.err_at("Invalid destructuring assignment target", pos),
                }
            }
            ExprKind::Array(items) => {
                let n = items.len();
                let mut elems = Vec::new();
                let mut rest = None;
                for (i, it) in items.into_iter().enumerate() {
                    match it {
                        ArrElem::Hole => elems.push(None),
                        ArrElem::Item(x) => elems.push(Some(self.elem_to_pat(x, binding)?)),
                        ArrElem::Spread(x) => {
                            if i + 1 != n {
                                return self.err_at("Rest element must be last element", pos);
                            }
                            if let ExprKind::Assign(..) = x.k {
                                return self.err_at(
                                    "Rest element may not have a default initializer",
                                    pos,
                                );
                            }
                            rest = Some(Box::new(self.expr_to_pattern(x, binding)?));
                        }
                    }
                }
                Ok(Pat::Array(elems, rest))
            }
            ExprKind::Object(props) => {
                let n = props.len();
                let mut out = Vec::new();
                let mut rest = None;
                for (i, p) in props.into_iter().enumerate() {
                    match p {
                        PropDef::KeyValue(key, v) => {
                            let el = self.elem_to_pat(v, binding)?;
                            out.push(PatProp {
                                key,
                                pat: el.pat,
                                default: el.default,
                            });
                        }
                        PropDef::Proto(v) => {
                            let el = self.elem_to_pat(v, binding)?;
                            out.push(PatProp {
                                key: PropName::Ident(kw::PROTO),
                                pat: el.pat,
                                default: el.default,
                            });
                        }
                        PropDef::Shorthand(a, p) => {
                            out.push(PatProp {
                                key: PropName::Ident(a),
                                pat: Pat::Ident(a, p),
                                default: None,
                            });
                        }
                        PropDef::CoverInit(a, p, d) => {
                            out.push(PatProp {
                                key: PropName::Ident(a),
                                pat: Pat::Ident(a, p),
                                default: Some(d),
                            });
                        }
                        PropDef::Spread(x) => {
                            if i + 1 != n {
                                return self.err_at("Rest element must be last element", pos);
                            }
                            rest = Some(Box::new(self.expr_to_pattern(x, binding)?));
                        }
                        PropDef::Method(..) => {
                            return self.err_at("Invalid destructuring assignment target", pos);
                        }
                    }
                }
                self.cover_init_pos = None;
                Ok(Pat::Object(out, rest))
            }
            ExprKind::Assign(AssignOp::Assign, _, _) => {
                self.err_at("Invalid destructuring assignment target", pos)
            }
            _ => self.err_at(
                if binding {
                    "Invalid destructuring target"
                } else {
                    "Invalid left-hand side in assignment"
                },
                pos,
            ),
        }
    }

    fn elem_to_pat(&mut self, x: Expr, binding: bool) -> PResult<PatElem> {
        if let ExprKind::Assign(AssignOp::Assign, target, value) = x.k {
            let pat = if binding {
                self.check_binding_pat(*target)?
            } else {
                *target
            };
            return Ok(PatElem {
                pat,
                default: Some(*value),
            });
        }
        Ok(PatElem {
            pat: self.expr_to_pattern(x, binding)?,
            default: None,
        })
    }
}

/// A binding pattern smuggled through an expression slot (for `...rest` in a paren cover).
fn pat_to_placeholder(p: Pat, pos: u32) -> Expr {
    match p {
        Pat::Ident(a, ip) => Expr {
            k: ExprKind::Ident(a),
            pos: ip,
        },
        other => Expr {
            k: ExprKind::Assign(
                AssignOp::Logic(LogicOp::Nullish),
                Box::new(other),
                Box::new(Expr {
                    k: ExprKind::Null,
                    pos,
                }),
            ),
            pos,
        },
    }
}

fn placeholder_to_pat(e: Expr) -> Pat {
    match e.k {
        ExprKind::Ident(a) => Pat::Ident(a, e.pos),
        ExprKind::Assign(_, p, _) => *p,
        _ => Pat::Ident(kw::UNDEFINED, e.pos),
    }
}

pub(crate) fn collect_pat_names(p: &Pat, out: &mut Vec<Atom>) {
    match p {
        Pat::Ident(a, _) => out.push(*a),
        Pat::Expr(_) => {}
        Pat::Array(elems, rest) => {
            for e in elems.iter().flatten() {
                collect_pat_names(&e.pat, out);
            }
            if let Some(r) = rest {
                collect_pat_names(r, out);
            }
        }
        Pat::Object(props, rest) => {
            for p in props {
                collect_pat_names(&p.pat, out);
            }
            if let Some(r) = rest {
                collect_pat_names(r, out);
            }
        }
    }
}
