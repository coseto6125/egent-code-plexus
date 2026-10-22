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
//! - `Name.foo()` where `Name` is no local, parameter, import or property
//!   of an enclosing type → [`CallSite::from_receiver`]: the resolver binds
//!   `Name.foo` when `Name` names a type (a same-package `object` or class,
//!   a companion's owner), else resolves the bare member
//!
//! - `Other().m()` where this file declares `class Other` → `Other.m`
//!
//! Falls back to the bare method name for other unresolved receivers, but
//! a computed receiver (`other().m()`) whose `m` an enclosing type declares
//! emits [`CallSite::untyped_member`], so it never binds the caller's own
//! `m` the way an implicit-`this` `m()` does.

use super::path_literals::{
    build_raw_path_literal, enclosing_symbol_and_owner_pub, strip_kotlin_string_value,
};
use crate::calls::{
    attach_to_enclosing, node_range, range_width, span_holds, span_width, start_pos, Pos, PosRange,
};
use ecp_core::analyzer::types::{CallSite, RawNode, RawPathLiteral, RawSqlRef};
use ecp_core::graph::NodeKind;
use std::collections::HashMap;
use tree_sitter::Node;

#[derive(Debug, Default)]
struct LocalTypes {
    scopes: Vec<(PosRange, HashMap<String, String>)>,
    imports: HashMap<String, String>,
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
        // An unknown local type still shadows an imported binding or a
        // typed name of a wider scope.
        best.or_else(|| self.imports.get(var).map(String::as_str))
            .filter(|ty| !ty.is_empty())
    }

    /// Whether a function or class scope holding `pos` declares `var`,
    /// typed or not, or an import binds it.
    fn binds(&self, pos: Pos, var: &str) -> bool {
        self.imports.contains_key(var) || self.binds_local(pos, var)
    }

    /// Whether a function or class scope holding `pos` declares `var`.
    fn binds_local(&self, pos: Pos, var: &str) -> bool {
        self.scopes
            .iter()
            .any(|((start, end), map)| *start <= pos && pos <= *end && map.contains_key(var))
    }
}

/// Walk every `function_declaration`, collecting typed parameters and local
/// `property_declaration` nodes (which represent `val`/`var` declarations),
/// and every class's primary-constructor parameters.
fn collect_local_types(root: Node<'_>, source: &[u8]) -> LocalTypes {
    let mut scopes: Vec<(PosRange, HashMap<String, String>)> = Vec::new();
    let mut imports = HashMap::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "import_header" {
            let mut children = n.named_children(&mut n.walk()).collect::<Vec<_>>();
            // `import a.b.*` binds no name: its last segment is a package,
            // and a receiver of that name is a property or a lambda parameter.
            if children
                .iter()
                .any(|child| child.kind() == "wildcard_import")
            {
                continue;
            }
            let alias = children
                .iter()
                .find(|child| child.kind() == "import_alias")
                .and_then(|child| child.named_child(0));
            if let Some(binding) = alias
                .or_else(|| {
                    children
                        .drain(..)
                        .find(|child| child.kind() == "identifier")
                })
                .and_then(|child| child.utf8_text(source).ok())
                .and_then(|name| name.rsplit('.').next())
            {
                imports.insert(binding.to_string(), binding.to_string());
            }
            continue;
        }
        if n.kind() == "class_declaration" {
            // Primary-constructor parameters are in scope for the whole
            // class body; a `val` / `var` one is also a property, which no
            // Property node records.
            let mut map: HashMap<String, String> = HashMap::new();
            let mut c = n.walk();
            for ctor in n.children(&mut c) {
                if ctor.kind() != "primary_constructor" {
                    continue;
                }
                let mut pc = ctor.walk();
                for param in ctor.children(&mut pc) {
                    let mut ic = param.walk();
                    let name = param
                        .children(&mut ic)
                        .find(|child| child.kind() == "simple_identifier");
                    if let Some(name) = name.and_then(|name| name.utf8_text(source).ok()) {
                        map.insert(name.to_string(), String::new());
                    }
                }
            }
            if !map.is_empty() {
                scopes.push((node_range(n), map));
            }
        }
        if n.kind() == "function_declaration" {
            let scope = node_range(n);
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
                    collect_property_decls(child, source, &mut scopes);
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
    LocalTypes { scopes, imports }
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
        if let Some(name) = name_node.and_then(|name| name.utf8_text(source).ok()) {
            out.insert(name.to_string(), String::new());
        }
        if let (Some(nm), Some(ty)) = (name_node, type_node) {
            if let Some((var, ty_s)) = simple_id_and_usertype(nm, ty, source) {
                out.insert(var, ty_s);
            }
        }
    }
}

