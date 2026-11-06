//! The worklist solver over one lowered function and its closures.
//!
//! Sparse by construction: the state vector covers only the tracked cells
//! (cells a `Use` reads, the cells they copy from, and the outer cells a
//! closure's capture reads, each written somewhere in the function or its
//! closures). A use whose answer is fixed (a pinned declared type, an
//! aliased cell, a cell nothing writes, which holds its entry state) is
//! answered without tracking.

use super::ir::{
    BindingId, BindingOrigin, Block, BlockId, CaptureMode, FunctionIr, Stmt, TypeId, Value,
};
use super::lattice::{join, Abstract, UnknownCause};
use super::{DeclaredTypePolicy, LangPolicy};

pub const MAX_BLOCKS: usize = 4096;
pub const MAX_TRACKED_CELLS: usize = 256;
/// Block visits x tracked cells, summed over a root function and its closures.
pub const MAX_STEPS: u32 = 1 << 18;

/// The receiver state at every `Use` of a root function and its closures,
/// keyed by the use's node. `Known` ids index the root function's `types`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Solved {
    pub uses: Vec<(usize, Abstract)>,
    /// Some function of the tree exceeded a budget: every use then answers
    /// `Unknown(Budget)`.
    pub over_budget: bool,
}

pub fn solve(ir: &FunctionIr, policy: LangPolicy) -> Solved {
    let mut solved = Solved::default();
    if !has_uses(ir) {
        return solved;
    }
    let plan = Plan::new(ir, policy);
    let mut steps = 0u32;
    if solve_function(ir, &plan, policy, &[], &mut steps, &mut solved.uses).is_err() {
        solved.uses.clear();
        push_budget_uses(ir, policy, &mut solved.uses);
        solved.over_budget = true;
    }
    solved
}

fn has_uses(ir: &FunctionIr) -> bool {
    !ir.uses.is_empty() || ir.closures.iter().any(|closure| has_uses(&closure.ir))
}

/// Every use of an over-budget tree: `Unknown(Budget)`, except a use of a
/// pinned declared type, whose answer no flow changes.
fn push_budget_uses(ir: &FunctionIr, policy: LangPolicy, out: &mut Vec<(usize, Abstract)>) {
    out.extend(ir.uses.iter().map(|u| {
        let state = pinned(policy, ir, u.binding)
            .map_or(Abstract::Unknown(UnknownCause::Budget), Abstract::Known);
        (u.node, state)
    }));
    for closure in &ir.closures {
        push_budget_uses(&closure.ir, policy, out);
    }
}

struct OverBudget;

const UNTRACKED: u32 = u32::MAX;

/// What the solver precomputes for one function before it runs.
struct Plan {
    /// Per cell: two or more bindings share it (a by-ref alias).
    aliased: Vec<bool>,
    /// Per cell: a binding of it. Unique for a cell that is not aliased.
    cell_binding: Vec<BindingId>,
    /// Per cell: this function or a closure nested in it writes the cell.
    writes: Vec<bool>,
    /// Per cell: some closure writes it through a by-ref capture.
    has_writer: Vec<bool>,
    /// Per closure: the cells of this function it writes through by-ref captures.
    closure_writes: Vec<Vec<u32>>,
    /// Per cell: a use, a copy or a closure's capture reads it, unpinned and
    /// not aliased. A demanded cell nothing writes is answered by its entry
    /// state and holds no slot.
    demanded: Vec<bool>,
    /// Per cell: its slot in the state vector, or `UNTRACKED`.
    slot: Vec<u32>,
    /// Per slot: its cell.
    tracked: Vec<u32>,
    /// Whether some closure's by-ref capture needs the writes that follow
    /// its creation.
    needs_writes_after: bool,
    closures: Vec<Plan>,
}

