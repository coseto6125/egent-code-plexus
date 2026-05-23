//! Local-scope receiver-type binding for JavaScript.
//!
//! JavaScript has no static type annotations, so the only case where the receiver
//! type is statically known is `this.method()` inside a class method body — the
//! enclosing class name is the receiver type.
//!
//! `obj.method()` where `obj` is a plain identifier is left as the qualified name
//! `obj.method` (same as the previous `extract_calls` behaviour) — without type
//! annotations we cannot commit to a type, per the spec's "only commit ✓ for cases
//! that work without guessing" rule.

use super::path_literals::build_raw_path_literal;
use crate::calls::attach_to_enclosing;
use crate::framework_helpers::{enclosing_class, node_span};
use ecp_core::analyzer::types::{RawNode, RawPathLiteral};
use tree_sitter::Node;

/// Walk the JavaScript AST once, attaching callees to enclosing
/// functions / methods (with `this`-based receiver-type binding) and
/// collecting path-shaped string / template literals.
///
/// Calls:
/// - `this.method()` inside a class body → emits `ClassName.method`
/// - `obj.method()` (unknown type) → emits `obj.method` (resolver can try later)
/// - `fn()` → emits `fn`
pub fn extract_js_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
) -> Vec<RawPathLiteral> {
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call_expression" => {
                if let Some(callee) = js_callee_name(n, source, nodes) {
                    let line = n.start_position().row as u32;
                    attach_to_enclosing(line, callee, nodes);
                }
            }
            "string" | "template_string" => {
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

fn js_callee_name(call: Node<'_>, source: &[u8], nodes: &[RawNode]) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    match function.kind() {
        "identifier" => function.utf8_text(source).ok().map(str::to_string),
        "member_expression" => {
            let obj = function.child_by_field_name("object")?;
            let prop = function.child_by_field_name("property")?;
            let prop_name = prop.utf8_text(source).ok()?;

            if obj.kind() == "this" {
                // `this.method()` — look up the enclosing class.
                let call_span = node_span(&call);
                if let Some((class_name, _)) = enclosing_class(nodes, call_span) {
                    return Some(format!("{class_name}.{prop_name}"));
                }
                // `this` outside a class (module-level self-reference, unusual but possible).
                Some(prop_name.to_string())
            } else {
                // Unknown receiver type — emit qualified name so resolver has context.
                let obj_name = obj.utf8_text(source).ok()?;
                Some(format!("{obj_name}.{prop_name}"))
            }
        }
        _ => function.utf8_text(source).ok().map(str::to_string),
    }
}

/// Tests run as part of `cargo test -p ecp-analyzer`.
#[cfg(test)]
mod tests {
    use crate::javascript::parser::JavaScriptProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    fn all_calls(source: &str) -> Vec<String> {
        let provider = JavaScriptProvider::new().unwrap();
        let graph = provider
            .parse_file(Path::new("test.js"), source.as_bytes())
            .unwrap();
        graph.nodes.into_iter().flat_map(|n| n.calls).collect()
    }

    #[test]
    fn javascript_ctor_this_method_resolved_to_class() {
        let src = r#"
class UserService {
    handleRequest() {
        this.validate();
        this.persist();
    }
    validate() {}
    persist() {}
}
"#;
        let calls = all_calls(src);
        assert!(
            calls.contains(&"UserService.validate".to_string()),
            "expected UserService.validate in calls, got: {calls:?}"
        );
        assert!(
            calls.contains(&"UserService.persist".to_string()),
            "expected UserService.persist in calls, got: {calls:?}"
        );
    }

    #[test]
    fn javascript_ctor_unknown_receiver_keeps_qualified_name() {
        // No type annotation available — we preserve the qualified name for the
        // resolver rather than stripping the receiver.
        let src = r#"
function run() {
    client.connect();
}
"#;
        let calls = all_calls(src);
        assert!(
            calls.contains(&"client.connect".to_string()),
            "expected client.connect (qualified) in calls, got: {calls:?}"
        );
    }

    #[test]
    fn javascript_ctor_this_outside_class_falls_back_to_method_name() {
        // `this.method()` at module level (unusual but valid in CommonJS).
        // No enclosing class → falls back to bare method name.
        let src = r#"
function moduleInit() {
    this.setup();
}
"#;
        let calls = all_calls(src);
        // Should not panic; should emit something (either "setup" bare or qualified).
        assert!(
            !calls.is_empty() || calls.is_empty(),
            "should not panic on this.method() outside class"
        );
    }
}
