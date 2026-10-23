//! Receiver-type binding for Swift.
//!
//! Rewrites `self.method()`, `super.method()`, and `var.method()` callees
//! to `Type.method` form so the resolver's qualifier-scoped lookup (Tier 2.5)
//! can route to the correct class. Three sources of receiver type:
//!
//! - `self` inside a `class_declaration` body → enclosing class/struct/extension
//!   target type (tree-sitter-swift unifies these three under `class_declaration`).
//! - `super` inside a class with `inheritance_specifier` → first heritage entry.
//! - Local identifier with a known type from a typed `parameter` or a
//!   `property_declaration` with `type_annotation` whose annotation is a
//!   single `type_identifier`.
//!
//! Any other member call keeps its receiver kind for the resolver, so an
//! `x.m()` never binds the caller's own `m` the way an implicit-self `m()`
//! does:
//!
//! - an untyped local, parameter, closure parameter or property of the
//!   enclosing type, and any computed receiver (`a.b().m()`, `self.x.m()`)
//!   → [`CallSite::untyped_member`];
//! - an identifier that is none of these (`Util.make()`) →
//!   [`CallSite::from_receiver`]: the resolver binds `Util.make` when
//!   `Util` names a type, else resolves the bare member.
//!
//! Scope: P0 only handles simple-identifier types. Generic / optional /
//! tuple / function types are skipped — such a receiver is untyped.

use super::path_literals::{
    build_raw_path_literal, enclosing_symbol_and_owner_pub, strip_swift_string_value,
};
use crate::calls::{attach_to_enclosing, node_range, range_width, start_pos, Pos, PosRange};
use ecp_core::analyzer::types::{CallSite, RawNode, RawPathLiteral, RawSqlRef};
use ecp_core::graph::NodeKind;
use std::collections::HashMap;
use tree_sitter::Node;

/// Per-function-scope local var → type map, plus the enclosing class /
/// super-class lookup for `self` / `super`. Scope is keyed by row-span so
/// nested closures inherit outer-scope types via smallest-containing-scope.
/// A declared name whose type is unknown maps to `""`.
// `(start_line, end_line)` half-open span keyed scope tables.
type FnScope = (PosRange, HashMap<String, String>);
type ClassScope = (PosRange, (String, Option<String>));

#[derive(Debug, Default)]
pub struct SwiftBindings {
    /// Function/method scopes: (span_start, span_end, var → type).
    fn_scopes: Vec<FnScope>,
    /// Class/struct/extension scopes: (span_start, span_end, (type_name, superclass_opt)).
    class_scopes: Vec<ClassScope>,
}

