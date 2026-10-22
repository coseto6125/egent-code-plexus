use ecp_core::analyzer::types::{CallSite, RawNode};
pub use ecp_core::analyzer::types::{NameRef, NameRefKind, VariantRef};
use ecp_core::graph::NodeKind;
use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use tree_sitter::{Node, TreeCursor};

/// Saturating conversion of a tree-sitter row (`usize`) to `u32`.
/// Files exceeding `u32::MAX` rows clamp to `u32::MAX` rather than silently
/// truncating to a wrong line number — the call would be misattributed to
/// whichever function happens to contain row `truncated_value`. With
/// saturation, the call is attached to whichever function contains the very
/// last line (almost certainly a no-op since no real function spans line
/// 4.29 billion).
#[inline]
pub fn safe_row(row: usize) -> u32 {
    u32::try_from(row).unwrap_or(u32::MAX)
}

/// `(row, column)` of a node's start, the position call attribution compares.
#[inline]
pub fn start_pos(node: Node<'_>) -> Pos {
    let point = node.start_position();
    (safe_row(point.row), safe_row(point.column))
}

/// The `RawNode.calls` entry of a `new T(..)` site, generic arguments cut
/// from every path segment: `Box<String>` → `Box`, `ns::Box<int>` →
/// `ns::Box`, `Outer<int>.Inner` → `Outer.Inner`. `None` for an empty type.
pub fn construction_call(type_text: &str) -> Option<String> {
    let mut ty = String::with_capacity(type_text.len());
    let mut depth = 0u32;
    for c in type_text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => ty.push(c),
            _ => {}
        }
    }
    let ty = ty.trim();
    (!ty.is_empty()).then(|| CallSite::construct(ty))
}

/// The construction entry of a TS / JS `new T(..)` / `new ns.T(..)`; `None`
/// for a computed constructor (`new (f())()`).
pub fn new_expression_call(new_expr: Node<'_>, source: &[u8]) -> Option<String> {
    let ctor = new_expr.child_by_field_name("constructor")?;
    match ctor.kind() {
        "identifier" | "member_expression" => construction_call(ctor.utf8_text(source).ok()?),
        _ => None,
    }
}

/// Walk the AST and attach `callee` names to the smallest enclosing
/// function/method/constructor `RawNode` by span containment. Reused across
/// languages — the `call_kinds` set lists this language's AST node kinds that
/// represent a function-call expression (e.g. "call_expression" in JS/TS,
/// "method_invocation" in Java, "function_call_expression" in PHP).
pub fn extract_calls(root: Node<'_>, source: &[u8], nodes: &mut [RawNode], call_kinds: &[&str]) {
    let containers = enclosing_containers(nodes);
    let mut calls: Vec<PendingCall> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if call_kinds.contains(&n.kind()) {
            let callee_name = callee_name_from(n, source);
            if let Some(name) = callee_name {
                calls.push(PendingCall {
                    pos: start_pos(n),
                    name,
                });
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    attach_pending_calls(&containers, calls, nodes);
}

pub fn callee_name_from(call_node: Node<'_>, source: &[u8]) -> Option<String> {
    // Try "function" field first (JS/TS/PHP), fall back to "name"/"called function" semantics.
    let target = call_node
        .child_by_field_name("function")
        .or_else(|| call_node.child_by_field_name("name"))
        .or_else(|| call_node.child(0))?;
    match target.kind() {
        "identifier" | "type_identifier" | "property_identifier" | "simple_identifier" => {
            target.utf8_text(source).ok().map(|s| s.to_string())
        }
        "member_expression" | "field_access" | "navigation_expression" => {
            // Prefer the full text of the member expression (e.g., "z.record") to preserve namespace context.
            // If extracting the full text fails, fall back to just the property name.
            target
                .utf8_text(source)
                .ok()
                .map(|s| s.to_string())
                .or_else(|| {
                    target
                        .child_by_field_name("property")
                        .or_else(|| target.child_by_field_name("field"))
                        .or_else(|| target.child_by_field_name("name"))
                        .and_then(|p| p.utf8_text(source).ok().map(|s| s.to_string()))
                })
        }
        "scoped_identifier" | "qualified_name" | "scoped_call_expression" => {
            // Preserve full qualifier path (`A::new`, `std::vec::Vec::new`,
            // `Outer::Inner::method`) so the resolver's Tier 2.5 can split on
            // `::` / `.` and scope the lookup to the qualifier's defining
            // file. Falls back to the bare member name if full-text extraction
            // fails (defensive — the parent slice is always valid utf-8 in
            // practice).
            target
                .utf8_text(source)
                .ok()
                .map(|s| s.to_string())
                .or_else(|| {
                    target
                        .child_by_field_name("name")
                        .or_else(|| {
                            target.named_child(target.named_child_count().saturating_sub(1) as u32)
                        })
                        .and_then(|p| p.utf8_text(source).ok().map(|s| s.to_string()))
                })
        }
        _ => target.utf8_text(source).ok().and_then(|s| {
            // last resort: take last segment after `.` or `::`
            let trimmed = s.trim();
            let after_dot = trimmed.rsplit_once('.').map(|(_, t)| t).unwrap_or(trimmed);
            let after_colon = after_dot
                .rsplit_once("::")
                .map(|(_, t)| t)
                .unwrap_or(after_dot);
            let id = after_colon
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>();
            if id.is_empty() {
                None
            } else {
                Some(id)
            }
        }),
    }
}

/// Call-node `Node::id` -> (caller node index, index in that node's `calls`);
/// built during extraction, consumed by the indirect-dispatch detectors, never persisted.
/// Keyed by id, not start byte: `a().b()` starts at the same byte as `a()`.
pub type CallSiteIndex = rustc_hash::FxHashMap<usize, (usize, u32)>;

/// Attach a call using columns as well as rows, including one-line closures.
/// Returns the caller's node index and the call's index in its `calls`.
pub fn attach_to_enclosing_span(
    span: crate::framework_helpers::Span,
    callee: String,
    nodes: &mut [RawNode],
) -> Option<(usize, u32)> {
    let i = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            matches!(
                node.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Constructor
            ) && crate::framework_helpers::span_contains(node.span, span)
        })
        .min_by_key(|(_, node)| crate::framework_helpers::span_area(node.span))
        .map(|(i, _)| i)?;
    let calls = &mut nodes[i].calls;
    let idx = calls.len() as u32;
    calls.push(callee);
    Some((i, idx))
}

/// Attach a call to the smallest function holding `pos`, by row and column so
/// functions sharing a line stay apart.
/// Returns the caller's node index and the call's index in its `calls`.
pub fn attach_to_enclosing(
    pos: Pos,
    callee: String,
    nodes: &mut [RawNode],
) -> Option<(usize, u32)> {
    let mut best: Option<(usize, (u32, i64))> = None;
    for (i, n) in nodes.iter().enumerate() {
        if !is_container_kind(n.kind) {
            continue;
        }
        let (start, end) = span_ends(n.span);
        if start <= pos && pos <= end {
            let width = span_width(n.span);
            if best.is_none_or(|(_, w)| width < w) {
                best = Some((i, width));
            }
        }
    }
    let (i, _) = best?;
    let idx = nodes[i].calls.len() as u32;
    nodes[i].calls.push(callee);
    Some((i, idx))
}

