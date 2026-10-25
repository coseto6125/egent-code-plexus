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
//! - extension:      `static` method, first parameter `this`, in a top-level
//!   non-generic `static` class → `EXTENSION_FLAG` (bit 10); a `this` type
//!   that is a method type parameter adds `GENERIC_THIS_FLAG` (bit 11)

use super::{extract_with, node_text, ts_span};
use ecp_core::analyzer::csharp_extension::{EXTENSION_FLAG, GENERIC_THIS_FLAG};
use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta};
use tree_sitter::Node;

const TEST_ATTRIBUTES: &[&str] = &["Test", "Fact", "Theory", "TestMethod", "TestCase"];

const CSHARP_FN_KINDS: &[&str] = &[
    "method_declaration",
    "constructor_declaration",
    "local_function_statement",
    "delegate_declaration",
];

pub fn extract(
    root: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    file_category: FileCategory,
) -> Vec<RawFunctionMeta> {
    extract_with(
        root,
        source,
        nodes,
        file_category,
        CSHARP_FN_KINDS,
        extract_one,
    )
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
    if flags & FunctionMeta::FLAG_STATIC != 0 {
        flags |= extension_flags(fn_node, source);
    }

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

/// [`EXTENSION_FLAG`] for a static method whose first parameter carries
/// `this` and whose declaring type is a top-level, non-generic `static`
/// class, plus [`GENERIC_THIS_FLAG`] when that parameter's type is one of the
/// method's type parameters. 0 for any other method. Preprocessor blocks
/// between the method and its class add nothing.
fn extension_flags(fn_node: &Node<'_>, source: &[u8]) -> u16 {
    if fn_node.kind() != "method_declaration" {
        return 0;
    }
    let Some(first) = fn_node
        .child_by_field_name("parameters")
        .and_then(|params| {
            let mut c = params.walk();
            let first = params
                .named_children(&mut c)
                .find(|p| p.kind() == "parameter");
            first
        })
    else {
        return 0;
    };
    let mut c = first.walk();
    let this_param = first
        .children(&mut c)
        .any(|m| m.kind() == "modifier" && node_text(&m, source).trim() == "this");
    let Some(class) = enclosing_declaration(*fn_node) else {
        return 0;
    };
    let static_class = class.kind() == "class_declaration"
        && !has_child_kind(class, "type_parameter_list")
        && has_modifier(class, source, "static")
        && enclosing_declaration(class).is_none_or(|outer| {
            !matches!(
                outer.kind(),
                "class_declaration"
                    | "struct_declaration"
                    | "interface_declaration"
                    | "record_declaration"
            )
        });
    if !this_param || !static_class {
        return 0;
    }
    let this_type = first
        .child_by_field_name("type")
        .map(|t| node_text(&t, source).trim())
        .unwrap_or_default();
    let this_head = this_type.trim_end_matches('?').trim_end();
    let generic_this = fn_node
        .child_by_field_name("type_parameters")
        .is_some_and(|list| {
            let mut c = list.walk();
            let generic = list
                .named_children(&mut c)
                .filter(|p| p.kind() == "type_parameter")
                .filter_map(|p| p.child_by_field_name("name"))
                .any(|name| node_text(&name, source) == this_head);
            generic
        });
    if generic_this {
        EXTENSION_FLAG | GENERIC_THIS_FLAG
    } else {
        EXTENSION_FLAG
    }
}

/// The nearest declaration enclosing `node` past declaration lists and
/// preprocessor blocks: its type or namespace.
fn enclosing_declaration(node: Node<'_>) -> Option<Node<'_>> {
    let mut parent = node.parent();
    while let Some(p) = parent {
        if p.kind() != "declaration_list" && !p.kind().starts_with("preproc_") {
            return Some(p);
        }
        parent = p.parent();
    }
    None
}

fn has_child_kind(node: Node<'_>, kind: &str) -> bool {
    let mut c = node.walk();
    let found = node.children(&mut c).any(|child| child.kind() == kind);
    found
}

fn has_modifier(node: Node<'_>, source: &[u8], modifier: &str) -> bool {
    let mut c = node.walk();
    let found = node
        .children(&mut c)
        .any(|child| child.kind() == "modifier" && node_text(&child, source).trim() == modifier);
    found
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
