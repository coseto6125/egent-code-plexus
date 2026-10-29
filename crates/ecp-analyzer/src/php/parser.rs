use super::receiver_types::{
    extract_php_calls_and_path_literals, literal_callable, passed_callable,
};
use super::spec::PhpSpec;
use crate::calls::{NameRefSpec, VariantRefSpec};
use crate::driver::{
    self, imports, node, AnonKey, Anonymous, Compiled, DeclSlot, Effect, FieldReads, FileCx,
    KindCapture, Op, ParseSpec, Step, TxRule,
};
use crate::framework_confidence;
use crate::framework_helpers::{
    collect_symfony_transactional_scopes, has_import_from, node_span, push_blind_spot,
};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{
    LocalGraph, NameRef, NameRefKind, RawFrameworkRef, RawFunctionMeta, RawImport, RawNode,
    RawRoute, Span,
};
use ecp_core::graph::{FileCategory, NodeKind};
use rustc_hash::FxHashMap;
use std::path::Path;
use tree_sitter::{Node, Query, Tree};

/// Pushed by the `blind.call_user_func` role, only for a non-literal callable.
const BLIND_CALL_USER_FUNC: (&str, &str) = (
    "php-call-user-func",
    "call_user_func(<callable>, ...) with non-literal callable — dispatch through a variable or expression resolved at runtime",
);

/// Pushed by the calls step for a dynamic call with no complete fan-out.
const BLIND_VARIABLE_METHOD_CALL: (&str, &str) = (
    "php-variable-method-call",
    "Cls::$m() / $o->$m() / $o?->{expr}() — method name bound at runtime; no edge names the called method",
);

/// Walk the root of a parsed PHP file and emit one `BlindSpot` per
/// `class_declaration` that looks like a pre-8.1 enum imitation:
///
/// - body has ≥ 2 `const_declaration` children
/// - body has 0 `property_declaration` children (no instance state)
/// - the class is not itself an `enum_declaration` (those are first-class 8.1+)
///
/// Pushes matching class-spans directly into `out`.
fn detect_enum_imitation_blind_spots(
    root: tree_sitter::Node<'_>,
    path: &Path,
    is_test_file: bool,
    out: &mut Vec<ecp_core::analyzer::types::BlindSpot>,
) {
    const SPEC: (&str, &str) = (
        "php-enum-imitation",
        "class with \u{2265}2 const members and no instance properties \
         \u{2014} may be a pre-8.1 enum imitation; verify before treating as plain Class",
    );
    let mut stack: Vec<tree_sitter::Node<'_>> = vec![root];

    while let Some(node) = stack.pop() {
        if node.kind() == "class_declaration" {
            if let Some(body) = node.child_by_field_name("body") {
                let mut const_count: u32 = 0;
                let mut prop_count: u32 = 0;
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    match child.kind() {
                        "const_declaration" => const_count += 1,
                        "property_declaration" => prop_count += 1,
                        _ => {}
                    }
                }
                if const_count >= 2 && prop_count == 0 {
                    push_blind_spot(out, SPEC, &node, path, is_test_file);
                    // Don't descend into the class body — emit one BlindSpot
                    // per class, not per nested class (nested class with
                    // identical shape would produce its own stack iteration).
                    continue;
                }
            }
        }
        // Push named children to continue the depth-first walk.
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.is_named() {
                stack.push(child);
            }
        }
    }
}