/// Walk a `function_body`, collecting `property_declaration` nodes, each
/// in a scope of its own: a local is visible from the end of its
/// declaration to the end of the block that declares it.
/// Does NOT descend into nested `function_declaration` / lambdas.
fn collect_property_decls(
    body: Node<'_>,
    source: &[u8],
    scopes: &mut Vec<(PosRange, HashMap<String, String>)>,
) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_declaration" || n.kind() == "lambda_literal" {
            continue;
        }
        if n.kind() == "property_declaration" {
            let mut out: HashMap<String, String> = HashMap::new();
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
                    if let Some(name) = nm.and_then(|name| name.utf8_text(source).ok()) {
                        out.insert(name.to_string(), String::new());
                    }
                    if let (Some(nm_n), Some(ty_n)) = (nm, ty) {
                        if let Some((var, ty_s)) = simple_id_and_usertype(nm_n, ty_n, source) {
                            out.insert(var, ty_s);
                        }
                    }
                }
            }
            if !out.is_empty() {
                let declared = node_range(n).1;
                let block_end = n.parent().map_or(declared, |block| node_range(block).1);
                scopes.push(((declared, block_end), out));
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

            let pos = start_pos(call);
            let ty: Option<String> = match receiver.kind() {
                "this_expression" => enclosing_class_name(nodes, pos),
                "super_expression" => enclosing_superclass(nodes, pos),
                "simple_identifier" => {
                    let var =
                        std::str::from_utf8(&source[receiver.start_byte()..receiver.end_byte()])
                            .ok()?;
                    // Only a name no scope, import or enclosing type binds
                    // may be a type, which the resolver decides.
                    if !locals.binds(pos, var) && !declares_member(nodes, pos, var) {
                        return Some(CallSite::from_receiver(var, method));
                    }
                    locals.lookup(pos, var).map(str::to_string)
                }
                "call_expression" => constructed_class(receiver, source, nodes)
                    .filter(|name| !locals.binds(pos, name) && !declares_member(nodes, pos, name)),
                _ => None,
            };
            let computed = !matches!(
                receiver.kind(),
                "this_expression" | "super_expression" | "simple_identifier"
            );
            let site = match ty {
                Some(t) => return Some(format!("{t}.{method}")),
                // A computed receiver (`other().m()`) is no `this`: where an
                // enclosing type declares `m`, the bare name would bind that
                // member. Elsewhere the bare name still reaches an extension
                // function, which Kotlin declares free and an untyped member
                // call never binds.
                None if computed && declares(nodes, pos, method, NodeKind::is_callable) => {
                    CallSite::untyped_member(method)
                }
                None => method.to_string(),
            };
            // A call receiver the parser cannot type: the resolver types it
            // by the callee's return type.
            // A callee that is a local or property (`val Other = { .. }`)
            // is a value, not a declaration whose return type types it.
            let calls_value = || {
                receiver
                    .named_child(0)
                    .filter(|f| f.kind() == "simple_identifier")
                    .and_then(|f| f.utf8_text(source).ok())
                    .is_some_and(|name| {
                        locals.binds_local(pos, name) || declares_member(nodes, pos, name)
                    })
            };
            Some(match receiver.kind() {
                "call_expression" if !calls_value() => {
                    match CallSite::nested_chain(|| kotlin_callee(receiver, source, locals, nodes))
                    {
                        Some(inner) => CallSite::chained_member(&inner, &site),
                        None => site,
                    }
                }
                _ => site,
            })
        }
        _ => None,
    }
}

