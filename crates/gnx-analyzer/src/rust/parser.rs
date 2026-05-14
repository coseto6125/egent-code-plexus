use crate::calls::extract_calls;
use crate::framework_helpers::{node_span, MODULE_LEVEL_SOURCE};
use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawImport, RawNode};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct RustProvider {
    query: Query,
    indices: RustCaptureIndices,
}

struct RustCaptureIndices {
    name_struct: Option<u32>,
    name_enum: Option<u32>,
    name_trait: Option<u32>,
    name_function: Option<u32>,
    import_name: Option<u32>,
    import_source: Option<u32>,
    import_alias: Option<u32>,
    function: Option<u32>,
    class: Option<u32>,
    method: Option<u32>,
    interface: Option<u32>,
    export: Option<u32>,
    heritage: Option<u32>,
    type_ann: Option<u32>,
    decorator: Option<u32>,
    axum_handler: Option<u32>,
    actix_method: Option<u32>,
    actix_handler: Option<u32>,
}

impl RustProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_rust::LANGUAGE.into();
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        let query = Query::new(&language, &query_source)?;
        let indices = RustCaptureIndices {
            name_struct: query.capture_index_for_name("struct_item.name"),
            name_enum: query.capture_index_for_name("enum_item.name"),
            name_trait: query.capture_index_for_name("trait_item.name"),
            name_function: query.capture_index_for_name("function_item.name"),
            import_name: query.capture_index_for_name("import.name"),
            import_source: query.capture_index_for_name("import.source"),
            import_alias: query.capture_index_for_name("import.alias"),
            function: query.capture_index_for_name("function"),
            class: query.capture_index_for_name("class"),
            method: query.capture_index_for_name("method"),
            interface: query.capture_index_for_name("interface"),
            export: query.capture_index_for_name("export"),
            heritage: query.capture_index_for_name("heritage"),
            type_ann: query.capture_index_for_name("type"),
            decorator: query.capture_index_for_name("decorator"),
            axum_handler: query.capture_index_for_name("axum.route.handler"),
            actix_method: query.capture_index_for_name("actix.route.method"),
            actix_handler: query.capture_index_for_name("actix.route.handler"),
        };
        Ok(Self { query, indices })
    }
}

impl LanguageProvider for RustProvider {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let language = tree_sitter_rust::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse rust file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        let idx = &self.indices;

        // Side-table: top-level free `fn` byte ranges + names, used to resolve
        // the enclosing function for framework-ref captures via byte-range containment.
        let mut fn_spans: Vec<(String, std::ops::Range<usize>)> = Vec::new();
        // (handler_ident, capture_start_byte, capture_end_byte, span)
        type AxumCapture = (String, usize, usize, (u32, u32, u32, u32));
        // (method, handler_ident, span)
        type ActixCapture = (String, String, (u32, u32, u32, u32));
        let mut axum_handler_captures: Vec<AxumCapture> = Vec::new();
        let mut actix_handler_captures: Vec<ActixCapture> = Vec::new();

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut is_exported = false;
            let mut heritage = Vec::new();
            let mut type_annotation = None;
            let mut decorators = Vec::new();

            let mut import_name = None;
            let mut import_src = None;
            let mut import_alias = None;

