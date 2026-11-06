//! Go lowering into the receiver core.
//!
//! A receiver occurrence is `x.m` with `x` a plain identifier that names a
//! binding of the function (own, or captured from an enclosing one), as a
//! callee or as a method value. Evidence is the static type the declaration
//! gives the binding: a parameter, receiver or named result `x T` / `x *T`,
//! `var x T`, and `x := T{..}` / `x := &T{..}`. A declared type pins the
//! binding on every path; a binding without one has no evidence unless it
//! copies a pinned one.
//!
//! Scope: a block, an `if` / `for` / `switch` / `select` statement and every
//! case clause open a scope. `:=` declares only the names its scope does not
//! hold yet and assigns the rest. A function literal captures by reference.

use super::receiver_types::bare_type_name;
use crate::framework_helpers::node_span;
use crate::function_meta::go::go_type_params;
use crate::receiver::ir::{
    BindingId, BindingOrigin, CaptureMode, ScopeId, ScopeKind, TypeId, TypeOrigin, Value,
};
use crate::receiver::{DeclaredTypePolicy, IrBuilder, LangPolicy, Lowered, Lowering, ScopeRule};
use tree_sitter::Node;

/// Nesting beyond this leaves the function to the extractor's own path.
const MAX_DEPTH: u32 = 256;

pub(super) struct GoLowering;

impl Lowering for GoLowering {
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
            "function_declaration" | "method_declaration" | "func_literal"
        )
    }

    fn lower(&self, _cx: &(), function: Node<'_>, source: &[u8], b: &mut IrBuilder) -> Lowered {
        if !has_receiver_member(function) {
            return Lowered::NoUses;
        }
        let mut lower = Lower {
            source,
            depth: 0,
            unsupported: None,
            type_params: go_type_params(&function, source),
            frame: 0,
            frames: 0,
            names: Vec::new(),
        };
        lower.callable(b, function);
        match lower.unsupported {
            Some(reason) => Lowered::Unsupported(reason),
            None if b.use_count() == 0 => Lowered::NoUses,
            None => Lowered::Done,
        }
    }
}

/// The identifier of `x.m`, the receiver of a selector.
pub(super) fn selector_receiver<'t>(selector: Node<'t>) -> Option<Node<'t>> {
    if selector.kind() != "selector_expression" {
        return None;
    }
    selector
        .child_by_field_name("operand")
        .filter(|operand| operand.kind() == "identifier")
}

/// The fast path: a function without a `x.m` selector has nothing to answer.
fn has_receiver_member(function: Node<'_>) -> bool {
    let mut stack = vec![function];
    while let Some(n) = stack.pop() {
        if selector_receiver(n).is_some() {
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

fn child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    named_children(node).into_iter().find(|c| c.kind() == kind)
}

/// Whether `node` has a `:=` token among its children.
fn declares(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|c| c.kind() == ":=");
    found
}

/// The identifiers of an `expression_list` (or a lone identifier).
fn identifiers<'t>(list: Node<'t>) -> Vec<Node<'t>> {
    if list.kind() == "identifier" {
        return vec![list];
    }
    named_children(list)
        .into_iter()
        .filter(|c| c.kind() == "identifier")
        .collect()
}

/// The expressions of an `expression_list` (or a lone expression).
fn expressions<'t>(list: Node<'t>) -> Vec<Node<'t>> {
    if list.kind() == "expression_list" {
        named_children(list)
    } else {
        vec![list]
    }
}

struct Lower<'s> {
    source: &'s [u8],
    depth: u32,
    unsupported: Option<&'static str>,
    /// Names that stand for a type parameter: a variable of such a type has
    /// no project type.
    type_params: Vec<String>,
    /// The function being lowered: a serial per root or function literal,
    /// because every `FunctionIr` numbers its scopes from zero.
    frame: u32,
    frames: u32,
    /// `(frame, scope, name)` of every name a scope holds, for `:=`.
    names: Vec<(u32, ScopeId, &'s str)>,
}

