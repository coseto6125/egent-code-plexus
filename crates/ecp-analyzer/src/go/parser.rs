use super::literal_key_facts::collect_literal_key_facts;
use super::receiver_types::{
    build_receiver_map, collect_local_types, extract_go_calls_and_path_literals_with_imports,
    receiver_type_from_method_decl, LocalTypes,
};
use super::spec::GoSpec;
use crate::calls::{self, NameRefKind, NameRefSpec};
use crate::driver::imports::ImportOrder;
use crate::driver::{
    self, node, AnonKey, Anonymous, Compiled, DeclSlot, Effect, FileCx, KindCapture, Op, ParseSpec,
    Step, TxRule,
};
use crate::event_topic::{EventTopicConfig, KAFKA_GO, RABBITMQ_GO, REDIS_GO, SQS_GO};
use crate::framework_confidence;
use crate::framework_helpers::{
    enclosing_fn_idx_by_span, has_import_from, node_span, push_blind_spot,
};
use crate::function_meta::subtree_contains_kind;
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{
    BlindSpot, FrameworkId, LocalGraph, RawFrameworkRef, RawFunctionMeta, RawNode, RawRoute,
    RawTxScope,
};
use ecp_core::graph::{FileCategory, NodeKind};
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::{Node, Query, Tree};

/// gin / echo / chi all share `r.METHOD("/path", handler)`. Gate the
/// framework_ref by the imported package — the route shape alone can't
/// distinguish gin from echo. Behavior cross-checked against GitNexus
/// `go.ts:23-39` (independent Rust implementation).
const GIN_REQUIRED: &[&str] = &["github.com/gin-gonic/gin"];
const ECHO_REQUIRED: &[&str] = &["github.com/labstack/echo"];

const BLIND_IOTA_CONST_BLOCK: (&str, &str) = (
    "go-iota-const-block",
    "const ( ... iota ) block with ≥2 entries — Go enum imitation; verify before treating as plain Const set",
);

/// Symbol-level Go compiler pragmas — directives that genuinely decorate a
/// single function / method / variable. Allowlisted explicitly rather than
/// denylisting `//go:build` so future additions like `//go:debug` (package
/// scope) or third-party generators don't silently bleed into `Decorates`.
///
/// Excluded by design:
/// - `//go:build` / `//go:binary-only-package` / `//go:debug` — file or
///   package scope; not symbol decoration.
/// - `//go:generate` — invokes external tooling at `go generate` time;
///   not a property of the symbol itself.
const GO_SYMBOL_PRAGMAS: &[&str] = &[
    "noinline",
    "nosplit",
    "noescape",
    "linkname",
    "norace",
    "notinheap",
    "nointerface",
    "nowritebarrier",
    "nowritebarrierrec",
    "yeswritebarrierrec",
    "registerparams",
    "wasmimport",
    "wasmexport",
    "embed",
];

