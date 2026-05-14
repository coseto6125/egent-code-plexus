use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct GoProvider {
    query: Query,
}

impl GoProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_go::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for GoProvider {
    fn name(&self) -> &'static str {
        "go"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let language = tree_sitter_go::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse go file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        let idx_struct_name = self.query.capture_index_for_name("struct.name");
        let idx_interface_name = self.query.capture_index_for_name("interface.name");
        let idx_method_name = self.query.capture_index_for_name("method.name");
        let idx_function_name = self.query.capture_index_for_name("function.name");

        let idx_struct = self.query.capture_index_for_name("struct");
        let idx_interface = self.query.capture_index_for_name("interface");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_function = self.query.capture_index_for_name("function");

        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");

        let idx_import = self.query.capture_index_for_name("import");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        let idx_route_call = self.query.capture_index_for_name("route.call");
        let idx_route_method = self.query.capture_index_for_name("route.method");
        let idx_route_path = self.query.capture_index_for_name("route.path");

        let mut routes = Vec::new();

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_node = None;
            let mut heritage = Vec::new();
            let mut type_annotation: Option<String> = None;

            let mut is_import = false;
            let mut import_alias = None;
            let mut import_source = None;

            let mut is_route = false;
            let mut route_method_node = None;
            let mut route_path_node = None;
            let mut route_span_node = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if cap_idx == idx_struct_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if cap_idx == idx_interface_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Interface);
                } else if cap_idx == idx_method_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
                } else if cap_idx == idx_function_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if cap_idx == idx_struct || cap_idx == idx_interface || cap_idx == idx_method || cap_idx == idx_function {
                    root_node = Some(cap.node);
                } else if cap_idx == idx_heritage {
                    if let Ok(h_name) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
                        heritage.push(h_name.to_string());
                    }
                } else if cap_idx == idx_type {
                    if let Ok(t_name) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
                        if let Some(ref mut existing) = type_annotation {
                            existing.push_str(" ");
                            existing.push_str(t_name);
                        } else {
                            type_annotation = Some(t_name.to_string());
                        }
                    }
                } else if cap_idx == idx_import {
                    is_import = true;
                } else if cap_idx == idx_import_alias {
                    import_alias = Some(cap.node);
                } else if cap_idx == idx_import_source {
                    import_source = Some(cap.node);
                } else if cap_idx == idx_route_call {
                    is_route = true;
                    route_span_node = Some(cap.node);
                } else if cap_idx == idx_route_method {
                    route_method_node = Some(cap.node);
                } else if cap_idx == idx_route_path {
                    route_path_node = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let name = name_str.to_string();
                    let is_exported = name.chars().next().map_or(false, |c| c.is_uppercase());
                    let start = root.start_position();
                    let end = root.end_position();

                    nodes.push(RawNode {
            decorators: vec![],
                        name,
                        kind: k,
                        is_exported,
                        heritage,
                        type_annotation,
                        span: (
                            start.row as u32,
                            start.column as u32,
                            end.row as u32,
                            end.column as u32,
                        ),
                    });
                }
            }

            if is_route {
                if let (Some(m_node), Some(p_node), Some(span_node)) = (route_method_node, route_path_node, route_span_node) {
                    let method_str = std::str::from_utf8(&source[m_node.start_byte()..m_node.end_byte()]).unwrap_or("").to_string();
                    let path_raw = std::str::from_utf8(&source[p_node.start_byte()..p_node.end_byte()]).unwrap_or("");
                    let path_str = path_raw.trim_matches(|c| c == '"' || c == '`').to_string();
                    let start = span_node.start_position();
                    let end = span_node.end_position();

                    routes.push(gnx_core::analyzer::types::RawRoute {
                        method: method_str,
                        path: path_str,
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

            if is_import {
                if let Some(src_node) = import_source {
                    if let Ok(src_quoted) = std::str::from_utf8(&source[src_node.start_byte()..src_node.end_byte()]) {
                        let source_path = src_quoted.trim_matches(|c| c == '"' || c == '`').to_string();
                        
                        let alias = if let Some(alias_node) = import_alias {
                            std::str::from_utf8(&source[alias_node.start_byte()..alias_node.end_byte()]).ok().map(|s| s.to_string())
                        } else {
                            None
                        };

                        let imported_name = if let Some(ref a) = alias {
                            a.clone()
                        } else if let Some(last_part) = source_path.split('/').last() {
                            last_part.to_string()
                        } else {
                            source_path.clone()
                        };

                        imports.push(RawImport {
                            source: source_path,
                            alias,
                            imported_name,
                        });
                    }
                }
            }
        }

        // Deduplicate imports
        imports.sort_by(|a, b| a.source.cmp(&b.source).then(a.imported_name.cmp(&b.imported_name)));
        imports.dedup_by(|a, b| a.source == b.source && a.imported_name == b.imported_name);

        Ok(LocalGraph {
            routes,
            file_path: path.to_path_buf(),
            nodes,
            imports,
        })
    }
}
