//! From source text to running code: parse, analyse scopes, compile, run. Syntax errors
//! become SyntaxError objects that point at the source position.

use alloc::rc::Rc;

use crate::api::{ErrorKind, ErrorReport, Exc, Exception, ScriptOptions};
use crate::ast::{Body, ExprKind, StmtKind};
use crate::bytecode::{Code, Source};
use crate::atoms::Atoms;
use crate::compiler::{Compiler, Parts};
use crate::errors::ErrInfo;
use crate::object::ObjKind;
use crate::parser::Parser;
use crate::realm::{Realm, Res};
use crate::resolve::{Resolver, Scopes};
use crate::value::Val;

impl Realm {
    pub(crate) fn syntax_error_at(&mut self, src: &Source, pos: u32, msg: &str) -> Exception {
        let (line, col) = src.line_col(pos);
        let e = self.make_error(ErrorKind::SyntaxError, msg);
        if let ObjKind::Error(info) = &mut self.om(e).kind {
            **info = ErrInfo {
                file: src.file.clone(),
                line,
                col,
                frames: alloc::format!("    at {}:{}:{}", src.file, line, col),
            };
        }
        let stack = alloc::format!("SyntaxError: {msg}\n    at {}:{}:{}", src.file, line, col);
        let sk = self.key(crate::names::N::stack);
        let sv = self.str_val(&stack);
        self.put_prop(e, sk, sv, crate::object::WC);
        Exception(Exc::Throw(Val::Obj(e)))
    }

    /// Parses and compiles a script. Its top-level statements are parsed, checked and
    /// compiled one after the other, each tree dropped before the next is read, so only the
    /// biggest statement (not the whole script) has to fit the compile allowance.
    pub(crate) fn compile_source(
        &mut self,
        source: &str,
        file: &str,
        line_delta: i32,
        first_col: u32,
    ) -> Res<Rc<Code>> {
        if source.len() > self.limits.max_source_bytes {
            return Err(self.range_error("The script is too large"));
        }
        let text: Rc<[u8]> = Rc::from(source.as_bytes());
        let src = Rc::new(Source::new(
            text.clone(),
            file,
            line_delta,
            first_col,
            self.heap.code_acct.clone(),
        ));
        let mut p = Parser::new(&text);
        p.stack = Some(self.stack);
        p.mem_limit = self.compile_limit();
        p.lazy_min = crate::lazy::lazy_min_bytes();
        if let Err(e) = p.script_begin() {
            return Err(self.syntax_error_at(&src, e.pos, &e.msg));
        }
        let shared = Rc::new(crate::lazy::LazyShared {
            atoms: core::cell::RefCell::new(None),
            default_atom: None,
        });
        let mut parts = Parts::new(Scopes::new(0, 1), shared.clone());
        // The script scope, and the code's start.
        {
            let atoms = core::mem::replace(&mut p.atoms, Atoms::empty());
            let mut c = Compiler::from_parts(self, &atoms, src.clone(), core::mem::take(&mut parts));
            let r = c.begin_script();
            parts = c.into_parts();
            p.atoms = atoms;
            if let Err(e) = r {
                return Err(self.syntax_error_at(&src, e.pos, &e.msg));
            }
        }
        parts.scopes[0].kind = crate::resolve::SK::Script;
        let mut directives = true;
        loop {
            let mark = p.scope_mark();
            p.mem = 0;
            let st = match p.script_next(&mut directives) {
                Ok(Some(st)) => st,
                Ok(None) => break,
                Err(e) => return Err(self.syntax_error_at(&src, e.pos, &e.msg)),
            };
            let hi = p.scope_mark();
            let strict = p.is_strict();
            let atoms = core::mem::replace(&mut p.atoms, Atoms::empty());
            let mut res = Resolver::for_script_stmt(&atoms, core::mem::take(&mut parts.scopes));
            res.stack = Some(self.stack);
            let resolved = res.script_stmt(&st, strict, hi);
            parts.scopes = core::mem::take(&mut res.scopes);
            drop(res);
            let out = match resolved {
                Err(e) => Err((e.pos, e.msg)),
                Ok(()) => {
                    let mut c = Compiler::from_parts(
                        self,
                        &atoms,
                        src.clone(),
                        core::mem::take(&mut parts),
                    );
                    let r = c.script_stmt(&st, strict).map_err(|e| (e.pos, e.msg));
                    parts = c.into_parts();
                    r
                }
            };
            p.atoms = atoms;
            if let Err((pos, msg)) = out {
                return Err(self.syntax_error_at(&src, pos, &msg));
            }
            // The statement's tree has done its job.
            drop(st);
            parts.forget_scopes(mark);
            p.reset_scopes(mark);
        }
        let atoms = core::mem::replace(&mut p.atoms, Atoms::empty());
        drop(p);
        let compiled = {
            let mut c = Compiler::from_parts(self, &atoms, src.clone(), parts);
            c.end_script().map_err(|e| (e.pos, e.msg))
        };
        // Functions left for later keep the identifier table.
        if Rc::strong_count(&shared) > 1 {
            *shared.atoms.borrow_mut() = Some(atoms);
        }
        match compiled {
            Ok(code) => Ok(code),
            Err((pos, msg)) => Err(self.syntax_error_at(&src, pos, &msg)),
        }
    }

    pub(crate) fn eval_script(&mut self, source: &str, opts: &ScriptOptions<'_>) -> Res<Val> {
        let delta = opts.first_line as i32 - 1;
        let code = self.compile_source(source, opts.file, delta, opts.first_column.max(1))?;
        self.run_script_code(code)
    }

    pub(crate) fn syntax_check(&mut self, source: &str, file: &str) -> Result<(), ErrorReport> {
        match self.compile_source(source, file, 0, 1) {
            Ok(_) => Ok(()),
            Err(e) => Err(self.report(&e)),
        }
    }

    pub(crate) fn make_function_from_source(
        &mut self,
        params: &[&str],
        body: &str,
        file: &str,
    ) -> Res<Val> {
        self.make_function_kind(params, body, file, "function")
    }

    /// new Function / GeneratorFunction / AsyncFunction: the parameters and body must form
    /// exactly one function (no smuggling extra code past the wrapper).
    pub(crate) fn make_function_kind(
        &mut self,
        params: &[&str],
        body: &str,
        file: &str,
        kind: &str,
    ) -> Res<Val> {
        let p = params.join(",");
        let src = alloc::format!("({kind} anonymous({p}\n) {{\n{body}\n}})");
        // Check the shape first.
        {
            let text = src.as_bytes();
            let mut parser = Parser::new(text);
            parser.stack = Some(self.stack);
            let ok = match parser.parse_script() {
                Ok(script) => {
                    script.body.len() == 1
                        && match &script.body[0].k {
                            StmtKind::Expr(e) => match &e.k {
                                ExprKind::Seq(items) if items.len() == 1 => match &items[0].k {
                                    ExprKind::Func(f) => {
                                        f.end as usize == text.len() - 1
                                            && !matches!(f.body, Body::Expr(_))
                                    }
                                    _ => false,
                                },
                                _ => false,
                            },
                            _ => false,
                        }
                }
                Err(e) => {
                    let s = Source::new(
                        Rc::from(text),
                        file,
                        -2,
                        1,
                        self.heap.code_acct.clone(),
                    );
                    return Err(self.syntax_error_at(&s, e.pos, &e.msg));
                }
            };
            if !ok {
                return Err(self.syntax_error("Arg string terminates parameters early"));
            }
        }
        let code = self.compile_source(&src, file, -2, 1)?;
        self.run_script_code(code)
    }
}
