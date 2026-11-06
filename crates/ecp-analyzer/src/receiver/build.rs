//! `IrBuilder`: what a language lowering writes a function body through.
//! It keeps the scope stack, the break/continue targets, the open exception
//! handlers and the frames of nested closures, so a lowering only walks its
//! own syntax.

use super::ir::{
    Binding, BindingId, BindingOrigin, Block, BlockId, Capture, CaptureMode, Closure, ClosureId,
    FunctionIr, Scope, ScopeId, ScopeKind, Stmt, Terminator, TypeId, TypeOrigin, TypeRef, Use,
    UseId, Value,
};
use super::lattice::UnknownCause;
use super::{LangPolicy, ScopeRule};
use ecp_core::analyzer::types::Span;

pub struct IrBuilder {
    policy: LangPolicy,
    /// The type table of the root function, shared by its closures.
    types: Vec<TypeRef>,
    /// `frames[0]` is the root function; the last frame is the closure being lowered.
    frames: Vec<Frame>,
    use_count: usize,
}

struct Breakable {
    continue_to: Option<BlockId>,
    break_to: BlockId,
}

struct Frame {
    span: Span,
    scopes: Vec<Scope>,
    /// Per scope: whether it is still open, so a lookup sees its bindings.
    open: Vec<bool>,
    /// Open scopes, innermost last. Never empty: the function scope stays.
    stack: Vec<ScopeId>,
    bindings: Vec<Binding>,
    /// Union-find parent per binding; `alias` joins two bindings' cells.
    parents: Vec<u32>,
    blocks: Vec<Block>,
    /// `None` after a terminator: a statement then opens a block with no predecessor.
    current: Option<BlockId>,
    uses: Vec<Use>,
    closures: Vec<Closure>,
    breakables: Vec<Breakable>,
    handlers: Vec<BlockId>,
}

const FUNCTION_SCOPE: ScopeId = ScopeId(0);
const ENTRY: BlockId = BlockId(0);

impl Frame {
    fn new(span: Span) -> Self {
        Frame {
            span,
            scopes: vec![Scope {
                parent: None,
                kind: ScopeKind::Function,
            }],
            open: vec![true],
            stack: vec![FUNCTION_SCOPE],
            bindings: Vec::new(),
            parents: Vec::new(),
            blocks: vec![Block {
                stmts: Vec::new(),
                term: Terminator::Return,
                handler: None,
            }],
            current: Some(ENTRY),
            uses: Vec::new(),
            closures: Vec::new(),
            breakables: Vec::new(),
            handlers: Vec::new(),
        }
    }

    fn lookup(&self, name: &str) -> Option<BindingId> {
        self.bindings
            .iter()
            .rposition(|binding| self.open[binding.scope.0 as usize] && &*binding.name == name)
            .map(|index| BindingId(index as u32))
    }

    fn declare(
        &mut self,
        scope: ScopeId,
        name: &str,
        origin: BindingOrigin,
        declared: Option<TypeId>,
    ) -> BindingId {
        let id = BindingId(self.bindings.len() as u32);
        self.bindings.push(Binding {
            name: name.into(),
            scope,
            origin,
            declared,
        });
        self.parents.push(id.0);
        id
    }

    fn find(&self, mut binding: u32) -> u32 {
        while self.parents[binding as usize] != binding {
            binding = self.parents[binding as usize];
        }
        binding
    }

    fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(Block {
            stmts: Vec::new(),
            term: Terminator::Return,
            handler: self.handlers.last().copied(),
        });
        id
    }

    fn push(&mut self, stmt: Stmt) {
        let block = match self.current {
            Some(block) => block,
            None => {
                let block = self.new_block();
                self.current = Some(block);
                block
            }
        };
        self.blocks[block.0 as usize].stmts.push(stmt);
    }

    fn terminate(&mut self, term: Terminator) {
        if let Some(block) = self.current.take() {
            self.blocks[block.0 as usize].term = term;
        }
    }

    fn finish(self, types: Vec<TypeRef>) -> FunctionIr {
        let mut dense = vec![u32::MAX; self.bindings.len()];
        let mut next = 0u32;
        let cells = (0..self.bindings.len() as u32)
            .map(|binding| {
                let root = self.find(binding) as usize;
                if dense[root] == u32::MAX {
                    dense[root] = next;
                    next += 1;
                }
                dense[root]
            })
            .collect();
        FunctionIr {
            span: self.span,
            scopes: self.scopes,
            bindings: self.bindings,
            types,
            blocks: self.blocks,
            entry: ENTRY,
            uses: self.uses,
            closures: self.closures,
            cells,
        }
    }
}

impl IrBuilder {
    pub fn new(policy: LangPolicy, span: Span) -> Self {
        IrBuilder {
            policy,
            types: Vec::new(),
            frames: vec![Frame::new(span)],
            use_count: 0,
        }
    }

