//! Lowers Python function bodies into the receiver IR, so one solve decides
//! the type of every `name.member` receiver on every path.
//!
//! Scope rules: a name binds where the body first writes it (a read before
//! that write still sees the module or an enclosing function); `global`
//! names bind in this function only from their first write on; `nonlocal`
//! names are the enclosing function's cell. A lambda or comprehension owns
//! its parameters and targets; a lambda default and the first iterable of a
//! comprehension run in the enclosing scope. A bare annotation types only the
//! value assigned at that point (`PointOfAssignment`).

use crate::framework_helpers::node_span;
use crate::receiver::ir::{
    BindingId, BindingOrigin, BlockId, CaptureMode, ScopeKind, TypeOrigin, Value,
};
use crate::receiver::lattice::UnknownCause;
use crate::receiver::{DeclaredTypePolicy, IrBuilder, LangPolicy, Lowered, Lowering, ScopeRule};
use rustc_hash::FxHashSet;
use tree_sitter::Node;

/// Expression and statement nesting the lowering recurses through before it
/// gives the function up (a generated `a + b + c + ...` chain).
const MAX_DEPTH: u32 = 400;

pub(super) struct PythonLowering;

impl Lowering for PythonLowering {
    type Cx<'a> = ();

    fn policy(&self) -> LangPolicy {
        LangPolicy {
            declared: DeclaredTypePolicy::PointOfAssignment,
            scope: ScopeRule::Function,
        }
    }

    /// A lambda or comprehension outside every `def` is its own root, so a
    /// module-level `lambda obj: obj.m()` still shadows `obj`.
    fn is_function(&self, node: Node<'_>) -> bool {
        matches!(
            node.kind(),
            "function_definition"
                | "lambda"
                | "list_comprehension"
                | "set_comprehension"
                | "dictionary_comprehension"
                | "generator_expression"
        )
    }

    fn lower(
        &self,
        _cx: &Self::Cx<'_>,
        function: Node<'_>,
        source: &[u8],
        b: &mut IrBuilder,
    ) -> Lowered {
        if !contains_attribute(function) {
            return Lowered::NoUses;
        }
        let mut lower = Lower {
            source,
            globals: Vec::new(),
            depth: 0,
            too_deep: false,
        };
        match function.kind() {
            "function_definition" => lower.function_body(b, function),
            "lambda" => lower.lambda_body(b, function),
            _ => lower.comprehension(b, function),
        }
        if lower.too_deep {
            Lowered::Unsupported("nesting depth")
        } else if b.use_count() == 0 {
            Lowered::NoUses
        } else {
            Lowered::Done
        }
    }
}

fn contains_attribute(root: Node<'_>) -> bool {
    let mut cursor = root.walk();
    loop {
        if cursor.node().kind() == "attribute" {
            return true;
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return false;
            }
        }
    }
}

fn contains_walrus(root: Node<'_>) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "named_expression" => return true,
            "lambda" => {}
            _ => {
                let mut cursor = node.walk();
                stack.extend(node.named_children(&mut cursor));
            }
        }
    }
    false
}

fn mentions(bytes: &[u8], word: &[u8]) -> bool {
    bytes.windows(word.len()).any(|window| window == word)
}

/// The names the body declares `global` and `nonlocal`, not looking into
/// nested scopes.
fn scan_declarations<'s>(body: Node<'_>, source: &'s [u8]) -> (Vec<&'s str>, Vec<&'s str>) {
    let (mut globals, mut nonlocals) = (Vec::new(), Vec::new());
    let text = &source[body.start_byte()..body.end_byte()];
    if !mentions(text, b"global") && !mentions(text, b"nonlocal") {
        return (globals, nonlocals);
    }
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        let names = match node.kind() {
            "function_definition" | "lambda" | "class_definition" => continue,
            "global_statement" => &mut globals,
            "nonlocal_statement" => &mut nonlocals,
            _ => {
                let mut cursor = node.walk();
                stack.extend(node.named_children(&mut cursor));
                continue;
            }
        };
        let mut cursor = node.walk();
        names.extend(
            node.named_children(&mut cursor)
                .filter_map(|name| name.utf8_text(source).ok()),
        );
    }
    (globals, nonlocals)
}

