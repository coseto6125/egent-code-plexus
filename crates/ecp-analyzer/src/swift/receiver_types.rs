//! Receiver-type binding for Swift.
//!
//! Rewrites `self.method()`, `super.method()`, and `var.method()` callees
//! to `Type.method` form so the resolver's qualifier-scoped lookup (Tier 2.5)
//! can route to the correct class. Three sources of receiver type:
//!
//! - `self` inside a `class_declaration` body → enclosing class/struct/extension
//!   target type (tree-sitter-swift unifies these three under `class_declaration`).
//! - `super` inside a class with `inheritance_specifier` → first heritage entry.
//! - Local identifier with a known type from a typed `parameter` or a
//!   `property_declaration` with `type_annotation` whose annotation is a
//!   single `type_identifier`.
//!
//! Scope: P0 only handles simple-identifier types. Generic / optional /
//! tuple / function types are skipped — the call falls back to the bare
//! member name as before.

use super::path_literals::build_raw_path_literal;
use crate::calls::attach_to_enclosing;
use ecp_core::analyzer::types::{RawNode, RawPathLiteral};
use std::collections::HashMap;
use tree_sitter::Node;

/// Per-function-scope local var → type map, plus the enclosing class /
/// super-class lookup for `self` / `super`. Scope is keyed by row-span so
/// nested closures inherit outer-scope types via smallest-containing-scope.
// `(start_line, end_line)` half-open span keyed scope tables.
type FnScope = ((u32, u32), HashMap<String, String>);
type ClassScope = ((u32, u32), (String, Option<String>));

#[derive(Debug, Default)]
pub struct SwiftBindings {
    /// Function/method scopes: (span_start, span_end, var → type).
    fn_scopes: Vec<FnScope>,
    /// Class/struct/extension scopes: (span_start, span_end, (type_name, superclass_opt)).
    class_scopes: Vec<ClassScope>,
}

impl SwiftBindings {
    fn lookup_local(&self, line: u32, var: &str) -> Option<&str> {
        let mut best: Option<&str> = None;
        let mut best_width = u32::MAX;
        for ((start, end), map) in &self.fn_scopes {
            if *start <= line && line <= *end {
                if let Some(t) = map.get(var) {
                    let w = end - start;
                    if w < best_width {
                        best_width = w;
                        best = Some(t.as_str());
                    }
                }
            }
        }
        best
    }

    fn enclosing_class(&self, line: u32) -> Option<&(String, Option<String>)> {
        let mut best: Option<&(String, Option<String>)> = None;
        let mut best_width = u32::MAX;
        for ((start, end), info) in &self.class_scopes {
            if *start <= line && line <= *end {
                let w = end - start;
                if w < best_width {
                    best_width = w;
                    best = Some(info);
                }
            }
        }
        best
    }
}

