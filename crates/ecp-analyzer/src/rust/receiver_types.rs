//! Receiver-type binding for Rust method calls.
//!
//! Rust methods live inside `impl` blocks:
//!   `impl Dog { fn bark(&self) { self.fetch(); } }`
//!   `impl Trait for Dog { fn method(&self) { ... } }`
//!
//! This module:
//! 1. Walks `impl_item` nodes to build a map `fn_name → impl_type`,
//!    covering both inherent impls and trait impls.
//! 2. Collects local bindings inside each function body:
//!    - `let x: Dog = ...` or `let x: &Dog = ...`  → x → "Dog"
//!    - typed function parameters `d: Dog` / `d: &Dog` → d → "Dog"
//! 3. Replaces the shared `extract_calls` for Rust so that:
//!    - `self.method()` inside an impl fn is recorded as `"Type.method"`
//!    - `obj.method()` where `obj`'s type is locally known is recorded as
//!      `"Type.method"` for the resolver's Tier 2.5 qualifier-scoped lookup.

use super::path_literals::build_raw_path_literal;
use crate::calls::attach_to_enclosing;
use ecp_core::analyzer::types::{RawNode, RawPathLiteral};
use std::collections::HashMap;
use tree_sitter::Node;

// ── impl map ─────────────────────────────────────────────────────────────────

/// Maps every `fn` name that appears in an `impl` block to its implementing
/// type.  When two impls define methods with the same name (e.g. `impl Dog`
/// and `impl Cat` both have `new()`), both are stored; callee resolution
/// picks by call-site type when known.
#[derive(Debug, Default)]
pub struct ImplMap {
    /// fn_name → impl type name (e.g. "Dog").  Pointer/ref stripped.
    pub entries: HashMap<String, String>,
}

/// Walk every `impl_item` and record each method's enclosing impl type.
pub fn build_impl_map(root: Node<'_>, source: &[u8]) -> ImplMap {
    let mut entries: HashMap<String, String> = HashMap::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "impl_item" {
            let impl_type = impl_self_type(&n, source);
            if let Some(ty) = impl_type {
                collect_impl_methods(&n, source, &ty, &mut entries);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    ImplMap { entries }
}

/// Extract the self-type name from an `impl_item`.
/// - `impl Dog { ... }` → "Dog"
/// - `impl Trait for Dog { ... }` → "Dog"  (the concrete type)
fn impl_self_type(impl_node: &Node<'_>, source: &[u8]) -> Option<String> {
    // `type` field: the concrete type being implemented (present for both
    // inherent impls and trait impls).
    let ty_node = impl_node.child_by_field_name("type")?;
    bare_type_name(ty_node, source)
}

/// Register every `function_item` directly inside the `impl_item`'s body.
fn collect_impl_methods(
    impl_node: &Node<'_>,
    source: &[u8],
    impl_type: &str,
    out: &mut HashMap<String, String>,
) {
    let Some(body) = impl_node.child_by_field_name("body") else {
        return;
    };
    let mut c = body.walk();
    for child in body.children(&mut c) {
        if child.kind() == "function_item" {
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Some(name) = node_text(name_node, source) {
                    out.insert(name, impl_type.to_string());
                }
            }
        }
    }
}

/// Extract the trait name from `impl Trait for Type`.
///
/// Returns the raw text of the `trait:` field (e.g. `"Display"`,
/// `"zed::Extension"`, `"Iterator<Item = u8>"`).  Returns `None` for
/// inherent impls (`impl Type { … }`) where no `trait:` field is present.
///
/// Callers that need a stable, non-generic token for UID purposes should
/// strip generics themselves; here we return the raw text so the caller
/// can decide.
pub fn impl_trait_name(impl_node: &Node<'_>, source: &[u8]) -> Option<String> {
    let trait_node = impl_node.child_by_field_name("trait")?;
    node_text(trait_node, source)
}

/// Walk `node`'s parent chain to find the nearest enclosing `struct_item`
/// or `enum_item` and return its name.  Used to set `owner_class` for
/// struct-field `Property` nodes so that two structs with the same field
/// name do not produce the same UID.
pub fn enclosing_struct_type(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if matches!(n.kind(), "struct_item" | "enum_item") {
            return n
                .child_by_field_name("name")
                .and_then(|nn| node_text(nn, source));
        }
        // Stop at impl or source boundaries — don't escape the struct.
        if matches!(n.kind(), "impl_item" | "source_file") {
            return None;
        }
        current = n.parent();
    }
    None
}

