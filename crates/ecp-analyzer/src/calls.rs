use ecp_core::analyzer::types::{CallSite, RawNode};
use ecp_core::graph::NodeKind;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use tree_sitter::Node;

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
                let line = safe_row(n.start_position().row);
                calls.push(PendingCall { line, name });
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

/// Returns the caller's node index and the call's index in its `calls`.
pub fn attach_to_enclosing(
    line: u32,
    callee: String,
    nodes: &mut [RawNode],
) -> Option<(usize, u32)> {
    let mut best: Option<usize> = None;
    let mut best_span: u32 = u32::MAX;
    for (i, n) in nodes.iter().enumerate() {
        if !matches!(
            n.kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Constructor
        ) {
            continue;
        }
        if n.span.0 <= line && n.span.2 >= line {
            let width = n.span.2 - n.span.0;
            if width < best_span {
                best_span = width;
                best = Some(i);
            }
        }
    }
    let i = best?;
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
pub fn extract_field_reads(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    field_kinds: &[&str],
) {
    let containers = enclosing_containers(nodes);
    if containers.is_empty() {
        return;
    }
    let mut reads: Vec<PendingCall> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if field_kinds.contains(&n.kind()) && !is_call_callee(n) {
            if let Some(name) = field_name_from(n, source) {
                let line = safe_row(n.start_position().row);
                reads.push(PendingCall { line, name });
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    attach_pending_field_reads(&containers, reads, nodes);
}

/// True when `access` is the callee of an enclosing call expression — i.e. its
/// parent is a call/invocation whose `function` field is `access`. Such nodes
/// are method calls handled by `extract_calls`, not data-field reads.
fn is_call_callee(access: Node<'_>) -> bool {
    let Some(parent) = access.parent() else {
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
    let targets = enclosing_targets(containers, &reads);
    for (read, target) in reads.into_iter().zip(targets) {
        if let Some(node_idx) = target {
            nodes[node_idx].field_reads.push(read.name);
        }
    }
}

/// For each pending entry, the node index of the smallest container whose
/// line range holds its line.
fn enclosing_targets(
    containers: &[EnclosingContainer],
    pending: &[PendingCall],
) -> Vec<Option<usize>> {
    let mut order: Vec<usize> = (0..pending.len()).collect();
    order.sort_unstable_by_key(|&idx| pending[idx].line);

    let mut active: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::new();
    let mut next_container = 0usize;
    let mut targets: Vec<Option<usize>> = vec![None; pending.len()];

    for &idx in &order {
        let line = pending[idx].line;
        while next_container < containers.len() && containers[next_container].start <= line {
            active.push(Reverse((containers[next_container].width, next_container)));
            next_container += 1;
        }
        while let Some(Reverse((_, container_idx))) = active.peek().copied() {
            if containers[container_idx].end >= line {
                targets[idx] = Some(containers[container_idx].node_idx);
                break;
            }
            active.pop();
        }
    }
    targets
}

/// One enum-variant reference a parser recorded in [`RawNode::field_reads`]:
/// `E::A { .. }`, `E::B(..)`, `Day.MONDAY`, a `case Color::Red:` label, or a
/// bare enumerator (`RED`) where the language names variants unqualified.
///
/// The entry rides in `field_reads` behind a U+0001 marker, like the
/// [`CallSite`] markers in `calls`: no field name holds a control
/// character, so the `ReadsField` tier never sees one. Pass 2 resolves the
/// path's qualifier as a type and the last segment as one of that type's
/// variants (`References` edge); a bare name resolves only to a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VariantRef<'a> {
    /// `Q::V`, `pkg.Q.V`, `\App\Q::V`, or a bare `V`, whitespace removed.
    pub path: &'a str,
    /// The reference sits in a pattern or case label, so it tests for the
    /// variant rather than producing or reading it.
    pub pattern: bool,
}

impl<'a> VariantRef<'a> {
    const USE_PREFIX: &'static str = "\u{1}variant ";
    const PATTERN_PREFIX: &'static str = "\u{1}variant-pattern ";

    pub fn encode(path: &str, pattern: bool) -> String {
        let prefix = if pattern {
            Self::PATTERN_PREFIX
        } else {
            Self::USE_PREFIX
        };
        [prefix, path].concat()
    }

    /// `None` for an ordinary field-read entry.
    pub fn parse(raw: &'a str) -> Option<Self> {
        if let Some(path) = raw.strip_prefix(Self::PATTERN_PREFIX) {
            Some(Self {
                path,
                pattern: true,
            })
        } else {
            raw.strip_prefix(Self::USE_PREFIX).map(|path| Self {
                path,
                pattern: false,
            })
        }
    }

    /// `(qualifier path, variant)` split at the last `::` / `.` / `\`;
    /// `None` for a bare name.
    pub fn split(self) -> Option<(&'a str, &'a str)> {
        let path = self.path;
        let idx = [
            path.rfind("::").map(|i| (i, 2)),
            path.rfind('.').map(|i| (i, 1)),
        ]
        .into_iter()
        .flatten()
        .max_by_key(|&(i, _)| i)?;
        let (qualifier, variant) = (&path[..idx.0], &path[idx.0 + idx.1..]);
        (!qualifier.is_empty() && !variant.is_empty()).then_some((qualifier, variant))
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
            line: safe_row(node.start_position().row),
            name: VariantRef::encode(path, pattern),
        });
    };
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        let kind = n.kind();
        if spec.path_kinds.contains(&kind) && !is_path_suffix(n) {
            if let Some(path) = n.utf8_text(source).ok().and_then(variant_path) {
                push(n, &path, in_label(n, spec.label_parents));
            }
        } else if kind == "identifier" {
            if let Some(parent) = n.parent() {
                let label = spec.bare_label_parents.contains(&parent.kind());
                if label || (spec.bare_expressions && is_bare_value(n, parent)) {
                    if let Some(name) = n.utf8_text(source).ok().filter(|t| starts_upper(t)) {
                        push(n, name, label || in_label(n, spec.label_parents));
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
            stack.push(child);
        }
    }
    if refs.is_empty() {
        return;
    }
    let targets = enclosing_targets(&containers, &refs);
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
fn is_path_suffix(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == node.kind()
            && parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id())
    })
}

/// Whether `node` is a case label or sits in a pattern.
fn in_label(node: Node<'_>, label_parents: &[&str]) -> bool {
    if node.kind().ends_with("pattern") {
        return true;
    }
    let Some(parent) = node.parent() else {
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

#[derive(Clone, Copy)]
struct EnclosingContainer {
    start: u32,
    end: u32,
    width: u32,
    node_idx: usize,
}

struct PendingCall {
    line: u32,
    name: String,
}

fn enclosing_containers(nodes: &[RawNode]) -> Vec<EnclosingContainer> {
    let mut containers: Vec<EnclosingContainer> = nodes
        .iter()
        .enumerate()
        .filter_map(|(node_idx, node)| {
            if !matches!(
                node.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Constructor
            ) {
                return None;
            }
            let start = node.span.0;
            let end = node.span.2;
            Some(EnclosingContainer {
                start,
                end,
                width: end.saturating_sub(start),
                node_idx,
            })
        })
        .collect();
    containers.sort_unstable_by_key(|c| (c.start, c.width, c.node_idx));
    containers
}

fn attach_pending_calls(
    containers: &[EnclosingContainer],
    calls: Vec<PendingCall>,
    nodes: &mut [RawNode],
) {
    if containers.is_empty() || calls.is_empty() {
        return;
    }

    let mut call_order: Vec<usize> = (0..calls.len()).collect();
    call_order.sort_unstable_by_key(|&idx| calls[idx].line);

    let mut active: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::new();
    let mut next_container = 0usize;
    let mut targets: Vec<Option<usize>> = vec![None; calls.len()];

    for &call_idx in &call_order {
        let line = calls[call_idx].line;
        while next_container < containers.len() && containers[next_container].start <= line {
            let container = containers[next_container];
            active.push(Reverse((container.width, next_container)));
            next_container += 1;
        }
        while let Some(Reverse((_, container_idx))) = active.peek().copied() {
            if containers[container_idx].end >= line {
                targets[call_idx] = Some(containers[container_idx].node_idx);
                break;
            }
            active.pop();
        }
    }

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
                line: 6,
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
                    line: 12,
                    name: "later".to_string(),
                },
                PendingCall {
                    line: 3,
                    name: "earlier".to_string(),
                },
            ],
            &mut nodes,
        );

        assert_eq!(nodes[0].calls, ["later", "earlier"]);
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