/// The names a function body binds anywhere in it, nested scopes aside.
/// Python makes each one local to the whole function, so a closure made
/// above the first assignment reads the local, not an enclosing variable.
fn local_names<'s>(body: Node<'_>, source: &'s [u8]) -> Vec<&'s str> {
    let mut out = Vec::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        let target = match node.kind() {
            "function_definition" | "lambda" | "class_definition" => continue,
            "assignment" | "augmented_assignment" | "for_statement" => {
                node.child_by_field_name("left")
            }
            "named_expression" => node.child_by_field_name("name"),
            "as_pattern" | "except_clause" | "except_group_clause" => {
                node.child_by_field_name("alias")
            }
            "delete_statement" => {
                let mut cursor = node.walk();
                for target in node.named_children(&mut cursor) {
                    bound_names(target, source, &mut out);
                }
                None
            }
            "case_pattern" => {
                case_capture_names(node, source, &mut out);
                None
            }
            _ => None,
        };
        if let Some(target) = target {
            bound_names(target, source, &mut out);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    out
}

/// Every name an assignment target, `for` target or parameter binds.
fn bound_names<'s>(node: Node<'_>, source: &'s [u8], out: &mut Vec<&'s str>) {
    match node.kind() {
        "identifier" => out.extend(node.utf8_text(source).ok()),
        "pattern_list"
        | "tuple_pattern"
        | "list_pattern"
        | "tuple"
        | "list"
        | "parenthesized_expression"
        | "as_pattern_target"
        | "list_splat_pattern"
        | "dictionary_splat_pattern" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                bound_names(child, source, out);
            }
        }
        _ => {}
    }
}

/// The names a `case` pattern captures: a bare name, `*rest` / `**rest`, and
/// the name after `as`. A dotted name is a value pattern; neither the class of
/// a class pattern nor the keyword of a keyword pattern binds.
fn case_capture_names<'s>(pattern: Node<'_>, source: &'s [u8], out: &mut Vec<&'s str>) {
    let skip = usize::from(matches!(
        pattern.kind(),
        "class_pattern" | "keyword_pattern"
    ));
    match pattern.kind() {
        "dotted_name" if pattern.named_child_count() == 1 => {
            if let Some(name) = pattern.named_child(0) {
                bound_names(name, source, out);
            }
        }
        "identifier" => bound_names(pattern, source, out),
        "case_pattern" | "as_pattern" | "list_pattern" | "tuple_pattern" | "union_pattern"
        | "splat_pattern" | "class_pattern" | "keyword_pattern" => {
            let mut cursor = pattern.walk();
            for child in pattern.named_children(&mut cursor).skip(skip) {
                case_capture_names(child, source, out);
            }
        }
        // A key is a literal or a value pattern; only values and `**rest`
        // capture.
        "dict_pattern" => {
            let mut cursor = pattern.walk();
            for child in pattern
                .named_children(&mut cursor)
                .filter(|c| matches!(c.kind(), "case_pattern" | "splat_pattern"))
            {
                case_capture_names(child, source, out);
            }
        }
        "as_pattern_target" => bound_names(pattern, source, out),
        _ => {}
    }
}

/// The type an annotation names when it is a single identifier. Generic,
/// subscripted and string annotations name nothing the resolver can match.
fn simple_type_name<'s>(type_node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    let inner = type_node.named_child(0).unwrap_or(type_node);
    if inner.kind() != "identifier" {
        return None;
    }
    let ty = inner.utf8_text(source).ok()?;
    (!ty.is_empty() && ty.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(ty)
}

/// The first parameter of a method declared in a class body (`self`, or
/// `cls` of a classmethod) and the class it stands for, so `self.m()` is
/// typed as `C.m` the way Java types `this.m()`. A `@staticmethod` has no
/// such parameter.
fn method_receiver<'s>(
    function: Node<'_>,
    params: Node<'_>,
    source: &'s [u8],
) -> Option<(&'s str, &'s str)> {
    let mut member = function;
    if let Some(decorated) = function
        .parent()
        .filter(|p| p.kind() == "decorated_definition")
    {
        let mut cursor = decorated.walk();
        let is_static = decorated.named_children(&mut cursor).any(|d| {
            d.kind() == "decorator"
                && d.utf8_text(source)
                    .is_ok_and(|t| t.trim_start_matches('@').trim() == "staticmethod")
        });
        if is_static {
            return None;
        }
        member = decorated;
    }
    let class = member
        .parent()
        .filter(|body| body.kind() == "block")?
        .parent()
        .filter(|class| class.kind() == "class_definition")?;
    let first = params.named_child(0)?;
    let receiver = match first.kind() {
        "identifier" => first,
        "typed_parameter" => first.named_child(0).filter(|n| n.kind() == "identifier")?,
        _ => return None,
    };
    Some((
        receiver.utf8_text(source).ok()?,
        class.child_by_field_name("name")?.utf8_text(source).ok()?,
    ))
}

/// Writes `value` to the innermost binding of `name`, declaring a local when
/// the body has none yet.
fn bind(b: &mut IrBuilder, name: &str, value: Value) {
    let binding = b
        .lookup(name)
        .unwrap_or_else(|| b.declare(name, BindingOrigin::Local, None));
    b.assign(binding, value);
}

