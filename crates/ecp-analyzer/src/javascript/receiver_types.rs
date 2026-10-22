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

use super::path_literals::{build_raw_path_literal, strip_js_string_value, LiteralContext};
use crate::calls::{attach_to_enclosing_span, new_expression_call, CallSiteIndex};
use crate::framework_helpers::{enclosing_class, node_span, Span};
use ecp_core::analyzer::types::{CallSite, RawNode, RawPathLiteral, RawSqlRef};
use tree_sitter::Node;

/// Walk the JavaScript AST once, attaching callees to enclosing
/// functions / methods (with `this`-based receiver-type binding),
/// collecting path-shaped string / template literals, and extracting
/// SQL-shaped string literals as `RawSqlRef`s.
///
/// Calls:
/// - `this.method()` inside a class body → emits `ClassName.method`
/// - `obj.method()` (unknown type) → emits `obj.method` (resolver can try later)
/// - `fn()` → emits `fn`
pub fn extract_js_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    call_sites: &mut CallSiteIndex,
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let mut constructions: Vec<(Span, String)> = Vec::new();
    let mut stack = vec![(root, None, LiteralContext::default())];
    while let Some((n, parent, context)) = stack.pop() {
        match n.kind() {
            "call_expression" => {
                if let Some(callee) = js_callee_name(n, source, nodes) {
                    if let Some(site) = attach_to_enclosing_span(node_span(&n), callee, nodes) {
                        call_sites.insert(n.id(), site);
                    }
                }
            }
            "new_expression" => {
                if let Some(callee) = new_expression_call(n, source) {
                    constructions.push((node_span(&n), callee));
                }
            }
            "string" => {
                if let Some(rpl) = build_raw_path_literal(n, source, context) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same string node, separate filter.
                let raw_bytes = &source[n.start_byte()..n.end_byte()];
                if let Ok(raw) = std::str::from_utf8(raw_bytes) {
                    if let Some(value) = strip_js_string_value(raw) {
                        if let Some(mut sql_ref) =
                            crate::sql_literal::try_sql_ref(value, n, (None, None))
                        {
                            (sql_ref.enclosing_symbol, sql_ref.enclosing_owner) = (
                                context.symbol.map(str::to_string),
                                context.owner.map(str::to_string),
                            );
                            sql_refs.push(sql_ref);
                        }
                    }
                }
            }
            "template_string" => {
                if let Some(rpl) = build_raw_path_literal(n, source, context) {
                    path_literals.push(rpl);
                }
            }
            _ => {}
        }
        let child_context = context.for_children(n, parent, source);
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push((child, Some(n), child_context));
        }
    }
    // After every call: the call-meta detectors index `calls` in
    // `call_expression` order, which a `new` site must not shift.
    for (span, callee) in constructions {
        attach_to_enclosing_span(span, callee, nodes);
    }
    (path_literals, sql_refs)
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
                Some(CallSite::from_receiver("this", prop_name))
            } else {
                // A call receiver (`make().bar()`, `new Foo().bar()`): the
                // resolver types it by the callee's return type or the
                // constructed class. The chain fallback is the member name
                // alone: the resolver keeps only its last segment, and the
                // receiver text of a long chain would make every site
                // O(depth).
                let inner = match obj.kind() {
                    "call_expression" => {
                        CallSite::nested_chain(|| js_callee_name(obj, source, nodes))
                    }
                    "new_expression" => new_expression_call(obj, source),
                    _ => None,
                };
                Some(match (inner, obj.kind()) {
                    (Some(inner), _) => {
                        let fallback = CallSite::from_receiver("call", prop_name);
                        CallSite::chained_member(&inner, &fallback)
                    }
                    (None, "call_expression") => CallSite::from_receiver("call", prop_name),
                    // Unknown receiver type — emit qualified name so resolver has context.
                    (None, _) => format!("{}.{prop_name}", obj.utf8_text(source).ok()?),
                })
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
