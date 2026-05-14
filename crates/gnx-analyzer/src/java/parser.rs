use crate::calls::extract_calls;
use crate::framework_helpers::node_span;
use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawImport, RawNode};
use gnx_core::graph::NodeKind;
use std::collections::HashMap;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct JavaProvider {
    query: Query,
    indices: JavaCaptureIndices,
}

struct JavaCaptureIndices {
    class_name: Option<u32>,
    interface_name: Option<u32>,
    method_name: Option<u32>,
    import_name: Option<u32>,
    import_source: Option<u32>,
    class: Option<u32>,
    interface: Option<u32>,
    method: Option<u32>,
    export: Option<u32>,
    heritage: Option<u32>,
    type_ann: Option<u32>,
    decorator: Option<u32>,
    // Spring @Autowired field injection.
    spring_autowired_class: Option<u32>,
    spring_autowired_target: Option<u32>,
    // Spring @RestController / @Controller route methods.
    spring_route_class: Option<u32>,
    spring_route_handler: Option<u32>,
}

impl JavaProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_java::LANGUAGE.into();
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        let query = Query::new(&language, &query_source)?;
        let indices = JavaCaptureIndices {
            class_name: query.capture_index_for_name("class.name"),
            interface_name: query.capture_index_for_name("interface.name"),
            method_name: query.capture_index_for_name("method.name"),
            import_name: query.capture_index_for_name("import.name"),
            import_source: query.capture_index_for_name("import.source"),
            class: query.capture_index_for_name("class"),
            interface: query.capture_index_for_name("interface"),
            method: query.capture_index_for_name("method"),
            export: query.capture_index_for_name("export"),
            heritage: query.capture_index_for_name("heritage"),
            type_ann: query.capture_index_for_name("type"),
            decorator: query.capture_index_for_name("decorator"),
            spring_autowired_class: query.capture_index_for_name("spring.autowired.class"),
            spring_autowired_target: query.capture_index_for_name("spring.autowired.target"),
            spring_route_class: query.capture_index_for_name("spring.route.class"),
            spring_route_handler: query.capture_index_for_name("spring.route.handler"),
        };
        Ok(Self { query, indices })
    }
}

impl LanguageProvider for JavaProvider {
    fn name(&self) -> &'static str {
        "java"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let language = tree_sitter_java::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language)?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse java file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut node_map: HashMap<usize, RawNode> = HashMap::new();
        let mut imports = Vec::new();
        let mut framework_refs: Vec<RawFrameworkRef> = Vec::new();

        let idx = &self.indices;

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut is_exported = false;
            let mut heritage = Vec::new();
            let mut type_annotation = None;
            let mut decorators = Vec::new();

            let mut import_name = None;
            let mut import_src = None;

            // Spring @Autowired captures.
            let mut autowired_class_node: Option<tree_sitter::Node> = None;
            let mut autowired_target_node: Option<tree_sitter::Node> = None;
            // Spring route handler captures.
            let mut route_class_node: Option<tree_sitter::Node> = None;
            let mut route_handler_node: Option<tree_sitter::Node> = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if cap_idx == idx.class_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if cap_idx == idx.interface_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Interface);
                } else if cap_idx == idx.method_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
                } else if cap_idx == idx.import_name {
                    import_name = Some(cap.node);
                } else if cap_idx == idx.import_source {
                    import_src = Some(cap.node);
                } else if cap_idx == idx.class
                    || cap_idx == idx.interface
                    || cap_idx == idx.method
                {
                    if root_span_node.is_none() {
                        root_span_node = Some(cap.node);
                    }
                } else if cap_idx == idx.export {
                    is_exported = true;
                } else if cap_idx == idx.heritage {
                    if let Ok(h_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h_str.to_string());
                    }
                } else if cap_idx == idx.type_ann {
                    if let Ok(t_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        type_annotation = Some(t_str.to_string());
                    }
                } else if cap_idx == idx.decorator {
                    if let Ok(d_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d_str.to_string());
                    }
                } else if cap_idx == idx.spring_autowired_class {
                    autowired_class_node = Some(cap.node);
                } else if cap_idx == idx.spring_autowired_target {
                    autowired_target_node = Some(cap.node);
                } else if cap_idx == idx.spring_route_class {
                    route_class_node = Some(cap.node);
                } else if cap_idx == idx.spring_route_handler {
                    route_handler_node = Some(cap.node);
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

                    for h in heritage {
                        if !entry.heritage.contains(&h) {
                            entry.heritage.push(h);
                        }
                    }
                    for d in decorators {
                        if !entry.decorators.contains(&d) {
                            entry.decorators.push(d);
                        }
                    }
                    if is_exported {
                        entry.is_exported = true;
                    }
                    if type_annotation.is_some() {
                        entry.type_annotation = type_annotation.clone();
                    }
                }
            }

            if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
                if let (Ok(name_str), Ok(src_str)) = (
                    std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]),
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    let exists = imports
                        .iter()
                        .any(|i: &RawImport| i.imported_name == name_str && i.source == src_str);
                    if !exists {
                        imports.push(RawImport {
                            alias: None,
                            imported_name: name_str.to_string(),
                            source: src_str.to_string(),
                        });
                    }
                }
            }

            // Spring @Autowired: enclosing class -> injected type.
            if let (Some(cls), Some(tgt)) = (autowired_class_node, autowired_target_node) {
                if let (Ok(class_name), Ok(target_name)) = (
                    std::str::from_utf8(&source[cls.start_byte()..cls.end_byte()]),
                    std::str::from_utf8(&source[tgt.start_byte()..tgt.end_byte()]),
                ) {
                    framework_refs.push(RawFrameworkRef {
                        source_name: class_name.to_string(),
                        target_name: target_name.to_string(),
                        confidence: 0.8,
                        reason: "spring-autowired".to_string(),
                        span: node_span(&tgt),
                    });
                }
            }

            // Spring @RestController/@Controller: class -> route handler method.
            if let (Some(cls), Some(mth)) = (route_class_node, route_handler_node) {
                if let (Ok(class_name), Ok(method_name)) = (
                    std::str::from_utf8(&source[cls.start_byte()..cls.end_byte()]),
                    std::str::from_utf8(&source[mth.start_byte()..mth.end_byte()]),
                ) {
                    framework_refs.push(RawFrameworkRef {
                        source_name: class_name.to_string(),
                        target_name: method_name.to_string(),
                        confidence: 0.9,
                        reason: "spring-route-handler".to_string(),
                        span: node_span(&mth),
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
            &["method_invocation", "object_creation_expression"],
        );

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes: vec![],
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs,
        })
    }
}
