use super::receiver_types::{collect_bindings, extract_swift_calls};
use crate::framework_confidence;
use crate::framework_helpers::{detect_ast_framework_patterns, FrameworkPatternSpec};
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

/// Per upstream `swift.ts:281-316` `astFrameworkPatterns`.
const SWIFT_FRAMEWORKS: &[FrameworkPatternSpec] = &[
    FrameworkPatternSpec {
        framework: "uikit",
        reason: "uikit-lifecycle",
        confidence: framework_confidence::UIKIT_HINT,
        patterns: &[
            "viewDidLoad",
            "viewWillAppear",
            "viewDidAppear",
            "UIViewController",
            "@IBOutlet",
            "@IBAction",
            "@objc",
        ],
    },
    FrameworkPatternSpec {
        framework: "swiftui",
        reason: "swiftui-pattern",
        confidence: framework_confidence::SWIFTUI_HINT,
        patterns: &[
            "@main",
            "WindowGroup",
            "ContentView",
            "@StateObject",
            "@ObservedObject",
            "@EnvironmentObject",
            "@Published",
        ],
    },
    FrameworkPatternSpec {
        framework: "vapor",
        reason: "vapor-routing",
        confidence: framework_confidence::VAPOR_HINT,
        patterns: &["app.get", "app.post", "req.content.decode", "Vapor"],
    },
];

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_swift::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct SwiftProvider {
    query: Query,
}

impl SwiftProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_swift::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for SwiftProvider {
    fn name(&self) -> &'static str {
        "swift"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        let idx_name_function = self.query.capture_index_for_name("name.function");
        let idx_name_class = self.query.capture_index_for_name("name.class");
        let idx_name_method = self.query.capture_index_for_name("name.method");
        let idx_name_interface = self.query.capture_index_for_name("name.interface");
        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_interface = self.query.capture_index_for_name("interface");

        let idx_export = self.query.capture_index_for_name("export");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");
        let idx_decorator = self.query.capture_index_for_name("decorator");

        let idx_param = self.query.capture_index_for_name("param");
        let idx_param_name = self.query.capture_index_for_name("param.name");
        let idx_param_type = self.query.capture_index_for_name("param.type");
        let idx_property = self.query.capture_index_for_name("property");
        let idx_property_name = self.query.capture_index_for_name("property.name");
        let idx_property_type = self.query.capture_index_for_name("property.type");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;

            let mut import_name = None;
            let mut import_src = None;

            let mut is_exported = false;
            let mut heritage = Vec::new();
            let mut type_annotation = None;
            let mut decorators = Vec::new();

            let mut param_root: Option<tree_sitter::Node<'_>> = None;
            let mut param_name: Option<tree_sitter::Node<'_>> = None;
            let mut param_type: Option<tree_sitter::Node<'_>> = None;
            let mut property_root: Option<tree_sitter::Node<'_>> = None;
            let mut property_name: Option<tree_sitter::Node<'_>> = None;
            let mut property_type: Option<tree_sitter::Node<'_>> = None;

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
                } else if Some(cap_idx) == idx_import_name {
                    import_name = Some(cap.node);
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_method
                    || Some(cap_idx) == idx_interface
                {
                    root_span_node = Some(cap.node);
                } else if Some(cap_idx) == idx_export {
                    if let Ok(export_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        if export_str == "public" || export_str == "open" {
                            is_exported = true;
                        }
                    }
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(heritage_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(heritage_str.to_string());
                    }
                } else if Some(cap_idx) == idx_type {
                    if let Ok(type_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        type_annotation = Some(type_str.to_string());
                    }
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
                } else if Some(cap_idx) == idx_property {
                    property_root = Some(cap.node);
                } else if Some(cap_idx) == idx_property_name {
                    property_name = Some(cap.node);
                } else if Some(cap_idx) == idx_property_type {
                    property_type = Some(cap.node);
                }
            }

            // Swift function parameter `name: Type` → Variable node with the
            // type as `type_annotation`. Mirrors C/C++/Go convention from
            // Wave 2: each declared name with a type becomes a separately
            // indexable node.
            if let (Some(p_root), Some(p_name)) = (param_root, param_name) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[p_name.start_byte()..p_name.end_byte()])
                {
                    let start = p_root.start_position();
                    let end = p_root.end_position();
                    let type_ann = param_type.and_then(|t| {
                        std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                            .ok()
                            .map(|s| s.to_string())
                    });
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: false,
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

            // Swift property declaration `var x: Int` / `let y: String` →
            // Property node. Captured separately from `(class ...)` body so
            // both class properties and top-level lets land here.
            if let (Some(pr_root), Some(pr_name)) = (property_root, property_name) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[pr_name.start_byte()..pr_name.end_byte()])
                {
                    let start = pr_root.start_position();
                    let end = pr_root.end_position();
                    let type_ann = property_type.and_then(|t| {
                        std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                            .ok()
                            .map(|s| s.to_string())
                    });
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: true,
                        heritage: vec![],
                        type_annotation: type_ann,
                        name: name_str.to_string(),
                        kind: NodeKind::Property,
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
                    let start = root.start_position();
                    let end = root.end_position();
                    nodes.push(RawNode {
                        decorators,
                        is_exported,
                        heritage,
                        type_annotation,
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

            if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
                if let (Ok(name_str), Ok(src_str)) = (
                    std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]),
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    imports.push(RawImport {
                        alias: None,
                        imported_name: name_str.to_string(),
                        source: src_str.to_string(),
                    });
                }
            }
        }

        // Extract call sites with receiver-type binding: `self.method()` →
        // `Class.method`, `super.method()` → `Super.method`, typed-var
        // `obj.method()` → `Type.method` (P0 of Constructor Inference, mirrors
        // Python's `4e4fb1b` for the resolver's Tier 2.5 qualifier lookup).
        let bindings = collect_bindings(tree.root_node(), source);
        extract_swift_calls(tree.root_node(), source, &mut nodes, &bindings);

        let framework_refs = detect_ast_framework_patterns(source, SWIFT_FRAMEWORKS);

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
