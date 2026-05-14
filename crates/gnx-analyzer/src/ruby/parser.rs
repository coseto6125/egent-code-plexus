use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

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
        let language = tree_sitter_ruby::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse ruby file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        let idx_name = self.query.capture_index_for_name("name");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_module = self.query.capture_index_for_name("module");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_import_name = self.query.capture_index_for_name("import.name");

        while let Some(m) = matches.next() {
            let mut node_name = None;
            let mut kind = None;
            let mut root_node = None;
            let mut heritage = Vec::new();
            let mut import_name = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if cap_idx == idx_name {
                    node_name = Some(cap.node);
                } else if cap_idx == idx_heritage {
                    if let Ok(h_str) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
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
                }
            }

            if let (Some(name_node), Some(k), Some(root)) = (node_name, kind, root_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
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
                    });
                }
            }

            if let Some(i_node) = import_name {
                if let Ok(name_str) = std::str::from_utf8(&source[i_node.start_byte()..i_node.end_byte()]) {
                    imports.push(RawImport {
                        alias: None,
                        imported_name: name_str.to_string(),
                        source: name_str.to_string(),
                    });
                }
            }
        }

        Ok(LocalGraph {
            file_path: path.to_path_buf(),
            nodes,
            imports,
        })
    }
}
