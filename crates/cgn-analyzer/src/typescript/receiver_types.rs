//! Local-scope receiver-type binding for TypeScript.
//!
//! Mirrors `crates/cgn-analyzer/src/python/receiver_types.rs`.
//!
//! Collects typed bindings from:
//! (a) Typed parameters: `function f(x: MyType)` — `required_parameter` with a
//!     `type_annotation` whose inner node is a `type_identifier`.
//! (b) Typed variable declarations: `const x: MyType = …` — `variable_declarator`
//!     with a `type_annotation (type_identifier)`.
//!
//! Scope rules follow the Python reference: smallest containing function scope wins,
//! `this` inside a method resolves to the enclosing class (handled at call-site in
//! `extract_ts_calls`).
//!
//! Generic / union / intersection types (e.g. `Array<T>`, `A | B`) are skipped — the
//! type annotation's inner node won't be a plain `type_identifier`, so we fall back to
//! the bare method name (same as for un-annotated code).

use crate::calls::attach_to_enclosing;
use crate::framework_helpers::{enclosing_class, node_span};
use cgn_core::analyzer::types::RawNode;
use std::collections::HashMap;
use tree_sitter::Node;

/// Map of function scopes (by row span) to their `var → type` bindings.
#[derive(Debug, Default)]
pub struct LocalTypes {
    scopes: Vec<((u32, u32), HashMap<String, String>)>,
}

