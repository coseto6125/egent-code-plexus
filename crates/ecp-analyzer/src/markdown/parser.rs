use crate::parse_budget::{parse_with_budget, ParseBudget};
use anyhow::Result;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawDocumentBlock};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

/// Tree-sitter Markdown provider. Document-only extractor (no symbols,
/// no calls) — we just emit `RawDocumentBlock` records so PathLiteral
/// nodes can be sink-attributed to the right .md file in the graph.
pub struct MarkdownProvider {
    query: Query,
}

impl MarkdownProvider {
    pub fn new() -> Result<Self, String> {
        let language = tree_sitter_md::LANGUAGE.into();
        let query_str = include_str!("queries.scm");
        let query = Query::new(&language, query_str)
            .map_err(|e| format!("Failed to create Markdown query: {}", e))?;

        Ok(Self { query })
    }
}

impl LanguageProvider for MarkdownProvider {
    fn name(&self) -> &'static str {
        "Markdown"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> Result<LocalGraph> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_md::LANGUAGE.into())
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        let tree = parse_with_budget(&mut parser, source, ParseBudget::DEFAULT)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Markdown file"))?;
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut documents: Vec<ecp_core::analyzer::types::RawDocumentBlock> = Vec::new();

        let idx_document = self
            .query
            .capture_index_for_name("document")
            .unwrap_or(u32::MAX);
        let idx_section = self
            .query
            .capture_index_for_name("section")
            .unwrap_or(u32::MAX);
        let idx_section_name = self
            .query
            .capture_index_for_name("section.name")
            .unwrap_or(u32::MAX);

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

                    let filename = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();

                    documents.push(RawDocumentBlock {
                        name: filename,
                        is_section: false,
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
                    if let Ok(name_str) =
                        std::str::from_utf8(&source[n_node.start_byte()..n_node.end_byte()])
                    {
                        let start = root.start_position();
                        let end = root.end_position();

                        documents.push(RawDocumentBlock {
                            name: name_str.trim().to_string(),
                            is_section: true,
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
            content_hash: [0; 8],
            routes: vec![],
            file_path: path.to_path_buf(),
            nodes: vec![],
            documents,
            imports: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: vec![],
            raw_function_metas: vec![],
        })
    }
}
