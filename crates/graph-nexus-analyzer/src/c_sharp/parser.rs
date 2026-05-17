use super::receiver_types::extract_csharp_calls;
use crate::framework_confidence;
use crate::framework_helpers::{detect_ast_framework_patterns, FrameworkPatternSpec};
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

/// Per upstream `csharp.ts:153-187` `astFrameworkPatterns`. Substring scan of
/// the file source; emits one `RawFrameworkRef` per detected framework.
const CSHARP_FRAMEWORKS: &[FrameworkPatternSpec] = &[
    FrameworkPatternSpec {
        framework: "aspnet",
        reason: "aspnet-attribute",
        confidence: framework_confidence::ASPNET_HINT,
        patterns: &[
            "[ApiController]",
            "[HttpGet]",
            "[HttpPost]",
            "[HttpPut]",
            "[HttpDelete]",
            "[Route]",
            "[Authorize]",
            "[AllowAnonymous]",
        ],
    },
    FrameworkPatternSpec {
        framework: "signalr",
        reason: "signalr-attribute",
        confidence: framework_confidence::SIGNALR_HINT,
        patterns: &["[HubMethodName]", ": Hub", ": Hub<"],
    },
    FrameworkPatternSpec {
        framework: "blazor",
        reason: "blazor-attribute",
        confidence: framework_confidence::BLAZOR_HINT,
        patterns: &["@page", "[Parameter]", "@inject"],
    },
    FrameworkPatternSpec {
        framework: "efcore",
        reason: "efcore-pattern",
        confidence: framework_confidence::EFCORE_HINT,
        patterns: &["DbContext", "DbSet<", "OnModelCreating"],
    },
];

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
    /// Cached capture indices for new (Property/Variable/Constructor)
    /// captures added by the 14-lang parity work. Pre-existing captures
    /// (name.function / name.class / etc.) still look up per parse_file
    /// — left untouched per surgical-change convention.
    idx_property_name: Option<u32>,
    idx_property: Option<u32>,
    idx_variable_name: Option<u32>,
    idx_variable: Option<u32>,
    idx_constructor_name: Option<u32>,
    idx_constructor: Option<u32>,
    idx_namespace_name: Option<u32>,
    idx_namespace: Option<u32>,
    idx_enum_name: Option<u32>,
    idx_enum: Option<u32>,
}

impl CSharpProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_c_sharp::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        let idx_property_name = query.capture_index_for_name("property.name");
        let idx_property = query.capture_index_for_name("property");
        let idx_variable_name = query.capture_index_for_name("variable.name");
        let idx_variable = query.capture_index_for_name("variable");
        let idx_constructor_name = query.capture_index_for_name("constructor.name");
        let idx_constructor = query.capture_index_for_name("constructor");
        let idx_namespace_name = query.capture_index_for_name("namespace.name");
        let idx_namespace = query.capture_index_for_name("namespace");
        let idx_enum_name = query.capture_index_for_name("enum.name");
        let idx_enum = query.capture_index_for_name("enum");
        Ok(Self {
            query,
            idx_property_name,
            idx_property,
            idx_variable_name,
            idx_variable,
            idx_constructor_name,
            idx_constructor,
            idx_namespace_name,
            idx_namespace,
            idx_enum_name,
            idx_enum,
        })
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
        let idx_interface = self.query.capture_index_for_name("interface");
        // New 14-lang-parity captures: read cached indices (computed once
        // in `new()`) instead of looking up by name per file.
        let idx_property_name = self.idx_property_name;
        let idx_property = self.idx_property;
        let idx_variable_name = self.idx_variable_name;
        let idx_variable = self.idx_variable;
        let idx_constructor_name = self.idx_constructor_name;
        let idx_constructor = self.idx_constructor;
        let idx_namespace_name = self.idx_namespace_name;
        let idx_namespace = self.idx_namespace;
        let idx_enum_name = self.idx_enum_name;
        let idx_enum = self.idx_enum;

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
                } else if Some(cap_idx) == idx_name_interface {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Interface);
                } else if Some(cap_idx) == idx_property_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Property);
                } else if Some(cap_idx) == idx_variable_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Variable);
                } else if Some(cap_idx) == idx_constructor_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Constructor);
                } else if Some(cap_idx) == idx_namespace_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Namespace);
                } else if Some(cap_idx) == idx_enum_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Enum);
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
                    || Some(cap_idx) == idx_interface
                    || Some(cap_idx) == idx_property
                    || Some(cap_idx) == idx_variable
                    || Some(cap_idx) == idx_constructor
                    || Some(cap_idx) == idx_namespace
                    || Some(cap_idx) == idx_enum)
                    && root_span_node.is_none()
                {
                    root_span_node = Some(cap.node);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();

                    // For Property + Variable nodes, multiple declarators
                    // share the same root node id (`int x, y, z;`); key on
                    // the identifier node so each declarator is distinct.
                    let node_id = if matches!(k, NodeKind::Property | NodeKind::Variable) {
                        n.id()
                    } else {
                        root.id()
                    };
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
                        binding_kind: None,
                    });
                }
            }
        }

        let mut nodes: Vec<RawNode> = node_map.into_values().collect();

        // Extract call sites with receiver-type binding for `this.Foo()`,
        // `base.Foo()`, and typed-variable `obj.Foo()` patterns.
        extract_csharp_calls(tree.root_node(), source, &mut nodes);

        let framework_refs = detect_ast_framework_patterns(source, CSHARP_FRAMEWORKS);

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes: vec![],
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs,
            fanout_refs: vec![],
            blind_spots: vec![],
        })
    }
}
