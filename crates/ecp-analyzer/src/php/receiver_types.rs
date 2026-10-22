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
use crate::calls::{attach_to_enclosing, construction_call, range_width, start_pos, Pos, PosRange};
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
                    attach_to_enclosing(start_pos(n), callee, nodes);
                } else if has_runtime_name(n) {
                    dynamic_calls.push(n);
                }
            }
            "scoped_call_expression" => {
                if let Some(callee) = php_scoped_callee(n, source, &ctx) {
                    attach_to_enclosing(start_pos(n), callee, nodes);
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
    let value = first.named_child(0).unwrap_or(first);
    match value.kind() {
        "string" | "encapsed_string" => {
            let text = string_literal_text(value, source)?;
            match text.split_once("::") {
                Some((class, method)) if is_ident(method) => Some(LiteralCallable::Static {
                    class: qualified_ident(class)?.to_string(),
                    method: method.to_string(),
                }),
                Some(_) => None,
                None => Some(LiteralCallable::Function(
                    qualified_ident(&text)?.to_string(),
                )),
            }
        }
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

    Some(CallSite::untyped_member(method_name))
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