/// Walk the AST and attach the **field name** of each member-access read
/// (`obj.field`, `self.count`, `cfg->timeout`) to the smallest enclosing
/// function/method/constructor `RawNode`'s `field_reads`. Mirrors
/// `extract_calls`; `field_kinds` lists this language's member-access AST node
/// kinds (e.g. `member_expression` in JS/TS, `field_expression` in C/C++/Rust,
/// `selector_expression` in Go).
///
/// A member access that is the callee of a call (`obj.method()`) is skipped —
/// `extract_calls` already records that as a `Calls` edge, and the field name
/// there is a method, not a data field. The check is structural: if the access
/// node is the `function` child of its parent call, it is a callee.
///
/// The same walk records each bare name the body uses as a value
/// ([`NameRef`]), once per enclosing function, as `names` describes.
pub fn extract_field_reads(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    field_kinds: &[&str],
    names: &NameRefSpec,
) {
    extract_field_reads_with_literal_key_facts(root, source, nodes, field_kinds, names, None);
}

/// Like [`extract_field_reads`], with facts that classify keyed composite
/// literal elements whose type is not written directly at the literal.
pub fn extract_field_reads_with_literal_key_facts(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    field_kinds: &[&str],
    names: &NameRefSpec,
    literal_key_facts: Option<&LiteralKeyFacts>,
) {
    let containers = enclosing_containers(nodes);
    if containers.is_empty() {
        return;
    }
    let mut reads: Vec<PendingCall> = Vec::new();
    let mut scan = NameScan::default();
    let mut memo = KindMemo::new(names, field_kinds);
    // Each node travels with its parent and its field in that parent:
    // `Node::parent` walks down from the root, which every node would pay.
    let mut stack: Vec<Frame<'_>> = vec![Frame {
        node: root,
        parent: None,
        parent_kind: 0,
        field: 0,
        scope: NameScope::Value,
    }];
    while let Some(frame) = stack.pop() {
        let n = frame.node;
        let kind = memo.kind(n);
        if kind & kind_flag::FIELD_ACCESS != 0 && !is_call_callee(n, frame.parent) {
            if let Some(name) = field_name_from(n, source) {
                reads.push(PendingCall {
                    pos: start_pos(n),
                    name,
                });
            }
        }
        let mut scope = frame.scope;
        if !matches!(scope, NameScope::Skip | NameScope::Off)
            && scan.visit(&frame, kind, source, &mut memo)
        {
            scope = NameScope::Off;
        }
        let mut cursor = n.walk();
        if cursor.goto_first_child() {
            let parent = ParentShape {
                node: n,
                kind,
                scope,
            };
            loop {
                let child = cursor.node();
                let field = memo.field(&cursor);
                let child_kind = memo.kind(child);
                stack.push(Frame {
                    node: child,
                    parent: Some(n),
                    parent_kind: kind,
                    field,
                    scope: names.child_scope(parent, child_kind, field, &cursor, literal_key_facts),
                });
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    attach_pending_field_reads(&containers, reads, nodes);
    scan.attach(&containers, nodes);
}

/// Per-file facts for keyed composite literal elements. A language-specific
/// prepass marks only elements whose key is an expression, rather than a
/// record field name.
#[derive(Default)]
pub struct LiteralKeyFacts {
    map_keys: rustc_hash::FxHashSet<usize>,
}

impl LiteralKeyFacts {
    /// Mark `keyed` as an element whose `key` is a value expression.
    pub fn mark_map_key(&mut self, keyed: Node<'_>) {
        self.map_keys.insert(keyed.id());
    }

    fn is_map_key(&self, keyed: Node<'_>) -> bool {
        self.map_keys.contains(&keyed.id())
    }
}

/// True when `access` is the callee of an enclosing call expression — i.e. its
/// parent is a call/invocation whose `function` field is `access`. Such nodes
/// are method calls handled by `extract_calls`, not data-field reads.
fn is_call_callee(access: Node<'_>, parent: Option<Node<'_>>) -> bool {
    let Some(parent) = parent else {
        return false;
    };
    matches!(
        parent.kind(),
        "call_expression"
            | "method_invocation"
            | "function_call_expression"
            | "invocation_expression"
            | "call"
    ) && parent.child_by_field_name("function").map(|f| f.id()) == Some(access.id())
}

/// Where a language's grammar names a value by a bare name, for the
/// [`NameRef`]s [`extract_field_reads`] records.
///
/// The fields every grammar here shares are built in: a `pattern`,
/// `parameter(s)`, `declarator`, `alias` or `bound_identifier` subtree
/// declares local names, as does a `name` under a `*parameter*` or
/// `*declarator*` node and every child of a `*parameter*` node; a `value`,
/// `right`, `body` … subtree is an expression again; a `type` subtree names
/// no value.
pub struct NameRefSpec {
    /// Node kinds of a bare name: `identifier`, Kotlin / Swift
    /// `simple_identifier`, Ruby `constant`, PHP `name`.
    pub ident_kinds: &'static [&'static str],
    /// What a bare name may name.
    pub kind: NameRefKind,
    /// `(parent kind, field)` whose subtree declares local names beyond the
    /// shared ones: Python `(assignment, left)`, Go `(var_spec, name)`.
    pub binding_fields: &'static [(&'static str, &'static str)],
    /// Parent kinds whose bare-name children declare local names: Kotlin
    /// `variable_declaration`, whose grammar has no fields.
    pub binding_parents: &'static [&'static str],
    /// `(pattern kind, wrapper kind)`: a wrapper whose only named child is a
    /// bare name declares that name. Python `(case_pattern, dotted_name)`:
    /// `case [LIMIT]:` captures, `case Color.RED:` compares.
    pub capture_patterns: &'static [(&'static str, &'static str)],
    /// `(parent kind, field)` whose subtree names no value, unless the
    /// literal's type is in `map_literal_types`: Go `(keyed_element, key)`,
    /// a struct literal's field name.
    pub skip_fields: &'static [(&'static str, &'static str)],
    /// Kinds whose subtree names no value: imports, annotations, types.
    pub skip_kinds: &'static [&'static str],
    /// Parent kinds whose bare-name children name no value: a member
    /// suffix (Kotlin `navigation_suffix`), a jump or argument label.
    pub skip_parents: &'static [&'static str],
    /// The sibling after a callee in a grammar with no `function` field:
    /// Kotlin / Swift `call_suffix`.
    pub call_suffixes: &'static [&'static str],
    /// Parent kinds whose middle bare name is a callee: Kotlin
    /// `infix_expression` (`a to b`).
    pub infix_calls: &'static [&'static str],
    /// Method-reference kinds whose last bare name is the function: Java
    /// `method_reference`, Kotlin `callable_reference`.
    pub method_refs: &'static [&'static str],
    /// Ruby: `method(:name)` names the method `name`.
    pub symbol_method_refs: bool,
    /// Parent kinds whose bare name right before `=` is an argument label:
    /// Kotlin `value_argument` (`sink(limit = 1)`).
    pub named_arg_parents: &'static [&'static str],
    /// Statement kinds that say where a name lives without reading it:
    /// Python `global`. A function's assignment to such a name writes the
    /// module's, so it declares no local.
    pub global_decls: &'static [&'static str],
    /// Composite-literal type kinds whose keys are expressions: Go
    /// `map_type`, `slice_type`, `array_type`. `skip_fields` skips the keys
    /// of any other literal, a named map type's included.
    pub map_literal_types: &'static [&'static str],
    /// A call kind, and what names the literal callable a call of that kind
    /// hands on: PHP `array_map('fmt', $xs)` hands on `fmt`.
    pub passed_callable: Option<(&'static str, CallableNamer)>,
}

