//! Bounded source-based value dependency analysis.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::{Node, Parser};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceFile {
    pub path: String,
    pub source: String,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Subject {
    Binding,
    #[default]
    Value,
    Return,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[default]
    Forward,
    Backward,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Budgets {
    pub max_nodes: usize,
    pub max_call_depth: usize,
    pub max_steps: usize,
}
impl Default for Budgets {
    fn default() -> Self {
        Self {
            max_nodes: 20000,
            max_call_depth: 16,
            max_steps: 100000,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlowRequest {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub subject: Subject,
    pub direction: Direction,
    pub budgets: Budgets,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlowNode {
    pub id: usize,
    pub kind: String,
    pub label: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct FlowEdge {
    pub from: usize,
    pub to: usize,
    pub kind: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Boundary {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub kind: String,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlowReport {
    pub source_hashes: BTreeMap<String, String>,
    pub anchor: Vec<usize>,
    pub nodes: Vec<FlowNode>,
    pub edges: Vec<FlowEdge>,
    pub consumers: Vec<usize>,
    pub boundaries: Vec<Boundary>,
    /// Boundaries recorded in files the selected slice never reaches. They are
    /// dropped from `boundaries` so a one-file query is not buried under the
    /// rest of the corpus; the count keeps the omission visible.
    pub boundaries_omitted: usize,
    pub truncated: bool,
    pub coverage: String,
}

fn language(path: &str) -> Option<tree_sitter::Language> {
    Some(match path.rsplit_once('.')?.1 {
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "ts" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "py" | "pyi" => tree_sitter_python::LANGUAGE.into(),
        "php" | "phtml" => tree_sitter_php::LANGUAGE_PHP.into(),
        _ => return None,
    })
}

fn normalize(path: &str) -> String {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}
impl Engine<'_> {
    fn import_target(&self, n: usize) -> Option<(usize, usize)> {
        let source = self
            .field(n, "source")
            .or_else(|| self.field(n, "module_name"))?;
        let file = self.ast[n].file;
        let module = self.ast[source].text.trim_matches(['\'', '"']);
        let parent = self.files[file]
            .path
            .rsplit_once('/')
            .map(|(p, _)| p)
            .unwrap_or("");
        let module = if self.files[file].path.ends_with(".py") {
            module.replace('.', "/")
        } else {
            module.into()
        };
        let path = normalize(&format!("{parent}/{module}"));
        let target = self.files.iter().position(|f| {
            let p = normalize(&f.path);
            p == path
                || ["js", "mjs", "ts", "tsx", "py", "php"].iter().any(|ext| {
                    p == format!("{path}.{ext}")
                        || p == format!("{path}/index.{ext}")
                        || p == format!("{path}/__init__.{ext}")
                })
        })?;
        Some((source, target))
    }
    fn module(&mut self, file: usize, visiting: &mut BTreeSet<usize>, done: &mut BTreeSet<usize>) {
        if done.contains(&file) {
            return;
        }
        let Some(root) = self.roots[file] else {
            done.insert(file);
            return;
        };
        if !visiting.insert(file) {
            self.boundary(
                root,
                "import_cycle",
                "Cyclic module initialization has unresolved execution order.",
            );
            return;
        }
        let dependencies: Vec<_> = self
            .ast
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                a.file == file
                    && matches!(
                        a.kind.as_str(),
                        "import_statement" | "import_from_statement" | "export_statement"
                    )
            })
            .filter_map(|(n, _)| self.import_target(n).map(|(_, f)| f))
            .collect();
        for dependency in dependencies {
            self.module(dependency, visiting, done);
        }
        self.imports(file);
        self.eval(root, &[file]);
        visiting.remove(&file);
        done.insert(file);
    }
    fn imports(&mut self, source_file: usize) {
        let imports: Vec<usize> = self
            .ast
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                matches!(
                    a.kind.as_str(),
                    "import_statement" | "import_from_statement" | "export_statement"
                ) && a.file == source_file
            })
            .map(|(i, _)| i)
            .collect();
        for n in imports {
            let file = self.ast[n].file;
            if self.ast[n].kind == "export_statement" && self.field(n, "source").is_none() {
                continue;
            }
            let Some((source, target)) = self.import_target(n) else {
                self.boundary(n, "import", "Only static named imports are resolved.");
                continue;
            };
            let mut stack = self.ast[n].children.clone();
            while let Some(c) = stack.pop() {
                let a = self.ast[c].clone();
                if matches!(
                    a.kind.as_str(),
                    "import_specifier" | "aliased_import" | "export_specifier"
                ) {
                    let name = self
                        .field(c, "name")
                        .or_else(|| a.children.first().copied());
                    if let Some(name) = name {
                        let alias = self.field(c, "alias").unwrap_or(name);
                        let value = self.scopes[target].get(&self.ast[name].text).cloned();
                        if let Some(v) = value {
                            self.scopes[file].insert(self.ast[alias].text.clone(), v);
                        } else {
                            self.boundary(c, "import", "The imported binding is unresolved.");
                        }
                    }
                } else if self.files[file].path.ends_with(".py")
                    && a.kind == "dotted_name"
                    && c != source
                {
                    if let Some(v) = self.scopes[target].get(&a.text).cloned() {
                        self.scopes[file].insert(a.text, v);
                    }
                } else {
                    stack.extend(a.children);
                }
            }
        }
    }
    fn report(self, anchors: Vec<usize>, direction: Direction) -> FlowReport {
        let mut selected: BTreeSet<usize> = anchors.iter().copied().collect();
        let mut adjacency: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for e in &self.edges {
            let (from, to) = if direction == Direction::Forward {
                (e.from, e.to)
            } else {
                (e.to, e.from)
            };
            adjacency.entry(from).or_default().push(to);
        }
        let mut pending = anchors.clone();
        while let Some(id) = pending.pop() {
            if let Some(next) = adjacency.get(&id) {
                for id in next {
                    if selected.insert(*id) {
                        pending.push(*id);
                    }
                }
            }
        }
        let nodes: Vec<FlowNode> = self
            .nodes
            .into_iter()
            .filter(|n| selected.contains(&n.id))
            .collect();
        // An empty slice keeps every boundary: nothing else explains why it is empty.
        let reached: Option<BTreeSet<&str>> =
            (!nodes.is_empty()).then(|| nodes.iter().map(|n| n.file.as_str()).collect());
        let total = self.boundaries.len();
        let boundaries: Vec<Boundary> = self
            .boundaries
            .into_iter()
            .filter(|b| reached.as_ref().is_none_or(|r| r.contains(b.file.as_str())))
            .collect();
        let boundaries_omitted = total - boundaries.len();
        let consumers = nodes
            .iter()
            .filter(|n| {
                matches!(
                    n.kind.as_str(),
                    "argument" | "return" | "field_write" | "condition"
                )
            })
            .map(|n| n.id)
            .collect();
        FlowReport {
            source_hashes: self.files.iter().map(|file| {
                let hash = ecp_core::uid::xxh3_64_bytes(file.source.as_bytes());
                (file.path.clone(), format!("xxh3:{hash:016x}"))
            }).collect(),
            anchor: anchors,
            nodes,
            edges: self.edges.into_iter().filter(|edge| {
                selected.contains(&edge.from) && selected.contains(&edge.to)
            }).collect(),
            consumers,
            boundaries,
            boundaries_omitted,
            truncated: self.truncated,
            coverage: "Bounded, path-insensitive source analysis with call-site expansion. Boundaries cover the files the selected slice reaches; boundaries_omitted counts the rest. Empty slices do not prove absence beyond supported semantics.".into(),
        }
    }
}
fn build<'a>(files: &'a [SourceFile], budgets: &Budgets) -> Result<Engine<'a>, String> {
    if budgets.max_nodes == 0 || budgets.max_steps == 0 || budgets.max_call_depth == 0 {
        return Err("Analysis budgets must be positive".into());
    }
    let mut engine = Engine {
        files,
        ast: vec![],
        roots: vec![],
        scopes: vec![Scope::new(); files.len()],
        function_scopes: (0..files.len()).collect(),
        references: BTreeMap::new(),
        functions: vec![],
        objects: vec![],
        nodes: vec![],
        origins: vec![],
        edges: BTreeSet::new(),
        boundaries: BTreeSet::new(),
        budgets: budgets.clone(),
        steps: 0,
        truncated: false,
        active: vec![],
        active_returns: vec![],
        active_parameters: vec![],
        exit_states: vec![],
        called: BTreeSet::new(),
        controls: vec![],
        loop_nodes: None,
        loop_objects: None,
    };
    let mut paths = BTreeSet::new();
    for (i, f) in files.iter().enumerate() {
        if !paths.insert(&f.path) {
            return Err(format!("Duplicate source path: {}", f.path));
        }
        let lang =
            language(&f.path).ok_or_else(|| format!("Unsupported source language: {}", f.path))?;
        let mut parser = Parser::new();
        parser.set_language(&lang).map_err(|e| e.to_string())?;
        let started = std::time::Instant::now();
        let mut progress = |_: &tree_sitter::ParseState| {
            if started.elapsed() > std::time::Duration::from_secs(2) {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        };
        let bytes = f.source.as_bytes();
        let Some(tree) = parser.parse_with_options(
            &mut |offset, _| &bytes[offset..],
            None,
            Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
        ) else {
            // One pathological file must not take the whole corpus down with it.
            engine.roots.push(None);
            engine.truncated = true;
            engine.boundaries.insert(Boundary {
                file: f.path.clone(),
                line: 1,
                column: 1,
                kind: "parse_timeout".into(),
                message: "Parsing exceeded its time budget; the file is excluded from analysis."
                    .into(),
            });
            continue;
        };
        let (root, truncated) = lower(
            tree.root_node(),
            i,
            &f.source,
            &mut engine.ast,
            budgets.max_steps,
        );
        engine.roots.push(Some(root));
        if truncated {
            engine.truncated = true;
            engine.boundary(
                root,
                "ast_budget",
                "AST lowering reached the node or nesting budget.",
            );
        }
        if tree.root_node().has_error() {
            engine.boundary(
                root,
                "parse_error",
                "Syntax errors limit analysis coverage.",
            );
        }
    }
    for i in 0..files.len() {
        if let Some(root) = engine.roots[i] {
            engine.predeclare(root, &[i]);
        }
    }
    let mut visiting = BTreeSet::new();
    let mut done = BTreeSet::new();
    for i in 0..files.len() {
        engine.module(i, &mut visiting, &mut done);
    }
    // Inspect uncalled functions independently, so source queries do not require a runtime entry point.
    // Each entry point starts from the module-exit state; otherwise the strong
    // writes of one uncalled function would kill values a sibling still reads.
    let module_exit = engine.snapshot();
    let mut f = 0;
    while f < engine.functions.len() && !engine.truncated {
        if !engine.called.contains(&f) {
            engine.restore(&module_exit);
            engine.invoke(f, &[]);
        }
        f += 1;
    }
    Ok(engine)
}
pub fn analyze(files: &[SourceFile], request: &FlowRequest) -> Result<FlowReport, String> {
    if request.line == 0 || request.column == 0 {
        return Err("Line and column are one-based".into());
    }
    if !files.iter().any(|f| f.path == request.file) {
        return Err(format!("Source not provided: {}", request.file));
    }
    let engine = build(files, &request.budgets)?;
    let contains = |a: &Ast| {
        (a.line, a.column) <= (request.line, request.column)
            && (request.line, request.column) < (a.end_line, a.end_column)
    };
    let target = engine
        .ast
        .iter()
        .enumerate()
        .filter(|(_, a)| engine.files[a.file].path == request.file && contains(a))
        .min_by_key(|(i, a)| (a.end_line - a.line, a.size, std::cmp::Reverse(*i)))
        .map(|(i, _)| i);
    let mut anchors = Vec::new();
    if request.subject == Subject::Return {
        let function = target
            .and_then(|t| engine.ast[t].parent)
            .and_then(|p| {
                if engine.ast[p].kind == "variable_declarator" {
                    engine.field(p, "value").filter(|v| engine.is_function(*v))
                } else {
                    None
                }
            })
            .or_else(|| {
                engine
                    .ast
                    .iter()
                    .enumerate()
                    .filter(|(i, a)| {
                        engine.files[a.file].path == request.file
                            && engine.is_function(*i)
                            && contains(a)
                    })
                    .min_by_key(|(_, a)| a.size)
                    .map(|(i, _)| i)
            });
        if let Some(function) = function {
            for node in &engine.nodes {
                let mut parent = engine.ast[engine.origins[node.id]].parent;
                while parent.is_some_and(|p| !engine.is_function(p)) {
                    parent = engine.ast[parent.unwrap()].parent;
                }
                if node.kind == "return" && parent == Some(function) {
                    anchors.push(node.id);
                }
            }
        }
    } else if let Some(target) = target {
        anchors = engine
            .origins
            .iter()
            .enumerate()
            .filter(|(i, o)| {
                **o == target
                    && if request.subject == Subject::Binding {
                        matches!(engine.nodes[*i].kind.as_str(), "binding" | "read")
                    } else {
                        matches!(
                            engine.nodes[*i].kind.as_str(),
                            "value" | "read" | "binding" | "field_read"
                        )
                    }
            })
            .map(|(i, _)| i)
            .collect();
    }
    if anchors.is_empty() {
        if engine.truncated {
            return Err(format!(
                "Analysis budget truncated the graph before locating {}:{}:{}",
                request.file, request.line, request.column
            ));
        }
        return Err(format!(
            "No analyzable value at {}:{}:{}",
            request.file, request.line, request.column
        ));
    }
    Ok(engine.report(anchors, request.direction))
}
/// Build once and inspect changed expressions, including changes that preserve dependency topology.
pub fn analyze_lines(
    files: &[SourceFile],
    file: &str,
    lines: &[usize],
    budgets: &Budgets,
) -> Result<FlowReport, String> {
    analyze_changes(
        files,
        &BTreeMap::from([(file.into(), lines.to_vec())]),
        budgets,
    )
}
/// Analyze all changed files against one shared source snapshot.
pub fn analyze_changes(
    files: &[SourceFile],
    changes: &BTreeMap<String, Vec<usize>>,
    budgets: &Budgets,
) -> Result<FlowReport, String> {
    for file in changes.keys() {
        if !files.iter().any(|f| &f.path == file) {
            return Err(format!("Source not provided: {file}"));
        }
    }
    let engine = build(files, budgets)?;
    let anchors = engine
        .nodes
        .iter()
        .filter(|n| {
            changes.get(&n.file).is_some_and(|lines| {
                lines
                    .iter()
                    .any(|line| n.line <= *line && *line <= n.end_line)
            }) && matches!(
                n.kind.as_str(),
                "value" | "binding" | "return" | "condition" | "field_write"
            )
        })
        .map(|n| n.id)
        .collect();
    Ok(engine.report(anchors, Direction::Forward))
}

#[cfg(test)]
mod tests;
pub fn supported_path(path: &str) -> bool {
    language(path).is_some()
}

#[derive(Clone)]
struct Ast {
    parent: Option<usize>,
    size: usize,
    operator: Option<String>,
    kind: String,
    text: String,
    file: usize,
    line: usize,
    column: usize,
    end_line: usize,
    end_column: usize,
    children: Vec<usize>,
    fields: BTreeMap<String, usize>,
}
fn lower(
    n: Node<'_>,
    file: usize,
    source: &str,
    ast: &mut Vec<Ast>,
    limit: usize,
) -> (usize, bool) {
    let root = ast.len();
    let mut pending = vec![(n, None, None, 0usize)];
    let mut truncated = false;
    while let Some((n, parent, field, depth)) = pending.pop() {
        if parent.is_some() && (ast.len() >= limit || depth >= 192) {
            truncated = true;
            continue;
        }
        let id = ast.len();
        let semantic_text = matches!(
            n.kind(),
            "identifier"
                | "name"
                | "variable_name"
                | "property_identifier"
                | "dotted_name"
                | "relative_import"
        ) || (matches!(n.kind(), "string" | "integer" | "number")
            && (matches!(field.as_deref(), Some("source" | "module_name" | "key"))
                || parent.is_some_and(|parent: usize| {
                    matches!(
                        ast[parent].kind.as_str(),
                        "subscript_expression" | "member_expression" | "attribute"
                    )
                })));
        let text = if semantic_text {
            source[n.byte_range()].to_string()
        } else {
            source[n.byte_range()].chars().take(256).collect()
        };
        ast.push(Ast {
            parent,
            size: n.end_byte() - n.start_byte(),
            operator: n
                .child_by_field_name("operator")
                .map(|op| source[op.byte_range()].to_string()),
            kind: n.kind().into(),
            text,
            file,
            line: n.start_position().row + 1,
            column: n.start_position().column + 1,
            end_line: n.end_position().row + 1,
            end_column: n.end_position().column + 1,
            children: vec![],
            fields: BTreeMap::new(),
        });
        if let Some(parent) = parent {
            ast[parent].children.push(id);
            if let Some(field) = field {
                ast[parent].fields.insert(field, id);
            }
        }
        for i in (0..n.child_count()).rev() {
            let child = n.child(i as u32).unwrap();
            if child.is_named() {
                pending.push((
                    child,
                    Some(id),
                    n.field_name_for_child(i as u32).map(String::from),
                    depth + 1,
                ));
            }
        }
    }
    (root, truncated)
}
#[derive(Clone, Default, PartialEq, Eq)]
struct Value {
    ids: BTreeSet<usize>,
    functions: BTreeSet<usize>,
    objects: BTreeSet<usize>,
}
impl Value {
    fn merge(&mut self, other: &Self) {
        self.ids.extend(&other.ids);
        self.functions.extend(&other.functions);
        self.objects.extend(&other.objects);
    }
}
#[derive(Clone)]
struct Function {
    ast: usize,
    scopes: Vec<usize>,
    defaults: BTreeMap<String, Value>,
}
type Scope = BTreeMap<String, Value>;
type State = (Vec<Scope>, Vec<Scope>);
/// One `(condition, body)` arm of an if / elif / ternary.
type Arm = (Option<usize>, Option<usize>);
struct Engine<'a> {
    files: &'a [SourceFile],
    ast: Vec<Ast>,
    roots: Vec<Option<usize>>,
    scopes: Vec<Scope>,
    function_scopes: BTreeSet<usize>,
    references: BTreeMap<(usize, String), (usize, String)>,
    functions: Vec<Function>,
    objects: Vec<Scope>,
    nodes: Vec<FlowNode>,
    origins: Vec<usize>,
    edges: BTreeSet<FlowEdge>,
    boundaries: BTreeSet<Boundary>,
    budgets: Budgets,
    steps: usize,
    truncated: bool,
    active: Vec<usize>,
    active_returns: Vec<Value>,
    active_parameters: Vec<Vec<Value>>,
    exit_states: Vec<Vec<(Vec<Scope>, Vec<Scope>)>>,
    called: BTreeSet<usize>,
    controls: Vec<Value>,
    loop_nodes: Option<BTreeMap<(usize, String), usize>>,
    loop_objects: Option<BTreeMap<usize, usize>>,
}
impl Engine<'_> {
    fn field(&self, n: usize, key: &str) -> Option<usize> {
        self.ast[n].fields.get(key).copied()
    }
    fn boundary(&mut self, n: usize, kind: &str, message: &str) {
        let a = &self.ast[n];
        self.boundaries.insert(Boundary {
            file: self.files[a.file].path.clone(),
            line: a.line,
            column: a.column,
            kind: kind.into(),
            message: message.into(),
        });
    }
    fn node(&mut self, n: usize, kind: &str, inputs: &Value) -> Value {
        if let Some(id) = self
            .loop_nodes
            .as_ref()
            .and_then(|cache| cache.get(&(n, kind.into())))
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
            cache.insert((n, kind.into()), id);
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
    fn lookup(&self, name: &str, scopes: &[usize]) -> Value {
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
    fn assign(&mut self, n: usize, value: Value, scopes: &[usize], declare: bool) -> Value {
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
                let weak = base.objects.len() > 1;
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
    fn member(&mut self, n: usize, scopes: &[usize]) -> (Value, Option<String>) {
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
    fn function(&mut self, n: usize, scopes: &[usize]) -> Value {
        // Loop iterations reuse the callable identity, like they reuse nodes, so a
        // closure created in a loop body does not defeat the fixed point.
        if self.loop_nodes.is_some() {
            if let Some(id) = self.functions.iter().rposition(|f| f.ast == n) {
                let mut v = self.node(n, "function", &Value::default());
                v.functions.insert(id);
                return v;
            }
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
        let mut v = self.node(n, "function", &Value::default());
        v.functions.insert(id);
        v
    }
    fn is_function(&self, n: usize) -> bool {
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
    fn predeclare(&mut self, n: usize, scopes: &[usize]) {
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
    fn eval(&mut self, n: usize, scopes: &[usize]) -> Value {
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
                let obj = match self.loop_objects.as_ref().and_then(|cache| cache.get(&n)) {
                    Some(obj) => *obj,
                    None => {
                        let obj = self.objects.len();
                        self.objects.push(Scope::new());
                        if let Some(cache) = self.loop_objects.as_mut() {
                            cache.insert(n, obj);
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
                        self.objects[obj]
                            .insert(self.ast[k].text.trim_matches(['\'', '"']).into(), v);
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
                        if let Some(condition) = self.field(c, "condition") {
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
    fn merge_scopes(&mut self, other: &[Scope]) {
        for (scope, old) in self.scopes.iter_mut().zip(other) {
            for (name, v) in old {
                scope.entry(name.clone()).or_default().merge(v);
            }
        }
    }
    fn snapshot(&self) -> State {
        (self.scopes.clone(), self.objects.clone())
    }
    /// Frames and allocations created after the snapshot stay: they belong to
    /// callables that may still be invoked from another path.
    fn restore(&mut self, state: &State) {
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
    fn join(&mut self, state: &State) {
        self.merge_scopes(&state.0);
        for (object, old) in self.objects.iter_mut().zip(&state.1) {
            for (key, value) in old {
                object.entry(key.clone()).or_default().merge(value);
            }
        }
    }
    /// Continue from the union of every path that falls through. With no such
    /// path the code after the fork is unreachable and the state is left as is.
    fn join_all(&mut self, exits: &[State]) {
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
    fn arms(&self, n: usize) -> (Vec<Arm>, Option<usize>) {
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
    fn branch(&mut self, n: usize, scopes: &[usize]) -> Value {
        let (arms, otherwise) = self.arms(n);
        let incoming = self.snapshot();
        let control_depth = self.controls.len();
        let mut result = Value::default();
        let mut exits: Vec<State> = Vec::new();
        for (index, &(condition, body)) in arms.iter().enumerate() {
            if index > 0 {
                self.restore(&incoming);
            }
            let cond = condition.map(|c| self.eval(c, scopes)).unwrap_or_default();
            let cond = self.node(condition.unwrap_or(n), "condition", &cond);
            // Each later arm is also control-dependent on every earlier condition.
            self.controls.push(cond);
            if let Some(body) = body {
                result.merge(&self.eval(body, scopes));
            }
            if !body.is_some_and(|b| self.always_returns(b)) {
                exits.push(self.snapshot());
            }
        }
        self.restore(&incoming);
        match otherwise {
            Some(otherwise) => {
                result.merge(&self.eval(otherwise, scopes));
                if !self.always_returns(otherwise) {
                    exits.push(self.snapshot());
                }
            }
            None => exits.push(incoming),
        }
        self.controls.truncate(control_depth);
        self.join_all(&exits);
        result
    }
    fn always_returns(&self, n: usize) -> bool {
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
    fn partial_return(&self, n: usize) -> bool {
        let (arms, otherwise) = self.arms(n);
        let paths = arms.len() + 1;
        let returning = arms
            .iter()
            .filter(|(_, body)| body.is_some_and(|b| self.always_returns(b)))
            .count()
            + usize::from(otherwise.is_some_and(|c| self.always_returns(c)));
        returning > 0 && returning < paths
    }
    fn hoist_locals(&mut self, n: usize, scope: usize, python: bool) {
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
    fn loop_flow(&mut self, n: usize, scopes: &[usize]) -> Value {
        let outer_cache = (self.loop_nodes.take(), self.loop_objects.take());
        self.loop_nodes = Some(BTreeMap::new());
        self.loop_objects = Some(BTreeMap::new());
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
        (self.loop_nodes, self.loop_objects) = outer_cache;
        result
    }
    fn call(&mut self, n: usize, scopes: &[usize]) -> Value {
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
    fn parameters(&self, n: usize) -> Vec<usize> {
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
    fn builtin(&self, target: usize, scopes: &[usize]) -> bool {
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
    fn invoke(&mut self, f: usize, args: &[Value]) -> Value {
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
        // Loop iterations reuse local expression nodes, but separate calls retain distinct contexts.
        let loop_cache = (self.loop_nodes.take(), self.loop_objects.take());
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
        (self.loop_nodes, self.loop_objects) = loop_cache;
        Value {
            ids: summary.ids,
            functions: result.functions,
            objects: result.objects,
        }
    }
}
