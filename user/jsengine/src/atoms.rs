//! The parser's identifier table: names become small integers, with the keywords and the
//! contextual words the parser checks for at fixed numbers.

use alloc::boxed::Box;
use alloc::vec::Vec;

pub(crate) type Atom = u32;

macro_rules! predef {
    ($($id:ident = $s:expr),* $(,)?) => {
        #[allow(non_upper_case_globals, dead_code)]
        pub(crate) mod kw {
            use super::Atom;
            predef!(@consts 0u32; $($id),*);
        }
        const PREDEF: &[&str] = &[$($s),*];
    };
    (@consts $n:expr; $id:ident $(, $rest:ident)*) => {
        pub const $id: Atom = $n;
        predef!(@consts $n + 1u32; $($rest),*);
    };
    (@consts $n:expr;) => {};
}

predef! {
    BREAK = "break", CASE = "case", CATCH = "catch", CLASS = "class", CONST = "const",
    CONTINUE = "continue", DEBUGGER = "debugger", DEFAULT = "default", DELETE = "delete",
    DO = "do", ELSE = "else", EXPORT = "export", EXTENDS = "extends", FINALLY = "finally",
    FOR = "for", FUNCTION = "function", IF = "if", IMPORT = "import", IN = "in",
    INSTANCEOF = "instanceof", NEW = "new", RETURN = "return", SUPER = "super",
    SWITCH = "switch", THIS = "this", THROW = "throw", TRY = "try", TYPEOF = "typeof",
    VAR = "var", VOID = "void", WHILE = "while", WITH = "with", NULL = "null", TRUE = "true",
    FALSE = "false", ENUM = "enum",
    // The last reserved word above is ENUM. Contextual words:
    LET = "let", STATIC = "static", YIELD = "yield", AWAIT = "await", ASYNC = "async",
    GET = "get", SET = "set", OF = "of", ARGUMENTS = "arguments", EVAL = "eval",
    TARGET = "target", META = "meta", CONSTRUCTOR = "constructor", PROTO = "__proto__",
    UNDEFINED = "undefined", USE_STRICT = "use strict", PROTOTYPE = "prototype",
    IMPLEMENTS = "implements", INTERFACE = "interface", PACKAGE = "package",
    PRIVATE = "private", PROTECTED = "protected", PUBLIC = "public", AS = "as", FROM = "from",
    LENGTH = "length", NAME = "name",
}

/// Words that can never be identifiers.
pub(crate) fn is_reserved(a: Atom) -> bool {
    a <= kw::ENUM
}

/// Words reserved in strict mode only.
pub(crate) fn is_strict_reserved(a: Atom) -> bool {
    matches!(
        a,
        kw::LET
            | kw::STATIC
            | kw::YIELD
            | kw::IMPLEMENTS
            | kw::INTERFACE
            | kw::PACKAGE
            | kw::PRIVATE
            | kw::PROTECTED
            | kw::PUBLIC
    )
}

pub(crate) struct Atoms {
    strs: Vec<Box<str>>,
    table: Vec<u32>,
}

fn hash(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

impl Atoms {
    pub fn new() -> Atoms {
        let mut a = Atoms {
            strs: Vec::with_capacity(256),
            table: alloc::vec![u32::MAX; 512],
        };
        for s in PREDEF {
            a.intern(s);
        }
        a
    }

    pub fn intern(&mut self, s: &str) -> Atom {
        let mask = self.table.len() - 1;
        let mut i = hash(s) as usize & mask;
        loop {
            let e = self.table[i];
            if e == u32::MAX {
                break;
            }
            if &*self.strs[e as usize] == s {
                return e;
            }
            i = (i + 1) & mask;
        }
        let id = self.strs.len() as u32;
        self.strs.push(s.into());
        self.table[i] = id;
        if self.strs.len() * 2 > self.table.len() {
            let n = self.table.len() * 2;
            self.table = alloc::vec![u32::MAX; n];
            let mask = n - 1;
            for (id, s) in self.strs.iter().enumerate() {
                let mut i = hash(s) as usize & mask;
                while self.table[i] != u32::MAX {
                    i = (i + 1) & mask;
                }
                self.table[i] = id as u32;
            }
        }
        id
    }

    pub fn get(&self, a: Atom) -> &str {
        self.strs.get(a as usize).map_or("", |s| s)
    }
}
