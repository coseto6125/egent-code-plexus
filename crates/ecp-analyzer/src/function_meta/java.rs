//! Java FunctionMeta extraction.
//!
//! Rules:
//! - `is_async`:     never (Java has no language-level async; CompletableFuture is library)
//! - `is_static`:    `static` modifier in the modifiers list
//! - `is_abstract`:  `abstract` modifier OR interface method (no body)
//! - `is_generator`: never (Java has no yield syntax)
//! - `is_extern`:    `native` modifier (JNI)
//! - `is_test`:      file category Test OR has @Test / @ParameterizedTest / @RepeatedTest /
//!   @BeforeEach / @AfterEach annotation
//! - `visibility`:   `public` → 0, `protected` → 1, `private` → 2, package-private → 4
//! - `params`:       `type identifier` pairs from `formal_parameter` nodes
//! - `return_type`:  return type before method name
//! - `decorators`:   annotation names with `@` stripped, args `(...)` dropped

use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta, NodeKind};
use tree_sitter::Node;

type FnSpan<'a> = ((u32, u32, u32, u32), &'a RawNode);

/// Java test annotation names (without `@`).
const TEST_ANNOTATIONS: &[&str] = &[
    "Test",
    "ParameterizedTest",
    "RepeatedTest",
    "BeforeEach",
    "AfterEach",
    "BeforeAll",
    "AfterAll",
];

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

/// Java function-like node kinds.
const JAVA_FN_KINDS: &[&str] = &[
    "method_declaration",
    "constructor_declaration",
    "annotation_type_element_declaration",
    // Interface method bodies: interface methods with no body are abstract.
    "interface_method_declaration", // may not exist in all grammar versions
];

fn collect_fn_nodes<'a>(
    node: Node<'a>,
    source: &[u8],
    fn_spans: &[FnSpan<'a>],
    file_category: FileCategory,
    out: &mut Vec<RawFunctionMeta>,
) {
    let k = node.kind();
    if JAVA_FN_KINDS.contains(&k) {
        let span = ts_span(&node);
        if let Some((_, raw)) = fn_spans.iter().find(|(s, _)| *s == span) {
            if let Some(meta) = extract_one(&node, source, raw, file_category) {
                out.push(meta);
            }
            return;
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
    let mut vis_code: u16 = 4; // package-private default

    // Collect annotations and modifiers.
    // In tree-sitter-java, `modifiers` is a direct unnamed child of method_declaration
    // (NOT a named field), so we must scan direct children for a node with kind
    // "modifiers" and then walk its children for individual modifier keywords.
    let mut decorators: Vec<String> = Vec::new();

    {
        let mut c = fn_node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                if child.kind() == "modifiers" {
                    // Walk inside the modifiers node.
                    let mut mc = child.walk();
                    if mc.goto_first_child() {
                        loop {
                            let m = mc.node();
                            match m.kind() {
                                "static" => {
                                    flags |= FunctionMeta::FLAG_STATIC;
                                }
                                "abstract" => {
                                    flags |= FunctionMeta::FLAG_ABSTRACT;
                                }
                                "native" => {
                                    flags |= FunctionMeta::FLAG_EXTERN;
                                }
                                "public" => vis_code = 0,
                                "protected" => vis_code = 1,
                                "private" => vis_code = 2,
                                "annotation" | "marker_annotation" => {
                                    let name = annotation_name(&m, source);
                                    if !name.is_empty() {
                                        decorators.push(name);
                                    }
                                }
                                _ => {}
                            }
                            if !mc.goto_next_sibling() {
                                break;
                            }
                        }
                    }
                    break; // modifiers always comes first; stop after finding it.
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    // Also merge decorators already captured by the query system.
    for d in &raw.decorators {
        let stripped = d.trim_start_matches('@').to_string();
        // Drop argument parens for storage: `Override(foo)` → `Override`.
        let name = stripped
            .split('(')
            .next()
            .unwrap_or(&stripped)
            .trim()
            .to_string();
        if !name.is_empty() && !decorators.contains(&name) {
            decorators.push(name);
        }
    }

    // Interface methods without a body are abstract.
    if fn_node.child_by_field_name("body").is_none() && fn_node.kind() == "method_declaration" {
        // Check parent is interface_body or annotation_type_body.
        if let Some(parent) = fn_node.parent() {
            if matches!(
                parent.kind(),
                "interface_body" | "annotation_type_body" | "interface_declaration"
            ) {
                flags |= FunctionMeta::FLAG_ABSTRACT;
            }
        }
    }

    // is_test: file category OR annotation in TEST_ANNOTATIONS list.
    let is_test = file_category == FileCategory::Test
        || decorators
            .iter()
            .any(|d| TEST_ANNOTATIONS.iter().any(|t| d == t));
    if is_test {
        flags |= FunctionMeta::FLAG_TEST;
    }

    flags |= vis_code << 6;

    // Parameters: walk formal_parameters or formal_parameter children.
    let params = extract_params(fn_node, source);

    // Return type: tree-sitter-java names the type field "type".
    let return_type = fn_node
        .child_by_field_name("type")
        .map(|n| node_text(&n, source).to_string())
        .unwrap_or_default();

    Some(RawFunctionMeta {
        span: ts_span(fn_node),
        flags,
        params,
        return_type,
        decorators,
    })
}

/// Extract annotation name from an `annotation` or `marker_annotation` node.
/// Returns just the bare class name with `@` stripped and args dropped.
fn annotation_name(node: &Node<'_>, source: &[u8]) -> String {
    // The `name` field of an annotation node holds the identifier.
    if let Some(name_node) = node.child_by_field_name("name") {
        return node_text(&name_node, source)
            .trim_start_matches('@')
            .to_string();
    }
    // Fallback: first identifier child.
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            let child = c.node();
            if child.kind() == "identifier" {
                return node_text(&child, source).to_string();
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
    String::new()
}

/// Extract `[name1, type1, name2, type2, ...]` from Java formal_parameters.
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
                "formal_parameter" | "spread_parameter" => {
                    // `Type name` — `type` and `name` fields.
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
                _ => {}
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    result
}
