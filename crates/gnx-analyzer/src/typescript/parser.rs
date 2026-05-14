use crate::calls::extract_calls;
use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawImport, RawNode, RawRoute};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct TypeScriptProvider {
    query: Query,
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
        Ok(Self { query })
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

        // Capture indices
        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_class_name = self.query.capture_index_for_name("class.name");
        let idx_method_name = self.query.capture_index_for_name("method.name");
        let idx_interface_name = self.query.capture_index_for_name("interface.name");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_interface = self.query.capture_index_for_name("interface");

        let idx_export = self.query.capture_index_for_name("export");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");
        let idx_decorator = self.query.capture_index_for_name("decorator");

        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import = self.query.capture_index_for_name("import");

        let idx_route_method = self.query.capture_index_for_name("route.method");
        let idx_route_path = self.query.capture_index_for_name("route.path");
        let idx_route_call = self.query.capture_index_for_name("route.call");

        let idx_express_handler = self.query.capture_index_for_name("express.route.handler");

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
            let mut is_route = false;

            for cap in m.captures {
                let cap_idx = Some(cap.index);

                if cap_idx == idx_function_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if cap_idx == idx_class_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if cap_idx == idx_method_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
                } else if cap_idx == idx_interface_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Interface);
                } else if cap_idx == idx_export {
                    is_exported = true;
                } else if cap_idx == idx_heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.to_string());
                    }
                } else if cap_idx == idx_type {
                    if let Ok(t) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        type_annotation = Some(t.to_string());
                    }
                } else if cap_idx == idx_decorator {
                    if let Ok(d) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d.to_string());
                    }
                } else if cap_idx == idx_import_name {
                    import_name = Some(cap.node);
                } else if cap_idx == idx_import_alias {
                    import_alias = Some(cap.node);
                } else if cap_idx == idx_import_source {
                    import_src = Some(cap.node);
                } else if cap_idx == idx_import {
                    is_import = true;
                } else if cap_idx == idx_route_method {
                    route_method = Some(cap.node);
                } else if cap_idx == idx_route_path {
                    route_path = Some(cap.node);
                } else if cap_idx == idx_route_call {
                    is_route = true;
                    root_span_node = Some(cap.node);
                } else if cap_idx == idx_express_handler {
                    if let Ok(handler_name) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        let start = cap.node.start_position();
                        let end = cap.node.end_position();
                        pending_express_handlers.push((
                            handler_name.to_string(),
                            (
                                start.row as u32,
                                start.column as u32,
                                end.row as u32,
                                end.column as u32,
                            ),
                        ));
                    }
                } else if cap_idx == idx_function
                    || cap_idx == idx_class
                    || cap_idx == idx_method
                    || cap_idx == idx_interface
                {
                    root_span_node = Some(cap.node);
                }
            }

            // Process definitions
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
                            span: node_span,
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

            // Process routes
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

        // Extract call sites and attach to enclosing function/method nodes.
        extract_calls(tree.root_node(), source, &mut nodes, &["call_expression"]);

        // Resolve framework-ref enclosing functions via span containment.
        // If the capture is at module-level, source_name is left empty.
        let framework_refs: Vec<RawFrameworkRef> = pending_express_handlers
            .into_iter()
            .map(|(target, cap_span)| {
                let source_name = enclosing_function_name(&nodes, cap_span)
                    .map(str::to_string)
                    .unwrap_or_default();
                RawFrameworkRef {
                    source_name,
                    target_name: target,
                    confidence: 0.8,
                    reason: "express-route-handler".to_string(),
                    span: cap_span,
                }
            })
            .collect();

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes,
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs,
        })
    }
}

/// Return the name of the smallest function/method `RawNode` whose span fully
/// contains `inner`. Returns `None` if `inner` is module-level.
fn enclosing_function_name(
    nodes: &[RawNode],
    inner: (u32, u32, u32, u32),
) -> Option<&str> {
    let mut best: Option<&RawNode> = None;
    for n in nodes {
        if !matches!(n.kind, NodeKind::Function | NodeKind::Method) {
            continue;
        }
        if span_contains(n.span, inner) {
            best = match best {
                None => Some(n),
                Some(b) if span_contains(b.span, n.span) => Some(n),
                Some(b) => Some(b),
            };
        }
    }
    best.map(|n| n.name.as_str())
}

/// `outer` fully contains `inner` (inclusive on the outer bounds).
fn span_contains(outer: (u32, u32, u32, u32), inner: (u32, u32, u32, u32)) -> bool {
    let (o_sr, o_sc, o_er, o_ec) = outer;
    let (i_sr, i_sc, i_er, i_ec) = inner;
    let starts_before_or_at = (o_sr < i_sr) || (o_sr == i_sr && o_sc <= i_sc);
    let ends_after_or_at = (o_er > i_er) || (o_er == i_er && o_ec >= i_ec);
    starts_before_or_at && ends_after_or_at
}
