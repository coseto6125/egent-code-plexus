//! Ruby FunctionMeta extraction.
//!
//! Ruby-specific rules:
//! - `is_async`: always false (Ruby concurrency via fibers/threads, not language-level async).
//! - `is_static`: `singleton_method` node (`def self.foo`) OR method inside `class << self` block.
//! - `is_abstract` (heuristic): method body consists only of `raise NotImplementedError` or
//!   `raise "must implement"`. This is a convention, not authoritative — documented as such.
//! - `is_generator`: method body contains `yield` (Ruby block-based iteration).
//! - `is_extern`: always false.
//! - `is_test`: method in RSpec `it`/`specify`/`describe` OR file in test/spec directory
//!   (FileCategory::Test) OR method name starts with `test_` (Minitest convention).
//! - visibility: track `private`/`protected`/`public` visibility-modifier sections; default = public.
//!   NOTE: Ruby visibility is a method call (`private; def foo`) not a syntactic modifier on the
//!   def node. We detect `singleton_class` wrappers for is_static; simple visibility-modifier
//!   adjacent detection is a best-effort heuristic since tree-sitter-ruby doesn't annotate the def.
//! - params: positional / optional / keyword / splat / block-arg; no type annotations in stock Ruby.
//! - return_type: always empty (Ruby has no explicit return type annotation).
//! - decorators: always empty Vec (no decorator syntax in stock Ruby).

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
    // Both `method` (instance) and `singleton_method` (`def self.foo`) map to Method nodes.
    if k == "method" || k == "singleton_method" {
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

    // is_static: `singleton_method` node OR method inside `singleton_class` (`class << self`).
    let is_static = fn_node.kind() == "singleton_method"
        || fn_node
            .parent()
            .and_then(|p| p.parent())
            .map(|gp| gp.kind() == "singleton_class")
            .unwrap_or(false);
    if is_static {
        flags |= FunctionMeta::FLAG_STATIC;
    }

    // is_generator: body contains a `yield` node.
    if let Some(body) = fn_node.child_by_field_name("body") {
        if subtree_contains_kind(body, "yield") {
            flags |= FunctionMeta::FLAG_GENERATOR;
        }
    }

    // is_abstract (heuristic): body is only a `raise` of NotImplementedError.
    if is_abstract_heuristic(fn_node, source) {
        flags |= FunctionMeta::FLAG_ABSTRACT;
    }

    // is_test: FileCategory::Test OR name starts with `test_` (Minitest).
    // RSpec `it`/`specify` blocks are captured as Method nodes when inside describe.
    if file_category == FileCategory::Test
        || raw.name.starts_with("test_")
        || matches!(raw.name.as_str(), "it" | "specify" | "example")
    {
        flags |= FunctionMeta::FLAG_TEST;
    }

    // visibility: best-effort — scan preceding siblings for `private`/`protected`/`public` calls.
    // Ruby visibility is applied by method-call siblings (`private; def foo`), not modifiers on def.
    let vis_code = infer_visibility(fn_node, source);
    flags |= vis_code << 6;

    // Params: Ruby params have no type annotations in stock syntax.
    let params = extract_params(fn_node, source);

    // return_type: always empty in standard Ruby.
    let return_type = String::new();

    // decorators: always empty (no decorator syntax in stock Ruby).
    let decorators: Vec<String> = Vec::new();

    Some(RawFunctionMeta {
        span: ts_span(fn_node),
        flags,
        params,
        return_type,
        decorators,
    })
}

/// Heuristic: method body contains only a `raise` of NotImplementedError / "must implement".
/// In tree-sitter-ruby, `raise` is a `call` node (keyword method call), not a separate kind.
/// We scan the body's source text for the pattern — it's a heuristic, not authoritative.
fn is_abstract_heuristic(fn_node: &Node<'_>, source: &[u8]) -> bool {
    let Some(body) = fn_node.child_by_field_name("body") else {
        return false;
    };
    // Scan source text of the body for raise patterns.
    let body_text = node_text(&body, source);
    if !body_text.contains("raise") {
        return false;
    }
    // Must mention NotImplementedError or "must implement".
    let has_not_implemented = body_text.contains("NotImplementedError")
        || body_text.contains("must implement")
        || body_text.contains("not implemented");
    if !has_not_implemented {
        return false;
    }
    // Heuristic: body should be short (only the raise statement).
    // Check that there's no significant other code by counting non-whitespace lines.
    let non_empty_lines: Vec<&str> = body_text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    // Allow at most 1-2 lines (the raise itself, possibly with a string arg).
    non_empty_lines.len() <= 2
}

/// Infer Ruby method visibility from adjacent sibling `identifier` calls.
/// Walks preceding siblings of `fn_node` within the same parent, looking for the
/// closest `private` / `protected` / `public` identifier call.
fn infer_visibility(fn_node: &Node<'_>, source: &[u8]) -> u16 {
    let Some(parent) = fn_node.parent() else {
        return 0;
    };
    let mut last_vis: u16 = 0; // default public
    let mut c = parent.walk();
    if c.goto_first_child() {
        loop {
            let sib = c.node();
            if sib.id() == fn_node.id() {
                break;
            }
            // Look for bare `private` / `protected` / `public` identifier nodes.
            if sib.kind() == "identifier" {
                let txt = node_text(&sib, source);
                match txt {
                    "public" => last_vis = 0,
                    "protected" => last_vis = 1,
                    "private" => last_vis = 2,
                    _ => {}
                }
            } else if sib.kind() == "call" {
                // `private :foo` style — the receiver changes visibility of named methods.
                // For now only track bare-call visibility changes (the common pattern).
                if let Some(method) = sib.child_by_field_name("method") {
                    let txt = node_text(&method, source);
                    match txt {
                        "public" => last_vis = 0,
                        "protected" => last_vis = 1,
                        "private" => last_vis = 2,
                        _ => {}
                    }
                }
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
    last_vis
}

/// Recursively check whether a subtree contains a node of the given kind.
fn subtree_contains_kind(node: Node<'_>, kind: &str) -> bool {
    if node.kind() == kind {
        return true;
    }
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            if subtree_contains_kind(cursor.node(), kind) {
                return true;
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    false
}

/// Extract flat `[name1, "", name2, "", ...]` (no type annotations in stock Ruby).
fn extract_params(fn_node: &Node<'_>, source: &[u8]) -> Vec<String> {
    let Some(params_node) = fn_node.child_by_field_name("parameters") else {
        return vec![];
    };
    let mut result = Vec::new();
    let mut cursor = params_node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            let name_opt: Option<String> = match child.kind() {
                "identifier" => Some(node_text(&child, source).to_string()),
                "optional_parameter" => child
                    .child_by_field_name("name")
                    .map(|n| node_text(&n, source).to_string()),
                "keyword_parameter" => child.child_by_field_name("name").map(|n| {
                    // keyword params have trailing `:` in the name node; strip it.
                    node_text(&n, source).trim_end_matches(':').to_string()
                }),
                "splat_parameter" => child
                    .child(1) // child(0) is `*`
                    .map(|n| format!("*{}", node_text(&n, source))),
                "hash_splat_parameter" => child
                    .child(1) // child(0) is `**`
                    .map(|n| format!("**{}", node_text(&n, source))),
                "block_parameter" => child
                    .child(1) // child(0) is `&`
                    .map(|n| format!("&{}", node_text(&n, source))),
                _ => None,
            };
            if let Some(name) = name_opt {
                if !name.is_empty() {
                    result.push(name);
                    result.push(String::new()); // no type annotation
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    result
}
