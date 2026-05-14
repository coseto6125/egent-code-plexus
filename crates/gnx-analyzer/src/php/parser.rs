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

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        let idx_name_function = self.query.capture_index_for_name("name.function");
        let idx_name_class = self.query.capture_index_for_name("name.class");
        let idx_name_interface = self.query.capture_index_for_name("name.interface");
        let idx_name_method = self.query.capture_index_for_name("name.method");
        let idx_type_function = self.query.capture_index_for_name("type.function");
        let idx_type_method = self.query.capture_index_for_name("type.method");
        let idx_export = self.query.capture_index_for_name("export");
        let idx_heritage = self.query.capture_index_for_name("heritage");

        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");
        let idx_import_prefix = self.query.capture_index_for_name("import.prefix");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_interface = self.query.capture_index_for_name("interface");
        let idx_method = self.query.capture_index_for_name("method");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut is_exported = true;
            let mut heritage = Vec::new();
            let mut type_annotation = None;

            let mut import_src = None;
            let mut import_alias = None;
            let mut import_prefix = None;

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
                    root_span_node = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
                        is_exported,
                        heritage,
                        type_annotation,
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

        Ok(LocalGraph {
            file_path: path.to_path_buf(),
            nodes,
            imports,
        })
    }
}
