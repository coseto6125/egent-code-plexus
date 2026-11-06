//! Receiver-type binding for Kotlin call sites.
//!
//! Handles:
//! - `this.foo()` → receiver is the enclosing class/object name
//! - `super.foo()` → first superclass from the enclosing class's `heritage`
//! - `obj.foo()` where `obj` has a known type from local declarations/params
//! - Extension functions: declared as `fun ReceiverType.name()` — the parser
//!   captures them as top-level functions; no special handling needed here
//!   because their receiver type is part of the function name recorded from
//!   the query. The call site `receiver.name()` uses the same `obj.foo()`
//!   lookup path.
//!
//! - `Name.foo()` where `Name` is no local, parameter, import or property
//!   of an enclosing type → [`CallSite::from_receiver`]: the resolver binds
//!   `Name.foo` when `Name` names a type (a same-package `object` or class,
//!   a companion's owner), else resolves the bare member
//!
//! - `Other().m()` where this file declares `class Other` → `Other.m`
//!
//! A receiver binding the receiver core left unproven (the budget) emits
//! [`CallSite::unproven_member`], never the bare name.
//!
//! Falls back to the bare method name for other unresolved receivers, but
//! a computed receiver (`other().m()`) whose `m` an enclosing type declares
//! emits [`CallSite::untyped_member`], so it never binds the caller's own
//! `m` the way an implicit-`this` `m()` does.

use super::path_literals::{
    build_raw_path_literal, enclosing_symbol_and_owner_pub, strip_kotlin_string_value,
};
use super::receiver_lower::KotlinLowering;
use crate::calls::{
    attach_to_enclosing, reference_function, span_holds, span_width, start_pos, Pos,
};
use crate::receiver::lattice::UnknownCause;
use crate::receiver::{analyze_file, FileReceivers, Receiver as CoreReceiver};
use ecp_core::analyzer::types::{BlindSpot, CallSite, RawNode, RawPathLiteral, RawSqlRef};
use ecp_core::graph::NodeKind;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Node;

#[derive(Debug, Default)]
struct LocalTypes {
    /// The receiver state of every local, parameter and property of an
    /// enclosing class that is read as a receiver, keyed by that identifier.
    receivers: FileReceivers,
    imports: HashMap<String, String>,
}

impl LocalTypes {
    /// The receiver state of the binding the identifier `node` reads, `None`
    /// for a name that is no binding here.
    fn local(&self, node: Node<'_>) -> Option<&CoreReceiver> {
        self.receivers.receiver_at(node)
    }

    fn lookup(&self, node: Node<'_>, var: &str) -> Option<&str> {
        match self.local(node) {
            Some(CoreReceiver::Known(ty)) => Some(&*ty.text),
            // An unknown local type still shadows an imported binding.
            Some(CoreReceiver::Unknown(_)) => None,
            None => self.imports.get(var).map(String::as_str),
        }
        .filter(|ty| !ty.is_empty())
    }

    /// Whether a local, parameter or class property declares the name `node`
    /// reads, typed or not, or an import binds `var`.
    fn binds(&self, node: Node<'_>, var: &str) -> bool {
        self.imports.contains_key(var) || self.binds_local(node)
    }

    fn binds_local(&self, node: Node<'_>) -> bool {
        self.local(node).is_some()
    }
}

