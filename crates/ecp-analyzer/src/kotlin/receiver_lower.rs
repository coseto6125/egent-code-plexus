//! Kotlin lowering into the receiver core.
//!
//! A receiver occurrence is a `simple_identifier` that is the receiver of a
//! navigation (`x.m()`), the callee of a call (`x()`) or the type position of
//! a callable reference (`x::m`). Kotlin pins the declared static type of a
//! `val` / `var` / parameter on every path, so a binding needs no control
//! flow: a class or a top-level function lowers as one straight run of
//! statements and the scopes alone decide which declaration a use reaches.
//! A binding with no plain class type (`val x = make()`, `Foo?`, a qualified
//! `Outer.Inner`, a `vararg`, an implicit lambda parameter, a member
//! property, a constructor parameter that is no property) is declared
//! untyped: its uses answer `Unknown(NoEvidence)`, which still shadows an
//! import or a type of that name.
//!
//! A class lowers as one unit, so its primary-constructor parameters reach
//! every member function, initializer and property initializer; a `val` /
//! `var` constructor parameter is a property of the class and carries its
//! declared type. A nested class without `inner`, an `object` and a
//! `companion object` have no enclosing instance, so each lowers in a frame
//! of its own that no enclosing binding reaches.

use crate::framework_helpers::node_span;
use crate::receiver::ir::{BindingOrigin, ScopeKind, TypeOrigin};
use crate::receiver::{DeclaredTypePolicy, IrBuilder, LangPolicy, Lowered, Lowering, ScopeRule};
use tree_sitter::Node;

/// Nested blocks, lambdas and local classes beyond this leave the unit to
/// the extractor's own path.
const MAX_DEPTH: u32 = 256;

pub(super) struct KotlinLowering;

impl Lowering for KotlinLowering {
    type Cx<'a> = ();

    fn policy(&self) -> LangPolicy {
        LangPolicy {
            declared: DeclaredTypePolicy::Pinned,
            scope: ScopeRule::Block,
        }
    }

    fn is_function(&self, node: Node<'_>) -> bool {
        matches!(node.kind(), "class_declaration" | "function_declaration")
    }

    fn lower(&self, _cx: &(), function: Node<'_>, source: &[u8], b: &mut IrBuilder) -> Lowered {
        let mut lower = Lower {
            source,
            depth: 0,
            unsupported: None,
        };
        lower.walk(b, function);
        match lower.unsupported {
            Some(reason) => Lowered::Unsupported(reason),
            None if b.use_count() == 0 => Lowered::NoUses,
            None => Lowered::Done,
        }
    }

    fn function_name<'s>(&self, function: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
        let mut cursor = function.walk();
        let name = function
            .named_children(&mut cursor)
            .find(|c| matches!(c.kind(), "simple_identifier" | "type_identifier"));
        name?.utf8_text(source).ok()
    }
}

/// The class name of a declared type that is a `user_type` of one segment
/// (`Foo`, `List<A>`). A qualified `Outer.Inner` names `Inner`, not its
/// head, so it is no plain class type here, and neither is anything else
/// (`Foo?`, a function type).
fn declared_class<'s>(ty: Option<Node<'_>>, source: &'s [u8]) -> Option<&'s str> {
    let ty = ty.filter(|ty| ty.kind() == "user_type")?;
    let mut segments = named_children(ty)
        .into_iter()
        .filter(|n| n.kind() == "type_identifier");
    let (Some(segment), None) = (segments.next(), segments.next()) else {
        return None;
    };
    segment
        .utf8_text(source)
        .ok()
        .filter(|name| !name.is_empty())
}

/// Whether `modifiers` holds a `kind` modifier spelled `keyword`
/// (`parameter_modifier` `vararg`, `class_modifier` `inner`).
fn has_modifier(modifiers: Node<'_>, kind: &str, keyword: &str) -> bool {
    named_children(modifiers)
        .into_iter()
        .any(|m| m.kind() == kind && m.child(0).is_some_and(|k| k.kind() == keyword))
}

/// A class declared in a class body without `inner`: Kotlin gives it no
/// enclosing instance, so the enclosing class's members are not in scope.
fn is_nested_class(node: Node<'_>) -> bool {
    node.parent().is_some_and(|p| is_class_body(p.kind()))
        && !child_of_kind(node, "modifiers")
            .is_some_and(|m| has_modifier(m, "class_modifier", "inner"))
}

fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    let children = node.named_children(&mut cursor).collect();
    children
}

fn child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    named_children(node).into_iter().find(|c| c.kind() == kind)
}

fn is_class_body(kind: &str) -> bool {
    matches!(kind, "class_body" | "enum_class_body")
}

struct Lower<'s> {
    source: &'s [u8],
    depth: u32,
    unsupported: Option<&'static str>,
}

impl<'s> Lower<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or("")
    }

    fn declare(
        &mut self,
        b: &mut IrBuilder,
        name: &str,
        class: Option<&str>,
        origin: BindingOrigin,
    ) {
        let declared = class.map(|class| b.intern(class, TypeOrigin::Annotation));
        b.declare(name, origin, declared);
    }

    /// Declares the `simple_identifier` of `node` with the `user_type` it
    /// carries; `typed: false` declares it untyped whatever it says.
    fn declare_named(
        &mut self,
        b: &mut IrBuilder,
        node: Node<'_>,
        typed: bool,
        origin: BindingOrigin,
    ) {
        let Some(name) = child_of_kind(node, "simple_identifier") else {
            return;
        };
        let class = typed
            .then(|| declared_class(child_of_kind(node, "user_type"), self.source))
            .flatten();
        self.declare(b, self.text(name), class, origin);
    }

    /// `(a, b)` / `[a, b]`: every entry untyped.
    fn declare_multi(&mut self, b: &mut IrBuilder, node: Node<'_>, origin: BindingOrigin) {
        for entry in named_children(node) {
            if entry.kind() == "variable_declaration" {
                self.declare_named(b, entry, false, origin);
            }
        }
    }

    fn declare_declarations(
        &mut self,
        b: &mut IrBuilder,
        declarations: &[Node<'_>],
        origin: BindingOrigin,
    ) {
        for declaration in declarations {
            match declaration.kind() {
                "variable_declaration" => self.declare_named(b, *declaration, true, origin),
                _ => self.declare_multi(b, *declaration, origin),
            }
        }
    }

    fn walk(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        if self.unsupported.is_some() {
            return;
        }
        if self.depth >= MAX_DEPTH {
            self.unsupported = Some("kotlin receiver lowering exceeds the nesting limit");
            return;
        }
        self.depth += 1;
        let mut cursor = node.walk();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if self.special(b, n) {
                continue;
            }
            self.note_use(b, n);
            let start = stack.len();
            stack.extend(n.named_children(&mut cursor));
            stack[start..].reverse();
        }
        self.depth -= 1;
    }

    /// Lowers `node` and its subtree when it declares a name or opens a
    /// scope; false leaves it to the generic descent.
    fn special(&mut self, b: &mut IrBuilder, node: Node<'_>) -> bool {
        match node.kind() {
            "statements" | "when_expression" => self.scoped(b, node),
            "function_declaration" | "anonymous_function" | "secondary_constructor" | "setter" => {
                self.callable(b, node)
            }
            "class_declaration" if is_nested_class(node) => {
                b.closure(node_span(&node), |inner| self.class(inner, node));
            }
            "class_declaration" => self.class(b, node),
            "object_declaration" | "companion_object" => {
                b.closure(node_span(&node), |inner| {
                    for child in named_children(node) {
                        self.walk(inner, child);
                    }
                });
            }
            "class_body" | "enum_class_body" => self.class_body(b, node),
            "lambda_literal" => self.lambda(b, node),
            "property_declaration" => self.property(b, node),
            "destructuring_declaration" => self.destructuring(b, node),
            "for_statement" => self.for_loop(b, node),
            "catch_block" => self.catch_block(b, node),
            "when_subject" => self.when_subject(b, node),
            _ => return false,
        }
        true
    }

    fn note_use(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let Some(parent) = node.parent() else {
            return;
        };
        let receiver = matches!(
            (node.kind(), parent.kind()),
            (
                "simple_identifier",
                "navigation_expression" | "call_expression"
            ) | ("type_identifier", "callable_reference")
        ) && parent
            .named_child(0)
            .is_some_and(|first| first.id() == node.id());
        if !receiver {
            return;
        }
        if let Some(binding) = b.lookup(self.text(node)) {
            b.use_binding(binding, node.id());
        }
    }

    fn scoped(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        b.push_scope(ScopeKind::Block);
        for child in named_children(node) {
            self.walk(b, child);
        }
        b.pop_scope();
    }

    /// A function, an anonymous function, a secondary constructor or a
    /// setter: its parameters, then everything else. A `vararg xs: T`
    /// holds an `Array<out T>`, so it is declared untyped.
    fn callable(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        b.push_scope(ScopeKind::Block);
        let is_parameters = |c: &Node<'_>| {
            matches!(
                c.kind(),
                "function_value_parameters" | "parameter_with_optional_type"
            )
        };
        let children = named_children(node);
        for params in children.iter().filter(|c| is_parameters(c)) {
            if params.kind() == "parameter_with_optional_type" {
                self.declare_named(b, *params, true, BindingOrigin::Param);
                continue;
            }
            let mut vararg = false;
            for param in named_children(*params) {
                match param.kind() {
                    "parameter_modifiers" => {
                        vararg = has_modifier(param, "parameter_modifier", "vararg")
                    }
                    "parameter" => {
                        self.declare_named(b, param, !vararg, BindingOrigin::Param);
                        vararg = false;
                    }
                    _ => {}
                }
            }
        }
        for child in children.into_iter().filter(|c| !is_parameters(c)) {
            self.walk(b, child);
        }
        b.pop_scope();
    }

    /// The primary-constructor parameters reach the whole class; a `val` /
    /// `var` one is a property and keeps its declared type, unless it is a
    /// `vararg` (an array).
    fn class(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        b.push_scope(ScopeKind::Block);
        let children = named_children(node);
        for constructor in children
            .iter()
            .filter(|c| c.kind() == "primary_constructor")
        {
            for param in named_children(*constructor) {
                if param.kind() == "class_parameter" {
                    let property = child_of_kind(param, "binding_pattern_kind").is_some();
                    let vararg = child_of_kind(param, "modifiers")
                        .is_some_and(|m| has_modifier(m, "parameter_modifier", "vararg"));
                    self.declare_named(b, param, property && !vararg, BindingOrigin::Param);
                }
            }
        }
        for child in children
            .into_iter()
            .filter(|c| c.kind() != "primary_constructor")
        {
            self.walk(b, child);
        }
        b.pop_scope();
    }

    /// A member property is visible to every member function, whatever the
    /// order, and is declared without a type here.
    fn class_body(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        b.push_scope(ScopeKind::Block);
        let children = named_children(node);
        for property in children
            .iter()
            .filter(|c| c.kind() == "property_declaration")
        {
            for declaration in named_children(*property) {
                match declaration.kind() {
                    "variable_declaration" => {
                        self.declare_named(b, declaration, false, BindingOrigin::Local)
                    }
                    "multi_variable_declaration" => {
                        self.declare_multi(b, declaration, BindingOrigin::Local)
                    }
                    _ => {}
                }
            }
        }
        for child in children {
            self.walk(b, child);
        }
        b.pop_scope();
    }

    fn lambda(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        b.push_scope(ScopeKind::Block);
        for child in named_children(node) {
            if child.kind() != "lambda_parameters" {
                self.walk(b, child);
                continue;
            }
            for entry in named_children(child) {
                match entry.kind() {
                    "variable_declaration" => {
                        self.declare_named(b, entry, true, BindingOrigin::Param)
                    }
                    "multi_variable_declaration" => {
                        self.declare_multi(b, entry, BindingOrigin::Param)
                    }
                    _ => {}
                }
            }
        }
        b.pop_scope();
    }

    /// `val x: T = e`: `e` lowers before the name exists. A member property
    /// was declared with its class body.
    fn property(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let member = node.parent().is_some_and(|p| is_class_body(p.kind()));
        let (declarations, rest): (Vec<_>, Vec<_>) =
            named_children(node).into_iter().partition(|c| {
                matches!(
                    c.kind(),
                    "variable_declaration" | "multi_variable_declaration"
                )
            });
        for child in rest {
            self.walk(b, child);
        }
        if !member {
            self.declare_declarations(b, &declarations, BindingOrigin::Local);
        }
    }

    fn destructuring(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let (declarations, rest): (Vec<_>, Vec<_>) = named_children(node)
            .into_iter()
            .partition(|c| c.kind() == "multi_variable_declaration");
        for child in rest {
            self.walk(b, child);
        }
        self.declare_declarations(b, &declarations, BindingOrigin::Local);
    }

    fn when_subject(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let (declarations, rest): (Vec<_>, Vec<_>) = named_children(node)
            .into_iter()
            .partition(|c| c.kind() == "variable_declaration");
        for child in rest {
            self.walk(b, child);
        }
        self.declare_declarations(b, &declarations, BindingOrigin::Local);
    }

    /// `for (x: T in e) body`: `e` lowers outside the loop scope.
    fn for_loop(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let mut declaration = None;
        let mut body = None;
        for child in named_children(node) {
            match child.kind() {
                "variable_declaration" | "multi_variable_declaration" if declaration.is_none() => {
                    declaration = Some(child)
                }
                "control_structure_body" => body = Some(child),
                _ => self.walk(b, child),
            }
        }
        b.push_scope(ScopeKind::Loop);
        if let Some(declaration) = declaration {
            self.declare_declarations(b, &[declaration], BindingOrigin::ForEach);
        }
        if let Some(body) = body {
            self.walk(b, body);
        }
        b.pop_scope();
    }

    fn catch_block(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        b.push_scope(ScopeKind::Catch);
        self.declare_named(b, node, true, BindingOrigin::CatchParam);
        for child in named_children(node) {
            if child.kind() == "statements" {
                self.walk(b, child);
            }
        }
        b.pop_scope();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receiver::{analyze_file, Receiver};
    use std::path::Path;

    /// `(member, text)` of every `x.member()` whose `x` is a `simple_identifier`,
    /// in member order; `text` is `known T`, `unknown Cause` or `none`.
    fn receivers(source: &str) -> Vec<(String, String)> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_kotlin::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let bytes = source.as_bytes();
        let found = analyze_file(
            &KotlinLowering,
            &(),
            tree.root_node(),
            bytes,
            Path::new("T.kt"),
        );
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            let navigation = (n.kind() == "call_expression")
                .then(|| n.named_child(0))
                .flatten()
                .filter(|c| c.kind() == "navigation_expression");
            if let Some(navigation) = navigation {
                let receiver = navigation
                    .named_child(0)
                    .filter(|r| r.kind() == "simple_identifier");
                let member = child_of_kind(navigation, "navigation_suffix")
                    .and_then(|suffix| child_of_kind(suffix, "simple_identifier"));
                if let (Some(receiver), Some(member)) = (receiver, member) {
                    let state = match found.receiver_at(receiver) {
                        Some(Receiver::Known(ty)) => format!("known {}", ty.text),
                        Some(Receiver::Unknown(cause)) => format!("unknown {cause:?}"),
                        None => "none".to_string(),
                    };
                    out.push((member.utf8_text(bytes).unwrap().to_string(), state));
                }
            }
            let mut cursor = n.walk();
            stack.extend(n.named_children(&mut cursor));
        }
        out.sort();
        out
    }

    fn only(source: &str) -> String {
        let mut found = receivers(source);
        assert_eq!(found.len(), 1, "{found:?}");
        found.remove(0).1
    }

    fn pair(member: &str, text: &str) -> (String, String) {
        (member.to_string(), text.to_string())
    }

    #[test]
    fn test_lower_declared_local_is_known() {
        assert_eq!(only("fun f() { val a: A = A()\n a.m() }"), "known A");
    }

    #[test]
    fn test_lower_semicolon_separated_lambda_statements_bind_own_types() {
        assert_eq!(
            receivers(
                "fun run() {\n    run { val obj: A = A() }\n    run { val obj: B = B(); obj.m() }\n}\n"
            ),
            vec![pair("m", "known B")]
        );
    }

    #[test]
    fn test_lower_same_name_lambda_blocks_bind_own_types() {
        assert_eq!(
            receivers(
                "fun f() {\n run { val x: A = A()\n x.first() }\n run { val x: B = B()\n x.second() }\n}"
            ),
            vec![pair("first", "known A"), pair("second", "known B")]
        );
    }

    #[test]
    fn test_lower_if_branch_local_shadows_only_inside_the_branch() {
        assert_eq!(
            receivers(
                "fun f(x: A, c: Boolean) {\n if (c) { val x: B = B()\n x.inner() }\n x.outer()\n}"
            ),
            vec![pair("inner", "known B"), pair("outer", "known A")]
        );
    }

    #[test]
    fn test_lower_untyped_local_answers_no_evidence() {
        assert_eq!(
            only("fun f() { val x = make()\n x.m() }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_nullable_local_answers_no_evidence() {
        assert_eq!(
            only("fun f() { val x: A? = null\n x?.m() }"),
            "unknown NoEvidence"
        );
        assert_eq!(only("fun g(x: A?) { x.n() }"), "unknown NoEvidence");
    }

    #[test]
    fn test_lower_parameter_is_known() {
        assert_eq!(only("fun f(a: A) { a.m() }"), "known A");
    }

    #[test]
    fn test_lower_val_constructor_parameter_is_a_typed_property() {
        assert_eq!(
            only("class VM(private val repository: UserRepository) { fun load() { repository.fetchUsers() } }"),
            "known UserRepository"
        );
    }

    #[test]
    fn test_lower_plain_constructor_parameter_is_untyped() {
        assert_eq!(
            only("class VM(repository: UserRepository) { fun load() { repository.fetchUsers() } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_constructor_parameter_reaches_an_init_block() {
        assert_eq!(
            only("class VM(val repo: Repo) { init { repo.load() } }"),
            "known Repo"
        );
    }

    #[test]
    fn test_lower_member_property_shadows_without_a_type() {
        assert_eq!(
            only("class VM { val repo: Repo = Repo()\n fun load() { repo.load() } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_local_shadows_constructor_property() {
        assert_eq!(
            only("class VM(val x: A) { fun f() { val x: B = B()\n x.m() } }"),
            "known B"
        );
    }

    #[test]
    fn test_lower_for_variables_of_one_name_bind_own_types() {
        assert_eq!(
            receivers(
                "fun f(xs: List<A>, ys: List<B>) {\n for (o: A in xs) { o.one() }\n for (o: B in ys) { o.two() }\n}"
            ),
            vec![pair("one", "known A"), pair("two", "known B")]
        );
    }

    #[test]
    fn test_lower_for_variable_dies_with_the_loop() {
        assert_eq!(
            receivers("class C(val i: C) { fun f(xs: List<A>) {\n for (i: A in xs) { i.one() }\n i.two()\n} }"),
            vec![pair("one", "known A"), pair("two", "known C")]
        );
    }

    #[test]
    fn test_lower_catch_parameter_types_the_caught_class() {
        assert_eq!(
            only("fun f() { try { g() } catch (e: IOException) { e.m() } }"),
            "known IOException"
        );
    }

    #[test]
    fn test_lower_explicit_lambda_parameter_is_known_and_implicit_is_not() {
        assert_eq!(
            receivers("fun f() { g { a: A -> a.p() }\n g { b -> b.q() } }"),
            vec![pair("p", "known A"), pair("q", "unknown NoEvidence")]
        );
    }

    #[test]
    fn test_lower_implicit_it_is_no_binding() {
        assert_eq!(only("fun f() { g { it.m() } }"), "none");
    }

    #[test]
    fn test_lower_destructured_names_are_untyped() {
        assert_eq!(
            only("fun f(p: P) { val (a, b) = p\n a.m() }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_local_function_sees_the_enclosing_local() {
        assert_eq!(
            only("fun f() { val a: A = A()\n fun g() { a.m() } }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_empty_function_body_records_nothing() {
        assert!(receivers("fun f() { }").is_empty());
    }

    #[test]
    fn test_lower_two_calls_on_one_line_each_answer() {
        assert_eq!(
            receivers("fun f(a: A) { a.p(); a.q() }"),
            vec![pair("p", "known A"), pair("q", "known A")]
        );
    }

    #[test]
    fn test_lower_class_without_primary_constructor_lowers_its_functions() {
        assert_eq!(only("class H { fun f(a: A) { a.m() } }"), "known A");
    }

    #[test]
    fn test_lower_qualified_constructor_property_type_is_untyped() {
        assert_eq!(
            only("class VM(private val state: Screen.State) {\n    fun f() {\n        state.copy()\n    }\n}\n"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_qualified_parameter_type_is_untyped_and_generic_keeps_its_class() {
        assert_eq!(
            receivers("fun f(b: Foo.Builder, c: List<A>) {\n    b.one()\n    c.two()\n}\n"),
            vec![pair("one", "unknown NoEvidence"), pair("two", "known List")]
        );
    }

    #[test]
    fn test_lower_qualified_local_and_catch_types_are_untyped() {
        assert_eq!(
            only("fun f() {\n    val s: Outer.Inner = make()\n    s.m()\n}\n"),
            "unknown NoEvidence"
        );
        assert_eq!(
            only("fun f() {\n    try {\n        g()\n    } catch (e: java.io.IOException) {\n        e.m()\n    }\n}\n"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_vararg_parameter_is_untyped_and_the_next_parameter_is_known() {
        assert_eq!(
            receivers("fun f(vararg rows: Row, r: Row) {\n    rows.one()\n    r.two()\n}\n"),
            vec![pair("one", "unknown NoEvidence"), pair("two", "known Row")]
        );
    }

    #[test]
    fn test_lower_annotated_parameter_is_known() {
        assert_eq!(only("fun f(@Ann a: A) {\n    a.m()\n}\n"), "known A");
    }

    #[test]
    fn test_lower_vararg_constructor_property_is_untyped() {
        assert_eq!(
            only("class C(vararg val xs: Row) {\n    fun f() {\n        xs.m()\n    }\n}\n"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_nested_class_does_not_see_the_outer_constructor_property() {
        assert_eq!(
            only("class Repo(private val logger: DbLogger) {\n    class Builder {\n        fun build() {\n            logger.info()\n        }\n    }\n}\n"),
            "none"
        );
    }

    #[test]
    fn test_lower_inner_class_sees_the_outer_constructor_property() {
        assert_eq!(
            only("class Repo(val logger: DbLogger) {\n    inner class Builder {\n        fun build() {\n            logger.info()\n        }\n    }\n}\n"),
            "known DbLogger"
        );
    }

    #[test]
    fn test_lower_companion_and_named_object_do_not_see_the_outer_property() {
        assert_eq!(
            only("class R(val a: A) {\n    companion object {\n        fun h() {\n            a.m()\n        }\n    }\n}\n"),
            "none"
        );
        assert_eq!(
            only("class R(val a: A) {\n    object O {\n        fun k() {\n            a.m()\n        }\n    }\n}\n"),
            "none"
        );
    }

    #[test]
    fn test_lower_nested_interface_does_not_see_the_outer_member_property() {
        assert_eq!(
            only("class R {\n    val a: A = A()\n    interface N {\n        fun g() {\n            a.m()\n        }\n    }\n}\n"),
            "none"
        );
    }

    #[test]
    fn test_lower_local_class_sees_the_enclosing_local() {
        assert_eq!(
            only("class R {\n    fun z() {\n        val a: A = A()\n        class L {\n            fun l() {\n                a.m()\n            }\n        }\n    }\n}\n"),
            "known A"
        );
    }

    #[test]
    fn test_lower_anonymous_object_sees_the_outer_constructor_property() {
        assert_eq!(
            only("class R(val a: A) {\n    fun f() {\n        val o = object : Base() {\n            fun p() {\n                a.m()\n            }\n        }\n    }\n}\n"),
            "known A"
        );
    }

    #[test]
    fn test_lower_nested_class_unit_root_binds_its_own_property() {
        assert_eq!(
            only("object Top {\n    class N(val b: B) {\n        fun g() {\n            b.m()\n        }\n    }\n}\n"),
            "known B"
        );
    }

    #[test]
    fn test_lower_inner_class_of_a_nested_class_sees_only_the_nested_class() {
        assert_eq!(
            receivers("class R(val a: A) {\n    class N(val b: B) {\n        inner class I {\n            fun g() {\n                b.m()\n                a.n()\n            }\n        }\n    }\n}\n"),
            vec![pair("m", "known B"), pair("n", "none")]
        );
    }

    #[test]
    fn test_lower_outer_class_keeps_its_property_after_a_nested_class() {
        assert_eq!(
            only("class R(val a: A) {\n    class N {\n        val b: B = B()\n    }\n    fun f() {\n        a.m()\n    }\n}\n"),
            "known A"
        );
    }

    #[test]
    fn test_lower_setter_parameter_shadows_the_constructor_property() {
        assert_eq!(
            only("class C(val value: A) {\n    var x: B = B()\n        set(value) {\n            value.m()\n        }\n}\n"),
            "unknown NoEvidence"
        );
        assert_eq!(
            only("class C(val v: A) {\n    var x: B = B()\n        set(v: B) {\n            v.m()\n        }\n}\n"),
            "known B"
        );
    }
}
