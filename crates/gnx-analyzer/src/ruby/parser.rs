use crate::calls::extract_calls;
use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawImport, RawNode, RawRoute};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};


thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_ruby::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct RubyProvider {
    query: Query,
}

impl RubyProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_ruby::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for RubyProvider {
    fn name(&self) -> &'static str {
        "ruby"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        
        let tree = PARSER.with(|p| {
            p.borrow_mut()
                .parse(source, None)
        }).ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;


        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();
        let mut routes: Vec<RawRoute> = Vec::new();

        let idx_name = self.query.capture_index_for_name("name");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_module = self.query.capture_index_for_name("module");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_decorator = self.query.capture_index_for_name("decorator");
        let idx_route_method = self.query.capture_index_for_name("route.method");
        let idx_route_path = self.query.capture_index_for_name("route.path");
        let idx_route = self.query.capture_index_for_name("route");

        while let Some(m) = matches.next() {
            let mut node_name = None;
            let mut kind = None;
            let mut root_node = None;
            let mut heritage = Vec::new();
            let mut import_name = None;
            let mut decorators = Vec::new();

            let mut route_method = None;
            let mut route_path = None;
            let mut route_root = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if cap_idx == idx_name {
                    node_name = Some(cap.node);
                } else if cap_idx == idx_heritage {
                    if let Ok(h_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h_str.to_string());
                    }
                } else if cap_idx == idx_class {
                    kind = Some(NodeKind::Class);
                    root_node = Some(cap.node);
                } else if cap_idx == idx_module {
                    kind = Some(NodeKind::Class); // Modules are treated as Class for graph
                    root_node = Some(cap.node);
                } else if cap_idx == idx_method {
                    kind = Some(NodeKind::Method);
                    root_node = Some(cap.node);
                } else if cap_idx == idx_import_name {
                    import_name = Some(cap.node);
                } else if cap_idx == idx_decorator {
                    if let Ok(d_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d_str.to_string());
                    }
                } else if cap_idx == idx_route_method {
                    route_method = Some(cap.node);
                } else if cap_idx == idx_route_path {
                    route_path = Some(cap.node);
                } else if cap_idx == idx_route {
                    route_root = Some(cap.node);
                }
            }

            if let (Some(name_node), Some(k), Some(root)) = (node_name, kind, root_node) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                {
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
                        decorators: decorators.clone(),
                        is_exported: true,
                        heritage,
                        type_annotation: None,
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
                }
            }

            if let Some(i_node) = import_name {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[i_node.start_byte()..i_node.end_byte()])
                {
                    imports.push(RawImport {
                        alias: None,
                        imported_name: name_str.to_string(),
                        source: name_str.to_string(),
                    });
                }
            }

            if let (Some(r_method), Some(r_path), Some(r_root)) =
                (route_method, route_path, route_root)
            {
                if let (Ok(method_str), Ok(path_str)) = (
                    std::str::from_utf8(&source[r_method.start_byte()..r_method.end_byte()]),
                    std::str::from_utf8(&source[r_path.start_byte()..r_path.end_byte()]),
                ) {
                    let start = r_root.start_position();
                    let end = r_root.end_position();
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

        // Extract call sites and attach to enclosing function/method nodes.
        extract_calls(
            tree.root_node(),
            source,
            &mut nodes,
            &["call", "method_call"],
        );

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes,
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
        })
    }
}
