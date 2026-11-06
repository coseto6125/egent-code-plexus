//! Java lowering into the receiver core.
//!
//! A receiver occurrence is `x.m(..)` or `x::m` with `x` a plain identifier
//! that names a local, a parameter or a pattern variable of the method.
//! Java pins the declared static type on every path, so a binding needs no
//! control flow: the whole body lowers as one straight run of statements and
//! the scopes alone decide which declaration a use reaches. A binding whose
//! type this lowering cannot spell (a generic, an array, a primitive, an
//! implicit lambda parameter, a field of a local class) is declared `opaque`:
//! it shadows outer names but records no use, so the extractor keeps its
//! bare-name path for it.

use super::receiver_types::expression_type;
use crate::receiver::ir::{BindingId, BindingOrigin, ScopeKind, TypeOrigin};
use crate::receiver::{DeclaredTypePolicy, IrBuilder, LangPolicy, Lowered, Lowering, ScopeRule};
use rustc_hash::FxHashSet;
use tree_sitter::Node;

/// Nested blocks, lambdas and local classes beyond this leave the method to
/// the extractor's own path.
const MAX_DEPTH: u32 = 256;

pub(super) struct JavaLowering;

impl Lowering for JavaLowering {
    type Cx<'a> = ();

    fn policy(&self) -> LangPolicy {
        LangPolicy {
            declared: DeclaredTypePolicy::Pinned,
            scope: ScopeRule::Block,
        }
    }

    fn is_function(&self, node: Node<'_>) -> bool {
        matches!(
            node.kind(),
            "method_declaration" | "constructor_declaration"
        )
    }

    fn lower(&self, _cx: &(), function: Node<'_>, source: &[u8], b: &mut IrBuilder) -> Lowered {
        let mut lower = Lower {
            source,
            depth: 0,
            unsupported: None,
            opaque: FxHashSet::default(),
        };
        lower.callable(b, function, false);
        match lower.unsupported {
            Some(reason) => Lowered::Unsupported(reason),
            None if b.use_count() == 0 => Lowered::NoUses,
            None => Lowered::Done,
        }
    }
}

/// The class name of a declared type that is one plain `type_identifier`.
/// `var` is no class name: the initializer decides it.
fn plain_type<'s>(ty: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    if ty.kind() != "type_identifier" {
        return None;
    }
    let text = ty.utf8_text(source).ok()?;
    let spelled = !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_');
    (spelled && text != "var").then_some(text)
}

/// Kinds that open their own scope and whose children lower in order.
fn scopes_children(kind: &str) -> bool {
    matches!(
        kind,
        "block"
            | "constructor_body"
            | "switch_block"
            | "switch_rule"
            | "if_statement"
            | "while_statement"
            | "do_statement"
            | "for_statement"
            | "expression_statement"
            | "return_statement"
            | "ternary_expression"
            | "try_with_resources_statement"
            | "catch_clause"
    )
}

