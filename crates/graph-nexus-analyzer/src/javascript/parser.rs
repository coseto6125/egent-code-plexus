use super::receiver_types::extract_js_calls;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode, RawRoute};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_javascript::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct JavaScriptProvider {
    query: Query,
}

impl JavaScriptProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_javascript::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for JavaScriptProvider {
    fn name(&self) -> &'static str {
        "javascript"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();
        let mut routes: Vec<RawRoute> = Vec::new();

        let idx_name_function = self.query.capture_index_for_name("name.function");
        let idx_name_class = self.query.capture_index_for_name("name.class");
        let idx_name_method = self.query.capture_index_for_name("name.method");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_export = self.query.capture_index_for_name("export");
        let idx_decorator = self.query.capture_index_for_name("decorator");

        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_method = self.query.capture_index_for_name("method");

        let idx_route_method = self.query.capture_index_for_name("route.method");
        let idx_route_path = self.query.capture_index_for_name("route.path");
        let idx_route_call = self.query.capture_index_for_name("route.call");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut heritage = Vec::new();
            let mut is_exported = false;
            let mut decorators = Vec::new();

            let mut import_name = None;
            let mut import_src = None;
            let mut import_alias = None;

            let mut route_method = None;
            let mut route_path = None;
            let mut is_route = false;

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
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(h_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h_str.to_string());
                    }
                } else if Some(cap_idx) == idx_export {
                    is_exported = true;
                } else if Some(cap_idx) == idx_decorator {
                    if let Ok(d_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d_str.to_string());
                    }
                } else if Some(cap_idx) == idx_import_name {
                    import_name = Some(cap.node);
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_import_alias {
                    import_alias = Some(cap.node);
                } else if Some(cap_idx) == idx_route_method {
                    route_method = Some(cap.node);
                } else if Some(cap_idx) == idx_route_path {
                    route_path = Some(cap.node);
                } else if Some(cap_idx) == idx_route_call {
                    is_route = true;
                    root_span_node = Some(cap.node);
                } else if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_method
                {
                    root_span_node = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    let node_span = (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    );

                    let mut existing_found = false;
                    for node in &mut nodes {
                        if node.span == node_span && node.name == name_str {
                            if is_exported {
                                node.is_exported = true;
                            }
                            if !heritage.is_empty() {
                                for h in &heritage {
                                    if !node.heritage.contains(h) {
                                        node.heritage.push(h.clone());
                                    }
                                }
                            }
                            if !decorators.is_empty() {
                                for d in &decorators {
                                    if !node.decorators.contains(d) {
                                        node.decorators.push(d.clone());
                                    }
                                }
                            }
                            existing_found = true;
                            break;
                        }
                    }

                    if !existing_found {
                        nodes.push(RawNode {
                            decorators: decorators.clone(),
                            is_exported,
                            heritage: heritage.clone(),
                            type_annotation: None,
                            name: name_str.to_string(),
                            kind: k,
                            span: node_span,
                            calls: Vec::new(),
                        });
                    }
                }
            }

            if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
                if let (Ok(name_str), Ok(src_str)) = (
                    std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]),
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    let alias = import_alias.and_then(|a| {
                        std::str::from_utf8(&source[a.start_byte()..a.end_byte()])
                            .ok()
                            .map(|s| s.to_string())
                    });

                    imports.push(RawImport {
                        alias,
                        imported_name: name_str.to_string(),
                        source: src_str.to_string(),
                    });
                }
            }

            if is_route {
                if let (Some(r_method), Some(r_path), Some(root)) =
                    (route_method, route_path, root_span_node)
                {
                    if let (Ok(method_str), Ok(path_str)) = (
                        std::str::from_utf8(&source[r_method.start_byte()..r_method.end_byte()]),
                        std::str::from_utf8(&source[r_path.start_byte()..r_path.end_byte()]),
                    ) {
                        let start = root.start_position();
                        let end = root.end_position();
                        routes.push(RawRoute {
                            method: method_str.to_string(),
                            path: path_str.to_string(),
                            handler: None,
                            span: (
                                start.row as u32,
                                start.column as u32,
                                end.row as u32,
                                end.column as u32,
                            ),
                        });
                    }
                }
            }
        }

        // Extract call sites with receiver-type binding:
        // - `this.method()` inside a class body → `ClassName.method`
        // - `obj.method()` (no type info in JS) → `obj.method` (qualified for resolver)
        // - `fn()` → `fn`
        extract_js_calls(tree.root_node(), source, &mut nodes);

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes,
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