fn pinned(policy: LangPolicy, ir: &FunctionIr, binding: BindingId) -> Option<TypeId> {
    match policy.declared {
        DeclaredTypePolicy::Pinned => ir.binding(binding).declared,
        DeclaredTypePolicy::PinnedUnlessDynamic | DeclaredTypePolicy::PointOfAssignment => None,
    }
}

fn copy_source(value: &Value) -> Option<BindingId> {
    match value {
        Value::Copy(source) => Some(*source),
        Value::Annotated { .. } | Value::Type(_) | Value::Dynamic | Value::Unknown => None,
    }
}

fn stmts(ir: &FunctionIr) -> impl Iterator<Item = &Stmt> {
    ir.blocks.iter().flat_map(|block| block.stmts.iter())
}

impl Plan {
    fn new(ir: &FunctionIr, policy: LangPolicy) -> Self {
        let cells = ir.cell_count();
        let mut members = vec![0u8; cells];
        let mut cell_binding = vec![BindingId(0); cells];
        for (binding, &cell) in ir.cells.iter().enumerate() {
            let cell = cell as usize;
            members[cell] = members[cell].saturating_add(1);
            cell_binding[cell] = BindingId(binding as u32);
        }
        let aliased: Vec<bool> = members.iter().map(|&n| n > 1).collect();
        let closures: Vec<Plan> = ir
            .closures
            .iter()
            .map(|closure| Plan::new(&closure.ir, policy))
            .collect();

        let mut writes = vec![false; cells];
        for stmt in stmts(ir) {
            match stmt {
                Stmt::Assign { target, .. }
                | Stmt::Havoc {
                    target: Some(target),
                    ..
                } => writes[ir.cell(*target) as usize] = true,
                Stmt::Alias { target, source } => {
                    writes[ir.cell(*target) as usize] = true;
                    writes[ir.cell(*source) as usize] = true;
                }
                Stmt::Havoc { target: None, .. } => writes.fill(true),
                Stmt::MakeClosure(_) | Stmt::Use(_) => {}
            }
        }
        let mut has_writer = vec![false; cells];
        let closure_writes: Vec<Vec<u32>> = ir
            .closures
            .iter()
            .zip(&closures)
            .map(|(closure, child)| {
                let inner = &closure.ir;
                inner
                    .bindings
                    .iter()
                    .enumerate()
                    .filter_map(|(binding, b)| match b.origin {
                        BindingOrigin::Captured(capture)
                            if capture.mode == CaptureMode::ByRef
                                && child.writes[inner.cells[binding] as usize] =>
                        {
                            Some(ir.cell(capture.outer))
                        }
                        _ => None,
                    })
                    .collect()
            })
            .collect();
        for &cell in closure_writes.iter().flatten() {
            writes[cell as usize] = true;
            has_writer[cell as usize] = true;
        }

        let mut need = vec![false; cells];
        let demand = |binding: BindingId, need: &mut [bool]| {
            let cell = ir.cell(binding) as usize;
            if !aliased[cell] && pinned(policy, ir, binding).is_none() && !need[cell] {
                need[cell] = true;
                return true;
            }
            false
        };
        for u in &ir.uses {
            demand(u.binding, &mut need);
        }
        let mut needs_writes_after = false;
        for (closure, child) in ir.closures.iter().zip(&closures) {
            let inner = &closure.ir;
            for (binding, b) in inner.bindings.iter().enumerate() {
                let BindingOrigin::Captured(capture) = b.origin else {
                    continue;
                };
                if !child.demanded[inner.cells[binding] as usize] {
                    continue;
                }
                let outer_cell = ir.cell(capture.outer) as usize;
                let by_ref = capture.mode == CaptureMode::ByRef;
                if by_ref && has_writer[outer_cell] {
                    continue;
                }
                demand(capture.outer, &mut need);
                needs_writes_after |= by_ref && !aliased[outer_cell];
            }
        }
        // Linear in the statements: each copy edge is followed once, from
        // the cell that first demands its target.
        let mut copies: Vec<(u32, BindingId)> = stmts(ir)
            .filter_map(|stmt| match stmt {
                Stmt::Assign { target, value } => {
                    copy_source(value).map(|source| (ir.cell(*target), source))
                }
                _ => None,
            })
            .collect();
        if !copies.is_empty() {
            copies.sort_unstable_by_key(|&(target, _)| target);
            let mut work: Vec<u32> = (0..cells as u32).filter(|&c| need[c as usize]).collect();
            while let Some(cell) = work.pop() {
                let from = copies.partition_point(|&(target, _)| target < cell);
                for &(_, source) in copies[from..].iter().take_while(|&&(t, _)| t == cell) {
                    if demand(source, &mut need) {
                        work.push(ir.cell(source));
                    }
                }
            }
        }

        let mut slot = vec![UNTRACKED; cells];
        let mut tracked = Vec::new();
        for cell in (0..cells).filter(|&cell| need[cell] && writes[cell]) {
            slot[cell] = tracked.len() as u32;
            tracked.push(cell as u32);
        }
        Plan {
            aliased,
            cell_binding,
            writes,
            has_writer,
            closure_writes,
            demanded: need,
            slot,
            tracked,
            needs_writes_after,
            closures,
        }
    }
}

