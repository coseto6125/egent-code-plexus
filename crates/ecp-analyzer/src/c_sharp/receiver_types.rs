//! Receiver-type binding for C# invocation sites.
//!
//! Handles:
//! - `this.Foo()` → receiver is the enclosing class/struct name
//! - `base.Foo()` → first base type from the enclosing class's `heritage`
//! - `obj.Foo()` where `obj` has a known type from local variable declarations,
//!   typed method parameters, or a field or property of the enclosing types
//!   (`TextWriter _writer;` → `TextWriter.Foo`), and `new T().Foo()`
//! - `Name.Foo()` where `Name` is no local, parameter, field or property in
//!   scope, and `Outer.Inner.Foo()` → [`CallSite::from_receiver`]: the
//!   resolver binds `Name` only as a project type (`StringAssert.AreEqual()`)
//!
//! A receiver whose type stays unknown (`var x`, a field of a generic type,
//! `a.b`, `a[i]`, `x?.Foo()` on such a value) is recorded as
//! [`CallSite::untyped_member`], never as the bare method name: a bare name
//! means `this.Foo()` (codex ruling P-recv Q1).
//!
//! C# AST shape (tree-sitter-c-sharp):
//! - `invocation_expression` with `member_access_expression` as `function` field
//! - `member_access_expression`:
//!   - `this` keyword node → `this_expression` or bare `this`
//!   - `base` keyword node → bare `base`
//!   - `identifier` node → variable receiver
//!   - `name` field holds the method identifier
//! - Local variable: `local_declaration_statement` → `variable_declaration`
//!   - First named child: type (`identifier` / `predefined_type`)
//!   - `variable_declarator` child with `name` field (`identifier`)

use super::path_literals::{
    build_raw_path_literal, enclosing_symbol_and_owner_pub, strip_csharp_string_value,
};
use crate::calls::{
    attach_to_enclosing, construction_call, node_range, range_width, span_holds, span_width,
    start_pos, Pos, PosRange,
};
use ecp_core::analyzer::types::{CallSite, RawNode, RawPathLiteral, RawSqlRef};
use ecp_core::graph::NodeKind;
use std::borrow::Cow;
use std::collections::HashMap;
use tree_sitter::Node;

#[derive(Debug, Default)]
struct LocalTypes {
    scopes: Vec<(PosRange, HashMap<String, String>)>,
    /// Every field and property by name: the range of the type declaration
    /// that declares it, and its type when that is a plain type name (`""`
    /// otherwise). Keyed by name, so a lookup reads only the same-name
    /// members.
    members: HashMap<String, Vec<(PosRange, String)>>,
}

impl LocalTypes {
    /// The declared type of the field or property `name` that the innermost
    /// type declaration enclosing `pos` and declaring `name` holds: `""` for
    /// a type that is no plain type name, `None` for no such member.
    fn member_type(&self, pos: Pos, name: &str) -> Option<&str> {
        self.members
            .get(name)?
            .iter()
            .filter(|((start, end), _)| *start <= pos && pos <= *end)
            .min_by_key(|(range, _)| range_width(*range))
            .map(|(_, ty)| ty.as_str())
    }

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
}

