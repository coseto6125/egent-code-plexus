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
        let language = tree_sitter_c_sharp::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct CSharpProvider {
    query: Query,
}

impl CSharpProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_c_sharp::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for CSharpProvider {
    fn name(&self) -> &'static str {
        "c_sharp"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        use std::collections::HashMap;
        let mut node_map: HashMap<usize, RawNode> = HashMap::new();
        let mut imports = Vec::new();

        let idx_name_function = self.query.capture_index_for_name("name.function");
        let idx_name_class = self.query.capture_index_for_name("name.class");
        let idx_name_method = self.query.capture_index_for_name("name.method");
        let idx_name_constructor = self.query.capture_index_for_name("name.constructor");
        let idx_name_interface = self.query.capture_index_for_name("name.interface");
        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");

        let idx_export = self.query.capture_index_for_name("export");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");
        let idx_decorator = self.query.capture_index_for_name("decorator");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_constructor = self.query.capture_index_for_name("constructor");
        let idx_interface = self.query.capture_index_for_name("interface");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;

            let mut import_name = None;
            let mut import_src = None;
            let mut import_alias = None;

            let mut is_exported = false;
            let mut heritage_list = Vec::new();
            let mut type_annotation = None;
            let mut decorators = Vec::new();

            for cap in m.captures {
                let cap_idx = cap.index;
                if Some(cap_idx) == idx_name_function {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if Some(cap_idx) == idx_name_class {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if Some(cap_idx) == idx_name_method {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
                } else if Some(cap_idx) == idx_name_constructor {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Constructor);
                } else if Some(cap_idx) == idx_name_interface {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Interface);
                } else if Some(cap_idx) == idx_import_name {
                    import_name = Some(cap.node);
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_import_alias {
                    if let Ok(text) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        import_alias = Some(text.to_string());
                    }
                } else if Some(cap_idx) == idx_export {
                    if let Ok(text) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        if text == "public" {
                            is_exported = true;
                        }
                    }
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(text) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage_list.push(text.to_string());
                    }
                } else if Some(cap_idx) == idx_type {
                    if let Ok(text) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        type_annotation = Some(text.to_string());
                    }
                } else if Some(cap_idx) == idx_decorator {
                    if let Ok(text) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(text.to_string());
                    }
                } else if (Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_method
                    || Some(cap_idx) == idx_constructor
                    || Some(cap_idx) == idx_interface)
                    && root_span_node.is_none()
                {
                    root_span_node = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();

                    let node_id = root.id();
                    let entry = node_map.entry(node_id).or_insert_with(|| RawNode {
                        decorators: vec![],
                        is_exported,
                        heritage: Vec::new(),
                        type_annotation: type_annotation.clone(),
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

                    if is_exported {
                        entry.is_exported = true;
                    }
                    if type_annotation.is_some() {
                        entry.type_annotation = type_annotation;
                    }
                    for h in heritage_list {
                        if !entry.heritage.contains(&h) {
                            entry.heritage.push(h);
                        }
                    }
                    for d in decorators {
                        if !entry.decorators.contains(&d) {
                            entry.decorators.push(d);
                        }
                    }
                }
            }

            if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
                if let (Ok(name_str), Ok(src_str)) = (
                    std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]),
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    imports.push(RawImport {
                        alias: import_alias,
                        imported_name: name_str.to_string(),
                        source: src_str.to_string(),
                    });
                }
            }
        }

        let mut nodes: Vec<RawNode> = node_map.into_values().collect();

        // Extract call sites and attach to enclosing function/method nodes.
        extract_calls(
            tree.root_node(),
            source,
            &mut nodes,
            &["invocation_expression", "object_creation_expression"],
        );

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
