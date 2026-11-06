//! PHP lowering into the receiver core.
//!
//! A receiver occurrence is `$var->m()` / `$var?->m()` / `$var->m(...)` and
//! the object of a literal callable array `[$var, 'm']`. The lowering walks a
//! function body in evaluation order, so the core can join the types a
//! variable holds on every path to the occurrence.
//!
//! Evidence: a typed parameter (`A $o`, `?A $o`), `new A(..)` and a copy of a
//! typed variable, a single-class `catch (A $e)`. `$this` stays with the
//! call extractor, which names the enclosing class itself.

use super::receiver_types::qualified_ident;
use crate::framework_helpers::node_span;
use crate::receiver::ir::{BindingId, BindingOrigin, CaptureMode, TypeOrigin, Value};
use crate::receiver::lattice::UnknownCause;
use crate::receiver::{DeclaredTypePolicy, IrBuilder, LangPolicy, Lowered, Lowering, ScopeRule};
use tree_sitter::Node;

/// Nesting beyond this leaves the function to the extractor's own path: the
/// walk recurses, and a generated file can nest a binary chain thousands deep.
const MAX_DEPTH: u32 = 256;

pub(super) struct PhpLowering;

pub(super) fn is_function_like(kind: &str) -> bool {
    matches!(
        kind,
        "function_definition" | "method_declaration" | "anonymous_function" | "arrow_function"
    )
}

impl Lowering for PhpLowering {
    type Cx<'a> = ();

    fn policy(&self) -> LangPolicy {
        LangPolicy {
            declared: DeclaredTypePolicy::PointOfAssignment,
            scope: ScopeRule::Function,
        }
    }

    fn is_function(&self, node: Node<'_>) -> bool {
        is_function_like(node.kind())
    }

    fn lower(&self, _cx: &(), function: Node<'_>, source: &[u8], b: &mut IrBuilder) -> Lowered {
        let scan = scan(function, source);
        if !scan.has_use {
            return Lowered::NoUses;
        }
        let mut lower = Lower {
            source,
            by_ref_names: scan.by_ref_names,
            frames: Vec::new(),
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

struct Scan<'s> {
    has_use: bool,
    /// Variables some `use (&$x)` list names: a call that takes one as an
    /// argument may write it through a reference parameter.
    by_ref_names: Vec<&'s str>,
}

/// One pass over the function: whether any receiver occurrence exists (the
/// fast path skips a function without one) and which variables closures
/// capture by reference.
fn scan<'s>(function: Node<'_>, source: &'s [u8]) -> Scan<'s> {
    let mut scan = Scan {
        has_use: false,
        by_ref_names: Vec::new(),
    };
    let mut stack = vec![function];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "member_call_expression" | "nullsafe_member_call_expression" => {
                scan.has_use |= n
                    .child_by_field_name("object")
                    .is_some_and(|object| is_receiver_variable(object, source));
            }
            "array_creation_expression" => {
                scan.has_use |= callable_array_receiver(n, source).is_some();
            }
            "anonymous_function_use_clause" => {
                let mut cursor = n.walk();
                for clause in n.named_children(&mut cursor) {
                    if clause.kind() == "by_ref" {
                        if let Some(name) =
                            clause.named_child(0).and_then(|v| v.utf8_text(source).ok())
                        {
                            scan.by_ref_names.push(name);
                        }
                    }
                }
            }
            _ => {}
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    scan
}

fn is_receiver_variable(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "variable_name" && node.utf8_text(source).is_ok_and(|t| t != "$this")
}

/// The variable of a literal callable array `[$var, 'method']`.
pub(super) fn callable_array_receiver<'t>(array: Node<'t>, source: &[u8]) -> Option<Node<'t>> {
    if array.named_child_count() != 2 {
        return None;
    }
    let element = |index| {
        array
            .named_child(index)
            .filter(|e| e.kind() == "array_element_initializer")?
            .named_child(0)
    };
    let (receiver, method) = (element(0)?, element(1)?);
    (is_receiver_variable(receiver, source)
        && matches!(method.kind(), "string" | "encapsed_string"))
    .then_some(receiver)
}

#[derive(Default)]
struct FrameInfo {
    arrow: bool,
    /// Parameters whose declared type excludes null: `??=` cannot rewrite them.
    non_null: Vec<BindingId>,
}

