use crate::calls::extract_calls;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_lua::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct LuaProvider {
    query: Query,
}

impl LuaProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_lua::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for LuaProvider {
    fn name(&self) -> &'static str {
        "lua"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();
        // Map from (start_row, start_col) to node index in `nodes`.
        // Used to let the @struct/Class pattern override a prior @const match for the same AST node
        // (both patterns fire on table assignments like `local T = {}`).
        let mut span_to_node_idx = std::collections::HashMap::<(u32, u32), usize>::new();

        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_struct_name = self.query.capture_index_for_name("struct.name");
        let idx_const_name = self.query.capture_index_for_name("const.name");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_struct = self.query.capture_index_for_name("struct");
        let idx_const = self.query.capture_index_for_name("const");
        let idx_import = self.query.capture_index_for_name("import");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut import_src = None;
            let mut is_struct = false;

            for cap in m.captures {
                let cap_idx = cap.index;
                if Some(cap_idx) == idx_function_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if Some(cap_idx) == idx_struct_name {
                    name_node = Some(cap.node);
                    is_struct = true;
                } else if Some(cap_idx) == idx_const_name {
                    // Only set if we don't already have a more specific kind
                    if name_node.is_none() {
                        name_node = Some(cap.node);
                        kind = Some(NodeKind::Const);
                    }
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_struct
                    || Some(cap_idx) == idx_const
                    || Some(cap_idx) == idx_import
                {
                    root_span_node = Some(cap.node);
                }
            }

            // Apply PascalCase heuristic for table-as-class (struct captures)
            if is_struct {
                if let Some(n) = name_node {
                    if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()])
                    {
                        if is_pascal_case(name_str) {
                            kind = Some(NodeKind::Class);
                        } else {
                            // Not PascalCase — treat as plain const
                            kind = Some(NodeKind::Const);
                        }
                    }
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();

                    let span_key = (start.row as u32, start.column as u32);
                    // Deduplicate: @struct and @const patterns both fire on `local T = {}`.
                    // If we already saw a node at this span and the new kind has higher priority
                    // (Class > Function > Const), upgrade the existing node's kind in place.
                    if let Some(&existing_idx) = span_to_node_idx.get(&span_key) {
                        let existing_kind = &nodes[existing_idx].kind;
                        let new_has_priority = matches!(k, NodeKind::Class | NodeKind::Function)
                            && matches!(existing_kind, NodeKind::Const);
                        if new_has_priority {
                            nodes[existing_idx].kind = k;
                        }
                        continue;
                    }
                    span_to_node_idx.insert(span_key, nodes.len());

                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: true,
                        heritage: vec![],
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

            if let Some(i_src) = import_src {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
                {
                    imports.push(RawImport {
                        alias: None,
                        imported_name: "*".to_string(),
                        source: src_str.to_string(),
                    });
                }
            }
        }

        // Extract call sites and attach to enclosing function/method nodes.
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

/// Returns true if the string starts with an uppercase letter followed by at least one lowercase.
/// This is a lightweight PascalCase check: "MyClass" → true, "MY_CONST" → false, "foo" → false.
fn is_pascal_case(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) if first.is_uppercase() => chars.any(|c| c.is_lowercase()),
        _ => false,
    }
}
