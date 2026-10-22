//! Receiver-type binding for C# invocation sites.
//!
//! Handles:
//! - `this.Foo()` → receiver is the enclosing class/struct name
//! - `base.Foo()` → first base type from the enclosing class's `heritage`
//! - `obj.Foo()` where `obj` has a known type from local variable declarations
//!   or typed method parameters
//! - `Name.Foo()` where `Name` is no local, parameter, field or property in
//!   scope → [`CallSite::from_receiver`]: the resolver tries `Name` as a type
//!   (`StringAssert.AreEqual()`) and else resolves the bare member
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

use super::path_literals::{
    build_raw_path_literal, enclosing_symbol_and_owner_pub, strip_csharp_string_value,
};
use crate::calls::{
    attach_to_enclosing, construction_call, node_range, range_width, span_holds, span_width,
    start_pos, Pos, PosRange,
};
use ecp_core::analyzer::types::{CallSite, RawNode, RawPathLiteral, RawSqlRef};
use ecp_core::graph::NodeKind;
use std::collections::HashMap;
use tree_sitter::Node;

#[derive(Debug, Default)]
struct LocalTypes {
    scopes: Vec<(PosRange, HashMap<String, String>)>,
}

impl LocalTypes {
    fn lookup(&self, pos: Pos, var: &str) -> Option<&str> {
        let mut best: Option<&str> = None;
        let mut best_width = (u32::MAX, i64::MAX);
        for ((start, end), map) in &self.scopes {
            if *start <= pos && pos <= *end {
                if let Some(t) = map.get(var) {
                    let w = range_width((*start, *end));
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
    let mut scopes: Vec<(PosRange, HashMap<String, String>)> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "method_declaration" | "constructor_declaration" | "local_function_statement" => {
                let scope = node_range(n);
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
        let typed = p
            .child_by_field_name("type")
            .and_then(|ty| simple_id_and_type(nm, ty, source));
        match typed {
            Some((var, ty_s)) => {
                out.insert(var, ty_s);
            }
            None => declare_untyped(nm, source, out),
        }
    }
}

/// Record a declared name whose type is not a plain class name as `""`: it
/// still shadows a type of the same name, so a call on it keeps the bare
/// member instead of resolving through that type.
fn declare_untyped(name_node: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    // `implicit_parameter` is the grammar's alias of the identifier in `x => ..`.
    if !matches!(name_node.kind(), "identifier" | "implicit_parameter") {
        return;
    }
    if let Ok(name) = name_node.utf8_text(source) {
        out.entry(name.to_string()).or_default();
    }
}

/// Walk a block, collecting `local_declaration_statement` → `variable_declaration`,
/// and every other declared name (`var x`, `for` / `using` / `foreach` / `catch`
/// variables, lambda parameters) as untyped.
/// Does NOT descend into nested method/constructor/local-function bodies.
fn collect_local_vars(body: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "method_declaration" | "constructor_declaration" | "local_function_statement" => {
                continue;
            }
            "variable_declarator" | "parameter" | "catch_declaration" => {
                if let Some(nm) = n.child_by_field_name("name") {
                    declare_untyped(nm, source, out);
                }
            }
            "foreach_statement" => {
                if let Some(left) = n.child_by_field_name("left") {
                    declare_untyped(left, source, out);
                }
            }
            "lambda_expression" => {
                if let Some(param) = n.child_by_field_name("parameters") {
                    declare_untyped(param, source, out);
                }
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

            let pos = start_pos(inv);
            let ty: Option<String> = match receiver.kind() {
                "this_expression" | "this" => enclosing_class_name(nodes, pos),
                "base_expression" | "base" => enclosing_base_name(nodes, pos),
                "identifier" => {
                    let var =
                        std::str::from_utf8(&source[receiver.start_byte()..receiver.end_byte()])
                            .ok()?;
                    // C# looks a simple name up as a local, then as a member
                    // of the enclosing types, then as a type. Only a name
                    // that is neither may be a type, which the resolver
                    // decides (`StringAssert.AreEqual()`).
                    match locals.lookup(pos, var) {
                        Some("") => None,
                        Some(t) => Some(t.to_string()),
                        None if declares_member(nodes, pos, var) => None,
                        None => return Some(CallSite::from_receiver(var, method)),
                    }
                }
                _ => None,
            };
            Some(match (ty, receiver.kind()) {
                (Some(t), _) => format!("{t}.{method}"),
                // `Foo.Create().Bar()`: the resolver types the receiver by
                // the callee's return type.
                (None, "invocation_expression") => {
                    match csharp_callee(receiver, source, locals, nodes) {
                        Some(inner) => CallSite::chained_member(&inner, method),
                        None => method.to_string(),
                    }
                }
                (None, _) => method.to_string(),
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
/// enclosing nodes), collecting path-shaped string literals (`string_literal`,
/// `verbatim_string_literal`, `raw_string_literal`), and SQL-shaped strings.
/// Interpolated strings (`$"..."`) are skipped by `build_raw_path_literal` and
/// by the SQL extractor (different node kind).
pub fn extract_csharp_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let local_types = collect_local_types(root, source);
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "invocation_expression" => {
                if let Some(callee) = csharp_callee(n, source, &local_types, nodes) {
                    attach_to_enclosing(start_pos(n), callee, nodes);
                }
            }
            "object_creation_expression" => {
                // Encoded as a construction: a plain C# call never constructs.
                let callee = n
                    .child_by_field_name("type")
                    .and_then(|t| construction_call(t.utf8_text(source).ok()?));
                if let Some(callee) = callee {
                    attach_to_enclosing(start_pos(n), callee, nodes);
                }
            }
            "string_literal" | "verbatim_string_literal" | "raw_string_literal" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same node kinds, separate filter.
                let raw_bytes = &source[n.start_byte()..n.end_byte()];
                if let Ok(raw) = std::str::from_utf8(raw_bytes) {
                    if let Some(value) = strip_csharp_string_value(raw) {
                        if let Some(sql_ref) = crate::sql_literal::try_sql_ref(
                            value,
                            n,
                            enclosing_symbol_and_owner_pub(n, source),
                        ) {
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

/// Whether a type enclosing `pos` declares a field or property named `name`
/// (both are `Property` nodes). A nested type's members count for the whole
/// outer type: that only keeps more calls on the bare member.
fn declares_member(nodes: &[RawNode], pos: Pos, name: &str) -> bool {
    let is_type = |n: &RawNode| {
        matches!(
            n.kind,
            NodeKind::Class | NodeKind::Struct | NodeKind::Interface
        )
    };
    nodes
        .iter()
        .filter(|m| m.kind == NodeKind::Property && m.name == name)
        .any(|m| {
            let at = (m.span.0, m.span.1);
            nodes
                .iter()
                .any(|t| is_type(t) && span_holds(t.span, pos) && span_holds(t.span, at))
        })
}

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

fn enclosing_base_name(nodes: &[RawNode], pos: Pos) -> Option<String> {
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

    /// An identifier receiver that is no local, parameter or member reaches
    /// the resolver as a `from` receiver; a declared name of any type, and a
    /// property, keep the bare member.
    #[test]
    fn test_csharp_callee_undeclared_identifier_receiver_emits_from_receiver() {
        let src = r#"
class App {
    Helper Tool { get; }
    void Use(List<int> items) {
        var local = Make();
        StringAssert.AreEqual("a", "b");
        local.Run();
        items.Add(1);
        Tool.Go();
        foreach (var each in items) { each.Stop(); }
        items.ForEach(x => x.Halt());
    }
}
"#;
        let graph = parse(src);
        // The lambda body's call attaches to an anonymous node.
        let mut got: Vec<String> = graph.nodes.iter().flat_map(|n| n.calls.clone()).collect();
        let mut want: Vec<String> = vec![
            "Make".into(),
            ecp_core::analyzer::types::CallSite::from_receiver("StringAssert", "AreEqual"),
            "Run".into(),
            "Add".into(),
            "Go".into(),
            "Stop".into(),
            "ForEach".into(),
            "Halt".into(),
        ];
        got.sort();
        want.sort();
        assert_eq!(got, want);
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
