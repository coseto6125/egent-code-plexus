use super::receiver_types::extract_php_calls_and_path_literals;
use super::spec::PhpSpec;
use crate::framework_confidence;
use crate::framework_helpers::{
    collect_symfony_transactional_scopes, enclosing_function_name, has_import_from, node_span,
    push_blind_spot, MODULE_LEVEL_SOURCE,
};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{
    BlindSpot, LocalGraph, NameRef, NameRefKind, RawFrameworkRef, RawImport, RawNode,
};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

/// Blind-spot kind/hint pairs, indexed by `parse_file`: the first three by
/// the capture-index dispatch, the last after the receiver walk.
const BLIND_SPEC: &[(&str, &str)] = &[
    (
        "php-eval",
        "eval(arg) — runtime PHP code execution; argument expression is not statically determinable as a callee",
    ),
    (
        "php-call-user-func",
        "call_user_func(<callable>, ...) with non-literal callable — dispatch through a variable or expression resolved at runtime",
    ),
    (
        "php-variable-call",
        "$<var>(...) — variable-function call; target callable bound at runtime via the variable's value",
    ),
    (
        "php-variable-method-call",
        "Cls::$m() / $o->$m() / $o?->{expr}() — method name bound at runtime; no edge names the called method",
    ),
];

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

/// Whether `node`'s subtree contains a PHP call expression. Gates emission of
/// `<anonymous>` callback nodes so empty callbacks (e.g. `array_map(fn($x) => $x * 2, $a)`)
/// stay out of the graph.
fn body_has_call(node: tree_sitter::Node) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "function_call_expression"
            | "member_call_expression"
            | "nullsafe_member_call_expression"
            | "scoped_call_expression" => {
                return true;
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    false
}

