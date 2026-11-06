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
//!
//! A receiver that names a class of the same file and no import or local
//! binds (`Util.make()`) emits `Util.make`, as a typed receiver does. It
//! cannot reuse [`CallSite::from_receiver`]: for Python that site means a
//! `from`-imported name, which the resolver binds as a submodule, else as a
//! type it names, else by its bare member through the caller's own type and
//! bases.

use super::path_literals::build_raw_path_literal;
use crate::calls::{
    attach_to_enclosing, is_call_callee, is_same_kind_field, node_range, range_width,
    reference_function, start_pos, unparenthesized, CallSiteIndex, Pos, PosRange,
};
use crate::dynamic_imports::{
    binding_mentions, is_field, may_load, rebound, synthetic_imports, DynamicBinding,
    DynamicImports,
};
use crate::framework_helpers::strip_python_string_quotes;
use ecp_core::analyzer::types::{CallSite, RawNode, RawPathLiteral, RawSqlRef};
use ecp_core::graph::NodeKind;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

/// A load assigned in one scope: the assigned name, its statement, and the
/// module specifier.
type ScopeLoad<'t, 's> = (Node<'t>, Node<'t>, &'s str);

/// Map of nested function scopes (by position range) to their var→type bindings.
/// Lookup picks the smallest containing scope that has the variable, so
/// closures correctly inherit outer-scope annotations.
#[derive(Debug, Default)]
pub struct LocalTypes {
    scopes: Vec<(PosRange, HashMap<String, String>)>,
    /// A `from` binding may be an object; only a module import proves that
    /// its receiver names a namespace for import-scoped resolution.
    imported: HashMap<String, ImportedBinding>,
    shadows: Vec<ScopeBindings>,
}

/// The names one scope binds (a function, lambda or comprehension): a
/// parameter or comprehension variable at 0, an assignment, `for` target,
/// `with ... as` name or `case` capture at its byte offset.
#[derive(Debug)]
struct ScopeBindings {
    bytes: (usize, usize),
    /// Bytes inside `bytes` that the enclosing scope evaluates: the first
    /// iterable of a comprehension.
    outside: Option<(usize, usize)>,
    start: Pos,
    names: HashMap<String, usize>,
    /// Names a `global` statement declares: they name module bindings, so an
    /// enclosing function's variable of that name is not visible.
    globals: HashSet<String>,
    /// The names this scope's own body binds, each with the bytes from which a
    /// read sees the new binding: the end of `xs` in `for obj in xs:`, the end
    /// of the statement for an assignment. `true` marks a write; `false` an
    /// annotated assignment, which gives the name the type a read resolves
    /// through. It keeps `global` and `nonlocal` names, whose writes replace
    /// a binding of another scope.
    rebinds: HashMap<String, Vec<(usize, bool)>>,
}

impl ScopeBindings {
    fn encloses(&self, node: Node<'_>) -> bool {
        let within =
            |(start, end): (usize, usize)| start <= node.start_byte() && node.end_byte() <= end;
        within(self.bytes) && !self.outside.is_some_and(within)
    }

    /// Whether the last binding of `name` before `receiver` is a write.
    fn rebinds_before(&self, name: &str, receiver: Node<'_>) -> bool {
        self.rebinds
            .get(name)
            .and_then(|events| {
                events
                    .iter()
                    .filter(|(from, _)| *from <= receiver.start_byte())
                    .max_by_key(|(from, _)| *from)
            })
            .is_some_and(|&(_, writes)| writes)
    }
}

fn note_rebinds(
    bound: HashMap<String, usize>,
    writes: bool,
    rebinds: &mut HashMap<String, Vec<(usize, bool)>>,
) {
    for (name, from) in bound {
        rebinds.entry(name).or_default().push((from, writes));
    }
}

#[derive(Debug, Clone, Copy)]
enum ImportedBinding {
    Module,
    Symbol,
}

impl LocalTypes {
    fn lookup(&self, pos: Pos, var: &str) -> Option<&str> {
        self.nearest_typed(pos, var).map(|(_, ty)| ty)
    }

    /// The start of the smallest scope around `pos` that types `var`, and the type.
    fn nearest_typed(&self, pos: Pos, var: &str) -> Option<(Pos, &str)> {
        let mut best: Option<(Pos, &str)> = None;
        let mut best_width = (u32::MAX, i64::MAX);
        for ((start, end), map) in &self.scopes {
            if *start <= pos && pos <= *end {
                if let Some(t) = map.get(var) {
                    let w = range_width((*start, *end));
                    if w < best_width {
                        best_width = w;
                        best = Some((*start, t.as_str()));
                    }
                }
            }
        }
        best
    }

    /// [`LocalTypes::lookup`] for a receiver in a value position: the type
    /// stands only when no scope nearer than the one that typed `var` rebinds
    /// the name (`def inner(x): return x.m` inside `def outer(x: A)`, or a
    /// lambda, comprehension, `for`, `with ... as`, `case` capture or
    /// `global` that binds it).
    fn lookup_visible(&self, receiver: Node<'_>, var: &str) -> Option<&str> {
        let (typed_start, ty) = self.nearest_typed(start_pos(receiver), var)?;
        let rebound_nearer = self.shadows.iter().any(|f| {
            f.encloses(receiver)
                && ((f.start > typed_start
                    && (f.names.contains_key(var) || f.globals.contains(var)))
                    || (f.start >= typed_start && f.rebinds_before(var, receiver)))
        });
        (!rebound_nearer).then_some(ty)
    }

    fn imported_receiver(&self, receiver: Node<'_>, source: &[u8]) -> Option<ImportedBinding> {
        let mut root = receiver;
        while root.kind() == "attribute" {
            root = root.child_by_field_name("object")?;
        }
        if root.kind() != "identifier" {
            return None;
        }
        let name = root.utf8_text(source).ok()?;
        if self.shadowed(receiver, name) {
            return None;
        }
        self.imported.get(name).copied()
    }

    /// Whether a function scope enclosing `receiver` binds `name` (a
    /// parameter, or an assignment before `receiver`).
    fn shadowed(&self, receiver: Node<'_>, name: &str) -> bool {
        self.shadows.iter().any(|f| {
            f.encloses(receiver)
                && (f
                    .names
                    .get(name)
                    .is_some_and(|assigned| *assigned <= receiver.start_byte())
                    || f.rebinds_before(name, receiver))
        })
    }
}

/// Walk every `function_definition` node, collecting typed parameters and
/// annotated assignments inside the function body, and every name an
/// `import` statement binds.
pub fn collect_local_types(root: Node<'_>, source: &[u8]) -> LocalTypes {
    let mut scopes: Vec<(PosRange, HashMap<String, String>)> = Vec::new();
    let mut imported = HashMap::new();
    let mut shadows = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "import_statement" | "import_from_statement") {
            collect_import_bindings(n, source, &mut imported);
        }
        if let Some(bindings) = inline_scope_bindings(n, source) {
            shadows.push(bindings);
        }
        if n.kind() == "function_definition" {
            let fn_span = node_range(n);
            let mut map: HashMap<String, String> = HashMap::new();
            let mut names = HashMap::new();
            let mut globals = HashSet::new();
            let mut nonlocals = HashSet::new();
            let mut rebinds = HashMap::new();

            if let Some(params) = n.child_by_field_name("parameters") {
                collect_typed_params(params, source, &mut map);
                if let Some((receiver, class)) = method_receiver(n, params, source) {
                    map.entry(receiver.to_string())
                        .or_insert_with(|| class.to_string());
                }
                let mut cursor = params.walk();
                for parameter in params.named_children(&mut cursor) {
                    let binding = match parameter.kind() {
                        "default_parameter" | "typed_default_parameter" => {
                            parameter.child_by_field_name("name")
                        }
                        "typed_parameter" => parameter.named_child(0),
                        _ => Some(parameter),
                    };
                    if let Some(binding) = binding {
                        collect_shadow_names(binding, source, 0, &mut names);
                    }
                }
            }

            if let Some(body) = n.child_by_field_name("body") {
                collect_typed_assignments(body, source, &mut map);
                let mut pending = vec![body];
                while let Some(node) = pending.pop() {
                    let target = match node.kind() {
                        "function_definition" | "class_definition" | "lambda" => continue,
                        "assignment"
                        | "augmented_assignment"
                        | "named_expression"
                        | "for_statement" => node
                            .child_by_field_name("left")
                            .or_else(|| node.child_by_field_name("name")),
                        "as_pattern" => node.child_by_field_name("alias"),
                        "case_clause" => {
                            let mut cursor = node.walk();
                            for pattern in node
                                .named_children(&mut cursor)
                                .filter(|c| c.kind() == "case_pattern")
                            {
                                collect_case_captures(
                                    pattern,
                                    source,
                                    node.start_byte(),
                                    &mut names,
                                );
                                let mut bound = HashMap::new();
                                collect_case_captures(
                                    pattern,
                                    source,
                                    pattern.end_byte(),
                                    &mut bound,
                                );
                                note_rebinds(bound, true, &mut rebinds);
                            }
                            None
                        }
                        "global_statement" | "nonlocal_statement" => {
                            let declared = if node.kind() == "global_statement" {
                                &mut globals
                            } else {
                                &mut nonlocals
                            };
                            let mut cursor = node.walk();
                            declared.extend(
                                node.named_children(&mut cursor)
                                    .filter_map(|c| c.utf8_text(source).ok())
                                    .map(str::to_string),
                            );
                            None
                        }
                        _ => None,
                    };
                    if let Some(target) = target {
                        collect_shadow_names(target, source, node.start_byte(), &mut names);
                        let declares_type = target.kind() == "identifier"
                            && node.child_by_field_name("type").is_some_and(|ty| {
                                simple_name_and_type(target, ty, source).is_some()
                            });
                        let from = match node.kind() {
                            "for_statement" => node.child_by_field_name("right"),
                            _ => Some(node),
                        };
                        let from = from.map_or(node.end_byte(), |n| n.end_byte());
                        let mut bound = HashMap::new();
                        collect_shadow_names(target, source, from, &mut bound);
                        note_rebinds(bound, !declares_type, &mut rebinds);
                    }
                    let mut cursor = node.walk();
                    pending.extend(node.named_children(&mut cursor));
                }
            }
            // A `global` or `nonlocal` name is a binding of another scope, not
            // a new local of this function.
            names.retain(|name, _| !globals.contains(name) && !nonlocals.contains(name));

            if !map.is_empty() {
                scopes.push((fn_span, map));
            }
            if !names.is_empty() || !globals.is_empty() || !rebinds.is_empty() {
                shadows.push(ScopeBindings {
                    bytes: (n.start_byte(), n.end_byte()),
                    outside: None,
                    start: fn_span.0,
                    names,
                    globals,
                    rebinds,
                });
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    LocalTypes {
        scopes,
        imported,
        shadows,
    }
}

/// The names a lambda (its parameters) or a comprehension (its `for`
/// variables) binds. A lambda parameter is visible in the lambda body only:
/// a default runs in the enclosing scope. A comprehension variable is
/// visible in the element, the conditions and every `for` clause but the
/// first iterable, which the enclosing scope evaluates.
fn inline_scope_bindings(n: Node<'_>, source: &[u8]) -> Option<ScopeBindings> {
    let mut names = HashMap::new();
    let mut bytes = (n.start_byte(), n.end_byte());
    let mut outside = None;
    match n.kind() {
        "lambda" => {
            let body = n.child_by_field_name("body")?;
            bytes = (body.start_byte(), body.end_byte());
            let params = n.child_by_field_name("parameters")?;
            let mut cursor = params.walk();
            for parameter in params.named_children(&mut cursor) {
                let binding = match parameter.kind() {
                    "default_parameter" | "typed_default_parameter" => {
                        parameter.child_by_field_name("name")
                    }
                    _ => Some(parameter),
                };
                if let Some(binding) = binding {
                    collect_shadow_names(binding, source, 0, &mut names);
                }
            }
        }
        "list_comprehension"
        | "set_comprehension"
        | "dictionary_comprehension"
        | "generator_expression" => {
            let mut cursor = n.walk();
            for clause in n
                .named_children(&mut cursor)
                .filter(|clause| clause.kind() == "for_in_clause")
            {
                if let Some(left) = clause.child_by_field_name("left") {
                    collect_shadow_names(left, source, 0, &mut names);
                }
                if outside.is_none() {
                    let mut iterables = clause.walk();
                    let mut right = clause.children_by_field_name("right", &mut iterables);
                    outside = right.next().map(|first| {
                        (first.start_byte(), right.last().unwrap_or(first).end_byte())
                    });
                }
            }
        }
        _ => return None,
    }
    (!names.is_empty()).then(|| ScopeBindings {
        bytes,
        outside,
        start: node_range(n).0,
        names,
        globals: HashSet::new(),
        rebinds: HashMap::new(),
    })
}

/// The names a `case` pattern captures: a bare name, `*rest` / `**rest`, and
/// the name after `as`. A dotted name is a value pattern; neither the class of
/// a class pattern nor the keyword of a keyword pattern binds.
fn collect_case_captures(
    pattern: Node<'_>,
    source: &[u8],
    position: usize,
    names: &mut HashMap<String, usize>,
) {
    let skip = usize::from(matches!(
        pattern.kind(),
        "class_pattern" | "keyword_pattern"
    ));
    match pattern.kind() {
        "dotted_name" if pattern.named_child_count() == 1 => {
            if let Some(name) = pattern.named_child(0) {
                collect_shadow_names(name, source, position, names);
            }
        }
        "identifier" => collect_shadow_names(pattern, source, position, names),
        "case_pattern" | "as_pattern" | "list_pattern" | "tuple_pattern" | "union_pattern"
        | "splat_pattern" | "class_pattern" | "keyword_pattern" => {
            let mut cursor = pattern.walk();
            for child in pattern.named_children(&mut cursor).skip(skip) {
                collect_case_captures(child, source, position, names);
            }
        }
        // A key is a literal or a value pattern; only values and `**rest`
        // capture.
        "dict_pattern" => {
            let mut cursor = pattern.walk();
            for child in pattern
                .named_children(&mut cursor)
                .filter(|c| matches!(c.kind(), "case_pattern" | "splat_pattern"))
            {
                collect_case_captures(child, source, position, names);
            }
        }
        _ => {}
    }
}

fn collect_shadow_names(
    node: Node<'_>,
    source: &[u8],
    position: usize,
    names: &mut HashMap<String, usize>,
) {
    match node.kind() {
        "identifier" => {
            if let Ok(name) = node.utf8_text(source) {
                names
                    .entry(name.to_string())
                    .and_modify(|previous| *previous = (*previous).min(position))
                    .or_insert(position);
            }
        }
        "pattern_list"
        | "tuple_pattern"
        | "list_pattern"
        | "tuple"
        | "list"
        | "as_pattern_target"
        | "list_splat_pattern"
        | "dictionary_splat_pattern" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_shadow_names(child, source, position, names);
            }
        }
        _ => {}
    }
}

