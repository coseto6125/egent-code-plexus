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
//!
//! `Name::MAP` matches classes by short name within this file. When the file
//! declares that short name twice, declares it only in another namespace, or
//! also imports it with `use`, the edge still lists the candidates but carries
//! the `-ambiguous` reason, so the reader checks which class the constant
//! belongs to. A qualified `\Ns\Name::MAP` names a class this file cannot
//! prove it declares, so the site stays a blind spot.

use super::laravel_routes::string_value;
use crate::framework_confidence;
use crate::framework_helpers::{enclosing_function_name, node_span};
use ecp_core::analyzer::types::{RawFanoutRef, RawNode};
use rustc_hash::{FxHashMap, FxHashSet};
use tree_sitter::Node;

const REASON: &str = "php-const-map-dispatch";
const REASON_AMBIGUOUS: &str = "php-const-map-dispatch-ambiguous";

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

/// The values a name variable can hold in one function, shared by every
/// dynamic call in that function that uses the same variable.
pub(super) type BoundValuesCache = FxHashMap<(usize, String), Option<BoundValues>>;

#[derive(Clone)]
pub(super) struct BoundValues {
    values: Vec<String>,
    ambiguous: bool,
    /// The variable is passed to a call, whose by-reference parameter can
    /// rebind it: the values stay candidates, but not a complete set.
    passed_as_argument: bool,
}

/// The fan-out of a runtime-named method call, when every value its name
/// variable can hold is a string literal written in this file and the
/// receiver names one class, plus whether the site keeps its blind spot:
/// `$this` and `static` dispatch on the runtime class, which may be a
/// subclass declaring the methods elsewhere. `None` leaves the site a blind
/// spot.
pub(super) fn const_map_fanout(
    call: Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
    cache: &mut BoundValuesCache,
) -> Option<(RawFanoutRef, bool)> {
    let var = call
        .child_by_field_name("name")
        .filter(|n| n.kind() == "variable_name")?
        .utf8_text(source)
        .ok()?;
    let function = enclosing_function(call)?;
    let class = enclosing_class(function);
    let (owner, late_bound) = receiver_owner(call, class, source)?;
    let bound = cache
        .entry((function.id(), var.to_string()))
        .or_insert_with(|| bound_values(function, var, class, source))
        .clone()?;
    let span = node_span(&call);
    let fanout = RawFanoutRef {
        source_name: enclosing_function_name(nodes, span)?,
        candidates: bound
            .values
            .iter()
            .map(|v| format!("{owner}.{v}"))
            .collect(),
        base_confidence: framework_confidence::FANOUT_BASE,
        reason: if bound.ambiguous {
            REASON_AMBIGUOUS
        } else {
            REASON
        }
        .to_string(),
        span,
    };
    Some((fanout, late_bound || bound.passed_as_argument))
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
/// writes it (`Rows`, `\App\Rows`), and whether the receiver is late-bound:
/// `self` / `static` / `$this` name the enclosing class, and `static` /
/// `$this` resolve to the runtime class. `parent::` and any other receiver
/// give `None`.
fn receiver_owner<'s>(
    call: Node<'_>,
    class: Option<Node<'_>>,
    source: &'s [u8],
) -> Option<(&'s str, bool)> {
    if call.kind() == "scoped_call_expression" {
        let scope = call.child_by_field_name("scope")?;
        let text = scope.utf8_text(source).ok()?;
        return match scope.kind() {
            "name" | "qualified_name" => Some((text, false)),
            "relative_scope" if text.eq_ignore_ascii_case("self") => {
                Some((class_name(class?, source)?, false))
            }
            "relative_scope" if text.eq_ignore_ascii_case("static") => {
                Some((class_name(class?, source)?, true))
            }
            _ => None,
        };
    }
    let object = call.child_by_field_name("object")?;
    if object.kind() == "variable_name" && object.utf8_text(source).ok()? == "$this" {
        return Some((class_name(class?, source)?, true));
    }
    None
}

fn is_self_or_static(scope: &str) -> bool {
    scope.eq_ignore_ascii_case("self") || scope.eq_ignore_ascii_case("static")
}

/// Pushes `n`'s named children so the stack pops them in source order, and
/// candidates list the values as written.
fn push_children<'t>(stack: &mut Vec<Node<'t>>, n: Node<'t>) {
    let start = stack.len();
    let mut c = n.walk();
    stack.extend(n.named_children(&mut c));
    stack[start..].reverse();
}

