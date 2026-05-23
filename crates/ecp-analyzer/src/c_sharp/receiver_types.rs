//! Receiver-type binding for C# invocation sites.
//!
//! Handles:
//! - `this.Foo()` → receiver is the enclosing class/struct name
//! - `base.Foo()` → first base type from the enclosing class's `heritage`
//! - `obj.Foo()` where `obj` has a known type from local variable declarations
//!   or typed method parameters
//!
//! Falls back to bare method name when the receiver type cannot be resolved.
//!
//! C# AST shape (tree-sitter-c-sharp):
//! - `invocation_expression` with `member_access_expression` as `function` field
//! - `member_access_expression`:
//!   - `this` keyword node → `this_expression` or bare `this`
//!   - `base` keyword node → bare `base`
//!   - `identifier` node → variable receiver
//!   - `name` field holds the method identifier
//! - Local variable: `local_declaration_statement` → `variable_declaration`
//!   - First named child: type (`identifier` / `predefined_type`)
//!   - `variable_declarator` child with `name` field (`identifier`)

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

/// Walk every method/constructor/local-function declaration, collecting typed
/// parameters and local variable declarations.
fn collect_local_types(root: Node<'_>, source: &[u8]) -> LocalTypes {
    let mut scopes: Vec<((u32, u32), HashMap<String, String>)> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "method_declaration" | "constructor_declaration" | "local_function_statement" => {
                let scope = (n.start_position().row as u32, n.end_position().row as u32);
                let mut map: HashMap<String, String> = HashMap::new();
                if let Some(params) = n.child_by_field_name("parameters") {
                    collect_params(params, source, &mut map);
                }
                if let Some(body) = n.child_by_field_name("body") {
                    collect_local_vars(body, source, &mut map);
                }
                if !map.is_empty() {
                    scopes.push((scope, map));
                }
                // Don't descend into nested method bodies.
                continue;
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    LocalTypes { scopes }
}

/// C# formal parameters: `parameter_list` → `parameter` with `name` and `type` fields.
fn collect_params(params: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut c = params.walk();
    for p in params.children(&mut c) {
        if p.kind() != "parameter" {
            continue;
        }
        let Some(nm) = p.child_by_field_name("name") else {
            continue;
        };
        let Some(ty) = p.child_by_field_name("type") else {
            continue;
        };
        if let Some((var, ty_s)) = simple_id_and_type(nm, ty, source) {
            out.insert(var, ty_s);
        }
    }
}

/// Walk a block, collecting `local_declaration_statement` → `variable_declaration`.
/// Does NOT descend into nested method/constructor/local-function bodies.
fn collect_local_vars(body: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "method_declaration" | "constructor_declaration" | "local_function_statement" => {
                continue;
            }
            "local_declaration_statement" => {
                if let Some(var_decl) = n.named_child(0) {
                    if var_decl.kind() == "variable_declaration" {
                        // First named child is the type; variable_declarator children hold names.
                        let mut ty_node = None;
                        let mut c = var_decl.walk();
                        for child in var_decl.children(&mut c) {
                            if child.is_named() {
                                if ty_node.is_none() && is_type_node(child) {
                                    ty_node = Some(child);
                                } else if child.kind() == "variable_declarator" {
                                    if let (Some(ty), Some(nm)) =
                                        (ty_node, child.child_by_field_name("name"))
                                    {
                                        if let Some((var, ty_s)) =
                                            simple_id_and_type(nm, ty, source)
                                        {
                                            out.insert(var, ty_s);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

fn is_type_node(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "identifier"
            | "predefined_type"
            | "nullable_type"
            | "generic_name"
            | "array_type"
            | "qualified_name"
    )
}

/// Return `(var_name, type_name)` only when the name is a plain identifier and
/// the type is a plain class name (single `identifier` node).
fn simple_id_and_type(
    name_node: Node<'_>,
    type_node: Node<'_>,
    source: &[u8],
) -> Option<(String, String)> {
    if name_node.kind() != "identifier" {
        return None;
    }
    // Only bind plain class-name types (single `identifier`).
    if type_node.kind() != "identifier" {
        return None;
    }
    let name = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;
    let ty = std::str::from_utf8(&source[type_node.start_byte()..type_node.end_byte()]).ok()?;
    if ty.is_empty() {
        return None;
    }
    Some((name.to_string(), ty.to_string()))
}

/// Resolve the callee for a C# `invocation_expression`, applying receiver binding.
fn csharp_callee(
    inv: Node<'_>,
    source: &[u8],
    locals: &LocalTypes,
    nodes: &[RawNode],
) -> Option<String> {
    let func = inv.child_by_field_name("function")?;
    match func.kind() {
        "identifier" => func.utf8_text(source).ok().map(|s| s.to_string()),
        "member_access_expression" => {
            let method_node = func.child_by_field_name("name")?;
            let method =
                std::str::from_utf8(&source[method_node.start_byte()..method_node.end_byte()])
                    .ok()?;

            // `member_access_expression` has an `expression` field for the receiver
            // (can be `this`, `base`, or an `identifier`).
            let receiver = func.child_by_field_name("expression")?;

            let line = inv.start_position().row as u32;
            let ty: Option<String> = match receiver.kind() {
                "this_expression" | "this" => enclosing_class_name(nodes, line),
                "base_expression" | "base" => enclosing_base_name(nodes, line),
                "identifier" => {
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
        _ => {
            // Fallback: take last segment from the full text.
            func.utf8_text(source).ok().and_then(|s| {
                let seg = s.rsplit_once('.').map(|(_, t)| t).unwrap_or(s);
                let id: String = seg
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if id.is_empty() {
                    None
                } else {
                    Some(id)
                }
            })
        }
    }
}

/// Walk the AST once, extracting C# invocation sites (attached to
/// enclosing nodes) and collecting path-shaped string literals (`string_literal`,
/// `verbatim_string_literal`, `raw_string_literal`).
/// Interpolated strings (`$"..."`) are skipped by `build_raw_path_literal`.
pub fn extract_csharp_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
) -> Vec<RawPathLiteral> {
    let local_types = collect_local_types(root, source);
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "invocation_expression" => {
                if let Some(callee) = csharp_callee(n, source, &local_types, nodes) {
                    let line = n.start_position().row as u32;
                    attach_to_enclosing(line, callee, nodes);
                }
            }
            "object_creation_expression" => {
                // `new Foo(...)` → emit `Foo` as constructor call.
                let callee = n
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source).ok().map(|s| s.to_string()));
                if let Some(callee) = callee {
                    let line = n.start_position().row as u32;
                    attach_to_enclosing(line, callee, nodes);
                }
            }
            "string_literal" | "verbatim_string_literal" | "raw_string_literal" => {
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
        if !matches!(n.kind, NodeKind::Class | NodeKind::Interface) {
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

fn enclosing_base_name(nodes: &[RawNode], line: u32) -> Option<String> {
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
    use super::super::parser::CSharpProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    fn parse(src: &str) -> ecp_core::analyzer::types::LocalGraph {
        let provider = CSharpProvider::new().expect("CSharpProvider::new");
        provider
            .parse_file(Path::new("Test.cs"), src.as_bytes())
            .expect("parse_file")
    }

    /// `this.Eat()` should be recorded as `Animal.Eat` using the enclosing class name.
    #[test]
    fn csharp_ctor_this_receiver_binds_class_name() {
        let src = r#"
class Animal {
    void Breathe() {
        this.Eat();
    }
    void Eat() {}
}
"#;
        let graph = parse(src);
        let breathe = graph
            .nodes
            .iter()
            .find(|n| n.name == "Breathe")
            .expect("Breathe method not found");
        assert!(
            breathe.calls.iter().any(|c| c == "Animal.Eat"),
            "expected 'Animal.Eat' in calls, got {:?}",
            breathe.calls
        );
    }

    /// `base.Init()` should be recorded as `Animal.Init` (first heritage type).
    #[test]
    fn csharp_ctor_base_receiver_binds_base_class() {
        let src = r#"
class Dog : Animal {
    void Setup() {
        base.Init();
    }
    void Init() {}
}
"#;
        let graph = parse(src);
        let setup = graph
            .nodes
            .iter()
            .find(|n| n.name == "Setup")
            .expect("Setup method not found");
        assert!(
            setup.calls.iter().any(|c| c == "Animal.Init"),
            "expected 'Animal.Init' in calls, got {:?}",
            setup.calls
        );
    }

    /// `MyService svc = new MyService(); svc.Run()` → `MyService.Run`.
    #[test]
    fn csharp_ctor_typed_variable_receiver_binds_declared_type() {
        let src = r#"
class App {
    void Start() {
        MyService svc = new MyService();
        svc.Run();
    }
}
"#;
        let graph = parse(src);
        let start = graph
            .nodes
            .iter()
            .find(|n| n.name == "Start")
            .expect("Start method not found");
        assert!(
            start.calls.iter().any(|c| c == "MyService.Run"),
            "expected 'MyService.Run' in calls, got {:?}",
            start.calls
        );
    }

    /// Property guard: a `var items = ...` or `List<string> items = ...`
    /// declaration must NOT bind `items → List` — calling `items.Add()` would
    /// then emit `"List.Add"`, which is meaningless against the generic
    /// instantiation. `simple_id_and_type` rejects non-`identifier` type
    /// nodes for this reason; this test pins that behaviour.
    #[test]
    fn csharp_generic_type_does_not_bind_bare_type() {
        let src = r#"
class App {
    void Use() {
        List<string> items = new List<string>();
        items.Add("x");
    }
}
"#;
        let graph = parse(src);
        let use_fn = graph
            .nodes
            .iter()
            .find(|n| n.name == "Use")
            .expect("Use method not found");
        assert!(
            !use_fn.calls.iter().any(|c| c == "List.Add"),
            "must NOT bind generic `List<string>` to bare `List`; got {:?}",
            use_fn.calls
        );
        // bare `Add` is acceptable fallback (no type info)
    }

    /// Regression: `base.Foo()` in a class WITHOUT base must emit bare `"Foo"`,
    /// not a synthetic `"base.Foo"` that pollutes the graph.
    #[test]
    fn csharp_base_no_base_emits_bare_method() {
        let src = r#"
class Standalone {
    void DoWork() {
        base.Foo();
    }
}
"#;
        let graph = parse(src);
        let do_work = graph
            .nodes
            .iter()
            .find(|n| n.name == "DoWork")
            .expect("DoWork method not found");
        assert!(
            do_work.calls.iter().any(|c| c == "Foo"),
            "expected bare 'Foo' in calls, got {:?}",
            do_work.calls
        );
        assert!(
            !do_work.calls.iter().any(|c| c == "base.Foo"),
            "must NOT contain synthetic 'base.Foo', got {:?}",
            do_work.calls
        );
    }
}
