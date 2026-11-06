//! C# lowering into the receiver core.
//!
//! A receiver occurrence is an identifier read as the object of a member
//! access, the callee of an invocation or the condition of `?.`, possibly
//! through parentheses. C# pins the declared static type on every path, so a
//! binding needs no control flow: the body lowers as one straight run of
//! statements and the scopes alone decide which declaration a use reaches.
//! A binding whose type is no plain class name (`var x = Make()`, a generic,
//! `dynamic`, an implicit lambda parameter) is declared without a type: its
//! uses answer `Unknown(NoEvidence)`, which the extractor encodes as an
//! untyped member call and which still shadows a field or a type of that name.

use super::receiver_types::{class_type_name, constructed_type};
use crate::receiver::ir::{BindingOrigin, ScopeKind, TypeOrigin};
use crate::receiver::{DeclaredTypePolicy, IrBuilder, LangPolicy, Lowered, Lowering, ScopeRule};
use tree_sitter::Node;

/// Nested blocks and lambdas beyond this leave the method to the
/// extractor's own path.
const MAX_DEPTH: u32 = 256;

pub(super) struct CSharpLowering;

impl Lowering for CSharpLowering {
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
            "method_declaration" | "constructor_declaration" | "local_function_statement"
        )
    }

    fn lower(&self, _cx: &(), function: Node<'_>, source: &[u8], b: &mut IrBuilder) -> Lowered {
        let mut lower = Lower {
            source,
            depth: 0,
            unsupported: None,
        };
        lower.callable(b, function, false);
        match lower.unsupported {
            Some(reason) => Lowered::Unsupported(reason),
            None if b.use_count() == 0 => Lowered::NoUses,
            None => Lowered::Done,
        }
    }
}

/// Kinds that open their own scope and whose children lower in order.
fn scopes_children(kind: &str) -> bool {
    matches!(
        kind,
        "block"
            | "if_statement"
            | "while_statement"
            | "do_statement"
            | "for_statement"
            | "using_statement"
            | "fixed_statement"
            | "lock_statement"
            | "switch_body"
            | "switch_expression_arm"
            | "catch_clause"
    )
}