/// The path prefix a route call `g.GET("/x", ...)` inherits from the router
/// groups it hangs on: `g := r.Group("/v1")`, `u := g.Group("/users")` gives
/// `/v1/users`. The declaration is the last `:=` / `=` of the receiver name
/// before the call inside the enclosing function. `None` for a receiver with
/// no such declaration, and for a `Group` whose first argument is not a
/// string literal: an unknown prefix is no prefix, never a guessed one.
fn go_group_prefix(call: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    fn last_group_call<'a>(
        scope: tree_sitter::Node<'a>,
        name: &str,
        before: usize,
        source: &[u8],
    ) -> Option<tree_sitter::Node<'a>> {
        let mut found: Option<tree_sitter::Node<'a>> = None;
        let mut stack = vec![scope];
        while let Some(node) = stack.pop() {
            if node.start_byte() >= before {
                continue;
            }
            if matches!(
                node.kind(),
                "short_var_declaration" | "assignment_statement"
            ) {
                let single = |side: Option<tree_sitter::Node<'a>>| {
                    side.filter(|l| l.named_child_count() == 1)
                        .and_then(|l| l.named_child(0))
                };
                let lhs = single(node.child_by_field_name("left"));
                let rhs = single(node.child_by_field_name("right"));
                if let (Some(lhs), Some(rhs)) = (lhs, rhs) {
                    let is_receiver = lhs.utf8_text(source).ok() == Some(name);
                    let is_later = found.is_none_or(|f| f.start_byte() < rhs.start_byte());
                    if is_receiver && is_later {
                        found = Some(rhs);
                    }
                }
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
        found
    }

    fn prefix_of(call: tree_sitter::Node<'_>, source: &[u8], depth: usize) -> Option<Vec<String>> {
        let receiver = call
            .child_by_field_name("function")?
            .child_by_field_name("operand")
            .filter(|o| o.kind() == "identifier")?;
        let name = receiver.utf8_text(source).ok()?;
        let scope = std::iter::successors(call.parent(), |n| n.parent()).find(|n| {
            matches!(
                n.kind(),
                "function_declaration" | "method_declaration" | "func_literal" | "source_file"
            )
        })?;
        let decl = last_group_call(scope, name, call.start_byte(), source)
            .filter(|d| d.kind() == "call_expression")?;
        let is_group = decl
            .child_by_field_name("function")
            .and_then(|f| f.child_by_field_name("field"))
            .and_then(|f| f.utf8_text(source).ok())
            == Some("Group");
        if !is_group || depth == 0 {
            return None;
        }
        let literal = decl
            .child_by_field_name("arguments")?
            .named_child(0)
            .filter(|a| {
                matches!(
                    a.kind(),
                    "interpreted_string_literal" | "raw_string_literal"
                )
            })?
            .utf8_text(source)
            .ok()?
            .trim_matches(['"', '`'])
            .to_string();
        let mut outer = prefix_of(decl, source, depth - 1).unwrap_or_default();
        outer.push(literal);
        Some(outer)
    }

    let prefixes = prefix_of(call, source, 8)?;
    let joined = prefixes.iter().fold(String::new(), |joined, prefix| {
        ecp_core::contracts::identity::join_route_path(&joined, prefix)
    });
    Some(joined)
}

/// Walk preceding sibling `comment` nodes and collect known symbol-level
/// `//go:` directives. See `GO_SYMBOL_PRAGMAS` for the allowlist and
/// rationale for what is excluded.
///
/// Returns pragmas in source order. Pass the outer declaration node
/// (`function_declaration` / `method_declaration` / `var_declaration`);
/// pragmas attached to inner `var_spec` are not recognised by `go vet`.
fn collect_go_pragmas(decl_node: tree_sitter::Node<'_>, source: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = decl_node.prev_sibling();
    while let Some(sib) = cur {
        if sib.kind() != "comment" {
            break;
        }
        if let Ok(text) = std::str::from_utf8(&source[sib.start_byte()..sib.end_byte()]) {
            if let Some(rest) = text.strip_prefix("//go:") {
                let directive = rest.split_whitespace().next().unwrap_or("");
                if GO_SYMBOL_PRAGMAS.contains(&directive) {
                    out.push(format!("//go:{}", rest.trim()));
                }
            }
        }
        cur = sib.prev_sibling();
    }
    out.reverse();
    out
}

/// Scan all top-level `const_declaration` blocks in the file and emit a
/// BlindSpot for any that are Go enum imitations: ≥2 `const_spec` children
/// where at least one spec's value expression contains the identifier `iota`.
///
/// Single-line `const X = 1` uses `const_declaration` too but has exactly one
/// `const_spec`, so the ≥2 guard excludes it. Function-body const blocks are
/// legal Go but rare; we limit to `source_file` direct children to match the
/// existing const/var design (see `function_body_const_is_dropped` test).
fn detect_iota_const_blocks(
    root: tree_sitter::Node<'_>,
    path: &Path,
    is_test_file: bool,
    blind_spots: &mut Vec<BlindSpot>,
) {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() != "const_declaration" {
            continue;
        }
        // Count const_spec children and check for iota in any spec's value.
        let mut spec_count = 0u32;
        let mut has_iota = false;
        let mut inner = child.walk();
        for spec in child.children(&mut inner) {
            if spec.kind() != "const_spec" {
                continue;
            }
            spec_count += 1;
            // The value field of a const_spec holds the expression after `=`.
            // Specs that omit the value (Go iota elision) have no value child.
            if let Some(val) = spec.child_by_field_name("value") {
                if subtree_contains_kind(val, "iota") {
                    has_iota = true;
                }
            }
        }
        if spec_count >= 2 && has_iota {
            push_blind_spot(
                blind_spots,
                BLIND_IOTA_CONST_BLOCK,
                &child,
                path,
                is_test_file,
            );
        }
    }
}

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_go::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct GoProvider {
    compiled: Compiled<GoSpec>,
}