/// Record `Type::CONST` reads only when the parser emitted that exact constant
/// as a member of the named type. Enum cases still go through `VariantRef`.
fn extract_qualified_constant_refs(
    root: tree_sitter::Node<'_>,
    source: &[u8],
    nodes: &mut [RawNode],
) {
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
            let owner = match scope.trim_start_matches('\\') {
                "self" | "static" => nodes
                    .iter()
                    .filter(|raw| {
                        matches!(raw.kind, NodeKind::Function | NodeKind::Method)
                            && ecp_core::analyzer::types::span_contains(raw.span, span)
                    })
                    .min_by_key(|raw| crate::framework_helpers::span_area(raw.span))
                    .and_then(|raw| raw.owner_class.as_deref()),
                scope => {
                    let scope = scope.trim_start_matches('\\');
                    // The qualified resolver identifies its type by the final
                    // segment. Do not discard a namespace path here: two
                    // namespaces can declare the same class name.
                    (!scope.contains('\\')).then_some(scope)
                }
            };
            if let Some(owner) = owner.filter(|owner| {
                owner_counts.get(*owner) == Some(&1) && constants.contains(&(*owner, constant))
            }) {
                refs.push((span, format!("{owner}.{constant}")));
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
    query: Query,
    /// Capture index → NodeKind mapping, pre-resolved from
    /// `PhpSpec::CAPTURE_KIND` at provider construction. The hot loop
    /// looks up by integer index — equivalent perf to the previous
    /// hard-coded if-chain, but the source of truth lives in `spec.rs`.
    capture_kind_by_idx: Vec<Option<NodeKind>>,
    /// All capture indices resolved once at provider construction.
    /// Cuts ~25 `query.capture_index_for_name()` calls per parse_file
    /// (~73k hashmap lookups across the 2907-file .sample_repo corpus).
    indices: PhpCaptureIndices,
}

struct PhpCaptureIndices {
    type_function: Option<u32>,
    type_method: Option<u32>,
    export: Option<u32>,
    heritage: Option<u32>,
    decorator: Option<u32>,
    import_source: Option<u32>,
    import_alias: Option<u32>,
    function: Option<u32>,
    class: Option<u32>,
    interface: Option<u32>,
    method: Option<u32>,
    property: Option<u32>,
    namespace: Option<u32>,
    trait_: Option<u32>,
    enum_: Option<u32>,
    enum_case_node: Option<u32>,
    const_: Option<u32>,
    route_call: Option<u32>,
    route_scope: Option<u32>,
    route_method: Option<u32>,
    route_path: Option<u32>,
    route_chained_call: Option<u32>,
    route_chained_object: Option<u32>,
    laravel_call: Option<u32>,
    laravel_args: Option<u32>,
    laravel_include_call: Option<u32>,
    laravel_route_set: Option<u32>,
    // BlindSpot captures (FU-001 P5a).
    blind_eval: Option<u32>,
    blind_call_user_func: Option<u32>,
    blind_variable_call: Option<u32>,
    // Anonymous callback capture.
    function_anonymous: Option<u32>,
}

impl PhpProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_php::LANGUAGE_PHP.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;

        // Pre-resolve capture-name → NodeKind from the spec table so the
        // hot loop stays an integer-index lookup (no per-capture string
        // compare). Capture names not in the spec map yield None and
        // fall through to the metadata-only branches (heritage, decorator,
        // route, etc.).
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| PhpSpec::CAPTURE_KIND.get(name).copied())
            .collect();

        let indices = PhpCaptureIndices {
            type_function: query.capture_index_for_name("type.function"),
            type_method: query.capture_index_for_name("type.method"),
            export: query.capture_index_for_name("export"),
            heritage: query.capture_index_for_name("heritage"),
            decorator: query.capture_index_for_name("decorator"),
            import_source: query.capture_index_for_name("import.source"),
            import_alias: query.capture_index_for_name("import.alias"),
            function: query.capture_index_for_name("function"),
            class: query.capture_index_for_name("class"),
            interface: query.capture_index_for_name("interface"),
            method: query.capture_index_for_name("method"),
            property: query.capture_index_for_name("property"),
            namespace: query.capture_index_for_name("namespace"),
            trait_: query.capture_index_for_name("trait"),
            enum_: query.capture_index_for_name("enum"),
            enum_case_node: query.capture_index_for_name("enum_case_node"),
            const_: query.capture_index_for_name("const"),
            route_call: query.capture_index_for_name("route.call"),
            route_scope: query.capture_index_for_name("route.scope"),
            route_method: query.capture_index_for_name("route.method"),
            route_path: query.capture_index_for_name("route.path"),
            route_chained_call: query.capture_index_for_name("route.chained.call"),
            route_chained_object: query.capture_index_for_name("route.chained.object"),
            laravel_call: query.capture_index_for_name("laravel.route.call"),
            laravel_args: query.capture_index_for_name("laravel.route.args"),
            laravel_include_call: query.capture_index_for_name("laravel.include.call"),
            laravel_route_set: query.capture_index_for_name("laravel.route.set"),
            blind_eval: query.capture_index_for_name("blind.eval"),
            blind_call_user_func: query.capture_index_for_name("blind.call_user_func"),
            blind_variable_call: query.capture_index_for_name("blind.variable_call"),
            function_anonymous: query.capture_index_for_name("function.anonymous"),
        };

        Ok(Self {
            query,
            capture_kind_by_idx,
            indices,
        })
    }
}

impl LanguageProvider for PhpProvider {
    fn name(&self) -> &'static str {
        "php"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse php file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        use ecp_core::analyzer::types::RawRoute;
        // Vec + idx-map pattern — see java/parser.rs same-site note.
        let mut nodes: Vec<RawNode> = Vec::new();
        let mut node_id_to_idx: rustc_hash::FxHashMap<usize, usize> =
            rustc_hash::FxHashMap::default();
        let mut imports = Vec::new();
        let mut routes = Vec::new();
        // Verb calls that passed the router gates, as `(method, path
        // literal, call)`. They become `routes` once the loop has seen the
        // file's `use` imports, which an `X::class` action resolves through.
        let mut route_calls: Vec<(String, &str, tree_sitter::Node)> = Vec::new();
        // Calls and `require`s that may load a route file. A group's
        // `X::class` middleware resolves through the imports too.
        let mut include_calls: Vec<tree_sitter::Node> = Vec::new();
        // `Route::resource` / `match` / `any` calls, each registering
        // several routes or a method set.
        let mut route_sets: Vec<tree_sitter::Node> = Vec::new();
        let mut blind_spots: Vec<BlindSpot> = Vec::new();
        let file_category =
            ecp_core::file_category::determine_category(path.to_str().unwrap_or(""));
        let is_test_file = file_category == ecp_core::graph::FileCategory::Test;