/// Every value `var` can hold in `function`, deduplicated the way PHP matches
/// method names (case-insensitively), spelled as first written. `None` when
/// a parameter, a closure by reference, a write into the string
/// (`$m[0] = 'x'`) or any write other than a plain assignment of an
/// enumerable value binds it, or nothing assigns it.
fn bound_values(
    function: Node<'_>,
    var: &str,
    class: Option<Node<'_>>,
    source: &[u8],
) -> Option<BoundValues> {
    if let Some(params) = function.child_by_field_name("parameters") {
        if mentions(params, var, source) {
            return None;
        }
    }
    let body = function.child_by_field_name("body")?;
    let mut scan = Scan {
        class,
        source,
        values: Vec::new(),
        ambiguous: false,
    };
    let mut assigned = false;
    let mut passed_as_argument = false;
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
                    scan.rhs(parent.child_by_field_name("right")?)?;
                } else if REBINDING_PARENTS.contains(&parent.kind())
                    || inside_list(n, function)
                    || (parent.kind() == "subscript_expression" && parent.named_child(0) == Some(n))
                {
                    return None;
                } else if parent.kind() == "argument" {
                    passed_as_argument = true;
                }
                continue;
            }
            _ => {}
        }
        push_children(&mut stack, n);
    }
    if !assigned {
        return None;
    }
    let mut seen = FxHashSet::default();
    scan.values.retain(|v| seen.insert(v.to_ascii_lowercase()));
    Some(BoundValues {
        values: scan.values,
        ambiguous: scan.ambiguous,
        passed_as_argument,
    })
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

/// The subscript of `C::MAP[...]` when it is a literal key: only that
/// element is reachable.
enum Key {
    Str(String),
    Int(usize),
}

/// Collects the values the assignments of one name variable give.
struct Scan<'t, 's> {
    class: Option<Node<'t>>,
    source: &'s [u8],
    values: Vec<String>,
    ambiguous: bool,
}

impl<'t> Scan<'t, '_> {
    /// One assignment's right side: a string literal, `C::MAP[$k]`, or
    /// `C::MAP[$k] ?? null` / `?? '<literal>'`.
    fn rhs(&mut self, rhs: Node<'t>) -> Option<()> {
        match rhs.kind() {
            "string" | "encapsed_string" => self.values.push(method_literal(rhs, self.source)?),
            "subscript_expression" => self.const_map(rhs)?,
            "binary_expression" => {
                if rhs.child_by_field_name("operator")?.kind() != "??" {
                    return None;
                }
                let left = rhs.child_by_field_name("left")?;
                if left.kind() != "subscript_expression" {
                    return None;
                }
                self.const_map(left)?;
                let default = rhs.child_by_field_name("right")?;
                if default.kind() != "null" {
                    self.values.push(method_literal(default, self.source)?);
                }
            }
            _ => return None,
        }
        Some(())
    }

    /// The values of `self::MAP[..]` / `static::MAP[..]` / `Name::MAP[..]`:
    /// the constant must be declared on that class in this file, as an array
    /// of string literals. A literal subscript keeps only its element.
    fn const_map(&mut self, subscript: Node<'t>) -> Option<()> {
        let source = self.source;
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
        let owners = match scope.kind() {
            "relative_scope" if is_self_or_static(scope_text) => vec![self.class?],
            "name" => {
                let root = file_root(subscript);
                let owners = file_classes(root, scope_text, source);
                if owners.is_empty() {
                    return None;
                }
                let here = namespace_of(subscript, source);
                self.ambiguous |= owners.len() > 1
                    || owners.iter().any(|o| namespace_of(*o, source) != here)
                    || imports_short_name(root, scope_text, source);
                owners
            }
            _ => return None,
        };
        let key = literal_key(subscript, source);
        let mut found = false;
        for owner in owners {
            if let Some(array) = const_initializer(owner, constant, source) {
                self.push_array_values(array, key.as_ref())?;
                found = true;
            }
        }
        found.then_some(())
    }

    /// The values of an array of string literals, or only the element `key`
    /// names; `None` when no value is pushed.
    fn push_array_values(&mut self, array: Node<'_>, key: Option<&Key>) -> Option<()> {
        let source = self.source;
        let mut c = array.walk();
        let elements: Vec<Vec<Node<'_>>> = array
            .named_children(&mut c)
            .filter(|n| n.kind() == "array_element_initializer")
            .map(|element| {
                let mut ec = element.walk();
                element
                    .named_children(&mut ec)
                    .filter(|n| n.kind() != "comment")
                    .collect()
            })
            .collect();
        let keyed = elements.iter().any(|parts| parts.len() == 2);
        let mut count = 0usize;
        for (position, parts) in elements.iter().enumerate() {
            if parts
                .iter()
                .any(|p| matches!(p.kind(), "by_ref" | "variadic_unpacking"))
            {
                return None;
            }
            let selected = match (key, parts.as_slice()) {
                (None, _) => true,
                (Some(Key::Str(k)), [name, _]) => string_value(*name, source).as_ref() == Some(k),
                (Some(Key::Int(i)), [index, _]) => {
                    index.kind() == "integer"
                        && index.utf8_text(source).ok()?.parse::<usize>().ok() == Some(*i)
                }
                // An unkeyed element's index is its position only in an array
                // with no explicit keys.
                (Some(Key::Int(i)), [_]) => !keyed && position == *i,
                _ => false,
            };
            if selected {
                self.values.push(method_literal(*parts.last()?, source)?);
                count += 1;
            }
        }
        (count > 0).then_some(())
    }
}

