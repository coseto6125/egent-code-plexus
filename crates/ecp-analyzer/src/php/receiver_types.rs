//! PHP receiver-type binding for method call sites.
//!
//! Handles three patterns that PHP 7+ makes statically knowable:
//!
//! - `$this->method()`  → receiver = enclosing class name
//! - `parent::method()` → receiver = first heritage class of the enclosing class
//! - `self::method()` / `static::method()` → receiver = enclosing class name
//!   (`static` is late-static-binding; for graph purposes we treat it the
//!   same as `self` since the graph is per-file and the resolver will follow
//!   the concrete class).
//!
//! Typed `$var->method()` where `$var` is not `$this` is intentionally left
//! unbound: PHP 7 property/param type hints require a second pass to propagate
//! types through the scope and are deferred to a later improvement task. It
//! is recorded as a [`CallSite::untyped_member`].

use super::path_literals::{
    build_raw_path_literal, enclosing_symbol_and_owner_pub, extract_php_string_value,
};
use crate::calls::{
    attach_to_enclosing, construction_call, range_width, reference_function, start_pos, Pos,
    PosRange,
};
use ecp_core::analyzer::types::{CallSite, RawNode, RawPathLiteral, RawSqlRef};
use ecp_core::graph::NodeKind;
use tree_sitter::Node;

/// Enclosing-class context built by a pre-walk over the node list.
/// Maps the `(start, end)` position range → `(class_name, Option<parent_name>,
/// declared members)`. Members are listed for a trait or an enum only.
pub struct ClassContext {
    entries: Vec<ClassEntry>,
}

/// `(span, class_name, parent_name, declared members)`.
type ClassEntry = (PosRange, String, Option<String>, Option<Vec<String>>);
/// [`range_width`]'s `(lines, columns)`.
type Width = (u32, i64);

impl ClassContext {
    pub fn from_nodes(nodes: &[RawNode]) -> Self {
        let range = |n: &RawNode| ((n.span.0, n.span.1), (n.span.2, n.span.3));
        let entries = nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.kind,
                    NodeKind::Class | NodeKind::Interface | NodeKind::Trait | NodeKind::Enum
                )
            })
            .map(|n| {
                let span = range(n);
                let parent = n.heritage.first().cloned();
                // `$this` in a trait is the class that uses it: only the
                // trait's own members are reachable through its name, and a
                // host class's member keeps the by-name route.
                let members = matches!(n.kind, NodeKind::Trait | NodeKind::Enum).then(|| {
                    nodes
                        .iter()
                        .filter(|m| {
                            matches!(m.kind, NodeKind::Method | NodeKind::Function) && {
                                let (start, end) = range(m);
                                span.0 <= start && end <= span.1
                            }
                        })
                        .map(|m| m.name.clone())
                        .collect()
                });
                (span, n.name.clone(), parent, members)
            })
            .collect();
        Self { entries }
    }

    /// The name of the innermost class enclosing `pos`, when `member` is
    /// reachable through it.
    fn enclosing_class_name(&self, pos: Pos, member: &str) -> Option<&str> {
        let mut best: Option<(&str, Option<&[String]>, Width)> = None;
        for ((start, end), name, _, members) in &self.entries {
            if *start <= pos && pos <= *end {
                let width = range_width((*start, *end));
                if best.is_none_or(|(_, _, w)| width < w) {
                    best = Some((name.as_str(), members.as_deref(), width));
                }
            }
        }
        best.filter(|(_, members, _)| members.is_none_or(|m| m.iter().any(|n| n == member)))
            .map(|(n, _, _)| n)
    }

    /// Return the first heritage (parent) class of the class enclosing `pos`.
    fn enclosing_parent_name(&self, pos: Pos) -> Option<&str> {
        let mut best: Option<(Option<&str>, (u32, i64))> = None;
        for ((start, end), _, parent, _) in &self.entries {
            if *start <= pos && pos <= *end {
                let width = range_width((*start, *end));
                if best.is_none_or(|(_, w)| width < w) {
                    best = Some((parent.as_deref(), width));
                }
            }
        }
        best.and_then(|(p, _)| p)
    }
}