        let idx = &self.indices;
        let idx_type_function = idx.type_function;
        let idx_type_method = idx.type_method;
        let idx_export = idx.export;
        let idx_heritage = idx.heritage;
        let idx_decorator = idx.decorator;

        let idx_import_source = idx.import_source;
        let idx_import_alias = idx.import_alias;

        let idx_function = idx.function;
        let idx_class = idx.class;
        let idx_interface = idx.interface;
        let idx_method = idx.method;
        let idx_property = idx.property;

        let idx_namespace = idx.namespace;
        let idx_trait = idx.trait_;
        let idx_enum = idx.enum_;
        let idx_enum_case_node = idx.enum_case_node;
        let idx_const = idx.const_;

        let idx_route_call = idx.route_call;
        let idx_route_scope = idx.route_scope;
        let idx_route_method = idx.route_method;
        let idx_route_path = idx.route_path;
        let idx_route_chained_call = idx.route_chained_call;
        let idx_route_chained_object = idx.route_chained_object;

        let idx_laravel_call = idx.laravel_call;
        let idx_laravel_args = idx.laravel_args;
        let idx_laravel_include_call = idx.laravel_include_call;
        let idx_laravel_route_set = idx.laravel_route_set;

        let idx_blind_eval = idx.blind_eval;
        let idx_blind_call_user_func = idx.blind_call_user_func;
        let idx_blind_variable_call = idx.blind_variable_call;
        let idx_function_anonymous = idx.function_anonymous;

        // Dedup spans for <anonymous> nodes — a callback may be captured once
        // per query match but must emit only one RawNode.
        let mut emitted_anon_spans: rustc_hash::FxHashSet<(u32, u32, u32, u32)> =
            rustc_hash::FxHashSet::default();

        // Pending Laravel framework refs; emitted after the loop if the
        // `Illuminate` import gate matches.
        let mut pending_laravel: Vec<(String, (u32, u32, u32, u32))> = Vec::new();

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut is_exported = true;
            let mut heritage = Vec::new();
            let mut type_annotation = None;
            let mut decorators = Vec::new();

            let mut import_src = None;
            let mut import_alias = None;

            let mut route_method = None;
            let mut route_path = None;
            let mut route_span_node = None;
            let mut route_scope: Option<String> = None;

            // Per-match Laravel route captures. `laravel_call_span` is
            // the whole `Route::method(...)` expression; `laravel_args_node`
            // is the `arguments` node — we walk it to find the 2nd arg.
            let mut laravel_call_span: Option<(u32, u32, u32, u32)> = None;
            let mut laravel_args_node: Option<tree_sitter::Node> = None;

