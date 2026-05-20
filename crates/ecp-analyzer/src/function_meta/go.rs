//! Go FunctionMeta extraction.
//!
//! Go-specific rules:
//! - `is_async`: always false (goroutines are call-site `go` statements, not function markers).
//! - `is_static`: always false (no static methods; package-level fns are shared by design).
//! - `is_abstract`: always false (interfaces list method signatures, no abstract hierarchy).
//! - `is_generator`: always false (Go uses channels, not yield-based generators).
//! - `is_extern`: function declaration without body (`function_declaration` with no `block` child).
//! - `is_test`: file path ends `_test.go` AND name starts with `Test`/`Benchmark`/`Example`/`Fuzz`.
//! - visibility: uppercase first letter → public (0); lowercase → package-private mapped to 2.
//! - params: each `parameter_declaration` may declare `name1, name2 Type`; we emit per-name.
//!   Variadic `...T` is kept as written in the type field.
//! - return_type: single `(type_identifier)` → as written; multi-return
//!   `(parameter_list ...)` → captured as the full `(T1, T2)` text literal.
//! - decorators: `//go:build` / `//go:noinline` / `//go:linkname` compiler directives
//!   immediately preceding the function are captured (LLM-relevant for optimization queries).
//!   Captured by scanning raw source bytes above the function start line.

use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta, NodeKind};
use tree_sitter::Node;

type FnSpan<'a> = ((u32, u32, u32, u32), &'a RawNode);

pub fn extract(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    file_category: FileCategory,
) -> Vec<RawFunctionMeta> {
    let fn_spans: Vec<_> = nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Constructor
            )
        })
        .map(|n| (n.span, n))
        .collect();

    if fn_spans.is_empty() {
        return vec![];
    }

    let mut out: Vec<RawFunctionMeta> = Vec::with_capacity(fn_spans.len());
    collect_fn_nodes(root, source, &fn_spans, file_category, &mut out);
    out
}

fn ts_span(n: &Node<'_>) -> (u32, u32, u32, u32) {
    let s = n.start_position();
    let e = n.end_position();
    (s.row as u32, s.column as u32, e.row as u32, e.column as u32)
}

fn node_text<'a>(n: &Node<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(&source[n.start_byte()..n.end_byte()]).unwrap_or("")
}

