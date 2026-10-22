//! Value-set fan-out for a PHP method call whose name is a variable bound
//! only to string literals:
//!
//! ```php
//! private const ROW_METHODS = ['H' => 'hero', 'S' => 'story'];
//! $map = self::ROW_METHODS[$type] ?? null;
//! return ProductRows::$map($payload);
//! ```
//!
//! Without it the static methods the map names have no caller, and
//! `ecp impact` reports them dead. The fan-out names one candidate per value,
//! so it stays below the high-trust bar: a `static::` constant can be
//! overridden by a subclass, and a by-reference argument can rebind the name.

use super::laravel_routes::string_value;
use crate::framework_confidence;
use crate::framework_helpers::{enclosing_function_name, node_span};
use ecp_core::analyzer::types::{RawFanoutRef, RawNode};
use tree_sitter::Node;

const REASON: &str = "php-const-map-dispatch";

/// Positions that write the variable other than `$m = <rhs>`: the value they
/// bind cannot be enumerated.
const REBINDING_PARENTS: &[&str] = &[
    "augmented_assignment_expression",
    "reference_assignment_expression",
    "update_expression",
    "foreach_statement",
    "pair",
    "list_literal",
    "by_ref",
    "global_declaration",
    "static_variable_declaration",
    "catch_clause",
    "unset_statement",
];

/// The fan-out of a runtime-named method call, when every value its name
/// variable can hold is a string literal written in this file and the
/// receiver names one class. `None` leaves the site a blind spot.
pub(super) fn const_map_fanout(
    call: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
) -> Option<RawFanoutRef> {
    let var = call
        .child_by_field_name("name")
        .filter(|n| n.kind() == "variable_name")?
        .utf8_text(source)
        .ok()?;
    let function = enclosing_function(call)?;
    let class = enclosing_class(function);
    let owner = receiver_owner(call, class, source)?;
    let values = bound_values(function, var, class, source)?;
    let span = node_span(&call);
    Some(RawFanoutRef {
        source_name: enclosing_function_name(nodes, span)?,
        candidates: values.iter().map(|v| format!("{owner}.{v}")).collect(),
        base_confidence: framework_confidence::FANOUT_BASE,
        reason: REASON.to_string(),
        span,
    })
}

/// The method or named function the call sits in; `None` inside a closure,
/// whose variables come from the outer scope or a `use` list.
fn enclosing_function(call: Node<'_>) -> Option<Node<'_>> {
    let mut cur = call.parent();
    while let Some(n) = cur {
        match n.kind() {
            "method_declaration" | "function_definition" => return Some(n),
            "anonymous_function" | "arrow_function" => return None,
            _ => cur = n.parent(),
        }
    }
    None
}

/// The class declaring `function`; `None` for a trait, an enum, an interface
/// or an anonymous class, whose `self` is not a class this file names.
fn enclosing_class(function: Node<'_>) -> Option<Node<'_>> {
    if function.kind() != "method_declaration" {
        return None;
    }
    let mut cur = function.parent();
    while let Some(n) = cur {
        match n.kind() {
            "class_declaration" => return Some(n),
            "anonymous_class"
            | "trait_declaration"
            | "enum_declaration"
            | "interface_declaration" => return None,
            _ => cur = n.parent(),
        }
    }
    None
}

fn class_name<'s>(class: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    class.child_by_field_name("name")?.utf8_text(source).ok()
}

/// The class that owns the called method, written as the scoped-call callee
/// writes it (`Rows`, `\App\Rows`): `self` / `static` / `$this` name the
/// enclosing class. `parent::` and any other receiver give `None`.
fn receiver_owner<'s>(
    call: Node<'_>,
    class: Option<Node<'_>>,
    source: &'s [u8],
) -> Option<&'s str> {
    if call.kind() == "scoped_call_expression" {
        let scope = call.child_by_field_name("scope")?;
        let text = scope.utf8_text(source).ok()?;
        return match scope.kind() {
            "name" | "qualified_name" => Some(text),
            "relative_scope" if is_self_or_static(text) => class_name(class?, source),
            _ => None,
        };
    }
    let object = call.child_by_field_name("object")?;
    if object.kind() == "variable_name" && object.utf8_text(source).ok()? == "$this" {
        return class_name(class?, source);
    }
    None
}

fn is_self_or_static(scope: &str) -> bool {
    scope.eq_ignore_ascii_case("self") || scope.eq_ignore_ascii_case("static")
}

/// Every value `var` can hold in `function`, deduplicated the way PHP matches
/// method names (case-insensitively), spelled as first written. `None` when
/// a parameter, a closure by reference, or any write other than a plain
/// assignment of an enumerable value binds it, or nothing assigns it.
fn bound_values(
    function: Node<'_>,
    var: &str,
    class: Option<Node<'_>>,
    source: &[u8],
) -> Option<Vec<String>> {
    if let Some(params) = function.child_by_field_name("parameters") {
        if mentions(params, var, source) {
            return None;
        }
    }
    let body = function.child_by_field_name("body")?;
    let mut values: Vec<String> = Vec::new();
    let mut assigned = false;
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "function_definition"
            | "method_declaration"
            | "class_declaration"
            | "declaration_list"
            | "arrow_function" => continue,
            "anonymous_function" => {
                let mut c = n.walk();
                stack.extend(
                    n.named_children(&mut c)
                        .filter(|ch| ch.kind() == "anonymous_function_use_clause"),
                );
                continue;
            }
            "variable_name" if n.utf8_text(source).ok() == Some(var) => {
                let parent = n.parent()?;
                if parent.kind() == "assignment_expression"
                    && parent.child_by_field_name("left") == Some(n)
                {
                    assigned = true;
                    rhs_values(
                        parent.child_by_field_name("right")?,
                        class,
                        source,
                        &mut values,
                    )?;
                } else if REBINDING_PARENTS.contains(&parent.kind()) || inside_list(n, function) {
                    return None;
                }
                continue;
            }
            _ => {}
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    if !assigned {
        return None;
    }
    let mut unique: Vec<String> = Vec::with_capacity(values.len());
    for v in values {
        if !unique.iter().any(|u| u.eq_ignore_ascii_case(&v)) {
            unique.push(v);
        }
    }
    Some(unique)
}