/// Walk the PHP AST once, attaching callees to enclosing nodes (with
/// receiver binding for `$this->`, `parent::`, `self::`, `static::`)
/// and collecting path-shaped and SQL-shaped `string` / `encapsed_string`
/// literals, plus the method calls whose name is bound at runtime
/// (`Cls::$m()`, `$o->$m()`, `$o?->{expr}()`).
pub fn extract_php_calls_and_path_literals<'t>(
    root: Node<'t>,
    source: &[u8],
    nodes: &mut [RawNode],
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>, Vec<Node<'t>>) {
    let ctx = ClassContext::from_nodes(nodes);
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let mut dynamic_calls: Vec<Node<'t>> = Vec::new();
    let mut constructions: Vec<((u32, u32), String)> = Vec::new();
    let mut stack: Vec<Node<'t>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "object_creation_expression" => {
                if let Some(callee) = php_object_creation_call(n, source) {
                    constructions.push((start_pos(n), callee));
                }
            }
            "function_call_expression" if is_first_class_callable(n) => {
                reference_first_class_function(n, source, nodes);
            }
            "function_call_expression" => {
                if let Some(target) = literal_callable(n, source) {
                    attach_to_enclosing(
                        start_pos(n),
                        literal_callable_callee(target, start_pos(n), &ctx),
                        nodes,
                    );
                }
            }
            "member_call_expression" | "nullsafe_member_call_expression" => {
                if let Some(callee) = php_member_callee(n, source, &ctx) {
                    attach_call_or_reference(n, callee, nodes);
                } else if has_runtime_name(n) {
                    dynamic_calls.push(n);
                }
            }
            "scoped_call_expression" => {
                if let Some(callee) = php_scoped_callee(n, source, &ctx) {
                    attach_call_or_reference(n, callee, nodes);
                } else if has_runtime_name(n) {
                    dynamic_calls.push(n);
                }
            }
            "string" | "encapsed_string" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same node, separate filter.
                // `extract_php_string_value` already skips interpolated strings.
                if let Some(value) = extract_php_string_value(n, source) {
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
    // After every call, so a `new` site never shifts the index of a call
    // recorded before it.
    for (pos, callee) in constructions {
        attach_to_enclosing(pos, callee, nodes);
    }
    (path_literals, sql_refs, dynamic_calls)
}

/// `f(...)`, `$o->m(...)`, `C::m(...)`: the first-class callable syntax hands
/// the function on as a value and calls nothing.
fn is_first_class_callable(call: Node<'_>) -> bool {
    call.child_by_field_name("arguments").is_some_and(|args| {
        args.named_child_count() == 1
            && args
                .named_child(0)
                .is_some_and(|arg| arg.kind() == "variadic_placeholder")
    })
}

/// A member or scoped call is a call; the same call in first-class callable
/// syntax whose receiver the parser typed (`$this->m(...)`, `C::m(...)`) is a
/// `References` edge. An untyped receiver stays the untyped-member call.
fn attach_call_or_reference(call: Node<'_>, callee: String, nodes: &mut [RawNode]) {
    let pos = start_pos(call);
    let typed_name = match CallSite::parse(&callee) {
        CallSite::Plain(name) if is_first_class_callable(call) => {
            Some(name.trim_start_matches('\\').to_string())
        }
        _ => None,
    };
    match typed_name {
        Some(name) => {
            reference_function(pos, &name, nodes);
        }
        None => {
            attach_to_enclosing(pos, callee, nodes);
        }
    }
}

