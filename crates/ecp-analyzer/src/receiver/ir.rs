//! The receiver IR of one function body: scopes, bindings, blocks, uses and
//! closures. Flow-neutral below the `Value` / `Stmt` layer, so `ecp flow`
//! can later add its own variants (field write, call with arguments, heap
//! object) on the same skeleton.

use super::lattice::UnknownCause;
use ecp_core::analyzer::types::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BindingId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UseId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClosureId(pub u32);

/// One function body, lowered. Closures nest as their own `FunctionIr`.
#[derive(Debug, Clone)]
pub struct FunctionIr {
    pub span: Span,
    pub scopes: Vec<Scope>,
    pub bindings: Vec<Binding>,
    /// Interned by `text`. One table serves a root function and every
    /// closure nested in it, so a captured `Known(t)` means the same type on
    /// both sides of the capture: the root holds the table and a closure's
    /// own `types` is empty.
    pub types: Vec<TypeRef>,
    pub blocks: Vec<Block>,
    pub entry: BlockId,
    pub uses: Vec<Use>,
    pub closures: Vec<Closure>,
    /// Binding -> cell. Two bindings that a by-ref alias joins (`$r = &$o`,
    /// `T& r = o`) share one cell for the whole function. The solver answers
    /// such a cell `Unknown(Escape)` everywhere (a pinned declared type
    /// excepted), so the flow-insensitive merge can lose an edge, never add one.
    pub cells: Vec<u32>,
}

impl FunctionIr {
    pub fn binding(&self, id: BindingId) -> &Binding {
        &self.bindings[id.0 as usize]
    }

    pub fn cell(&self, id: BindingId) -> u32 {
        self.cells[id.0 as usize]
    }

    pub fn cell_count(&self) -> usize {
        self.cells.iter().max().map_or(0, |&max| max as usize + 1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scope {
    pub parent: Option<ScopeId>,
    pub kind: ScopeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Function,
    Block,
    Loop,
    Catch,
    Comprehension,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub name: Box<str>,
    pub scope: ScopeId,
    pub origin: BindingOrigin,
    /// The declared static type, when the language pins it.
    pub declared: Option<TypeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingOrigin {
    Param,
    Local,
    ForEach,
    CatchParam,
    Pattern,
    Captured(Capture),
    Free,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capture {
    /// The captured binding in the enclosing `FunctionIr`.
    pub outer: BindingId,
    pub mode: CaptureMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    ByValue,
    ByRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub term: Terminator,
    /// The handler a statement of this block may throw to. The handler's
    /// entry joins the state before and after each statement.
    pub handler: Option<BlockId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    /// `x = v`, `let x = v`, `T x = v`, `for x in ..` (value Unknown), `catch (T e)`.
    Assign { target: BindingId, value: Value },
    /// `$r = &$o`: the lowering also merged the two cells (`FunctionIr::cells`).
    Alias {
        target: BindingId,
        source: BindingId,
    },
    /// A write whose value the lowering cannot see: `f(&$x)`, `extract()`,
    /// `$$v`, `locals()[..] =`, `eval`, an out-parameter.
    /// `target: None` writes every cell.
    Havoc {
        target: Option<BindingId>,
        cause: UnknownCause,
    },
    /// Closure creation; its capture effects apply from here.
    MakeClosure(ClosureId),
    /// A receiver occurrence the extractor asks about.
    Use(UseId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `new T()`, `T()` that constructs, `self`/`this`, a literal of known class.
    Type(TypeId),
    /// `x = y`.
    Copy(BindingId),
    /// A value of explicitly dynamic static type: TS `any`/`unknown`, Dart `dynamic`.
    Dynamic,
    /// Any value the lowering cannot type (a call result, a field, an untyped param read).
    Unknown,
    /// `x: T = v` (Python, TS): the annotation types the value at this point.
    Annotated { declared: TypeId, value: Box<Value> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    Goto(BlockId),
    /// Any successor may run. `if`, `match`, `switch`, `&&`/`||`, `?:`, loop heads.
    Branch(Box<[BlockId]>),
    Return,
    Throw,
}

impl Terminator {
    pub fn successors(&self) -> &[BlockId] {
        match self {
            Terminator::Goto(target) => std::slice::from_ref(target),
            Terminator::Branch(targets) => &targets[..],
            Terminator::Return | Terminator::Throw => &[],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Use {
    pub binding: BindingId,
    /// `tree_sitter::Node::id()` of the receiver expression.
    pub node: usize,
}

#[derive(Debug, Clone)]
pub struct Closure {
    pub ir: FunctionIr,
    pub span: Span,
}

/// A type as written, re-resolvable later in the occurrence's file. It never
/// holds a node index of any build: the resolver binds `text` in the
/// occurrence's file, with that file's imports.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeRef {
    pub text: Box<str>,
    pub origin: TypeOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeOrigin {
    Annotation,
    Construction,
    SelfType,
    Literal,
}
