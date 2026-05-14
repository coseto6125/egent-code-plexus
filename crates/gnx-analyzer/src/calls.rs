use gnx_core::analyzer::types::RawNode;
use gnx_core::graph::NodeKind;
use tree_sitter::Node;

/// Walk the AST and attach `callee` names to the smallest enclosing
/// function/method/constructor `RawNode` by span containment. Reused across
/// languages — the `call_kinds` set lists this language's AST node kinds that
/// represent a function-call expression (e.g. "call_expression" in JS/TS,
/// "method_invocation" in Java, "function_call_expression" in PHP).
pub fn extract_calls(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    call_kinds: &[&str],
) {
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if call_kinds.contains(&n.kind()) {
            let callee_name = callee_name_from(n, source);
            if let Some(name) = callee_name {
                let line = n.start_position().row as u32;
                attach_to_enclosing(line, name, nodes);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

pub fn callee_name_from(call_node: Node<'_>, source: &[u8]) -> Option<String> {
    // Try "function" field first (JS/TS/PHP), fall back to "name"/"called function" semantics.
    let target = call_node
        .child_by_field_name("function")
        .or_else(|| call_node.child_by_field_name("name"))
        .or_else(|| call_node.child(0))?;
    match target.kind() {
        "identifier" | "type_identifier" | "property_identifier" | "simple_identifier" => target
            .utf8_text(source)
            .ok()
            .map(|s| s.to_string()),
        "member_expression" | "field_access" | "navigation_expression" => {
            // Prefer the full text of the member expression (e.g., "z.record") to preserve namespace context.
            // If extracting the full text fails, fall back to just the property name.
            target.utf8_text(source).ok().map(|s| s.to_string()).or_else(|| {
                target
                    .child_by_field_name("property")
                    .or_else(|| target.child_by_field_name("field"))
                    .or_else(|| target.child_by_field_name("name"))
                    .and_then(|p| p.utf8_text(source).ok().map(|s| s.to_string()))
            })
        }
        "scoped_identifier" | "qualified_name" | "scoped_call_expression" => target
            .child_by_field_name("name")
            .or_else(|| target.named_child(target.named_child_count().saturating_sub(1)))
            .and_then(|p| p.utf8_text(source).ok().map(|s| s.to_string())),
        _ => target.utf8_text(source).ok().and_then(|s| {
            // last resort: take last segment after `.` or `::`
            let trimmed = s.trim();
            let after_dot = trimmed.rsplit_once('.').map(|(_, t)| t).unwrap_or(trimmed);
            let after_colon = after_dot.rsplit_once("::").map(|(_, t)| t).unwrap_or(after_dot);
            let id = after_colon
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>();
            if id.is_empty() { None } else { Some(id) }
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