/// Record `Type::CONST` reads only when the parser emitted that exact constant
/// as a member of the named type. Enum cases still go through `VariantRef`.
fn extract_qualified_constant_refs(
    root: tree_sitter::Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
) {
    let has_namespaces = nodes.iter().any(|node| node.kind == NodeKind::Namespace);
    let owner_counts = nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                NodeKind::Class | NodeKind::Interface | NodeKind::Trait | NodeKind::Enum
            )
        })
        .fold(rustc_hash::FxHashMap::default(), |mut counts, node| {
            *counts.entry(node.name.as_str()).or_insert(0usize) += 1;
            counts
        });
    let constants: rustc_hash::FxHashSet<(&str, &str)> = nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Const)
        .filter_map(|node| Some((node.owner_class.as_deref()?, node.name.as_str())))
        .collect();
    if constants.is_empty() {
        return;
    }

    let mut refs: Vec<((u32, u32, u32, u32), String)> = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "class_constant_access_expression" {
            let Some(text) = node.utf8_text(source).ok() else {
                continue;
            };
            let Some((scope, constant)) = text.rsplit_once("::") else {
                continue;
            };
            if constant.starts_with('$') {
                continue;
            }
            let span = node_span(&node);
            let owner = match scope.trim() {
                "self" | "static" => {
                    let mut parent = node.parent();
                    let mut owner = None;
                    while let Some(container) = parent {
                        match container.kind() {
                            "anonymous_class"
                                if container.child_by_field_name("body").is_some_and(|body| {
                                    body.start_byte() <= node.start_byte()
                                        && node.end_byte() <= body.end_byte()
                                }) =>
                            {
                                break;
                            }
                            "class_declaration"
                            | "interface_declaration"
                            | "trait_declaration"
                            | "enum_declaration" => {
                                owner = container
                                    .child_by_field_name("name")
                                    .and_then(|name| name.utf8_text(source).ok());
                                break;
                            }
                            _ => parent = container.parent(),
                        }
                    }
                    owner
                }
                scope => {
                    // The qualified resolver identifies its type by the final
                    // segment and cannot prove PHP namespace visibility.
                    (!has_namespaces && !scope.contains('\\')).then_some(scope)
                }
            };
            if let Some(owner) = owner.filter(|owner| {
                owner_counts.get(*owner) == Some(&1)
                    && constants.contains(&(*owner, constant.trim()))
            }) {
                refs.push((span, format!("{owner}.{}", constant.trim())));
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    for (span, name) in refs {
        let Some(target) = nodes
            .iter()
            .enumerate()
            .filter(|(_, raw)| {
                matches!(raw.kind, NodeKind::Function | NodeKind::Method)
                    && ecp_core::analyzer::types::span_contains(raw.span, span)
            })
            .min_by_key(|(_, raw)| crate::framework_helpers::span_area(raw.span))
            .map(|(idx, _)| idx)
        else {
            continue;
        };
        let entry = NameRef::encode(&name, NameRefKind::Constant);
        if !nodes[target].field_reads.contains(&entry) {
            nodes[target].field_reads.push(entry);
        }
    }
}

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_php::LANGUAGE_PHP.into();
        parser.set_language(&language).expect("Failed to set PHP language");
        parser
    });
}

/// Laravel route detection import gate. Behavior cross-checked against GitNexus
/// `php.ts:34-42` (independent Rust implementation).
/// `use Illuminate\...` is required — bare `Route::` in a non-Laravel
/// codebase shouldn't surface as a Laravel route.
const LARAVEL_REQUIRED: &[&str] = &["Illuminate"];

/// PHP HTTP-framework namespace allowlist. Generic Route emission
/// (`Route::get('/x')`, `Slim::get(...)`, etc.) requires the file to
/// import from one of these — without this gate, a user-defined
/// `class Route { static get($k) {...} }` would fire route emission
/// just by matching the scope-name allowlist. P1 review finding on
/// PR #50; see `2026-05-17-route-precision-design.md`.
const PHP_HTTP_FRAMEWORK_NAMESPACES: &[&str] = &[
    "Illuminate", // Laravel
    "Laravel",    // Lumen (Laravel\Lumen\...) and other Laravel-flavored
    "Slim",       // Slim
    "Symfony",    // Symfony
    "Laminas",    // Laminas
    "Zend",       // Zend Framework
    "CodeIgniter",
];

/// Router-class scope allowlist for the generic `scoped_call_expression`
/// route capture. Without it, `Cache::get('key')` / `Config::get('app.name')`
/// / `Auth::get(...)` all match the regex and surface as fake routes.
/// Pairs with `PHP_HTTP_FRAMEWORK_NAMESPACES` import gate: scope match
/// alone isn't enough, the file must also import from a known framework
/// namespace. List intentionally narrow — `App` was removed (too generic;
/// any user `class App` would FP) and `Lumen` was removed (Lumen routes
/// flow through `$app->get(...)` instance calls, not `Lumen::get(...)`
/// static calls).
pub(super) const PHP_ROUTER_SCOPES: &[&str] = &["Route", "Router", "Slim", "Symfony"];

