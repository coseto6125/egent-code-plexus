//! Receiver-type binding for Java method-call sites.
//!
//! Handles three receiver patterns:
//! - `this.foo()` → receiver is the enclosing class name
//! - `super.foo()` → receiver is the first superclass from the enclosing
//!   class's `heritage` list (if present), otherwise `"super"`
//! - `obj.foo()` → receiver is the declared type of `obj`, collected from
//!   local variable declarations and typed method parameters in scope
//! - `new T(..).foo()`, `new T<X>(){..}.foo()`, `outer.new Inner().foo()`,
//!   `((T) o).foo()` → receiver is the created or cast type `T`, written as
//!   a simple (possibly generic) class name
//!
//! Falls back to the bare method name for unresolved receivers, matching
//! the prior behavior of the generic `extract_calls` helper.

use super::path_literals::{
    build_raw_path_literal, enclosing_symbol_and_owner_pub, strip_java_string_value,
};
use super::receiver_lower::JavaLowering;
use crate::calls::{
    attach_to_enclosing, construction_call, reference_function, span_holds, span_width, start_pos,
    Pos,
};
use crate::receiver::{analyze_file, FileReceivers, Receiver};
use ecp_core::analyzer::types::{CallSite, RawNode, RawPathLiteral, RawSqlRef};
use ecp_core::graph::NodeKind;
use std::path::Path;
use tree_sitter::Node;

/// Resolve the callee name for a Java `method_invocation` node, applying
/// receiver-type binding. Returns `None` to drop the edge entirely (rare).
fn java_callee(
    mi: Node<'_>,
    source: &[u8],
    receivers: &FileReceivers,
    nodes: &[RawNode],
) -> Option<String> {
    // Java `method_invocation` field layout:
    //   object: (<this> | <super> | <identifier> | ...) (optional)
    //   name:   <identifier>
    let name_node = mi.child_by_field_name("name")?;
    let method_name =
        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;

    let receiver_str: Option<String> = if let Some(obj) = mi.child_by_field_name("object") {
        let pos = start_pos(mi);
        match obj.kind() {
            "this" => enclosing_class_name(nodes, pos),
            "super" => enclosing_superclass(nodes, pos),
            "identifier" => {
                // A tracked local resolves to its declared type. Anything else
                // (a static class name like `Util`, a field) keeps the
                // identifier as-is so Tier 2.5 qualifier-scoped lookup can match
                // `Util.helper` against the definition node.
                if let Some(receiver) = receivers.receiver_at(obj) {
                    return Some(receiver.call_entry(method_name));
                }
                let var = std::str::from_utf8(&source[obj.start_byte()..obj.end_byte()]).ok()?;
                Some(var.to_string())
            }
            // A computed receiver (`a.b().m()`, `this.f.m()`): the member is
            // never a static import's name. Its type is known by syntax when
            // a `new` or a cast spells it, and by the resolver when it is a
            // call (the callee's return type).
            _ => match expression_type(obj, source) {
                Some(ty) => Some(ty.to_string()),
                None => {
                    let untyped = CallSite::untyped_member(method_name);
                    return Some(match obj.kind() {
                        "method_invocation" => {
                            match CallSite::nested_chain(|| {
                                java_callee(obj, source, receivers, nodes)
                            }) {
                                Some(inner) => CallSite::chained_member(&inner, &untyped),
                                None => untyped,
                            }
                        }
                        _ => untyped,
                    });
                }
            },
        }
    } else {
        None
    };

    Some(match receiver_str {
        Some(ty) => format!("{ty}.{method_name}"),
        None => method_name.to_string(),
    })
}