/// Reads the callable name a call hands on, from the call node.
pub type CallableNamer = fn(Node<'_>, &[u8]) -> Option<String>;

/// Fields whose subtree declares local names in every grammar here.
const BINDING_FIELDS: &[&str] = &[
    "pattern",
    "parameter",
    "parameters",
    "declarator",
    "alias",
    "bound_identifier",
];

/// Fields whose subtree is an expression again inside a declaration: the
/// initializer of `let x = LIMIT`, a parameter default, a loop's iterable.
const VALUE_FIELDS: &[&str] = &[
    "value",
    "right",
    "default_value",
    "body",
    "condition",
    "initializer",
    "arguments",
    "size",
    "index",
    "collection",
];

/// Fields whose subtree names no value.
const SKIP_FIELDS: &[&str] = &["type", "return_type", "type_arguments"];

/// Fields under which a bare name is a declared or member name, a callee,
/// or a label, never a value read.
const NON_VALUE_FIELDS: &[&str] = &[
    "name",
    "field",
    "property",
    "label",
    "attribute",
    "function",
    "method",
    "constructor",
    "scope",
    "suffix",
    "macro",
    "path",
    "external_name",
];

/// Where a node sits for [`NameRef`] capture.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NameScope {
    /// An expression: a bare name reads a value.
    Value,
    /// A declaration target: a bare name declares a local.
    Binding,
    /// A type, import, annotation or label: a bare name is neither.
    Skip,
    /// Below a node the scan recorded whole (a method reference): nothing
    /// more is recorded, whatever a field says.
    Off,
}

struct Frame<'t> {
    node: Node<'t>,
    parent: Option<Node<'t>>,
    /// The parent's [`kind_flag`]s.
    parent_kind: u32,
    /// The [`field_flag`]s of the field this node fills in its parent.
    field: u16,
    scope: NameScope,
}

/// What a node passes on to its children's [`NameScope`].
#[derive(Clone, Copy)]
struct ParentShape<'t> {
    node: Node<'t>,
    kind: u32,
    scope: NameScope,
}

/// Whether an `=` token precedes `node` among its siblings: in a C#
/// `let x = LIMIT`, `x` declares and `LIMIT` reads.
fn after_equals(node: Node<'_>) -> bool {
    let mut sibling = node.prev_sibling();
    while let Some(prev) = sibling {
        if prev.kind() == "=" {
            return true;
        }
        sibling = prev.prev_sibling();
    }
    false
}

/// Bits [`KindMemo::kind`] records for a node kind.
mod kind_flag {
    pub const KNOWN: u32 = 1;
    /// One of the language's member-access kinds.
    pub const FIELD_ACCESS: u32 = 1 << 1;
    pub const IDENT: u32 = 1 << 2;
    pub const SKIP_KIND: u32 = 1 << 3;
    /// A `*parameter*` node: every child declares, unless a field says else.
    pub const PARAMETER: u32 = 1 << 4;
    /// A `*declarator*` node: its `name` declares.
    pub const DECLARATOR: u32 = 1 << 5;
    pub const METHOD_REF: u32 = 1 << 6;
    /// Ruby `call`, which may be `method(:name)`.
    pub const SYMBOL_CALL: u32 = 1 << 7;
    pub const BINDING_PARENT: u32 = 1 << 8;
    pub const SKIP_PARENT: u32 = 1 << 9;
    pub const CALL_SUFFIX: u32 = 1 << 10;
    pub const INFIX_CALL: u32 = 1 << 11;
    /// An `*assignment*` node, whose `left` is a target.
    pub const ASSIGNMENT: u32 = 1 << 12;
    pub const NAMED_ARG_PARENT: u32 = 1 << 13;
    pub const GLOBAL_DECL: u32 = 1 << 14;
    /// The spec's [`NameRefSpec::passed_callable`] call kind.
    pub const PASSED_CALL: u32 = 1 << 15;
    /// A wrapper kind of the spec's [`NameRefSpec::capture_patterns`].
    pub const CAPTURE_WRAPPER: u32 = 1 << 16;
}

/// Bits [`KindMemo::field`] records for a field name.
mod field_flag {
    pub const KNOWN: u16 = 1;
    pub const BINDING: u16 = 1 << 1;
    pub const VALUE: u16 = 1 << 2;
    pub const SKIP: u16 = 1 << 3;
    pub const NON_VALUE: u16 = 1 << 4;
    pub const NAME: u16 = 1 << 5;
    pub const LEFT: u16 = 1 << 6;
    /// Named in the spec's `(parent kind, field)` pairs: the parent's kind
    /// decides.
    pub const PAIR: u16 = 1 << 7;
    /// A part of a member or index target (`CONFIG.debug`, `CACHE[KEY]`)
    /// other than the target itself: a binding scope does not declare it.
    pub const MEMBER_PART: u16 = 1 << 8;
}

/// What each node kind and field means to the walk, recorded per file on
/// first sight. Matching kind and field strings at every node cost a third
/// of the walk.
struct KindMemo<'a> {
    spec: &'a NameRefSpec,
    field_kinds: &'a [&'a str],
    kinds: Vec<u32>,
    fields: Vec<u16>,
}

