use super::receiver_types::extract_php_calls;
use crate::framework_confidence;
use crate::framework_helpers::{
    enclosing_function_name, has_import_from, node_span, MODULE_LEVEL_SOURCE,
};
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
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
}

impl PhpProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_php::LANGUAGE_PHP.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
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

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse php file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        use graph_nexus_core::analyzer::types::RawRoute;
        use std::collections::HashMap;
        let mut node_map: HashMap<usize, RawNode> = HashMap::new();
        let mut imports = Vec::new();
        let mut routes = Vec::new();

        let idx_name_function = self.query.capture_index_for_name("name.function");
        let idx_name_class = self.query.capture_index_for_name("name.class");
        let idx_name_interface = self.query.capture_index_for_name("name.interface");
        let idx_name_method = self.query.capture_index_for_name("name.method");
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

        let idx_route_call = self.query.capture_index_for_name("route.call");
        let idx_route_scope = self.query.capture_index_for_name("route.scope");
        let idx_route_method = self.query.capture_index_for_name("route.method");
        let idx_route_path = self.query.capture_index_for_name("route.path");

        // Router-class allowlist gates generic Route emission. Without it,
        // `Cache::get('key')` / `Config::get('app.name')` / `Auth::get(...)`
        // all match the regex and surface as fake routes. List covers
        // mainstream PHP routers and is short enough that adding a new one
        // is a one-liner. Spec: 2026-05-17-route-precision-design.md.
        const PHP_ROUTER_SCOPES: &[&str] = &["Route", "Router", "Slim", "App", "Symfony", "Lumen"];

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
                if Some(cap_idx) == idx_name_function {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if Some(cap_idx) == idx_name_class {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if Some(cap_idx) == idx_name_interface {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Interface);
                } else if Some(cap_idx) == idx_name_method {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
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
                        let trimmed = p_str.trim_matches(|c| c == '\'' || c == '"');
                        // Laravel allows leading-slash-optional routes:
                        // `Route::get('register', ...)` is semantically `/register`.
                        // Normalize so the builder-side `looks_like_path` (which
                        // requires leading `/`) doesn't drop them. Without this,
                        // bare-path Laravel routes get extracted by the parser but
                        // silently rejected when the builder constructs Route nodes.
                        let path = if trimmed.starts_with('/') {
                            trimmed.to_string()
                        } else {
                            format!("/{trimmed}")
                        };
                        route_path = Some(path);
                    }
                } else if Some(cap_idx) == idx_route_call {
                    route_span_node = Some(cap.node);
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

                    let node_id = root.id();
                    let entry = node_map.entry(node_id).or_insert_with(|| RawNode {
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
                    });

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

        let mut nodes: Vec<RawNode> = node_map.into_values().collect();

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

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes,
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs,
            fanout_refs: vec![],
            blind_spots: vec![],
        })
    }
}