/// The local names one import statement binds: `import a.b` → `a`,
/// `import a as x` / `from m import n as x` → `x`, `from m import n` → `n`.
fn collect_import_bindings(
    stmt: Node<'_>,
    source: &[u8],
    out: &mut HashMap<String, ImportedBinding>,
) {
    let mut c = stmt.walk();
    for name in stmt.children_by_field_name("name", &mut c) {
        let bound = match name.kind() {
            "aliased_import" => name.child_by_field_name("alias"),
            "dotted_name" => name.named_child(0),
            _ => None,
        };
        if let Some(text) = bound.and_then(|b| b.utf8_text(source).ok()) {
            let binding = if stmt.kind() == "import_statement" {
                ImportedBinding::Module
            } else {
                ImportedBinding::Symbol
            };
            out.insert(text.to_string(), binding);
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

/// The module whose `import_module` function loads a module by name.
const LOADER_MODULE: &str = "importlib";
const LOADER_FUNCTION: &str = "import_module";

/// Whether `node` only reads the value of its name.
fn is_read_position(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "attribute" => is_field(parent, "object", node),
        "call" => is_field(parent, "function", node),
        "keyword_argument"
        | "named_expression"
        | "default_parameter"
        | "typed_default_parameter" => is_field(parent, "value", node),
        "assignment" | "augmented_assignment" | "for_statement" | "for_in_clause" => {
            is_field(parent, "right", node)
        }
        "lambda" => is_field(parent, "body", node),
        "if_statement" | "elif_clause" | "while_statement" => is_field(parent, "condition", node),
        // A container reads its items when it is itself read: `del (m, x)`
        // and a destructuring target do not.
        "parenthesized_expression"
        | "tuple"
        | "list"
        | "set"
        | "expression_list"
        | "list_splat"
        | "dictionary_splat" => is_read_position(parent),
        "argument_list"
        | "assert_statement"
        | "await"
        | "binary_operator"
        | "boolean_operator"
        | "comparison_operator"
        | "conditional_expression"
        | "decorator"
        | "dictionary_comprehension"
        | "expression_statement"
        | "generator_expression"
        | "if_clause"
        | "interpolation"
        | "list_comprehension"
        | "not_operator"
        | "pair"
        | "raise_statement"
        | "return_statement"
        | "set_comprehension"
        | "slice"
        | "subscript"
        | "unary_operator"
        | "yield" => true,
        _ => false,
    }
}

/// Whether the identifier `node` names no variable: an attribute name, or the
/// module of a `from` import.
fn names_no_variable(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "attribute" => is_field(parent, "attribute", node),
        "dotted_name" => parent.parent().is_some_and(|g| {
            g.kind() == "import_from_statement" && is_field(g, "module_name", parent)
        }),
        _ => false,
    }
}

/// Whether the leaf `node` is an identifier that binds or writes its name.
/// Any mention that is not a plain read counts, so a parameter, a loop
/// target, an `import`, a `global` and a `del` all do.
fn binds_name(node: Node<'_>) -> bool {
    node.kind() == "identifier" && !names_no_variable(node) && !is_read_position(node)
}

/// The one identifier in the file under `root` that binds or writes `name`,
/// when exactly one does ([`binds_name`]).
pub(super) fn sole_binding<'t>(root: Node<'t>, name: &str, source: &[u8]) -> Option<Node<'t>> {
    match binding_mentions(root, &[name], source, binds_name)
        .get(name)?
        .as_slice()
    {
        [only] => Some(*only),
        _ => None,
    }
}

