//! Bytecode: the instructions of the stack VM and the compiled function (`Code`).
//!
//! Stack conventions: a call site pushes `this`, the callee, then the arguments; a frame's
//! locals start right after them. Operands are u32 indices: locals, constants (`Code::consts`),
//! jump targets (instruction indices), or an environment reference packed as
//! `depth << 24 | slot` (depth = how many environments up the chain).

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use crate::heap::Tracer;
use crate::value::{ObjId, StrId};

pub(crate) const ENV_SLOT_BITS: u32 = 24;

#[inline]
pub(crate) fn env_ref(depth: u32, slot: u32) -> u32 {
    (depth << ENV_SLOT_BITS) | slot
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Op {
    // constants
    Undef,
    Null,
    True,
    False,
    /// The "uninitialized" marker (TDZ).
    Empty,
    Int(i32),
    /// consts[i]: a number or a string.
    Const(u32),
    // stack
    Pop,
    Dup,
    /// a b → a b a b
    Dup2,
    Swap,
    /// a b c → c a b
    Rot3,
    /// a b c d → d a b c
    Rot4,
    // locals
    GetLocal(u32),
    /// Stores the top of stack (keeps it).
    SetLocal(u32),
    /// Stores and pops.
    PutLocal(u32),
    /// GetLocal with a TDZ check; the second operand is the name (a constant).
    GetLocalChk(u32, u32),
    /// SetLocal with a TDZ check.
    SetLocalChk(u32, u32),
    // environments
    GetEnv(u32),
    SetEnv(u32),
    PutEnv(u32),
    GetEnvChk(u32, u32),
    SetEnvChk(u32, u32),
    /// Reads an imported binding: the environment slot holds a reference to the exporting
    /// module's slot (TDZ-checked; the second operand is the name).
    GetImport(u32, u32),
    /// `import.meta` of the running module.
    ImportMeta,
    /// spec options → promise: `import(spec, options)`.
    ImportCall,
    /// A new environment of n slots (undefined) whose parent is the current one.
    PushEnv(u32),
    PopEnv,
    /// Replaces the current environment by a copy (a `for (let ...)` iteration).
    CopyEnv,
    /// Saves the current environment in a local / restores it (around try blocks).
    SaveEnv(u32),
    RestoreEnv(u32),
    // globals (operand: name constant)
    GetGlobal(u32),
    /// typeof of a global that may not exist.
    TypeofGlobal(u32),
    SetGlobal(u32),
    /// var / function declarations at script level.
    DeclareVar(u32),
    /// let / const / class at script level (TDZ until initialized); second: 1 = const.
    DeclareLex(u32, u32),
    /// Initializes a script-level lexical binding with the popped value.
    InitLex(u32),
    // properties
    GetProp(u32),
    /// obj → obj value (for method calls).
    GetPropKeep(u32),
    /// obj v → v
    SetProp(u32),
    /// obj key → v
    GetElem,
    /// obj key → obj v
    GetElemKeep,
    /// obj key v → v
    SetElem,
    DelProp(u32),
    DelElem,
    /// key obj → bool
    In,
    InstanceOf,
    /// super.name → v
    GetSuper(u32),
    /// key → v
    GetSuperElem,
    /// v → v
    SetSuper(u32),
    /// key v → v
    SetSuperElem,
    // calls
    /// this f a1..an → result
    Call(u32),
    /// this f array → result
    CallSpread,
    /// placeholder f a1..an → object
    New(u32),
    NewSpread,
    /// a1..an → (this is bound)
    SuperCall(u32),
    SuperCallSpread,
    // operators
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Gt,
    Le,
    Ge,
    Not,
    Neg,
    /// unary + (ToNumber)
    Plus,
    ToNumeric,
    BitNot,
    Typeof,
    Inc,
    Dec,
    ToPropKey,
    /// ToString (template literal substitutions).
    ToStr,
    /// Concatenates n strings.
    Concat(u32),
    // jumps (operand: target)
    Jump(u32),
    JumpIfFalse(u32),
    JumpIfTrue(u32),
    /// && : jump keeping the value if falsy, else pop.
    JumpIfFalseKeep(u32),
    /// || : jump keeping the value if truthy, else pop.
    JumpIfTrueKeep(u32),
    /// ?? : jump keeping the value if not nullish, else pop.
    JumpIfNotNullishKeep(u32),
    /// Optional chains: jump if nullish (nothing popped either way).
    JumpIfNullish(u32),
    /// Defaults: pop; jump if not undefined.
    JumpIfNotUndef(u32),
    // literals
    NewObject,
    NewArray(u32),
    /// arr v → arr
    ArrPush,
    /// arr → arr (a hole)
    ArrHole,
    /// arr iterable → arr
    ArrSpread,
    /// obj v → obj (CreateDataProperty)
    InitProp(u32),
    /// obj key v → obj
    InitElem,
    /// obj f → obj; second operand: 0 method, 1 getter, 2 setter, +4 = enumerable
    InitMethod(u32, u32),
    /// obj key f → obj
    InitMethodElem(u32),
    /// obj src → obj
    CopyProps,
    /// obj v → obj (`__proto__: v`)
    SetProtoLit,
    /// src keys → rest object
    ObjRest,
    /// f key → f (names an anonymous function from a computed key)
    SetFnName,
    // functions and classes
    Closure(u32),
    /// heritage? → ctor proto; operands: ctor code const, 1 if there is a heritage value
    MakeClass(u32, u32),
    /// ctor → ctor: attaches the instance field initializer (code const) with the computed
    /// keys array on the stack: ctor keys → ctor
    SetFieldInit(u32),
    /// ctor keys → ctor: runs the static initializer (code const)
    RunStaticInit(u32),
    /// Runs this function's class's instance field initializer on `this`.
    InitFields,
    /// The i-th computed field key of the running field initializer.
    FieldKey(u32),
    /// A fresh private name (a private symbol) described by the string constant.
    NewPrivName(u32),
    /// obj name → v (`obj.#x`)
    GetPriv,
    /// obj name → obj v (for calls)
    GetPrivKeep,
    /// obj name v → v (`obj.#x = v`)
    SetPriv,
    /// name obj → bool (`#x in obj`)
    PrivIn,
    /// obj name v → obj (a private field is added)
    DefinePrivField,
    /// obj name f → obj (a private method or accessor half is added: 0 method, 1 getter,
    /// 2 setter)
    DefinePrivMethod(u32),
    /// obj v → obj (DefineField: CreateDataPropertyOrThrow)
    DefineField(u32),
    /// obj key v → obj
    DefineFieldElem,
    // iteration (operand: the first of three locals: iterator, next, done)
    GetIter(u32),
    GetAsyncIter(u32),
    /// Pushes the next value, or jumps when done.
    IterStep(u32, u32),
    /// Pushes the next value, or undefined when done.
    IterValue(u32),
    /// Calls `return` if the iterator isn't done.
    IterClose(u32),
    /// The same, ignoring errors from `return` (when an exception is already on its way).
    IterCloseQuiet(u32),
    /// The rest of the iterator as an array.
    IterRest(u32),
    /// obj → (a for-in iterator in the local)
    ForInStart(u32),
    /// Pushes the next key, or jumps when done.
    ForInNext(u32, u32),
    /// for await: awaits the next result; pushes value or jumps when done.
    AsyncIterStep(u32, u32),
    // exceptions and returns
    Throw,
    /// Rethrows the completion saved in a local by a finally handler.
    Rethrow(u32),
    /// Throws a TypeError naming the constant (assignment to a constant).
    ThrowConst(u32),
    /// Throws a ReferenceError (a TDZ write the compiler can see).
    ThrowRef(u32),
    Return,
    ReturnUndef,
    // generators and async
    /// The initial suspension of a generator (the call returns the generator object).
    GenStart,
    Yield,
    /// yield*: the delegate iterator is in locals (operand), the received value on the stack.
    YieldStar(u32),
    Await,
    // misc
    This,
    /// `this` in a derived constructor (ReferenceError before super()).
    ThisChk,
    NewTarget,
    ArgumentsObj,
    /// An array of the arguments from index n on.
    RestArgs(u32),
    TemplateObj(u32),
    RegExp(u32),
    /// Throws a TypeError if the top is null or undefined (destructuring).
    CheckCoercible,
    /// Throws if the top is not an object (iterator results).
    Nop,
    // Fused pairs (see `fuse`): executed in place of the first op of the pair; the second op
    // stays in the next slot (so a jump to it still works) and is skipped on the way past.
    /// This; GetProp(c)
    ThisProp(u32),
    /// This; GetPropKeep(c)
    ThisPropKeep(u32),
    /// GetLocal(l); GetProp(c)
    LocalProp(u32, u32),
    /// GetLocal(l); GetPropKeep(c)
    LocalPropKeep(u32, u32),
    /// GetLocal(a); GetLocal(b)
    Local2(u32, u32),
    /// GetProp(c); GetProp(d)
    PropProp(u32, u32),
    /// SetProp(c); Pop
    SetPropPop(u32),
    /// StrictEq / StrictNe / Lt / Gt / Le / Ge; JumpIfFalse(t) (the target is read from the
    /// next slot)
    StrictEqJf,
    StrictNeJf,
    LtJf,
    GtJf,
    LeJf,
    GeJf,
    /// Int(i); StrictEq
    StrictEqInt(i32),
    /// Null; StrictEq / StrictNe
    StrictEqNull,
    StrictNeNull,
}

/// Replaces the first op of each hot pair by one op that does both, leaving the second op
/// in place after it. A fused op ends up two slots on, so a jump into the middle of a pair
/// (to the second op) still runs it alone, and handler ranges, line tables and jump targets
/// (all instruction indices) need no changes. The fused op sets the program counter to the
/// second op's slot before anything that can throw, so errors are placed exactly as they
/// are for the unfused pair.
pub(crate) fn fuse(ops: &mut [Op]) {
    #[cfg(test)]
    if NO_FUSE.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let orig: alloc::vec::Vec<Op> = ops.to_vec();
    for i in 0..orig.len().saturating_sub(1) {
        let f = match (orig[i], orig[i + 1]) {
            (Op::This, Op::GetProp(c)) => Op::ThisProp(c),
            (Op::This, Op::GetPropKeep(c)) => Op::ThisPropKeep(c),
            (Op::GetLocal(l), Op::GetProp(c)) => Op::LocalProp(l, c),
            (Op::GetLocal(l), Op::GetPropKeep(c)) => Op::LocalPropKeep(l, c),
            (Op::GetLocal(a), Op::GetLocal(b)) => Op::Local2(a, b),
            (Op::GetProp(c), Op::GetProp(d)) => Op::PropProp(c, d),
            (Op::SetProp(c), Op::Pop) => Op::SetPropPop(c),
            (Op::StrictEq, Op::JumpIfFalse(_)) => Op::StrictEqJf,
            (Op::StrictNe, Op::JumpIfFalse(_)) => Op::StrictNeJf,
            (Op::Lt, Op::JumpIfFalse(_)) => Op::LtJf,
            (Op::Gt, Op::JumpIfFalse(_)) => Op::GtJf,
            (Op::Le, Op::JumpIfFalse(_)) => Op::LeJf,
            (Op::Ge, Op::JumpIfFalse(_)) => Op::GeJf,
            (Op::Int(n), Op::StrictEq) => Op::StrictEqInt(n),
            (Op::Null, Op::StrictEq) => Op::StrictEqNull,
            (Op::Null, Op::StrictNe) => Op::StrictNeNull,
            _ => continue,
        };
        ops[i] = f;
    }
}

/// Tests: run without fusing, to compare results with and without.
#[cfg(test)]
pub(crate) static NO_FUSE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub(crate) enum Const {
    Num(f64),
    Str(StrId),
    Code(Rc<Code>),
    /// A template's cooked strings (None = undefined) and raw strings; the object is made
    /// once per site.
    Template(Vec<Option<StrId>>, Vec<StrId>, RefCell<Option<ObjId>>),
    Regex(StrId, StrId),
}

/// A try region: `start..end` (instruction indices) jumps to `target` on an exception.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Handler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
    /// Stack height (relative to the frame's base) to restore.
    pub depth: u32,
    /// A finally handler (also runs for generator returns); else a catch.
    pub finally: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CodeKind {
    Script,
    Normal,
    Arrow,
    Method,
    ClassCtor,
    DerivedCtor,
    FieldInit,
}

pub(crate) struct Source {
    pub text: Rc<[u8]>,
    pub file: String,
    pub line_starts: Vec<u32>,
    /// Added to the source's own line numbers (an inline script's line in its page).
    pub line_delta: i32,
    /// The column of the first line (1-based).
    pub first_col: u32,
}

impl Source {
    pub fn line_col(&self, pos: u32) -> (u32, u32) {
        let (l, c) = crate::lexer::line_col(&self.text, &self.line_starts, pos);
        let line = (l as i64 + self.line_delta as i64).max(1) as u32;
        let col = if l == 1 {
            c + self.first_col.saturating_sub(1)
        } else {
            c
        };
        (line, col)
    }
}

pub(crate) struct Code {
    pub ops: Vec<Op>,
    pub consts: Vec<Const>,
    /// For each string constant, its property key (precomputed: property names are used
    /// as keys far more often than as values).
    pub keys: Vec<crate::object::PropKey>,
    pub handlers: Vec<Handler>,
    /// (instruction index, source position), ascending.
    pub lines: Vec<(u32, u32)>,
    pub nparams: u32,
    pub nlocals: u32,
    /// fn.length
    pub length: u32,
    pub name: Option<StrId>,
    pub kind: CodeKind,
    pub strict: bool,
    pub is_async: bool,
    pub is_generator: bool,
    /// The frame keeps arguments past `nparams` (for `arguments` / rest).
    pub keeps_extra_args: bool,
    pub source: Rc<Source>,
    pub span: (u32, u32),
    pub marked: Cell<u32>,
    /// The module this code belongs to (None for scripts).
    pub module: Option<crate::api::ModuleId>,
}

impl Code {
    pub fn trace(&self, t: &mut Tracer, epoch: u32) {
        if self.marked.get() == epoch {
            return;
        }
        self.marked.set(epoch);
        if let Some(n) = self.name {
            t.str(n);
        }
        for c in &self.consts {
            match c {
                Const::Str(s) => t.str(*s),
                Const::Code(c) => c.trace(t, epoch),
                Const::Template(cooked, raw, obj) => {
                    for s in cooked.iter().flatten() {
                        t.str(*s);
                    }
                    for s in raw {
                        t.str(*s);
                    }
                    if let Some(o) = *obj.borrow() {
                        t.obj(o);
                    }
                }
                Const::Regex(p, f) => {
                    t.str(*p);
                    t.str(*f);
                }
                Const::Num(_) => {}
            }
        }
    }

    /// The source position of the instruction at `pc`.
    pub fn pos_at(&self, pc: u32) -> u32 {
        match self.lines.binary_search_by(|e| e.0.cmp(&pc)) {
            Ok(i) => self.lines[i].1,
            Err(0) => self.span.0,
            Err(i) => self.lines[i - 1].1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuse_replaces_the_first_op_of_a_pair_only() {
        let mut ops = alloc::vec![
            Op::This,
            Op::GetProp(3),
            Op::GetProp(4),
            Op::StrictEq,
            Op::JumpIfFalse(9),
            Op::GetLocal(1),
            Op::GetLocal(2),
            Op::Pop,
        ];
        let before = ops.clone();
        fuse(&mut ops);
        assert_eq!(ops[0], Op::ThisProp(3));
        assert_eq!(
            ops[1],
            Op::PropProp(3, 4),
            "overlapping pairs fuse independently"
        );
        assert_eq!(
            ops[2],
            Op::GetProp(4),
            "no pair starts here: GetProp, StrictEq"
        );
        assert_eq!(ops[3], Op::StrictEqJf);
        assert_eq!(
            ops[4], before[4],
            "the second op stays for a jump into the middle"
        );
        assert_eq!(ops[5], Op::Local2(1, 2));
        assert_eq!(ops[6], before[6]);
        assert_eq!(ops[7], before[7]);
        assert_eq!(ops.len(), before.len());
    }
}
