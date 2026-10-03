//! Local-scope receiver type binding for Python.
//!
//! Collects simple-identifier type annotations on (a) typed parameters
//! `def f(x: T)` and (b) typed assignments `x: T = ...` inside function
//! bodies. The resulting `LocalTypes` map is consulted during call
//! extraction so `var.method()` can be rewritten to `Type.method` for
//! the resolver's qualifier-scoped lookup (Tier 2.5).
//!
//! Scope: P0 only handles single-identifier receivers (`x.method`) with
//! single-identifier type annotations (`Apple`, not `dict[str, Apple]`).
//! Generic / subscripted / forward-reference types are skipped — the
//! call falls back to the bare member name as before.

use super::path_literals::build_raw_path_literal;
use crate::calls::{attach_to_enclosing, CallSiteIndex};
use crate::framework_helpers::strip_python_string_quotes;
use ecp_core::analyzer::types::{CallSite, RawNode, RawPathLiteral, RawSqlRef};
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

/// Map of nested function scopes (by row span) to their var→type bindings.
/// Lookup picks the smallest containing scope that has the variable, so
/// closures correctly inherit outer-scope annotations.
#[derive(Debug, Default)]
pub struct LocalTypes {
    scopes: Vec<((u32, u32), HashMap<String, String>)>,
    /// Names an `import` binds anywhere in the file: a receiver rooted in
    /// one (`widget.Widget()`) names a module member, not an object's method.
    imported: HashSet<String>,
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

    /// True when `receiver` (`m`, `m.sub`) is rooted in an imported name.
    fn is_imported_path(&self, receiver: Node<'_>, source: &[u8]) -> bool {
        let mut root = receiver;
        while root.kind() == "attribute" {
            let Some(object) = root.child_by_field_name("object") else {
                return false;
            };
            root = object;
        }
        root.kind() == "identifier"
            && root
                .utf8_text(source)
                .is_ok_and(|name| self.imported.contains(name))
    }
}

