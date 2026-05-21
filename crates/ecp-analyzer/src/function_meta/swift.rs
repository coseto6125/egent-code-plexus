//! Swift FunctionMeta extraction.
//!
//! Rules:
//! - `is_async`:     `async` keyword in the function signature (before `throws` / `->`)
//! - `is_static`:    `static` keyword OR `class` keyword (class methods in class hierarchy)
//! - `is_abstract`:  protocol requirement without body (`protocol_function_declaration`)
//! - `is_generator`: never (Sequence/IteratorProtocol is library, not language)
//! - `is_extern`:    `@_silgen_name(...)` or `@_cdecl(...)` attribute (private FFI markers)
//! - `is_test`:      function name starts with `test` AND in XCTestCase context,
//!   OR has `@Test` attribute (Swift Testing framework)
//! - `visibility`:   `open` → 0, `public` → 0, `internal` (default) → 3,
//!   `fileprivate` → 5, `private` → 2
//! - `params`:       internal parameter name + type captured (external label dropped)
//! - `return_type`:  `-> Type` after `)`, absent → empty
//! - `decorators`:   attribute names like `@MainActor`, `@Sendable`, `@Test`, etc.
//!
//! **Swift parameter label handling**: Swift parameters have both an external label and an
//! internal name: `func greet(to name: String)`. We capture the internal name (`name`)
//! and drop the external label (`to`) for simplicity. This matches what LLMs need for
//! call-site context: they see the parameter variable name used inside the function body.

use ecp_core::analyzer::types::{RawFunctionMeta, RawNode};
use ecp_core::graph::{FileCategory, FunctionMeta, NodeKind};
use tree_sitter::Node;

type FnSpan<'a> = ((u32, u32, u32, u32), &'a RawNode);

const FFI_ATTRIBUTES: &[&str] = &["_silgen_name", "_cdecl"];

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

const SWIFT_FN_KINDS: &[&str] = &[
    "function_declaration",
    "protocol_function_declaration",
    "init_declaration",
];