/// Whether the file declares or imports a class named like a router scope
/// (`class Route`, `use App\Support\Route`): its `Route::get(...)` is that
/// class's method, not the framework's router. `use Route;` imports
/// Laravel's global `Route` facade alias, the router itself; any other
/// import binds the name, `use Router;` too: Laravel registers no global
/// `Router` alias.
fn binds_own_router(imports: &[RawImport], nodes: &[RawNode]) -> bool {
    imports.iter().any(|import| {
        let local = import
            .alias
            .as_deref()
            .unwrap_or_else(|| import.imported_name.rsplit('\\').next().unwrap_or_default());
        let facade_alias = import
            .source
            .trim_start_matches('\\')
            .eq_ignore_ascii_case("Route");
        !facade_alias && PHP_ROUTER_SCOPES.contains(&local)
    }) || nodes
        .iter()
        .any(|node| node.kind.is_type() && PHP_ROUTER_SCOPES.contains(&node.name.as_str()))
}

/// Walk a chained member-call expression inward through any depth of
/// `member_call_expression` until it reaches a `scoped_call_expression` root.
/// Returns the scope name (`Route`, `Router`, etc.) when the chain terminates
/// in a scoped call; `None` for any other shape (regular method calls, deep
/// object navigation, etc.).
///
/// Powers chained-route detection like `Route::middleware(['auth'])->get(...)`
/// or `Route::middleware(...)->prefix(...)->post(...)`. The query captures the
/// outer member_call; this walks the object field inward to find the root.
pub(super) fn walk_to_scoped_root_scope(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut cur = node;
    loop {
        match cur.kind() {
            "member_call_expression" | "member_access_expression" => {
                cur = cur.child_by_field_name("object")?;
            }
            "scoped_call_expression" => {
                let scope_node = cur.child_by_field_name("scope")?;
                let scope_text =
                    std::str::from_utf8(&source[scope_node.start_byte()..scope_node.end_byte()])
                        .ok()?;
                return Some(scope_text.to_string());
            }
            _ => return None,
        }
    }
}

/// Walk an `array_creation_expression` of shape `[Controller::class, 'action']`
/// and produce `"Controller@action"` matching Laravel's `@`-routing syntax.
/// Returns `None` for any array that doesn't match this exact shape.
fn extract_controller_action(arr_node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut controller: Option<String> = None;
    let mut action: Option<String> = None;
    let mut cursor = arr_node.walk();
    for child in arr_node.children(&mut cursor) {
        if child.kind() != "array_element_initializer" {
            continue;
        }
        let mut inner_cursor = child.walk();
        for sub in child.children(&mut inner_cursor) {
            match sub.kind() {
                // `Controller::class` — first array element.
                "class_constant_access_expression" => {
                    let name_node = sub.child_by_field_name("class").or_else(|| {
                        let mut sc = sub.walk();
                        let first = sub.children(&mut sc).find(|c| c.kind() == "name");
                        first
                    });
                    if let Some(n) = name_node {
                        if let Ok(text) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()])
                        {
                            controller = Some(text.to_string());
                        }
                    }
                }
                // `'action'` — second array element (a quoted string).
                "string" | "encapsed_string" => {
                    let mut sc = sub.walk();
                    let content_node = sub
                        .children(&mut sc)
                        .find(|c| c.kind() == "string_content" || c.kind() == "string_value");
                    let text = match content_node {
                        Some(c) => std::str::from_utf8(&source[c.start_byte()..c.end_byte()])
                            .ok()
                            .map(str::to_string),
                        None => std::str::from_utf8(&source[sub.start_byte()..sub.end_byte()])
                            .ok()
                            .map(|s| s.trim_matches(|c| c == '\'' || c == '"').to_string()),
                    };
                    if action.is_none() {
                        action = text;
                    }
                }
                _ => {}
            }
        }
    }
    match (controller, action) {
        (Some(c), Some(a)) => Some(format!("{c}@{a}")),
        _ => None,
    }
}

pub struct PhpProvider {
    compiled: Compiled<PhpSpec>,
}

impl PhpProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
        })
    }
}

