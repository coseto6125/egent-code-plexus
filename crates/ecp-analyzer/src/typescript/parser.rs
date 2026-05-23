use super::receiver_types::{collect_local_types, extract_ts_calls_and_path_literals};
use super::spec::TypeScriptSpec;
use crate::framework_confidence;
use crate::framework_helpers::{
    collect_typeorm_transactional_scopes, enclosing_function_name, has_import_from,
    js_ts_first_arg_is_literal_string, node_span, push_blind_spot, MODULE_LEVEL_SOURCE,
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
use tree_sitter::{Parser, Query, QueryCursor};

/// Blind-spot kind/hint pairs. Order matches the capture-index lookup in
/// `parse_file` (eval / Function-ctor / dynamic-import / dynamic-require)
/// so the dispatch reads as a flat table.
const BLIND_SPEC: &[(&str, &str)] = &[
    (
        "ts-eval",
        "eval(arg) — runtime JS execution; argument expression is not statically determinable as a callee",
    ),
    (
        "ts-function-ctor",
        "Function(arg) / new Function(arg) — runtime function compilation; body source is not statically determinable",
    ),
    (
        "ts-dynamic-import",
        "import(<expr>) with non-literal specifier — dynamic module loading; target module depends on runtime value",
    ),
    (
        "ts-dynamic-require",
        "require(<expr>) with non-literal specifier — dynamic CommonJS load; target module depends on runtime value",
    ),
];

/// Scalar literal kinds that qualify an object pair value as a valid
/// enum-imitation member. Function, call, identifier, and template
/// expressions with substitutions are excluded: they indicate a plain
/// options object, not a discriminated-union enum imitation.
const SCALAR_VALUE_KINDS: &[&str] = &["number", "string", "true", "false", "null"];

/// True iff the `object` node has ≥ `min` `pair` children where every
/// value is a scalar literal (number / string / bool / null).
fn object_has_min_scalar_pairs(object_node: &tree_sitter::Node, min: usize) -> bool {
    let mut scalar_count = 0usize;
    let mut cursor = object_node.walk();
    for child in object_node.named_children(&mut cursor) {
        if child.kind() != "pair" {
            continue;
        }
        let Some(val) = child.named_child(1) else {
            return false;
        };
        if !SCALAR_VALUE_KINDS.contains(&val.kind()) {
            return false;
        }
        scalar_count += 1;
    }
    scalar_count >= min
}

/// Emit `ts-object-freeze-enum` BlindSpots for every `Object.freeze({…})`
/// call with ≥2 scalar-literal pair entries found anywhere in `root`.
/// Traversal is depth-first; the `variable_declarator` parent node is used
/// for span to match the position consumers expect.
fn collect_freeze_enum_spots(
    root: tree_sitter::Node,
    source: &[u8],
    path: &Path,
    is_test_file: bool,
    out: &mut Vec<BlindSpot>,
) {
    // Iterative DFS — avoids recursion depth limits on deeply nested sources.
    let mut stack: Vec<tree_sitter::Node> = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression" {
            if let Some(func) = node.child_by_field_name("function") {
                if func.kind() == "member_expression" {
                    let obj_ok = func.child_by_field_name("object").is_some_and(|o| {
                        matches!(
                            std::str::from_utf8(&source[o.start_byte()..o.end_byte()]),
                            Ok("Object")
                        )
                    });
                    let prop_ok = func.child_by_field_name("property").is_some_and(|p| {
                        matches!(
                            std::str::from_utf8(&source[p.start_byte()..p.end_byte()]),
                            Ok("freeze")
                        )
                    });
                    if obj_ok && prop_ok {
                        if let Some(args) = node.child_by_field_name("arguments") {
                            if let Some(obj_arg) = args.named_child(0) {
                                if obj_arg.kind() == "object"
                                    && object_has_min_scalar_pairs(&obj_arg, 2)
                                {
                                    // Prefer the enclosing variable_declarator span when available.
                                    let span_node = if node
                                        .parent()
                                        .is_some_and(|p| p.kind() == "variable_declarator")
                                        || node
                                            .parent()
                                            .is_some_and(|p| p.kind() == "lexical_declaration")
                                        || node
                                            .parent()
                                            .is_some_and(|p| p.kind() == "variable_declaration")
                                    {
                                        node.parent().unwrap()
                                    } else {
                                        node
                                    };
                                    push_blind_spot(
                                        out,
                                        (
                                            "ts-object-freeze-enum",
                                            "const X = Object.freeze({...}) or X as const with ≥2 scalar entries \
                                             — TS enum imitation; verify before treating as plain Const",
                                        ),
                                        &span_node,
                                        path,
                                        is_test_file,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        } else if node.kind() == "as_expression" {
            // `{ ... } as const` — `const` keyword is an unnamed child of
            // `as_expression`; check it before inspecting the object.
            let has_const_type = {
                let mut c = node.walk();
                let result = node
                    .children(&mut c)
                    .any(|ch| !ch.is_named() && ch.kind() == "const");
                result
            };
            if has_const_type {
                if let Some(obj_node) = node.named_child(0) {
                    if obj_node.kind() == "object" && object_has_min_scalar_pairs(&obj_node, 2) {
                        // Use the variable_declarator or declaration for span.
                        let span_node = node
                            .parent()
                            .filter(|p| p.kind() == "variable_declarator")
                            .and_then(|vd| vd.parent())
                            .filter(|p| {
                                matches!(p.kind(), "lexical_declaration" | "variable_declaration")
                            })
                            .unwrap_or(node);
                        push_blind_spot(
                            out,
                            (
                                "ts-object-freeze-enum",
                                "const X = Object.freeze({...}) or X as const with ≥2 scalar entries \
                                 — TS enum imitation; verify before treating as plain Const",
                            ),
                            &span_node,
                            path,
                            is_test_file,
                        );
                    }
                }
                // Don't recurse into as_expression children — the object literal
                // nested inside cannot be a separate enum-imitation.
                continue;
            }
        }

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
}

pub struct TypeScriptProvider {
    query: Query,
    indices: TypeScriptCaptureIndices,
    /// Capture index → NodeKind mapping, pre-resolved from
    /// `TypeScriptSpec::CAPTURE_KIND` at provider construction. The hot loop
    /// looks up by integer index (cap.index as usize) — equivalent perf to
    /// the previous hard-coded if-chain but the source of truth lives in
    /// `spec.rs` const tables.
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

struct TypeScriptCaptureIndices {
    // Root-span anchors — track outer declaration node for span/dedup.
    function: Option<u32>,
    class: Option<u32>,
    method: Option<u32>,
    constructor: Option<u32>,
    interface: Option<u32>,
    property: Option<u32>,
    const_kind: Option<u32>,
    variable: Option<u32>,
    typedef: Option<u32>,
    enum_kind: Option<u32>,
    enum_member_node: Option<u32>,
    // Metadata-only captures.
    export: Option<u32>,
    heritage: Option<u32>,
    type_ann: Option<u32>,
    decorator: Option<u32>,
    // Import captures.
    import_name: Option<u32>,
    import_alias: Option<u32>,
    import_source: Option<u32>,
    import: Option<u32>,
    /// `export * as ns from 'lib'` — captured separately because there's no
    /// `name` identifier; `imported_name` is set to the "*" sentinel.
    import_namespace: Option<u32>,
    // Route captures.
    route_method: Option<u32>,
    route_path: Option<u32>,
    route_call: Option<u32>,
    /// Named handler argument of `app.METHOD(path, handler)` — used by the
    /// builder to materialize a `HandlesRoute` edge from the handler back
    /// to the Route node. Absent for inline arrow / anonymous handlers.
    route_handler: Option<u32>,
    // Framework captures.
    express_handler: Option<u32>,
    nestjs_class: Option<u32>,
    nestjs_method: Option<u32>,
    /// `@Get('users')` / `@Post(':id')` decorator-route captures — separate
    /// pattern in frameworks.scm because the decorator is independent of the
    /// `@Controller` shape (any method-level decorator carrying a path-shaped
    /// string literal is a route, gated by `has_nestjs` import presence).
    nestjs_decorator_verb: Option<u32>,
    nestjs_decorator_path: Option<u32>,
    // BlindSpot captures (FU-001 P1).
    blind_eval: Option<u32>,
    blind_function_ctor: Option<u32>,
    blind_dynamic_import: Option<u32>,
    blind_dynamic_require: Option<u32>,
}

impl TypeScriptProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        let query = Query::new(&language, &query_source)?;
        let indices = TypeScriptCaptureIndices {
            function: query.capture_index_for_name("function"),
            class: query.capture_index_for_name("class"),
            method: query.capture_index_for_name("method"),
            constructor: query.capture_index_for_name("constructor"),
            interface: query.capture_index_for_name("interface"),
            property: query.capture_index_for_name("property"),
            const_kind: query.capture_index_for_name("const"),
            variable: query.capture_index_for_name("variable"),
            typedef: query.capture_index_for_name("typedef"),
            enum_kind: query.capture_index_for_name("enum"),
            enum_member_node: query.capture_index_for_name("enum_member_node"),
            export: query.capture_index_for_name("export"),
            heritage: query.capture_index_for_name("heritage"),
            type_ann: query.capture_index_for_name("type"),
            decorator: query.capture_index_for_name("decorator"),
            import_name: query.capture_index_for_name("import.name"),
            import_alias: query.capture_index_for_name("import.alias"),
            import_source: query.capture_index_for_name("import.source"),
            import: query.capture_index_for_name("import"),
            import_namespace: query.capture_index_for_name("import.namespace"),
            route_method: query.capture_index_for_name("route.method"),
            route_path: query.capture_index_for_name("route.path"),
            route_call: query.capture_index_for_name("route.call"),
            route_handler: query.capture_index_for_name("route.handler"),
            express_handler: query.capture_index_for_name("express.route.handler"),
            nestjs_class: query.capture_index_for_name("nestjs.controller.class"),
            nestjs_method: query.capture_index_for_name("nestjs.method.name"),
            nestjs_decorator_verb: query.capture_index_for_name("nestjs.decorator.verb"),
            nestjs_decorator_path: query.capture_index_for_name("nestjs.decorator.path"),
            blind_eval: query.capture_index_for_name("blind.eval"),
            blind_function_ctor: query.capture_index_for_name("blind.function_ctor"),
            blind_dynamic_import: query.capture_index_for_name("blind.dynamic_import"),
            blind_dynamic_require: query.capture_index_for_name("blind.dynamic_require"),
        };

        // Pre-resolve capture-name → NodeKind from the spec table so the
        // hot loop stays an integer-index lookup (no per-capture string
        // compare). Capture names not in the spec map yield None and fall
        // through to the metadata-only branches below.
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| TypeScriptSpec::CAPTURE_KIND.get(name).copied())
            .collect();

        Ok(Self {
            query,
            indices,
            capture_kind_by_idx,
        })
    }
}

impl LanguageProvider for TypeScriptProvider {
    fn name(&self) -> &'static str {
        "typescript"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let tree = parse_with_budget(&mut parser, source, ParseBudget::DEFAULT)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse typescript file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();
        let mut routes: Vec<RawRoute> = Vec::new();
        // Pending framework-handler captures: (handler_name, capture_span).
        // Enclosing function is resolved after all nodes are collected.
        let mut pending_express_handlers: Vec<(String, (u32, u32, u32, u32))> = Vec::new();
        // (class_name, method_name, method_span)
        type NestJsHandler = (String, String, (u32, u32, u32, u32));
        let mut pending_nestjs_handlers: Vec<NestJsHandler> = Vec::new();
        // NestJS decorator-routes pending `has_nestjs` import gate.
        // (HTTP_METHOD, raw_path_with_quotes_stripped_by_capture, decorator_span)
        type NestJsDecoratorRoute = (String, String, (u32, u32, u32, u32));
        let mut pending_nestjs_decorator_routes: Vec<NestJsDecoratorRoute> = Vec::new();
        let mut blind_spots: Vec<BlindSpot> = Vec::new();
        let is_test_file = is_test_path(path.to_str().unwrap_or(""));

        let idx = &self.indices;

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut is_exported = false;
            let mut heritage = Vec::new();
            let mut type_annotation = None;
            let mut decorators = Vec::new();

            let mut import_name = None;
            let mut import_alias = None;
            let mut import_src = None;
            let mut is_import = false;
            let mut is_import_namespace = false;

            let mut route_method = None;
            let mut route_path = None;
            let mut route_handler_node: Option<tree_sitter::Node> = None;
            let mut is_route = false;

            let mut nestjs_class_node: Option<tree_sitter::Node> = None;
            let mut nestjs_method_node: Option<tree_sitter::Node> = None;
            let mut nestjs_decorator_verb_node: Option<tree_sitter::Node> = None;
            let mut nestjs_decorator_path_node: Option<tree_sitter::Node> = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);

                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap.index as usize)
                    .copied()
                    .flatten()
                {
                    // Single config-driven dispatch replaces the eight explicit
                    // Function/Class/Method/Interface/Typedef/Property/Const/Variable arms.
                    // Source of truth: TypeScriptSpec::CAPTURE_KIND in spec.rs.
                    name_node = Some(cap.node);
                    kind = Some(k_from_spec);
                } else if cap_idx == idx.export {
                    is_exported = true;
                } else if cap_idx == idx.heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.to_string());
                    }
                } else if cap_idx == idx.type_ann {
                    if let Ok(t) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        type_annotation = Some(t.to_string());
                    }
                } else if cap_idx == idx.decorator {
                    if let Ok(d) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d.to_string());
                    }
                } else if cap_idx == idx.import_name {
                    import_name = Some(cap.node);
                } else if cap_idx == idx.import_alias {
                    import_alias = Some(cap.node);
                } else if cap_idx == idx.import_source {
                    import_src = Some(cap.node);
                } else if cap_idx == idx.import {
                    is_import = true;
                } else if cap_idx == idx.import_namespace {
                    is_import_namespace = true;
                } else if cap_idx == idx.route_method {
                    route_method = Some(cap.node);
                } else if cap_idx == idx.route_path {
                    route_path = Some(cap.node);
                } else if cap_idx == idx.route_call {
                    is_route = true;
                    root_span_node = Some(cap.node);
                } else if cap_idx == idx.route_handler {
                    route_handler_node = Some(cap.node);
                } else if cap_idx == idx.express_handler {
                    if let Ok(handler_name) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        pending_express_handlers
                            .push((handler_name.to_string(), node_span(&cap.node)));
                    }
                } else if cap_idx == idx.nestjs_class {
                    nestjs_class_node = Some(cap.node);
                } else if cap_idx == idx.nestjs_method {
                    nestjs_method_node = Some(cap.node);
                } else if cap_idx == idx.nestjs_decorator_verb {
                    nestjs_decorator_verb_node = Some(cap.node);
                } else if cap_idx == idx.nestjs_decorator_path {
                    nestjs_decorator_path_node = Some(cap.node);
                } else if cap_idx == idx.blind_eval {
                    push_blind_spot(
                        &mut blind_spots,
                        BLIND_SPEC[0],
                        &cap.node,
                        path,
                        is_test_file,
                    );
                } else if cap_idx == idx.blind_function_ctor {
                    push_blind_spot(
                        &mut blind_spots,
                        BLIND_SPEC[1],
                        &cap.node,
                        path,
                        is_test_file,
                    );
                } else if cap_idx == idx.blind_dynamic_import {
                    if !js_ts_first_arg_is_literal_string(&cap.node) {
                        push_blind_spot(
                            &mut blind_spots,
                            BLIND_SPEC[2],
                            &cap.node,
                            path,
                            is_test_file,
                        );
                    }
                } else if cap_idx == idx.blind_dynamic_require {
                    if !js_ts_first_arg_is_literal_string(&cap.node) {
                        push_blind_spot(
                            &mut blind_spots,
                            BLIND_SPEC[3],
                            &cap.node,
                            path,
                            is_test_file,
                        );
                    }
                } else if cap_idx == idx.function
                    || cap_idx == idx.class
                    || cap_idx == idx.method
                    || cap_idx == idx.constructor
                    || cap_idx == idx.interface
                    || cap_idx == idx.property
                    || cap_idx == idx.const_kind
                    || cap_idx == idx.variable
                    || cap_idx == idx.typedef
                    || cap_idx == idx.enum_kind
                    || cap_idx == idx.enum_member_node
                {
                    root_span_node = Some(cap.node);
                }
            }

            // Process definitions
            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let span = node_span(&root);

                    let mut existing_found = false;
                    for node in &mut nodes {
                        if node.span == span && node.name == name_str {
                            // Function is more specific than Const/Variable: an
                            // arrow-function assigned to a const matches both the
                            // @function and @const patterns; the @const pattern fires
                            // first in tree-sitter's match order, so we upgrade the
                            // kind here when the later Function match arrives.
                            if k == NodeKind::Function
                                && matches!(node.kind, NodeKind::Const | NodeKind::Variable)
                            {
                                node.kind = NodeKind::Function;
                            }
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
                            if type_annotation.is_some() {
                                node.type_annotation = type_annotation.clone();
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
                        nodes.push(RawNode {
                            decorators: decorators.clone(),
                            name: name_str.to_string(),
                            kind: k,
                            span,
                            is_exported,
                            heritage: heritage.clone(),
                            type_annotation: type_annotation.clone(),
                            calls: Vec::new(),
                            owner_class: None,
                            content_hash: ecp_core::uid::xxh3_64_bytes(
                                &source[root.start_byte()..root.end_byte()],
                            ),
                        });
                    }
                }
            }

            // Process imports
            if is_import {
                if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
                    if let (Ok(name_str), Ok(src_str)) = (
                        std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]),
                        std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                    ) {
                        let alias_str = import_alias.and_then(|a| {
                            std::str::from_utf8(&source[a.start_byte()..a.end_byte()])
                                .ok()
                                .map(|s| s.to_string())
                        });

                        imports.push(RawImport {
                            alias: alias_str,
                            imported_name: name_str.to_string(),
                            source: src_str.to_string(),
                            binding_kind: None,
                        });
                    }
                }
            }

            // Process namespace re-export `export * as ns from 'lib'`.
            // No `name` identifier exists; emit with "*" sentinel as imported_name
            // and the local namespace binding as the alias.
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

            // Process NestJS @Controller method handler pairs.
            if let (Some(cls), Some(mth)) = (nestjs_class_node, nestjs_method_node) {
                if let (Ok(class_name), Ok(method_name)) = (
                    std::str::from_utf8(&source[cls.start_byte()..cls.end_byte()]),
                    std::str::from_utf8(&source[mth.start_byte()..mth.end_byte()]),
                ) {
                    pending_nestjs_handlers.push((
                        class_name.to_string(),
                        method_name.to_string(),
                        node_span(&mth),
                    ));
                }
            }

            // Process routes
            if is_route {
                if let (Some(r_method), Some(r_path), Some(root)) =
                    (route_method, route_path, root_span_node)
                {
                    if let (Ok(method_str), Ok(path_str)) = (
                        std::str::from_utf8(&source[r_method.start_byte()..r_method.end_byte()]),
                        std::str::from_utf8(&source[r_path.start_byte()..r_path.end_byte()]),
                    ) {
                        let handler_name = route_handler_node.and_then(|n| {
                            std::str::from_utf8(&source[n.start_byte()..n.end_byte()])
                                .ok()
                                .map(|s| s.to_string())
                        });
                        routes.push(RawRoute {
                            method: method_str.to_string(),
                            path: path_str.to_string(),
                            handler: handler_name,
                            span: node_span(&root),
                        });
                    }
                }
            }

            // NestJS decorator-route — `@Get('users')` / `@Post(':id')`.
            // Path string is captured separately from the generic Express
            // matcher, and the relaxed `clean_route_path_lax` accepts bare
            // names (NestJS convention) below in the final route cleanup.
            // Stash as a pending decorator-route; resolve once the
            // `has_nestjs` import flag is known (a few lines later in this
            // function — we can't gate here because import processing
            // happens in the same match loop).
            if let (Some(verb_node), Some(path_node)) =
                (nestjs_decorator_verb_node, nestjs_decorator_path_node)
            {
                if let (Ok(verb_str), Ok(path_str)) = (
                    std::str::from_utf8(&source[verb_node.start_byte()..verb_node.end_byte()]),
                    std::str::from_utf8(&source[path_node.start_byte()..path_node.end_byte()]),
                ) {
                    pending_nestjs_decorator_routes.push((
                        verb_str.to_uppercase(),
                        path_str.to_string(),
                        node_span(&verb_node),
                    ));
                }
            }
        }

        // Object.freeze({...}) and `{...} as const` enum-imitation detection.
        // Runs as a separate AST walk (not query-based) because the heuristic
        // requires counting pair children and checking value kinds — logic that
        // can't be expressed as a single tree-sitter predicate.
        collect_freeze_enum_spots(
            tree.root_node(),
            source,
            path,
            is_test_file,
            &mut blind_spots,
        );

        // Extract call sites with receiver-type binding:
        // - `this.method()` → resolved to `ClassName.method` via enclosing class
        // - `obj.method()` where `obj` has a type annotation → `Type.method`
        // - everything else falls back to the bare/qualified method name.
        let local_types = collect_local_types(tree.root_node(), source);
        let raw_path_literals =
            extract_ts_calls_and_path_literals(tree.root_node(), source, &mut nodes, &local_types);

        // Framework-presence gates: only emit Express/NestJS refs when the file
        // actually imports the matching package.
        const EXPRESS_REQUIRED: &[&str] = &["express"];
        const NESTJS_REQUIRED: &[&str] = &["@nestjs"];
        let has_express = has_import_from(&imports, EXPRESS_REQUIRED);
        let has_nestjs = has_import_from(&imports, NESTJS_REQUIRED);

        // Path-shape filter for generic Route emission — see
        // `docs/superpowers/specs/2026-05-17-route-precision-design.md`.
        // No framework-presence gate here: the parser only captures ES
        // `import` statements, so gating would regress Node.js code using
        // `require('express')`. The path-shape predicate alone removes
        // the dominant FP class (Map/headers/cache `.get("key")`).
        routes.retain_mut(|r| match crate::route_detector::clean_route_path(&r.path) {
            Some(clean) => {
                r.path = clean;
                true
            }
            None => false,
        });

        // NestJS decorator-routes — `@Get('users')` shape. Gated on
        // `@nestjs/*` import presence so a custom `@Get(...)` decorator in
        // an unrelated codebase does not surface a false Route. Uses the
        // lax path helper because NestJS conventionally elides the leading
        // `/` (the framework prepends the controller prefix at runtime).
        if has_nestjs {
            for (verb, raw_path, span) in pending_nestjs_decorator_routes {
                if let Some(cleaned) = crate::route_detector::clean_route_path_lax(&raw_path) {
                    routes.push(RawRoute {
                        method: verb,
                        path: cleaned,
                        handler: None,
                        span,
                    });
                }
            }
        }

        // Resolve framework-ref enclosing functions via span containment.
        // Module-level captures use the MODULE_LEVEL_SOURCE sentinel (consistent with Actix).
        let mut framework_refs: Vec<RawFrameworkRef> = if has_express {
            pending_express_handlers
                .into_iter()
                .map(|(target, cap_span)| {
                    let source_name = enclosing_function_name(&nodes, cap_span)
                        .unwrap_or_else(|| MODULE_LEVEL_SOURCE.to_string());
                    RawFrameworkRef {
                        source_name,
                        target_name: target,
                        confidence: framework_confidence::EXPRESS_ROUTE,
                        reason: "express-route-handler".to_string(),
                        span: cap_span,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };

        // NestJS @Controller → @Get/@Post/... method bindings.
        if has_nestjs {
            for (class_name, method_name, span) in pending_nestjs_handlers {
                framework_refs.push(RawFrameworkRef {
                    source_name: class_name,
                    target_name: method_name,
                    confidence: framework_confidence::NESTJS_ROUTE,
                    reason: "nestjs-route-handler".to_string(),
                    span,
                });
            }
        }

        let param_names = collect_js_param_names(tree.root_node(), source);
        let call_metas = detect_js_ts_indirect(tree.root_node(), source, &nodes, &param_names);
        let file_category = crate::resolution::builder::determine_category(&path.to_string_lossy());
        let raw_function_metas = crate::function_meta::typescript::extract(
            tree.root_node(),
            source,
            &nodes,
            file_category,
        );

        crate::framework_helpers::stamp_owner_class_by_span(&mut nodes);
        crate::framework_helpers::stamp_owner_fn_by_span(&mut nodes);

        let tx_scopes =
            collect_typeorm_transactional_scopes(&nodes, &[NodeKind::Method, NodeKind::Function]);

        // T4-7 refactor: `RawSchemaField` now stores owned `Box<str>` so the
        // per-file parser scope can drop cleanly without dangling-pool risk.
        let fields = crate::schema_field::extract_schema_fields(
            &tree,
            source,
            &self.query,
            &[crate::typescript::schema_extractors::TS_INTERFACE_CONFIG],
            &imports,
        );
        let schema_fields = (!fields.is_empty()).then(|| fields.into_boxed_slice());

        let event_topics = {
            let topics = crate::event_topic::extract_event_topics(
                &tree,
                source,
                &self.query,
                &[
                    crate::event_topic::REDIS_TS,
                    crate::event_topic::KAFKA_NODE,
                    crate::event_topic::RABBITMQ_TS,
                    crate::event_topic::SQS_TS,
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
            schema_fields,
            event_topics,
            tx_scopes,
            path_literals,
            call_metas,
            raw_function_metas,
        })
    }
}
