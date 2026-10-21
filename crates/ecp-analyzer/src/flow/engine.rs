use super::ast::Ast;
use super::frames::Frames;
use super::php::is_php;
use super::python::{is_python, Receiver};
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
/// Scopes and objects; copying one shares every frame with the original.
pub(super) type State = (Frames, Frames);
/// One `(condition, body)` arm of an if / elif / ternary.
pub(super) type Arm = (Option<usize>, Option<usize>);
pub(super) struct Engine<'a> {
    pub(super) files: &'a [SourceFile],
    pub(super) ast: Vec<Ast>,
    pub(super) roots: Vec<Option<usize>>,
    pub(super) scopes: Frames,
    pub(super) function_scopes: BTreeSet<usize>,
    pub(super) references: BTreeMap<(usize, String), (usize, String)>,
    pub(super) functions: Vec<Function>,
    pub(super) objects: Frames,
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
    pub(super) exit_states: Vec<Vec<State>>,
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
    /// PHP or Python class declaration (AST node) to the object holding its methods.
    pub(super) classes: BTreeMap<usize, usize>,
    /// Lowercased fully qualified PHP class name to its declarations.
    pub(super) declarations: BTreeMap<String, BTreeSet<usize>>,
    /// Python class-body scopes: visible to the class body, not to the
    /// functions defined in it.
    pub(super) class_scopes: BTreeSet<usize>,
    /// Python class object to its bases in declared order, one set of
    /// possible classes per base expression.
    pub(super) bases: BTreeMap<usize, Vec<BTreeSet<usize>>>,
    /// PHP `use` aliases per (file, lowercased namespace): PHP scopes them to
    /// the namespace block, and class names are case-insensitive, so each
    /// alias is keyed lowercased.
    pub(super) php_aliases: BTreeMap<(usize, String), Scope>,
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
                    let object = &mut self.objects[obj];
                    if weak {
                        object.entry(key.clone()).or_default().merge(&result);
                    } else {
                        object.insert(key.clone(), result.clone());
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
    /// Read member `n`: the receiver's value and the value read. A class
    /// receiver also yields what it inherits.
    pub(super) fn field_read(&mut self, n: usize, scopes: &[usize]) -> (Value, Value) {
        let (base, key) = self.member(n, scopes);
        let mut value = Value::default();
        if let Some(key) = key {
            for obj in &base.objects {
                self.inherited(*obj, &key, &mut value);
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
        let read = self.node(n, "field_read", &value);
        (base, read)
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
        let (python, php) = (is_python(path), is_php(path));
        let mut capture_scopes = scopes.to_vec();
        if python {
            capture_scopes.retain(|scope| !self.class_scopes.contains(scope));
        }
        if php {
            let mut captured = Scope::new();
            let mut references = Vec::new();
            for scope in scopes {
                for (name, value) in self.scopes[*scope].iter() {
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
                    for (name, value) in self.scopes[*scope].iter() {
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
                | "method_declaration"
        )
    }
    pub(super) fn predeclare(&mut self, n: usize, scopes: &[usize]) {
        let path = &self.files[self.ast[n].file].path;
        if is_python(path) {
            return;
        }
        let php = is_php(path);
        // PHP methods capture the enclosing scope by value when their class
        // is created, so every function is bound before any class.
        self.predeclare_pass(n, scopes, false);
        if php {
            self.predeclare_pass(n, scopes, true);
        }
    }
    fn predeclare_pass(&mut self, n: usize, scopes: &[usize], classes: bool) {
        for c in self.ast[n].children.clone() {
            let c = if self.ast[c].kind == "export_statement" {
                self.field(c, "declaration")
                    .or_else(|| self.ast[c].children.first().copied())
                    .unwrap_or(c)
            } else {
                c
            };
            if self.ast[c].kind == "namespace_definition" {
                if let Some(body) = self.field(c, "body") {
                    self.predeclare_pass(body, scopes, classes);
                }
                continue;
            }
            if classes {
                if self.is_class(c) {
                    self.php_class(c, scopes);
                }
                continue;
            }
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
    /// Spend one evaluation step; false once the step budget is exhausted.
    fn step(&mut self) -> bool {
        self.steps += 1;
        if self.steps > self.budgets.max_steps {
            self.truncated = true;
            return false;
        }
        true
    }
    pub(super) fn eval(&mut self, n: usize, scopes: &[usize]) -> Value {
        if !self.step() {
            return Value::default();
        }
        let a = self.ast[n].clone();
        if self.is_function(n) {
            // A method belongs to its class object, never to the enclosing
            // scope (a PHP anonymous class body is evaluated in place).
            if let Some(name) = self
                .field(n, "name")
                .filter(|_| a.kind != "method_declaration")
            {
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
                    a.kind == "variable_declarator" || is_python(&self.files[a.file].path);
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
            "call_expression"
            | "call"
            | "function_call_expression"
            | "member_call_expression"
            | "nullsafe_member_call_expression"
            | "scoped_call_expression" => self.call(n, scopes),
            "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "enum_declaration"
                if self.is_class(n) =>
            {
                self.php_class(n, scopes);
                Value::default()
            }
            "qualified_name" => {
                let value = self.php_class_ref(n, scopes);
                self.node(n, "read", &value)
            }
            "member_expression"
            | "attribute"
            | "member_access_expression"
            | "subscript_expression" => self.field_read(n, scopes).1,
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
                        let object = &mut self.objects[obj];
                        if reused.is_some() {
                            object.entry(key).or_default().merge(&v);
                        } else {
                            object.insert(key, v);
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
            // PHP `use` clauses are bound, or reported, before any file is evaluated.
            "import_statement" | "import_from_statement" | "namespace_use_declaration" => {
                Value::default()
            }
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
            | "argument"
            | "arguments"
            | "argument_list" => {
                let mut v = Value::default();
                for c in a.children {
                    v.merge(&self.eval(c, scopes));
                }
                v
            }
            // A declaration is not a value: it runs its nested statements, and
            // its name, parameter list, modifiers and types have no runtime
            // effect at the point of declaration.
            "class_declaration"
            | "abstract_class_declaration"
            | "class_body"
            | "class_heritage"
            | "extends_clause"
            | "namespace_definition" => {
                let name = self.field(n, "name");
                for c in a.children {
                    if Some(c) != name {
                        self.eval(c, scopes);
                    }
                }
                Value::default()
            }
            "class_definition" => {
                self.python_class(n, scopes);
                Value::default()
            }
            "method_definition" => {
                // The body runs in place with unbound parameters, and calls
                // through the class or an instance never reach it.
                self.boundary(
                    n,
                    "unsupported_semantics",
                    "method_definition is traversed without its full execution semantics.",
                );
                if let Some(body) = self.field(n, "body") {
                    self.eval(body, scopes);
                }
                Value::default()
            }
            "comment"
            | "php_tag"
            | "string_content"
            | "formal_parameters"
            | "type_annotation"
            | "predefined_type"
            | "type_identifier"
            | "type_list"
            | "named_type"
            | "optional_type"
            | "union_type"
            | "primitive_type"
            | "cast_type"
            | "namespace_name"
            | "relative_scope"
            | "implements_clause"
            | "interface_declaration"
            | "type_alias_declaration"
            | "pass_statement" => Value::default(),
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
        self.scopes.restore(&state.0);
        self.objects.restore(&state.1);
    }
    /// Union another path's exit state into the current one.
    pub(super) fn join(&mut self, state: &State) {
        self.scopes.merge(&state.0);
        self.objects.merge(&state.1);
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
        let python = is_python(&self.files[a.file].path);
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
            // A class body binds its own names, not the enclosing function's.
            if self.is_function(n) || self.ast[n].kind == "class_definition" {
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
            self.scopes.merge(&before);
            self.objects.merge(&objects_before);
            // Block-local frames and allocations that first appear inside the body do not
            // escape unless captured; compare the incoming state and the incoming heap.
            if self.scopes.starts_with(&before) && self.objects.starts_with(&objects_before) {
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
        // A PHP method named like a builtin (`$list->count()`) is not that builtin.
        let method = matches!(
            a.kind.as_str(),
            "member_call_expression" | "nullsafe_member_call_expression" | "scoped_call_expression"
        );
        let bound = target
            .filter(|_| !method)
            .and_then(|t| self.python_self_call(t));
        let builtin = !method && bound.is_none() && target.is_some_and(|t| self.builtin(t, scopes));
        let python = is_python(&self.files[a.file].path);
        // The receiver of a Python `Cls.m(...)` call: what a class method's
        // `cls` receives.
        let mut class_receiver = None;
        let fun = if builtin {
            Value::default()
        } else if method {
            self.php_method_target(n, scopes)
        } else if let Some((callees, _)) = &bound {
            callees.clone()
        } else {
            match target {
                Some(t) if python && self.ast[t].kind == "attribute" && self.step() => {
                    let (base, read) = self.field_read(t, scopes);
                    // Only a class object is known to pass itself to a class method.
                    if self.only_classes(&base) {
                        class_receiver = Some(base);
                    }
                    read
                }
                Some(t) => self.eval(t, scopes),
                None => Value::default(),
            }
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
        let instance = bound.map(|(_, receiver)| self.eval(receiver, scopes));
        // What each Python callee receives ahead of the arguments, as its
        // decorators decide. A callee whose binding is unknown is not run.
        let mut callees = Vec::with_capacity(fun.functions.len());
        let mut unbound = false;
        for &f in &fun.functions {
            let receiver = if python {
                self.python_method(self.functions[f].ast).map(|(r, _)| r)
            } else {
                None
            };
            let prefix = match (receiver, &instance) {
                (Some(Receiver::Static), _) => Some(None),
                // `self.cm()` passes `type(self)`, not the class that lexically
                // encloses the caller: `A.run(B)` runs `cm` with `cls` bound to B.
                // The engine models no instance-to-class link, so only a
                // receiver that is itself a class object names that class.
                (Some(Receiver::Class), Some(value)) => self.only_classes(value).then(|| {
                    Some(Value {
                        ids: value.ids.clone(),
                        objects: value.objects.clone(),
                        ..Value::default()
                    })
                }),
                (Some(Receiver::Class), None) => class_receiver.clone().map(Some),
                (Some(Receiver::Unknown), _) => None,
                (None | Some(Receiver::Instance), Some(value)) => Some(Some(value.clone())),
                (None | Some(Receiver::Instance), None) => Some(None),
            };
            match prefix {
                Some(prefix) => callees.push((f, prefix)),
                None => unbound = true,
            }
        }
        if unbound {
            self.boundary(
                n,
                "external_call",
                "A decorator or class-method receiver of the callee is not modelled; argument-to-return and side effects are unknown.",
            );
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
        } else if callees.is_empty() {
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
            let fork = (callees.len() > 1).then(|| self.snapshot());
            let mut exits: Vec<State> = Vec::new();
            for (f, prefix) in callees {
                if let Some(incoming) = &fork {
                    if !exits.is_empty() {
                        self.restore(incoming);
                    }
                }
                let prefixed;
                let (args, arg_names): (&[Value], &[Option<String>]) = match prefix {
                    Some(prefix) => {
                        prefixed = (
                            std::iter::once(prefix)
                                .chain(values.iter().cloned())
                                .collect::<Vec<_>>(),
                            std::iter::once(None)
                                .chain(names.iter().cloned())
                                .collect::<Vec<_>>(),
                        );
                        (&prefixed.0[..], &prefixed.1[..])
                    }
                    None => (&values[..], &names[..]),
                };
                if arg_names.iter().any(Option::is_some) {
                    let parameters = self.parameters(self.functions[f].ast);
                    let mut ordered = vec![Value::default(); parameters.len()];
                    let mut positional = 0;
                    for (name, value) in arg_names.iter().zip(args) {
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
                    result.merge(&self.invoke(f, args));
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
        if is_python(path) {
            return matches!(
                a.text.as_str(),
                "abs" | "round" | "min" | "max" | "len" | "int" | "float"
            ) && self.lookup(&a.text, scopes) == Value::default();
        }
        if is_php(path) {
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
                is_python(&self.files[self.ast[body].file].path),
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
        for state in self.exit_states.pop().unwrap() {
            self.join(&state);
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
