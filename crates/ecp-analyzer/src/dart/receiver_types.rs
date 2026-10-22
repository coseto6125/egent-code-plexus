//! Receiver-type binding for Dart.
//!
//! Rewrites three call shapes so the resolver's qualifier-scoped lookup
//! (Tier 2.5) can route to the correct class:
//!
//! - `this.method()` → `Class.method` where `Class` is the enclosing
//!   `class_declaration` / `mixin_declaration`.
//! - `super.method()` → `Super.method` from the class's `superclass`
//!   field's first type identifier.
//! - `obj.method()` where `obj` is a `formal_parameter` or
//!   `initialized_variable_definition` with a single `type_identifier`
//!   annotation → `Type.method`.
//!
//! - `Name.method()` where `Name` is no local, parameter, import prefix or
//!   field of an enclosing type → [`CallSite::from_receiver`]: the resolver
//!   binds `Name.method` when `Name` names a type (`Util.make()`), else
//!   resolves the bare member.
//!
//! - `Other().method()` where this file declares `class Other` →
//!   `Other.method`; any other computed receiver (`a.b.method()`) →
//!   [`CallSite::untyped_member`], which never binds the caller's own
//!   `method`.
//!
//! `obj?.method()` reads its receiver as `obj.method()` does. Scope: P0
//! simple identifier types only. Generic / function / nullable types fall
//! back to the bare member name.

use super::path_literals::{
    build_raw_path_literal, has_template_substitution, strip_dart_string_value,
};
use crate::calls::{
    attach_to_enclosing, node_range, range_width, span_holds, start_pos, Pos, PosRange,
};
use ecp_core::analyzer::types::{CallSite, RawNode, RawPathLiteral, RawSqlRef};
use ecp_core::graph::NodeKind;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

// `(start_line, end_line)` half-open span keyed scope tables. The tuple-of-
// tuples shape mirrors the rest of the receiver_types modules; the type aliases
// silence `clippy::type_complexity` without changing behaviour.
type FnScope = (PosRange, HashMap<String, String>);
type ClassScope = (PosRange, (String, Option<String>));

/// A declared name whose type is unknown maps to `""` in its scope.
#[derive(Debug, Default)]
pub struct DartBindings {
    fn_scopes: Vec<FnScope>,
    class_scopes: Vec<ClassScope>,
    /// `import '..' as p;` prefixes: `p.f()` names a library member.
    prefixes: HashSet<String>,
}

