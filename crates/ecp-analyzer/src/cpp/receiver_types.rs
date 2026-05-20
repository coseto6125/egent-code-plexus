//! Receiver-type binding for C++.
//!
//! Rewrites three call shapes so the resolver's qualifier-scoped lookup
//! (Tier 2.5) can route to the correct class:
//!
//! - `this->method()` / `this.method()` → `Class.method` where `Class` is
//!   the enclosing `class_specifier` / `struct_specifier`.
//! - `Base::method()` → `Base.method` (the shared `calls.rs` already
//!   captures the full qualifier; we preserve and normalize to `.` form).
//! - `obj.method()` / `obj->method()` → `Type.method` when `obj` is a
//!   parameter or local variable with a single-identifier declared type.
//!
//! Scope: P0 simple identifier types only. Templates, references, pointer
//! depth, namespace-qualified types, `auto`, and operator-overloaded calls
//! fall back to the bare member name.

use crate::calls::attach_to_enclosing;
use ecp_core::analyzer::types::RawNode;
use std::collections::HashMap;
use tree_sitter::Node;

#[derive(Debug, Default)]
pub struct CppBindings {
    fn_scopes: Vec<((u32, u32), HashMap<String, String>)>,
    class_scopes: Vec<((u32, u32), String)>,
}

impl CppBindings {
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

    fn enclosing_class(&self, line: u32) -> Option<&str> {
        let mut best: Option<&str> = None;
        let mut best_width = u32::MAX;
        for ((start, end), name) in &self.class_scopes {
            if *start <= line && line <= *end {
                let w = end - start;
                if w < best_width {
                    best_width = w;
                    best = Some(name.as_str());
                }
            }
        }
        best
    }
}