fn mentions(node: Node<'_>, var: &str, source: &[u8]) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "variable_name" && n.utf8_text(source).ok() == Some(var) {
            return true;
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    false
}

/// `[$m] = $row` / `list($m) = $row` nest the variable below the list.
fn inside_list(var: Node<'_>, function: Node<'_>) -> bool {
    let mut cur = var.parent();
    while let Some(n) = cur {
        if n.id() == function.id() {
            return false;
        }
        if n.kind() == "list_literal" {
            return true;
        }
        cur = n.parent();
    }
    false
}

/// The values one assignment's right side gives: a string literal,
/// `C::MAP[$k]`, or `C::MAP[$k] ?? null` / `?? '<literal>'`.
fn rhs_values(
    rhs: Node<'_>,
    class: Option<Node<'_>>,
    source: &[u8],
    out: &mut Vec<String>,
) -> Option<()> {
    match rhs.kind() {
        "string" | "encapsed_string" => out.push(method_literal(rhs, source)?),
        "subscript_expression" => const_map_values(rhs, class, source, out)?,
        "binary_expression" => {
            if rhs.child_by_field_name("operator")?.kind() != "??" {
                return None;
            }
            let left = rhs.child_by_field_name("left")?;
            if left.kind() != "subscript_expression" {
                return None;
            }
            const_map_values(left, class, source, out)?;
            let default = rhs.child_by_field_name("right")?;
            if default.kind() != "null" {
                out.push(method_literal(default, source)?);
            }
        }
        _ => return None,
    }
    Some(())
}

/// The values of `self::MAP[$k]` / `static::MAP[$k]` / `Name::MAP[$k]`: the
/// constant must be declared on that class in this file, as a non-empty
/// array of string literals.
fn const_map_values(
    subscript: Node<'_>,
    class: Option<Node<'_>>,
    source: &[u8],
    out: &mut Vec<String>,
) -> Option<()> {
    let access = subscript
        .named_child(0)
        .filter(|n| n.kind() == "class_constant_access_expression")?;
    let scope = access.named_child(0)?;
    let constant = access
        .named_child(access.named_child_count().checked_sub(1)? as u32)
        .filter(|n| n.kind() == "name")?
        .utf8_text(source)
        .ok()?;
    let scope_text = scope.utf8_text(source).ok()?;
    let owner = match scope.kind() {
        "relative_scope" if is_self_or_static(scope_text) => class?,
        "name" | "qualified_name" => {
            let short = scope_text.rsplit('\\').next()?;
            file_class(subscript, short, source)?
        }
        _ => return None,
    };
    let array = const_initializer(owner, constant, source)?;
    let mut c = array.walk();
    let mut count = 0usize;
    for element in array
        .named_children(&mut c)
        .filter(|n| n.kind() == "array_element_initializer")
    {
        let mut ec = element.walk();
        let parts: Vec<Node<'_>> = element
            .named_children(&mut ec)
            .filter(|n| n.kind() != "comment")
            .collect();
        if parts
            .iter()
            .any(|p| matches!(p.kind(), "by_ref" | "variadic_unpacking"))
        {
            return None;
        }
        out.push(method_literal(*parts.last()?, source)?);
        count += 1;
    }
    (count > 0).then_some(())
}

/// The `class_declaration` named `name` in the file holding `node`.
fn file_class<'t>(node: Node<'t>, name: &str, source: &[u8]) -> Option<Node<'t>> {
    let mut root = node;
    while let Some(p) = root.parent() {
        root = p;
    }
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "class_declaration" {
            if class_name(n, source) == Some(name) {
                return Some(n);
            }
            continue;
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    None
}

/// The array literal `class` assigns to its own constant `constant`; an
/// inherited constant is not in the body and gives `None`.
fn const_initializer<'t>(class: Node<'t>, constant: &str, source: &[u8]) -> Option<Node<'t>> {
    let body = class.child_by_field_name("body")?;
    let mut c = body.walk();
    for decl in body
        .named_children(&mut c)
        .filter(|n| n.kind() == "const_declaration")
    {
        let mut dc = decl.walk();
        for element in decl
            .named_children(&mut dc)
            .filter(|n| n.kind() == "const_element")
        {
            let mut ec = element.walk();
            let parts: Vec<Node<'t>> = element
                .named_children(&mut ec)
                .filter(|n| n.kind() != "comment")
                .collect();
            if parts.first()?.utf8_text(source).ok() != Some(constant) {
                continue;
            }
            return parts
                .get(1)
                .copied()
                .filter(|v| v.kind() == "array_creation_expression");
        }
    }
    None
}

/// A non-interpolated string literal that is a valid PHP method name; a
/// heredoc, `'Rows::hero'` or `' hero'` gives `None`.
fn method_literal(node: Node<'_>, source: &[u8]) -> Option<String> {
    let value = string_value(node, source)?;
    let mut chars = value.chars();
    let first = chars.next()?;
    let ident = |c: char| c == '_' || c.is_ascii_alphanumeric() || !c.is_ascii();
    (ident(first) && !first.is_ascii_digit() && chars.all(ident)).then_some(value)
}
