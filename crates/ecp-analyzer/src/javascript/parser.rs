use super::receiver_types::extract_js_calls_and_path_literals;
use super::spec::JavaScriptSpec;
use crate::framework_confidence;
use crate::framework_helpers::{
    enclosing_function_name, has_import_from, js_ts_first_arg_is_literal_string, node_span,
    push_blind_spot, MODULE_LEVEL_SOURCE, SCALAR_VALUE_KINDS,
};
use crate::indirect_dispatch::{collect_js_param_names, detect_js_ts_indirect};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::algorithms::process_trace::is_test_path;
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{
    BlindSpot, LocalGraph, RawFrameworkRef, RawImport, RawNode, RawRoute,
};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

/// Blind-spot kind/hint pairs. Order matches the capture-index lookup
/// in `parse_file` so the dispatch reads as a flat table.
const BLIND_SPEC: &[(&str, &str)] = &[
    (
        "js-eval",
        "eval(arg) — runtime JS execution; argument expression is not statically determinable as a callee",
    ),
    (
        "js-function-ctor",
        "Function(arg) / new Function(arg) — runtime function compilation; body source is not statically determinable",
    ),
    (
        "js-dynamic-import",
        "import(<expr>) with non-literal specifier — dynamic module loading; target module depends on runtime value",
    ),
    (
        "js-dynamic-require",
        "require(<expr>) with non-literal specifier — dynamic CommonJS load; target module depends on runtime value",
    ),
    (
        "js-object-freeze-enum",
        "const X = Object.freeze({...}) with ≥2 scalar entries — JS enum imitation pattern; verify before treating as plain Const",
    ),
];

/// Returns true if `node` is a `variable_declaration` or `lexical_declaration`
/// whose initializer is `Object.freeze(<obj>)` with ≥2 scalar-valued pairs.
///
/// Scalar values: number, string, true, false, null.
/// Non-scalar (excluded): arrow_function, function_expression, call_expression,
/// template_string with substitutions, identifier.
fn is_object_freeze_enum(node: &tree_sitter::Node) -> bool {
    check_object_freeze_enum(node).unwrap_or(false)
}

/// Whether `node`'s subtree contains a `call_expression`. Gates emission of
/// `<anonymous>` callback nodes so empty callbacks (e.g. `arr.map(x => x * 2)`)
/// stay out of the graph.
fn body_has_call(node: tree_sitter::Node) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "call_expression" {
            return true;
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    false
}

fn check_object_freeze_enum(node: &tree_sitter::Node) -> Option<bool> {
    // Walk: declaration → variable_declarator → call_expression → arguments → object
    let declarator = (0..node.child_count())
        .filter_map(|i| node.child(i as u32))
        .find(|c| c.kind() == "variable_declarator")?;

    let call = (0..declarator.child_count())
        .filter_map(|i| declarator.child(i as u32))
        .find(|c| c.kind() == "call_expression")?;

    let args = (0..call.child_count())
        .filter_map(|i| call.child(i as u32))
        .find(|c| c.kind() == "arguments")?;

    // Find the object literal inside arguments (skip `(` and `)`)
    let obj = (0..args.child_count())
        .filter_map(|i| args.child(i as u32))
        .find(|c| c.kind() == "object")?;

    // Count pair nodes with scalar values only
    let scalar_pairs = (0..obj.child_count())
        .filter_map(|i| obj.child(i as u32))
        .filter(|c| c.kind() == "pair")
        .filter(|pair| {
            // value is the last named child of the pair
            let value = (0..pair.child_count())
                .filter_map(|i| pair.child(i as u32))
                .next_back();
            value.is_some_and(|v| SCALAR_VALUE_KINDS.contains(&v.kind()))
        })
        .count();

    Some(scalar_pairs >= 2)
}

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_javascript::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct JavaScriptProvider {
    query: Query,
    /// Capture index → NodeKind mapping, pre-resolved from
    /// `JavaScriptSpec::CAPTURE_KIND` at provider construction. The hot loop
    /// looks up by integer index (cap.index as usize) — no per-capture string
    /// compare. Source of truth: `spec.rs` const tables.
    capture_kind_by_idx: Vec<Option<NodeKind>>,
    /// Cached variable capture indices — variable emission has special
    /// post-processing (arrow dedup, const/let/var kind split) that stays in
    /// parser.rs; indices are pre-resolved here to avoid per-call string lookup.
    idx_variable_name: Option<u32>,
    idx_variable: Option<u32>,
    idx_export_variable: Option<u32>,
}

