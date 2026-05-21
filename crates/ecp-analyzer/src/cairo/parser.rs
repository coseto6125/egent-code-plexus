use super::spec::CairoSpec;
use crate::calls::extract_calls;
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_cairo::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct CairoProvider {
    query: Query,
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

impl CairoProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_cairo::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| CairoSpec::CAPTURE_KIND.get(name).copied())
            .collect();
        Ok(Self {
            query,
            capture_kind_by_idx,
        })
    }
}

impl LanguageProvider for CairoProvider {
    fn name(&self) -> &'static str {
        "cairo"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        // Metadata-only captures (kind dispatch is via spec table below)
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_heritage = self.query.capture_index_for_name("heritage");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_struct = self.query.capture_index_for_name("struct");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_const = self.query.capture_index_for_name("const");
        let idx_typedef = self.query.capture_index_for_name("typedef");
        let idx_import = self.query.capture_index_for_name("import");

        // tree-sitter-cairo v0.0.1 strips `pub` from the grammar entirely —
        // detect it via a backward source-text scan instead.
        use crate::framework_helpers::source_before_node_ends_with;

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut heritage = Vec::new();
            let mut import_src = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap_idx as usize)
                    .copied()
                    .flatten()
                {
                    // struct/class both map to Class via the spec table.
                    name_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(k_from_spec);
                    }
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.to_string());
                    }
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_struct
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_const
                    || Some(cap_idx) == idx_typedef
                    || Some(cap_idx) == idx_import
                {
                    root_span_node = Some(cap.node);
                }
            }

            // Emit symbol node.
            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: source_before_node_ends_with(source, root, b"pub"),
                        heritage,
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

            // Emit import record.
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

        extract_calls(tree.root_node(), source, &mut nodes, &["call_expression"]);

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
