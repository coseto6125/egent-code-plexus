//! Receiver core: lattice properties, solver semantics on hand-lowered IR,
//! capture rules, and the budget answer through `analyze_file`.

use ecp_analyzer::receiver::ir::{
    BindingOrigin, CaptureMode, ScopeKind, TypeId, TypeOrigin, TypeRef, Value,
};
use ecp_analyzer::receiver::lattice::{join, Abstract, UnknownCause};
use ecp_analyzer::receiver::solve::{solve, MAX_BLOCKS, MAX_TRACKED_CELLS};
use ecp_analyzer::receiver::{
    analyze_file, DeclaredTypePolicy, IrBuilder, LangPolicy, Lowered, Lowering, Receiver,
    ScopeRule, RECEIVER_BUDGET_KIND,
};
use ecp_core::analyzer::types::{CallSite, MemberRead, ReadIntent};
use std::path::Path;
use tree_sitter::{Node, Parser, Tree};

const SPAN: (u32, u32, u32, u32) = (0, 0, 0, 0);

const DUCK: LangPolicy = LangPolicy {
    declared: DeclaredTypePolicy::PointOfAssignment,
    scope: ScopeRule::Function,
};
const PINNED: LangPolicy = LangPolicy {
    declared: DeclaredTypePolicy::Pinned,
    scope: ScopeRule::Block,
};
const TYPESCRIPT: LangPolicy = LangPolicy {
    declared: DeclaredTypePolicy::PinnedUnlessDynamic,
    scope: ScopeRule::Block,
};

const CONFLICT: Abstract = Abstract::Unknown(UnknownCause::Conflict);
const ESCAPE: Abstract = Abstract::Unknown(UnknownCause::Escape);
const NO_EVIDENCE: Abstract = Abstract::Unknown(UnknownCause::NoEvidence);

/// The solved state of the use recorded at `node`.
fn state_at(b: IrBuilder, node: usize) -> Abstract {
    let policy = b.policy();
    let ir = b.finish();
    let solved = solve(&ir, policy);
    assert!(!solved.over_budget, "unexpected budget answer");
    solved
        .uses
        .iter()
        .find(|(n, _)| *n == node)
        .map(|&(_, state)| state)
        .unwrap_or_else(|| panic!("no use at node {node}: {:?}", solved.uses))
}

/// `if c { then }`, continuing at the join.
fn if_then(b: &mut IrBuilder, then: impl FnOnce(&mut IrBuilder)) {
    let then_block = b.new_block();
    let join_block = b.new_block();
    b.branch(&[then_block, join_block]);
    b.switch_to(then_block);
    then(b);
    b.goto(join_block);
    b.switch_to(join_block);
}

// ---- lattice ----

fn lattice_elements() -> Vec<Abstract> {
    let mut all = vec![Abstract::Unreachable, Abstract::Unbound];
    all.extend((0..3).map(|t| Abstract::Known(TypeId(t))));
    all.extend(
        UnknownCause::ALL
            .iter()
            .map(|&cause| Abstract::Unknown(cause)),
    );
    all
}

#[test]
fn test_join_every_pair_is_commutative_associative_idempotent() {
    let all = lattice_elements();
    for &a in &all {
        assert_eq!(join(a, a), a, "idempotent: {a:?}");
        for &b in &all {
            assert_eq!(join(a, b), join(b, a), "commutative: {a:?} {b:?}");
            assert_eq!(
                join(join(a, b), a),
                join(a, b),
                "a join only moves up: {a:?} {b:?}"
            );
            for &c in &all {
                assert_eq!(
                    join(join(a, b), c),
                    join(a, join(b, c)),
                    "associative: {a:?} {b:?} {c:?}"
                );
            }
        }
    }
    assert_eq!(
        join(Abstract::Unbound, Abstract::Known(TypeId(0))),
        CONFLICT
    );
    assert_eq!(join(Abstract::Known(TypeId(0)), NO_EVIDENCE), CONFLICT);
    assert_eq!(join(Abstract::Unbound, NO_EVIDENCE), NO_EVIDENCE);
    assert_eq!(join(Abstract::Known(TypeId(0)), ESCAPE), ESCAPE);
}

// ---- random acyclic CFGs against path enumeration ----

struct XorShift(u64);

impl XorShift {
    fn below(&mut self, n: usize) -> usize {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x % n as u64) as usize
    }
}

#[derive(Debug, Clone, Copy)]
enum Gen {
    /// `x = T<n>()`, or `x = call()` for `None`.
    Set(Option<u32>),
    Use,
}

#[derive(Debug)]
struct GenBlock {
    stmts: Vec<Gen>,
    succs: Vec<usize>,
}

