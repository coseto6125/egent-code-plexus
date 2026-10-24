//! Bounded source-based value dependency analysis.
use ast::{lower, Ast};
use engine::{Engine, Scope};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use tree_sitter::{Parser, Tree};

mod ast;
mod classes;
mod engine;
mod frames;
mod module;
mod php;
mod python;

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
    /// Hashes of the files the result names (nodes and boundaries) only.
    pub source_hashes: BTreeMap<String, String>,
    /// One hash over every corpus file's path and hash, so a change in a file
    /// the result does not name still shows.
    #[serde(default)]
    pub corpus_digest: String,
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

/// Case-sensitive like Laravel's view resolver: `V.BLADE.php` is plain PHP.
pub fn is_blade_template(path: &str) -> bool {
    path.ends_with(".blade.php")
}

fn language(path: &str) -> Option<tree_sitter::Language> {
    // tree-sitter-php reads a Blade template (no `<?php`) as one HTML leaf, so
    // flow would anchor the whole file instead of the requested span.
    if is_blade_template(path) {
        return None;
    }
    Some(match path.rsplit_once('.')?.1 {
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "ts" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "py" | "pyi" => tree_sitter_python::LANGUAGE.into(),
        "php" | "phtml" => tree_sitter_php::LANGUAGE_PHP.into(),
        _ => return None,
    })
}

