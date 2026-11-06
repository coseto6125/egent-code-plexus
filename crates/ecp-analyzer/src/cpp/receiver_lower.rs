//! C++ lowering into the receiver core.
//!
//! A receiver occurrence is `x.m()` / `x->m()` (also `(*x).m()`) with `x` a
//! plain identifier that names a local, a parameter or a lambda capture. The
//! declared type is pinned: a `type_identifier` type holds on every path, any
//! other declared type (`auto`, `std::string`, a template) leaves the binding
//! without evidence. The scope is the only flow fact the lowering keeps, so it
//! stays one straight line of statements: a block-scoped `T obj;` binds its own
//! type and a same-named sibling block binds another.

use crate::framework_helpers::node_span;
use crate::receiver::ir::{
    BindingId, BindingOrigin, CaptureMode, ScopeKind, TypeId, TypeOrigin, Value,
};
use crate::receiver::{DeclaredTypePolicy, IrBuilder, LangPolicy, Lowered, Lowering, ScopeRule};
use tree_sitter::Node;

use super::receiver_types::{
    callable_param_identifier, find_function_declarator, inner_declarator,
    unwrap_declarator_identifier,
};

/// Nesting beyond this leaves the function to the extractor's own path.
const MAX_DEPTH: u32 = 256;

pub(super) struct CppLowering;

impl Lowering for CppLowering {
    type Cx<'a> = ();

    fn policy(&self) -> LangPolicy {
        LangPolicy {
            declared: DeclaredTypePolicy::Pinned,
            scope: ScopeRule::Block,
        }
    }

    fn is_function(&self, node: Node<'_>) -> bool {
        node.kind() == "function_definition"
    }

    fn lower(&self, _cx: &(), function: Node<'_>, source: &[u8], b: &mut IrBuilder) -> Lowered {
        if !has_receiver_call(function) {
            return Lowered::NoUses;
        }
        let mut lower = Lower {
            source,
            depth: 0,
            unsupported: None,
        };
        lower.function(b, function);
        match lower.unsupported {
            Some(reason) => Lowered::Unsupported(reason),
            None if b.use_count() == 0 => Lowered::NoUses,
            None => Lowered::Done,
        }
    }
}

/// The identifier of `x.m(..)` / `x->m(..)` / `(*x).m(..)`.
fn call_receiver<'t>(call: Node<'t>) -> Option<Node<'t>> {
    let function = call.child_by_field_name("function")?;
    if function.kind() != "field_expression" {
        return None;
    }
    let receiver = member_operand(function.child_by_field_name("argument")?)?;
    (receiver.kind() == "identifier").then_some(receiver)
}