/// Walk every method/constructor/local-function declaration, collecting typed
/// parameters and local variable declarations, and every field and property
/// a type declares.
fn collect_local_types(root: Node<'_>, source: &[u8]) -> LocalTypes {
    let mut scopes: Vec<(PosRange, HashMap<String, String>)> = Vec::new();
    let mut members: HashMap<String, Vec<(PosRange, String)>> = HashMap::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "field_declaration" | "property_declaration" => {
                collect_members(n, source, &mut members);
            }
            "method_declaration" | "constructor_declaration" | "local_function_statement" => {
                let scope = node_range(n);
                let mut map: HashMap<String, String> = HashMap::new();
                if let Some(params) = n.child_by_field_name("parameters") {
                    collect_params(params, source, &mut map);
                }
                if let Some(body) = n.child_by_field_name("body") {
                    collect_local_vars(body, source, &mut map);
                }
                if !map.is_empty() {
                    scopes.push((scope, map));
                }
                // Don't descend into nested method bodies.
                continue;
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    LocalTypes { scopes, members }
}

/// Record the fields or the property `decl` declares, under the range of
/// the type declaration that holds it, each with its type when that is a
/// plain type name: `""` for a generic, nullable, array or keyword type.
fn collect_members(
    decl: Node<'_>,
    source: &[u8],
    out: &mut HashMap<String, Vec<(PosRange, String)>>,
) {
    let Some(owner) = std::iter::successors(decl.parent(), |n| n.parent()).find(|n| {
        matches!(
            n.kind(),
            "class_declaration"
                | "struct_declaration"
                | "record_declaration"
                | "interface_declaration"
        )
    }) else {
        return;
    };
    let scope = node_range(owner);
    let plain_type = |ty: Option<Node<'_>>| {
        ty.filter(|ty| ty.kind() == "identifier")
            .and_then(|ty| ty.utf8_text(source).ok())
            .unwrap_or_default()
            .to_string()
    };
    let mut push = |name: Option<Node<'_>>, ty: &str| {
        if let Some(name) = name
            .filter(|name| name.kind() == "identifier")
            .and_then(|name| name.utf8_text(source).ok())
        {
            out.entry(name.to_string())
                .or_default()
                .push((scope, ty.to_string()));
        }
    };
    if decl.kind() == "property_declaration" {
        push(
            decl.child_by_field_name("name"),
            &plain_type(decl.child_by_field_name("type")),
        );
        return;
    }
    let mut cursor = decl.walk();
    let Some(declaration) = decl
        .named_children(&mut cursor)
        .find(|child| child.kind() == "variable_declaration")
    else {
        return;
    };
    let ty = plain_type(declaration.child_by_field_name("type"));
    let mut cursor = declaration.walk();
    for declarator in declaration.named_children(&mut cursor) {
        if declarator.kind() == "variable_declarator" {
            push(declarator.child_by_field_name("name"), &ty);
        }
    }
}

/// C# formal parameters: `parameter_list` → `parameter` with `name` and `type` fields.
fn collect_params(params: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut c = params.walk();
    for p in params.children(&mut c) {
        if p.kind() != "parameter" {
            continue;
        }
        let Some(nm) = p.child_by_field_name("name") else {
            continue;
        };
        let typed = p
            .child_by_field_name("type")
            .and_then(|ty| simple_id_and_type(nm, ty, source));
        match typed {
            Some((var, ty_s)) => {
                out.insert(var, ty_s);
            }
            None => declare_untyped(nm, source, out),
        }
    }
}

/// Record a declared name whose type is not a plain class name as `""`: it
/// still shadows a type of the same name, so a call on it is an untyped
/// member call instead of resolving through that type.
fn declare_untyped(name_node: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    // `implicit_parameter` is the grammar's alias of the identifier in `x => ..`.
    if !matches!(name_node.kind(), "identifier" | "implicit_parameter") {
        return;
    }
    if let Ok(name) = name_node.utf8_text(source) {
        out.entry(name.to_string()).or_default();
    }
}

