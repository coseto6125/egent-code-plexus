//! Local-scope receiver type binding for Python.
//!
//! Collects simple-identifier type annotations on (a) typed parameters
//! `def f(x: T)` and (b) typed assignments `x: T = ...` inside function
//! bodies. The resulting [`ScopeMap`] is consulted during call extraction
//! so `var.method()` can be rewritten to `Type.method` for the resolver's
//! qualifier-scoped lookup (Tier 2.5).
//!
//! Scope: P0 only handles single-identifier receivers (`x.method`) with
//! single-identifier type annotations (`Apple`, not `dict[str, Apple]`).
//! Generic / subscripted / forward-reference types are skipped — the
//! call falls back to the bare member name as before.
//!
//! The scope-map data structure and lookup live in
//! [`crate::receiver_types`]; this file only contains the Python-specific
//! AST walk that populates one.

use crate::calls::attach_to_enclosing;
use crate::receiver_types::ScopeMap;
use graph_nexus_core::analyzer::types::RawNode;
use std::collections::HashMap;
use tree_sitter::Node;

/// Walk every `function_definition` node, collecting typed parameters and
/// annotated assignments inside the function body.
pub fn collect_local_types(root: Node<'_>, source: &[u8]) -> ScopeMap {
    let mut scopes = ScopeMap::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_definition" {
            let fn_span = (n.start_position().row as u32, n.end_position().row as u32);
            let mut map: HashMap<String, String> = HashMap::new();

            if let Some(params) = n.child_by_field_name("parameters") {
                collect_typed_params(params, source, &mut map);
            }

            if let Some(body) = n.child_by_field_name("body") {
                collect_typed_assignments(body, source, &mut map);
            }

            scopes.push(fn_span, map);
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    scopes
}

/// Extract `typed_parameter` children under a `parameters` node.
/// Tree-sitter-python shape: `typed_parameter` has the identifier as its
/// first named child and a `type` field for the annotation.
fn collect_typed_params(params: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut c = params.walk();
    for p in params.children(&mut c) {
        if p.kind() != "typed_parameter" {
            continue;
        }
        let Some(id) = p.named_child(0) else { continue };
        if id.kind() != "identifier" {
            continue;
        }
        let Some(ty_node) = p.child_by_field_name("type") else {
            continue;
        };
        if let Some((name, ty)) = simple_name_and_type(id, ty_node, source) {
            out.insert(name, ty);
        }
    }
}

/// Walk a function body for `assignment` nodes with a `type` field and a
/// simple-identifier `left`. Descends through compound statements so that
/// annotations inside `if`/`for`/`with` blocks are captured. Does NOT
/// descend into nested `function_definition` — those get their own scope.
fn collect_typed_assignments(body: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_definition" {
            continue;
        }
        if n.kind() == "assignment" {
            if let (Some(left), Some(ty_node)) =
                (n.child_by_field_name("left"), n.child_by_field_name("type"))
            {
                if left.kind() == "identifier" {
                    if let Some((name, ty)) = simple_name_and_type(left, ty_node, source) {
                        out.insert(name, ty);
                    }
                }
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

/// Extract `(name, type)` only when the type is a single identifier.
/// Generics / subscripts / strings are skipped — they cannot be matched
/// against class names by the resolver.
fn simple_name_and_type(
    name_node: Node<'_>,
    type_node: Node<'_>,
    source: &[u8],
) -> Option<(String, String)> {
    let inner = type_node.named_child(0).unwrap_or(type_node);
    if inner.kind() != "identifier" {
        return None;
    }
    let name = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;
    let ty = std::str::from_utf8(&source[inner.start_byte()..inner.end_byte()]).ok()?;
    if !ty.chars().all(|c| c.is_alphanumeric() || c == '_') || ty.is_empty() {
        return None;
    }
    Some((name.to_string(), ty.to_string()))
}

/// Walk the Python AST and attach callees to enclosing functions, with
/// receiver-type binding applied where annotations are known. Replaces
/// the shared `extract_calls` for Python: identifiers/attributes are
/// handled here; other call-target shapes (subscript, lambda, ...) emit
/// no edge, matching the previous catch-all behavior's "last identifier
/// segment" rule for those rare cases.
pub fn extract_python_calls(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    locals: &ScopeMap,
) {
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "call" {
            if let Some(callee) = python_callee_name(n, source, locals) {
                let line = n.start_position().row as u32;
                attach_to_enclosing(line, callee, nodes);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

fn python_callee_name(call: Node<'_>, source: &[u8], locals: &ScopeMap) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    match function.kind() {
        "identifier" => std::str::from_utf8(&source[function.start_byte()..function.end_byte()])
            .ok()
            .map(str::to_string),
        "attribute" => {
            let attr = function.child_by_field_name("attribute")?;
            let attr_name =
                std::str::from_utf8(&source[attr.start_byte()..attr.end_byte()]).ok()?;
            if let Some(obj) = function.child_by_field_name("object") {
                if obj.kind() == "identifier" {
                    let obj_name =
                        std::str::from_utf8(&source[obj.start_byte()..obj.end_byte()]).ok()?;
                    let line = call.start_position().row as u32;
                    if let Some(ty) = locals.lookup(line, obj_name) {
                        return Some(format!("{ty}.{attr_name}"));
                    }
                }
            }
            Some(attr_name.to_string())
        }
        _ => None,
    }
}
