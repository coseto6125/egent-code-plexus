//! Swift lowering into the receiver core.
//!
//! A receiver occurrence is `x.m` / `x?.m` with `x` a plain identifier that
//! names a binding of the function (own, or captured from an enclosing one),
//! as a callee or as a method value. The only evidence is the declared type
//! of a parameter or `let` / `var` (`x: T`, a single `type_identifier`), which
//! pins the binding on every path; an unannotated binding has no evidence
//! unless it copies a pinned one.
//!
//! Scope: every `statements` body is a block, so a `let` in a `do { .. }` or
//! an `if` branch is private to it. `if let` / `while let` / `for` / `catch` /
//! `case let` bindings live in their statement; a `guard let` binds in the
//! enclosing block after the `else` body. A nested function or closure
//! captures by reference. A nested type declaration is opaque.

use crate::framework_helpers::node_span;
use crate::receiver::ir::{
    BindingId, BindingOrigin, CaptureMode, ScopeKind, TypeId, TypeOrigin, Value,
};
use crate::receiver::{DeclaredTypePolicy, IrBuilder, LangPolicy, Lowered, Lowering, ScopeRule};
use tree_sitter::Node;

/// Nesting beyond this leaves the function to the extractor's own path.
const MAX_DEPTH: u32 = 256;

pub(super) struct SwiftLowering;

impl Lowering for SwiftLowering {
    type Cx<'a> = ();

    fn policy(&self) -> LangPolicy {
        LangPolicy {
            declared: DeclaredTypePolicy::Pinned,
            scope: ScopeRule::Block,
        }
    }

    fn is_function(&self, node: Node<'_>) -> bool {
        matches!(node.kind(), "function_declaration" | "init_declaration")
    }

    fn lower(&self, _cx: &(), function: Node<'_>, source: &[u8], b: &mut IrBuilder) -> Lowered {
        if !has_receiver_member(function) {
            return Lowered::NoUses;
        }
        let mut lower = Lower {
            source,
            depth: 0,
            unsupported: None,
        };
        lower.callable(b, function);
        match lower.unsupported {
            Some(reason) => Lowered::Unsupported(reason),
            None if b.use_count() == 0 => Lowered::NoUses,
            None => Lowered::Done,
        }
    }
}

/// The identifier of `x.m`, the receiver of a navigation.
pub(super) fn navigation_receiver<'t>(navigation: Node<'t>) -> Option<Node<'t>> {
    if navigation.kind() != "navigation_expression" {
        return None;
    }
    navigation
        .child_by_field_name("target")
        .filter(|target| target.kind() == "simple_identifier")
}

/// The fast path: a function without a `x.m` navigation has nothing to answer.
fn has_receiver_member(function: Node<'_>) -> bool {
    let mut stack = vec![function];
    while let Some(n) = stack.pop() {
        if navigation_receiver(n).is_some() {
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
        .filter(|c| !c.kind().ends_with("comment"))
        .collect();
    children
}

fn children_by_field<'t>(node: Node<'t>, field: &str) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    let children = node.children_by_field_name(field, &mut cursor).collect();
    children
}

/// The class a type node names, when it is a single `type_identifier`
/// (generic arguments and a qualified path are not looked at).
fn simple_type<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    let ident = match node.kind() {
        "user_type" => node
            .named_child(0)
            .filter(|c| c.kind() == "type_identifier")?,
        "type_identifier" => node,
        _ => return None,
    };
    let text = ident.utf8_text(source).ok()?;
    (!text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(text)
}

/// The names a `pattern` binds: its `bound_identifier` and the plain
/// identifiers of a tuple pattern. A `case .a(let w)` also yields the case
/// name `a`; declaring it is harmless, as nothing reads it as a receiver.
fn pattern_names<'t>(pattern: Node<'t>, out: &mut Vec<Node<'t>>) {
    let mut cursor = pattern.walk();
    for child in pattern.named_children(&mut cursor) {
        match child.kind() {
            "simple_identifier" => out.push(child),
            "pattern" => pattern_names(child, out),
            _ => {}
        }
    }
}