/// Walk a block, collecting `local_declaration_statement` → `variable_declaration`,
/// and every other declared name (`var x`, `for` / `using` / `foreach` / `catch`
/// variables, lambda parameters) as untyped.
/// Does NOT descend into nested method/constructor/local-function bodies.
fn collect_local_vars(body: Node<'_>, source: &[u8], out: &mut HashMap<String, String>) {
    let mut stack: Vec<Node<'_>> = vec![body];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "method_declaration" | "constructor_declaration" | "local_function_statement" => {
                continue;
            }
            "variable_declarator" | "parameter" | "catch_declaration" => {
                if let Some(nm) = n.child_by_field_name("name") {
                    declare_untyped(nm, source, out);
                }
            }
            "foreach_statement" => {
                if let Some(left) = n.child_by_field_name("left") {
                    declare_untyped(left, source, out);
                }
            }
            "lambda_expression" => {
                if let Some(param) = n.child_by_field_name("parameters") {
                    declare_untyped(param, source, out);
                }
            }
            "local_declaration_statement" => {
                if let Some(var_decl) = n.named_child(0) {
                    if var_decl.kind() == "variable_declaration" {
                        // First named child is the type; variable_declarator children hold names.
                        let mut ty_node = None;
                        let mut c = var_decl.walk();
                        for child in var_decl.children(&mut c) {
                            if child.is_named() {
                                if ty_node.is_none() && is_type_node(child) {
                                    ty_node = Some(child);
                                } else if child.kind() == "variable_declarator" {
                                    // `var x = new T(..)`: the construction
                                    // names the type `var` leaves out.
                                    let ty = ty_node.or_else(|| constructed_type(child));
                                    if let (Some(ty), Some(nm)) =
                                        (ty, child.child_by_field_name("name"))
                                    {
                                        if let Some((var, ty_s)) =
                                            simple_id_and_type(nm, ty, source)
                                        {
                                            out.insert(var, ty_s);
                                        }
                                    }
                                }
                            }
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
}

/// The type node of the `new T(..)` that initializes `declarator`.
fn constructed_type(declarator: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = declarator.walk();
    let creation = declarator
        .named_children(&mut cursor)
        .find(|child| child.kind() == "object_creation_expression")?;
    creation.child_by_field_name("type")
}

fn is_type_node(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "identifier"
            | "predefined_type"
            | "nullable_type"
            | "generic_name"
            | "array_type"
            | "qualified_name"
    )
}

/// Return `(var_name, type_name)` only when the name is a plain identifier and
/// the type is a plain class name (single `identifier` node).
fn simple_id_and_type(
    name_node: Node<'_>,
    type_node: Node<'_>,
    source: &[u8],
) -> Option<(String, String)> {
    if name_node.kind() != "identifier" {
        return None;
    }
    // Only bind plain class-name types (single `identifier`).
    if type_node.kind() != "identifier" {
        return None;
    }
    let name = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]).ok()?;
    let ty = std::str::from_utf8(&source[type_node.start_byte()..type_node.end_byte()]).ok()?;
    if ty.is_empty() {
        return None;
    }
    Some((name.to_string(), ty.to_string()))
}

/// Whether `call` invokes a local, parameter, field or property. Its result
/// may have a type, but the binder does not infer delegate return types, so a
/// same-named member must not type an enclosing member call.
fn calls_value(call: Node<'_>, source: &[u8], locals: &LocalTypes, nodes: &[RawNode]) -> bool {
    call.child_by_field_name("function")
        .filter(|function| function.kind() == "identifier")
        .and_then(|function| function.utf8_text(source).ok())
        .is_some_and(|name| {
            let pos = start_pos(call);
            locals.lookup(pos, name).is_some() || declares_member(nodes, pos, name)
        })
}

/// `entry` with the arguments of the call `inv` recorded
/// ([`CallSite::with_arguments`]): the C# extension retry reads them to
/// tell overloads apart. Every member site records them; a bare call, and
/// a `this.M()` / `base.M()` with no type to name, record none. A list the
/// parser cannot model records nothing: a syntax error, or a positional
/// argument after a named one.
fn with_call_arguments(entry: String, inv: Node<'_>, source: &[u8]) -> String {
    let Some(list) = inv
        .child_by_field_name("arguments")
        .filter(|list| !list.has_error())
    else {
        return entry;
    };
    let mut positional = 0u32;
    let mut named: Vec<&str> = Vec::new();
    let mut cursor = list.walk();
    for argument in list.named_children(&mut cursor) {
        if argument.is_extra() {
            continue;
        }
        if argument.kind() != "argument" {
            return entry;
        }
        match argument
            .child_by_field_name("name")
            .map(|name| name.utf8_text(source))
        {
            Some(Ok(name)) => named.push(name),
            None if named.is_empty() => positional += 1,
            _ => return entry,
        }
    }
    CallSite::with_arguments(&entry, positional, &named)
}

