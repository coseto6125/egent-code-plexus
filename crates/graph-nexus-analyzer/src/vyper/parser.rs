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
        let language = tree_sitter_vyper::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct VyperProvider {
    query: Query,
}

impl VyperProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_vyper::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for VyperProvider {
    fn name(&self) -> &'static str {
        "vyper"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();

        // Name captures
        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_const_name = self.query.capture_index_for_name("const.name");

        // Span captures
        let idx_function = self.query.capture_index_for_name("function");
        let idx_const = self.query.capture_index_for_name("const");

        // Decorator and import captures
        let idx_decorator = self.query.capture_index_for_name("decorator");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut decorators: Vec<String> = Vec::new();
            let mut import_src: Option<tree_sitter::Node> = None;

            for cap in m.captures {
                let ci = cap.index;
                if Some(ci) == idx_function_name {
                    name_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Function);
                    }
                } else if Some(ci) == idx_const_name {
                    name_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(NodeKind::Const);
                    }
                } else if Some(ci) == idx_function || Some(ci) == idx_const {
                    root_span_node = Some(cap.node);
                } else if Some(ci) == idx_decorator {
                    if let Ok(d) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d.to_string());
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
                        nodes.push(RawNode {
                            decorators,
                            is_exported: true,
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