/// Where a loader name is bound: the byte range it is visible in, and the
/// byte offset from which it is bound.
struct LoaderBinding {
    scope: (usize, usize),
    from: usize,
}

impl LoaderBinding {
    fn covers(&self, call: Node<'_>) -> bool {
        self.scope.0 <= call.start_byte()
            && call.end_byte() <= self.scope.1
            && self.from <= call.start_byte()
    }
}

/// The binding of `name` (`importlib` or `import_module`) when exactly one
/// plain import statement binds it in the whole file and nothing else binds
/// or writes it: `import importlib`, `from importlib import import_module`.
/// An alias, a `from` import of a different module, a class-body import or a
/// second binding gives `None`.
fn loader_binding(root: Node<'_>, source: &[u8], name: &str) -> Option<LoaderBinding> {
    let mention = sole_binding(root, name, source)?;
    let dotted = mention.parent().filter(|p| p.kind() == "dotted_name")?;
    let statement = dotted.parent()?;
    let plain = match (name, statement.kind()) {
        (LOADER_MODULE, "import_statement") => dotted
            .named_child(0)
            .is_some_and(|first| first.id() == mention.id()),
        (LOADER_FUNCTION, "import_from_statement") => {
            !is_field(statement, "module_name", dotted)
                && statement
                    .child_by_field_name("module_name")
                    .and_then(|module| module.utf8_text(source).ok())
                    == Some(LOADER_MODULE)
        }
        _ => false,
    };
    if !plain {
        return None;
    }
    let mut scope = (root.start_byte(), root.end_byte());
    let mut ancestor = statement.parent();
    while let Some(a) = ancestor {
        match a.kind() {
            "function_definition" => {
                scope = (a.start_byte(), a.end_byte());
                break;
            }
            "class_definition" => return None,
            _ => ancestor = a.parent(),
        }
    }
    Some(LoaderBinding {
        scope,
        from: statement.end_byte(),
    })
}

