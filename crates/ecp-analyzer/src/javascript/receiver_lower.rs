//! JavaScript and TypeScript lowering into the receiver core: one
//! control-flow lowering, with the TypeScript evidence behind [`Dialect`].
//!
//! A receiver occurrence is `x.m()` / `x?.m()` with `x` a plain identifier
//! that names a binding of the function (own, or captured from an enclosing
//! one); TypeScript also counts a method value `x.m`. Evidence is `new C(..)`
//! assigned to the binding. TypeScript adds a simple class annotation
//! (`x: T`, which pins the binding), and an assignment of an `any` /
//! `unknown` value makes a pinned binding `NoEvidence`. A binding the paths
//! disagree on or a closure may rewrite stops being `Known`, and the call
//! extractor then keeps its qualified `x.m` name.
//!
//! Scope: `let` / `const` and a catch parameter are block scoped, `var` and
//! function declarations are function scoped (and hoisted), and every
//! closure captures by reference.

use crate::calls::unparenthesized;
use crate::framework_helpers::node_span;
use crate::receiver::ir::{
    BindingId, BindingOrigin, BlockId, CaptureMode, ScopeId, ScopeKind, TypeId, TypeOrigin, Value,
};
use crate::receiver::lattice::UnknownCause;
use crate::receiver::{DeclaredTypePolicy, IrBuilder, LangPolicy, Lowered, Lowering, ScopeRule};
use std::mem;
use tree_sitter::Node;

/// Nesting beyond this leaves the function to the extractor's own path.
const MAX_DEPTH: u32 = 256;

pub(super) struct JsLowering;

/// The grammar a lowering reads. Annotations, typed parameters and method
/// values check this value rather than a field the JavaScript grammar
/// lacks, so a grammar change cannot switch them on for JavaScript.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    JavaScript,
    TypeScript,
}

impl Dialect {
    /// TypeScript's extractor also asks about a method value
    /// (`xs.forEach(o.m)`); JavaScript's asks about calls only.
    fn member_values(self) -> bool {
        self == Dialect::TypeScript
    }

    fn depth_limit(self) -> &'static str {
        match self {
            Dialect::JavaScript => "javascript receiver lowering exceeds the nesting limit",
            Dialect::TypeScript => "typescript receiver lowering exceeds the nesting limit",
        }
    }

    fn labeled_jump(self) -> &'static str {
        match self {
            Dialect::JavaScript => "javascript labeled break or continue",
            Dialect::TypeScript => "typescript labeled break or continue",
        }
    }

    fn with_statement(self) -> &'static str {
        match self {
            Dialect::JavaScript => "javascript with statement",
            Dialect::TypeScript => "typescript with statement",
        }
    }
}

pub(crate) fn is_function_like(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "arrow_function"
            | "method_definition"
    )
}

impl Lowering for JsLowering {
    type Cx<'a> = ();

    fn policy(&self) -> LangPolicy {
        LangPolicy {
            declared: DeclaredTypePolicy::PointOfAssignment,
            scope: ScopeRule::Block,
        }
    }

    fn is_function(&self, node: Node<'_>) -> bool {
        is_function_like(node.kind())
    }

    fn lower(&self, _cx: &(), function: Node<'_>, source: &[u8], b: &mut IrBuilder) -> Lowered {
        lower_function(Dialect::JavaScript, function, source, b)
    }
}

/// [`Lowering::lower`] for both dialects.
pub(crate) fn lower_function(
    dialect: Dialect,
    function: Node<'_>,
    source: &[u8],
    b: &mut IrBuilder,
) -> Lowered {
    if !has_receiver(dialect, function) {
        return Lowered::NoUses;
    }
    let mut lower = Lower {
        dialect,
        source,
        depth: 0,
        unsupported: None,
        ahead: Vec::new(),
        dynamic: Vec::new(),
        inherited_dynamic: Vec::new(),
    };
    lower.callable(b, function);
    match lower.unsupported {
        Some(reason) => Lowered::Unsupported(reason),
        None if b.use_count() == 0 => Lowered::NoUses,
        None => Lowered::Done,
    }
}

/// The identifier of `x.m`, the receiver of a member access.
pub(crate) fn member_receiver<'t>(member: Node<'t>) -> Option<Node<'t>> {
    if member.kind() != "member_expression" {
        return None;
    }
    member
        .child_by_field_name("object")
        .filter(|object| object.kind() == "identifier")
}

/// The identifier of `x.m(..)`, the receiver of a call.
fn call_receiver<'t>(call: Node<'t>) -> Option<Node<'t>> {
    member_receiver(unparenthesized(call.child_by_field_name("function")?))
}

/// The fast path: a function without a receiver occurrence has nothing to
/// answer.
fn has_receiver(dialect: Dialect, function: Node<'_>) -> bool {
    let mut stack = vec![function];
    while let Some(n) = stack.pop() {
        let found = if dialect.member_values() {
            member_receiver(n).is_some()
        } else {
            n.kind() == "call_expression" && call_receiver(n).is_some()
        };
        if found {
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

/// The identifiers a binding pattern declares.
fn pattern_names<'t>(pattern: Node<'t>, out: &mut Vec<Node<'t>>) {
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => out.push(pattern),
        "assignment_pattern" | "object_assignment_pattern" => {
            if let Some(left) = pattern.child_by_field_name("left") {
                pattern_names(left, out);
            }
        }
        "pair_pattern" => {
            if let Some(value) = pattern.child_by_field_name("value") {
                pattern_names(value, out);
            }
        }
        "object_pattern" | "array_pattern" | "rest_pattern" => {
            for child in named_children(pattern) {
                pattern_names(child, out);
            }
        }
        _ => {}
    }
}

/// The class name of a `: T` annotation that is a single `type_identifier`:
/// no generic, union or predefined type.
fn simple_annotation<'s>(annotation: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    let inner = if annotation.kind() == "type_annotation" {
        annotation.named_child(0)?
    } else {
        annotation
    };
    if inner.kind() != "type_identifier" {
        return None;
    }
    let text = inner.utf8_text(source).ok()?;
    (!text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(text)
}

