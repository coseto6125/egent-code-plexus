use crate::calls::extract_calls;
use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct CairoProvider {
    query: Query,
}

impl CairoProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_cairo::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for CairoProvider {
    fn name(&self) -> &'static str {
        "cairo"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let language = tree_sitter_cairo::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Cairo file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_struct_name = self.query.capture_index_for_name("struct.name");
        let idx_class_name = self.query.capture_index_for_name("class.name");
        let idx_const_name = self.query.capture_index_for_name("const.name");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_heritage = self.query.capture_index_for_name("heritage");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_struct = self.query.capture_index_for_name("struct");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_const = self.query.capture_index_for_name("const");
        let idx_import = self.query.capture_index_for_name("import");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut heritage = Vec::new();
            let mut import_src = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if Some(cap_idx) == idx_function_name {
                    name_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Function);
                    }
                } else if Some(cap_idx) == idx_struct_name || Some(cap_idx) == idx_class_name {
                    // struct 與 class 在 gnx NodeKind 統一映射為 Class。
                    name_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Class);
                    }
                } else if Some(cap_idx) == idx_const_name {
                    name_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Const);
                    }
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.to_string());
                    }
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_struct
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_const
                    || Some(cap_idx) == idx_import
                {
                    root_span_node = Some(cap.node);
                }
            }

            // Emit symbol node.
            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
                        decorators: vec![],
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

            // Emit import record.
            if let Some(i_src) = import_src {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
                {
                    imports.push(RawImport {
                        alias: None,
                        imported_name: "*".to_string(),
                        source: src_str.to_string(),
                    });
                }
            }
        }

        extract_calls(tree.root_node(), source, &mut nodes, &["call_expression"]);

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes: vec![],
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