impl JavaScriptProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_javascript::LANGUAGE.into();
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        let query = Query::new(&language, &query_source)?;

        // Pre-resolve capture-name → NodeKind from the spec table so the
        // hot loop stays an integer-index lookup (no per-capture string compare).
        // Variable has special post-processing so its name capture is also in
        // the spec table for completeness, but variable emission is handled
        // via the separate idx_variable_name path below.
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| JavaScriptSpec::CAPTURE_KIND.get(name).copied())
            .collect();

        let idx_variable_name = query.capture_index_for_name("variable.name");
        let idx_variable = query.capture_index_for_name("variable");
        let idx_export_variable = query.capture_index_for_name("export.variable");
        Ok(Self {
            query,
            capture_kind_by_idx,
            idx_variable_name,
            idx_variable,
            idx_export_variable,
        })
    }
}

impl LanguageProvider for JavaScriptProvider {
    fn name(&self) -> &'static str {
        "javascript"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        // Spans of already-emitted nodes — lookup table for the Variable
        // dedup at L301 (was O(n²) linear scan of `nodes`).
        let mut emitted_spans: std::collections::HashSet<(u32, u32, u32, u32)> =
            std::collections::HashSet::new();
        let mut imports: Vec<RawImport> = Vec::new();
        let mut routes: Vec<RawRoute> = Vec::new();

        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_export = self.query.capture_index_for_name("export");
        let idx_decorator = self.query.capture_index_for_name("decorator");

        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");
        let idx_import_namespace = self.query.capture_index_for_name("import.namespace");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_property = self.query.capture_index_for_name("property");
        let idx_function_anonymous = self.query.capture_index_for_name("function.anonymous");

        // 14-lang-parity Variable captures — use cached indices.
        let idx_variable_name = self.idx_variable_name;
        let idx_variable = self.idx_variable;
        let idx_export_variable = self.idx_export_variable;

        let idx_route_method = self.query.capture_index_for_name("route.method");
        let idx_route_path = self.query.capture_index_for_name("route.path");
        let idx_route_call = self.query.capture_index_for_name("route.call");

        let idx_express_handler = self.query.capture_index_for_name("express.route.handler");
        let idx_hapi_handler = self.query.capture_index_for_name("hapi.route.handler");

        let idx_blind_eval = self.query.capture_index_for_name("blind.eval");
        let idx_blind_function_ctor = self.query.capture_index_for_name("blind.function_ctor");
        let idx_blind_dynamic_import = self.query.capture_index_for_name("blind.dynamic_import");
        let idx_blind_dynamic_require = self.query.capture_index_for_name("blind.dynamic_require");
        let idx_blind_object_freeze_enum = self
            .query
            .capture_index_for_name("blind.object_freeze_enum");

        // Pending framework-handler captures: (handler_name, capture_span).
        // Enclosing function is resolved after all nodes are collected so the
        // `enclosing_function_name` span search sees the full node set.
        let mut pending_express_handlers: Vec<(String, (u32, u32, u32, u32))> = Vec::new();
        let mut pending_hapi_handlers: Vec<(String, (u32, u32, u32, u32))> = Vec::new();
        let mut blind_spots: Vec<BlindSpot> = Vec::new();
        let is_test_file = is_test_path(path.to_str().unwrap_or(""));

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut heritage = Vec::new();
            let mut is_exported = false;
            let mut decorators = Vec::new();

            let mut import_name = None;
            let mut import_src = None;
            let mut import_alias = None;
            let mut is_import_namespace = false;