fn generate(rng: &mut XorShift) -> Vec<GenBlock> {
    let n = 1 + rng.below(8);
    let mut blocks: Vec<GenBlock> = (0..n)
        .map(|i| {
            let stmts = (0..rng.below(3))
                .map(|_| match rng.below(4) {
                    3 => Gen::Set(None),
                    t => Gen::Set(Some(t as u32)),
                })
                .collect();
            let succs = if i + 1 == n {
                Vec::new()
            } else {
                let shape = rng.below(3);
                let mut forward = || i + 1 + rng.below(n - i - 1);
                match shape {
                    0 => Vec::new(),
                    1 => vec![forward()],
                    _ => vec![forward(), forward()],
                }
            };
            GenBlock { stmts, succs }
        })
        .collect();
    let at = rng.below(n);
    let position = rng.below(blocks[at].stmts.len() + 1);
    blocks[at].stmts.insert(position, Gen::Use);
    blocks
}

fn enumerate_paths(blocks: &[GenBlock], block: usize, mut state: Abstract, acc: &mut Abstract) {
    for stmt in &blocks[block].stmts {
        match stmt {
            Gen::Use => *acc = join(*acc, state),
            Gen::Set(Some(t)) => state = Abstract::Known(TypeId(*t)),
            Gen::Set(None) => state = NO_EVIDENCE,
        }
    }
    for &succ in &blocks[block].succs {
        enumerate_paths(blocks, succ, state, acc);
    }
}