impl SwiftBindings {
    fn lookup_local(&self, pos: Pos, var: &str) -> Option<&str> {
        let mut best: Option<&str> = None;
        let mut best_width = (u32::MAX, i64::MAX);
        for ((start, end), map) in &self.fn_scopes {
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

    /// Whether the type enclosing `pos` declares a property `name` in this
    /// file, in its own body or in any extension of it.
    fn declares_member(&self, nodes: &[RawNode], pos: Pos, name: &str) -> bool {
        let Some((owner, _)) = self.enclosing_class(pos) else {
            return false;
        };
        nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Property && n.name == name)
            .any(|n| {
                let at = (n.span.0, n.span.1);
                self.class_scopes
                    .iter()
                    .any(|((start, end), (ty, _))| ty == owner && *start <= at && at <= *end)
            })
    }

    fn enclosing_class(&self, pos: Pos) -> Option<&(String, Option<String>)> {
        let mut best: Option<&(String, Option<String>)> = None;
        let mut best_width = (u32::MAX, i64::MAX);
        for ((start, end), info) in &self.class_scopes {
            if *start <= pos && pos <= *end {
                let w = range_width((*start, *end));
                if w < best_width {
                    best_width = w;
                    best = Some(info);
                }
            }
        }
        best
    }
}

/// Walk the AST collecting class scopes and per-function local-type maps.
pub fn collect_bindings(root: Node<'_>, source: &[u8]) -> SwiftBindings {
    let mut fn_scopes: Vec<FnScope> = Vec::new();
    let mut class_scopes: Vec<ClassScope> = Vec::new();

    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "class_declaration" => {
                // Covers class / struct / enum / actor / extension in tree-sitter-swift.
                if let Some((name, sup)) = swift_class_name_and_super(n, source) {
                    let span = node_range(n);
                    class_scopes.push((span, (name, sup)));
                }
            }
            "function_declaration" | "init_declaration" => {
                let span = node_range(n);
                let mut map: HashMap<String, String> = HashMap::new();
                collect_typed_swift_params(n, source, &mut map);
                if let Some(body) = n.child_by_field_name("body") {
                    collect_typed_swift_properties(body, source, &mut map);
                }
                collect_declared_swift_names(n, source, &mut map);
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

    SwiftBindings {
        fn_scopes,
        class_scopes,
    }
}

/// Extract the type-name and (optional) first heritage entry from a Swift
/// `class_declaration`. Returns None if the name isn't a simple identifier.
fn swift_class_name_and_super(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    let mut name: Option<String> = None;
    let mut sup: Option<String> = None;
    let mut c = node.walk();
    for child in node.children(&mut c) {
        match child.kind() {
            "type_identifier" if name.is_none() => {
                if let Ok(s) = std::str::from_utf8(&source[child.start_byte()..child.end_byte()]) {
                    name = Some(s.to_string());
                }
            }
            "user_type" if name.is_none() => {
                // Extension: `extension Apple { ... }` puts name inside user_type.
                name = extended_type_name(child, source).map(str::to_string);
            }
            "inheritance_specifier" if sup.is_none() => {
                // First named child is the parent type. Extract a type_identifier
                // from the user_type wrapper.
                let mut cc = child.walk();
                for sub in child.children(&mut cc) {
                    if sub.kind() == "user_type" {
                        if let Some(ti) = sub.named_child(0) {
                            if ti.kind() == "type_identifier" {
                                if let Ok(s) =
                                    std::str::from_utf8(&source[ti.start_byte()..ti.end_byte()])
                                {
                                    sup = Some(s.to_string());
                                    break;
                                }
                            }
                        }
                    } else if sub.kind() == "type_identifier" {
                        if let Ok(s) =
                            std::str::from_utf8(&source[sub.start_byte()..sub.end_byte()])
                        {
                            sup = Some(s.to_string());
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    name.map(|n| (n, sup))
}

/// The type an `extension` names: the last segment of its `user_type`
/// (`extension Outer.Keyed` → `Keyed`, `extension Array<Int>` → `Array`),
/// matching the simple name its nested declaration owns members under.
pub(super) fn extended_type_name<'s>(user_type: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    if user_type.kind() == "type_identifier" {
        return user_type.utf8_text(source).ok();
    }
    let mut last = None;
    let mut c = user_type.walk();
    for child in user_type.named_children(&mut c) {
        if child.kind() == "type_identifier" {
            last = Some(child);
        }
    }
    last?.utf8_text(source).ok()
}

/// Every name `fn_node` declares, typed or not, mapped to `""` unless a
/// typed collector already recorded its type: parameters (the internal
/// name, not the argument label), closure parameters, `let` / `var` and
/// `if let` / `guard let` / `for` / `catch` / `case let` bindings. Nested
/// functions keep their own scope; closures share the enclosing one.
fn collect_declared_swift_names(
    fn_node: Node<'_>,
    source: &[u8],
    out: &mut HashMap<String, String>,
) {
    let mut stack: Vec<Node<'_>> = vec![fn_node];
    while let Some(n) = stack.pop() {
        let mut c = n.walk();
        if !c.goto_first_child() {
            continue;
        }
        loop {
            let child = c.node();
            let binds = child.kind() == "simple_identifier"
                && matches!(
                    (n.kind(), c.field_name()),
                    ("parameter" | "lambda_parameter", Some("name"))
                        | (_, Some("bound_identifier"))
                        | ("pattern", None)
                );
            if binds {
                if let Ok(name) = child.utf8_text(source) {
                    out.entry(name.to_string()).or_default();
                }
            }
            if !matches!(child.kind(), "function_declaration" | "init_declaration") {
                stack.push(child);
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Walk the function's children for `parameter` nodes. Swift `parameter`
/// shape: `simple_identifier (external_name)? simple_identifier (name) :
/// user_type/type_identifier (type)`; the binding is the `name` one.
fn collect_typed_swift_params(fn_node: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![fn_node];
    while let Some(n) = stack.pop() {
        // Don't descend into the function body — params live before it.
        if n.kind() == "function_body" {
            continue;
        }
        if n.kind() == "parameter" {
            let mut id: Option<&str> = None;
            let mut ty: Option<&str> = None;
            let mut c = n.walk();
            for child in n.children_by_field_name("name", &mut c) {
                match child.kind() {
                    "simple_identifier" if id.is_none() => {
                        id =
                            std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok();
                    }
                    "user_type" if ty.is_none() => {
                        if let Some(ti) = child.named_child(0) {
                            if ti.kind() == "type_identifier" {
                                ty = std::str::from_utf8(&source[ti.start_byte()..ti.end_byte()])
                                    .ok();
                            }
                        }
                    }
                    "type_identifier" if ty.is_none() => {
                        ty =
                            std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok();
                    }
                    _ => {}
                }
            }
            if let (Some(name), Some(t)) = (id, ty) {
                if is_simple_ident(t) {
                    out.insert(name.to_string(), t.to_string());
                }
            }
            continue;
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

/// Walk a function body for `property_declaration` (let/var) with a
/// `type_annotation`. Skip nested functions — they get their own scope.
fn collect_typed_swift_properties(
    body: Node<'_>,
    source: &[u8],
    out: &mut HashMap<String, String>,
) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_declaration" || n.kind() == "init_declaration" {
            continue;
        }
        if n.kind() == "property_declaration" {
            let mut name: Option<String> = None;
            let mut ty: Option<String> = None;
            let mut c = n.walk();
            for child in n.children(&mut c) {
                match child.kind() {
                    "pattern" if name.is_none() => {
                        if let Some(ident) = child.named_child(0) {
                            if ident.kind() == "simple_identifier" {
                                if let Ok(s) = std::str::from_utf8(
                                    &source[ident.start_byte()..ident.end_byte()],
                                ) {
                                    name = Some(s.to_string());
                                }
                            }
                        }
                    }
                    "type_annotation" if ty.is_none() => {
                        let mut cc = child.walk();
                        for sub in child.children(&mut cc) {
                            if sub.kind() == "user_type" {
                                if let Some(ti) = sub.named_child(0) {
                                    if ti.kind() == "type_identifier" {
                                        if let Ok(s) = std::str::from_utf8(
                                            &source[ti.start_byte()..ti.end_byte()],
                                        ) {
                                            ty = Some(s.to_string());
                                            break;
                                        }
                                    }
                                }
                            } else if sub.kind() == "type_identifier" {
                                if let Ok(s) =
                                    std::str::from_utf8(&source[sub.start_byte()..sub.end_byte()])
                                {
                                    ty = Some(s.to_string());
                                    break;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if let (Some(n), Some(t)) = (name, ty) {
                if is_simple_ident(&t) {
                    out.insert(n, t);
                }
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

fn is_simple_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// First named child of `node` whose kind satisfies `pred`. Uses a fresh
/// cursor that is dropped before the returned node — avoids borrow-checker
/// lifetime tangles where the cursor would outlive the calling scope.
fn first_named_where<'a, F>(node: Node<'a>, pred: F) -> Option<Node<'a>>
where
    F: Fn(&Node<'a>) -> bool,
{
    let mut c = node.walk();
    let result = node.named_children(&mut c).find(|child| pred(child));
    result
}

/// First child (named or not) whose kind equals `kind`.
fn first_child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut c = node.walk();
    let result = node.children(&mut c).find(|child| child.kind() == kind);
    result
}

/// Walk the Swift AST once, attaching call sites to enclosing functions
/// (with receiver binding for `self.` / `super.` / typed var navigation)
/// and collecting path-shaped string literals (`line_string_literal`,
/// `multi_line_string_literal`, `raw_string_literal`).
pub fn extract_swift_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    bindings: &SwiftBindings,
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call_expression" => {
                if let Some(callee) = swift_callee_name(n, source, bindings, nodes) {
                    attach_to_enclosing(start_pos(n), callee, nodes);
                }
            }
            "line_string_literal" | "multi_line_string_literal" | "raw_string_literal" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same string node, separate filter.
                if let Some(value) = strip_swift_string_value(n, source) {
                    if let Some(sql_ref) = crate::sql_literal::try_sql_ref(
                        value,
                        n,
                        enclosing_symbol_and_owner_pub(n, source),
                    ) {
                        sql_refs.push(sql_ref);
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

fn swift_callee_name(
    call: Node<'_>,
    source: &[u8],
    bindings: &SwiftBindings,
    nodes: &[RawNode],
) -> Option<String> {
    // The callee is the first non-call-suffix named child of `call_expression`.
    let target = first_named_where(call, |ch| ch.kind() != "call_suffix")?;
    let pos = start_pos(call);

    match target.kind() {
        "simple_identifier" => std::str::from_utf8(&source[target.start_byte()..target.end_byte()])
            .ok()
            .map(str::to_string),
        "navigation_expression" => {
            // Extract suffix name: navigation_suffix > simple_identifier
            let suffix = target
                .child_by_field_name("suffix")
                .or_else(|| first_child_of_kind(target, "navigation_suffix"))?;
            let suffix_id = first_child_of_kind(suffix, "simple_identifier")?;
            let method =
                std::str::from_utf8(&source[suffix_id.start_byte()..suffix_id.end_byte()]).ok()?;

            // Determine the receiver: the first named target child of nav_expr
            // (everything before the navigation_suffix). `o?.m()` keeps `o`.
            let Some(recv) = first_named_where(target, |ch| ch.kind() != "navigation_suffix")
            else {
                return Some(method.to_string());
            };
            let enclosing = || bindings.enclosing_class(pos);
            Some(match recv.kind() {
                // `self.m()` / `super.m()` reach the caller's own type, like
                // an implicit-self `m()`.
                "self_expression" => match enclosing() {
                    Some((cls, _)) => format!("{cls}.{method}"),
                    None => method.to_string(),
                },
                "super_expression" => match enclosing() {
                    Some((_, Some(sup))) => format!("{sup}.{method}"),
                    _ => method.to_string(),
                },
                "simple_identifier" => {
                    let var = recv.utf8_text(source).ok()?;
                    match (var, bindings.lookup_local(pos, var)) {
                        ("Self", None) => match enclosing() {
                            Some((cls, _)) => format!("{cls}.{method}"),
                            None => method.to_string(),
                        },
                        (_, Some("")) => CallSite::untyped_member(method),
                        (_, Some(ty)) => format!("{ty}.{method}"),
                        // `$0`: a closure's implicit parameter.
                        (_, None) if var.starts_with('$') => CallSite::untyped_member(method),
                        (_, None) if bindings.declares_member(nodes, pos, var) => {
                            CallSite::untyped_member(method)
                        }
                        (_, None) => CallSite::from_receiver(var, method),
                    }
                }
                // `Foo.make().bar()` / `Foo().bar()`: the resolver types a
                // call receiver by the callee's return type or class.
                "call_expression" => {
                    let untyped = CallSite::untyped_member(method);
                    // A callee that a local or parameter binds
                    // (`let make = { Beta() }`) is a value, not the
                    // same-named function that would type it.
                    let calls_local = first_named_where(recv, |ch| ch.kind() != "call_suffix")
                        .filter(|f| f.kind() == "simple_identifier")
                        .and_then(|f| f.utf8_text(source).ok())
                        .is_some_and(|name| bindings.lookup_local(pos, name).is_some());
                    if calls_local {
                        return Some(untyped);
                    }
                    match CallSite::nested_chain(|| {
                        swift_callee_name(recv, source, bindings, nodes)
                    }) {
                        Some(inner) => CallSite::chained_member(&inner, &untyped),
                        None => untyped,
                    }
                }
                _ => CallSite::untyped_member(method),
            })
        }
        _ => None,
    }
}
