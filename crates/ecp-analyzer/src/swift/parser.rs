use super::receiver_types::{collect_bindings, extract_swift_calls_and_path_literals};
use super::spec::SwiftSpec;
use crate::framework_confidence;
use crate::framework_helpers::{
    detect_ast_framework_patterns, node_span, push_blind_spot, FrameworkPatternSpec,
};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::algorithms::process_trace::is_test_path;
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{BlindSpot, LocalGraph, RawImport, RawNode};

/// Blind-spot kind/hint pairs. Order matches the capture-index dispatch
/// in `parse_file`.
const BLIND_SPEC: &[(&str, &str)] = &[
    (
        "swift-nsclass-from-string",
        "NSClassFromString(<expr>) — runtime Objective-C class load by name; resolved class body is not statically determinable",
    ),
    (
        "swift-perform-selector",
        "<expr>.perform(Selector(<name>)) — Objective-C selector dispatch; target method bound at runtime via selector lookup",
    ),
];
use ecp_core::graph::NodeKind;
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

// Swift TransactionScope: wontfix in v1 — no canonical pattern across
// CoreData / GRDB / Realm / SQLite.swift without type inference. Audit
// details: `docs/language-matrix.md` Swift `TransactionScope` footnote.

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
    /// Capture index → NodeKind mapping, pre-resolved from
    /// `SwiftSpec::CAPTURE_KIND` at provider construction. The hot loop
    /// looks up by integer index — no per-capture string compare.
    capture_kind_by_idx: Vec<Option<NodeKind>>,
    /// CI-L #2: capture indices resolved once. Same pattern as PHP / Kotlin.
    indices: SwiftCaptureIndices,
}

struct SwiftCaptureIndices {
    import_name: Option<u32>,
    import_source: Option<u32>,
    function: Option<u32>,
    class: Option<u32>,
    method: Option<u32>,
    interface: Option<u32>,
    typealias: Option<u32>,
    enum_case: Option<u32>,
    enum_case_name: Option<u32>,
    trait_: Option<u32>,
    export: Option<u32>,
    decorator: Option<u32>,
    heritage: Option<u32>,
    type_: Option<u32>,
    property: Option<u32>,
    property_name_pat: Option<u32>,
    constructor: Option<u32>,
    function_anonymous: Option<u32>,
    // BlindSpot captures (FU-001 P6a).
    blind_nsclass_from_string: Option<u32>,
    blind_perform_selector: Option<u32>,
}

impl SwiftProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_swift::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| SwiftSpec::CAPTURE_KIND.get(name).copied())
            .collect();
        let indices = SwiftCaptureIndices {
            import_name: query.capture_index_for_name("import.name"),
            import_source: query.capture_index_for_name("import.source"),
            function: query.capture_index_for_name("function"),
            class: query.capture_index_for_name("class"),
            method: query.capture_index_for_name("method"),
            interface: query.capture_index_for_name("interface"),
            typealias: query.capture_index_for_name("typealias"),
            enum_case: query.capture_index_for_name("enum_case"),
            enum_case_name: query.capture_index_for_name("enum_case.name"),
            trait_: query.capture_index_for_name("trait"),
            export: query.capture_index_for_name("export"),
            decorator: query.capture_index_for_name("decorator"),
            heritage: query.capture_index_for_name("heritage"),
            type_: query.capture_index_for_name("type"),
            property: query.capture_index_for_name("property"),
            property_name_pat: query.capture_index_for_name("property.name.pat"),
            constructor: query.capture_index_for_name("constructor"),
            function_anonymous: query.capture_index_for_name("function.anonymous"),
            blind_nsclass_from_string: query.capture_index_for_name("blind.nsclass_from_string"),
            blind_perform_selector: query.capture_index_for_name("blind.perform_selector"),
        };
        Ok(Self {
            query,
            capture_kind_by_idx,
            indices,
        })
    }
}

impl LanguageProvider for SwiftProvider {
    fn name(&self) -> &'static str {
        "swift"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();
        let mut blind_spots: Vec<BlindSpot> = Vec::new();
        let is_test_file = is_test_path(path.to_str().unwrap_or(""));