/// Walk the AST collecting class scopes and per-function local-type maps.
pub fn collect_bindings(root: Node<'_>, source: &[u8]) -> SwiftBindings {
    let mut fn_scopes: Vec<FnScope> = Vec::new();
    let mut class_scopes: Vec<ClassScope> = Vec::new();

    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "class_declaration" => {
                // Covers class / struct / enum / actor / extension in tree-sitter-swift.
                if let Some((name, sup)) = swift_class_name_and_super(n, source) {
                    let span = (n.start_position().row as u32, n.end_position().row as u32);
                    class_scopes.push((span, (name, sup)));
                }
            }
            "function_declaration" | "init_declaration" => {
                let span = (n.start_position().row as u32, n.end_position().row as u32);
                let mut map: HashMap<String, String> = HashMap::new();
                collect_typed_swift_params(n, source, &mut map);
                if let Some(body) = n.child_by_field_name("body") {
                    collect_typed_swift_properties(body, source, &mut map);
                }
                if !map.is_empty() {
                    fn_scopes.push((span, map));
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }

    SwiftBindings {
        fn_scopes,
        class_scopes,
    }
}

/// Extract the type-name and (optional) first heritage entry from a Swift
/// `class_declaration`. Returns None if the name isn't a simple identifier.
fn swift_class_name_and_super(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    let mut name: Option<String> = None;
    let mut sup: Option<String> = None;
    let mut c = node.walk();
    for child in node.children(&mut c) {
        match child.kind() {
            "type_identifier" if name.is_none() => {
                if let Ok(s) = std::str::from_utf8(&source[child.start_byte()..child.end_byte()]) {
                    name = Some(s.to_string());
                }
            }
            "user_type" if name.is_none() => {
                // Extension: `extension Apple { ... }` puts name inside user_type.
                if let Some(ti) = child.named_child(0) {
                    if ti.kind() == "type_identifier" {
                        if let Ok(s) = std::str::from_utf8(&source[ti.start_byte()..ti.end_byte()])
                        {
                            name = Some(s.to_string());
                        }
                    }
                }
            }
            "inheritance_specifier" if sup.is_none() => {
                // First named child is the parent type. Extract a type_identifier
                // from the user_type wrapper.
                let mut cc = child.walk();
                for sub in child.children(&mut cc) {
                    if sub.kind() == "user_type" {
                        if let Some(ti) = sub.named_child(0) {
                            if ti.kind() == "type_identifier" {
                                if let Ok(s) =
                                    std::str::from_utf8(&source[ti.start_byte()..ti.end_byte()])
                                {
                                    sup = Some(s.to_string());
                                    break;
                                }
                            }
                        }
                    } else if sub.kind() == "type_identifier" {
                        if let Ok(s) =
                            std::str::from_utf8(&source[sub.start_byte()..sub.end_byte()])
                        {
                            sup = Some(s.to_string());
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    name.map(|n| (n, sup))
}

/// Walk the function's children for `parameter` nodes. Swift `parameter`
/// shape: `simple_identifier (name) : user_type/type_identifier (type)`.
fn collect_typed_swift_params(fn_node: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![fn_node];
    while let Some(n) = stack.pop() {
        // Don't descend into the function body — params live before it.
        if n.kind() == "function_body" {
            continue;
        }
        if n.kind() == "parameter" {
            let mut id: Option<&str> = None;
            let mut ty: Option<&str> = None;
            let mut c = n.walk();
            for child in n.children(&mut c) {
                match child.kind() {
                    "simple_identifier" if id.is_none() => {
                        id =
                            std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok();
                    }
                    "user_type" if ty.is_none() => {
                        if let Some(ti) = child.named_child(0) {
                            if ti.kind() == "type_identifier" {
                                ty = std::str::from_utf8(&source[ti.start_byte()..ti.end_byte()])
                                    .ok();
                            }
                        }
                    }
                    "type_identifier" if ty.is_none() => {
                        ty =
                            std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok();
                    }
                    _ => {}
                }
            }
            if let (Some(name), Some(t)) = (id, ty) {
                if is_simple_ident(t) {
                    out.insert(name.to_string(), t.to_string());
                }
            }
            continue;
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

/// Walk a function body for `property_declaration` (let/var) with a
/// `type_annotation`. Skip nested functions — they get their own scope.
fn collect_typed_swift_properties(
    body: Node<'_>,
    source: &[u8],
    out: &mut HashMap<String, String>,
) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_declaration" || n.kind() == "init_declaration" {
            continue;
        }
        if n.kind() == "property_declaration" {
            let mut name: Option<String> = None;
            let mut ty: Option<String> = None;
            let mut c = n.walk();
            for child in n.children(&mut c) {
                match child.kind() {
                    "pattern" if name.is_none() => {
                        if let Some(ident) = child.named_child(0) {
                            if ident.kind() == "simple_identifier" {
                                if let Ok(s) = std::str::from_utf8(
                                    &source[ident.start_byte()..ident.end_byte()],
                                ) {
                                    name = Some(s.to_string());
                                }
                            }
                        }
                    }
                    "type_annotation" if ty.is_none() => {
                        let mut cc = child.walk();
                        for sub in child.children(&mut cc) {
                            if sub.kind() == "user_type" {
                                if let Some(ti) = sub.named_child(0) {
                                    if ti.kind() == "type_identifier" {
                                        if let Ok(s) = std::str::from_utf8(
                                            &source[ti.start_byte()..ti.end_byte()],
                                        ) {
                                            ty = Some(s.to_string());
                                            break;
                                        }
                                    }
                                }
                            } else if sub.kind() == "type_identifier" {
                                if let Ok(s) =
                                    std::str::from_utf8(&source[sub.start_byte()..sub.end_byte()])
                                {
                                    ty = Some(s.to_string());
                                    break;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if let (Some(n), Some(t)) = (name, ty) {
                if is_simple_ident(&t) {
                    out.insert(n, t);
                }
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

fn is_simple_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// First named child of `node` whose kind satisfies `pred`. Uses a fresh
/// cursor that is dropped before the returned node — avoids borrow-checker
/// lifetime tangles where the cursor would outlive the calling scope.
fn first_named_where<'a, F>(node: Node<'a>, pred: F) -> Option<Node<'a>>
where
    F: Fn(&Node<'a>) -> bool,
{
    let mut c = node.walk();
    let result = node.named_children(&mut c).find(|child| pred(child));
    result
}

/// First child (named or not) whose kind equals `kind`.
fn first_child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut c = node.walk();
    let result = node.children(&mut c).find(|child| child.kind() == kind);
    result
}

/// Walk the Swift AST once, attaching call sites to enclosing functions
/// (with receiver binding for `self.` / `super.` / typed var navigation)
/// and collecting path-shaped string literals (`line_string_literal`,
/// `multi_line_string_literal`, `raw_string_literal`).
pub fn extract_swift_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    bindings: &SwiftBindings,
) -> Vec<RawPathLiteral> {
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call_expression" => {
                if let Some(callee) = swift_callee_name(n, source, bindings) {
                    let line = n.start_position().row as u32;
                    attach_to_enclosing(line, callee, nodes);
                }
            }
            "line_string_literal" | "multi_line_string_literal" | "raw_string_literal" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    path_literals
}

fn swift_callee_name(call: Node<'_>, source: &[u8], bindings: &SwiftBindings) -> Option<String> {
    // The callee is the first non-call-suffix named child of `call_expression`.
    let target = first_named_where(call, |ch| ch.kind() != "call_suffix")?;
    let line = call.start_position().row as u32;

    match target.kind() {
        "simple_identifier" => std::str::from_utf8(&source[target.start_byte()..target.end_byte()])
            .ok()
            .map(str::to_string),
        "navigation_expression" => {
            // Extract suffix name: navigation_suffix > simple_identifier
            let suffix = target
                .child_by_field_name("suffix")
                .or_else(|| first_child_of_kind(target, "navigation_suffix"))?;
            let suffix_id = first_child_of_kind(suffix, "simple_identifier")?;
            let method =
                std::str::from_utf8(&source[suffix_id.start_byte()..suffix_id.end_byte()]).ok()?;

            // Determine the receiver: the first named target child of nav_expr
            // (everything before the navigation_suffix).
            let receiver = first_named_where(target, |ch| ch.kind() != "navigation_suffix");

            if let Some(recv) = receiver {
                match recv.kind() {
                    "self_expression" => {
                        if let Some((cls, _)) = bindings.enclosing_class(line) {
                            return Some(format!("{cls}.{method}"));
                        }
                    }
                    "super_expression" => {
                        if let Some((_, Some(sup))) = bindings.enclosing_class(line) {
                            return Some(format!("{sup}.{method}"));
                        }
                    }
                    "simple_identifier" => {
                        if let Ok(var) =
                            std::str::from_utf8(&source[recv.start_byte()..recv.end_byte()])
                        {
                            if let Some(ty) = bindings.lookup_local(line, var) {
                                return Some(format!("{ty}.{method}"));
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some(method.to_string())
        }
        _ => None,
    }
}
