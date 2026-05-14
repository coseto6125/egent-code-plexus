use crate::calls::extract_calls;
use crate::framework_helpers::{MODULE_LEVEL_SOURCE, enclosing_function_name, node_span};
use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawImport, RawNode, RawRoute};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct PythonProvider {
    query: Query,
    indices: PythonCaptureIndices,
}

struct PythonCaptureIndices {
    function_name: Option<u32>,
    class_name: Option<u32>,
    type_ann: Option<u32>,
    heritage: Option<u32>,
    export: Option<u32>,
    import_name: Option<u32>,
    import_source: Option<u32>,
    import_alias: Option<u32>,
    decorator: Option<u32>,
    function: Option<u32>,
    class: Option<u32>,
    route_method: Option<u32>,
    route_path: Option<u32>,
    route_call: Option<u32>,
    fastapi_depends_target: Option<u32>,
    fastapi_route_app: Option<u32>,
    fastapi_route_method: Option<u32>,
    fastapi_route_handler: Option<u32>,
    django_url_handler: Option<u32>,
    celery_task_handler: Option<u32>,
}

impl PythonProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_python::LANGUAGE.into();
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        let query = Query::new(&language, &query_source)?;
        let indices = PythonCaptureIndices {
            function_name: query.capture_index_for_name("function.name"),
            class_name: query.capture_index_for_name("class.name"),
            type_ann: query.capture_index_for_name("type"),
            heritage: query.capture_index_for_name("heritage"),
            export: query.capture_index_for_name("export"),
            import_name: query.capture_index_for_name("import.name"),
            import_source: query.capture_index_for_name("import.source"),
            import_alias: query.capture_index_for_name("import.alias"),
            decorator: query.capture_index_for_name("decorator"),
            function: query.capture_index_for_name("function"),
            class: query.capture_index_for_name("class"),
            route_method: query.capture_index_for_name("route.method"),
            route_path: query.capture_index_for_name("route.path"),
            route_call: query.capture_index_for_name("route.call"),
            fastapi_depends_target: query.capture_index_for_name("fastapi.depends.target"),
            fastapi_route_app: query.capture_index_for_name("fastapi.route.app"),
            fastapi_route_method: query.capture_index_for_name("fastapi.route.method"),
            fastapi_route_handler: query.capture_index_for_name("fastapi.route.handler"),
            django_url_handler: query.capture_index_for_name("django.url.handler"),
            celery_task_handler: query.capture_index_for_name("celery.task.handler"),
        };
        Ok(Self { query, indices })
    }
}

impl LanguageProvider for PythonProvider {
    fn name(&self) -> &'static str {
        "python"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let language = tree_sitter_python::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse python file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();
        let mut routes: Vec<RawRoute> = Vec::new();

        let idx = &self.indices;

        // Collect (target_name, span) for FastAPI Depends() refs; resolve
        // the enclosing function via span containment after nodes are built.
        let mut pending_depends: Vec<(String, (u32, u32, u32, u32))> = Vec::new();

        // Directly emitted route decorator refs (no span resolution needed).
        let mut route_refs: Vec<RawFrameworkRef> = Vec::new();

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut type_annotation_node = None;
            let mut heritage = Vec::new();
            let mut is_exported_explicit = false;
            let mut decorators = Vec::new();

            let mut import_name_node = None;
            let mut import_src_node = None;
            let mut import_alias_node = None;

            let mut route_method = None;
            let mut route_path = None;
            let mut is_route = false;