    pub fn policy(&self) -> LangPolicy {
        self.policy
    }

    /// The `Use`s recorded so far, closures included. A lowering that ends
    /// with none returns `Lowered::NoUses`.
    pub fn use_count(&self) -> usize {
        self.use_count
    }

    fn frame(&mut self) -> &mut Frame {
        self.frames
            .last_mut()
            .expect("IrBuilder keeps the root frame until finish")
    }

    fn frame_ref(&self) -> &Frame {
        self.frames
            .last()
            .expect("IrBuilder keeps the root frame until finish")
    }

    /// The type written as `text`, one id per distinct text. A function
    /// names few types, so a scan beats hashing an owned key.
    pub fn intern(&mut self, text: &str, origin: TypeOrigin) -> TypeId {
        if let Some(index) = self.types.iter().position(|ty| &*ty.text == text) {
            return TypeId(index as u32);
        }
        self.types.push(TypeRef {
            text: text.into(),
            origin,
        });
        TypeId(self.types.len() as u32 - 1)
    }

    pub fn push_scope(&mut self, kind: ScopeKind) -> ScopeId {
        let frame = self.frame();
        let parent = frame.stack.last().copied();
        let id = ScopeId(frame.scopes.len() as u32);
        frame.scopes.push(Scope { parent, kind });
        frame.open.push(true);
        frame.stack.push(id);
        id
    }

    /// Closes the innermost scope; its bindings stop being visible to
    /// `lookup`. The function scope never closes.
    pub fn pop_scope(&mut self) {
        let frame = self.frame();
        if frame.stack.len() > 1 {
            if let Some(scope) = frame.stack.pop() {
                frame.open[scope.0 as usize] = false;
            }
        }
    }

    pub fn current_scope(&self) -> ScopeId {
        self.frame_ref()
            .stack
            .last()
            .copied()
            .unwrap_or(FUNCTION_SCOPE)
    }

    pub fn function_scope(&self) -> ScopeId {
        FUNCTION_SCOPE
    }

    /// Declares `name` where the language's `ScopeRule` puts it: the
    /// innermost scope (`Block`) or the function scope (`Function`).
    pub fn declare(
        &mut self,
        name: &str,
        origin: BindingOrigin,
        declared: Option<TypeId>,
    ) -> BindingId {
        let scope = match self.policy.scope {
            ScopeRule::Block => self.current_scope(),
            ScopeRule::Function => FUNCTION_SCOPE,
        };
        self.declare_in(scope, name, origin, declared)
    }

    /// Declares `name` in `scope`, whatever the language's rule (JS `var`
    /// versus `let`, a Ruby block parameter, a comprehension variable).
    pub fn declare_in(
        &mut self,
        scope: ScopeId,
        name: &str,
        origin: BindingOrigin,
        declared: Option<TypeId>,
    ) -> BindingId {
        self.frame().declare(scope, name, origin, declared)
    }

    /// The innermost visible binding of `name` in the function being lowered.
    pub fn lookup(&self, name: &str) -> Option<BindingId> {
        self.frame_ref().lookup(name)
    }

    /// The binding a closure's capture of `name` creates (PHP `use (&$x)`,
    /// C++ `[x]`, Python `nonlocal x`). `None` when no enclosing function
    /// declares `name`, or when the root function is being lowered.
    /// Intermediate closures capture with the same `mode`.
    pub fn capture(&mut self, name: &str, mode: CaptureMode) -> Option<BindingId> {
        let depth = self.frames.len() - 1;
        if depth == 0 {
            return None;
        }
        let outer = self.resolve(depth - 1, name, mode)?;
        Some(self.capture_in(depth, outer, name, mode))
    }

    /// `name` as the function being lowered sees it: its own binding, else
    /// an implicit capture of an enclosing one with `mode` (the language's
    /// default capture).
    pub fn lookup_or_capture(&mut self, name: &str, mode: CaptureMode) -> Option<BindingId> {
        self.lookup(name).or_else(|| self.capture(name, mode))
    }

    fn resolve(&mut self, depth: usize, name: &str, mode: CaptureMode) -> Option<BindingId> {
        if let Some(found) = self.frames[depth].lookup(name) {
            return Some(found);
        }
        if depth == 0 {
            return None;
        }
        let outer = self.resolve(depth - 1, name, mode)?;
        Some(self.capture_in(depth, outer, name, mode))
    }

    fn capture_in(
        &mut self,
        depth: usize,
        outer: BindingId,
        name: &str,
        mode: CaptureMode,
    ) -> BindingId {
        let declared = self.frames[depth - 1].bindings[outer.0 as usize].declared;
        self.frames[depth].declare(
            FUNCTION_SCOPE,
            name,
            BindingOrigin::Captured(Capture { outer, mode }),
            declared,
        )
    }

