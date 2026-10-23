//! The syntax tree. Positions are byte offsets into the source (turned into line:column only
//! for messages). Scopes (functions, blocks, loops with lexical heads, catch clauses, switch
//! bodies, classes) carry a `ScopeId` the parser hands out; the compiler's scope analysis
//! fills a table indexed by it.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::atoms::Atom;

pub(crate) type Pos = u32;
pub(crate) type ScopeId = u32;

pub(crate) struct Expr {
    pub k: ExprKind,
    pub pos: Pos,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum UnOp {
    Neg,
    Plus,
    Not,
    BitNot,
    Typeof,
    Void,
    Delete,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Gt,
    Le,
    Ge,
    In,
    InstanceOf,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LogicOp {
    And,
    Or,
    Nullish,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AssignOp {
    Assign,
    Op(BinOp),
    Logic(LogicOp),
}

pub(crate) enum ArrElem {
    Hole,
    Item(Expr),
    Spread(Expr),
}

pub(crate) enum Arg {
    Item(Expr),
    Spread(Expr),
}

pub(crate) enum PropName {
    Ident(Atom),
    Str(Vec<u16>),
    Num(f64),
    Computed(Box<Expr>),
    /// `#name` (class members only); the atom includes the `#`.
    Private(Atom),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MethodKind {
    Method,
    Get,
    Set,
}

pub(crate) enum PropDef {
    KeyValue(PropName, Expr),
    /// `{ a }`
    Shorthand(Atom, Pos),
    /// `{ a = 1 }`: only valid when the literal becomes a pattern.
    CoverInit(Atom, Pos, Expr),
    Method(PropName, MethodKind, Box<Func>),
    Spread(Expr),
    /// `__proto__: value` (sets the prototype).
    Proto(Expr),
}

pub(crate) struct TemplateParts {
    pub cooked: Vec<Option<Vec<u16>>>,
    pub raw: Vec<Vec<u16>>,
}

pub(crate) enum ExprKind {
    Num(f64),
    Str(Vec<u16>),
    Template(Box<TemplateParts>, Vec<Expr>),
    Tagged(Box<Expr>, Box<TemplateParts>, Vec<Expr>),
    Regex(Vec<u16>, Vec<u16>),
    Ident(Atom),
    This,
    NewTarget,
    Null,
    Bool(bool),
    Array(Vec<ArrElem>),
    Object(Vec<PropDef>),
    Func(Box<Func>),
    Class(Box<Class>),
    Unary(UnOp, Box<Expr>),
    Update {
        inc: bool,
        prefix: bool,
        target: Box<Expr>,
    },
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Logic(LogicOp, Box<Expr>, Box<Expr>),
    Assign(AssignOp, Box<Pat>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Arg>, bool),
    New(Box<Expr>, Vec<Arg>),
    Member(Box<Expr>, Atom, bool),
    Index(Box<Expr>, Box<Expr>, bool),
    /// The end of an optional chain: where `?.` short-circuits to.
    OptChain(Box<Expr>),
    Seq(Vec<Expr>),
    Yield(Option<Box<Expr>>, bool),
    Await(Box<Expr>),
    SuperCall(Vec<Arg>),
    SuperMember(Atom),
    SuperIndex(Box<Expr>),
    /// `...x` inside a parenthesized list (only before `=>`).
    Spread(Box<Expr>),
    /// `obj.#name` / `obj?.#name`.
    PrivMember(Box<Expr>, Atom, bool),
    /// `#name` as the left operand of `in`.
    PrivName(Atom),
    /// `import(specifier, options)`.
    ImportCall(Box<Expr>, Option<Box<Expr>>),
    /// `import.meta` (modules only).
    ImportMeta,
}

pub(crate) struct PatElem {
    pub pat: Pat,
    pub default: Option<Expr>,
}

pub(crate) struct PatProp {
    pub key: PropName,
    pub pat: Pat,
    pub default: Option<Expr>,
}

pub(crate) enum Pat {
    Ident(Atom, Pos),
    /// A member expression (assignment targets only).
    Expr(Box<Expr>),
    Array(Vec<Option<PatElem>>, Option<Box<Pat>>),
    Object(Vec<PatProp>, Option<Box<Pat>>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum VarKind {
    Var,
    Let,
    Const,
}

pub(crate) struct VarDecl {
    pub pat: Pat,
    pub init: Option<Expr>,
}

pub(crate) enum ForInit {
    Var(VarKind, Vec<VarDecl>),
    Expr(Expr),
}

pub(crate) enum ForHead {
    Var(VarKind, Pat),
    Target(Pat),
}

pub(crate) struct Block {
    pub body: Vec<Stmt>,
    pub scope: ScopeId,
}

pub(crate) struct Case {
    pub test: Option<Expr>,
    pub body: Vec<Stmt>,
}

pub(crate) enum FieldKey {
    Name(PropName),
    /// The i-th computed key, evaluated when the class is defined.
    Computed(u32),
}

pub(crate) struct FieldDef {
    pub key: FieldKey,
    pub value: Option<Expr>,
    pub pos: Pos,
}

pub(crate) struct Stmt {
    pub k: StmtKind,
    pub pos: Pos,
}

pub(crate) enum StmtKind {
    Expr(Expr),
    Var(VarKind, Vec<VarDecl>),
    FuncDecl(Box<Func>),
    ClassDecl(Box<Class>),
    Return(Option<Expr>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    Block(Block),
    For {
        init: Option<ForInit>,
        test: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
        scope: ScopeId,
    },
    ForIn {
        left: ForHead,
        right: Expr,
        body: Box<Stmt>,
        scope: ScopeId,
    },
    ForOf {
        left: ForHead,
        right: Expr,
        body: Box<Stmt>,
        scope: ScopeId,
        is_await: bool,
    },
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    Break(Option<Atom>),
    Continue(Option<Atom>),
    Throw(Expr),
    Try {
        block: Block,
        param: Option<Pat>,
        handler: Option<Block>,
        catch_scope: ScopeId,
        finalizer: Option<Block>,
    },
    Switch {
        disc: Expr,
        cases: Vec<Case>,
        scope: ScopeId,
    },
    Labeled(Atom, Box<Stmt>),
    Empty,
    Debugger,
    /// Class fields (inside the synthetic field-initializer functions): `this[key] = value`.
    Fields(Vec<FieldDef>),
    /// The start of an instance field initializer: installs the class's private methods and
    /// accessors on `this`.
    PrivMethods(Vec<PrivInstall>),
}

/// One private method or accessor half to install on each instance.
pub(crate) struct PrivInstall {
    /// The binding holding the private name (`#m`).
    pub name: Atom,
    /// The binding holding the function (`#m/m`, `#m/g` or `#m/s`).
    pub func: Atom,
    /// 0 method, 1 getter, 2 setter.
    pub kind: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PrivKind {
    Field,
    Method,
    Getter,
    Setter,
}

/// A private name a class declares.
pub(crate) struct PrivDecl {
    pub name: Atom,
    pub is_static: bool,
    pub kind: PrivKind,
    /// For a method or accessor half: the hidden binding that keeps its function.
    pub func: Option<Atom>,
    pub pos: Pos,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FuncKind {
    Normal,
    Arrow,
    Method,
    Getter,
    Setter,
    /// A class constructor (base class).
    ClassCtor,
    /// A derived class constructor (`extends`).
    DerivedCtor,
    /// The synthetic function that initializes instance fields, or runs static fields and
    /// static blocks (with `this` the class).
    FieldInit,
}

pub(crate) struct Param {
    pub pat: Pat,
    pub default: Option<Expr>,
}

pub(crate) enum Body {
    Stmts(Vec<Stmt>),
    Expr(Box<Expr>),
}

pub(crate) struct Func {
    pub name: Option<Atom>,
    pub params: Vec<Param>,
    pub rest: Option<Pat>,
    pub body: Body,
    pub kind: FuncKind,
    pub is_async: bool,
    pub is_generator: bool,
    pub strict: bool,
    /// The function's own scope (parameters, vars and top-level declarations).
    pub scope: ScopeId,
    /// For a named function expression: the scope binding its own name.
    pub name_scope: Option<ScopeId>,
    pub start: Pos,
    pub end: Pos,
    /// Declared as a statement (a declaration, not an expression).
    pub is_decl: bool,
}

pub(crate) enum MemberKind {
    Method(Box<Func>),
    Get(Box<Func>),
    Set(Box<Func>),
}

pub(crate) struct ClassMember {
    pub is_static: bool,
    pub key: PropName,
    pub kind: MemberKind,
}

pub(crate) struct Class {
    pub name: Option<Atom>,
    pub heritage: Option<Box<Expr>>,
    pub ctor: Option<Box<Func>>,
    pub members: Vec<ClassMember>,
    /// The synthetic instance-field initializer (None without instance fields).
    pub fields: Option<Box<Func>>,
    /// The synthetic static initializer (static fields and blocks, in order).
    pub statics: Option<Box<Func>>,
    /// Computed field keys (instance and static), in source order with the members: the
    /// compiler evaluates them while defining the class.
    pub field_keys: Vec<Expr>,
    /// For each computed field key, whether it belongs to the static initializer.
    pub field_key_static: Vec<bool>,
    /// Where field keys are evaluated relative to the members: field key i is evaluated
    /// just before member `field_key_at[i]` (or after all members if that equals the count).
    pub field_key_at: Vec<u32>,
    /// The private names the class body declares (a getter and a setter share a name).
    pub privates: Vec<PrivDecl>,
    /// The class scope (binds the class name inside the class).
    pub scope: ScopeId,
    pub start: Pos,
    pub end: Pos,
}

pub(crate) struct Script {
    pub body: Vec<Stmt>,
    pub scope: ScopeId,
    pub strict: bool,
}

/// One distinct (specifier, attributes) pair a module asks for.
pub(crate) struct ModRequest {
    pub specifier: alloc::string::String,
    /// Sorted by key.
    pub attrs: Vec<(alloc::string::String, alloc::string::String)>,
    pub pos: Pos,
}

pub(crate) enum ImportName {
    Name(alloc::string::String),
    /// `import * as ns` / `export * as ns from`
    All,
}

/// `import { a as b } from "m"` and friends: the local binding and what it imports.
pub(crate) struct ImportEntry {
    pub request: u32,
    pub name: ImportName,
    pub local: Atom,
    pub pos: Pos,
}

/// `export { x as y }` (and exported declarations): a binding of this module.
pub(crate) struct LocalExport {
    pub export_name: alloc::string::String,
    pub local: Atom,
    pub pos: Pos,
}

/// `export { a as b } from "m"`, `export * as ns from "m"`.
pub(crate) struct IndirectExport {
    pub export_name: alloc::string::String,
    pub request: u32,
    pub name: ImportName,
    pub pos: Pos,
}

#[derive(Default)]
pub(crate) struct ModuleInfo {
    pub requests: Vec<ModRequest>,
    pub imports: Vec<ImportEntry>,
    pub local_exports: Vec<LocalExport>,
    pub indirect_exports: Vec<IndirectExport>,
    /// `export * from "m"`: request index and position.
    pub star_exports: Vec<(u32, Pos)>,
    /// The module body uses `await` or `for await` outside any function.
    pub has_tla: bool,
    /// The atom of the hidden `*default*` binding.
    pub default_atom: Atom,
}

pub(crate) struct ModuleAst {
    pub body: Vec<Stmt>,
    pub scope: ScopeId,
    pub info: ModuleInfo,
}
