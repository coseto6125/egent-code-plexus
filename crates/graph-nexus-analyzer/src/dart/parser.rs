use super::receiver_types::{collect_bindings, extract_dart_calls};
use crate::framework_confidence;
use crate::framework_helpers::{detect_ast_framework_patterns, FrameworkPatternSpec};
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

/// Per upstream `dart.ts:109-132` `astFrameworkPatterns`.
const DART_FRAMEWORKS: &[FrameworkPatternSpec] = &[
    FrameworkPatternSpec {
        framework: "flutter",
        reason: "flutter-widget",
        confidence: framework_confidence::FLUTTER_HINT,
        patterns: &[
            "StatelessWidget",
            "StatefulWidget",
            "BuildContext",
            "Widget build",
            "ChangeNotifier",
            "GetxController",
            "Cubit<",
            "Bloc<",
            "ConsumerWidget",
        ],
    },
    FrameworkPatternSpec {
        framework: "riverpod",
        reason: "riverpod-pattern",
        confidence: framework_confidence::RIVERPOD_HINT,
        patterns: &[
            "@riverpod",
            "ref.watch",
            "ref.read",
            "AsyncNotifier",
            "Notifier",
        ],
    },
];

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_dart::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct DartProvider {
    query: Query,
}

impl DartProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_dart::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for DartProvider {
    fn name(&self) -> &'static str {
        "dart"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        let idx_class_name = self.query.capture_index_for_name("class.name");
        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_method_name = self.query.capture_index_for_name("method.name");
        let idx_interface_name = self.query.capture_index_for_name("interface.name");
        let idx_property_name = self.query.capture_index_for_name("property.name");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");
        let idx_decorator = self.query.capture_index_for_name("decorator");

        let idx_class = self.query.capture_index_for_name("class");
        let idx_function = self.query.capture_index_for_name("function");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_interface = self.query.capture_index_for_name("interface");
        let idx_property = self.query.capture_index_for_name("property");
        let idx_import = self.query.capture_index_for_name("import");

        let idx_param = self.query.capture_index_for_name("param");
        let idx_param_name = self.query.capture_index_for_name("param.name");
        let idx_param_type = self.query.capture_index_for_name("param.type");
        let idx_var = self.query.capture_index_for_name("var");
        let idx_var_name = self.query.capture_index_for_name("var.name");
        let idx_var_type = self.query.capture_index_for_name("var.type");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut heritage = Vec::new();
            let mut type_annotation = None;
            let mut decorators = Vec::new();

            let mut import_source = None;
            let mut import_alias = None;

            let mut param_root: Option<tree_sitter::Node<'_>> = None;
            let mut param_name: Option<tree_sitter::Node<'_>> = None;
            let mut param_type: Option<tree_sitter::Node<'_>> = None;
            let mut var_root: Option<tree_sitter::Node<'_>> = None;
            let mut var_name: Option<tree_sitter::Node<'_>> = None;
            let mut var_type: Option<tree_sitter::Node<'_>> = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if Some(cap_idx) == idx_class_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Class);
                } else if Some(cap_idx) == idx_function_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if Some(cap_idx) == idx_method_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
                } else if Some(cap_idx) == idx_interface_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Interface);
                } else if Some(cap_idx) == idx_property_name {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Property);
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.trim().to_string());
                    }
                } else if Some(cap_idx) == idx_type {
                    if let Ok(t) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        type_annotation = Some(t.trim().to_string());
                    }
                } else if Some(cap_idx) == idx_import_source {
                    import_source = Some(cap.node);
                } else if Some(cap_idx) == idx_import_alias {
                    import_alias = Some(cap.node);
                } else if Some(cap_idx) == idx_decorator {
                    if let Ok(d_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d_str.to_string());
                    }
                } else if Some(cap_idx) == idx_param {
                    param_root = Some(cap.node);
                } else if Some(cap_idx) == idx_param_name {
                    param_name = Some(cap.node);
                } else if Some(cap_idx) == idx_param_type {
                    param_type = Some(cap.node);
                } else if Some(cap_idx) == idx_var {
                    var_root = Some(cap.node);
                } else if Some(cap_idx) == idx_var_name {
                    var_name = Some(cap.node);
                } else if Some(cap_idx) == idx_var_type {
                    var_type = Some(cap.node);
                }

                if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_method
                    || Some(cap_idx) == idx_interface
                    || Some(cap_idx) == idx_property
                    || Some(cap_idx) == idx_import
                {
                    root_span_node = Some(cap.node);
                }
            }

            // Dart formal parameter `String name` → Variable node carrying
            // the declared type. Mirrors C/C++/Swift conventions: each
            // declared name becomes a separately indexable RawNode.
            if let (Some(p_root), Some(p_name)) = (param_root, param_name) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[p_name.start_byte()..p_name.end_byte()])
                {
                    let name_str = name_str.trim();
                    let start = p_root.start_position();
                    let end = p_root.end_position();
                    let type_ann = param_type.and_then(|t| {
                        std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                            .ok()
                            .map(|s| s.trim().to_string())
                    });
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: !name_str.starts_with('_'),
                        heritage: vec![],
                        type_annotation: type_ann,
                        name: name_str.to_string(),
                        kind: NodeKind::Variable,
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

            // Dart top-level variable `double pi = 3.14` → Variable node.
            if let (Some(v_root), Some(v_name)) = (var_root, var_name) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[v_name.start_byte()..v_name.end_byte()])
                {
                    let name_str = name_str.trim();
                    let start = v_root.start_position();
                    let end = v_root.end_position();
                    let type_ann = var_type.and_then(|t| {
                        std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                            .ok()
                            .map(|s| s.trim().to_string())
                    });
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: !name_str.starts_with('_'),
                        heritage: vec![],
                        type_annotation: type_ann,
                        name: name_str.to_string(),
                        kind: NodeKind::Variable,
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

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let name_str = name_str.trim();
                    // Dart visibility convention: identifiers starting with `_` are
                    // library-private regardless of `library` directive (per Dart
                    // language spec). Applies to all symbol kinds (Class, Function,
                    // Method, Interface, Property).
                    let is_exported = !name_str.starts_with('_');
                    let start = root.start_position();
                    let end = root.end_position();

                    nodes.push(RawNode {
                        decorators,
                        is_exported,
                        heritage: heritage.clone(),
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
                }
            }

            if let Some(i_src) = import_source {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
                {
                    let clean_src = src_str
                        .trim()
                        .trim_matches('\'')
                        .trim_matches('"')
                        .to_string();

                    let alias_str = if let Some(i_alias) = import_alias {
                        std::str::from_utf8(&source[i_alias.start_byte()..i_alias.end_byte()])
                            .ok()
                            .map(|s| s.trim().to_string())
                    } else {
                        None
                    };

                    imports.push(RawImport {
                        alias: alias_str,
                        imported_name: clean_src.clone(),
                        source: clean_src,
                    });
                }
            }
        }

        // Deduplicate simple identical node extractions
        nodes.dedup_by(|a, b| a.name == b.name && a.span == b.span && a.kind == b.kind);

        // Extract call sites with receiver-type binding: `this.method()` →
        // `Class.method`, `super.method()` → `Super.method`, typed-var
        // `obj.method()` → `Type.method`. Feeds the resolver's Tier 2.5
        // qualifier-scoped lookup.
        let bindings = collect_bindings(tree.root_node(), source);
        extract_dart_calls(tree.root_node(), source, &mut nodes, &bindings);

        let framework_refs = detect_ast_framework_patterns(source, DART_FRAMEWORKS);

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
