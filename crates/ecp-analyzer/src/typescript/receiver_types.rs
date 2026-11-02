//! Local-scope receiver-type binding for TypeScript.
//!
//! Mirrors `crates/ecp-analyzer/src/python/receiver_types.rs`.
//!
//! Collects typed bindings from:
//! (a) Typed parameters: `function f(x: MyType)` — `required_parameter` with a
//!     `type_annotation` whose inner node is a `type_identifier`.
//! (b) Typed variable declarations: `const x: MyType = …` — `variable_declarator`
//!     with a `type_annotation (type_identifier)`.
//!
//! Scope rules follow the Python reference: smallest containing function scope wins,
//! `this` inside a method resolves to the enclosing class (handled at call-site in
//! `extract_ts_calls`).
//!
//! Generic / union / intersection types (e.g. `Array<T>`, `A | B`) are skipped — the
//! type annotation's inner node won't be a plain `type_identifier`, so we fall back to
//! the bare method name (same as for un-annotated code).

use super::path_literals::{
    build_raw_path_literal, enclosing_symbol_and_owner_pub, strip_ts_string_value,
    strip_ts_template_value,
};
use crate::calls::{
    attach_to_enclosing_span, new_expression_call, node_range, range_width, start_pos,
    CallSiteIndex, Pos, PosRange,
};
use crate::dynamic_imports::{
    binding_mentions, is_field, may_load, rebound, synthetic_imports, DynamicBinding,
    DynamicImports,
};
use crate::framework_helpers::{
    enclosing_class, js_ts_first_arg_is_literal_string, node_span, Span,
};
use ecp_core::analyzer::types::{CallSite, RawNode, RawPathLiteral, RawSqlRef};
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

/// Map of function scopes (by position range) to their `var → type` bindings.
#[derive(Debug, Default)]
pub struct LocalTypes {
    scopes: Vec<(PosRange, HashMap<String, String>)>,
    bindings: Vec<(PosRange, HashSet<String>)>,
}

impl LocalTypes {
    /// Smallest enclosing scope that declares `var`.
    fn lookup(&self, pos: Pos, var: &str) -> Option<&str> {
        let mut best: Option<&str> = None;
        let mut best_width = (u32::MAX, i64::MAX);
        for ((start, end), map) in &self.scopes {
            if *start <= pos && pos <= *end {
                if let Some(t) = map.get(var) {
                    let w = range_width((*start, *end));
                    if w < best_width {
                        best_width = w;
                        best = Some(t.as_str());
                    }
                }
            }
        }
        best
    }

    /// Whether a parameter or local declaration shadows a module function.
    fn binds_local(&self, pos: Pos, name: &str) -> bool {
        self.bindings
            .iter()
            .any(|((start, end), names)| *start <= pos && pos <= *end && names.contains(name))
    }
}