impl<'a> KindMemo<'a> {
    fn new(spec: &'a NameRefSpec, field_kinds: &'a [&'a str]) -> Self {
        Self {
            spec,
            field_kinds,
            kinds: Vec::new(),
            fields: Vec::new(),
        }
    }

    fn kind(&mut self, node: Node<'_>) -> u32 {
        let id = usize::from(node.kind_id());
        match self.kinds.get(id) {
            Some(&flags) if flags != 0 => flags,
            _ => {
                let flags = self.kind_flags(node.kind());
                if self.kinds.len() <= id {
                    self.kinds.resize(id + 1, 0);
                }
                self.kinds[id] = flags;
                flags
            }
        }
    }

    fn kind_flags(&self, kind: &str) -> u32 {
        use kind_flag::*;
        let spec = self.spec;
        [
            (self.field_kinds.contains(&kind), FIELD_ACCESS),
            (spec.ident_kinds.contains(&kind), IDENT),
            (spec.skip_kinds.contains(&kind), SKIP_KIND),
            (kind.contains("parameter"), PARAMETER),
            (kind.contains("declarator"), DECLARATOR),
            (spec.method_refs.contains(&kind), METHOD_REF),
            (spec.symbol_method_refs && kind == "call", SYMBOL_CALL),
            (spec.binding_parents.contains(&kind), BINDING_PARENT),
            (spec.skip_parents.contains(&kind), SKIP_PARENT),
            (spec.call_suffixes.contains(&kind), CALL_SUFFIX),
            (spec.infix_calls.contains(&kind), INFIX_CALL),
            (kind.contains("assignment"), ASSIGNMENT),
            (spec.named_arg_parents.contains(&kind), NAMED_ARG_PARENT),
            (spec.global_decls.contains(&kind), GLOBAL_DECL),
            (
                spec.capture_patterns
                    .iter()
                    .any(|&(_, wrapper)| wrapper == kind),
                CAPTURE_WRAPPER,
            ),
            (
                spec.passed_callable.is_some_and(|(call, _)| call == kind),
                PASSED_CALL,
            ),
        ]
        .into_iter()
        .fold(KNOWN, |acc, (hit, bit)| if hit { acc | bit } else { acc })
    }

    /// The flags of the field `cursor`'s node fills; 0 for none.
    fn field(&mut self, cursor: &TreeCursor<'_>) -> u16 {
        let Some(id) = cursor.field_id() else {
            return 0;
        };
        let id = usize::from(id.get());
        match self.fields.get(id) {
            Some(&flags) if flags != 0 => flags,
            _ => {
                let flags = cursor.field_name().map_or(0, |f| self.field_flags(f));
                if self.fields.len() <= id {
                    self.fields.resize(id + 1, 0);
                }
                self.fields[id] = flags;
                flags
            }
        }
    }

    fn field_flags(&self, field: &str) -> u16 {
        use field_flag::*;
        let spec = self.spec;
        [
            (BINDING_FIELDS.contains(&field), BINDING),
            (VALUE_FIELDS.contains(&field), VALUE),
            (SKIP_FIELDS.contains(&field), SKIP),
            (NON_VALUE_FIELDS.contains(&field), NON_VALUE),
            (field == "name", NAME),
            (field == "left", LEFT),
            (
                matches!(field, "object" | "operand" | "attribute" | "subscript"),
                MEMBER_PART,
            ),
            (
                spec.binding_fields
                    .iter()
                    .chain(spec.skip_fields)
                    .any(|&(_, f)| f == field),
                PAIR,
            ),
        ]
        .into_iter()
        .fold(KNOWN, |acc, (hit, bit)| if hit { acc | bit } else { acc })
    }
}

impl NameRefSpec {
    pub const EMPTY: Self = Self {
        ident_kinds: &[],
        kind: NameRefKind::Value,
        binding_fields: &[],
        binding_parents: &[],
        capture_patterns: &[],
        skip_fields: &[],
        skip_kinds: &[],
        skip_parents: &[],
        call_suffixes: &[],
        infix_calls: &[],
        method_refs: &[],
        symbol_method_refs: false,
        named_arg_parents: &[],
        global_decls: &[],
        map_literal_types: &[],
        passed_callable: None,
    };

    fn child_scope(
        &self,
        parent: ParentShape<'_>,
        child_kind: u32,
        field: u16,
        cursor: &TreeCursor<'_>,
        literal_key_facts: Option<&LiteralKeyFacts>,
    ) -> NameScope {
        use field_flag as f;
        use kind_flag as k;
        if parent.scope == NameScope::Off {
            return NameScope::Off;
        }
        if child_kind & k::CAPTURE_WRAPPER != 0 {
            let child = cursor.node();
            if child.named_child_count() == 1
                && self
                    .capture_patterns
                    .contains(&(parent.node.kind(), child.kind()))
            {
                return NameScope::Binding;
            }
        }
        if child_kind & k::SKIP_KIND != 0 {
            return NameScope::Skip;
        }
        let parameter = parent.kind & k::PARAMETER != 0;
        if field != 0 {
            let pair = |pairs: &[(&'static str, &'static str)]| {
                field & f::PAIR != 0 && {
                    let key = (parent.node.kind(), cursor.field_name().unwrap_or_default());
                    pairs.contains(&key)
                }
            };
            // A destructuring assignment writes the names it lists: they
            // are neither read nor declared (`({ data: current } = res)`).
            if field & f::LEFT != 0
                && parent.kind & k::ASSIGNMENT != 0
                && matches!(cursor.node().kind(), "object_pattern" | "array_pattern")
            {
                return NameScope::Skip;
            }
            // A member or index target (`CONFIG.debug = True`,
            // `CACHE[KEY] = 1`) declares nothing: its receiver and index are
            // read.
            if field & f::MEMBER_PART != 0 && parent.scope == NameScope::Binding {
                return NameScope::Value;
            }
            if field & f::BINDING != 0
                || (field & f::NAME != 0 && parent.kind & (k::PARAMETER | k::DECLARATOR) != 0)
            {
                return NameScope::Binding;
            }
            if pair(self.binding_fields) {
                return if parent.scope == NameScope::Skip {
                    NameScope::Skip
                } else {
                    NameScope::Binding
                };
            }
            if field & f::VALUE != 0 {
                return NameScope::Value;
            }
            if field & f::SKIP != 0 {
                return NameScope::Skip;
            }
            if pair(self.skip_fields) {
                return if literal_typed_by(parent.node, self.map_literal_types)
                    || literal_key_facts.is_some_and(|facts| facts.is_map_key(parent.node))
                {
                    NameScope::Value
                } else {
                    NameScope::Skip
                };
            }
        }
        if parameter && parent.scope != NameScope::Skip {
            return NameScope::Binding;
        }
        parent.scope
    }

    /// Whether `ident`, a bare name in an expression, reads a value: not a
    /// member or declared name, not a callee, not an assignment target.
    fn reads_value(&self, ident: Node<'_>, frame: &Frame<'_>, memo: &mut KindMemo<'_>) -> bool {
        use field_flag as f;
        use kind_flag as k;
        let (parent, field) = (frame.parent_kind, frame.field);
        if field & f::NON_VALUE != 0
            || (field & f::LEFT != 0 && parent & k::ASSIGNMENT != 0)
            || parent & k::SKIP_PARENT != 0
        {
            return false;
        }
        if !self.call_suffixes.is_empty()
            && ident
                .next_named_sibling()
                .is_some_and(|s| memo.kind(s) & k::CALL_SUFFIX != 0)
        {
            return false;
        }
        if parent & k::NAMED_ARG_PARENT != 0
            && ident.next_sibling().is_some_and(|s| s.kind() == "=")
        {
            return false;
        }
        !(parent & k::INFIX_CALL != 0
            && ident.prev_named_sibling().is_some()
            && ident.next_named_sibling().is_some())
    }
}

/// Whether the composite literal holding `keyed`, a keyed element, is
/// typed by one of `types`. An elided inner literal (`{"a": {..}}`) names
/// no type, so its keys stay field names.
fn literal_typed_by(keyed: Node<'_>, types: &[&str]) -> bool {
    !types.is_empty()
        && keyed
            .parent()
            .and_then(|body| body.parent())
            .and_then(|literal| literal.child_by_field_name("type"))
            .is_some_and(|ty| types.contains(&ty.kind()))
}

/// A bare name read inside a body, pending its enclosing function.
struct PendingName<'s> {
    pos: Pos,
    name: Cow<'s, str>,
    kind: NameRefKind,
}

