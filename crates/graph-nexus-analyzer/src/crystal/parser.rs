use crate::calls::extract_calls;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode, RawRoute};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_crystal::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct CrystalProvider {
    query: Query,
}

impl CrystalProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_crystal::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for CrystalProvider {
    fn name(&self) -> &'static str {
        "crystal"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();
        let routes: Vec<RawRoute> = Vec::new();

        let idx_class_name = self.query.capture_index_for_name("class.name");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_method_name = self.query.capture_index_for_name("method.name");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_const_name = self.query.capture_index_for_name("const.name");
        let idx_const = self.query.capture_index_for_name("const");

        while let Some(m) = matches.next() {
            let mut class_name_node = None;
            let mut class_root = None;
            let mut method_name_node = None;
            let mut method_root = None;
            let mut heritage = Vec::new();
            let mut import_source_node = None;
            let mut const_name_node = None;
            let mut const_root = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if cap_idx == idx_class_name {
                    class_name_node = Some(cap.node);
                } else if cap_idx == idx_class {
                    class_root = Some(cap.node);
                } else if cap_idx == idx_heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.to_string());
                    }
                } else if cap_idx == idx_method_name {
                    method_name_node = Some(cap.node);
                } else if cap_idx == idx_method {
                    method_root = Some(cap.node);
                } else if cap_idx == idx_import_source {
                    import_source_node = Some(cap.node);
                } else if cap_idx == idx_const_name {
                    const_name_node = Some(cap.node);
                } else if cap_idx == idx_const {
                    const_root = Some(cap.node);
                }
            }

            if let (Some(name_node), Some(root)) = (class_name_node, class_root) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                {
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
                        decorators: Vec::new(),
                        is_exported: true,
                        heritage,
                        type_annotation: None,
                        name: name_str.to_string(),
                        kind: NodeKind::Class,
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

            if let (Some(name_node), Some(root)) = (method_name_node, method_root) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                {
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
                        decorators: Vec::new(),
                        is_exported: true,
                        heritage: Vec::new(),
                        type_annotation: None,
                        name: name_str.to_string(),
                        kind: NodeKind::Method,
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

            if let Some(src_node) = import_source_node {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[src_node.start_byte()..src_node.end_byte()])
                {
                    imports.push(RawImport {
                        alias: None,
                        imported_name: src_str.to_string(),
                        source: src_str.to_string(),
                    });
                }
            }

            if let (Some(name_node), Some(root)) = (const_name_node, const_root) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                {
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
                        decorators: Vec::new(),
                        is_exported: true,
                        heritage: Vec::new(),
                        type_annotation: None,
                        name: name_str.to_string(),
                        kind: NodeKind::Const,
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
        }

        extract_calls(tree.root_node(), source, &mut nodes, &["call"]);

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes,
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