        // CI-L #2: capture indices pre-resolved in `new()`.
        let idx = &self.indices;
        let idx_import_name = idx.import_name;
        let idx_import_source = idx.import_source;
        let idx_function = idx.function;
        let idx_class = idx.class;
        let idx_method = idx.method;
        let idx_interface = idx.interface;
        let idx_typealias = idx.typealias;
        let idx_enum_case = idx.enum_case;
        let idx_enum_case_name = idx.enum_case_name;
        let idx_trait = idx.trait_;
        let idx_export = idx.export;
        let idx_decorator = idx.decorator;
        let idx_heritage = idx.heritage;
        let idx_type = idx.type_;
        let idx_property = idx.property;
        let idx_property_name_pat = idx.property_name_pat;
        let idx_constructor = idx.constructor;
        let idx_function_anonymous = idx.function_anonymous;
        let idx_blind_nsclass = idx.blind_nsclass_from_string;
        let idx_blind_perform = idx.blind_perform_selector;

        // Spans of already-emitted anonymous nodes — prevents duplicate
        // <anonymous> nodes when both trailing and arg-position queries match
        // the same lambda_literal.
        let mut emitted_anon_spans: std::collections::HashSet<(u32, u32, u32, u32)> =
            std::collections::HashSet::new();

        // Per (root, name-byte-offset) dedup. tree-sitter-swift fires the
        // same property_declaration match ~3-4× per declared name when the
        // optional `(type_annotation ...)?` resolves as both present and
        // absent alternatives, AND when nested `bound_identifier` re-binds
        // through pattern matching. Tracking (root_id, name_start_byte)
        // collapses true duplicates while keeping tuple-pattern
        // declarations (`let (a, b) = …`) distinct (different name byte
        // offsets within the same root).
        let mut seen_properties: std::collections::HashSet<(usize, usize)> =
            std::collections::HashSet::new();

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;

            let mut import_name = None;
            let mut import_src = None;

            let mut is_exported = false;
            let mut decorators: Vec<String> = Vec::new();
            let mut heritage = Vec::new();
            let mut type_annotation = None;

            let mut property_root: Option<tree_sitter::Node<'_>> = None;
            let mut property_name: Option<tree_sitter::Node<'_>> = None;
            let mut constructor_node: Option<tree_sitter::Node<'_>> = None;
            let mut typealias_node: Option<tree_sitter::Node<'_>> = None;
            let mut enum_case_root: Option<tree_sitter::Node<'_>> = None;
            let mut enum_case_names: Vec<tree_sitter::Node<'_>> = Vec::new();