/// One function's run: the IR, its plan, the states its captures enter
/// with, and the states it reads.
struct Run<'a> {
    ir: &'a FunctionIr,
    plan: &'a Plan,
    policy: LangPolicy,
    captured: &'a [(BindingId, Abstract)],
}

impl Run<'_> {
    fn slot_of(&self, binding: BindingId) -> Option<usize> {
        match self.plan.slot[self.ir.cell(binding) as usize] {
            UNTRACKED => None,
            slot => Some(slot as usize),
        }
    }

    fn read(&self, binding: BindingId, state: &[Abstract]) -> Abstract {
        if let Some(declared) = pinned(self.policy, self.ir, binding) {
            return Abstract::Known(declared);
        }
        let cell = self.ir.cell(binding) as usize;
        if self.plan.aliased[cell] {
            return Abstract::Unknown(UnknownCause::Escape);
        }
        match self.slot_of(binding) {
            Some(slot) => state[slot],
            None if !self.plan.writes[cell] => self.entry_state(binding, self.captured),
            // A written cell the plan did not demand: no evidence.
            None => Abstract::Unknown(UnknownCause::NoEvidence),
        }
    }

    fn eval(&self, value: &Value, state: &[Abstract]) -> Abstract {
        match value {
            Value::Type(ty) => Abstract::Known(*ty),
            Value::Copy(source) => self.read(*source, state),
            Value::Dynamic | Value::Unknown => Abstract::Unknown(UnknownCause::NoEvidence),
            Value::Annotated { declared, .. } => Abstract::Known(*declared),
        }
    }

    fn assigned(&self, target: BindingId, value: &Value, state: &[Abstract]) -> Abstract {
        match (self.policy.declared, self.ir.binding(target).declared) {
            (DeclaredTypePolicy::Pinned, Some(declared)) => Abstract::Known(declared),
            (DeclaredTypePolicy::PinnedUnlessDynamic, Some(declared)) => match value {
                Value::Dynamic => Abstract::Unknown(UnknownCause::NoEvidence),
                _ => Abstract::Known(declared),
            },
            _ => self.eval(value, state),
        }
    }

    fn havoc(&self, binding: BindingId, before: Abstract, cause: UnknownCause) -> Abstract {
        if cause == UnknownCause::Budget {
            return Abstract::Unknown(UnknownCause::Budget);
        }
        if before.is_sticky() {
            return before;
        }
        match pinned(self.policy, self.ir, binding) {
            Some(declared) => Abstract::Known(declared),
            None => Abstract::Unknown(cause),
        }
    }

    /// Applies `stmt` to `state`.
    fn transfer(&self, stmt: &Stmt, state: &mut [Abstract]) {
        match stmt {
            Stmt::Assign { target, value } => {
                if let Some(slot) = self.slot_of(*target) {
                    if !state[slot].is_sticky() {
                        state[slot] = self.assigned(*target, value, state);
                    }
                }
            }
            Stmt::Havoc {
                target: Some(target),
                cause,
            } => {
                if let Some(slot) = self.slot_of(*target) {
                    state[slot] = self.havoc(*target, state[slot], *cause);
                }
            }
            Stmt::Havoc {
                target: None,
                cause,
            } => {
                for (slot, &cell) in self.plan.tracked.iter().enumerate() {
                    let binding = self.plan.cell_binding[cell as usize];
                    state[slot] = self.havoc(binding, state[slot], *cause);
                }
            }
            Stmt::MakeClosure(closure) => {
                for &cell in &self.plan.closure_writes[closure.0 as usize] {
                    let slot = self.plan.slot[cell as usize];
                    if slot != UNTRACKED {
                        state[slot as usize] = Abstract::Unknown(UnknownCause::Escape);
                    }
                }
            }
            Stmt::Alias { .. } | Stmt::Use(_) => {}
        }
    }

    fn entry_state(&self, binding: BindingId, captured: &[(BindingId, Abstract)]) -> Abstract {
        let b = self.ir.binding(binding);
        match b.origin {
            BindingOrigin::Param => b
                .declared
                .map_or(Abstract::Unknown(UnknownCause::NoEvidence), Abstract::Known),
            BindingOrigin::Free => Abstract::Unknown(UnknownCause::NoEvidence),
            BindingOrigin::Captured(_) => captured.iter().find(|(id, _)| *id == binding).map_or(
                Abstract::Unknown(UnknownCause::NoEvidence),
                |&(_, state)| state,
            ),
            BindingOrigin::Local
            | BindingOrigin::ForEach
            | BindingOrigin::CatchParam
            | BindingOrigin::Pattern => Abstract::Unbound,
        }
    }
}

