use crate::calls::extract_calls;
use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawImport, RawNode, RawRoute};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct PythonProvider {
    query: Query,
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
        Ok(Self { query })
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

        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_class_name = self.query.capture_index_for_name("class.name");
        let idx_type = self.query.capture_index_for_name("type");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_export = self.query.capture_index_for_name("export");
        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");
        let idx_decorator = self.query.capture_index_for_name("decorator");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");

        let idx_route_method = self.query.capture_index_for_name("route.method");
        let idx_route_path = self.query.capture_index_for_name("route.path");
        let idx_route_call = self.query.capture_index_for_name("route.call");

        let idx_fastapi_depends_target =
            self.query.capture_index_for_name("fastapi.depends.target");

        // Collect (target_name, span) for FastAPI Depends() refs; resolve
        // the enclosing function via span containment after nodes are built.
        let mut pending_depends: Vec<(String, (u32, u32, u32, u32))> = Vec::new();

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

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if cap_idx == idx_function_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if cap_idx == idx_class_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if cap_idx == idx_type {
                    type_annotation_node = Some(cap.node);
                } else if cap_idx == idx_heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.to_string());
                    }
                } else if cap_idx == idx_export {
                    is_exported_explicit = true;
                } else if cap_idx == idx_decorator {
                    if let Ok(d_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d_str.to_string());
                    }
                } else if cap_idx == idx_import_name {
                    import_name_node = Some(cap.node);
                } else if cap_idx == idx_import_source {
                    import_src_node = Some(cap.node);
                } else if cap_idx == idx_import_alias {
                    import_alias_node = Some(cap.node);
                } else if cap_idx == idx_route_method {
                    route_method = Some(cap.node);
                } else if cap_idx == idx_route_path {
                    route_path = Some(cap.node);
                } else if cap_idx == idx_route_call {
                    is_route = true;
                    root_span_node = Some(cap.node);
                } else if cap_idx == idx_function || cap_idx == idx_class {
                    root_span_node = Some(cap.node);
                } else if cap_idx == idx_fastapi_depends_target {
                    if let Ok(target_name) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        let start = cap.node.start_position();
                        let end = cap.node.end_position();
                        pending_depends.push((
                            target_name.to_string(),
                            (
                                start.row as u32,
                                start.column as u32,
                                end.row as u32,
                                end.column as u32,
                            ),
                        ));
                    }
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    let span = (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    );

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
        extract_calls(tree.root_node(), source, &mut nodes, &["call"]);

        // Resolve FastAPI Depends() refs: find the innermost enclosing
        // Function/Method node whose span contains the capture span.
        let mut framework_refs: Vec<RawFrameworkRef> = Vec::new();
        for (target_name, span) in pending_depends {
            let enclosing = nodes
                .iter()
                .filter(|n| {
                    matches!(n.kind, NodeKind::Function | NodeKind::Method)
                        && span_contains(n.span, span)
                })
                .min_by_key(|n| span_area(n.span));
            if let Some(source_node) = enclosing {
                framework_refs.push(RawFrameworkRef {
                    source_name: source_node.name.clone(),
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

/// Returns true if `outer` fully contains `inner` (inclusive of equal bounds).
fn span_contains(outer: (u32, u32, u32, u32), inner: (u32, u32, u32, u32)) -> bool {
    let (o_sr, o_sc, o_er, o_ec) = outer;
    let (i_sr, i_sc, i_er, i_ec) = inner;
    let start_ok = (o_sr, o_sc) <= (i_sr, i_sc);
    let end_ok = (i_er, i_ec) <= (o_er, o_ec);
    start_ok && end_ok
}

/// Rough size proxy used to pick the innermost containing span.
/// Compares row span first, then column span — small wins.
fn span_area(span: (u32, u32, u32, u32)) -> (u32, u32) {
    let (sr, sc, er, ec) = span;
    (er.saturating_sub(sr), ec.saturating_sub(sc))
}