/// The module name a string literal spells when it is an absolute dotted name
/// written without prefix, escape or interpolation.
fn literal_module_name(literal: Node<'_>, source: &[u8]) -> Option<String> {
    if literal.kind() != "string" {
        return None;
    }
    let text = literal.utf8_text(source).ok()?;
    if !matches!(text.as_bytes().first(), Some(b'"' | b'\'')) {
        return None;
    }
    let inner = strip_python_string_quotes(text)?;
    (!inner.contains('\\')
        && inner
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_alphanumeric() || c == '_')))
    .then(|| inner.to_string())
}

/// A call `importlib.import_module("<lit>")` (`via_module`) or
/// `import_module("<lit>")` with one positional literal argument: its
/// specifier. Whether the callee really is the loader is [`loader_binding`]'s.
fn loader_call(call: Node<'_>, source: &[u8]) -> Option<(bool, String)> {
    let function = call.child_by_field_name("function")?;
    let text = |node: Node<'_>| node.utf8_text(source).ok();
    let via_module = match function.kind() {
        "attribute" => {
            let object = function.child_by_field_name("object")?;
            let attribute = function.child_by_field_name("attribute")?;
            if object.kind() != "identifier"
                || text(object) != Some(LOADER_MODULE)
                || text(attribute) != Some(LOADER_FUNCTION)
            {
                return None;
            }
            true
        }
        "identifier" if text(function) == Some(LOADER_FUNCTION) => false,
        _ => return None,
    };
    let arguments = call.child_by_field_name("arguments")?;
    if arguments.kind() != "argument_list" || arguments.named_child_count() != 1 {
        return None;
    }
    Some((
        via_module,
        literal_module_name(arguments.named_child(0)?, source)?,
    ))
}

