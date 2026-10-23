//! Receiver-type binding for Go method calls.
//!
//! Go methods carry an explicit receiver declaration:
//!   `func (r *Dog) Bark() { r.Fetch() }`
//!
//! This module:
//! 1. Walks `method_declaration` nodes to build a map
//!    `method_name → receiver_type` (stripping the pointer `*` prefix).
//! 2. Collects local var-type bindings inside each function/method body:
//!    - `var x Dog` / `var x *Dog`      → x → "Dog"
//!    - parameter `d *Dog` / `d Dog`    → d → "Dog"
//!    - short-var `:=` with a composite literal `Dog{...}` → x → "Dog"
//! 3. Replaces the bare `extract_calls` pass for Go so that
//!    `d.Bark()` inside `(*Dog).Fetch` is recorded as `"Dog.Bark"` rather
//!    than just `"Bark"`, feeding the resolver's Tier 2.5 qualifier lookup.

use super::path_literals::{build_raw_path_literal, strip_go_string_value};
use crate::calls::{attach_to_enclosing, node_range, range_width, start_pos, Pos, PosRange};
use ecp_core::analyzer::types::{CallSite, RawImport, RawNode, RawPathLiteral, RawSqlRef};
use std::collections::HashMap;
use tree_sitter::Node;

// ── receiver map ─────────────────────────────────────────────────────────────

/// `method_name → receiver_type` built from the file's `method_declaration`
/// nodes.  Used to resolve `self_name.Method()` inside the method body.
#[derive(Debug, Default)]
pub struct ReceiverMap {
    /// Maps method name → bare receiver type (pointer stripped).
    pub entries: HashMap<String, String>,
}