/// `cb(...)` / `\App\cb(...)`: a reference to the function, not a call. The
/// generic call walk already recorded the call, so it is taken back.
fn reference_first_class_function(call: Node<'_>, source: &[u8], nodes: &mut [RawNode]) {
    let Some(function) = call.child_by_field_name("function") else {
        return;
    };
    if !matches!(function.kind(), "name" | "qualified_name") {
        return;
    }
    let Some(recorded) = crate::calls::callee_name_from(call, source) else {
        return;
    };
    let name = local_function_name(&recorded, enclosing_namespace(call, source));
    let Some(holder) = reference_function(start_pos(call), &name, nodes) else {
        return;
    };
    let calls = &mut nodes[holder].calls;
    if let Some(at) = calls
        .iter()
        .position(|c| CallSite::parse(c) == CallSite::Plain(&recorded))
    {
        calls.remove(at);
    }
}

/// The namespace the code at `node` runs in: the enclosing braced
/// `namespace X { .. }`, else the last `namespace X;` statement before it.
fn enclosing_namespace<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    let name_of = |namespace: Node<'_>| {
        namespace
            .child_by_field_name("name")?
            .utf8_text(source)
            .ok()
    };
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "namespace_definition" => return name_of(parent),
            "program" => {
                let mut cursor = parent.walk();
                return parent
                    .named_children(&mut cursor)
                    .take_while(|c| c.start_byte() < node.start_byte())
                    .filter(|c| {
                        c.kind() == "namespace_definition"
                            && c.child_by_field_name("body").is_none()
                    })
                    .last()
                    .and_then(name_of);
            }
            _ => current = parent,
        }
    }
    None
}

/// A function named with the namespace the code already runs in
/// (`P\cb` inside `namespace P`) is the bare name `cb`, so it binds as the
/// bare name does. Any other qualified name keeps its qualifier.
fn local_function_name(name: &str, namespace: Option<&str>) -> String {
    let name = name.strip_prefix('\\').unwrap_or(name);
    match name.rsplit_once('\\') {
        Some((qualifier, base)) if Some(qualifier) == namespace => base.to_string(),
        _ => name.to_string(),
    }
}

/// The class a parameter named `var` is declared with, in the innermost
/// function around `node` that declares it, as long as no function body on
/// the way assigns `var` and every closure on the way captures it. `None` for
/// an undeclared, reassigned, uncaptured, primitive, union or `self` /
/// `static` / `parent` type.
fn parameter_class(node: Node<'_>, var: &str, source: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        current = n.parent();
        if !matches!(
            n.kind(),
            "method_declaration" | "function_definition" | "anonymous_function" | "arrow_function"
        ) {
            continue;
        }
        let params = n.child_by_field_name("parameters")?;
        let mut cursor = params.walk();
        let declared = params.named_children(&mut cursor).find(|p| {
            matches!(
                p.kind(),
                "simple_parameter" | "property_promotion_parameter"
            ) && p
                .child_by_field_name("name")
                .and_then(|v| v.utf8_text(source).ok())
                == Some(var)
        });
        if assigns_variable(n, var, source, node) {
            return None;
        }
        let Some(declared) = declared else {
            // `fn` captures by value and a `use` list names what a `function`
            // closure captures; a named function or method sees no outer
            // variable.
            let crosses = match n.kind() {
                "arrow_function" => true,
                "anonymous_function" => captures_variable(n, var, source),
                _ => false,
            };
            if crosses {
                continue;
            }
            return None;
        };
        let ty = declared.child_by_field_name("type")?;
        let ty = if ty.kind() == "optional_type" {
            ty.named_child(0)?
        } else {
            ty
        };
        if ty.kind() != "named_type" {
            return None;
        }
        let class = qualified_ident(ty.named_child(0)?.utf8_text(source).ok()?)?;
        return (!matches!(class, "self" | "static" | "parent")).then(|| class.to_string());
    }
    None
}

fn any_named_child<'t>(node: Node<'t>, pred: impl FnMut(Node<'t>) -> bool) -> bool {
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).any(pred);
    found
}

