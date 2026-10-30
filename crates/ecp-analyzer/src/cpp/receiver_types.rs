//! Receiver-type binding for C++.
//!
//! Rewrites three call shapes so the resolver's qualifier-scoped lookup
//! (Tier 2.5) can route to the correct class:
//!
//! - `this->method()` / `this.method()` → `Class.method` where `Class` is
//!   the enclosing `class_specifier` / `struct_specifier`.
//! - `Base::method()` → `Base.method` (the shared `calls.rs` already
//!   captures the full qualifier; we preserve and normalize to `.` form).
//! - `obj.method()` / `obj->method()` → `Type.method` when `obj` is a
//!   parameter, a local variable, or a field of the enclosing class
//!   (`sax->m()`, `this->sax->m()`) with a single-identifier declared type.
//!   A local of any type hides a same-name field.
//!
//! Scope: P0 simple identifier types only. Templates, namespace-qualified
//! types, `auto`, and computed receivers (`a.b().m()`) are untyped: the
//! call is recorded as [`CallSite::untyped_member`], so it never binds a
//! free function or an implicit-`this` member of the same name.
//!
//! The enclosing class of an out-of-class definition (`void Foo::bar()`) is
//! its qualifier, so `this` and fields type there too.

use super::path_literals::{
    build_concatenated, build_raw_path_literal, enclosing_symbol_and_owner, strip_quotes,
};
use crate::calls::{
    attach_to_enclosing, construction_call, node_range, range_width, start_pos, CallSiteIndex, Pos,
    PosRange,
};
use crate::indirect_dispatch::FnPtrVars;
use ecp_core::analyzer::types::{CallSite, RawNode, RawPathLiteral, RawSqlRef};
use rustc_hash::{FxHashMap, FxHashSet};
use tree_sitter::Node;

#[derive(Debug, Default)]
pub struct CppBindings {
    /// Locals and parameters per function. An empty type marks a name
    /// declared with a type the binder does not read (`auto x`,
    /// `std::string s`): it still hides a same-name field.
    fn_scopes: Vec<(PosRange, FxHashMap<String, String>)>,
    /// Class bodies, and out-of-class member definitions under their
    /// qualifier (`void Foo::bar() {}` → `Foo`), each by its path of
    /// enclosing namespaces and classes (`a::Foo`).
    class_scopes: Vec<(PosRange, String)>,
    /// Typed data members per class path (`a::Foo`), wherever the class
    /// declares them: two namespaces' `Foo` keep their own fields.
    class_fields: FxHashMap<String, FxHashMap<String, String>>,
}

impl CppBindings {
    /// Every local and parameter binding in its own function, lambda or
    /// `catch` scope, and every data member of a class this file declares
    /// over the class body and the class's out-of-class definitions, for
    /// indirect-call detection: a callee or receiver name then takes the
    /// type its own scope declares. An untyped binding still hides a
    /// same-name one of an enclosing scope, and a local or parameter hides
    /// a member over the same out-of-class definition.
    pub fn scoped_bindings(&self) -> FnPtrVars {
        let mut vars = FnPtrVars::default();
        let mut locals: FxHashSet<(PosRange, &str)> = FxHashSet::default();
        for (scope, map) in &self.fn_scopes {
            for (name, ty) in map {
                locals.insert((*scope, name.as_str()));
                vars.declare(name.clone(), *scope, (!ty.is_empty()).then(|| ty.clone()));
            }
        }
        for (scope, path) in &self.class_scopes {
            let Some(fields) = self.fields_of(path) else {
                continue;
            };
            for (name, ty) in fields {
                if !locals.contains(&(*scope, name.as_str())) {
                    vars.declare(name.clone(), *scope, (!ty.is_empty()).then(|| ty.clone()));
                }
            }
        }
        vars
    }

    /// A class body the tree lost and the lexical recovery found.
    pub fn push_class_scope(&mut self, span: (u32, u32, u32, u32), name: String) {
        self.class_scopes
            .push((((span.0, span.1), (span.2, span.3)), name));
    }

    /// The declared type of the receiver variable `var` at `pos`: a local
    /// or parameter first, then a field of the enclosing class.
    fn receiver_type(&self, pos: Pos, var: &str) -> Option<&str> {
        match self.lookup_local(pos, var) {
            Some(ty) => (!ty.is_empty()).then_some(ty),
            None => self.field_type(pos, var).filter(|ty| !ty.is_empty()),
        }
    }

    fn field_type(&self, pos: Pos, field: &str) -> Option<&str> {
        let path = self.enclosing_class_path(pos)?;
        self.fields_of(path)?.get(field).map(String::as_str)
    }

