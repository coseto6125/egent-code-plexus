use super::spec::LuaSpec;
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
        let language = tree_sitter_lua::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct LuaProvider {
    query: Query,
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

impl LuaProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_lua::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;

        let capture_names = query.capture_names();
        let capture_kind_by_idx: Vec<Option<NodeKind>> = capture_names
            .iter()
            .map(|name| LuaSpec::CAPTURE_KIND.get(name).copied())
            .collect();

        Ok(Self {
            query,
            capture_kind_by_idx,
        })
    }
}

impl LanguageProvider for LuaProvider {
    fn name(&self) -> &'static str {
        "lua"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();
        // Map from (start_row, start_col) to node index in `nodes`.
        // Used to let the @struct/Class pattern override a prior @const match for the same AST node
        // (both patterns fire on table assignments like `local T = {}`).
        let mut span_to_node_idx = std::collections::HashMap::<(u32, u32), usize>::new();
        // Names of tables that were promoted to Class kind. Used to attach
        // `setmetatable(child, {__index = parent})` heritage updates only when
        // the child is actually a known class node in this file.
        let mut class_name_to_idx = std::collections::HashMap::<String, usize>::new();
        // Spans of `function_call` nodes already accounted for by the aliased
        // require pattern, so the bare-require pattern doesn't emit a duplicate.
        let mut require_inner_spans = std::collections::HashSet::<(u32, u32, u32, u32)>::new();
        // Deferred metatable inheritance edges (child_name, parent_name).
        // Processed after the main loop so the child node's index is known
        // regardless of capture ordering.
        let mut pending_meta: Vec<(String, String)> = Vec::new();

        // Metadata-only captures
        let idx_function_table = self.query.capture_index_for_name("function.table");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");
        let idx_import_alias_source = self.query.capture_index_for_name("import.alias.source");
        let idx_import_inner = self.query.capture_index_for_name("import.inner");
        let idx_meta_child = self.query.capture_index_for_name("meta.child");
        let idx_meta_parent = self.query.capture_index_for_name("meta.parent");

        // Name captures handled via spec
        let idx_struct_name = self.query.capture_index_for_name("struct.name");

