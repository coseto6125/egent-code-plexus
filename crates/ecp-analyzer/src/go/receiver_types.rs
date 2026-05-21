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

use crate::calls::attach_to_enclosing;
use ecp_core::analyzer::types::RawNode;
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
    start_row: u32,
    end_row: u32,
    bindings: HashMap<String, String>,
}

/// Collection of all scopes in the file.
#[derive(Debug, Default)]
pub struct LocalTypes {
    scopes: Vec<Scope>,
}

impl LocalTypes {
    /// Find the type bound to `var` at `line`, picking the smallest enclosing
    /// scope that defines it (innermost wins for closures / nested functions).
    pub fn lookup(&self, line: u32, var: &str) -> Option<&str> {
        let mut best: Option<&str> = None;
        let mut best_width = u32::MAX;
        for scope in &self.scopes {
            if scope.start_row <= line && line <= scope.end_row {
                if let Some(ty) = scope.bindings.get(var) {
                    let w = scope.end_row - scope.start_row;
                    if w < best_width {
                        best_width = w;
                        best = Some(ty.as_str());
                    }
                }
            }
        }
        best
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
                let mut bindings: HashMap<String, String> = HashMap::new();

                // For method_declaration: the receiver var itself is in scope.
                if n.kind() == "method_declaration" {
                    if let Some(recv_list) = n.child_by_field_name("receiver") {
                        collect_params(&recv_list, source, &mut bindings);
                    }
                    // Also bind the receiver var to the receiver type via recv_map.
                    // (Already handled by collect_params which reads the type field.)
                }

                // Collect typed parameters from the function signature.
                if let Some(params) = n.child_by_field_name("parameters") {
                    collect_params(&params, source, &mut bindings);
                }

                // Collect var declarations and short-var assignments in the body.
                if let Some(body) = n.child_by_field_name("body") {
                    collect_body_bindings(&body, source, &mut bindings);
                }

                if !bindings.is_empty() {
                    scopes.push(Scope {
                        start_row: n.start_position().row as u32,
                        end_row: n.end_position().row as u32,
                        bindings,
                    });
                }

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
fn collect_body_bindings(body: &Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![*body];
    while let Some(n) = stack.pop() {
        // Don't descend into nested function literals (they get their own scope).
        if n.kind() == "func_literal" || n.kind() == "function_declaration" {
            continue;
        }
        match n.kind() {
            "var_declaration" => {
                let mut c = n.walk();
                for child in n.children(&mut c) {
                    if child.kind() == "var_spec" {
                        collect_var_spec(&child, source, out);
                    }
                }
            }
            "short_var_declaration" => {
                collect_short_var(&n, source, out);
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
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

/// Walk the Go AST and attach callee names to enclosing function/method nodes.
/// When a call site is `obj.Method()` and `obj`'s type is known (from
/// `local_types`), the callee is recorded as `"Type.Method"` instead of just
/// `"Method"`, enabling the resolver's qualifier-scoped (Tier 2.5) lookup.
pub fn extract_go_calls(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    local_types: &LocalTypes,
) {
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "call_expression" {
            if let Some(callee) = go_callee_name(n, source, local_types) {
                let line = n.start_position().row as u32;
                attach_to_enclosing(line, callee, nodes);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

/// Derive the callee name for a `call_expression` node.
///
/// - `foo()` → `"foo"`
/// - `obj.Method()` where `obj`'s type is known → `"Type.Method"`
/// - `obj.Method()` where type is unknown → `"Method"` (fallback)
fn go_callee_name(call: Node<'_>, source: &[u8], locals: &LocalTypes) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    match function.kind() {
        "identifier" => node_text(function, source),
        "selector_expression" => {
            // selector_expression: operand . field
            let field = function.child_by_field_name("field")?;
            let method_name = node_text(field, source)?;
            if let Some(operand) = function.child_by_field_name("operand") {
                if operand.kind() == "identifier" {
                    let var_name = node_text(operand, source)?;
                    let line = call.start_position().row as u32;
                    if let Some(ty) = locals.lookup(line, &var_name) {
                        return Some(format!("{ty}.{method_name}"));
                    }
                }
            }
            // Fallback: bare method name.
            Some(method_name)
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