fn successors(block: &Block) -> impl Iterator<Item = BlockId> + '_ {
    block.term.successors().iter().copied().chain(block.handler)
}

/// Blocks reachable from the entry, in reverse post-order. Handler edges count.
fn reverse_post_order(ir: &FunctionIr) -> Vec<u32> {
    let mut seen = vec![false; ir.blocks.len()];
    let mut post = Vec::with_capacity(ir.blocks.len());
    let mut stack: Vec<(u32, usize)> = vec![(ir.entry.0, 0)];
    seen[ir.entry.0 as usize] = true;
    while let Some(&(block, next)) = stack.last() {
        match successors(&ir.blocks[block as usize]).nth(next) {
            Some(succ) => {
                if let Some(top) = stack.last_mut() {
                    top.1 = next + 1;
                }
                if !seen[succ.0 as usize] {
                    seen[succ.0 as usize] = true;
                    stack.push((succ.0, 0));
                }
            }
            None => {
                post.push(block);
                stack.pop();
            }
        }
    }
    post.reverse();
    post
}

/// Joins `state` into the in-state row of `block`; true when the row rose.
fn join_into(ins: &mut [Abstract], width: usize, block: BlockId, state: &[Abstract]) -> bool {
    let start = block.0 as usize * width;
    let mut changed = false;
    for (into, &from) in ins[start..start + width].iter_mut().zip(state) {
        let joined = join(*into, from);
        if joined != *into {
            *into = joined;
            changed = true;
        }
    }
    changed
}

fn charge(steps: &mut u32, width: usize) -> Result<(), OverBudget> {
    *steps = steps.saturating_add(width.max(1) as u32);
    if *steps > MAX_STEPS {
        Err(OverBudget)
    } else {
        Ok(())
    }
}

/// A write to a tracked slot, logged for the by-ref captures that read
/// every value assigned after their closure's creation.
struct Write {
    block: u32,
    stmt: u32,
    slot: u32,
    value: Abstract,
}

