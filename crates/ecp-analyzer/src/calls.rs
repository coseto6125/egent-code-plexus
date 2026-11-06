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

/// Walk the AST and attach `callee` names to their [`holder`] by span
/// containment. Reused across languages — the `call_kinds` set lists this language's AST node kinds that
/// represent a function-call expression (e.g. "call_expression" in JS/TS,
/// "method_invocation" in Java, "function_call_expression" in PHP).
pub fn extract_calls(root: Node<'_>, source: &[u8], nodes: &mut [RawNode], call_kinds: &[&str]) {
    let mut calls: Vec<PendingCall> = Vec::new();
    let mut sites: Vec<Node<'_>> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if call_kinds.contains(&n.kind()) {
            let callee_name = callee_name_from(n, source);
            if let Some(name) = callee_name {
                calls.push(PendingCall {
                    pos: start_pos(n),
                    name,
                });
                sites.push(n);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    attach_pending_calls(calls, nodes, |i, run| declared_in(sites[i], source, run));
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

/// Attach the call at `at` to its [`holder`] by its full span, so one-line
/// closures stay apart.
/// Returns the caller's node index and the call's index in its `calls`.
pub fn attach_to_enclosing_span(
    at: Node<'_>,
    source: &[u8],
    callee: String,
    nodes: &mut [RawNode],
) -> Option<(usize, u32)> {
    let i = holder_of_span(at, source, nodes)?;
    Some(push_call(i, callee, nodes))
}

/// Attach the call at `at` to its [`holder`], by row and column so functions
/// sharing a line stay apart.
/// Returns the caller's node index and the call's index in its `calls`.
pub fn attach_to_enclosing(
    at: Node<'_>,
    source: &[u8],
    callee: String,
    nodes: &mut [RawNode],
) -> Option<(usize, u32)> {
    let i = holder_at(at, source, nodes)?;
    Some(push_call(i, callee, nodes))
}

fn push_call(i: usize, callee: String, nodes: &mut [RawNode]) -> (usize, u32) {
    let calls = &mut nodes[i].calls;
    let idx = calls.len() as u32;
    calls.push(callee);
    (i, idx)
}

/// The [`holder`] of `at`, ranked by area.
fn holder_of_span(at: Node<'_>, source: &[u8], nodes: &[RawNode]) -> Option<usize> {
    let span = crate::framework_helpers::node_span(&at);
    holder(
        nodes,
        start_pos(at),
        |node| crate::framework_helpers::span_contains(node.span, span),
        |node| crate::framework_helpers::span_area(node.span),
        |run| declared_in(at, source, run),
    )
}

/// The [`holder`] of `at`, by row and column.
fn holder_at(at: Node<'_>, source: &[u8], nodes: &[RawNode]) -> Option<usize> {
    holder_at_pos(start_pos(at), nodes, |run| declared_in(at, source, run))
}

fn holder_at_pos<'s>(
    pos: Pos,
    nodes: &[RawNode],
    declared: impl FnOnce(PosRange) -> Vec<Declarator<'s>>,
) -> Option<usize> {
    holder(
        nodes,
        pos,
        |node| span_holds(node.span, pos),
        |node| span_width(node.span),
        declared,
    )
}

/// Which tier of [`holder`] a node of `kind` belongs to.
fn holder_tier(kind: NodeKind) -> Option<usize> {
    match kind {
        k if is_container_kind(k) => Some(0),
        k if is_value_kind(k) => Some(1),
        k if ecp_core::analyzer::heritage::holds_heritage(k) => Some(2),
        _ => None,
    }
}

/// The node that owns code at `pos`, among the nodes that `holds` accepts:
/// the smallest enclosing function, else the smallest enclosing value
/// declaration (an initializer, a property accessor), else the smallest
/// enclosing type (a static block, a class body). Without the last two, a
/// call there had no caller and `ecp impact` missed it. Values that share one
/// statement hold the code only through their own declarator
/// ([`pick_declared`] over the declarators that `declared` lists for the
/// statement's range); code that no declarator names goes to the type. A
/// module or namespace holds nothing: a Rust `#[cfg(test)] mod` would make
/// test code a non-test caller.
fn holder<'s, K: Ord + Copy>(
    nodes: &[RawNode],
    pos: Pos,
    holds: impl Fn(&RawNode) -> bool,
    size: impl Fn(&RawNode) -> K,
    declared: impl FnOnce(PosRange) -> Vec<Declarator<'s>>,
) -> Option<usize> {
    let mut best: [Option<(K, usize)>; 3] = [None; 3];
    for (i, node) in nodes.iter().enumerate() {
        let Some(tier) = holder_tier(node.kind) else {
            continue;
        };
        if !holds(node) {
            continue;
        }
        let key = size(node);
        if best[tier].is_none_or(|(smallest, _)| key < smallest) {
            best[tier] = Some((key, i));
        }
    }
    if let Some((_, i)) = best[0] {
        return Some(i);
    }
    if let Some((_, head)) = best[1] {
        let end = span_ends(nodes[head].span).1;
        let run: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                is_value_kind(node.kind) && span_ends(node.span).1 == end && holds(node)
            })
            .map(|(i, _)| i)
            .collect();
        let start = run
            .iter()
            .map(|&i| span_ends(nodes[i].span).0)
            .min()
            .unwrap_or(end);
        let declarators = declared((start, end));
        if let Some(i) = pick_declared(&run, |&i| i, (start, end), &declarators, pos, nodes) {
            return Some(i);
        }
    }
    best[2].map(|(_, i)| i)
}

/// The declarators of each statement around `at` that declares several
/// values, up to the statement that `within` spans: `a = h1()` and
/// `b = h2()` of `const a = h1(), b = h2()`.
fn declared_in<'s>(at: Node<'_>, source: &'s [u8], within: PosRange) -> Vec<Declarator<'s>> {
    let (start, end) = within;
    let mut declarators = Vec::new();
    let mut node = at;
    while let Some(parent) = node.parent() {
        let (from, to) = node_range(parent);
        // A row past the value's end still belongs to its statement: a span
        // may stop short of the `;`.
        if from < start || to.0 > end.0 {
            break;
        }
        declarators.extend(statement_declarators(parent, source));
        node = parent;
    }
    declarators
}

