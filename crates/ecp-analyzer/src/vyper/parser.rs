use super::spec::VyperSpec;
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
        let language = tree_sitter_vyper::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}

/// Extract the alias identifier from `X as Y` or `X as Y  # comment`.
/// Returns `Some("Y")` when an `as` clause is present, `None` otherwise.
fn extract_as_alias(s: &str) -> Option<&str> {
    // Find ` as ` (with surrounding spaces) to distinguish from names
    // that happen to contain "as" (e.g. "class", "base").
    let as_pos = s.find(" as ")?;
    // Strip inline comment first — works whether or not there's a space
    // before `#` (`as m # c` and `as m#c` both yield "m").
    let before_comment = s[as_pos + 4..].split('#').next().unwrap_or("");
    let alias = before_comment.split_whitespace().next()?;
    if alias.is_empty() {
        None
    } else {
        Some(alias)
    }
}

pub struct VyperProvider {
    query: Query,
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

impl VyperProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_vyper::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| VyperSpec::CAPTURE_KIND.get(name).copied())
            .collect();
        Ok(Self {
            query,
            capture_kind_by_idx,
        })
    }
}

impl LanguageProvider for VyperProvider {
    fn name(&self) -> &'static str {
        "vyper"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();

        // Span captures (root-span anchors — kind dispatch is via spec table below)
        let idx_function = self.query.capture_index_for_name("function");
        let idx_const = self.query.capture_index_for_name("const");

        // Metadata-only captures (attach attributes; not NodeKind-producing)
        let idx_import_source = self.query.capture_index_for_name("import.source");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut decorators: Vec<String> = Vec::new();
            let mut import_src: Option<tree_sitter::Node> = None;

            for cap in m.captures {
                let ci = cap.index;
                if let Some(k_from_spec) =
                    self.capture_kind_by_idx.get(ci as usize).copied().flatten()
                {
                    name_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(k_from_spec);
                    }
                } else if Some(ci) == idx_function || Some(ci) == idx_const {
                    root_span_node = Some(cap.node);
                    // Decorator captures arrive in separate match iterations
                    // (tree-sitter pattern boundaries), so walk children here
                    // instead of relying on a co-emitted @decorator capture.
                    if Some(ci) == idx_function {
                        let mut walker = cap.node.walk();
                        for child in cap.node.children(&mut walker) {
                            if child.kind() != "decorator" {
                                continue;
                            }
                            let mut dwalker = child.walk();
                            for grandchild in child.children(&mut dwalker) {
                                if grandchild.kind() != "identifier" {
                                    continue;
                                }
                                if let Ok(d) = std::str::from_utf8(
                                    &source[grandchild.start_byte()..grandchild.end_byte()],
                                ) {
                                    decorators.push(d.to_string());
                                }
                            }
                        }
                    }
                } else if Some(ci) == idx_import_source {
                    import_src = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    // Deduplicate by span (multiple query patterns can match the same node)
                    let span = (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    );
                    if let Some(existing) = nodes.iter_mut().find(|node| node.span == span) {
                        for d in &decorators {
                            if !existing.decorators.contains(d) {
                                existing.decorators.push(d.clone());
                            }
                        }
                    } else {
                        let is_exported = decorators
                            .iter()
                            .any(|d| matches!(d.trim(), "external" | "view" | "payable"));
                        nodes.push(RawNode {
                            decorators,
                            is_exported,
                            heritage: vec![],
                            type_annotation: None,
                            name: name_str.to_string(),
                            kind: k,
                            span,
                            calls: Vec::new(),
                        });
                    }
                }
            }

            if let Some(src_node) = import_src {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[src_node.start_byte()..src_node.end_byte()])
                {
                    imports.push(RawImport {
                        alias: None,
                        imported_name: src_str.to_string(),
                        source: src_str.to_string(),
                        binding_kind: None,
                    });
                }
            }
        }

        // Vyper's grammar is minimal and does not parse `import X as Y` or
        // `from X import Y as Z` — those produce ERROR nodes.  Detect aliases
        // by scanning source text lines directly.
        //
        // Patterns handled:
        //   import math as m              → alias "m"
        //   from vyper.interfaces import ERC20 as Token  → alias "Token"
        for (row, line) in std::str::from_utf8(source)
            .unwrap_or("")
            .lines()
            .enumerate()
        {
            let trimmed = line.trim();
            // `import X as Y`
            if let Some(rest) = trimmed.strip_prefix("import ") {
                if let Some(alias) = extract_as_alias(rest) {
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: false,
                        heritage: vec![],
                        type_annotation: None,
                        name: alias.to_string(),
                        kind: NodeKind::Typedef,
                        span: (row as u32, 0, row as u32, line.len() as u32),
                        calls: Vec::new(),
                    });
                }
            }
            // `from X import Y as Z`
            if let Some(rest) = trimmed.strip_prefix("from ") {
                if let Some(after_import) = rest.find(" import ").map(|i| &rest[i + 8..]) {
                    if let Some(alias) = extract_as_alias(after_import) {
                        nodes.push(RawNode {
                            decorators: vec![],
                            is_exported: false,
                            heritage: vec![],
                            type_annotation: None,
                            name: alias.to_string(),
                            kind: NodeKind::Typedef,
                            span: (row as u32, 0, row as u32, line.len() as u32),
                            calls: Vec::new(),
                        });
                    }
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
