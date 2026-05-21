use super::spec::MoveSpec;
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
        let language = tree_sitter_move::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct MoveProvider {
    query: Query,
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

impl MoveProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_move::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| MoveSpec::CAPTURE_KIND.get(name).copied())
            .collect();
        Ok(Self {
            query,
            capture_kind_by_idx,
        })
    }
}

impl LanguageProvider for MoveProvider {
    fn name(&self) -> &'static str {
        "move"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        // Root-span anchors (kind dispatch is via spec table)
        let idx_class = self.query.capture_index_for_name("class");
        let idx_function = self.query.capture_index_for_name("function");
        let idx_struct = self.query.capture_index_for_name("struct");
        let idx_const = self.query.capture_index_for_name("const");
        let idx_typedef = self.query.capture_index_for_name("typedef");

        // Metadata captures
        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        // Functions carry `public`/`public(friend)`/`public(package)`/`entry`
        // as a named `modifier` child. Structs have no `modifier` child in the
        // grammar — fall back to a source-text prefix scan via the helper.
        use crate::framework_helpers::node_source_starts_with;
        let has_modifier_child = |node: tree_sitter::Node| -> bool {
            // `cursor` must outlive the iterator returned by `named_children`,
            // so the result has to land in a binding before the closure ends —
            // otherwise the cursor is dropped while still borrowed (E0597).
            let mut cursor = node.walk();
            let has = node
                .named_children(&mut cursor)
                .any(|child| child.kind() == "modifier");
            has
        };

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut is_struct_root = false;
            let mut import_name = None;
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
                } else if [idx_class, idx_function, idx_struct, idx_const, idx_typedef]
                    .contains(&Some(cap_idx))
                {
                    root_span_node = Some(cap.node);
                    if Some(cap_idx) == idx_struct {
                        is_struct_root = true;
                    }
                } else if Some(cap_idx) == idx_import_name {
                    import_name = Some(cap.node);
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    let is_exported = if is_struct_root {
                        node_source_starts_with(source, root, b"public ")
                    } else {
                        has_modifier_child(root)
                    };
                    nodes.push(RawNode {
                        name: name_str.to_string(),
                        kind: k,
                        span: (
                            start.row as u32,
                            start.column as u32,
                            end.row as u32,
                            end.column as u32,
                        ),
                        is_exported,
                        heritage: Vec::new(),
                        type_annotation: None,
                        decorators: Vec::new(),
                        calls: Vec::new(),
                    });
                }
            }

            if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
                if let (Ok(name_str), Ok(src_str)) = (
                    std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]),
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    imports.push(RawImport {
                        imported_name: name_str.to_string(),
                        source: src_str.to_string(),
                        alias: None,
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
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
        })
    }
}
