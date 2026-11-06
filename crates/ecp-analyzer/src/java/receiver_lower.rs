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
//! bare-name path for it. Every receiver that names a binding, opaque or
//! not, lands in the lowering's context ([`JavaLowering`]'s `Cx`), so the
//! extractor can tell a local from a field of the enclosing class.
//!
//! Every class member with code lowers: methods, constructors, initializer
//! blocks, field initializers and enum constants, so no local reaches the
//! extractor's field lookup. Java scopes a pattern variable by flow (JLS
//! 6.3.1); the lowering types it only where it is definitely matched, and a
//! receiver that names a pattern variable outside that region lands in the
//! context untyped.

use super::receiver_types::expression_type;
use crate::framework_helpers::node_span;
use crate::receiver::ir::{BindingId, BindingOrigin, ScopeId, ScopeKind, TypeOrigin};
use crate::receiver::{DeclaredTypePolicy, IrBuilder, LangPolicy, Lowered, Lowering, ScopeRule};
use rustc_hash::FxHashSet;
use std::cell::RefCell;
use tree_sitter::Node;

/// Nested blocks, lambdas and local classes beyond this leave the method to
/// the extractor's own path.
const MAX_DEPTH: u32 = 256;

pub(super) struct JavaLowering;

impl Lowering for JavaLowering {
    /// The `Node::id()` of every receiver that names a local, a parameter
    /// or a pattern variable.
    type Cx<'a> = RefCell<FxHashSet<usize>>;

    fn policy(&self) -> LangPolicy {
        LangPolicy {
            declared: DeclaredTypePolicy::Pinned,
            scope: ScopeRule::Block,
        }
    }

    fn is_function(&self, node: Node<'_>) -> bool {
        match node.kind() {
            "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "static_initializer"
            | "field_declaration"
            | "constant_declaration"
            | "enum_constant" => true,
            // An instance initializer.
            "block" => node
                .parent()
                .is_some_and(|p| matches!(p.kind(), "class_body" | "enum_body_declarations")),
            _ => false,
        }
    }

    fn lower(
        &self,
        cx: &RefCell<FxHashSet<usize>>,
        function: Node<'_>,
        source: &[u8],
        b: &mut IrBuilder,
    ) -> Lowered {
        let mut lower = Lower {
            source,
            depth: 0,
            unsupported: None,
            opaque: FxHashSet::default(),
            locals: cx,
            pattern_names: FxHashSet::default(),
            typed_patterns: true,
            hoist: None,
            erroneous: function.has_error(),
        };
        match function.kind() {
            "method_declaration" | "constructor_declaration" => lower.callable(b, function, false),
            "compact_constructor_declaration" => lower.compact_constructor(b, function),
            _ => lower.walk(b, function),
        }
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
            | "while_statement"
            | "do_statement"
            | "for_statement"
            | "expression_statement"
            | "return_statement"
            | "try_with_resources_statement"
            | "catch_clause"
    )
}

/// Whether `node` is a `&&` expression.
fn is_and(node: Node<'_>) -> bool {
    node.kind() == "binary_expression"
        && node
            .child_by_field_name("operator")
            .is_some_and(|op| op.kind() == "&&")
}

/// Whether `node` is the `condition` of a statement or `?:` whose scope
/// holds only its true branch.
fn is_condition_of(node: Node<'_>, parent: Node<'_>) -> bool {
    matches!(
        parent.kind(),
        "if_statement" | "while_statement" | "for_statement" | "ternary_expression"
    ) && parent
        .child_by_field_name("condition")
        .is_some_and(|c| c.id() == node.id())
}

/// Whether the pattern of `instance_of` is definitely matched in the scope
/// the lowering declares it in: the condition's true branch, reached through
/// parentheses and `&&` only, or the right operand of an `&&` (JLS 6.3.1).
fn affirmed(instance_of: Node<'_>) -> bool {
    let mut node = instance_of;
    let mut left_of_and = false;
    while let Some(parent) = node.parent() {
        if is_condition_of(node, parent) {
            return true;
        }
        if is_and(parent) {
            left_of_and |= parent
                .child_by_field_name("left")
                .is_some_and(|l| l.id() == node.id());
        } else if parent.kind() != "parenthesized_expression" {
            break;
        }
        node = parent;
    }
    left_of_and
}

/// Whether the `&&` expression `and` closes the scope of the pattern
/// variables its operands declare: it is the outermost `&&` of its chain,
/// and that chain is no condition whose statement scopes them.
fn owns_and_scope(and: Node<'_>) -> bool {
    let mut node = and;
    while let Some(parent) = node.parent() {
        if is_and(parent) || is_condition_of(node, parent) {
            return false;
        }
        if parent.kind() != "parenthesized_expression" {
            break;
        }
        node = parent;
    }
    true
}