impl LanguageProvider for PhpProvider {
    fn name(&self) -> &'static str {
        "php"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures PHP handles itself. A root capture sets the declaration
/// root once per match; the `name.*` kind capture sets name and kind.
#[derive(Clone, Copy)]
pub enum PhpRole {
    Root,
    Type,
    Decorator,
    Export,
    Heritage,
    ImportSource,
    ImportAlias,
    RouteScope,
    RouteMethod,
    RoutePath,
    /// `route.call` and `route.chained.call`: the whole registration.
    RouteCall,
    RouteChainedObject,
    LaravelCall,
    LaravelArgs,
    LaravelInclude,
    LaravelRouteSet,
    CallUserFunc,
}

#[derive(Clone, Copy)]
pub enum PhpHook {
    Routes,
    Calls,
    LaravelGate,
    EnumImitation,
    ConstOwners,
    QualifiedConstants,
}

#[derive(Default)]
pub struct PhpMatch<'t> {
    /// A `private` or `protected` modifier; every other match is exported.
    not_exported: bool,
    heritage: Vec<String>,
    type_annotation: Option<String>,
    decorators: Vec<String>,
    import_src: Option<Node<'t>>,
    import_alias: Option<Node<'t>>,
    route_method: Option<String>,
    route_path: Option<&'t str>,
    route_span_node: Option<Node<'t>>,
    route_scope: Option<String>,
    /// The whole `Route::method(...)` expression.
    laravel_call_span: Option<Span>,
    /// The `arguments` node, walked for the 2nd argument.
    laravel_args_node: Option<Node<'t>>,
}

#[derive(Default)]
pub struct PhpFile<'t> {
    /// Dedup lookup for multi-capture merge only; nodes are pushed in
    /// match order and the map is never iterated.
    node_id_to_idx: FxHashMap<usize, usize>,
    /// Verb calls that passed the router gates, as `(method, path
    /// literal, call)`. They become `routes` once the loop has seen the
    /// file's `use` imports, which an `X::class` action resolves through.
    route_calls: Vec<(String, &'t str, Node<'t>)>,
    /// Calls and `require`s that may load a route file. A group's
    /// `X::class` middleware resolves through the imports too.
    include_calls: Vec<Node<'t>>,
    /// `Route::resource` / `match` / `any` calls, each registering
    /// several routes or a method set.
    route_sets: Vec<Node<'t>>,
    /// Laravel framework refs, emitted if the `Illuminate` import gate matches.
    pending_laravel: Vec<(String, Span)>,
    /// The first `Namespace` node's name, read by the routes step and
    /// again by the controller-middleware step.
    file_namespace: Option<String>,
}

const fn step(pass: Pass, effect: Effect, op: Op<PhpHook>) -> Step<PhpHook> {
    Step { pass, effect, op }
}

impl ParseSpec for PhpSpec {
    type Role = PhpRole;
    type Match<'t> = PhpMatch<'t>;
    type File<'t> = PhpFile<'t>;
    type Hook = PhpHook;