/// The declarators of `statement` when it declares two or more values;
/// empty otherwise. Calls ([`declared_in`]) and reads ([`field_reads_walk`])
/// both split a shared statement by this rule.
fn statement_declarators<'s>(statement: Node<'_>, source: &'s [u8]) -> Vec<Declarator<'s>> {
    let paired = paired_declarators(statement, source);
    if !paired.is_empty() {
        return paired;
    }
    let range = node_range(statement);
    // C `int a, b = g();` lists the bare `a` in a `declarator` field next to
    // the `init_declarator`; it still counts as a declarator.
    let mut shared = named_fields(statement, "declarator");
    let by_field = shared.len() >= 2;
    if !by_field {
        let mut cursor = statement.walk();
        shared = statement
            .named_children(&mut cursor)
            .filter(|child| is_declarator_kind(child.kind()))
            .collect();
    }
    if shared.len() < 2 {
        return Vec::new();
    }
    // A C++ member `int a, b = Y;` keeps `Y` in a `default_value` field, and
    // `unsigned a : 1, b : W;` keeps `W` in a bitfield clause, after `b` and
    // outside `b`'s node: each declarator there reaches to the next, and the
    // last to the statement's last named child.
    let mut ends: Vec<Pos> = shared.iter().map(|d| node_range(*d).1).collect();
    if by_field {
        for (end, next) in ends.iter_mut().zip(&shared[1..]) {
            *end = node_range(*next).0;
        }
        let mut cursor = statement.walk();
        if let Some(tail) = statement.named_children(&mut cursor).last() {
            let last = ends.len() - 1;
            ends[last] = ends[last].max(node_range(tail).1);
        }
    }
    shared
        .into_iter()
        .zip(ends)
        .map(|(d, to)| Declarator {
            range: (node_range(d).0, to),
            name: declared_name(d.child_by_field_name("name").unwrap_or(d), source),
            statement: range,
        })
        .collect()
}

/// A node that declares one value of its statement and holds that value's
/// initializer: `variable_declarator`, C `init_declarator`, Dart
/// `initialized_identifier`, PHP `property_element`.
fn is_declarator_kind(kind: &str) -> bool {
    kind.contains("declarator")
        || matches!(
            kind,
            "initialized_identifier" | "static_final_declaration" | "property_element"
        )
}

/// Go `var C, D = h1(), h2()`, Swift `let a = f(), b = g()` and C++ members
/// `int a = f(), b = g();` list names and values in parallel fields, so the
/// i-th name declares the i-th value. Counts that differ pair nothing:
/// `var cfg, _ = load()` fills every name from one call. Only a declaration
/// pairs: a Swift labelled tuple `(width: W, size: S)` has the same fields
/// and declares nothing.
fn paired_declarators<'s>(statement: Node<'_>, source: &'s [u8]) -> Vec<Declarator<'s>> {
    let kind = statement.kind();
    if !(kind.contains("declaration") || kind.ends_with("_spec")) {
        return Vec::new();
    }
    let mut names = named_fields(statement, "name");
    if names.len() < 2 {
        names = named_fields(statement, "declarator");
    }
    if names.len() < 2 {
        return Vec::new();
    }
    let range = node_range(statement);
    let mut values = named_fields(statement, "value");
    if values.is_empty() {
        values = named_fields(statement, "default_value");
    }
    if let [list] = values[..] {
        if list.kind() == "expression_list" {
            let mut cursor = list.walk();
            values = list.named_children(&mut cursor).collect();
        }
    }
    if values.len() != names.len() {
        return Vec::new();
    }
    names
        .into_iter()
        .zip(values)
        .map(|(name, value)| Declarator {
            range: node_range(value),
            name: declared_name(name, source),
            statement: range,
        })
        .collect()
}

fn named_fields<'t>(node: Node<'t>, field: &str) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.children_by_field_name(field, &mut cursor)
        .filter(|child| child.is_named())
        .collect()
}

/// The bare name that `name` declares: `b`, the `b` of C `*b`, the `b` a
/// Swift `pattern` or a PHP `$b` wraps; none for `{ cb }`.
fn declared_name<'s>(mut name: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    while let Some(inner) = name.child_by_field_name("declarator") {
        name = inner;
    }
    if matches!(name.kind(), "pattern" | "variable_name") && name.named_child_count() == 1 {
        name = name.named_child(0)?;
    }
    (name.named_child_count() == 0)
        .then(|| name.utf8_text(source).ok())
        .flatten()
}

/// Records `name` as a function value the [`holder`] of `at` hands on:
/// Pass 2 binds it as a `References` edge, never as a call. A name already
/// recorded for that holder stays one entry. Returns the holder's index.
pub fn reference_function(
    at: Node<'_>,
    source: &[u8],
    name: &str,
    nodes: &mut [RawNode],
) -> Option<usize> {
    let holder = holder_at(at, source, nodes)?;
    push_function_reference(holder, name, nodes);
    Some(holder)
}

/// [`reference_function`] for a holder located by `at`'s full span, so
/// one-line closures stay apart.
pub fn reference_function_span(at: Node<'_>, source: &[u8], name: &str, nodes: &mut [RawNode]) {
    if let Some(holder) = holder_of_span(at, source, nodes) {
        push_function_reference(holder, name, nodes);
    }
}

fn push_function_reference(holder: usize, name: &str, nodes: &mut [RawNode]) {
    let entry = NameRef::encode(name, NameRefKind::Function);
    if !nodes[holder].field_reads.contains(&entry) {
        nodes[holder].field_reads.push(entry);
    }
}

/// Walk the AST and attach the **field name** of each member-access read
/// (`obj.field`, `self.count`, `cfg->timeout`) to the smallest enclosing
/// function/method/constructor `RawNode`'s `field_reads`, else (an initializer
/// outside every function) the smallest enclosing const/variable/property.
/// Mirrors `extract_calls`; `field_kinds` lists this language's member-access AST node
/// kinds (e.g. `member_expression` in JS/TS, `field_expression` in C/C++/Rust,
/// `selector_expression` in Go).
///
/// A member access that is the callee of a call (`obj.method()`) is skipped —
/// `extract_calls` already records that as a `Calls` edge, and the field name
/// there is a method, not a data field. The check is structural: if the access
/// node is the `function` child of its parent call, it is a callee.
///
/// The same walk records each bare name the body uses as a value
/// ([`NameRef`]), once per its smallest enclosing function, else value
/// declaration ([`ReadContainers`]), as `names` describes.
///
/// `called` holds the node ids of bare names the language's call walker
/// already recorded as calls (Rust macro arguments, `println!("{}", f())`):
/// such a name reads no value, so one site gives one edge. Empty for every
/// other language.
pub fn extract_field_reads(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    field_kinds: &[&str],
    names: &NameRefSpec,
    called: &CalledNames,
) {
    field_reads_walk(root, source, nodes, field_kinds, names, None, called);
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
    let called = CalledNames::default();
    field_reads_walk(
        root,
        source,
        nodes,
        field_kinds,
        names,
        literal_key_facts,
        &called,
    );
}