/// What each element of a destructured or iterated `source` holds: dynamic
/// when the source is, else a value the lowering cannot type.
fn element_of(source: Value) -> Value {
    match source {
        Value::Dynamic => Value::Dynamic,
        _ => Value::Unknown,
    }
}

/// Whether a type annotation (`: any`) names `any` or `unknown`.
fn annotates_dynamic(annotation: Option<Node<'_>>, source: &[u8]) -> bool {
    annotation.is_some_and(|annotation| {
        let inner = match annotation.kind() {
            "type_annotation" => annotation.named_child(0),
            _ => Some(annotation),
        };
        inner.is_some_and(|inner| {
            inner.kind() == "predefined_type"
                && matches!(inner.utf8_text(source).unwrap_or(""), "any" | "unknown")
        })
    })
}

/// Whether `node` (an `as` / `<T>` assertion) casts to `any` or `unknown`.
fn casts_to_dynamic(node: Node<'_>, source: &[u8]) -> bool {
    named_children(node).into_iter().any(|child| {
        let text = child.utf8_text(source).unwrap_or("").trim();
        match child.kind() {
            "predefined_type" => matches!(text, "any" | "unknown"),
            "type_arguments" => matches!(text, "<any>" | "<unknown>"),
            _ => false,
        }
    })
}

fn is_hoisted(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration" | "generator_function_declaration"
    )
}

