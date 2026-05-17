use crate::calls::extract_calls;
use super::spec::ZigSpec;
use graph_nexus_core::analyzer::lang_spec::LangSpec;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
use rustc_hash::FxHashMap;
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
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

impl ZigProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_zig::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;

        let capture_names = query.capture_names();
        let capture_kind_by_idx: Vec<Option<NodeKind>> = capture_names
            .iter()
            .map(|name| ZigSpec::CAPTURE_KIND.get(name).copied())
            .collect();

        Ok(Self { query, capture_kind_by_idx })
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
        let idx_struct = self.query.capture_index_for_name("struct");
        let idx_const = self.query.capture_index_for_name("const");

        // Collect all matches first, keyed by root node start byte.
        // Multiple patterns may match the same variable_declaration (e.g. struct
        // is also captured by the const fallback). We keep only the highest-
        // priority match per declaration site.
        // Vec + idx-map pattern (see java/parser.rs same-site note) — `nodes` /
        // `imports` Vecs are populated in tree-sitter match order, so per-file
        // output is deterministic without a downstream sort.
        let mut pending_nodes: Vec<PendingNode> = Vec::new();
        let mut node_idx_by_key: FxHashMap<usize, usize> = FxHashMap::default();
        let mut pending_imports: Vec<PendingImport> = Vec::new();
        let mut import_idx_by_key: FxHashMap<usize, usize> = FxHashMap::default();

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind: Option<NodeKind> = None;
            let mut priority = MatchPriority::Const;
            let mut root_span_node = None;
            let mut root_start_byte = 0usize;
            let mut import_src = None;
            let mut is_import = false;

            for cap in m.captures {
                let cap_idx = cap.index as usize;
                if let Some(k) = self.capture_kind_by_idx.get(cap_idx).and_then(|opt| *opt) {
                    // This is a .name capture with a NodeKind
                    if kind.is_none() {
                        name_node = Some(cap.node);
                        kind = Some(k);
                        priority = match k {
                            NodeKind::Function => MatchPriority::Function,
                            NodeKind::Class => MatchPriority::Struct,
                            NodeKind::Const => MatchPriority::Const,
                            _ => MatchPriority::Const,
                        };
                    }
                } else if Some(cap_idx as u32) == idx_import_source {
                    import_src = Some(cap.node);
                    is_import = true;
                    priority = MatchPriority::Import;
                } else if Some(cap_idx as u32) == idx_function
                    || Some(cap_idx as u32) == idx_struct
                    || Some(cap_idx as u32) == idx_import
                    || Some(cap_idx as u32) == idx_const
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
                        let import = PendingImport { source: src_str.to_string() };
                        if let Some(&i) = import_idx_by_key.get(&root_start_byte) {
                            pending_imports[i] = import;
                        } else {
                            let i = pending_imports.len();
                            pending_imports.push(import);
                            import_idx_by_key.insert(root_start_byte, i);
                        }
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
                    // Keep only the highest-priority match per declaration site.
                    if let Some(&i) = node_idx_by_key.get(&root_start_byte) {
                        if candidate.priority >= pending_nodes[i].priority {
                            pending_nodes[i] = candidate;
                        }
                    } else {
                        let i = pending_nodes.len();
                        pending_nodes.push(candidate);
                        node_idx_by_key.insert(root_start_byte, i);
                    }
                }
            }
        }

        // Suppress const entries that have an import at the same start byte
        // (the @import pattern fires independently from @const). Mark with
        // empty name so the filter below drops them — order-preserving vs the
        // earlier `node_map.remove(key)` set-semantic operation.
        for key in import_idx_by_key.keys() {
            if let Some(&i) = node_idx_by_key.get(key) {
                pending_nodes[i].name.clear();
            }
        }

        let mut nodes: Vec<RawNode> = pending_nodes
            .into_iter()
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

        let imports: Vec<RawImport> = pending_imports
            .into_iter()
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
