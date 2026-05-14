//! Receiver-type binding for C.
//!
//! C has no method syntax. We emulate methods via the widely used
//! "first-arg-is-a-receiver-pointer" convention: a function defined as
//! `void op(struct T *self, ...)` — where the first parameter is a pointer
//! to a struct/typedef type AND its name is one of the conventional
//! receiver identifiers (`self`, `this`, `me`) — is treated as a method on
//! that type. Call sites `op(receiver, ...)` then bind to `T.op` so the
//! resolver's qualifier-scoped lookup (Tier 2.5) can route correctly.
//!
//! Conservative scope (matches Python `4e4fb1b`'s discipline):
//!
//! - Only single-identifier struct types (`struct Calc *` or typedef
//!   `Calc *`) are recognized. Anonymous structs, function-pointer params,
//!   and array-of-struct first params are skipped.
//! - Only receiver names in `RECEIVER_NAMES` qualify — `void op(Foo *x)`
//!   does NOT register, because `x` is too generic to be confident it's a
//!   receiver convention versus a regular parameter.
//! - Free functions (no receiver-shaped first param) are left as bare names.

use crate::calls::attach_to_enclosing;
use graph_nexus_core::analyzer::types::RawNode;
use std::collections::HashMap;
use tree_sitter::Node;

const RECEIVER_NAMES: &[&str] = &["self", "this", "me"];

/// Map of function-name → receiver-type, populated by scanning every
/// `function_definition` for the receiver convention.
#[derive(Debug, Default)]
pub struct CReceiverMap {
    map: HashMap<String, String>,
}

impl CReceiverMap {
    fn get(&self, fn_name: &str) -> Option<&str> {
        self.map.get(fn_name).map(String::as_str)
    }
}

/// Walk the AST collecting receiver-shaped function definitions.
pub fn collect_receiver_methods(root: Node<'_>, source: &[u8]) -> CReceiverMap {
    let mut map: HashMap<String, String> = HashMap::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_definition" {
            if let Some((fn_name, recv_type)) = c_function_receiver(n, source) {
                map.insert(fn_name, recv_type);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    CReceiverMap { map }
}

/// Extract `(function_name, receiver_type)` if the function matches the
/// receiver convention. Returns None otherwise.
fn c_function_receiver(fn_def: Node<'_>, source: &[u8]) -> Option<(String, String)> {
    let declarator = fn_def.child_by_field_name("declarator")?;
    // Function name lives at function_declarator.declarator (an identifier),
    // possibly wrapped in a pointer_declarator (returns-pointer signature).
    let fn_decl = find_function_declarator(declarator)?;
    let name_node = fn_decl.child_by_field_name("declarator")?;
    if name_node.kind() != "identifier" {
        return None;
    }
    let fn_name = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
        .ok()?
        .to_string();

    let params = fn_decl.child_by_field_name("parameters")?;
    let first_param = params.named_child(0)?;
    if first_param.kind() != "parameter_declaration" {
        return None;
    }

    let ty_node = first_param.child_by_field_name("type")?;
    let recv_type = match ty_node.kind() {
        "struct_specifier" => {
            let ti = ty_node.child_by_field_name("name")?;
            if ti.kind() != "type_identifier" {
                return None;
            }
            std::str::from_utf8(&source[ti.start_byte()..ti.end_byte()]).ok()?
        }
        "type_identifier" => {
            // Typedef-aliased struct, e.g. `Calc` after `typedef struct {...} Calc;`.
            std::str::from_utf8(&source[ty_node.start_byte()..ty_node.end_byte()]).ok()?
        }
        _ => return None,
    };

    // The declarator must be a pointer_declarator wrapping an identifier
    // whose name is a recognized receiver convention.
    let param_declarator = first_param.child_by_field_name("declarator")?;
    if param_declarator.kind() != "pointer_declarator" {
        return None;
    }
    let inner = param_declarator.child_by_field_name("declarator")?;
    if inner.kind() != "identifier" {
        return None;
    }
    let recv_name = std::str::from_utf8(&source[inner.start_byte()..inner.end_byte()]).ok()?;
    if !RECEIVER_NAMES.contains(&recv_name) {
        return None;
    }

    Some((fn_name, recv_type.to_string()))
}

/// Walk past pointer_declarator wrappers (used when a function returns a
/// pointer, e.g. `T *foo(...)`) to reach the underlying `function_declarator`.
fn find_function_declarator<'a>(mut node: Node<'a>) -> Option<Node<'a>> {
    loop {
        match node.kind() {
            "function_declarator" => return Some(node),
            "pointer_declarator" => {
                node = node.child_by_field_name("declarator")?;
            }
            _ => return None,
        }
    }
}

/// Walk the C AST attaching call sites to enclosing functions. When the
/// callee is a bare identifier that names a receiver-convention function,
/// rewrite it to `Type.fn` so the resolver picks up the qualifier.
pub fn extract_c_calls(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    methods: &CReceiverMap,
) {
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "call_expression" {
            if let Some(callee) = c_callee_name(n, source, methods) {
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

fn c_callee_name(call: Node<'_>, source: &[u8], methods: &CReceiverMap) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    match function.kind() {
        "identifier" => {
            let name =
                std::str::from_utf8(&source[function.start_byte()..function.end_byte()]).ok()?;
            if let Some(ty) = methods.get(name) {
                Some(format!("{ty}.{name}"))
            } else {
                Some(name.to_string())
            }
        }
        "field_expression" => {
            // Direct struct-member call: `obj.field()` or `obj->field()`.
            // Without per-var type tracking (no decl annotation in plain C
            // beyond function bodies), fall back to the bare field name —
            // matches the existing extract_calls behavior.
            let field = function.child_by_field_name("field")?;
            std::str::from_utf8(&source[field.start_byte()..field.end_byte()])
                .ok()
                .map(str::to_string)
        }
        _ => std::str::from_utf8(&source[function.start_byte()..function.end_byte()])
            .ok()
            .map(str::to_string),
    }
}