/// Where a closure is created (block, statement) and the capture states it
/// sees there.
type ClosureCapture = (u32, u32, Vec<(BindingId, Abstract)>);

fn solve_function(
    ir: &FunctionIr,
    plan: &Plan,
    policy: LangPolicy,
    captured: &[(BindingId, Abstract)],
    steps: &mut u32,
    out: &mut Vec<(usize, Abstract)>,
) -> Result<(), OverBudget> {
    let width = plan.tracked.len();
    let blocks = ir.blocks.len();
    // With no tracked cell the pass is one replay, linear in the blocks, so
    // only a tracked state pays the blocks x cells budget.
    if width > MAX_TRACKED_CELLS || (width > 0 && blocks > MAX_BLOCKS) {
        return Err(OverBudget);
    }
    let run = Run {
        ir,
        plan,
        policy,
        captured,
    };
    let order = reverse_post_order(ir);
    let mut live = vec![false; blocks];
    for &block in &order {
        live[block as usize] = true;
    }

    let mut ins = vec![Abstract::Unreachable; blocks * width];
    let mut state = vec![Abstract::Unreachable; width];
    if width > 0 {
        for (slot, &cell) in plan.tracked.iter().enumerate() {
            let binding = plan.cell_binding[cell as usize];
            ins[ir.entry.0 as usize * width + slot] = run.entry_state(binding, captured);
        }
        let mut pending = vec![false; blocks];
        pending[ir.entry.0 as usize] = true;
        let mut visited = true;
        while visited {
            visited = false;
            for &block in &order {
                if !pending[block as usize] {
                    continue;
                }
                pending[block as usize] = false;
                visited = true;
                charge(steps, width)?;
                let start = block as usize * width;
                state.copy_from_slice(&ins[start..start + width]);
                let b = &ir.blocks[block as usize];
                for stmt in &b.stmts {
                    if let Some(handler) = b.handler {
                        if join_into(&mut ins, width, handler, &state) {
                            pending[handler.0 as usize] = true;
                        }
                    }
                    run.transfer(stmt, &mut state);
                }
                for succ in successors(b) {
                    if join_into(&mut ins, width, succ, &state) {
                        pending[succ.0 as usize] = true;
                    }
                }
            }
        }
    }

    // The final replay records each use and each closure's capture states
    // from the converged in-states.
    let mut writes: Vec<Write> = Vec::new();
    let mut made: Vec<Option<ClosureCapture>> = vec![None; ir.closures.len()];
    for (block, b) in ir.blocks.iter().enumerate() {
        if !live[block] {
            out.extend(b.stmts.iter().filter_map(|stmt| match stmt {
                Stmt::Use(u) => Some((ir.uses[u.0 as usize].node, Abstract::Unreachable)),
                _ => None,
            }));
            continue;
        }
        charge(steps, width)?;
        let start = block * width;
        state.copy_from_slice(&ins[start..start + width]);
        for (index, stmt) in b.stmts.iter().enumerate() {
            match stmt {
                Stmt::Use(u) => {
                    let u = ir.uses[u.0 as usize];
                    out.push((u.node, run.read(u.binding, &state)));
                }
                Stmt::MakeClosure(closure) => {
                    let entries = capture_states(&run, closure.0 as usize, &state);
                    made[closure.0 as usize] = Some((block as u32, index as u32, entries));
                }
                Stmt::Assign { .. } | Stmt::Alias { .. } | Stmt::Havoc { .. } => {}
            }
            run.transfer(stmt, &mut state);
            if plan.needs_writes_after {
                log_writes(&run, stmt, block as u32, index as u32, &state, &mut writes);
            }
        }
    }

    for (index, closure) in ir.closures.iter().enumerate() {
        let entries = match made[index].take() {
            Some((block, stmt, mut entries)) => {
                if plan.needs_writes_after {
                    add_writes_after(&run, index, BlockId(block), stmt, &writes, &mut entries);
                }
                entries
            }
            None => Vec::new(),
        };
        solve_function(
            &closure.ir,
            &plan.closures[index],
            policy,
            &entries,
            steps,
            out,
        )?;
    }
    Ok(())
}