fn use_clause(closure: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = closure.walk();
    let clause = closure
        .named_children(&mut cursor)
        .find(|c| c.kind() == "anonymous_function_use_clause");
    clause
}

/// Whether the `use (..)` list of a closure names `var`, by value or by
/// reference.
fn captures_variable(closure: Node<'_>, var: &str, source: &[u8]) -> bool {
    use_clause(closure)
        .is_some_and(|clause| any_named_child(clause, |c| names_variable(c, var, source)))
}

/// Whether `target` is, or destructures into, the variable `var`:
/// `$o`, `[$o, $p]`, `&$o`, `$k => $o`. Also tests a `use ($o)` capture.
fn names_variable(target: Node<'_>, var: &str, source: &[u8]) -> bool {
    match target.kind() {
        "variable_name" => target.utf8_text(source).ok() == Some(var),
        "list_literal"
        | "array_creation_expression"
        | "array_element_initializer"
        | "by_ref"
        | "pair" => any_named_child(target, |c| names_variable(c, var, source)),
        _ => false,
    }
}

/// The outermost loop between `at` and `function`, as a byte range: a write
/// anywhere in it can run before `at` on a later pass.
fn enclosing_loop_range(at: Node<'_>, function: Node<'_>) -> Option<(usize, usize)> {
    let mut outermost = None;
    let mut current = at.parent();
    while let Some(n) = current.filter(|n| n.id() != function.id()) {
        if matches!(
            n.kind(),
            "for_statement" | "foreach_statement" | "while_statement" | "do_statement"
        ) {
            outermost = Some((n.start_byte(), n.end_byte()));
        }
        current = n.parent();
    }
    outermost
}

/// Whether the body of `function` gives `var` a new value before `at` runs:
/// an assignment, an update, a `foreach` target, a `catch` name, `global`,
/// `static`, `unset`, or a closure that captures it by reference. A write
/// after `at` does not count, unless a loop around `at` repeats it. `??=`
/// keeps a non-null value of the declared type, so it does not count. The
/// body of a nested function, closure or class is its own scope.
fn assigns_variable(function: Node<'_>, var: &str, source: &[u8], at: Node<'_>) -> bool {
    let Some(body) = function.child_by_field_name("body") else {
        return false;
    };
    let looped = enclosing_loop_range(at, function);
    let before_use = |n: Node<'_>| {
        n.end_byte() <= at.start_byte()
            || looped.is_some_and(|(start, end)| start <= n.start_byte() && n.end_byte() <= end)
    };
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        let assigned = match n.kind() {
            "function_definition"
            | "method_declaration"
            | "class_declaration"
            | "arrow_function" => {
                continue;
            }
            "anonymous_function" => {
                let by_ref = use_clause(n).is_some_and(|clause| {
                    any_named_child(clause, |c| {
                        c.kind() == "by_ref" && names_variable(c, var, source)
                    })
                });
                if by_ref && before_use(n) {
                    return true;
                }
                continue;
            }
            "assignment_expression" | "reference_assignment_expression" => n
                .child_by_field_name("left")
                .is_some_and(|left| names_variable(left, var, source)),
            "augmented_assignment_expression" => {
                let left = n.child_by_field_name("left");
                let right = n.child_by_field_name("right");
                let coalesces = left.zip(right).is_some_and(|(l, r)| {
                    source[l.end_byte()..r.start_byte()].trim_ascii() == b"??="
                });
                !coalesces && left.is_some_and(|l| names_variable(l, var, source))
            }
            "update_expression" => n
                .child_by_field_name("argument")
                .is_some_and(|argument| names_variable(argument, var, source)),
            "catch_clause" => n
                .child_by_field_name("name")
                .is_some_and(|name| names_variable(name, var, source)),
            "static_variable_declaration" => n
                .child_by_field_name("name")
                .is_some_and(|name| names_variable(name, var, source)),
            "global_declaration" | "unset_statement" => {
                any_named_child(n, |c| names_variable(c, var, source))
            }
            // The iterated expression is the first child, the body the field.
            "foreach_statement" => {
                let body = n.child_by_field_name("body");
                let mut cursor = n.walk();
                let found = n
                    .named_children(&mut cursor)
                    .skip(1)
                    .filter(|c| Some(*c) != body)
                    .any(|c| names_variable(c, var, source));
                found
            }
            _ => false,
        };
        if assigned && before_use(n) {
            return true;
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    false
}

