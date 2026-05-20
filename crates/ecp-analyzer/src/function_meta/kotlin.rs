//! Kotlin FunctionMeta extraction.
//!
//! Rules:
//! - `is_async`:     `suspend` modifier (Kotlin coroutine marker — semantically async)
//! - `is_static`:    top-level functions (no enclosing class) OR `@JvmStatic` annotation
//! - `is_abstract`:  `abstract` modifier OR interface function without body
//! - `is_generator`: never (sequence builder is library, not language)
//! - `is_extern`:    `external` modifier
//! - `is_test`:      file category Test OR `@Test` annotation OR file matches *Test.kt/*Spec.kt
//! - `visibility`:   `public` (default) → 0, `protected` → 1, `private` → 2, `internal` → 3
//! - `params`:       `name: type` pairs from `parameter` nodes
//! - `return_type`:  `: ReturnType` after `)`, absent → empty
//! - `decorators`:   annotation names with `@` stripped, args `(...)` dropped

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

const KOTLIN_FN_KINDS: &[&str] = &["function_declaration", "secondary_constructor"];

fn collect_fn_nodes<'a>(
    node: Node<'a>,
    source: &[u8],
    fn_spans: &[FnSpan<'a>],
    file_category: FileCategory,
    out: &mut Vec<RawFunctionMeta>,
) {
    let k = node.kind();
    if KOTLIN_FN_KINDS.contains(&k) {
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
    let mut vis_code: u16 = 0; // Kotlin default is public

    // Collect modifiers and annotations.
    // In tree-sitter-kotlin, `modifiers` is a direct child of function_declaration
    // (not a named field). Inside `modifiers`, individual modifiers are wrapped in
    // typed nodes like `function_modifier`, `visibility_modifier`, `member_modifier`,
    // `annotation`, `multiplatform_modifier`, etc. The text of each modifier is a
    // leaf keyword inside these wrappers.
    let mut decorators: Vec<String> = Vec::new();
    let mut has_jvm_static = false;

    {
        let mut c = fn_node.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                if child.kind() == "modifiers" {
                    scan_kotlin_modifiers(
                        &child,
                        source,
                        &mut flags,
                        &mut vis_code,
                        &mut decorators,
                    );
                    if decorators.contains(&"JvmStatic".to_string()) {
                        has_jvm_static = true;
                    }
                    break;
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    // Merge query-captured decorators (already stripped by query capture).
    for d in &raw.decorators {
        let name = kotlin_annotation_name(d);
        if !name.is_empty() && !decorators.contains(&name) {
            if name == "JvmStatic" {
                has_jvm_static = true;
            }
            decorators.push(name);
        }
    }

    // is_static: top-level function (parent is source_file or kotlin_file)
    // OR @JvmStatic annotation.
    let is_top_level = fn_node
        .parent()
        .map(|p| matches!(p.kind(), "source_file" | "kotlin_file"))
        .unwrap_or(false);
    if is_top_level || has_jvm_static {
        flags |= FunctionMeta::FLAG_STATIC;
    }

    // Abstract: check for function without body when parent is interface.
    // The `abstract` keyword is already handled in scan_kotlin_modifiers.
    if fn_node.child_by_field_name("body").is_none() {
        // Check if parent chain leads to an interface declaration.
        let in_interface = fn_node
            .parent()
            .and_then(|p| {
                if p.kind() == "class_body" {
                    p.parent()
                } else {
                    None
                }
            })
            .map(|gp| {
                // Check for `interface` keyword among direct children.
                let mut gc = gp.walk();
                if gc.goto_first_child() {
                    loop {
                        if gc.node().kind() == "interface" {
                            return true;
                        }
                        if !gc.goto_next_sibling() {
                            break;
                        }
                    }
                }
                false
            })
            .unwrap_or(false);
        if in_interface {
            flags |= FunctionMeta::FLAG_ABSTRACT;
        }
    }

    // is_test: file category OR @Test annotation.
    let is_test = file_category == FileCategory::Test || decorators.contains(&"Test".to_string());
    if is_test {
        flags |= FunctionMeta::FLAG_TEST;
    }

    flags |= vis_code << 6;

    // Parameters: field name is `value_parameters` in Kotlin grammar.
    let params = extract_params(fn_node, source);

    // Return type: in tree-sitter-kotlin the return type annotation `optional(seq(":", $._type))`
    // is a direct child of function_declaration that comes AFTER `function_value_parameters`.
    // The type appears as a `type_reference`, `nullable_type`, or `user_type` node.
    // No named field is defined for it in the 0.3.x / 0.4.x grammar, so we scan
    // direct children after the `function_value_parameters` node.
    let return_type = fn_node
        .child_by_field_name("type")
        .map(|n| node_text(&n, source).to_string())
        .or_else(|| {
            let mut past_params = false;
            let mut ret = None;
            let mut c = fn_node.walk();
            if c.goto_first_child() {
                loop {
                    let child = c.node();
                    if child.kind() == "function_value_parameters" {
                        past_params = true;
                    }
                    if past_params
                        && matches!(
                            child.kind(),
                            "type_reference" | "nullable_type" | "user_type"
                        )
                    {
                        ret = Some(node_text(&child, source).to_string());
                        break;
                    }
                    // Stop at function_body to avoid scanning inside the body.
                    if child.kind() == "function_body" {
                        break;
                    }
                    if !c.goto_next_sibling() {
                        break;
                    }
                }
            }
            ret
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

fn scan_kotlin_modifiers(
    mods: &Node<'_>,
    source: &[u8],
    flags: &mut u16,
    vis_code: &mut u16,
    decorators: &mut Vec<String>,
) {
    // In tree-sitter-kotlin, `modifiers` contains typed modifier wrapper nodes.
    // Grammar (0.3.8 / 0.4.0):
    //   function_modifier:    tailrec, operator, infix, inline, external, suspend
    //   visibility_modifier:  public, private, internal, protected
    //   member_modifier:      override, lateinit
    //   inheritance_modifier: abstract, final, open
    //   multiplatform_modifier / platform_modifier: expect, actual
    //   annotation:           @Foo / @Foo(args)
    //
    // Each typed wrapper node is a leaf whose text is the keyword itself.
    let mut c = mods.walk();
    if !c.goto_first_child() {
        return;
    }
    loop {
        let child = c.node();
        let txt = node_text(&child, source).trim();
        match child.kind() {
            "function_modifier" => match txt {
                "suspend" => *flags |= FunctionMeta::FLAG_ASYNC,
                "external" => *flags |= FunctionMeta::FLAG_EXTERN,
                _ => {}
            },
            "visibility_modifier" => match txt {
                "public" => *vis_code = 0,
                "protected" => *vis_code = 1,
                "private" => *vis_code = 2,
                "internal" => *vis_code = 3,
                _ => {}
            },
            "member_modifier" => {} // override, lateinit — not mapped to flags
            "inheritance_modifier" if txt == "abstract" => {
                *flags |= FunctionMeta::FLAG_ABSTRACT;
            }
            "annotation" | "single_annotation" | "multi_annotation" => {
                let name = extract_kotlin_annotation_name(&child, source);
                if !name.is_empty() {
                    decorators.push(name);
                }
            }
            _ => {}
        }
        if !c.goto_next_sibling() {
            break;
        }
    }
}

/// Extract the annotation name from an `annotation` or `single_annotation` node.
fn extract_kotlin_annotation_name(node: &Node<'_>, source: &[u8]) -> String {
    // The annotation text may include `@` and args; strip them.
    let txt = node_text(node, source);
    kotlin_annotation_name(txt)
}

/// Strip `@` prefix and any `(...)` suffix from an annotation string.
fn kotlin_annotation_name(s: &str) -> String {
    let s = s.trim().trim_start_matches('@');
    s.split('(').next().unwrap_or(s).trim().to_string()
}

fn extract_params(fn_node: &Node<'_>, source: &[u8]) -> Vec<String> {
    // Kotlin uses `value_parameters` as the field name for the parameter list.
    // Fallback: scan children for a node of kind `function_value_parameters`.
    let params_node = fn_node
        .child_by_field_name("value_parameters")
        .or_else(|| find_child_kind(fn_node, "function_value_parameters"));

    let Some(params_node) = params_node else {
        return vec![];
    };
    let mut result = Vec::new();
    let mut cursor = params_node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if matches!(child.kind(), "function_value_parameter" | "parameter") {
                // `name: Type` — fields: `simple_identifier` and `type`.
                // In Kotlin grammar, `function_value_parameter` contains a `parameter`
                // which in turn has `simple_identifier` and `type` children.
                let (name, ty) = extract_kotlin_param(&child, source);
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

fn extract_kotlin_param(param: &Node<'_>, source: &[u8]) -> (String, String) {
    // Direct parameter: `simple_identifier : type`.
    // First try the inner `parameter` child if this is a `function_value_parameter`.
    if param.kind() == "function_value_parameter" {
        if let Some(inner) = find_child_kind(param, "parameter") {
            return extract_kotlin_param(&inner, source);
        }
    }
    let name = find_child_kind(param, "simple_identifier")
        .map(|n| node_text(&n, source).to_string())
        .unwrap_or_default();
    let ty = find_child_kind(param, "type_reference")
        .or_else(|| find_child_kind(param, "nullable_type"))
        .or_else(|| find_child_kind(param, "user_type"))
        .or_else(|| param.child_by_field_name("type"))
        .map(|n| node_text(&n, source).to_string())
        .unwrap_or_default();
    (name, ty)
}

fn find_child_kind<'a>(node: &Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut c = node.walk();
    if !c.goto_first_child() {
        return None;
    }
    loop {
        let child = c.node();
        if child.kind() == kind {
            return Some(child);
        }
        if !c.goto_next_sibling() {
            break;
        }
    }
    None
}