fn literal_key(subscript: Node<'_>, source: &[u8]) -> Option<Key> {
    let index = subscript.named_child(1)?;
    match index.kind() {
        "string" | "encapsed_string" => string_value(index, source).map(Key::Str),
        "integer" => index.utf8_text(source).ok()?.parse().ok().map(Key::Int),
        _ => None,
    }
}

fn file_root(node: Node<'_>) -> Node<'_> {
    let mut root = node;
    while let Some(p) = root.parent() {
        root = p;
    }
    root
}

/// The namespace `node` sits in: the braced `namespace X { }` around it, or
/// the last `namespace X;` statement before it. `None` is the global one.
fn namespace_of<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    let mut top = node;
    while let Some(p) = top.parent() {
        match p.kind() {
            "namespace_definition" => return p.child_by_field_name("name")?.utf8_text(source).ok(),
            "program" => break,
            _ => top = p,
        }
    }
    let mut sibling = top.prev_named_sibling();
    while let Some(s) = sibling {
        if s.kind() == "namespace_definition" {
            return s.child_by_field_name("name")?.utf8_text(source).ok();
        }
        sibling = s.prev_named_sibling();
    }
    None
}

/// Every `class_declaration` named `name` in the file.
fn file_classes<'t>(root: Node<'t>, name: &str, source: &[u8]) -> Vec<Node<'t>> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "class_declaration" {
            if class_name(n, source) == Some(name) {
                found.push(n);
            }
            continue;
        }
        push_children(&mut stack, n);
    }
    found
}

/// Whether a class `use` in the file binds `short` (by alias or last
/// segment). `use` declarations sit at file level or in a namespace body.
fn imports_short_name(root: Node<'_>, short: &str, source: &[u8]) -> bool {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "program"
            | "namespace_definition"
            | "compound_statement"
            | "namespace_use_declaration"
            | "namespace_use_group" => {
                let mut c = n.walk();
                stack.extend(n.named_children(&mut c));
            }
            "namespace_use_clause" => {
                // `use function` / `use const` sits on the declaration, or on
                // the clause inside a `use A\{function f}` group.
                let declaration = n.parent().and_then(|p| match p.kind() {
                    "namespace_use_group" => p.parent(),
                    _ => Some(p),
                });
                let is_class = n.child_by_field_name("type").is_none()
                    && declaration.is_none_or(|d| d.child_by_field_name("type").is_none());
                let bound = n.child_by_field_name("alias").or_else(|| {
                    let mut c = n.walk();
                    let name = n
                        .named_children(&mut c)
                        .find(|ch| matches!(ch.kind(), "name" | "qualified_name"));
                    name
                });
                let bound_short = bound
                    .and_then(|b| b.utf8_text(source).ok())
                    .and_then(|t| t.rsplit('\\').next());
                if is_class && bound_short == Some(short) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
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