    fn fields_of(&self, path: &str) -> Option<&FxHashMap<String, String>> {
        self.class_fields.get(path).or_else(|| {
            // A definition that names its class by less of the path
            // (`using namespace a; void Foo::bar() {}`) reads the fields of
            // the one class of that name, if the file declares only one.
            let name = last_segment(path);
            let mut same = self
                .class_fields
                .iter()
                .filter(|(key, _)| last_segment(key) == name);
            let (_, fields) = same.next()?;
            same.next().is_none().then_some(fields)
        })
    }

    fn lookup_local(&self, pos: Pos, var: &str) -> Option<&str> {
        let mut best: Option<&str> = None;
        let mut best_width = (u32::MAX, i64::MAX);
        for ((start, end), map) in &self.fn_scopes {
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

    fn enclosing_class(&self, pos: Pos) -> Option<&str> {
        self.enclosing_class_path(pos).map(last_segment)
    }

    fn enclosing_class_path(&self, pos: Pos) -> Option<&str> {
        let mut best: Option<&str> = None;
        let mut best_width = (u32::MAX, i64::MAX);
        for ((start, end), name) in &self.class_scopes {
            if *start <= pos && pos <= *end {
                let w = range_width((*start, *end));
                if w < best_width {
                    best_width = w;
                    best = Some(name.as_str());
                }
            }
        }
        best
    }
}

pub fn collect_bindings(root: Node<'_>, source: &[u8]) -> CppBindings {
    let mut fn_scopes: Vec<(PosRange, FxHashMap<String, String>)> = Vec::new();
    let mut class_scopes: Vec<(PosRange, String)> = Vec::new();
    let mut class_fields: FxHashMap<String, FxHashMap<String, String>> = FxHashMap::default();
    // The namespace and class paths the walk has entered, by index; each
    // stack entry carries the index of the path around it.
    let mut paths: Vec<String> = vec![String::new()];

    let mut stack: Vec<(Node<'_>, usize)> = vec![(root, 0)];
    while let Some((n, outer)) = stack.pop() {
        let mut inner = outer;
        match n.kind() {
            "namespace_definition" => {
                if let Some(name) = n
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(source).ok())
                {
                    inner = paths.len();
                    paths.push(join_path(&paths[outer], name));
                }
            }
            "class_specifier" | "struct_specifier" | "union_specifier" => {
                if let Some(name) = n
                    .child_by_field_name("name")
                    .and_then(|name_node| scope_type_name(name_node, source))
                {
                    let path = join_path(&paths[outer], name);
                    if let Some(body) = n.child_by_field_name("body") {
                        let fields = class_fields.entry(path.clone()).or_default();
                        collect_typed_fields(body, source, fields);
                    }
                    class_scopes.push((node_range(n), path.clone()));
                    inner = paths.len();
                    paths.push(path);
                }
            }
            "function_definition" => {
                let span = node_range(n);
                if let Some(qualifier) = n
                    .child_by_field_name("declarator")
                    .and_then(find_function_declarator)
                    .and_then(|d| d.child_by_field_name("declarator"))
                    .and_then(|d| qualifier_path(d, source))
                {
                    class_scopes.push((span, join_path(&paths[outer], &qualifier)));
                }
                let mut map: FxHashMap<String, String> = FxHashMap::default();
                collect_typed_cpp_params(n, source, &mut map);
                if let Some(body) = n.child_by_field_name("body") {
                    collect_typed_cpp_decls(body, source, &mut map, &mut fn_scopes);
                }
                if !map.is_empty() {
                    fn_scopes.push((span, map));
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push((child, inner));
        }
    }

    CppBindings {
        fn_scopes,
        class_scopes,
        class_fields,
    }
}

/// The type a class name or a qualifier scope names: `Foo` for
/// `type_identifier` / `namespace_identifier` `Foo`, `basic_json` for the
/// `template_type` `basic_json<T>`. `None` for any other node.
pub(crate) fn scope_type_name<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    match node.kind() {
        "type_identifier" | "namespace_identifier" => node.utf8_text(source).ok(),
        "template_type" => node.child_by_field_name("name")?.utf8_text(source).ok(),
        _ => None,
    }
}

/// The qualifier of the qualified declarator `decl` as a path: `ns::Foo`
/// for `ns::Foo::bar`, `basic_json` for `basic_json<T>::size`. `None` when
/// `decl` is not a `qualified_identifier`, or its innermost scope names no
/// type or namespace.
fn qualifier_path(mut decl: Node<'_>, source: &[u8]) -> Option<String> {
    let mut path = String::new();
    let mut named = false;
    while decl.kind() == "qualified_identifier" {
        let scope = decl
            .child_by_field_name("scope")
            .and_then(|scope| scope_type_name(scope, source));
        named = scope.is_some();
        if let Some(scope) = scope {
            path = join_path(&path, scope);
        }
        decl = decl.child_by_field_name("name")?;
    }
    named.then_some(path)
}

fn join_path(outer: &str, name: &str) -> String {
    if outer.is_empty() {
        name.to_string()
    } else {
        format!("{outer}::{name}")
    }
}

/// The class name a path ends in: `Foo` for `a::Foo`.
fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// The inner declarator of a pointer / reference / init / array
/// declarator. `reference_declarator` has no `declarator` field.
fn inner_declarator(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("declarator")
        .or_else(|| node.named_child(0))
}

/// Record `name` with its declared type when the binder reads that type
/// (a single `type_identifier`), and as declared-untyped otherwise. A typed
/// declaration wins over an untyped one of the same name.
fn bind_local(
    ty_node: Option<Node<'_>>,
    name: String,
    source: &[u8],
    out: &mut FxHashMap<String, String>,
) {
    match ty_node
        .filter(|t| t.kind() == "type_identifier")
        .and_then(|t| t.utf8_text(source).ok())
    {
        Some(ty) => {
            out.insert(name, ty.to_string());
        }
        None => {
            out.entry(name).or_default();
        }
    }
}

/// Bind one `parameter_declaration` / `optional_parameter_declaration`.
fn bind_param(p: Node<'_>, source: &[u8], out: &mut FxHashMap<String, String>) {
    if !matches!(
        p.kind(),
        "parameter_declaration" | "optional_parameter_declaration"
    ) {
        return;
    }
    let Some(declarator) = p.child_by_field_name("declarator") else {
        return;
    };
    if let Some(name) = callable_param_identifier(declarator, source) {
        // A callable parameter's declared type is its result type. Keeping
        // that as the callable's type would incorrectly type `make().m()`.
        out.entry(name).or_default();
        return;
    }
    if let Some(name) = unwrap_declarator_identifier(declarator, source) {
        bind_local(p.child_by_field_name("type"), name, source, out);
    }
}

/// The name in a parameter declarator that is itself callable, such as
/// `Beta* (*make)()`. This only runs for parameters, so ordinary function
/// declarations never become local values.
fn callable_param_identifier(node: Node<'_>, source: &[u8]) -> Option<String> {
    let node = find_function_declarator(node)?;
    let paren = node.child_by_field_name("declarator")?;
    if paren.kind() != "parenthesized_declarator" {
        return None;
    }
    let pointer = paren.named_child(0)?;
    if !matches!(
        pointer.kind(),
        "pointer_declarator" | "reference_declarator"
    ) {
        return None;
    }
    unwrap_declarator_identifier(pointer, source)
}

/// Scan the function's declarator for parameter declarations.
fn collect_typed_cpp_params(fn_def: Node<'_>, source: &[u8], out: &mut FxHashMap<String, String>) {
    let Some(declarator) = fn_def.child_by_field_name("declarator") else {
        return;
    };
    let Some(fn_decl) = find_function_declarator(declarator) else {
        return;
    };
    let Some(params) = fn_decl.child_by_field_name("parameters") else {
        return;
    };
    let mut c = params.walk();
    for p in params.children(&mut c) {
        bind_param(p, source, out);
    }
}

/// Walk the function body for local declarations: `declaration` nodes and
/// the range-`for` variable. A lambda's or a `catch` handler's parameters
/// and locals go to a scope of their own in `scopes`, so a name they share
/// with the enclosing function keeps its outer type outside them.
fn collect_typed_cpp_decls(
    body: Node<'_>,
    source: &[u8],
    out: &mut FxHashMap<String, String>,
    scopes: &mut Vec<(PosRange, FxHashMap<String, String>)>,
) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "function_definition" => continue,
            "lambda_expression" | "catch_clause" => {
                let mut own: FxHashMap<String, String> = FxHashMap::default();
                let mut c = n.walk();
                for child in n.children(&mut c) {
                    collect_typed_cpp_decls(child, source, &mut own, scopes);
                }
                if !own.is_empty() {
                    scopes.push((node_range(n), own));
                }
                continue;
            }
            "declaration" => {
                // `T a, *b = x;` has one `declarator` per name; the
                // initializer of a condition declaration (`if (T* p = q)`)
                // is a sibling `value`, never a declared name.
                let mut c = n.walk();
                for decl in n.children_by_field_name("declarator", &mut c) {
                    if let Some(name) = unwrap_declarator_identifier(decl, source) {
                        bind_local(n.child_by_field_name("type"), name, source, out);
                    }
                }
            }
            "for_range_loop" => {
                if let Some(name) = n
                    .child_by_field_name("declarator")
                    .and_then(|d| unwrap_declarator_identifier(d, source))
                {
                    bind_local(n.child_by_field_name("type"), name, source, out);
                }
            }
            "parameter_declaration" | "optional_parameter_declaration" => {
                bind_param(n, source, out)
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

/// Typed data members of a class body: `T* sax;`, `T a, *b;`, `static T s;`,
/// including members inside a preprocessor conditional. A member function
/// declaration has a `function_declarator` and binds nothing.
fn collect_typed_fields(body: Node<'_>, source: &[u8], out: &mut FxHashMap<String, String>) {
    let mut c = body.walk();
    for member in body.children(&mut c) {
        match member.kind() {
            "field_declaration" => {
                let ty = member
                    .child_by_field_name("type")
                    .filter(|t| t.kind() == "type_identifier")
                    .and_then(|t| t.utf8_text(source).ok());
                let mut dc = member.walk();
                for decl in member.children_by_field_name("declarator", &mut dc) {
                    if let Some(name) = unwrap_field_identifier(decl, source) {
                        out.entry(name.to_string())
                            .or_insert_with(|| ty.map_or_else(String::new, str::to_string));
                    }
                }
            }
            "preproc_if" | "preproc_ifdef" | "preproc_else" | "preproc_elif" => {
                collect_typed_fields(member, source, out)
            }
            _ => {}
        }
    }
}

fn unwrap_field_identifier<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    match node.kind() {
        "field_identifier" => node.utf8_text(source).ok(),
        "pointer_declarator" | "reference_declarator" | "array_declarator" => {
            unwrap_field_identifier(inner_declarator(node)?, source)
        }
        _ => None,
    }
}

/// Recursively unwrap declarator nodes to extract the bound identifier.
/// Handles `identifier`, `init_declarator`, `pointer_declarator`,
/// `reference_declarator`. Returns None for unrelated kinds.
fn unwrap_declarator_identifier(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
            .ok()
            .map(str::to_string),
        "init_declarator" | "pointer_declarator" | "reference_declarator" => {
            unwrap_declarator_identifier(inner_declarator(node)?, source)
        }
        _ => None,
    }
}

fn find_function_declarator<'a>(mut node: Node<'a>) -> Option<Node<'a>> {
    loop {
        match node.kind() {
            "function_declarator" => return Some(node),
            "pointer_declarator" | "reference_declarator" => {
                node = inner_declarator(node)?;
            }
            _ => return None,
        }
    }
}

