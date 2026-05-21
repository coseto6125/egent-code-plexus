//! C# FunctionMeta extraction.
//!
//! Rules:
//! - `is_async`:     `async` modifier
//! - `is_static`:    `static` modifier
//! - `is_abstract`:  `abstract` modifier OR interface method without body
//! - `is_generator`: body contains `yield return` or `yield break`
//! - `is_extern`:    `extern` modifier
//! - `is_test`:      has [Test] / [Fact] / [Theory] / [TestMethod] attribute OR file category Test
//! - `visibility`:   `public` → 0, `protected` → 1, `private` → 2, `internal` → 3.
//!   Default in class → 2 (private); default in interface → 0 (public).
//! - `params`:       `type identifier` pairs from `parameter` nodes
//! - `return_type`:  before method name; `void` → empty string for clean consumption
//! - `decorators`:   C# attribute names with `[]` stripped and `(...)` dropped

use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta, NodeKind};
use tree_sitter::Node;

type FnSpan<'a> = ((u32, u32, u32, u32), &'a RawNode);

const TEST_ATTRIBUTES: &[&str] = &["Test", "Fact", "Theory", "TestMethod", "TestCase"];

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

const CSHARP_FN_KINDS: &[&str] = &[
    "method_declaration",
    "constructor_declaration",
    "local_function_statement",
    "delegate_declaration",
];

fn collect_fn_nodes<'a>(
    node: Node<'a>,
    source: &[u8],
    fn_spans: &[FnSpan<'a>],
    file_category: FileCategory,
    out: &mut Vec<RawFunctionMeta>,
) {
    let k = node.kind();
    if CSHARP_FN_KINDS.contains(&k) {
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

    // Determine default visibility from context:
    // - interface methods default to public (0)
    // - class methods default to private (2)
    let in_interface = fn_node
        .parent()
        .and_then(|p| p.parent())
        .map(|gp| gp.kind() == "interface_declaration")
        .unwrap_or(false);
    let mut vis_code: u16 = if in_interface { 0 } else { 2 };

    // Collect attributes (decorators) and modifiers.
    let mut decorators: Vec<String> = Vec::new();

    // In tree-sitter-c-sharp, method_declaration has `repeat($.modifier)` followed by
    // `attribute_list*` as direct unnamed children. Each modifier keyword is a node of
    // kind `"modifier"` whose text is the keyword itself (e.g. "async", "static").
    {
        let mut c = fn_node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                match child.kind() {
                    "modifier" => {
                        let txt = node_text(&child, source).trim();
                        match txt {
                            "async" => flags |= FunctionMeta::FLAG_ASYNC,
                            "static" => flags |= FunctionMeta::FLAG_STATIC,
                            "abstract" => flags |= FunctionMeta::FLAG_ABSTRACT,
                            "extern" => flags |= FunctionMeta::FLAG_EXTERN,
                            "public" => vis_code = 0,
                            "protected" => vis_code = 1,
                            "private" => vis_code = 2,
                            "internal" => vis_code = 3,
                            _ => {}
                        }
                    }
                    "attribute_list" => {
                        // `[Attr]` / `[Attr(args)]` nodes contain `attribute` children.
                        collect_csharp_attributes(&child, source, &mut decorators);
                    }
                    _ => {}
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    // Merge query-captured decorators.
    for d in &raw.decorators {
        let name = csharp_attribute_name(d);
        if !name.is_empty() && !decorators.contains(&name) {
            decorators.push(name);
        }
    }

    // Abstract: interface method without body OR no body AND abstract modifier.
    if in_interface && fn_node.child_by_field_name("body").is_none() {
        flags |= FunctionMeta::FLAG_ABSTRACT;
    }

    // is_generator: body contains `yield_return_statement` or `yield_break_statement`.
    if let Some(body) = fn_node.child_by_field_name("body") {
        if subtree_has_yield(body) {
            flags |= FunctionMeta::FLAG_GENERATOR;
        }
    }

    // is_test: file category OR known test attribute.
    let is_test = file_category == FileCategory::Test
        || decorators.iter().any(|d| TEST_ATTRIBUTES.contains(&&**d));
    if is_test {
        flags |= FunctionMeta::FLAG_TEST;
    }

    flags |= vis_code << 6;

    let params = extract_params(fn_node, source);

    // Return type field name is "returns" in tree-sitter-c-sharp.
    let return_type = fn_node
        .child_by_field_name("returns")
        .map(|n| {
            let t = node_text(&n, source).to_string();
            if t == "void" {
                String::new()
            } else {
                t
            }
        })
        .unwrap_or_default();

    Some(RawFunctionMeta {
        span: ts_span(fn_node),
        flags,
        params,
        return_type,
        decorators,
    })
}

/// Recursively collect `attribute` children of an `attribute_list` node.
fn collect_csharp_attributes(attr_list: &Node<'_>, source: &[u8], out: &mut Vec<String>) {
    let mut c = attr_list.walk();
    if c.goto_first_child() {
        loop {
            let child = c.node();
            if child.kind() == "attribute" {
                // The name is the first child (qualified_name or identifier).
                let name = if let Some(n) = child.child_by_field_name("name") {
                    // Take just the rightmost identifier for qualified names.
                    let txt = node_text(&n, source);
                    txt.split('.').next_back().unwrap_or(txt).to_string()
                } else if let Some(n) = child.child(0) {
                    node_text(&n, source).to_string()
                } else {
                    String::new()
                };
                let clean = csharp_attribute_name(&name);
                if !clean.is_empty() && !out.contains(&clean) {
                    out.push(clean);
                }
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Strip `[`, `]` wrappers and `(...)` argument suffix from an attribute string.
fn csharp_attribute_name(s: &str) -> String {
    let s = s
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    s.split('(').next().unwrap_or(s).trim().to_string()
}

/// Walk subtree looking for `yield_statement` (C# grammar uses a single node for
/// both `yield return ...` and `yield break`).
fn subtree_has_yield(node: Node<'_>) -> bool {
    if node.kind() == "yield_statement" {
        return true;
    }
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            if subtree_has_yield(c.node()) {
                return true;
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
    false
}

fn extract_params(fn_node: &Node<'_>, source: &[u8]) -> Vec<String> {
    let Some(params_node) = fn_node.child_by_field_name("parameters") else {
        return vec![];
    };
    let mut result = Vec::new();
    let mut cursor = params_node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.kind() == "parameter" {
                let ty = child
                    .child_by_field_name("type")
                    .map(|n| node_text(&n, source).to_string())
                    .unwrap_or_default();
                let name = child
                    .child_by_field_name("name")
                    .map(|n| node_text(&n, source).to_string())
                    .unwrap_or_default();
                if !name.is_empty() {
                    result.push(name);
                    result.push(ty);
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    result
}