/// Walk the AST once, extracting Kotlin `call_expression` nodes with
/// receiver-type binding and collecting path-shaped string literals
/// (`string_literal` / `multiline_string_literal`, interpolated forms
/// filtered out in `build_raw_path_literal`) and SQL-shaped string literals.
pub fn extract_kotlin_calls_and_path_literals(
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
            "call_expression" => {
                if let Some(callee) = kotlin_callee(n, source, &local_types, nodes) {
                    attach_to_enclosing(start_pos(n), callee, nodes);
                }
            }
            "string_literal" | "multiline_string_literal" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same string node, separate filter.
                // Skip interpolated strings (have `interpolation` children).
                let has_interpolation = {
                    let mut c = n.walk();
                    let x = n.children(&mut c).any(|ch| ch.kind() == "interpolation");
                    x
                };
                if !has_interpolation {
                    let raw_bytes = &source[n.start_byte()..n.end_byte()];
                    if let Ok(raw) = std::str::from_utf8(raw_bytes) {
                        if let Some(value) = strip_kotlin_string_value(raw) {
                            if !value.contains("${") {
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

/// Whether a type enclosing `pos` declares a property `name` as its own
/// member. A property of a nested type that does not enclose `pos` binds
/// nothing there, so it shadows no class of that name.
fn declares_member(nodes: &[RawNode], pos: Pos, name: &str) -> bool {
    nodes
        .iter()
        .filter(|m| m.kind == NodeKind::Property && m.name == name)
        .any(|m| {
            let at = (m.span.0, m.span.1);
            nodes
                .iter()
                .filter(|t| is_type(t) && span_holds(t.span, at))
                .min_by_key(|t| span_width(t.span))
                .is_some_and(|owner| span_holds(owner.span, pos))
        })
}

fn is_type(n: &RawNode) -> bool {
    matches!(
        n.kind,
        NodeKind::Class | NodeKind::Interface | NodeKind::Enum
    )
}

/// Whether a type enclosing `pos` declares a member `name` of a kind
/// `member` accepts. A nested type's members count for the whole outer
/// type: that only keeps more calls on the bare member.
fn declares(nodes: &[RawNode], pos: Pos, name: &str, member: fn(NodeKind) -> bool) -> bool {
    nodes
        .iter()
        .filter(|m| member(m.kind) && m.name == name)
        .any(|m| {
            let at = (m.span.0, m.span.1);
            nodes
                .iter()
                .any(|t| is_type(t) && span_holds(t.span, pos) && span_holds(t.span, at))
        })
}

/// The class a constructor call receiver (`Other()`) builds, when this
/// file declares a class of that name. A local, parameter or property of
/// that name shadows the class (`val Other = { Actual() }`); the caller
/// checks those.
fn constructed_class(call: Node<'_>, source: &[u8], nodes: &[RawNode]) -> Option<String> {
    let callee = call
        .named_child(0)
        .filter(|n| n.kind() == "simple_identifier")?;
    let name = callee.utf8_text(source).ok()?;
    nodes
        .iter()
        .any(|n| n.kind == NodeKind::Class && n.name == name)
        .then(|| name.to_string())
}

fn enclosing_class_name(nodes: &[RawNode], pos: Pos) -> Option<String> {
    let mut best: Option<((u32, i64), &str)> = None;
    for n in nodes {
        if !matches!(n.kind, NodeKind::Class) {
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

    /// `import a.b.*` binds no receiver: a property named like the package
    /// keeps the bare member name.
    #[test]
    fn test_kotlin_callee_wildcard_package_named_receiver_emits_bare_method() {
        let src = r#"
package com.app.ui
import com.app.repository.*
class VM(private val repository: UserRepository) {
    fun load() {
        repository.fetchUsers()
    }
}
"#;
        let graph = parse(src);
        let load = graph
            .nodes
            .iter()
            .find(|n| n.name == "load")
            .expect("load function not found");
        assert_eq!(load.calls, ["fetchUsers"]);
    }

    /// An explicit class import still qualifies a static-style receiver.
    #[test]
    fn test_kotlin_callee_class_import_receiver_emits_qualified_method() {
        let src = "import pkg.Helper\nfun go() { Helper.work() }\n";
        let graph = parse(src);
        let go = graph
            .nodes
            .iter()
            .find(|n| n.name == "go")
            .expect("go function not found");
        assert_eq!(go.calls, ["Helper.work"]);
    }
}
