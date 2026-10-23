//! From source text to running code: parse, analyse scopes, compile, run. Syntax errors
//! become SyntaxError objects that point at the source position.

use alloc::rc::Rc;
use alloc::string::String;

use crate::api::{ErrorKind, ErrorReport, Exc, Exception, ScriptOptions};
use crate::ast::{Body, ExprKind, StmtKind};
use crate::bytecode::{Code, Source};
use crate::compiler::Compiler;
use crate::errors::ErrInfo;
use crate::object::ObjKind;
use crate::parser::Parser;
use crate::realm::{Realm, Res};
use crate::resolve::Resolver;
use crate::value::Val;

impl Realm {
    fn syntax_error_at(&mut self, src: &Source, pos: u32, msg: &str) -> Exception {
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

    /// Parses and compiles a script.
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
        let src = Source {
            text: text.clone(),
            file: String::from(file),
            line_starts: crate::lexer::line_starts(&text),
            line_delta,
            first_col,
        };
        let mut p = Parser::new(&text);
        p.stack = Some(self.stack);
        let script = match p.parse_script() {
            Ok(s) => s,
            Err(e) => return Err(self.syntax_error_at(&src, e.pos, &e.msg)),
        };
        let atoms = core::mem::replace(&mut p.atoms, crate::atoms::Atoms::new());
        let nscopes = p.scope_count();
        let mut res = Resolver::new(&atoms, nscopes);
        res.stack = Some(self.stack);
        if let Err(e) = res.script(&script) {
            return Err(self.syntax_error_at(&src, e.pos, &e.msg));
        }
        let scopes = core::mem::take(&mut res.scopes);
        let src = Rc::new(src);
        let mut c = Compiler::new(self, &atoms, scopes, src.clone());
        match c.compile_script(&script) {
            Ok(code) => Ok(code),
            Err(e) => {
                let msg = e.msg;
                Err(self.syntax_error_at(&src, e.pos, &msg))
            }
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
                                            && matches!(f.body, Body::Stmts(_))
                                    }
                                    _ => false,
                                },
                                _ => false,
                            },
                            _ => false,
                        }
                }
                Err(e) => {
                    let s = Source {
                        text: Rc::from(text),
                        file: String::from(file),
                        line_starts: crate::lexer::line_starts(text),
                        line_delta: -2,
                        first_col: 1,
                    };
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