#[test]
fn test_solve_random_acyclic_cfgs_match_path_enumeration() {
    for seed in 1..=600u64 {
        let mut rng = XorShift(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let blocks = generate(&mut rng);
        let mut b = IrBuilder::new(DUCK, SPAN);
        let types: Vec<TypeId> = (0..3)
            .map(|t| b.intern(&format!("T{t}"), TypeOrigin::Construction))
            .collect();
        let (origin, declared, entry) = match rng.below(3) {
            0 => (BindingOrigin::Local, None, Abstract::Unbound),
            1 => (BindingOrigin::Param, None, NO_EVIDENCE),
            _ => (
                BindingOrigin::Param,
                Some(types[0]),
                Abstract::Known(types[0]),
            ),
        };
        let x = b.declare("x", origin, declared);
        let entry_block = b.current_block().expect("the entry block is open");
        let ids: Vec<_> = std::iter::once(entry_block)
            .chain((1..blocks.len()).map(|_| b.new_block()))
            .collect();
        for (i, block) in blocks.iter().enumerate() {
            b.switch_to(ids[i]);
            for stmt in &block.stmts {
                match stmt {
                    Gen::Set(Some(t)) => b.assign(x, Value::Type(types[*t as usize])),
                    Gen::Set(None) => b.assign(x, Value::Unknown),
                    Gen::Use => {
                        b.use_binding(x, 1);
                    }
                }
            }
            match block.succs.as_slice() {
                [] => b.ret(),
                [only] => b.goto(ids[*only]),
                many => b.branch(&many.iter().map(|&s| ids[s]).collect::<Vec<_>>()),
            }
        }
        let mut expected = Abstract::Unreachable;
        enumerate_paths(&blocks, 0, entry, &mut expected);
        assert_eq!(state_at(b, 1), expected, "seed {seed}: {blocks:?}");
    }
}

// ---- branches ----

/// A02 / M04: `obj = loose; if c: obj: Cls = Cls()` with the use in the
/// `else` branch or after the join; `swap` writes the branches the other
/// way round.
fn annotated_branch(swap: bool, use_in_else: bool) -> Abstract {
    let mut b = IrBuilder::new(DUCK, SPAN);
    let cls = b.intern("Cls", TypeOrigin::Annotation);
    let loose = b.declare("loose", BindingOrigin::Param, None);
    let obj = b.declare("obj", BindingOrigin::Local, None);
    b.assign(obj, Value::Copy(loose));
    let (annotated, other) = if swap {
        let other = b.new_block();
        (b.new_block(), other)
    } else {
        let annotated = b.new_block();
        (annotated, b.new_block())
    };
    let join_block = b.new_block();
    let targets = if swap {
        [other, annotated]
    } else {
        [annotated, other]
    };
    b.branch(&targets);
    let lower_annotated = |b: &mut IrBuilder| {
        b.switch_to(annotated);
        b.assign(
            obj,
            Value::Annotated {
                declared: cls,
                value: Box::new(Value::Type(cls)),
            },
        );
        b.goto(join_block);
    };
    let lower_other = |b: &mut IrBuilder| {
        b.switch_to(other);
        if use_in_else {
            b.use_binding(obj, 1);
            b.ret();
        } else {
            b.goto(join_block);
        }
    };
    if swap {
        lower_other(&mut b);
        lower_annotated(&mut b);
    } else {
        lower_annotated(&mut b);
        lower_other(&mut b);
    }
    b.switch_to(join_block);
    if !use_in_else {
        b.use_binding(obj, 1);
    }
    b.ret();
    state_at(b, 1)
}

#[test]
fn test_solve_branch_order_swapped_returns_same_state() {
    for use_in_else in [true, false] {
        assert_eq!(
            annotated_branch(false, use_in_else),
            annotated_branch(true, use_in_else),
            "use_in_else={use_in_else}"
        );
    }
    assert_eq!(annotated_branch(false, true), NO_EVIDENCE);
    assert_eq!(annotated_branch(false, false), CONFLICT);
}

#[test]
fn test_solve_annotation_on_sibling_path_returns_conflict() {
    let state = annotated_branch(false, false);
    assert_eq!(state, CONFLICT, "the annotation must not restore Known");

    // use_pos_branch: the use inside the annotated branch.
    let mut b = IrBuilder::new(DUCK, SPAN);
    let cls = b.intern("Cls", TypeOrigin::Annotation);
    let loose = b.declare("loose", BindingOrigin::Param, None);
    let obj = b.declare("obj", BindingOrigin::Local, None);
    b.assign(obj, Value::Copy(loose));
    if_then(&mut b, |b| {
        b.assign(
            obj,
            Value::Annotated {
                declared: cls,
                value: Box::new(Value::Type(cls)),
            },
        );
        b.use_binding(obj, 1);
        b.ret();
    });
    b.ret();
    assert_eq!(state_at(b, 1), Abstract::Known(cls));

    // use_pos_join: both branches annotate the same type.
    let mut b = IrBuilder::new(DUCK, SPAN);
    let cls = b.intern("Cls", TypeOrigin::Annotation);
    let obj = b.declare("obj", BindingOrigin::Local, None);
    let (then_block, else_block, join_block) = (b.new_block(), b.new_block(), b.new_block());
    b.branch(&[then_block, else_block]);
    for block in [then_block, else_block] {
        b.switch_to(block);
        b.assign(
            obj,
            Value::Annotated {
                declared: cls,
                value: Box::new(Value::Type(cls)),
            },
        );
        b.goto(join_block);
    }
    b.switch_to(join_block);
    b.use_binding(obj, 1);
    b.ret();
    assert_eq!(state_at(b, 1), Abstract::Known(cls));
}

/// M05 Python: `def run(c, obj: Cls, loose): if c: obj = loose [; return 0]`
/// then `return obj.m(1)`; `dominating` moves the write after the `if`.
fn write_and_join(leaves: bool, dominating: bool) -> (Abstract, TypeId) {
    let mut b = IrBuilder::new(DUCK, SPAN);
    let cls = b.intern("Cls", TypeOrigin::Annotation);
    let obj = b.declare("obj", BindingOrigin::Param, Some(cls));
    let loose = b.declare("loose", BindingOrigin::Param, None);
    if_then(&mut b, |b| {
        if !dominating {
            b.assign(obj, Value::Copy(loose));
        }
        if leaves {
            b.ret();
        }
    });
    if dominating {
        b.assign(obj, Value::Copy(loose));
    }
    b.use_binding(obj, 1);
    b.ret();
    (state_at(b, 1), cls)
}

#[test]
fn test_solve_write_on_leaving_branch_returns_known() {
    let (state, cls) = write_and_join(true, false);
    assert_eq!(state, Abstract::Known(cls));
    assert_eq!(write_and_join(false, false).0, CONFLICT, "one path unknown");
    assert_eq!(
        write_and_join(false, true).0,
        NO_EVIDENCE,
        "write dominates the use"
    );
}

/// `obj = Cls(); while c: obj.m(); obj = <rewrite>()` then `obj.m()` after the loop.
fn loop_case(rewrite: &str) -> (Abstract, Abstract, TypeId) {
    let mut b = IrBuilder::new(DUCK, SPAN);
    let cls = b.intern("Cls", TypeOrigin::Construction);
    let rewritten = b.intern(rewrite, TypeOrigin::Construction);
    let obj = b.declare("obj", BindingOrigin::Local, None);
    b.assign(obj, Value::Type(cls));
    let (head, body, exit) = (b.new_block(), b.new_block(), b.new_block());
    b.goto(head);
    b.switch_to(head);
    b.branch(&[body, exit]);
    b.switch_to(body);
    b.push_breakable(Some(head), exit);
    b.use_binding(obj, 1);
    b.assign(obj, Value::Type(rewritten));
    b.continue_loop();
    b.pop_breakable();
    b.switch_to(exit);
    b.use_binding(obj, 2);
    b.ret();
    let policy = b.policy();
    let ir = b.finish();
    let solved = solve(&ir, policy);
    let at = |node: usize| {
        solved
            .uses
            .iter()
            .find(|(n, _)| *n == node)
            .map(|&(_, s)| s)
            .expect("use recorded")
    };
    (at(1), at(2), cls)
}

#[test]
fn test_solve_loop_reaches_fixed_point() {
    let (in_body, after, _) = loop_case("Other");
    assert_eq!(
        in_body, CONFLICT,
        "the back edge carries Other into the body"
    );
    assert_eq!(after, CONFLICT);
    let (in_body, after, cls) = loop_case("Cls");
    assert_eq!(in_body, Abstract::Known(cls));
    assert_eq!(after, Abstract::Known(cls));
}

// ---- closures and captures ----

/// `$o = new A; $f = function () use ($o) { $o->m(); }; $o = new B; $o->m();`
fn capture_then_rewrite(mode: CaptureMode) -> (Abstract, Abstract, TypeId, TypeId) {
    let mut b = IrBuilder::new(DUCK, SPAN);
    let a = b.intern("A", TypeOrigin::Construction);
    let later = b.intern("B", TypeOrigin::Construction);
    let o = b.declare("o", BindingOrigin::Local, None);
    b.assign(o, Value::Type(a));
    b.closure(SPAN, |b| {
        let inner = b.capture("o", mode).expect("outer declares o");
        b.use_binding(inner, 1);
    });
    b.assign(o, Value::Type(later));
    b.use_binding(o, 2);
    b.ret();
    let policy = b.policy();
    let ir = b.finish();
    let solved = solve(&ir, policy);
    let at = |node: usize| {
        solved
            .uses
            .iter()
            .find(|(n, _)| *n == node)
            .map(|&(_, s)| s)
            .expect("use recorded")
    };
    (at(1), at(2), a, later)
}

#[test]
fn test_solve_by_value_capture_returns_state_at_creation() {
    let (inside, outside, a, later) = capture_then_rewrite(CaptureMode::ByValue);
    assert_eq!(inside, Abstract::Known(a));
    assert_eq!(outside, Abstract::Known(later));
    let (inside, _, _, _) = capture_then_rewrite(CaptureMode::ByRef);
    assert_eq!(
        inside, CONFLICT,
        "a by-ref capture also sees the later write"
    );
}

/// A01: `function (Cb $o, $a) { $f = function () use (&$o, $a) { $o->cb_ptr(); };
/// <sibling>; return $f(); }` with the use at node 1, an outer use before
/// the closures at node 3 and after them at node 2.
fn sibling_capture(sibling: impl FnOnce(&mut IrBuilder)) -> (Abstract, Abstract, Abstract, TypeId) {
    let mut b = IrBuilder::new(DUCK, SPAN);
    let cb = b.intern("Cb", TypeOrigin::Annotation);
    let o = b.declare("o", BindingOrigin::Param, Some(cb));
    b.declare("a", BindingOrigin::Param, None);
    let f = b.declare("f", BindingOrigin::Local, None);
    b.use_binding(o, 3);
    b.closure(SPAN, |b| {
        let o = b
            .capture("o", CaptureMode::ByRef)
            .expect("outer declares o");
        b.capture("a", CaptureMode::ByValue);
        b.use_binding(o, 1);
    });
    b.assign(f, Value::Unknown);
    sibling(&mut b);
    b.use_binding(o, 2);
    b.ret();
    let policy = b.policy();
    let ir = b.finish();
    let solved = solve(&ir, policy);
    let at = |node: usize| {
        solved
            .uses
            .iter()
            .find(|(n, _)| *n == node)
            .map(|&(_, s)| s)
            .expect("use recorded")
    };
    (at(1), at(2), at(3), cb)
}

#[test]
fn test_solve_read_only_by_ref_sibling_returns_known() {
    // A01-pos: `$g = function () use (&$o) { return $o; };`
    let (inside, after, before, cb) = sibling_capture(|b| {
        b.closure(SPAN, |b| {
            b.capture("o", CaptureMode::ByRef);
        });
    });
    assert_eq!(inside, Abstract::Known(cb));
    assert_eq!(after, Abstract::Known(cb));
    assert_eq!(before, Abstract::Known(cb));
    // M06 by value: `$g = function () use ($o) { return $o; };`
    let (inside, _, _, cb) = sibling_capture(|b| {
        b.closure(SPAN, |b| {
            b.capture("o", CaptureMode::ByValue);
        });
    });
    assert_eq!(inside, Abstract::Known(cb));
    // A by-value sibling that writes its own copy writes nothing outside.
    let (inside, after, _, cb) = sibling_capture(|b| {
        b.closure(SPAN, |b| {
            let o = b
                .capture("o", CaptureMode::ByValue)
                .expect("outer declares o");
            b.assign(o, Value::Unknown);
        });
    });
    assert_eq!(inside, Abstract::Known(cb));
    assert_eq!(after, Abstract::Known(cb));
}

#[test]
fn test_solve_writer_by_ref_sibling_returns_escape() {
    // A01-neg-write: `$g = function () use (&$o, $a) { $o = $a; };`
    let (inside, after, before, cb) = sibling_capture(|b| {
        b.closure(SPAN, |b| {
            let o = b
                .capture("o", CaptureMode::ByRef)
                .expect("outer declares o");
            let a = b
                .capture("a", CaptureMode::ByValue)
                .expect("outer declares a");
            b.assign(o, Value::Copy(a));
        });
    });
    assert_eq!(inside, ESCAPE);
    assert_eq!(
        after, ESCAPE,
        "the writer poisons the outer cell from its creation"
    );
    assert_eq!(
        before,
        Abstract::Known(cb),
        "the poison starts at the writer's creation"
    );
    // A01-neg-escape: `$g = function () use (&$o) { return take_ref($o); };`
    let (inside, _, _, _) = sibling_capture(|b| {
        b.closure(SPAN, |b| {
            let o = b
                .capture("o", CaptureMode::ByRef)
                .expect("outer declares o");
            b.havoc(Some(o), UnknownCause::Escape);
        });
    });
    assert_eq!(inside, ESCAPE);
    // A writer two closures deep still writes the outer cell.
    let (inside, _, _, _) = sibling_capture(|b| {
        b.closure(SPAN, |b| {
            b.closure(SPAN, |b| {
                let o = b
                    .lookup_or_capture("o", CaptureMode::ByRef)
                    .expect("an enclosing function declares o");
                b.assign(o, Value::Unknown);
            });
        });
    });
    assert_eq!(inside, ESCAPE);
}

#[test]
fn test_solve_alias_write_returns_unknown() {
    // A01-neg-alias: `$r = &$o; $r = $a;`
    let (inside, after, _, _) = sibling_capture(|b| {
        let o = b.lookup("o").expect("o declared");
        let a = b.lookup("a").expect("a declared");
        let r = b.declare("r", BindingOrigin::Local, None);
        b.alias(r, o);
        b.assign(r, Value::Copy(a));
    });
    assert_eq!(inside, ESCAPE);
    assert_eq!(after, ESCAPE);
    // Control: `$r = $a;` with no alias leaves `$o` alone.
    let (inside, after, _, cb) = sibling_capture(|b| {
        let a = b.lookup("a").expect("a declared");
        let r = b.declare("r", BindingOrigin::Local, None);
        b.assign(r, Value::Copy(a));
    });
    assert_eq!(inside, Abstract::Known(cb));
    assert_eq!(after, Abstract::Known(cb));
}

// ---- budget, through analyze_file ----

fn parse_python(source: &str) -> Tree {
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("python grammar loads");
    parser.parse(source, None).expect("python source parses")
}

fn find_identifier<'t>(node: Node<'t>, source: &[u8], name: &str) -> Option<Node<'t>> {
    if node.kind() == "identifier" && node.utf8_text(source).ok() == Some(name) {
        return Some(node);
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'t>> = node.children(&mut cursor).collect();
    children
        .into_iter()
        .find_map(|child| find_identifier(child, source, name))
}