            for cap in m.captures {
                let cap_idx = cap.index;
                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap_idx as usize)
                    .copied()
                    .flatten()
                {
                    // Single config-driven dispatch replaces the five explicit
                    // Class/Function/Method/Interface/Trait arms.
                    // Source of truth: SwiftSpec::CAPTURE_KIND in spec.rs.
                    name_node = Some(cap.node);
                    kind = Some(k_from_spec);
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
                } else if Some(cap_idx) == idx_decorator {
                    if let Ok(d_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d_str.trim().to_string());
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
                } else if Some(cap_idx) == idx_enum_case {
                    enum_case_root = Some(cap.node);
                } else if Some(cap_idx) == idx_enum_case_name {
                    enum_case_names.push(cap.node);
                } else if Some(cap_idx) == idx_function_anonymous {
                    // Trailing or arg-position closure whose body contains a call.
                    // Emit an <anonymous> Function node so attach_to_enclosing can
                    // host the inner calls instead of dropping them (filter (A)
                    // callback registration — no named enclosing scope).
                    if body_has_call(cap.node) {
                        let span = node_span(&cap.node);
                        if emitted_anon_spans.insert(span) {
                            nodes.push(RawNode {
                                decorators: Vec::new(),
                                is_exported: false,
                                heritage: Vec::new(),
                                type_annotation: None,
                                name: format!("<anonymous:{}:{}>", span.0 + 1, span.1),
                                kind: NodeKind::Function,
                                span,
                                calls: Vec::new(),
                                field_reads: Vec::new(),
                                owner_class: None,
                                content_hash: ecp_core::uid::xxh3_64_bytes(
                                    &source[cap.node.start_byte()..cap.node.end_byte()],
                                ),
                            });
                        }
                    }
                } else if Some(cap_idx) == idx_blind_nsclass {
                    push_blind_spot(
                        &mut blind_spots,
                        BLIND_SPEC[0],
                        &cap.node,
                        path,
                        is_test_file,
                    );
                } else if Some(cap_idx) == idx_blind_perform {
                    push_blind_spot(
                        &mut blind_spots,
                        BLIND_SPEC[1],
                        &cap.node,
                        path,
                        is_test_file,
                    );
                }
            }

            // Swift `typealias MyInt = Int` / `typealias R<T> = Swift.Result<T, Error>`.
            // Emit twice:
            //   1. A Typedef RawNode so graph queries by NodeKind find it (parity
            //      with Rust `type_item` and TS `type_alias_declaration`, both of
            //      which map to NodeKind::Typedef). The aggregator's EQUIV class
            //      `{Typedef, TypeAlias}` pairs this with ref's TypeAlias label.
            //   2. A RawImport with alias = Some(lhs) so the binding still surfaces
            //      through the alias-resolution path used by Java static-import.
            // rhs text covers the full type expression (including generics).
            if let Some(ta_node) = typealias_node {
                if let Some((lhs, rhs)) = extract_typealias_parts(ta_node, source) {
                    let start = ta_node.start_position();
                    let end = ta_node.end_position();
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported,
                        heritage: vec![],
                        type_annotation: None,
                        name: lhs.clone(),
                        kind: NodeKind::Typedef,
                        span: (
                            start.row as u32,
                            start.column as u32,
                            end.row as u32,
                            end.column as u32,
                        ),
                        calls: Vec::new(),
                        field_reads: Vec::new(),
                        owner_class: None,
                        content_hash: ecp_core::uid::xxh3_64_bytes(
                            &source[ta_node.start_byte()..ta_node.end_byte()],
                        ),
                    });
                    imports.push(RawImport {
                        alias: Some(lhs.clone()),
                        imported_name: lhs,
                        source: rhs,
                        binding_kind: None,
                    });
                }
            }

            // Swift enum cases — `case foo` / `case bar(Int)` / `case a, b, c`.
            // tree-sitter-swift packs all names of a multi-name `case a, b, c`
            // into a single `enum_entry`, each as a separate `simple_identifier`
            // child, so the query produces N name captures per match. Always
            // type-level (enum_entry only ever sits inside enum_class_body),
            // so no scope walker is needed. Emits EnumVariant (changed from
            // Property — owner_class stamped by stamp_owner_class_by_span).
            if let (Some(ec_root), false) = (enum_case_root, enum_case_names.is_empty()) {
                let start = ec_root.start_position();
                let end = ec_root.end_position();
                let span = (
                    start.row as u32,
                    start.column as u32,
                    end.row as u32,
                    end.column as u32,
                );
                for name_node in &enum_case_names {
                    if let Ok(name_str) =
                        std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                    {
                        nodes.push(RawNode {
                            decorators: vec![],
                            is_exported: true, // enum cases follow enum visibility
                            heritage: vec![],
                            type_annotation: None,
                            name: name_str.to_string(),
                            kind: NodeKind::EnumVariant,
                            span,
                            calls: Vec::new(),
                            field_reads: Vec::new(),
                            owner_class: None, // stamped by stamp_owner_class_by_span
                            content_hash: ecp_core::uid::xxh3_64_bytes(
                                &source[ec_root.start_byte()..ec_root.end_byte()],
                            ),
                        });
                    }
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
                    field_reads: Vec::new(),
                    owner_class: None,
                    content_hash: ecp_core::uid::xxh3_64_bytes(
                        &source[ctor_node.start_byte()..ctor_node.end_byte()],
                    ),
                });
            }

            // Swift property: `var x: Int` / `var x = 0` / `let (a,b) = ...`.
            // Emitted only at class/struct/protocol/extension/top-level scope —
            // filter out locals inside function_body, computed_property,
            // willset_didset_block, and lambda_literal.
            if let (Some(pr_root), Some(pat_node)) = (property_root, property_name) {
                // Locality check: walk up from property_declaration.
                // Stops at the first scope boundary and records which one:
                //   source_file        → top-level Variable
                //   class/struct/etc.  → member Property
                //   function_body / computed_property / lambda / control-flow
                //                      → local, skip entirely
                let mut anc = pr_root.parent();
                let mut emit_kind: Option<NodeKind> = None;
                while let Some(a) = anc {
                    match a.kind() {
                        "function_body"
                        | "computed_property"
                        | "willset_didset_block"
                        | "lambda_literal"
                        | "if_statement"
                        | "guard_statement"
                        | "for_statement"
                        | "while_statement" => {
                            // local binding — skip
                            break;
                        }
                        "class_body" | "protocol_body" | "enum_class_body" => {
                            emit_kind = Some(NodeKind::Property);
                            break;
                        }
                        "source_file" => {
                            emit_kind = Some(NodeKind::Variable);
                            break;
                        }
                        _ => {}
                    }
                    anc = a.parent();
                }
                let Some(node_kind) = emit_kind else { continue };

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
                        kind: node_kind,
                        span,
                        calls: Vec::new(),
                        field_reads: Vec::new(),
                        owner_class: None,
                        content_hash: ecp_core::uid::xxh3_64_bytes(
                            &source[pr_root.start_byte()..pr_root.end_byte()],
                        ),
                    });
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                // Disambiguate class_declaration into Class/Struct/Enum via leading keyword.
                // `extension TypeName { ... }` shares the same class_declaration CST node in
                // tree-sitter-swift. Extensions are additive continuations of an existing type —
                // emitting a duplicate Class/Struct/Enum node for them is the root cause of
                // ~700 uid collisions in the Swift cluster. Skip the type-level emit; members
                // declared inside the extension body still emit individually via the
                // function_declaration / property_declaration / init_declaration paths.
                let k = if k == NodeKind::Class {
                    match swift_decl_keyword(root) {
                        "extension" => continue,
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
                        field_reads: Vec::new(),
                        owner_class: None,
                        content_hash: ecp_core::uid::xxh3_64_bytes(
                            &source[root.start_byte()..root.end_byte()],
                        ),
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
        let raw_path_literals =
            extract_swift_calls_and_path_literals(tree.root_node(), source, &mut nodes, &bindings);
        crate::calls::extract_field_reads(
            tree.root_node(),
            source,
            &mut nodes,
            &["navigation_expression"],
        );

        let framework_refs = detect_ast_framework_patterns(source, SWIFT_FRAMEWORKS);

        let file_category =
            crate::resolution::builder::determine_category(path.to_str().unwrap_or(""));
        let raw_function_metas =
            crate::function_meta::swift::extract(tree.root_node(), source, &nodes, file_category);

        crate::framework_helpers::stamp_owner_class_by_span(&mut nodes);
        Ok(LocalGraph {
            content_hash: [0; 8],
            routes: vec![],
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs,
            fanout_refs: vec![],
            blind_spots,
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: (!raw_path_literals.is_empty())
                .then(|| raw_path_literals.into_boxed_slice()),
            call_metas: vec![],
            raw_function_metas,
        })
    }
}

