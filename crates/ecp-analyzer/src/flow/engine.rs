use super::ast::Ast;
use super::{Boundary, Budgets, FlowEdge, FlowNode, SourceFile};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct Value {
    pub(super) ids: BTreeSet<usize>,
    pub(super) functions: BTreeSet<usize>,
    pub(super) objects: BTreeSet<usize>,
}
impl Value {
    pub(super) fn merge(&mut self, other: &Self) {
        self.ids.extend(&other.ids);
        self.functions.extend(&other.functions);
        self.objects.extend(&other.objects);
    }
}
#[derive(Clone)]
pub(super) struct Function {
    pub(super) ast: usize,
    pub(super) scopes: Vec<usize>,
    pub(super) defaults: BTreeMap<String, Value>,
}
pub(super) type Scope = BTreeMap<String, Value>;
pub(super) type State = (Vec<Scope>, Vec<Scope>);
/// One `(condition, body)` arm of an if / elif / ternary.
pub(super) type Arm = (Option<usize>, Option<usize>);
pub(super) struct Engine<'a> {
    pub(super) files: &'a [SourceFile],
    pub(super) ast: Vec<Ast>,
    pub(super) roots: Vec<Option<usize>>,
    pub(super) scopes: Vec<Scope>,
    pub(super) function_scopes: BTreeSet<usize>,
    pub(super) references: BTreeMap<(usize, String), (usize, String)>,
    pub(super) functions: Vec<Function>,
    pub(super) objects: Vec<Scope>,
    pub(super) nodes: Vec<FlowNode>,
    pub(super) origins: Vec<usize>,
    pub(super) edges: BTreeSet<FlowEdge>,
    pub(super) boundaries: BTreeSet<Boundary>,
    pub(super) budgets: Budgets,
    pub(super) steps: usize,
    pub(super) truncated: bool,
    pub(super) active: Vec<usize>,
    pub(super) active_returns: Vec<Value>,
    pub(super) active_parameters: Vec<Vec<Value>>,
    pub(super) exit_states: Vec<Vec<(Vec<Scope>, Vec<Scope>)>>,
    pub(super) called: BTreeSet<usize>,
    pub(super) controls: Vec<Value>,
    /// Identity caches for the innermost loop, keyed by call context and AST
    /// node: iteration k reuses what iteration k-1 created at the same point.
    pub(super) loop_nodes: Option<BTreeMap<(usize, usize, String), usize>>,
    pub(super) loop_objects: Option<BTreeMap<(usize, usize), usize>>,
    pub(super) loop_functions: Option<BTreeMap<(usize, usize), usize>>,
    /// Calls made so far in the current iteration; each call's ordinal is its
    /// context, so two call sites stay distinct and one site converges.
    pub(super) loop_call: usize,
    pub(super) loop_ctx: Vec<usize>,
    /// (importer, dependency) file pairs resolved during module initialisation.
    pub(super) links: BTreeSet<(usize, usize)>,
}
impl Engine<'_> {
    pub(super) fn field(&self, n: usize, key: &str) -> Option<usize> {
        self.ast[n].fields.get(key).copied()
    }
    pub(super) fn boundary(&mut self, n: usize, kind: &str, message: &str) {
        let a = &self.ast[n];
        self.boundaries.insert(Boundary {
            file: self.files[a.file].path.clone(),
            line: a.line,
            column: a.column,
            kind: kind.into(),
            message: message.into(),
        });
    }
    pub(super) fn node(&mut self, n: usize, kind: &str, inputs: &Value) -> Value {
        let ctx = self.loop_ctx();
        if let Some(id) = self
            .loop_nodes
            .as_ref()
            .and_then(|cache| cache.get(&(ctx, n, kind.into())))
            .copied()
        {
            for from in &inputs.ids {
                self.edges.insert(FlowEdge {
                    from: *from,
                    to: id,
                    kind: "value".into(),
                });
            }
            for control in &self.controls {
                for from in &control.ids {
                    self.edges.insert(FlowEdge {
                        from: *from,
                        to: id,
                        kind: "control".into(),
                    });
                }
            }
            return Value {
                ids: BTreeSet::from([id]),
                functions: inputs.functions.clone(),
                objects: inputs.objects.clone(),
            };
        }
        if self.nodes.len() >= self.budgets.max_nodes {
            self.truncated = true;
            return Value::default();
        }
        let a = &self.ast[n];
        let id = self.nodes.len();
        self.nodes.push(FlowNode {
            id,
            kind: kind.into(),
            label: a.text.chars().take(160).collect(),
            file: self.files[a.file].path.clone(),
            line: a.line,
            column: a.column,
            end_line: a.end_line,
            end_column: a.end_column,
        });
        self.origins.push(n);
        if let Some(cache) = self.loop_nodes.as_mut() {
            cache.insert((ctx, n, kind.into()), id);
        }
        for from in &inputs.ids {
            self.edges.insert(FlowEdge {
                from: *from,
                to: id,
                kind: "value".into(),
            });
        }
        for control in &self.controls {
            for from in &control.ids {
                self.edges.insert(FlowEdge {
                    from: *from,
                    to: id,
                    kind: "control".into(),
                });
            }
        }
        Value {
            ids: BTreeSet::from([id]),
            functions: inputs.functions.clone(),
            objects: inputs.objects.clone(),
        }
    }
    pub(super) fn lookup(&self, name: &str, scopes: &[usize]) -> Value {
        scopes
            .iter()
            .rev()
            .find_map(|s| {
                if let Some((scope, name)) = self.references.get(&(*s, name.into())) {
                    self.scopes[*scope].get(name)
                } else {
                    self.scopes[*s].get(name)
                }
            })
            .cloned()
            .unwrap_or_default()
    }
    pub(super) fn assign(
        &mut self,
        n: usize,
        value: Value,
        scopes: &[usize],
        declare: bool,
    ) -> Value {
        let a = self.ast[n].clone();
        if matches!(a.kind.as_str(), "identifier" | "variable_name" | "name") {
            let result = self.node(n, "binding", &value);
            let scope = if declare {
                *scopes.last().unwrap()
            } else {
                scopes
                    .iter()
                    .rev()
                    .find(|s| self.scopes[**s].contains_key(&a.text))
                    .copied()
                    .unwrap_or(*scopes.last().unwrap())
            };
            let (scope, name) = self
                .references
                .get(&(scope, a.text.clone()))
                .cloned()
                .unwrap_or((scope, a.text));
            self.scopes[scope].insert(name, result.clone());
            return result;
        }
        if matches!(
            a.kind.as_str(),
            "member_expression" | "attribute" | "member_access_expression" | "subscript_expression"
        ) {
            let (base, key) = self.member(n, scopes);
            let result = self.node(n, "field_write", &value);
            if let Some(key) = key {
                if base.objects.is_empty() {
                    self.boundary(
                        n,
                        "unknown_alias",
                        "The receiver has no resolved allocation.",
                    );
                }
                let weak = base.objects.len() > 1
                    || base.objects.iter().any(|obj| self.is_loop_summary(*obj));
                for obj in base.objects {
                    if weak {
                        self.objects[obj]
                            .entry(key.clone())
                            .or_default()
                            .merge(&result);
                    } else {
                        self.objects[obj].insert(key.clone(), result.clone());
                    }
                }
            } else {
                self.boundary(
                    n,
                    "dynamic_field",
                    "Computed property identity is unresolved.",
                );
            }
            return result;
        }
        self.boundary(
            n,
            "destructuring",
            "Destructuring targets require language-specific binding semantics.",
        );
        self.node(n, "binding", &value)
    }
    pub(super) fn member(&mut self, n: usize, scopes: &[usize]) -> (Value, Option<String>) {
        let a = self.ast[n].clone();
        let base = self
            .field(n, "object")
            .or_else(|| a.children.first().copied());
        let property = self
            .field(n, "property")
            .or_else(|| self.field(n, "attribute"))
            .or_else(|| self.field(n, "name"))
            .or_else(|| a.children.get(1).copied());
        let value = base.map(|b| self.eval(b, scopes)).unwrap_or_default();
        let key = property.and_then(|p| {
            let a = &self.ast[p];
            if self.ast[n].kind == "subscript_expression"
                && !matches!(a.kind.as_str(), "string" | "integer" | "number")
            {
                return None;
            }
            if matches!(
                a.kind.as_str(),
                "property_identifier" | "identifier" | "name" | "string" | "integer" | "number"
            ) {
                Some(a.text.trim_matches(['\'', '"']).into())
            } else {
                None
            }
        });
        (value, key)
    }
    pub(super) fn function(&mut self, n: usize, scopes: &[usize]) -> Value {
        // Loop iterations reuse the callable identity, like they reuse nodes, so a
        // closure created in a loop body does not defeat the fixed point. Arrow
        // functions capture scope by value at creation, so the reused closure
        // keeps the first iteration's captured values in later iterations. The
        // alternative never converged, which set `truncated` and skipped every
        // remaining uncalled function. Per-iteration capture precision, if ever
        // needed, refreshes the captured scope on reuse instead of allocating a
        // new `Function`.
        let ctx = self.loop_ctx();
        if let Some(id) = self
            .loop_functions
            .as_ref()
            .and_then(|cache| cache.get(&(ctx, n)))
        {
            let id = *id;
            let mut v = self.node(n, "function", &Value::default());
            v.functions.insert(id);
            return v;
        }
        if self.ast[n].text.starts_with("async ") {
            self.boundary(
                n,
                "async",
                "Async function results and scheduling require promise or coroutine semantics.",
            );
        }
        let path = &self.files[self.ast[n].file].path;
        let python = path.ends_with(".py") || path.ends_with(".pyi");
        let php = path.ends_with(".php") || path.ends_with(".phtml");
        let mut capture_scopes = scopes.to_vec();
        if php {
            let mut captured = Scope::new();
            let mut references = Vec::new();
            for scope in scopes {
                for (name, value) in &self.scopes[*scope] {
                    if !name.starts_with('$') {
                        captured.insert(name.clone(), value.clone());
                    }
                }
            }
            let uses = self.ast[n]
                .children
                .iter()
                .copied()
                .find(|c| self.ast[*c].kind == "anonymous_function_use_clause");
            if let Some(uses) = uses {
                for c in self.ast[uses].children.clone() {
                    if self.ast[c].kind == "variable_name" {
                        captured.insert(
                            self.ast[c].text.clone(),
                            self.lookup(&self.ast[c].text, scopes),
                        );
                    } else if self.ast[c].kind == "by_ref" {
                        if let Some(name) = self.ast[c]
                            .children
                            .iter()
                            .copied()
                            .find(|n| self.ast[*n].kind == "variable_name")
                        {
                            let name = self.ast[name].text.clone();
                            if let Some(scope) = scopes
                                .iter()
                                .rev()
                                .find(|s| self.scopes[**s].contains_key(&name))
                            {
                                references.push((name.clone(), *scope));
                                captured.insert(name, Value::default());
                            }
                        }
                    } else {
                        self.boundary(
                            c,
                            "reference_capture",
                            "PHP reference captures have unresolved write-back semantics.",
                        );
                    }
                }
            }
            if self.ast[n].kind == "arrow_function" {
                for scope in scopes {
                    for (name, value) in &self.scopes[*scope] {
                        captured.insert(name.clone(), value.clone());
                    }
                }
            }
            let scope = self.scopes.len();
            self.scopes.push(captured);
            for (name, source) in references {
                self.references
                    .insert((scope, name.clone()), (source, name));
            }
            capture_scopes = vec![scope];
        }
        let mut defaults = BTreeMap::new();
        if python {
            if let Some(params) = self.field(n, "parameters") {
                for param in self.ast[params].children.clone() {
                    if let (Some(name), Some(value)) =
                        (self.field(param, "name"), self.field(param, "value"))
                    {
                        defaults.insert(self.ast[name].text.clone(), self.eval(value, scopes));
                    }
                }
            }
        }
        let id = self.functions.len();
        self.functions.push(Function {
            ast: n,
            scopes: capture_scopes,
            defaults,
        });
        if let Some(cache) = self.loop_functions.as_mut() {
            cache.insert((ctx, n), id);
        }
        let mut v = self.node(n, "function", &Value::default());
        v.functions.insert(id);
        v
    }
    pub(super) fn is_function(&self, n: usize) -> bool {
        matches!(
            self.ast[n].kind.as_str(),
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "function_definition"
                | "lambda"
                | "anonymous_function"
                | "anonymous_function_creation_expression"
        )
    }
    pub(super) fn predeclare(&mut self, n: usize, scopes: &[usize]) {
        if self.files[self.ast[n].file].path.ends_with(".py") {
            return;
        }
        for c in self.ast[n].children.clone() {
            let c = if self.ast[c].kind == "export_statement" {
                self.field(c, "declaration")
                    .or_else(|| self.ast[c].children.first().copied())
                    .unwrap_or(c)
            } else {
                c
            };
            if self.is_function(c) {
                if let Some(name) = self.field(c, "name") {
                    let key = self.ast[name].text.clone();
                    if !self.scopes[*scopes.last().unwrap()].contains_key(&key) {
                        let v = self.function(c, scopes);
                        self.assign(name, v, scopes, true);
                    }
                }
            }
        }
    }
    pub(super) fn eval(&mut self, n: usize, scopes: &[usize]) -> Value {
        self.steps += 1;
        if self.steps > self.budgets.max_steps {
            self.truncated = true;
            return Value::default();
        }
        let a = self.ast[n].clone();
        if self.is_function(n) {
            if let Some(name) = self.field(n, "name") {
                let prior = self.lookup(&self.ast[name].text, scopes);
                if prior.functions.iter().any(|f| self.functions[*f].ast == n) {
                    return prior;
                }
                let v = self.function(n, scopes);
                return self.assign(name, v, scopes, true);
            }
            return self.function(n, scopes);
        }
        match a.kind.as_str() {
            "binary_expression" | "boolean_operator"
                if a.operator
                    .as_deref()
                    .is_some_and(|op| matches!(op, "&&" | "||" | "??" | "and" | "or")) =>
            {
                let left = self
                    .field(n, "left")
                    .map(|l| self.eval(l, scopes))
                    .unwrap_or_default();
                let condition = self.node(self.field(n, "left").unwrap_or(n), "condition", &left);
                self.controls.push(condition);
                // The right operand may be skipped, so its writes are one path of two.
                let skipped = self.snapshot();
                let right = self
                    .field(n, "right")
                    .map(|r| self.eval(r, scopes))
                    .unwrap_or_default();
                self.controls.pop();
                self.join(&skipped);
                let mut value = left;
                value.merge(&right);
                self.node(n, "value", &value)
            }
            "identifier" | "variable_name" | "name" => {
                let value = self.lookup(&a.text, scopes);
                self.node(n, "read", &value)
            }
            "variable_declarator"
            | "assignment"
            | "assignment_expression"
            | "augmented_assignment"
            | "augmented_assignment_expression" => {
                let left = self.field(n, "name").or_else(|| self.field(n, "left"));
                let right = self.field(n, "value").or_else(|| self.field(n, "right"));
                let mut value = right.map(|r| self.eval(r, scopes)).unwrap_or_default();
                if a.kind.starts_with("augmented") {
                    if let Some(left) = left {
                        value.merge(&self.eval(left, scopes));
                    }
                }
                let declaration =
                    a.kind == "variable_declarator" || self.files[a.file].path.ends_with(".py");
                let target_scopes = if a
                    .parent
                    .is_some_and(|p| self.ast[p].kind == "variable_declaration")
                {
                    let last = scopes
                        .iter()
                        .rposition(|scope| self.function_scopes.contains(scope))
                        .unwrap_or(0);
                    &scopes[..=last]
                } else {
                    scopes
                };
                left.map(|l| self.assign(l, value, target_scopes, declaration))
                    .unwrap_or_default()
            }
            "call_expression" | "call" | "function_call_expression" | "member_call_expression" => {
                self.call(n, scopes)
            }
            "member_expression"
            | "attribute"
            | "member_access_expression"
            | "subscript_expression" => {
                let (base, key) = self.member(n, scopes);
                let mut value = Value::default();
                if let Some(key) = key {
                    for obj in &base.objects {
                        if let Some(v) = self.objects[*obj].get(&key) {
                            value.merge(v);
                        }
                    }
                    if base.objects.is_empty() {
                        self.boundary(
                            n,
                            "unknown_alias",
                            "Field reads from an unresolved receiver have unknown provenance.",
                        );
                        value.merge(&base);
                    }
                } else {
                    self.boundary(
                        n,
                        "dynamic_field",
                        "Computed field reads have unknown provenance.",
                    );
                }
                self.node(n, "field_read", &value)
            }
            "object" | "dictionary" | "array_creation_expression" => {
                // Loop iterations reuse the allocation, like they reuse nodes, so the heap converges.
                let ctx = self.loop_ctx();
                let reused = self
                    .loop_objects
                    .as_ref()
                    .and_then(|cache| cache.get(&(ctx, n)))
                    .copied();
                let obj = match reused {
                    Some(obj) => obj,
                    None => {
                        let obj = self.objects.len();
                        self.objects.push(Scope::new());
                        if let Some(cache) = self.loop_objects.as_mut() {
                            cache.insert((ctx, n), obj);
                        }
                        obj
                    }
                };
                let mut inputs = Value::default();
                for c in &a.children {
                    let key = self.field(*c, "key");
                    let val = self.field(*c, "value");
                    if let (Some(k), Some(v)) = (key, val) {
                        let v = self.eval(v, scopes);
                        inputs.merge(&v);
                        let key: String = self.ast[k].text.trim_matches(['\'', '"']).into();
                        // A reused allocation summarises every iteration: its fields
                        // accumulate instead of restarting from the literal.
                        if reused.is_some() {
                            self.objects[obj].entry(key).or_default().merge(&v);
                        } else {
                            self.objects[obj].insert(key, v);
                        }
                    } else {
                        self.boundary(
                            *c,
                            "object_member",
                            "Object spreads and method semantics are unresolved.",
                        );
                    }
                }
                let mut v = self.node(n, "value", &inputs);
                v.objects.insert(obj);
                v
            }
            "if_statement" | "conditional_expression" | "ternary_expression" => {
                self.branch(n, scopes)
            }
            "for_statement" | "for_in_statement" | "while_statement" | "do_statement" => {
                self.loop_flow(n, scopes)
            }
            "return_statement" => {
                let mut v = Value::default();
                for c in a.children {
                    v.merge(&self.eval(c, scopes));
                }
                let result = self.node(n, "return", &v);
                // A direct body return leaves the current state intact until invoke merges exits.
                // Only nested returns need snapshots before their enclosing branch restores state.
                let direct_exit = self
                    .active
                    .last()
                    .is_some_and(|function| self.field(*function, "body") == a.parent);
                if !direct_exit {
                    if let Some(exits) = self.exit_states.last_mut() {
                        exits.push((self.scopes.clone(), self.objects.clone()));
                    }
                }
                result
            }
            "await_expression" | "yield" | "yield_expression" => {
                self.boundary(
                    n,
                    "async",
                    "Scheduling, suspension, and resumed values are not resolved.",
                );
                let mut v = Value::default();
                for c in a.children {
                    v.merge(&self.eval(c, scopes));
                }
                self.node(n, "value", &v)
            }
            "namespace_use_declaration" => {
                self.boundary(n,"import","PHP namespace imports and include execution are not resolved across snapshots.");
                Value::default()
            }
            "import_statement" | "import_from_statement" => Value::default(),
            "program" | "module" | "statement_block" | "block" | "compound_statement" => {
                let mut block_scopes = scopes.to_vec();
                if a.kind == "statement_block" {
                    block_scopes.push(self.scopes.len());
                    self.scopes.push(Scope::new());
                }
                let scopes = block_scopes.as_slice();
                self.predeclare(n, scopes);
                let mut v = Value::default();
                let control_depth = self.controls.len();
                for c in a.children {
                    let current = self.eval(c, scopes);
                    if self.always_returns(c) {
                        v.merge(&current);
                        break;
                    }
                    if matches!(
                        self.ast[c].kind.as_str(),
                        "if_statement" | "for_statement" | "while_statement"
                    ) {
                        v.merge(&current);
                    }
                    if self.ast[c].kind == "if_statement" && self.partial_return(c) {
                        // Every arm's condition decides whether the statements after
                        // the branch run at all.
                        let (arms, _) = self.arms(c);
                        for condition in arms.iter().filter_map(|(condition, _)| *condition) {
                            if let Some(id) =
                                self.origins
                                    .iter()
                                    .enumerate()
                                    .rev()
                                    .find_map(|(id, origin)| {
                                        (*origin == condition && self.nodes[id].kind == "condition")
                                            .then_some(id)
                                    })
                            {
                                self.controls.push(Value {
                                    ids: BTreeSet::from([id]),
                                    ..Value::default()
                                });
                            }
                        }
                    }
                }
                self.controls.truncate(control_depth);
                v
            }
            "expression_statement"
            | "lexical_declaration"
            | "variable_declaration"
            | "export_statement"
            | "parenthesized_expression"
            | "argument" => {
                let mut v = Value::default();
                for c in a.children {
                    v.merge(&self.eval(c, scopes));
                }
                v
            }
            "comment" | "php_tag" | "string_content" => Value::default(),
            _ => {
                if !matches!(
                    a.kind.as_str(),
                    "number"
                        | "integer"
                        | "float"
                        | "string"
                        | "true"
                        | "false"
                        | "null"
                        | "none"
                        | "binary_expression"
                        | "binary_operator"
                        | "unary_expression"
                        | "unary_operator"
                        | "comparison_operator"
                        | "boolean_operator"
                        | "not_operator"
                        | "concatenation_expression"
                        | "string_fragment"
                        | "escape_sequence"
                        | "encapsed_string"
                        | "string_value"
                        | "else_clause"
                        | "else"
                        | "type_annotation"
                        | "predefined_type"
                ) {
                    self.boundary(
                        n,
                        "unsupported_semantics",
                        &format!(
                            "{} is traversed without its full execution semantics.",
                            a.kind
                        ),
                    );
                }
                let mut v = Value::default();
                for c in a.children {
                    v.merge(&self.eval(c, scopes));
                }
                let mut result = self.node(n, "value", &v);
                result.functions.clear();
                result.objects.clear();
                result
            }
        }
    }
    pub(super) fn merge_scopes(&mut self, other: &[Scope]) {
        for (scope, old) in self.scopes.iter_mut().zip(other) {
            for (name, v) in old {
                scope.entry(name.clone()).or_default().merge(v);
            }
        }
    }
    pub(super) fn loop_ctx(&self) -> usize {
        self.loop_ctx.last().copied().unwrap_or(0)
    }
    pub(super) fn is_loop_summary(&self, obj: usize) -> bool {
        self.loop_objects
            .as_ref()
            .is_some_and(|cache| cache.values().any(|o| *o == obj))
    }
    pub(super) fn snapshot(&self) -> State {
        (self.scopes.clone(), self.objects.clone())
    }
    /// Frames and allocations created after the snapshot stay: they belong to
    /// callables that may still be invoked from another path.
    pub(super) fn restore(&mut self, state: &State) {
        for (dst, src) in self.scopes.iter_mut().zip(&state.0) {
            if *dst != *src {
                *dst = src.clone();
            }
        }
        for (dst, src) in self.objects.iter_mut().zip(&state.1) {
            if *dst != *src {
                *dst = src.clone();
            }
        }
    }
    /// Union another path's exit state into the current one.
    pub(super) fn join(&mut self, state: &State) {
        self.merge_scopes(&state.0);
        for (object, old) in self.objects.iter_mut().zip(&state.1) {
            for (key, value) in old {
                object.entry(key.clone()).or_default().merge(value);
            }
        }
    }
    /// Continue from the union of every path that falls through. With no such
    /// path the code after the fork is unreachable and the state is left as is.
    pub(super) fn join_all(&mut self, exits: &[State]) {
        if let Some((first, rest)) = exits.split_first() {
            self.restore(first);
            for state in rest {
                self.join(state);
            }
        }
    }
    /// `(condition, body)` arms in source order plus the else body. Python
    /// `if_statement` repeats the `alternative` field once per `elif` and
    /// Python `conditional_expression` has no fields at all, so both are read
    /// positionally instead of through the single-valued field map.
    pub(super) fn arms(&self, n: usize) -> (Vec<Arm>, Option<usize>) {
        let a = &self.ast[n];
        let python =
            self.files[a.file].path.ends_with(".py") || self.files[a.file].path.ends_with(".pyi");
        if python && a.kind == "conditional_expression" {
            let c = &a.children;
            return (
                vec![(c.get(1).copied(), c.first().copied())],
                c.get(2).copied(),
            );
        }
        let mut arms = vec![(
            self.field(n, "condition"),
            self.field(n, "consequence")
                .or_else(|| self.field(n, "body")),
        )];
        if python && a.kind == "if_statement" {
            let mut otherwise = None;
            for &c in &a.children {
                match self.ast[c].kind.as_str() {
                    "elif_clause" => {
                        arms.push((self.field(c, "condition"), self.field(c, "consequence")))
                    }
                    "else_clause" => otherwise = self.field(c, "body").or(Some(c)),
                    _ => {}
                }
            }
            return (arms, otherwise);
        }
        (arms, self.field(n, "alternative"))
    }
    pub(super) fn branch(&mut self, n: usize, scopes: &[usize]) -> Value {
        let (arms, otherwise) = self.arms(n);
        let control_depth = self.controls.len();
        let mut result = Value::default();
        let mut exits: Vec<State> = Vec::new();
        // State after the latest condition ran and was false. The next arm's
        // condition and the else body start from it, so a condition's own side
        // effects (a call that writes) survive into the arms that follow.
        let mut fallthrough: Option<State> = None;
        for &(condition, body) in &arms {
            if let Some(state) = &fallthrough {
                self.restore(state);
            }
            let cond = condition.map(|c| self.eval(c, scopes)).unwrap_or_default();
            let cond = self.node(condition.unwrap_or(n), "condition", &cond);
            fallthrough = Some(self.snapshot());
            // Each later arm is also control-dependent on every earlier condition.
            self.controls.push(cond);
            if let Some(body) = body {
                result.merge(&self.eval(body, scopes));
            }
            if !body.is_some_and(|b| self.always_returns(b)) {
                exits.push(self.snapshot());
            }
        }
        let fallthrough = fallthrough.unwrap_or_else(|| self.snapshot());
        self.restore(&fallthrough);
        match otherwise {
            Some(otherwise) => {
                result.merge(&self.eval(otherwise, scopes));
                if !self.always_returns(otherwise) {
                    exits.push(self.snapshot());
                }
            }
            None => exits.push(fallthrough),
        }
        self.controls.truncate(control_depth);
        self.join_all(&exits);
        result
    }
    pub(super) fn always_returns(&self, n: usize) -> bool {
        match self.ast[n].kind.as_str() {
            "return_statement" | "throw_statement" | "raise_statement" => true,
            "if_statement" => {
                let (arms, otherwise) = self.arms(n);
                arms.iter()
                    .all(|(_, body)| body.is_some_and(|b| self.always_returns(b)))
                    && otherwise.is_some_and(|c| self.always_returns(c))
            }
            "statement_block" | "block" | "compound_statement" | "else_clause" => {
                self.ast[n].children.iter().any(|c| self.always_returns(*c))
            }
            _ => false,
        }
    }
    pub(super) fn partial_return(&self, n: usize) -> bool {
        let (arms, otherwise) = self.arms(n);
        let paths = arms.len() + 1;
        let returning = arms
            .iter()
            .filter(|(_, body)| body.is_some_and(|b| self.always_returns(b)))
            .count()
            + usize::from(otherwise.is_some_and(|c| self.always_returns(c)));
        returning > 0 && returning < paths
    }
    pub(super) fn hoist_locals(&mut self, n: usize, scope: usize, python: bool) {
        let mut pending = vec![n];
        while let Some(n) = pending.pop() {
            if self.is_function(n) {
                continue;
            }
            let a = &self.ast[n];
            if (a.kind == "variable_declarator"
                && a.parent
                    .is_some_and(|p| self.ast[p].kind == "variable_declaration"))
                || (python && a.kind == "assignment")
            {
                let name = self.field(n, "name").or_else(|| self.field(n, "left"));
                if let Some(name) = name {
                    if matches!(self.ast[name].kind.as_str(), "identifier" | "variable_name") {
                        self.scopes[scope]
                            .entry(self.ast[name].text.clone())
                            .or_default();
                    }
                }
            }
            pending.extend(a.children.iter().copied());
        }
    }
    pub(super) fn loop_flow(&mut self, n: usize, scopes: &[usize]) -> Value {
        let outer_cache = (
            self.loop_nodes.take(),
            self.loop_objects.take(),
            self.loop_functions.take(),
            self.loop_call,
            std::mem::take(&mut self.loop_ctx),
        );
        self.loop_nodes = Some(BTreeMap::new());
        self.loop_objects = Some(BTreeMap::new());
        self.loop_functions = Some(BTreeMap::new());
        let mut result = Value::default();
        if let Some(initializer) = self.field(n, "initializer") {
            self.eval(initializer, scopes);
        }
        let condition = self
            .field(n, "condition")
            .or_else(|| self.field(n, "right"));
        let body = self.field(n, "body");
        let increment = self.field(n, "increment");
        let mut converged = false;
        for _ in 0..32 {
            self.loop_call = 0;
            let before = self.scopes.clone();
            let objects_before = self.objects.clone();
            let control = condition.map(|c| self.eval(c, scopes)).unwrap_or_default();
            let control = self.node(condition.unwrap_or(n), "condition", &control);
            self.controls.push(control);
            if let Some(body) = body {
                result.merge(&self.eval(body, scopes));
            }
            if let Some(increment) = increment {
                self.eval(increment, scopes);
            }
            self.controls.pop();
            self.merge_scopes(&before);
            for (object, old) in self.objects.iter_mut().zip(&objects_before) {
                for (key, value) in old {
                    object.entry(key.clone()).or_default().merge(value);
                }
            }
            // Block-local frames and allocations that first appear inside the body do not
            // escape unless captured; compare the incoming state and the incoming heap.
            if self.scopes[..before.len()] == before
                && self.objects[..objects_before.len()] == objects_before
            {
                converged = true;
                break;
            }
            if self.truncated {
                break;
            }
        }
        if !converged {
            self.truncated = true;
            self.boundary(
                n,
                "loop_budget",
                "Loop state did not converge within the iteration budget.",
            );
        }
        if matches!(
            self.ast[n].kind.as_str(),
            "for_in_statement" | "for_statement"
        ) && self.field(n, "left").is_some()
        {
            self.boundary(
                n,
                "iteration_binding",
                "Iterator element binding and protocol effects are unresolved.",
            );
        }
        (
            self.loop_nodes,
            self.loop_objects,
            self.loop_functions,
            self.loop_call,
            self.loop_ctx,
        ) = outer_cache;
        result
    }
    pub(super) fn call(&mut self, n: usize, scopes: &[usize]) -> Value {
        let a = self.ast[n].clone();
        let target = self
            .field(n, "function")
            .or_else(|| self.field(n, "name"))
            .or_else(|| a.children.first().copied());
        let builtin = target.is_some_and(|t| self.builtin(t, scopes));
        let fun = if builtin {
            Value::default()
        } else {
            target.map(|t| self.eval(t, scopes)).unwrap_or_default()
        };
        let args = self.field(n, "arguments");
        let mut values = vec![];
        let mut names = vec![];
        if let Some(args) = args {
            for arg in self.ast[args].children.clone() {
                let keyword = self.ast[arg].kind == "keyword_argument";
                let name = if keyword {
                    self.field(arg, "name").map(|n| self.ast[n].text.clone())
                } else {
                    None
                };
                let value = if keyword {
                    self.field(arg, "value").unwrap_or(arg)
                } else {
                    arg
                };
                let v = self.eval(value, scopes);
                values.push(self.node(arg, "argument", &v));
                names.push(name);
            }
        }
        self.node(n, "call", &fun);
        let mut result = Value::default();
        if builtin {
            for value in &values {
                result.merge(value);
            }
            self.boundary(n,"builtin_model","Numeric builtin summary assumes the standard implementation; coercion and monkey-patching remain outside the model.");
            let result = self.node(n, "builtin_summary", &result);
            return self.node(n, "value", &result);
        } else if fun.functions.is_empty() {
            self.boundary(
                n,
                "external_call",
                "The call target is unresolved; argument-to-return and side effects are unknown.",
            );
            for value in &values {
                for f in &value.functions {
                    let node = self.functions[*f].ast;
                    self.boundary(
                        node,
                        "callback_escape",
                        "Callback execution order and supplied arguments are unknown.",
                    );
                }
            }
        } else {
            // Every possible callee runs from the same call-site state; running them
            // back to back would let the last one overwrite the others' effects.
            let fork = (fun.functions.len() > 1).then(|| self.snapshot());
            let mut exits: Vec<State> = Vec::new();
            for f in fun.functions {
                if let Some(incoming) = &fork {
                    if !exits.is_empty() {
                        self.restore(incoming);
                    }
                }
                if names.iter().any(Option::is_some) {
                    let parameters = self.parameters(self.functions[f].ast);
                    let mut ordered = vec![Value::default(); parameters.len()];
                    let mut positional = 0;
                    for (name, value) in names.iter().zip(&values) {
                        let index = if let Some(name) = name {
                            parameters.iter().position(|p| self.ast[*p].text == *name)
                        } else {
                            let index = positional;
                            positional += 1;
                            Some(index)
                        };
                        if let Some(index) = index.filter(|i| *i < ordered.len()) {
                            ordered[index] = value.clone();
                        } else {
                            self.boundary(
                                n,
                                "keyword_argument",
                                "An argument could not be matched to a formal parameter.",
                            );
                        }
                    }
                    result.merge(&self.invoke(f, &ordered));
                } else {
                    result.merge(&self.invoke(f, &values));
                }
                if fork.is_some() {
                    exits.push(self.snapshot());
                }
            }
            self.join_all(&exits);
        }
        self.node(n, "value", &result)
    }
    pub(super) fn parameters(&self, n: usize) -> Vec<usize> {
        let Some(params) = self
            .field(n, "parameters")
            .or_else(|| self.field(n, "parameter"))
        else {
            return vec![];
        };
        let parameters = if self.ast[params].kind == "identifier" {
            vec![params]
        } else {
            self.ast[params].children.clone()
        };
        parameters
            .into_iter()
            .map(|p| {
                self.field(p, "name")
                    .or_else(|| self.field(p, "pattern"))
                    .unwrap_or(p)
            })
            .collect()
    }
    pub(super) fn builtin(&self, target: usize, scopes: &[usize]) -> bool {
        let a = &self.ast[target];
        let path = &self.files[a.file].path;
        if path.ends_with(".py") {
            return matches!(
                a.text.as_str(),
                "abs" | "round" | "min" | "max" | "len" | "int" | "float"
            ) && self.lookup(&a.text, scopes) == Value::default();
        }
        if path.ends_with(".php") || path.ends_with(".phtml") {
            return matches!(
                a.text.as_str(),
                "abs"
                    | "floor"
                    | "ceil"
                    | "round"
                    | "min"
                    | "max"
                    | "count"
                    | "intval"
                    | "floatval"
            ) && self.lookup(&a.text, scopes) == Value::default();
        }
        matches!(
            a.text.as_str(),
            "Math.floor"
                | "Math.ceil"
                | "Math.round"
                | "Math.trunc"
                | "Math.abs"
                | "Math.min"
                | "Math.max"
                | "Math.sqrt"
                | "Math.pow"
        ) && self.lookup("Math", scopes) == Value::default()
    }
    pub(super) fn invoke(&mut self, f: usize, args: &[Value]) -> Value {
        let fun = self.functions[f].clone();
        self.called.insert(f);
        if let Some(position) = self.active.iter().position(|ast| *ast == fun.ast) {
            for (arg, param) in args.iter().zip(&self.active_parameters[position]) {
                for from in &arg.ids {
                    for to in &param.ids {
                        self.edges.insert(FlowEdge {
                            from: *from,
                            to: *to,
                            kind: "value".into(),
                        });
                    }
                }
            }
            self.boundary(fun.ast, "recursive_summary", "Recursive calls share a dependency fixed point; heap and callable return identities remain approximate.");
            return self.active_returns[position].clone();
        }
        if self.active.len() >= self.budgets.max_call_depth {
            self.boundary(
                fun.ast,
                "recursion",
                "Call expansion exceeded the depth budget.",
            );
            self.truncated = true;
            return Value::default();
        }
        self.active.push(fun.ast);
        // Inside a loop, the k-th call of an iteration is the same context as the
        // k-th call of the previous one, so what the callee creates converges too.
        self.loop_call += 1;
        self.loop_ctx.push(self.loop_call);
        let summary = self.node(fun.ast, "return_summary", &Value::default());
        self.active_returns.push(summary.clone());
        self.active_parameters.push(vec![]);
        self.exit_states.push(vec![]);
        let mut scopes = fun.scopes.clone();
        let local = self.scopes.len();
        self.scopes.push(Scope::new());
        self.function_scopes.insert(local);
        scopes.push(local);
        let params = self
            .field(fun.ast, "parameters")
            .or_else(|| self.field(fun.ast, "parameter"));
        if let Some(params) = params {
            let parameters = if self.ast[params].kind == "identifier" {
                vec![params]
            } else {
                self.ast[params].children.clone()
            };
            for (i, param) in parameters.into_iter().enumerate() {
                let default = self
                    .field(param, "value")
                    .or_else(|| self.field(param, "right"))
                    .or_else(|| self.field(param, "default_value"));
                let param = self
                    .field(param, "name")
                    .or_else(|| self.field(param, "pattern"))
                    .or_else(|| self.field(param, "left"))
                    .unwrap_or(param);
                let value = args
                    .get(i)
                    .filter(|v| !v.ids.is_empty())
                    .cloned()
                    .or_else(|| fun.defaults.get(&self.ast[param].text).cloned())
                    .or_else(|| default.map(|d| self.eval(d, &scopes)))
                    .unwrap_or_default();
                let parameter = self.assign(param, value, &scopes, true);
                self.active_parameters.last_mut().unwrap().push(parameter);
            }
        }
        let body = self.field(fun.ast, "body");
        if let Some(body) = body {
            self.hoist_locals(
                body,
                local,
                self.files[self.ast[body].file].path.ends_with(".py"),
            );
        }
        let result = body
            .map(|b| {
                let v = self.eval(b, &scopes);
                if matches!(
                    self.ast[b].kind.as_str(),
                    "statement_block" | "block" | "compound_statement"
                ) {
                    v
                } else {
                    self.node(b, "return", &v)
                }
            })
            .unwrap_or_default();
        self.active.pop();
        self.active_returns.pop();
        self.active_parameters.pop();
        for (state, objects) in self.exit_states.pop().unwrap() {
            self.merge_scopes(&state);
            for (object, old) in self.objects.iter_mut().zip(objects) {
                for (key, value) in old {
                    object.entry(key).or_default().merge(&value);
                }
            }
        }
        for from in &result.ids {
            for to in &summary.ids {
                self.edges.insert(FlowEdge {
                    from: *from,
                    to: *to,
                    kind: "value".into(),
                });
            }
        }
        self.loop_ctx.pop();
        Value {
            ids: summary.ids,
            functions: result.functions,
            objects: result.objects,
        }
    }
}