/// A local name a declaration introduces at `pos`.
struct PendingBinding<'s> {
    pos: Pos,
    name: &'s str,
}

#[derive(Default)]
struct NameScan<'s> {
    reads: Vec<PendingName<'s>>,
    bindings: Vec<PendingBinding<'s>>,
    /// Names a [`NameRefSpec::global_decls`] statement places in the module.
    globals: Vec<PendingBinding<'s>>,
}

impl<'s> NameScan<'s> {
    /// Record what `frame`'s node declares or reads. True when its subtree
    /// holds no further name to record (a method reference); the walk still
    /// descends for field reads.
    fn visit(
        &mut self,
        frame: &Frame<'_>,
        kind: u32,
        source: &'s [u8],
        memo: &mut KindMemo<'_>,
    ) -> bool {
        let n = frame.node;
        let spec = memo.spec;
        if kind & kind_flag::IDENT != 0 {
            if frame.parent.is_none() {
                return false;
            }
            let Ok(name) = n.utf8_text(source) else {
                return false;
            };
            match frame.scope {
                NameScope::Binding => self.bindings.push(PendingBinding {
                    pos: start_pos(n),
                    name,
                }),
                NameScope::Value if frame.parent_kind & kind_flag::GLOBAL_DECL != 0 => {
                    self.globals.push(PendingBinding {
                        pos: start_pos(n),
                        name,
                    });
                }
                NameScope::Value
                    if frame.parent_kind & kind_flag::BINDING_PARENT != 0 && !after_equals(n) =>
                {
                    self.bindings.push(PendingBinding {
                        pos: start_pos(n),
                        name,
                    });
                }
                NameScope::Value if spec.reads_value(n, frame, memo) => {
                    self.reads.push(PendingName {
                        pos: start_pos(n),
                        name: Cow::Borrowed(name),
                        kind: spec.kind,
                    });
                }
                _ => {}
            }
            return false;
        }
        if kind & kind_flag::METHOD_REF != 0 {
            if let Some(name) = method_reference(n, source, spec) {
                self.reads.push(PendingName {
                    pos: start_pos(n),
                    name,
                    kind: NameRefKind::Function,
                });
            }
            return true;
        }
        if kind & kind_flag::PASSED_CALL != 0 {
            if let Some(name) = spec.passed_callable.and_then(|(_, named)| named(n, source)) {
                self.reads.push(PendingName {
                    pos: start_pos(n),
                    name: Cow::Owned(name),
                    kind: NameRefKind::Function,
                });
            }
        }
        if kind & kind_flag::SYMBOL_CALL != 0 {
            if let Some(name) = symbol_method_ref(n, source) {
                self.reads.push(PendingName {
                    pos: start_pos(n),
                    name: Cow::Borrowed(name),
                    kind: NameRefKind::Function,
                });
            }
        }
        false
    }

    /// Attach each read to its smallest enclosing function, once per
    /// function, unless a parameter or local of a function holding it
    /// declares the same name.
    fn attach(self, containers: &[EnclosingContainer], nodes: &mut [RawNode]) {
        if self.reads.is_empty() {
            return;
        }
        let globals: rustc_hash::FxHashSet<(usize, &str)> = self
            .globals
            .iter()
            .zip(enclosing_targets(containers, &self.globals, |g| g.pos))
            .filter_map(|(global, target)| Some((target?, global.name)))
            .collect();
        let mut bound: rustc_hash::FxHashMap<&str, Vec<usize>> = rustc_hash::FxHashMap::default();
        let binding_targets = enclosing_targets(containers, &self.bindings, |b| b.pos);
        for (binding, target) in self.bindings.iter().zip(binding_targets) {
            if let Some(node_idx) = target.filter(|&t| !globals.contains(&(t, binding.name))) {
                bound.entry(binding.name).or_default().push(node_idx);
            }
        }
        let targets = enclosing_targets(containers, &self.reads, |r| r.pos);
        let mut seen: rustc_hash::FxHashSet<(usize, NameRefKind, &str)> =
            rustc_hash::FxHashSet::default();
        for (read, target) in self.reads.iter().zip(targets) {
            let Some(node_idx) = target else { continue };
            let shadowed = read.kind != NameRefKind::Function
                && bound.get(read.name.as_ref()).is_some_and(|scopes| {
                    scopes
                        .iter()
                        .any(|&scope| span_holds(nodes[scope].span, read.pos))
                });
            if !shadowed && seen.insert((node_idx, read.kind, read.name.as_ref())) {
                nodes[node_idx]
                    .field_reads
                    .push(NameRef::encode(&read.name, read.kind));
            }
        }
    }
}

/// The function a method reference names: `print` for `this::print` /
/// `::print`, `Foo.bar` for `Foo::bar`. `None` for a constructor
/// (`Foo::new`), a `super::` reference, or a computed receiver.
fn method_reference<'s>(
    reference: Node<'_>,
    source: &'s [u8],
    spec: &NameRefSpec,
) -> Option<Cow<'s, str>> {
    if reference
        .child(reference.child_count().checked_sub(1)?)
        .is_some_and(|last| last.kind() == "new")
    {
        return None;
    }
    let mut cursor = reference.walk();
    let named: Vec<Node<'_>> = reference.named_children(&mut cursor).collect();
    let (member, rest) = named.split_last()?;
    if !spec.ident_kinds.contains(&member.kind()) {
        return None;
    }
    let member = member.utf8_text(source).ok()?;
    let Some(receiver) = rest.iter().rev().find(|n| n.kind() != "type_arguments") else {
        return Some(Cow::Borrowed(member));
    };
    match receiver.kind() {
        "this" | "this_expression" => Some(Cow::Borrowed(member)),
        "identifier"
        | "type_identifier"
        | "simple_identifier"
        | "scoped_type_identifier"
        | "field_access"
        | "user_type" => {
            let text = receiver.utf8_text(source).ok()?;
            let qualifier = text.rsplit('.').next().unwrap_or(text).trim();
            Some(Cow::Owned(format!("{qualifier}.{member}")))
        }
        _ => None,
    }
}

