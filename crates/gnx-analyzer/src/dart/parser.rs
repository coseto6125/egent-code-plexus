use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct DartProvider {
    query: Query,
}

impl DartProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_dart::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for DartProvider {
    fn name(&self) -> &'static str {
        "dart"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let language = tree_sitter_dart::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse dart file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        let idx_class_name = self.query.capture_index_for_name("class.name");
        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_method_name = self.query.capture_index_for_name("method.name");
        let idx_interface_name = self.query.capture_index_for_name("interface.name");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");
        let idx_decorator = self.query.capture_index_for_name("decorator");

        let idx_class = self.query.capture_index_for_name("class");
        let idx_function = self.query.capture_index_for_name("function");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_interface = self.query.capture_index_for_name("interface");
        let idx_import = self.query.capture_index_for_name("import");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut heritage = Vec::new();
            let mut type_annotation = None;
            let mut decorators = Vec::new();

            let mut import_source = None;
            let mut import_alias = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if Some(cap_idx) == idx_class_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if Some(cap_idx) == idx_function_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if Some(cap_idx) == idx_method_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
                } else if Some(cap_idx) == idx_interface_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Interface);
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(h) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
                        heritage.push(h.trim().to_string());
                    }
                } else if Some(cap_idx) == idx_type {
                    if let Ok(t) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
                        type_annotation = Some(t.trim().to_string());
                    }
                } else if Some(cap_idx) == idx_import_source {
                    import_source = Some(cap.node);
                } else if Some(cap_idx) == idx_import_alias {
                    import_alias = Some(cap.node);
                } else if Some(cap_idx) == idx_decorator {
                    if let Ok(d_str) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
                        decorators.push(d_str.to_string());
                    }
                }

                if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_method
                    || Some(cap_idx) == idx_interface
                    || Some(cap_idx) == idx_import
                {
                    root_span_node = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let name_str = name_str.trim();
                    let is_exported = !name_str.starts_with('_');
                    let start = root.start_position();
                    let end = root.end_position();
                    
                    nodes.push(RawNode {
                        decorators,
                        is_exported,
                        heritage: heritage.clone(),
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
                }
            }

            if let Some(i_src) = import_source {
                if let Ok(src_str) = std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]) {
                    let clean_src = src_str.trim().trim_matches('\'').trim_matches('"').to_string();
                    
                    let alias_str = if let Some(i_alias) = import_alias {
                        std::str::from_utf8(&source[i_alias.start_byte()..i_alias.end_byte()])
                            .ok()
                            .map(|s| s.trim().to_string())
                    } else {
                        None
                    };

                    imports.push(RawImport {
                        alias: alias_str,
                        imported_name: clean_src.clone(),
                        source: clean_src,
                    });
                }
            }
        }

        // Deduplicate simple identical node extractions
        nodes.dedup_by(|a, b| a.name == b.name && a.span == b.span && a.kind == b.kind);

        Ok(LocalGraph {
            routes: vec![],
            file_path: path.to_path_buf(),
            nodes,
            imports,
        })
    }
}
