use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawImport, RawNode};
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
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
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
        let mut imports = Vec::new();

        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_class_name = self.query.capture_index_for_name("class.name");
        let idx_type = self.query.capture_index_for_name("type");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut type_annotation_node = None;
            let mut heritage = Vec::new();

            let mut import_name_node = None;
            let mut import_src_node = None;
            let mut import_alias_node = None;

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
                    if let Ok(h) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
                        heritage.push(h.to_string());
                    }
                } else if cap_idx == idx_import_name {
                    import_name_node = Some(cap.node);
                } else if cap_idx == idx_import_source {
                    import_src_node = Some(cap.node);
                } else if cap_idx == idx_import_alias {
                    import_alias_node = Some(cap.node);
                } else if cap_idx == idx_function || cap_idx == idx_class {
                    root_span_node = Some(cap.node);
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
                        std::str::from_utf8(&source[t.start_byte()..t.end_byte()]).ok().map(|s| s.to_string())
                    });

                    if let Some(existing) = nodes.iter_mut().find(|node| node.span == span) {
                        for h in heritage {
                            if !existing.heritage.contains(&h) {
                                existing.heritage.push(h);
                            }
                        }
                    } else {
                        nodes.push(RawNode {
                            is_exported: !name_str.starts_with('_'),
                            heritage,
                            type_annotation: type_str,
                            name: name_str.to_string(),
                            kind: k,
                            span,
                        });
                    }
                }
            }

            if let Some(i_name) = import_name_node {
                if let Ok(name_str) = std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]) {
                    let src_str = if let Some(i_src) = import_src_node {
                        std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]).unwrap_or("").to_string()
                    } else {
                        "".to_string()
                    };
                    
                    let alias = import_alias_node.and_then(|a| {
                        std::str::from_utf8(&source[a.start_byte()..a.end_byte()]).ok().map(|s| s.to_string())
                    });

                    imports.push(RawImport {
                        alias,
                        imported_name: name_str.to_string(),
                        source: src_str,
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