struct Lower<'s> {
    dialect: Dialect,
    source: &'s [u8],
    depth: u32,
    unsupported: Option<&'static str>,
    /// `let` / `const` names a block declared before its statements
    /// lowered, so a closure above the declaration captures the same
    /// binding the declaration later assigns: `(name, scope)`. Per frame:
    /// `ScopeId`s restart in every closure.
    ahead: Vec<(&'s str, ScopeId)>,
    /// Bindings annotated `any` / `unknown`: a read of one is a dynamic
    /// value, which drops the pin of the binding it is written to. Per
    /// frame: `BindingId`s restart in every closure.
    dynamic: Vec<(BindingId, &'s str)>,
    /// Names an enclosing frame binds to a dynamic binding that this frame
    /// does not shadow: a capture of one is dynamic too.
    inherited_dynamic: Vec<&'s str>,
}

impl<'s> Lower<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or("")
    }

    /// The binding `name` names here: own, else captured by reference.
    fn resolve(&mut self, b: &mut IrBuilder, name: &'s str) -> Option<BindingId> {
        if let Some(id) = b.lookup(name) {
            return Some(id);
        }
        let id = b.capture(name, CaptureMode::ByRef)?;
        if self.inherited_dynamic.contains(&name) {
            self.dynamic.push((id, name));
        }
        Some(id)
    }

    fn is_dynamic(&self, id: BindingId) -> bool {
        self.dynamic.iter().any(|&(dynamic, _)| dynamic == id)
    }

    fn use_receiver(&mut self, b: &mut IrBuilder, receiver: Node<'_>) {
        if let Some(binding) = self.resolve(b, self.text(receiver)) {
            b.use_binding(binding, receiver.id());
        }
    }

    fn assign_unknown(&mut self, b: &mut IrBuilder, name: &'s str) {
        self.assign_element(b, name, &Value::Unknown);
    }

    /// Writes `value`, `Unknown` or `Dynamic`, an element of a pattern's
    /// source, to `name`.
    fn assign_element(&mut self, b: &mut IrBuilder, name: &'s str, value: &Value) {
        if let Some(id) = self.resolve(b, name) {
            b.assign(id, value.clone());
        }
    }

    /// The `: T` annotation of a declarator or parameter; none in JavaScript.
    fn annotation<'t>(&self, node: Node<'t>) -> Option<Node<'t>> {
        match self.dialect {
            Dialect::TypeScript => node.child_by_field_name("type"),
            Dialect::JavaScript => None,
        }
    }

    /// Records `name`, just declared with `annotation`, as dynamic when the
    /// annotation is `any` / `unknown`.
    fn note_dynamic(&mut self, b: &IrBuilder, name: Node<'_>, annotation: Option<Node<'_>>) {
        if name.kind() == "identifier" && annotates_dynamic(annotation, self.source) {
            let text = self.text(name);
            match b.lookup(text) {
                Some(id) if !self.is_dynamic(id) => self.dynamic.push((id, text)),
                _ => {}
            }
        }
    }

    fn declared_type(&self, b: &mut IrBuilder, annotation: Option<Node<'_>>) -> Option<TypeId> {
        let text = simple_annotation(annotation?, self.source)?;
        Some(b.intern(text, TypeOrigin::Annotation))
    }

    fn callable(&mut self, b: &mut IrBuilder, function: Node<'_>) {
        if let Some(params) = function.child_by_field_name("parameters") {
            for param in named_children(params) {
                self.parameter(b, param);
            }
        }
        if let Some(param) = function.child_by_field_name("parameter") {
            self.declare_pattern(b, param, BindingOrigin::Param, false, None);
        }
        match function.child_by_field_name("body") {
            Some(body) if body.kind() == "statement_block" => {
                self.declare_vars(b, body);
                self.block_body(b, body, false);
            }
            Some(body) => {
                self.walk(b, body);
            }
            None => {}
        }
    }

    fn parameter(&mut self, b: &mut IrBuilder, param: Node<'_>) {
        let typed = self.dialect == Dialect::TypeScript
            && matches!(param.kind(), "required_parameter" | "optional_parameter");
        if !typed {
            self.declare_pattern(b, param, BindingOrigin::Param, false, None);
            return;
        }
        if let Some(value) = param.child_by_field_name("value") {
            self.walk(b, value);
        }
        let Some(pattern) = param.child_by_field_name("pattern") else {
            return;
        };
        let annotation = self.annotation(param);
        let declared = self.declared_type(b, annotation);
        self.declare_pattern(b, pattern, BindingOrigin::Param, false, declared);
        self.note_dynamic(b, pattern, annotation);
    }

    /// Declares every name `pattern` binds. A default value or a computed key
    /// is an expression, walked in place. `declared` types only a plain
    /// identifier pattern.
    fn declare_pattern(
        &mut self,
        b: &mut IrBuilder,
        pattern: Node<'_>,
        origin: BindingOrigin,
        function_scoped: bool,
        declared: Option<TypeId>,
    ) {
        match pattern.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                let name = self.text(pattern);
                if function_scoped {
                    if b.lookup(name).is_none() {
                        let scope = b.function_scope();
                        b.declare_in(scope, name, origin, declared);
                    }
                } else {
                    let scope = b.current_scope();
                    match self
                        .ahead
                        .iter()
                        .position(|&(n, s)| n == name && s == scope)
                    {
                        Some(index) => {
                            self.ahead.swap_remove(index);
                        }
                        None => {
                            b.declare(name, origin, declared);
                        }
                    }
                }
            }
            "assignment_pattern" | "object_assignment_pattern" => {
                if let Some(right) = pattern.child_by_field_name("right") {
                    self.walk(b, right);
                }
                if let Some(left) = pattern.child_by_field_name("left") {
                    self.declare_pattern(b, left, origin, function_scoped, None);
                }
            }
            "pair_pattern" => {
                if let Some(value) = pattern.child_by_field_name("value") {
                    self.declare_pattern(b, value, origin, function_scoped, None);
                }
            }
            "object_pattern" | "array_pattern" | "rest_pattern" => {
                for child in named_children(pattern) {
                    self.declare_pattern(b, child, origin, function_scoped, None);
                }
            }
            _ => {}
        }
    }

    /// Writes an unknown value to every existing name `pattern` binds.
    fn assign_pattern(&mut self, b: &mut IrBuilder, pattern: Node<'_>) {
        self.assign_pattern_from(b, pattern, &Value::Unknown);
    }

    /// Writes `element`, what each part of a pattern's source holds
    /// (`Dynamic` when the source is), to every existing name `pattern` binds.
    fn assign_pattern_from(&mut self, b: &mut IrBuilder, pattern: Node<'_>, element: &Value) {
        match pattern.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                self.assign_element(b, self.text(pattern), element);
            }
            "assignment_pattern" | "object_assignment_pattern" => {
                if let Some(right) = pattern.child_by_field_name("right") {
                    self.walk(b, right);
                }
                if let Some(left) = pattern.child_by_field_name("left") {
                    self.assign_pattern_from(b, left, element);
                }
            }
            "parenthesized_expression" => {
                if let Some(inner) = pattern.named_child(0) {
                    self.assign_pattern_from(b, inner, element);
                }
            }
            "pair_pattern" => {
                if let Some(value) = pattern.child_by_field_name("value") {
                    self.assign_pattern_from(b, value, element);
                }
            }
            "object_pattern" | "array_pattern" | "rest_pattern" => {
                for child in named_children(pattern) {
                    self.assign_pattern_from(b, child, element);
                }
            }
            _ => {
                self.walk(b, pattern);
            }
        }
    }

    fn walk(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if self.unsupported.is_some() {
            return Value::Unknown;
        }
        if self.depth >= MAX_DEPTH {
            self.unsupported = Some(self.dialect.depth_limit());
            return Value::Unknown;
        }
        self.depth += 1;
        let value = self.walk_node(b, node);
        self.depth -= 1;
        value
    }

    fn children(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        for child in named_children(node) {
            self.walk(b, child);
        }
        Value::Unknown
    }

    /// A function or block body. Function declarations hoist: they are made
    /// first, so a call above them sees the variables as the closure may.
    fn block_body(&mut self, b: &mut IrBuilder, block: Node<'_>, scoped: bool) {
        if scoped {
            b.push_scope(ScopeKind::Block);
        }
        let statements = named_children(block);
        self.declare_ahead(b, &statements);
        for declaration in statements.iter().filter(|s| is_hoisted(s.kind())) {
            self.function_declaration(b, *declaration);
        }
        for statement in statements.iter().filter(|s| !is_hoisted(s.kind())) {
            self.walk(b, *statement);
        }
        if scoped {
            b.pop_scope();
        }
    }

    /// Declares the names the block's own `let` / `const` statements bind
    /// before any statement lowers: the scope of a `let` is its whole block,
    /// so a closure above the declaration reads it, not an outer name.
    fn declare_ahead(&mut self, b: &mut IrBuilder, statements: &[Node<'_>]) {
        for statement in statements {
            if statement.kind() != "lexical_declaration" {
                continue;
            }
            for declarator in named_children(*statement) {
                self.declare_declarator(b, declarator, false);
            }
        }
    }

    /// Declares every `var` of a function body, nested blocks included,
    /// before the body lowers: `var` hoists to the function.
    fn declare_vars(&mut self, b: &mut IrBuilder, body: Node<'_>) {
        let mut stack = vec![body];
        while let Some(node) = stack.pop() {
            match node.kind() {
                "variable_declaration" => {
                    for declarator in named_children(node) {
                        self.declare_declarator(b, declarator, true);
                    }
                }
                "for_in_statement"
                    if children_by_field(node, "kind")
                        .iter()
                        .any(|kind| kind.kind() == "var") =>
                {
                    if let Some(left) = node.child_by_field_name("left") {
                        self.declare_names(b, left, true, None);
                    }
                }
                kind if is_function_like(kind) || kind == "class_static_block" => continue,
                _ => {}
            }
            stack.extend(named_children(node));
        }
    }

    fn declare_declarator(
        &mut self,
        b: &mut IrBuilder,
        declarator: Node<'_>,
        function_scoped: bool,
    ) {
        let Some(name) = declarator.child_by_field_name("name") else {
            return;
        };
        let annotation = self.annotation(declarator);
        let declared = if name.kind() == "identifier" {
            self.declared_type(b, annotation)
        } else {
            None
        };
        self.declare_names(b, name, function_scoped, declared);
        self.note_dynamic(b, name, annotation);
    }

    /// Declares the names `pattern` binds ahead of their declaration; the
    /// declaration then finds them.
    fn declare_names(
        &mut self,
        b: &mut IrBuilder,
        pattern: Node<'_>,
        function_scoped: bool,
        declared: Option<TypeId>,
    ) {
        let mut names = Vec::new();
        pattern_names(pattern, &mut names);
        for identifier in names {
            let name = self.text(identifier);
            if function_scoped {
                if b.lookup(name).is_none() {
                    let scope = b.function_scope();
                    b.declare_in(scope, name, BindingOrigin::Local, declared);
                }
            } else {
                b.declare(name, BindingOrigin::Local, declared);
                self.ahead.push((name, b.current_scope()));
            }
        }
    }

    /// Lowers the function-like `node` as a closure. The per-frame lists
    /// start empty in it; its captures of a dynamic outer binding stay dynamic.
    fn closure(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let inherited: Vec<&'s str> = self
            .dynamic
            .iter()
            .filter(|&&(id, name)| b.lookup(name) == Some(id))
            .map(|&(_, name)| name)
            .chain(
                self.inherited_dynamic
                    .iter()
                    .copied()
                    .filter(|name| b.lookup(name).is_none()),
            )
            .collect();
        let ahead = mem::take(&mut self.ahead);
        let dynamic = mem::take(&mut self.dynamic);
        let outer = mem::replace(&mut self.inherited_dynamic, inherited);
        b.closure(node_span(&node), |inner| self.callable(inner, node));
        self.ahead = ahead;
        self.dynamic = dynamic;
        self.inherited_dynamic = outer;
    }

    fn function_declaration(&mut self, b: &mut IrBuilder, declaration: Node<'_>) {
        if let Some(name) = declaration.child_by_field_name("name") {
            let name = self.text(name);
            let id = match b.lookup(name) {
                Some(id) => id,
                None => {
                    let scope = b.function_scope();
                    b.declare_in(scope, name, BindingOrigin::Local, None)
                }
            };
            b.assign(id, Value::Unknown);
        }
        self.closure(b, declaration);
    }

    fn walk_node(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        match node.kind() {
            "identifier" => match self.resolve(b, self.text(node)) {
                Some(id) if self.is_dynamic(id) => Value::Dynamic,
                Some(id) => Value::Copy(id),
                None => Value::Unknown,
            },
            // `non_null_expression`, `satisfies_expression`, `as_expression`
            // and `type_assertion` are TypeScript's; the JavaScript grammar
            // has none of them.
            "parenthesized_expression" | "non_null_expression" | "satisfies_expression" => node
                .named_child(0)
                .map_or(Value::Unknown, |inner| self.walk(b, inner)),
            "as_expression" | "type_assertion" => {
                self.children(b, node);
                if casts_to_dynamic(node, self.source) {
                    Value::Dynamic
                } else {
                    Value::Unknown
                }
            }
            "member_expression" if self.dialect.member_values() => {
                match member_receiver(node) {
                    Some(object) => self.use_receiver(b, object),
                    None => {
                        self.children(b, node);
                    }
                }
                Value::Unknown
            }
            "new_expression" => {
                self.children(b, node);
                match node.child_by_field_name("constructor") {
                    // A constructor held in a parameter or a local names no class.
                    Some(class)
                        if class.kind() == "identifier" && b.lookup(self.text(class)).is_none() =>
                    {
                        Value::Type(b.intern(self.text(class), TypeOrigin::Construction))
                    }
                    _ => Value::Unknown,
                }
            }
            "statement_block" => {
                self.block_body(b, node, true);
                Value::Unknown
            }
            "lexical_declaration" | "variable_declaration" => {
                let function_scoped = node.kind() == "variable_declaration";
                for declarator in named_children(node) {
                    self.declarator(b, declarator, function_scoped);
                }
                Value::Unknown
            }
            "assignment_expression" => self.assignment(b, node),
            "augmented_assignment_expression" => self.augmented_assignment(b, node),
            "update_expression" => {
                match node.child_by_field_name("argument") {
                    Some(argument) if argument.kind() == "identifier" => {
                        self.assign_unknown(b, self.text(argument));
                    }
                    _ => {
                        self.children(b, node);
                    }
                }
                Value::Unknown
            }
            "call_expression" => self.call(b, node),
            "function_declaration" | "generator_function_declaration" => {
                self.function_declaration(b, node);
                Value::Unknown
            }
            "function_expression"
            | "generator_function"
            | "arrow_function"
            | "method_definition" => {
                self.closure(b, node);
                Value::Unknown
            }
            "binary_expression" => self.binary(b, node),
            "ternary_expression" => self.ternary(b, node),
            "throw_statement" => {
                self.children(b, node);
                b.throw();
                Value::Unknown
            }
            "return_statement" => {
                self.children(b, node);
                b.ret();
                Value::Unknown
            }
            "break_statement" | "continue_statement" => {
                if node.child_by_field_name("label").is_some() {
                    self.unsupported = Some(self.dialect.labeled_jump());
                } else if node.kind() == "break_statement" {
                    b.break_out();
                } else {
                    b.continue_loop();
                }
                Value::Unknown
            }
            "with_statement" => {
                self.unsupported = Some(self.dialect.with_statement());
                Value::Unknown
            }
            "if_statement" => self.if_statement(b, node),
            "while_statement" => self.while_statement(b, node),
            "do_statement" => self.do_statement(b, node),
            "for_statement" => self.for_statement(b, node),
            "for_in_statement" => self.for_in_statement(b, node),
            "switch_statement" => self.switch_statement(b, node),
            "try_statement" => self.try_statement(b, node),
            _ => self.children(b, node),
        }
    }

    fn declarator(&mut self, b: &mut IrBuilder, declarator: Node<'_>, function_scoped: bool) {
        if declarator.kind() != "variable_declarator" {
            return;
        }
        let value = declarator.child_by_field_name("value");
        let evaluated = value.map(|value| self.walk(b, value));
        let Some(name) = declarator.child_by_field_name("name") else {
            return;
        };
        let annotation = self.annotation(declarator);
        let declared = if name.kind() == "identifier" {
            self.declared_type(b, annotation)
        } else {
            None
        };
        self.declare_pattern(b, name, BindingOrigin::Local, function_scoped, declared);
        self.note_dynamic(b, name, annotation);
        match (name.kind(), evaluated) {
            ("identifier", Some(value)) => {
                if let Some(id) = b.lookup(self.text(name)) {
                    b.assign(id, value);
                }
            }
            (_, Some(value)) => self.assign_pattern_from(b, name, &element_of(value)),
            // `let x;` starts undefined on every pass of an enclosing loop;
            // `var x;` keeps its value. A pinned `let x: T;` is `T` throughout.
            ("identifier", None) if !function_scoped => self.assign_unknown(b, self.text(name)),
            _ => {}
        }
    }

    fn assignment(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return self.children(b, node);
        };
        match left.kind() {
            "identifier" => {
                let value = self.walk(b, right);
                match self.resolve(b, self.text(left)) {
                    Some(id) => {
                        b.assign(id, value);
                        Value::Copy(id)
                    }
                    None => Value::Unknown,
                }
            }
            "object_pattern" | "array_pattern" => {
                let source = self.walk(b, right);
                self.assign_pattern_from(b, left, &element_of(source));
                Value::Unknown
            }
            _ => {
                self.walk(b, left);
                self.walk(b, right);
                Value::Unknown
            }
        }
    }

    fn augmented_assignment(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return self.children(b, node);
        };
        if left.kind() != "identifier" {
            self.walk(b, left);
            self.walk(b, right);
            return Value::Unknown;
        }
        let name = self.text(left);
        let conditional = node
            .child_by_field_name("operator")
            .is_some_and(|o| matches!(o.kind(), "||=" | "&&=" | "??="));
        if !conditional {
            self.walk(b, right);
            self.assign_unknown(b, name);
            return Value::Unknown;
        }
        self.maybe(b, |this, b| {
            let value = this.walk(b, right);
            if let Some(id) = this.resolve(b, name) {
                b.assign(id, value);
            }
        });
        Value::Unknown
    }

    /// Runs `body` so that its abrupt exits (`return`, `throw`, `break`,
    /// `continue`) also reach `finally`, when there is one.
    fn guarded(
        &mut self,
        b: &mut IrBuilder,
        finally: Option<BlockId>,
        body: impl FnOnce(&mut Self, &mut IrBuilder),
    ) {
        match finally {
            Some(finally) => {
                b.enter_try(finally);
                body(self, b);
                b.leave_try();
            }
            None => body(self, b),
        }
    }

    /// Runs `body` on a path that may be skipped, then joins.
    fn maybe(&mut self, b: &mut IrBuilder, body: impl FnOnce(&mut Self, &mut IrBuilder)) {
        let run = b.new_block();
        let join = b.new_block();
        b.branch(&[run, join]);
        b.switch_to(run);
        body(self, b);
        b.goto(join);
        b.switch_to(join);
    }

    fn binary(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return self.children(b, node);
        };
        self.walk(b, left);
        let short_circuit = node
            .child_by_field_name("operator")
            .is_some_and(|o| matches!(o.kind(), "&&" | "||" | "??"));
        if short_circuit {
            self.maybe(b, |this, b| {
                this.walk(b, right);
            });
        } else {
            self.walk(b, right);
        }
        Value::Unknown
    }

    fn ternary(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if let Some(condition) = node.child_by_field_name("condition") {
            self.walk(b, condition);
        }
        let join = b.new_block();
        let arms: Vec<_> = ["consequence", "alternative"]
            .into_iter()
            .filter_map(|field| node.child_by_field_name(field))
            .collect();
        let blocks: Vec<_> = arms.iter().map(|_| b.new_block()).collect();
        b.branch(&blocks);
        for (arm, block) in arms.into_iter().zip(blocks) {
            b.switch_to(block);
            self.walk(b, arm);
            b.goto(join);
        }
        b.switch_to(join);
        Value::Unknown
    }

    fn call(&mut self, b: &mut IrBuilder, call: Node<'_>) -> Value {
        // With member values on, the callee walk meets `x.m` itself.
        let receiver = if self.dialect.member_values() {
            None
        } else {
            call_receiver(call)
        };
        match (receiver, call.child_by_field_name("function")) {
            (Some(receiver), _) => self.use_receiver(b, receiver),
            (None, Some(function)) => {
                self.walk(b, function);
                if function.kind() == "identifier" && self.text(function) == "eval" {
                    b.havoc(None, UnknownCause::Unsupported);
                }
            }
            (None, None) => {}
        }
        if let Some(arguments) = call.child_by_field_name("arguments") {
            self.walk(b, arguments);
        }
        Value::Unknown
    }

    fn if_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if let Some(condition) = node.child_by_field_name("condition") {
            self.walk(b, condition);
        }
        let join = b.new_block();
        let then = b.new_block();
        let alternative = node.child_by_field_name("alternative");
        let other = if alternative.is_some() {
            b.new_block()
        } else {
            join
        };
        b.branch(&[then, other]);
        b.switch_to(then);
        if let Some(consequence) = node.child_by_field_name("consequence") {
            self.walk(b, consequence);
        }
        b.goto(join);
        if let Some(alternative) = alternative {
            b.switch_to(other);
            self.children(b, alternative);
            b.goto(join);
        }
        b.switch_to(join);
        Value::Unknown
    }

    /// `head: cond -> body | exit`; the body returns to `latch`.
    fn loop_with(
        &mut self,
        b: &mut IrBuilder,
        condition: Option<Node<'_>>,
        body: Option<Node<'_>>,
        update: Option<Node<'_>>,
    ) {
        let head = b.new_block();
        let body_block = b.new_block();
        let exit = b.new_block();
        let latch = if update.is_some() {
            b.new_block()
        } else {
            head
        };
        b.goto(head);
        b.switch_to(head);
        if let Some(condition) = condition {
            self.walk(b, condition);
        }
        b.branch(&[body_block, exit]);
        b.push_breakable(Some(latch), exit);
        b.switch_to(body_block);
        if let Some(body) = body {
            self.walk(b, body);
        }
        b.goto(latch);
        if let Some(update) = update {
            b.switch_to(latch);
            self.walk(b, update);
            b.goto(head);
        }
        b.pop_breakable();
        b.switch_to(exit);
    }

    fn while_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        self.loop_with(
            b,
            node.child_by_field_name("condition"),
            node.child_by_field_name("body"),
            None,
        );
        Value::Unknown
    }

    fn do_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let body_block = b.new_block();
        let test = b.new_block();
        let exit = b.new_block();
        b.goto(body_block);
        b.switch_to(body_block);
        b.push_breakable(Some(test), exit);
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(b, body);
        }
        b.goto(test);
        b.switch_to(test);
        if let Some(condition) = node.child_by_field_name("condition") {
            self.walk(b, condition);
        }
        b.branch(&[body_block, exit]);
        b.pop_breakable();
        b.switch_to(exit);
        Value::Unknown
    }

    fn for_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        b.push_scope(ScopeKind::Loop);
        if let Some(initializer) = node.child_by_field_name("initializer") {
            self.walk(b, initializer);
        }
        self.loop_with(
            b,
            node.child_by_field_name("condition"),
            node.child_by_field_name("body"),
            node.child_by_field_name("increment"),
        );
        b.pop_scope();
        Value::Unknown
    }

    fn for_in_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let element = node
            .child_by_field_name("right")
            .map_or(Value::Unknown, |right| element_of(self.walk(b, right)));
        let declares = node.child_by_field_name("kind").is_some();
        b.push_scope(ScopeKind::Loop);
        let head = b.new_block();
        let body_block = b.new_block();
        let exit = b.new_block();
        b.goto(head);
        b.switch_to(head);
        b.branch(&[body_block, exit]);
        b.push_breakable(Some(head), exit);
        b.switch_to(body_block);
        if let Some(left) = node.child_by_field_name("left") {
            if declares {
                let function_scoped = children_by_field(node, "kind")
                    .iter()
                    .any(|kind| kind.kind() == "var");
                self.declare_pattern(b, left, BindingOrigin::ForEach, function_scoped, None);
            }
            self.assign_pattern_from(b, left, &element);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(b, body);
        }
        b.goto(head);
        b.pop_breakable();
        b.switch_to(exit);
        b.pop_scope();
        Value::Unknown
    }

    fn switch_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if let Some(value) = node.child_by_field_name("value") {
            self.walk(b, value);
        }
        let cases = node
            .child_by_field_name("body")
            .map(named_children)
            .unwrap_or_default();
        let exit = b.new_block();
        let blocks: Vec<_> = cases.iter().map(|_| b.new_block()).collect();
        let has_default = cases.iter().any(|c| c.kind() == "switch_default");
        let mut targets = blocks.clone();
        if !has_default {
            targets.push(exit);
        }
        b.branch(&targets);
        // The case clauses share one block scope.
        b.push_scope(ScopeKind::Block);
        let statements: Vec<_> = cases
            .iter()
            .flat_map(|case| children_by_field(*case, "body"))
            .collect();
        self.declare_ahead(b, &statements);
        b.push_breakable(None, exit);
        for (index, case) in cases.into_iter().enumerate() {
            b.switch_to(blocks[index]);
            self.children(b, case);
            b.goto(blocks.get(index + 1).copied().unwrap_or(exit));
        }
        b.pop_breakable();
        b.pop_scope();
        b.switch_to(exit);
        Value::Unknown
    }

    fn try_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let catch = node.child_by_field_name("handler");
        let finally = node.child_by_field_name("finalizer");
        let exit = b.new_block();
        let finally_block = finally.map(|_| b.new_block());
        let join = finally_block.unwrap_or(exit);
        let handler = b.new_block();
        let catch_block = catch.map(|_| b.new_block());

        b.enter_try(handler);
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(b, body);
        }
        b.leave_try();
        b.goto(join);

        // An exception no `catch` takes still runs the `finally`.
        let targets: Vec<_> = catch_block.into_iter().chain(finally_block).collect();
        b.switch_to(handler);
        if targets.is_empty() {
            b.throw();
        } else {
            b.branch(&targets);
        }
        if let (Some(catch), Some(block)) = (catch, catch_block) {
            b.switch_to(block);
            b.push_scope(ScopeKind::Catch);
            if let Some(parameter) = catch.child_by_field_name("parameter") {
                self.declare_pattern(b, parameter, BindingOrigin::CatchParam, false, None);
                self.assign_pattern(b, parameter);
            }
            if let Some(body) = catch.child_by_field_name("body") {
                self.guarded(b, finally_block, |this, b| {
                    this.walk(b, body);
                });
            }
            b.pop_scope();
            b.goto(join);
        }
        if let (Some(finally), Some(block)) = (finally, finally_block) {
            b.switch_to(block);
            self.children(b, finally);
            b.goto(exit);
        }
        b.switch_to(exit);
        Value::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receiver::{analyze_file, Receiver};
    use std::path::Path;

    /// `(method, text)` of every `x.method()` whose `x` is an identifier, in
    /// method-name order; `text` is `known T`, `unknown Cause` or `none`.
    fn receivers(source: &str) -> Vec<(String, String)> {
        receivers_in(tree_sitter_javascript::LANGUAGE.into(), source)
    }

    fn parse(language: tree_sitter::Language, source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        parser.parse(source, None).unwrap()
    }

    /// [`receivers`] of `source` parsed with `language`, lowered as
    /// JavaScript.
    fn receivers_in(language: tree_sitter::Language, source: &str) -> Vec<(String, String)> {
        let tree = parse(language, source);
        let bytes = source.as_bytes();
        let found = analyze_file(&JsLowering, &(), tree.root_node(), bytes, Path::new("t.js"));
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if n.kind() == "call_expression" {
                if let Some(object) = call_receiver(n) {
                    let method = unparenthesized(n.child_by_field_name("function").unwrap())
                        .child_by_field_name("property")
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
    fn test_lower_constructor_assignment_is_known() {
        assert_eq!(
            only("function f() { const o = new A(); o.m(); }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_branches_with_two_classes_conflict() {
        assert_eq!(
            only("function f(c) { let o; if (c) { o = new A(); } else { o = new B(); } o.m(); }"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_lower_parameter_has_no_evidence() {
        assert_eq!(only("function f(o) { o.m(); }"), "unknown NoEvidence");
    }

    #[test]
    fn test_lower_global_receiver_is_not_a_binding() {
        assert_eq!(only("function f() { console.log(1); }"), "none");
    }

    #[test]
    fn test_lower_closure_write_poisons_the_outer_binding() {
        assert_eq!(
            only("function f() { let o = new A(); const g = () => { o = new B(); }; o.m(); }"),
            "unknown Escape"
        );
    }

    #[test]
    fn test_lower_read_only_closure_keeps_the_type() {
        assert_eq!(
            only("function f() { const o = new A(); const g = () => o.m(); }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_closure_sees_later_outer_write() {
        assert_eq!(
            only("function f() { let o = new A(); const g = () => o.m(); o = new B(); }"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_lower_block_scoped_shadow_keeps_each_type() {
        let found = receivers(
            "function f() { const o = new A(); { const o = new B(); o.inner(); } o.outer(); }",
        );
        assert_eq!(
            found,
            vec![
                ("inner".to_string(), "known B".to_string()),
                ("outer".to_string(), "known A".to_string())
            ]
        );
    }

    #[test]
    fn test_lower_loop_write_reaches_earlier_use() {
        assert_eq!(
            only("function f(xs) { let o = new A(); for (const x of xs) { o.m(); o = x; } }"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_lower_empty_function_body_records_nothing() {
        assert!(receivers("function f() {}").is_empty());
    }

    #[test]
    fn test_lower_labeled_break_leaves_function_to_extractor() {
        assert_eq!(
            only("function f(xs) { const o = new A(); outer: for (const x of xs) { break outer; } o.m(); }"),
            "none"
        );
    }

    #[test]
    fn test_lower_hoisted_function_declaration_sees_both_writes() {
        assert_eq!(
            only("function f() { let o = new A(); g(); o = new B(); function g() { o.m(); } }"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_lower_destructuring_rebinds_receiver() {
        assert_eq!(
            only("function f(p) { let o = new A(); ({ o } = p); o.m(); }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_optional_chain_call_is_a_receiver() {
        assert_eq!(
            only("function f() { const o = new A(); o?.m(); }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_parenthesized_for_of_target_rebinds_receiver() {
        assert_eq!(
            only("function f(xs) { let o = new A(); for ((o) of xs) { o.m(); } }"),
            "unknown NoEvidence"
        );
    }

    /// Stands for: the JavaScript dialect reads no annotation even when the
    /// tree carries one, so the guard is the dialect, not the grammar.
    #[test]
    fn test_lower_javascript_dialect_ignores_type_annotation() {
        let found = receivers_in(
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "function f() { const o: A = make(); o.m(); }",
        );
        assert_eq!(
            found,
            vec![("m".to_string(), "unknown NoEvidence".to_string())]
        );
    }

    #[test]
    fn test_lower_method_value_is_not_a_receiver() {
        let source = "function f(xs) { const o = new A(); xs.forEach(o.m); }";
        let tree = parse(tree_sitter_javascript::LANGUAGE.into(), source);
        let found = analyze_file(
            &JsLowering,
            &(),
            tree.root_node(),
            source.as_bytes(),
            Path::new("t.js"),
        );
        let mut stack = vec![tree.root_node()];
        let mut value = None;
        while let Some(n) = stack.pop() {
            if n.kind() == "arguments" {
                value = value.or_else(|| n.named_child(0).and_then(member_receiver));
            }
            let mut cursor = n.walk();
            stack.extend(n.named_children(&mut cursor));
        }
        assert!(found.receiver_at(value.unwrap()).is_none());
    }

    /// The TypeScript dialect under `TsLowering`'s policy
    /// (`typescript/receiver_lower.rs`), which this module cannot name.
    struct TsDialect;

    impl Lowering for TsDialect {
        type Cx<'a> = ();

        fn policy(&self) -> LangPolicy {
            LangPolicy {
                declared: DeclaredTypePolicy::PinnedUnlessDynamic,
                scope: ScopeRule::Block,
            }
        }

        fn is_function(&self, node: Node<'_>) -> bool {
            is_function_like(node.kind())
        }

        fn lower(&self, _cx: &(), function: Node<'_>, source: &[u8], b: &mut IrBuilder) -> Lowered {
            lower_function(Dialect::TypeScript, function, source, b)
        }
    }

    /// [`receivers`] of TypeScript `source`, lowered in the TypeScript dialect.
    fn ts_receivers(source: &str) -> Vec<(String, String)> {
        let tree = parse(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), source);
        let bytes = source.as_bytes();
        let found = analyze_file(&TsDialect, &(), tree.root_node(), bytes, Path::new("t.ts"));
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if n.kind() == "call_expression" {
                if let Some(object) = call_receiver(n) {
                    let method = unparenthesized(n.child_by_field_name("function").unwrap())
                        .child_by_field_name("property")
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

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(method, state)| (method.to_string(), state.to_string()))
            .collect()
    }

    #[test]
    fn test_switch_statement_case_let_does_not_shadow_after_switch() {
        let found = receivers(
            "function f(k) { let o = new A(); switch (k) { case 1: prep(); default: let o = new B(); o.init(); } o.m(); }",
        );
        assert_eq!(found, pairs(&[("init", "known B"), ("m", "known A")]));
    }

    #[test]
    fn test_switch_statement_without_default_keeps_outer_binding() {
        let found = receivers(
            "function f(k) { let o = new A(); switch (k) { case 1: let o = new B(); o.init(); break; } o.m(); }",
        );
        assert_eq!(found, pairs(&[("init", "known B"), ("m", "known A")]));
    }

    #[test]
    fn test_walk_node_new_of_parameter_constructor_is_untyped() {
        assert_eq!(
            only(
                "function mount(Component, props) { const c = new Component(props); c.render(); }"
            ),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_walk_node_new_of_captured_local_constructor_is_untyped() {
        assert_eq!(
            only("function f(make) { const Klass = make(); const g = () => { const c = new Klass(); c.run(); }; }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_walk_node_new_of_unbound_class_name_stays_known() {
        assert_eq!(
            only("function f(Other) { const c = new Component(); c.render(); }"),
            "known Component"
        );
    }

    #[test]
    fn test_block_body_closure_above_let_reads_the_inner_binding() {
        assert_eq!(
            only("function outer() { let o = new A(); const inner = function () { const g = () => o.m(); let o = new B(); g(); }; }"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_callable_nested_var_is_hoisted_above_an_earlier_closure() {
        assert_eq!(
            only("function outer() { let o = new A(); const inner = function (c) { const g = () => o.m(); if (c) { var o = new B(); } g(); }; }"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_callable_var_redeclaring_a_parameter_keeps_one_binding() {
        assert_eq!(only("function f(o) { var o = new A(); o.m(); }"), "known A");
    }

    #[test]
    fn test_try_statement_catch_rethrow_reaches_finally() {
        assert_eq!(
            only("function f() { let o = new A(); try { risky(); } catch (e) { o = new B(); throw e; } finally { o.m(); } }"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_try_statement_catch_return_reaches_finally() {
        assert_eq!(
            only("function f() { let o = new A(); try { risky(); } catch (e) { o = new B(); return; } finally { o.m(); } }"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_try_statement_catch_break_reaches_finally() {
        assert_eq!(
            only("function f(xs) { let o = new A(); for (const x of xs) { try { risky(); } catch (e) { o = new B(); break; } finally { o.m(); } } }"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_try_statement_catch_without_finally_keeps_its_exit() {
        assert_eq!(
            only("function f() { let o = new A(); try { risky(); } catch (e) { o = new B(); return; } o.m(); }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_function_hoisted_function_local_does_not_steal_outer_let() {
        let found = ts_receivers(
            "function main() { start(); const server: Server = make(); server.listen(); function start() { let server = alt(); server.boot(); } }",
        );
        assert_eq!(
            found,
            pairs(&[("boot", "unknown NoEvidence"), ("listen", "known Server")])
        );
    }

    #[test]
    fn test_lower_function_hoisted_function_parameter_keeps_its_annotation() {
        let found = ts_receivers(
            "function main() { start(); const server = new Server(); server.listen(); function start(server: DevServer) { server.boot(); } }",
        );
        assert_eq!(
            found,
            pairs(&[("boot", "known DevServer"), ("listen", "known Server")])
        );
    }

    #[test]
    fn test_lower_function_any_callback_parameter_does_not_mark_outer_binding() {
        let found = ts_receivers(
            "function f(svc: Service) { items.forEach((e: any) => log(e)); const s = svc; s.run(); }",
        );
        assert_eq!(
            found,
            pairs(&[("forEach", "none"), ("run", "known Service")])
        );
    }

    #[test]
    fn test_lower_function_captured_any_binding_drops_the_pin() {
        let found =
            ts_receivers("function f(raw: any) { const g = (o: A) => { o = raw; o.m(); }; }");
        assert_eq!(found, pairs(&[("m", "unknown NoEvidence")]));
    }

    #[test]
    fn test_lower_function_shadowed_any_binding_is_not_dynamic_in_closure() {
        let found = ts_receivers(
            "function f(raw: any) { const g = (o: A, raw: B) => { o = raw; o.m(); }; }",
        );
        assert_eq!(found, pairs(&[("m", "known A")]));
    }
}