/// Node ids of bare names a call walker recorded as calls
/// ([`extract_field_reads`]).
pub type CalledNames = rustc_hash::FxHashSet<usize>;

fn field_reads_walk(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    field_kinds: &[&str],
    names: &NameRefSpec,
    literal_key_facts: Option<&LiteralKeyFacts>,
    called: &CalledNames,
) {
    let mut containers = ReadContainers::new(nodes);
    if containers.is_empty() {
        return;
    }
    let mut reads: Vec<PendingCall> = Vec::new();
    let mut scan = NameScan::default();
    let mut declarators: Vec<Declarator<'_>> = Vec::new();
    let mut memo = KindMemo::new(names, field_kinds);
    // Each node travels with its parent and its field in that parent:
    // `Node::parent` walks down from the root, which every node would pay.
    let mut stack: Vec<Frame<'_>> = vec![Frame {
        node: root,
        parent: None,
        parent_kind: 0,
        field: 0,
        scope: NameScope::Value,
        local_scope: None,
    }];
    while let Some(frame) = stack.pop() {
        let n = frame.node;
        let kind = memo.kind(n);
        if kind & kind_flag::FIELD_ACCESS != 0 && !is_direct_callee(n, frame.parent) {
            if let Some(name) = field_name_from(n, source) {
                reads.push(PendingCall {
                    pos: start_pos(n),
                    name,
                });
            }
        }
        let mut scope = frame.scope;
        if !matches!(scope, NameScope::Skip | NameScope::Off)
            && scan.visit(&frame, kind, source, &mut memo, called)
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
            let mut local_scope = if kind & kind_flag::COMPREHENSION != 0 {
                Some(LocalScope::comprehension(node_range(n)))
            } else if kind & kind_flag::COMPREHENSION_CLAUSE != 0 {
                frame
                    .parent
                    .map(|p| LocalScope::comprehension(node_range(p)))
            } else {
                frame.local_scope
            };
            // Two of a kind mark a statement that may declare several values;
            // only those pay for `statement_declarators`.
            let (mut declarator_kinds, mut name_fields, mut declarator_fields) = (0u32, 0u32, 0u32);
            let mut after_dot = 0;
            let mut path = PathTokens::default();
            loop {
                let child = cursor.node();
                let field = memo.field(&cursor);
                let child_kind = memo.kind(child);
                let mut child_scope = local_scope;
                if opens_parameters(kind, field, child_kind) {
                    // C++ `lambda_declarator` and Swift `lambda_function_type`
                    // hold the parameters; their parent holds the body too.
                    let owner = if kind & kind_flag::DECLARATOR != 0 || scope == NameScope::Skip {
                        frame.parent.unwrap_or(n)
                    } else {
                        n
                    };
                    child_scope = Some(LocalScope::lambda(node_range(owner)));
                    // A type's parameter list (Java record, C# primary
                    // constructor) scopes only its own names: the members
                    // keep the outer scope.
                    if kind & kind_flag::TYPE_DECL == 0 {
                        local_scope = child_scope;
                    }
                }
                if child.is_named() {
                    declarator_kinds += u32::from(child_kind & kind_flag::VALUE_DECLARATOR != 0);
                    name_fields += u32::from(field & field_flag::NAME != 0);
                    declarator_fields += u32::from(field & field_flag::DECLARATOR != 0);
                }
                stack.push(Frame {
                    node: child,
                    parent: Some(n),
                    parent_kind: kind,
                    field: field | after_dot,
                    scope: names.child_scope(parent, child_kind, field, &cursor, literal_key_facts),
                    local_scope: child_scope,
                });
                // A comment between `.` and the name (`v./* c */join`) keeps
                // the mark for the name.
                if !child.is_extra() {
                    after_dot = 0;
                    if kind & kind_flag::TOKEN_TREE != 0 {
                        path.see(&mut stack, child_kind);
                        if child_kind & kind_flag::DOT != 0 {
                            after_dot = field_flag::AFTER_DOT;
                        }
                    }
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            if declarator_kinds.max(name_fields).max(declarator_fields) >= 2 {
                declarators.extend(statement_declarators(n, source));
            }
        }
    }
    containers.set_declarators(declarators);
    attach_pending_field_reads(&containers, reads, nodes);
    scan.attach(&containers, nodes, names.scoped_qualifiers);
}

/// Marks the path segments among the loose tokens of one token tree. A
/// name's frame stays on the walk's stack until its siblings are pushed, so
/// the `::` after it sets [`field_flag::BEFORE_PATH`] there; asking the name
/// for its next sibling instead walks down from the root.
#[derive(Default)]
struct PathTokens {
    /// Stack slot and kind flags of the previous token.
    last: Option<(usize, u32)>,
    /// The name the last `::` marked.
    marked: Option<usize>,
    /// The name of an open turbofish (`handle::<A>`), a value until a `::`
    /// follows the `>` (`Foo::<u8>::BAR`).
    turbofish: Option<usize>,
}

