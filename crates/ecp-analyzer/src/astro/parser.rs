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
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

type Span = (u32, u32, u32, u32);

pub struct AstroProvider {
    astro_query: Query,
    ts_query: Query,
    ts_capture_by_idx: Vec<Option<NodeKind>>,
}

fn capture_kind(name: &str) -> Option<NodeKind> {
    match name {
        "function.name" => Some(NodeKind::Function),
        "class.name" => Some(NodeKind::Class),
        "method.name" => Some(NodeKind::Method),
        "constructor.name" => Some(NodeKind::Constructor),
        "interface.name" => Some(NodeKind::Interface),
        "typedef.name" => Some(NodeKind::Typedef),
        "property.name" => Some(NodeKind::Property),
        "const.name" => Some(NodeKind::Const),
        "variable.name" => Some(NodeKind::Variable),
        "enum.name" => Some(NodeKind::Enum),
        _ => None,
    }
}

impl AstroProvider {
    pub fn new() -> anyhow::Result<Self> {
        let astro_language = tree_sitter_astro::LANGUAGE.into();
        let astro_query = Query::new(&astro_language, include_str!("queries.scm"))?;

        let ts_language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let ts_query_src = include_str!("../typescript/queries.scm");
        let ts_query = Query::new(&ts_language, ts_query_src)?;
        let ts_capture_by_idx = ts_query
            .capture_names()
            .iter()
            .map(|n| capture_kind(n))
            .collect();

        Ok(Self {
            astro_query,
            ts_query,
            ts_capture_by_idx,
        })
    }
}

impl LanguageProvider for AstroProvider {
    fn name(&self) -> &'static str {
        "astro"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        // ── Pass 1: Astro SFC structure ───────────────────────────────────────
        let astro_language = tree_sitter_astro::LANGUAGE.into();
        let mut astro_parser = Parser::new();
        astro_parser.set_language(&astro_language)?;

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

            let (nodes, imports) = parse_frontmatter_content(
                frontmatter_source,
                &self.ts_query,
                &self.ts_capture_by_idx,
                row_offset,
                col_offset,
            );
            all_nodes.extend(nodes);
            all_imports.extend(imports);
        }

        Ok(LocalGraph {
            content_hash: [0; 8],
            file_path: path.to_path_buf(),
            nodes: all_nodes,
            imports: all_imports,
            routes: vec![],
            documents: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
        })
    }
}

