//! Receiver-type binding for Dart.
//!
//! Rewrites three call shapes so the resolver's qualifier-scoped lookup
//! (Tier 2.5) can route to the correct class:
//!
//! - `this.method()` → `Class.method` where `Class` is the enclosing
//!   `class_declaration` / `mixin_declaration`.
//! - `super.method()` → `Super.method` from the class's `superclass`
//!   field's first type identifier.
//! - `obj.method()` where `obj` is a `formal_parameter` or
//!   `initialized_variable_definition` with a single `type_identifier`
//!   annotation → `Type.method`.
//!
//! Scope: P0 simple identifier types only. Generic / function / nullable
//! types fall back to the bare member name.

use crate::calls::attach_to_enclosing;
use ecp_core::analyzer::types::RawNode;
use std::collections::HashMap;
use tree_sitter::Node;

// `(start_line, end_line)` half-open span keyed scope tables. The tuple-of-
// tuples shape mirrors the rest of the receiver_types modules; the type aliases
// silence `clippy::type_complexity` without changing behaviour.
type FnScope = ((u32, u32), HashMap<String, String>);
type ClassScope = ((u32, u32), (String, Option<String>));

#[derive(Debug, Default)]
pub struct DartBindings {
    fn_scopes: Vec<FnScope>,
    class_scopes: Vec<ClassScope>,
}

impl DartBindings {
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

pub fn collect_bindings(root: Node<'_>, source: &[u8]) -> DartBindings {
    let mut fn_scopes: Vec<FnScope> = Vec::new();
    let mut class_scopes: Vec<ClassScope> = Vec::new();

    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "class_declaration" | "mixin_declaration" => {
                if let Some((name, sup)) = dart_class_name_and_super(n, source) {
                    let span = (n.start_position().row as u32, n.end_position().row as u32);
                    class_scopes.push((span, (name, sup)));
                }
            }
            "method_declaration" | "function_declaration" => {
                let span = (n.start_position().row as u32, n.end_position().row as u32);
                let mut map: HashMap<String, String> = HashMap::new();
                collect_typed_dart_params(n, source, &mut map);
                collect_typed_dart_locals(n, source, &mut map);
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

    DartBindings {
        fn_scopes,
        class_scopes,
    }
}

fn dart_class_name_and_super(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    let name_node = node.child_by_field_name("name")?;
    if name_node.kind() != "identifier" {
        return None;
    }
    let name = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;

    let sup = node
        .child_by_field_name("superclass")
        .and_then(|sc| {
            let mut c = sc.walk();
            let mut found: Option<Node<'_>> = None;
            for child in sc.children(&mut c) {
                if child.kind() == "type" {
                    found = Some(child);
                    break;
                }
            }
            drop(c);
            found
        })
        .and_then(|t| t.named_child(0))
        .and_then(|ti| {
            if ti.kind() == "type_identifier" {
                std::str::from_utf8(&source[ti.start_byte()..ti.end_byte()])
                    .ok()
                    .map(str::to_string)
            } else {
                None
            }
        });

    Some((name.to_string(), sup))
}

fn collect_typed_dart_params(fn_node: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![fn_node];
    while let Some(n) = stack.pop() {
        // Avoid descending into nested fn bodies — they get their own scope.
        if n.kind() == "function_body" {
            continue;
        }
        if n.kind() == "formal_parameter" {
            let mut ty: Option<String> = None;
            let mut name: Option<String> = None;
            let mut c = n.walk();
            for child in n.children(&mut c) {
                if child.kind() == "type" && ty.is_none() {
                    if let Some(ti) = child.named_child(0) {
                        if ti.kind() == "type_identifier" {
                            if let Ok(s) =
                                std::str::from_utf8(&source[ti.start_byte()..ti.end_byte()])
                            {
                                ty = Some(s.to_string());
                            }
                        }
                    }
                } else if child.kind() == "identifier" && name.is_none() {
                    if let Ok(s) =
                        std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                    {
                        name = Some(s.to_string());
                    }
                }
            }
            if let (Some(n), Some(t)) = (name, ty) {
                out.insert(n, t);
            }
            continue;
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

fn collect_typed_dart_locals(fn_node: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![fn_node];
    while let Some(n) = stack.pop() {
        if (n.kind() == "method_declaration" || n.kind() == "function_declaration")
            && !std::ptr::eq(n.id() as *const u8, fn_node.id() as *const u8)
        {
            continue;
        }
        if n.kind() == "initialized_variable_definition" {
            let mut ty: Option<String> = None;
            let mut name: Option<String> = None;
            let mut c = n.walk();
            for child in n.children(&mut c) {
                if child.kind() == "type" && ty.is_none() {
                    if let Some(ti) = child.named_child(0) {
                        if ti.kind() == "type_identifier" {
                            if let Ok(s) =
                                std::str::from_utf8(&source[ti.start_byte()..ti.end_byte()])
                            {
                                ty = Some(s.to_string());
                            }
                        }
                    }
                } else if child.kind() == "identifier" && name.is_none() {
                    if let Ok(s) =
                        std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                    {
                        name = Some(s.to_string());
                    }
                }
            }
            if let (Some(n), Some(t)) = (name, ty) {
                out.insert(n, t);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

pub fn extract_dart_calls(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    bindings: &DartBindings,
) {
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "call_expression" {
            if let Some(callee) = dart_callee_name(n, source, bindings) {
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

fn dart_callee_name(call: Node<'_>, source: &[u8], bindings: &DartBindings) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    let line = call.start_position().row as u32;

    match function.kind() {
        "identifier" => std::str::from_utf8(&source[function.start_byte()..function.end_byte()])
            .ok()
            .map(str::to_string),
        "member_expression" => {
            let prop = function.child_by_field_name("property")?;
            let method = std::str::from_utf8(&source[prop.start_byte()..prop.end_byte()]).ok()?;
            let obj = function.child_by_field_name("object")?;
            match obj.kind() {
                "this" => {
                    if let Some((cls, _)) = bindings.enclosing_class(line) {
                        return Some(format!("{cls}.{method}"));
                    }
                }
                "super" => {
                    if let Some((_, Some(sup))) = bindings.enclosing_class(line) {
                        return Some(format!("{sup}.{method}"));
                    }
                }
                "identifier" => {
                    if let Ok(var) = std::str::from_utf8(&source[obj.start_byte()..obj.end_byte()])
                    {
                        if let Some(ty) = bindings.lookup_local(line, var) {
                            return Some(format!("{ty}.{method}"));
                        }
                    }
                }
                _ => {}
            }
            Some(method.to_string())
        }
        _ => std::str::from_utf8(&source[function.start_byte()..function.end_byte()])
            .ok()
            .map(str::to_string),
    }
}