impl PathTokens {
    fn see(&mut self, stack: &mut [Frame<'_>], kind: u32) {
        use kind_flag as k;
        if kind & k::PATH_SEP != 0 {
            self.marked = match self.last {
                Some((_, last)) if last & k::ANGLE_CLOSE != 0 => self.turbofish.take(),
                last => last.map(|(slot, _)| slot),
            };
            if let Some(slot) = self.marked {
                stack[slot].field |= field_flag::BEFORE_PATH;
            }
        } else if kind & k::ANGLE_OPEN != 0 {
            if let Some(slot) = self.marked.take() {
                stack[slot].field &= !field_flag::BEFORE_PATH;
                self.turbofish = Some(slot);
            }
        } else {
            self.marked = None;
        }
        self.last = Some((stack.len() - 1, kind));
    }
}

/// Whether `child`, under a node of kind flags `parent_kind`, lists the
/// parameters of a function: the parent then scopes them and its body.
fn opens_parameters(parent_kind: u32, field: u16, child_kind: u32) -> bool {
    parent_kind & kind_flag::PARAMETER == 0
        && child_kind & kind_flag::TYPE_PARAMETERS == 0
        && (field & field_flag::PARAMS != 0 || child_kind & kind_flag::PARAMETER != 0)
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

/// True when `parent` is a call/invocation whose `function` field is
/// `access`, the direct callee: such nodes are method calls handled by
/// `extract_calls`, not data-field reads. A parenthesized callee
/// `(self.f)(x)` is not direct: it still reads the field `f`.
fn is_direct_callee(access: Node<'_>, parent: Option<Node<'_>>) -> bool {
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

/// [`is_direct_callee`] seen through parentheses (`(a.m)()`): the method-value
/// arms skip such an access because a call already records it.
pub(crate) fn is_call_callee(access: Node<'_>, parent: Option<Node<'_>>) -> bool {
    let mut outer = access;
    let mut parent = parent;
    while let Some(wrapper) = parent.filter(|p| p.kind() == "parenthesized_expression") {
        outer = wrapper;
        parent = wrapper.parent();
    }
    is_direct_callee(outer, parent)
}

/// `node` with the parentheses around it removed: the callee `(a.m)` of
/// `(a.m)(1)` is `a.m`, which a call records as it records `a.m(1)`. A
/// parenthesized node with no single inner node stays as it is.
pub(crate) fn unparenthesized(mut node: Node<'_>) -> Node<'_> {
    while node.kind() == "parenthesized_expression" && node.named_child_count() == 1 {
        let Some(inner) = node.named_child(0) else {
            break;
        };
        node = inner;
    }
    node
}

/// Reads the name a qualified value node records: see
/// [`NameRefSpec::qualified_name`].
pub type QualifiedName = fn(Node<'_>, &[u8], &NameRefSpec) -> Option<String>;

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
    /// Qualified-path or member-access kinds whose last segment may name a
    /// function when the node sits in value position (a call argument, a
    /// `let` or assignment value, a returned or stored value): Rust
    /// `scoped_identifier` (`for_each(parse::migrate)`), Python `attribute`
    /// (`map(mod.f, xs)`), TS `member_expression` (`xs.map(ns.f)`).
    ///
    /// A language may list a kind when all of these hold:
    /// - its [`Self::qualified_name`] reader returns the qualified text
    ///   (`qualifier.member`, every qualifier segment kept, or the path text)
    ///   only for the outermost node of a chain, and `None` for a callee,
    ///   a `this` / `self` / `super` / `base` qualifier and a computed one;
    /// - the callee, type, import, pattern and macro positions reach the
    ///   node under a non-value field, a skip kind or a skipped scope, so
    ///   only value uses remain.
    ///
    /// The qualifier may be a module, a type or a value. A local or parameter
    /// named like its first segment hides the name in the scan (unless
    /// [`Self::scoped_qualifiers`]); the resolver binds the rest only through
    /// a module, a namespace or a type, with a multi-segment qualifier as the
    /// path that encloses the function (`name_ref::resolve_name_ref`).
    ///
    /// The scan still descends into the node, so a qualifier that is a value
    /// (`obj` in `obj.f`) keeps its own read.
    pub qualified_values: &'static [&'static str],
    /// The name a [`Self::qualified_values`] node records, or `None` to
    /// record nothing. Defaults to [`qualified_reference`], which reads a
    /// `name` field and the path text.
    pub qualified_name: QualifiedName,
    /// Whether a qualifier of a recorded qualified name lives where no
    /// local can shadow it: C++ `ns::f`, whose `ns` is a namespace or type
    /// scope. Elsewhere a local named like the qualifier's first segment
    /// (`def run(mod): map(mod.f, xs)`) makes the value a member of that
    /// local, and the scan drops it.
    pub scoped_qualifiers: bool,
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
    /// The innermost function whose parameters precede this node, or the
    /// comprehension that holds it, which scopes the names declared below it.
    local_scope: Option<LocalScope>,
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
    /// A generic's type or template parameter list, which declares no value.
    pub const TYPE_PARAMETERS: u32 = 1 << 17;
    /// A Dart `for_element`, whose variables live inside it only. A C#
    /// `query_expression` is none: its first source binds outside it.
    pub const COMPREHENSION: u32 = 1 << 18;
    /// A Python `for_in_clause`: its variables live in the comprehension that
    /// holds it. A walrus elsewhere in that comprehension binds outside it.
    pub const COMPREHENSION_CLAUSE: u32 = 1 << 19;
    /// A class, record or struct declaration, whose parameter list (Java
    /// record, C# primary constructor) is no lambda over its members.
    pub const TYPE_DECL: u32 = 1 << 20;
    /// A Rust macro argument list or `$( .. )*` repetition: bare tokens, no
    /// member node.
    pub const TOKEN_TREE: u32 = 1 << 21;
    /// A `.` token.
    pub const DOT: u32 = 1 << 22;
    /// A kind of the spec's [`NameRefSpec::qualified_values`].
    pub const QUALIFIED_VALUE: u32 = 1 << 23;
    /// A node that declares one value of its statement ([`super::is_declarator_kind`]).
    pub const VALUE_DECLARATOR: u32 = 1 << 24;
    /// A `::` token.
    pub const PATH_SEP: u32 = 1 << 25;
    /// A `<` token.
    pub const ANGLE_OPEN: u32 = 1 << 26;
    /// A `>` or `>>` token.
    pub const ANGLE_CLOSE: u32 = 1 << 27;
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
    /// `parameter` / `parameters`: a function's parameter list.
    pub const PARAMS: u16 = 1 << 9;
    /// Not a field: a token-tree child right after a `.` token, the member
    /// name of `parts.join(",")` inside `format!(..)`.
    pub const AFTER_DOT: u16 = 1 << 10;
    /// `declarator`: C, C++ and Java list each declared name in one.
    pub const DECLARATOR: u16 = 1 << 11;
    /// Not a field: a token-tree name right before `::`, a path segment
    /// (module, type, generic parameter) of `S::NAME` inside a macro. A
    /// turbofish `handle::<A>` keeps `handle` a value.
    pub const BEFORE_PATH: u16 = 1 << 12;
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
            (
                kind.starts_with("type_parameter") || kind.starts_with("template_parameter"),
                TYPE_PARAMETERS,
            ),
            (kind.contains("declarator"), DECLARATOR),
            (is_declarator_kind(kind), VALUE_DECLARATOR),
            (kind == "for_element", COMPREHENSION),
            (kind == "for_in_clause", COMPREHENSION_CLAUSE),
            (
                kind.ends_with("class_declaration")
                    || kind.ends_with("record_declaration")
                    || kind.ends_with("struct_declaration"),
                TYPE_DECL,
            ),
            (spec.method_refs.contains(&kind), METHOD_REF),
            (spec.qualified_values.contains(&kind), QUALIFIED_VALUE),
            (spec.symbol_method_refs && kind == "call", SYMBOL_CALL),
            (spec.binding_parents.contains(&kind), BINDING_PARENT),
            (spec.skip_parents.contains(&kind), SKIP_PARENT),
            (spec.call_suffixes.contains(&kind), CALL_SUFFIX),
            (spec.infix_calls.contains(&kind), INFIX_CALL),
            (kind.contains("assignment"), ASSIGNMENT),
            (
                matches!(kind, "token_tree" | "token_repetition"),
                TOKEN_TREE,
            ),
            (kind == ".", DOT),
            (kind == "::", PATH_SEP),
            (kind == "<", ANGLE_OPEN),
            (matches!(kind, ">" | ">>"), ANGLE_CLOSE),
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
            (field == "declarator", DECLARATOR),
            (field == "left", LEFT),
            (matches!(field, "parameter" | "parameters"), PARAMS),
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
        qualified_values: &[],
        qualified_name: qualified_reference,
        scoped_qualifiers: false,
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
        if field & (f::NON_VALUE | f::AFTER_DOT | f::BEFORE_PATH) != 0
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

/// A bare name read inside a body, pending its smallest enclosing function,
/// else value declaration ([`ReadContainers`]).
struct PendingName<'s> {
    pos: Pos,
    name: Cow<'s, str>,
    kind: NameRefKind,
}

/// A local name a declaration introduces at `pos`.
struct PendingBinding<'s> {
    pos: Pos,
    name: &'s str,
    /// The innermost function or comprehension that scopes it
    /// ([`Frame::local_scope`]): its scope when no function node holds it, or when
    /// it is a comprehension inside the function node that holds it.
    local_scope: Option<LocalScope>,
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
        called: &CalledNames,
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
            let binding = PendingBinding {
                pos: start_pos(n),
                name,
                local_scope: frame.local_scope,
            };
            match frame.scope {
                NameScope::Binding => self.bindings.push(binding),
                NameScope::Value if frame.parent_kind & kind_flag::GLOBAL_DECL != 0 => {
                    self.globals.push(binding);
                }
                NameScope::Value
                    if frame.parent_kind & kind_flag::BINDING_PARENT != 0 && !after_equals(n) =>
                {
                    self.bindings.push(binding);
                }
                // The emptiness check first: no other language fills the
                // set, and this runs at every bare name of every file.
                NameScope::Value
                    if spec.reads_value(n, frame, memo)
                        && (called.is_empty() || !called.contains(&n.id())) =>
                {
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
        if kind & kind_flag::QUALIFIED_VALUE != 0
            && frame.scope == NameScope::Value
            && frame.field & field_flag::NON_VALUE == 0
            && frame.parent_kind & kind_flag::SKIP_PARENT == 0
        {
            if let Some(name) = (spec.qualified_name)(n, source, spec) {
                self.reads.push(PendingName {
                    pos: start_pos(n),
                    name: Cow::Owned(name),
                    kind: NameRefKind::Function,
                });
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
                    name,
                    kind: NameRefKind::Function,
                });
            }
        }
        false
    }

    /// Attach each read to its smallest enclosing read container, once per
    /// container, unless a name declared in a scope holding it hides it: a
    /// parameter or local of a function node (a comprehension variable inside
    /// it: of that comprehension), else of a nodeless lambda, else of the
    /// value declaration ([`ReadContainers::binding_scope`]). A qualified
    /// function value (`mod.f`) is hidden by a local named like its first
    /// segment, unless `scoped_qualifiers` says no local can be a qualifier.
    fn attach(
        self,
        containers: &ReadContainers<'_>,
        nodes: &mut [RawNode],
        scoped_qualifiers: bool,
    ) {
        if self.reads.is_empty() {
            return;
        }
        let globals: rustc_hash::FxHashSet<(usize, &str)> = self
            .globals
            .iter()
            .zip(enclosing_targets(
                &containers.callables,
                &self.globals,
                |g| g.pos,
            ))
            .filter_map(|(global, target)| Some((target?, global.name)))
            .collect();
        let mut bound: rustc_hash::FxHashMap<&str, Vec<PosRange>> =
            rustc_hash::FxHashMap::default();
        let mut open: Vec<usize> = Vec::new();
        let callables = enclosing_targets(&containers.callables, &self.bindings, |b| b.pos);
        for (i, (binding, target)) in self.bindings.iter().zip(callables).enumerate() {
            match target {
                Some(t) if globals.contains(&(t, binding.name)) => {}
                Some(t) => {
                    let function = span_ends(nodes[t].span);
                    let scope = binding
                        .local_scope
                        .filter(|l| {
                            l.comprehension && function.0 <= l.range.0 && l.range.1 <= function.1
                        })
                        .map_or(function, |l| l.range);
                    bound.entry(binding.name).or_default().push(scope);
                }
                None => open.push(i),
            }
        }
        let runs = containers.value_runs(&open, |i| self.bindings[i].pos);
        for (i, run) in open.into_iter().zip(runs) {
            let binding = &self.bindings[i];
            if let Some(scope) = containers.binding_scope(run, binding, nodes) {
                bound.entry(binding.name).or_default().push(scope);
            }
        }
        let targets = containers.targets(nodes, &self.reads, |r| r.pos);
        let mut seen: rustc_hash::FxHashSet<(usize, NameRefKind, &str)> =
            rustc_hash::FxHashSet::default();
        for (read, target) in self.reads.iter().zip(targets) {
            let Some(node_idx) = target else { continue };
            // `map(mod.f, xs)` inside `def run(mod, xs)` reads the parameter
            // `mod`, not the module the file imports under that name.
            let key = match read.kind {
                NameRefKind::Function if scoped_qualifiers => None,
                NameRefKind::Function => read.name.split_once('.').map(|(head, _)| head),
                _ => Some(read.name.as_ref()),
            };
            let shadowed = key.is_some_and(|key| {
                bound.get(key).is_some_and(|scopes| {
                    scopes
                        .iter()
                        .any(|&(start, end)| start <= read.pos && read.pos <= end)
                })
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
/// `::print`, `Foo.bar` for `Foo::bar`, and the whole receiver path for a
/// qualified one (`a.Outer.Inner.bar` for `a.Outer.Inner::bar`), which Pass 2
/// binds only when the path encloses the function. `None` for a constructor
/// (`Foo::new`), a `super::` reference, a generic receiver, or a computed one.
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
            let mut segments = receiver_path(*receiver, source)?;
            segments.push(member);
            Some(Cow::Owned(segments.join(".")))
        }
        _ => None,
    }
}

/// The name segments of a method reference's receiver, read from the tree:
/// Java `a.b.Foo` (`field_access`, `scoped_type_identifier`), Kotlin
/// `a.Foo` (`user_type`). `None` when a segment is no plain name (`this`, a
/// call) or the receiver carries generic arguments.
fn receiver_path<'s>(receiver: Node<'_>, source: &'s [u8]) -> Option<Vec<&'s str>> {
    let mut segments = Vec::new();
    let mut stack = vec![receiver];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "identifier" | "type_identifier" | "simple_identifier" => {
                segments.push(node.utf8_text(source).ok()?);
            }
            "field_access" | "scoped_type_identifier" | "user_type" => {
                let mut cursor = node.walk();
                let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
                stack.extend(children.into_iter().rev());
            }
            _ => return None,
        }
    }
    Some(segments)
}

/// The function a qualified path in value position names, as the path text
/// the source writes (`parse::migrate`, `crate::a::Entry::new`), so Pass 2
/// resolves it through the module path the way it resolves `parse::migrate()`.
/// `Self::parse` stays as written: Pass 2 reads `Self` as the impl owner.
/// `None` for a longer `Self::` path (an associated type's member) or a path
/// with generic arguments (`Vec::<u8>::new`).
pub fn qualified_reference(path: Node<'_>, source: &[u8], spec: &NameRefSpec) -> Option<String> {
    path.child_by_field_name("name")
        .filter(|n| spec.ident_kinds.contains(&n.kind()))?;
    let text = path.utf8_text(source).ok()?;
    let plain = !text
        .strip_prefix("Self::")
        .is_some_and(|rest| rest.contains("::"))
        && !text.contains(['<', ' ', '\n']);
    plain.then(|| text.to_string())
}

/// Ruby `method(:name)`: the method `name` handed on as a `Method` object.
/// A `self` or absent receiver names `name`; a constant receiver names
/// `Mod.name`, and a scoped one (`A::Mod`) its whole path `A.Mod.name`, so
/// Pass 2 binds it through the module path. Any other receiver is a value of
/// unknown type and records nothing.
fn symbol_method_ref<'s>(call: Node<'_>, source: &'s [u8]) -> Option<Cow<'s, str>> {
    if call.child_by_field_name("method")?.utf8_text(source).ok()? != "method" {
        return None;
    }
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let first = args.named_children(&mut cursor).next()?;
    if first.kind() != "simple_symbol" {
        return None;
    }
    let name = first
        .utf8_text(source)
        .ok()?
        .strip_prefix(':')
        .filter(|name| !name.is_empty())?;
    let Some(receiver) = call.child_by_field_name("receiver") else {
        return Some(Cow::Borrowed(name));
    };
    if receiver.kind() == "self" {
        return Some(Cow::Borrowed(name));
    }
    let mut segments = vec![name];
    let mut node = Some(receiver);
    // `A::B::Mod` nests as `scope_resolution(scope: A::B, name: Mod)`; a
    // leading `::Mod` has no scope.
    while let Some(current) = node {
        let constant = match current.kind() {
            "constant" => {
                node = None;
                current
            }
            "scope_resolution" => {
                node = current.child_by_field_name("scope");
                current
                    .child_by_field_name("name")
                    .filter(|segment| segment.kind() == "constant")?
            }
            _ => return None,
        };
        segments.push(constant.utf8_text(source).ok()?);
    }
    segments.reverse();
    Some(Cow::Owned(segments.join(".")))
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
    // `pair.0` indexes a tuple; recorded by name it would bind to any
    // tuple struct's field "0" in the repo.
    if text.is_empty() || text.bytes().all(|b| b.is_ascii_digit()) {
        None
    } else {
        Some(text.to_string())
    }
}

fn attach_pending_field_reads(
    containers: &ReadContainers<'_>,
    reads: Vec<PendingCall>,
    nodes: &mut [RawNode],
) {
    if reads.is_empty() {
        return;
    }
    let targets = containers.targets(nodes, &reads, |r| r.pos);
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
/// in the `field_reads` of its [`holder`], once per distinct entry.
///
/// Capture is a cheap syntactic filter; Pass 2 keeps only references that
/// resolve to an `EnumVariant`. A `.`-qualified path is kept only when its
/// qualifier starts upper-case (`Day.MONDAY`, not `obj.field`), and a bare C
/// / C++ name only when it does (`RED`, not a local): member accesses and
/// locals outnumber variant references by far, and enum names follow that
/// case in every grammar here.
pub fn extract_variant_refs<'t>(
    root: Node<'t>,
    source: &[u8],
    nodes: &mut [RawNode],
    spec: &VariantRefSpec,
) {
    if nodes.is_empty() {
        return;
    }
    let mut refs: Vec<PendingCall> = Vec::new();
    let mut sites: Vec<Node<'t>> = Vec::new();
    let mut push = |node: Node<'t>, path: &str, pattern: bool| {
        refs.push(PendingCall {
            pos: start_pos(node),
            name: VariantRef::encode(path, pattern),
        });
        sites.push(node);
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
    let targets = holder_targets(
        nodes,
        &refs,
        |r| r.pos,
        |i, run| declared_in(sites[i], source, run),
    );
    let mut seen: rustc_hash::FxHashSet<(usize, String)> = rustc_hash::FxHashSet::default();
    for (entry, target) in refs.into_iter().zip(targets) {
        let Some(node_idx) = target else {
            continue;
        };
        if seen.insert((node_idx, entry.name.clone())) {
            nodes[node_idx].field_reads.push(entry.name);
        }
    }
}

/// C++ nests `ns::Color::Blue` as `ns` + `Color::Blue`; the outer node
/// already holds the whole path.
fn is_path_suffix(node: Node<'_>, parent: Option<Node<'_>>) -> bool {
    parent.is_some_and(|parent| is_same_kind_field(node, parent, "name"))
}

/// Whether `node` is the `field` child of a parent of its own kind: the inner
/// `a.b` of `a.b.c` (`field` names the qualifier: `object`, `operand`,
/// `expression`, `target`), which the outer node records whole.
pub fn heads_same_kind_parent(node: Node<'_>, field: &str) -> bool {
    node.parent()
        .is_some_and(|parent| is_same_kind_field(node, parent, field))
}

pub(crate) fn is_same_kind_field(node: Node<'_>, parent: Node<'_>, field: &str) -> bool {
    parent.kind() == node.kind()
        && parent
            .child_by_field_name(field)
            .is_some_and(|child| child.id() == node.id())
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

/// A declaration whose initializer holds name and field reads when no
/// function does: `const OPT = Some(handler)`, a Java field `Runnable r = Main::run`.
fn is_value_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Const | NodeKind::Variable | NodeKind::Property
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

fn containers_of(nodes: &[RawNode], keep: fn(NodeKind) -> bool) -> Vec<EnclosingContainer> {
    let mut containers: Vec<EnclosingContainer> = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| keep(node.kind))
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

/// Whether `value` belongs to the run headed by `head`: the same span, or a
/// span that starts inside `head` and ends where it ends. Go gives each name
/// of `var C, D = h1, h2` a span from that name to the end of the spec, so
/// the names nest instead of tying; a read there belongs to no single one.
fn same_run(head: &EnclosingContainer, value: &EnclosingContainer) -> bool {
    value.end == head.end && head.start <= value.start
}

/// A range that scopes the names declared in it.
#[derive(Clone, Copy)]
struct LocalScope {
    range: PosRange,
    /// A comprehension's range. A function's own parameter list may open a
    /// range that holds only its signature (Dart `function_signature`), so
    /// only a comprehension narrows a binding that a function node holds.
    comprehension: bool,
}

impl LocalScope {
    fn lambda(range: PosRange) -> Self {
        Self {
            range,
            comprehension: false,
        }
    }

    fn comprehension(range: PosRange) -> Self {
        Self {
            range,
            comprehension: true,
        }
    }
}

/// One declarator of a statement that declares several:
/// `b = h2` in `const a = h1, b = h2`.
struct Declarator<'s> {
    range: PosRange,
    /// The name it declares, when that is a bare name (`b`, not `{ cb }`).
    name: Option<&'s str>,
    /// The statement that declares it. Only a statement around the code
    /// splits that code's holder.
    statement: PosRange,
}

/// The member of `run`, the value declarations that share the statement
/// spanning `(start, end)`, that holds code at `pos`: the one whose own
/// declarator holds `pos`. When `pos` sits in a sibling declarator that
/// declares no node (`{ cb }` in `const unused = 0, { cb } = …`), or no
/// declarator tells the run apart (Go `var C, D = h1, h2`), none.
fn pick_declared<'d, 's: 'd, T>(
    run: &[T],
    node_of: impl Fn(&T) -> usize,
    (start, end): PosRange,
    declarators: impl IntoIterator<Item = &'d Declarator<'s>>,
    pos: Pos,
    nodes: &[RawNode],
) -> Option<usize> {
    // Only the declarators that hold `pos` look up a member: a statement of
    // M values is then O(M) per call, not O(M²).
    let mut sibling_declares_only = false;
    for declarator in declarators.into_iter().filter(|d| {
        start <= d.range.0 && d.range.1 <= end && d.statement.0 <= pos && pos <= d.statement.1
    }) {
        let Some(name) = declarator.name else {
            continue;
        };
        if declarator.range.0 <= pos && pos <= declarator.range.1 {
            if let Some(node_idx) = run.iter().map(&node_of).find(|&i| nodes[i].name == name) {
                return Some(node_idx);
            }
        } else if let [only] = run {
            sibling_declares_only |= nodes[node_of(only)].name == name;
        }
    }
    match run {
        [only] if !sibling_declares_only => Some(node_of(only)),
        _ => None,
    }
}

