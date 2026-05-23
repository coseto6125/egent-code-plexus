//! Receiver-type binding for Kotlin call sites.
//!
//! Handles:
//! - `this.foo()` → receiver is the enclosing class/object name
//! - `super.foo()` → first superclass from the enclosing class's `heritage`
//! - `obj.foo()` where `obj` has a known type from local declarations/params
//! - Extension functions: declared as `fun ReceiverType.name()` — the parser
//!   captures them as top-level functions; no special handling needed here
//!   because their receiver type is part of the function name recorded from
//!   the query. The call site `receiver.name()` uses the same `obj.foo()`
//!   lookup path.
//!
//! Falls back to the bare method name for unresolved receivers.

use super::path_literals::build_raw_path_literal;
use crate::calls::attach_to_enclosing;
use ecp_core::analyzer::types::{RawNode, RawPathLiteral};
use ecp_core::graph::NodeKind;
use std::collections::HashMap;
use tree_sitter::Node;

#[derive(Debug, Default)]
struct LocalTypes {
    scopes: Vec<((u32, u32), HashMap<String, String>)>,
}

impl LocalTypes {
    fn lookup(&self, line: u32, var: &str) -> Option<&str> {
        let mut best: Option<&str> = None;
        let mut best_width = u32::MAX;
        for ((start, end), map) in &self.scopes {
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
}

/// Walk every `function_declaration`, collecting typed parameters and local
/// `property_declaration` nodes (which represent `val`/`var` declarations).
fn collect_local_types(root: Node<'_>, source: &[u8]) -> LocalTypes {
    let mut scopes: Vec<((u32, u32), HashMap<String, String>)> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_declaration" {
            let scope = (n.start_position().row as u32, n.end_position().row as u32);
            let mut map: HashMap<String, String> = HashMap::new();
            if let Some(params) = n.child_by_field_name("parameters") {
                collect_params(params, source, &mut map);
            }
            // Also handle function_value_parameters (alternative field name in tree-sitter-kotlin)
            let mut c = n.walk();
            for child in n.children(&mut c) {
                if child.kind() == "function_value_parameters" {
                    collect_params(child, source, &mut map);
                }
                if child.kind() == "function_body" {
                    collect_property_decls(child, source, &mut map);
                }
            }
            if !map.is_empty() {
                scopes.push((scope, map));
            }
            // Don't descend — nested lambdas form their own scopes.
            continue;
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    LocalTypes { scopes }
}

/// `function_value_parameters` → `parameter` with `simple_identifier` name
/// and `user_type` → `type_identifier` type (second named child).
fn collect_params(params: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut c = params.walk();
    for p in params.children(&mut c) {
        if p.kind() != "parameter" {
            continue;
        }
        let mut name_node = None;
        let mut type_node = None;
        let mut pc = p.walk();
        for child in p.children(&mut pc) {
            if child.kind() == "simple_identifier" && name_node.is_none() {
                name_node = Some(child);
            } else if child.kind() == "user_type" {
                type_node = Some(child);
            }
        }
        if let (Some(nm), Some(ty)) = (name_node, type_node) {
            if let Some((var, ty_s)) = simple_id_and_usertype(nm, ty, source) {
                out.insert(var, ty_s);
            }
        }
    }
}

/// Walk a `function_body`, collecting `property_declaration` nodes.
/// Does NOT descend into nested `function_declaration` / lambdas.
fn collect_property_decls(body: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_declaration" || n.kind() == "lambda_literal" {
            continue;
        }
        if n.kind() == "property_declaration" {
            // variable_declaration: first named child `simple_identifier`, second `user_type`
            let mut c = n.walk();
            for child in n.children(&mut c) {
                if child.kind() == "variable_declaration" {
                    let mut nm = None;
                    let mut ty = None;
                    let mut vc = child.walk();
                    for vc_child in child.children(&mut vc) {
                        match vc_child.kind() {
                            "simple_identifier" if nm.is_none() => nm = Some(vc_child),
                            "user_type" => ty = Some(vc_child),
                            _ => {}
                        }
                    }
                    if let (Some(nm_n), Some(ty_n)) = (nm, ty) {
                        if let Some((var, ty_s)) = simple_id_and_usertype(nm_n, ty_n, source) {
                            out.insert(var, ty_s);
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

fn simple_id_and_usertype(
    name_node: Node<'_>,
    type_node: Node<'_>,
    source: &[u8],
) -> Option<(String, String)> {
    if name_node.kind() != "simple_identifier" {
        return None;
    }
    // user_type → type_identifier (simple class name only)
    let ty_inner = type_node
        .named_child(0)
        .filter(|n| n.kind() == "type_identifier")
        .unwrap_or(type_node);
    if ty_inner.kind() != "type_identifier" {
        return None;
    }
    let name = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;
    let ty = std::str::from_utf8(&source[ty_inner.start_byte()..ty_inner.end_byte()]).ok()?;
    if ty.is_empty() {
        return None;
    }
    Some((name.to_string(), ty.to_string()))
}

/// Resolve a Kotlin `call_expression` callee, applying receiver-type binding.
///
/// Kotlin call shapes:
/// - Simple call `foo(...)` → function node, kind `simple_identifier`
/// - Member call `obj.foo(...)` → `navigation_expression` child with
///   `navigation_suffix` containing the method name
fn kotlin_callee(
    call: Node<'_>,
    source: &[u8],
    locals: &LocalTypes,
    nodes: &[RawNode],
) -> Option<String> {
    // First child of call_expression is either simple_identifier or navigation_expression.
    let callee = call.named_child(0)?;
    match callee.kind() {
        "simple_identifier" => callee.utf8_text(source).ok().map(|s| s.to_string()),
        "navigation_expression" => {
            // navigation_expression: receiver, then navigation_suffix(.method)
            let receiver = callee.named_child(0)?;
            let suffix = callee
                .named_children(&mut callee.walk())
                .find(|n| n.kind() == "navigation_suffix")?;
            let method_node = suffix
                .named_children(&mut suffix.walk())
                .find(|n| n.kind() == "simple_identifier")?;
            let method =
                std::str::from_utf8(&source[method_node.start_byte()..method_node.end_byte()])
                    .ok()?;

            let line = call.start_position().row as u32;
            let ty: Option<String> = match receiver.kind() {
                "this_expression" => enclosing_class_name(nodes, line),
                "super_expression" => enclosing_superclass(nodes, line),
                "simple_identifier" => {
                    let var =
                        std::str::from_utf8(&source[receiver.start_byte()..receiver.end_byte()])
                            .ok()?;
                    locals.lookup(line, var).map(|t| t.to_string())
                }
                _ => None,
            };
            Some(match ty {
                Some(t) => format!("{t}.{method}"),
                None => method.to_string(),
            })
        }
        _ => None,
    }
}

/// Walk the AST once, extracting Kotlin `call_expression` nodes with
/// receiver-type binding and collecting path-shaped string literals
/// (`string_literal` / `multiline_string_literal`, interpolated forms
/// filtered out in `build_raw_path_literal`).
pub fn extract_kotlin_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
) -> Vec<RawPathLiteral> {
    let local_types = collect_local_types(root, source);
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call_expression" => {
                if let Some(callee) = kotlin_callee(n, source, &local_types, nodes) {
                    let line = n.start_position().row as u32;
                    attach_to_enclosing(line, callee, nodes);
                }
            }
            "string_literal" | "multiline_string_literal" => {
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

fn enclosing_class_name(nodes: &[RawNode], line: u32) -> Option<String> {
    let mut best: Option<(u32, &str)> = None;
    for n in nodes {
        if !matches!(n.kind, NodeKind::Class) {
            continue;
        }
        if n.span.0 <= line && n.span.2 >= line {
            let w = n.span.2 - n.span.0;
            if best.is_none_or(|(bw, _)| w < bw) {
                best = Some((w, &n.name));
            }
        }
    }
    best.map(|(_, name)| name.to_string())
}

fn enclosing_superclass(nodes: &[RawNode], line: u32) -> Option<String> {
    let mut best_w = u32::MAX;
    let mut result = None;
    for n in nodes {
        if !matches!(n.kind, NodeKind::Class) {
            continue;
        }
        if n.span.0 <= line && n.span.2 >= line {
            let w = n.span.2 - n.span.0;
            if w < best_w {
                best_w = w;
                result = n.heritage.first().cloned();
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::super::parser::KotlinProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    fn parse(src: &str) -> ecp_core::analyzer::types::LocalGraph {
        let provider = KotlinProvider::new().expect("KotlinProvider::new");
        provider
            .parse_file(Path::new("Test.kt"), src.as_bytes())
            .expect("parse_file")
    }

    /// `this.eat()` inside a Kotlin class should be recorded as `Animal.eat`.
    #[test]
    fn kotlin_ctor_this_receiver_binds_class_name() {
        let src = r#"
class Animal {
    fun breathe() {
        this.eat()
    }
    fun eat() {}
}
"#;
        let graph = parse(src);
        let breathe = graph
            .nodes
            .iter()
            .find(|n| n.name == "breathe")
            .expect("breathe function not found");
        assert!(
            breathe.calls.iter().any(|c| c == "Animal.eat"),
            "expected 'Animal.eat' in calls, got {:?}",
            breathe.calls
        );
    }

    /// `super.init()` should bind to the first superclass in heritage.
    #[test]
    fn kotlin_ctor_super_receiver_binds_superclass() {
        let src = r#"
class Dog : Animal() {
    fun setup() {
        super.init()
    }
    fun init() {}
}
"#;
        let graph = parse(src);
        let setup = graph
            .nodes
            .iter()
            .find(|n| n.name == "setup")
            .expect("setup function not found");
        assert!(
            setup.calls.iter().any(|c| c == "Animal.init"),
            "expected 'Animal.init' in calls, got {:?}",
            setup.calls
        );
    }

    /// Typed `val obj: MyService` → `obj.run()` should yield `MyService.run`.
    #[test]
    fn kotlin_ctor_typed_variable_receiver_binds_declared_type() {
        let src = r#"
class App {
    fun start() {
        val svc: MyService = MyService()
        svc.run()
    }
}
"#;
        let graph = parse(src);
        let start = graph
            .nodes
            .iter()
            .find(|n| n.name == "start")
            .expect("start function not found");
        assert!(
            start.calls.iter().any(|c| c == "MyService.run"),
            "expected 'MyService.run' in calls, got {:?}",
            start.calls
        );
    }

    /// Regression: `super.foo()` in a class WITHOUT base must emit bare `"foo"`,
    /// not a synthetic `"super.foo"` that pollutes the graph.
    #[test]
    fn kotlin_super_no_base_emits_bare_method() {
        let src = r#"
class Standalone {
    fun doWork() {
        super.foo()
    }
}
"#;
        let graph = parse(src);
        let do_work = graph
            .nodes
            .iter()
            .find(|n| n.name == "doWork")
            .expect("doWork function not found");
        assert!(
            do_work.calls.iter().any(|c| c == "foo"),
            "expected bare 'foo' in calls, got {:?}",
            do_work.calls
        );
        assert!(
            !do_work.calls.iter().any(|c| c == "super.foo"),
            "must NOT contain synthetic 'super.foo', got {:?}",
            do_work.calls
        );
    }
}