pub fn extract_cpp_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    bindings: &CppBindings,
    call_sites: &mut CallSiteIndex,
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let mut constructions: Vec<((u32, u32), String)> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call_expression" => {
                if let Some(callee) = cpp_callee_name(n, source, bindings) {
                    if let Some(site) = attach_to_enclosing(start_pos(n), callee, nodes) {
                        call_sites.insert(n.id(), site);
                    }
                }
            }
            "new_expression" => {
                if let Some(callee) = cpp_new_expression_call(n, source) {
                    constructions.push((start_pos(n), callee));
                }
            }
            "string_literal" | "raw_string_literal" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same string node, separate filter.
                let is_raw = n.kind() == "raw_string_literal";
                let raw_bytes = &source[n.start_byte()..n.end_byte()];
                if let Ok(raw) = std::str::from_utf8(raw_bytes) {
                    if let Some(value) = strip_quotes(raw, is_raw) {
                        if let Some(sql_ref) = crate::sql_literal::try_sql_ref(
                            value,
                            n,
                            enclosing_symbol_and_owner(n, source),
                        ) {
                            sql_refs.push(sql_ref);
                        }
                    }
                }
            }
            "concatenated_string" => {
                if let Some(rpl) = build_concatenated(n, source) {
                    path_literals.push(rpl);
                }
                // Skip descent — child `string_literal` pieces would
                // double-emit. C++ has no calls inside string literals.
                continue;
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
    for (pos, callee) in constructions {
        attach_to_enclosing(pos, callee, nodes);
    }
    (path_literals, sql_refs)
}