/// The type an expression receiver has by its syntax alone: the type a
/// `new` creates (`new T<X>(){..}` → `T`; `outer.new Inner()` → `Inner`) or
/// a cast names, through parentheses. Only a simple class name, generic
/// arguments cut: a qualified `new a.b.T()` names a package path, which
/// must not land on a project `T`, so it stays untyped.
pub(super) fn expression_type<'s>(expr: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    match expr.kind() {
        "parenthesized_expression" => expression_type(expr.named_child(0)?, source),
        "object_creation_expression" | "cast_expression" => {
            let ty = expr.child_by_field_name("type")?;
            let name = match ty.kind() {
                "type_identifier" => ty,
                "generic_type" => ty
                    .named_child(0)
                    .filter(|n| n.kind() == "type_identifier")?,
                _ => return None,
            };
            name.utf8_text(source).ok()
        }
        _ => None,
    }
}

/// Walk the AST once, extracting `method_invocation` /
/// `object_creation_expression` call sites (attached to enclosing
/// function/method nodes with receiver binding) and collecting path-shaped
/// `string_literal` / `text_block` literals and SQL-shaped string literals.
pub fn extract_java_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let receivers = analyze_file(&JavaLowering, &(), root, source, Path::new(""));

    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "method_invocation" => {
                if let Some(callee) = java_callee(n, source, &receivers, nodes) {
                    attach_to_enclosing(start_pos(n), callee, nodes);
                }
            }
            "object_creation_expression" => {
                // Encoded as a construction: a plain Java call never constructs.
                let callee = n
                    .child_by_field_name("type")
                    .and_then(|t| construction_call(t.utf8_text(source).ok()?));
                if let Some(callee) = callee {
                    attach_to_enclosing(start_pos(n), callee, nodes);
                }
            }
            "method_reference" => {
                if let Some(name) = bound_method_reference(n, source, &receivers) {
                    reference_function(start_pos(n), &name, nodes);
                }
            }
            "string_literal" | "text_block" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same string node, separate filter.
                let raw_bytes = &source[n.start_byte()..n.end_byte()];
                if let Ok(raw) = std::str::from_utf8(raw_bytes) {
                    if let Some(value) = strip_java_string_value(raw) {
                        if let Some(mut sql_ref) =
                            crate::sql_literal::try_sql_ref(value, n, (None, None))
                        {
                            (sql_ref.enclosing_symbol, sql_ref.enclosing_owner) =
                                enclosing_symbol_and_owner_pub(n, source);
                            sql_refs.push(sql_ref);
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
    (path_literals, sql_refs)
}

/// `c::m` and `c::<A>m` where `c` is a local or parameter of a declared type
/// `T`: the method `T.m`, typed as a call `c.m()` is. The name scan drops
/// `c::m`, as `c` is a local; a receiver of no known type names nothing.
fn bound_method_reference(
    reference: Node<'_>,
    source: &[u8],
    receivers: &FileReceivers,
) -> Option<String> {
    let last = reference.named_child_count().checked_sub(1)?;
    let known_shape = match last {
        1 => true,
        2 => reference
            .named_child(1)
            .is_some_and(|between| between.kind() == "type_arguments"),
        _ => false,
    };
    if !known_shape {
        return None;
    }
    let (receiver, member) = (
        reference.named_child(0)?,
        reference.named_child(last as u32)?,
    );
    if receiver.kind() != "identifier" || member.kind() != "identifier" {
        return None;
    }
    match receivers.receiver_at(receiver)? {
        Receiver::Known(ty) => Some(format!("{}.{}", ty.text, member.utf8_text(source).ok()?)),
        Receiver::Unknown(_) => None,
    }
}

/// Name of the innermost enclosing Class/Interface node at `pos`.
fn enclosing_class_name(nodes: &[RawNode], pos: Pos) -> Option<String> {
    let mut best: Option<((u32, i64), &str)> = None;
    for n in nodes {
        if !matches!(n.kind, NodeKind::Class | NodeKind::Interface) {
            continue;
        }
        if span_holds(n.span, pos) {
            let w = span_width(n.span);
            if best.is_none_or(|(bw, _)| w < bw) {
                best = Some((w, &n.name));
            }
        }
    }
    best.map(|(_, name)| name.to_string())
}

/// First superclass in the enclosing class's heritage list.
fn enclosing_superclass(nodes: &[RawNode], pos: Pos) -> Option<String> {
    let mut best_w = (u32::MAX, i64::MAX);
    let mut result = None;
    for n in nodes {
        if !matches!(n.kind, NodeKind::Class) {
            continue;
        }
        if span_holds(n.span, pos) {
            let w = span_width(n.span);
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
    use super::super::parser::JavaProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    fn parse(src: &str) -> ecp_core::analyzer::types::LocalGraph {
        let provider = JavaProvider::new().expect("JavaProvider::new");
        provider
            .parse_file(Path::new("Test.java"), src.as_bytes())
            .expect("parse_file")
    }

    /// `this.foo()` should be recorded as `ClassName.foo` in the enclosing
    /// method's calls list, using the enclosing class name as receiver.
    #[test]
    fn java_ctor_this_receiver_binds_class_name() {
        let src = r#"
class Animal {
    void breathe() {
        this.eat();
    }
    void eat() {}
}
"#;
        let graph = parse(src);
        let breathe = graph
            .nodes
            .iter()
            .find(|n| n.name == "breathe")
            .expect("breathe method not found");
        assert!(
            breathe.calls.iter().any(|c| c == "Animal.eat"),
            "expected 'Animal.eat' in calls, got {:?}",
            breathe.calls
        );
    }

    /// `super.init()` should be recorded as `BaseClass.init`.
    #[test]
    fn java_ctor_super_receiver_binds_superclass() {
        let src = r#"
class Dog extends Animal {
    void setup() {
        super.init();
    }
    void init() {}
}
"#;
        let graph = parse(src);
        let setup = graph
            .nodes
            .iter()
            .find(|n| n.name == "setup")
            .expect("setup method not found");
        assert!(
            setup.calls.iter().any(|c| c == "Animal.init"),
            "expected 'Animal.init' in calls, got {:?}",
            setup.calls
        );
    }

    /// `MyService obj = new MyService(); obj.run()` should yield `MyService.run`.
    #[test]
    fn java_ctor_typed_variable_receiver_binds_declared_type() {
        let src = r#"
class App {
    void start() {
        MyService svc = new MyService();
        svc.run();
    }
}
"#;
        let graph = parse(src);
        let start = graph
            .nodes
            .iter()
            .find(|n| n.name == "start")
            .expect("start method not found");
        assert!(
            start.calls.iter().any(|c| c == "MyService.run"),
            "expected 'MyService.run' in calls, got {:?}",
            start.calls
        );
    }

    /// Regression: `super.foo()` in a class WITHOUT `extends` must emit bare
    /// `"foo"`, not a synthetic `"super.foo"` that pollutes the graph with a
    /// node that never resolves.
    #[test]
    fn java_super_no_extends_emits_bare_method() {
        let src = r#"
class Standalone {
    void doWork() {
        super.foo();
    }
}
"#;
        let graph = parse(src);
        let do_work = graph
            .nodes
            .iter()
            .find(|n| n.name == "doWork")
            .expect("doWork method not found");
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

    // Regression: after the wave-1 merge dropped `idx.constructor` (main's
    // 95653b2 collapsed `(constructor_declaration ... ) @method`), method
    // calls inside a constructor body must still receiver-bind. The risk was
    // that the constructor node wouldn't be classified as Method and thus
    // skip the receiver_types pass entirely.
    #[test]
    fn java_call_inside_constructor_binds_self() {
        let src = r#"
class Foo {
    public Foo() {
        this.init();
    }
    void init() {}
}
"#;
        let graph = parse(src);
        let ctor = graph
            .nodes
            .iter()
            .find(|n| n.name == "Foo" && !n.calls.is_empty())
            .expect("Foo constructor not found or has no calls");
        assert!(
            ctor.calls.iter().any(|c| c == "Foo.init"),
            "constructor's this.init() should bind to Foo.init; got {:?}",
            ctor.calls
        );
    }
}