            for cap in m.captures() {
                let cap_idx = cap.index;
                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap_idx as usize)
                    .copied()
                    .flatten()
                {
                    // Single table-driven dispatch replaces the eight explicit
                    // Function/Class/Interface/Method/Property/Namespace/Trait/Enum arms.
                    // Source of truth: PhpSpec::CAPTURE_KIND in spec.rs.
                    name_node = Some(cap.node);
                    kind = Some(k_from_spec);
                } else if Some(cap_idx) == idx_type_function || Some(cap_idx) == idx_type_method {
                    if let Ok(t) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        type_annotation = Some(t.to_string());
                    }
                } else if Some(cap_idx) == idx_decorator {
                    if let Ok(d) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d.to_string());
                    }
                } else if Some(cap_idx) == idx_export {
                    if let Ok(mod_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        if mod_str == "private" || mod_str == "protected" {
                            is_exported = false;
                        }
                    }
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.to_string());
                    }
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_import_alias {
                    import_alias = Some(cap.node);
                } else if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_interface
                    || Some(cap_idx) == idx_method
                    || Some(cap_idx) == idx_property
                    || Some(cap_idx) == idx_namespace
                    || Some(cap_idx) == idx_trait
                    || Some(cap_idx) == idx_enum
                    || Some(cap_idx) == idx_enum_case_node
                    || Some(cap_idx) == idx_const
                {
                    if root_span_node.is_none() {
                        root_span_node = Some(cap.node);
                    }
                } else if Some(cap_idx) == idx_route_scope {
                    if let Ok(s_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        route_scope = Some(s_str.to_string());
                    }
                } else if Some(cap_idx) == idx_route_method {
                    if let Ok(m_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        route_method = Some(m_str.to_uppercase());
                    }
                } else if Some(cap_idx) == idx_route_path {
                    // Kept raw: `laravel_routes::route_site` joins it to the
                    // group prefixes, then normalizes it like
                    // `clean_route_path_lax` (`'register'` is `/register`).
                    route_path =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                            .ok();
                } else if Some(cap_idx) == idx_route_call {
                    route_span_node = Some(cap.node);
                } else if Some(cap_idx) == idx_route_chained_call {
                    // Same role as idx_route_call for the chained variant — the
                    // outer member_call_expression spans the whole route registration.
                    route_span_node = Some(cap.node);
                } else if Some(cap_idx) == idx_route_chained_object {
                    // Walk the object chain inward to find the scoped_call root.
                    // If found, treat that scope as `route_scope` so the scope-name
                    // allowlist gate downstream fires the same way as for direct
                    // `Route::get(...)` calls.
                    if let Some(root_scope) = walk_to_scoped_root_scope(cap.node, source) {
                        route_scope = Some(root_scope);
                    }
                } else if Some(cap_idx) == idx_laravel_call {
                    laravel_call_span = Some(node_span(&cap.node));
                } else if Some(cap_idx) == idx_laravel_args {
                    laravel_args_node = Some(cap.node);
                } else if Some(cap_idx) == idx_laravel_include_call {
                    include_calls.push(cap.node);
                } else if Some(cap_idx) == idx_laravel_route_set {
                    if !route_sets.iter().any(|call| call.id() == cap.node.id()) {
                        route_sets.push(cap.node);
                    }
                } else if Some(cap_idx) == idx_function_anonymous {
                    // Anonymous callback (arg-position anonymous function or arrow function).
                    // Emit an `<anonymous>` Function node only when the body holds a
                    // call, so attach_to_enclosing can host those calls instead of
                    // dropping them; empty callbacks stay out of the graph.
                    if body_has_call(cap.node) {
                        let span = node_span(&cap.node);
                        if emitted_anon_spans.insert(span) {
                            nodes.push(RawNode {
                                decorators: Vec::new(),
                                is_exported: false,
                                heritage: Vec::new(),
                                type_annotation: None,
                                name: format!("<anonymous:{}:{}>", span.0 + 1, span.1),
                                kind: NodeKind::Function,
                                span,
                                calls: Vec::new(),
                                field_reads: Vec::new(),
                                owner_class: None,
                                content_hash: ecp_core::uid::xxh3_64_bytes(
                                    &source[cap.node.start_byte()..cap.node.end_byte()],
                                ),
                            });
                        }
                    }
                } else if Some(cap_idx) == idx_blind_eval {
                    push_blind_spot(
                        &mut blind_spots,
                        BLIND_SPEC[0],
                        &cap.node,
                        path,
                        is_test_file,
                    );
                } else if Some(cap_idx) == idx_blind_call_user_func {
                    if super::receiver_types::literal_callable(cap.node, source).is_none() {
                        push_blind_spot(
                            &mut blind_spots,
                            BLIND_SPEC[1],
                            &cap.node,
                            path,
                            is_test_file,
                        );
                    }
                } else if Some(cap_idx) == idx_blind_variable_call {
                    push_blind_spot(
                        &mut blind_spots,
                        BLIND_SPEC[2],
                        &cap.node,
                        path,
                        is_test_file,
                    );
                }
            }

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
                            "anonymous_function" | "arrow_function" => {
                                Some("<anonymous>".to_string())
                            }
                            _ => None,
                        };
                        break;
                    }
                }
                if let Some(t) = target_name {
                    pending_laravel.push((t, span));
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
                let exists = route_calls
                    .iter()
                    .any(|(method, _, call)| *method == rm && call.id() == rs_node.id());
                if !exists {
                    route_calls.push((rm, rp, rs_node));
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();

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
                    let idx = *node_id_to_idx.entry(node_id).or_insert_with(|| {
                        let i = nodes.len();
                        nodes.push(RawNode {
                            decorators: vec![],
                            is_exported,
                            heritage: Vec::new(),
                            type_annotation: type_annotation.clone(),
                            name: name_str.to_string(),
                            kind: k,
                            span: (
                                start.row as u32,
                                start.column as u32,
                                end.row as u32,
                                end.column as u32,
                            ),
                            calls: Vec::new(),
                            field_reads: Vec::new(),
                            owner_class: None,
                            content_hash: ecp_core::uid::xxh3_64_bytes(
                                &source[root.start_byte()..root.end_byte()],
                            ),
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
                    for d in decorators {
                        if !entry.decorators.contains(&d) {
                            entry.decorators.push(d);
                        }
                    }
                }
            }

            if let Some(i_src) = import_src {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
                {
                    let group = i_src
                        .parent()
                        .and_then(|clause| clause.parent())
                        .filter(|parent| parent.kind() == "namespace_use_group");
                    let group_prefix =
                        group
                            .and_then(|group| group.parent())
                            .and_then(|declaration| {
                                declaration
                                    .named_children(&mut declaration.walk())
                                    .find(|child| child.kind() == "namespace_name")
                            });
                    let full_src = if let Some(p) = group_prefix {
                        if let Ok(p_str) =
                            std::str::from_utf8(&source[p.start_byte()..p.end_byte()])
                        {
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

                    imports.push(RawImport {
                        alias: if group.is_some() {
                            Some(alias.unwrap_or_else(|| imported_name.clone()))
                        } else {
                            alias
                        },
                        imported_name: if group.is_some() {
                            "*".to_string()
                        } else {
                            imported_name
                        },
                        source: full_src,
                        binding_kind: None,
                    });
                }
            }
        }

        // `nodes` already in source order — Vec + idx-map at parse-loop start.

        let file_namespace = nodes
            .iter()
            .find(|node| node.kind == NodeKind::Namespace)
            .map(|node| node.name.clone());
        let names = super::laravel_routes::FileNames {
            imports: &imports,
            namespace: file_namespace.as_deref(),
        };
        let route_sites = route_calls
            .into_iter()
            .filter_map(|(method, raw_path, call)| {
                let site = super::laravel_routes::route_site(call, raw_path, source, names)?;
                Some((method, site, call))
            })
            .chain(route_sets.into_iter().flat_map(|call| {
                super::laravel_routes::route_set_sites(call, source, names)
                    .into_iter()
                    .map(move |(method, site)| (method, site, call))
            }));
        for (method, site, call) in route_sites {
            routes.push(RawRoute {
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
        routes.append(&mut route_includes);

        // Extract call sites with receiver-type binding.
        // Handles $this->method(), parent::method(), self::method(), static::method().
        // function_call_expression (bare calls) still go through the shared helper.
        use crate::calls::extract_calls;
        extract_calls(
            tree.root_node(),
            source,
            &mut nodes,
            &["function_call_expression"],
        );
        let (raw_path_literals, raw_sql_refs, dynamic_calls) =
            extract_php_calls_and_path_literals(tree.root_node(), source, &mut nodes);
        // A fan-out names every value the method variable can hold, so its
        // site keeps a blind spot only for a late-bound receiver.
        let mut fanout_refs = Vec::new();
        let mut bound_cache = super::dynamic_dispatch::BoundValuesCache::default();
        for call in dynamic_calls {
            let fanout =
                super::dynamic_dispatch::const_map_fanout(call, source, &nodes, &mut bound_cache);
            let keep_blind_spot = fanout.as_ref().is_none_or(|(_, late_bound)| *late_bound);
            if keep_blind_spot {
                push_blind_spot(&mut blind_spots, BLIND_SPEC[3], &call, path, is_test_file);
            }
            fanout_refs.extend(fanout.map(|(fanout, _)| fanout));
        }
        crate::calls::extract_field_reads(
            tree.root_node(),
            source,
            &mut nodes,
            &["member_access_expression"],
            &crate::calls::NameRefSpec {
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
                passed_callable: Some((
                    "function_call_expression",
                    super::receiver_types::passed_callable,
                )),
                ..crate::calls::NameRefSpec::EMPTY
            },
        );
        crate::calls::extract_variant_refs(
            tree.root_node(),
            source,
            &mut nodes,
            &crate::calls::VariantRefSpec {
                path_kinds: &["class_constant_access_expression"],
                label_parents: &["case_statement", "match_condition_list"],
                bare_label_parents: &[],
                bare_expressions: false,
                token_trees: false,
            },
        );

        // Gate Laravel framework_refs by the `Illuminate` import — without
        // that, bare `Route::` is just an unrelated class name.
        let mut framework_refs: Vec<RawFrameworkRef> = Vec::new();
        if has_import_from(&imports, LARAVEL_REQUIRED) {
            for (target_name, span) in &pending_laravel {
                let source_name = enclosing_function_name(&nodes, *span)
                    .unwrap_or_else(|| MODULE_LEVEL_SOURCE.to_string());
                framework_refs.push(RawFrameworkRef {
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
        if !has_import_from(&imports, PHP_HTTP_FRAMEWORK_NAMESPACES) {
            let own_router = binds_own_router(&imports, &nodes);
            routes.retain_mut(|route| match route.scope.as_deref_mut() {
                Some(scope) if !own_router => {
                    scope.needs_include = true;
                    true
                }
                _ => false,
            });
        }
        // A controller imports no router; its middleware records count only
        // for the routes whose actions bind its methods.
        super::laravel_routes::push_controller_middleware(
            tree.root_node(),
            source,
            names,
            &mut routes,
        );

        let raw_function_metas =
            crate::function_meta::php::extract(tree.root_node(), source, &nodes, file_category);

        // Pre-8.1 enum-imitation detection — emits one BlindSpot per class
        // that looks like `class Status { const A = 1; const B = 2; }` with
        // no instance properties. Must run after the main query loop so that
        // it can still emit for classes whose class_declaration node was
        // captured by the query (those are in `nodes`; the blind_spot is a
        // separate signal).
        detect_enum_imitation_blind_spots(tree.root_node(), path, is_test_file, &mut blind_spots);

        crate::framework_helpers::stamp_owner_class_by_span(&mut nodes);
        extract_qualified_constant_refs(tree.root_node(), source, &mut nodes);
        // A class constant is its class's member: only `self::STATUS` or
        // `Order::STATUS` reaches it, never a bare `STATUS`. The kinds match
        // the containers `class_membership` links a member to.
        if nodes
            .iter()
            .any(|n| n.kind == NodeKind::Const && n.owner_class.is_none())
        {
            let types: Vec<_> = nodes
                .iter()
                .filter(|n| {
                    matches!(
                        n.kind,
                        NodeKind::Class | NodeKind::Interface | NodeKind::Trait
                    )
                })
                .map(|n| (n.name.clone(), n.span))
                .collect();
            for node in nodes
                .iter_mut()
                .filter(|n| n.kind == NodeKind::Const && n.owner_class.is_none())
            {
                node.owner_class = types
                    .iter()
                    .filter(|(_, span)| ecp_core::analyzer::types::span_contains(*span, node.span))
                    .min_by_key(|(_, span)| crate::framework_helpers::span_area(*span))
                    .map(|(name, _)| name.clone());
            }
        }
        let tx_scopes =
            collect_symfony_transactional_scopes(&nodes, &[NodeKind::Method, NodeKind::Function]);
        Ok(LocalGraph {
            content_hash: [0; 8],
            routes,
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs,
            fanout_refs,
            blind_spots,
            schema_fields: None,
            event_topics: None,
            tx_scopes,
            path_literals: (!raw_path_literals.is_empty())
                .then(|| raw_path_literals.into_boxed_slice()),
            sql_refs: (!raw_sql_refs.is_empty()).then(|| raw_sql_refs.into_boxed_slice()),
            call_metas: vec![],
            raw_function_metas,
            export_aliases: None,
        })
    }
}