/// `first` names the file that is lowered, initialised and swept before the
/// rest of the corpus.
fn build<'a>(
    files: &'a [SourceFile],
    budgets: &Budgets,
    first: Option<&str>,
) -> Result<Engine<'a>, String> {
    if budgets.max_nodes == 0 || budgets.max_steps == 0 || budgets.max_call_depth == 0 {
        return Err("Analysis budgets must be positive".into());
    }
    let mut engine = Engine {
        files,
        ast: vec![],
        roots: vec![None; files.len()],
        scopes: frames::Frames::default(),
        function_scopes: (0..files.len()).collect(),
        references: BTreeMap::new(),
        functions: vec![],
        objects: frames::Frames::default(),
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
        loop_functions: None,
        loop_call: 0,
        loop_ctx: vec![],
        links: BTreeSet::new(),
        classes: BTreeMap::new(),
        declarations: BTreeMap::new(),
        class_scopes: BTreeSet::new(),
        bases: BTreeMap::new(),
        php_aliases: BTreeMap::new(),
    };
    for _ in files {
        engine.scopes.push(Scope::new());
    }
    let mut paths = BTreeSet::new();
    for f in files {
        if !paths.insert(&f.path) {
            return Err(format!("Duplicate source path: {}", f.path));
        }
        if language(&f.path).is_none() {
            return Err(format!("Unsupported source language: {}", f.path));
        }
    }
    // Each tree is dropped once lowered: holding every tree until the last
    // file is lowered costs several times the memory of the lowered ASTs.
    let first = first.and_then(|path| files.iter().position(|f| f.path == path));
    let mut queued = vec![false; files.len()];
    if let Some(first) = first {
        let mut classes = None;
        queued[first] = true;
        let mut pending = VecDeque::from([first]);
        while let Some(i) = pending.pop_front() {
            let Some(tree) = load(&mut engine, i)? else {
                continue;
            };
            for target in imported_files(files, i, &tree, &mut classes)? {
                if !queued[target] {
                    queued[target] = true;
                    pending.push_back(target);
                }
            }
        }
    }
    for i in (0..files.len()).filter(|i| !queued[*i]) {
        load(&mut engine, i)?;
    }
    // PHP classes are autoloaded rather than initialised in file order: every
    // class exists and every `use` is bound before any method is created.
    let php_files: Vec<usize> = (0..files.len())
        .filter(|i| php::is_php(&files[*i].path))
        .collect();
    for &i in &php_files {
        if let Some(root) = engine.roots[i] {
            engine.php_declare(root, i);
        }
    }
    for &i in &php_files {
        if let Some(root) = engine.roots[i] {
            engine.php_imports(root, i);
        }
    }
    for i in 0..files.len() {
        if let Some(root) = engine.roots[i] {
            engine.predeclare(root, &[i]);
        }
    }
    let mut visiting = BTreeSet::new();
    let mut done = BTreeSet::new();
    // The queried file and the modules it imports initialise first, so the
    // rest of the snapshot cannot spend the budget before the query exists.
    if let Some(first) = first {
        engine.module(first, &mut visiting, &mut done);
    }
    for i in 0..files.len() {
        engine.module(i, &mut visiting, &mut done);
    }
    // Inspect uncalled functions independently, so source queries do not require a runtime entry point.
    // Each entry point starts from the module-exit state; otherwise the strong
    // writes of one uncalled function would kill values a sibling still reads.
    // Gated on the budgets themselves, not the `truncated` latch: an AST cut,
    // a recursion or loop boundary elsewhere in the corpus must not cost the
    // remaining entry points their analysis. An exhausted step or node budget
    // makes every further `invoke` return at once, so the loop stays bounded.
    // The queried file goes first: budget spent on the rest of the corpus
    // must not keep the queried value from being created at all.
    if first.is_some() {
        sweep(&mut engine, first);
    }
    let f = sweep(&mut engine, None);
    // An entry point the budget kept the sweep from reaching is a truncation
    // even when no evaluation step ever tripped a latch.
    if (f..engine.functions.len()).any(|f| !engine.called.contains(&f)) {
        engine.truncated = true;
    }
    Ok(engine)
}
/// Parse one file under a time budget; `None` when the budget ran out.
fn parse(file: &SourceFile) -> Result<Option<Tree>, String> {
    let lang = language(&file.path)
        .ok_or_else(|| format!("Unsupported source language: {}", file.path))?;
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
    let bytes = file.source.as_bytes();
    Ok(parser.parse_with_options(
        &mut |offset, _| &bytes[offset..],
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
    ))
}
/// Parse and lower file `i`. The tree is returned so the caller can read its
/// imports before it drops it.
fn load(engine: &mut Engine<'_>, i: usize) -> Result<Option<Tree>, String> {
    let files = engine.files;
    let Some(tree) = parse(&files[i])? else {
        // One pathological file must not take the whole corpus down with it.
        engine.boundaries.insert(Boundary {
            file: files[i].path.clone(),
            line: 1,
            column: 1,
            kind: "parse_timeout".into(),
            message: "Parsing exceeded its time budget; the file is excluded from analysis.".into(),
        });
        return Ok(None);
    };
    let (root, truncated) = lower(
        tree.root_node(),
        i,
        &files[i].source,
        &mut engine.ast,
        engine.budgets.max_steps,
    );
    engine.roots[i] = Some(root);
    if truncated {
        // Not the `truncated` latch: that marker is for budgets the whole run
        // shares, while a cut file only limits the slices that reach it.
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
    Ok(Some(tree))
}
/// The files the imports of file `i` name. The AST budget is global, so the
/// caller lowers `first`, then these, breadth-first, before unrelated files
/// spend it. PHP `use` names a class, not a file: `classes` maps declared
/// class names to files, built on first need by a pass that keeps no tree.
fn imported_files(
    files: &[SourceFile],
    i: usize,
    tree: &Tree,
    classes: &mut Option<BTreeMap<String, Vec<usize>>>,
) -> Result<Vec<usize>, String> {
    let (root, source) = (tree.root_node(), files[i].source.as_str());
    if !php::is_php(&files[i].path) {
        return Ok(module::imported_modules(root, source)
            .iter()
            .filter_map(|specifier| module::resolve(files, i, specifier))
            .collect());
    }
    if classes.is_none() {
        let mut declared: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (j, f) in files.iter().enumerate() {
            if !php::is_php(&f.path) {
                continue;
            }
            if let Some(tree) = parse(f)? {
                for name in php::declared_names(tree.root_node(), &f.source) {
                    declared.entry(name).or_default().push(j);
                }
            }
        }
        *classes = Some(declared);
    }
    let Some(classes) = classes.as_ref() else {
        return Ok(vec![]);
    };
    Ok(php::used_names(root, source)
        .iter()
        .filter_map(|name| classes.get(name))
        .flatten()
        .copied()
        .collect())
}
/// Invoke each uncalled function (of `file` only, when given) as an entry
/// point; returns where the budget stopped the sweep.
fn sweep(engine: &mut Engine<'_>, file: Option<usize>) -> usize {
    let mut f = 0;
    while f < engine.functions.len()
        && engine.steps <= engine.budgets.max_steps
        && engine.nodes.len() < engine.budgets.max_nodes
    {
        if !engine.called.contains(&f)
            && file.is_none_or(|file| engine.ast[engine.functions[f].ast].file == file)
        {
            // Each entry point runs from the state it was discovered in (its
            // enclosing function's frame included) and leaves no effects behind.
            let base = engine.snapshot();
            engine.invoke(f, &[]);
            engine.restore(&base);
        }
        f += 1;
    }
    f
}
pub fn analyze(files: &[SourceFile], request: &FlowRequest) -> Result<FlowReport, String> {
    if request.line == 0 || request.column == 0 {
        return Err("Line and column are one-based".into());
    }
    if !files.iter().any(|f| f.path == request.file) {
        return Err(format!("Source not provided: {}", request.file));
    }
    let engine = build(files, &request.budgets, Some(request.file.as_str()))?;
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
        let anchor_file_cut = engine
            .boundaries
            .iter()
            .any(|b| b.kind == "ast_budget" && b.file == request.file);
        if engine.truncated || anchor_file_cut {
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
    let engine = build(files, budgets, None)?;
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