/// Parse frontmatter content with tree-sitter-typescript, offsetting all span
/// rows by `row_offset` so they reference positions in the containing .astro file.
fn parse_frontmatter_content(
    frontmatter_source: &[u8],
    query: &Query,
    capture_kind_by_idx: &[Option<NodeKind>],
    row_offset: u32,
    col_offset: u32,
) -> (Vec<RawNode>, Vec<RawImport>) {
    let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return (vec![], vec![]);
    }
    let Some(tree) = parse_with_budget(&mut parser, frontmatter_source, ParseBudget::DEFAULT)
    else {
        return (vec![], vec![]);
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), frontmatter_source);

    let idx_import = query.capture_index_for_name("import");
    let idx_import_name = query.capture_index_for_name("import.name");
    let idx_import_alias = query.capture_index_for_name("import.alias");
    let idx_import_source = query.capture_index_for_name("import.source");
    let idx_import_ns = query.capture_index_for_name("import.namespace");

    let root_span_indices: Vec<u32> = query
        .capture_names()
        .iter()
        .enumerate()
        .filter(|(_, name)| {
            matches!(
                **name,
                "function"
                    | "class"
                    | "method"
                    | "constructor"
                    | "interface"
                    | "property"
                    | "const"
                    | "variable"
                    | "typedef"
                    | "enum"
            )
        })
        .map(|(i, _)| i as u32)
        .collect();

    let export_idx = query.capture_index_for_name("export");

    let mut nodes: Vec<RawNode> = Vec::new();
    let mut imports: Vec<RawImport> = Vec::new();

    while let Some(m) = matches.next() {
        let mut name_node: Option<tree_sitter::Node> = None;
        let mut kind: Option<NodeKind> = None;
        let mut root_span_node: Option<tree_sitter::Node> = None;
        let mut is_exported = false;

        let mut import_name_node: Option<tree_sitter::Node> = None;
        let mut import_alias_node: Option<tree_sitter::Node> = None;
        let mut import_src_node: Option<tree_sitter::Node> = None;
        let mut is_import = false;
        let mut is_import_ns = false;

        for cap in m.captures {
            let ci = cap.index;
            if let Some(Some(k)) = capture_kind_by_idx.get(ci as usize) {
                name_node = Some(cap.node);
                kind = Some(*k);
            } else if Some(ci) == export_idx {
                is_exported = true;
            } else if Some(ci) == idx_import_name {
                import_name_node = Some(cap.node);
            } else if Some(ci) == idx_import_alias {
                import_alias_node = Some(cap.node);
            } else if Some(ci) == idx_import_source {
                import_src_node = Some(cap.node);
            } else if Some(ci) == idx_import {
                is_import = true;
            } else if Some(ci) == idx_import_ns {
                is_import_ns = true;
            } else if root_span_indices.contains(&ci) {
                root_span_node = Some(cap.node);
            }
        }

        if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
            if let Ok(name_str) =
                std::str::from_utf8(&frontmatter_source[n.start_byte()..n.end_byte()])
            {
                let raw_span = node_span(&root);
                let span = offset_span(raw_span, row_offset, col_offset);
                let mut existing = false;
                for node in &mut nodes {
                    if node.span == span && node.name == name_str {
                        if k == NodeKind::Function
                            && matches!(node.kind, NodeKind::Const | NodeKind::Variable)
                        {
                            node.kind = NodeKind::Function;
                        }
                        if is_exported {
                            node.is_exported = true;
                        }
                        existing = true;
                        break;
                    }
                }
                if !existing {
                    nodes.push(RawNode {
                        name: name_str.to_string(),
                        kind: k,
                        span,
                        is_exported,
                        heritage: vec![],
                        type_annotation: None,
                        decorators: vec![],
                        calls: vec![],
                    });
                }
            }
        }

        if is_import {
            if let (Some(i_name), Some(i_src)) = (import_name_node, import_src_node) {
                if let (Ok(name_str), Ok(src_str)) = (
                    std::str::from_utf8(
                        &frontmatter_source[i_name.start_byte()..i_name.end_byte()],
                    ),
                    std::str::from_utf8(&frontmatter_source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    let alias_str = import_alias_node.and_then(|a| {
                        std::str::from_utf8(&frontmatter_source[a.start_byte()..a.end_byte()])
                            .ok()
                            .map(|s| s.to_string())
                    });
                    imports.push(RawImport {
                        imported_name: name_str.to_string(),
                        source: src_str.to_string(),
                        alias: alias_str,
                        binding_kind: None,
                    });
                }
            }
        }

        if is_import_ns {
            if let (Some(a), Some(i_src)) = (import_alias_node, import_src_node) {
                if let (Ok(alias_str), Ok(src_str)) = (
                    std::str::from_utf8(&frontmatter_source[a.start_byte()..a.end_byte()]),
                    std::str::from_utf8(&frontmatter_source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    imports.push(RawImport {
                        imported_name: "*".to_string(),
                        source: src_str.to_string(),
                        alias: Some(alias_str.to_string()),
                        binding_kind: None,
                    });
                }
            }
        }
    }

    (nodes, imports)
}

/// Add `row_offset` to the start and end rows of a span, leaving columns intact.
/// Remaps frontmatter-local line numbers to .astro file line numbers.
#[inline]
fn offset_span(span: Span, row_offset: u32, col_offset: u32) -> Span {
    let start_col = if span.0 == 0 {
        span.1.saturating_add(col_offset)
    } else {
        span.1
    };
    let end_col = if span.2 == 0 {
        span.3.saturating_add(col_offset)
    } else {
        span.3
    };
    (
        span.0.saturating_add(row_offset),
        start_col,
        span.2.saturating_add(row_offset),
        end_col,
    )
}
