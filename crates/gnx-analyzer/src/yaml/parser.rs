use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawNode};
use gnx_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct YamlProvider {
    query: Query,
}

impl YamlProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_yaml::LANGUAGE.into();
        let query_str = include_str!("queries.scm");
        let query = Query::new(&language, query_str)
            .map_err(|e| anyhow::anyhow!("Failed to create YAML query: {}", e))?;

        Ok(Self { query })
    }
}

impl LanguageProvider for YamlProvider {
    fn name(&self) -> &'static str {
        "YAML"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_yaml::LANGUAGE.into())
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse YAML file"))?;
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();

        let idx_document = self.query.capture_index_for_name("document").unwrap_or(u32::MAX);
        let idx_section = self.query.capture_index_for_name("section").unwrap_or(u32::MAX);
        let idx_section_name = self.query.capture_index_for_name("section.name").unwrap_or(u32::MAX);

        let mut has_document = false;

        while let Some(m) = matches.next() {
            let mut is_document = false;
            let mut is_section = false;
            let mut section_name = None;
            let mut root_node: Option<tree_sitter::Node> = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if cap_idx == idx_document {
                    is_document = true;
                    root_node = Some(cap.node);
                } else if cap_idx == idx_section {
                    is_section = true;
                    root_node = Some(cap.node);
                } else if cap_idx == idx_section_name {
                    section_name = Some(cap.node);
                }
            }

            if is_document && !has_document {
                if let Some(root) = root_node {
                    let start = root.start_position();
                    let end = root.end_position();
                    
                    let filename = path.file_name().unwrap_or_default().to_string_lossy().to_string();

                    nodes.push(RawNode {
                        decorators: vec![],
                        name: filename,
                        kind: NodeKind::Document,
                        is_exported: true,
                        heritage: vec![],
                        type_annotation: None,
                        calls: vec![],
                        span: (
                            start.row as u32,
                            start.column as u32,
                            end.row as u32,
                            end.column as u32,
                        ),
                    });
                    has_document = true;
                }
            } else if is_section {
                if let (Some(root), Some(n_node)) = (root_node, section_name) {
                    if let Ok(name_str) = std::str::from_utf8(&source[n_node.start_byte()..n_node.end_byte()]) {
                        let start = root.start_position();
                        let end = root.end_position();
                        
                        nodes.push(RawNode {
                            decorators: vec![],
                            name: name_str.trim().to_string(),
                            kind: NodeKind::Section,
                            is_exported: true,
                            heritage: vec![],
                            type_annotation: None,
                            calls: vec![],
                            span: (
                                start.row as u32,
                                start.column as u32,
                                end.row as u32,
                                end.column as u32,
                            ),
                        });
                    }
                }
            }
        }

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes: vec![],
            file_path: path.to_path_buf(),
            nodes,
            imports: vec![],
        })
    }
}