fn receiver_of<'t>(function: Node<'t>, source: &[u8]) -> Option<Node<'t>> {
    find_identifier(function.child_by_field_name("body")?, source, "obj")
}

/// Lowers `def run(obj): return obj.m()` as an untyped parameter read
/// after `extra_blocks` empty blocks.
struct ChainLowering {
    extra_blocks: usize,
    /// Declares `obj` with this type under a `Pinned` policy.
    pinned: Option<&'static str>,
}

impl Lowering for ChainLowering {
    type Cx<'a> = ();

    fn policy(&self) -> LangPolicy {
        if self.pinned.is_some() {
            PINNED
        } else {
            DUCK
        }
    }

    fn is_function(&self, node: Node<'_>) -> bool {
        node.kind() == "function_definition"
    }

    fn lower(
        &self,
        _cx: &Self::Cx<'_>,
        function: Node<'_>,
        source: &[u8],
        b: &mut IrBuilder,
    ) -> Lowered {
        let Some(receiver) = receiver_of(function, source) else {
            return Lowered::NoUses;
        };
        let declared = self.pinned.map(|ty| b.intern(ty, TypeOrigin::Annotation));
        let obj = b.declare("obj", BindingOrigin::Param, declared);
        for _ in 0..self.extra_blocks {
            let next = b.new_block();
            b.goto(next);
            b.switch_to(next);
        }
        b.use_binding(obj, receiver.id());
        Lowered::Done
    }
}