/// A callable that `call_user_func` / `call_user_func_array` names with a
/// literal, so the call is as static as the ordinary call of the same shape.
pub(super) enum LiteralCallable {
    /// `'helper'`
    Function(String),
    /// `'App\\Rows::hero'`, `[Rows::class, 'hero']`, `['App\\Rows', 'hero']`
    Static { class: String, method: String },
    /// `[self::class, 'm']`, `[static::class, 'm']`
    Relative { method: String },
    /// `[$this, 'm']`
    This { method: String },
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// `Foo` / `\App\Foo` / `App\Foo` -> `App\Foo`; `None` for anything that is
/// no name (spaces, `$`, empty).
fn qualified_ident(s: &str) -> Option<&str> {
    let s = s.strip_prefix('\\').unwrap_or(s);
    s.split('\\').all(is_ident).then_some(s)
}

/// The unquoted, `\\`-unescaped text of a PHP string literal; interpolated
/// text keeps its `$` and fails the name check.
fn string_literal_text(n: Node<'_>, source: &[u8]) -> Option<String> {
    if !matches!(n.kind(), "string" | "encapsed_string") {
        return None;
    }
    let raw = n.utf8_text(source).ok()?;
    let inner = raw.get(1..raw.len().checked_sub(1)?)?;
    Some(inner.replace("\\\\", "\\"))
}

fn class_of_array_element(n: Node<'_>, source: &[u8]) -> Option<LiteralCallable> {
    let method = String::new();
    match n.kind() {
        "string" | "encapsed_string" => {
            let text = string_literal_text(n, source)?;
            let class = qualified_ident(&text)?.to_string();
            Some(LiteralCallable::Static { class, method })
        }
        "variable_name" if n.utf8_text(source).ok()? == "$this" => {
            Some(LiteralCallable::This { method })
        }
        "variable_name" => {
            let class = parameter_class(n, n.utf8_text(source).ok()?, source)?;
            Some(LiteralCallable::Static { class, method })
        }
        "class_constant_access_expression" => {
            let scope = n.named_child(0)?;
            let constant = n.named_child(1)?;
            if constant.utf8_text(source).ok()? != "class" {
                return None;
            }
            match scope.kind() {
                "name" | "qualified_name" => {
                    let class = qualified_ident(scope.utf8_text(source).ok()?)?.to_string();
                    Some(LiteralCallable::Static { class, method })
                }
                "relative_scope" if matches!(scope.utf8_text(source).ok()?, "self" | "static") => {
                    Some(LiteralCallable::Relative { method })
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The callable a string names: `'helper'` or `'App\\Rows::hero'`.
fn string_callable(text: &str) -> Option<LiteralCallable> {
    match text.split_once("::") {
        Some((class, method)) if is_ident(method) => Some(LiteralCallable::Static {
            class: qualified_ident(class)?.to_string(),
            method: method.to_string(),
        }),
        Some(_) => None,
        None => Some(LiteralCallable::Function(
            qualified_ident(text)?.to_string(),
        )),
    }
}

/// The literal callable of `call_user_func(<callable>, ..)` /
/// `call_user_func_array(<callable>, ..)`; `None` for any other call, a
/// non-literal callable, or a literal that names no callable.
pub(super) fn literal_callable(call: Node<'_>, source: &[u8]) -> Option<LiteralCallable> {
    let function = call.child_by_field_name("function")?;
    if function.kind() != "name"
        || !matches!(
            function.utf8_text(source).ok()?,
            "call_user_func" | "call_user_func_array"
        )
    {
        return None;
    }
    let first = call.child_by_field_name("arguments")?.named_child(0)?;
    callable_from_value(first.named_child(0).unwrap_or(first), source)
}

/// The literal callable a value names: a string (`'helper'`,
/// `'Rows::hero'`) or a two-element array (`[Rows::class, 'hero']`).
fn callable_from_value(value: Node<'_>, source: &[u8]) -> Option<LiteralCallable> {
    match value.kind() {
        "string" | "encapsed_string" => string_callable(&string_literal_text(value, source)?),
        "array_creation_expression" => {
            let mut cursor = value.walk();
            let elements: Vec<Node<'_>> = value.named_children(&mut cursor).collect();
            let [class, method] = elements[..] else {
                return None;
            };
            let method_text = string_literal_text(method.named_child(0)?, source)?;
            if !is_ident(&method_text) {
                return None;
            }
            let mut target = class_of_array_element(class.named_child(0)?, source)?;
            match &mut target {
                LiteralCallable::Static { method, .. }
                | LiteralCallable::Relative { method }
                | LiteralCallable::This { method } => *method = method_text,
                LiteralCallable::Function(_) => return None,
            }
            Some(target)
        }
        _ => None,
    }
}

/// The function a builtin call hands on as a literal callable, for the
/// name scan to record: `array_map('fmt', $xs)` hands on `fmt`,
/// `usort($rows, 'Sorter::byName')` and `usort($rows, [Sorter::class, 'byName'])`
/// hand on `Sorter.byName`, as do `[$this, 'byName']`, `[self::class,
/// 'byName']` and `[static::class, 'byName']` inside `Sorter`. The name keeps
/// its namespace, as [`literal_callable`] records it. A file that declares
/// its own function of the builtin's name (`namespace App; function
/// array_map`) calls that one instead. `call_user_func` calls its callable,
/// so [`literal_callable`] owns it.
pub(crate) fn passed_callable(call: Node<'_>, source: &[u8]) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    if function.kind() != "name" {
        return None;
    }
    let builtin = function.utf8_text(source).ok()?;
    let position = match builtin {
        "array_map"
        | "register_shutdown_function"
        | "spl_autoload_register"
        | "set_error_handler"
        | "set_exception_handler"
        | "header_register_callback"
        | "is_callable" => 0,
        "array_filter"
        | "array_walk"
        | "array_walk_recursive"
        | "array_reduce"
        | "usort"
        | "uasort"
        | "uksort"
        | "preg_replace_callback"
        | "iterator_apply" => 1,
        _ => return None,
    };
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let arguments: Vec<Node<'_>> = args
        .named_children(&mut cursor)
        .filter(|arg| arg.kind() == "argument")
        .collect();
    // Every builtin above names its callable parameter `callback`, except
    // `is_callable`, which names it `value` because it accepts any value.
    let parameter = if builtin == "is_callable" {
        "value"
    } else {
        "callback"
    };
    let arg = arguments
        .iter()
        .find(|arg| {
            arg.child_by_field_name("name")
                .is_some_and(|name| name.utf8_text(source).ok() == Some(parameter))
        })
        .or_else(|| {
            arguments
                .get(position)
                .filter(|arg| arg.child_by_field_name("name").is_none())
        })?;
    let last = u32::try_from(arg.named_child_count().checked_sub(1)?).ok()?;
    let value = arg.named_child(last)?;
    let target = match callable_from_value(value, source)? {
        LiteralCallable::Function(name) => {
            local_function_name(&name, enclosing_namespace(call, source))
        }
        LiteralCallable::Static { class, method } => format!("{class}.{method}"),
        LiteralCallable::Relative { method } | LiteralCallable::This { method } => {
            format!("{}.{method}", enclosing_class_of(call, &method, source)?)
        }
    };
    (!declares_function(call, builtin, source)).then_some(target)
}

/// The class `$this`, `self::class` and `static::class` stand for at `node`:
/// the innermost class, trait or enum declaration around it, when `method`
/// is reachable through its name (a trait's or an enum's own method only, as
/// [`ClassContext::enclosing_class_name`] decides for a call). `None` inside
/// an anonymous class or an interface, and outside any class.
fn enclosing_class_of<'s>(node: Node<'_>, method: &str, source: &'s [u8]) -> Option<&'s str> {
    let mut current = node.parent();
    while let Some(n) = current {
        let own_members_only = match n.kind() {
            "class_declaration" => false,
            "trait_declaration" | "enum_declaration" => true,
            "anonymous_class" | "interface_declaration" => return None,
            _ => {
                current = n.parent();
                continue;
            }
        };
        let declares = || {
            n.child_by_field_name("body").is_some_and(|body| {
                let mut cursor = body.walk();
                let found = body.named_children(&mut cursor).any(|member| {
                    member.kind() == "method_declaration"
                        && member
                            .child_by_field_name("name")
                            .and_then(|name| name.utf8_text(source).ok())
                            == Some(method)
                });
                found
            })
        };
        if own_members_only && !declares() {
            return None;
        }
        return n.child_by_field_name("name")?.utf8_text(source).ok();
    }
    None
}

/// Whether the file holding `node` declares a top-level or namespace-level
/// function named `name`.
fn declares_function(node: Node<'_>, name: &str, source: &[u8]) -> bool {
    let mut root = node;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    let mut cursor = root.walk();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_definition"
            && n.child_by_field_name("name")
                .is_some_and(|f| f.utf8_text(source).ok() == Some(name))
        {
            return true;
        }
        // A function declares at the top level or in a namespace block.
        if matches!(
            n.kind(),
            "program" | "namespace_definition" | "compound_statement"
        ) {
            stack.extend(n.named_children(&mut cursor));
        }
    }
    false
}

/// The callee entry the ordinary call of the same shape records.
fn literal_callable_callee(target: LiteralCallable, pos: Pos, ctx: &ClassContext) -> String {
    match target {
        LiteralCallable::Function(name) => name,
        LiteralCallable::Static { class, method } => format!("{class}.{method}"),
        LiteralCallable::Relative { method } | LiteralCallable::This { method } => {
            match ctx.enclosing_class_name(pos, &method) {
                Some(class) => format!("{class}.{method}"),
                None => CallSite::untyped_member(&method),
            }
        }
    }
}

/// Whether a member, nullsafe member or scoped call names its method at
/// runtime: `$m`, `$$m`, or a braced expression (`{$p . 'x'}`, `{FOO}`).
fn has_runtime_name(call: Node<'_>) -> bool {
    call.child_by_field_name("name")
        .is_some_and(is_runtime_name)
}

fn is_runtime_name(name: Node<'_>) -> bool {
    if name.kind() != "name" {
        return true;
    }
    // A comment is an extra node, so `{/* c */ FOO}` puts it between the
    // brace and the name.
    let mut prev = name.prev_sibling();
    while let Some(p) = prev.filter(|p| p.kind() == "comment") {
        prev = p.prev_sibling();
    }
    prev.is_some_and(|s| s.kind() == "{")
}

/// The construction entry of `new Foo(..)` / `new \App\Foo(..)`; `None`
/// for `new $class`, `new static` and anonymous classes.
fn php_object_creation_call(creation: Node<'_>, source: &[u8]) -> Option<String> {
    let class = creation
        .named_children(&mut creation.walk())
        .find(|c| matches!(c.kind(), "name" | "qualified_name"))?;
    construction_call(class.utf8_text(source).ok()?.trim_start_matches('\\'))
}

/// Resolve the callee for `$obj->method(args)` and `$obj?->method(args)`.
/// Only `$this` is bound to the enclosing class; other receivers give an
/// untyped member call, which resolves by the method name and never binds a
/// `use function` import. `None` for a runtime method name.
fn php_member_callee(call: Node<'_>, source: &[u8], ctx: &ClassContext) -> Option<String> {
    let name_node = call
        .child_by_field_name("name")
        .filter(|n| !is_runtime_name(*n))?;
    let method_name = name_node.utf8_text(source).ok()?;

    let obj_node = call.child_by_field_name("object")?;
    let pos = start_pos(call);

    if obj_node.kind() == "variable_name" {
        if let Ok(var_text) = obj_node.utf8_text(source) {
            if var_text == "$this" {
                if let Some(class_name) = ctx.enclosing_class_name(pos, method_name) {
                    return Some(format!("{class_name}.{method_name}"));
                }
            }
        }
    }

    let untyped = CallSite::untyped_member(method_name);
    // `Foo::create()->bar()`: the resolver types a call receiver by the
    // callee's return type (`self` / `static` included).
    Some(
        match CallSite::nested_chain(|| php_receiver_call(obj_node, source, ctx)) {
            Some(inner) => CallSite::chained_member(&inner, &untyped),
            None => untyped,
        },
    )
}

/// The `calls` entry of a call written as a receiver (`make()->m()`,
/// `Foo::create()->m()`, `$a->b()->m()`, `(new Foo)->m()`), the same entry
/// that call records for itself. `None` for any other receiver.
fn php_receiver_call(obj: Node<'_>, source: &[u8], ctx: &ClassContext) -> Option<String> {
    match obj.kind() {
        "parenthesized_expression" => php_receiver_call(obj.named_child(0)?, source, ctx),
        "member_call_expression" | "nullsafe_member_call_expression" => {
            php_member_callee(obj, source, ctx)
        }
        "scoped_call_expression" => php_scoped_callee(obj, source, ctx),
        "function_call_expression" => crate::calls::callee_name_from(obj, source),
        "object_creation_expression" => php_object_creation_call(obj, source),
        _ => None,
    }
}

/// Resolve the callee for `Scope::method(args)`.
///
/// - `parent::method` → `ParentClass.method` (using heritage)
/// - `self::method` / `static::method` → `EnclosingClass.method`
/// - `ClassName::method` → `ClassName.method` (explicit named scope)
/// - `ClassName::$m` → `None` (runtime method name)
fn php_scoped_callee(call: Node<'_>, source: &[u8], ctx: &ClassContext) -> Option<String> {
    let name_node = call
        .child_by_field_name("name")
        .filter(|n| !is_runtime_name(*n))?;
    let method_name = name_node.utf8_text(source).ok()?;

    let scope_node = call.child_by_field_name("scope")?;
    let pos = start_pos(call);

    match scope_node.kind() {
        "relative_scope" => {
            let scope_text = scope_node.utf8_text(source).ok()?;
            match scope_text {
                "parent" => {
                    if let Some(parent_name) = ctx.enclosing_parent_name(pos) {
                        return Some(format!("{parent_name}.{method_name}"));
                    }
                    // A member of an unknown class, never a free function:
                    // the bare name would bind none, since a bare PHP call
                    // reaches free functions only at the Global tier.
                    Some(CallSite::untyped_member(method_name))
                }
                "self" | "static" => {
                    if let Some(class_name) = ctx.enclosing_class_name(pos, method_name) {
                        return Some(format!("{class_name}.{method_name}"));
                    }
                    // Declared outside the class body (a trait, an ancestor).
                    Some(CallSite::untyped_member(method_name))
                }
                _ => Some(CallSite::untyped_member(method_name)),
            }
        }
        "name" | "qualified_name" => {
            // Explicit class name: `Foo::bar()` → `Foo.bar`
            let class_name = scope_node.utf8_text(source).ok()?;
            Some(format!("{class_name}.{method_name}"))
        }
        // `$cls::m()`: a runtime class.
        _ => Some(CallSite::untyped_member(method_name)),
    }
}