/// Walk `node`'s parent chain to find the nearest enclosing `enum_item` and
/// return its name.  Used to set `owner_class` for `EnumVariant` nodes so that
/// same-named variants in different enums (e.g. `Color::Red` / `Status::Red`)
/// produce distinct UIDs.
pub fn enclosing_enum_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "enum_item" {
            return n
                .child_by_field_name("name")
                .and_then(|nn| node_text(nn, source));
        }
        // Stop at source-file boundary — variants never escape their enum body.
        if n.kind() == "source_file" {
            return None;
        }
        current = n.parent();
    }
    None
}

/// Walk `node`'s parent chain to find the nearest enclosing `function_item`
/// and return its name.  Used to set `owner_class` for items nested inside
/// functions (e.g. `const`/`fn`/`macro_rules!` defined inside a function
/// body) so that top-level and function-local definitions with the same name
/// do not collide on UID.
pub fn enclosing_function_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "function_item" {
            return n
                .child_by_field_name("name")
                .and_then(|nn| node_text(nn, source));
        }
        // Stop at source-file boundary.
        if n.kind() == "source_file" {
            return None;
        }
        current = n.parent();
    }
    None
}

/// Walk `node`'s parent chain to find the nearest enclosing `impl_item` or
/// `trait_item` and return a discriminating context string for associated types.
///
/// - `impl Trait for Type { type Assoc = …; }` → `Some("Trait")` (trait name)
/// - `impl Type { type Assoc = …; }` → `Some("")` (inherent impl, no trait)
/// - `trait Trait { type Assoc; }` → `Some("Trait")` (trait definition name)
/// - top-level type alias → `None`
///
/// This ensures that `type Error` in `impl Encoder for Http` and `type Error`
/// in `impl Decoder for Http` get distinct owner_class values and thus
/// distinct UIDs.
pub fn enclosing_impl_or_trait_context(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "impl_item" {
            // Use the trait name for trait-impls; empty string for inherent.
            return Some(impl_trait_name(&n, source).unwrap_or_default());
        }
        if n.kind() == "trait_item" {
            return n
                .child_by_field_name("name")
                .and_then(|nn| node_text(nn, source));
        }
        if n.kind() == "source_file" {
            return None;
        }
        current = n.parent();
    }
    None
}

/// Walk `node`'s parent chain to find the innermost enclosing `impl_item`
/// and return the self-type name.  Returns `None` when the node is not inside
/// any impl block (free function, module-level item, etc.).
///
/// This is the at-emit-time approach: each function_item node has grammar
/// ancestry `declaration_list → impl_item`, so the parent walk is O(depth)
/// rather than a hash lookup.  Correctly handles two structs with the same
/// method name — each function_item's ancestry leads to a distinct impl_item.
pub fn enclosing_impl_type(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "impl_item" {
            return impl_self_type(&n, source);
        }
        current = n.parent();
    }
    None
}

// ── local type scope ──────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct Scope {
    start_row: u32,
    end_row: u32,
    /// var_name → bare type name (references/pointers stripped).
    pub bindings: HashMap<String, String>,
    /// The impl-type in scope when this fn is inside an `impl_item`
    /// (used to resolve `self`).
    pub self_type: Option<String>,
}

#[derive(Debug, Default)]
pub struct LocalTypes {
    scopes: Vec<Scope>,
}

impl LocalTypes {
    /// Iterate all scopes for flat export (used by indirect-dispatch detection).
    pub fn scopes(&self) -> &[Scope] {
        &self.scopes
    }

    /// Look up `var`'s type at `line`, preferring the innermost scope.
    /// Special case: `var == "self"` falls back to `self_type` of the scope.
    pub fn lookup(&self, line: u32, var: &str) -> Option<&str> {
        let mut best: Option<&str> = None;
        let mut best_width = u32::MAX;
        for scope in &self.scopes {
            if scope.start_row <= line && line <= scope.end_row {
                let w = scope.end_row - scope.start_row;
                // Try explicit binding first.
                if let Some(ty) = scope.bindings.get(var) {
                    if w < best_width {
                        best_width = w;
                        best = Some(ty.as_str());
                    }
                } else if var == "self" {
                    if let Some(ref st) = scope.self_type {
                        if w < best_width {
                            best_width = w;
                            best = Some(st.as_str());
                        }
                    }
                }
            }
        }
        best
    }
}