fn has_static_modifier(declaration: Node<'_>) -> bool {
    let mut cursor = declaration.walk();
    let modifiers = declaration
        .named_children(&mut cursor)
        .find(|c| c.kind() == "modifiers");
    modifiers.is_some_and(|m| {
        let mut inner = m.walk();
        let has = m.children(&mut inner).any(|c| c.kind() == "static");
        has
    })
}

struct Lower<'s> {
    source: &'s [u8],
    depth: u32,
    unsupported: Option<&'static str>,
    opaque: FxHashSet<BindingId>,
    locals: &'s RefCell<FxHashSet<usize>>,
    /// Every pattern variable name declared so far: a receiver of that name
    /// that no open scope binds is one the lowering scopes too narrowly.
    pattern_names: FxHashSet<&'s str>,
    /// Whether a type or record pattern being lowered types its variables.
    typed_patterns: bool,
    /// `(group, block)` while a classic switch group's statements lower: a
    /// local declared directly in the group belongs to the switch block
    /// (JLS 6.3: its scope is the rest of the switch block).
    hoist: Option<(ScopeId, ScopeId)>,
    /// A syntax error may hide a declaration, so an unbound receiver is no
    /// proof of a field.
    erroneous: bool,
}

impl<'s> Lower<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or("")
    }

    fn declare(
        &mut self,
        b: &mut IrBuilder,
        name: &'s str,
        class: Option<&str>,
        origin: BindingOrigin,
    ) {
        let declared = class.map(|class| b.intern(class, TypeOrigin::Annotation));
        let scope = match self.hoist {
            Some((group, block)) if b.current_scope() == group => block,
            _ => b.current_scope(),
        };
        if origin == BindingOrigin::Pattern {
            self.pattern_names.insert(name);
        }
        let id = b.declare_in(scope, name, origin, declared);
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

    /// A compact canonical constructor declares the record's components as
    /// its parameters (JLS 8.10.4.2).
    fn compact_constructor(&mut self, b: &mut IrBuilder, function: Node<'_>) {
        if let Some(params) = function
            .parent()
            .and_then(|body| body.parent())
            .and_then(|record| record.child_by_field_name("parameters"))
        {
            self.parameters(b, params);
        }
        if let Some(body) = function.child_by_field_name("body") {
            self.walk(b, body);
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
            "if_statement" | "ternary_expression" => self.conditional(b, node),
            "binary_expression" if is_and(node) && owns_and_scope(node) => {
                b.push_scope(ScopeKind::Block);
                self.walk_children(b, node);
                b.pop_scope();
            }
            "switch_block_statement_group" => self.switch_group(b, node),
            "record_declaration"
            | "enum_declaration"
            | "interface_declaration"
            | "annotation_type_declaration" => self.static_type(b, node),
            "class_declaration" if has_static_modifier(node) => self.static_type(b, node),
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
        let name = self.text(receiver);
        match b.lookup(name) {
            Some(binding) => {
                self.locals.borrow_mut().insert(receiver.id());
                if !self.opaque.contains(&binding) {
                    b.use_binding(binding, receiver.id());
                }
            }
            None if self.erroneous || self.pattern_names.contains(name) => {
                self.locals.borrow_mut().insert(receiver.id());
            }
            None => {}
        }
    }

    /// `if (c) t else e` and `c ? t : e`: a pattern variable `c` declares
    /// is visible in `t` only.
    fn conditional(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        b.push_scope(ScopeKind::Block);
        for field in ["condition", "consequence"] {
            if let Some(child) = node.child_by_field_name(field) {
                self.walk(b, child);
            }
        }
        b.pop_scope();
        if let Some(alternative) = node.child_by_field_name("alternative") {
            b.push_scope(ScopeKind::Block);
            self.walk(b, alternative);
            b.pop_scope();
        }
    }

    /// `case A a: ..`: the labels' pattern variables live in the group, its
    /// directly declared locals in the switch block.
    fn switch_group(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let block = b.current_scope();
        let group = b.push_scope(ScopeKind::Block);
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        for child in children {
            if child.kind() == "switch_label" {
                self.walk(b, child);
            } else {
                let outer = self.hoist.replace((group, block));
                self.walk(b, child);
                self.hoist = outer;
            }
        }
        b.pop_scope();
    }

    /// A record, enum, interface or static class declared in a body is
    /// static (JLS 8.1.3): it sees none of the enclosing locals, so it lowers
    /// in a frame of its own. A record's components are its fields.
    fn static_type(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let outer = self.hoist.take();
        b.closure(node_span(&node), |b| {
            b.push_scope(ScopeKind::Block);
            if node.kind() == "record_declaration" {
                if let Some(params) = node.child_by_field_name("parameters") {
                    self.parameters(b, params);
                }
            }
            if let Some(body) = node.child_by_field_name("body") {
                self.walk(b, body);
            }
            b.pop_scope();
        });
        self.hoist = outer;
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

    /// `left instanceof T name`; a record pattern declares through its
    /// components. A pattern that is not definitely matched where it lands
    /// declares its variables untyped.
    fn instance_of(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        if let Some(left) = node.child_by_field_name("left") {
            self.walk(b, left);
        }
        let affirmed = affirmed(node);
        if let (Some(name), Some(ty)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("right"),
        ) {
            let ty = Some(ty).filter(|_| affirmed);
            self.declare_typed(b, name, ty, false, BindingOrigin::Pattern);
        }
        if let Some(pattern) = node.child_by_field_name("pattern") {
            let outer = std::mem::replace(&mut self.typed_patterns, affirmed);
            self.walk(b, pattern);
            self.typed_patterns = outer;
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
        let ty = Some(ty).filter(|_| self.typed_patterns);
        self.declare_typed(b, name, ty, false, BindingOrigin::Pattern);
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
            &Default::default(),
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
    fn test_lower_static_initializer_local_is_known() {
        assert_eq!(
            only("class R { Config cfg; static { Loader cfg = new Loader(); cfg.load(); } }"),
            "known Loader"
        );
    }

    #[test]
    fn test_lower_instance_initializer_local_is_known() {
        assert_eq!(
            receivers("class R { { X y = null; y.m(); } } enum E { K; { Z z = null; z.n(); } }"),
            vec![pair("m", "known X"), pair("n", "known Z")]
        );
    }

    #[test]
    fn test_lower_field_initializer_lambda_local_is_known() {
        assert_eq!(
            only("class R { A q; Runnable r = () -> { B q = null; q.m(); }; }"),
            "known B"
        );
    }

    #[test]
    fn test_lower_compact_constructor_sees_record_components() {
        assert_eq!(only("record Q(A a) { Q { a.m(); } }"), "known A");
    }

    #[test]
    fn test_lower_negated_instanceof_guard_records_no_use() {
        assert_eq!(
            receivers(
                "class H { void f(Object o) { if (!(o instanceof A a)) { a.p(); return; } a.q(); } }"
            ),
            vec![pair("p", "none"), pair("q", "none")]
        );
    }

    #[test]
    fn test_lower_instanceof_variable_is_absent_from_the_else_branch() {
        assert_eq!(
            receivers(
                "class H { void f(Object o) { if (o instanceof A a) { a.p(); } else { a.q(); } } }"
            ),
            vec![pair("p", "known A"), pair("q", "none")]
        );
    }

    #[test]
    fn test_lower_instanceof_in_conjunction_types_only_the_right_operand() {
        assert_eq!(
            receivers(
                "class H { boolean f(Object o) { g(o instanceof A a && a.p(), a.q()); return !(o instanceof B b) || b.r(); } }"
            ),
            vec![pair("p", "known A"), pair("q", "none"), pair("r", "none")]
        );
    }

    #[test]
    fn test_lower_instanceof_in_ternary_and_while_types_the_true_branch() {
        assert_eq!(
            receivers(
                "class H { void f(Object o) { x(o instanceof A a ? a.p() : a.q()); while (o instanceof B b) { b.r(); } } }"
            ),
            vec![pair("p", "known A"), pair("q", "none"), pair("r", "known B")]
        );
    }

    #[test]
    fn test_lower_record_pattern_under_negation_records_no_use() {
        assert_eq!(
            only("class H { void f(Object o) { if (!(o instanceof P(A a))) { a.m(); } } }"),
            "none"
        );
    }

    #[test]
    fn test_lower_classic_switch_pattern_variable_is_scoped_to_its_group() {
        assert_eq!(
            receivers(
                "class H { void f(Object o) { switch (o) { case A a: a.p(); break; case B b: a.q(); } } }"
            ),
            vec![pair("p", "known A"), pair("q", "none")]
        );
    }

    #[test]
    fn test_lower_classic_switch_group_local_reaches_later_groups() {
        assert_eq!(
            only("class H { void f(int k) { switch (k) { case 1: A x = null; break; case 2: x.m(); } } }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_local_record_does_not_see_method_locals() {
        assert_eq!(
            receivers(
                "class H { void f() { Store s = null; Store t = null; record View(Page s) { void show() { s.render(); t.drop(); } } } }"
            ),
            vec![pair("drop", "none"), pair("render", "known Page")]
        );
    }

    #[test]
    fn test_lower_local_enum_and_interface_do_not_see_method_locals() {
        assert_eq!(
            receivers(
                "class H { void f() { Store s = null; enum E { X; void g() { s.a(); } } interface I { default void h() { s.b(); } } } }"
            ),
            vec![pair("a", "none"), pair("b", "none")]
        );
    }

    #[test]
    fn test_lower_static_member_class_of_anonymous_class_does_not_see_locals() {
        assert_eq!(
            only("class H { void f() { A x = null; new Base() { static class N { void g() { x.m(); } } }; } }"),
            "none"
        );
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
