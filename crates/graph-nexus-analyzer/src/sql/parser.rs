// Grammar: tree-sitter-sequel v0.3.11 (PostgreSQL/ANSI dialect)
// Dialect notes:
//   - Supports: CREATE TABLE, CREATE FUNCTION, CREATE VIEW
//   - NOT supported: CREATE PROCEDURE (parses as ERROR node in this grammar)
//   - Column field names captured via `name:` field selector to avoid matching
//     identifiers inside REFERENCES(...) clauses
//   - Foreign key REFERENCES become import-style edges
use crate::calls::extract_calls;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::collections::HashMap;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_sequel::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct SqlProvider {
    query: Query,
}

impl SqlProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_sequel::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for SqlProvider {
    fn name(&self) -> &'static str {
        "sql"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();
        // Side-map: referencing-table-name -> referenced-table-names.
        // Populated by the foreign-key patterns in `queries.scm` and merged
        // into each table's `RawNode.heritage` after the main loop.
        let mut fk_heritage: HashMap<String, Vec<String>> = HashMap::new();

        let idx_class_name = self.query.capture_index_for_name("class.name");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_function = self.query.capture_index_for_name("function");
        let idx_const_name = self.query.capture_index_for_name("const.name");
        let idx_const = self.query.capture_index_for_name("const");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_heritage_table = self.query.capture_index_for_name("heritage.table");
        let idx_heritage_target = self.query.capture_index_for_name("heritage.target");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut import_src = None;
            let mut fk_table_node = None;
            let mut fk_target_node = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if Some(cap_idx) == idx_class_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if Some(cap_idx) == idx_function_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if Some(cap_idx) == idx_const_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Const);
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_heritage_table {
                    fk_table_node = Some(cap.node);
                } else if Some(cap_idx) == idx_heritage_target {
                    fk_target_node = Some(cap.node);
                } else if Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_const
                {
                    root_span_node = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
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

            // Foreign-key pair: referencing table -> referenced table.
            if let (Some(t), Some(tgt)) = (fk_table_node, fk_target_node) {
                if let (Ok(table_str), Ok(target_str)) = (
                    std::str::from_utf8(&source[t.start_byte()..t.end_byte()]),
                    std::str::from_utf8(&source[tgt.start_byte()..tgt.end_byte()]),
                ) {
                    let entry = fk_heritage.entry(table_str.to_string()).or_default();
                    let target_owned = target_str.to_string();
                    if !entry.contains(&target_owned) {
                        entry.push(target_owned);
                    }
                }
            }
        }

        // Merge collected FK targets into the matching table's heritage list.
        // Tables are emitted as `NodeKind::Class` by the `class` query patterns
        // above (see CREATE TABLE / CREATE VIEW). We only attach heritage to
        // class-kind nodes to avoid colliding with same-named columns/funcs.
        if !fk_heritage.is_empty() {
            for n in nodes.iter_mut() {
                if !matches!(n.kind, NodeKind::Class) {
                    continue;
                }
                if let Some(targets) = fk_heritage.get(&n.name) {
                    for tgt in targets {
                        if !n.heritage.contains(tgt) {
                            n.heritage.push(tgt.clone());
                        }
                    }
                }
            }
        }

        // SQL function calls appear as `invocation` nodes inside function bodies.
        extract_calls(tree.root_node(), source, &mut nodes, &["invocation"]);

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
