//! Receiver core: the language-neutral analysis that decides, per member
//! occurrence, which type its receiver binding holds on every reachable path.
//!
//! Each language lowers a function body into [`ir::FunctionIr`] through
//! [`IrBuilder`] (its `<lang>/receiver_lower.rs`); [`analyze_file`] solves
//! every function of a file once, and the call extractor and the
//! method-value extractor both read the one [`FileReceivers`] it returns.

mod build;
pub mod ir;
pub mod lattice;
pub mod solve;

pub use build::IrBuilder;

use crate::framework_helpers::node_span;
use ecp_core::analyzer::types::{BlindSpot, CallSite, MemberRead, ReadIntent};
use ecp_core::file_category::is_test_path;
use ir::{TypeId, TypeRef};
use lattice::{join, Abstract, UnknownCause};
use rustc_hash::FxHashMap;
use std::path::Path;
use tree_sitter::Node;

/// The `BlindSpot.kind` of a function the solver gave up on.
pub const RECEIVER_BUDGET_KIND: &str = "receiver-budget";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LangPolicy {
    pub declared: DeclaredTypePolicy,
    pub scope: ScopeRule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclaredTypePolicy {
    /// The declared static type holds on every path: Java, C#, Kotlin, Swift, Go, C++.
    Pinned,
    /// As `Pinned`, except an assigned `Value::Dynamic` makes the cell `Unknown(NoEvidence)`: TypeScript.
    PinnedUnlessDynamic,
    /// An annotation types only the value assigned at that point: Python, PHP, Ruby, JavaScript.
    PointOfAssignment,
}

/// Where `IrBuilder::declare` puts a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeRule {
    Block,
    Function,
}

/// One language's lowering of function bodies into the receiver IR.
pub trait Lowering {
    /// File-level facts this lowering reads: declared class names, import
    /// bindings, module-level typed globals. Each language keeps its own.
    type Cx<'a>;

    fn policy(&self) -> LangPolicy;