#[test]
fn test_solve_over_budget_returns_budget_and_blind_spot() {
    let source = "def run(obj):\n    return obj.m()\n";
    let tree = parse_python(source);
    let function = tree
        .root_node()
        .named_child(0)
        .expect("function_definition");
    let receiver = receiver_of(function, source.as_bytes()).expect("obj receiver");
    let file = Path::new("pkg/run.py");

    let over = ChainLowering {
        extra_blocks: MAX_BLOCKS + 1,
        pinned: None,
    };
    let receivers = analyze_file(&over, &(), tree.root_node(), source.as_bytes(), file);
    assert_eq!(
        receivers.receiver_at(receiver),
        Some(&Receiver::Unknown(UnknownCause::Budget))
    );
    assert_eq!(
        receivers.blind_spots.len(),
        1,
        "{:?}",
        receivers.blind_spots
    );
    let spot = &receivers.blind_spots[0];
    assert_eq!(spot.kind, RECEIVER_BUDGET_KIND);
    assert_eq!(spot.file_path, file);
    let (start, end) = (function.start_position(), function.end_position());
    assert_eq!(
        spot.span,
        (
            start.row as u32,
            start.column as u32,
            end.row as u32,
            end.column as u32
        )
    );
    assert!(spot.hint.contains("`run`"), "{}", spot.hint);
    assert!(!spot.is_test);

    let within = ChainLowering {
        extra_blocks: 0,
        pinned: None,
    };
    let receivers = analyze_file(&within, &(), tree.root_node(), source.as_bytes(), file);
    assert_eq!(
        receivers.receiver_at(receiver),
        Some(&Receiver::Unknown(UnknownCause::NoEvidence))
    );
    assert!(receivers.blind_spots.is_empty());
}