            // Per-match Actix capture pair (one match = one fn + its attribute).
            let mut actix_method: Option<tree_sitter::Node> = None;
            let mut actix_handler: Option<tree_sitter::Node> = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if cap_idx == idx.name_struct || cap_idx == idx.name_enum {
                    // struct 與 enum 在 gnx NodeKind 統一映射為 Class。
                    name_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Class);
                    }
                } else if cap_idx == idx.name_trait {
                    name_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Interface);
                    }
                } else if cap_idx == idx.name_function {
                    name_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Function);
                    }
                } else if cap_idx == idx.import_name {
                    import_name = Some(cap.node);
                } else if cap_idx == idx.import_source {
                    import_src = Some(cap.node);
                } else if cap_idx == idx.import_alias {
                    import_alias = Some(cap.node);
                } else if cap_idx == idx.function {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if cap_idx == idx.class {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if cap_idx == idx.method {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
                } else if cap_idx == idx.interface {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Interface);
                } else if cap_idx == idx.export {
                    is_exported = true;
                } else if cap_idx == idx.heritage {
                    if let Ok(h_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h_str.to_string());
                    }
                } else if cap_idx == idx.type_ann {
                    if let Ok(t_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        type_annotation = Some(t_str.to_string());
                    }
                } else if cap_idx == idx.decorator {
                    if let Ok(d_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d_str.to_string());
                    }
                } else if cap_idx == idx.axum_handler {
                    if let Ok(h_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        axum_handler_captures.push((
                            h_str.to_string(),
                            cap.node.start_byte(),
                            cap.node.end_byte(),
                            node_span(&cap.node),
                        ));
                    }
                } else if cap_idx == idx.actix_method {
                    actix_method = Some(cap.node);
                } else if cap_idx == idx.actix_handler {
                    actix_handler = Some(cap.node);
                }
            }

            if let (Some(method_node), Some(handler_node)) = (actix_method, actix_handler) {
                if let (Ok(method_str), Ok(handler_str)) = (
                    std::str::from_utf8(&source[method_node.start_byte()..method_node.end_byte()]),
                    std::str::from_utf8(
                        &source[handler_node.start_byte()..handler_node.end_byte()],
                    ),
                ) {
                    actix_handler_captures.push((
                        method_str.to_string(),
                        handler_str.to_string(),
                        node_span(&handler_node),
                    ));
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    if matches!(k, NodeKind::Function | NodeKind::Method) {
                        fn_spans.push((name_str.to_string(), root.start_byte()..root.end_byte()));
                    }
                    nodes.push(RawNode {
                        decorators,
                        is_exported,
                        heritage,
                        type_annotation,
                        name: name_str.to_string(),
                        kind: k,
                        span: node_span(&root),
                        calls: Vec::new(),
                    });
                }
            }

            if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
                if let (Ok(name_str), Ok(src_str)) = (
                    std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]),
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    let alias = if let Some(a_node) = import_alias {
                        std::str::from_utf8(&source[a_node.start_byte()..a_node.end_byte()])
                            .ok()
                            .map(|s| s.to_string())
                    } else {
                        None
                    };

                    imports.push(RawImport {
                        alias,
                        imported_name: name_str.to_string(),
                        source: src_str.to_string(),
                    });
                }
            }
        }

        // Extract call sites and attach to enclosing function/method nodes.
        extract_calls(
            tree.root_node(),
            source,
            &mut nodes,
            &["call_expression", "macro_invocation"],
        );

        // Resolve enclosing function for each framework-ref capture via byte-range
        // containment; pick the innermost (smallest) function that contains the capture.
        let mut framework_refs = Vec::with_capacity(axum_handler_captures.len());
        for (handler_name, cap_start, cap_end, span) in axum_handler_captures {
            let enclosing = fn_spans
                .iter()
                .filter(|(_, range)| range.start <= cap_start && cap_end <= range.end)
                .min_by_key(|(_, range)| range.end - range.start);
            if let Some((fn_name, _)) = enclosing {
                framework_refs.push(RawFrameworkRef {
                    source_name: fn_name.clone(),
                    target_name: handler_name,
                    confidence: 0.8,
                    reason: "axum-route-handler".to_string(),
                    span,
                });
            }
        }

        // Actix attribute-macro routes: emit one ref per #[verb] → fn pair.
        // No natural source — use module-level sentinel; confidence 0.9 (syntactic, unambiguous).
        for (method, handler, span) in actix_handler_captures {
            framework_refs.push(RawFrameworkRef {
                source_name: MODULE_LEVEL_SOURCE.to_string(),
                target_name: handler,
                confidence: 0.9,
                reason: format!("actix-route-{}", method),
                span,
            });
        }

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes: vec![],
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