/// The import bindings of the file, and the receiver state of its locals
/// ([`KotlinLowering`]).
fn collect_local_types(root: Node<'_>, source: &[u8], path: &Path) -> LocalTypes {
    let mut imports = HashMap::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "import_header" {
            let mut children = n.named_children(&mut n.walk()).collect::<Vec<_>>();
            // `import a.b.*` binds no name: its last segment is a package,
            // and a receiver of that name is a property or a lambda parameter.
            if children
                .iter()
                .any(|child| child.kind() == "wildcard_import")
            {
                continue;
            }
            let alias = children
                .iter()
                .find(|child| child.kind() == "import_alias")
                .and_then(|child| child.named_child(0));
            if let Some(binding) = alias
                .or_else(|| {
                    children
                        .drain(..)
                        .find(|child| child.kind() == "identifier")
                })
                .and_then(|child| child.utf8_text(source).ok())
                .and_then(|name| name.rsplit('.').next())
            {
                imports.insert(binding.to_string(), binding.to_string());
            }
            continue;
        }
        // Imports head the file: a declaration holds none.
        if matches!(n.kind(), "class_declaration" | "function_declaration") {
            continue;
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    LocalTypes {
        receivers: analyze_file(&KotlinLowering, &(), root, source, path),
        imports,
    }
}

/// Resolve a Kotlin `call_expression` callee, applying receiver-type binding.
///
/// Kotlin call shapes:
/// - Simple call `foo(...)` → function node, kind `simple_identifier`
/// - Member call `obj.foo(...)` → `navigation_expression` child with
///   `navigation_suffix` containing the method name
fn kotlin_callee(
    call: Node<'_>,
    source: &[u8],
    locals: &LocalTypes,
    nodes: &[RawNode],
) -> Option<String> {
    // First child of call_expression is either simple_identifier or navigation_expression.
    let callee = call.named_child(0)?;
    match callee.kind() {
        "simple_identifier" => callee.utf8_text(source).ok().map(|s| s.to_string()),
        "navigation_expression" => {
            // navigation_expression: receiver, then navigation_suffix(.method)
            let receiver = callee.named_child(0)?;
            let suffix = callee
                .named_children(&mut callee.walk())
                .find(|n| n.kind() == "navigation_suffix")?;
            let method_node = suffix
                .named_children(&mut suffix.walk())
                .find(|n| n.kind() == "simple_identifier")?;
            let method =
                std::str::from_utf8(&source[method_node.start_byte()..method_node.end_byte()])
                    .ok()?;

            let pos = start_pos(call);
            let ty: Option<String> = match receiver.kind() {
                "this_expression" => this_class(receiver, source, nodes, pos),
                "super_expression" => enclosing_superclass(nodes, pos),
                "simple_identifier" => {
                    let var =
                        std::str::from_utf8(&source[receiver.start_byte()..receiver.end_byte()])
                            .ok()?;
                    // Only a name no scope, import or enclosing type binds
                    // may be a type, which the resolver decides.
                    if !locals.binds(receiver, var) && !declares_member(nodes, pos, var) {
                        return Some(CallSite::from_receiver(var, method));
                    }
                    // A binding the core could not follow (the budget) lost
                    // its type, pinned or not: a bare name would bind the
                    // caller's own `m`, so the site stays unproven.
                    let unproven = locals.local(receiver).filter(|state| {
                        !matches!(
                            state,
                            CoreReceiver::Known(_)
                                | CoreReceiver::Unknown(UnknownCause::NoEvidence)
                        )
                    });
                    if let Some(unproven) = unproven {
                        return Some(unproven.call_entry(method));
                    }
                    locals.lookup(receiver, var).map(str::to_string)
                }
                "call_expression" => constructed_class(receiver, source, nodes).filter(|name| {
                    let shadowed = receiver
                        .named_child(0)
                        .is_some_and(|callee| locals.binds(callee, name));
                    !shadowed && !declares_member(nodes, pos, name)
                }),
                _ => None,
            };
            let not_this = !matches!(receiver.kind(), "this_expression" | "super_expression");
            let site = match ty {
                Some(t) => return Some(format!("{t}.{method}")),
                // A computed receiver (`other().m()`) or an untyped value
                // (`val x = make(); x.m()`) is no `this`: where an enclosing
                // type declares `m`, the bare name would bind that member.
                // Elsewhere the bare name still reaches an extension
                // function, which Kotlin declares free and an untyped member
                // call never binds.
                None if not_this && declares(nodes, pos, method, NodeKind::is_callable) => {
                    CallSite::untyped_member(method)
                }
                None => method.to_string(),
            };
            // A call receiver the parser cannot type: the resolver types it
            // by the callee's return type.
            // A callee that is a local or property (`val Other = { .. }`)
            // is a value, not a declaration whose return type types it.
            let calls_value = || {
                receiver
                    .named_child(0)
                    .filter(|f| f.kind() == "simple_identifier")
                    .is_some_and(|f| {
                        locals.binds_local(f)
                            || f.utf8_text(source)
                                .is_ok_and(|name| declares_member(nodes, pos, name))
                    })
            };
            Some(match receiver.kind() {
                "call_expression" if !calls_value() => {
                    match CallSite::nested_chain(|| kotlin_callee(receiver, source, locals, nodes))
                    {
                        Some(inner) => CallSite::chained_member(&inner, &site),
                        None => site,
                    }
                }
                _ => site,
            })
        }
        _ => None,
    }
}

/// Walk the AST once, extracting Kotlin `call_expression` nodes with
/// receiver-type binding and collecting path-shaped string literals
/// (`string_literal` / `multiline_string_literal`, interpolated forms
/// filtered out in `build_raw_path_literal`) and SQL-shaped string literals.
pub fn extract_kotlin_calls_and_path_literals(
    root: Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
    path: &Path,
    blind_spots: &mut Vec<BlindSpot>,
) -> (Vec<RawPathLiteral>, Vec<RawSqlRef>) {
    let mut local_types = collect_local_types(root, source, path);
    blind_spots.append(&mut local_types.receivers.blind_spots);
    let mut path_literals: Vec<RawPathLiteral> = Vec::new();
    let mut sql_refs: Vec<RawSqlRef> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "call_expression" => {
                if let Some(callee) = kotlin_callee(n, source, &local_types, nodes) {
                    attach_to_enclosing(start_pos(n), callee, nodes);
                }
            }
            "callable_reference" | "navigation_expression" => {
                if let Some(name) = bound_callable_reference(n, source, &local_types, nodes) {
                    reference_function(start_pos(n), &name, nodes);
                }
            }
            "string_literal" | "multiline_string_literal" => {
                if let Some(rpl) = build_raw_path_literal(n, source) {
                    path_literals.push(rpl);
                }
                // SQL ref extraction: same string node, separate filter.
                // Skip interpolated strings (have `interpolation` children).
                let has_interpolation = {
                    let mut c = n.walk();
                    let x = n.children(&mut c).any(|ch| ch.kind() == "interpolation");
                    x
                };
                if !has_interpolation {
                    let raw_bytes = &source[n.start_byte()..n.end_byte()];
                    if let Ok(raw) = std::str::from_utf8(raw_bytes) {
                        if let Some(value) = strip_kotlin_string_value(raw) {
                            if !value.contains("${") {
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

/// The method a bound callable reference names, typed as the call
/// `c.m()` / `this.m()` is: `c::m` with `c` a local or parameter of a
/// declared type `T` names `T.m`, and `this::m` names `m` of the enclosing
/// class. tree-sitter-kotlin parses `this::m` as a `navigation_expression`
/// whose suffix starts with `::`. A receiver of no known type names nothing;
/// a type receiver (`Cb::m`) is the name scan's.
fn bound_callable_reference(
    reference: Node<'_>,
    source: &[u8],
    locals: &LocalTypes,
    nodes: &[RawNode],
) -> Option<String> {
    if reference.named_child_count() != 2 {
        return None;
    }
    let (receiver, member) = (reference.named_child(0)?, reference.named_child(1)?);
    let pos = start_pos(reference);
    let (ty, member) = match (reference.kind(), receiver.kind()) {
        ("callable_reference", "type_identifier") => {
            let var = receiver.utf8_text(source).ok()?;
            let ty = locals
                .binds_local(receiver)
                .then(|| locals.lookup(receiver, var))??;
            (ty.to_string(), member)
        }
        ("navigation_expression", "this_expression")
            if member.child(0).is_some_and(|op| op.kind() == "::") =>
        {
            let name = member
                .named_children(&mut member.walk())
                .find(|n| n.kind() == "simple_identifier")?;
            (this_class(receiver, source, nodes, pos)?, name)
        }
        _ => return None,
    };
    if member.kind() != "simple_identifier" {
        return None;
    }
    Some(format!("{ty}.{}", member.utf8_text(source).ok()?))
}

/// Whether a type enclosing `pos` declares a property `name` as its own
/// member. A property of a nested type that does not enclose `pos` binds
/// nothing there, so it shadows no class of that name.
fn declares_member(nodes: &[RawNode], pos: Pos, name: &str) -> bool {
    nodes
        .iter()
        .filter(|m| m.kind == NodeKind::Property && m.name == name)
        .any(|m| {
            let at = (m.span.0, m.span.1);
            nodes
                .iter()
                .filter(|t| is_type(t) && span_holds(t.span, at))
                .min_by_key(|t| span_width(t.span))
                .is_some_and(|owner| span_holds(owner.span, pos))
        })
}

fn is_type(n: &RawNode) -> bool {
    matches!(
        n.kind,
        NodeKind::Class | NodeKind::Interface | NodeKind::Enum
    )
}

/// Whether a type enclosing `pos` declares a member `name` of a kind
/// `member` accepts. A nested type's members count for the whole outer
/// type: that only keeps more calls on the bare member.
fn declares(nodes: &[RawNode], pos: Pos, name: &str, member: fn(NodeKind) -> bool) -> bool {
    nodes
        .iter()
        .filter(|m| member(m.kind) && m.name == name)
        .any(|m| {
            let at = (m.span.0, m.span.1);
            nodes
                .iter()
                .any(|t| is_type(t) && span_holds(t.span, pos) && span_holds(t.span, at))
        })
}

/// The class a constructor call receiver (`Other()`) builds, when this
/// file declares a class of that name. A local, parameter or property of
/// that name shadows the class (`val Other = { Actual() }`); the caller
/// checks those.
fn constructed_class(call: Node<'_>, source: &[u8], nodes: &[RawNode]) -> Option<String> {
    let callee = call
        .named_child(0)
        .filter(|n| n.kind() == "simple_identifier")?;
    let name = callee.utf8_text(source).ok()?;
    nodes
        .iter()
        .any(|n| n.kind == NodeKind::Class && n.name == name)
        .then(|| name.to_string())
}

/// The class a `this` stands for at `pos`: the innermost one, or for
/// `this@Outer` the enclosing class labelled `Outer`, and none when no
/// enclosing class carries the label (`this@forEach` names a lambda).
fn this_class(this: Node<'_>, source: &[u8], nodes: &[RawNode], pos: Pos) -> Option<String> {
    let Some(label) = this.named_child(0) else {
        return enclosing_class_name(nodes, pos);
    };
    let label = label.utf8_text(source).ok()?;
    nodes
        .iter()
        .any(|n| n.kind == NodeKind::Class && n.name == label && span_holds(n.span, pos))
        .then(|| label.to_string())
}

fn enclosing_class_name(nodes: &[RawNode], pos: Pos) -> Option<String> {
    let mut best: Option<((u32, i64), &str)> = None;
    for n in nodes {
        if !matches!(n.kind, NodeKind::Class) {
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

fn enclosing_superclass(nodes: &[RawNode], pos: Pos) -> Option<String> {
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
    use super::super::parser::KotlinProvider;
    use super::{kotlin_callee, LocalTypes};
    use crate::kotlin::receiver_lower::KotlinLowering;
    use crate::receiver::ir::{BindingOrigin, Value};
    use crate::receiver::solve::MAX_TRACKED_CELLS;
    use crate::receiver::{analyze_file, IrBuilder, LangPolicy, Lowered, Lowering};
    use ecp_core::analyzer::provider::LanguageProvider;
    use ecp_core::analyzer::types::CallSite;
    use std::collections::HashMap;
    use std::path::Path;
    use tree_sitter::Node;

    fn parse(src: &str) -> ecp_core::analyzer::types::LocalGraph {
        let provider = KotlinProvider::new().expect("KotlinProvider::new");
        provider
            .parse_file(Path::new("Test.kt"), src.as_bytes())
            .expect("parse_file")
    }

    /// `this.eat()` inside a Kotlin class should be recorded as `Animal.eat`.
    #[test]
    fn kotlin_ctor_this_receiver_binds_class_name() {
        let src = r#"
class Animal {
    fun breathe() {
        this.eat()
    }
    fun eat() {}
}
"#;
        let graph = parse(src);
        let breathe = graph
            .nodes
            .iter()
            .find(|n| n.name == "breathe")
            .expect("breathe function not found");
        assert!(
            breathe.calls.iter().any(|c| c == "Animal.eat"),
            "expected 'Animal.eat' in calls, got {:?}",
            breathe.calls
        );
    }

    /// `super.init()` should bind to the first superclass in heritage.
    #[test]
    fn kotlin_ctor_super_receiver_binds_superclass() {
        let src = r#"
class Dog : Animal() {
    fun setup() {
        super.init()
    }
    fun init() {}
}
"#;
        let graph = parse(src);
        let setup = graph
            .nodes
            .iter()
            .find(|n| n.name == "setup")
            .expect("setup function not found");
        assert!(
            setup.calls.iter().any(|c| c == "Animal.init"),
            "expected 'Animal.init' in calls, got {:?}",
            setup.calls
        );
    }

    /// Typed `val obj: MyService` → `obj.run()` should yield `MyService.run`.
    #[test]
    fn kotlin_ctor_typed_variable_receiver_binds_declared_type() {
        let src = r#"
class App {
    fun start() {
        val svc: MyService = MyService()
        svc.run()
    }
}
"#;
        let graph = parse(src);
        let start = graph
            .nodes
            .iter()
            .find(|n| n.name == "start")
            .expect("start function not found");
        assert!(
            start.calls.iter().any(|c| c == "MyService.run"),
            "expected 'MyService.run' in calls, got {:?}",
            start.calls
        );
    }

    /// Regression: `super.foo()` in a class WITHOUT base must emit bare `"foo"`,
    /// not a synthetic `"super.foo"` that pollutes the graph.
    #[test]
    fn kotlin_super_no_base_emits_bare_method() {
        let src = r#"
class Standalone {
    fun doWork() {
        super.foo()
    }
}
"#;
        let graph = parse(src);
        let do_work = graph
            .nodes
            .iter()
            .find(|n| n.name == "doWork")
            .expect("doWork function not found");
        assert!(
            do_work.calls.iter().any(|c| c == "foo"),
            "expected bare 'foo' in calls, got {:?}",
            do_work.calls
        );
        assert!(
            !do_work.calls.iter().any(|c| c == "super.foo"),
            "must NOT contain synthetic 'super.foo', got {:?}",
            do_work.calls
        );
    }

    /// `import a.b.*` binds no receiver, and a `val` constructor parameter is
    /// a property of the class: `repository.fetchUsers()` types its receiver
    /// as the parameter's declared class.
    #[test]
    fn test_kotlin_callee_val_constructor_parameter_receiver_emits_declared_type() {
        let src = r#"
package com.app.ui
import com.app.repository.*
class VM(private val repository: UserRepository) {
    fun load() {
        repository.fetchUsers()
    }
}
"#;
        let graph = parse(src);
        let load = graph
            .nodes
            .iter()
            .find(|n| n.name == "load")
            .expect("load function not found");
        assert_eq!(load.calls, ["UserRepository.fetchUsers"]);
    }

    /// An untyped local (`val x = make()`) is no `this`: `x.save()` must not
    /// emit the bare `save`, which would bind the enclosing class's own
    /// `save`. Where no enclosing type declares the name, the bare name
    /// stays, so an extension function can still bind.
    #[test]
    fn test_kotlin_callee_untyped_local_receiver_own_method_namesake_emits_untyped_member() {
        let src = "class Repo {\n    fun save() {}\n    fun go() {\n        val x = make()\n        x.save()\n        x.close()\n    }\n}\n";
        let graph = parse(src);
        let go = graph
            .nodes
            .iter()
            .find(|n| n.name == "go")
            .expect("go function not found");
        let mut calls = go.calls.clone();
        calls.sort_unstable();
        let mut expected = [
            "make".to_string(),
            CallSite::untyped_member("save"),
            "close".to_string(),
        ];
        expected.sort_unstable();
        assert_eq!(calls, expected);
    }

    /// An explicit class import still qualifies a static-style receiver.
    #[test]
    fn test_kotlin_callee_class_import_receiver_emits_qualified_method() {
        let src = "import pkg.Helper\nfun go() { Helper.work() }\n";
        let graph = parse(src);
        let go = graph
            .nodes
            .iter()
            .find(|n| n.name == "go")
            .expect("go function not found");
        assert_eq!(go.calls, ["Helper.work"]);
    }

    /// Lowers the function that reads `repo` with more written, read cells
    /// than the receiver core tracks, `repo` untyped among them, so `repo`
    /// answers `Unknown(Budget)` whatever the solver's tracking rule.
    struct OverBudget;

    impl Lowering for OverBudget {
        type Cx<'a> = ();

        fn policy(&self) -> LangPolicy {
            KotlinLowering.policy()
        }

        fn is_function(&self, node: Node<'_>) -> bool {
            node.kind() == "function_declaration"
        }

        fn lower(&self, _cx: &(), function: Node<'_>, source: &[u8], b: &mut IrBuilder) -> Lowered {
            let mut stack = vec![function];
            let mut repo = None;
            while let Some(n) = stack.pop() {
                // The receiver of `repo.save()`, not the parameter name.
                if n.kind() == "simple_identifier"
                    && n.utf8_text(source) == Ok("repo")
                    && n.parent()
                        .is_some_and(|p| p.kind() == "navigation_expression")
                {
                    repo = Some(n);
                }
                stack.extend(n.named_children(&mut n.walk()));
            }
            let Some(repo) = repo else {
                return Lowered::NoUses;
            };
            for i in 0..MAX_TRACKED_CELLS {
                let filler = b.declare(&format!("x{i}"), BindingOrigin::Local, None);
                b.assign(filler, Value::Unknown);
                b.use_binding(filler, i);
            }
            let binding = b.declare("repo", BindingOrigin::Param, None);
            b.assign(binding, Value::Unknown);
            b.use_binding(binding, repo.id());
            Lowered::Done
        }
    }

    /// Over the budget a receiver's type is lost. The bare `save` would bind
    /// the caller's own `save` the way an implicit-`this` call does; the
    /// site must be an unproven member, which binds nothing in Kotlin.
    #[test]
    fn test_kotlin_callee_over_budget_receiver_emits_unproven_member() {
        let src = "class Big {\n    fun save() {}\n    fun store(repo: Repo) {\n        repo.save()\n    }\n}\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_kotlin::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        let locals = LocalTypes {
            receivers: analyze_file(
                &OverBudget,
                &(),
                tree.root_node(),
                src.as_bytes(),
                Path::new("Big.kt"),
            ),
            imports: HashMap::new(),
        };
        let mut stack = vec![tree.root_node()];
        let mut call = None;
        while let Some(n) = stack.pop() {
            if n.kind() == "call_expression" {
                call = Some(n);
            }
            stack.extend(n.named_children(&mut n.walk()));
        }
        let call = call.expect("repo.save() call");
        assert_eq!(
            kotlin_callee(call, src.as_bytes(), &locals, &[]),
            Some(CallSite::unproven_member("save"))
        );
    }

    /// Under the budget the same parameter keeps its declared type.
    #[test]
    fn test_kotlin_callee_typed_parameter_in_a_class_emits_declared_type() {
        let graph = parse("class Big {\n    fun save() {}\n    fun store(repo: Repo) {\n        repo.save()\n    }\n}\n");
        let store = graph
            .nodes
            .iter()
            .find(|n| n.name == "store")
            .expect("store function not found");
        assert_eq!(store.calls, ["Repo.save"]);
    }
}
