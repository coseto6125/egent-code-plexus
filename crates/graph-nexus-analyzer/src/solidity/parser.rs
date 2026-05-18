use crate::calls::extract_calls;
use super::spec::SoliditySpec;
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
        let language = tree_sitter_solidity::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct SolidityProvider {
    query: Query,
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

impl SolidityProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_solidity::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;

        let capture_names = query.capture_names();
        let capture_kind_by_idx: Vec<Option<NodeKind>> = capture_names
            .iter()
            .map(|name| SoliditySpec::CAPTURE_KIND.get(name).copied())
            .collect();

        Ok(Self { query, capture_kind_by_idx })
    }
}

impl LanguageProvider for SolidityProvider {
    fn name(&self) -> &'static str {
        "solidity"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        // Span captures
        let idx_class = self.query.capture_index_for_name("class");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_function = self.query.capture_index_for_name("function");
        let idx_const = self.query.capture_index_for_name("const");
        let idx_typedef = self.query.capture_index_for_name("typedef");
        let idx_state_var = self.query.capture_index_for_name("state_var");

        // Name captures that need special state tracking
        let idx_state_var_name = self.query.capture_index_for_name("state_var.name");

        // State variable visibility
        let idx_state_var_visibility = self.query.capture_index_for_name("state_var.visibility");

        // Import / heritage
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_heritage = self.query.capture_index_for_name("heritage");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut heritage = Vec::new();
            let mut import_src = None;
            let mut is_state_var = false;
            let mut state_var_visibility: Option<&[u8]> = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap_idx as usize)
                    .copied()
                    .flatten()
                {
                    name_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(k_from_spec);
                    }
                    if Some(cap_idx) == idx_state_var_name {
                        is_state_var = true;
                    }
                } else if Some(cap_idx) == idx_class {
                    root_span_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Class);
                    }
                } else if Some(cap_idx) == idx_method {
                    root_span_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Method);
                    }
                } else if Some(cap_idx) == idx_function {
                    root_span_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Function);
                    }
                } else if Some(cap_idx) == idx_const {
                    root_span_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Const);
                    }
                } else if Some(cap_idx) == idx_typedef {
                    root_span_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Typedef);
                    }
                } else if Some(cap_idx) == idx_state_var {
                    root_span_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Const);
                    }
                    is_state_var = true;
                } else if Some(cap_idx) == idx_state_var_visibility {
                    state_var_visibility =
                        Some(&source[cap.node.start_byte()..cap.node.end_byte()]);
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.to_string());
                    }
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    // State-variable visibility: `public`/`external` are exported;
                    // `private`/`internal` and the implicit default (no keyword) are not.
                    // Solidity's default state-variable visibility is `internal`.
                    let is_exported = if is_state_var {
                        matches!(state_var_visibility, Some(b"public") | Some(b"external"))
                    } else {
                        true
                    };
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported,
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

            if let Some(src_node) = import_src {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[src_node.start_byte()..src_node.end_byte()])
                {
                    // Strip surrounding quotes from the string literal
                    let trimmed = src_str.trim_matches('"').trim_matches('\'');
                    imports.push(RawImport {
                        alias: None,
                        imported_name: "*".to_string(),
                        source: trimmed.to_string(),
                        binding_kind: None,
                    });
                }
            }
        }

        extract_calls(tree.root_node(), source, &mut nodes, &["call_expression"]);

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
