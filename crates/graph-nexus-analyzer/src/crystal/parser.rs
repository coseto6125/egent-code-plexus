use super::spec::CrystalSpec;
use crate::calls::extract_calls;
use graph_nexus_core::analyzer::lang_spec::LangSpec;
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
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

impl CrystalProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_crystal::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| CrystalSpec::CAPTURE_KIND.get(name).copied())
            .collect();
        Ok(Self {
            query,
            capture_kind_by_idx,
        })
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

        // Root-span anchors (kind dispatch is via spec table)
        let idx_class = self.query.capture_index_for_name("class");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_const = self.query.capture_index_for_name("const");
        // Metadata captures
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut root_span_node = None;
            let mut kind = None;
            let mut heritage = Vec::new();
            let mut import_source_node = None;

            for cap in m.captures {
                let ci = cap.index;
                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(ci as usize)
                    .copied()
                    .flatten()
                {
                    name_node = Some(cap.node);
                    kind = Some(k_from_spec);
                } else if Some(ci) == idx_class
                    || Some(ci) == idx_method
                    || Some(ci) == idx_const
                {
                    root_span_node = Some(cap.node);
                } else if Some(ci) == idx_heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.to_string());
                    }
                } else if Some(ci) == idx_import_source {
                    import_source_node = Some(cap.node);
                }
            }

            if let (Some(n), Some(root), Some(k)) = (name_node, root_span_node, kind) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[n.start_byte()..n.end_byte()])
                {
                    let start = root.start_position();
                    let end = root.end_position();
                    // Heritage only meaningful for Class; other kinds get empty.
                    let heritage_for_emit = if matches!(k, NodeKind::Class) {
                        std::mem::take(&mut heritage)
                    } else {
                        Vec::new()
                    };
                    nodes.push(RawNode {
                        decorators: Vec::new(),
                        is_exported: true,
                        heritage: heritage_for_emit,
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

            if let Some(src_node) = import_source_node {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[src_node.start_byte()..src_node.end_byte()])
                {
                    imports.push(RawImport {
                        alias: None,
                        imported_name: src_str.to_string(),
                        source: src_str.to_string(),
                        binding_kind: None,
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