/// The construction entry of `new T(..)`, `new ns::T(..)` or
/// `new Box<int>(..)`; `None` for a built-in type (`new int[4]`).
fn cpp_new_expression_call(new_expr: Node<'_>, source: &[u8]) -> Option<String> {
    let ty = new_expr.child_by_field_name("type")?;
    match ty.kind() {
        "type_identifier" | "template_type" | "qualified_identifier" => {
            construction_call(ty.utf8_text(source).ok()?)
        }
        _ => None,
    }
}

/// The declared type of a member call's receiver: a typed local, parameter
/// or field (`sax`), or a field through `this` (`this->sax`).
fn member_receiver_type<'b>(
    receiver: Node<'_>,
    source: &[u8],
    bindings: &'b CppBindings,
    pos: Pos,
) -> Option<&'b str> {
    match receiver.kind() {
        "identifier" => bindings.receiver_type(pos, receiver.utf8_text(source).ok()?),
        "field_expression" => {
            let this = receiver.child_by_field_name("argument")?;
            let field = receiver.child_by_field_name("field")?;
            if this.kind() != "this" {
                return None;
            }
            bindings
                .field_type(pos, field.utf8_text(source).ok()?)
                .filter(|ty| !ty.is_empty())
        }
        _ => None,
    }
}