        // Span captures
        let idx_function = self.query.capture_index_for_name("function");
        let idx_struct = self.query.capture_index_for_name("struct");
        let idx_const = self.query.capture_index_for_name("const");
        let idx_typedef = self.query.capture_index_for_name("typedef");
        let idx_import = self.query.capture_index_for_name("import");
        let idx_import_aliased = self.query.capture_index_for_name("import.aliased");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut function_table_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut import_src = None;
            let mut import_alias_node = None;
            let mut import_inner_span: Option<(u32, u32, u32, u32)> = None;
            let mut is_struct = false;
            let mut meta_child_node = None;
            let mut meta_parent_node = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap_idx as usize)
                    .copied()
                    .flatten()
                {
                    // Spec-based dispatch for function.name, struct.name, const.name
                    name_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(k_from_spec);
                    }
                    if Some(cap_idx) == idx_struct_name {
                        is_struct = true;
                    }
                } else if Some(cap_idx) == idx_function_table {
                    function_table_node = Some(cap.node);
                } else if Some(cap_idx) == idx_import_source
                    || Some(cap_idx) == idx_import_alias_source
                {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_import_alias {
                    import_alias_node = Some(cap.node);
                } else if Some(cap_idx) == idx_import_inner {
                    let s = cap.node.start_position();
                    let e = cap.node.end_position();
                    import_inner_span =
                        Some((s.row as u32, s.column as u32, e.row as u32, e.column as u32));
                } else if Some(cap_idx) == idx_meta_child {
                    meta_child_node = Some(cap.node);
                } else if Some(cap_idx) == idx_meta_parent {
                    meta_parent_node = Some(cap.node);
                } else if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_struct
                    || Some(cap_idx) == idx_const
                    || Some(cap_idx) == idx_typedef
                    || Some(cap_idx) == idx_import
                    || Some(cap_idx) == idx_import_aliased
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

            // Method via `M.foo = function() end` — emit a Method node whose
            // name is the field; the table name is currently dropped (no
            // explicit Method↔Class binding pass exists for Lua yet), but the
            // method itself becomes addressable in the call graph.
            let is_table_assigned_method =
                function_table_node.is_some() && matches!(kind, Some(NodeKind::Function));
            if is_table_assigned_method {
                kind = Some(NodeKind::Method);
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();

                    // tree-sitter-lua 0.5.0 aliases `local function foo()` as
                    // function_declaration, indistinguishable from the global form
                    // by node type alone. Source-text scan is the only signal.
                    let is_local_fn = matches!(k, NodeKind::Function)
                        && crate::framework_helpers::node_source_starts_with(
                            source, root, b"local",
                        );

                    let span_key = (start.row as u32, start.column as u32);
                    // Deduplicate: @struct and @const patterns both fire on `local T = {}`.
                    // If we already saw a node at this span and the new kind has higher priority
                    // (Class > Function > Const), upgrade the existing node's kind in place.
                    if let Some(&existing_idx) = span_to_node_idx.get(&span_key) {
                        let existing_kind = &nodes[existing_idx].kind;
                        let new_has_priority =
                            matches!(k, NodeKind::Class | NodeKind::Function | NodeKind::Typedef)
                                && matches!(existing_kind, NodeKind::Const);
                        if new_has_priority {
                            nodes[existing_idx].kind = k;
                            if matches!(k, NodeKind::Class) {
                                class_name_to_idx.insert(name_str.to_string(), existing_idx);
                            }
                        }
                        continue;
                    }
                    span_to_node_idx.insert(span_key, nodes.len());
                    if matches!(k, NodeKind::Class) {
                        class_name_to_idx.insert(name_str.to_string(), nodes.len());
                    }

                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: !is_local_fn,
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

            // Imports: aliased form takes precedence — record the inner
            // function_call span so the bare-require pattern can dedupe.
            if let (Some(i_src), Some(alias_node)) = (import_src, import_alias_node) {
                if let (Ok(src_str), Ok(alias_str)) = (
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                    std::str::from_utf8(&source[alias_node.start_byte()..alias_node.end_byte()]),
                ) {
                    imports.push(RawImport {
                        alias: Some(alias_str.to_string()),
                        imported_name: "*".to_string(),
                        source: src_str.to_string(),
                        binding_kind: None,
                    });
                    if let Some(span) = import_inner_span {
                        require_inner_spans.insert(span);
                    }
                }
            } else if let Some(i_src) = import_src {
                // Bare require — dedupe against any aliased require we already
                // emitted for the same `function_call` AST node.
                let req_span = root_span_node.map(|r| {
                    let s = r.start_position();
                    let e = r.end_position();
                    (s.row as u32, s.column as u32, e.row as u32, e.column as u32)
                });
                let is_dup = req_span
                    .map(|sp| require_inner_spans.contains(&sp))
                    .unwrap_or(false);
                if !is_dup {
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

            // Metatable inheritance: defer to a second pass so the child
            // class node (which may have been declared in any earlier match)
            // is reliably present in `class_name_to_idx`.
            if let (Some(child), Some(parent)) = (meta_child_node, meta_parent_node) {
                if let (Ok(child_str), Ok(parent_str)) = (
                    std::str::from_utf8(&source[child.start_byte()..child.end_byte()]),
                    std::str::from_utf8(&source[parent.start_byte()..parent.end_byte()]),
                ) {
                    pending_meta.push((child_str.to_string(), parent_str.to_string()));
                }
            }
        }

        // Apply metatable-inheritance edges. If the child isn't a known class
        // (e.g. `setmetatable({}, {__index = X})` on a literal table), skip.
        for (child, parent) in pending_meta {
            if let Some(&idx) = class_name_to_idx.get(&child) {
                if !nodes[idx].heritage.iter().any(|h| h == &parent) {
                    nodes[idx].heritage.push(parent);
                }
            }
        }

        // Extract call sites and attach to enclosing function/method nodes.
        extract_calls(tree.root_node(), source, &mut nodes, &["function_call"]);

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