/// The receiver of a member call `r.M()`, as the call site encodes it
/// (codex ruling P-recv Q1). A receiver written in the call is never `this`
/// unless it says so: only `M()`, `this.M()` and `base.M()` keep the
/// implicit-`this` rules.
enum Receiver<'a> {
    /// A value or a type whose type the parser reads by name: `this`,
    /// `base`, a local, parameter, field or property declared with a plain
    /// type name, `new T()`, a keyword type (`string.Join`).
    Typed(Cow<'a, str>),
    /// `this` or `base` with no type to name: the bare member.
    Implicit,
    /// A name that may be a type (`StringAssert`), the last segment of a
    /// dotted name that may be a type path (`Outer.Inner`, `Box<int>`), or a
    /// value named as its type (`Logger Logger`): the resolver binds it only
    /// as a project type.
    MaybeType(&'a str),
    /// A call whose return type the resolver may read (`Foo.Create().M()`).
    Call,
    /// A value of unknown type: a field of a generic type, `var x`, `a.b`,
    /// `a[i]`, `(T)x`, a delegate's result.
    Untyped,
}

/// The receiver `receiver` of the member call at `pos`. C# looks a simple
/// name up as a local, then as a member of the enclosing types, then as a
/// type: only a name that is neither a local nor a member may be a type.
fn classify_receiver<'a>(
    receiver: Node<'_>,
    pos: Pos,
    source: &'a [u8],
    locals: &'a LocalTypes,
    nodes: &[RawNode],
) -> Option<Receiver<'a>> {
    let text =
        move |node: Node<'_>| std::str::from_utf8(&source[node.start_byte()..node.end_byte()]);
    let typed = |ty: &'a str| match ty {
        "" => Receiver::Untyped,
        ty => Receiver::Typed(Cow::Borrowed(ty)),
    };
    let member = move |name: &str| match locals.member_type(pos, name) {
        Some(ty) => Some(typed(ty)),
        None if declares_member(nodes, pos, name) => Some(Receiver::Untyped),
        None => None,
    };
    let is_this = |node: Node<'_>| matches!(node.kind(), "this_expression" | "this");
    Some(match receiver.kind() {
        "this_expression" | "this" => enclosing_class_name(nodes, pos)
            .map_or(Receiver::Implicit, |ty| Receiver::Typed(Cow::Owned(ty))),
        "base_expression" | "base" => enclosing_base_name(nodes, pos)
            .map_or(Receiver::Implicit, |ty| Receiver::Typed(Cow::Owned(ty))),
        "identifier" => {
            let name = text(receiver).ok()?;
            let value = match locals.lookup(pos, name) {
                Some(ty) => typed(ty),
                None => member(name).unwrap_or(Receiver::MaybeType(name)),
            };
            match value {
                // "Color Color" (C# spec 12.8.7.2): a local, parameter, field
                // or property whose type has its own spelling is the value
                // and the type at once, so `Logger.Create()` reaches a static
                // member as well as an instance one.
                Receiver::Typed(ty) if ty == name => Receiver::MaybeType(name),
                value => value,
            }
        }
        // `this._writer.Dispose()`: a member of the caller's own type.
        "member_access_expression"
            if receiver
                .child_by_field_name("expression")
                .is_some_and(is_this) =>
        {
            receiver
                .child_by_field_name("name")
                .filter(|name| name.kind() == "identifier")
                .and_then(|name| text(name).ok())
                .and_then(member)
                .unwrap_or(Receiver::Untyped)
        }
        "member_access_expression" | "generic_name" | "alias_qualified_name" => {
            type_path_tail(receiver, pos, source, locals, nodes)
                .map_or(Receiver::Untyped, Receiver::MaybeType)
        }
        "predefined_type" => Receiver::Typed(Cow::Borrowed(text(receiver).ok()?)),
        "object_creation_expression" => receiver
            .child_by_field_name("type")
            .filter(|ty| ty.kind() == "identifier")
            .and_then(|ty| text(ty).ok())
            .map_or(Receiver::Untyped, |ty| Receiver::Typed(Cow::Borrowed(ty))),
        "invocation_expression" if !calls_value(receiver, source, locals, nodes) => Receiver::Call,
        _ => Receiver::Untyped,
    })
}