impl<'s> Lower<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or("")
    }

    fn resolve(&mut self, b: &mut IrBuilder, name: &str) -> Option<BindingId> {
        b.lookup_or_capture(name, CaptureMode::ByRef)
    }

    fn declared_type(&self, b: &mut IrBuilder, ty: Option<Node<'_>>) -> Option<TypeId> {
        let text = bare_type_name(ty?, self.source)?;
        if self.type_params.contains(&text) {
            return None;
        }
        Some(b.intern(&text, TypeOrigin::Annotation))
    }

    /// The type `T{..}` / `&T{..}` constructs, as the declared type of a
    /// binding that `:=` or `var x =` initialises with it.
    fn literal_type(&self, b: &mut IrBuilder, value: Node<'_>) -> Option<TypeId> {
        let literal = match value.kind() {
            "unary_expression" => value.child_by_field_name("operand")?,
            _ => value,
        };
        if literal.kind() != "composite_literal" {
            return None;
        }
        self.declared_type(b, literal.child_by_field_name("type"))
    }

    fn is_blank(&self, node: Node<'_>) -> bool {
        self.text(node) == "_"
    }

    fn declared_here(&self, b: &IrBuilder, name: &str) -> bool {
        let scope = b.current_scope();
        self.names
            .iter()
            .any(|&(frame, s, n)| frame == self.frame && s == scope && n == name)
    }

    fn declare(
        &mut self,
        b: &mut IrBuilder,
        name: Node<'_>,
        origin: BindingOrigin,
        declared: Option<TypeId>,
        value: Value,
    ) {
        let name = self.text(name);
        self.names.push((self.frame, b.current_scope(), name));
        let id = b.declare(name, origin, declared);
        b.assign(id, value);
    }

    fn assign_unknown(&mut self, b: &mut IrBuilder, name: Node<'_>) {
        if let Some(id) = self.resolve(b, self.text(name)) {
            b.assign(id, Value::Unknown);
        }
    }

    fn callable(&mut self, b: &mut IrBuilder, function: Node<'_>) {
        for field in ["receiver", "parameters", "result"] {
            if let Some(list) = function
                .child_by_field_name(field)
                .filter(|list| list.kind() == "parameter_list")
            {
                self.parameters(b, list, field == "result");
            }
        }
        if let Some(body) = function.child_by_field_name("body") {
            // The parameters and the body share one scope.
            for child in named_children(body) {
                self.walk(b, child);
            }
        }
    }

    fn parameters(&mut self, b: &mut IrBuilder, list: Node<'_>, result: bool) {
        for param in named_children(list) {
            let declared = (param.kind() == "parameter_declaration")
                .then(|| self.declared_type(b, param.child_by_field_name("type")))
                .flatten();
            for name in children_by_field(param, "name") {
                if self.is_blank(name) {
                    continue;
                }
                if result {
                    self.declare(b, name, BindingOrigin::Local, declared, Value::Unknown);
                } else {
                    let text = self.text(name);
                    self.names.push((self.frame, b.current_scope(), text));
                    b.declare(text, BindingOrigin::Param, declared);
                }
            }
        }
    }

    fn walk(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if self.unsupported.is_some() {
            return Value::Unknown;
        }
        if self.depth >= MAX_DEPTH {
            self.unsupported = Some("go receiver lowering exceeds the nesting limit");
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

    fn scoped(
        &mut self,
        b: &mut IrBuilder,
        kind: ScopeKind,
        body: impl FnOnce(&mut Self, &mut IrBuilder),
    ) {
        b.push_scope(kind);
        body(self, b);
        b.pop_scope();
    }

    fn walk_node(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        match node.kind() {
            "identifier" => match self.resolve(b, self.text(node)) {
                Some(id) => Value::Copy(id),
                None => Value::Unknown,
            },
            "parenthesized_expression" => node
                .named_child(0)
                .map_or(Value::Unknown, |inner| self.walk(b, inner)),
            "selector_expression" => {
                match selector_receiver(node) {
                    Some(operand) => {
                        if let Some(binding) = self.resolve(b, self.text(operand)) {
                            b.use_binding(binding, operand.id());
                        }
                    }
                    None => {
                        self.children(b, node);
                    }
                }
                Value::Unknown
            }
            "block" => {
                self.scoped(b, ScopeKind::Block, |this, b| {
                    this.children(b, node);
                });
                Value::Unknown
            }
            "func_literal" => {
                self.func_literal(b, node);
                Value::Unknown
            }
            "var_declaration" => {
                for spec in self.var_specs(node) {
                    self.var_spec(b, spec);
                }
                Value::Unknown
            }
            "const_declaration" => {
                for spec in named_children(node) {
                    for name in children_by_field(spec, "name") {
                        self.declare(b, name, BindingOrigin::Local, None, Value::Unknown);
                    }
                }
                Value::Unknown
            }
            "type_declaration" => Value::Unknown,
            "short_var_declaration" => {
                self.short_var_declaration(b, node);
                Value::Unknown
            }
            "assignment_statement" => {
                self.assignment(b, node);
                Value::Unknown
            }
            "inc_statement" | "dec_statement" => self.children(b, node),
            "binary_expression" => self.binary(b, node),
            "return_statement" => {
                self.children(b, node);
                b.ret();
                Value::Unknown
            }
            "break_statement" | "continue_statement" => {
                if child_of_kind(node, "label_name").is_some() {
                    self.unsupported = Some("go labeled break or continue");
                } else if node.kind() == "break_statement" {
                    b.break_out();
                } else {
                    b.continue_loop();
                }
                Value::Unknown
            }
            "goto_statement" => {
                self.unsupported = Some("go goto statement");
                Value::Unknown
            }
            "fallthrough_statement" => {
                self.unsupported = Some("go fallthrough statement");
                Value::Unknown
            }
            "if_statement" => self.if_statement(b, node),
            "for_statement" => self.for_statement(b, node),
            "expression_switch_statement" => self.switch_statement(b, node, false),
            "type_switch_statement" => self.switch_statement(b, node, true),
            "select_statement" => self.select_statement(b, node),
            _ => self.children(b, node),
        }
    }

    fn func_literal(&mut self, b: &mut IrBuilder, literal: Node<'_>) {
        let outer = self.frame;
        self.frames += 1;
        self.frame = self.frames;
        b.closure(node_span(&literal), |inner| self.callable(inner, literal));
        self.frame = outer;
    }

    fn var_specs<'t>(&self, declaration: Node<'t>) -> Vec<Node<'t>> {
        let mut specs = Vec::new();
        for child in named_children(declaration) {
            match child.kind() {
                "var_spec" => specs.push(child),
                "var_spec_list" => specs.extend(named_children(child)),
                _ => {}
            }
        }
        specs
    }

    fn var_spec(&mut self, b: &mut IrBuilder, spec: Node<'_>) {
        let values = spec
            .child_by_field_name("value")
            .map(expressions)
            .unwrap_or_default();
        let evaluated: Vec<Value> = values.iter().map(|v| self.walk(b, *v)).collect();
        let annotation = self.declared_type(b, spec.child_by_field_name("type"));
        let names = children_by_field(spec, "name");
        let pair = names.len() == values.len();
        for (index, name) in names.into_iter().enumerate() {
            if self.is_blank(name) {
                continue;
            }
            let (declared, value) = match annotation {
                Some(declared) => (Some(declared), Value::Unknown),
                None if pair => (
                    self.literal_type(b, values[index]),
                    evaluated[index].clone(),
                ),
                None => (None, Value::Unknown),
            };
            self.declare(b, name, BindingOrigin::Local, declared, value);
        }
    }

    fn short_var_declaration(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let values = node
            .child_by_field_name("right")
            .map(expressions)
            .unwrap_or_default();
        let evaluated: Vec<Value> = values.iter().map(|v| self.walk(b, *v)).collect();
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        let names = identifiers(left);
        let pair = names.len() == values.len();
        for (index, name) in names.into_iter().enumerate() {
            if self.is_blank(name) {
                continue;
            }
            let value = if pair {
                evaluated[index].clone()
            } else {
                Value::Unknown
            };
            if self.declared_here(b, self.text(name)) {
                if let Some(id) = self.resolve(b, self.text(name)) {
                    b.assign(id, value);
                }
                continue;
            }
            let declared = if pair {
                self.literal_type(b, values[index])
            } else {
                None
            };
            self.declare(b, name, BindingOrigin::Local, declared, value);
        }
    }

    fn assignment(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            self.children(b, node);
            return;
        };
        let rights = expressions(right);
        let lefts = expressions(left);
        let plain = node
            .child_by_field_name("operator")
            .is_some_and(|o| o.kind() == "=");
        let evaluated: Vec<Value> = rights.iter().map(|v| self.walk(b, *v)).collect();
        // A tuple assignment reads every right side before it writes.
        let single = plain && lefts.len() == 1 && rights.len() == 1;
        for target in lefts {
            if target.kind() == "identifier" {
                if self.is_blank(target) {
                    continue;
                }
                if let Some(id) = self.resolve(b, self.text(target)) {
                    let value = if single {
                        evaluated[0].clone()
                    } else {
                        Value::Unknown
                    };
                    b.assign(id, value);
                }
            } else {
                self.walk(b, target);
            }
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
            .is_some_and(|o| matches!(o.kind(), "&&" | "||"));
        if short_circuit {
            self.maybe(b, |this, b| {
                this.walk(b, right);
            });
        } else {
            self.walk(b, right);
        }
        Value::Unknown
    }

    fn if_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        b.push_scope(ScopeKind::Block);
        if let Some(initializer) = node.child_by_field_name("initializer") {
            self.walk(b, initializer);
        }
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
            self.walk(b, alternative);
            b.goto(join);
        }
        b.switch_to(join);
        b.pop_scope();
        Value::Unknown
    }

    fn for_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let body = node.child_by_field_name("body");
        let clause = named_children(node)
            .into_iter()
            .find(|c| matches!(c.kind(), "for_clause" | "range_clause"));
        let condition = named_children(node)
            .into_iter()
            .find(|c| !matches!(c.kind(), "for_clause" | "range_clause" | "block"));
        b.push_scope(ScopeKind::Loop);
        match clause {
            Some(range) if range.kind() == "range_clause" => {
                if let Some(right) = range.child_by_field_name("right") {
                    self.walk(b, right);
                }
                self.loop_around(b, None, None, |this, b| {
                    this.range_variables(b, range);
                    if let Some(body) = body {
                        this.walk(b, body);
                    }
                });
            }
            Some(clause) => {
                if let Some(initializer) = clause.child_by_field_name("initializer") {
                    self.walk(b, initializer);
                }
                self.loop_around(
                    b,
                    clause.child_by_field_name("condition"),
                    clause.child_by_field_name("update"),
                    |this, b| {
                        if let Some(body) = body {
                            this.walk(b, body);
                        }
                    },
                );
            }
            None => {
                self.loop_around(b, condition, None, |this, b| {
                    if let Some(body) = body {
                        this.walk(b, body);
                    }
                });
            }
        }
        b.pop_scope();
        Value::Unknown
    }

    /// `head: cond -> body | exit`; the body returns to `latch`.
    fn loop_around(
        &mut self,
        b: &mut IrBuilder,
        condition: Option<Node<'_>>,
        update: Option<Node<'_>>,
        body: impl FnOnce(&mut Self, &mut IrBuilder),
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
        body(self, b);
        b.goto(latch);
        if let Some(update) = update {
            b.switch_to(latch);
            self.walk(b, update);
            b.goto(head);
        }
        b.pop_breakable();
        b.switch_to(exit);
    }

    /// The `k, v` of `for k, v := range ..` (declared) or `for k = range ..`
    /// (assigned): each pass writes an unknown value.
    fn range_variables(&mut self, b: &mut IrBuilder, clause: Node<'_>) {
        let Some(left) = clause.child_by_field_name("left") else {
            return;
        };
        let declared = declares(clause);
        for target in expressions(left) {
            if target.kind() != "identifier" || self.is_blank(target) {
                if target.kind() != "identifier" {
                    self.walk(b, target);
                }
                continue;
            }
            if declared {
                self.declare(b, target, BindingOrigin::ForEach, None, Value::Unknown);
            } else {
                self.assign_unknown(b, target);
            }
        }
    }

    fn switch_statement(&mut self, b: &mut IrBuilder, node: Node<'_>, typed: bool) -> Value {
        b.push_scope(ScopeKind::Block);
        if let Some(initializer) = node.child_by_field_name("initializer") {
            self.walk(b, initializer);
        }
        if let Some(value) = node.child_by_field_name("value") {
            self.walk(b, value);
        }
        let cases: Vec<_> = named_children(node)
            .into_iter()
            .filter(|c| matches!(c.kind(), "expression_case" | "type_case" | "default_case"))
            .collect();
        let alias = typed
            .then(|| node.child_by_field_name("alias"))
            .flatten()
            .map(identifiers)
            .unwrap_or_default();
        let has_default = cases.iter().any(|c| c.kind() == "default_case");
        let exit = b.new_block();
        let blocks: Vec<_> = cases.iter().map(|_| b.new_block()).collect();
        let mut targets = blocks.clone();
        if !has_default {
            targets.push(exit);
        }
        b.branch(&targets);
        b.push_breakable(None, exit);
        for (case, block) in cases.into_iter().zip(blocks) {
            b.switch_to(block);
            self.scoped(b, ScopeKind::Block, |this, b| {
                for name in &alias {
                    this.declare(b, *name, BindingOrigin::Pattern, None, Value::Unknown);
                }
                this.children(b, case);
            });
            b.goto(exit);
        }
        b.pop_breakable();
        b.switch_to(exit);
        b.pop_scope();
        Value::Unknown
    }

    fn select_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let cases: Vec<_> = named_children(node)
            .into_iter()
            .filter(|c| matches!(c.kind(), "communication_case" | "default_case"))
            .collect();
        let exit = b.new_block();
        if cases.is_empty() {
            b.throw();
            b.switch_to(exit);
            return Value::Unknown;
        }
        let blocks: Vec<_> = cases.iter().map(|_| b.new_block()).collect();
        b.branch(&blocks);
        b.push_breakable(None, exit);
        for (case, block) in cases.into_iter().zip(blocks) {
            b.switch_to(block);
            self.scoped(b, ScopeKind::Block, |this, b| {
                for child in named_children(case) {
                    match child.kind() {
                        "receive_statement" => this.receive(b, child),
                        _ => {
                            this.walk(b, child);
                        }
                    }
                }
            });
            b.goto(exit);
        }
        b.pop_breakable();
        b.switch_to(exit);
        Value::Unknown
    }

    /// `v := <-ch` declares, `v = <-ch` assigns; the channel is read first.
    fn receive(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        if let Some(right) = node.child_by_field_name("right") {
            self.walk(b, right);
        }
        if let Some(left) = node.child_by_field_name("left") {
            let declared = declares(node);
            for target in expressions(left) {
                if target.kind() != "identifier" {
                    self.walk(b, target);
                } else if self.is_blank(target) {
                } else if declared {
                    self.declare(b, target, BindingOrigin::Pattern, None, Value::Unknown);
                } else {
                    self.assign_unknown(b, target);
                }
            }
        }
    }
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
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let bytes = source.as_bytes();
        let found = analyze_file(&GoLowering, &(), tree.root_node(), bytes, Path::new("t.go"));
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if let Some(object) = selector_receiver(n) {
                let name = n.child_by_field_name("field").unwrap();
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

    fn only(body: &str) -> String {
        let source = format!("package main\n\n{body}\n");
        let mut found = receivers(&source);
        assert_eq!(found.len(), 1, "{found:?}");
        found.remove(0).1
    }

    #[test]
    fn test_lower_parameter_is_known() {
        assert_eq!(only("func f(o *A) { o.m() }"), "known A");
    }

    #[test]
    fn test_lower_method_receiver_is_known() {
        assert_eq!(only("func (d *Dog) Run() { d.Bark() }"), "known Dog");
    }

    #[test]
    fn test_lower_var_declaration_is_known() {
        assert_eq!(only("func f() { var o A; o.m() }"), "known A");
    }

    #[test]
    fn test_lower_composite_literal_short_var_is_known() {
        assert_eq!(only("func f() { o := A{}; o.m() }"), "known A");
        assert_eq!(only("func f() { o := &A{}; o.m() }"), "known A");
    }

    #[test]
    fn test_lower_call_result_short_var_has_no_evidence() {
        assert_eq!(
            only("func f() { o := make(); o.m() }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_tuple_short_var_pairs_each_name_with_its_value() {
        let found = receivers("package main\nfunc f() { a, b := A{}, B{}; a.x(); b.y() }");
        assert_eq!(
            found,
            vec![
                ("x".to_string(), "known A".to_string()),
                ("y".to_string(), "known B".to_string())
            ]
        );
    }

    #[test]
    fn test_lower_multi_value_call_binds_no_type() {
        assert_eq!(
            only("func f() { o, err := make(); o.m(); _ = err }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_sibling_blocks_bind_second_block_type() {
        assert_eq!(
            only("func f() { { var o A; _ = o }; { var o B; o.m() } }"),
            "known B"
        );
    }

    #[test]
    fn test_lower_inner_block_shadow_keeps_each_type() {
        let found =
            receivers("package main\nfunc f() { var o A; { var o B; o.inner() }; o.outer() }");
        assert_eq!(
            found,
            vec![
                ("inner".to_string(), "known B".to_string()),
                ("outer".to_string(), "known A".to_string())
            ]
        );
    }

    #[test]
    fn test_lower_short_var_redeclaration_assigns_the_existing_binding() {
        assert_eq!(
            only("func f() { var o A; o, err := make(); _ = err; o.m() }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_short_var_in_inner_scope_shadows() {
        assert_eq!(
            only("func f(c bool) { var o A; if c { o := make(); o.m() } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_if_initializer_binding_is_private_to_the_if() {
        assert_eq!(
            only("func f(o A) { if o := make(); o != nil { }; o.m() }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_range_variable_shadows_outer_binding() {
        assert_eq!(
            only("func f(o A, xs []B) { for _, o := range xs { o.m() } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_range_assignment_keeps_the_pinned_type() {
        assert_eq!(
            only("func f(o A, xs []B) { for _, o = range xs { }; o.m() }"),
            "known A"
        );
    }

    #[test]
    fn test_lower_type_switch_alias_shadows_outer_binding() {
        assert_eq!(
            only("func f(t A, v any) { switch t := v.(type) { case B: t.m() } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_select_receive_binding_shadows_outer_binding() {
        assert_eq!(
            only("func f(r A, ch chan B) { select { case r := <-ch: r.m() } }"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_function_literal_captures_outer_binding() {
        assert_eq!(only("func f(o A) { go func() { o.m() }() }"), "known A");
    }

    #[test]
    fn test_lower_function_literal_parameter_shadows_outer_binding() {
        assert_eq!(
            only("func f(o A) { g := func(o B) { o.m() }; _ = g }"),
            "known B"
        );
    }

    #[test]
    fn test_lower_variadic_parameter_is_not_the_element_type() {
        assert_eq!(only("func f(o ...A) { o.m() }"), "unknown NoEvidence");
    }

    #[test]
    fn test_lower_type_parameter_binds_no_type() {
        assert_eq!(only("func f[T any](o T) { o.m() }"), "unknown NoEvidence");
    }

    #[test]
    fn test_lower_generic_type_binds_its_base_name() {
        assert_eq!(only("func f(o Wrap[int]) { o.m() }"), "known Wrap");
    }

    #[test]
    fn test_lower_qualified_type_binds_no_type() {
        assert_eq!(only("func f(o pkg.A) { o.m() }"), "unknown NoEvidence");
    }

    #[test]
    fn test_lower_named_result_is_known() {
        assert_eq!(only("func f() (r Res) { r.m(); return }"), "known Res");
    }

    #[test]
    fn test_lower_package_receiver_is_not_a_binding() {
        assert_eq!(only("func f() { fmt.Println() }"), "none");
    }

    #[test]
    fn test_lower_empty_function_body_records_nothing() {
        assert!(receivers("package main\nfunc f() {}").is_empty());
    }

    #[test]
    fn test_lower_labeled_break_leaves_function_to_extractor() {
        assert_eq!(
            only("func f(o A) {\nL:\n\tfor {\n\t\tbreak L\n\t}\n\to.m()\n}"),
            "none"
        );
    }

    #[test]
    fn test_lower_goto_leaves_function_to_extractor() {
        assert_eq!(only("func f(o A) {\n\tgoto L\nL:\n\to.m()\n}"), "none");
    }

    #[test]
    fn test_lower_two_calls_on_one_line_each_get_a_state() {
        let found = receivers("package main\nfunc f(o A, p B) { o.a(); p.b() }");
        assert_eq!(
            found,
            vec![
                ("a".to_string(), "known A".to_string()),
                ("b".to_string(), "known B".to_string())
            ]
        );
    }

    #[test]
    fn test_lower_copy_of_pinned_binding_is_known() {
        assert_eq!(only("func f(o A) { p := o; p.m() }"), "known A");
    }

    #[test]
    fn test_lower_method_value_selector_is_a_receiver() {
        assert_eq!(only("func f(o A) { apply(o.m) }"), "known A");
    }
}