/// Where a name or field read lands: the smallest enclosing function, else
/// the smallest enclosing value declaration. Functions rank first, so
/// `const f = () => g` keeps `g` on the arrow function and not on `f`.
struct ReadContainers<'s> {
    callables: Vec<EnclosingContainer>,
    /// Every value declaration, in [`containers_of`] order: declarations
    /// that share one span sit side by side.
    values: Vec<EnclosingContainer>,
    /// One entry per run of `values` (see [`same_run`]); its `node_idx` is
    /// the index in `values` of the run's first declaration.
    value_spans: Vec<EnclosingContainer>,
    /// The declarators of every statement that declares several, by start.
    declarators: Vec<Declarator<'s>>,
}

impl<'s> ReadContainers<'s> {
    fn new(nodes: &[RawNode]) -> Self {
        Self::with_callables(nodes, containers_of(nodes, is_container_kind))
    }

    fn with_callables(nodes: &[RawNode], callables: Vec<EnclosingContainer>) -> Self {
        let values = containers_of(nodes, is_value_kind);
        let mut value_spans: Vec<EnclosingContainer> = Vec::with_capacity(values.len());
        for (first, value) in values.iter().enumerate() {
            if value_spans.last().is_none_or(|span| !same_run(span, value)) {
                value_spans.push(EnclosingContainer {
                    node_idx: first,
                    ..*value
                });
            }
        }
        Self {
            callables,
            values,
            value_spans,
            declarators: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.callables.is_empty() && self.values.is_empty()
    }

    fn set_declarators(&mut self, mut declarators: Vec<Declarator<'s>>) {
        declarators.sort_unstable_by_key(|d| d.range.0);
        self.declarators = declarators;
    }

    fn targets<T>(
        &self,
        nodes: &[RawNode],
        pending: &[T],
        pos_of: impl Fn(&T) -> Pos,
    ) -> Vec<Option<usize>> {
        let mut targets = enclosing_targets(&self.callables, pending, &pos_of);
        if self.value_spans.is_empty() {
            return targets;
        }
        let open: Vec<usize> = (0..pending.len())
            .filter(|&i| targets[i].is_none())
            .collect();
        let runs = self.value_runs(&open, |i| pos_of(&pending[i]));
        for (i, run) in open.into_iter().zip(runs) {
            targets[i] = run.and_then(|first| self.pick(first, pos_of(&pending[i]), nodes));
        }
        targets
    }

    /// For each index in `open`, the `values` index of the first declaration
    /// of the smallest value span holding its position.
    fn value_runs(&self, open: &[usize], pos_of: impl Fn(usize) -> Pos) -> Vec<Option<usize>> {
        enclosing_targets(&self.value_spans, open, |&i| pos_of(i))
    }

    /// The declarations that share the span of `values[first]`.
    fn run(&self, first: usize) -> &[EnclosingContainer] {
        let head = &self.values[first];
        let len = self.values[first..]
            .iter()
            .take_while(|v| same_run(head, v))
            .count();
        &self.values[first..first + len]
    }

    /// The declaration of the run at `first` that holds a read at `pos`
    /// ([`pick_declared`]).
    fn pick(&self, first: usize, pos: Pos, nodes: &[RawNode]) -> Option<usize> {
        let run = self.run(first);
        let (start, end) = (run[0].start, run[0].end);
        let from = self.declarators.partition_point(|d| d.range.0 < start);
        let declarators = self.declarators[from..]
            .iter()
            .take_while(|d| d.range.0 <= end);
        pick_declared(run, |v| v.node_idx, (start, end), declarators, pos, nodes)
    }

    /// The range where `binding`, which no function node holds, hides its
    /// name. `first` is the run of the smallest value span holding it. A
    /// declaration's own name hides nothing: a method of its literal still
    /// reads it. A parameter of a nodeless lambda or a comprehension variable
    /// hides the name inside that lambda or comprehension only. Any other name
    /// (a local of a lambda without parameters, a Python walrus) hides it
    /// across the whole declaration.
    fn binding_scope(
        &self,
        first: Option<usize>,
        binding: &PendingBinding<'_>,
        nodes: &[RawNode],
    ) -> Option<PosRange> {
        let run = first.map(|first| self.run(first));
        let declared = run.filter(|run| run.iter().any(|v| nodes[v.node_idx].name == binding.name));
        match (declared, binding.local_scope.map(|l| l.range)) {
            // A lambda inside the declaration: `TABLE = {"id": lambda TABLE: TABLE}`.
            (Some(run), Some(lambda)) if run[0].start < lambda.0 || lambda.1 < run[0].end => {
                Some(lambda)
            }
            (Some(_), _) => None,
            (None, Some(lambda)) => Some(lambda),
            (None, None) => run.map(|run| (run[0].start, run[0].end)),
        }
    }
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

/// For each pending entry, the node index of its [`holder`], by one sweep per
/// tier rather than one scan of `nodes` per entry. `declared(i, run)` lists
/// the declarators around entry `i` ([`declared_in`]).
fn holder_targets<'s, T>(
    nodes: &[RawNode],
    pending: &[T],
    pos_of: impl Fn(&T) -> Pos,
    declared: impl Fn(usize, PosRange) -> Vec<Declarator<'s>>,
) -> Vec<Option<usize>> {
    let callables = containers_of(nodes, is_container_kind);
    let mut targets = enclosing_targets(&callables, pending, &pos_of);
    let unheld = |targets: &[Option<usize>]| -> Vec<usize> {
        (0..pending.len())
            .filter(|&i| targets[i].is_none())
            .collect()
    };
    let open = unheld(&targets);
    if open.is_empty() {
        return targets;
    }
    let holders = ReadContainers::with_callables(nodes, callables);
    let runs = holders.value_runs(&open, |i| pos_of(&pending[i]));
    for (&i, run) in open.iter().zip(runs) {
        targets[i] = run.and_then(|first| {
            let run = holders.run(first);
            let span = (run[0].start, run[0].end);
            let declarators = declared(i, span);
            pick_declared(
                run,
                |v| v.node_idx,
                span,
                &declarators,
                pos_of(&pending[i]),
                nodes,
            )
        });
    }
    let open = unheld(&targets);
    let types = containers_of(nodes, ecp_core::analyzer::heritage::holds_heritage);
    for (i, target) in open
        .iter()
        .zip(enclosing_targets(&types, &open, |&i| pos_of(&pending[i])))
    {
        targets[*i] = target;
    }
    targets
}

fn attach_pending_calls<'s>(
    calls: Vec<PendingCall>,
    nodes: &mut [RawNode],
    declared: impl Fn(usize, PosRange) -> Vec<Declarator<'s>>,
) {
    let targets = holder_targets(nodes, &calls, |c| c.pos, declared);
    for (call, target) in calls.into_iter().zip(targets) {
        if let Some(node_idx) = target {
            nodes[node_idx].calls.push(call.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_declarators(_: usize, _: PosRange) -> Vec<Declarator<'static>> {
        Vec::new()
    }

    fn attach_at(pos: Pos, callee: String, nodes: &mut [RawNode]) -> Option<(usize, u32)> {
        let i = holder_at_pos(pos, nodes, |_| Vec::new())?;
        Some(push_call(i, callee, nodes))
    }

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
        attach_pending_calls(
            vec![PendingCall {
                pos: (6, 4),
                name: "callee".to_string(),
            }],
            &mut nodes,
            no_declarators,
        );

        assert!(nodes[0].calls.is_empty());
        assert_eq!(nodes[1].calls, ["callee"]);
    }

    #[test]
    fn attach_pending_calls_preserves_original_call_order() {
        let mut nodes = vec![raw_node("f", NodeKind::Function, (1, 0, 20, 0))];
        attach_pending_calls(
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
            no_declarators,
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
        let call = |col, name: &str| PendingCall {
            pos: (0, col),
            name: name.to_string(),
        };

        attach_pending_calls(
            vec![
                call(5, "x"),
                call(22, "y"),
                call(30, "z"),
                call(20, "at_end"),
                call(21, "at_start"),
                call(50, "outside"),
            ],
            &mut nodes,
            no_declarators,
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
        attach_pending_calls(
            vec![PendingCall {
                pos: (1, 0),
                name: "c".to_string(),
            }],
            &mut nodes,
            no_declarators,
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
        assert_eq!(attach_at((0, 25), "y".into(), &mut nodes), Some((1, 0)));
        assert_eq!(attach_at((0, 5), "x".into(), &mut nodes), Some((0, 0)));
        assert_eq!(attach_at((1, 0), "m".into(), &mut nodes), None);
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
