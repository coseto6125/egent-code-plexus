//! Receiver-type binding for Java method-call sites.
//!
//! Handles three receiver patterns:
//! - `this.foo()` → receiver is the enclosing class name
//! - `super.foo()` → receiver is the first superclass from the enclosing
//!   class's `heritage` list (if present), otherwise `"super"`
//! - `obj.foo()` → receiver is the declared type of `obj`, collected from
//!   local variable declarations and typed method parameters in scope
//!
//! Falls back to the bare method name for unresolved receivers, matching
//! the prior behavior of the generic `extract_calls` helper.

use crate::calls::attach_to_enclosing;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use std::collections::HashMap;
use tree_sitter::Node;

/// Map of variable name → declared type within a single method scope.
#[derive(Debug, Default)]
struct LocalTypes {
    /// `(scope_start_row, scope_end_row)` → `{var_name → type_name}`
    scopes: Vec<((u32, u32), HashMap<String, String>)>,
}

impl LocalTypes {
    /// Find the innermost scope containing `line` that has `var`.
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

/// Walk every method/constructor declaration, collecting typed parameters and
/// local variable declarations.
///
/// Java local variable: `local_variable_declaration` with a `type_identifier`
/// (or `generic_type` etc.) type child and a `variable_declarator` with an
/// `identifier` name child.
///
/// Java typed param: `formal_parameters` → `formal_parameter` with `type` and
/// `name` fields.
fn collect_local_types(root: Node<'_>, source: &[u8]) -> LocalTypes {
    let mut scopes: Vec<((u32, u32), HashMap<String, String>)> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "method_declaration" | "constructor_declaration" => {
                let scope_span = (
                    n.start_position().row as u32,
                    n.end_position().row as u32,
                );
                let mut map: HashMap<String, String> = HashMap::new();
                // Collect typed parameters.
                if let Some(params) = n.child_by_field_name("parameters") {
                    collect_formal_params(params, source, &mut map);
                }
                // Collect local variable declarations inside the body.
                if let Some(body) = n.child_by_field_name("body") {
                    collect_local_vars(body, source, &mut map);
                }
                if !map.is_empty() {
                    scopes.push((scope_span, map));
                }
                // Don't descend into nested method bodies — they form
                // their own scopes (anonymous class methods, lambdas).
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

fn collect_formal_params(params: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut c = params.walk();
    for p in params.children(&mut c) {
        if p.kind() != "formal_parameter" {
            continue;
        }
        let Some(ty) = p.child_by_field_name("type") else {
            continue;
        };
        let Some(name) = p.child_by_field_name("name") else {
            continue;
        };
        if let Some((var, ty_s)) = simple_id_and_type(name, ty, source) {
            out.insert(var, ty_s);
        }
    }
}

/// Walk a block collecting `local_variable_declaration` nodes. Does NOT descend
/// into nested method/constructor bodies (those get their own scope entry).
fn collect_local_vars(body: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "method_declaration" | "constructor_declaration" => continue,
            "local_variable_declaration" => {
                // type child: first named child (type_identifier / generic_type / etc.)
                // declarator child: variable_declarator with name field
                let mut ty_node = None;
                let mut c = n.walk();
                for child in n.children(&mut c) {
                    if child.is_named() {
                        if ty_node.is_none()
                            && matches!(
                                child.kind(),
                                "type_identifier"
                                    | "generic_type"
                                    | "integral_type"
                                    | "floating_point_type"
                                    | "boolean_type"
                                    | "array_type"
                            )
                        {
                            ty_node = Some(child);
                        } else if child.kind() == "variable_declarator" {
                            if let (Some(ty), Some(nm)) =
                                (ty_node, child.child_by_field_name("name"))
                            {
                                if let Some((var, ty_s)) = simple_id_and_type(nm, ty, source) {
                                    out.insert(var, ty_s);
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

/// Return `(var_name, type_name)` only when both name is a plain identifier
/// and type is a simple `type_identifier` (single class name, no generics).
fn simple_id_and_type(
    name_node: Node<'_>,
    type_node: Node<'_>,
    source: &[u8],
) -> Option<(String, String)> {
    if name_node.kind() != "identifier" {
        return None;
    }
    // Only bind plain class-name types; skip primitives and generics for now.
    let ty_inner = if type_node.kind() == "type_identifier" {
        type_node
    } else {
        return None;
    };
    let name = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;
    let ty = std::str::from_utf8(&source[ty_inner.start_byte()..ty_inner.end_byte()]).ok()?;
    if ty.is_empty() || !ty.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some((name.to_string(), ty.to_string()))
}

/// Resolve the callee name for a Java `method_invocation` node, applying
/// receiver-type binding. Returns `None` to drop the edge entirely (rare).
fn java_callee(
    mi: Node<'_>,
    source: &[u8],
    locals: &LocalTypes,
    nodes: &[RawNode],
) -> Option<String> {
    // Java `method_invocation` field layout:
    //   object: (<this> | <super> | <identifier> | ...) (optional)
    //   name:   <identifier>
    let name_node = mi.child_by_field_name("name")?;
    let method_name =
        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;

    let receiver_str: Option<String> = if let Some(obj) = mi.child_by_field_name("object") {
        let line = mi.start_position().row as u32;
        match obj.kind() {
            "this" => enclosing_class_name(nodes, line),
            "super" => enclosing_superclass(nodes, line),
            "identifier" => {
                let var =
                    std::str::from_utf8(&source[obj.start_byte()..obj.end_byte()]).ok()?;
                locals.lookup(line, var).map(|t| t.to_string())
            }
            _ => None,
        }
    } else {
        None
    };

    Some(match receiver_str {
        Some(ty) => format!("{ty}.{method_name}"),
        None => method_name.to_string(),
    })
}

/// Walk the AST, extract all `method_invocation` and `object_creation_expression`
/// call sites, attach them to enclosing function/method nodes with receiver
/// binding applied where resolvable.
pub fn extract_java_calls(root: Node<'_>, source: &[u8], nodes: &mut [RawNode]) {
    let local_types = collect_local_types(root, source);

    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "method_invocation" => {
                if let Some(callee) = java_callee(n, source, &local_types, nodes) {
                    let line = n.start_position().row as u32;
                    attach_to_enclosing(line, callee, nodes);
                }
            }
            "object_creation_expression" => {
                // `new Foo(...)` — emit `Foo` as a call to the constructor.
                let callee = n
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source).ok().map(|s| s.to_string()));
                if let Some(callee) = callee {
                    let line = n.start_position().row as u32;
                    attach_to_enclosing(line, callee, nodes);
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

/// Name of the innermost enclosing Class/Interface node at `line`.
fn enclosing_class_name(nodes: &[RawNode], line: u32) -> Option<String> {
    let mut best: Option<(u32, &str)> = None;
    for n in nodes {
        if !matches!(n.kind, NodeKind::Class | NodeKind::Interface) {
            continue;
        }
        if n.span.0 <= line && n.span.2 >= line {
            let w = n.span.2 - n.span.0;
            if best.map_or(true, |(bw, _)| w < bw) {
                best = Some((w, &n.name));
            }
        }
    }
    best.map(|(_, name)| name.to_string())
}

/// First superclass in the enclosing class's heritage list.
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
                result = n.heritage.first().map(|s| s.clone());
            }
        }
    }
    result.or_else(|| Some("super".to_string()))
}

#[cfg(test)]
mod tests {
    use super::super::parser::JavaProvider;
    use graph_nexus_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    fn parse(src: &str) -> graph_nexus_core::analyzer::types::LocalGraph {
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
