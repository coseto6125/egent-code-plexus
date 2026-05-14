use crate::calls::extract_calls;
use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};


thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_cpp::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
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
        
        let tree = PARSER.with(|p| {
            p.borrow_mut()
                .parse(source, None)
        }).ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;


        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        let idx_name_function = self.query.capture_index_for_name("name.function");
        let idx_name_class = self.query.capture_index_for_name("name.class");
        let idx_name_method = self.query.capture_index_for_name("name.method");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");
        let idx_export = self.query.capture_index_for_name("export");
        let idx_alias = self.query.capture_index_for_name("alias");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_import = self.query.capture_index_for_name("import");

        let is_header = path
            .extension()
            .map(|ext| ext == "h" || ext == "hpp" || ext == "hxx" || ext == "hh")
            .unwrap_or(false);

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut type_node = None;
            let mut heritage_nodes = Vec::new();
            let mut is_exported_by_query = false;

            let mut import_src_node = None;
            let mut import_alias_node = None;
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
                } else if cap_idx == idx_heritage {
                    heritage_nodes.push(cap.node);
                } else if cap_idx == idx_type {
                    type_node = Some(cap.node);
                } else if cap_idx == idx_export {
                    is_exported_by_query = true;
                } else if cap_idx == idx_alias {
                    import_alias_node = Some(cap.node);
                } else if cap_idx == idx_import_source {
                    import_src_node = Some(cap.node);
                } else if cap_idx == idx_function || cap_idx == idx_class || cap_idx == idx_method {
                    root_span_node = Some(cap.node);
                } else if cap_idx == idx_import {
                    is_import = true;
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();

                    let type_annotation = type_node.and_then(|t| {
                        std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                            .ok()
                            .map(|s| s.trim().to_string())
                    });

                    let heritage = heritage_nodes
                        .iter()
                        .filter_map(|h| {
                            std::str::from_utf8(&source[h.start_byte()..h.end_byte()])
                                .ok()
                                .map(|s| s.to_string())
                        })
                        .collect();

                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: is_header || is_exported_by_query,
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
                        calls: Vec::new(),
                    });
                }
            }

            if is_import {
                if let Some(src_node) = import_src_node {
                    if let Ok(src_str) =
                        std::str::from_utf8(&source[src_node.start_byte()..src_node.end_byte()])
                    {
                        let mut src_s = src_str.to_string();
                        if (src_s.starts_with('"') && src_s.ends_with('"'))
                            || (src_s.starts_with('<') && src_s.ends_with('>'))
                        {
                            src_s = src_s[1..src_s.len() - 1].to_string();
                        }

                        let alias = import_alias_node.and_then(|a| {
                            std::str::from_utf8(&source[a.start_byte()..a.end_byte()])
                                .ok()
                                .map(|s| s.to_string())
                        });

                        let imported_name = src_s.clone();

                        imports.push(RawImport {
                            alias,
                            imported_name,
                            source: src_s,
                        });
                    }
                }
            }
        }

        imports.sort_by(|a, b| {
            a.imported_name
                .cmp(&b.imported_name)
                .then(a.source.cmp(&b.source))
                .then(a.alias.cmp(&b.alias))
        });
        imports.dedup_by(|a, b| {
            a.imported_name == b.imported_name && a.source == b.source && a.alias == b.alias
        });

        // Extract call sites and attach to enclosing function/method nodes.
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
