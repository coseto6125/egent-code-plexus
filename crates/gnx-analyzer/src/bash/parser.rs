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
        let language = tree_sitter_bash::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct BashProvider {
    query: Query,
}

impl BashProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_bash::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for BashProvider {
    fn name(&self) -> &'static str {
        "bash"
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

        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_function = self.query.capture_index_for_name("function");
        let idx_const_name = self.query.capture_index_for_name("const.name");
        let idx_const = self.query.capture_index_for_name("const");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut import_src = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if Some(cap_idx) == idx_function_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if Some(cap_idx) == idx_const_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Const);
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_function || Some(cap_idx) == idx_const {
                    root_span_node = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();

                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: true,
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
                        calls: Vec::new(),
                    });
                }
            }

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

        extract_calls(tree.root_node(), source, &mut nodes, &["command"]);

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
