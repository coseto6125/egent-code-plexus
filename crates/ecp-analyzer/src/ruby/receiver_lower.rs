//! Ruby lowering into the receiver core.
//!
//! A receiver occurrence is `x.m` / `x&.m` with `x` an identifier that names
//! a local of the method (own, or captured from an enclosing block scope).
//! The only evidence is `x = Const.new(..)`; a local the paths disagree on or
//! a block may rewrite stops being `Known`.
//!
//! Scope: a `def`, `class`, `module` and `class << self` body sees no outer
//! local; a block or lambda captures every outer local by reference and
//! declares its parameters in its own scope.

use crate::framework_helpers::node_span;
use crate::receiver::ir::{BindingId, BindingOrigin, BlockId, CaptureMode, TypeOrigin, Value};
use crate::receiver::lattice::UnknownCause;
use crate::receiver::{DeclaredTypePolicy, IrBuilder, LangPolicy, Lowered, Lowering, ScopeRule};
use tree_sitter::Node;

/// Nesting beyond this leaves the function to the extractor's own path.
const MAX_DEPTH: u32 = 256;

pub(super) struct RubyLowering;

pub(super) fn is_function_like(kind: &str) -> bool {
    matches!(
        kind,
        "method" | "singleton_method" | "block" | "do_block" | "lambda"
    )
}

impl Lowering for RubyLowering {
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
        if !has_receiver_call(function) {
            return Lowered::NoUses;
        }
        let mut lower = Lower {
            source,
            depth: 0,
            unsupported: None,
            frames: Vec::new(),
        };
        lower.callable(b, function);
        match lower.unsupported {
            Some(reason) => Lowered::Unsupported(reason),
            None if b.use_count() == 0 => Lowered::NoUses,
            None => Lowered::Done,
        }
    }
}

/// The identifier of `x.m` / `x&.m`.
fn call_receiver<'t>(call: Node<'t>) -> Option<Node<'t>> {
    call.child_by_field_name("receiver")
        .filter(|receiver| receiver.kind() == "identifier")
}

/// Whether `call` may rebind the caller's locals: a bare `eval` /
/// `binding`, or a string evaluated with any receiver (`Kernel.eval(s)`,
/// `o.instance_eval(s)`, `C.class_eval(s)`). Ruby evaluates such a string
/// in the caller's local scope; the block form is a closure instead.
fn writes_caller_locals(call: Node<'_>, has_receiver: bool, method: Option<&str>) -> bool {
    match method {
        Some("eval" | "binding") if !has_receiver => true,
        Some("eval" | "instance_eval" | "class_eval" | "module_eval") => call
            .child_by_field_name("arguments")
            .is_some_and(|arguments| {
                named_children(arguments)
                    .iter()
                    .any(|argument| argument.kind() != "block_argument")
            }),
        _ => false,
    }
}

