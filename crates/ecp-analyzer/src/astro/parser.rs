//! Astro SFC parser — two-pass: tree-sitter-astro for block structure,
//! tree-sitter-typescript for frontmatter content.
//!
//! ## LLM-utility justification (CLAUDE.md §B — Node coverage)
//!
//! Without this parser, Astro frontmatter (the `---` fenced block that serves
//! as the component's server-side script) is invisible to the graph. Agents
//! querying `ecp impact` on a variable defined in frontmatter — or on an
//! import like `import Layout from '../layouts/Base.astro'` — get zero callers
//! or wrong results. This parser makes those symbols first-class graph nodes
//! with correct file:line spans, enabling accurate cross-file impact analysis.
//!
//! ## Two-pass strategy
//!
//! **Pass 1 (Astro grammar):** locate the `frontmatter`, `<style>`, and client
//! `<script>` block positions.  The `frontmatter` node contains a
//! `frontmatter_js_block` child whose byte range gives the content between the
//! `---` fences.
//!
//! **Pass 2 (TypeScript grammar):** re-parse the frontmatter bytes with
//! tree-sitter-typescript.  All span rows are offset by the frontmatter start
//! row so they reference the `.astro` file's line numbers, not frontmatter-
//! local line numbers.
//!
//! ## Design decisions
//!
//! - Frontmatter is always parsed as TypeScript, even for JS-only projects —
//!   Astro's frontmatter syntax is a TS superset and tree-sitter-typescript
//!   handles plain JS correctly.
//! - Template expressions (`{value}`) are **not** parsed; linking them back to
//!   frontmatter identifiers is template-traversal work deferred to a future PR
//!   (same stance as the Vue SFC parser).
//! - `<style>` and client `<script>` each receive a `Section` node; their
//!   contents are not parsed.
//! - **gitnexus comparison:** gitnexus has no Astro support; this is an
//!   ecp-leading feature.

use crate::framework_helpers::node_span;
use crate::parse_budget::{parse_with_budget, ParseBudget};
use crate::sfc_common;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor};

pub struct AstroProvider {
    astro_language: Language,
    ts_language: Language,
    astro_query: Query,
    ts_query: Query,
    ts_capture_by_idx: Vec<Option<NodeKind>>,
    ts_root_span_mask: u64,
}

impl AstroProvider {
    pub fn new() -> anyhow::Result<Self> {
        let astro_language: Language = tree_sitter_astro::LANGUAGE.into();
        let astro_query = Query::new(&astro_language, include_str!("queries.scm"))?;

        let ts_language: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let ts_query = Query::new(&ts_language, include_str!("../typescript/queries.scm"))?;
        let ts_capture_by_idx = sfc_common::capture_kind_by_idx(&ts_query);
        let ts_root_span_mask = sfc_common::root_span_mask(&ts_query);

        Ok(Self {
            astro_language,
            ts_language,
            astro_query,
            ts_query,
            ts_capture_by_idx,
            ts_root_span_mask,
        })
    }
}

impl LanguageProvider for AstroProvider {
    fn name(&self) -> &'static str {
        "astro"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        // ── Pass 1: Astro SFC structure ───────────────────────────────────────
        let mut astro_parser = Parser::new();
        astro_parser.set_language(&self.astro_language)?;

        let astro_tree = parse_with_budget(&mut astro_parser, source, ParseBudget::DEFAULT)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter-astro failed to parse {:?}", path))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.astro_query, astro_tree.root_node(), source);

        let idx_frontmatter = self.astro_query.capture_index_for_name("frontmatter");
        let idx_frontmatter_body = self.astro_query.capture_index_for_name("frontmatter.body");
        let idx_style = self.astro_query.capture_index_for_name("style");
        let idx_script = self.astro_query.capture_index_for_name("script");

        let mut section_nodes: Vec<RawNode> = Vec::new();
        // The frontmatter_js_block node (content between the --- fences).
        let mut frontmatter_body: Option<tree_sitter::Node> = None;
        // The frontmatter node itself (for the Section span).
        let mut frontmatter_node: Option<tree_sitter::Node> = None;

        while let Some(m) = matches.next() {
            for cap in m.captures {
                let ci = Some(cap.index);
                if ci == idx_frontmatter {
                    frontmatter_node = Some(cap.node);
                } else if ci == idx_frontmatter_body {
                    frontmatter_body = Some(cap.node);
                } else if ci == idx_style {
                    section_nodes.push(RawNode {
                        name: "style".to_string(),
                        kind: NodeKind::Section,
                        span: node_span(&cap.node),
                        is_exported: false,
                        heritage: vec![],
                        type_annotation: None,
                        decorators: vec![],
                        calls: vec![],
                        field_reads: Vec::new(),
                        owner_class: None,
                        content_hash: 0,
                    });
                } else if ci == idx_script {
                    section_nodes.push(RawNode {
                        name: "script".to_string(),
                        kind: NodeKind::Section,
                        span: node_span(&cap.node),
                        is_exported: false,
                        heritage: vec![],
                        type_annotation: None,
                        decorators: vec![],
                        calls: vec![],
                        field_reads: Vec::new(),
                        owner_class: None,
                        content_hash: 0,
                    });
                }
            }
        }

        // Emit a Section node for the frontmatter block.
        if let Some(ref fm_node) = frontmatter_node {
            section_nodes.push(RawNode {
                name: "frontmatter".to_string(),
                kind: NodeKind::Section,
                span: node_span(fm_node),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
            });
        }

        // Compute template Section: the region of the document after the
        // frontmatter (and not covered by <style>/<script> nodes captured
        // above). We represent it as a single Section spanning from the first
        // non-frontmatter byte to the end of the document.
        let root = astro_tree.root_node();
        let doc_end_row = root.end_position().row as u32;
        let doc_end_col = root.end_position().column as u32;
        let template_start_row = frontmatter_node
            .as_ref()
            .map(|n| n.end_position().row as u32 + 1)
            .unwrap_or(0);

        // Only emit a template Section if there is any content after frontmatter.
        if template_start_row <= doc_end_row {
            section_nodes.push(RawNode {
                name: "template".to_string(),
                kind: NodeKind::Section,
                span: (template_start_row, 0, doc_end_row, doc_end_col),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
            });
        }

        // ── Pass 2: re-parse frontmatter with TypeScript ──────────────────────
        let mut all_nodes: Vec<RawNode> = section_nodes;
        let mut all_imports: Vec<RawImport> = Vec::new();

        if let Some(body_node) = frontmatter_body {
            // frontmatter_js_block starts on the line immediately after the
            // opening `---` fence (row index 1 for a file starting with `---`).
            // Its start_position().row gives that row in .astro file coordinates,
            // so we use it directly as the offset for inner-parse row remapping.
            let row_offset = body_node.start_position().row as u32;
            let col_offset = body_node.start_position().column as u32;
            let body_start = body_node.start_byte();
            let body_end = body_node.end_byte();
            let frontmatter_source = &source[body_start..body_end];

            let (nodes, imports) = sfc_common::parse_embedded_script(
                frontmatter_source,
                &self.ts_query,
                &self.ts_capture_by_idx,
                self.ts_root_span_mask,
                &self.ts_language,
                row_offset,
                col_offset,
            );
            all_nodes.extend(nodes);
            all_imports.extend(imports);
        }

        Ok(LocalGraph {
            file_path: path.to_path_buf(),
            nodes: all_nodes,
            imports: all_imports,
            ..Default::default()
        })
    }
}