/// Walk every `function_definition` node, collecting typed parameters and
/// annotated assignments inside the function body, and every name an
/// `import` statement binds.
pub fn collect_local_types(root: Node<'_>, source: &[u8]) -> LocalTypes {
    let mut scopes: Vec<((u32, u32), HashMap<String, String>)> = Vec::new();
    let mut imported: HashSet<String> = HashSet::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "import_statement" | "import_from_statement") {
            collect_import_bindings(n, source, &mut imported);
        }
        if n.kind() == "function_definition" {
            let fn_span = (n.start_position().row as u32, n.end_position().row as u32);
            let mut map: HashMap<String, String> = HashMap::new();

            if let Some(params) = n.child_by_field_name("parameters") {
                collect_typed_params(params, source, &mut map);
            }

            if let Some(body) = n.child_by_field_name("body") {
                collect_typed_assignments(body, source, &mut map);
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
    LocalTypes { scopes, imported }
}

/// The local names one import statement binds: `import a.b` → `a`,
/// `import a as x` / `from m import n as x` → `x`, `from m import n` → `n`.
fn collect_import_bindings(stmt: Node<'_>, source: &[u8], out: &mut HashSet<String>) {
    let mut c = stmt.walk();
    for name in stmt.children_by_field_name("name", &mut c) {
        let bound = match name.kind() {
            "aliased_import" => name.child_by_field_name("alias"),
            "dotted_name" => name.named_child(0),
            _ => None,
        };
        if let Some(text) = bound.and_then(|b| b.utf8_text(source).ok()) {
            out.insert(text.to_string());
        }
    }
}

/// Extract `typed_parameter` children under a `parameters` node.
/// Tree-sitter-python shape: `typed_parameter` has the identifier as its
/// first named child and a `type` field for the annotation.
fn collect_typed_params(params: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut c = params.walk();
    for p in params.children(&mut c) {
        if p.kind() != "typed_parameter" {
            continue;
        }
        let Some(id) = p.named_child(0) else { continue };
        if id.kind() != "identifier" {
            continue;
        }
        let Some(ty_node) = p.child_by_field_name("type") else {
            continue;
        };
        if let Some((name, ty)) = simple_name_and_type(id, ty_node, source) {
            out.insert(name, ty);
        }
    }
}

/// Walk a function body for `assignment` nodes with a `type` field and a
/// simple-identifier `left`. Descends through compound statements so that
/// annotations inside `if`/`for`/`with` blocks are captured. Does NOT
/// descend into nested `function_definition` — those get their own scope.
fn collect_typed_assignments(body: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_definition" {
            continue;
        }
        if n.kind() == "assignment" {
            if let (Some(left), Some(ty_node)) =
                (n.child_by_field_name("left"), n.child_by_field_name("type"))
            {
                if left.kind() == "identifier" {
                    if let Some((name, ty)) = simple_name_and_type(left, ty_node, source) {
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

/// Extract `(name, type)` only when the type is a single identifier.
/// Generics / subscripts / strings are skipped — they cannot be matched
/// against class names by the resolver.
fn simple_name_and_type(
    name_node: Node<'_>,
    type_node: Node<'_>,
    source: &[u8],
) -> Option<(String, String)> {
    let inner = type_node.named_child(0).unwrap_or(type_node);
    if inner.kind() != "identifier" {
        return None;
    }
    let name = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;
    let ty = std::str::from_utf8(&source[inner.start_byte()..inner.end_byte()]).ok()?;
    if !ty.chars().all(|c| c.is_alphanumeric() || c == '_') || ty.is_empty() {
        return None;
    }
    Some((name.to_string(), ty.to_string()))
}

/// Walk the Python AST once, attaching callees to enclosing functions
/// (with receiver-type binding) and collecting path-shaped string literals
/// as `RawPathLiteral` side-table entries.
///
/// Calls: identifiers/attributes are handled here; other call-target shapes
/// (subscript, lambda, ...) emit no edge, matching the previous catch-all
/// behavior's "last identifier segment" rule for those rare cases.
///
/// Path literals and SQL refs: every `string` node is fed through
/// `path_literals::build_raw_path_literal` (which itself filters out
/// f-strings by checking for `interpolation` children) and through
/// `sql_literal::is_sql_shaped`/`parse_tables` for SQL extraction.
pub fn extract_python_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    locals: &LocalTypes,
    call_sites: &mut CallSiteIndex,
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call" => {
                if let Some(callee) = python_callee_name(n, source, locals) {
                    let line = n.start_position().row as u32;
                    if let Some(site) = attach_to_enclosing(line, callee, nodes) {
                        call_sites.insert(n.id(), site);
                    }
                }
            }
            "string" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same string node, separate filter.
                // Skip f-strings (they have `interpolation` children).
                let has_interpolation = {
                    let mut c = n.walk();
                    let x = n.children(&mut c).any(|ch| ch.kind() == "interpolation");
                    x
                };
                if !has_interpolation {
                    let raw_bytes = &source[n.start_byte()..n.end_byte()];
                    if let Ok(raw) = std::str::from_utf8(raw_bytes) {
                        if let Some(value) = strip_python_string_quotes(raw) {
                            if let Some(sql_ref) = crate::sql_literal::try_sql_ref(
                                value,
                                n,
                                enclosing_symbol_and_owner(n, source),
                            ) {
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

/// Climb from a string literal to find the innermost enclosing
/// `function_definition` (free function or method) and `class_definition`
/// (owner). Returns `(function_name, owner_class)`.
fn enclosing_symbol_and_owner(
    str_node: Node<'_>,
    source: &[u8],
) -> (Option<String>, Option<String>) {
    let mut cur = str_node.parent();
    let mut function_name: Option<String> = None;
    let mut owner: Option<String> = None;

    while let Some(n) = cur {
        match n.kind() {
            "function_definition" if function_name.is_none() => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    function_name =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            "class_definition" if owner.is_none() => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    owner =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                            .ok()
                            .map(str::to_string);
                }
            }
            _ => {}
        }
        cur = n.parent();
    }
    (function_name, owner)
}

fn python_callee_name(call: Node<'_>, source: &[u8], locals: &LocalTypes) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    match function.kind() {
        "identifier" => std::str::from_utf8(&source[function.start_byte()..function.end_byte()])
            .ok()
            .map(str::to_string),
        "attribute" => {
            let attr = function.child_by_field_name("attribute")?;
            let attr_name =
                std::str::from_utf8(&source[attr.start_byte()..attr.end_byte()]).ok()?;
            if let Some(obj) = function.child_by_field_name("object") {
                if obj.kind() == "identifier" {
                    let obj_name =
                        std::str::from_utf8(&source[obj.start_byte()..obj.end_byte()]).ok()?;
                    let line = call.start_position().row as u32;
                    if let Some(ty) = locals.lookup(line, obj_name) {
                        return Some(format!("{ty}.{attr_name}"));
                    }
                }
                // Only a module receiver (`widget.Widget()`) can name a class
                // to construct; any other untyped receiver calls a method,
                // and a class may be spelled in any case (`class widget`).
                if !locals.is_imported_path(obj, source) {
                    return Some(CallSite::untyped_member(attr_name));
                }
            }
            Some(attr_name.to_string())
        }
        _ => None,
    }
}