/// The entry state of each tracked capture of closure `index`, as seen at
/// its creation. By-ref captures get the later writes in `add_writes_after`.
fn capture_states(run: &Run<'_>, index: usize, state: &[Abstract]) -> Vec<(BindingId, Abstract)> {
    let inner = &run.ir.closures[index].ir;
    let child = &run.plan.closures[index];
    inner
        .bindings
        .iter()
        .enumerate()
        .filter_map(|(binding, b)| {
            let BindingOrigin::Captured(capture) = b.origin else {
                return None;
            };
            if !child.demanded[inner.cells[binding] as usize] {
                return None;
            }
            let outer_cell = run.ir.cell(capture.outer) as usize;
            let entry = if capture.mode == CaptureMode::ByRef && run.plan.has_writer[outer_cell] {
                Abstract::Unknown(UnknownCause::Escape)
            } else {
                run.read(capture.outer, state)
            };
            Some((BindingId(binding as u32), entry))
        })
        .collect()
}

fn log_writes(
    run: &Run<'_>,
    stmt: &Stmt,
    block: u32,
    index: u32,
    state: &[Abstract],
    writes: &mut Vec<Write>,
) {
    let mut log = |slot: usize| {
        writes.push(Write {
            block,
            stmt: index,
            slot: slot as u32,
            value: state[slot],
        })
    };
    match stmt {
        Stmt::Assign { target, .. }
        | Stmt::Havoc {
            target: Some(target),
            ..
        } => {
            if let Some(slot) = run.slot_of(*target) {
                log(slot);
            }
        }
        Stmt::Havoc { target: None, .. } => (0..run.plan.tracked.len()).for_each(&mut log),
        Stmt::MakeClosure(closure) => {
            for &cell in &run.plan.closure_writes[closure.0 as usize] {
                let slot = run.plan.slot[cell as usize];
                if slot != UNTRACKED {
                    log(slot as usize);
                }
            }
        }
        Stmt::Alias { .. } | Stmt::Use(_) => {}
    }
}

/// Joins into each by-ref capture of closure `index` every value its outer
/// cell receives at a point reachable from the closure's creation at
/// (`block`, `stmt`): the closure may run at any of them.
fn add_writes_after(
    run: &Run<'_>,
    index: usize,
    block: BlockId,
    stmt: u32,
    writes: &[Write],
    entries: &mut [(BindingId, Abstract)],
) {
    let inner = &run.ir.closures[index].ir;
    let mut reach: Option<Vec<bool>> = None;
    for (binding, entry) in entries.iter_mut() {
        let BindingOrigin::Captured(capture) = inner.binding(*binding).origin else {
            continue;
        };
        if capture.mode != CaptureMode::ByRef || entry.is_sticky() {
            continue;
        }
        let Some(slot) = run.slot_of(capture.outer) else {
            continue;
        };
        let reach = reach.get_or_insert_with(|| reachable_after(run.ir, block));
        for write in writes {
            let after =
                reach[write.block as usize] || (write.block == block.0 && write.stmt > stmt);
            if after && write.slot as usize == slot {
                *entry = join(*entry, write.value);
            }
        }
    }
}

/// Blocks reachable from the successors (and handler) of `block`.
fn reachable_after(ir: &FunctionIr, block: BlockId) -> Vec<bool> {
    let mut seen = vec![false; ir.blocks.len()];
    let mut stack: Vec<BlockId> = successors(&ir.blocks[block.0 as usize]).collect();
    while let Some(next) = stack.pop() {
        if std::mem::replace(&mut seen[next.0 as usize], true) {
            continue;
        }
        stack.extend(successors(&ir.blocks[next.0 as usize]));
    }
    seen
}