    /// A block with no predecessor yet. It inherits the innermost open handler.
    pub fn new_block(&mut self) -> BlockId {
        self.frame().new_block()
    }

    /// `None` right after a terminator.
    pub fn current_block(&self) -> Option<BlockId> {
        self.frame_ref().current
    }

    /// Continues lowering into `block`. The block the builder was in keeps
    /// the terminator it has; give it one first.
    pub fn switch_to(&mut self, block: BlockId) {
        self.frame().current = Some(block);
    }

    pub fn goto(&mut self, target: BlockId) {
        self.frame().terminate(Terminator::Goto(target));
    }

    pub fn branch(&mut self, targets: &[BlockId]) {
        self.frame().terminate(Terminator::Branch(targets.into()));
    }

    pub fn ret(&mut self) {
        self.frame().terminate(Terminator::Return);
    }

    pub fn throw(&mut self) {
        self.frame().terminate(Terminator::Throw);
    }

    /// Opens a loop (`continue_to: Some(head)`) or a `switch`
    /// (`continue_to: None`) that `break_out` leaves to `break_to`.
    pub fn push_breakable(&mut self, continue_to: Option<BlockId>, break_to: BlockId) {
        self.frame().breakables.push(Breakable {
            continue_to,
            break_to,
        });
    }

    pub fn pop_breakable(&mut self) {
        self.frame().breakables.pop();
    }

    /// `break`: a `Goto` to the innermost breakable's exit. With none open
    /// (invalid code) the path ends.
    pub fn break_out(&mut self) {
        let target = self.frame_ref().breakables.last().map(|b| b.break_to);
        match target {
            Some(target) => self.goto(target),
            None => self.ret(),
        }
    }

    /// `continue`: a `Goto` to the innermost loop's head.
    pub fn continue_loop(&mut self) {
        let target = self
            .frame_ref()
            .breakables
            .iter()
            .rev()
            .find_map(|b| b.continue_to);
        match target {
            Some(target) => self.goto(target),
            None => self.ret(),
        }
    }

    /// Starts a `try` body whose statements may throw to `handler`. Create
    /// `handler` before the call, so it inherits the outer handler.
    pub fn enter_try(&mut self, handler: BlockId) {
        self.frame().handlers.push(handler);
        let body = self.new_block();
        self.goto(body);
        self.switch_to(body);
    }

    /// Ends the `try` body opened by `enter_try`; lowering continues in a
    /// fresh block that the body falls through to.
    pub fn leave_try(&mut self) {
        self.frame().handlers.pop();
        let next = self.new_block();
        self.goto(next);
        self.switch_to(next);
    }

    pub fn assign(&mut self, target: BindingId, value: Value) {
        self.frame().push(Stmt::Assign { target, value });
    }

    /// `target = &source`: both bindings share one cell for the whole function.
    pub fn alias(&mut self, target: BindingId, source: BindingId) {
        let frame = self.frame();
        let (a, b) = (frame.find(target.0), frame.find(source.0));
        if a != b {
            frame.parents[a as usize] = b;
        }
        frame.push(Stmt::Alias { target, source });
    }

    pub fn havoc(&mut self, target: Option<BindingId>, cause: UnknownCause) {
        self.frame().push(Stmt::Havoc { target, cause });
    }

    /// Records a receiver occurrence: `node` is the receiver expression's
    /// `tree_sitter::Node::id()`.
    pub fn use_binding(&mut self, binding: BindingId, node: usize) -> UseId {
        self.use_count += 1;
        let frame = self.frame();
        let id = UseId(frame.uses.len() as u32);
        frame.uses.push(Use { binding, node });
        frame.push(Stmt::Use(id));
        id
    }

    /// Lowers a nested closure through `lower`, then records its creation
    /// at the current point of the enclosing function.
    pub fn closure(&mut self, span: Span, lower: impl FnOnce(&mut Self)) -> ClosureId {
        let depth = self.frames.len();
        self.frames.push(Frame::new(span));
        lower(self);
        self.frames.truncate(depth + 1);
        let frame = self
            .frames
            .pop()
            .expect("the closure frame was pushed above");
        let ir = frame.finish(Vec::new());
        let parent = self.frame();
        let id = ClosureId(parent.closures.len() as u32);
        parent.closures.push(Closure { ir, span });
        parent.push(Stmt::MakeClosure(id));
        id
    }

    pub fn finish(mut self) -> FunctionIr {
        self.frames.truncate(1);
        let root = self
            .frames
            .pop()
            .expect("IrBuilder keeps the root frame until finish");
        root.finish(self.types)
    }
}