/// Whether `call` invokes a local, parameter or field. Its result may have a
/// type, but the binder does not infer callable return types, so a same-named
/// free function must not type an enclosing member call.
fn calls_value(call: Node<'_>, source: &[u8], bindings: &CppBindings) -> bool {
    call.child_by_field_name("function")
        .filter(|function| function.kind() == "identifier")
        .and_then(|function| function.utf8_text(source).ok())
        .is_some_and(|name| {
            let pos = start_pos(call);
            bindings.lookup_local(pos, name).is_some() || bindings.field_type(pos, name).is_some()
        })
}

fn cpp_callee_name(call: Node<'_>, source: &[u8], bindings: &CppBindings) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    let pos = start_pos(call);

    match function.kind() {
        "identifier" => std::str::from_utf8(&source[function.start_byte()..function.end_byte()])
            .ok()
            .map(str::to_string),
        "field_expression" => {
            let field = function.child_by_field_name("field")?;
            let method = std::str::from_utf8(&source[field.start_byte()..field.end_byte()]).ok()?;
            let arg = function.child_by_field_name("argument")?;
            // `(*p).method()` / `(&x).method()` / `(*this).method()` type as
            // the operand.
            let receiver = match arg.kind() {
                "pointer_expression" => arg.named_child(0)?,
                _ => arg,
            };
            if receiver.kind() == "this" {
                // `this->method()`; bare when no class encloses the call.
                return Some(match bindings.enclosing_class(pos) {
                    Some(cls) => format!("{cls}.{method}"),
                    None => method.to_string(),
                });
            }
            Some(
                match member_receiver_type(receiver, source, bindings, pos) {
                    Some(ty) => format!("{ty}.{method}"),
                    // `Foo::create().bar()` / `make()->bar()`: the resolver
                    // types a call receiver by the callee's return type.
                    None if receiver.kind() == "call_expression"
                        && !calls_value(receiver, source, bindings) =>
                    {
                        let untyped = CallSite::untyped_member(method);
                        match CallSite::nested_chain(|| cpp_callee_name(receiver, source, bindings))
                        {
                            Some(inner) => CallSite::chained_member(&inner, &untyped),
                            None => untyped,
                        }
                    }
                    None => CallSite::untyped_member(method),
                },
            )
        }
        "qualified_identifier" => {
            // `Base::method`. Pull the namespace_identifier + final name and
            // emit `Base.method` so the resolver's `.`-keyed lookup picks up
            // the qualifier scope.
            let mut c = function.walk();
            let children: Vec<_> = function.children(&mut c).collect();
            let scope_node = children
                .iter()
                .find(|ch| ch.kind() == "namespace_identifier");
            let name_node = function.child_by_field_name("name").or_else(|| {
                children
                    .iter()
                    .rev()
                    .find(|ch| {
                        matches!(
                            ch.kind(),
                            "identifier" | "field_identifier" | "destructor_name"
                        )
                    })
                    .copied()
            })?;
            let name =
                std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;
            if let Some(scope) = scope_node {
                if let Ok(scope_text) =
                    std::str::from_utf8(&source[scope.start_byte()..scope.end_byte()])
                {
                    return Some(format!("{scope_text}.{name}"));
                }
            }
            Some(name.to_string())
        }
        _ => std::str::from_utf8(&source[function.start_byte()..function.end_byte()])
            .ok()
            .map(str::to_string),
    }
}
