use crate::framework_confidence;
use crate::framework_helpers::{
    enclosing_function_name, has_import_from, node_span, MODULE_LEVEL_SOURCE,
};
use super::receiver_types::{collect_local_types, extract_ts_calls};
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{
    LocalGraph, RawFrameworkRef, RawImport, RawNode, RawRoute,
};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct TypeScriptProvider {
    query: Query,
    indices: TypeScriptCaptureIndices,
}

struct TypeScriptCaptureIndices {
    function_name: Option<u32>,
    class_name: Option<u32>,
    method_name: Option<u32>,
    interface_name: Option<u32>,
    function: Option<u32>,
    class: Option<u32>,
    method: Option<u32>,
    interface: Option<u32>,
    export: Option<u32>,
    heritage: Option<u32>,
    type_ann: Option<u32>,
    decorator: Option<u32>,
    import_name: Option<u32>,
    import_alias: Option<u32>,
    import_source: Option<u32>,
    import: Option<u32>,
    route_method: Option<u32>,
    route_path: Option<u32>,
    route_call: Option<u32>,
    /// Named handler argument of `app.METHOD(path, handler)` — used by the
    /// builder to materialize a `HandlesRoute` edge from the handler back
    /// to the Route node. Absent for inline arrow / anonymous handlers.
    route_handler: Option<u32>,
    express_handler: Option<u32>,
    nestjs_class: Option<u32>,
    nestjs_method: Option<u32>,
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
            function_name: query.capture_index_for_name("function.name"),
            class_name: query.capture_index_for_name("class.name"),
            method_name: query.capture_index_for_name("method.name"),
            interface_name: query.capture_index_for_name("interface.name"),
            function: query.capture_index_for_name("function"),
            class: query.capture_index_for_name("class"),
            method: query.capture_index_for_name("method"),
            interface: query.capture_index_for_name("interface"),
            export: query.capture_index_for_name("export"),
            heritage: query.capture_index_for_name("heritage"),
            type_ann: query.capture_index_for_name("type"),
            decorator: query.capture_index_for_name("decorator"),
            import_name: query.capture_index_for_name("import.name"),
            import_alias: query.capture_index_for_name("import.alias"),
            import_source: query.capture_index_for_name("import.source"),
            import: query.capture_index_for_name("import"),
            route_method: query.capture_index_for_name("route.method"),
            route_path: query.capture_index_for_name("route.path"),
            route_call: query.capture_index_for_name("route.call"),
            route_handler: query.capture_index_for_name("route.handler"),
            express_handler: query.capture_index_for_name("express.route.handler"),
            nestjs_class: query.capture_index_for_name("nestjs.controller.class"),
            nestjs_method: query.capture_index_for_name("nestjs.method.name"),
        };
        Ok(Self { query, indices })
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

        let tree = parser
            .parse(source, None)
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

            let mut route_method = None;
            let mut route_path = None;
            let mut route_handler_node: Option<tree_sitter::Node> = None;
            let mut is_route = false;

            let mut nestjs_class_node: Option<tree_sitter::Node> = None;
            let mut nestjs_method_node: Option<tree_sitter::Node> = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);

                if cap_idx == idx.function_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if cap_idx == idx.class_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if cap_idx == idx.method_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
                } else if cap_idx == idx.interface_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Interface);
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
                } else if cap_idx == idx.function
                    || cap_idx == idx.class
                    || cap_idx == idx.method
                    || cap_idx == idx.interface
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
        }

        // Extract call sites with receiver-type binding:
        // - `this.method()` → resolved to `ClassName.method` via enclosing class
        // - `obj.method()` where `obj` has a type annotation → `Type.method`
        // - everything else falls back to the bare/qualified method name.
        let local_types = collect_local_types(tree.root_node(), source);
        extract_ts_calls(tree.root_node(), source, &mut nodes, &local_types);

        // Framework-presence gates: only emit Express/NestJS refs when the file
        // actually imports the matching package.
        const EXPRESS_REQUIRED: &[&str] = &["express"];
        const NESTJS_REQUIRED: &[&str] = &["@nestjs"];
        let has_express = has_import_from(&imports, EXPRESS_REQUIRED);
        let has_nestjs = has_import_from(&imports, NESTJS_REQUIRED);

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