pub fn collect_bindings(root: Node<'_>, source: &[u8]) -> CppBindings {
    let mut fn_scopes: Vec<((u32, u32), HashMap<String, String>)> = Vec::new();
    let mut class_scopes: Vec<((u32, u32), String)> = Vec::new();

    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "class_specifier" | "struct_specifier" => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    if name_node.kind() == "type_identifier" {
                        if let Ok(s) = std::str::from_utf8(
                            &source[name_node.start_byte()..name_node.end_byte()],
                        ) {
                            let span = (n.start_position().row as u32, n.end_position().row as u32);
                            class_scopes.push((span, s.to_string()));
                        }
                    }
                }
            }
            "function_definition" => {
                let span = (n.start_position().row as u32, n.end_position().row as u32);
                let mut map: HashMap<String, String> = HashMap::new();
                collect_typed_cpp_params(n, source, &mut map);
                if let Some(body) = n.child_by_field_name("body") {
                    collect_typed_cpp_decls(body, source, &mut map);
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

    CppBindings {
        fn_scopes,
        class_scopes,
    }
}

/// Scan the function's declarator for `parameter_declaration` nodes.
/// A simple type_identifier + identifier (possibly wrapped in
/// reference_declarator / pointer_declarator) is recognized.
fn collect_typed_cpp_params(fn_def: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let Some(declarator) = fn_def.child_by_field_name("declarator") else {
        return;
    };
    let Some(fn_decl) = find_function_declarator(declarator) else {
        return;
    };
    let Some(params) = fn_decl.child_by_field_name("parameters") else {
        return;
    };
    let mut c = params.walk();
    for p in params.children(&mut c) {
        if p.kind() != "parameter_declaration" {
            continue;
        }
        let Some(ty_node) = p.child_by_field_name("type") else {
            continue;
        };
        if ty_node.kind() != "type_identifier" {
            continue;
        }
        let Ok(ty) = std::str::from_utf8(&source[ty_node.start_byte()..ty_node.end_byte()]) else {
            continue;
        };
        let Some(param_decl) = p.child_by_field_name("declarator") else {
            continue;
        };
        if let Some(name) = unwrap_declarator_identifier(param_decl, source) {
            out.insert(name, ty.to_string());
        }
    }
}

/// Walk the function body for `declaration` nodes with a single
/// `type_identifier` type and an `init_declarator`/declarator name.
fn collect_typed_cpp_decls(body: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_definition" {
            continue;
        }
        if n.kind() == "declaration" {
            if let Some(ty_node) = n.child_by_field_name("type") {
                if ty_node.kind() == "type_identifier" {
                    if let Ok(ty) =
                        std::str::from_utf8(&source[ty_node.start_byte()..ty_node.end_byte()])
                    {
                        // declarator field can be multiple init_declarator /
                        // pointer_declarator / identifier — walk siblings.
                        let mut c = n.walk();
                        for child in n.children(&mut c) {
                            if let Some(name) = unwrap_declarator_identifier(child, source) {
                                out.insert(name, ty.to_string());
                            }
                        }
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

/// Recursively unwrap declarator nodes to extract the bound identifier.
/// Handles `identifier`, `init_declarator`, `pointer_declarator`,
/// `reference_declarator`. Returns None for unrelated kinds.
fn unwrap_declarator_identifier(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
            .ok()
            .map(str::to_string),
        "init_declarator" | "pointer_declarator" | "reference_declarator" => {
            let inner = node.child_by_field_name("declarator")?;
            unwrap_declarator_identifier(inner, source)
        }
        _ => None,
    }
}

fn find_function_declarator<'a>(mut node: Node<'a>) -> Option<Node<'a>> {
    loop {
        match node.kind() {
            "function_declarator" => return Some(node),
            "pointer_declarator" | "reference_declarator" => {
                node = node.child_by_field_name("declarator")?;
            }
            _ => return None,
        }
    }
}

pub fn extract_cpp_calls(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    bindings: &CppBindings,
) {
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "call_expression" {
            if let Some(callee) = cpp_callee_name(n, source, bindings) {
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

fn cpp_callee_name(call: Node<'_>, source: &[u8], bindings: &CppBindings) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    let line = call.start_position().row as u32;

    match function.kind() {
        "identifier" => std::str::from_utf8(&source[function.start_byte()..function.end_byte()])
            .ok()
            .map(str::to_string),
        "field_expression" => {
            let field = function.child_by_field_name("field")?;
            let method = std::str::from_utf8(&source[field.start_byte()..field.end_byte()]).ok()?;
            let arg = function.child_by_field_name("argument")?;
            // `this->method()` / `this.method()`
            if arg.kind() == "this" {
                if let Some(cls) = bindings.enclosing_class(line) {
                    return Some(format!("{cls}.{method}"));
                }
            }
            // `obj.method()` — typed var lookup
            if arg.kind() == "identifier" {
                if let Ok(var) = std::str::from_utf8(&source[arg.start_byte()..arg.end_byte()]) {
                    if let Some(ty) = bindings.lookup_local(line, var) {
                        return Some(format!("{ty}.{method}"));
                    }
                }
            }
            // `(*p).method()` / `(&x).method()` — extract underlying identifier.
            if arg.kind() == "pointer_expression" {
                if let Some(inner) = arg.named_child(0) {
                    if inner.kind() == "identifier" {
                        if let Ok(var) =
                            std::str::from_utf8(&source[inner.start_byte()..inner.end_byte()])
                        {
                            if let Some(ty) = bindings.lookup_local(line, var) {
                                return Some(format!("{ty}.{method}"));
                            }
                        }
                    }
                }
            }
            Some(method.to_string())
        }
        "qualified_identifier" => {
            // `Base::method`. Pull the namespace_identifier + final name and
            // emit `Base.method` so the resolver's `.`-keyed lookup picks up
            // the qualifier scope.
            let mut c = function.walk();
            let children: Vec<_> = function.children(&mut c).collect();
            let scope_node = children
                .iter()
                .find(|ch| ch.kind() == "namespace_identifier");
            let name_node = function.child_by_field_name("name").or_else(|| {
                children
                    .iter()
                    .rev()
                    .find(|ch| {
                        matches!(
                            ch.kind(),
                            "identifier" | "field_identifier" | "destructor_name"
                        )
                    })
                    .copied()
            })?;
            let name =
                std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;
            if let Some(scope) = scope_node {
                if let Ok(scope_text) =
                    std::str::from_utf8(&source[scope.start_byte()..scope.end_byte()])
                {
                    return Some(format!("{scope_text}.{name}"));
                }
            }
            Some(name.to_string())
        }
        _ => std::str::from_utf8(&source[function.start_byte()..function.end_byte()])
            .ok()
            .map(str::to_string),
    }
}