impl DartBindings {
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

pub fn collect_bindings(root: Node<'_>, source: &[u8]) -> DartBindings {
    let mut fn_scopes: Vec<FnScope> = Vec::new();
    let mut class_scopes: Vec<ClassScope> = Vec::new();
    let mut prefixes: HashSet<String> = HashSet::new();

    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "import_specification" => {
                if let Some(alias) = n
                    .child_by_field_name("alias")
                    .and_then(|alias| alias.utf8_text(source).ok())
                {
                    prefixes.insert(alias.to_string());
                }
                continue;
            }
            "class_declaration" | "mixin_declaration" => {
                if let Some((name, sup)) = dart_class_name_and_super(n, source) {
                    let span = node_range(n);
                    class_scopes.push((span, (name, sup)));
                }
            }
            "method_declaration" | "function_declaration" => {
                let span = node_range(n);
                let mut map: HashMap<String, String> = HashMap::new();
                collect_typed_dart_params(n, source, &mut map);
                if !map.is_empty() {
                    fn_scopes.push((span, map));
                }
                collect_typed_dart_locals(n, source, &mut fn_scopes);
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }

    DartBindings {
        fn_scopes,
        class_scopes,
        prefixes,
    }
}

fn dart_class_name_and_super(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    let name_node = node.child_by_field_name("name")?;
    if name_node.kind() != "identifier" {
        return None;
    }
    let name = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;

    let sup = node
        .child_by_field_name("superclass")
        .and_then(|sc| {
            let mut c = sc.walk();
            let mut found: Option<Node<'_>> = None;
            for child in sc.children(&mut c) {
                if child.kind() == "type" {
                    found = Some(child);
                    break;
                }
            }
            drop(c);
            found
        })
        .and_then(|t| t.named_child(0))
        .and_then(|ti| {
            if ti.kind() == "type_identifier" {
                std::str::from_utf8(&source[ti.start_byte()..ti.end_byte()])
                    .ok()
                    .map(str::to_string)
            } else {
                None
            }
        });

    Some((name.to_string(), sup))
}

fn collect_typed_dart_params(fn_node: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![fn_node];
    while let Some(n) = stack.pop() {
        // Avoid descending into nested fn bodies — they get their own scope.
        if n.kind() == "function_body" {
            continue;
        }
        if n.kind() == "formal_parameter" {
            let mut ty: Option<String> = None;
            let mut name: Option<String> = None;
            let mut c = n.walk();
            for child in n.children(&mut c) {
                if child.kind() == "type" && ty.is_none() {
                    if let Some(ti) = child.named_child(0) {
                        if ti.kind() == "type_identifier" {
                            if let Ok(s) =
                                std::str::from_utf8(&source[ti.start_byte()..ti.end_byte()])
                            {
                                ty = Some(s.to_string());
                            }
                        }
                    }
                } else if child.kind() == "identifier" && name.is_none() {
                    if let Ok(s) =
                        std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                    {
                        name = Some(s.to_string());
                    }
                }
            }
            match (name, ty) {
                (Some(n), Some(t)) => {
                    out.insert(n, t);
                }
                (Some(n), None) => {
                    out.entry(n).or_default();
                }
                _ => {}
            }
            continue;
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

/// Each local of `fn_node` in a scope of its own: a local binds from the
/// end of its declaration to the end of the block or loop that declares it.
fn collect_typed_dart_locals(fn_node: Node<'_>, source: &[u8], scopes: &mut Vec<FnScope>) {
    let mut stack: Vec<Node<'_>> = vec![fn_node];
    while let Some(n) = stack.pop() {
        if (n.kind() == "method_declaration" || n.kind() == "function_declaration")
            && !std::ptr::eq(n.id() as *const u8, fn_node.id() as *const u8)
        {
            continue;
        }
        if n.kind() == "initialized_variable_definition" {
            let mut ty: Option<String> = None;
            let mut name: Option<String> = None;
            let mut first_comma = None;
            let mut c = n.walk();
            for child in n.children(&mut c) {
                if child.kind() == "type" && ty.is_none() {
                    if let Some(ti) = child.named_child(0) {
                        if ti.kind() == "type_identifier" {
                            if let Ok(s) =
                                std::str::from_utf8(&source[ti.start_byte()..ti.end_byte()])
                            {
                                ty = Some(s.to_string());
                            }
                        }
                    }
                } else if child.kind() == "," && first_comma.is_none() {
                    first_comma = Some(child);
                } else if child.kind() == "identifier" && name.is_none() {
                    if let Ok(s) =
                        std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                    {
                        name = Some(s.to_string());
                    }
                }
            }
            if let Some(name) = name {
                // `A a = A(), b = a.copy()` binds `a` before `b`'s initializer.
                let declared = first_comma.map_or(node_range(n).1, |comma| node_range(comma).0);
                let mut block = n.parent();
                while let Some(b) = block.filter(|b| {
                    !matches!(b.kind(), "block" | "for_statement" | "for_element") && *b != fn_node
                }) {
                    block = b.parent();
                }
                let block_end = node_range(block.unwrap_or(fn_node)).1;
                scopes.push((
                    (declared, block_end),
                    HashMap::from([(name, ty.unwrap_or_default())]),
                ));
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

/// Walk the Dart AST collecting call sites, path literals, and SQL-shaped
/// string literals.
pub fn extract_dart_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    bindings: &DartBindings,
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call_expression" => {
                if let Some(callee) = dart_callee_name(n, source, bindings, nodes) {
                    attach_to_enclosing(start_pos(n), callee, nodes);
                }
            }
            "string_literal" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same string node, separate filter.
                // Skip interpolated strings (`$var` / `${expr}`) — they can't
                // be statically parsed as SQL.
                if !has_template_substitution(n) {
                    let raw_bytes = &source[n.start_byte()..n.end_byte()];
                    if let Ok(raw) = std::str::from_utf8(raw_bytes) {
                        if let Some(value) = strip_dart_string_value(raw) {
                            let enclosing =
                                super::path_literals::enclosing_symbol_and_owner_pub(n, source);
                            if let Some(sql_ref) =
                                crate::sql_literal::try_sql_ref(value, n, enclosing)
                            {
                                sql_refs.push(sql_ref);
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

fn dart_callee_name(
    call: Node<'_>,
    source: &[u8],
    bindings: &DartBindings,
    nodes: &[RawNode],
) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    let pos = start_pos(call);

    match function.kind() {
        "identifier" => std::str::from_utf8(&source[function.start_byte()..function.end_byte()])
            .ok()
            .map(str::to_string),
        "member_expression" | "null_aware_member_expression" => {
            let prop = function.child_by_field_name("property")?;
            let method = std::str::from_utf8(&source[prop.start_byte()..prop.end_byte()]).ok()?;
            let obj = function.child_by_field_name("object")?;
            match obj.kind() {
                "this" => {
                    if let Some((cls, _)) = bindings.enclosing_class(pos) {
                        return Some(format!("{cls}.{method}"));
                    }
                }
                "super" => {
                    if let Some((_, Some(sup))) = bindings.enclosing_class(pos) {
                        return Some(format!("{sup}.{method}"));
                    }
                }
                "identifier" => {
                    let var = obj.utf8_text(source).ok()?;
                    match bindings.lookup_local(pos, var) {
                        Some("") => {}
                        Some(ty) => return Some(format!("{ty}.{method}")),
                        None if bindings.prefixes.contains(var)
                            || declares_member(nodes, pos, var) => {}
                        // Only a name no scope, prefix or enclosing type
                        // binds may be a type, which the resolver decides.
                        None => return Some(CallSite::from_receiver(var, method)),
                    }
                }
                // A computed receiver (`a.b.m()`, `(x as T).m()`) is no
                // `this`: the bare name would bind the caller's own `m`.
                // Dart reaches no free function through a receiver, and an
                // `extension` member is its extension's.
                _ => {
                    return Some(
                        match constructed_class(obj, source, nodes).filter(|cls| {
                            bindings.lookup_local(pos, cls).is_none()
                                && !declares_member(nodes, pos, cls)
                        }) {
                            Some(cls) => format!("{cls}.{method}"),
                            // `Foo.create().bar()`: the resolver types a call
                            // receiver by the callee's return type.
                            None if obj.kind() == "call_expression"
                                && !calls_local(obj, source, bindings, nodes, pos) =>
                            {
                                let untyped = CallSite::untyped_member(method);
                                match CallSite::nested_chain(|| {
                                    dart_callee_name(obj, source, bindings, nodes)
                                }) {
                                    Some(inner) => CallSite::chained_member(&inner, &untyped),
                                    None => untyped,
                                }
                            }
                            None => CallSite::untyped_member(method),
                        },
                    );
                }
            }
            Some(method.to_string())
        }
        _ => std::str::from_utf8(&source[function.start_byte()..function.end_byte()])
            .ok()
            .map(str::to_string),
    }
}

/// Whether `call` calls a local, a parameter or a field of an enclosing
/// type (`final make = () => A(); make()`): its callee is a value, not a
/// declaration whose return type types the result.
fn calls_local(
    call: Node<'_>,
    source: &[u8],
    bindings: &DartBindings,
    nodes: &[RawNode],
    pos: Pos,
) -> bool {
    call.child_by_field_name("function")
        .filter(|f| f.kind() == "identifier")
        .and_then(|f| f.utf8_text(source).ok())
        .is_some_and(|name| {
            bindings.lookup_local(pos, name).is_some() || declares_member(nodes, pos, name)
        })
}

/// The class a constructor call receiver (`Other()`) builds, when this
/// file declares a class of that name. A local, parameter or field of that
/// name shadows the class (`final Other = () => Actual();`); the caller
/// checks those.
fn constructed_class(call: Node<'_>, source: &[u8], nodes: &[RawNode]) -> Option<String> {
    let callee = Some(call)
        .filter(|call| call.kind() == "call_expression")?
        .child_by_field_name("function")
        .filter(|n| n.kind() == "identifier")?;
    let name = callee.utf8_text(source).ok()?;
    nodes
        .iter()
        .any(|n| n.kind == NodeKind::Class && n.name == name)
        .then(|| name.to_string())
}

/// Whether a type enclosing `pos` declares a field `name`. A nested type's
/// members count for the whole outer type: that only keeps more calls on
/// the bare member.
fn declares_member(nodes: &[RawNode], pos: Pos, name: &str) -> bool {
    let is_type = |n: &RawNode| {
        matches!(
            n.kind,
            NodeKind::Class | NodeKind::Trait | NodeKind::Enum | NodeKind::Interface
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