            let mut fa_route_app_node = None;
            let mut fa_route_method_node = None;
            let mut fa_route_handler_node = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if cap_idx == idx.function_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if cap_idx == idx.class_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if cap_idx == idx.type_ann {
                    type_annotation_node = Some(cap.node);
                } else if cap_idx == idx.heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.to_string());
                    }
                } else if cap_idx == idx.export {
                    is_exported_explicit = true;
                } else if cap_idx == idx.decorator {
                    if let Ok(d_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d_str.to_string());
                    }
                } else if cap_idx == idx.import_name {
                    import_name_node = Some(cap.node);
                } else if cap_idx == idx.import_source {
                    import_src_node = Some(cap.node);
                } else if cap_idx == idx.import_alias {
                    import_alias_node = Some(cap.node);
                } else if cap_idx == idx.route_method {
                    route_method = Some(cap.node);
                } else if cap_idx == idx.route_path {
                    route_path = Some(cap.node);
                } else if cap_idx == idx.route_call {
                    is_route = true;
                    root_span_node = Some(cap.node);
                } else if cap_idx == idx.function || cap_idx == idx.class {
                    root_span_node = Some(cap.node);
                } else if cap_idx == idx.fastapi_depends_target {
                    if let Ok(target_name) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        pending_depends.push((target_name.to_string(), node_span(&cap.node)));
                    }
                } else if cap_idx == idx.fastapi_route_app {
                    fa_route_app_node = Some(cap.node);
                } else if cap_idx == idx.fastapi_route_method {
                    fa_route_method_node = Some(cap.node);
                } else if cap_idx == idx.fastapi_route_handler {
                    fa_route_handler_node = Some(cap.node);
                } else if cap_idx == idx.django_url_handler {
                    if let Ok(target_name) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        route_refs.push(RawFrameworkRef {
                            source_name: MODULE_LEVEL_SOURCE.to_string(),
                            target_name: target_name.to_string(),
                            confidence: 0.9,
                            reason: "django-url-path".to_string(),
                            span: node_span(&cap.node),
                        });
                    }
                } else if cap_idx == idx.celery_task_handler {
                    if let Ok(target_name) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        route_refs.push(RawFrameworkRef {
                            source_name: MODULE_LEVEL_SOURCE.to_string(),
                            target_name: target_name.to_string(),
                            confidence: 0.9,
                            reason: "celery-task".to_string(),
                            span: node_span(&cap.node),
                        });
                    }
                }
            }

            if let (Some(app_n), Some(method_n), Some(handler_n)) =
                (fa_route_app_node, fa_route_method_node, fa_route_handler_node)
            {
                if let (Ok(app_str), Ok(method_str), Ok(handler_str)) = (
                    std::str::from_utf8(&source[app_n.start_byte()..app_n.end_byte()]),
                    std::str::from_utf8(&source[method_n.start_byte()..method_n.end_byte()]),
                    std::str::from_utf8(&source[handler_n.start_byte()..handler_n.end_byte()]),
                ) {
                    route_refs.push(RawFrameworkRef {
                        source_name: app_str.to_string(),
                        target_name: handler_str.to_string(),
                        confidence: 0.9,
                        reason: format!("fastapi-route-{}", method_str),
                        span: node_span(&handler_n),
                    });
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let span = node_span(&root);

                    let type_str = type_annotation_node.and_then(|t| {
                        std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                            .ok()
                            .map(|s| s.to_string())
                    });

                    if let Some(existing) = nodes.iter_mut().find(|node| node.span == span) {
                        for h in heritage {
                            if !existing.heritage.contains(&h) {
                                existing.heritage.push(h);
                            }
                        }
                        if existing.type_annotation.is_none() && type_str.is_some() {
                            existing.type_annotation = type_str;
                        }
                        if !decorators.is_empty() {
                            for d in &decorators {
                                if !existing.decorators.contains(d) {
                                    existing.decorators.push(d.clone());
                                }
                            }
                        }
                    } else {
                        nodes.push(RawNode {
                            decorators: decorators.clone(),
                            is_exported: is_exported_explicit || !name_str.starts_with('_'),
                            heritage,
                            type_annotation: type_str,
                            name: name_str.to_string(),
                            kind: k,
                            span,
                            calls: Vec::new(),
                        });
                    }
                }
            }

            if let Some(i_name) = import_name_node {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()])
                {
                    let src_str = if let Some(i_src) = import_src_node {
                        std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
                            .unwrap_or("")
                            .to_string()
                    } else {
                        "".to_string()
                    };

                    let alias = import_alias_node.and_then(|a| {
                        std::str::from_utf8(&source[a.start_byte()..a.end_byte()])
                            .ok()
                            .map(|s| s.to_string())
                    });

                    imports.push(RawImport {
                        alias,
                        imported_name: name_str.to_string(),
                        source: src_str,
                    });
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
                        routes.push(RawRoute {
                            method: method_str.to_string(),
                            path: path_str.to_string(),
                            handler: None,
                            span: node_span(&root),
                        });
                    }
                }
            }
        }

        // Extract call sites and attach to enclosing function/method nodes.
        extract_calls(tree.root_node(), source, &mut nodes, &["call"]);

        // Resolve FastAPI Depends() refs: find the innermost enclosing
        // Function/Method node whose span contains the capture span.
        let mut framework_refs: Vec<RawFrameworkRef> = route_refs;
        for (target_name, span) in pending_depends {
            if let Some(source_name) = enclosing_function_name(&nodes, span) {
                framework_refs.push(RawFrameworkRef {
                    source_name,
                    target_name,
                    confidence: 0.6,
                    reason: "fastapi-depends".to_string(),
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
        })
    }
}
