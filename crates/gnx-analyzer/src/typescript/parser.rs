use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct TypeScriptProvider {
    query: Query,
}

impl TypeScriptProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for TypeScriptProvider {
    fn name(&self) -> &'static str {
        "typescript"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse typescript file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();

        // Need mapping for capture indices
        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_class_name = self.query.capture_index_for_name("class.name");
        let idx_method_name = self.query.capture_index_for_name("method.name");
        let idx_interface_name = self.query.capture_index_for_name("interface.name");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_interface = self.query.capture_index_for_name("interface");

        let idx_export = self.query.capture_index_for_name("export");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");

        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import = self.query.capture_index_for_name("import");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut is_exported = false;
            let mut heritage = Vec::new();
            let mut type_annotation = None;

            let mut import_name = None;
            let mut import_alias = None;
            let mut import_src = None;
            let mut is_import = false;

            for cap in m.captures {
                let cap_idx_opt = Some(cap.index);

                if cap_idx_opt == idx_function_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if cap_idx_opt == idx_class_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if cap_idx_opt == idx_method_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
                } else if cap_idx_opt == idx_interface_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Interface);
                } else if cap_idx_opt == idx_export {
                    is_exported = true;
                } else if cap_idx_opt == idx_heritage {
                    if let Ok(h) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
                        heritage.push(h.to_string());
                    }
                } else if cap_idx_opt == idx_type {
                    if let Ok(t) = std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()]) {
                        type_annotation = Some(t.to_string());
                    }
                } else if cap_idx_opt == idx_import_name {
                    import_name = Some(cap.node);
                } else if cap_idx_opt == idx_import_alias {
                    import_alias = Some(cap.node);
                } else if cap_idx_opt == idx_import_source {
                    import_src = Some(cap.node);
                } else if cap_idx_opt == idx_function
                    || cap_idx_opt == idx_class
                    || cap_idx_opt == idx_method
                    || cap_idx_opt == idx_interface
                {
                    root_span_node = Some(cap.node);
                } else if cap_idx_opt == idx_import {
                    is_import = true;
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();

                    let node_span = (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    );

                    let mut found = false;
                    for existing in &mut nodes {
                        if existing.span == node_span && existing.name == name_str {
                            if is_exported {
                                existing.is_exported = true;
                            }
                            if !heritage.is_empty() {
                                existing.heritage.extend(heritage.clone());
                                existing.heritage.sort();
                                existing.heritage.dedup();
                            }
                            if type_annotation.is_some() {
                                existing.type_annotation = type_annotation.clone();
                            }
                            found = true;
                            break;
                        }
                    }

                    if !found {
                        nodes.push(RawNode {
                            name: name_str.to_string(),
                            kind: k,
                            span: node_span,
                            is_exported,
                            heritage,
                            type_annotation,
                        });
                    }
                }
            }

            if is_import {
                if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
                    if let (Ok(name_str), Ok(src_str)) = (
                        std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]),
                        std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                    ) {
                        let alias_str = import_alias.and_then(|a| {
                            std::str::from_utf8(&source[a.start_byte()..a.end_byte()])
                                .ok()
                                .map(|s| s.to_string())
                        });
                        imports.push(RawImport {
                            alias: alias_str,
                            imported_name: name_str.to_string(),
                            source: src_str.to_string(),
                        });
                    }
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
