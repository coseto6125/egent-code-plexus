//! Rust FunctionMeta extraction.
//!
//! Walks the tree-sitter AST to detect `async fn`, visibility modifiers,
//! `#[test]`/`#[tokio::test]` attributes, `extern "ABI"` declarations, and
//! parameter/return_type signatures.

use super::{extract_with, node_text, ts_span};
use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta, NodeKind};
use tree_sitter::Node;

/// `function_item` = concrete fn; `function_signature_item` = trait abstract fn.
const RUST_FN_KINDS: &[&str] = &[
    "function_item",
    "function_signature_item",
    "closure_expression",
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
        RUST_FN_KINDS,
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

    // Collect attribute items (`#[...]`) that precede this function in the parent block.
    // In Rust's grammar, attributes are sibling nodes (attribute_item / inner_attribute_item),
    // NOT children of function_item. The query system doesn't capture them generically,
    // so we walk the parent's children to find contiguous attributes immediately before
    // this fn node. Merge with any decorators already captured by the query system.
    let decorators: Vec<String> = {
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
                    "async" if fn_node.kind() == "closure_expression" => has_async = true,
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

    let is_test = file_category == FileCategory::Test || in_test_scope(*fn_node, source);
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

fn in_test_scope(mut node: Node<'_>, source: &[u8]) -> bool {
    // The ancestor walk never enters the callable's own body, yet a leading
    // `#![cfg(test)]` there gates the whole function.
    if node
        .child_by_field_name("body")
        .is_some_and(|body| leading_test_attribute(body, "inner_attribute_item", source))
    {
        return true;
    }
    loop {
        let mut previous = node.prev_named_sibling();
        while let Some(sibling) = previous {
            match sibling.kind() {
                "attribute_item" => {
                    if test_attribute(sibling, source, RUST_FN_KINDS.contains(&node.kind())) {
                        return true;
                    }
                }
                "line_comment" | "block_comment" => {}
                _ => break,
            }
            previous = sibling.prev_named_sibling();
        }

        let leading = match node.kind() {
            "source_file" | "declaration_list" | "block" => Some("inner_attribute_item"),
            // These nodes hold their outer attributes as leading children,
            // not as preceding siblings.
            "match_arm" | "field_initializer" => Some("attribute_item"),
            _ => None,
        };
        if leading.is_some_and(|kind| leading_test_attribute(node, kind, source)) {
            return true;
        }
        let Some(parent) = node.parent() else {
            return false;
        };
        node = parent;
    }
}

fn leading_test_attribute(node: Node<'_>, attribute_kind: &str, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "line_comment" | "block_comment" => {}
            kind if kind == attribute_kind => {
                if test_attribute(child, source, false) {
                    return true;
                }
            }
            _ => break,
        }
    }
    false
}

fn test_attribute(item: Node<'_>, source: &[u8], allow_test: bool) -> bool {
    let Some(attribute) = item.named_child(0) else {
        return false;
    };
    let Some(path) = attribute.named_child(0) else {
        return false;
    };
    let name = path.child_by_field_name("name").unwrap_or(path);
    if allow_test && node_text(&name, source) == "test" {
        return true;
    }
    if node_text(&path, source) != "cfg" {
        return false;
    }
    let Some(arguments) = attribute.child_by_field_name("arguments") else {
        return false;
    };
    cfg_outcomes(arguments, "cfg", source, false).is_some_and(|(possible, _)| !possible)
        && cfg_outcomes(arguments, "cfg", source, true).is_some_and(|(possible, _)| possible)
}

// Other cfg options remain unknown, so mixed `any(test, feature = "...")`
// never excludes production. Both configurations distinguish tests from dead code.
fn cfg_outcomes(
    tree: Node<'_>,
    operator: &str,
    source: &[u8],
    test_enabled: bool,
) -> Option<(bool, bool)> {
    let mut cursor = tree.walk();
    let mut tokens = tree
        .children(&mut cursor)
        .filter(|node| !matches!(node.kind(), "(" | ")" | "line_comment" | "block_comment"))
        .peekable();
    let mut count = 0;
    let mut all = (true, false);
    let mut any = (false, true);
    while let Some(name) = tokens.next() {
        if name.kind() != "identifier" {
            return None;
        }
        let name = node_text(&name, source);
        let outcome = match tokens.peek().map(|node| node.kind()) {
            Some("token_tree") => cfg_outcomes(tokens.next()?, name, source, test_enabled)?,
            Some("=") => {
                tokens.next();
                let value = tokens.next()?;
                if !matches!(value.kind(), "string_literal" | "raw_string_literal") {
                    return None;
                }
                (true, true)
            }
            _ if name == "test" => (test_enabled, !test_enabled),
            _ => (true, true),
        };
        count += 1;
        all = (all.0 && outcome.0, all.1 || outcome.1);
        any = (any.0 || outcome.0, any.1 && outcome.1);
        if let Some(separator) = tokens.next() {
            if node_text(&separator, source) != "," {
                return None;
            }
        }
    }
    match operator {
        "all" => Some(all),
        "any" => Some(any),
        "not" if count == 1 => Some((all.1, all.0)),
        "cfg" if count == 1 => Some(all),
        _ => None,
    }
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