/// Parents under which an identifier may be the receiver of a member call.
fn reads_as_receiver(parent: &str) -> bool {
    matches!(
        parent,
        "member_access_expression"
            | "invocation_expression"
            | "conditional_access_expression"
            | "parenthesized_expression"
    )
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

    /// Declares `name` as `ty` writes it. A missing or non-plain type leaves it untyped.
    fn declare_typed(
        &mut self,
        b: &mut IrBuilder,
        name: Node<'_>,
        ty: Option<Node<'_>>,
        origin: BindingOrigin,
    ) {
        if !matches!(name.kind(), "identifier" | "implicit_parameter") {
            return;
        }
        let class = ty.and_then(|ty| class_type_name(ty, self.source));
        self.declare(b, self.text(name), class, origin);
    }

    /// Declares every identifier of a tuple or parenthesized designation untyped.
    fn declare_designation(&mut self, b: &mut IrBuilder, node: Node<'_>, origin: BindingOrigin) {
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        for child in children {
            match child.kind() {
                "identifier" => self.declare_typed(b, child, None, origin),
                "tuple_pattern" | "parenthesized_variable_designation" => {
                    self.declare_designation(b, child, origin)
                }
                _ => {}
            }
        }
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
        if matches!(params.kind(), "implicit_parameter" | "identifier") {
            self.declare_typed(b, params, None, BindingOrigin::Param);
            return;
        }
        let mut cursor = params.walk();
        let children: Vec<_> = params.named_children(&mut cursor).collect();
        for param in children {
            match param.kind() {
                "parameter" => {
                    if let Some(name) = param.child_by_field_name("name") {
                        let ty = param.child_by_field_name("type");
                        self.declare_typed(b, name, ty, BindingOrigin::Param);
                    }
                }
                "implicit_parameter" | "identifier" => {
                    self.declare_typed(b, param, None, BindingOrigin::Param)
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
            self.unsupported = Some("c# receiver lowering exceeds the nesting limit");
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
            "method_declaration" | "constructor_declaration" | "local_function_statement" => {
                self.callable(b, node, true)
            }
            "lambda_expression" | "anonymous_method_expression" => self.lambda(b, node),
            "variable_declaration" => self.variable_declaration(b, node),
            "foreach_statement" => self.foreach(b, node),
            "catch_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    let ty = node.child_by_field_name("type");
                    self.declare_typed(b, name, ty, BindingOrigin::CatchParam);
                }
            }
            "declaration_pattern" | "declaration_expression" | "recursive_pattern" => {
                self.pattern_variable(b, node)
            }
            "var_pattern" => self.declare_designation(b, node, BindingOrigin::Pattern),
            "from_clause" => self.query_variable(b, node),
            "let_clause" | "join_clause" => self.clause_variable(b, node),
            _ => return false,
        }
        true
    }

    fn note_use(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        if node.kind() != "identifier"
            || !node.parent().is_some_and(|p| reads_as_receiver(p.kind()))
        {
            return;
        }
        if let Some(binding) = b.lookup(self.text(node)) {
            b.use_binding(binding, node.id());
        }
    }

    fn lambda(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        b.push_scope(ScopeKind::Block);
        if let Some(params) = node.child_by_field_name("parameters") {
            self.parameters(b, params);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(b, body);
        }
        b.pop_scope();
    }

    /// `T a = .., b = ..;`: one declarator per name. `var x = new T()` takes
    /// the constructed type, which `var` leaves out.
    fn variable_declaration(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let ty = node.child_by_field_name("type");
        let mut cursor = node.walk();
        let declarators: Vec<_> = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "variable_declarator")
            .collect();
        for declarator in declarators {
            if let Some(name) = declarator.child_by_field_name("name") {
                let written = ty.filter(|ty| ty.kind() != "implicit_type");
                let spelled = written.or_else(|| constructed_type(declarator));
                self.declare_typed(b, name, spelled, BindingOrigin::Local);
            } else {
                self.declare_designation(b, declarator, BindingOrigin::Local);
            }
            let mut inner = declarator.walk();
            let initializers: Vec<_> = declarator
                .named_children(&mut inner)
                .filter(|c| c.kind() != "tuple_pattern")
                .collect();
            for initializer in initializers {
                self.walk(b, initializer);
            }
        }
    }

    fn foreach(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        if let Some(right) = node.child_by_field_name("right") {
            self.walk(b, right);
        }
        b.push_scope(ScopeKind::Loop);
        if let Some(left) = node.child_by_field_name("left") {
            match left.kind() {
                "identifier" => {
                    let ty = node.child_by_field_name("type");
                    self.declare_typed(b, left, ty, BindingOrigin::ForEach);
                }
                "tuple_pattern" => self.declare_designation(b, left, BindingOrigin::ForEach),
                _ => {}
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(b, body);
        }
        b.pop_scope();
    }

    /// `T name` in a declaration pattern, an `out T name` argument or a
    /// recursive pattern's designation; its subpatterns lower in place.
    fn pattern_variable(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        if let Some(name) = node.child_by_field_name("name") {
            let ty = node.child_by_field_name("type");
            self.declare_typed(b, name, ty, BindingOrigin::Pattern);
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        for child in children {
            if child.kind() == "parenthesized_variable_designation" {
                self.declare_designation(b, child, BindingOrigin::Pattern);
            } else {
                self.walk(b, child);
            }
        }
    }

    /// `from [T] x in e`: `e` lowers before the range variable exists.
    fn query_variable(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let name = node.child_by_field_name("name");
        self.walk_except(b, node, name);
        if let Some(name) = name {
            let ty = node.child_by_field_name("type");
            self.declare_typed(b, name, ty, BindingOrigin::Local);
        }
    }

    /// `let x = e` and `join x in e ..`: the first identifier is an untyped
    /// range variable; the other children are expressions.
    fn clause_variable(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let mut cursor = node.walk();
        let name = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "identifier");
        self.walk_except(b, node, name);
        if let Some(name) = name {
            self.declare_typed(b, name, None, BindingOrigin::Local);
        }
    }

    fn walk_except(&mut self, b: &mut IrBuilder, node: Node<'_>, skipped: Option<Node<'_>>) {
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        for child in children {
            if Some(child.id()) != skipped.map(|s| s.id()) {
                self.walk(b, child);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receiver::{analyze_file, Receiver};
    use std::path::Path;

    /// `(member, text)` of every `x.Member()` whose `x` is an identifier, in
    /// member order; `text` is `known T`, `unknown Cause` or `none`.
    fn receivers(source: &str) -> Vec<(String, String)> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let bytes = source.as_bytes();
        let found = analyze_file(
            &CSharpLowering,
            &(),
            tree.root_node(),
            bytes,
            Path::new("T.cs"),
        );
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if n.kind() == "invocation_expression" {
                let access = n
                    .child_by_field_name("function")
                    .filter(|f| f.kind() == "member_access_expression");
                let site = access.and_then(|a| {
                    Some((
                        a.child_by_field_name("expression")?,
                        a.child_by_field_name("name")?,
                    ))
                });
                if let Some((object, member)) = site.filter(|(o, _)| o.kind() == "identifier") {
                    let state = match found.receiver_at(object) {
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
        assert_eq!(
            only("class H { void F() { A a = new A(); a.M(); } }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_same_name_sibling_blocks_bind_own_types() {
        assert_eq!(
            receivers(
                "class H { void F() { { A x = new A(); x.First(); } { B x = new B(); x.Second(); } } }"
            ),
            vec![pair("First", "known A"), pair("Second", "known B")]
        );
    }

    #[test]
    fn test_lower_block_local_shadows_only_inside_block() {
        assert_eq!(
            receivers("class H { A x; void F() { { B x = new B(); x.Inner(); } x.Outer(); } }"),
            vec![pair("Inner", "known B"), pair("Outer", "none")]
        );
    }

    #[test]
    fn test_lower_untyped_local_answers_no_evidence() {
        assert_eq!(
            only("class H { void F() { var x = Make(); x.M(); } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_var_with_constructor_takes_the_constructed_type() {
        assert_eq!(
            only("class H { void F() { var x = new A(); x.M(); } }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_dynamic_local_answers_no_evidence() {
        assert_eq!(
            only("class H { void F() { dynamic x = 1; x.M(); } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_generic_local_answers_no_evidence() {
        assert_eq!(
            only("class H { void F() { List<A> x = null; x.Add(1); } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_untyped_inner_name_hides_typed_outer_one() {
        assert_eq!(
            only("class H { void F(A x) { { var x = Make(); x.M(); } } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_parameter_is_known() {
        assert_eq!(only("class H { void F(A a) { a.M(); } }"), "known A");
    }

    #[test]
    fn test_lower_foreach_variables_of_one_name_bind_own_types() {
        assert_eq!(
            receivers(
                "class H { void F(List<A> xs, List<B> ys) { foreach (A o in xs) { o.One(); } foreach (B o in ys) { o.Two(); } } }"
            ),
            vec![pair("One", "known A"), pair("Two", "known B")]
        );
    }

    #[test]
    fn test_lower_foreach_var_is_untyped() {
        assert_eq!(
            only("class H { void F(List<A> xs) { foreach (var o in xs) { o.M(); } } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_for_variable_dies_with_the_loop() {
        assert_eq!(
            receivers("class H { C i; void F() { for (A i = null; ; ) { i.One(); } i.Two(); } }"),
            vec![pair("One", "known A"), pair("Two", "none")]
        );
    }

    #[test]
    fn test_lower_multi_declarator_types_each_name() {
        assert_eq!(
            receivers("class H { void F() { A a = null, b = null; a.P(); b.Q(); } }"),
            vec![pair("P", "known A"), pair("Q", "known A")]
        );
    }

    #[test]
    fn test_lower_catch_declaration_types_the_caught_class() {
        assert_eq!(
            only("class H { void F() { try { } catch (IOException e) { e.M(); } } }"),
            "known IOException"
        );
    }

    #[test]
    fn test_lower_is_pattern_variable_is_known() {
        assert_eq!(
            only("class H { void F(object o) { if (o is A a) { a.M(); } } }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_out_variable_is_known() {
        assert_eq!(
            only("class H { void F() { Get(out A a); a.M(); } }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_implicit_lambda_parameter_answers_no_evidence() {
        assert_eq!(
            only("class H { void F() { G(x => x.M()); } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_typed_lambda_parameter_is_known() {
        assert_eq!(
            only("class H { void F() { G((A a) => a.M()); } }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_local_function_sees_the_enclosing_local() {
        assert_eq!(
            only("class H { void F() { A a = null; void L() { a.M(); } } }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_parenthesized_receiver_is_known() {
        let source = "class H { void F() { A a = null; (a).M(); } }";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let found = analyze_file(
            &CSharpLowering,
            &(),
            tree.root_node(),
            source.as_bytes(),
            Path::new("T.cs"),
        );
        let mut stack = vec![tree.root_node()];
        let mut seen = None;
        while let Some(n) = stack.pop() {
            if n.kind() == "identifier"
                && n.utf8_text(source.as_bytes()) == Ok("a")
                && n.parent()
                    .is_some_and(|p| p.kind() == "parenthesized_expression")
            {
                seen = found.receiver_at(n).cloned();
            }
            let mut cursor = n.walk();
            stack.extend(n.named_children(&mut cursor));
        }
        assert!(matches!(seen, Some(Receiver::Known(ty)) if &*ty.text == "A"));
    }

    #[test]
    fn test_lower_empty_method_body_records_nothing() {
        assert!(receivers("class H { void F() { } }").is_empty());
    }

    #[test]
    fn test_lower_two_calls_on_one_line_each_answer() {
        assert_eq!(
            receivers("class H { void F() { A a = null; a.P(); a.Q(); } }"),
            vec![pair("P", "known A"), pair("Q", "known A")]
        );
    }

    #[test]
    fn test_lower_query_range_variable_answers_no_evidence() {
        assert_eq!(
            only("class H { void F(List<A> xs) { var q = from x in xs select x.M(); } }"),
            "unknown NoEvidence"
        );
    }
}