/// Whether `node`'s subtree contains a `call_expression`. Gates emission of
/// `<anonymous>` callback nodes so closures without calls (e.g. `arr.map { $0 + 1 }`)
/// stay out of the graph.
fn body_has_call(node: tree_sitter::Node) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "call_expression" {
            return true;
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    false
}

/// Return the leading keyword of a `class_declaration` node: "class", "struct", "enum",
/// "extension", or "actor". tree-sitter-swift uses `class_declaration` for all of these;
/// the `declaration_kind` field is the first non-modifier keyword child.
fn swift_decl_keyword(class_decl: tree_sitter::Node<'_>) -> &'static str {
    for i in 0..class_decl.child_count() {
        if let Some(c) = class_decl.child(i as u32) {
            match c.kind() {
                "class" => return "class",
                "struct" => return "struct",
                "enum" => return "enum",
                "extension" => return "extension",
                "actor" => return "actor",
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
    // `simple_identifier` is the identifier terminal in tree-sitter-swift.
    // Treat it as a leaf regardless of `child_count`: Swift 5.9+ context-
    // keywords (`package`, `actor`, `await`, …) reuse identifier slots, and
    // tree-sitter-swift represents that by wrapping them in `simple_identifier
    // > <keyword-token>`. The previous `child_count() == 0` guard skipped
    // those wrappers — `let package = Package(...)` collected no name at all.
    // Read the source text of the simple_identifier node directly; its byte
    // range is the identifier text whether or not it has a keyword child.
    if node.kind() == "simple_identifier" {
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
