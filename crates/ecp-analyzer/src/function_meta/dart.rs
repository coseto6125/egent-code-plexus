//! Dart FunctionMeta extraction.
//!
//! Rules:
//! - `is_async`:     `async` or `async*` modifier after `)` (both true — async* is async generator)
//! - `is_static`:    `static` modifier
//! - `is_abstract`:  `abstract` modifier OR method without body in abstract class
//! - `is_generator`: `sync*` or `async*` modifier (explicit Dart generator markers)
//! - `is_extern`:    `external` modifier
//! - `is_test`:      function inside `test(...)` / `group(...)` call OR file in test/ directory
//! - `visibility`:   leading `_` in name → 2 (private); else → 0 (public).
//!   Dart has no protected/internal at language level.
//! - `params`:       positional + optional named (`{}`) + optional positional (`[]`).
//!   Named params store name + type; positional optional params likewise.
//! - `return_type`:  type before function name; absent → empty (dynamic)
//! - `decorators`:   `@override` / `@deprecated` / `@protected` etc.

use super::{extract_with, find_child_kind, node_text, ts_span};
use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta};
use tree_sitter::Node;

const DART_FN_KINDS: &[&str] = &[
    "function_declaration",
    "function_signature",
    "method_declaration",
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
        DART_FN_KINDS,
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

    // Visibility from name convention: leading `_` → private (2), else public (0).
    let vis_code: u16 = if raw.name.starts_with('_') { 2 } else { 0 };

    // Scan modifiers from the function body / declaration signature.
    // Dart function bodies contain an `async_marker` or function_signature has modifiers.
    // We use a lightweight text scan of the function header for `async`, `async*`, `sync*`,
    // `static`, `abstract`, `external` — these always appear before `{` or `=>`.
    // Scan the function header (before the body `{` or `=>`) for modifiers.
    // Also scan the narrow window between `)` of params and `{`/`=>` for
    // async/sync markers. We use a two-step approach:
    //   1. Walk direct children for known modifier token nodes.
    //   2. Text-scan the inter-params-to-body window for async*/sync*/async.
    {
        let mut c = fn_node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                let kind = child.kind();
                match kind {
                    "static" => flags |= FunctionMeta::FLAG_STATIC,
                    "abstract" => flags |= FunctionMeta::FLAG_ABSTRACT,
                    "external" => flags |= FunctionMeta::FLAG_EXTERN,
                    _ => {}
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    // Also check the parent node for modifiers that wrap the function node.
    // In Dart, `external_function_declaration` wraps a `function_signature` with
    // `external` as a sibling; similarly `declaration` can have `external` before the
    // inner `function_signature`. We scan the parent's text prefix before this node.
    if let Some(parent) = fn_node.parent() {
        let parent_kind = parent.kind();
        if matches!(
            parent_kind,
            "external_function_declaration" | "declaration" | "top_level_declaration"
        ) {
            let parent_text = node_text(&parent, source);
            let fn_start = fn_node.start_byte() - parent.start_byte();
            let prefix = &parent_text[..fn_start.min(parent_text.len())];
            for word in prefix.split_whitespace() {
                match word {
                    "external" => flags |= FunctionMeta::FLAG_EXTERN,
                    "static" => flags |= FunctionMeta::FLAG_STATIC,
                    "abstract" => flags |= FunctionMeta::FLAG_ABSTRACT,
                    _ => {}
                }
            }
        }
    }

    // Also scan the function signature text for words in the header (before `{`/`=>`).
    // This catches `external` / `static` / `abstract` modifiers that may appear
    // as plain text tokens not captured as separate child kinds.
    {
        let fn_text = node_text(fn_node, source);
        let body_pos = fn_text
            .find('{')
            .or_else(|| fn_text.find("=>"))
            .unwrap_or(fn_text.len());
        let header = &fn_text[..body_pos];
        for word in header.split_whitespace() {
            match word {
                "static" => flags |= FunctionMeta::FLAG_STATIC,
                "abstract" => flags |= FunctionMeta::FLAG_ABSTRACT,
                "external" => flags |= FunctionMeta::FLAG_EXTERN,
                _ => {}
            }
        }

        // The `async` / `async*` / `sync*` marker appears in Dart between the closing
        // `)` of the formal parameter list and `{`/`=>`. We find the FIRST `{` or `=>`
        // and scan backward from there to find the marker.
        // To avoid matching `)` inside the body (e.g. `for (...) yield`), we restrict
        // the scan to the header portion only.
        if header.contains("async*") {
            flags |= FunctionMeta::FLAG_ASYNC;
            flags |= FunctionMeta::FLAG_GENERATOR;
        } else if header.contains("sync*") {
            flags |= FunctionMeta::FLAG_GENERATOR;
        } else if header.contains("async") {
            flags |= FunctionMeta::FLAG_ASYNC;
        }
    }

    // Abstract: method without body in an abstract class context.
    if fn_node.child_by_field_name("body").is_none()
        && fn_node.child_by_field_name("function_body").is_none()
    {
        // Try to detect if the parent is a method_declaration inside an abstract class.
        let parent_is_method_decl = fn_node
            .parent()
            .map(|p| p.kind() == "method_declaration")
            .unwrap_or(false);
        if parent_is_method_decl {
            flags |= FunctionMeta::FLAG_ABSTRACT;
        }
    }

    // Collect decorators from annotations.
    // Dart annotations can appear:
    //   (a) As children of the fn_node itself (e.g. `method_declaration` has `optional($._metadata)` first)
    //   (b) As siblings immediately before fn_node in the parent (for `function_declaration`)
    let mut decorators: Vec<String> = Vec::new();
    collect_dart_annotations_from_children(fn_node, source, &mut decorators);
    collect_dart_decorators(fn_node, source, &mut decorators);

    // Merge query-captured decorators.
    for d in &raw.decorators {
        let name = d
            .trim_start_matches('@')
            .split('(')
            .next()
            .unwrap_or(d)
            .trim()
            .to_string();
        if !name.is_empty() && !decorators.contains(&name) {
            decorators.push(name);
        }
    }

    // is_test: file category Test (which includes test/ directory via determine_category).
    let is_test = file_category == FileCategory::Test;
    if is_test {
        flags |= FunctionMeta::FLAG_TEST;
    }

    flags |= vis_code << 6;

    let params = extract_params(fn_node, source);

    // Return type: the `return_type` field or the `type` child before the function name.
    let return_type = fn_node
        .child_by_field_name("return_type")
        .or_else(|| {
            // For function_declaration, the function_signature child has the return_type.
            find_child_by_kind(fn_node, "function_signature")
                .and_then(|sig| sig.child_by_field_name("return_type"))
        })
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

/// Collect Dart annotation nodes that are direct children of fn_node
/// (appearing before the signature — covers `method_declaration` which embeds `_metadata`).
fn collect_dart_annotations_from_children(
    fn_node: &Node<'_>,
    source: &[u8],
    out: &mut Vec<String>,
) {
    let mut c = fn_node.walk();
    if !c.goto_first_child() {
        return;
    }
    loop {
        let child = c.node();
        if child.kind() == "annotation" {
            let name = if let Some(n) = child.child_by_field_name("name") {
                node_text(&n, source)
                    .trim_start_matches('@')
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string()
            } else {
                node_text(&child, source)
                    .trim_start_matches('@')
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
        } else {
            // Stop at first non-annotation child (metadata is always at the start).
            break;
        }
        if !c.goto_next_sibling() {
            break;
        }
    }
}

/// Collect Dart annotation nodes that are siblings immediately preceding this function.
fn collect_dart_decorators(fn_node: &Node<'_>, source: &[u8], out: &mut Vec<String>) {
    let Some(parent) = fn_node.parent() else {
        return;
    };
    let mut pending: Vec<String> = Vec::new();
    let mut c = parent.walk();
    if !c.goto_first_child() {
        return;
    }
    loop {
        let sib = c.node();
        if sib.id() == fn_node.id() {
            out.append(&mut pending);
            break;
        }
        if sib.kind() == "annotation" {
            // Extract annotation name.
            let name = if let Some(n) = sib.child_by_field_name("name") {
                node_text(&n, source)
                    .trim_start_matches('@')
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string()
            } else {
                let txt = node_text(&sib, source);
                txt.trim_start_matches('@')
                    .split('(')
                    .next()
                    .unwrap_or(txt)
                    .trim()
                    .to_string()
            };
            if !name.is_empty() {
                pending.push(name);
            }
        } else {
            // Non-annotation sibling resets accumulator.
            pending.clear();
        }
        if !c.goto_next_sibling() {
            break;
        }
    }
}

/// Shim around the workspace-shared helper so the old call sites keep their
/// original `find_child_by_kind` name without an extra source-wide rename.
#[allow(dead_code)]
fn find_child_by_kind<'a>(node: &Node<'a>, kind: &str) -> Option<Node<'a>> {
    find_child_kind(node, kind)
}

/// Extract `[name1, type1, ...]` from Dart formal_parameter_list.
fn extract_params(fn_node: &Node<'_>, source: &[u8]) -> Vec<String> {
    // Dart: parameters field or formal_parameter_list child.
    let params_node = fn_node
        .child_by_field_name("parameters")
        .or_else(|| find_child_by_kind(fn_node, "formal_parameter_list"))
        .or_else(|| {
            // For function_declaration → function_signature → formal_parameter_list.
            find_child_by_kind(fn_node, "function_signature").and_then(|sig| {
                sig.child_by_field_name("parameters")
                    .or_else(|| find_child_by_kind(&sig, "formal_parameter_list"))
            })
        });

    let Some(params_node) = params_node else {
        return vec![];
    };

    let mut result = Vec::new();
    collect_param_list(&params_node, source, &mut result);
    result
}

fn collect_param_list(params_node: &Node<'_>, source: &[u8], result: &mut Vec<String>) {
    let mut cursor = params_node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            match child.kind() {
                "formal_parameter" => {
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
                "optional_formal_parameters" | "named_argument" => {
                    // Recurse into `{}` or `[]` optional/named parameter groups.
                    collect_param_list(&child, source, result);
                }
                _ => {}
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}
