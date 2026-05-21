use super::spec::NimSpec;
use crate::calls::extract_calls;
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct NimProvider {
    query: Query,
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

impl NimProvider {
    pub fn new() -> anyhow::Result<Self> {
        // alaviss/tree-sitter-nim exposes language() rather than the LANGUAGE constant;
        // calling .into() converts it to tree_sitter::Language, compatible with 0.25 API.
        let language = tree_sitter_nim::language();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| NimSpec::CAPTURE_KIND.get(name).copied())
            .collect();
        Ok(Self {
            query,
            capture_kind_by_idx,
        })
    }
}

impl LanguageProvider for NimProvider {
    fn name(&self) -> &'static str {
        "nim"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let language = tree_sitter_nim::language();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let tree = parse_with_budget(&mut parser, source, ParseBudget::DEFAULT)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Nim file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();

        // Span captures (root anchors) and metadata-only captures.
        // Kind dispatch is via spec table below; the *.name index fields
        // are no longer needed.
        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_const = self.query.capture_index_for_name("const");
        let idx_typedef = self.query.capture_index_for_name("typedef");
        let idx_import = self.query.capture_index_for_name("import");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut import_src_node = None;
            let mut is_import = false;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap.index as usize)
                    .copied()
                    .flatten()
                {
                    name_node = Some(cap.node);
                    kind = Some(k_from_spec);
                } else if cap_idx == idx_import_source {
                    import_src_node = Some(cap.node);
                } else if cap_idx == idx_import {
                    is_import = true;
                    root_span_node = Some(cap.node);
                } else if cap_idx == idx_typedef {
                    // Defer Typedef root: only set if the parent type_declaration
                    // has no object_declaration / distinct_type / ref_type child
                    // (those stay as Class).  We store the node and validate below.
                    root_span_node = Some(cap.node);
                } else if cap_idx == idx_function || cap_idx == idx_class || cap_idx == idx_const {
                    root_span_node = Some(cap.node);
                }
            }

            // Emit a node for proc/func/method/iterator/template/macro/type/const.
            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                // For Typedef candidates: skip if the parent type_declaration has
                // an object/distinct/ref/enum child — those stay Class/Enum.
                if k == NodeKind::Typedef {
                    if let Some(type_decl) = root.parent() {
                        let mut wc = type_decl.walk();
                        let has_class_child = type_decl.named_children(&mut wc).any(|c| {
                            matches!(
                                c.kind(),
                                "object_declaration"
                                    | "distinct_type"
                                    | "ref_type"
                                    | "enum_declaration"
                            )
                        });
                        if has_class_child {
                            continue;
                        }
                    }
                }

                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    // exported_symbol nodes include the trailing `*`; strip it for the name.
                    let clean_name = name_str.trim_end_matches('*');
                    let start = root.start_position();
                    let end = root.end_position();

                    // Nim procedures are exported iff the name node carries the
                    // trailing `*` export marker (tree-sitter-nim uses
                    // `exported_symbol` for that form).
                    let is_exported = name_str.ends_with('*');

                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported,
                        heritage: vec![],
                        type_annotation: None,
                        name: clean_name.to_string(),
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

            // Emit an import for bare `import foo` statements.
            if is_import && import_src_node.is_none() {
                if let Some(root) = root_span_node {
                    if let Ok(raw) =
                        std::str::from_utf8(&source[root.start_byte()..root.end_byte()])
                    {
                        // `import_statement` text is e.g. "import strutils, sequtils"
                        // Strip the "import " keyword and push each module.
                        let body = raw.trim_start_matches("import").trim();
                        for module in body.split(',') {
                            let module = module.trim();
                            if !module.is_empty() {
                                imports.push(RawImport {
                                    alias: None,
                                    imported_name: module.to_string(),
                                    source: module.to_string(),
                                    binding_kind: None,
                                });
                            }
                        }
                    }
                }
            }

            // Emit an import for `from foo import bar`.
            if let Some(i_src) = import_src_node {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
                {
                    imports.push(RawImport {
                        alias: None,
                        imported_name: "*".to_string(),
                        source: src_str.to_string(),
                        binding_kind: None,
                    });
                }
            }
        }

        extract_calls(tree.root_node(), source, &mut nodes, &["call"]);

        Ok(LocalGraph {
            content_hash: [0; 8],
            routes: vec![],
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: vec![],
            event_topics: vec![],
            tx_scopes: vec![],
        })
    }
}