#[test]
fn test_solve_pinned_uses_over_block_budget_keep_declared_type() {
    // A pinned use needs no tracked cell, so the replay is linear in the
    // blocks; a huge generated method keeps its typed receivers.
    let source = "def run(obj):\n    return obj.m()\n";
    let tree = parse_python(source);
    let function = tree
        .root_node()
        .named_child(0)
        .expect("function_definition");
    let receiver = receiver_of(function, source.as_bytes()).expect("obj receiver");
    let lowering = ChainLowering {
        extra_blocks: MAX_BLOCKS + 1,
        pinned: Some("A"),
    };
    let receivers = analyze_file(
        &lowering,
        &(),
        tree.root_node(),
        source.as_bytes(),
        Path::new("pkg/run.py"),
    );
    assert_eq!(
        receivers.receiver_at(receiver),
        Some(&Receiver::Known(TypeRef {
            text: "A".into(),
            origin: TypeOrigin::Annotation,
        }))
    );
    assert!(
        receivers.blind_spots.is_empty(),
        "{:?}",
        receivers.blind_spots
    );
}

#[test]
fn test_analyze_file_function_without_uses_records_nothing() {
    let source = "def run(x):\n    return 1\n";
    let tree = parse_python(source);
    let receivers = analyze_file(
        &ChainLowering {
            extra_blocks: 0,
            pinned: None,
        },
        &(),
        tree.root_node(),
        source.as_bytes(),
        Path::new("run.py"),
    );
    assert!(receivers.is_empty());
    assert!(receivers.blind_spots.is_empty());
}

/// Records two uses on one receiver node: `obj = A(); use; obj = <second>(); use`.
struct TwoUsesLowering {
    second: &'static str,
}

impl Lowering for TwoUsesLowering {
    type Cx<'a> = ();

    fn policy(&self) -> LangPolicy {
        DUCK
    }

    fn is_function(&self, node: Node<'_>) -> bool {
        node.kind() == "function_definition"
    }

    fn lower(
        &self,
        _cx: &Self::Cx<'_>,
        function: Node<'_>,
        source: &[u8],
        b: &mut IrBuilder,
    ) -> Lowered {
        let Some(receiver) = receiver_of(function, source) else {
            return Lowered::NoUses;
        };
        let a = b.intern("A", TypeOrigin::Construction);
        let second = b.intern(self.second, TypeOrigin::Construction);
        let obj = b.declare("obj", BindingOrigin::Local, None);
        b.assign(obj, Value::Type(a));
        b.use_binding(obj, receiver.id());
        b.assign(obj, Value::Type(second));
        b.use_binding(obj, receiver.id());
        Lowered::Done
    }
}

#[test]
fn test_analyze_file_two_uses_of_one_node_join_their_states() {
    let source = "def run():\n    return obj.m()\n";
    let tree = parse_python(source);
    let function = tree
        .root_node()
        .named_child(0)
        .expect("function_definition");
    let receiver = receiver_of(function, source.as_bytes()).expect("obj receiver");
    let analyze = |second| {
        analyze_file(
            &TwoUsesLowering { second },
            &(),
            tree.root_node(),
            source.as_bytes(),
            Path::new("run.py"),
        )
    };
    assert_eq!(
        analyze("B").receiver_at(receiver),
        Some(&Receiver::Unknown(UnknownCause::Conflict))
    );
    let same = analyze("A");
    assert_eq!(
        same.receiver_at(receiver),
        Some(&Receiver::Known(TypeRef {
            text: "A".into(),
            origin: TypeOrigin::Construction,
        }))
    );
    assert_eq!(
        same.receiver_at(function),
        None,
        "no use at the function node"
    );
}

// ---- declared types ----

/// M05 Java: `void run(boolean c, A obj) { if (c) { obj = make(); } obj.m(); }`.
fn declared_param_rewritten(policy: LangPolicy, value: Value, leaves: bool) -> (Abstract, TypeId) {
    let mut b = IrBuilder::new(policy, SPAN);
    let a = b.intern("A", TypeOrigin::Annotation);
    let obj = b.declare("obj", BindingOrigin::Param, Some(a));
    if_then(&mut b, |b| {
        b.assign(obj, value);
        if leaves {
            b.ret();
        }
    });
    b.use_binding(obj, 1);
    b.ret();
    (state_at(b, 1), a)
}

#[test]
fn test_solve_pinned_declared_type_survives_reassignment() {
    let (state, a) = declared_param_rewritten(PINNED, Value::Unknown, false);
    assert_eq!(state, Abstract::Known(a));
    let (state, _) = declared_param_rewritten(PINNED, Value::Dynamic, false);
    assert_eq!(state, Abstract::Known(a));
    // The same body where the annotation types only the point of assignment.
    let (state, _) = declared_param_rewritten(DUCK, Value::Unknown, false);
    assert_eq!(state, CONFLICT);
    // A havoc keeps the pinned type.
    let mut b = IrBuilder::new(PINNED, SPAN);
    let a = b.intern("A", TypeOrigin::Annotation);
    let obj = b.declare("obj", BindingOrigin::Param, Some(a));
    b.havoc(Some(obj), UnknownCause::Unsupported);
    b.use_binding(obj, 1);
    assert_eq!(state_at(b, 1), Abstract::Known(a));
}

