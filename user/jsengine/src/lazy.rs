//! Functions compiled on their first call. The parser checks every big function in full (so
//! syntax and scope errors still come when the script loads), notes which outer names it
//! uses and drops its tree; the compiler leaves a stub [`Code`] behind. The first call of a
//! closure made from the stub parses the function again from its source text, resolves it
//! against the environments around it (a [`ChainNode`] list the compiler saved), compiles it,
//! and the closure goes on with the real code. A page that loads megabytes of script and
//! runs a small part of it only ever compiles that part, and the parse tree of a huge script
//! never exists all at once.

use alloc::rc::Rc;
use alloc::string::String;
use core::cell::RefCell;

use crate::atoms::{Atom, Atoms};
use crate::bytecode::Code;
use crate::compiler::Compiler;
use crate::parser::{LazyHead, Parser};
use crate::realm::{Realm, Res};
use crate::resolve::{ChainNode, Resolver};

/// Functions at least this many source bytes long are left for their first call.
const LAZY_MIN_BYTES: u32 = 64;

/// The size from which functions are left for later. (The tests can set JSENGINE_LAZY_ALL to
/// make every function lazy and run the whole suite through that path.)
pub(crate) fn lazy_min_bytes() -> u32 {
    #[cfg(test)]
    if std::env::var_os("JSENGINE_LAZY_ALL").is_some() {
        return 0;
    }
    match LAZY_OVERRIDE.load(core::sync::atomic::Ordering::Relaxed) {
        u32::MAX => LAZY_MIN_BYTES,
        n => n,
    }
}

/// Tests: how many functions have been compiled on their first call.
#[cfg(test)]
pub(crate) static COMPILES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

static LAZY_OVERRIDE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(u32::MAX);

/// For tests: functions of at least this many source bytes are left for their first call
/// (`None`: the default). `Some(0)` makes every function lazy, `Some(u32::MAX - 1)` none.
#[doc(hidden)]
pub fn set_lazy_min_bytes(n: Option<u32>) {
    LAZY_OVERRIDE.store(
        n.map_or(u32::MAX, |n| n.min(u32::MAX - 1)),
        core::sync::atomic::Ordering::Relaxed,
    );
}

/// What the functions of one script (or module) have in common.
pub(crate) struct LazyShared {
    /// The identifier table of the script's parse, lent out while one function is parsed.
    pub atoms: RefCell<Option<Atoms>>,
    pub default_atom: Option<Atom>,
}

pub(crate) struct LazyCode {
    pub head: LazyHead,
    pub shared: Rc<LazyShared>,
    /// The environments around the function, innermost first.
    pub chain: Option<Rc<ChainNode>>,
    /// The compiler's `this` check state where the function was made (arrows in derived
    /// constructors).
    pub this_chk: bool,
    /// The compiled function, once there is one.
    pub done: RefCell<Option<Rc<Code>>>,
}

impl Realm {
    /// The real code of a stub, compiling it now if need be.
    pub(crate) fn force_lazy(&mut self, stub: &Rc<Code>) -> Res<Rc<Code>> {
        let Some(lazy) = stub.lazy.as_ref() else {
            return Ok(stub.clone());
        };
        if let Some(c) = lazy.done.borrow().as_ref() {
            return Ok(c.clone());
        }
        let Some(mut atoms) = lazy.shared.atoms.borrow_mut().take() else {
            // (Compiling a function cannot run script, so this cannot be reached again from
            // inside; refuse rather than guess.)
            return Err(self.range_error("Maximum call stack size exceeded"));
        };
        #[cfg(test)]
        COMPILES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let r = self.compile_lazy(stub, lazy, &mut atoms);
        *lazy.shared.atoms.borrow_mut() = Some(atoms);
        match r {
            Ok(c) => {
                *lazy.done.borrow_mut() = Some(c.clone());
                Ok(c)
            }
            Err((pos, msg)) => {
                if msg.contains("nested too deeply") {
                    Err(self.range_error("Maximum call stack size exceeded"))
                } else if msg.contains("too large") {
                    Err(self.range_error("The function is too large to compile"))
                } else {
                    Err(self.syntax_error_at(&stub.source, pos, &msg))
                }
            }
        }
    }

    fn compile_lazy(
        &mut self,
        stub: &Rc<Code>,
        lazy: &LazyCode,
        atoms: &mut Atoms,
    ) -> Result<Rc<Code>, (u32, String)> {
        let text = stub.source.text.clone();
        let mut depth = 0u32;
        let mut n = lazy.chain.clone();
        while let Some(c) = n {
            depth += 1;
            n = c.parent.clone();
        }
        let mut p = Parser::with_atoms(&text, core::mem::replace(atoms, Atoms::empty()));
        p.stack = Some(self.stack);
        p.mem_limit = self.compile_limit();
        p.lazy_min = lazy_min_bytes();
        p.set_first_scope(depth + 1);
        let parsed = p.parse_lazy(&lazy.head);
        let nscopes = p.scope_count();
        *atoms = core::mem::replace(&mut p.atoms, Atoms::empty());
        drop(p);
        let f = parsed.map_err(|e| (e.pos, e.msg))?;
        let (mut res, parent) = Resolver::for_lazy(atoms, nscopes, &lazy.chain);
        res.stack = Some(self.stack);
        res.lazy_function(&f, parent).map_err(|e| (e.pos, e.msg))?;
        let scopes = core::mem::take(&mut res.scopes);
        let mut c = Compiler::new(self, atoms, scopes, stub.source.clone());
        c.set_module(stub.module, lazy.shared.default_atom);
        c.set_lazy_shared(lazy.shared.clone());
        c.compile_lazy_function(&f, stub.name, parent, lazy.this_chk, stub.kind)
            .map_err(|e| (e.pos, e.msg))
    }

    /// Whether `pending` more bytes of code being built fit the heap budget (collecting
    /// first if that can be done here).
    pub(crate) fn code_fits(&mut self, pending: usize) -> bool {
        let max = self.heap.max_bytes;
        if self.heap.used().saturating_add(pending) <= max {
            return true;
        }
        if self.native_frames.is_empty() && self.frames_empty() {
            self.collect_garbage();
        }
        self.heap.live_bytes.saturating_add(pending) <= max
    }

    /// The most estimated bytes of syntax tree one parse may keep alive at once.
    pub(crate) fn compile_limit(&self) -> u64 {
        self.limits.max_compile_bytes as u64
    }
}