/// Walk every `function_declaration` / `method_definition` / `arrow_function` node
/// and collect typed bindings plus every parameter and local declaration name.
pub fn collect_local_types(root: Node<'_>, source: &[u8]) -> LocalTypes {
    let mut scopes: Vec<(PosRange, HashMap<String, String>)> = Vec::new();
    let mut bindings: Vec<(PosRange, HashSet<String>)> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        let is_fn = matches!(
            n.kind(),
            "function_declaration"
                | "function"
                | "method_definition"
                | "arrow_function"
                | "function_signature"
        );
        if is_fn {
            let fn_span = node_range(n);
            let mut map: HashMap<String, String> = HashMap::new();
            let mut names: HashSet<String> = HashSet::new();

            if let Some(params) = n.child_by_field_name("parameters") {
                collect_typed_params(params, source, &mut map, &mut names);
            } else if let Some(param) = n.child_by_field_name("parameter") {
                insert_identifier(param, source, &mut names);
            }

            if let Some(body) = n.child_by_field_name("body") {
                collect_typed_vars(body, source, &mut map, &mut names);
            }

            if !map.is_empty() {
                scopes.push((fn_span, map));
            }
            if !names.is_empty() {
                bindings.push((fn_span, names));
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    LocalTypes { scopes, bindings }
}

/// Collect typed parameters from a `formal_parameters` node.
/// Shape: `(required_parameter pattern: (identifier) type: (type_annotation (type_identifier)))`.
fn collect_typed_params(
    params: Node<'_>,
    source: &[u8],
    out: &mut HashMap<String, String>,
    bindings: &mut HashSet<String>,
) {
    let mut c = params.walk();
    for p in params.children(&mut c) {
        // required_parameter and optional_parameter both carry `pattern` + `type`.
        if !matches!(p.kind(), "required_parameter" | "optional_parameter") {
            continue;
        }
        let Some(pat) = p.child_by_field_name("pattern") else {
            continue;
        };
        if pat.kind() != "identifier" {
            continue;
        }
        insert_identifier(pat, source, bindings);
        let Some(type_ann) = p.child_by_field_name("type") else {
            continue;
        };
        if let Some((name, ty)) = simple_name_and_type(pat, type_ann, source) {
            out.insert(name, ty);
        }
    }
}

/// Walk a function body collecting typed `variable_declarator` nodes.
/// Shape: `(variable_declarator name: (identifier) type: (type_annotation (type_identifier)))`.
/// Does NOT descend into nested function declarations.
fn collect_typed_vars(
    body: Node<'_>,
    source: &[u8],
    out: &mut HashMap<String, String>,
    bindings: &mut HashSet<String>,
) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        // Don't descend into nested functions — they get their own scope.
        if matches!(
            n.kind(),
            "function_declaration" | "function" | "method_definition" | "arrow_function"
        ) {
            if n.kind() == "function_declaration" {
                if let Some(name) = n.child_by_field_name("name") {
                    insert_identifier(name, source, bindings);
                }
            }
            continue;
        }
        if n.kind() == "variable_declarator" {
            if let Some(name_node) = n.child_by_field_name("name") {
                insert_identifier(name_node, source, bindings);
                if let Some(type_ann) = n.child_by_field_name("type") {
                    if let Some((name, ty)) = simple_name_and_type(name_node, type_ann, source) {
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

fn insert_identifier(node: Node<'_>, source: &[u8], out: &mut HashSet<String>) {
    if node.kind() == "identifier" {
        if let Ok(name) = node.utf8_text(source) {
            out.insert(name.to_string());
        }
    }
}

/// Extract `(name, type)` only when the type annotation contains a single
/// `type_identifier` (no generics, no unions).
fn simple_name_and_type(
    name_node: Node<'_>,
    type_ann: Node<'_>,
    source: &[u8],
) -> Option<(String, String)> {
    if name_node.kind() != "identifier" {
        return None;
    }
    // type_annotation wraps the actual type; unwrap one level.
    let inner = if type_ann.kind() == "type_annotation" {
        type_ann.named_child(0)?
    } else {
        type_ann
    };
    if inner.kind() != "type_identifier" {
        return None;
    }
    let name = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;
    let ty = std::str::from_utf8(&source[inner.start_byte()..inner.end_byte()]).ok()?;
    if !ty.chars().all(|c| c.is_alphanumeric() || c == '_') || ty.is_empty() {
        return None;
    }
    Some((name.to_string(), ty.to_string()))
}

/// Kinds whose `body` field is a `statement_block` that opens a function scope.
const FUNCTION_SCOPES: &[&str] = &[
    "function_declaration",
    "function",
    "function_expression",
    "generator_function",
    "generator_function_declaration",
    "method_definition",
    "arrow_function",
];

/// Kinds under which a `type_identifier` declares a value name.
const TYPE_DECLARATIONS: &[&str] = &[
    "class_declaration",
    "abstract_class_declaration",
    "class",
    "enum_declaration",
    "interface_declaration",
    "type_alias_declaration",
];

/// A `const` declarator that holds a dynamically loaded module.
struct DynamicCandidate<'t> {
    scope: Node<'t>,
    pattern: (usize, usize),
    names: Vec<(String, Option<String>)>,
    specifier: String,
    call: Node<'t>,
    is_require: bool,
    range: PosRange,
}

/// The string the first argument of `call` spells, when it is a literal
/// ([`js_ts_first_arg_is_literal_string`]) without escapes that names
/// something. A literal this refuses still loads a module at run time, so
/// the `import()` and `require()` blind-spot gates ask this, not the literal
/// test alone.
pub(crate) fn literal_specifier(call: Node<'_>, source: &[u8]) -> Option<String> {
    if !js_ts_first_arg_is_literal_string(&call) {
        return None;
    }
    let text = call
        .child_by_field_name("arguments")?
        .named_child(0)?
        .utf8_text(source)
        .ok()?;
    let inner = text.get(1..text.len().checked_sub(1)?)?;
    (!inner.trim().is_empty() && !inner.contains('\\')).then(|| inner.to_string())
}

/// Whether `node` only reads the value of its name.
fn is_read_position(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "member_expression" | "subscript_expression" => is_field(parent, "object", node),
        "call_expression" => is_field(parent, "function", node),
        "new_expression" => is_field(parent, "constructor", node),
        "pair" | "variable_declarator" => is_field(parent, "value", node),
        "assignment_expression" | "augmented_assignment_expression" | "for_in_statement" => {
            is_field(parent, "right", node)
        }
        "arrow_function" => is_field(parent, "body", node),
        "arguments"
        | "array"
        | "await_expression"
        | "binary_expression"
        | "expression_statement"
        | "non_null_expression"
        | "as_expression"
        | "parenthesized_expression"
        | "return_statement"
        | "satisfies_expression"
        | "sequence_expression"
        | "spread_element"
        | "template_substitution"
        | "ternary_expression"
        | "throw_statement"
        | "type_assertion"
        | "unary_expression"
        | "yield_expression" => true,
        _ => false,
    }
}

/// Whether the leaf `node` binds or writes its name. Any mention that is not
/// a plain read counts, so a parameter, a nested declaration, an assignment
/// target and a destructuring all do.
fn binds_name(node: Node<'_>) -> bool {
    let declares = match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => true,
        "type_identifier" => node
            .parent()
            .is_some_and(|p| TYPE_DECLARATIONS.contains(&p.kind())),
        _ => false,
    };
    declares && !is_read_position(node)
}

/// The module load a `const` declarator's `value` is: `await import('<lit>')`,
/// or outside the program `require('<lit>')` with one argument. Gives the
/// call, whether it is a `require`, and the specifier.
fn dynamic_load<'t>(
    value: Node<'t>,
    is_program: bool,
    source: &[u8],
) -> Option<(Node<'t>, bool, String)> {
    let awaited = value.kind() == "await_expression";
    let call = if awaited {
        value.named_child(0)?
    } else {
        value
    };
    if call.kind() != "call_expression" {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    let is_require = match (function.kind(), awaited) {
        ("import", true) => false,
        ("identifier", false)
            if !is_program
                && function.utf8_text(source) == Ok("require")
                && call
                    .child_by_field_name("arguments")
                    .is_some_and(|args| args.named_child_count() == 1) =>
        {
            true
        }
        _ => return None,
    };
    Some((call, is_require, literal_specifier(call, source)?))
}

/// The names a declarator `pattern` binds, each with the export it holds:
/// `None` for the namespace name of `const m`, the export for `{ run }` and
/// `{ run: execute }`. `default` and every other property shape bind nothing.
fn destructured_names(pattern: Node<'_>, source: &[u8]) -> Vec<(String, Option<String>)> {
    let text = |node: Node<'_>| node.utf8_text(source).ok().map(str::to_string);
    match pattern.kind() {
        "identifier" => text(pattern).map(|name| (name, None)).into_iter().collect(),
        "object_pattern" => {
            let mut props = pattern.walk();
            let names: Vec<_> = pattern
                .named_children(&mut props)
                .filter_map(|prop| {
                    let (name, member) = match prop.kind() {
                        "shorthand_property_identifier_pattern" => {
                            text(prop).map(|name| (name.clone(), name))
                        }
                        "pair_pattern" => {
                            let key = prop
                                .child_by_field_name("key")
                                .filter(|key| key.kind() == "property_identifier")
                                .and_then(text);
                            let local = prop
                                .child_by_field_name("value")
                                .filter(|local| local.kind() == "identifier")
                                .and_then(text);
                            local.zip(key)
                        }
                        _ => None,
                    }?;
                    // `default` is an export the namespace tier does not name.
                    (member != "default").then_some((name, Some(member)))
                })
                .collect();
            names
        }
        _ => Vec::new(),
    }
}

/// The `const` declarators among the direct statements of `body` that load a
/// module dynamically: `const m = await import('<lit>')`, and in a function
/// `const m = require('<lit>')`. A program-level `require` is the static
/// query's.
fn collect_scope_candidates<'t>(
    scope: Node<'t>,
    body: Node<'t>,
    source: &[u8],
    out: &mut Vec<DynamicCandidate<'t>>,
) {
    let is_program = scope.kind() == "program";
    let scope_end = node_range(scope).1;
    let mut statements = body.walk();
    for declaration in body.named_children(&mut statements) {
        if declaration.kind() != "lexical_declaration"
            || declaration.child(0).map(|keyword| keyword.kind()) != Some("const")
        {
            continue;
        }
        let range = (node_range(declaration).1, scope_end);
        let mut declarators = declaration.walk();
        for declarator in declaration.named_children(&mut declarators) {
            if declarator.kind() != "variable_declarator"
                || declarator.child_by_field_name("type").is_some()
            {
                continue;
            }
            let (Some(pattern), Some(value)) = (
                declarator.child_by_field_name("name"),
                declarator.child_by_field_name("value"),
            ) else {
                continue;
            };
            let Some((call, is_require, specifier)) = dynamic_load(value, is_program, source)
            else {
                continue;
            };
            out.push(DynamicCandidate {
                scope,
                pattern: (pattern.start_byte(), pattern.end_byte()),
                names: destructured_names(pattern, source),
                specifier,
                call,
                is_require,
                range,
            });
        }
    }
}

/// Whether `require` is the CommonJS loader in the file under `root`: no
/// declaration or write of `require` exists there.
pub(crate) fn require_loads(root: Node<'_>, source: &[u8]) -> bool {
    let require = binding_mentions(root, &["require"], source, binds_name);
    !rebound(require.get("require"), (0, 0))
}

/// Collect the synthetic imports of every literal `import('<lit>')` and of
/// each function-scoped `const m = require('<lit>')`, and bind the names that
/// hold a loaded module ([`DynamicImports`]).
///
/// A name binds only when its `const` is a direct statement of a function or
/// program body and the name is declared or written nowhere else in that
/// scope's subtree. A use before the declaration's end never binds. Every
/// other shape keeps no binding. `require` loads only when no declaration of
/// `require` exists in the file.
pub fn collect_dynamic_imports(root: Node<'_>, source: &[u8]) -> DynamicImports {
    // The walk below costs a pass over the tree, and every load spells its loader.
    if !may_load(source, &["import", "require"]) {
        return DynamicImports::default();
    }
    let mut loads: Vec<(Node<'_>, String)> = Vec::new();
    let mut candidates: Vec<DynamicCandidate<'_>> = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "call_expression"
            && n.child_by_field_name("function")
                .is_some_and(|function| function.kind() == "import")
        {
            loads.extend(literal_specifier(n, source).map(|spec| (n, spec)));
        }
        let body = match n.kind() {
            "program" => Some(n),
            kind if FUNCTION_SCOPES.contains(&kind) => n
                .child_by_field_name("body")
                .filter(|body| body.kind() == "statement_block"),
            _ => None,
        };
        if let Some(body) = body {
            collect_scope_candidates(n, body, source, &mut candidates);
        }
        let mut c = n.walk();
        stack.extend(n.children(&mut c));
    }
    if loads.is_empty() && candidates.is_empty() {
        return DynamicImports::default();
    }

    if candidates.iter().any(|c| c.is_require) {
        let require_loads = require_loads(root, source);
        candidates.retain(|c| !c.is_require || require_loads);
        loads.extend(
            candidates
                .iter()
                .filter(|c| c.is_require)
                .map(|c| (c.call, c.specifier.clone())),
        );
    }
    loads.sort_by_key(|(call, _)| call.start_byte());
    let (imports, aliases) = synthetic_imports(loads.iter().map(|(_, spec)| spec.as_str()));

    // A scope's candidates are adjacent: the walk collects them in one call.
    let mut bindings = Vec::new();
    for group in candidates.chunk_by(|a, b| a.scope.id() == b.scope.id()) {
        let names: Vec<&str> = group
            .iter()
            .flat_map(|c| c.names.iter().map(|(name, _)| name.as_str()))
            .collect();
        let mentions = binding_mentions(group[0].scope, &names, source, binds_name);
        for candidate in group {
            for (name, member) in &candidate.names {
                if !rebound(mentions.get(name.as_str()), candidate.pattern) {
                    bindings.push(DynamicBinding {
                        name: name.clone(),
                        range: candidate.range,
                        alias: aliases[candidate.specifier.as_str()].clone(),
                        member: member.clone(),
                    });
                }
            }
        }
    }
    DynamicImports {
        imports,
        bindings,
        loads: loads.iter().map(|(call, _)| node_range(*call)).collect(),
    }
}

/// Walk the TypeScript AST and attach callees to enclosing functions/methods,
/// with receiver-type binding:
///
/// - `this.method()` → looks up the innermost enclosing class → emits `ClassName.method`
/// - `obj.method()` where `obj` is a typed param/var → emits `Type.method`
/// - `m.method()` / `method()` through a name bound by a literal dynamic load
///   ([`collect_dynamic_imports`]) → emits `@import<N>.member`
/// - anything else falls back to the bare method name (or full expression as before)
///
/// Also collects SQL-shaped string/template literals into the returned `Vec<RawSqlRef>`.
pub fn extract_ts_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    locals: &LocalTypes,
    dynamic: &DynamicImports,
    call_sites: &mut CallSiteIndex,
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let mut constructions: Vec<(Span, String)> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call_expression" => {
                if let Some(callee) = ts_callee_name(n, source, locals, dynamic, nodes) {
                    if let Some(site) = attach_to_enclosing_span(node_span(&n), callee, nodes) {
                        call_sites.insert(n.id(), site);
                    }
                }
            }
            "new_expression" => {
                if let Some(callee) = new_expression_call(n, source) {
                    constructions.push((node_span(&n), callee));
                }
            }
            "string" | "template_string" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same string node, separate filter.
                let raw_bytes = &source[n.start_byte()..n.end_byte()];
                if let Ok(raw) = std::str::from_utf8(raw_bytes) {
                    let value_opt = if n.kind() == "string" {
                        strip_ts_string_value(raw)
                    } else {
                        // template_string: skip if it has substitutions.
                        let has_substitution = (0..n.child_count()).any(|i| {
                            n.child(i)
                                .is_some_and(|ch| ch.kind() == "template_substitution")
                        });
                        if has_substitution {
                            None
                        } else {
                            strip_ts_template_value(raw)
                        }
                    };
                    if let Some(value) = value_opt {
                        if let Some(mut sql_ref) =
                            crate::sql_literal::try_sql_ref(value, n, (None, None))
                        {
                            (sql_ref.enclosing_symbol, sql_ref.enclosing_owner) =
                                enclosing_symbol_and_owner_pub(n, source);
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
    // After every call: the call-meta detectors index `calls` in
    // `call_expression` order, which a `new` site must not shift.
    for (span, callee) in constructions {
        attach_to_enclosing_span(span, callee, nodes);
    }
    (path_literals, sql_refs)
}

fn ts_callee_name(
    call: Node<'_>,
    source: &[u8],
    locals: &LocalTypes,
    dynamic: &DynamicImports,
    nodes: &[RawNode],
) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    match function.kind() {
        "identifier" => dynamic.bare_callee(call, function, source),
        "member_expression" => {
            let obj = function.child_by_field_name("object")?;
            let prop = function.child_by_field_name("property")?;
            let prop_name = prop.utf8_text(source).ok()?;
            let pos = start_pos(call);

            match obj.kind() {
                "this" => {
                    // `this.method()` — look up enclosing class.
                    let call_span = node_span(&call);
                    if let Some((class_name, _)) = enclosing_class(nodes, call_span) {
                        return Some(format!("{class_name}.{prop_name}"));
                    }
                    Some(CallSite::from_receiver("this", prop_name))
                }
                "identifier" => {
                    let obj_name = obj.utf8_text(source).ok()?;
                    if let Some(callee) = dynamic.member_callee(pos, obj_name, prop_name) {
                        return Some(callee);
                    }
                    if let Some(ty) = locals.lookup(pos, obj_name) {
                        return Some(format!("{ty}.{prop_name}"));
                    }
                    // Unknown type — emit qualified name so the resolver can try.
                    Some(format!("{obj_name}.{prop_name}"))
                }
                // `Foo.create().bar()` / `new Foo().bar()`: the resolver
                // types the receiver by the callee's return type, else the
                // site binds the prop name as before.
                "call_expression" => {
                    let local_callee = obj
                        .child_by_field_name("function")
                        .filter(|callee| callee.kind() == "identifier")
                        .and_then(|callee| callee.utf8_text(source).ok())
                        .is_some_and(|name| locals.binds_local(pos, name));
                    Some(
                        match CallSite::nested_chain(|| {
                            if local_callee {
                                None
                            } else {
                                ts_callee_name(obj, source, locals, dynamic, nodes)
                            }
                        }) {
                            Some(inner) => {
                                let fallback = CallSite::untyped_member(prop_name);
                                CallSite::chained_member(&inner, &fallback)
                            }
                            None => CallSite::untyped_member(prop_name),
                        },
                    )
                }
                "new_expression" => Some(match new_expression_call(obj, source) {
                    Some(inner) => {
                        let fallback = CallSite::untyped_member(prop_name);
                        CallSite::chained_member(&inner, &fallback)
                    }
                    None => CallSite::untyped_member(prop_name),
                }),
                _ => {
                    // Chained member expression or other complex form: fall back to prop name.
                    Some(CallSite::from_receiver("member", prop_name))
                }
            }
        }
        _ => function.utf8_text(source).ok().map(str::to_string),
    }
}

/// Tests live here so they run as part of `cargo test -p ecp-analyzer`.
#[cfg(test)]
mod tests {
    use crate::typescript::TypeScriptProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    /// Helper: parse source and collect all `calls` from all nodes.
    fn all_calls(source: &str) -> Vec<String> {
        let provider = TypeScriptProvider::new().unwrap();
        let graph = provider
            .parse_file(Path::new("test.ts"), source.as_bytes())
            .unwrap();
        graph.nodes.into_iter().flat_map(|n| n.calls).collect()
    }

    #[test]
    fn typescript_ctor_this_method_resolved_to_class() {
        let src = r#"
class OrderService {
    process(): void {
        this.validate();
        this.save();
    }
    validate(): void {}
    save(): void {}
}
"#;
        let calls = all_calls(src);
        assert!(
            calls.contains(&"OrderService.validate".to_string()),
            "expected OrderService.validate in calls, got: {calls:?}"
        );
        assert!(
            calls.contains(&"OrderService.save".to_string()),
            "expected OrderService.save in calls, got: {calls:?}"
        );
    }

    #[test]
    fn typescript_ctor_typed_param_resolved() {
        let src = r#"
function process(repo: UserRepository): void {
    repo.findAll();
    repo.save();
}
"#;
        let calls = all_calls(src);
        assert!(
            calls.contains(&"UserRepository.findAll".to_string()),
            "expected UserRepository.findAll in calls, got: {calls:?}"
        );
        assert!(
            calls.contains(&"UserRepository.save".to_string()),
            "expected UserRepository.save in calls, got: {calls:?}"
        );
    }

    #[test]
    fn typescript_ctor_typed_variable_resolved() {
        let src = r#"
function run(): void {
    const svc: PaymentService = new PaymentService();
    svc.charge();
}
"#;
        let calls = all_calls(src);
        assert!(
            calls.contains(&"PaymentService.charge".to_string()),
            "expected PaymentService.charge in calls, got: {calls:?}"
        );
    }

    #[test]
    fn typescript_ctor_untyped_param_not_prefixed() {
        // Without a type annotation the receiver falls back to `obj.method` (qualified).
        let src = r#"
function process(obj: any): void {
    obj.doWork();
}
"#;
        let calls = all_calls(src);
        // `any` is a predefined_type, not a type_identifier — so no binding.
        // The fallback should still produce something usable for the resolver.
        assert!(
            !calls.iter().any(|c| c.starts_with("any.")),
            "should not bind predefined type 'any': {calls:?}"
        );
    }
}
