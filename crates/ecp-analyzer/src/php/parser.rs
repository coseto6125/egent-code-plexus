use super::receiver_types::extract_php_calls;
use super::spec::PhpSpec;
use crate::framework_confidence;
use crate::framework_helpers::{
    enclosing_function_name, has_import_from, node_span, MODULE_LEVEL_SOURCE,
};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

/// Laravel route detection import gate. Ported from upstream
/// `gitnexus/src/core/group/extractors/http-patterns/php.ts:34-42`.
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
const PHP_ROUTER_SCOPES: &[&str] = &["Route", "Router", "Slim", "Symfony"];

/// Walk a chained member-call expression inward through any depth of
/// `member_call_expression` until it reaches a `scoped_call_expression` root.
/// Returns the scope name (`Route`, `Router`, etc.) when the chain terminates
/// in a scoped call; `None` for any other shape (regular method calls, deep
/// object navigation, etc.).
///
/// Powers chained-route detection like `Route::middleware(['auth'])->get(...)`
/// or `Route::middleware(...)->prefix(...)->post(...)`. The query captures the
/// outer member_call; this walks the object field inward to find the root.
fn walk_to_scoped_root_scope(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
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

        Ok(Self {
            query,
            capture_kind_by_idx,
        })
    }
}

impl LanguageProvider for PhpProvider {
    fn name(&self) -> &'static str {
        "php"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let language = tree_sitter_php::LANGUAGE_PHP.into();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let tree = parse_with_budget(&mut parser, source, ParseBudget::DEFAULT)
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

        let idx_type_function = self.query.capture_index_for_name("type.function");
        let idx_type_method = self.query.capture_index_for_name("type.method");
        let idx_export = self.query.capture_index_for_name("export");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_decorator = self.query.capture_index_for_name("decorator");

        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");
        let idx_import_prefix = self.query.capture_index_for_name("import.prefix");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_interface = self.query.capture_index_for_name("interface");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_property = self.query.capture_index_for_name("property");

        let idx_namespace = self.query.capture_index_for_name("namespace");
        let idx_trait = self.query.capture_index_for_name("trait");
        let idx_enum = self.query.capture_index_for_name("enum");
        let idx_const = self.query.capture_index_for_name("const");

        let idx_route_call = self.query.capture_index_for_name("route.call");
        let idx_route_scope = self.query.capture_index_for_name("route.scope");
        let idx_route_method = self.query.capture_index_for_name("route.method");
        let idx_route_path = self.query.capture_index_for_name("route.path");
        // Chained-call variant: `Route::middleware(...)->get('/x', ...)`.
        let idx_route_chained_call = self.query.capture_index_for_name("route.chained.call");
        let idx_route_chained_object = self.query.capture_index_for_name("route.chained.object");

        // Laravel `Route::method('/path', <handler>)`. The outer call
        // anchors the match; the parser walks the `arguments` node to
        // extract path + handler shape.
        let idx_laravel_call = self.query.capture_index_for_name("laravel.route.call");
        let idx_laravel_args = self.query.capture_index_for_name("laravel.route.args");

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
            let mut import_prefix = None;

            let mut route_method = None;
            let mut route_path = None;
            let mut route_span_node = None;
            let mut route_scope: Option<String> = None;

            // Per-match Laravel route captures. `laravel_call_span` is
            // the whole `Route::method(...)` expression; `laravel_args_node`
            // is the `arguments` node — we walk it to find the 2nd arg.
            let mut laravel_call_span: Option<(u32, u32, u32, u32)> = None;
            let mut laravel_args_node: Option<tree_sitter::Node> = None;

            for cap in m.captures {
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
                } else if Some(cap_idx) == idx_import_prefix {
                    import_prefix = Some(cap.node);
                } else if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_interface
                    || Some(cap_idx) == idx_method
                    || Some(cap_idx) == idx_property
                    || Some(cap_idx) == idx_namespace
                    || Some(cap_idx) == idx_trait
                    || Some(cap_idx) == idx_enum
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
                    if let Ok(p_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        // Laravel allows leading-slash-optional routes:
                        // `Route::get('register', ...)` is semantically `/register`.
                        // `clean_route_path_lax` strips quotes and prepends `/`
                        // when missing — same helper Python parser uses for
                        // Sanic / Flask / FastAPI bare paths so the builder's
                        // strict `looks_like_path` filter accepts both forms.
                        route_path = crate::route_detector::clean_route_path_lax(p_str);
                    }
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
                let start = rs_node.start_position();
                let end = rs_node.end_position();
                let exists = routes.iter().any(|r: &RawRoute| {
                    r.method == rm
                        && r.path == rp
                        && r.span
                            == (
                                start.row as u32,
                                start.column as u32,
                                end.row as u32,
                                end.column as u32,
                            )
                });
                if !exists {
                    routes.push(RawRoute {
                        method: rm,
                        path: rp,
                        handler: None,
                        span: (
                            start.row as u32,
                            start.column as u32,
                            end.row as u32,
                            end.column as u32,
                        ),
                    });
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
                            owner_class: None,
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
                    let full_src = if let Some(p) = import_prefix {
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

                    let imported_name = if let Some(ref a_str) = alias {
                        a_str.clone()
                    } else {
                        full_src.split('\\').next_back().unwrap_or("").to_string()
                    };

                    imports.push(RawImport {
                        alias,
                        imported_name,
                        source: full_src,
                        binding_kind: None,
                    });
                }
            }
        }

        // `nodes` already in source order — Vec + idx-map at parse-loop start.

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
        extract_php_calls(tree.root_node(), source, &mut nodes);

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
        if !has_import_from(&imports, PHP_HTTP_FRAMEWORK_NAMESPACES) {
            routes.clear();
        }

        crate::framework_helpers::stamp_owner_class_by_span(&mut nodes);
        Ok(LocalGraph {
            content_hash: [0; 8],
            routes,
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs,
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            call_metas: vec![],
            raw_function_metas: vec![],
        })
    }
}