fn collect_fn_nodes<'a>(
    node: Node<'a>,
    source: &[u8],
    fn_spans: &[FnSpan<'a>],
    file_category: FileCategory,
    out: &mut Vec<RawFunctionMeta>,
) {
    let k = node.kind();
    if SWIFT_FN_KINDS.contains(&k) {
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
    let mut vis_code: u16 = 3; // Swift default is `internal`

    let mut decorators: Vec<String> = Vec::new();
    let mut is_extern = false;

    // Scan direct children for modifiers and attributes.
    // In tree-sitter-swift, `function_declaration` is built from `_bodyless_function_declaration`
    // which has: `optional($.modifiers)`, `optional("class")`, then the rest.
    // The `modifiers` node is a direct unnamed child of the function node.
    {
        let mut c = fn_node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                match child.kind() {
                    "modifiers" => {
                        scan_swift_modifiers(
                            &child,
                            source,
                            &mut flags,
                            &mut vis_code,
                            &mut decorators,
                            &mut is_extern,
                        );
                    }
                    "class" => {
                        // `class func` — Swift class method (not `static` but acts like it).
                        flags |= FunctionMeta::FLAG_STATIC;
                    }
                    _ => {}
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    // `async` in Swift appears between `)` closing params and `throws`/`->` as an
    // `async_keyword` node or `async` literal. Use a text scan of the header section
    // (before `{`) to handle grammar version variance.
    {
        let fn_text = node_text(fn_node, source);
        let header_end = fn_text
            .find('{')
            .unwrap_or(fn_text.len())
            .min(fn_text.find("->").unwrap_or(fn_text.len()));
        let header = &fn_text[..header_end];
        if header.split_whitespace().any(|w| w == "async") {
            flags |= FunctionMeta::FLAG_ASYNC;
        }
    }

    if is_extern {
        flags |= FunctionMeta::FLAG_EXTERN;
    }

    // Merge query-captured decorators.
    for d in &raw.decorators {
        let name = swift_attr_name(d);
        if !name.is_empty() && !decorators.contains(&name) {
            decorators.push(name);
        }
    }

    // Protocol function declarations are abstract (no body).
    if fn_node.kind() == "protocol_function_declaration" {
        flags |= FunctionMeta::FLAG_ABSTRACT;
    }

    // is_test: file category OR @Test attribute OR name starts with `test`.
    let has_test_attr = decorators.contains(&"Test".to_string());
    let name_is_test = raw.name.starts_with("test");
    let is_test = file_category == FileCategory::Test || has_test_attr || name_is_test;
    if is_test {
        flags |= FunctionMeta::FLAG_TEST;
    }

    flags |= vis_code << 6;

    let params = extract_params(fn_node, source);

    // Return type: `-> Type` field.
    let return_type = fn_node
        .child_by_field_name("return_type")
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

fn scan_swift_modifiers(
    mods: &Node<'_>,
    source: &[u8],
    flags: &mut u16,
    vis_code: &mut u16,
    decorators: &mut Vec<String>,
    is_extern: &mut bool,
) {
    // Swift `modifiers` is a `repeat1(choice($._non_local_scope_modifier, ...))`.
    // Each modifier is a typed wrapper node:
    //   `property_modifier`:  "static", "dynamic", "optional", "class", "distributed"
    //   `visibility_modifier`: "public", "private", "internal", "fileprivate", "open", "package"
    //   `member_modifier`:    "override", "convenience", "required", "nonisolated"
    //   `function_modifier`:  "infix", "postfix", "prefix"
    //   `attribute`:          @MainActor, @Sendable, etc.
    //   `inheritance_modifier`: "final"
    let mut c = mods.walk();
    if !c.goto_first_child() {
        return;
    }
    loop {
        let child = c.node();
        match child.kind() {
            "property_modifier" => {
                let txt = node_text(&child, source).trim();
                match txt {
                    "static" | "class" => *flags |= FunctionMeta::FLAG_STATIC,
                    _ => {}
                }
            }
            "visibility_modifier" => {
                let txt = node_text(&child, source);
                // visibility_modifier may include `(set)` qualifier: strip it.
                let base = txt.split('(').next().unwrap_or(txt).trim();
                *vis_code = match base {
                    "open" | "public" => 0,
                    "private" => 2,
                    "fileprivate" => 5,
                    "internal" => 3,
                    // default stays as-is (3 = internal)
                    _ => *vis_code,
                };
            }
            "attribute" => {
                let name = swift_attr_name(node_text(&child, source));
                if FFI_ATTRIBUTES.iter().any(|ffi| name.starts_with(ffi)) {
                    *is_extern = true;
                }
                if !name.is_empty() {
                    decorators.push(name);
                }
            }
            // member_modifier (override, convenience, required, nonisolated),
            // function_modifier (infix, postfix, prefix), inheritance_modifier (final):
            // not mapped to flags in our schema.
            _ => {}
        }
        if !c.goto_next_sibling() {
            break;
        }
    }
}

/// Strip `@` and `(...)` from a Swift attribute string.
fn swift_attr_name(s: &str) -> String {
    let s = s.trim().trim_start_matches('@');
    s.split('(').next().unwrap_or(s).trim().to_string()
}

/// Extract params from Swift function declaration — capture the internal name + type.
/// External labels (e.g. `to` in `to name: String`) are dropped intentionally.
///
/// In tree-sitter-swift, `_function_value_parameters` is a hidden inline rule that
/// directly produces `(`, `parameter`, `,`, `)` tokens as siblings of the function node
/// (not as children of a named wrapper node). We must recursively scan the subtree for
/// `parameter` nodes.
fn extract_params(fn_node: &Node<'_>, source: &[u8]) -> Vec<String> {
    let mut result = Vec::new();
    collect_swift_params(*fn_node, source, &mut result, 0);
    result
}

/// Recursively collect `parameter` nodes from the subtree, stopping at `function_body`.
fn collect_swift_params(node: Node<'_>, source: &[u8], result: &mut Vec<String>, depth: u32) {
    // Don't recurse into the function body.
    if node.kind() == "function_body" || node.kind() == "statements" {
        return;
    }
    if node.kind() == "parameter" {
        let (name, ty) = extract_swift_param(&node, source);
        if !name.is_empty() {
            result.push(name);
            result.push(ty);
        }
        return; // Don't recurse into the parameter node itself.
    }
    // Limit recursion depth to avoid traversing too deeply (body guard above handles most cases).
    if depth > 5 {
        return;
    }
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            collect_swift_params(c.node(), source, result, depth + 1);
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Extract (internal_name, type) from a Swift `parameter` node.
/// External label is dropped.
fn extract_swift_param(param: &Node<'_>, source: &[u8]) -> (String, String) {
    // Try named fields first.
    if let Some(name_node) = param.child_by_field_name("name") {
        let name = node_text(&name_node, source).to_string();
        let ty = param
            .child_by_field_name("type")
            .map(|n| node_text(&n, source).to_string())
            .unwrap_or_default();
        return (name, ty);
    }

    // Fallback: collect simple_identifier children. When there are two, the second
    // is the internal name; when there's one, it's both external and internal.
    let mut idents: Vec<String> = Vec::new();
    let mut ty = String::new();
    let mut c = param.walk();
    if c.goto_first_child() {
        loop {
            let child = c.node();
            match child.kind() {
                "simple_identifier" => idents.push(node_text(&child, source).to_string()),
                "user_type" | "optional_type" | "array_type" | "dictionary_type"
                | "function_type" | "tuple_type" | "opaque_type" | "some_type"
                | "type_identifier" => {
                    ty = node_text(&child, source).to_string();
                }
                "type_annotation" => {
                    // Strip the leading `:`.
                    ty = node_text(&child, source)
                        .trim_start_matches(':')
                        .trim()
                        .to_string();
                }
                _ => {}
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
    }

    // When two identifiers: first = external label, second = internal name.
    // When one: use it as the internal name.
    let name = match idents.len() {
        0 => String::new(),
        1 => idents.remove(0),
        _ => idents.remove(1),
    };
    (name, ty)
}
