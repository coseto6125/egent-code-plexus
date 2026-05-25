use ecp_core::analyzer::types::RawNode;
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

pub fn attach_to_enclosing(line: u32, callee: String, nodes: &mut [RawNode]) {
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
    if let Some(i) = best {
        nodes[i].calls.push(callee);
    }
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
    let mut order: Vec<usize> = (0..reads.len()).collect();
    order.sort_unstable_by_key(|&idx| reads[idx].line);

    let mut active: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::new();
    let mut next_container = 0usize;
    let mut targets: Vec<Option<usize>> = vec![None; reads.len()];

    for &idx in &order {
        let line = reads[idx].line;
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

    for (read, target) in reads.into_iter().zip(targets) {
        if let Some(node_idx) = target {
            nodes[node_idx].field_reads.push(read.name);
        }
    }
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
}