/// A literal load that is the whole right side of `name = <load>`, a direct
/// statement of a function or module body: its scope, name and statement end.
fn direct_assignment(call: Node<'_>) -> Option<(Node<'_>, Node<'_>, Node<'_>)> {
    let assignment = call.parent().filter(|a| {
        a.kind() == "assignment"
            && is_field(*a, "right", call)
            && a.child_by_field_name("type").is_none()
    })?;
    let target = assignment
        .child_by_field_name("left")
        .filter(|left| left.kind() == "identifier")?;
    let statement = assignment
        .parent()
        .filter(|s| s.kind() == "expression_statement")?;
    let container = statement.parent()?;
    let scope = match container.kind() {
        "module" => container,
        "block" => container
            .parent()
            .filter(|f| f.kind() == "function_definition" && is_field(*f, "body", container))?,
        _ => return None,
    };
    Some((scope, target, statement))
}

/// Collect the synthetic imports of every literal
/// `importlib.import_module("<absolute name>")`, and bind the names that hold
/// a loaded module ([`DynamicImports`]).
///
/// A call counts only when `importlib` (or `import_module`) is bound by one
/// plain import that precedes it and nothing else in the file binds the name.
/// A name binds only when its assignment `m = <load>` is a direct statement
/// of a function or module body and `m` is declared or written nowhere else
/// in that scope's subtree. A use before the assignment never binds. Every
/// other shape (`__import__`, a relative name, a `package` argument, a
/// non-literal argument) keeps today's result.
pub fn collect_dynamic_imports(root: Node<'_>, source: &[u8]) -> DynamicImports {
    // The walk below costs a pass over the tree, and every load spells its loader.
    if !may_load(source, &[LOADER_FUNCTION]) {
        return DynamicImports::default();
    }
    let mut loads: Vec<(Node<'_>, bool, String)> = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            // A star import may rebind any name, loader included.
            "wildcard_import" => return DynamicImports::default(),
            "call" => {
                if let Some((via_module, specifier)) = loader_call(n, source) {
                    loads.push((n, via_module, specifier));
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        stack.extend(n.children(&mut c));
    }

    let binding_of = |via_module: bool| {
        let name = if via_module {
            LOADER_MODULE
        } else {
            LOADER_FUNCTION
        };
        loads
            .iter()
            .any(|(_, via, _)| *via == via_module)
            .then(|| loader_binding(root, source, name))
            .flatten()
    };
    let (module_binding, function_binding) = (binding_of(true), binding_of(false));
    loads.retain(|(call, via_module, _)| {
        let binding = if *via_module {
            &module_binding
        } else {
            &function_binding
        };
        binding.as_ref().is_some_and(|b| b.covers(*call))
    });
    if loads.is_empty() {
        return DynamicImports::default();
    }
    loads.sort_by_key(|(call, ..)| call.start_byte());
    let (imports, aliases) = synthetic_imports(loads.iter().map(|(.., spec)| spec.as_str()));

    // One walk per scope checks every name assigned a load there.
    let mut scopes: Vec<(Node<'_>, Vec<ScopeLoad<'_, '_>>)> = Vec::new();
    for (call, _, specifier) in &loads {
        let Some((scope, target, statement)) = direct_assignment(*call) else {
            continue;
        };
        let entry = (target, statement, specifier.as_str());
        match scopes.iter_mut().find(|(s, _)| s.id() == scope.id()) {
            Some((_, entries)) => entries.push(entry),
            None => scopes.push((scope, vec![entry])),
        }
    }
    let mut bindings = Vec::new();
    for (scope, entries) in &scopes {
        let names: Vec<&str> = entries
            .iter()
            .filter_map(|(target, ..)| target.utf8_text(source).ok())
            .collect();
        let mentions = binding_mentions(*scope, &names, source, binds_name);
        for (target, statement, specifier) in entries {
            let Ok(name) = target.utf8_text(source) else {
                continue;
            };
            if !rebound(mentions.get(name), (target.start_byte(), target.end_byte())) {
                bindings.push(DynamicBinding {
                    name: name.to_string(),
                    range: (node_range(*statement).1, node_range(*scope).1),
                    alias: aliases[*specifier].clone(),
                    member: None,
                });
            }
        }
    }
    DynamicImports {
        imports,
        bindings,
        loads: loads.iter().map(|(call, ..)| node_range(*call)).collect(),
    }
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
    dynamic: &DynamicImports,
    call_sites: &mut CallSiteIndex,
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let classes: HashSet<String> = nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Class | NodeKind::Enum))
        .map(|n| n.name.clone())
        .collect();
    let mut stack: Vec<(Node<'_>, Option<Node<'_>>)> = vec![(root, None)];
    while let Some((n, parent)) = stack.pop() {
        match n.kind() {
            "call" => {
                if let Some(callee) = python_callee_name(n, source, locals, dynamic, &classes) {
                    if let Some(site) = attach_to_enclosing(start_pos(n), callee, nodes) {
                        call_sites.insert(n.id(), site);
                    }
                }
            }
            "attribute" => {
                if let Some(name) = bound_method_value(n, parent, source, locals) {
                    reference_function(start_pos(n), &name, nodes);
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
                            if let Some(mut sql_ref) =
                                crate::sql_literal::try_sql_ref(value, n, (None, None))
                            {
                                (sql_ref.enclosing_symbol, sql_ref.enclosing_owner) =
                                    enclosing_symbol_and_owner(n, source);
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
            stack.push((child, Some(n)));
        }
    }
    (path_literals, sql_refs)
}

/// `obj.m` in value position (`map(obj.m, xs)`, `f = self.m`) where `obj` is
/// `self` / `cls` or a parameter or local annotated with a class `T`: the
/// method `T.m`, typed as the call `obj.m()` is. The name scan drops `obj.m`,
/// as `obj` is a local, and records no `self.m`. A callee, the object of a
/// longer attribute, an assignment or `del` target and a receiver of no known
/// type name nothing; a data attribute `obj.n` is no callable, so Pass 2
/// binds no edge to it.
fn bound_method_value(
    attribute: Node<'_>,
    parent: Option<Node<'_>>,
    source: &[u8],
    locals: &LocalTypes,
) -> Option<String> {
    let object = attribute
        .child_by_field_name("object")
        .filter(|object| object.kind() == "identifier")?;
    let member = attribute
        .child_by_field_name("attribute")
        .filter(|member| member.kind() == "identifier")?;
    let parent = parent?;
    let written = match parent.kind() {
        "assignment" | "augmented_assignment" => {
            parent.child_by_field_name("left") == Some(attribute)
        }
        "delete_statement" => true,
        _ => false,
    };
    if written
        || is_same_kind_field(attribute, parent, "object")
        || is_call_callee(attribute, Some(parent))
    {
        return None;
    }
    let ty = locals.lookup_visible(attribute, object.utf8_text(source).ok()?)?;
    Some(format!("{ty}.{}", member.utf8_text(source).ok()?))
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

/// `classes`: the names of the classes this file declares.
fn python_callee_name(
    call: Node<'_>,
    source: &[u8],
    locals: &LocalTypes,
    dynamic: &DynamicImports,
    classes: &HashSet<String>,
) -> Option<String> {
    let function = unparenthesized(call.child_by_field_name("function")?);
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
                    let pos = start_pos(call);
                    if let Some(callee) = dynamic.member_callee(pos, obj_name, attr_name) {
                        return Some(callee);
                    }
                    if let Some(ty) = locals.lookup(pos, obj_name) {
                        return Some(format!("{ty}.{attr_name}"));
                    }
                }
                if is_super_call(obj, source) {
                    return Some(format!("{}.{attr_name}", CallSite::SUPER_RECEIVER));
                }
                // Only a module receiver (`widget.Widget()`) can name a class
                // to construct; any other untyped receiver calls a method,
                // and a class may be spelled in any case (`class widget`).
                match locals.imported_receiver(obj, source) {
                    Some(ImportedBinding::Module) => {
                        let mut segments = Vec::new();
                        let mut node = function;
                        while node.kind() == "attribute" {
                            segments.push(
                                node.child_by_field_name("attribute")?
                                    .utf8_text(source)
                                    .ok()?,
                            );
                            node = node.child_by_field_name("object")?;
                        }
                        segments.push(node.utf8_text(source).ok()?);
                        segments.reverse();
                        return Some(segments.join("."));
                    }
                    // A `from` binding may name a submodule, which only the
                    // resolver can tell from an object. Keep nested
                    // attributes in the receiver path so the resolver can
                    // distinguish `mod.client.m()` from `mod.m()`.
                    Some(ImportedBinding::Symbol) => {
                        return match attribute_receiver_path(obj, source) {
                            Some(receiver) => Some(CallSite::from_receiver(&receiver, attr_name)),
                            // `sym.attr.m()`: a member call on a value the import
                            // names; a bare `m` would read as receiverless.
                            None => Some(CallSite::untyped_member(attr_name)),
                        };
                    }
                    None => {
                        let class = (obj.kind() == "identifier")
                            .then(|| obj.utf8_text(source).ok())
                            .flatten()
                            .filter(|name| classes.contains(*name))
                            .filter(|name| !locals.shadowed(obj, name));
                        return Some(match class {
                            Some(class) => format!("{class}.{attr_name}"),
                            // `make().bar()`: the resolver types the receiver
                            // by the callee's return type or constructed class.
                            None if obj.kind() == "call" => {
                                let untyped = CallSite::untyped_member(attr_name);
                                // A callee that a parameter or local binds
                                // (`make = lambda: Beta()`) is a value, not
                                // the same-named function that would type it.
                                let calls_local = obj
                                    .child_by_field_name("function")
                                    .filter(|f| f.kind() == "identifier")
                                    .and_then(|f| f.utf8_text(source).ok())
                                    .is_some_and(|name| locals.shadowed(obj, name));
                                if calls_local {
                                    return Some(untyped);
                                }
                                match CallSite::nested_chain(|| {
                                    python_callee_name(obj, source, locals, dynamic, classes)
                                }) {
                                    Some(inner) => CallSite::chained_member(&inner, &untyped),
                                    None => untyped,
                                }
                            }
                            None => CallSite::untyped_member(attr_name),
                        });
                    }
                }
            }
            Some(attr_name.to_string())
        }
        _ => None,
    }
}

/// The identifier-led attribute path of `node`, joined for an internal call
/// receiver. A slash is not valid in a Python identifier, so it preserves the
/// final call member separator in [`CallSite::from_receiver`].
fn attribute_receiver_path(mut node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut segments = Vec::new();
    while node.kind() == "attribute" {
        segments.push(
            node.child_by_field_name("attribute")?
                .utf8_text(source)
                .ok()?,
        );
        node = node.child_by_field_name("object")?;
    }
    if node.kind() != "identifier" {
        return None;
    }
    segments.push(node.utf8_text(source).ok()?);
    segments.reverse();
    Some(segments.join("/"))
}

/// The first parameter of a method declared in a class body (`self`, or
/// `cls` of a classmethod) and the class it stands for, so `self.m()` is
/// typed as `C.m` the way Java types `this.m()`. A `@staticmethod` has no
/// such parameter.
fn method_receiver<'s>(
    function: Node<'_>,
    params: Node<'_>,
    source: &'s [u8],
) -> Option<(&'s str, &'s str)> {
    let mut member = function;
    if let Some(decorated) = function
        .parent()
        .filter(|p| p.kind() == "decorated_definition")
    {
        let mut cursor = decorated.walk();
        let is_static = decorated.named_children(&mut cursor).any(|d| {
            d.kind() == "decorator"
                && d.utf8_text(source)
                    .is_ok_and(|t| t.trim_start_matches('@').trim() == "staticmethod")
        });
        if is_static {
            return None;
        }
        member = decorated;
    }
    let class = member
        .parent()
        .filter(|body| body.kind() == "block")?
        .parent()
        .filter(|class| class.kind() == "class_definition")?;
    let first = params.named_child(0)?;
    let receiver = match first.kind() {
        "identifier" => first,
        "typed_parameter" => first.named_child(0).filter(|n| n.kind() == "identifier")?,
        _ => return None,
    };
    Some((
        receiver.utf8_text(source).ok()?,
        class.child_by_field_name("name")?.utf8_text(source).ok()?,
    ))
}

/// Zero-argument `super()`: the receiver of a call through the caller
/// class's bases, which the resolver binds from its heritage. `super(C,
/// self)` starts the lookup after `C`, not after the caller class, so it
/// stays an untyped member call.
fn is_super_call(receiver: Node<'_>, source: &[u8]) -> bool {
    receiver.kind() == "call"
        && receiver
            .child_by_field_name("function")
            .is_some_and(|f| f.kind() == "identifier" && f.utf8_text(source) == Ok("super"))
        && receiver
            .child_by_field_name("arguments")
            .is_some_and(|args| args.named_child_count() == 0)
}