struct Lower<'s> {
    source: &'s [u8],
    by_ref_names: Vec<&'s str>,
    frames: Vec<FrameInfo>,
    depth: u32,
    unsupported: Option<&'static str>,
}

fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    let children = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() != "comment")
        .collect();
    children
}

impl<'s> Lower<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or("")
    }

    fn frame(&mut self) -> &mut FrameInfo {
        self.frames
            .last_mut()
            .expect("callable pushes a frame before the body")
    }

    fn is_this(&self, node: Node<'_>) -> bool {
        self.text(node) == "$this"
    }

    fn read_var(&mut self, b: &mut IrBuilder, name: &str) -> BindingId {
        if let Some(id) = b.lookup(name) {
            return id;
        }
        if self.frames.last().is_some_and(|f| f.arrow) {
            if let Some(id) = b.capture(name, CaptureMode::ByValue) {
                return id;
            }
        }
        b.declare(name, BindingOrigin::Free, None)
    }

    fn write_var(&mut self, b: &mut IrBuilder, name: &str) -> BindingId {
        match b.lookup(name) {
            Some(id) => id,
            None => b.declare(name, BindingOrigin::Local, None),
        }
    }

    fn assign_unknown(&mut self, b: &mut IrBuilder, name: &str) {
        let id = self.write_var(b, name);
        self.frame().non_null.retain(|&p| p != id);
        b.assign(id, Value::Unknown);
    }

    /// Lowers a function-like node as the function being built: its
    /// parameters, its `use` list and its body.
    fn callable(&mut self, b: &mut IrBuilder, function: Node<'_>) {
        self.frames.push(FrameInfo {
            arrow: function.kind() == "arrow_function",
            non_null: Vec::new(),
        });
        if let Some(params) = function.child_by_field_name("parameters") {
            for param in named_children(params) {
                self.parameter(b, param);
            }
        }
        if function.kind() == "anonymous_function" {
            self.use_clause(b, function);
        }
        if let Some(body) = function.child_by_field_name("body") {
            self.walk(b, body);
        }
        self.frames.pop();
    }

    fn parameter(&mut self, b: &mut IrBuilder, param: Node<'_>) {
        if !matches!(
            param.kind(),
            "simple_parameter" | "property_promotion_parameter" | "variadic_parameter"
        ) {
            return;
        }
        let Some(name_node) = param.child_by_field_name("name") else {
            return;
        };
        let variable = if name_node.kind() == "by_ref" {
            match name_node.named_child(0) {
                Some(v) => v,
                None => return,
            }
        } else {
            name_node
        };
        let class = (param.kind() != "variadic_parameter")
            .then(|| self.parameter_class(param))
            .flatten();
        let declared = class.map(|(class, _)| b.intern(class, TypeOrigin::Annotation));
        let id = b.declare(self.text(variable), BindingOrigin::Param, declared);
        if class.is_some_and(|(_, nullable)| !nullable) {
            self.frame().non_null.push(id);
        }
    }

    /// The class a parameter declares, and whether it admits null: `None`
    /// for an untyped, primitive, union, `self` / `static` / `parent` type.
    fn parameter_class(&self, param: Node<'_>) -> Option<(&'s str, bool)> {
        let ty = param.child_by_field_name("type")?;
        let optional = ty.kind() == "optional_type";
        let ty = if optional { ty.named_child(0)? } else { ty };
        if ty.kind() != "named_type" {
            return None;
        }
        let class = qualified_ident(self.text(ty.named_child(0)?))?;
        if matches!(class, "self" | "static" | "parent") {
            return None;
        }
        let null_default = param
            .child_by_field_name("default_value")
            .is_some_and(|d| d.kind() == "null");
        Some((class, optional || null_default))
    }

    /// Declares in the enclosing function each variable a `use (..)` list
    /// names, so the capture finds it (`use (&$n)` creates `$n` as null).
    fn predeclare_captures(&mut self, b: &mut IrBuilder, closure: Node<'_>) {
        for (variable, _) in self.captures(closure) {
            self.write_var(b, self.text(variable));
        }
    }

    fn captures<'t>(&self, closure: Node<'t>) -> Vec<(Node<'t>, CaptureMode)> {
        let mut found = Vec::new();
        for clause in named_children(closure) {
            if clause.kind() != "anonymous_function_use_clause" {
                continue;
            }
            for item in named_children(clause) {
                match item.kind() {
                    "variable_name" => found.push((item, CaptureMode::ByValue)),
                    "by_ref" => found.extend(
                        item.named_child(0)
                            .map(|variable| (variable, CaptureMode::ByRef)),
                    ),
                    _ => {}
                }
            }
        }
        found
    }

    fn use_clause(&mut self, b: &mut IrBuilder, closure: Node<'_>) {
        for (variable, mode) in self.captures(closure) {
            let name = self.text(variable);
            if b.capture(name, mode).is_none() {
                b.declare(name, BindingOrigin::Free, None);
            }
        }
    }

    fn walk(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if self.unsupported.is_some() {
            return Value::Unknown;
        }
        if self.depth >= MAX_DEPTH {
            self.unsupported = Some("php receiver lowering exceeds the nesting limit");
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
            "variable_name" => {
                if self.is_this(node) {
                    return Value::Unknown;
                }
                let name = self.text(node);
                Value::Copy(self.read_var(b, name))
            }
            "object_creation_expression" => {
                self.children(b, node);
                self.construction(b, node)
            }
            "parenthesized_expression" | "clone_expression" => node
                .named_child(0)
                .map_or(Value::Unknown, |inner| self.walk(b, inner)),
            "assignment_expression" => self.assignment(b, node),
            "reference_assignment_expression" => self.reference_assignment(b, node),
            "augmented_assignment_expression" => self.augmented_assignment(b, node),
            "update_expression" => {
                match node.child_by_field_name("argument") {
                    Some(argument) if argument.kind() == "variable_name" => {
                        self.assign_unknown(b, self.text(argument));
                    }
                    _ => {
                        self.children(b, node);
                    }
                }
                Value::Unknown
            }
            "member_call_expression" | "nullsafe_member_call_expression" => {
                self.member_call(b, node)
            }
            "function_call_expression" => self.function_call(b, node),
            "arguments" => {
                self.arguments(b, node);
                Value::Unknown
            }
            "array_creation_expression" => {
                self.children(b, node);
                if let Some(receiver) = callable_array_receiver(node, self.source) {
                    self.record_use(b, receiver);
                }
                Value::Unknown
            }
            "anonymous_function" | "arrow_function" => {
                if node.kind() == "anonymous_function" {
                    self.predeclare_captures(b, node);
                }
                b.closure(node_span(&node), |inner| self.callable(inner, node));
                Value::Unknown
            }
            "function_definition" | "method_declaration" => {
                b.closure(node_span(&node), |inner| self.callable(inner, node));
                Value::Unknown
            }
            "binary_expression" => self.binary(b, node),
            "conditional_expression" => self.ternary(b, node),
            "match_expression" => self.match_expression(b, node),
            "throw_expression" => {
                self.children(b, node);
                b.throw();
                Value::Unknown
            }
            "return_statement" => {
                self.children(b, node);
                b.ret();
                Value::Unknown
            }
            "break_statement" => {
                self.jump(b, node, false);
                Value::Unknown
            }
            "continue_statement" => {
                self.jump(b, node, true);
                Value::Unknown
            }
            "goto_statement" => {
                self.unsupported = Some("php goto");
                Value::Unknown
            }
            "if_statement" => self.if_statement(b, node),
            "while_statement" => self.while_statement(b, node),
            "do_statement" => self.do_statement(b, node),
            "for_statement" => self.for_statement(b, node),
            "foreach_statement" => self.foreach_statement(b, node),
            "switch_statement" => self.switch_statement(b, node),
            "try_statement" => self.try_statement(b, node),
            "global_declaration" => {
                for target in named_children(node) {
                    self.assign_target(b, target);
                }
                Value::Unknown
            }
            "static_variable_declaration" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.walk(b, value);
                }
                if let Some(name) = node.child_by_field_name("name") {
                    self.assign_unknown(b, self.text(name));
                }
                Value::Unknown
            }
            "unset_statement" => {
                for target in named_children(node) {
                    if target.kind() == "variable_name" {
                        self.assign_unknown(b, self.text(target));
                    } else {
                        self.walk(b, target);
                    }
                }
                Value::Unknown
            }
            "include_expression"
            | "include_once_expression"
            | "require_expression"
            | "require_once_expression" => {
                self.children(b, node);
                b.havoc(None, UnknownCause::Unsupported);
                Value::Unknown
            }
            _ => self.children(b, node),
        }
    }

    fn record_use(&mut self, b: &mut IrBuilder, receiver: Node<'_>) {
        let name = self.text(receiver);
        let binding = self.read_var(b, name);
        b.use_binding(binding, receiver.id());
    }

    fn construction(&mut self, b: &mut IrBuilder, creation: Node<'_>) -> Value {
        let class = named_children(creation)
            .into_iter()
            .find(|c| matches!(c.kind(), "name" | "qualified_name"));
        let Some(class) = class else {
            return Value::Unknown;
        };
        match qualified_ident(self.text(class)) {
            Some(class) if !matches!(class, "self" | "static" | "parent") => {
                Value::Type(b.intern(class, TypeOrigin::Construction))
            }
            _ => Value::Unknown,
        }
    }

    fn member_call(&mut self, b: &mut IrBuilder, call: Node<'_>) -> Value {
        if let Some(object) = call.child_by_field_name("object") {
            if is_receiver_variable(object, self.source) {
                self.record_use(b, object);
            } else {
                self.walk(b, object);
            }
        }
        if let Some(name) = call.child_by_field_name("name") {
            if name.kind() != "name" {
                self.walk(b, name);
            }
        }
        if let Some(arguments) = call.child_by_field_name("arguments") {
            self.arguments(b, arguments);
        }
        Value::Unknown
    }

    fn function_call(&mut self, b: &mut IrBuilder, call: Node<'_>) -> Value {
        let function = call.child_by_field_name("function");
        let writes_scope = function.is_some_and(|f| {
            f.kind() == "name"
                && matches!(
                    self.text(f),
                    "extract" | "eval" | "parse_str" | "mb_parse_str"
                )
        });
        if let Some(f) = function.filter(|f| f.kind() != "name") {
            self.walk(b, f);
        }
        if let Some(arguments) = call.child_by_field_name("arguments") {
            self.arguments(b, arguments);
        }
        if writes_scope {
            b.havoc(None, UnknownCause::Unsupported);
        }
        Value::Unknown
    }

    /// A variable some closure captures by reference, passed as an argument,
    /// may be written through the callee's reference parameter (decision
    /// A01-neg-escape).
    fn arguments(&mut self, b: &mut IrBuilder, arguments: Node<'_>) {
        for argument in named_children(arguments) {
            if argument.kind() != "argument" {
                self.walk(b, argument);
                continue;
            }
            let Some(value) = named_children(argument).pop() else {
                continue;
            };
            self.walk(b, value);
            if value.kind() == "variable_name" && !self.is_this(value) {
                let name = self.text(value);
                if self.by_ref_names.contains(&name) {
                    let id = self.read_var(b, name);
                    b.havoc(Some(id), UnknownCause::Escape);
                }
            }
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
            "variable_name" if !self.is_this(left) => {
                let value = self.walk(b, right);
                let id = self.write_var(b, self.text(left));
                self.frame().non_null.retain(|&p| p != id);
                b.assign(id, value);
                Value::Copy(id)
            }
            "list_literal" | "array_creation_expression" => {
                self.walk(b, right);
                self.assign_target(b, left);
                Value::Unknown
            }
            "dynamic_variable_name" => {
                self.walk(b, right);
                b.havoc(None, UnknownCause::Unsupported);
                Value::Unknown
            }
            _ => {
                self.walk(b, left);
                self.walk(b, right);
                Value::Unknown
            }
        }
    }

    /// Every variable `target` writes (`$a`, `[$a, $b]`, `$k => $v`,
    /// `&$a`) gets a value the lowering cannot see.
    fn assign_target(&mut self, b: &mut IrBuilder, target: Node<'_>) {
        match target.kind() {
            "variable_name" if !self.is_this(target) => {
                self.assign_unknown(b, self.text(target));
            }
            "list_literal"
            | "array_creation_expression"
            | "array_element_initializer"
            | "pair"
            | "by_ref" => {
                for child in named_children(target) {
                    self.assign_target(b, child);
                }
            }
            "dynamic_variable_name" => b.havoc(None, UnknownCause::Unsupported),
            _ => {
                self.walk(b, target);
            }
        }
    }

    fn reference_assignment(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return self.children(b, node);
        };
        if left.kind() != "variable_name" || self.is_this(left) {
            self.walk(b, left);
            self.walk(b, right);
            return Value::Unknown;
        }
        if right.kind() == "variable_name" && !self.is_this(right) {
            let source = self.read_var(b, self.text(right));
            let target = self.write_var(b, self.text(left));
            b.alias(target, source);
        } else {
            self.walk(b, right);
            self.assign_unknown(b, self.text(left));
        }
        Value::Unknown
    }

    fn augmented_assignment(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return self.children(b, node);
        };
        if left.kind() != "variable_name" || self.is_this(left) {
            self.walk(b, left);
            self.walk(b, right);
            return Value::Unknown;
        }
        let coalesces = node
            .child_by_field_name("operator")
            .is_some_and(|o| o.kind() == "??=");
        if !coalesces {
            self.walk(b, right);
            self.assign_unknown(b, self.text(left));
            return Value::Unknown;
        }
        let id = self.write_var(b, self.text(left));
        let non_null = self.frames.last().is_some_and(|f| f.non_null.contains(&id));
        let rhs = b.new_block();
        let join = b.new_block();
        b.branch(&[rhs, join]);
        b.switch_to(rhs);
        let value = self.walk(b, right);
        if !non_null {
            b.assign(id, value);
        }
        b.goto(join);
        b.switch_to(join);
        Value::Unknown
    }

    fn binary(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let short_circuit = node
            .child_by_field_name("operator")
            .is_some_and(|o| matches!(o.kind(), "&&" | "||" | "and" | "or" | "??"));
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return self.children(b, node);
        };
        self.walk(b, left);
        if short_circuit {
            self.maybe(b, |this, b| {
                this.walk(b, right);
            });
        } else {
            self.walk(b, right);
        }
        Value::Unknown
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

    fn ternary(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if let Some(condition) = node.child_by_field_name("condition") {
            self.walk(b, condition);
        }
        let join = b.new_block();
        let arms: Vec<_> = ["body", "alternative"]
            .into_iter()
            .filter_map(|field| node.child_by_field_name(field))
            .collect();
        let blocks: Vec<_> = arms.iter().map(|_| b.new_block()).collect();
        // `a ?: b` has no body: the condition's own value is the other arm.
        let mut targets = blocks.clone();
        if arms.len() < 2 {
            targets.push(join);
        }
        b.branch(&targets);
        for (arm, block) in arms.into_iter().zip(blocks) {
            b.switch_to(block);
            self.walk(b, arm);
            b.goto(join);
        }
        b.switch_to(join);
        Value::Unknown
    }

    fn match_expression(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if let Some(condition) = node.child_by_field_name("condition") {
            self.walk(b, condition);
        }
        let arms = node
            .child_by_field_name("body")
            .map(named_children)
            .unwrap_or_default();
        let join = b.new_block();
        let blocks: Vec<_> = arms.iter().map(|_| b.new_block()).collect();
        if blocks.is_empty() {
            b.throw();
        } else {
            b.branch(&blocks);
        }
        for (arm, block) in arms.into_iter().zip(blocks) {
            b.switch_to(block);
            self.children(b, arm);
            b.goto(join);
        }
        b.switch_to(join);
        Value::Unknown
    }

    fn jump(&mut self, b: &mut IrBuilder, node: Node<'_>, is_continue: bool) {
        let levels = node
            .named_child(0)
            .and_then(|n| self.text(n).parse::<u32>().ok())
            .unwrap_or(1);
        if levels > 1 {
            self.unsupported = Some("php multi-level break or continue");
        } else if is_continue {
            b.continue_loop();
        } else {
            b.break_out();
        }
    }

    fn if_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if let Some(condition) = node.child_by_field_name("condition") {
            self.walk(b, condition);
        }
        let alternatives: Vec<Node<'_>> = {
            let mut cursor = node.walk();
            let found = node
                .children_by_field_name("alternative", &mut cursor)
                .collect();
            found
        };
        let join = b.new_block();
        let then = b.new_block();
        let mut next = if alternatives.is_empty() {
            join
        } else {
            b.new_block()
        };
        b.branch(&[then, next]);
        b.switch_to(then);
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(b, body);
        }
        b.goto(join);
        let last = alternatives.len().saturating_sub(1);
        for (index, alternative) in alternatives.into_iter().enumerate() {
            b.switch_to(next);
            let follow = if index == last { join } else { b.new_block() };
            if alternative.kind() == "else_if_clause" {
                if let Some(condition) = alternative.child_by_field_name("condition") {
                    self.walk(b, condition);
                }
                let taken = b.new_block();
                b.branch(&[taken, follow]);
                b.switch_to(taken);
            }
            if let Some(body) = alternative.child_by_field_name("body") {
                self.walk(b, body);
            }
            b.goto(join);
            next = follow;
        }
        b.switch_to(join);
        Value::Unknown
    }

    /// `head: cond -> body | exit`, the body returning to `head` (or to
    /// `continue_to` when given).
    fn loop_with(
        &mut self,
        b: &mut IrBuilder,
        condition: Option<Node<'_>>,
        body: &[Node<'_>],
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
        for statement in body {
            self.walk(b, *statement);
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
        let body: Vec<_> = node.child_by_field_name("body").into_iter().collect();
        self.loop_with(b, node.child_by_field_name("condition"), &body, None);
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
        if let Some(init) = node.child_by_field_name("initialize") {
            self.walk(b, init);
        }
        let body: Vec<Node<'_>> = {
            let mut cursor = node.walk();
            let found = node.children_by_field_name("body", &mut cursor).collect();
            found
        };
        self.loop_with(
            b,
            node.child_by_field_name("condition"),
            &body,
            node.child_by_field_name("update"),
        );
        Value::Unknown
    }

    fn foreach_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let body = node.child_by_field_name("body");
        let mut parts = named_children(node)
            .into_iter()
            .filter(|c| Some(c.id()) != body.map(|n| n.id()));
        if let Some(iterated) = parts.next() {
            self.walk(b, iterated);
        }
        let targets: Vec<_> = parts.collect();
        let head = b.new_block();
        let body_block = b.new_block();
        let exit = b.new_block();
        b.goto(head);
        b.switch_to(head);
        b.branch(&[body_block, exit]);
        b.push_breakable(Some(head), exit);
        b.switch_to(body_block);
        for target in targets {
            self.assign_target(b, target);
        }
        if let Some(body) = body {
            self.walk(b, body);
        }
        b.goto(head);
        b.pop_breakable();
        b.switch_to(exit);
        Value::Unknown
    }

    fn switch_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if let Some(condition) = node.child_by_field_name("condition") {
            self.walk(b, condition);
        }
        let cases = node
            .child_by_field_name("body")
            .map(named_children)
            .unwrap_or_default();
        let exit = b.new_block();
        let blocks: Vec<_> = cases.iter().map(|_| b.new_block()).collect();
        let has_default = cases.iter().any(|c| c.kind() == "default_statement");
        let mut targets = blocks.clone();
        if !has_default {
            targets.push(exit);
        }
        b.branch(&targets);
        b.push_breakable(None, exit);
        for (index, case) in cases.into_iter().enumerate() {
            b.switch_to(blocks[index]);
            self.children(b, case);
            b.goto(blocks.get(index + 1).copied().unwrap_or(exit));
        }
        b.pop_breakable();
        b.switch_to(exit);
        Value::Unknown
    }

    fn try_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let parts = named_children(node);
        let catches: Vec<_> = parts
            .iter()
            .filter(|c| c.kind() == "catch_clause")
            .collect();
        let finally = parts.iter().find(|c| c.kind() == "finally_clause");
        let exit = b.new_block();
        let finally_block = finally.map(|_| b.new_block());
        let join = finally_block.unwrap_or(exit);
        let handler = b.new_block();
        let catch_blocks: Vec<_> = catches.iter().map(|_| b.new_block()).collect();

        b.enter_try(handler);
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(b, body);
        }
        b.leave_try();
        b.goto(join);

        // An exception no `catch` takes still runs the `finally`.
        let mut targets = catch_blocks.clone();
        targets.extend(finally_block);
        b.switch_to(handler);
        if targets.is_empty() {
            b.throw();
        } else {
            b.branch(&targets);
        }
        for (clause, block) in catches.into_iter().zip(catch_blocks) {
            b.switch_to(block);
            self.catch_variable(b, *clause);
            if let Some(body) = clause.child_by_field_name("body") {
                self.walk(b, body);
            }
            b.goto(join);
        }
        if let (Some(finally), Some(block)) = (finally, finally_block) {
            b.switch_to(block);
            self.children(b, *finally);
            b.goto(exit);
        }
        b.switch_to(exit);
        Value::Unknown
    }

    fn catch_variable(&mut self, b: &mut IrBuilder, clause: Node<'_>) {
        let Some(name) = clause.child_by_field_name("name") else {
            return;
        };
        let id = self.write_var(b, self.text(name));
        self.frame().non_null.retain(|&p| p != id);
        let classes = clause
            .child_by_field_name("type")
            .map(named_children)
            .unwrap_or_default();
        let value = match classes.as_slice() {
            [single] if single.kind() == "named_type" => single
                .named_child(0)
                .and_then(|n| qualified_ident(self.text(n)))
                .map_or(Value::Unknown, |class| {
                    Value::Type(b.intern(class, TypeOrigin::Annotation))
                }),
            _ => Value::Unknown,
        };
        b.assign(id, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receiver::lattice::UnknownCause;
    use crate::receiver::{analyze_file, Receiver};
    use std::path::Path;

    /// `(method, receiver)` of every `$var->method()` in source order.
    fn receivers(source: &str) -> Vec<(String, Option<Receiver>)> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let bytes = source.as_bytes();
        let found = analyze_file(
            &PhpLowering,
            &(),
            tree.root_node(),
            bytes,
            Path::new("t.php"),
        );
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if n.kind() == "member_call_expression" {
                let object = n.child_by_field_name("object").unwrap();
                let name = n.child_by_field_name("name").unwrap();
                out.push((
                    name.utf8_text(bytes).unwrap().to_string(),
                    found.receiver_at(object).cloned(),
                ));
            }
            let mut cursor = n.walk();
            stack.extend(n.named_children(&mut cursor));
        }
        out.sort_by_key(|(name, _)| name.clone());
        out
    }

    fn only(source: &str) -> Option<Receiver> {
        let mut found = receivers(source);
        assert_eq!(found.len(), 1, "{found:?}");
        found.remove(0).1
    }

    fn known(class: &str) -> Option<Receiver> {
        Some(Receiver::Known(crate::receiver::ir::TypeRef {
            text: class.into(),
            origin: TypeOrigin::Annotation,
        }))
    }

    fn text_of(receiver: Option<Receiver>) -> String {
        match receiver {
            Some(Receiver::Known(ty)) => format!("known {}", ty.text),
            Some(Receiver::Unknown(cause)) => format!("unknown {cause:?}"),
            None => "none".to_string(),
        }
    }

    #[test]
    fn test_lower_typed_parameter_receiver_is_known() {
        let got = only("<?php function f(A $o) { $o->m(); }");
        assert_eq!(text_of(got), text_of(known("A")));
    }

    #[test]
    fn test_lower_untyped_parameter_receiver_has_no_evidence() {
        let got = only("<?php function f($o) { $o->m(); }");
        assert_eq!(got, Some(Receiver::Unknown(UnknownCause::NoEvidence)));
    }

    #[test]
    fn test_lower_branches_with_two_classes_conflict() {
        let got = only(
            "<?php function f($c) { if ($c) { $o = new A(); } else { $o = new B(); } $o->m(); }",
        );
        assert_eq!(got, Some(Receiver::Unknown(UnknownCause::Conflict)));
    }

    #[test]
    fn test_lower_branches_with_one_class_stay_known() {
        let got = only(
            "<?php function f($c) { if ($c) { $o = new A(); } else { $o = new A(); } $o->m(); }",
        );
        assert_eq!(text_of(got), "known A");
    }

    #[test]
    fn test_lower_coalesce_assign_of_non_null_parameter_keeps_type() {
        let got = only("<?php function f(A $o, $x) { $o ??= $x; $o->m(); }");
        assert_eq!(text_of(got), "known A");
    }

    #[test]
    fn test_lower_coalesce_assign_of_nullable_parameter_conflicts() {
        let got = only("<?php function f(?A $o, $x) { $o ??= $x; $o->m(); }");
        assert_eq!(got, Some(Receiver::Unknown(UnknownCause::Conflict)));
    }

    #[test]
    fn test_lower_read_only_by_ref_capture_keeps_type() {
        let got = only(
            "<?php function f(A $o) { $g = function () use (&$o) { return $o; }; $f = function () use (&$o) { return $o->m(); }; }",
        );
        assert_eq!(text_of(got), "known A");
    }

    #[test]
    fn test_lower_by_ref_capture_written_by_sibling_escapes() {
        let got = only(
            "<?php function f(A $o, $a) { $f = function () use (&$o) { return $o->m(); }; $g = function () use (&$o, $a) { $o = $a; }; }",
        );
        assert_eq!(got, Some(Receiver::Unknown(UnknownCause::Escape)));
    }

    #[test]
    fn test_lower_by_ref_alias_escapes() {
        let got = only("<?php function f(A $o) { $r = &$o; $o->m(); }");
        assert_eq!(got, Some(Receiver::Unknown(UnknownCause::Escape)));
    }

    #[test]
    fn test_lower_by_ref_captured_variable_passed_to_call_escapes() {
        let got = only(
            "<?php function f(A $o) { $f = function () use (&$o) { return $o->m(); }; $g = function () use (&$o) { return take($o); }; }",
        );
        assert_eq!(got, Some(Receiver::Unknown(UnknownCause::Escape)));
    }

    #[test]
    fn test_lower_by_value_capture_keeps_state_at_creation() {
        let got = only(
            "<?php function f(A $o, $a) { $f = function () use ($o) { return $o->m(); }; $o = $a; }",
        );
        assert_eq!(text_of(got), "known A");
    }

    #[test]
    fn test_lower_arrow_function_captures_by_value() {
        let got = only("<?php function f(A $o) { return fn () => $o->m(); }");
        assert_eq!(text_of(got), "known A");
    }

    #[test]
    fn test_lower_foreach_target_rebinds_receiver() {
        let got = only("<?php function f(A $o, $xs) { foreach ($xs as $o) {} $o->m(); }");
        assert_eq!(got, Some(Receiver::Unknown(UnknownCause::Conflict)));
    }

    #[test]
    fn test_lower_list_destructuring_rebinds_receiver() {
        let got = only("<?php function f(A $o, $xs) { [$o] = $xs; $o->m(); }");
        assert_eq!(got, Some(Receiver::Unknown(UnknownCause::NoEvidence)));
    }

    #[test]
    fn test_lower_catch_variable_takes_the_caught_class() {
        let got = only("<?php function f() { try { g(); } catch (\\E $e) { $e->m(); } }");
        assert_eq!(text_of(got), "known E");
    }

    #[test]
    fn test_lower_catch_variable_of_union_is_unknown() {
        let got = only("<?php function f() { try { g(); } catch (E | F $e) { $e->m(); } }");
        assert_eq!(got, Some(Receiver::Unknown(UnknownCause::NoEvidence)));
    }

    #[test]
    fn test_lower_write_in_loop_reaches_earlier_use() {
        let got = only("<?php function f(A $o, $a) { while ($a) { $o->m(); $o = $a; } }");
        assert_eq!(got, Some(Receiver::Unknown(UnknownCause::Conflict)));
    }

    #[test]
    fn test_lower_empty_function_body_records_nothing() {
        let found = receivers("<?php function f($o) {}");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn test_lower_this_receiver_is_left_to_the_extractor() {
        let got = only("<?php class C { function f() { $this->m(); } }");
        assert_eq!(got, None);
    }

    #[test]
    fn test_lower_two_calls_on_one_line_answer_separately() {
        let found = receivers("<?php function f(A $o, $p) { $o->a(); $p->b(); }");
        assert_eq!(
            found
                .into_iter()
                .map(|(name, r)| (name, text_of(r)))
                .collect::<Vec<_>>(),
            vec![
                ("a".to_string(), "known A".to_string()),
                ("b".to_string(), "unknown NoEvidence".to_string())
            ]
        );
    }

    #[test]
    fn test_lower_callable_array_variable_is_a_receiver() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
            .unwrap();
        let source = "<?php function f(A $o) { return array_map([$o, 'm'], []); }";
        let tree = parser.parse(source, None).unwrap();
        let found = analyze_file(
            &PhpLowering,
            &(),
            tree.root_node(),
            source.as_bytes(),
            Path::new("t.php"),
        );
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn test_lower_multi_level_break_leaves_function_to_extractor() {
        let got = only(
            "<?php function f(A $o, $xs) { foreach ($xs as $x) { foreach ($xs as $y) { break 2; } } $o->m(); }",
        );
        assert_eq!(got, None);
    }
}
