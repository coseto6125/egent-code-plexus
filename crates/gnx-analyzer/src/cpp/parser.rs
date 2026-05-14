use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct CppProvider {
    query: Query,
}

impl CppProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_cpp::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for CppProvider {
    fn name(&self) -> &'static str {
        "cpp"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let language = tree_sitter_cpp::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse cpp file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

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
        let idx_import = self.query.capture_index_for_name("import");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;

            let mut import_name = None;
            let mut import_src = None;
            let mut is_import = false;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if cap_idx == idx_name_function {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if cap_idx == idx_name_class {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if cap_idx == idx_name_method {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
                } else if cap_idx == idx_name_interface {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Interface);
                } else if cap_idx == idx_import_name {
                    import_name = Some(cap.node);
                } else if cap_idx == idx_import_source {
                    import_src = Some(cap.node);
                } else if cap_idx == idx_function
                    || cap_idx == idx_class
                    || cap_idx == idx_method
                    || cap_idx == idx_interface
                {
                    root_span_node = Some(cap.node);
                } else if cap_idx == idx_import {
                    is_import = true;
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
                        is_exported: false,
                        heritage: vec![],
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

            if import_src.is_some() || is_import {
                let final_import_name = import_name.or(import_src);
                if let (Some(i_name), Some(i_src)) = (final_import_name, import_src) {
                    if let (Ok(name_str), Ok(src_str)) =
                        (std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]), std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]))
                    {
                        let mut name_s = name_str.to_string();
                        if name_s.starts_with('<') && name_s.ends_with('>') {
                            name_s = name_s[1..name_s.len() - 1].to_string();
                        } else if name_s.starts_with('"') && name_s.ends_with('"') {
                            name_s = name_s[1..name_s.len() - 1].to_string();
                        }

                        let mut src_s = src_str.to_string();
                        if src_s.starts_with('<') && src_s.ends_with('>') {
                            src_s = src_s[1..src_s.len() - 1].to_string();
                        } else if src_s.starts_with('"') && src_s.ends_with('"') {
                            src_s = src_s[1..src_s.len() - 1].to_string();
                        }

                        imports.push(RawImport {
                        alias: None,
                            imported_name: name_s,
                            source: src_s,
                        });
                    }
                }
            }
        }

        // Deduplicate imports natively using dict to keep uniqueness but keep order (if necessary),
        // or just sort and dedup.
        imports.sort_by(|a, b| {
            a.imported_name
                .cmp(&b.imported_name)
                .then(a.source.cmp(&b.source))
        });
        imports.dedup_by(|a, b| a.imported_name == b.imported_name && a.source == b.source);

        Ok(LocalGraph {
            file_path: path.to_path_buf(),
            nodes,
            imports,
        })
    }
}