/// The last segment of the dotted name `receiver` when that name may be a
/// type path: every segment is a name, and the root is no local, parameter
/// or member in scope (`Outer.Inner`, `global::Ns.Type`, `Box<int>`).
/// `None` for a value path (`a.b` with `a` a local or a field) and for any
/// other expression.
fn type_path_tail<'a>(
    receiver: Node<'_>,
    pos: Pos,
    source: &'a [u8],
    locals: &LocalTypes,
    nodes: &[RawNode],
) -> Option<&'a str> {
    let name_of = move |node: Node<'_>| -> Option<&'a str> {
        let id = match node.kind() {
            "identifier" => node,
            "generic_name" => node.named_child(0).filter(|id| id.kind() == "identifier")?,
            "alias_qualified_name" => return name_of_field(node, source),
            _ => return None,
        };
        std::str::from_utf8(&source[id.start_byte()..id.end_byte()]).ok()
    };
    let mut node = receiver;
    let mut tail = None;
    while node.kind() == "member_access_expression" {
        let segment = name_of(node.child_by_field_name("name")?)?;
        tail.get_or_insert(segment);
        node = node.child_by_field_name("expression")?;
    }
    let root = name_of(node)?;
    let value = node.kind() == "identifier"
        && (locals.lookup(pos, root).is_some()
            || locals.member_type(pos, root).is_some()
            || declares_member(nodes, pos, root));
    (!value).then_some(tail.unwrap_or(root))
}

/// The name segment of an `alias_qualified_name` (`global::Ns` → `Ns`).
fn name_of_field<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    let name = node.child_by_field_name("name")?;
    let id = match name.kind() {
        "generic_name" => name.named_child(0).filter(|id| id.kind() == "identifier")?,
        _ => name,
    };
    std::str::from_utf8(&source[id.start_byte()..id.end_byte()]).ok()
}

/// Resolve the callee for a C# `invocation_expression`, applying receiver binding.
fn csharp_callee(
    inv: Node<'_>,
    source: &[u8],
    locals: &LocalTypes,
    nodes: &[RawNode],
) -> Option<String> {
    let func = inv.child_by_field_name("function")?;
    let (receiver, method_node) = match func.kind() {
        "identifier" => return func.utf8_text(source).ok().map(str::to_string),
        "member_access_expression" => (
            func.child_by_field_name("expression")?,
            func.child_by_field_name("name")?,
        ),
        // `_writer?.Close()`: the condition is the receiver, read from the
        // tree, never from the text after the last `.`.
        "conditional_access_expression" => {
            let mut cursor = func.walk();
            let binding = func
                .named_children(&mut cursor)
                .find(|child| child.kind() == "member_binding_expression")?;
            (
                func.child_by_field_name("condition")?,
                binding.child_by_field_name("name")?,
            )
        }
        _ => {
            // Fallback: take last segment from the full text.
            return func.utf8_text(source).ok().and_then(|s| {
                let seg = s.rsplit_once('.').map(|(_, t)| t).unwrap_or(s);
                let id: String = seg
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if id.is_empty() {
                    None
                } else {
                    Some(id)
                }
            });
        }
    };
    // `DeserializeObject<Foo>`: the declaration names the method, not its
    // type arguments.
    let method_node = match method_node.kind() {
        "generic_name" => method_node
            .named_child(0)
            .filter(|id| id.kind() == "identifier")?,
        _ => method_node,
    };
    let method = method_node.utf8_text(source).ok()?;
    let entry = match classify_receiver(receiver, start_pos(inv), source, locals, nodes)? {
        Receiver::Typed(ty) => format!("{ty}.{method}"),
        Receiver::Implicit => return Some(method.to_string()),
        Receiver::MaybeType(name) => CallSite::from_receiver(name, method),
        // `Foo.Create().Bar()`: the resolver types the receiver by the
        // callee's return type, and else resolves the untyped member.
        Receiver::Call => {
            let untyped = CallSite::untyped_member(method);
            match CallSite::nested_chain(|| csharp_callee(receiver, source, locals, nodes)) {
                Some(inner) => CallSite::chained_member(&inner, &untyped),
                None => untyped,
            }
        }
        Receiver::Untyped => CallSite::untyped_member(method),
    };
    Some(with_call_arguments(entry, inv, source))
}