#[test]
fn test_solve_dynamic_assignment_under_pinned_unless_dynamic_returns_conflict_at_join() {
    // `function run(c: boolean, obj: Cls, loose: any) { if (c) { obj = loose; } return obj.m(1); }`
    let (state, cls) = declared_param_rewritten(TYPESCRIPT, Value::Dynamic, false);
    assert_eq!(state, CONFLICT);
    // Same with `return;` inside the `if`.
    let (state, _) = declared_param_rewritten(TYPESCRIPT, Value::Dynamic, true);
    assert_eq!(state, Abstract::Known(cls));
    // A non-dynamic value keeps the declared type.
    let (state, _) = declared_param_rewritten(TYPESCRIPT, Value::Unknown, false);
    assert_eq!(state, Abstract::Known(cls));
}

// ---- exceptions ----

/// `obj = A(); try { obj = <second>(); } catch { obj.m() }; obj.m()`.
fn try_case(second: &str) -> (Abstract, Abstract, TypeId) {
    let mut b = IrBuilder::new(DUCK, SPAN);
    let a = b.intern("A", TypeOrigin::Construction);
    let second = b.intern(second, TypeOrigin::Construction);
    let obj = b.declare("obj", BindingOrigin::Local, None);
    b.assign(obj, Value::Type(a));
    let handler = b.new_block();
    let after = b.new_block();
    b.enter_try(handler);
    b.assign(obj, Value::Type(second));
    b.leave_try();
    b.goto(after);
    b.switch_to(handler);
    b.push_scope(ScopeKind::Catch);
    b.use_binding(obj, 1);
    b.pop_scope();
    b.goto(after);
    b.switch_to(after);
    b.use_binding(obj, 2);
    b.ret();
    let policy = b.policy();
    let ir = b.finish();
    let solved = solve(&ir, policy);
    let at = |node: usize| {
        solved
            .uses
            .iter()
            .find(|(n, _)| *n == node)
            .map(|&(_, s)| s)
            .expect("use recorded")
    };
    (at(1), at(2), a)
}

#[test]
fn test_solve_handler_entry_joins_states_inside_try() {
    let (in_handler, after, _) = try_case("B");
    assert_eq!(
        in_handler, CONFLICT,
        "the throw may come before or after the write"
    );
    assert_eq!(after, CONFLICT);
    let (in_handler, after, a) = try_case("A");
    assert_eq!(in_handler, Abstract::Known(a));
    assert_eq!(after, Abstract::Known(a));
}

// ---- builder scopes ----

#[test]
fn test_builder_block_scoped_same_name_locals_resolve_own_binding() {
    let mut b = IrBuilder::new(PINNED, SPAN);
    b.push_scope(ScopeKind::Block);
    let first = b.declare("x", BindingOrigin::Local, None);
    assert_eq!(b.lookup("x"), Some(first));
    b.pop_scope();
    assert_eq!(b.lookup("x"), None);
    b.push_scope(ScopeKind::Block);
    let second = b.declare("x", BindingOrigin::Local, None);
    assert_ne!(first, second);
    assert_eq!(b.lookup("x"), Some(second));
    b.pop_scope();

    // Function-scoped languages keep a block's declaration after the block.
    let mut b = IrBuilder::new(DUCK, SPAN);
    b.push_scope(ScopeKind::Block);
    let x = b.declare("x", BindingOrigin::Local, None);
    b.pop_scope();
    assert_eq!(b.lookup("x"), Some(x));
}

// ---- occurrence entries ----

#[test]
fn test_receiver_call_entry_encodes_known_untyped_and_unproven() {
    let known = Receiver::Known(TypeRef {
        text: "Cls".into(),
        origin: TypeOrigin::Annotation,
    });
    assert_eq!(known.call_entry("m"), "Cls.m");
    assert_eq!(
        Receiver::Unknown(UnknownCause::NoEvidence).call_entry("m"),
        CallSite::untyped_member("m")
    );
    for cause in [
        UnknownCause::Conflict,
        UnknownCause::Unsupported,
        UnknownCause::Escape,
        UnknownCause::Budget,
    ] {
        assert_eq!(
            Receiver::Unknown(cause).call_entry("m"),
            CallSite::unproven_member("m"),
            "{cause:?}"
        );
    }

    let read = known
        .member_read_entry("m", ReadIntent::Unknown)
        .expect("a known receiver records its member read");
    assert_eq!(
        MemberRead::parse(&read),
        Some(MemberRead {
            receiver_type: "Cls",
            member: "m",
            intent: ReadIntent::Unknown,
        })
    );
    assert_eq!(
        Receiver::Unknown(UnknownCause::Conflict).member_read_entry("m", ReadIntent::MethodValue),
        None,
        "a method value never guesses"
    );
}