/// Walk up the AST from `node` (a `field_identifier` inside a
/// `field_declaration`) to find the name of the enclosing named struct type.
///
/// Tree shape for a named struct:
///   `field_identifier` → `field_declaration` → `field_declaration_list`
///   → `struct_type` → `type_spec` (which has `name: type_identifier`)
///
/// For fields in anonymous inline structs there is no enclosing `type_spec`,
/// so this returns `None` (uid remains without owner_class — collisions inside
/// a single anonymous struct are already impossible because of line position).
fn enclosing_struct_type(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let mut current = node.parent()?;
    while current.kind() != "struct_type" {
        current = current.parent()?;
    }
    let parent = current.parent()?;
    if parent.kind() != "type_spec" {
        return None;
    }
    let name_node = parent.child_by_field_name("name")?;
    std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
        .ok()
        .map(String::from)
}

/// Walk up the AST from `node` until a `function_declaration` or
/// `method_declaration` is found. Returns the enclosing function name —
/// receiver methods are qualified as `RecvType.MethodName` so two methods
/// with the same name on different receivers (`(d *Dog) name()` and
/// `(c *Cat) name()`) don't collide.
///
/// Used to scope short-var (`:=`) locals; without this, every `pairs := ...`
/// across all funcs in a file uid-collides.
fn enclosing_func_or_method_name(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let mut current = node.parent()?;
    loop {
        match current.kind() {
            "function_declaration" => {
                let name_node = current.child_by_field_name("name")?;
                return std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                    .ok()
                    .map(String::from);
            }
            "method_declaration" => {
                let name_node = current.child_by_field_name("name")?;
                let method_name =
                    std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                        .ok()?
                        .to_string();
                return Some(
                    match super::receiver_types::receiver_type_from_method_decl(current, source) {
                        Some(recv) => format!("{recv}.{method_name}"),
                        None => method_name,
                    },
                );
            }
            _ => {}
        }
        current = current.parent()?;
    }
}

/// Walk the full tree for `call_expression` nodes whose callee is a
/// `selector_expression` with method name `Begin` or `BeginTx`. For each
/// hit, find the innermost enclosing `Function`/`Method` node in `nodes`
/// via span containment and record its index. Returns one `RawTxScope` per
/// unique enclosing function (multiple Begin() calls in the same fn → one
/// scope). Conservative: matches on method name only, no receiver-type
/// check (acceptable false positives for non-DB `.Begin()` calls in v1).
fn collect_go_sql_tx_scopes(
    root: tree_sitter::Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
) -> Option<Box<[RawTxScope]>> {
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut scopes: Vec<RawTxScope> = Vec::new();

    let mut cursor = root.walk();
    // Walk entire tree depth-first.
    loop {
        let node = cursor.node();
        if node.kind() == "call_expression" && is_db_begin_call(node, source) {
            // Resolve the enclosing fn via the shared span-area helper so
            // Go / Ruby / Dart agree on innermost-match in nested-fn scenarios.
            let call_start = node.start_position();
            let row = crate::calls::safe_row(call_start.row);
            let col = u32::try_from(call_start.column).unwrap_or(u32::MAX);
            if let Some(idx) = enclosing_fn_idx_by_span(nodes, row, col) {
                if seen.insert(idx) {
                    scopes.push(RawTxScope::new(idx, FrameworkId::GoSqlTx));
                }
            }
        }

        // Depth-first traversal: go to first child, then sibling, then up.
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return (!scopes.is_empty()).then(|| scopes.into_boxed_slice());
            }
        }
    }
}