    const PARSE_ERROR: &'static str = "Failed to parse php file";
    const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
    /// An arg-position anonymous function or arrow function gets an
    /// `<anonymous>` Function node only when its body holds a call, so
    /// attach_to_enclosing can host those calls instead of dropping them;
    /// empty callbacks (`array_map(fn($x) => $x * 2, $a)`) stay out of the
    /// graph. A callback may be captured once per query match.
    const ANONYMOUS: Option<Anonymous> = Some(Anonymous {
        capture: "function.anonymous",
        call_kinds: &[
            "function_call_expression",
            "member_call_expression",
            "nullsafe_member_call_expression",
            "scoped_call_expression",
        ],
        key: AnonKey::Span,
    });
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[
        (
            "blind.eval",
            (
                "php-eval",
                "eval(arg) — runtime PHP code execution; argument expression is not statically determinable as a callee",
            ),
        ),
        (
            "blind.variable_call",
            (
                "php-variable-call",
                "$<var>(...) — variable-function call; target callable bound at runtime via the variable's value",
            ),
        ),
    ];
    const FIELD_READS: Option<FieldReads> = Some(FieldReads {
        member_kinds: &["member_access_expression"],
        names: NameRefSpec {
            ident_kinds: &["name"],
            kind: crate::calls::NameRefKind::Constant,
            skip_kinds: &[
                "variable_name",
                "namespace_use_declaration",
                "namespace_definition",
                "named_type",
                "optional_type",
                "union_type",
                "intersection_type",
                "qualified_name",
                "relative_name",
                "attribute_list",
                "class_constant_access_expression",
            ],
            skip_parents: &[
                "object_creation_expression",
                "named_label_statement",
                "goto_statement",
            ],
            passed_callable: Some(("function_call_expression", passed_callable)),
            ..NameRefSpec::EMPTY
        },
    });
    const VARIANT_REFS: Option<VariantRefSpec> = Some(VariantRefSpec {
        path_kinds: &["class_constant_access_expression"],
        label_parents: &["case_statement", "match_condition_list"],
        bare_label_parents: &[],
        bare_expressions: false,
        token_trees: false,
    });
    const TX: TxRule = TxRule::Nodes {
        collect: collect_symfony_transactional_scopes,
        kinds: &[NodeKind::Method, NodeKind::Function],
    };
    const STEPS: &'static [Step<PhpHook>] = &[
        step(Pass::Query, Effect::Nodes, Op::Query),
        step(Pass::Build, Effect::Neutral, Op::Hook(PhpHook::Routes)),
        step(Pass::Calls, Effect::Nodes, Op::Hook(PhpHook::Calls)),
        step(Pass::Walk, Effect::Nodes, Op::FieldReads),
        step(Pass::Walk, Effect::Nodes, Op::VariantRefs),
        step(Pass::Build, Effect::Neutral, Op::Hook(PhpHook::LaravelGate)),
        step(Pass::Walk, Effect::Neutral, Op::FunctionMeta),
        step(
            Pass::Walk,
            Effect::Neutral,
            Op::Hook(PhpHook::EnumImitation),
        ),
        step(Pass::Build, Effect::Nodes, Op::StampOwners),
        step(Pass::Build, Effect::Nodes, Op::Hook(PhpHook::ConstOwners)),
        step(
            Pass::Walk,
            Effect::Nodes,
            Op::Hook(PhpHook::QualifiedConstants),
        ),
        step(Pass::Build, Effect::Bind, Op::TxScopes),
    ];

    fn query() -> anyhow::Result<Query> {
        Ok(Query::new(
            &tree_sitter_php::LANGUAGE_PHP.into(),
            include_str!("queries.scm"),
        )?)
    }

    fn role(capture: &str) -> Option<PhpRole> {
        Some(match capture {
            "function" | "class" | "interface" | "method" | "property" | "namespace" | "trait"
            | "enum" | "enum_case_node" | "const" => PhpRole::Root,
            "type.function" | "type.method" => PhpRole::Type,
            "decorator" => PhpRole::Decorator,
            "export" => PhpRole::Export,
            "heritage" => PhpRole::Heritage,
            "import.source" => PhpRole::ImportSource,
            "import.alias" => PhpRole::ImportAlias,
            "route.scope" => PhpRole::RouteScope,
            "route.method" => PhpRole::RouteMethod,
            "route.path" => PhpRole::RoutePath,
            "route.call" | "route.chained.call" => PhpRole::RouteCall,
            "route.chained.object" => PhpRole::RouteChainedObject,
            "laravel.route.call" => PhpRole::LaravelCall,
            "laravel.route.args" => PhpRole::LaravelArgs,
            "laravel.include.call" => PhpRole::LaravelInclude,
            "laravel.route.set" => PhpRole::LaravelRouteSet,
            "blind.call_user_func" => PhpRole::CallUserFunc,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: PhpRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut PhpMatch<'t>,
        f: &mut PhpFile<'t>,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        match r {
            PhpRole::Type => {
                if let Ok(t) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
                    m.type_annotation = Some(t.to_string());
                }
            }
            PhpRole::Decorator => {
                if let Ok(d_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    m.decorators.push(d_str.to_string());
                }
            }
            PhpRole::Export => {
                if let Ok(mod_str) =
                    std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    if mod_str == "private" || mod_str == "protected" {
                        m.not_exported = true;
                    }
                }
            }
            PhpRole::Heritage => {
                if let Ok(h) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
                    m.heritage.push(h.to_string());
                }
            }
            PhpRole::ImportSource => m.import_src = Some(node),
            PhpRole::ImportAlias => m.import_alias = Some(node),
            PhpRole::Root => {
                if d.root.is_none() {
                    d.root = Some(node);
                }
            }
            PhpRole::RouteScope => {
                if let Ok(s_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    m.route_scope = Some(s_str.to_string());
                }
            }
            PhpRole::RouteMethod => {
                if let Ok(m_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    m.route_method = Some(m_str.to_uppercase());
                }
            }
            // Kept raw: `laravel_routes::route_site` joins it to the
            // group prefixes, then normalizes it like
            // `clean_route_path_lax` (`'register'` is `/register`).
            PhpRole::RoutePath => {
                m.route_path =
                    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).ok();
            }
            PhpRole::RouteCall => m.route_span_node = Some(node),
            // Walk the object chain inward to find the scoped_call root.
            // If found, treat that scope as `route_scope` so the scope-name
            // allowlist gate downstream fires the same way as for direct
            // `Route::get(...)` calls.
            PhpRole::RouteChainedObject => {
                if let Some(root_scope) = walk_to_scoped_root_scope(node, source) {
                    m.route_scope = Some(root_scope);
                }
            }
            PhpRole::LaravelCall => m.laravel_call_span = Some(node_span(&node)),
            PhpRole::LaravelArgs => m.laravel_args_node = Some(node),
            PhpRole::LaravelInclude => f.include_calls.push(node),
            PhpRole::LaravelRouteSet => {
                if !f.route_sets.iter().any(|call| call.id() == node.id()) {
                    f.route_sets.push(node);
                }
            }
            PhpRole::CallUserFunc => {
                if literal_callable(node, source).is_none() {
                    cx.push_blind(BLIND_CALL_USER_FUNC, &node);
                }
            }
        }
    }

    fn end_match<'t>(
        d: DeclSlot<'t>,
        m: &mut PhpMatch<'t>,
        f: &mut PhpFile<'t>,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        let PhpMatch {
            not_exported,
            heritage,
            type_annotation,
            decorators,
            import_src,
            import_alias,
            route_method,
            route_path,
            route_span_node,
            route_scope,
            laravel_call_span,
            laravel_args_node,
        } = std::mem::take(m);
        let is_exported = !not_exported;

        // Stage Laravel framework_ref. Walk the `arguments` node:
        //   1st `argument` (the path) is ignored here — already in routes.
        //   2nd `argument` content drives target_name:
        //     array_creation_expression → "Controller@action"
        //     anonymous_function / arrow_function → "<anonymous>"
        //     anything else → skip.
        if let (Some(span), Some(args)) = (laravel_call_span, laravel_args_node) {
            let mut cur = args.walk();
            let mut arg_count = 0;
            let mut target_name: Option<String> = None;
            for child in args.children(&mut cur) {
                if child.kind() != "argument" {
                    continue;
                }
                arg_count += 1;
                if arg_count == 2 {
                    let inner = child.child(0).unwrap_or(child);
                    target_name = match inner.kind() {
                        "array_creation_expression" => extract_controller_action(inner, source),
                        "anonymous_function" | "arrow_function" => Some("<anonymous>".to_string()),
                        _ => None,
                    };
                    break;
                }
            }
            if let Some(t) = target_name {
                f.pending_laravel.push((t, span));
            }
        }

        // Router-class allowlist gate — drop matches where the scope
        // doesn't belong to a known PHP router class. This is the
        // primary FP filter for PHP (replaces the path-shape filter
        // that would mis-reject Laravel bare paths like 'register').
        let scope_ok = route_scope
            .as_deref()
            .map(|s| PHP_ROUTER_SCOPES.contains(&s))
            .unwrap_or(false);
        if let (true, Some(rm), Some(rp), Some(rs_node)) =
            (scope_ok, route_method, route_path, route_span_node)
        {
            let exists = f
                .route_calls
                .iter()
                .any(|(method, _, call)| *method == rm && call.id() == rs_node.id());
            if !exists {
                f.route_calls.push((rm, rp, rs_node));
            }
        }

        if let (Some(n), Some(k), Some(root)) = (d.name, d.kind, d.root) {
            if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                // PHP's constructor convention is `__construct`; the spec
                // table maps it as Method, so promote here.
                let k = if k == NodeKind::Method && name_str == "__construct" {
                    NodeKind::Constructor
                } else {
                    k
                };

                // Property dedupe on name-node id so multi-declarator
                // (`public int $x, $y;`) each gets its own entry.
                let node_id = if k == NodeKind::Property {
                    n.id()
                } else {
                    root.id()
                };
                let nodes = &mut cx.nodes;
                let idx = *f.node_id_to_idx.entry(node_id).or_insert_with(|| {
                    let i = nodes.len();
                    nodes.push(RawNode {
                        is_exported,
                        type_annotation: type_annotation.clone(),
                        ..node::decl(
                            name_str.to_string(),
                            k,
                            node::span(root),
                            &source[root.start_byte()..root.end_byte()],
                        )
                    });
                    i
                });
                let entry = &mut nodes[idx];

                if !is_exported {
                    entry.is_exported = false;
                }
                if type_annotation.is_some() {
                    entry.type_annotation = type_annotation;
                }
                for h in heritage {
                    if !entry.heritage.contains(&h) {
                        entry.heritage.push(h);
                    }
                }
                for dec in decorators {
                    if !entry.decorators.contains(&dec) {
                        entry.decorators.push(dec);
                    }
                }
            }
        }

        if let Some(i_src) = import_src {
            if let Ok(src_str) = std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
            {
                let group = i_src
                    .parent()
                    .and_then(|clause| clause.parent())
                    .filter(|parent| parent.kind() == "namespace_use_group");
                let group_prefix = group
                    .and_then(|group| group.parent())
                    .and_then(|declaration| {
                        declaration
                            .named_children(&mut declaration.walk())
                            .find(|child| child.kind() == "namespace_name")
                    });
                let full_src = if let Some(p) = group_prefix {
                    if let Ok(p_str) = std::str::from_utf8(&source[p.start_byte()..p.end_byte()]) {
                        format!(
                            "{}\\{}",
                            p_str.trim_end_matches('\\'),
                            src_str.trim_start_matches('\\')
                        )
                    } else {
                        src_str.to_string()
                    }
                } else {
                    src_str.to_string()
                };

                let alias = if let Some(a) = import_alias {
                    std::str::from_utf8(&source[a.start_byte()..a.end_byte()])
                        .ok()
                        .map(|s| s.to_string())
                } else {
                    None
                };

                let imported_name = full_src.split('\\').next_back().unwrap_or("").to_string();
                let alias = if group.is_some() {
                    Some(alias.unwrap_or_else(|| imported_name.clone()))
                } else {
                    alias
                };
                let imported_name = if group.is_some() {
                    "*".to_string()
                } else {
                    imported_name
                };
                cx.imports
                    .push(imports::raw(full_src, imported_name, alias, None));
            }
        }
    }

    fn hook<'t>(h: PhpHook, _c: &Compiled<Self>, f: &mut PhpFile<'t>, cx: &mut FileCx<'t>) {
        let root = cx.tree.root_node();
        let source = cx.source;
        match h {
            PhpHook::Routes => {
                f.file_namespace = cx
                    .nodes
                    .iter()
                    .find(|node| node.kind == NodeKind::Namespace)
                    .map(|node| node.name.clone());
                let route_calls = std::mem::take(&mut f.route_calls);
                let route_sets = std::mem::take(&mut f.route_sets);
                let include_calls = std::mem::take(&mut f.include_calls);
                let names = super::laravel_routes::FileNames {
                    imports: &cx.imports,
                    namespace: f.file_namespace.as_deref(),
                };
                let route_sites = route_calls
                    .into_iter()
                    .filter_map(|(method, raw_path, call)| {
                        let site =
                            super::laravel_routes::route_site(call, raw_path, source, names)?;
                        Some((method, site, call))
                    })
                    .chain(route_sets.into_iter().flat_map(|call| {
                        super::laravel_routes::route_set_sites(call, source, names)
                            .into_iter()
                            .map(move |(method, site)| (method, site, call))
                    }));
                for (method, site, call) in route_sites {
                    cx.routes.push(RawRoute {
                        method,
                        path: site.path,
                        handler: site.handler,
                        span: node_span(&call),
                        scope: Some(Box::new(site.scope)),
                    });
                }
                let mut route_includes: Vec<RawRoute> = Vec::new();
                for call in include_calls {
                    super::laravel_routes::push_includes(call, source, names, &mut route_includes);
                }
                super::laravel_routes::resolve_include_controllers(&mut route_includes, names);
                cx.routes.append(&mut route_includes);
            }
            // Extract call sites with receiver-type binding.
            // Handles $this->method(), parent::method(), self::method(), static::method().
            // function_call_expression (bare calls) still go through the shared helper.
            PhpHook::Calls => {
                crate::calls::extract_calls(
                    root,
                    source,
                    &mut cx.nodes,
                    &["function_call_expression"],
                );
                let (raw_path_literals, raw_sql_refs, dynamic_calls) =
                    extract_php_calls_and_path_literals(root, source, &mut cx.nodes);
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
                // A fan-out names every value the method variable can hold, so its
                // site keeps a blind spot only for a late-bound receiver.
                let mut bound_cache = super::dynamic_dispatch::BoundValuesCache::default();
                for call in dynamic_calls {
                    let fanout = super::dynamic_dispatch::const_map_fanout(
                        call,
                        source,
                        &cx.nodes,
                        &mut bound_cache,
                    );
                    let keep_blind_spot = fanout.as_ref().is_none_or(|(_, late_bound)| *late_bound);
                    if keep_blind_spot {
                        cx.push_blind(BLIND_VARIABLE_METHOD_CALL, &call);
                    }
                    cx.fanout_refs.extend(fanout.map(|(fanout, _)| fanout));
                }
            }
            PhpHook::LaravelGate => {
                // Gate Laravel framework_refs by the `Illuminate` import — without
                // that, bare `Route::` is just an unrelated class name.
                if has_import_from(&cx.imports, LARAVEL_REQUIRED) {
                    for (target_name, span) in &f.pending_laravel {
                        let source_name = node::enclosing_or_module(&cx.nodes, *span);
                        cx.framework_refs.push(RawFrameworkRef {
                            source_name,
                            target_name: target_name.clone(),
                            confidence: framework_confidence::LARAVEL_ROUTE,
                            reason: "laravel-route".to_string(),
                            span: *span,
                        });
                    }
                }

                // Framework-presence gate (P1 review fix). Scope-name allowlist
                // alone isn't enough — a user-defined `class Route { static function
                // get($k) {...} }` in a non-framework PHP project would still pass
                // (reviewer's regression test confirmed). Require the file to
                // import from a known PHP web-framework namespace before any Route
                // is emitted. No path-shape filter (Laravel uses bare paths like
                // `'register'`). Spec: 2026-05-17-route-precision-design.md.
                //
                // A route file a provider loads often calls the global `Route`
                // facade alias with no `use` line. Its records stay, marked, and the
                // builder keeps them only when a file that passes this gate loads
                // the file. A file that binds `Route` to a class of its own keeps
                // nothing.
                if !has_import_from(&cx.imports, PHP_HTTP_FRAMEWORK_NAMESPACES) {
                    let own_router = binds_own_router(&cx.imports, &cx.nodes);
                    cx.routes
                        .retain_mut(|route| match route.scope.as_deref_mut() {
                            Some(scope) if !own_router => {
                                scope.needs_include = true;
                                true
                            }
                            _ => false,
                        });
                }
                // A controller imports no router; its middleware records count only
                // for the routes whose actions bind its methods.
                let names = super::laravel_routes::FileNames {
                    imports: &cx.imports,
                    namespace: f.file_namespace.as_deref(),
                };
                super::laravel_routes::push_controller_middleware(
                    root,
                    source,
                    names,
                    &mut cx.routes,
                );
            }
            // Pre-8.1 enum-imitation detection — emits one BlindSpot per class
            // that looks like `class Status { const A = 1; const B = 2; }` with
            // no instance properties. Must run after the main query loop so that
            // it can still emit for classes whose class_declaration node was
            // captured by the query (those are in `nodes`; the blind_spot is a
            // separate signal).
            PhpHook::EnumImitation => {
                detect_enum_imitation_blind_spots(root, cx.path, cx.is_test, &mut cx.blind_spots);
            }
            // Constants belong to the innermost named or anonymous type. Anonymous
            // owners retain distinct identities without exposing their names globally.
            PhpHook::ConstOwners => {
                let nodes = &mut cx.nodes;
                if nodes.iter().any(|node| node.kind == NodeKind::Const) {
                    let mut owners: Vec<_> = nodes
                        .iter()
                        .filter(|node| {
                            matches!(
                                node.kind,
                                NodeKind::Class
                                    | NodeKind::Interface
                                    | NodeKind::Trait
                                    | NodeKind::Enum
                            )
                        })
                        .map(|node| (node.name.clone(), node.span))
                        .collect();
                    owners.extend(
                        anonymous_class_body_spans(root)
                            .into_iter()
                            .map(|span| (format!("@anonymous_{}_{}", span.0, span.1), span)),
                    );
                    for node in nodes.iter_mut().filter(|node| node.kind == NodeKind::Const) {
                        if let Some(owner) = owners
                            .iter()
                            .filter(|(_, span)| {
                                ecp_core::analyzer::types::span_contains(*span, node.span)
                            })
                            .min_by_key(|(_, span)| crate::framework_helpers::span_area(*span))
                            .map(|(name, _)| name.clone())
                        {
                            node.owner_class = Some(owner);
                        }
                    }
                }
            }
            PhpHook::QualifiedConstants => {
                extract_qualified_constant_refs(root, source, &mut cx.nodes);
            }
        }
    }

    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta> {
        crate::function_meta::php::extract(root, source, nodes, category)
    }
}

fn anonymous_class_body_spans(root: tree_sitter::Node<'_>) -> Vec<(u32, u32, u32, u32)> {
    let mut bodies = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "anonymous_class" {
            if let Some(body) = node.child_by_field_name("body") {
                bodies.push(node_span(&body));
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    bodies
}
