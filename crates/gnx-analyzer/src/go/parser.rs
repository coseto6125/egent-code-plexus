use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use gnx_core::graph::NodeKind;
use std::path::Path;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct GoProvider {
    query: Query,
}

impl GoProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_go::language();
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
        let language = tree_sitter_go::language();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let tree = parser.parse(source, None).ok_or_else(|| anyhow::anyhow!("Failed to parse go file"))?;
        
        let mut cursor = QueryCursor::new();
        let matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        let idx_name_function = self.query.capture_index_for_name("name.function");
        let idx_name_class = self.query.capture_index_for_name("name.class");
        let idx_name_method = self.query.capture_index_for_name("name.method");
        let idx_name_interface = self.query.capture_index_for_name("name.interface");
        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_interface = self.query.capture_index_for_name("interface");

        for m in matches {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            
            let mut import_name = None;
            let mut import_src = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if Some(cap_idx) == idx_name_function {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if Some(cap_idx) == idx_name_class {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if Some(cap_idx) == idx_name_method {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
                } else if Some(cap_idx) == idx_name_interface {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Interface);
                } else if Some(cap_idx) == idx_import_name {
                    import_name = Some(cap.node);
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_function || Some(cap_idx) == idx_class || Some(cap_idx) == idx_method || Some(cap_idx) == idx_interface {
                    root_span_node = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = n.utf8_text(source) {
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
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

            if let Some(i_src) = import_src {
                if let Ok(src_str) = i_src.utf8_text(source) {
                    let clean_src = src_str.trim_matches(|c| c == '"' || c == '`').to_string();
                    let imported_name = if let Some(i_name) = import_name {
                        if let Ok(name_str) = i_name.utf8_text(source) {
                            name_str.to_string()
                        } else {
                            clean_src.clone()
                        }
                    } else if let Some(slash_idx) = clean_src.rfind('/') {
                        clean_src[slash_idx + 1..].to_string()
                    } else {
                        clean_src.clone()
                    };

                    // Prevent duplicate import pushing since the query matches might overlap
                    // but usually tree-sitter will return one match per import
                    imports.push(RawImport {
                        imported_name,
                        source: clean_src,
                    });
                }
            }
        }

        // Deduplicate imports as multiple captures might match the same import statement
        imports.sort_by(|a, b| a.source.cmp(&b.source).then(a.imported_name.cmp(&b.imported_name)));
        imports.dedup_by(|a, b| a.source == b.source && a.imported_name == b.imported_name);

        Ok(LocalGraph {
            file_path: path.to_path_buf(),
            nodes,
            imports,
        })
    }
}