/// Ruby `method(:name)` with no receiver: the method `name` handed on as a
/// `Method` object.
fn symbol_method_ref<'s>(call: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    if call.child_by_field_name("receiver").is_some()
        || call.child_by_field_name("method")?.utf8_text(source).ok()? != "method"
    {
        return None;
    }
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let first = args.named_children(&mut cursor).next()?;
    if first.kind() != "simple_symbol" {
        return None;
    }
    first
        .utf8_text(source)
        .ok()?
        .strip_prefix(':')
        .filter(|name| !name.is_empty())
}

/// The accessed field's short name from a member-access node. Tries the
/// grammar's named field child (`field` / `property` / `name`), else falls
/// back to the last identifier-like child (the token after `.` / `->`).
fn field_name_from(access: Node<'_>, source: &[u8]) -> Option<String> {
    // Kotlin / Swift wrap the field name in a `navigation_suffix` child rather
    // than exposing it as a direct field — descend into it first.
    let suffix = access.child_by_field_name("suffix").or_else(|| {
        let mut c = access.walk();
        let found = access
            .children(&mut c)
            .find(|ch| ch.kind() == "navigation_suffix");
        found
    });
    let suffix = suffix.unwrap_or(access);
    let field_node = suffix
        .child_by_field_name("field")
        .or_else(|| suffix.child_by_field_name("property"))
        .or_else(|| suffix.child_by_field_name("name"))
        .or_else(|| suffix.child_by_field_name("suffix"))
        .or_else(|| {
            let mut c = suffix.walk();
            suffix
                .children(&mut c)
                .filter(|ch| {
                    matches!(
                        ch.kind(),
                        "identifier"
                            | "property_identifier"
                            | "field_identifier"
                            | "simple_identifier"
                            | "shorthand_field_identifier"
                    )
                })
                .last()
        })?;
    let text = field_node.utf8_text(source).ok()?;
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn attach_pending_field_reads(
    containers: &[EnclosingContainer],
    reads: Vec<PendingCall>,
    nodes: &mut [RawNode],
) {
    if reads.is_empty() {
        return;
    }
    let targets = enclosing_targets(containers, &reads, |r| r.pos);
    for (read, target) in reads.into_iter().zip(targets) {
        if let Some(node_idx) = target {
            nodes[node_idx].field_reads.push(read.name);
        }
    }
}

/// Where a language's grammar names enum variants, for
/// [`extract_variant_refs`].
pub struct VariantRefSpec {
    /// Nodes whose whole text is a qualified path that may end in a variant:
    /// Rust `scoped_identifier`, Java `field_access`, TS `member_expression`.
    pub path_kinds: &'static [&'static str],
    /// Parent kinds that make their child a case label or pattern
    /// (`switch_case`, `when_condition`). Any kind ending in `pattern` does
    /// too, except through its `condition` (a Rust match guard).
    pub label_parents: &'static [&'static str],
    /// Parent kinds whose `identifier` child is a bare variant name: Java
    /// `case MONDAY:`.
    pub bare_label_parents: &'static [&'static str],
    /// C / C++: enumerators are named bare in any expression.
    pub bare_expressions: bool,
    /// Rust: `Q::V` token runs inside macro token trees (`matches!(x, E::A)`).
    pub token_trees: bool,
}

/// Walk the AST and record each enum-variant reference as a [`VariantRef`]
/// in the `field_reads` of the smallest enclosing function / method /
/// constructor, once per distinct entry.
///
/// Capture is a cheap syntactic filter; Pass 2 keeps only references that
/// resolve to an `EnumVariant`. A `.`-qualified path is kept only when its
/// qualifier starts upper-case (`Day.MONDAY`, not `obj.field`), and a bare C
/// / C++ name only when it does (`RED`, not a local): member accesses and
/// locals outnumber variant references by far, and enum names follow that
/// case in every grammar here.
pub fn extract_variant_refs(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    spec: &VariantRefSpec,
) {
    let containers = enclosing_containers(nodes);
    if containers.is_empty() {
        return;
    }
    let mut refs: Vec<PendingCall> = Vec::new();
    let mut push = |node: Node<'_>, path: &str, pattern: bool| {
        refs.push(PendingCall {
            pos: start_pos(node),
            name: VariantRef::encode(path, pattern),
        });
    };
    // Each node travels with its parent: `Node::parent` walks down from the
    // root, which every identifier of the file would pay.
    let mut stack: Vec<(Node<'_>, Option<Node<'_>>)> = vec![(root, None)];
    while let Some((n, parent)) = stack.pop() {
        let kind = n.kind();
        if spec.path_kinds.contains(&kind) && !is_path_suffix(n, parent) {
            if let Some(path) = n.utf8_text(source).ok().and_then(variant_path) {
                push(n, &path, in_label(n, parent, spec.label_parents));
            }
        } else if kind == "identifier" {
            if let Some(parent) = parent {
                let label = spec.bare_label_parents.contains(&parent.kind());
                if label || (spec.bare_expressions && is_bare_value(n, parent)) {
                    if let Some(name) = n.utf8_text(source).ok().filter(|t| starts_upper(t)) {
                        push(
                            n,
                            name,
                            label || in_label(n, Some(parent), spec.label_parents),
                        );
                    }
                }
            }
        } else if spec.token_trees && kind == "token_tree" {
            for path in token_tree_paths(n, source) {
                push(n, &path, false);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push((child, Some(n)));
        }
    }
    if refs.is_empty() {
        return;
    }
    let targets = enclosing_targets(&containers, &refs, |r| r.pos);
    let mut seen: rustc_hash::FxHashSet<(usize, String)> = rustc_hash::FxHashSet::default();
    for (entry, target) in refs.into_iter().zip(targets) {
        let Some(node_idx) = target else { continue };
        if seen.insert((node_idx, entry.name.clone())) {
            nodes[node_idx].field_reads.push(entry.name);
        }
    }
}

/// C++ nests `ns::Color::Blue` as `ns` + `Color::Blue`; the outer node
/// already holds the whole path.
fn is_path_suffix(node: Node<'_>, parent: Option<Node<'_>>) -> bool {
    parent.is_some_and(|parent| {
        parent.kind() == node.kind()
            && parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id())
    })
}

/// Whether `node`, a child of `parent`, is a case label or sits in a
/// pattern.
fn in_label(node: Node<'_>, parent: Option<Node<'_>>, label_parents: &[&str]) -> bool {
    if node.kind().ends_with("pattern") {
        return true;
    }
    let Some(parent) = parent else {
        return false;
    };
    let is_field =
        |field: &str| parent.child_by_field_name(field).map(|n| n.id()) == Some(node.id());
    if is_field("pattern") {
        return true;
    }
    !is_field("condition")
        && (parent.kind().ends_with("pattern") || label_parents.contains(&parent.kind()))
}

