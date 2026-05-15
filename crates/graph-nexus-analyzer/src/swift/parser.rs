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

        let idx_name_function = self.query.capture_index_for_name("function.name");
        let idx_name_class = self.query.capture_index_for_name("class.name");
        let idx_name_method = self.query.capture_index_for_name("method.name");
        let idx_name_interface = self.query.capture_index_for_name("interface.name");
        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_interface = self.query.capture_index_for_name("interface");
        let idx_typealias = self.query.capture_index_for_name("typealias");

        let idx_export = self.query.capture_index_for_name("export");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");

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

            let mut param_root: Option<tree_sitter::Node<'_>> = None;
            let mut param_name: Option<tree_sitter::Node<'_>> = None;
            let mut param_type: Option<tree_sitter::Node<'_>> = None;
            let mut property_root: Option<tree_sitter::Node<'_>> = None;
            let mut property_name: Option<tree_sitter::Node<'_>> = None;
            let mut property_type: Option<tree_sitter::Node<'_>> = None;
            let mut typealias_node: Option<tree_sitter::Node<'_>> = None;

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
                } else if Some(cap_idx) == idx_typealias {
                    typealias_node = Some(cap.node);
                }
            }

            // Swift `typealias MyInt = Int` / `typealias R<T> = Swift.Result<T, Error>`.
            // Emit a RawImport with alias = Some(lhs) so the binding surfaces
            // through the same downstream path as Java static-import aliases.
            // rhs text covers the full type expression (including generics).
            if let Some(ta_node) = typealias_node {
                if let Some((lhs, rhs)) = extract_typealias_parts(ta_node, source) {
                    imports.push(RawImport {
                        alias: Some(lhs.clone()),
                        imported_name: lhs,
                        source: rhs,
                        binding_kind: None,
                    });
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
                    // `@objc(extName)` exposes a Swift symbol under an
                    // Obj-C-visible alias. Emit an alias-only RawImport so the
                    // rename binding shows up in the named-binding dimension
                    // alongside Java static-import aliases. The attribute node
                    // is nested under `(modifiers)` (not a direct
                    // `function_declaration` child), so walk the subtree.
                    if k == NodeKind::Function {
                        if let Some(ext) = find_objc_rename_attribute(root, source) {
                            imports.push(RawImport {
                                alias: Some(ext.clone()),
                                imported_name: ext,
                                source: name_str.to_string(),
                                binding_kind: None,
                            });
                        }
                    }
                    nodes.push(RawNode {
                        decorators: vec![],
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
                        binding_kind: None,
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

/// Pull (lhs name, rhs type text) from a `typealias_declaration` node.
/// rhs is the full byte range from after `=` to the end of the typealias —
/// preserving any generic parameters or qualified paths verbatim.
fn extract_typealias_parts(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<(String, String)> {
    let mut cur = node.walk();
    let mut lhs: Option<String> = None;
    let mut eq_end: Option<usize> = None;
    for child in node.children(&mut cur) {
        match child.kind() {
            "type_identifier" if lhs.is_none() => {
                lhs = std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                    .ok()
                    .map(str::to_string);
            }
            "=" => {
                eq_end = Some(child.end_byte());
            }
            _ => {}
        }
    }
    let lhs = lhs?;
    let eq_end = eq_end?;
    let rhs_start = source[eq_end..node.end_byte()]
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map(|off| eq_end + off)
        .unwrap_or(eq_end);
    let rhs = std::str::from_utf8(&source[rhs_start..node.end_byte()])
        .ok()?
        .trim_end()
        .to_string();
    Some((lhs, rhs))
}

/// Walk a function_declaration's `(modifiers (attribute ...))` subtree for an
/// `@objc(externalName)` and return `externalName`. Plain `@objc` (no parens)
/// returns None — there is no rename binding to emit.
fn find_objc_rename_attribute(func_node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let mut cur = func_node.walk();
    for child in func_node.children(&mut cur) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mut mcur = child.walk();
        for attr in child.children(&mut mcur) {
            if attr.kind() != "attribute" {
                continue;
            }
            if let Some(name) = attribute_objc_external_name(attr, source) {
                return Some(name);
            }
        }
    }
    None
}

/// For an `(attribute @ user_type(type_identifier=objc) ( <name> ))` node,
/// return `<name>` if the leading user_type is `objc` and a single
/// simple_identifier argument is present.
fn attribute_objc_external_name(attr: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let mut cur = attr.walk();
    let mut is_objc = false;
    let mut external: Option<String> = None;
    for child in attr.children(&mut cur) {
        match child.kind() {
            "user_type" => {
                let txt = std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok()?;
                if txt == "objc" {
                    is_objc = true;
                }
            }
            "simple_identifier" if external.is_none() => {
                external = std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                    .ok()
                    .map(str::to_string);
            }
            _ => {}
        }
    }
    if is_objc {
        external
    } else {
        None
    }
}