/// Build `LocalTypes` from every `function_item` in the file, resolving the
/// enclosing `impl_item`'s type (if any) for `self` resolution.
pub fn collect_local_types(root: Node<'_>, source: &[u8], impl_map: &ImplMap) -> LocalTypes {
    let mut scopes: Vec<Scope> = Vec::new();
    collect_scopes(root, source, impl_map, None, &mut scopes);
    LocalTypes { scopes }
}

fn collect_scopes<'a>(
    node: Node<'a>,
    source: &[u8],
    impl_map: &ImplMap,
    enclosing_impl_type: Option<&str>,
    out: &mut Vec<Scope>,
) {
    // When we enter an impl_item, note the self-type for all methods inside it.
    let mut current_impl_type: Option<String> = enclosing_impl_type.map(str::to_string);
    if node.kind() == "impl_item" {
        current_impl_type = impl_self_type(&node, source);
    }

    let mut c = node.walk();
    for child in node.children(&mut c) {
        if child.kind() == "function_item" {
            let self_type = current_impl_type.clone().or_else(|| {
                // Fallback: look up the function name in impl_map.
                child
                    .child_by_field_name("name")
                    .and_then(|n| node_text(n, source))
                    .and_then(|name| impl_map.entries.get(&name).cloned())
            });

            let mut bindings: HashMap<String, String> = HashMap::new();

            // Typed parameters.
            if let Some(params) = child.child_by_field_name("parameters") {
                collect_params(&params, source, &mut bindings);
            }

            // `let` bindings in the body.
            if let Some(body) = child.child_by_field_name("body") {
                collect_body_bindings(&body, source, &mut bindings);
            }

            out.push(Scope {
                start_row: child.start_position().row as u32,
                end_row: child.end_position().row as u32,
                bindings,
                self_type,
            });

            // Recurse into the function body (nested closures get their own scope).
            if let Some(body) = child.child_by_field_name("body") {
                collect_scopes(body, source, impl_map, current_impl_type.as_deref(), out);
            }
        } else {
            collect_scopes(child, source, impl_map, current_impl_type.as_deref(), out);
        }
    }
}

/// Collect typed parameters from a `parameters` node.
/// Handles `self`, `&self`, `&mut self` (bind to impl type via self_type),
/// and named params `d: Dog` / `d: &Dog` / `d: &mut Dog`.
fn collect_params(params: &Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut c = params.walk();
    for p in params.children(&mut c) {
        // `self` / `&self` / `&mut self` have their own node kinds;
        // we don't need explicit bindings here — the `self_type` field
        // on the Scope handles `self` resolution in `LocalTypes::lookup`.
        if p.kind() != "parameter" {
            continue;
        }
        let Some(pat) = p.child_by_field_name("pattern") else {
            continue;
        };
        let Some(ty_node) = p.child_by_field_name("type") else {
            continue;
        };
        let Some(ty) = bare_type_name(ty_node, source) else {
            continue;
        };
        // Pattern can be an identifier or a mut-identifier.
        let var_name = if pat.kind() == "identifier" {
            node_text(pat, source)
        } else if pat.kind() == "mut_pattern" {
            pat.named_child(0)
                .filter(|n| n.kind() == "identifier")
                .and_then(|n| node_text(n, source))
        } else {
            None
        };
        if let Some(name) = var_name {
            out.insert(name, ty);
        }
    }
}