/// The fast path: a function without a `x.m` call has nothing to answer.
fn has_receiver_call(function: Node<'_>) -> bool {
    let mut stack = vec![function];
    while let Some(n) = stack.pop() {
        if n.kind() == "call" && call_receiver(n).is_some() {
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    /// `def`, `class`, `module`, `class << self`: no outer local is visible.
    Def,
    /// A block or lambda: outer locals are captured by reference.
    Block,
}

struct Lower<'s> {
    source: &'s [u8],
    depth: u32,
    unsupported: Option<&'static str>,
    frames: Vec<FrameKind>,
}

impl<'s> Lower<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or("")
    }

    fn resolve(&mut self, b: &mut IrBuilder, name: &str) -> Option<BindingId> {
        match self.frames.last() {
            Some(FrameKind::Block) => b.lookup_or_capture(name, CaptureMode::ByRef),
            _ => b.lookup(name),
        }
    }

    fn write_var(&mut self, b: &mut IrBuilder, name: &str) -> BindingId {
        match self.resolve(b, name) {
            Some(id) => id,
            None => b.declare(name, BindingOrigin::Local, None),
        }
    }

    fn assign_unknown(&mut self, b: &mut IrBuilder, name: &str) {
        let id = self.write_var(b, name);
        b.assign(id, Value::Unknown);
    }

    fn callable(&mut self, b: &mut IrBuilder, function: Node<'_>) {
        let kind = match function.kind() {
            "block" | "do_block" | "lambda" => FrameKind::Block,
            _ => FrameKind::Def,
        };
        self.frames.push(kind);
        if let Some(params) = function.child_by_field_name("parameters") {
            for param in named_children(params) {
                self.parameter(b, param);
            }
        }
        // A lambda's own body node is the block that holds the statements.
        let body = function.child_by_field_name("body").map(|body| {
            if function.kind() == "lambda" {
                body.child_by_field_name("body").unwrap_or(body)
            } else {
                body
            }
        });
        if let Some(body) = body {
            self.walk(b, body);
        }
        self.frames.pop();
    }

    fn parameter(&mut self, b: &mut IrBuilder, param: Node<'_>) {
        match param.kind() {
            "identifier" => {
                b.declare(self.text(param), BindingOrigin::Param, None);
            }
            "optional_parameter" | "keyword_parameter" => {
                if let Some(default) = param.child_by_field_name("value") {
                    self.walk(b, default);
                }
                if let Some(name) = param.child_by_field_name("name") {
                    b.declare(self.text(name), BindingOrigin::Param, None);
                }
            }
            "splat_parameter" | "hash_splat_parameter" | "block_parameter" => {
                if let Some(name) = param.child_by_field_name("name") {
                    b.declare(self.text(name), BindingOrigin::Param, None);
                }
            }
            "destructured_parameter" => {
                for inner in named_children(param) {
                    self.parameter(b, inner);
                }
            }
            _ => {}
        }
    }

    fn walk(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if self.unsupported.is_some() {
            return Value::Unknown;
        }
        if self.depth >= MAX_DEPTH {
            self.unsupported = Some("ruby receiver lowering exceeds the nesting limit");
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

    fn closure(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        b.closure(node_span(&node), |inner| self.callable(inner, node));
    }

    fn walk_node(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        match node.kind() {
            "identifier" => match self.resolve(b, self.text(node)) {
                Some(id) => Value::Copy(id),
                None => Value::Unknown,
            },
            "parenthesized_statements" => {
                let mut last = Value::Unknown;
                for child in named_children(node) {
                    last = self.walk(b, child);
                }
                last
            }
            "assignment" => self.assignment(b, node),
            "operator_assignment" => self.operator_assignment(b, node),
            "call" => self.call(b, node),
            "method" | "singleton_method" | "class" | "module" | "singleton_class" | "block"
            | "do_block" | "lambda" => {
                self.closure(b, node);
                Value::Unknown
            }
            "if" | "unless" | "elsif" => self.if_like(b, node),
            "if_modifier" | "unless_modifier" => {
                if let Some(condition) = node.child_by_field_name("condition") {
                    self.walk(b, condition);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.maybe(b, |this, b| {
                        this.walk(b, body);
                    });
                }
                Value::Unknown
            }
            "while" | "until" | "while_modifier" | "until_modifier" => {
                self.loop_with(
                    b,
                    node.child_by_field_name("condition"),
                    node.child_by_field_name("body"),
                    None,
                );
                Value::Unknown
            }
            "for" => self.for_loop(b, node),
            "case" => self.case(b, node),
            "begin" | "body_statement" => self.begin(b, node),
            "rescue_modifier" => {
                if let Some(body) = node.child_by_field_name("body") {
                    self.walk(b, body);
                }
                if let Some(handler) = node.child_by_field_name("handler") {
                    self.maybe(b, |this, b| {
                        this.walk(b, handler);
                    });
                }
                Value::Unknown
            }
            "conditional" => self.conditional(b, node),
            "binary" => self.binary(b, node),
            "return" => {
                self.children(b, node);
                b.ret();
                Value::Unknown
            }
            "break" => {
                self.children(b, node);
                b.break_out();
                Value::Unknown
            }
            "next" => {
                self.children(b, node);
                b.continue_loop();
                Value::Unknown
            }
            "redo" | "retry" => {
                self.unsupported = Some("ruby redo or retry");
                Value::Unknown
            }
            "case_match" => {
                self.unsupported = Some("ruby pattern matching");
                Value::Unknown
            }
            // `value => pattern` raises unless it matches, so it binds.
            "match_pattern" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.walk(b, value);
                }
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    self.bind_pattern(b, pattern);
                }
                Value::Unknown
            }
            // `value in pattern` binds only when it matches.
            "test_pattern" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.walk(b, value);
                }
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    self.maybe(b, |this, b| this.bind_pattern(b, pattern));
                }
                Value::Unknown
            }
            "self" => Value::Unknown,
            _ => self.children(b, node),
        }
    }

    /// `x = value`, `a, b = ..`: the value is `Const.new(..)` or another
    /// local; anything else is a value the lowering cannot type.
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
                let id = self.write_var(b, self.text(left));
                b.assign(id, value);
                Value::Copy(id)
            }
            "left_assignment_list" => {
                self.walk(b, right);
                self.assign_targets(b, left);
                Value::Unknown
            }
            _ => {
                self.walk(b, left);
                self.walk(b, right);
                Value::Unknown
            }
        }
    }

    fn assign_targets(&mut self, b: &mut IrBuilder, target: Node<'_>) {
        match target.kind() {
            "identifier" => self.assign_unknown(b, self.text(target)),
            "left_assignment_list" | "destructured_left_assignment" | "rest_assignment" => {
                for child in named_children(target) {
                    self.assign_targets(b, child);
                }
            }
            _ => {
                self.walk(b, target);
            }
        }
    }

    /// Writes an unknown value to every local `pattern` binds: a bare name,
    /// `{key:}`, `*rest`, `**rest` and `=> name`. A `^pin`, a literal or a
    /// constant is read.
    fn bind_pattern(&mut self, b: &mut IrBuilder, pattern: Node<'_>) {
        match pattern.kind() {
            "identifier" => self.assign_unknown(b, self.text(pattern)),
            "splat_parameter" | "hash_splat_parameter" => {
                if let Some(name) = pattern.child_by_field_name("name") {
                    self.assign_unknown(b, self.text(name));
                }
            }
            "as_pattern" => {
                if let Some(value) = pattern.child_by_field_name("value") {
                    self.bind_pattern(b, value);
                }
                if let Some(name) = pattern.child_by_field_name("name") {
                    self.assign_unknown(b, self.text(name));
                }
            }
            "keyword_pattern" => match pattern.child_by_field_name("value") {
                Some(value) => self.bind_pattern(b, value),
                // `{user:}` binds the local `user`.
                None => {
                    if let Some(key) = pattern
                        .child_by_field_name("key")
                        .filter(|key| key.kind() == "hash_key_symbol")
                    {
                        let name = self.text(key);
                        if name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
                            self.assign_unknown(b, name);
                        }
                    }
                }
            },
            "array_pattern"
            | "find_pattern"
            | "hash_pattern"
            | "alternative_pattern"
            | "parenthesized_pattern" => {
                for child in named_children(pattern) {
                    self.bind_pattern(b, child);
                }
            }
            _ => {
                self.walk(b, pattern);
            }
        }
    }

    fn operator_assignment(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
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
            .is_some_and(|o| matches!(o.kind(), "||=" | "&&="));
        if conditional {
            self.maybe(b, |this, b| {
                let value = this.walk(b, right);
                let id = this.write_var(b, name);
                b.assign(id, value);
            });
        } else {
            self.walk(b, right);
            self.assign_unknown(b, name);
        }
        Value::Unknown
    }

    fn call(&mut self, b: &mut IrBuilder, call: Node<'_>) -> Value {
        let receiver = call.child_by_field_name("receiver");
        let method = call.child_by_field_name("method").map(|m| self.text(m));
        match receiver {
            Some(receiver) if call_receiver(call).is_some() => {
                if let Some(binding) = self.resolve(b, self.text(receiver)) {
                    b.use_binding(binding, receiver.id());
                }
            }
            Some(receiver) => {
                self.walk(b, receiver);
            }
            None => {}
        }
        if let Some(arguments) = call.child_by_field_name("arguments") {
            self.walk(b, arguments);
        }
        if writes_caller_locals(call, receiver.is_some(), method) {
            b.havoc(None, UnknownCause::Unsupported);
        }
        if let Some(block) = call.child_by_field_name("block") {
            self.closure(b, block);
        }
        // `Const.new(..)` is the constructor evidence.
        match (receiver, method) {
            (Some(receiver), Some("new")) if receiver.kind() == "constant" => {
                Value::Type(b.intern(self.text(receiver), TypeOrigin::Construction))
            }
            _ => Value::Unknown,
        }
    }

    /// Runs `body` so that its abrupt exits (`return`, `next`, `break`, a
    /// raise) also reach `ensure`, when there is one.
    fn guarded(
        &mut self,
        b: &mut IrBuilder,
        ensure: Option<BlockId>,
        body: impl FnOnce(&mut Self, &mut IrBuilder),
    ) {
        match ensure {
            Some(ensure) => {
                b.enter_try(ensure);
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
            .is_some_and(|o| matches!(o.kind(), "&&" | "||" | "and" | "or"));
        if short_circuit {
            self.maybe(b, |this, b| {
                this.walk(b, right);
            });
        } else {
            self.walk(b, right);
        }
        Value::Unknown
    }

    fn conditional(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
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

    /// `if` / `unless` / `elsif`: the consequence and the alternative are
    /// both optional paths.
    fn if_like(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
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
        Value::Unknown
    }

    /// `head: cond -> body | exit`; the body returns to `head`.
    fn loop_with(
        &mut self,
        b: &mut IrBuilder,
        condition: Option<Node<'_>>,
        body: Option<Node<'_>>,
        before_body: Option<Node<'_>>,
    ) {
        let head = b.new_block();
        let body_block = b.new_block();
        let exit = b.new_block();
        b.goto(head);
        b.switch_to(head);
        if let Some(condition) = condition {
            self.walk(b, condition);
        }
        b.branch(&[body_block, exit]);
        b.push_breakable(Some(head), exit);
        b.switch_to(body_block);
        if let Some(target) = before_body {
            self.assign_targets(b, target);
        }
        if let Some(body) = body {
            self.walk(b, body);
        }
        b.goto(head);
        b.pop_breakable();
        b.switch_to(exit);
    }

    fn for_loop(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if let Some(value) = node.child_by_field_name("value") {
            self.walk(b, value);
        }
        self.loop_with(
            b,
            None,
            node.child_by_field_name("body"),
            node.child_by_field_name("pattern"),
        );
        Value::Unknown
    }

    fn case(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        if let Some(value) = node.child_by_field_name("value") {
            self.walk(b, value);
        }
        let arms: Vec<_> = named_children(node)
            .into_iter()
            .filter(|c| matches!(c.kind(), "when" | "else"))
            .collect();
        let exit = b.new_block();
        let blocks: Vec<_> = arms.iter().map(|_| b.new_block()).collect();
        let mut targets = blocks.clone();
        if !arms.iter().any(|arm| arm.kind() == "else") {
            targets.push(exit);
        }
        b.branch(&targets);
        for (arm, block) in arms.into_iter().zip(blocks) {
            b.switch_to(block);
            self.children(b, arm);
            b.goto(exit);
        }
        b.switch_to(exit);
        Value::Unknown
    }

    /// `begin .. rescue .. else .. ensure .. end`, and a method body with the
    /// same clauses.
    fn begin(&mut self, b: &mut IrBuilder, node: Node<'_>) -> Value {
        let parts = named_children(node);
        let rescues: Vec<_> = parts.iter().filter(|c| c.kind() == "rescue").collect();
        let ensure = parts.iter().find(|c| c.kind() == "ensure");
        let otherwise = parts.iter().find(|c| c.kind() == "else");
        if rescues.is_empty() && ensure.is_none() {
            for part in &parts {
                self.walk(b, *part);
            }
            return Value::Unknown;
        }
        let exit = b.new_block();
        let ensure_block = ensure.map(|_| b.new_block());
        let join = ensure_block.unwrap_or(exit);
        let handler = b.new_block();
        let rescue_blocks: Vec<_> = rescues.iter().map(|_| b.new_block()).collect();

        b.enter_try(handler);
        for part in parts
            .iter()
            .filter(|c| !matches!(c.kind(), "rescue" | "else" | "ensure"))
        {
            self.walk(b, *part);
        }
        b.leave_try();
        if let Some(otherwise) = otherwise {
            self.guarded(b, ensure_block, |this, b| {
                this.children(b, *otherwise);
            });
        }
        b.goto(join);

        // An exception no `rescue` takes still runs the `ensure`.
        let mut targets = rescue_blocks.clone();
        targets.extend(ensure_block);
        b.switch_to(handler);
        if targets.is_empty() {
            b.throw();
        } else {
            b.branch(&targets);
        }
        for (rescue, block) in rescues.into_iter().zip(rescue_blocks) {
            b.switch_to(block);
            if let Some(variable) = rescue.child_by_field_name("variable") {
                for target in named_children(variable) {
                    self.assign_targets(b, target);
                }
            }
            if let Some(body) = rescue.child_by_field_name("body") {
                self.guarded(b, ensure_block, |this, b| {
                    this.walk(b, body);
                });
            }
            b.goto(join);
        }
        if let (Some(ensure), Some(block)) = (ensure, ensure_block) {
            b.switch_to(block);
            self.children(b, *ensure);
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

    /// `(method, state)` of every `x.method` whose `x` is an identifier, in
    /// method-name order; `state` is `known T`, `unknown Cause` or `none`.
    fn receivers(source: &str) -> Vec<(String, String)> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_ruby::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let bytes = source.as_bytes();
        let found = analyze_file(
            &RubyLowering,
            &(),
            tree.root_node(),
            bytes,
            Path::new("t.rb"),
        );
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if n.kind() == "call" {
                if let Some(object) = call_receiver(n) {
                    let method = n.child_by_field_name("method").unwrap();
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

    /// The state of the one `x.method` call named `method`.
    fn state_of(source: &str, method: &str) -> String {
        let found: Vec<_> = receivers(source)
            .into_iter()
            .filter(|(name, _)| name == method)
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        found[0].1.clone()
    }

    fn only(source: &str) -> String {
        state_of(source, "m")
    }

    #[test]
    fn test_lower_constructor_assignment_is_known() {
        assert_eq!(only("def f\n  o = Foo.new\n  o.m\nend\n"), "known Foo");
    }

    #[test]
    fn test_lower_branches_with_two_classes_conflict() {
        assert_eq!(
            only("def f(c)\n  if c\n    o = A.new\n  else\n    o = B.new\n  end\n  o.m\nend\n"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_lower_branches_with_one_class_stay_known() {
        assert_eq!(
            only("def f(c)\n  if c\n    o = A.new\n  else\n    o = A.new\n  end\n  o.m\nend\n"),
            "known A"
        );
    }

    #[test]
    fn test_lower_parameter_has_no_evidence() {
        assert_eq!(only("def f(o)\n  o.m\nend\n"), "unknown NoEvidence");
    }

    #[test]
    fn test_lower_bare_method_call_is_not_a_binding() {
        assert_eq!(only("def f\n  helper.m\nend\n"), "none");
    }

    #[test]
    fn test_lower_block_write_poisons_the_outer_local() {
        assert_eq!(
            only("def f(xs)\n  o = A.new\n  xs.each { |x| o = B.new }\n  o.m\nend\n"),
            "unknown Escape"
        );
    }

    #[test]
    fn test_lower_read_only_block_keeps_the_type() {
        assert_eq!(
            only("def f(xs)\n  o = A.new\n  xs.each { |x| o.m }\nend\n"),
            "known A"
        );
    }

    #[test]
    fn test_lower_block_parameter_shadows_the_outer_local() {
        let source = "def f(xs)\n  o = A.new\n  xs.each { |o| o.inner }\n  o.outer\nend\n";
        assert_eq!(state_of(source, "inner"), "unknown NoEvidence");
        assert_eq!(state_of(source, "outer"), "known A");
    }

    #[test]
    fn test_lower_def_does_not_capture_outer_local() {
        assert_eq!(
            only("def f\n  o = A.new\n  def g\n    o.m\n  end\nend\n"),
            "none"
        );
    }

    #[test]
    fn test_lower_or_assign_keeps_the_type() {
        assert_eq!(
            only("def f\n  o = A.new\n  o ||= A.new\n  o.m\nend\n"),
            "known A"
        );
    }

    #[test]
    fn test_lower_multiple_assignment_rebinds_the_local() {
        assert_eq!(
            only("def f(p)\n  o = A.new\n  o, q = p\n  o.m\nend\n"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_loop_write_reaches_earlier_use() {
        assert_eq!(
            only("def f(xs)\n  o = A.new\n  while xs\n    o.m\n    o = xs\n  end\nend\n"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_lower_rescue_variable_has_no_evidence() {
        assert_eq!(
            only("def f\n  begin\n    g\n  rescue => e\n    e.m\n  end\nend\n"),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_lower_empty_method_body_records_nothing() {
        assert!(receivers("def f(o)\nend\n").is_empty());
    }

    #[test]
    fn test_lower_retry_leaves_function_to_extractor() {
        assert_eq!(
            only("def f\n  o = A.new\n  begin\n    g\n  rescue\n    retry\n  end\n  o.m\nend\n"),
            "none"
        );
    }

    #[test]
    fn test_walk_node_rightward_hash_pattern_rebinds_the_local() {
        assert_eq!(
            state_of(
                "def show(input)\n  user = Guest.new\n  input => {user:}\n  user.permissions\nend\n",
                "permissions"
            ),
            "unknown NoEvidence"
        );
    }

    #[test]
    fn test_walk_node_test_pattern_binds_on_the_matching_path_only() {
        assert_eq!(
            only("def f(input)\n  v = A.new\n  if input in [v, *rest]\n  end\n  v.m\nend\n"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_bind_pattern_splat_as_and_value_bind_but_pin_reads() {
        let source = "def g(i)\n  u = A.new\n  w = A.new\n  r = A.new\n  i => {k: ^u, n: Integer => w, **r}\n  u.pinned\n  w.aliased\n  r.rest\nend\n";
        assert_eq!(state_of(source, "pinned"), "known A");
        assert_eq!(state_of(source, "aliased"), "unknown NoEvidence");
        assert_eq!(state_of(source, "rest"), "unknown NoEvidence");
    }

    #[test]
    fn test_bind_pattern_constant_and_literal_bind_nothing() {
        assert_eq!(
            only("def f(i)\n  o = A.new\n  i => [Integer, 1, :sym]\n  o.m\nend\n"),
            "known A"
        );
    }

    #[test]
    fn test_call_string_instance_eval_havocs_caller_locals() {
        assert_eq!(
            state_of(
                "def f(settings, dsl_text)\n  store = MemoryStore.new\n  settings.instance_eval(dsl_text)\n  store.connect\nend\n",
                "connect"
            ),
            "unknown Unsupported"
        );
    }

    #[test]
    fn test_call_string_class_eval_and_kernel_eval_havoc_caller_locals() {
        for call in [
            "Object.class_eval(\"o = 3\")",
            "Kernel.eval(\"o = 3\")",
            "M.module_eval code",
        ] {
            let source = format!("def f(code)\n  o = A.new\n  {call}\n  o.m\nend\n");
            assert_eq!(only(&source), "unknown Unsupported", "{call}");
        }
    }

    #[test]
    fn test_call_block_instance_eval_keeps_caller_locals() {
        for call in [
            "settings.instance_eval { x }",
            "settings.instance_eval(&blk)",
        ] {
            let source = format!("def f(settings, blk)\n  o = A.new\n  {call}\n  o.m\nend\n");
            assert_eq!(only(&source), "known A", "{call}");
        }
    }

    #[test]
    fn test_begin_rescue_return_reaches_ensure() {
        assert_eq!(
            only("def f\n  o = A.new\n  begin\n    risky\n  rescue E => e\n    o = B.new\n    return\n  ensure\n    o.m\n  end\nend\n"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_begin_rescue_next_in_block_reaches_ensure() {
        assert_eq!(
            only("def f(xs)\n  xs.each do |x|\n    o = A.new\n    begin\n      risky\n    rescue\n      o = B.new\n      next\n    ensure\n      o.m\n    end\n  end\nend\n"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_begin_else_return_reaches_ensure() {
        assert_eq!(
            only("def f\n  o = A.new\n  begin\n    risky\n  rescue E\n    log\n  else\n    o = C.new\n    return\n  ensure\n    o.m\n  end\nend\n"),
            "unknown Conflict"
        );
    }

    #[test]
    fn test_begin_rescue_return_without_ensure_keeps_the_type() {
        assert_eq!(
            only("def f\n  o = A.new\n  begin\n    risky\n  rescue E\n    o = B.new\n    return\n  end\n  o.m\nend\n"),
            "known A"
        );
    }
}