/// A C / C++ `identifier` in value position: not a declared name, a callee,
/// or the member of a qualified name.
fn is_bare_value(node: Node<'_>, parent: Node<'_>) -> bool {
    let is_field =
        |field: &str| parent.child_by_field_name(field).map(|n| n.id()) == Some(node.id());
    !parent.kind().ends_with("declarator")
        && !matches!(
            parent.kind(),
            "parameter_declaration" | "enumerator" | "preproc_def" | "preproc_function_def"
        )
        && !is_field("declarator")
        && !is_field("name")
        && !(parent.kind() == "call_expression" && is_field("function"))
}

fn starts_upper(text: &str) -> bool {
    text.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
}

/// The text of a qualified path with whitespace removed, when every segment
/// is an identifier and the path could end in a variant; else `None`.
fn variant_path(text: &str) -> Option<String> {
    let path: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let body = path.strip_prefix('\\').unwrap_or(&path);
    let mut segments: Vec<&str> = Vec::new();
    let mut dotted = false;
    let mut rest = body;
    loop {
        let end = rest.find([':', '.', '\\']).unwrap_or(rest.len());
        segments.push(&rest[..end]);
        let tail = &rest[end..];
        rest = if let Some(next) = tail.strip_prefix("::") {
            next
        } else if let Some(next) = tail.strip_prefix('.') {
            dotted = true;
            next
        } else if let Some(next) = tail.strip_prefix('\\') {
            next
        } else if tail.is_empty() {
            break;
        } else {
            return None;
        };
    }
    let is_ident = |s: &&str| {
        s.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && s.chars().all(|c| c.is_alphanumeric() || c == '_')
    };
    if segments.len() < 2 || !segments.iter().all(is_ident) {
        return None;
    }
    let qualifier = segments[segments.len() - 2];
    if dotted && !starts_upper(qualifier) {
        return None;
    }
    Some(path)
}

/// Rust `Q::V` runs among a macro token tree's direct tokens: `matches!`
/// patterns, `assert_eq!` arguments. Nested trees are walked on their own.
fn token_tree_paths(tree: Node<'_>, source: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut run: Vec<&str> = Vec::new();
    let mut after_sep = false;
    let mut flush = |run: &mut Vec<&str>| {
        if run.len() >= 2 {
            if let Some(path) = variant_path(&run.join("::")) {
                paths.push(path);
            }
        }
        run.clear();
    };
    let mut c = tree.walk();
    for token in tree.children(&mut c) {
        match token.kind() {
            "identifier" | "crate" | "self" | "super" => {
                let text = token.utf8_text(source).unwrap_or("");
                if !after_sep {
                    flush(&mut run);
                }
                run.push(text);
                after_sep = false;
            }
            "::" if !run.is_empty() && !after_sep => after_sep = true,
            _ => {
                flush(&mut run);
                after_sep = false;
            }
        }
    }
    flush(&mut run);
    paths
}

/// `(row, column)`.
pub type Pos = (u32, u32);

/// Inclusive `(start, end)` positions of a scope.
pub type PosRange = (Pos, Pos);

/// The `(start, end)` positions of a node.
#[inline]
pub fn node_range(node: Node<'_>) -> PosRange {
    let end = node.end_position();
    (start_pos(node), (safe_row(end.row), safe_row(end.column)))
}

/// Whether the inclusive `span` holds `pos`.
#[inline]
pub fn span_holds(span: (u32, u32, u32, u32), pos: Pos) -> bool {
    let (start, end) = span_ends(span);
    start <= pos && pos <= end
}

/// Width of a `(start, end)` range, ordered like [`span_width`].
#[inline]
pub fn range_width((start, end): PosRange) -> (u32, i64) {
    span_width((start.0, start.1, end.0, end.1))
}

#[derive(Clone, Copy)]
struct EnclosingContainer {
    start: Pos,
    end: Pos,
    width: (u32, i64),
    node_idx: usize,
}

struct PendingCall {
    pos: Pos,
    name: String,
}

fn is_container_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function | NodeKind::Method | NodeKind::Constructor
    )
}

fn span_ends(span: (u32, u32, u32, u32)) -> (Pos, Pos) {
    ((span.0, span.1), (span.2, span.3))
}

/// Rows first, then columns: a container nested on its parent's rows starts no
/// earlier and ends no later, so it ranks narrower.
pub fn span_width(span: (u32, u32, u32, u32)) -> (u32, i64) {
    (
        span.2.saturating_sub(span.0),
        i64::from(span.3) - i64::from(span.1),
    )
}

fn enclosing_containers(nodes: &[RawNode]) -> Vec<EnclosingContainer> {
    let mut containers: Vec<EnclosingContainer> = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| is_container_kind(node.kind))
        .map(|(node_idx, node)| {
            let (start, end) = span_ends(node.span);
            EnclosingContainer {
                start,
                end,
                width: span_width(node.span),
                node_idx,
            }
        })
        .collect();
    containers.sort_unstable_by_key(|c| (c.start, c.width, c.node_idx));
    containers
}

/// For each pending entry, the node index of the smallest container whose
/// span holds its position.
fn enclosing_targets<T>(
    containers: &[EnclosingContainer],
    pending: &[T],
    pos_of: impl Fn(&T) -> Pos,
) -> Vec<Option<usize>> {
    let mut targets: Vec<Option<usize>> = vec![None; pending.len()];
    if containers.is_empty() {
        return targets;
    }
    let mut order: Vec<usize> = (0..pending.len()).collect();
    order.sort_unstable_by_key(|&idx| pos_of(&pending[idx]));

    let mut active: BinaryHeap<Reverse<((u32, i64), usize)>> = BinaryHeap::new();
    let mut next_container = 0usize;

    for &idx in &order {
        let pos = pos_of(&pending[idx]);
        while next_container < containers.len() && containers[next_container].start <= pos {
            active.push(Reverse((containers[next_container].width, next_container)));
            next_container += 1;
        }
        while let Some(Reverse((_, container_idx))) = active.peek().copied() {
            if containers[container_idx].end >= pos {
                targets[idx] = Some(containers[container_idx].node_idx);
                break;
            }
            active.pop();
        }
    }
    targets
}