/// Walk every `method_declaration` in the file and record
/// `method_name → receiver_type` (pointer `*` stripped).
pub fn build_receiver_map(root: Node<'_>, source: &[u8]) -> ReceiverMap {
    let mut entries: HashMap<String, String> = HashMap::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "method_declaration" {
            if let Some((method, recv_ty)) = extract_method_receiver(&n, source) {
                entries.insert(method, recv_ty);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    ReceiverMap { entries }
}

/// Extract the receiver type from a `method_declaration` node.
///
/// Used at emit time (rather than a post-loop map lookup) so that two
/// methods with the same name on different receiver types are correctly
/// distinguished: `func (f *Foo) Run()` and `func (b *Bar) Run()` both
/// emit `Run`, each with the correct `owner_class`.
pub fn receiver_type_from_method_decl(node: Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() != "method_declaration" {
        return None;
    }
    let receiver_list = node.child_by_field_name("receiver")?;
    let param = receiver_list
        .children(&mut receiver_list.walk())
        .find(|c| c.kind() == "parameter_declaration")?;
    let ty_node = param.child_by_field_name("type")?;
    bare_type_name(ty_node, source)
}

/// Extract `(method_name, receiver_type)` from a single `method_declaration`.
/// Returns `None` when source bytes can't be decoded as UTF-8.
fn extract_method_receiver(node: &Node<'_>, source: &[u8]) -> Option<(String, String)> {
    // field "name" → the method identifier
    let name_node = node.child_by_field_name("name")?;
    let method_name = node_text(name_node, source)?;

    // field "receiver" → parameter_list containing the receiver declaration
    let receiver_list = node.child_by_field_name("receiver")?;

    // The receiver list has exactly one parameter_declaration child.
    let param = receiver_list
        .children(&mut receiver_list.walk())
        .find(|c| c.kind() == "parameter_declaration")?;

    // field "type" → type_identifier or pointer_type(type_identifier)
    let ty_node = param.child_by_field_name("type")?;
    let recv_type = bare_type_name(ty_node, source)?;

    Some((method_name, recv_type))
}

// ── local type scope ──────────────────────────────────────────────────────────

/// Per-function scope: `var_name → type_name`.
#[derive(Debug, Default)]
struct Scope {
    range: PosRange,
    bindings: HashMap<String, String>,
    /// Every name a parameter, `var` or `:=` binds, typed or not, with the
    /// position it takes effect from.
    values: HashMap<String, Pos>,
}

/// Collection of all scopes in the file.
#[derive(Debug, Default)]
pub struct LocalTypes {
    scopes: Vec<Scope>,
}

impl LocalTypes {
    /// Find the type bound to `var` at `pos`. The innermost visible binding
    /// wins even when it has no reducible type, so an outer typed binding
    /// cannot leak through a local shadow.
    pub fn lookup(&self, pos: Pos, var: &str) -> Option<&str> {
        let mut best: Option<&Scope> = None;
        let mut best_width = (u32::MAX, i64::MAX);
        for scope in &self.scopes {
            if scope.range.0 <= pos
                && pos <= scope.range.1
                && scope.values.get(var).is_some_and(|from| *from <= pos)
            {
                let w = range_width(scope.range);
                if w < best_width {
                    best_width = w;
                    best = Some(scope);
                }
            }
        }
        best.and_then(|scope| scope.bindings.get(var).map(String::as_str))
    }

    /// Whether a parameter or local declared before `pos` binds `name`.
    pub fn binds_local(&self, pos: Pos, name: &str) -> bool {
        self.scopes.iter().any(|scope| {
            scope.range.0 <= pos
                && pos <= scope.range.1
                && scope.values.get(name).is_some_and(|from| *from <= pos)
        })
    }
}

/// Build `LocalTypes` by scanning every `function_declaration` and
/// `method_declaration` in the file.
pub fn collect_local_types(root: Node<'_>, source: &[u8], recv_map: &ReceiverMap) -> LocalTypes {
    let mut scopes: Vec<Scope> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "function_declaration" | "method_declaration" => {
                collect_function_scope(n, source, &mut scopes);

                // Descend into body for nested function literals.
                if let Some(body) = n.child_by_field_name("body") {
                    let mut c = body.walk();
                    for child in body.children(&mut c) {
                        stack.push(child);
                    }
                }
                continue;
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    // Supplement: for each method, add a scope that maps the receiver var name
    // to the receiver type (already included via collect_params above).
    // recv_map is currently unused after the refactor but kept for API stability.
    let _ = recv_map;
    LocalTypes { scopes }
}

fn collect_function_scope(function: Node<'_>, source: &[u8], scopes: &mut Vec<Scope>) {
    let mut bindings = HashMap::new();
    let mut values = HashMap::new();
    let scope_start = start_pos(function);

    if function.kind() == "method_declaration" {
        if let Some(receiver) = function.child_by_field_name("receiver") {
            collect_params(&receiver, source, &mut bindings);
            collect_param_names(&receiver, source, scope_start, &mut values);
        }
    }
    if let Some(params) = function.child_by_field_name("parameters") {
        collect_params(&params, source, &mut bindings);
        collect_param_names(&params, source, scope_start, &mut values);
    }
    if let Some(body) = function.child_by_field_name("body") {
        collect_scope_body(&body, source, &mut bindings, &mut values);
        collect_nested_scopes(body, source, scopes);
    }
    if !bindings.is_empty() || !values.is_empty() {
        scopes.push(Scope {
            range: node_range(function),
            bindings,
            values,
        });
    }
}

fn collect_literal_scope(literal: Node<'_>, source: &[u8], scopes: &mut Vec<Scope>) {
    let mut bindings = HashMap::new();
    let mut values = HashMap::new();
    let scope_start = start_pos(literal);
    if let Some(params) = literal.child_by_field_name("parameters") {
        collect_params(&params, source, &mut bindings);
        collect_param_names(&params, source, scope_start, &mut values);
    }
    if let Some(body) = literal.child_by_field_name("body") {
        collect_scope_body(&body, source, &mut bindings, &mut values);
        collect_nested_scopes(body, source, scopes);
    }
    if !bindings.is_empty() || !values.is_empty() {
        scopes.push(Scope {
            range: node_range(literal),
            bindings,
            values,
        });
    }
}

fn is_scope_boundary(kind: &str) -> bool {
    matches!(
        kind,
        "block"
            | "if_statement"
            | "for_statement"
            | "expression_case"
            | "type_case"
            | "communication_case"
            | "default_case"
    )
}

fn collect_nested_scopes(node: Node<'_>, source: &[u8], scopes: &mut Vec<Scope>) {
    let mut stack = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        stack.push(child);
    }
    while let Some(node) = stack.pop() {
        if node.kind() == "func_literal" {
            collect_literal_scope(node, source, scopes);
            continue;
        }
        if is_scope_boundary(node.kind()) {
            collect_block_scope(node, source, scopes);
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn collect_block_scope(node: Node<'_>, source: &[u8], scopes: &mut Vec<Scope>) {
    let mut bindings = HashMap::new();
    let mut values = HashMap::new();
    collect_scope_body(&node, source, &mut bindings, &mut values);
    collect_nested_scopes(node, source, scopes);
    if !bindings.is_empty() || !values.is_empty() {
        scopes.push(Scope {
            range: node_range(node),
            bindings,
            values,
        });
    }
}

/// Collect typed parameter declarations from a `parameter_list`.
/// Handles: `d Dog`, `d *Dog`, `d, e Dog` (multiple names, same type).
fn collect_params(params: &Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut c = params.walk();
    for p in params.children(&mut c) {
        if p.kind() != "parameter_declaration" {
            continue;
        }
        let Some(ty_node) = p.child_by_field_name("type") else {
            continue;
        };
        let Some(ty) = bare_type_name(ty_node, source) else {
            continue;
        };
        // A parameter_declaration may have multiple names: `a, b Dog`
        let mut nc = p.walk();
        for child in p.children(&mut nc) {
            if child.kind() == "identifier" {
                if let Some(name) = node_text(child, source) {
                    out.insert(name, ty.clone());
                }
            }
        }
    }
}

/// Walk a function body and collect:
/// - `var_declaration` → `var x Dog` / `var x *Dog`
/// - `short_var_declaration` where the RHS is a composite literal → `x := Dog{}`
fn collect_scope_body(
    body: &Node<'_>,
    source: &[u8],
    out: &mut HashMap<String, String>,
    values: &mut HashMap<String, Pos>,
) {
    let mut stack: Vec<Node<'_>> = vec![*body];
    while let Some(n) = stack.pop() {
        // Nested blocks and function literals own their bindings. They are
        // collected separately so their shadows do not escape their scope.
        if n != *body
            && (n.kind() == "func_literal"
                || n.kind() == "function_declaration"
                || is_scope_boundary(n.kind()))
        {
            continue;
        }
        match n.kind() {
            "var_declaration" => {
                let mut c = n.walk();
                for child in n.children(&mut c) {
                    if child.kind() == "var_spec" {
                        collect_var_spec(&child, source, out);
                        record_names(&child, source, start_pos(child), values);
                    }
                }
            }
            "short_var_declaration" => {
                collect_short_var(&n, source, out);
                if let Some(left) = n.child_by_field_name("left") {
                    record_names(&left, source, start_pos(n), values);
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

/// Record the identifier children of `node` as bound from `from`; the first
/// declaration of a name wins.
fn record_names(node: &Node<'_>, source: &[u8], from: Pos, out: &mut HashMap<String, Pos>) {
    let mut c = node.walk();
    for child in node.children(&mut c) {
        if child.kind() == "identifier" {
            if let Some(name) = node_text(child, source) {
                let slot = out.entry(name).or_insert(from);
                *slot = (*slot).min(from);
            }
        }
    }
}

/// Record every parameter name of a `parameter_list`, typed or not.
fn collect_param_names(
    params: &Node<'_>,
    source: &[u8],
    from: Pos,
    out: &mut HashMap<String, Pos>,
) {
    let mut c = params.walk();
    for p in params.children(&mut c) {
        if matches!(
            p.kind(),
            "parameter_declaration" | "variadic_parameter_declaration"
        ) {
            record_names(&p, source, from, out);
        }
    }
}

/// `var_spec` node: `name : type` or `name = expr` (no type, skip).
fn collect_var_spec(spec: &Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let Some(ty_node) = spec.child_by_field_name("type") else {
        return;
    };
    let Some(ty) = bare_type_name(ty_node, source) else {
        return;
    };
    // Names are identifier children before the type node.
    let mut c = spec.walk();
    for child in spec.children(&mut c) {
        if child.kind() == "identifier" {
            if let Some(name) = node_text(child, source) {
                out.insert(name, ty.clone());
            }
        }
    }
}

/// `short_var_declaration`: `left := right`.
/// Only handle the case where right is a composite literal so we can infer
/// the type from the literal's type name. e.g. `d := Dog{Name: "Spot"}`.
fn collect_short_var(node: &Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    let Some(right) = node.child_by_field_name("right") else {
        return;
    };
    // right is an `expression_list`; grab first child.
    let first_rhs = right.named_child(0).unwrap_or(right);
    // composite_literal: type_name { ... }
    if first_rhs.kind() != "composite_literal" {
        return;
    }
    let Some(ty_node) = first_rhs.child_by_field_name("type") else {
        return;
    };
    let Some(ty) = bare_type_name(ty_node, source) else {
        return;
    };
    // left is an `expression_list`; bind all simple identifiers.
    let mut c = left.walk();
    for child in left.children(&mut c) {
        if child.kind() == "identifier" {
            if let Some(name) = node_text(child, source) {
                out.insert(name, ty.clone());
            }
        }
    }
}

// ── call extraction ───────────────────────────────────────────────────────────

/// Walk the Go AST once, attaching callee names to enclosing
/// function/method nodes (with receiver-type binding) and collecting
/// path-shaped string literals (`interpreted_string_literal` /
/// `raw_string_literal`) and SQL-shaped string literals.
pub fn extract_go_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    local_types: &LocalTypes,
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    extract_go_calls_and_path_literals_with_imports(root, source, nodes, local_types, &[])
}

pub(super) fn extract_go_calls_and_path_literals_with_imports(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    local_types: &LocalTypes,
    imports: &[RawImport],
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call_expression" => {
                if let Some(callee) = go_callee_name(n, source, local_types, imports) {
                    attach_to_enclosing(start_pos(n), callee, nodes);
                }
            }
            "interpreted_string_literal" | "raw_string_literal" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same string node, separate filter.
                let raw_bytes = &source[n.start_byte()..n.end_byte()];
                if let Ok(raw) = std::str::from_utf8(raw_bytes) {
                    if let Some(value) = strip_go_string_value(raw) {
                        if let Some(sql_ref) = crate::sql_literal::try_sql_ref(
                            value,
                            n,
                            super::path_literals::enclosing_symbol_and_owner_pub(n, source),
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

/// Derive the callee name for a `call_expression` node.
///
/// - `foo()` → `"foo"`
/// - `obj.Method()` where `obj`'s type is known → `"Type.Method"`
/// - `obj.Method()` where type is unknown → an untyped member call
fn go_callee_name(
    call: Node<'_>,
    source: &[u8],
    locals: &LocalTypes,
    imports: &[RawImport],
) -> Option<String> {
    let mut function = call.child_by_field_name("function")?;
    while function.kind() == "parenthesized_expression" {
        function = function.named_child(0)?;
    }
    match function.kind() {
        "func_literal" => None,
        "identifier" => node_text(function, source),
        "selector_expression" => {
            // selector_expression: operand . field
            let field = function.child_by_field_name("field")?;
            let method_name = node_text(field, source)?;
            if let Some(operand) = function.child_by_field_name("operand") {
                if operand.kind() == "identifier" {
                    let var_name = node_text(operand, source)?;
                    let pos = start_pos(call);
                    if let Some(ty) = locals.lookup(pos, &var_name) {
                        return Some(format!("{ty}.{method_name}"));
                    }
                    if !locals.binds_local(pos, &var_name)
                        && imports.iter().any(|import| {
                            import.alias.as_deref() == Some(var_name.as_str())
                                || import.alias.is_none()
                        })
                    {
                        return Some(CallSite::from_receiver(&var_name, &method_name));
                    }
                    return Some(CallSite::untyped_member(&method_name));
                }
                // `NewFoo().Bar()`: the resolver types the operand by the
                // callee's return type.
                // A callee that a parameter or local binds is a closure, not
                // the same-named function whose return type would type it.
                let calls_local = operand.kind() == "call_expression"
                    && operand
                        .child_by_field_name("function")
                        .filter(|f| f.kind() == "identifier")
                        .and_then(|f| node_text(f, source))
                        .is_some_and(|name| locals.binds_local(start_pos(operand), &name));
                if operand.kind() == "call_expression" && !calls_local {
                    if let Some(inner) =
                        CallSite::nested_chain(|| go_callee_name(operand, source, locals, imports))
                    {
                        let fallback = CallSite::untyped_member(&method_name);
                        return Some(CallSite::chained_member(&inner, &fallback));
                    }
                }
            }
            // The receiver syntax still distinguishes this from a bare call.
            Some(CallSite::untyped_member(&method_name))
        }
        _ => {
            // Last resort: full text, then last segment after `.`.
            let text = node_text(function, source)?;
            let after_dot = text.rsplit_once('.').map(|(_, t)| t).unwrap_or(&text);
            let id: String = after_dot
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if id.is_empty() {
                None
            } else {
                Some(id)
            }
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Extract the bare type identifier from a type node, stripping pointer `*`.
/// Returns `None` for generic / qualified types (they can't be matched against
/// a single class name in the resolver anyway).
fn bare_type_name(ty_node: Node<'_>, source: &[u8]) -> Option<String> {
    match ty_node.kind() {
        "type_identifier" => node_text(ty_node, source),
        "pointer_type" => {
            // pointer_type has one named child: the underlying type.
            let inner = ty_node.named_child(0)?;
            if inner.kind() == "type_identifier" {
                node_text(inner, source)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Decode the UTF-8 text span of `node` from `source`.
fn node_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
        .ok()
        .map(str::to_string)
}