/// Walk the AST once, extracting C# invocation sites (attached to
/// enclosing nodes), collecting path-shaped string literals (`string_literal`,
/// `verbatim_string_literal`, `raw_string_literal`), and SQL-shaped strings.
/// Interpolated strings (`$"..."`) are skipped by `build_raw_path_literal` and
/// by the SQL extractor (different node kind).
pub fn extract_csharp_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let local_types = collect_local_types(root, source);
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "invocation_expression" => {
                if let Some(callee) = csharp_callee(n, source, &local_types, nodes) {
                    attach_to_enclosing(start_pos(n), callee, nodes);
                }
            }
            "object_creation_expression" => {
                // Encoded as a construction: a plain C# call never constructs.
                let callee = n
                    .child_by_field_name("type")
                    .and_then(|t| construction_call(t.utf8_text(source).ok()?));
                if let Some(callee) = callee {
                    attach_to_enclosing(start_pos(n), callee, nodes);
                }
            }
            "string_literal" | "verbatim_string_literal" | "raw_string_literal" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same node kinds, separate filter.
                let raw_bytes = &source[n.start_byte()..n.end_byte()];
                if let Ok(raw) = std::str::from_utf8(raw_bytes) {
                    if let Some(value) = strip_csharp_string_value(raw) {
                        if let Some(sql_ref) = crate::sql_literal::try_sql_ref(
                            value,
                            n,
                            enclosing_symbol_and_owner_pub(n, source),
                        ) {
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
    (path_literals, sql_refs)
}

/// Whether a type enclosing `pos` declares a field or property named `name`
/// (both are `Property` nodes). A nested type's members count for the whole
/// outer type: that only keeps more calls on the bare member.
fn declares_member(nodes: &[RawNode], pos: Pos, name: &str) -> bool {
    let is_type = |n: &RawNode| {
        matches!(
            n.kind,
            NodeKind::Class | NodeKind::Struct | NodeKind::Interface
        )
    };
    nodes
        .iter()
        .filter(|m| m.kind == NodeKind::Property && m.name == name)
        .any(|m| {
            let at = (m.span.0, m.span.1);
            nodes
                .iter()
                .any(|t| is_type(t) && span_holds(t.span, pos) && span_holds(t.span, at))
        })
}

fn enclosing_class_name(nodes: &[RawNode], pos: Pos) -> Option<String> {
    let mut best: Option<((u32, i64), &str)> = None;
    for n in nodes {
        if !matches!(n.kind, NodeKind::Class | NodeKind::Interface) {
            continue;
        }
        if span_holds(n.span, pos) {
            let w = span_width(n.span);
            if best.is_none_or(|(bw, _)| w < bw) {
                best = Some((w, &n.name));
            }
        }
    }
    best.map(|(_, name)| name.to_string())
}

fn enclosing_base_name(nodes: &[RawNode], pos: Pos) -> Option<String> {
    let mut best_w = (u32::MAX, i64::MAX);
    let mut result = None;
    for n in nodes {
        if !matches!(n.kind, NodeKind::Class) {
            continue;
        }
        if span_holds(n.span, pos) {
            let w = span_width(n.span);
            if w < best_w {
                best_w = w;
                result = n.heritage.first().cloned();
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::super::parser::CSharpProvider;
    use ecp_core::analyzer::provider::LanguageProvider;
    use ecp_core::analyzer::types::CallSite;
    use std::path::Path;

    fn parse(src: &str) -> ecp_core::analyzer::types::LocalGraph {
        let provider = CSharpProvider::new().expect("CSharpProvider::new");
        provider
            .parse_file(Path::new("Test.cs"), src.as_bytes())
            .expect("parse_file")
    }

    /// `this.Eat()` should be recorded as `Animal.Eat` using the enclosing class name.
    #[test]
    fn csharp_ctor_this_receiver_binds_class_name() {
        let src = r#"
class Animal {
    void Breathe() {
        this.Eat();
    }
    void Eat() {}
}
"#;
        let graph = parse(src);
        let breathe = graph
            .nodes
            .iter()
            .find(|n| n.name == "Breathe")
            .expect("Breathe method not found");
        assert!(
            breathe
                .calls
                .iter()
                .any(|c| CallSite::parse(c) == CallSite::Plain("Animal.Eat")),
            "expected 'Animal.Eat' in calls, got {:?}",
            breathe.calls
        );
    }

    /// `base.Init()` should be recorded as `Animal.Init` (first heritage type).
    #[test]
    fn csharp_ctor_base_receiver_binds_base_class() {
        let src = r#"
class Dog : Animal {
    void Setup() {
        base.Init();
    }
    void Init() {}
}
"#;
        let graph = parse(src);
        let setup = graph
            .nodes
            .iter()
            .find(|n| n.name == "Setup")
            .expect("Setup method not found");
        assert!(
            setup
                .calls
                .iter()
                .any(|c| CallSite::parse(c) == CallSite::Plain("Animal.Init")),
            "expected 'Animal.Init' in calls, got {:?}",
            setup.calls
        );
    }

    /// `MyService svc = new MyService(); svc.Run()` → `MyService.Run`.
    #[test]
    fn csharp_ctor_typed_variable_receiver_binds_declared_type() {
        let src = r#"
class App {
    void Start() {
        MyService svc = new MyService();
        svc.Run();
    }
}
"#;
        let graph = parse(src);
        let start = graph
            .nodes
            .iter()
            .find(|n| n.name == "Start")
            .expect("Start method not found");
        assert!(
            start
                .calls
                .iter()
                .any(|c| CallSite::parse(c) == CallSite::Plain("MyService.Run")),
            "expected 'MyService.Run' in calls, got {:?}",
            start.calls
        );
    }

    /// Property guard: a `var items = ...` or `List<string> items = ...`
    /// declaration must NOT bind `items → List` — calling `items.Add()` would
    /// then emit `"List.Add"`, which is meaningless against the generic
    /// instantiation. `simple_id_and_type` rejects non-`identifier` type
    /// nodes for this reason; this test pins that behaviour.
    #[test]
    fn csharp_generic_type_does_not_bind_bare_type() {
        let src = r#"
class App {
    void Use() {
        List<string> items = new List<string>();
        items.Add("x");
    }
}
"#;
        let graph = parse(src);
        let use_fn = graph
            .nodes
            .iter()
            .find(|n| n.name == "Use")
            .expect("Use method not found");
        assert!(
            !use_fn
                .calls
                .iter()
                .any(|c| CallSite::parse(c) == CallSite::Plain("List.Add")),
            "must NOT bind generic `List<string>` to bare `List`; got {:?}",
            use_fn.calls
        );
        // bare `Add` is acceptable fallback (no type info)
    }

    /// An identifier receiver that is no local, parameter or member reaches
    /// the resolver as a `from` receiver. A property declared with a plain
    /// type name types the call; a local, parameter, `foreach` or lambda
    /// variable of no plain type name is an untyped member, never the bare
    /// member, which means `this.M()` (codex ruling P-recv Q1). Every member
    /// site records its arguments.
    #[test]
    fn test_csharp_callee_undeclared_identifier_receiver_emits_from_receiver() {
        let src = r#"
class App {
    Helper Tool { get; }
    void Use(List<int> items) {
        var local = Make();
        StringAssert.AreEqual("a", "b");
        local.Run();
        items.Add(1);
        Tool.Go();
        foreach (var each in items) { each.Stop(); }
        items.ForEach(x => x.Halt());
    }
}
"#;
        let graph = parse(src);
        // The lambda body's call attaches to an anonymous node.
        let mut got: Vec<String> = graph.nodes.iter().flat_map(|n| n.calls.clone()).collect();
        let args =
            |entry: String, positional: u32| CallSite::with_arguments(&entry, positional, &[]);
        let mut want: Vec<String> = vec![
            "Make".into(),
            args(CallSite::from_receiver("StringAssert", "AreEqual"), 2),
            args(CallSite::untyped_member("Run"), 0),
            args(CallSite::untyped_member("Add"), 1),
            args("Helper.Go".into(), 0),
            args(CallSite::untyped_member("Stop"), 0),
            args(CallSite::untyped_member("ForEach"), 1),
            args(CallSite::untyped_member("Halt"), 0),
        ];
        got.sort();
        want.sort();
        assert_eq!(got, want);
    }

    /// Regression: `base.Foo()` in a class WITHOUT base must emit bare `"Foo"`,
    /// not a synthetic `"base.Foo"` that pollutes the graph.
    #[test]
    fn csharp_base_no_base_emits_bare_method() {
        let src = r#"
class Standalone {
    void DoWork() {
        base.Foo();
    }
}
"#;
        let graph = parse(src);
        let do_work = graph
            .nodes
            .iter()
            .find(|n| n.name == "DoWork")
            .expect("DoWork method not found");
        assert!(
            do_work.calls.iter().any(|c| c == "Foo"),
            "expected bare 'Foo' in calls, got {:?}",
            do_work.calls
        );
        assert!(
            !do_work.calls.iter().any(|c| c == "base.Foo"),
            "must NOT contain synthetic 'base.Foo', got {:?}",
            do_work.calls
        );
    }

    /// Contract (P-ext2, P-recv): every member site and a chain record the
    /// arguments the extension retry reads; each chain level keeps its own.
    /// `ref` / `out` arguments and a lambda count as one argument each. A
    /// bare call and a list with a positional argument after a named one
    /// record none, and every entry decodes as before. A chain falls back
    /// to the untyped member, and `object o; o.Spin(1)` is one.
    #[test]
    fn test_csharp_callee_typed_member_and_chain_record_arguments() {
        let src = r#"
class Widget {
    Widget Make(int n) { return this; }
    void Use(Widget w, object o) {
        w.Spin();
        w.Spin(3, size: 2, mode: 1);
        w.Make(1).Spin(ref o, out var x);
        w.Spin(size: 2, 3);
        w.Spin(x => x);
        Helper(1, 2);
        o.Spin(1);
    }
}
"#;
        let graph = parse(src);
        let use_fn = graph.nodes.iter().find(|n| n.name == "Use").expect("Use");
        // The walk attaches calls in no source order; compare as a sorted list.
        let decode = |raw: &str| {
            (
                format!("{:?}", CallSite::parse(raw)),
                CallSite::arguments(raw)
                    .map(|a| (a.positional, a.named().collect::<Vec<_>>().join(","))),
            )
        };
        let mut got: Vec<_> = use_fn.calls.iter().map(|raw| decode(raw)).collect();
        got.sort();
        let chain = use_fn
            .calls
            .iter()
            .find(|raw| matches!(CallSite::parse(raw), CallSite::ChainedMember { .. }))
            .expect("a chained entry");
        let CallSite::ChainedMember { inner, fallback } = CallSite::parse(chain) else {
            unreachable!()
        };
        let mut want = vec![
            (
                format!("{:?}", CallSite::Plain("Widget.Spin")),
                Some((0, String::new())),
            ),
            (
                format!("{:?}", CallSite::Plain("Widget.Spin")),
                Some((1, "size,mode".to_owned())),
            ),
            (
                format!("{:?}", CallSite::Plain("Widget.Make")),
                Some((1, String::new())),
            ),
            (
                format!("{:?}", CallSite::parse(chain)),
                Some((2, String::new())),
            ),
            (format!("{:?}", CallSite::Plain("Widget.Spin")), None),
            (
                format!("{:?}", CallSite::Plain("Widget.Spin")),
                Some((1, String::new())),
            ),
            (format!("{:?}", CallSite::Plain("Helper")), None),
            (
                format!("{:?}", CallSite::UntypedMember("Spin")),
                Some((1, String::new())),
            ),
        ];
        want.sort();
        assert_eq!(got, want, "{:?}", use_fn.calls);
        assert_eq!(CallSite::parse(fallback), CallSite::UntypedMember("Spin"));
        assert_eq!(
            decode(inner),
            (
                format!("{:?}", CallSite::Plain("Widget.Make")),
                Some((1, String::new()))
            )
        );
    }
}