struct Lower<'s> {
    source: &'s [u8],
    /// Per function being lowered: the names it declares `global`.
    globals: Vec<FxHashSet<&'s str>>,
    depth: u32,
    too_deep: bool,
}

impl<'s> Lower<'s> {
    fn is_global(&self, name: &str) -> bool {
        self.globals
            .last()
            .is_some_and(|names| names.contains(name))
    }

    /// The binding a read of `name` sees: a local, a comprehension variable,
    /// or an enclosing function's variable (an implicit by-reference
    /// capture). A module-level name has none, so the extractor keeps its
    /// import and class rules for it.
    fn read_binding(&self, b: &mut IrBuilder, name: &str) -> Option<BindingId> {
        if self.is_global(name) {
            b.lookup(name)
        } else {
            b.lookup_or_capture(name, CaptureMode::ByRef)
        }
    }

    /// Records the receiver occurrence `ident` when a binding of its name is visible.
    fn note_use(&self, b: &mut IrBuilder, ident: Node<'_>) {
        let Ok(name) = ident.utf8_text(self.source) else {
            return;
        };
        if let Some(binding) = self.read_binding(b, name) {
            b.use_binding(binding, ident.id());
        }
    }

    fn value_of(&self, b: &mut IrBuilder, expr: Node<'_>) -> Value {
        match expr.kind() {
            "identifier" => expr
                .utf8_text(self.source)
                .ok()
                .and_then(|name| self.read_binding(b, name))
                .map_or(Value::Unknown, Value::Copy),
            "parenthesized_expression" => expr
                .named_child(0)
                .map_or(Value::Unknown, |inner| self.value_of(b, inner)),
            _ => Value::Unknown,
        }
    }

    fn visit(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        if self.depth >= MAX_DEPTH {
            self.too_deep = true;
            return;
        }
        self.depth += 1;
        self.dispatch(b, node);
        self.depth -= 1;
    }

    fn visit_opt(&mut self, b: &mut IrBuilder, node: Option<Node<'_>>) {
        if let Some(node) = node {
            self.visit(b, node);
        }
    }

