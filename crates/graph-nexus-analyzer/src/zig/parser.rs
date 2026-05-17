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
        let language = tree_sitter_zig::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct ZigProvider {
    query: Query,
}

impl ZigProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_zig::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

/// Priority of a match for the same variable_declaration byte range.
/// Higher value wins.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum MatchPriority {
    Const = 0,
    Struct = 1,
    Import = 2,
    Function = 3,
}

struct PendingNode {
    priority: MatchPriority,
    name: String,
    kind: NodeKind,
    span: (u32, u32, u32, u32),
}

struct PendingImport {
    source: String,
}

impl LanguageProvider for ZigProvider {
    fn name(&self) -> &'static str {
        "zig"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let idx_import = self.query.capture_index_for_name("import");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_function = self.query.capture_index_for_name("function");
        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_struct = self.query.capture_index_for_name("struct");
        let idx_struct_name = self.query.capture_index_for_name("struct.name");
        let idx_const = self.query.capture_index_for_name("const");
        let idx_const_name = self.query.capture_index_for_name("const.name");

        // Collect all matches first, keyed by root node start byte.
        // Multiple patterns may match the same variable_declaration (e.g. struct
        // is also captured by the const fallback). We keep only the highest-priority
        // match per declaration site.
        let mut node_map: HashMap<usize, PendingNode> = HashMap::new();
        let mut import_map: HashMap<usize, PendingImport> = HashMap::new();

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind: Option<NodeKind> = None;
            let mut priority = MatchPriority::Const;
            let mut root_span_node = None;
            let mut root_start_byte = 0usize;
            let mut import_src = None;
            let mut is_import = false;

            for cap in m.captures {
                let cap_idx = cap.index;
                if Some(cap_idx) == idx_function_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                    priority = MatchPriority::Function;
                } else if Some(cap_idx) == idx_struct_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                    priority = MatchPriority::Struct;
                } else if Some(cap_idx) == idx_const_name {
                    if kind.is_none() {
                        name_node = Some(cap.node);
                        kind = Some(NodeKind::Const);
                        priority = MatchPriority::Const;
                    }
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                    is_import = true;
                    priority = MatchPriority::Import;
                } else if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_struct
                    || Some(cap_idx) == idx_import
                    || Some(cap_idx) == idx_const
                {
                    root_span_node = Some(cap.node);
                    root_start_byte = cap.node.start_byte();
                }
            }

            if is_import {
                if let Some(i_src) = import_src {
                    if let Ok(src_str) =
                        std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
                    {
                        import_map.insert(
                            root_start_byte,
                            PendingImport {
                                source: src_str.to_string(),
                            },
                        );
                    }
                }
                continue;
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    let candidate = PendingNode {
                        priority,
                        name: name_str.to_string(),
                        kind: k,
                        span: (
                            start.row as u32,
                            start.column as u32,
                            end.row as u32,
                            end.column as u32,
                        ),
                    };
                    // Keep only the highest-priority match for each declaration site
                    let existing = node_map
                        .entry(root_start_byte)
                        .or_insert_with(|| PendingNode {
                            priority: MatchPriority::Const,
                            name: String::new(),
                            kind: NodeKind::Const,
                            span: (0, 0, 0, 0),
                        });
                    if candidate.priority >= existing.priority {
                        *existing = candidate;
                    }
                }
            }
        }

        // Also remove const entries that have an import at the same start byte
        // (the @import pattern fires independently from @const)
        for key in import_map.keys() {
            node_map.remove(key);
        }

        // See java/parser.rs node ordering note — HashMap iteration drift
        // surfaces as Calls-edge run-to-run variance via Pass 1 last-write-
        // wins on (file_path, name). Pin canonical source-span order.
        let mut nodes: Vec<RawNode> = node_map
            .into_values()
            .filter(|n| !n.name.is_empty())
            .map(|n| RawNode {
                decorators: vec![],
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                name: n.name,
                kind: n.kind,
                span: n.span,
                calls: Vec::new(),
            })
            .collect();
        nodes.sort_by_key(|n| n.span);

        let imports: Vec<RawImport> = import_map
            .into_values()
            .map(|i| RawImport {
                alias: None,
                imported_name: "*".to_string(),
                source: i.source,
                binding_kind: None,
            })
            .collect();

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