/// True iff `call` is a `db.Begin()` / `db.BeginTx(...)` invocation: the callee
/// is a `selector_expression` whose `field` matches `Begin` or `BeginTx`.
/// Conservative — method name only, no receiver-type check (acceptable false
/// positives for non-DB `.Begin()` calls in v1, per `collect_go_sql_tx_scopes`).
#[inline]
fn is_db_begin_call(call: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let Some(function) = call.child_by_field_name("function") else {
        return false;
    };
    if function.kind() != "selector_expression" {
        return false;
    }
    let Some(field) = function.child_by_field_name("field") else {
        return false;
    };
    let Ok(method_name) = std::str::from_utf8(&source[field.start_byte()..field.end_byte()]) else {
        return false;
    };
    matches!(method_name, "Begin" | "BeginTx")
}

fn is_exported(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_uppercase())
}

impl GoProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
        })
    }
}

impl LanguageProvider for GoProvider {
    fn name(&self) -> &'static str {
        "go"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures Go handles itself. A root capture sets the declaration
/// root; the `.name` kind capture of the same match sets name and kind.
#[derive(Clone, Copy)]
pub enum GoRole {
    Root,
    Heritage,
    Type,
    Import,
    ImportAlias,
    ImportSource,
    RouteCall,
    RouteMethod,
    RoutePath,
    FieldName,
    FieldType,
    Var,
    VarName,
    VarType,
    Variable,
    VariableName,
    Local,
    LocalName,
}

#[derive(Clone, Copy)]
pub enum GoHook {
    Iota,
    MergeFileVars,
    Calls,
    FieldReads,
    Routes,
}

#[derive(Default)]
pub struct GoMatch<'t> {
    heritage: Vec<String>,
    type_annotation: Option<String>,
    is_import: bool,
    import_alias: Option<Node<'t>>,
    import_source: Option<Node<'t>>,
    is_route: bool,
    route_method_node: Option<Node<'t>>,
    route_path_node: Option<Node<'t>>,
    route_span_node: Option<Node<'t>>,
    /// `X, Y int` gives one `@field.name` per name and one `@field.type`;
    /// every name's Property shares the type text.
    field_name_nodes: Vec<Node<'t>>,
    field_type_text: Option<String>,
    /// `var x, y int` is one `var_spec` with several `name:` children.
    is_var: bool,
    var_name_nodes: Vec<Node<'t>>,
    var_type_text: Option<String>,
    var_root_node: Option<Node<'t>>,
    /// File-scope `var X [T] = ...`, typed or untyped; the capture anchors
    /// at `source_file`, so body-local `var` blocks do not match.
    is_file_var: bool,
    file_var_name_nodes: Vec<Node<'t>>,
    /// Short-var `x := expr`: no type field in the grammar.
    is_local: bool,
    local_name_nodes: Vec<Node<'t>>,
    local_root_node: Option<Node<'t>>,
}

#[derive(Default)]
pub struct GoFile<'t> {
    /// The argument list of each route registration, by its index in
    /// `routes`. Which argument is the handler depends on the framework
    /// the file imports, so the routes step reads them after the query.
    route_arguments: Vec<(usize, Option<Node<'t>>)>,
    /// One route per registration call: the optional handler capture
    /// can match one call more than once.
    route_calls: HashSet<usize>,
    /// File-scope `@variable` emissions, merged after the query so a typed
    /// `@var` Variable with the same name and span takes precedence.
    file_var_pending: Vec<RawNode>,
    /// Written by the calls step, read by the field-reads step.
    local_types: LocalTypes,
}

impl ParseSpec for GoSpec {
    type Role = GoRole;
    type Match<'t> = GoMatch<'t>;
    type File<'t> = GoFile<'t>;
    type Hook = GoHook;