            let mut variable_name_node: Option<tree_sitter::Node> = None;
            let mut variable_root_node: Option<tree_sitter::Node> = None;
            let mut is_exported_variable = false;

            let mut route_method = None;
            let mut route_path = None;
            let mut is_route = false;

            for cap in m.captures {
                let cap_idx = cap.index;
                if Some(cap_idx) == idx_variable_name {
                    // Variable emission has dedicated post-processing (arrow
                    // dedup, const/let/var kind split, span dedup) — handled
                    // via the separate variable_name_node path below.
                    variable_name_node = Some(cap.node);
                } else if Some(cap_idx) == idx_variable {
                    variable_root_node = Some(cap.node);
                } else if Some(cap_idx) == idx_export_variable {
                    is_exported_variable = true;
                    if variable_root_node.is_none() {
                        variable_root_node = Some(cap.node);
                    }
                } else if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap_idx as usize)
                    .copied()
                    .flatten()
                {
                    // Single config-driven dispatch replaces the three explicit
                    // Function/Class/Method name arms. Source of truth:
                    // JavaScriptSpec::CAPTURE_KIND in spec.rs.
                    name_node = Some(cap.node);
                    kind = Some(k_from_spec);
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(h_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h_str.to_string());
                    }
                } else if Some(cap_idx) == idx_export {
                    is_exported = true;
                } else if Some(cap_idx) == idx_decorator {
                    if let Ok(d_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d_str.to_string());
                    }
                } else if Some(cap_idx) == idx_import_name {
                    import_name = Some(cap.node);
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_import_alias {
                    import_alias = Some(cap.node);
                } else if Some(cap_idx) == idx_import_namespace {
                    is_import_namespace = true;
                } else if Some(cap_idx) == idx_route_method {
                    route_method = Some(cap.node);
                } else if Some(cap_idx) == idx_route_path {
                    route_path = Some(cap.node);
                } else if Some(cap_idx) == idx_route_call {
                    is_route = true;
                    root_span_node = Some(cap.node);
                } else if Some(cap_idx) == idx_express_handler {
                    // The handler can be an identifier, member access, or
                    // an inline function/arrow expression (fixed: PR #2
                    // review issue #2). Inline functions have no symbolic
                    // target — emit "<anonymous>" so shape_check etc.
                    // recognise the route exists but is unreferenceable.
                    let kind = cap.node.kind();
                    let target_name = if kind == "arrow_function" || kind == "function_expression" {
                        "<anonymous>".to_string()
                    } else if let Ok(text) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        text.to_string()
                    } else {
                        continue;
                    };
                    pending_express_handlers.push((target_name, node_span(&cap.node)));
                } else if Some(cap_idx) == idx_hapi_handler {
                    if let Ok(handler_name) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        pending_hapi_handlers
                            .push((handler_name.to_string(), node_span(&cap.node)));
                    }
                } else if Some(cap_idx) == idx_function_anonymous {
                    // Anonymous callback (arg-position arrow / function expression).
                    // Emit an `<anonymous>` Function node only when the body holds a
                    // call, so attach_to_enclosing can host those calls instead of
                    // dropping them; empty callbacks stay out of the graph.
                    if body_has_call(cap.node) {
                        let span = node_span(&cap.node);
                        if emitted_spans.insert(span) {
                            nodes.push(RawNode {
                                decorators: Vec::new(),
                                is_exported: false,
                                heritage: Vec::new(),
                                type_annotation: None,
                                // Position-suffixed so each anonymous callback gets a
                                // distinct uid (uid = kind+path+owner+name, no span) —
                                // bare "<anonymous>" collides → all but the first are
                                // dropped as tombstones at GraphBuilder::build.
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
                } else if Some(cap_idx) == idx_blind_function_ctor {
                    push_blind_spot(
                        &mut blind_spots,
                        BLIND_SPEC[1],
                        &cap.node,
                        path,
                        is_test_file,
                    );
                } else if Some(cap_idx) == idx_blind_dynamic_import {
                    if !js_ts_first_arg_is_literal_string(&cap.node) {
                        push_blind_spot(
                            &mut blind_spots,
                            BLIND_SPEC[2],
                            &cap.node,
                            path,
                            is_test_file,
                        );
                    }
                } else if Some(cap_idx) == idx_blind_dynamic_require {
                    if !js_ts_first_arg_is_literal_string(&cap.node) {
                        push_blind_spot(
                            &mut blind_spots,
                            BLIND_SPEC[3],
                            &cap.node,
                            path,
                            is_test_file,
                        );
                    }
                } else if Some(cap_idx) == idx_blind_object_freeze_enum {
                    if is_object_freeze_enum(&cap.node) {
                        push_blind_spot(
                            &mut blind_spots,
                            BLIND_SPEC[4],
                            &cap.node,
                            path,
                            is_test_file,
                        );
                    }
                } else if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_method
                    || Some(cap_idx) == idx_property
                {
                    root_span_node = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    let node_span = (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    );
                    // JS has no separate constructor_declaration node — constructors
                    // are method_definition nodes whose property_identifier is literally
                    // "constructor". Promote them here so the graph emits
                    // NodeKind::Constructor (parity with Java / Dart / C#).
                    let k = if k == NodeKind::Method && name_str == "constructor" {
                        NodeKind::Constructor
                    } else {
                        k
                    };

                    let mut existing_found = false;
                    for node in &mut nodes {
                        if node.span == node_span && node.name == name_str {
                            if is_exported {
                                node.is_exported = true;
                            }
                            if !heritage.is_empty() {
                                for h in &heritage {
                                    if !node.heritage.contains(h) {
                                        node.heritage.push(h.clone());
                                    }
                                }
                            }
                            if !decorators.is_empty() {
                                for d in &decorators {
                                    if !node.decorators.contains(d) {
                                        node.decorators.push(d.clone());
                                    }
                                }
                            }
                            existing_found = true;
                            break;
                        }
                    }

                    if !existing_found {
                        emitted_spans.insert(node_span);
                        nodes.push(RawNode {
                            decorators: decorators.clone(),
                            is_exported,
                            heritage: heritage.clone(),
                            type_annotation: None,
                            name: name_str.to_string(),
                            kind: k,
                            span: node_span,
                            calls: Vec::new(),
                            field_reads: Vec::new(),
                            owner_class: None,
                            content_hash: ecp_core::uid::xxh3_64_bytes(
                                &source[root.start_byte()..root.end_byte()],
                            ),
                        });
                    }
                }
            }

            // Variable / Const emission — module-level only via queries.scm's
            // `(program …)` anchor on the bare `lexical_declaration` /
            // `variable_declaration` patterns. Arrow-function-assigned
            // declarators are already captured as `name.function` above and
            // produce a Function node; skip them here so we don't shadow it
            // with a duplicate Variable.
            if let (Some(vn), Some(vr)) = (variable_name_node, variable_root_node) {
                // `vr` is the lexical_declaration / variable_declaration node
                // itself, except for the `export_statement` wrapper which binds
                // `@variable` to the inner `variable_declarator` — in that case
                // we walk up one step to get the declaration for kind detection.
                let decl_node = if vr.kind() == "variable_declarator" {
                    vr.parent().unwrap_or(vr)
                } else {
                    vr
                };
                // Find the declarator to check its value kind (skip arrow functions).
                let mut is_arrow = false;
                let mut cur = decl_node.child(0);
                while let Some(child) = cur {
                    if child.kind() == "variable_declarator" {
                        // Check if value is an arrow_function — those are already
                        // emitted as Function nodes by the @name.function capture.
                        for i in 0..child.child_count() {
                            if let Some(gc) = child.child(i as u32) {
                                if gc.kind() == "arrow_function" {
                                    is_arrow = true;
                                    break;
                                }
                            }
                        }
                        break;
                    }
                    cur = child.next_sibling();
                }

                if !is_arrow {
                    {
                        if let Ok(name_str) =
                            std::str::from_utf8(&source[vn.start_byte()..vn.end_byte()])
                        {
                            let start = decl_node.start_position();
                            let end = decl_node.end_position();
                            let var_span = (
                                start.row as u32,
                                start.column as u32,
                                end.row as u32,
                                end.column as u32,
                            );
                            // Dedup: skip if a node already occupies this span (an
                            // arrow-function const declarator was already pushed as
                            // Function via the @function.name capture). HashSet
                            // lookup is O(1) — earlier impl scanned `nodes` linearly
                            // which is O(k²) per file on declarator-heavy code.
                            let already_emitted = emitted_spans.contains(&var_span);
                            if !already_emitted {
                                // `const` declarations map to NodeKind::Const so
                                // parity with ref-gitnexus is maintained; var/let → Variable.
                                let var_kind = if decl_node.kind() == "lexical_declaration" {
                                    // lexical_declaration covers both `let` and `const`.
                                    // Inspect the first token to distinguish them.
                                    let is_const = decl_node
                                        .child(0)
                                        .and_then(|t| {
                                            std::str::from_utf8(
                                                &source[t.start_byte()..t.end_byte()],
                                            )
                                            .ok()
                                            .map(|s| s == "const")
                                        })
                                        .unwrap_or(false);
                                    if is_const {
                                        NodeKind::Const
                                    } else {
                                        NodeKind::Variable
                                    }
                                } else {
                                    NodeKind::Variable
                                };
                                nodes.push(RawNode {
                                    decorators: vec![],
                                    is_exported: is_exported_variable || is_exported,
                                    heritage: vec![],
                                    type_annotation: None,
                                    name: name_str.to_string(),
                                    kind: var_kind,
                                    span: var_span,
                                    calls: Vec::new(),
                                    field_reads: Vec::new(),
                                    owner_class: None,
                                    content_hash: ecp_core::uid::xxh3_64_bytes(
                                        &source[decl_node.start_byte()..decl_node.end_byte()],
                                    ),
                                });
                            }
                        }
                    }
                }
            }

            if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
                if let (Ok(name_str), Ok(src_str)) = (
                    std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]),
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    let alias = import_alias.and_then(|a| {
                        std::str::from_utf8(&source[a.start_byte()..a.end_byte()])
                            .ok()
                            .map(|s| s.to_string())
                    });

                    imports.push(RawImport {
                        alias,
                        imported_name: name_str.to_string(),
                        source: src_str.to_string(),
                        binding_kind: None,
                    });
                }
            }

            // Namespace re-export `export * as ns from 'lib'` — no `name`
            // identifier exists; emit with "*" sentinel as imported_name.
            if is_import_namespace {
                if let (Some(a), Some(i_src)) = (import_alias, import_src) {
                    if let (Ok(alias_str), Ok(src_str)) = (
                        std::str::from_utf8(&source[a.start_byte()..a.end_byte()]),
                        std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                    ) {
                        imports.push(RawImport {
                            alias: Some(alias_str.to_string()),
                            imported_name: "*".to_string(),
                            source: src_str.to_string(),
                            binding_kind: None,
                        });
                    }
                }
            }

            if is_route {
                if let (Some(r_method), Some(r_path), Some(root)) =
                    (route_method, route_path, root_span_node)
                {
                    if let (Ok(method_str), Ok(path_str)) = (
                        std::str::from_utf8(&source[r_method.start_byte()..r_method.end_byte()]),
                        std::str::from_utf8(&source[r_path.start_byte()..r_path.end_byte()]),
                    ) {
                        let start = root.start_position();
                        let end = root.end_position();
                        routes.push(RawRoute {
                            method: method_str.to_string(),
                            path: path_str.to_string(),
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
            }
        }

        // Extract call sites with receiver-type binding:
        // - `this.method()` inside a class body → `ClassName.method`
        // - `obj.method()` (no type info in JS) → `obj.method` (qualified for resolver)
        // - `fn()` → `fn`
        let raw_path_literals =
            extract_js_calls_and_path_literals(tree.root_node(), source, &mut nodes);
        crate::calls::extract_field_reads(
            tree.root_node(),
            source,
            &mut nodes,
            &["member_expression"],
        );

        // Framework-presence gates: only emit Express/Hapi refs when the file
        // actually imports the matching package. Each framework has its own
        // explicit signal list (rather than a shared regex) so adding more
        // frameworks stays a one-liner.
        const EXPRESS_REQUIRED: &[&str] = &["express"];
        const HAPI_REQUIRED: &[&str] = &["@hapi/hapi", "hapi"];
        let has_express = has_import_from(&imports, EXPRESS_REQUIRED);
        let has_hapi = has_import_from(&imports, HAPI_REQUIRED);

        // Path-shape filter for generic Route emission. The JS parser
        // captures imports via ES `import` statements only — CommonJS
        // `require()` is not tracked, so a framework-presence gate would
        // regress Node.js codebases that use `require('express')`. The
        // path-shape predicate alone removes the dominant FP class
        // (`Map.get("k")` / `headers.get("x-trace")` / `cache.get(id)`)
        // because none of those literals start with `/`. Spec:
        // `docs/superpowers/specs/2026-05-17-route-precision-design.md`.
        routes.retain_mut(|r| match crate::route_detector::clean_route_path(&r.path) {
            Some(clean) => {
                r.path = clean;
                true
            }
            None => false,
        });

        // Resolve framework-ref enclosing functions via span containment.
        // Module-level captures use the MODULE_LEVEL_SOURCE sentinel (consistent
        // with TS Express and Actix).
        let mut framework_refs: Vec<RawFrameworkRef> = Vec::new();
        if has_express {
            framework_refs.extend(pending_express_handlers.into_iter().map(|(target, span)| {
                let source_name = enclosing_function_name(&nodes, span)
                    .unwrap_or_else(|| MODULE_LEVEL_SOURCE.to_string());
                RawFrameworkRef {
                    source_name,
                    target_name: target,
                    confidence: framework_confidence::EXPRESS_ROUTE,
                    reason: "express-route".to_string(),
                    span,
                }
            }));
        }
        if has_hapi {
            framework_refs.extend(pending_hapi_handlers.into_iter().map(|(target, span)| {
                let source_name = enclosing_function_name(&nodes, span)
                    .unwrap_or_else(|| MODULE_LEVEL_SOURCE.to_string());
                RawFrameworkRef {
                    source_name,
                    target_name: target,
                    confidence: framework_confidence::HAPI_ROUTE,
                    reason: "hapi-route".to_string(),
                    span,
                }
            }));
        }

        let param_names = collect_js_param_names(tree.root_node(), source);
        let call_metas = detect_js_ts_indirect(tree.root_node(), source, &nodes, &param_names);
        let file_category = crate::resolution::builder::determine_category(&path.to_string_lossy());
        let raw_function_metas = crate::function_meta::javascript::extract(
            tree.root_node(),
            source,
            &nodes,
            file_category,
        );

        crate::framework_helpers::stamp_owner_class_by_span(&mut nodes);
        crate::framework_helpers::stamp_owner_fn_by_span(&mut nodes);

        let event_topics = {
            let topics = crate::event_topic::extract_event_topics(
                &tree,
                source,
                &self.query,
                &[
                    crate::event_topic::REDIS_JS,
                    crate::event_topic::KAFKA_NODE,
                    crate::event_topic::RABBITMQ_JS,
                    crate::event_topic::SQS_JS,
                ],
                &imports,
            );
            (!topics.is_empty()).then(|| topics.into_boxed_slice())
        };

        let path_literals =
            (!raw_path_literals.is_empty()).then(|| raw_path_literals.into_boxed_slice());

        Ok(LocalGraph {
            content_hash: [0; 8],
            routes,
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs,
            fanout_refs: vec![],
            blind_spots,
            schema_fields: None,
            event_topics,
            tx_scopes: None,
            path_literals,
            sql_refs: None,
            call_metas,
            raw_function_metas,
        })
    }
}