impl LocalTypes {
    /// Smallest enclosing scope that declares `var`.
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

/// Walk every `function_declaration` / `method_definition` / `arrow_function` node
/// and collect typed parameter and variable bindings.
pub fn collect_local_types(root: Node<'_>, source: &[u8]) -> LocalTypes {
    let mut scopes: Vec<((u32, u32), HashMap<String, String>)> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        let is_fn = matches!(
            n.kind(),
            "function_declaration"
                | "function"
                | "method_definition"
                | "arrow_function"
                | "function_signature"
        );
        if is_fn {
            let fn_span = (n.start_position().row as u32, n.end_position().row as u32);
            let mut map: HashMap<String, String> = HashMap::new();

            if let Some(params) = n.child_by_field_name("parameters") {
                collect_typed_params(params, source, &mut map);
            }

            if let Some(body) = n.child_by_field_name("body") {
                collect_typed_vars(body, source, &mut map);
            }

            if !map.is_empty() {
                scopes.push((fn_span, map));
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    LocalTypes { scopes }
}

/// Collect typed parameters from a `formal_parameters` node.
/// Shape: `(required_parameter pattern: (identifier) type: (type_annotation (type_identifier)))`.
fn collect_typed_params(params: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut c = params.walk();
    for p in params.children(&mut c) {
        // required_parameter and optional_parameter both carry `pattern` + `type`.
        if !matches!(p.kind(), "required_parameter" | "optional_parameter") {
            continue;
        }
        let Some(pat) = p.child_by_field_name("pattern") else {
            continue;
        };
        if pat.kind() != "identifier" {
            continue;
        }
        let Some(type_ann) = p.child_by_field_name("type") else {
            continue;
        };
        if let Some((name, ty)) = simple_name_and_type(pat, type_ann, source) {
            out.insert(name, ty);
        }
    }
}

/// Walk a function body collecting typed `variable_declarator` nodes.
/// Shape: `(variable_declarator name: (identifier) type: (type_annotation (type_identifier)))`.
/// Does NOT descend into nested function declarations.
fn collect_typed_vars(body: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        // Don't descend into nested functions — they get their own scope.
        if matches!(
            n.kind(),
            "function_declaration" | "function" | "method_definition" | "arrow_function"
        ) {
            continue;
        }
        if n.kind() == "variable_declarator" {
            if let (Some(name_node), Some(type_ann)) =
                (n.child_by_field_name("name"), n.child_by_field_name("type"))
            {
                if name_node.kind() == "identifier" {
                    if let Some((name, ty)) = simple_name_and_type(name_node, type_ann, source) {
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

/// Extract `(name, type)` only when the type annotation contains a single
/// `type_identifier` (no generics, no unions).
fn simple_name_and_type(
    name_node: Node<'_>,
    type_ann: Node<'_>,
    source: &[u8],
) -> Option<(String, String)> {
    // type_annotation wraps the actual type; unwrap one level.
    let inner = if type_ann.kind() == "type_annotation" {
        type_ann.named_child(0)?
    } else {
        type_ann
    };
    if inner.kind() != "type_identifier" {
        return None;
    }
    let name = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;
    let ty = std::str::from_utf8(&source[inner.start_byte()..inner.end_byte()]).ok()?;
    if !ty.chars().all(|c| c.is_alphanumeric() || c == '_') || ty.is_empty() {
        return None;
    }
    Some((name.to_string(), ty.to_string()))
}

/// Walk the TypeScript AST and attach callees to enclosing functions/methods,
/// with receiver-type binding:
///
/// - `this.method()` → looks up the innermost enclosing class → emits `ClassName.method`
/// - `obj.method()` where `obj` is a typed param/var → emits `Type.method`
/// - anything else falls back to the bare method name (or full expression as before)
pub fn extract_ts_calls(root: Node<'_>, source: &[u8], nodes: &mut [RawNode], locals: &LocalTypes) {
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "call_expression" {
            if let Some(callee) = ts_callee_name(n, source, locals, nodes) {
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

fn ts_callee_name(
    call: Node<'_>,
    source: &[u8],
    locals: &LocalTypes,
    nodes: &[RawNode],
) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    match function.kind() {
        "identifier" => function.utf8_text(source).ok().map(str::to_string),
        "member_expression" => {
            let obj = function.child_by_field_name("object")?;
            let prop = function.child_by_field_name("property")?;
            let prop_name = prop.utf8_text(source).ok()?;
            let line = call.start_position().row as u32;

            match obj.kind() {
                "this" => {
                    // `this.method()` — look up enclosing class.
                    let call_span = node_span(&call);
                    if let Some((class_name, _)) = enclosing_class(nodes, call_span) {
                        return Some(format!("{class_name}.{prop_name}"));
                    }
                    // No enclosing class (shouldn't happen for valid TS, but be safe).
                    Some(prop_name.to_string())
                }
                "identifier" => {
                    let obj_name = obj.utf8_text(source).ok()?;
                    if let Some(ty) = locals.lookup(line, obj_name) {
                        return Some(format!("{ty}.{prop_name}"));
                    }
                    // Unknown type — emit qualified name so the resolver can try.
                    Some(format!("{obj_name}.{prop_name}"))
                }
                _ => {
                    // Chained member expression or other complex form: fall back to prop name.
                    Some(prop_name.to_string())
                }
            }
        }
        _ => function.utf8_text(source).ok().map(str::to_string),
    }
}

/// Tests live here so they run as part of `cargo test -p cgn-analyzer`.
#[cfg(test)]
mod tests {
    use crate::typescript::TypeScriptProvider;
    use cgn_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    /// Helper: parse source and collect all `calls` from all nodes.
    fn all_calls(source: &str) -> Vec<String> {
        let provider = TypeScriptProvider::new().unwrap();
        let graph = provider
            .parse_file(Path::new("test.ts"), source.as_bytes())
            .unwrap();
        graph.nodes.into_iter().flat_map(|n| n.calls).collect()
    }

    #[test]
    fn typescript_ctor_this_method_resolved_to_class() {
        let src = r#"
class OrderService {
    process(): void {
        this.validate();
        this.save();
    }
    validate(): void {}
    save(): void {}
}
"#;
        let calls = all_calls(src);
        assert!(
            calls.contains(&"OrderService.validate".to_string()),
            "expected OrderService.validate in calls, got: {calls:?}"
        );
        assert!(
            calls.contains(&"OrderService.save".to_string()),
            "expected OrderService.save in calls, got: {calls:?}"
        );
    }

    #[test]
    fn typescript_ctor_typed_param_resolved() {
        let src = r#"
function process(repo: UserRepository): void {
    repo.findAll();
    repo.save();
}
"#;
        let calls = all_calls(src);
        assert!(
            calls.contains(&"UserRepository.findAll".to_string()),
            "expected UserRepository.findAll in calls, got: {calls:?}"
        );
        assert!(
            calls.contains(&"UserRepository.save".to_string()),
            "expected UserRepository.save in calls, got: {calls:?}"
        );
    }

    #[test]
    fn typescript_ctor_typed_variable_resolved() {
        let src = r#"
function run(): void {
    const svc: PaymentService = new PaymentService();
    svc.charge();
}
"#;
        let calls = all_calls(src);
        assert!(
            calls.contains(&"PaymentService.charge".to_string()),
            "expected PaymentService.charge in calls, got: {calls:?}"
        );
    }

    #[test]
    fn typescript_ctor_untyped_param_not_prefixed() {
        // Without a type annotation the receiver falls back to `obj.method` (qualified).
        let src = r#"
function process(obj: any): void {
    obj.doWork();
}
"#;
        let calls = all_calls(src);
        // `any` is a predefined_type, not a type_identifier — so no binding.
        // The fallback should still produce something usable for the resolver.
        assert!(
            !calls.iter().any(|c| c.starts_with("any.")),
            "should not bind predefined type 'any': {calls:?}"
        );
    }
}