    const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
    /// A func_literal inside an argument_list gets an `<anonymous>` node only
    /// when its body holds a call, so attach_to_enclosing can host those calls
    /// instead of dropping them; empty func literals stay out of the graph.
    const ANONYMOUS: Option<Anonymous> = Some(Anonymous {
        capture: "function.anonymous",
        call_kinds: &["call_expression"],
        key: AnonKey::Span,
    });
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[
        (
            "blind.reflect_method_by_name",
            (
                "go-reflect-method-by-name",
                "x.MethodByName(name) — runtime method resolution by string; the subsequent .Call(...) is dispatch through an unknown target",
            ),
        ),
        (
            "blind.plugin_open",
            (
                "go-plugin-open",
                "plugin.Open(...) — dynamic library load (.so/.dll); symbols obtained via subsequent .Lookup(...) are resolved at runtime",
            ),
        ),
    ];
    const EVENT_TOPICS: &'static [EventTopicConfig] = &[REDIS_GO, KAFKA_GO, RABBITMQ_GO, SQS_GO];
    /// Conservative: `db.Begin()` / `db.BeginTx(...)` by method name only,
    /// one scope per enclosing function.
    const TX: TxRule = TxRule::Tree(collect_go_sql_tx_scopes);
    const STEPS: &'static [Step<GoHook>] = &[
        Step::hook(Pass::Walk, Effect::Neutral, GoHook::Iota),
        Step::fixed(Pass::Query, Op::Query),
        Step::hook(Pass::Build, Effect::Nodes, GoHook::MergeFileVars),
        Step::fixed(Pass::Build, Op::SortDedupImports(ImportOrder::SourceName)),
        Step::hook(Pass::Calls, Effect::Nodes, GoHook::Calls),
        Step::hook(Pass::Walk, Effect::Nodes, GoHook::FieldReads),
        Step::hook(Pass::Build, Effect::Neutral, GoHook::Routes),
        Step::fixed(Pass::Walk, Op::FunctionMeta),
        Step::fixed(Pass::Walk, Op::TxScopes),
        Step::fixed(Pass::Walk, Op::EventTopicsRequery),
    ];

    fn query() -> anyhow::Result<Query> {
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        Ok(Query::new(&tree_sitter_go::LANGUAGE.into(), &query_source)?)
    }

    fn role(capture: &str) -> Option<GoRole> {
        Some(match capture {
            "namespace" | "struct" | "interface" | "typedef" | "method" | "function" | "const" => {
                GoRole::Root
            }
            "heritage" => GoRole::Heritage,
            "type" => GoRole::Type,
            "import" => GoRole::Import,
            "import.alias" => GoRole::ImportAlias,
            "import.source" => GoRole::ImportSource,
            "route.call" => GoRole::RouteCall,
            "route.method" => GoRole::RouteMethod,
            "route.path" => GoRole::RoutePath,
            "field.name" => GoRole::FieldName,
            "field.type" => GoRole::FieldType,
            "var" => GoRole::Var,
            "var.name" => GoRole::VarName,
            "var.type" => GoRole::VarType,
            "variable" => GoRole::Variable,
            "variable.name" => GoRole::VariableName,
            "local" => GoRole::Local,
            "local.name" => GoRole::LocalName,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: GoRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut GoMatch<'t>,
        _f: &mut GoFile<'t>,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        match r {
            GoRole::Root => d.root = Some(node),
            GoRole::Heritage => {
                if let Ok(h_name) = node.utf8_text(source) {
                    m.heritage.push(h_name.to_string());
                }
            }
            GoRole::Type => {
                if let Ok(t_name) = node.utf8_text(source) {
                    if let Some(ref mut existing) = m.type_annotation {
                        existing.push(' ');
                        existing.push_str(t_name);
                    } else {
                        m.type_annotation = Some(t_name.to_string());
                    }
                }
            }
            GoRole::Import => m.is_import = true,
            GoRole::ImportAlias => m.import_alias = Some(node),
            GoRole::ImportSource => m.import_source = Some(node),
            GoRole::RouteCall => {
                m.is_route = true;
                m.route_span_node = Some(node);
            }
            GoRole::RouteMethod => m.route_method_node = Some(node),
            GoRole::RoutePath => m.route_path_node = Some(node),
            GoRole::FieldName => m.field_name_nodes.push(node),
            GoRole::FieldType => {
                if let Ok(t_str) = node.utf8_text(source) {
                    m.field_type_text = Some(t_str.to_string());
                }
            }
            GoRole::Var => {
                m.is_var = true;
                m.var_root_node = Some(node);
            }
            GoRole::VarName => m.var_name_nodes.push(node),
            GoRole::VarType => {
                if let Ok(t_str) = node.utf8_text(source) {
                    m.var_type_text = Some(t_str.to_string());
                }
            }
            GoRole::Variable => m.is_file_var = true,
            GoRole::VariableName => m.file_var_name_nodes.push(node),
            GoRole::Local => {
                m.is_local = true;
                m.local_root_node = Some(node);
            }
            GoRole::LocalName => {
                // The query descends into sub-expressions (`n` in
                // `n.children`); only the left side's identifiers declare.
                if node.parent().is_some_and(|p| p.kind() == "expression_list") {
                    m.local_name_nodes.push(node);
                }
            }
        }
    }

    fn end_match<'t>(
        d: DeclSlot<'t>,
        m: &mut GoMatch<'t>,
        f: &mut GoFile<'t>,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        let GoMatch {
            heritage,
            type_annotation,
            is_import,
            import_alias,
            import_source,
            is_route,
            route_method_node,
            route_path_node,
            route_span_node,
            field_name_nodes,
            field_type_text,
            is_var,
            var_name_nodes,
            var_type_text,
            var_root_node,
            is_file_var,
            file_var_name_nodes,
            is_local,
            local_name_nodes,
            local_root_node,
        } = std::mem::take(m);

        // The span is the name token, so multi-name decls stay distinct.
        // owner_class = enclosing named struct type, so two structs with a
        // same-named field get distinct uids.
        for name_node in &field_name_nodes {
            if let Ok(name_str) = name_node.utf8_text(source) {
                let name = name_str.to_string();
                cx.nodes.push(RawNode {
                    is_exported: is_exported(&name),
                    type_annotation: field_type_text.clone(),
                    owner_class: enclosing_struct_type(*name_node, source),
                    ..node::decl(
                        name,
                        NodeKind::Property,
                        node::span(*name_node),
                        &source[name_node.start_byte()..name_node.end_byte()],
                    )
                });
            }
        }

        if is_var {
            if let Some(root) = var_root_node {
                // `@var` captures `var_spec`; pragmas like //go:embed
                // attach to the enclosing `var_declaration`.
                let var_pragmas = root
                    .parent()
                    .filter(|p| p.kind() == "var_declaration")
                    .map(|d| collect_go_pragmas(d, source))
                    .unwrap_or_default();
                for n in &var_name_nodes {
                    if let Ok(name_str) = n.utf8_text(source) {
                        if name_str == "_" {
                            continue;
                        }
                        let name = name_str.to_string();
                        cx.nodes.push(RawNode {
                            decorators: var_pragmas.clone(),
                            is_exported: is_exported(&name),
                            type_annotation: var_type_text.clone(),
                            ..node::decl(
                                name,
                                NodeKind::Variable,
                                node::span_from(*n, root),
                                &source[root.start_byte()..root.end_byte()],
                            )
                        });
                    }
                }
            }
        }

        // The `@variable` capture is the whole source_file, so its end would
        // not match the `@var` span and would defeat the merge check. The
        // span ends at the name's `var_spec` instead, as on the `@var` path.
        if is_file_var {
            for n in &file_var_name_nodes {
                if let Ok(name_str) = n.utf8_text(source) {
                    if name_str == "_" {
                        continue;
                    }
                    // identifier → var_spec, directly or through var_spec_list.
                    let var_spec_root = {
                        let mut cur = n.parent();
                        loop {
                            match cur {
                                None => break None,
                                Some(p) if p.kind() == "var_spec" => break Some(p),
                                Some(p) => cur = p.parent(),
                            }
                        }
                    };
                    let Some(var_spec) = var_spec_root else {
                        continue;
                    };
                    // Pragmas attach to `var_declaration`, not `var_spec`.
                    // Walk up through optional `var_spec_list`.
                    let pragmas = {
                        let mut p = var_spec.parent();
                        while let Some(cur) = p {
                            match cur.kind() {
                                "var_declaration" => break,
                                "var_spec_list" => p = cur.parent(),
                                _ => {
                                    p = None;
                                    break;
                                }
                            }
                        }
                        p.map(|d| collect_go_pragmas(d, source)).unwrap_or_default()
                    };
                    let name = name_str.to_string();
                    f.file_var_pending.push(RawNode {
                        decorators: pragmas,
                        is_exported: is_exported(&name),
                        ..node::decl(
                            name,
                            NodeKind::Variable,
                            node::span_from(*n, var_spec),
                            &source[var_spec.start_byte()..var_spec.end_byte()],
                        )
                    });
                }
            }
        }

        // owner_class = enclosing function name: `:=` is statement-level, and
        // without it every `pairs := ...` across a file's funcs uid-collides.
        if is_local {
            if let Some(root) = local_root_node {
                let enclosing = enclosing_func_or_method_name(root, source);
                for n in &local_name_nodes {
                    if let Ok(name_str) = n.utf8_text(source) {
                        if name_str == "_" {
                            continue;
                        }
                        let name = name_str.to_string();
                        cx.nodes.push(RawNode {
                            is_exported: is_exported(&name),
                            owner_class: enclosing.clone(),
                            ..node::decl(
                                name,
                                NodeKind::Variable,
                                node::span_from(*n, root),
                                &source[root.start_byte()..root.end_byte()],
                            )
                        });
                    }
                }
            }
        }

        // A `const_spec` match carries only its first name; `X, Y = 1, 2`
        // emits one node per name on the shared spec, and `_` as `var` does.
        if let (Some(NodeKind::Const), Some(root)) = (d.kind, d.root) {
            let mut cursor = root.walk();
            for n in root.children_by_field_name("name", &mut cursor) {
                let Ok(name_str) = n.utf8_text(source) else {
                    continue;
                };
                if name_str == "_" {
                    continue;
                }
                cx.nodes.push(RawNode {
                    is_exported: is_exported(name_str),
                    type_annotation: type_annotation.clone(),
                    ..node::decl(
                        name_str.to_string(),
                        NodeKind::Const,
                        node::span(root),
                        &source[root.start_byte()..root.end_byte()],
                    )
                });
            }
        } else if let (Some(n), Some(k), Some(root)) = (d.name, d.kind, d.root) {
            if let Ok(name_str) = n.utf8_text(source) {
                let name = name_str.to_string();
                // Each match carries the full declaration node, so two methods
                // with the same name on different receivers stay distinct.
                let owner = if k == NodeKind::Method {
                    receiver_type_from_method_decl(root, source)
                } else {
                    None
                };
                let decorators = if matches!(k, NodeKind::Function | NodeKind::Method) {
                    collect_go_pragmas(root, source)
                } else {
                    vec![]
                };
                cx.nodes.push(RawNode {
                    decorators,
                    is_exported: is_exported(&name),
                    heritage,
                    type_annotation,
                    owner_class: owner,
                    ..node::decl(
                        name,
                        k,
                        node::span(root),
                        &source[root.start_byte()..root.end_byte()],
                    )
                });
            }
        }

        if is_route {
            if let (Some(m_node), Some(p_node), Some(span_node)) =
                (route_method_node, route_path_node, route_span_node)
            {
                if f.route_calls.insert(span_node.id()) {
                    let method_str = m_node.utf8_text(source).ok().unwrap_or("").to_string();
                    let path_raw = p_node.utf8_text(source).ok().unwrap_or("");
                    let path_str = path_raw.trim_matches(|c| c == '"' || c == '`').to_string();
                    let path_str = match go_group_prefix(span_node, source) {
                        Some(prefix) => {
                            ecp_core::contracts::identity::join_route_path(&prefix, &path_str)
                        }
                        None => path_str,
                    };
                    f.route_arguments
                        .push((cx.routes.len(), span_node.child_by_field_name("arguments")));
                    cx.routes.push(RawRoute {
                        method: method_str,
                        path: path_str,
                        handler: None,
                        span: node::span(span_node),
                        scope: None,
                    });
                }
            }
        }

        if is_import {
            if let Some(src_node) = import_source {
                if let Ok(src_quoted) = src_node.utf8_text(source) {
                    let source_path = src_quoted
                        .trim_matches(|c| c == '"' || c == '`')
                        .to_string();
                    let alias = import_alias
                        .and_then(|a| a.utf8_text(source).ok())
                        .map(str::to_string);
                    // `.` and `_` bind no package name; keep them in `alias`
                    // only, so no symbol named `_` resolves as the import.
                    let imported_name =
                        if let Some(a) = alias.as_ref().filter(|a| *a != "." && *a != "_") {
                            a.clone()
                        } else if let Some(last_part) = source_path.split('/').next_back() {
                            last_part.to_string()
                        } else {
                            source_path.clone()
                        };
                    cx.imports.push(driver::imports::raw(
                        source_path,
                        imported_name,
                        alias,
                        None,
                    ));
                }
            }
        }
    }

    fn hook<'t>(h: GoHook, _c: &Compiled<Self>, f: &mut GoFile<'t>, cx: &mut FileCx<'t>) {
        let root = cx.tree.root_node();
        let source = cx.source;
        match h {
            GoHook::Iota => {
                detect_iota_const_blocks(root, cx.path, cx.is_test, &mut cx.blind_spots)
            }
            GoHook::MergeFileVars => {
                // Skip a pending var only when a Variable has both its name
                // and its span, so a local `:=` shadow by name keeps it.
                for pending in std::mem::take(&mut f.file_var_pending) {
                    if !cx.nodes.iter().any(|n| {
                        n.name == pending.name
                            && n.span == pending.span
                            && n.kind == NodeKind::Variable
                    }) {
                        cx.nodes.push(pending);
                    }
                }
            }
            GoHook::Calls => {
                // Replaces the shared `extract_calls` for Go so `obj.Method()`
                // is recorded as `Type.Method` when `obj`'s type is locally known.
                let recv_map = build_receiver_map(root, source);
                f.local_types = collect_local_types(root, source, &recv_map);
                let (raw_path_literals, raw_sql_refs) =
                    extract_go_calls_and_path_literals_with_imports(
                        root,
                        source,
                        &mut cx.nodes,
                        &f.local_types,
                        &cx.imports,
                    );
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
            }
            GoHook::FieldReads => {
                let literal_key_facts =
                    collect_literal_key_facts(root, source, &cx.imports, &f.local_types);
                calls::extract_field_reads_with_literal_key_facts(
                    root,
                    source,
                    &mut cx.nodes,
                    &["selector_expression"],
                    &NameRefSpec {
                        ident_kinds: &["identifier"],
                        kind: NameRefKind::Value,
                        binding_fields: &[
                            ("short_var_declaration", "left"),
                            ("range_clause", "left"),
                            ("receive_statement", "left"),
                            ("var_spec", "name"),
                            ("const_spec", "name"),
                        ],
                        skip_fields: &[("keyed_element", "key")],
                        skip_kinds: &["import_declaration", "type_arguments"],
                        map_literal_types: &[
                            "map_type",
                            "slice_type",
                            "array_type",
                            "implicit_length_array_type",
                        ],
                        ..NameRefSpec::EMPTY
                    },
                    Some(&literal_key_facts),
                );
            }
            GoHook::Routes => {
                // Echo's `GET(path, h, m ...MiddlewareFunc)` takes the handler
                // right after the path; gin's `GET(path, handlers...)` and any
                // other router run the last argument, after the middleware.
                // A gin or echo route also stages a framework ref to a handler
                // that the registration names by a bare identifier.
                let gin = has_import_from(&cx.imports, GIN_REQUIRED);
                let echo = !gin && has_import_from(&cx.imports, ECHO_REQUIRED);
                for (route, arguments) in std::mem::take(&mut f.route_arguments) {
                    let arguments = crate::route_detector::call_arguments(arguments);
                    let handler_arg = if echo {
                        arguments.get(1)
                    } else {
                        arguments.last()
                    };
                    let Some(&handler_arg) = handler_arg else {
                        continue;
                    };
                    cx.routes[route].handler = crate::route_detector::argument_handler(
                        handler_arg,
                        source,
                        &["func_literal"],
                        &["identifier", "selector_expression"],
                    );
                    if !(gin || echo) || handler_arg.kind() != "identifier" {
                        continue;
                    }
                    let Ok(target_name) = handler_arg.utf8_text(source) else {
                        continue;
                    };
                    let span = node_span(&handler_arg);
                    let (confidence, reason) = if gin {
                        (framework_confidence::GIN_ROUTE, "gin-route")
                    } else {
                        (framework_confidence::ECHO_ROUTE, "echo-route")
                    };
                    cx.framework_refs.push(RawFrameworkRef {
                        source_name: node::enclosing_or_module(&cx.nodes, span),
                        target_name: target_name.to_string(),
                        confidence,
                        reason: reason.to_string(),
                        span,
                    });
                }
            }
        }
    }

    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta> {
        crate::function_meta::go::extract(root, source, nodes, category)
    }
}