struct Lower<'s> {
    source: &'s [u8],
    depth: u32,
    unsupported: Option<&'static str>,
    opaque: FxHashSet<BindingId>,
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
        let id = b.declare(name, origin, declared);
        if declared.is_none() {
            self.opaque.insert(id);
        }
    }

    /// Declares `name` as `ty` writes it; an array dimension on the name makes it opaque.
    fn declare_typed(
        &mut self,
        b: &mut IrBuilder,
        name: Node<'_>,
        ty: Option<Node<'_>>,
        array: bool,
        origin: BindingOrigin,
    ) {
        if name.kind() != "identifier" {
            return;
        }
        let class = ty
            .filter(|_| !array)
            .and_then(|ty| plain_type(ty, self.source));
        self.declare(b, self.text(name), class, origin);
    }

    fn callable(&mut self, b: &mut IrBuilder, function: Node<'_>, nested: bool) {
        if nested {
            b.push_scope(ScopeKind::Block);
        }
        if let Some(params) = function.child_by_field_name("parameters") {
            self.parameters(b, params);
        }
        if let Some(body) = function.child_by_field_name("body") {
            self.walk(b, body);
        }
        if nested {
            b.pop_scope();
        }
    }

    fn parameters(&mut self, b: &mut IrBuilder, params: Node<'_>) {
        let mut cursor = params.walk();
        let children: Vec<_> = params.named_children(&mut cursor).collect();
        for param in children {
            match param.kind() {
                "formal_parameter" => {
                    if let Some(name) = param.child_by_field_name("name") {
                        let array = param.child_by_field_name("dimensions").is_some();
                        self.declare_typed(
                            b,
                            name,
                            param.child_by_field_name("type"),
                            array,
                            BindingOrigin::Param,
                        );
                    }
                }
                "spread_parameter" => {
                    let mut inner = param.walk();
                    let declarator = param
                        .named_children(&mut inner)
                        .find(|c| c.kind() == "variable_declarator");
                    if let Some(name) = declarator.and_then(|d| d.child_by_field_name("name")) {
                        self.declare_typed(b, name, None, true, BindingOrigin::Param);
                    }
                }
                _ => {}
            }
        }
    }

    fn walk(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        if self.unsupported.is_some() {
            return;
        }
        if self.depth >= MAX_DEPTH {
            self.unsupported = Some("java receiver lowering exceeds the nesting limit");
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

    fn walk_children(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        for child in children {
            self.walk(b, child);
        }
    }

    /// Lowers `node` and its subtree when it declares a name or opens a
    /// scope; false leaves it to the generic descent.
    fn special(&mut self, b: &mut IrBuilder, node: Node<'_>) -> bool {
        let kind = node.kind();
        if scopes_children(kind) {
            b.push_scope(ScopeKind::Block);
            self.walk_children(b, node);
            b.pop_scope();
            return true;
        }
        match kind {
            "method_declaration" | "constructor_declaration" => self.callable(b, node, true),
            "class_body" => self.class_body(b, node),
            "lambda_expression" => self.lambda(b, node),
            "local_variable_declaration" => self.local_declaration(b, node),
            "enhanced_for_statement" => self.enhanced_for(b, node),
            "catch_formal_parameter" => self.catch_parameter(b, node),
            "resource" => self.resource(b, node),
            "instanceof_expression" => self.instance_of(b, node),
            "type_pattern" | "record_pattern_component" => self.pattern_variable(b, node),
            _ => return false,
        }
        true
    }

    fn note_use(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let receiver = match node.kind() {
            "method_invocation" => node.child_by_field_name("object"),
            "method_reference" => node.named_child(0),
            _ => None,
        };
        let Some(receiver) = receiver.filter(|r| r.kind() == "identifier") else {
            return;
        };
        if let Some(binding) = b.lookup(self.text(receiver)) {
            if !self.opaque.contains(&binding) {
                b.use_binding(binding, receiver.id());
            }
        }
    }

    /// A local or anonymous class body: its fields shadow the method's
    /// locals, and its methods nest.
    fn class_body(&mut self, b: &mut IrBuilder, body: Node<'_>) {
        b.push_scope(ScopeKind::Block);
        let mut cursor = body.walk();
        let members: Vec<_> = body.named_children(&mut cursor).collect();
        for field in members.iter().filter(|m| m.kind() == "field_declaration") {
            let mut inner = field.walk();
            let declarators: Vec<_> = field
                .children_by_field_name("declarator", &mut inner)
                .collect();
            for name in declarators
                .iter()
                .filter_map(|d| d.child_by_field_name("name"))
            {
                self.declare_typed(b, name, None, true, BindingOrigin::Local);
            }
        }
        for member in members {
            self.walk(b, member);
        }
        b.pop_scope();
    }

    fn lambda(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        b.push_scope(ScopeKind::Block);
        if let Some(params) = node.child_by_field_name("parameters") {
            match params.kind() {
                "formal_parameters" => self.parameters(b, params),
                "identifier" => self.declare_typed(b, params, None, true, BindingOrigin::Param),
                _ => {
                    let mut cursor = params.walk();
                    let names: Vec<_> = params.named_children(&mut cursor).collect();
                    for name in names {
                        self.declare_typed(b, name, None, true, BindingOrigin::Param);
                    }
                }
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(b, body);
        }
        b.pop_scope();
    }

    fn local_declaration(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let ty = node.child_by_field_name("type");
        let is_var = ty.is_some_and(|ty| self.text(ty) == "var");
        let mut cursor = node.walk();
        let declarators: Vec<_> = node
            .children_by_field_name("declarator", &mut cursor)
            .collect();
        for declarator in declarators {
            let Some(name) = declarator.child_by_field_name("name") else {
                continue;
            };
            let value = declarator.child_by_field_name("value");
            let array = declarator.child_by_field_name("dimensions").is_some();
            if is_var {
                let class = value.and_then(|v| expression_type(v, self.source));
                if name.kind() == "identifier" {
                    self.declare(b, self.text(name), class, BindingOrigin::Local);
                }
            } else {
                self.declare_typed(b, name, ty, array, BindingOrigin::Local);
            }
            if let Some(value) = value {
                b.push_scope(ScopeKind::Block);
                self.walk(b, value);
                b.pop_scope();
            }
        }
    }

    fn enhanced_for(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        if let Some(value) = node.child_by_field_name("value") {
            self.walk(b, value);
        }
        b.push_scope(ScopeKind::Loop);
        if let Some(name) = node.child_by_field_name("name") {
            let array = node.child_by_field_name("dimensions").is_some();
            self.declare_typed(
                b,
                name,
                node.child_by_field_name("type"),
                array,
                BindingOrigin::ForEach,
            );
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(b, body);
        }
        b.pop_scope();
    }

    /// `catch (A e)` types `e`; `catch (A | B e)` has no one static class.
    fn catch_parameter(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let mut cursor = node.walk();
        let caught = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "catch_type");
        let single = caught
            .filter(|c| c.named_child_count() == 1)
            .and_then(|c| c.named_child(0));
        let array = node.child_by_field_name("dimensions").is_some();
        self.declare_typed(b, name, single, array, BindingOrigin::CatchParam);
    }

    fn resource(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let (Some(name), Some(ty)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("type"),
        ) else {
            self.walk_children(b, node);
            return;
        };
        let is_var = self.text(ty) == "var";
        let array = node.child_by_field_name("dimensions").is_some();
        if is_var {
            let class = node
                .child_by_field_name("value")
                .and_then(|v| expression_type(v, self.source));
            self.declare(b, self.text(name), class, BindingOrigin::Local);
        } else {
            self.declare_typed(b, name, Some(ty), array, BindingOrigin::Local);
        }
        if let Some(value) = node.child_by_field_name("value") {
            self.walk(b, value);
        }
    }

    /// `left instanceof T name`; a record pattern declares through its components.
    fn instance_of(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        if let Some(left) = node.child_by_field_name("left") {
            self.walk(b, left);
        }
        if let (Some(name), Some(ty)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("right"),
        ) {
            self.declare_typed(b, name, Some(ty), false, BindingOrigin::Pattern);
        }
        if let Some(pattern) = node.child_by_field_name("pattern") {
            self.walk(b, pattern);
        }
    }

    /// `T name` as a type pattern or a record component: the type is the
    /// first named child, the name the last.
    fn pattern_variable(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let count = node.named_child_count();
        if count < 2 {
            return;
        }
        let (Some(ty), Some(name)) = (node.named_child(0), node.named_child(count as u32 - 1))
        else {
            return;
        };
        self.declare_typed(b, name, Some(ty), false, BindingOrigin::Pattern);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receiver::{analyze_file, Receiver};
    use std::path::Path;

    /// `(member, text)` of every `x.member()` and `x::member` whose `x` is an
    /// identifier, in member order; `text` is `known T`, `unknown Cause` or `none`.
    fn receivers(source: &str) -> Vec<(String, String)> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let bytes = source.as_bytes();
        let found = analyze_file(
            &JavaLowering,
            &(),
            tree.root_node(),
            bytes,
            Path::new("T.java"),
        );
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            let site = match n.kind() {
                "method_invocation" => n
                    .child_by_field_name("object")
                    .zip(n.child_by_field_name("name")),
                "method_reference" => n
                    .named_child(0)
                    .zip(n.named_child(n.named_child_count() as u32 - 1)),
                _ => None,
            };
            if let Some((object, member)) = site.filter(|(o, _)| o.kind() == "identifier") {
                let state = match found.receiver_at(object) {
                    Some(Receiver::Known(ty)) => format!("known {}", ty.text),
                    Some(Receiver::Unknown(cause)) => format!("unknown {cause:?}"),
                    None => "none".to_string(),
                };
                out.push((member.utf8_text(bytes).unwrap().to_string(), state));
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
        assert_eq!(
            only("class H { void f() { A a = new A(); a.m(); } }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_same_name_sibling_blocks_bind_own_types() {
        assert_eq!(
            receivers(
                "class H { void f() { { A x = new A(); x.first(); } { B x = new B(); x.second(); } } }"
            ),
            vec![pair("first", "known A"), pair("second", "known B")]
        );
    }

    #[test]
    fn test_lower_block_local_shadows_only_inside_block() {
        assert_eq!(
            receivers("class H { A x; void f() { { B x = new B(); x.inner(); } x.outer(); } }"),
            vec![pair("inner", "known B"), pair("outer", "none")]
        );
    }

    #[test]
    fn test_lower_for_variables_of_one_name_bind_own_types() {
        assert_eq!(
            receivers(
                "class H { void f(List<A> as, List<B> bs) { for (A o : as) { o.a(); } for (B o : bs) { o.b(); } } }"
            ),
            vec![pair("a", "known A"), pair("b", "known B")]
        );
    }

    #[test]
    fn test_lower_classic_for_variable_dies_with_the_loop() {
        assert_eq!(
            receivers("class H { C i; void f() { for (A i = null; ; ) { i.a(); } i.b(); } }"),
            vec![pair("a", "known A"), pair("b", "none")]
        );
    }

    #[test]
    fn test_lower_method_reference_receiver_is_a_use() {
        assert_eq!(
            only("class H { void f() { B x = new B(); Runnable r = x::m; } }"),
            "known B"
        );
    }

    #[test]
    fn test_lower_generic_local_records_no_use() {
        assert_eq!(
            only("class H { void f(List<A> xs) { xs.add(1); } }"),
            "none"
        );
    }

    #[test]
    fn test_lower_opaque_inner_name_hides_typed_outer_one() {
        assert_eq!(
            only("class H { void f(A x) { { List<B> x = null; x.add(1); } } }"),
            "none"
        );
    }

    #[test]
    fn test_lower_array_declarator_records_no_use() {
        assert_eq!(
            only("class H { void f() { A x[] = null; x.clone(); } }"),
            "none"
        );
    }

    #[test]
    fn test_lower_multi_declarator_types_each_name() {
        assert_eq!(
            receivers("class H { void f() { A a = null, b = null; a.p(); b.q(); } }"),
            vec![pair("p", "known A"), pair("q", "known A")]
        );
    }

    #[test]
    fn test_lower_var_with_constructor_takes_the_created_type() {
        assert_eq!(
            only("class H { void f() { var x = new A(); x.m(); } }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_var_without_constructor_records_no_use() {
        assert_eq!(
            only("class H { void f() { var x = make(); x.m(); } }"),
            "none"
        );
    }

    #[test]
    fn test_lower_catch_parameter_types_the_caught_class() {
        assert_eq!(
            only("class H { void f() { try { } catch (IOException e) { e.m(); } } }"),
            "known IOException"
        );
    }

    #[test]
    fn test_lower_multi_catch_parameter_records_no_use() {
        assert_eq!(
            only("class H { void f() { try { } catch (A | B e) { e.m(); } } }"),
            "none"
        );
    }

    #[test]
    fn test_lower_instanceof_pattern_types_the_variable() {
        assert_eq!(
            only("class H { void f(Object o) { if (o instanceof A a) { a.m(); } } }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_typed_lambda_parameter_is_known_and_implicit_is_not() {
        assert_eq!(
            receivers("class H { void f() { g((A a) -> a.p()); g(b -> b.q()); } }"),
            vec![pair("p", "known A"), pair("q", "none")]
        );
    }

    #[test]
    fn test_lower_anonymous_class_field_shadows_outer_local() {
        assert_eq!(
            only("class H { void f() { A x = null; new Base() { C x; void g() { x.m(); } }; } }"),
            "none"
        );
    }

    #[test]
    fn test_lower_anonymous_class_method_sees_outer_local() {
        assert_eq!(
            only("class H { void f() { A x = null; new Base() { void g() { x.m(); } }; } }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_empty_method_body_records_nothing() {
        assert!(receivers("class H { void f() { } }").is_empty());
    }

    #[test]
    fn test_lower_two_calls_on_one_line_each_answer() {
        assert_eq!(
            receivers("class H { void f() { A a = null; a.p(); a.q(); } }"),
            vec![pair("p", "known A"), pair("q", "known A")]
        );
    }

    #[test]
    fn test_lower_constructor_parameter_is_known() {
        assert_eq!(only("class H { H(A a) { a.m(); } }"), "known A");
    }

    #[test]
    fn test_lower_switch_rule_pattern_variable_is_scoped_to_its_rule() {
        assert_eq!(
            receivers(
                "class H { void f(Object o) { switch (o) { case A a -> a.p(); case B a -> a.q(); default -> {} } } }"
            ),
            vec![pair("p", "known A"), pair("q", "known B")]
        );
    }
}
