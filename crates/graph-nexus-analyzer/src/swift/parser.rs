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

        let idx_name_trait = self.query.capture_index_for_name("trait.name");
        let idx_trait = self.query.capture_index_for_name("trait");

        let idx_export = self.query.capture_index_for_name("export");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");

        let idx_property = self.query.capture_index_for_name("property");
        let idx_property_name_pat = self.query.capture_index_for_name("property.name.pat");
        let idx_constructor = self.query.capture_index_for_name("constructor");

        // Per (root_id, name_start_byte) dedup. Two query patterns fire for
        // every property_declaration (typed + untyped alternatives); dedup
        // collapses duplicate matches while keeping tuple-pattern names distinct.
        let mut seen_properties: std::collections::HashSet<(usize, usize)> =
            std::collections::HashSet::new();

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;

            let mut import_name = None;
            let mut import_src = None;

            let mut is_exported = false;
            let mut heritage = Vec::new();
            let mut type_annotation = None;

            let mut property_root: Option<tree_sitter::Node<'_>> = None;
            let mut property_name: Option<tree_sitter::Node<'_>> = None;
            let mut constructor_node: Option<tree_sitter::Node<'_>> = None;
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
                } else if Some(cap_idx) == idx_name_trait {
                    name_node = Some(cap.node);
                    kind = Some(NodeKind::Trait);
                } else if Some(cap_idx) == idx_import_name {
                    import_name = Some(cap.node);
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_method
                    || Some(cap_idx) == idx_interface
                    || Some(cap_idx) == idx_trait
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
                } else if Some(cap_idx) == idx_property {
                    property_root = Some(cap.node);
                } else if Some(cap_idx) == idx_property_name_pat {
                    property_name = Some(cap.node);
                } else if Some(cap_idx) == idx_constructor {
                    constructor_node = Some(cap.node);
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

            // Swift `init(...)` → Constructor. Emitted here before the
            // function_declaration path so `init` never falls through to Function.
            if let Some(ctor_node) = constructor_node {
                let start = ctor_node.start_position();
                let end = ctor_node.end_position();
                nodes.push(RawNode {
                    decorators: vec![],
                    is_exported,
                    heritage: vec![],
                    type_annotation: None,
                    name: "init".to_string(),
                    kind: NodeKind::Constructor,
                    span: (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    ),
                    calls: Vec::new(),
                });
            }

            // Swift property: `var x: Int` / `var x = 0` / `let (a,b) = ...`.
            // Emitted only at class/struct/protocol/extension/top-level scope —
            // filter out locals inside function_body, computed_property,
            // willset_didset_block, and lambda_literal.
            if let (Some(pr_root), Some(pat_node)) = (property_root, property_name) {
                // Locality check: walk up from property_declaration.
                let mut anc = pr_root.parent();
                let mut is_local = false;
                while let Some(a) = anc {
                    match a.kind() {
                        "computed_property" | "willset_didset_block" | "lambda_literal" => {
                            is_local = true;
                            break;
                        }
                        "class_body" | "protocol_body" | "enum_class_body" | "source_file" => {
                            break;
                        }
                        _ => {}
                    }
                    anc = a.parent();
                }
                if is_local {
                    continue;
                }

                // Walk the property_declaration's direct children to find
                // type_annotation (if any). Text is `: <type>` — drop the colon.
                let type_ann = property_type_from_decl(pr_root, source);

                // Collect (name, byte_offset) pairs from the pattern node.
                // Handles both `var x` (one leaf) and `let (a, b)` (multiple).
                let names = collect_pattern_names(pat_node, source);

                let start = pr_root.start_position();
                let end = pr_root.end_position();
                let span = (
                    start.row as u32,
                    start.column as u32,
                    end.row as u32,
                    end.column as u32,
                );

                for (name_str, name_byte) in names {
                    if !seen_properties.insert((pr_root.id(), name_byte)) {
                        continue;
                    }
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: true,
                        heritage: vec![],
                        type_annotation: type_ann.clone(),
                        name: name_str,
                        kind: NodeKind::Property,
                        span,
                        calls: Vec::new(),
                    });
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                // Disambiguate class_declaration into Class/Struct/Enum via leading keyword.
                let k = if k == NodeKind::Class {
                    match swift_decl_keyword(root) {
                        "struct" => NodeKind::Struct,
                        "enum" => NodeKind::Enum,
                        _ => NodeKind::Class,
                    }
                } else if k == NodeKind::Function && is_class_method(root) {
                    NodeKind::Method
                } else {
                    k
                };
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();
                    // `@objc(extName)` exposes a Swift symbol under an
                    // Obj-C-visible alias. Emit an alias-only RawImport so the
                    // rename binding shows up in the named-binding dimension
                    // alongside Java static-import aliases. The attribute node
                    // is nested under `(modifiers)` (not a direct
                    // `function_declaration` child), so walk the subtree.
                    if k == NodeKind::Function || k == NodeKind::Method {
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

/// Return the leading keyword of a `class_declaration` node ("class", "struct", or "enum").
/// tree-sitter-swift uses `class_declaration` for all three; the first non-modifier
/// child is the literal keyword token.
fn swift_decl_keyword(class_decl: tree_sitter::Node<'_>) -> &'static str {
    for i in 0..class_decl.child_count() {
        if let Some(c) = class_decl.child(i) {
            match c.kind() {
                "class" => return "class",
                "struct" => return "struct",
                "enum" => return "enum",
                _ => {}
            }
        }
    }
    "class"
}

/// Return true when `func_node` (a `function_declaration`) is directly nested inside
/// a class-like body (`class_body`, `enum_class_body`, `protocol_body`, or struct body).
/// Mirrors the python `is_class_method` parent-chain walk.
fn is_class_method(func_node: tree_sitter::Node<'_>) -> bool {
    let mut anc = func_node.parent();
    while let Some(a) = anc {
        match a.kind() {
            "class_body" | "enum_class_body" | "protocol_body" => return true,
            // Stop at file root or a function body — don't ascend further.
            "source_file" | "function_body" | "computed_property" | "lambda_literal" => {
                return false
            }
            _ => {}
        }
        anc = a.parent();
    }
    false
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
                let txt =
                    std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok()?;
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

/// Walk a `property_declaration` node's direct children for a `type_annotation`
/// child and return its type text (stripping the leading ": ").
fn property_type_from_decl(decl: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let mut cur = decl.walk();
    for child in decl.children(&mut cur) {
        if child.kind() == "type_annotation" {
            let raw = std::str::from_utf8(&source[child.start_byte()..child.end_byte()]).ok()?;
            return Some(raw.trim_start_matches(':').trim_start().to_string());
        }
    }
    None
}

/// Collect all `simple_identifier` leaf names from a `pattern` node.
/// Returns `(name_text, start_byte)` pairs — start_byte used for dedup.
/// Handles simple `var x` (one leaf) and tuple `let (a, b)` (multiple).
fn collect_pattern_names(pat: tree_sitter::Node<'_>, source: &[u8]) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    collect_pattern_names_rec(pat, source, &mut out);
    out
}

fn collect_pattern_names_rec(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    out: &mut Vec<(String, usize)>,
) {
    if node.kind() == "simple_identifier" && node.child_count() == 0 {
        if let Ok(s) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
            // Skip `_` wildcards — they're not named bindings.
            if s != "_" {
                out.push((s.to_string(), node.start_byte()));
            }
        }
        return;
    }
    let mut cur = node.walk();
    for child in node.children(&mut cur) {
        collect_pattern_names_rec(child, source, out);
    }
}