    /// Whether `node` is a function-like node whose body owns bindings.
    /// `analyze_file` visits the outermost ones, the file's root included
    /// (a lowering that answers true for its module node lowers top-level
    /// code as one function); nested closures lower through
    /// `IrBuilder::closure` from inside `lower`.
    fn is_function(&self, node: Node<'_>) -> bool;

    /// Lowers `function` into `b`. `Lowered::NoUses` when the body holds no
    /// receiver occurrence: the core then builds nothing for it.
    fn lower(
        &self,
        cx: &Self::Cx<'_>,
        function: Node<'_>,
        source: &[u8],
        b: &mut IrBuilder,
    ) -> Lowered;

    /// The name a budget blind spot quotes.
    fn function_name<'s>(&self, function: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
        function.child_by_field_name("name")?.utf8_text(source).ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lowered {
    Done,
    NoUses,
    /// The lowering gave up on the function; its occurrences keep the
    /// extractor's existing path (`receiver_at` answers `None`).
    Unsupported(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receiver {
    Known(TypeRef),
    Unknown(UnknownCause),
}

static UNKNOWN: [Receiver; 5] = [
    Receiver::Unknown(UnknownCause::NoEvidence),
    Receiver::Unknown(UnknownCause::Conflict),
    Receiver::Unknown(UnknownCause::Unsupported),
    Receiver::Unknown(UnknownCause::Escape),
    Receiver::Unknown(UnknownCause::Budget),
];

impl Receiver {
    /// The `RawNode.calls` entry of `receiver.member(..)`:
    /// Known -> "T.member"; Unknown(NoEvidence) -> `CallSite::untyped_member`;
    /// any other Unknown -> `CallSite::unproven_member`.
    pub fn call_entry(&self, member: &str) -> String {
        match self {
            Receiver::Known(ty) => [&*ty.text, ".", member].concat(),
            Receiver::Unknown(UnknownCause::NoEvidence) => CallSite::untyped_member(member),
            Receiver::Unknown(_) => CallSite::unproven_member(member),
        }
    }

    /// The `RawNode.field_reads` entry of `receiver.member` in value
    /// position: Some only for Known (a method value never guesses).
    pub fn member_read_entry(&self, member: &str, intent: ReadIntent) -> Option<String> {
        match self {
            Receiver::Known(ty) => Some(MemberRead::encode(&ty.text, member, intent)),
            Receiver::Unknown(_) => None,
        }
    }
}

/// A receiver state in the file table: no owned type text per occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Unknown(UnknownCause),
    /// Index into `FileReceivers::known`.
    Known(u32),
}

impl Slot {
    fn from_state(state: Abstract, base: u32) -> Self {
        match state {
            Abstract::Unreachable | Abstract::Unbound => Slot::Unknown(UnknownCause::NoEvidence),
            Abstract::Known(ty) => Slot::Known(base + ty.0),
            Abstract::Unknown(cause) => Slot::Unknown(cause),
        }
    }

    fn state(self) -> Abstract {
        match self {
            Slot::Unknown(cause) => Abstract::Unknown(cause),
            Slot::Known(index) => Abstract::Known(TypeId(index)),
        }
    }
}

/// The receiver states of one file, keyed by the receiver node's `Node::id()`.
#[derive(Debug, Default)]
pub struct FileReceivers {
    states: FxHashMap<usize, Slot>,
    known: Vec<Receiver>,
    pub blind_spots: Vec<BlindSpot>,
}

impl FileReceivers {
    /// `None`: the node is no binding receiver the core tracked (`self`,
    /// `this`, a call, a field). The extractor then keeps its own path.
    pub fn receiver_at(&self, receiver: Node<'_>) -> Option<&Receiver> {
        self.states.get(&receiver.id()).map(|slot| match *slot {
            Slot::Unknown(cause) => &UNKNOWN[cause as usize],
            Slot::Known(index) => &self.known[index as usize],
        })
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Adds one root function's answers. A node two uses share keeps the
    /// join of both.
    fn absorb(&mut self, types: Vec<TypeRef>, uses: Vec<(usize, Abstract)>) {
        let base = self.known.len() as u32;
        self.known.extend(types.into_iter().map(Receiver::Known));
        for (node, state) in uses {
            let slot = Slot::from_state(state, base);
            self.states
                .entry(node)
                .and_modify(|old| *old = Slot::from_state(join(old.state(), slot.state()), 0))
                .or_insert(slot);
        }
    }
}

/// Lowers and solves every outermost function of the file under `root`.
pub fn analyze_file<L: Lowering>(
    lowering: &L,
    cx: &L::Cx<'_>,
    root: Node<'_>,
    source: &[u8],
    file: &Path,
) -> FileReceivers {
    let mut out = FileReceivers::default();
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let descend = if lowering.is_function(node) {
            analyze_function(lowering, cx, node, source, file, &mut out);
            false
        } else {
            true
        };
        if descend && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return out;
            }
        }
    }
}

fn analyze_function<L: Lowering>(
    lowering: &L,
    cx: &L::Cx<'_>,
    function: Node<'_>,
    source: &[u8],
    file: &Path,
    out: &mut FileReceivers,
) {
    let span = node_span(&function);
    let policy = lowering.policy();
    let mut builder = IrBuilder::new(policy, span);
    match lowering.lower(cx, function, source, &mut builder) {
        Lowered::Done => {}
        Lowered::NoUses | Lowered::Unsupported(_) => return,
    }
    let ir = builder.finish();
    let solved = solve::solve(&ir, policy);
    if solved.over_budget {
        let name = lowering
            .function_name(function, source)
            .unwrap_or("<anonymous>");
        out.blind_spots.push(BlindSpot {
            kind: RECEIVER_BUDGET_KIND.to_owned(),
            file_path: file.to_path_buf(),
            span,
            hint: format!(
                "receiver analysis budget exceeded in `{name}`; its member calls bind as untyped"
            ),
            is_test: is_test_path(&file.to_string_lossy()),
        });
    }
    out.absorb(ir.types, solved.uses);
}