fn attach_pending_calls(
    containers: &[EnclosingContainer],
    calls: Vec<PendingCall>,
    nodes: &mut [RawNode],
) {
    let targets = enclosing_targets(containers, &calls, |c| c.pos);
    for (call, target) in calls.into_iter().zip(targets) {
        if let Some(node_idx) = target {
            nodes[node_idx].calls.push(call.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_node(name: &str, kind: NodeKind, span: (u32, u32, u32, u32)) -> RawNode {
        RawNode {
            name: name.to_string(),
            kind,
            span,
            is_exported: false,
            heritage: vec![],
            type_annotation: None,
            decorators: vec![],
            calls: vec![],
            field_reads: Vec::new(),
            owner_class: None,
            content_hash: 0,
        }
    }

    #[test]
    fn attach_pending_calls_uses_smallest_enclosing_container() {
        let mut nodes = vec![
            raw_node("outer", NodeKind::Function, (1, 0, 20, 0)),
            raw_node("inner", NodeKind::Function, (5, 0, 10, 0)),
            raw_node("not_container", NodeKind::Variable, (6, 0, 6, 8)),
        ];
        let containers = enclosing_containers(&nodes);

        attach_pending_calls(
            &containers,
            vec![PendingCall {
                pos: (6, 4),
                name: "callee".to_string(),
            }],
            &mut nodes,
        );

        assert!(nodes[0].calls.is_empty());
        assert_eq!(nodes[1].calls, ["callee"]);
    }

    #[test]
    fn attach_pending_calls_preserves_original_call_order() {
        let mut nodes = vec![raw_node("f", NodeKind::Function, (1, 0, 20, 0))];
        let containers = enclosing_containers(&nodes);

        attach_pending_calls(
            &containers,
            vec![
                PendingCall {
                    pos: (12, 0),
                    name: "later".to_string(),
                },
                PendingCall {
                    pos: (3, 0),
                    name: "earlier".to_string(),
                },
            ],
            &mut nodes,
        );

        assert_eq!(nodes[0].calls, ["later", "earlier"]);
    }

    #[test]
    fn attach_pending_calls_same_line_containers_split_by_column() {
        let mut nodes = vec![
            raw_node("a", NodeKind::Function, (0, 0, 0, 20)),
            raw_node("b", NodeKind::Function, (0, 21, 0, 40)),
            raw_node("inner", NodeKind::Function, (0, 25, 0, 35)),
        ];
        let containers = enclosing_containers(&nodes);
        let call = |col, name: &str| PendingCall {
            pos: (0, col),
            name: name.to_string(),
        };

        attach_pending_calls(
            &containers,
            vec![
                call(5, "x"),
                call(22, "y"),
                call(30, "z"),
                call(20, "at_end"),
                call(21, "at_start"),
                call(50, "outside"),
            ],
            &mut nodes,
        );

        assert_eq!(nodes[0].calls, ["x", "at_end"]);
        assert_eq!(nodes[1].calls, ["y", "at_start"]);
        assert_eq!(nodes[2].calls, ["z"]);
    }

    #[test]
    fn attach_pending_calls_identical_spans_pick_first_node() {
        let mut nodes = vec![
            raw_node("first", NodeKind::Function, (0, 0, 3, 0)),
            raw_node("second", NodeKind::Function, (0, 0, 3, 0)),
        ];
        let containers = enclosing_containers(&nodes);
        attach_pending_calls(
            &containers,
            vec![PendingCall {
                pos: (1, 0),
                name: "c".to_string(),
            }],
            &mut nodes,
        );
        assert_eq!(nodes[0].calls, ["c"]);
        assert!(nodes[1].calls.is_empty());
    }

    #[test]
    fn attach_to_enclosing_same_line_functions_split_by_column() {
        let mut nodes = vec![
            raw_node("a", NodeKind::Function, (0, 0, 0, 20)),
            raw_node("b", NodeKind::Function, (0, 21, 0, 40)),
        ];
        assert_eq!(
            attach_to_enclosing((0, 25), "y".into(), &mut nodes),
            Some((1, 0))
        );
        assert_eq!(
            attach_to_enclosing((0, 5), "x".into(), &mut nodes),
            Some((0, 0))
        );
        assert_eq!(attach_to_enclosing((1, 0), "m".into(), &mut nodes), None);
        assert_eq!(nodes[0].calls, ["x"]);
        assert_eq!(nodes[1].calls, ["y"]);
    }

    #[test]
    fn test_construction_call_generic_segments_returns_path_without_arguments() {
        for (text, want) in [
            ("Box", "Box"),
            ("Box<String>", "Box"),
            ("ns::Box<int>", "ns::Box"),
            ("Outer<int>.Inner", "Outer.Inner"),
            ("Outer<String>.Inner<Integer>", "Outer.Inner"),
            ("Map<K, List<V>>::Entry", "Map::Entry"),
            (" Widget ", "Widget"),
        ] {
            let entry = construction_call(text).expect(text);
            assert_eq!(
                CallSite::parse(&entry),
                CallSite::Construct(want),
                "{text:?}"
            );
        }
        assert_eq!(construction_call(""), None);
        assert_eq!(construction_call("<T>"), None);
    }

    #[test]
    fn test_variant_ref_encode_parse_round_trips_and_ignores_field_names() {
        for (path, pattern) in [("E::A", false), ("Day.MONDAY", true), ("RED", false)] {
            let entry = VariantRef::encode(path, pattern);
            assert_eq!(
                VariantRef::parse(&entry),
                Some(VariantRef { path, pattern }),
                "{entry:?}"
            );
        }
        assert_eq!(VariantRef::parse("timeout"), None);
        assert_eq!(VariantRef::parse("variant E::A"), None);
    }

    #[test]
    fn test_name_ref_encode_parse_round_trips_and_ignores_other_entries() {
        for (name, kind) in [
            ("LIMITS", NameRefKind::Value),
            ("TIMEOUT", NameRefKind::Constant),
            ("Foo.bar", NameRefKind::Function),
        ] {
            let entry = NameRef::encode(name, kind);
            assert_eq!(
                NameRef::parse(&entry),
                Some(NameRef { name, kind }),
                "{entry:?}"
            );
            assert_eq!(VariantRef::parse(&entry), None, "{entry:?}");
        }
        assert_eq!(NameRef::parse("timeout"), None);
        assert_eq!(NameRef::parse(&VariantRef::encode("E::A", false)), None);
    }

    #[test]
    fn test_variant_ref_split_last_separator_returns_qualifier_path_and_variant() {
        fn split(path: &str) -> Option<(&str, &str)> {
            VariantRef {
                path,
                pattern: false,
            }
            .split()
        }
        assert_eq!(split("E::A"), Some(("E", "A")));
        assert_eq!(split("crate::m::E::A"), Some(("crate::m::E", "A")));
        assert_eq!(split("pkg.Color.RED"), Some(("pkg.Color", "RED")));
        assert_eq!(
            split("\\App\\Status::Active"),
            Some(("\\App\\Status", "Active"))
        );
        assert_eq!(split("RED"), None);
    }

    #[test]
    fn test_variant_path_shapes_keep_qualified_identifiers_only() {
        for (text, want) in [
            ("E::A", Some("E::A")),
            (
                "crate::error::EcpError::AmbiguousSymbol",
                Some("crate::error::EcpError::AmbiguousSymbol"),
            ),
            ("std::fs::read", Some("std::fs::read")),
            ("Day.MONDAY", Some("Day.MONDAY")),
            ("Suit\n  .clubs", Some("Suit.clubs")),
            ("\\App\\Status::Active", Some("\\App\\Status::Active")),
            // A lower-case `.` qualifier is a value, not a type.
            ("obj.field", None),
            ("self.count", None),
            ("a?.b", None),
            ("f().B", None),
            ("Vec::<u8>::new", None),
            ("A", None),
            ("A::", None),
        ] {
            assert_eq!(variant_path(text).as_deref(), want, "{text:?}");
        }
    }
}
