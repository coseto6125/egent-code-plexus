//! PHP FunctionMeta extraction.
//!
//! PHP-specific rules:
//! - `is_async`: always false (PHP has Fibers since 8.1 but no `async` function keyword).
//! - `is_static`: `static` modifier present.
//! - `is_abstract`: `abstract` modifier OR method inside an interface.
//! - `is_generator`: function body contains `yield` (any variant).
//! - `is_extern`: always false (FFI is library-based, no declaration-only syntax).
//! - `is_test`: `#[Test]` PHP 8 attribute OR `@test` PHPDoc OR name starts with `test`
//!   (PHPUnit Minitest convention) OR FileCategory::Test.
//! - visibility: `public`/`protected`/`private` modifier; class default (no modifier) → public (0).
//! - params: `$name` (strip `$`) + optional type (including union types `int|string`).
//! - return_type: `: ReturnType` after `)`.
//! - decorators: PHP 8 attributes `#[Attr]` / `#[Attr(args)]` — capture name only (strip `#[` and `]`).

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

const PHP_FN_KINDS: &[&str] = &["function_definition", "method_declaration"];

fn collect_fn_nodes<'a>(
    node: Node<'a>,
    source: &[u8],
    fn_spans: &[FnSpan<'a>],
    file_category: FileCategory,
    out: &mut Vec<RawFunctionMeta>,
) {
    let k = node.kind();
    if PHP_FN_KINDS.contains(&k) {
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
    let mut vis_code: u16 = 0; // PHP default = public

    // Collect PHP 8 attributes.
    // In tree-sitter-php, attribute_list is a direct child of function_definition /
    // method_declaration (not a preceding sibling). We scan fn_node's own children.
    let mut attributes: Vec<String> = Vec::new();
    // Also collect PHPDoc comments for @test detection (siblings in parent).
    let mut has_phpdoc_test = false;
    {
        // 1. Direct children of fn_node: attribute_list.
        let mut c = fn_node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                if child.kind() == "attribute_list" {
                    extract_php_attribute_names(child, source, &mut attributes);
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    {
        // 2. Sibling comments for PHPDoc @test.
        if let Some(parent) = fn_node.parent() {
            let mut c = parent.walk();
            if c.goto_first_child() {
                loop {
                    let sib = c.node();
                    if sib.id() == fn_node.id() {
                        break;
                    }
                    if sib.kind() == "comment" {
                        let txt = node_text(&sib, source);
                        if txt.contains("@test") {
                            has_phpdoc_test = true;
                        }
                    }
                    if !c.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
    }

    // Scan direct children for modifiers.
    let mut is_abstract = false;
    let mut is_generator = false;
    {
        let mut c = fn_node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                match child.kind() {
                    "abstract_modifier" | "abstract" => {
                        is_abstract = true;
                    }
                    "static_modifier" | "static" => {
                        flags |= FunctionMeta::FLAG_STATIC;
                    }
                    "visibility_modifier" => {
                        let txt = node_text(&child, source);
                        vis_code = php_visibility(txt);
                    }
                    _ => {}
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    // Interface method → abstract.
    // Parent chain: method_declaration → declaration_list → interface_declaration.
    if fn_node.kind() == "method_declaration" {
        if let Some(parent) = fn_node.parent() {
            if parent.kind() == "declaration_list" {
                if let Some(grandparent) = parent.parent() {
                    if grandparent.kind() == "interface_declaration" {
                        is_abstract = true;
                    }
                }
            }
        }
    }

    if is_abstract {
        flags |= FunctionMeta::FLAG_ABSTRACT;
    }

    // is_generator: walk body for `yield_expression`.
    if let Some(body) = fn_node.child_by_field_name("body") {
        if subtree_contains_kind(body, "yield_expression") {
            is_generator = true;
        }
    }
    if is_generator {
        flags |= FunctionMeta::FLAG_GENERATOR;
    }

    // is_test: attribute `#[Test]` / `Test\Attributes\Test`, PHPDoc `@test`, name starts with `test`, or FileCategory::Test.
    let has_test_attr = attributes.iter().any(|a| {
        let s = a.as_str();
        s == "Test" || s.ends_with("\\Test") || s.ends_with("::Test")
    });
    if has_test_attr
        || has_phpdoc_test
        || raw.name.starts_with("test")
        || file_category == FileCategory::Test
    {
        flags |= FunctionMeta::FLAG_TEST;
    }

    flags |= vis_code << 6;

    // Params.
    let params = extract_params(fn_node, source);

    // Return type: `return_type` field (tree-sitter-php names this field).
    let return_type = fn_node
        .child_by_field_name("return_type")
        .map(|rt| {
            // return_type includes the `:` prefix in some grammar versions; strip it.
            let txt = node_text(&rt, source);
            txt.trim_start_matches(':').trim().to_string()
        })
        .unwrap_or_default();

    // Decorators from PHP 8 attributes (already collected above) merged with raw.decorators.
    let mut decorators = attributes;
    for d in &raw.decorators {
        // raw.decorators may contain `#[Attr]` strings; normalize them.
        let cleaned = d
            .trim()
            .trim_start_matches("#[")
            .trim_end_matches(']')
            .split('(')
            .next()
            .unwrap_or(d)
            .trim()
            .to_string();
        if !cleaned.is_empty() && !decorators.contains(&cleaned) {
            decorators.push(cleaned);
        }
    }

    Some(RawFunctionMeta {
        span: ts_span(fn_node),
        flags,
        params,
        return_type,
        decorators,
    })
}

/// Extract attribute names from an `attribute_list` node.
/// Tree-sitter-php grammar: attribute_list → attribute_group+ → `#[` attribute+ `]`
/// Each `attribute` has the name as its first child.
fn extract_php_attribute_names(attr_list: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    let mut c = attr_list.walk();
    if c.goto_first_child() {
        loop {
            let group = c.node();
            // attribute_group contains attribute nodes.
            if group.kind() == "attribute_group" {
                let mut gc = group.walk();
                if gc.goto_first_child() {
                    loop {
                        let anode = gc.node();
                        if anode.kind() == "attribute" {
                            // First child is the attribute name (qualified_name or name).
                            if let Some(name_node) = anode.child(0) {
                                let name = node_text(&name_node, source).to_string();
                                if !name.is_empty() && !out.contains(&name) {
                                    out.push(name);
                                }
                            }
                        }
                        if !gc.goto_next_sibling() {
                            break;
                        }
                    }
                }
            } else if group.kind() == "attribute" {
                // Some grammar versions flatten attribute_list → attribute directly.
                if let Some(name_node) = group.child(0) {
                    let name = node_text(&name_node, source).to_string();
                    if !name.is_empty() && !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
}

fn php_visibility(txt: &str) -> u16 {
    match txt.trim() {
        "public" => 0,
        "protected" => 1,
        "private" => 2,
        _ => 0,
    }
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

/// Extract flat `[name1, type1, name2, type2, ...]` from PHP function parameters.
/// Handles `simple_parameter` and `variadic_parameter`.
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
                "simple_parameter" | "variadic_parameter" | "property_promotion_parameter" => {
                    // Name: `variable_name` child contains the `$name`.
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| {
                            // variable_name node includes `$`; strip it.
                            let txt = node_text(&n, source);
                            txt.trim_start_matches('$').to_string()
                        })
                        .unwrap_or_default();
                    // Type: `type` field — may be union_type `int|string`.
                    let ty = child
                        .child_by_field_name("type")
                        .map(|n| node_text(&n, source).to_string())
                        .unwrap_or_default();
                    if !name.is_empty() {
                        result.push(name);
                        result.push(ty);
                    }
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