    fn children(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(b, child);
        }
    }

    fn dispatch(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        match node.kind() {
            "attribute" => match node.child_by_field_name("object") {
                Some(object) if object.kind() == "identifier" => self.note_use(b, object),
                object => self.visit_opt(b, object),
            },
            "call" => self.call(b, node),
            "assignment" => self.assignment(b, node),
            "augmented_assignment" => {
                self.visit_opt(b, node.child_by_field_name("right"));
                if let Some(left) = node.child_by_field_name("left") {
                    self.assign_target(b, left, Value::Unknown);
                }
            }
            "named_expression" => {
                self.visit_opt(b, node.child_by_field_name("value"));
                if let Some(name) = node.child_by_field_name("name") {
                    self.assign_target(b, name, Value::Unknown);
                }
            }
            "lambda" => self.lambda(b, node),
            "list_comprehension"
            | "set_comprehension"
            | "dictionary_comprehension"
            | "generator_expression" => self.comprehension(b, node),
            "boolean_operator" => self.boolean(b, node),
            "conditional_expression" => self.conditional(b, node),
            "if_statement" => self.if_statement(b, node),
            "for_statement" => self.for_statement(b, node),
            "while_statement" => self.while_statement(b, node),
            "try_statement" => self.try_statement(b, node),
            "match_statement" => self.match_statement(b, node),
            "as_pattern" => {
                let alias = node.child_by_field_name("alias");
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if Some(child.id()) != alias.map(|a| a.id()) {
                        self.visit(b, child);
                    }
                }
                if let Some(alias) = alias {
                    self.assign_target(b, alias, Value::Unknown);
                }
            }
            "return_statement" => {
                self.children(b, node);
                b.ret();
            }
            "raise_statement" => {
                self.children(b, node);
                b.throw();
            }
            "break_statement" => b.break_out(),
            "continue_statement" => b.continue_loop(),
            "function_definition" => self.nested_def(b, node),
            "class_definition" => self.nested_class(b, node),
            "delete_statement" => {
                let mut cursor = node.walk();
                for target in node.named_children(&mut cursor) {
                    self.assign_target(b, target, Value::Unknown);
                }
            }
            _ => self.children(b, node),
        }
    }

    fn call(&mut self, b: &mut IrBuilder, call: Node<'_>) {
        if let Some(function) = call.child_by_field_name("function") {
            let is_receiver_call = call.parent().is_some_and(|parent| {
                parent.kind() == "attribute"
                    && parent
                        .child_by_field_name("object")
                        .is_some_and(|object| object.id() == call.id())
            });
            // `make().bar()`: whether a local binds `make` decides how the
            // chain's inner call types the receiver.
            if function.kind() == "identifier" {
                if is_receiver_call {
                    self.note_use(b, function);
                }
            } else {
                self.visit(b, function);
            }
        }
        self.visit_opt(b, call.child_by_field_name("arguments"));
    }

    fn assignment(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let right = node.child_by_field_name("right");
        self.visit_opt(b, right);
        let plain = match right {
            Some(right) if right.kind() != "assignment" => self.value_of(b, right),
            _ => Value::Unknown,
        };
        let annotation = node
            .child_by_field_name("type")
            .and_then(|ty| simple_type_name(ty, self.source));
        let value = match annotation {
            Some(ty) => Value::Annotated {
                declared: b.intern(ty, TypeOrigin::Annotation),
                value: Box::new(plain),
            },
            None => plain,
        };
        if let Some(left) = node.child_by_field_name("left") {
            self.assign_target(b, left, value);
        }
    }

    /// Writes `value` to the name `target` spells; a destructuring target
    /// writes an unknown value to each of its names.
    fn assign_target(&mut self, b: &mut IrBuilder, target: Node<'_>, value: Value) {
        match target.kind() {
            "identifier" => {
                if let Ok(name) = target.utf8_text(self.source) {
                    bind(b, name, value);
                    // Any call may rebind a module global.
                    if self.is_global(name) {
                        if let Some(binding) = b.lookup(name) {
                            b.havoc(Some(binding), UnknownCause::Escape);
                        }
                    }
                }
            }
            "pattern_list"
            | "tuple_pattern"
            | "list_pattern"
            | "tuple"
            | "list"
            | "parenthesized_expression"
            | "expression_list"
            | "as_pattern_target"
            | "list_splat_pattern"
            | "dictionary_splat_pattern" => {
                let mut cursor = target.walk();
                for child in target.named_children(&mut cursor) {
                    self.assign_target(b, child, Value::Unknown);
                }
            }
            _ => self.visit(b, target),
        }
    }

    fn boolean(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let (left, right) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        );
        self.visit_opt(b, left);
        let Some(right) = right else { return };
        if !contains_walrus(right) {
            self.visit(b, right);
            return;
        }
        let (rhs, join) = (b.new_block(), b.new_block());
        b.branch(&[rhs, join]);
        b.switch_to(rhs);
        self.visit(b, right);
        b.goto(join);
        b.switch_to(join);
    }

    fn conditional(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let mut cursor = node.walk();
        let parts: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        let [body, condition, otherwise] = parts[..] else {
            self.children(b, node);
            return;
        };
        self.visit(b, condition);
        if !contains_walrus(body) && !contains_walrus(otherwise) {
            self.visit(b, body);
            self.visit(b, otherwise);
            return;
        }
        let (then_blk, else_blk, join) = (b.new_block(), b.new_block(), b.new_block());
        b.branch(&[then_blk, else_blk]);
        b.switch_to(then_blk);
        self.visit(b, body);
        b.goto(join);
        b.switch_to(else_blk);
        self.visit(b, otherwise);
        b.goto(join);
        b.switch_to(join);
    }

    fn if_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let mut cursor = node.walk();
        let mut arms = vec![(
            node.child_by_field_name("condition"),
            node.child_by_field_name("consequence"),
        )];
        for alternative in node.children_by_field_name("alternative", &mut cursor) {
            arms.push(match alternative.kind() {
                "elif_clause" => (
                    alternative.child_by_field_name("condition"),
                    alternative.child_by_field_name("consequence"),
                ),
                _ => (None, alternative.child_by_field_name("body")),
            });
        }
        let join = b.new_block();
        for (condition, body) in arms {
            match condition {
                Some(condition) => {
                    self.visit(b, condition);
                    let (then_blk, next) = (b.new_block(), b.new_block());
                    b.branch(&[then_blk, next]);
                    b.switch_to(then_blk);
                    self.visit_opt(b, body);
                    b.goto(join);
                    b.switch_to(next);
                }
                None => self.visit_opt(b, body),
            }
        }
        b.goto(join);
        b.switch_to(join);
    }

    fn while_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let (head, body, exit) = (b.new_block(), b.new_block(), b.new_block());
        let alternative = node.child_by_field_name("alternative");
        let else_blk = alternative.map(|_| b.new_block());
        b.goto(head);
        b.switch_to(head);
        self.visit_opt(b, node.child_by_field_name("condition"));
        b.branch(&[body, else_blk.unwrap_or(exit)]);
        b.push_breakable(Some(head), exit);
        b.switch_to(body);
        self.visit_opt(b, node.child_by_field_name("body"));
        b.goto(head);
        b.pop_breakable();
        if let (Some(alternative), Some(else_blk)) = (alternative, else_blk) {
            b.switch_to(else_blk);
            self.visit(b, alternative);
            b.goto(exit);
        }
        b.switch_to(exit);
    }

    fn for_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        self.visit_opt(b, node.child_by_field_name("right"));
        let (head, body, exit) = (b.new_block(), b.new_block(), b.new_block());
        let alternative = node.child_by_field_name("alternative");
        let else_blk = alternative.map(|_| b.new_block());
        b.goto(head);
        b.switch_to(head);
        b.branch(&[body, else_blk.unwrap_or(exit)]);
        b.push_breakable(Some(head), exit);
        b.switch_to(body);
        if let Some(left) = node.child_by_field_name("left") {
            self.assign_target(b, left, Value::Unknown);
        }
        self.visit_opt(b, node.child_by_field_name("body"));
        b.goto(head);
        b.pop_breakable();
        if let (Some(alternative), Some(else_blk)) = (alternative, else_blk) {
            b.switch_to(else_blk);
            self.visit(b, alternative);
            b.goto(exit);
        }
        b.switch_to(exit);
    }

    fn try_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let mut cursor = node.walk();
        let (mut clauses, mut otherwise, mut finally) = (Vec::new(), None, None);
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "except_clause" | "except_group_clause" => clauses.push(child),
                "else_clause" => otherwise = Some(child),
                "finally_clause" => finally = Some(child),
                _ => {}
            }
        }
        let handler = b.new_block();
        // `finally` lowers inline here; created before `enter_try`, it
        // inherits the outer handler.
        let after = b.new_block();
        let finally_entry = finally.map(|_| after);
        b.enter_try(handler);
        self.visit_opt(b, node.child_by_field_name("body"));
        b.leave_try();
        self.guarded(b, finally_entry, |this, b| this.visit_opt(b, otherwise));
        b.goto(after);

        let clause_blocks: Vec<_> = clauses.iter().map(|_| b.new_block()).collect();
        let mut targets = clause_blocks.clone();
        if finally.is_some() || targets.is_empty() {
            targets.push(after);
        }
        b.switch_to(handler);
        b.branch(&targets);
        for (clause, block) in clauses.into_iter().zip(clause_blocks) {
            b.switch_to(block);
            self.guarded(b, finally_entry, |this, b| this.except_clause(b, clause));
            b.goto(after);
        }
        b.switch_to(after);
        self.visit_opt(b, finally);
    }

    /// Runs `body` so that its abrupt exits (`return`, `raise`, `break`,
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

    fn except_clause(&mut self, b: &mut IrBuilder, clause: Node<'_>) {
        let mut cursor = clause.walk();
        let mut body = None;
        for child in clause.named_children(&mut cursor) {
            if child.kind() == "block" {
                body = Some(child);
            } else {
                self.visit(b, child);
            }
        }
        if let Some(alias) = clause.child_by_field_name("alias") {
            self.assign_target(b, alias, Value::Unknown);
        }
        self.visit_opt(b, body);
    }

    fn match_statement(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let mut cursor = node.walk();
        for subject in node.children_by_field_name("subject", &mut cursor) {
            self.visit(b, subject);
        }
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let mut cursor = body.walk();
        let cases: Vec<Node<'_>> = body
            .named_children(&mut cursor)
            .filter(|case| case.kind() == "case_clause")
            .collect();
        let join = b.new_block();
        let case_blocks: Vec<_> = cases.iter().map(|_| b.new_block()).collect();
        let mut targets = case_blocks.clone();
        targets.push(join);
        b.branch(&targets);
        for (case, block) in cases.into_iter().zip(case_blocks) {
            b.switch_to(block);
            let mut cursor = case.walk();
            let mut names = Vec::new();
            for pattern in case
                .named_children(&mut cursor)
                .filter(|child| child.kind() == "case_pattern")
            {
                case_capture_names(pattern, self.source, &mut names);
            }
            for name in names {
                bind(b, name, Value::Unknown);
            }
            self.visit_opt(b, case.child_by_field_name("guard"));
            self.visit_opt(b, case.child_by_field_name("consequence"));
            b.goto(join);
        }
        b.switch_to(join);
    }

    fn lambda(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        if let Some(params) = node.child_by_field_name("parameters") {
            self.visit_defaults(b, params);
        }
        b.closure(node_span(&node), |inner| self.lambda_body(inner, node));
    }

    fn lambda_body(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        self.globals.push(FxHashSet::default());
        if let Some(params) = node.child_by_field_name("parameters") {
            self.declare_params(b, params, None);
        }
        self.visit_opt(b, node.child_by_field_name("body"));
        self.globals.pop();
    }

    /// Parameter defaults run in the scope that creates the function.
    fn visit_defaults(&mut self, b: &mut IrBuilder, params: Node<'_>) {
        let mut cursor = params.walk();
        for param in params.named_children(&mut cursor) {
            if matches!(
                param.kind(),
                "default_parameter" | "typed_default_parameter"
            ) {
                self.visit_opt(b, param.child_by_field_name("value"));
            }
        }
    }

    fn declare_params(
        &mut self,
        b: &mut IrBuilder,
        params: Node<'_>,
        receiver: Option<(&str, &str)>,
    ) {
        let mut cursor = params.walk();
        for param in params.named_children(&mut cursor) {
            let (names, annotation) = match param.kind() {
                "typed_parameter" => (param.named_child(0), param.child_by_field_name("type")),
                "default_parameter" | "typed_default_parameter" => {
                    (param.child_by_field_name("name"), None)
                }
                _ => (Some(param), None),
            };
            let mut bound = Vec::new();
            if let Some(names) = names {
                bound_names(names, self.source, &mut bound);
            }
            let annotated = names
                .filter(|names| names.kind() == "identifier")
                .and(annotation)
                .and_then(|ty| simple_type_name(ty, self.source));
            for name in bound {
                let declared = match (annotated, receiver) {
                    (Some(ty), _) => Some(b.intern(ty, TypeOrigin::Annotation)),
                    (None, Some((receiver, class))) if receiver == name => {
                        Some(b.intern(class, TypeOrigin::SelfType))
                    }
                    _ => None,
                };
                b.declare(name, BindingOrigin::Param, declared);
            }
        }
    }

    fn comprehension(&mut self, b: &mut IrBuilder, node: Node<'_>) {
        let mut cursor = node.walk();
        let clauses: Vec<Node<'_>> = node
            .named_children(&mut cursor)
            .filter(|clause| matches!(clause.kind(), "for_in_clause" | "if_clause"))
            .collect();
        let mut scope = None;
        for clause in clauses {
            if clause.kind() == "if_clause" {
                self.children(b, clause);
                continue;
            }
            // The first iterable runs before the scope opens.
            let mut cursor = clause.walk();
            let iterables: Vec<Node<'_>> = clause
                .children_by_field_name("right", &mut cursor)
                .filter(|iterable| iterable.is_named())
                .collect();
            for iterable in iterables {
                self.visit(b, iterable);
            }
            let scope = *scope.get_or_insert_with(|| b.push_scope(ScopeKind::Comprehension));
            let mut names = Vec::new();
            if let Some(left) = clause.child_by_field_name("left") {
                bound_names(left, self.source, &mut names);
            }
            for name in names {
                let binding = b.declare_in(scope, name, BindingOrigin::ForEach, None);
                b.assign(binding, Value::Unknown);
            }
        }
        self.visit_opt(b, node.child_by_field_name("body"));
        if scope.is_some() {
            b.pop_scope();
        }
    }

    fn nested_def(&mut self, b: &mut IrBuilder, def: Node<'_>) {
        if let Some(params) = def.child_by_field_name("parameters") {
            self.visit_defaults(b, params);
        }
        b.closure(node_span(&def), |inner| self.function_body(inner, def));
    }

    /// Methods of a class declared inside a function close over its variables;
    /// the class body's own statements bind class attributes and are skipped.
    fn nested_class(&mut self, b: &mut IrBuilder, class: Node<'_>) {
        self.visit_opt(b, class.child_by_field_name("superclasses"));
        let Some(body) = class.child_by_field_name("body") else {
            return;
        };
        let mut cursor = body.walk();
        for member in body.named_children(&mut cursor) {
            if matches!(
                member.kind(),
                "function_definition" | "decorated_definition" | "class_definition"
            ) {
                self.visit(b, member);
            }
        }
    }

    fn function_body(&mut self, b: &mut IrBuilder, def: Node<'_>) {
        let body = def.child_by_field_name("body");
        let (globals, nonlocals) = body.map_or_else(Default::default, |body| {
            scan_declarations(body, self.source)
        });
        self.globals.push(globals.into_iter().collect());
        if let Some(params) = def.child_by_field_name("parameters") {
            let receiver = method_receiver(def, params, self.source);
            self.declare_params(b, params, receiver);
        }
        for name in nonlocals {
            b.capture(name, CaptureMode::ByRef);
        }
        if let Some(body) = body {
            for name in local_names(body, self.source) {
                if !self.is_global(name) && b.lookup(name).is_none() {
                    b.declare(name, BindingOrigin::Local, None);
                }
            }
        }
        self.visit_opt(b, body);
        self.globals.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receiver::lattice::UnknownCause;
    use crate::receiver::{analyze_file, Receiver};
    use std::path::Path;

    /// `(object text, receiver)` of every `name.member` whose object is a
    /// plain name, in source order.
    fn receivers(source: &str) -> Vec<(String, Option<Receiver>)> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let found = analyze_file(
            &PythonLowering,
            &(),
            tree.root_node(),
            source.as_bytes(),
            Path::new("t.py"),
        );
        let mut out = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "attribute" {
                if let Some(object) = node
                    .child_by_field_name("object")
                    .filter(|object| object.kind() == "identifier")
                {
                    let text = object.utf8_text(source.as_bytes()).unwrap().to_string();
                    out.push((
                        object.start_byte(),
                        text,
                        found.receiver_at(object).cloned(),
                    ));
                }
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
        out.sort_by_key(|(start, ..)| *start);
        out.into_iter().map(|(_, text, r)| (text, r)).collect()
    }

    fn unknown(cause: UnknownCause) -> Option<Receiver> {
        Some(Receiver::Unknown(cause))
    }

    fn same_type(got: &Option<Receiver>, ty: &str) -> bool {
        matches!(got, Some(Receiver::Known(t)) if &*t.text == ty)
    }

    #[test]
    fn test_lower_annotated_parameter_returns_its_type() {
        let got = receivers("def f(x: A):\n    x.m()\n");
        assert!(same_type(&got[0].1, "A"), "{got:?}");
    }

    #[test]
    fn test_lower_untyped_parameter_returns_no_evidence() {
        let got = receivers("def f(x):\n    x.m()\n");
        assert_eq!(got[0].1, unknown(UnknownCause::NoEvidence));
    }

    #[test]
    fn test_lower_annotation_in_sibling_branch_keeps_the_copied_value() {
        let got = receivers(
            "def f(c, loose):\n    obj = loose\n    if c:\n        obj: A = A()\n    else:\n        obj.m()\n",
        );
        assert_eq!(got[0].1, unknown(UnknownCause::NoEvidence));
    }

    #[test]
    fn test_lower_annotation_on_one_branch_only_returns_conflict_at_join() {
        let got = receivers("def f(c):\n    if c:\n        obj: A = A()\n    obj.m()\n");
        assert_eq!(got[0].1, unknown(UnknownCause::Conflict));
    }

    #[test]
    fn test_lower_annotation_on_both_branches_returns_the_type() {
        let got = receivers(
            "def f(c):\n    if c:\n        obj: A = A()\n    else:\n        obj: A = A()\n    obj.m()\n",
        );
        assert!(same_type(&got[0].1, "A"), "{got:?}");
    }

    #[test]
    fn test_lower_read_before_rebinding_keeps_type_and_read_after_loses_it() {
        let got = receivers("def f(obj: A, loose):\n    obj.m()\n    obj = loose\n    obj.n()\n");
        assert!(same_type(&got[0].1, "A"), "{got:?}");
        assert_eq!(got[1].1, unknown(UnknownCause::NoEvidence));
    }

    #[test]
    fn test_lower_global_name_binds_no_receiver_until_written() {
        let got = receivers("def f(v):\n    global x\n    x.m()\n    x = v\n    x.n()\n");
        assert_eq!(got[0].1, None);
        assert_eq!(got[1].1, unknown(UnknownCause::Escape));
    }

    #[test]
    fn test_lower_lambda_default_and_first_iterable_read_the_enclosing_type() {
        let got = receivers("def f(obj: A):\n    return lambda obj=obj.m: obj\n");
        assert!(same_type(&got[0].1, "A"), "{got:?}");
        let got = receivers("def f(obj: A):\n    return [o for o in obj.m]\n");
        assert!(same_type(&got[0].1, "A"), "{got:?}");
    }

    #[test]
    fn test_lower_comprehension_variable_shadows_the_enclosing_type() {
        let got = receivers("def f(obj: A, xs):\n    return [obj.m for obj in xs]\n");
        assert_eq!(got[0].1, unknown(UnknownCause::NoEvidence));
    }

    #[test]
    fn test_lower_tuple_target_writes_unknown_to_each_name() {
        let got = receivers("def f(a: A):\n    a, b = g()\n    a.m()\n    b.n()\n");
        assert_eq!(got[0].1, unknown(UnknownCause::NoEvidence));
        assert_eq!(got[1].1, unknown(UnknownCause::NoEvidence));
    }

    #[test]
    fn test_lower_nonlocal_write_in_closure_makes_the_receiver_unknown() {
        let got = receivers(
            "def outer(obj: A):\n    def inner():\n        nonlocal obj\n        obj = g()\n        obj.m()\n    return inner\n",
        );
        assert!(matches!(got[0].1, Some(Receiver::Unknown(_))), "{got:?}");
    }

    #[test]
    fn test_lower_read_only_capture_keeps_the_outer_type() {
        let got = receivers(
            "def outer(obj: A):\n    def inner():\n        return obj.m\n    return inner\n",
        );
        assert!(same_type(&got[0].1, "A"), "{got:?}");
    }

    #[test]
    fn test_lower_method_self_returns_the_enclosing_class() {
        let got = receivers("class C:\n    def m(self):\n        self.n()\n");
        assert!(same_type(&got[0].1, "C"), "{got:?}");
    }

    #[test]
    fn test_lower_staticmethod_first_parameter_is_untyped() {
        let got = receivers("class C:\n    @staticmethod\n    def m(self):\n        self.n()\n");
        assert_eq!(got[0].1, unknown(UnknownCause::NoEvidence));
    }

    #[test]
    fn test_lower_module_level_lambda_parameter_is_a_binding() {
        let got = receivers("f = lambda obj: obj.m\n");
        assert_eq!(got[0].1, unknown(UnknownCause::NoEvidence));
    }

    #[test]
    fn test_lower_module_name_and_function_without_attribute_record_nothing() {
        let got = receivers("import os\n\ndef f():\n    os.path.join()\n");
        assert!(got.iter().all(|(_, r)| r.is_none()), "{got:?}");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        let source = "def f():\n    pass\n";
        let tree = parser.parse(source, None).unwrap();
        let found = analyze_file(
            &PythonLowering,
            &(),
            tree.root_node(),
            source.as_bytes(),
            Path::new("t.py"),
        );
        assert!(found.is_empty());
    }

    #[test]
    fn test_lower_for_target_after_loop_joins_to_conflict() {
        let got = receivers("def f(obj: A, xs):\n    for obj in xs:\n        pass\n    obj.m()\n");
        assert_eq!(got[0].1, unknown(UnknownCause::Conflict));
    }

    #[test]
    fn test_lower_except_alias_writes_unknown() {
        let got = receivers(
            "def f(obj: A):\n    try:\n        pass\n    except ValueError as obj:\n        obj.m()\n",
        );
        assert_eq!(got[0].1, unknown(UnknownCause::NoEvidence));
    }

    #[test]
    fn test_try_statement_except_raise_reaches_finally() {
        let got = receivers(
            "def f():\n    obj: A = A()\n    try:\n        risky()\n    except ValueError:\n        obj: B = B()\n        raise\n    finally:\n        obj.m()\n",
        );
        assert_eq!(got[0].1, unknown(UnknownCause::Conflict));
    }

    #[test]
    fn test_try_statement_except_return_reaches_finally() {
        let got = receivers(
            "def f():\n    obj: A = A()\n    try:\n        risky()\n    except ValueError:\n        obj: B = B()\n        return\n    finally:\n        obj.m()\n",
        );
        assert_eq!(got[0].1, unknown(UnknownCause::Conflict));
    }

    #[test]
    fn test_try_statement_except_break_reaches_finally() {
        let got = receivers(
            "def f(xs):\n    obj: A = A()\n    for x in xs:\n        try:\n            risky()\n        except ValueError:\n            obj: B = B()\n            break\n        finally:\n            obj.m()\n",
        );
        assert_eq!(got[0].1, unknown(UnknownCause::Conflict));
    }

    #[test]
    fn test_try_statement_else_return_reaches_finally() {
        let got = receivers(
            "def f():\n    obj: A = A()\n    try:\n        risky()\n    except ValueError:\n        pass\n    else:\n        obj: C = C()\n        return\n    finally:\n        obj.m()\n",
        );
        assert_eq!(got[0].1, unknown(UnknownCause::Conflict));
    }

    #[test]
    fn test_try_statement_except_return_without_finally_keeps_the_type() {
        let got = receivers(
            "def f():\n    obj: A = A()\n    try:\n        risky()\n    except ValueError:\n        obj: B = B()\n        return\n    obj.m()\n",
        );
        assert!(same_type(&got[0].1, "A"), "{got:?}");
    }

    #[test]
    fn test_function_body_closure_above_assignment_reads_the_local() {
        let got = receivers(
            "def outer(a: A, c: B):\n    o = a\n    def inner():\n        g = lambda: o.m()\n        o = c\n        return g()\n    return inner\n",
        );
        assert_eq!(got[0].1, unknown(UnknownCause::Conflict));
    }

    #[test]
    fn test_function_body_name_bound_only_in_inner_scope_is_still_captured() {
        let got = receivers(
            "def outer(o: A):\n    def inner():\n        g = lambda: o.m()\n        return [o for o in g()]\n    return inner\n",
        );
        assert!(same_type(&got[0].1, "A"), "{got:?}");
    }

    #[test]
    fn test_assign_target_global_write_escapes() {
        let got = receivers("def f(a: A):\n    global x\n    x = a\n    reset()\n    x.m()\n");
        assert_eq!(got[0].1, unknown(UnknownCause::Escape));
    }
}