// ---- tracked cells: what the budget counts ----

/// `count` untyped locals, each assigned a fresh `T` (`written`) or never
/// assigned, and each read once at node `100 + i`, then a pinned parameter
/// `p: P` read at node 1.
fn locals_then_pinned(count: usize, written: bool) -> (IrBuilder, TypeId) {
    let mut b = IrBuilder::new(PINNED, SPAN);
    let p_type = b.intern("P", TypeOrigin::Annotation);
    let t_type = b.intern("T", TypeOrigin::Annotation);
    let p = b.declare("p", BindingOrigin::Param, Some(p_type));
    for i in 0..count {
        let local = b.declare(&format!("x{i}"), BindingOrigin::Local, None);
        if written {
            b.assign(local, Value::Type(t_type));
        }
        b.use_binding(local, 100 + i);
    }
    b.use_binding(p, 1);
    b.ret();
    (b, p_type)
}

#[test]
fn test_solve_over_budget_pinned_use_keeps_declared_type() {
    let (b, p_type) = locals_then_pinned(MAX_TRACKED_CELLS + 1, true);
    let policy = b.policy();
    let solved = solve(&b.finish(), policy);
    assert!(solved.over_budget);
    let at = |node: usize| {
        solved
            .uses
            .iter()
            .find(|(n, _)| *n == node)
            .map(|&(_, s)| s)
    };
    assert_eq!(at(1), Some(Abstract::Known(p_type)), "{:?}", solved.uses);
    assert_eq!(at(100), Some(Abstract::Unknown(UnknownCause::Budget)));
}

/// A local nothing writes holds its entry state, so it costs no tracked
/// cell: any number of them stays within budget and reads `Unbound`, as a
/// tracked one would.
#[test]
fn test_solve_unwritten_locals_take_no_tracked_cell() {
    let (b, p_type) = locals_then_pinned(MAX_TRACKED_CELLS * 4, false);
    let policy = b.policy();
    let solved = solve(&b.finish(), policy);
    assert!(!solved.over_budget);
    let at = |node: usize| {
        solved
            .uses
            .iter()
            .find(|(n, _)| *n == node)
            .map(|&(_, s)| s)
    };
    assert_eq!(at(1), Some(Abstract::Known(p_type)));
    assert_eq!(at(100), Some(Abstract::Unbound));
    let (within, _) = locals_then_pinned(3, false);
    let (tracked, _) = locals_then_pinned(3, true);
    let unwritten = state_at(within, 101);
    assert_eq!(unwritten, Abstract::Unbound);
    assert!(matches!(state_at(tracked, 101), Abstract::Known(_)));
}

/// A closure that reads a captured outer local nothing writes sees the
/// outer's entry state, and one that reads a written outer local sees the
/// state at its creation: untracked captures still pass their state.
#[test]
fn test_solve_closure_reads_unwritten_and_written_outer_locals() {
    for written in [false, true] {
        let mut b = IrBuilder::new(PINNED, SPAN);
        let t_type = b.intern("T", TypeOrigin::Annotation);
        let p = b.declare("p", BindingOrigin::Param, None);
        let x = b.declare("x", BindingOrigin::Local, None);
        if written {
            b.assign(x, Value::Type(t_type));
        }
        b.closure(SPAN, |b| {
            let inner_p = b.lookup_or_capture("p", CaptureMode::ByValue).unwrap();
            let inner_x = b.lookup_or_capture("x", CaptureMode::ByValue).unwrap();
            b.use_binding(inner_p, 1);
            b.use_binding(inner_x, 2);
            b.ret();
        });
        b.use_binding(p, 3);
        b.ret();
        let policy = b.policy();
        let solved = solve(&b.finish(), policy);
        let at = |node: usize| {
            solved
                .uses
                .iter()
                .find(|(n, _)| *n == node)
                .map(|&(_, s)| s)
        };
        assert_eq!(at(1), Some(NO_EVIDENCE), "written={written}");
        let expected = if written {
            Abstract::Known(t_type)
        } else {
            Abstract::Unbound
        };
        assert_eq!(at(2), Some(expected), "written={written}");
    }
}

/// A long copy chain (`x_{i+1} = x_i`) resolves every link: the plan
/// follows each copy edge once from the cell that demands it.
#[test]
fn test_solve_long_copy_chain_resolves_every_link() {
    let mut b = IrBuilder::new(DUCK, SPAN);
    let t_type = b.intern("T", TypeOrigin::Annotation);
    let mut prev = b.declare("x0", BindingOrigin::Local, None);
    b.assign(prev, Value::Type(t_type));
    for i in 1..200 {
        let next = b.declare(&format!("x{i}"), BindingOrigin::Local, None);
        b.assign(next, Value::Copy(prev));
        prev = next;
    }
    b.use_binding(prev, 1);
    b.ret();
    assert_eq!(state_at(b, 1), Abstract::Known(t_type));
}
