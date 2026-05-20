//! Rust FunctionMeta extraction.
//!
//! Walks the tree-sitter AST to detect `async fn`, visibility modifiers,
//! `#[test]`/`#[tokio::test]` attributes, `extern "ABI"` declarations, and
//! parameter/return_type signatures.

use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta, NodeKind};
use tree_sitter::Node;

/// Span-keyed index entry: `(span, &RawNode)`.
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
    // function_item (concrete fn) or function_signature_item (trait abstract fn).
    if k == "function_item" || k == "function_signature_item" {
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

    // Collect attribute items (`#[...]`) that precede this function in the parent block.
    // In Rust's grammar, attributes are sibling nodes (attribute_item / inner_attribute_item),
    // NOT children of function_item. The query system doesn't capture them generically,
    // so we walk the parent's children to find contiguous attributes immediately before
    // this fn node. Merge with any decorators already captured by the query system.
    let ast_decorators: Vec<String> = {
        let mut decs: Vec<String> = Vec::new();
        if let Some(parent) = fn_node.parent() {
            // Collect all siblings in order; capture attributes immediately preceding fn.
            let mut pending_attrs: Vec<String> = Vec::new();
            let mut c = parent.walk();
            if c.goto_first_child() {
                loop {
                    let sib = c.node();
                    if sib.kind() == "attribute_item" || sib.kind() == "inner_attribute_item" {
                        let txt = node_text(&sib, source).to_string();
                        if !txt.is_empty() {
                            pending_attrs.push(txt);
                        }
                    } else if sib.id() == fn_node.id() {
                        // Found our function — all accumulated attrs belong to it.
                        decs.append(&mut pending_attrs);
                        break;
                    } else {
                        // Non-attribute, non-fn sibling — reset accumulator.
                        pending_attrs.clear();
                    }
                    if !c.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
        // Merge with query-captured decorators (should be none for Rust normally).
        for d in &raw.decorators {
            if !decs.contains(d) {
                decs.push(d.clone());
            }
        }
        decs
    };
    let decorators = ast_decorators.clone();

    // Detect flags from children of the function_item node.
    // In tree-sitter-rust, `async`/`unsafe`/`const` live inside a
    // `function_modifiers` named-child node (they are NOT direct `"async"` children).
    let mut has_async = false;
    let mut has_self = false;
    let mut vis_code: u16 = 2; // default: private (no vis modifier)
    let mut is_extern_fn = false;

    {
        let mut c = fn_node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                match child.kind() {
                    "function_modifiers" => {
                        // Modifiers text may be "async", "unsafe async", etc.
                        let txt = node_text(&child, source);
                        if txt.contains("async") {
                            has_async = true;
                        }
                        // extern modifier (e.g. `unsafe extern "C" fn`) is represented
                        // as an `extern_modifier` child inside function_modifiers.
                        let mut mc = child.walk();
                        if mc.goto_first_child() {
                            loop {
                                if mc.node().kind() == "extern_modifier" {
                                    is_extern_fn = true;
                                }
                                if !mc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                    "visibility_modifier" => {
                        let txt = node_text(&child, source);
                        vis_code = rust_visibility(txt);
                    }
                    "extern_modifier" => {
                        // Can also appear as direct child in some grammar versions.
                        is_extern_fn = true;
                    }
                    _ => {}
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    if has_async {
        flags |= FunctionMeta::FLAG_ASYNC;
    }
    if is_extern_fn {
        flags |= FunctionMeta::FLAG_EXTERN;
    }

    // Abstract: function_signature_item (no body in a trait block).
    if fn_node.kind() == "function_signature_item" {
        flags |= FunctionMeta::FLAG_ABSTRACT;
    }

    // is_static: no `self`/`&self`/`&mut self` in params.
    // Only relevant for methods; free functions are always "static" but flag only when Method/Constructor.
    let params_node = fn_node.child_by_field_name("parameters");
    if let Some(params) = params_node {
        has_self = first_param_is_self(&params, source);
    }
    // Static = associated fn with no self receiver (in impl/trait context).
    // For top-level free functions this flag isn't semantically meaningful,
    // but we set it for consistency: a free fn has no receiver, like a static method.
    if matches!(raw.kind, NodeKind::Method | NodeKind::Constructor) && !has_self {
        flags |= FunctionMeta::FLAG_STATIC;
    }

    // is_test: #[test] / #[tokio::test] / #[async_std::test] etc.
    let is_test = file_category == FileCategory::Test
        || ast_decorators.iter().any(|d| {
            let stripped = d.trim_start_matches('#').trim();
            // Match [test], [tokio::test], [async_std::test], [actix_rt::test], etc.
            stripped == "[test]" || stripped.ends_with("::test]") || stripped == "[cfg(test)]"
        });
    if is_test {
        flags |= FunctionMeta::FLAG_TEST;
    }

    flags |= vis_code << 6;

    // Parameters.
    let params = extract_params(fn_node, source);

    // Return type.
    let return_type = fn_node
        .child_by_field_name("return_type")
        .map(|rt| node_text(&rt, source).to_string())
        .unwrap_or_default();

    Some(RawFunctionMeta {
        span: ts_span(fn_node),
        flags,
        params,
        return_type,
        decorators,
    })
}

/// Parse Rust's visibility modifier text into a 3-bit code.
fn rust_visibility(txt: &str) -> u16 {
    let t = txt.trim();
    match t {
        "pub" => 0,
        t if t.starts_with("pub(crate)") => 3,
        t if t.starts_with("pub(super)") => 4,
        t if t.starts_with("pub(in ") => 3,
        // No modifier → private.
        _ => 2,
    }
}

/// Returns `true` if the first parameter of `params_node` is a self receiver
/// (`self`, `&self`, `&mut self`, `mut self`).
fn first_param_is_self(params_node: &Node<'_>, source: &[u8]) -> bool {
    let mut c = params_node.walk();
    if !c.goto_first_child() {
        return false;
    }
    // Skip `(` open paren.
    loop {
        let n = c.node();
        if n.kind() == "(" {
            if !c.goto_next_sibling() {
                return false;
            }
            continue;
        }
        // self_parameter kind covers `self`, `&self`, `&mut self`, `mut self`.
        return n.kind() == "self_parameter"
            || node_text(&n, source)
                .trim_start_matches("mut ")
                .trim_start_matches("&mut ")
                .trim_start_matches("& mut ")
                .trim_start_matches("&")
                == "self";
    }
}

/// Extract flat `[name1, type1, name2, type2, ...]` from Rust `parameters`.
fn extract_params(fn_node: &Node<'_>, source: &[u8]) -> Vec<String> {
    let Some(params_node) = fn_node.child_by_field_name("parameters") else {
        return vec![];
    };
    let mut result = Vec::new();
    let mut cursor = params_node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            match child.kind() {
                "parameter" => {
                    // `pattern: type` — field names `pattern` and `type`.
                    let name = child
                        .child_by_field_name("pattern")
                        .map(|n| {
                            // Strip leading `mut ` from `mut x`.
                            node_text(&n, source).trim_start_matches("mut ").to_string()
                        })
                        .unwrap_or_default();
                    let ty = child
                        .child_by_field_name("type")
                        .map(|n| node_text(&n, source).to_string())
                        .unwrap_or_default();
                    if !name.is_empty() {
                        result.push(name);
                        result.push(ty);
                    }
                }
                "self_parameter" => {
                    // `self` / `&self` / `&mut self`.
                    result.push(node_text(&child, source).to_string());
                    result.push(String::new());
                }
                _ => {}
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    result
}
