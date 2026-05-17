use crate::calls::extract_calls;
use super::spec::HclSpec;
use graph_nexus_core::analyzer::lang_spec::LangSpec;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_hcl::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct HclProvider {
    query: Query,
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

impl HclProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_hcl::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;

        let capture_names = query.capture_names();
        let capture_kind_by_idx: Vec<Option<NodeKind>> = capture_names
            .iter()
            .map(|name| HclSpec::CAPTURE_KIND.get(name).copied())
            .collect();

        Ok(Self { query, capture_kind_by_idx })
    }
}

impl LanguageProvider for HclProvider {
    fn name(&self) -> &'static str {
        "hcl"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        // Capture index pairs — resolve once before the loop.
        let idx_class = self.query.capture_index_for_name("class");
        let idx_const = self.query.capture_index_for_name("const");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import = self.query.capture_index_for_name("import");
        // Auxiliary label captures for resource/data two-label patterns.
        let idx_res_type = self.query.capture_index_for_name("_res_type");
        let idx_data_type = self.query.capture_index_for_name("_data_type");
        // Output block capture — these nodes are the module's public interface.
        let idx_output_name = self.query.capture_index_for_name("output.name");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut import_src = None;
            // For resource/data: accumulate prefix label to build "type.name".
            let mut prefix_node = None;
            // Track whether this match came from an `output` block.
            let mut is_output_block = false;

            for cap in m.captures {
                let cap_idx = cap.index as usize;
                if let Some(k) = self.capture_kind_by_idx.get(cap_idx).and_then(|opt| *opt) {
                    // This is a .name capture with a NodeKind
                    name_node = Some(cap.node);
                    kind = Some(k);
                    if Some(cap_idx as u32) == idx_output_name {
                        is_output_block = true;
                    }
                } else if Some(cap_idx as u32) == idx_class || Some(cap_idx as u32) == idx_const {
                    root_span_node = Some(cap.node);
                } else if Some(cap_idx as u32) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx as u32) == idx_import {
                    root_span_node = Some(cap.node);
                } else if Some(cap_idx as u32) == idx_res_type || Some(cap_idx as u32) == idx_data_type {
                    prefix_node = Some(cap.node);
                }
            }

            // Push a declaration node if we have a name + kind + span.
            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let full_name = if let Some(pfx) = prefix_node {
                        // resource / data → combine "type.name" for uniqueness.
                        if let Ok(pfx_str) =
                            std::str::from_utf8(&source[pfx.start_byte()..pfx.end_byte()])
                        {
                            format!("{}.{}", pfx_str, name_str)
                        } else {
                            name_str.to_string()
                        }
                    } else {
                        name_str.to_string()
                    };

                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
                        decorators: vec![],
                        // Only `output` blocks are the module's public interface.
                        is_exported: is_output_block,
                        heritage: vec![],
                        type_annotation: None,
                        name: full_name,
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

            // Push an import edge for module source attributes.
            if let Some(i_src) = import_src {
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

        // HCL has function_call nodes — wire up call extraction.
        extract_calls(tree.root_node(), source, &mut nodes, &["function_call"]);

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