/// Walk a function body collecting `let_declaration` type annotations.
/// Does NOT descend into nested closures (they get their own scope entry).
fn collect_body_bindings(body: &Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![*body];
    while let Some(n) = stack.pop() {
        // Don't descend into nested closures.
        if n.kind() == "closure_expression" || n.kind() == "function_item" {
            continue;
        }
        if n.kind() == "let_declaration" {
            collect_let_binding(&n, source, out);
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

/// `let x: Dog = ...` — extract `x → "Dog"`.
/// Skips patterns more complex than a plain identifier or `mut x`.
fn collect_let_binding(node: &Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let Some(ty_node) = node.child_by_field_name("type") else {
        return;
    };
    let Some(ty) = bare_type_name(ty_node, source) else {
        return;
    };
    let Some(pat) = node.child_by_field_name("pattern") else {
        return;
    };
    let var_name = match pat.kind() {
        "identifier" => node_text(pat, source),
        "mut_pattern" => pat
            .named_child(0)
            .filter(|n| n.kind() == "identifier")
            .and_then(|n| node_text(n, source)),
        _ => None,
    };
    if let Some(name) = var_name {
        out.insert(name, ty);
    }
}

// ── call extraction ───────────────────────────────────────────────────────────

/// Walk the Rust AST once, attaching callee names to enclosing function /
/// method nodes (with receiver-type binding) and collecting path-shaped
/// string literals as `RawPathLiteral` side-table entries.
///
/// Call resolution:
/// - `self.method()` inside `impl Dog` → `"Dog.method"`
/// - `obj.method()` where `obj: Dog` locally → `"Dog.method"`
/// - `Foo::bar()` (scoped call) → `"Foo::bar"` (unchanged, already qualified)
/// - bare `func()` → `"func"`
///
/// Path literals: every `string_literal` / `raw_string_literal` is fed
/// through `path_literals::build_raw_path_literal`, which applies the
/// path-shape predicate + sink classifier. Merging this with the call
/// walk halves the per-file DFS cost vs the prior two-pass layout
/// (see PR #367 comparison).
pub fn extract_rust_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    local_types: &LocalTypes,
) -> Vec<RawPathLiteral> {
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            // Rust uses `call_expression` for both `foo()` and `obj.method()`.
            // `field_expression` children of `call_expression` represent `obj.method`.
            "call_expression" => {
                if let Some(callee) = rust_callee_name(n, source, local_types) {
                    let line = n.start_position().row as u32;
                    attach_to_enclosing(line, callee, nodes);
                }
            }
            "string_literal" | "raw_string_literal" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    path_literals
}

/// Derive the callee name for a Rust `call_expression`.
fn rust_callee_name(call: Node<'_>, source: &[u8], locals: &LocalTypes) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    match function.kind() {
        // Plain function call: `foo()`
        "identifier" => node_text(function, source),

        // Method call via field expression: `obj.method()`
        // tree-sitter-rust represents `obj.method()` as:
        //   call_expression
        //     function: field_expression
        //       value: <obj>
        //       field: <method_name>
        "field_expression" => {
            let field = function.child_by_field_name("field")?;
            let method_name = node_text(field, source)?;
            if let Some(value) = function.child_by_field_name("value") {
                let obj_text = node_text(value, source)?;
                // Strip leading `*` (deref) and `&` to get the plain var name.
                let var_name = obj_text.trim_start_matches(['*', '&', ' ']);
                let line = call.start_position().row as u32;
                if let Some(ty) = locals.lookup(line, var_name) {
                    return Some(format!("{ty}.{method_name}"));
                }
            }
            // Fallback: bare method name.
            Some(method_name)
        }

        // Scoped path call: `Dog::new()` or `std::vec::Vec::new()`
        "scoped_identifier" | "generic_function" => node_text(function, source).or_else(|| {
            function
                .child_by_field_name("name")
                .and_then(|n| node_text(n, source))
        }),

        _ => {
            let text = node_text(function, source)?;
            let after_colon = text.rsplit_once("::").map(|(_, t)| t).unwrap_or(&text);
            let after_dot = after_colon
                .rsplit_once('.')
                .map(|(_, t)| t)
                .unwrap_or(after_colon);
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

/// Extract the bare type name, stripping `&`, `&mut`, and `*` reference/pointer
/// wrappers.  Returns `None` for generic types (`Vec<T>`, etc.) since those
/// cannot be matched to a simple class name in the resolver.
fn bare_type_name(ty_node: Node<'_>, source: &[u8]) -> Option<String> {
    match ty_node.kind() {
        "type_identifier" | "primitive_type" => node_text(ty_node, source),
        // &T or &mut T
        "reference_type" => {
            let inner = ty_node.child_by_field_name("type")?;
            bare_type_name(inner, source)
        }
        // *T or *mut T
        "raw_pointer_type" => {
            // raw_pointer_type: `*` (`const`|`mut`) type
            let inner = ty_node.named_child(ty_node.named_child_count().saturating_sub(1) as u32)?;
            bare_type_name(inner, source)
        }
        _ => None,
    }
}

fn node_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
        .ok()
        .map(str::to_string)
}