fn collect_fn_nodes<'a>(
    node: Node<'a>,
    source: &[u8],
    fn_spans: &[FnSpan<'a>],
    file_category: FileCategory,
    out: &mut Vec<RawFunctionMeta>,
) {
    let k = node.kind();
    if k == "function_declaration" || k == "method_declaration" {
        let span = ts_span(&node);
        if let Some((_, raw)) = fn_spans.iter().find(|(s, _)| *s == span) {
            if let Some(meta) = extract_one(&node, source, raw, file_category) {
                out.push(meta);
            }
        }
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            collect_fn_nodes(cursor.node(), source, fn_spans, file_category, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

fn extract_one(
    fn_node: &Node<'_>,
    source: &[u8],
    raw: &RawNode,
    file_category: FileCategory,
) -> Option<RawFunctionMeta> {
    let mut flags: u16 = 0;

    // is_extern: function_declaration with no `block` child (cgo / asm declaration).
    let has_body = {
        let mut cursor = fn_node.walk();
        let found = fn_node.children(&mut cursor).any(|c| c.kind() == "block");
        found
    };
    if !has_body && fn_node.kind() == "function_declaration" {
        flags |= FunctionMeta::FLAG_EXTERN;
    }

    // is_test: FileCategory::Test (file path ends `_test.go`) AND name convention.
    if file_category == FileCategory::Test
        && (raw.name.starts_with("Test")
            || raw.name.starts_with("Benchmark")
            || raw.name.starts_with("Example")
            || raw.name.starts_with("Fuzz"))
    {
        flags |= FunctionMeta::FLAG_TEST;
    }

    // visibility: exported = uppercase first letter → 0 (public); else → 2 (package-private).
    let vis_code: u16 = if raw
        .name
        .chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
    {
        0
    } else {
        2
    };
    flags |= vis_code << 6;

    // Parameters from `parameters` field (function_declaration) or after `receiver` (method_declaration).
    let params = extract_params(fn_node, source);

    // Return type: `result` field — may be a type_identifier, pointer_type, qualified_type,
    // or parameter_list (multi-return). Capture the full text.
    let return_type = fn_node
        .child_by_field_name("result")
        .map(|rt| node_text(&rt, source).to_string())
        .unwrap_or_default();

    // Decorators: `//go:noinline`, `//go:linkname`, `//go:build`, etc. — compiler directives
    // on the lines immediately before this function. We scan source lines.
    let decorators = collect_go_directives(source, fn_node.start_position().row);

    Some(RawFunctionMeta {
        span: ts_span(fn_node),
        flags,
        params,
        return_type,
        decorators,
    })
}

/// Collect `//go:...` compiler directives from lines immediately preceding `fn_start_row`.
/// Walks backwards through source lines, stopping at the first non-directive, non-blank line.
fn collect_go_directives(source: &[u8], fn_start_row: usize) -> Vec<String> {
    if fn_start_row == 0 {
        return vec![];
    }

    // Split source into lines once.
    let text = std::str::from_utf8(source).unwrap_or("");
    let lines: Vec<&str> = text.lines().collect();

    let mut directives: Vec<String> = Vec::new();
    let mut row = fn_start_row;
    loop {
        if row == 0 {
            break;
        }
        row -= 1;
        let line = lines.get(row).copied().unwrap_or("").trim();
        if line.is_empty() {
            // blank line → stop
            break;
        }
        if let Some(rest) = line.strip_prefix("//go:") {
            // e.g. "//go:noinline" → "noinline"
            // Strip trailing spaces / arguments after first space.
            let directive = rest.split_whitespace().next().unwrap_or(rest).to_string();
            directives.push(format!("go:{directive}"));
        } else if line.starts_with("//") {
            // Regular comment — keep scanning (build tags like `//go:build linux` may
            // be preceded by other comments).
            continue;
        } else {
            break;
        }
    }
    directives.reverse();
    directives
}

/// Extract flat `[name1, type1, name2, type2, ...]` from Go function parameters.
/// Go allows `name1, name2 Type` in one `parameter_declaration`.
fn extract_params(fn_node: &Node<'_>, source: &[u8]) -> Vec<String> {
    // For method_declaration the parameters field is named "parameters"; skip "receiver".
    let Some(params_node) = fn_node.child_by_field_name("parameters") else {
        return vec![];
    };
    let mut result = Vec::new();
    let mut cursor = params_node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.kind() == "parameter_declaration"
                || child.kind() == "variadic_parameter_declaration"
            {
                let type_text = child
                    .child_by_field_name("type")
                    .map(|n| node_text(&n, source).to_string())
                    .unwrap_or_default();

                // Collect all `name:` children — Go allows `a, b int` in one decl.
                // tree-sitter-go grammar: parameter_declaration children with field "name"
                // are `identifier` nodes before the `type` node. We use child_count and
                // field_name via cursor to enumerate them.
                let mut names: Vec<String> = Vec::new();
                {
                    let mut pc = child.walk();
                    if pc.goto_first_child() {
                        loop {
                            // field_name() on the cursor returns the field of the current child.
                            if pc.field_name() == Some("name") {
                                names.push(node_text(&pc.node(), source).to_string());
                            }
                            if !pc.goto_next_sibling() {
                                break;
                            }
                        }
                    }
                }

                if names.is_empty() {
                    // Unnamed parameter — emit type only with empty name.
                    if !type_text.is_empty() {
                        result.push(String::new());
                        result.push(type_text);
                    }
                } else {
                    for name in names {
                        result.push(name);
                        result.push(type_text.clone());
                    }
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    result
}