struct Lower<'s> {
    source: &'s [u8],
    depth: u32,
    unsupported: Option<&'static str>,
}

/// A name a condition clause binds, declared once its initializer ran.
type Pending<'s> = Vec<&'s str>;

impl<'s> Lower<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or("")
    }

    fn resolve(&mut self, b: &mut IrBuilder, name: &str) -> Option<BindingId> {
        b.lookup_or_capture(name, CaptureMode::ByRef)
    }

    fn declared_type(&self, b: &mut IrBuilder, annotation: Option<Node<'_>>) -> Option<TypeId> {
        let annotation = annotation?;
        let ty = named_children(annotation)
            .into_iter()
            .find_map(|c| simple_type(c, self.source))?;
        Some(b.intern(ty, TypeOrigin::Annotation))
    }

    fn declare_local(
        &mut self,
        b: &mut IrBuilder,
        name: &str,
        origin: BindingOrigin,
        declared: Option<TypeId>,
        value: Value,
    ) {
        let id = b.declare(name, origin, declared);
        b.assign(id, value);
    }

    fn callable(&mut self, b: &mut IrBuilder, function: Node<'_>) {
        for param in children_by_field_kind(function, "parameter") {
            self.parameter(b, param);
        }
        if let Some(body) = function.child_by_field_name("body") {
            self.walk(b, body);
        }
    }

    /// `parameter`: the first `name` is the internal name, the second the type.
    fn parameter(&mut self, b: &mut IrBuilder, param: Node<'_>) {
        let names = children_by_field(param, "name");
        let Some(identifier) = names.iter().find(|n| n.kind() == "simple_identifier") else {
            return;
        };
        let ty = names.iter().find_map(|n| simple_type(*n, self.source));
        let declared = ty.map(|ty| b.intern(ty, TypeOrigin::Annotation));
        b.declare(self.text(*identifier), BindingOrigin::Param, declared);
    }

    fn walk(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if self.unsupported.is_some() {
            return Value::Unknown;
        }
        if self.depth >= MAX_DEPTH {
            self.unsupported = Some("swift receiver lowering exceeds the nesting limit");
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

    fn walk_node(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        match node.kind() {
            "simple_identifier" => match self.resolve(b, self.text(node)) {
                Some(id) => Value::Copy(id),
                None => Value::Unknown,
            },
            "navigation_expression" => {
                match navigation_receiver(node) {
                    Some(target) => {
                        if let Some(binding) = self.resolve(b, self.text(target)) {
                            b.use_binding(binding, target.id());
                        }
                    }
                    None => {
                        self.children(b, node);
                    }
                }
                Value::Unknown
            }
            "statements" => {
                b.push_scope(ScopeKind::Block);
                self.children(b, node);
                b.pop_scope();
                Value::Unknown
            }
            "property_declaration" => {
                self.property_declaration(b, node);
                Value::Unknown
            }
            "assignment" => {
                self.assignment(b, node);
                Value::Unknown
            }
            "lambda_literal" => {
                self.closure(b, node);
                Value::Unknown
            }
            "function_declaration" | "init_declaration" => {
                b.closure(node_span(&node), |inner| self.callable(inner, node));
                Value::Unknown
            }
            "class_declaration" | "protocol_declaration" => Value::Unknown,
            "if_statement" => self.if_statement(b, node),
            "guard_statement" => self.guard_statement(b, node),
            "while_statement" => self.while_statement(b, node),
            "repeat_while_statement" => self.repeat_while_statement(b, node),
            "for_statement" => self.for_statement(b, node),
            "switch_statement" => self.switch_statement(b, node),
            "do_statement" => self.do_statement(b, node),
            "control_transfer_statement" => self.control_transfer(b, node),
            "ternary_expression" => self.ternary(b, node),
            "conjunction_expression" | "disjunction_expression" => {
                self.short_circuit(b, node, "lhs", "rhs")
            }
            "nil_coalescing_expression" => self.short_circuit(b, node, "value", "if_nil"),
            _ => self.children(b, node),
        }
    }

    fn property_declaration(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        // `let a: A = x, b: B = y` lists its bindings as sibling fields.
        let mut groups: Vec<(Node<'_>, Option<Node<'_>>, Option<Node<'_>>)> = Vec::new();
        let mut cursor = node.walk();
        for (index, child) in node.children(&mut cursor).enumerate() {
            match (node.field_name_for_child(index as u32), child.kind()) {
                (Some("name"), "pattern") => groups.push((child, None, None)),
                (_, "type_annotation") => {
                    if let Some(group) = groups.last_mut() {
                        group.1 = Some(child);
                    }
                }
                (Some("value"), _) => {
                    if let Some(group) = groups.last_mut() {
                        group.2 = Some(child);
                    }
                }
                (Some("computed_value"), _) => {
                    self.children(b, child);
                }
                _ => {}
            }
        }
        for (pattern, annotation, value) in groups {
            let evaluated = value.map(|v| self.walk(b, v));
            let mut names = Vec::new();
            pattern_names(pattern, &mut names);
            match names[..] {
                [identifier] => {
                    let declared = self.declared_type(b, annotation);
                    let value = evaluated.unwrap_or(Value::Unknown);
                    self.declare_local(
                        b,
                        self.text(identifier),
                        BindingOrigin::Local,
                        declared,
                        value,
                    );
                }
                _ => {
                    for identifier in names {
                        self.declare_local(
                            b,
                            self.text(identifier),
                            BindingOrigin::Local,
                            None,
                            Value::Unknown,
                        );
                    }
                }
            }
        }
    }

    fn assignment(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let (Some(target), Some(result)) = (
            node.child_by_field_name("target"),
            node.child_by_field_name("result"),
        ) else {
            self.children(b, node);
            return;
        };
        let plain = node
            .child_by_field_name("operator")
            .is_some_and(|o| o.kind() == "=");
        let value = self.walk(b, result);
        let identifier = named_children(target)
            .into_iter()
            .find(|c| c.kind() == "simple_identifier")
            .filter(|_| target.named_child_count() == 1);
        match identifier {
            Some(identifier) => {
                if let Some(id) = self.resolve(b, self.text(identifier)) {
                    b.assign(id, if plain { value } else { Value::Unknown });
                }
            }
            None => {
                self.walk(b, target);
            }
        }
    }

    /// A closure literal: parameters, then capture-list items, then the body.
    fn closure(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        b.closure(node_span(&node), |inner| {
            let mut cursor = node.walk();
            let parts: Vec<_> = node.named_children(&mut cursor).collect();
            if let Some(list) = parts.iter().find(|p| p.kind() == "capture_list") {
                for item in named_children(*list) {
                    self.capture_item(inner, item);
                }
            }
            for part in &parts {
                if part.kind() != "lambda_function_type" {
                    continue;
                }
                for params in named_children(*part) {
                    for param in named_children(params) {
                        self.closure_parameter(inner, param);
                    }
                }
            }
            for part in parts.iter().filter(|p| p.kind() == "statements") {
                self.walk(inner, *part);
            }
        });
    }

    fn capture_item(&mut self, b: &mut IrBuilder, item: Node<'_>) {
        let Some(name) = children_by_field(item, "name")
            .into_iter()
            .find(|n| n.kind() == "simple_identifier")
        else {
            return;
        };
        let name = self.text(name);
        match item.child_by_field_name("value") {
            Some(value) => {
                let value = self.walk(b, value);
                self.declare_local(b, name, BindingOrigin::Local, None, value);
            }
            None => {
                b.capture(name, CaptureMode::ByValue);
            }
        }
    }

    fn closure_parameter(&mut self, b: &mut IrBuilder, param: Node<'_>) {
        if param.kind() != "lambda_parameter" {
            return;
        }
        let names = children_by_field(param, "name");
        let Some(identifier) = names.iter().find(|n| n.kind() == "simple_identifier") else {
            return;
        };
        let ty = names.iter().find_map(|n| simple_type(*n, self.source));
        let declared = ty.map(|ty| b.intern(ty, TypeOrigin::Annotation));
        b.declare(self.text(*identifier), BindingOrigin::Param, declared);
    }

    /// Walks the `condition` clauses of an `if` / `guard` / `while`. A
    /// `let x = e` clause declares `x` after `e` ran, so `e` still reads the
    /// outer `x`. With `defer`, the names wait in `pending` (a `guard` binds
    /// after its `else` body); otherwise they are declared at once.
    fn conditions<'t>(
        &mut self,
        b: &mut IrBuilder,
        node: Node<'t>,
        defer: bool,
        pending: &mut Pending<'s>,
    ) {
        let mut names: Pending<'s> = Vec::new();
        let mut after_equals = false;
        let mut cursor = node.walk();
        for (index, child) in node.children(&mut cursor).enumerate() {
            if node.field_name_for_child(index as u32) != Some("condition") {
                if child.kind() == "simple_identifier"
                    && node.field_name_for_child(index as u32) == Some("bound_identifier")
                {
                    names.push(self.text(child));
                }
                continue;
            }
            match child.kind() {
                "=" => after_equals = true,
                "pattern" => {
                    let mut found = Vec::new();
                    pattern_names(child, &mut found);
                    names.extend(found.into_iter().map(|n| self.text(n)));
                }
                _ if !child.is_named() => {}
                "value_binding_pattern" => {}
                _ => {
                    self.walk(b, child);
                    if after_equals {
                        self.bind_clause(b, &mut names, defer, pending);
                        after_equals = false;
                    }
                }
            }
        }
        self.bind_clause(b, &mut names, defer, pending);
    }

    fn bind_clause(
        &mut self,
        b: &mut IrBuilder,
        names: &mut Pending<'s>,
        defer: bool,
        pending: &mut Pending<'s>,
    ) {
        if defer {
            pending.append(names);
            return;
        }
        for name in names.drain(..) {
            self.declare_local(b, name, BindingOrigin::Pattern, None, Value::Unknown);
        }
    }

    fn if_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        b.push_scope(ScopeKind::Block);
        self.conditions(b, node, false, &mut Vec::new());
        let join = b.new_block();
        let then = b.new_block();
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        let else_at = children
            .iter()
            .position(|c| c.kind() == "else")
            .unwrap_or(children.len());
        let has_else = else_at < children.len();
        let other = if has_else { b.new_block() } else { join };
        b.branch(&[then, other]);
        b.switch_to(then);
        for child in &children[..else_at] {
            if child.kind() == "statements" {
                self.walk(b, *child);
            }
        }
        b.pop_scope();
        b.goto(join);
        if has_else {
            b.switch_to(other);
            for child in &children[else_at..] {
                if matches!(child.kind(), "statements" | "if_statement") {
                    self.walk(b, *child);
                }
            }
            b.goto(join);
        }
        b.switch_to(join);
        Value::Unknown
    }

    fn guard_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let mut pending = Vec::new();
        self.conditions(b, node, true, &mut pending);
        let escape = b.new_block();
        let join = b.new_block();
        b.branch(&[escape, join]);
        b.switch_to(escape);
        for body in named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "statements")
        {
            self.walk(b, body);
        }
        b.goto(join);
        b.switch_to(join);
        for name in pending {
            self.declare_local(b, name, BindingOrigin::Pattern, None, Value::Unknown);
        }
        Value::Unknown
    }

    /// `head: cond -> body | exit`; the body returns to `head`.
    fn loop_around(
        &mut self,
        b: &mut IrBuilder,
        head_work: impl FnOnce(&mut Self, &mut IrBuilder),
        body_work: impl FnOnce(&mut Self, &mut IrBuilder),
    ) {
        let head = b.new_block();
        let body_block = b.new_block();
        let exit = b.new_block();
        b.goto(head);
        b.switch_to(head);
        b.push_scope(ScopeKind::Loop);
        head_work(self, b);
        b.branch(&[body_block, exit]);
        b.push_breakable(Some(head), exit);
        b.switch_to(body_block);
        body_work(self, b);
        b.goto(head);
        b.pop_breakable();
        b.pop_scope();
        b.switch_to(exit);
    }

    fn while_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        self.loop_around(
            b,
            |this, b| this.conditions(b, node, false, &mut Vec::new()),
            |this, b| {
                for body in named_children(node)
                    .into_iter()
                    .filter(|c| c.kind() == "statements")
                {
                    this.walk(b, body);
                }
            },
        );
        Value::Unknown
    }

    fn repeat_while_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let body_block = b.new_block();
        let test = b.new_block();
        let exit = b.new_block();
        b.goto(body_block);
        b.switch_to(body_block);
        b.push_breakable(Some(test), exit);
        for body in named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "statements")
        {
            self.walk(b, body);
        }
        b.goto(test);
        b.switch_to(test);
        for condition in children_by_field(node, "condition") {
            self.walk(b, condition);
        }
        b.branch(&[body_block, exit]);
        b.pop_breakable();
        b.switch_to(exit);
        Value::Unknown
    }

    fn for_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if let Some(collection) = node.child_by_field_name("collection") {
            self.walk(b, collection);
        }
        self.loop_around(
            b,
            |this, b| {
                let mut names = Vec::new();
                if let Some(item) = node.child_by_field_name("item") {
                    pattern_names(item, &mut names);
                }
                for name in names {
                    this.declare_local(
                        b,
                        this.text(name),
                        BindingOrigin::ForEach,
                        None,
                        Value::Unknown,
                    );
                }
                for clause in named_children(node)
                    .into_iter()
                    .filter(|c| c.kind() == "where_clause")
                {
                    this.children(b, clause);
                }
            },
            |this, b| {
                for body in named_children(node)
                    .into_iter()
                    .filter(|c| c.kind() == "statements")
                {
                    this.walk(b, body);
                }
            },
        );
        Value::Unknown
    }

    fn switch_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if let Some(expr) = node.child_by_field_name("expr") {
            self.walk(b, expr);
        }
        let entries: Vec<_> = named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "switch_entry")
            .collect();
        let exit = b.new_block();
        if entries.is_empty() {
            b.goto(exit);
            b.switch_to(exit);
            return Value::Unknown;
        }
        let blocks: Vec<_> = entries.iter().map(|_| b.new_block()).collect();
        b.branch(&blocks);
        b.push_breakable(None, exit);
        for (entry, block) in entries.into_iter().zip(blocks) {
            b.switch_to(block);
            b.push_scope(ScopeKind::Block);
            for child in named_children(entry) {
                match child.kind() {
                    "switch_pattern" => {
                        let mut names = Vec::new();
                        if let Some(pattern) = named_children(child)
                            .into_iter()
                            .find(|c| c.kind() == "pattern")
                        {
                            pattern_names(pattern, &mut names);
                        }
                        for name in names {
                            self.declare_local(
                                b,
                                self.text(name),
                                BindingOrigin::Pattern,
                                None,
                                Value::Unknown,
                            );
                        }
                    }
                    "statements" => {
                        self.walk(b, child);
                    }
                    "default_keyword" | "where_keyword" => {}
                    _ => {
                        self.walk(b, child);
                    }
                }
            }
            b.pop_scope();
            b.goto(exit);
        }
        b.pop_breakable();
        b.switch_to(exit);
        Value::Unknown
    }

    fn do_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let catches: Vec<_> = named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "catch_block")
            .collect();
        let body = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "statements");
        if catches.is_empty() {
            if let Some(body) = body {
                self.walk(b, body);
            }
            return Value::Unknown;
        }
        let exit = b.new_block();
        let handler = b.new_block();
        let blocks: Vec<_> = catches.iter().map(|_| b.new_block()).collect();
        b.enter_try(handler);
        if let Some(body) = body {
            self.walk(b, body);
        }
        b.leave_try();
        b.goto(exit);
        b.switch_to(handler);
        b.branch(&blocks);
        for (catch, block) in catches.into_iter().zip(blocks) {
            b.switch_to(block);
            b.push_scope(ScopeKind::Catch);
            match catch.child_by_field_name("error") {
                Some(pattern) => {
                    let mut names = Vec::new();
                    pattern_names(pattern, &mut names);
                    for name in names {
                        self.declare_local(
                            b,
                            self.text(name),
                            BindingOrigin::CatchParam,
                            None,
                            Value::Unknown,
                        );
                    }
                }
                None => {
                    self.declare_local(b, "error", BindingOrigin::CatchParam, None, Value::Unknown)
                }
            }
            for child in named_children(catch) {
                if matches!(child.kind(), "statements" | "where_clause") {
                    self.walk(b, child);
                }
            }
            b.pop_scope();
            b.goto(exit);
        }
        b.switch_to(exit);
        Value::Unknown
    }

    fn control_transfer(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let text = self.text(node);
        let keyword = text
            .split(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or("");
        match keyword {
            "return" => {
                self.children(b, node);
                b.ret();
            }
            "throw" => {
                self.children(b, node);
                b.throw();
            }
            "break" | "continue" => {
                if node.child_by_field_name("result").is_some() {
                    self.unsupported = Some("swift labeled break or continue");
                } else if keyword == "break" {
                    b.break_out();
                } else {
                    b.continue_loop();
                }
            }
            _ => {
                self.children(b, node);
            }
        }
        Value::Unknown
    }

    fn ternary(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if let Some(condition) = node.child_by_field_name("condition") {
            self.walk(b, condition);
        }
        let join = b.new_block();
        let arms: Vec<_> = ["if_true", "if_false"]
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

    fn short_circuit(
        &mut self,
        b: &mut IrBuilder,
        node: Node<'_>,
        first: &str,
        second: &str,
    ) -> Value {
        let (Some(left), Some(right)) = (
            node.child_by_field_name(first),
            node.child_by_field_name(second),
        ) else {
            return self.children(b, node);
        };
        self.walk(b, left);
        let run = b.new_block();
        let join = b.new_block();
        b.branch(&[run, join]);
        b.switch_to(run);
        self.walk(b, right);
        b.goto(join);
        b.switch_to(join);
        Value::Unknown
    }
}

fn children_by_field_kind<'t>(node: Node<'t>, kind: &str) -> Vec<Node<'t>> {
    named_children(node)
        .into_iter()
        .filter(|c| c.kind() == kind)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receiver::{analyze_file, Receiver};
    use std::path::Path;

    /// `(member, text)` of every `x.member` whose `x` is an identifier, in
    /// member-name order; `text` is `known T`, `unknown Cause` or `none`.
    fn receivers(source: &str) -> Vec<(String, String)> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let bytes = source.as_bytes();
        let found = analyze_file(
            &SwiftLowering,
            &(),
            tree.root_node(),
            bytes,
            Path::new("t.swift"),
        );
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if let Some(object) = navigation_receiver(n) {
                let name = n
                    .child_by_field_name("suffix")
                    .and_then(|s| s.child_by_field_name("suffix"))
                    .unwrap();
                let state = match found.receiver_at(object) {
                    Some(Receiver::Known(ty)) => format!("known {}", ty.text),
                    Some(Receiver::Unknown(cause)) => format!("unknown {cause:?}"),
                    None => "none".to_string(),
                };
                out.push((name.utf8_text(bytes).unwrap().to_string(), state));
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

    /// The state of the receiver of `member`, the one member the test asks
    /// about; other receivers in the fixture (`xs` of `xs.forEach`) may
    /// record uses of their own.
    fn of(source: &str, member: &str) -> String {
        let found = receivers(source);
        let mut rows = found.iter().filter(|(name, _)| name == member);
        let row = rows
            .next()
            .unwrap_or_else(|| panic!("no {member}: {found:?}"));
        assert!(rows.next().is_none(), "{found:?}");
        row.1.clone()
    }

    #[test]
    fn test_lower_annotated_parameter_is_known() {
        assert_eq!(only("func f(_ o: A) { o.m() }"), "known A");
    }

    #[test]
    fn test_lower_annotated_local_is_known() {
        assert_eq!(only("func f() { let o: A = make(); o.m() }"), "known A");
    }

    #[test]
    fn test_lower_unannotated_local_has_no_evidence() {
        assert_eq!(
            only("func f() { let o = make(); o.m() }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_optional_annotation_has_no_evidence() {
        assert_eq!(only("func f(o: A?) { o?.m() }"), "unknown NoEvidence");
    }

    #[test]
    fn test_lower_sibling_do_blocks_bind_second_block_type() {
        assert_eq!(
            only("func f() { do { let o: A = A() } do { let o: B = B(); o.m() } }"),
            "known B"
        );
    }

    #[test]
    fn test_lower_inner_block_shadow_keeps_each_type() {
        let found =
            receivers("func f() { let o: A = a(); do { let o: B = b(); o.inner() }; o.outer() }");
        assert_eq!(
            found,
            vec![
                ("inner".to_string(), "known B".to_string()),
                ("outer".to_string(), "known A".to_string())
            ]
        );
    }

    #[test]
    fn test_lower_if_let_binding_is_private_to_its_branch() {
        assert_eq!(
            only("func f(o: A, p: B?) { if let o = p { o.m() } }"),
            "unknown NoEvidence"
        );
        assert_eq!(
            only("func f(o: A, p: B?) { if let q = p { }; o.m() }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_if_let_initializer_reads_the_outer_binding() {
        assert_eq!(
            only("func f(o: A?) { if let o = o?.next() { } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_guard_let_binds_after_the_else_body() {
        assert_eq!(
            only("func f(o: A, p: B?) { guard let o = p else { return }; o.m() }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_for_variable_shadows_outer_binding() {
        assert_eq!(
            only("func f(o: A, xs: [B]) { for o in xs { o.m() } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_for_in_binding_does_not_leak_out_of_the_loop() {
        assert_eq!(
            only("func f(o: A, xs: [B]) { for o in xs { }; o.m() }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_catch_binding_shadows_outer_binding() {
        assert_eq!(
            only("func f(e: A) { do { try g() } catch let e { e.m() } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_switch_case_binding_is_private_to_its_entry() {
        assert_eq!(
            only("func f(w: A, s: S) { switch s { case .a(let w): w.m(); default: break } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_closure_parameter_shadows_outer_binding() {
        assert_eq!(
            only("func f(a: A) { let g = { (a: B) in a.m() } }"),
            "known B"
        );
    }

    #[test]
    fn test_lower_closure_captures_outer_annotation() {
        assert_eq!(of("func f(o: A) { xs.forEach { o.m() } }", "m"), "known A");
    }

    #[test]
    fn test_lower_implicit_closure_parameter_is_not_a_binding() {
        assert_eq!(of("func f() { xs.forEach { $0.m() } }", "m"), "none");
    }

    #[test]
    fn test_lower_type_name_receiver_is_not_a_binding() {
        assert_eq!(only("func f() { Util.make() }"), "none");
    }

    #[test]
    fn test_lower_empty_function_body_records_nothing() {
        assert!(receivers("func f() { }").is_empty());
    }

    #[test]
    fn test_lower_labeled_break_leaves_function_to_extractor() {
        assert_eq!(
            only("func f(o: A, xs: [B]) { outer: for x in xs { break outer }; o.m() }"),
            "none"
        );
    }

    #[test]
    fn test_lower_tuple_destructuring_declares_unknown_bindings() {
        assert_eq!(
            only("func f(p: A) { let (p, q) = pair(); p.m() }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_two_calls_on_one_line_each_get_a_state() {
        let found = receivers("func f(o: A, p: B) { o.a(); p.b() }");
        assert_eq!(
            found,
            vec![
                ("a".to_string(), "known A".to_string()),
                ("b".to_string(), "known B".to_string())
            ]
        );
    }

    #[test]
    fn test_lower_nested_type_declaration_is_opaque() {
        assert_eq!(
            only("func f(o: A) { struct S { var o: B }; o.m() }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_copy_of_pinned_binding_is_known() {
        assert_eq!(only("func f(o: A) { let p = o; p.m() }"), "known A");
    }
}
