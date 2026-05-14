use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

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

        use std::collections::HashMap;
        use gnx_core::analyzer::types::RawRoute;
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
        let idx_route_method = self.query.capture_index_for_name("route.method");
        let idx_route_path = self.query.capture_index_for_name("route.path");

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
                    if let Ok(t) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
                        type_annotation = Some(t.to_string());
                    }
                } else if Some(cap_idx) == idx_decorator {
                    if let Ok(d) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
                        decorators.push(d.to_string());
                    }
                } else if Some(cap_idx) == idx_export {
                    if let Ok(mod_str) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
                        if mod_str == "private" || mod_str == "protected" {
                            is_exported = false;
                        }
                    }
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(h) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
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
                } else if Some(cap_idx) == idx_route_method {
                    if let Ok(m_str) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
                        route_method = Some(m_str.to_uppercase());
                    }
                } else if Some(cap_idx) == idx_route_path {
                    if let Ok(p_str) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
                        let path = p_str.trim_matches(|c| c == '\'' || c == '"').to_string();
                        route_path = Some(path);
                    }
                } else if Some(cap_idx) == idx_route_call {
                    route_span_node = Some(cap.node);
                }
            }

            if let (Some(rm), Some(rp), Some(rs_node)) = (route_method, route_path, route_span_node) {
                let start = rs_node.start_position();
                let end = rs_node.end_position();
                let exists = routes.iter().any(|r: &RawRoute| {
                    r.method == rm && r.path == rp && r.span == (start.row as u32, start.column as u32, end.row as u32, end.column as u32)
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
                if let Ok(src_str) = std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]) {
                    let full_src = if let Some(p) = import_prefix {
                        if let Ok(p_str) = std::str::from_utf8(&source[p.start_byte()..p.end_byte()]) {
                            format!("{}\\{}", p_str.trim_end_matches('\\'), src_str.trim_start_matches('\\'))
                        } else {
                            src_str.to_string()
                        }
                    } else {
                        src_str.to_string()
                    };

                    let alias = if let Some(a) = import_alias {
                        std::str::from_utf8(&source[a.start_byte()..a.end_byte()]).ok().map(|s| s.to_string())
                    } else {
                        None
                    };

                    let imported_name = if let Some(ref a_str) = alias {
                        a_str.clone()
                    } else {
                        full_src.split('\\').last().unwrap_or("").to_string()
                    };

                    imports.push(RawImport {
                        alias,
                        imported_name,
                        source: full_src,
                    });
                }
            }
        }

        let nodes = node_map.into_values().collect();

        Ok(LocalGraph {
            routes,
            file_path: path.to_path_buf(),
            nodes,
            imports,
        })
    }
}
