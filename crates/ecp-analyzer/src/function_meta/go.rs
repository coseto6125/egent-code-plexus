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

use super::{extract_with, go_declared_type_params, node_text, scoped_type_params, ts_span};
use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta};
use tree_sitter::Node;

const GO_FN_KINDS: &[&str] = &["function_declaration", "method_declaration"];

pub fn extract(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    file_category: FileCategory,
) -> Vec<RawFunctionMeta> {
    // Hoist `text.lines().collect()` once per file (previously per function:
    // 20 fns × 500 lines × 1000 Go files = 10M iter + 20k Vec allocs in the
    // cold-ingest hot path). Closure captures the slice; `collect_go_directives`
    // becomes O(directive_count) instead of O(file_lines).
    let text = std::str::from_utf8(source).unwrap_or("");
    let lines: Vec<&str> = text.lines().collect();
    let lines_ref = &lines;
    extract_with(
        root,
        source,
        nodes,
        file_category,
        GO_FN_KINDS,
        |fn_node, src, raw, fc| extract_one(fn_node, src, lines_ref, raw, fc),
    )
}

fn extract_one(
    fn_node: &Node<'_>,
    source: &[u8],
    lines: &[&str],
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
    let decorators = collect_go_directives(lines, fn_node.start_position().row);

    let mut type_params = scoped_type_params(
        fn_node,
        source,
        &["type_parameter_list"],
        go_declared_type_params,
    );
    type_params.extend(receiver_type_params(fn_node, source));

    Some(RawFunctionMeta {
        span: ts_span(fn_node),
        flags,
        params,
        return_type,
        decorators,
        type_params,
    })
}

/// Collect `//go:...` compiler directives from lines immediately preceding `fn_start_row`.
/// Walks backwards through source lines, stopping at the first non-directive, non-blank line.
///
/// `lines` is hoisted by the caller (`extract`) so a Go file with N functions
/// pays O(file_lines) once instead of O(N × file_lines).
fn collect_go_directives(lines: &[&str], fn_start_row: usize) -> Vec<String> {
    if fn_start_row == 0 {
        return vec![];
    }

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

/// The variables a method receiver declares: `func (b *Box[K, V]) Put(..)`
/// names K and V between the brackets of the receiver type.
fn receiver_type_params(fn_node: &Node<'_>, source: &[u8]) -> Vec<String> {
    let Some(receiver) = fn_node.child_by_field_name("receiver") else {
        return Vec::new();
    };
    let text = node_text(&receiver, source);
    let (Some(open), Some(close)) = (text.find('['), text.rfind(']')) else {
        return Vec::new();
    };
    text.get(open + 1..close)
        .unwrap_or_default()
        .split(',')
        .filter_map(|entry| entry.split_whitespace().next())
        .filter(|name| *name != "_")
        .map(str::to_string)
        .collect()
}