/// The object a member access reads: `(*p).m()`, `(&x).m()` and `p->m()`
/// all read `p` / `x`, whatever parentheses wrap the dereference.
pub(super) fn member_operand(mut argument: Node<'_>) -> Option<Node<'_>> {
    while argument.kind() == "parenthesized_expression" {
        argument = argument.named_child(0)?;
    }
    match argument.kind() {
        "pointer_expression" => argument.child_by_field_name("argument"),
        _ => Some(argument),
    }
}

fn has_receiver_call(function: Node<'_>) -> bool {
    let mut stack = vec![function];
    while let Some(n) = stack.pop() {
        if n.kind() == "call_expression" && call_receiver(n).is_some() {
            return true;
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    false
}

fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    let children = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() != "comment")
        .collect();
    children
}

fn children_by_field<'t>(node: Node<'t>, field: &str) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    let children = node.children_by_field_name(field, &mut cursor).collect();
    children
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

    /// The declared type of a `type_identifier` type: the only type text the
    /// binder reads. Any other type node declares an untyped binding.
    fn declared_type(&self, b: &mut IrBuilder, ty: Option<Node<'_>>) -> Option<TypeId> {
        let ty = ty.filter(|t| t.kind() == "type_identifier")?;
        Some(b.intern(self.text(ty), TypeOrigin::Annotation))
    }

    fn declare_name(
        &mut self,
        b: &mut IrBuilder,
        name: &str,
        origin: BindingOrigin,
        declared: Option<TypeId>,
    ) {
        let id = b.declare(name, origin, declared);
        b.assign(id, Value::Unknown);
    }

    fn function(&mut self, b: &mut IrBuilder, function: Node<'_>) {
        let declarator = function.child_by_field_name("declarator");
        if let Some(parameters) = declarator
            .and_then(find_function_declarator)
            .and_then(|d| d.child_by_field_name("parameters"))
        {
            self.parameters(b, parameters);
        }
        for child in named_children(function) {
            if Some(child.id()) != declarator.map(|d| d.id()) && child.kind() != "type_identifier" {
                self.walk(b, child);
            }
        }
    }

    fn parameters(&mut self, b: &mut IrBuilder, list: Node<'_>) {
        for param in named_children(list) {
            if !matches!(
                param.kind(),
                "parameter_declaration" | "optional_parameter_declaration"
            ) {
                continue;
            }
            let Some(declarator) = param.child_by_field_name("declarator") else {
                continue;
            };
            // A callable parameter's declared type is its result type, not
            // the callable's.
            if let Some(name) = callable_param_identifier(declarator, self.source) {
                self.declare_name(b, &name, BindingOrigin::Param, None);
            } else if let Some(name) = unwrap_declarator_identifier(declarator, self.source) {
                let declared = self.declared_type(b, param.child_by_field_name("type"));
                self.declare_name(b, &name, BindingOrigin::Param, declared);
            }
        }
    }

    fn walk(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        if self.unsupported.is_some() {
            return;
        }
        if self.depth >= MAX_DEPTH {
            self.unsupported = Some("cpp receiver lowering exceeds the nesting limit");
            return;
        }
        self.depth += 1;
        self.walk_node(b, node);
        self.depth -= 1;
    }

    fn children(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        for child in named_children(node) {
            self.walk(b, child);
        }
    }

    fn scoped(&mut self, b: &mut IrBuilder, kind: ScopeKind, node: Node<'_>) {
        b.push_scope(kind);
        self.children(b, node);
        b.pop_scope();
    }

    fn walk_node(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        match node.kind() {
            // A local class's member bodies keep the extractor's own path.
            "function_definition" => {}
            "compound_statement" | "if_statement" | "while_statement" | "do_statement"
            | "switch_statement" | "for_statement" => {
                let kind = match node.kind() {
                    "for_statement" | "while_statement" | "do_statement" => ScopeKind::Loop,
                    _ => ScopeKind::Block,
                };
                self.scoped(b, kind, node);
            }
            "declaration" => self.declaration(b, node),
            "for_range_loop" => self.range_loop(b, node),
            "catch_clause" => self.catch_clause(b, node),
            "lambda_expression" => {
                b.closure(node_span(&node), |inner| self.lambda(inner, node));
            }
            "call_expression" => {
                if let Some(receiver) = call_receiver(node) {
                    if let Some(binding) = self.resolve(b, self.text(receiver)) {
                        b.use_binding(binding, receiver.id());
                    }
                }
                self.children(b, node);
            }
            _ => self.children(b, node),
        }
    }

    /// Lambda captures resolve lazily at the first use. A capture is `ByValue`
    /// whatever its mode: a declared type is pinned and an undeclared one
    /// never becomes `Known`, so a by-reference write changes no answer.
    fn resolve(&mut self, b: &mut IrBuilder, name: &str) -> Option<BindingId> {
        b.lookup_or_capture(name, CaptureMode::ByValue)
    }

    fn lambda(&mut self, b: &mut IrBuilder, lambda: Node<'_>) {
        if let Some(parameters) = lambda
            .child_by_field_name("declarator")
            .and_then(|d| d.child_by_field_name("parameters"))
        {
            self.parameters(b, parameters);
        }
        if let Some(body) = lambda.child_by_field_name("body") {
            self.walk(b, body);
        }
    }

    fn catch_clause(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        b.push_scope(ScopeKind::Catch);
        if let Some(parameters) = node.child_by_field_name("parameters") {
            self.parameters(b, parameters);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(b, body);
        }
        b.pop_scope();
    }

    fn range_loop(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        b.push_scope(ScopeKind::Loop);
        if let Some(range) = node.child_by_field_name("right") {
            self.walk(b, range);
        }
        let declared = self.declared_type(b, node.child_by_field_name("type"));
        if let Some(declarator) = node.child_by_field_name("declarator") {
            self.declarator(b, declarator, declared, BindingOrigin::ForEach);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(b, body);
        }
        b.pop_scope();
    }

    /// `T a, *b = x;` declares one binding per `declarator` field. A condition
    /// declaration's initializer is a sibling `value`.
    fn declaration(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        if let Some(value) = node.child_by_field_name("value") {
            self.walk(b, value);
        }
        let declared = self.declared_type(b, node.child_by_field_name("type"));
        for declarator in children_by_field(node, "declarator") {
            self.declarator(b, declarator, declared, BindingOrigin::Local);
        }
    }

    fn declarator(
        &mut self,
        b: &mut IrBuilder,
        declarator: Node<'_>,
        declared: Option<TypeId>,
        origin: BindingOrigin,
    ) {
        match declarator.kind() {
            "identifier" => {
                let name = self.text(declarator);
                self.declare_name(b, name, origin, declared);
            }
            "init_declarator" => {
                if let Some(value) = declarator.child_by_field_name("value") {
                    self.walk(b, value);
                }
                if let Some(inner) = inner_declarator(declarator) {
                    self.declarator(b, inner, declared, origin);
                }
            }
            "pointer_declarator" | "reference_declarator" | "array_declarator" => {
                if let Some(inner) = inner_declarator(declarator) {
                    self.declarator(b, inner, declared, origin);
                }
            }
            // `auto [a, b] = pair;` binds names the binder cannot type.
            "structured_binding_declarator" => {
                for name in named_children(declarator) {
                    if name.kind() == "identifier" {
                        self.declare_name(b, self.text(name), origin, None);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receiver::{analyze_file, Receiver};
    use std::path::Path;

    /// `(method, text)` of every `x.method()` / `x->method()` whose `x` is an
    /// identifier, in method-name order; `text` is `known T`, `unknown Cause`
    /// or `none`.
    fn receivers(source: &str) -> Vec<(String, String)> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let bytes = source.as_bytes();
        let found = analyze_file(
            &CppLowering,
            &(),
            tree.root_node(),
            bytes,
            Path::new("t.cpp"),
        );
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if n.kind() == "call_expression" {
                if let Some(object) = call_receiver(n) {
                    let method = n
                        .child_by_field_name("function")
                        .and_then(|f| f.child_by_field_name("field"))
                        .unwrap();
                    let state = match found.receiver_at(object) {
                        Some(Receiver::Known(ty)) => format!("known {}", ty.text),
                        Some(Receiver::Unknown(cause)) => format!("unknown {cause:?}"),
                        None => "none".to_string(),
                    };
                    out.push((method.utf8_text(bytes).unwrap().to_string(), state));
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

    #[test]
    fn test_lower_sibling_blocks_bind_own_types() {
        assert_eq!(
            only("void run() { { A obj; } { B obj; obj.m(); } }"),
            "known B"
        );
    }

    #[test]
    fn test_lower_inner_block_shadow_keeps_each_type() {
        assert_eq!(
            receivers("void run() { A obj; { B obj; obj.inner(); } obj.outer(); }"),
            vec![
                ("inner".to_string(), "known B".to_string()),
                ("outer".to_string(), "known A".to_string())
            ]
        );
    }

    #[test]
    fn test_lower_multi_declarator_binds_each_name() {
        assert_eq!(
            receivers("void run() { A a, *b; a.x(); b->y(); }"),
            vec![
                ("x".to_string(), "known A".to_string()),
                ("y".to_string(), "known A".to_string())
            ]
        );
    }

    #[test]
    fn test_lower_auto_local_has_no_evidence() {
        assert_eq!(
            only("void run() { auto o = make(); o.m(); }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_qualified_type_has_no_evidence() {
        assert_eq!(
            only("void run() { std::string s; s.m(); }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_parameter_binds_declared_type() {
        assert_eq!(only("void run(A* p) { p->m(); }"), "known A");
    }

    #[test]
    fn test_lower_callable_parameter_is_untyped() {
        assert_eq!(
            only("void run(B* (*make)()) { make.m(); }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_field_receiver_is_not_a_binding() {
        assert_eq!(only("void C::run() { sax->m(); }"), "none");
    }

    #[test]
    fn test_lower_deref_receiver_types_as_operand() {
        assert_eq!(only("void run(A* p) { (*p).m(); }"), "known A");
    }

    #[test]
    fn test_lower_lambda_capture_keeps_outer_type() {
        assert_eq!(
            only("void run() { A o; auto f = [&] { o.m(); }; }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_lambda_parameter_shadows_outer_local() {
        assert_eq!(
            only("void run() { A o; auto f = [](B o) { o.m(); }; }"),
            "known B"
        );
    }

    #[test]
    fn test_lower_condition_declaration_does_not_leak() {
        assert_eq!(
            receivers("void run() { if (A* p = q()) { p->a(); } B p; p.b(); }"),
            vec![
                ("a".to_string(), "known A".to_string()),
                ("b".to_string(), "known B".to_string())
            ]
        );
    }

    #[test]
    fn test_lower_range_for_variable_binds_declared_type() {
        assert_eq!(only("void run() { for (A x : xs) { x.m(); } }"), "known A");
    }

    #[test]
    fn test_lower_catch_parameter_scopes_to_handler() {
        assert_eq!(
            only("void run() { try { } catch (E& e) { e.m(); } }"),
            "known E"
        );
    }

    #[test]
    fn test_lower_empty_function_body_records_nothing() {
        assert!(receivers("void run() {}").is_empty());
    }

    #[test]
    fn test_lower_structured_binding_hides_outer_type() {
        assert_eq!(
            only("void run() { A a; auto [a, b] = f(); a.m(); }"),
            "unknown NoEvidence"
        );
    }
}
