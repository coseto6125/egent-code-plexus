use super::receiver_types::{
    collect_bindings, extended_type_name, extract_swift_calls_and_path_literals,
};
use super::spec::SwiftSpec;
use crate::calls::{NameRefKind, NameRefSpec, VariantRefSpec};
use crate::driver::{
    self, imports, node, AnonKey, Anonymous, Compiled, DeclSlot, Effect, FieldReads, FileCx,
    KindCapture, Op, ParseSpec, Step,
};
use crate::framework_confidence;
use crate::framework_helpers::{
    detect_ast_framework_patterns, node_span, span_area, span_contains, FrameworkPatternSpec, Span,
};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawFunctionMeta, RawNode, OVERRIDE_SENTINEL};
use ecp_core::graph::{FileCategory, NodeKind};
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::{Node, Query, Tree};

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
    compiled: Compiled<SwiftSpec>,
}

impl SwiftProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
        })
    }
}

impl LanguageProvider for SwiftProvider {
    fn name(&self) -> &'static str {
        "swift"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures Swift handles itself. The `.name` kind capture sets name and
/// kind; every other capture buffers a node or a text for `end_match`.
#[derive(Clone, Copy)]
pub enum SwiftRole {
    Root,
    ImportName,
    ImportSource,
    Export,
    Decorator,
    Heritage,
    Property,
    PropertyNamePat,
    Constructor,
    Typealias,
    EnumCase,
    EnumCaseName,
}

#[derive(Clone, Copy)]
pub enum SwiftHook {
    Calls,
    TextFrameworks,
    MemberOwners,
}

#[derive(Default)]
pub struct SwiftMatch<'t> {
    import_name: Option<Node<'t>>,
    import_src: Option<Node<'t>>,
    is_exported: bool,
    decorators: Vec<String>,
    heritage: Vec<String>,
    property_root: Option<Node<'t>>,
    property_name: Option<Node<'t>>,
    constructor_node: Option<Node<'t>>,
    typealias_node: Option<Node<'t>>,
    enum_case_root: Option<Node<'t>>,
    enum_case_names: Vec<Node<'t>>,
}

#[derive(Default)]
pub struct SwiftFile {
    /// Per (root, name-byte-offset) dedup. tree-sitter-swift fires the
    /// same property_declaration match ~3-4× per declared name when the
    /// optional `(type_annotation ...)?` resolves as both present and
    /// absent alternatives, AND when nested `bound_identifier` re-binds
    /// through pattern matching. Tracking (root_id, name_start_byte)
    /// collapses true duplicates while keeping tuple-pattern
    /// declarations (`let (a, b) = …`) distinct (different name byte
    /// offsets within the same root).
    seen_properties: HashSet<(usize, usize)>,
}

impl ParseSpec for SwiftSpec {
    type Role = SwiftRole;
    type Match<'t> = SwiftMatch<'t>;
    type File<'t> = SwiftFile;
    type Hook = SwiftHook;

    const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
    /// Trailing or arg-position closure whose body contains a call. Emit an
    /// `<anonymous>` Function node so attach_to_enclosing can host the inner
    /// calls instead of dropping them (filter (A) callback registration — no
    /// named enclosing scope). Both queries can match one lambda_literal,
    /// hence the span dedupe.
    const ANONYMOUS: Option<Anonymous> = Some(Anonymous {
        capture: "function.anonymous",
        call_kinds: &["call_expression"],
        key: AnonKey::Span,
    });
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[
        (
            "blind.nsclass_from_string",
            (
                "swift-nsclass-from-string",
                "NSClassFromString(<expr>) — runtime Objective-C class load by name; resolved class body is not statically determinable",
            ),
        ),
        (
            "blind.perform_selector",
            (
                "swift-perform-selector",
                "<expr>.perform(Selector(<name>)) — Objective-C selector dispatch; target method bound at runtime via selector lookup",
            ),
        ),
    ];
    const FIELD_READS: Option<FieldReads> = Some(FieldReads {
        member_kinds: &["navigation_expression"],
        names: NameRefSpec {
            ident_kinds: &["simple_identifier"],
            kind: NameRefKind::Value,
            binding_fields: &[
                ("property_declaration", "name"),
                ("for_statement", "item"),
                ("catch_block", "error"),
            ],
            skip_kinds: &[
                "import_declaration",
                "attribute",
                "type_annotation",
                "user_type",
                "type_arguments",
            ],
            skip_parents: &["value_argument_label"],
            call_suffixes: &["call_suffix"],
            ..NameRefSpec::EMPTY
        },
    });
    const VARIANT_REFS: Option<VariantRefSpec> = Some(VariantRefSpec {
        path_kinds: &["navigation_expression"],
        label_parents: &[],
        bare_label_parents: &[],
        bare_expressions: false,
        token_trees: false,
    });
    const STEPS: &'static [Step<SwiftHook>] = &[
        Step::fixed(Pass::Query, Op::Query),
        Step::hook(Pass::Calls, Effect::Nodes, SwiftHook::Calls),
        Step::fixed(Pass::Walk, Op::FieldReads),
        Step::fixed(Pass::Walk, Op::VariantRefs),
        Step::hook(Pass::Walk, Effect::Neutral, SwiftHook::TextFrameworks),
        Step::fixed(Pass::Walk, Op::FunctionMeta),
        Step::hook(Pass::Build, Effect::Nodes, SwiftHook::MemberOwners),
        Step::fixed(Pass::Build, Op::StampOwners),
    ];

    fn query() -> anyhow::Result<Query> {
        Ok(Query::new(
            &tree_sitter_swift::LANGUAGE.into(),
            include_str!("queries.scm"),
        )?)
    }

    fn role(capture: &str) -> Option<SwiftRole> {
        Some(match capture {
            "function" | "class" | "trait" => SwiftRole::Root,
            "import.name" => SwiftRole::ImportName,
            "import.source" => SwiftRole::ImportSource,
            "export" => SwiftRole::Export,
            "decorator" => SwiftRole::Decorator,
            "heritage" => SwiftRole::Heritage,
            "property" => SwiftRole::Property,
            "property.name.pat" => SwiftRole::PropertyNamePat,
            "constructor" => SwiftRole::Constructor,
            "typealias" => SwiftRole::Typealias,
            "enum_case" => SwiftRole::EnumCase,
            "enum_case.name" => SwiftRole::EnumCaseName,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: SwiftRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut SwiftMatch<'t>,
        _f: &mut SwiftFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        match r {
            SwiftRole::Root => d.root = Some(node),
            SwiftRole::ImportName => m.import_name = Some(node),
            SwiftRole::ImportSource => m.import_src = Some(node),
            SwiftRole::Export => {
                if let Ok(export_str) = node.utf8_text(source) {
                    if export_str == "public" || export_str == "open" {
                        m.is_exported = true;
                    }
                }
            }
            SwiftRole::Decorator => {
                if let Ok(d_str) = node.utf8_text(source) {
                    m.decorators.push(d_str.trim().to_string());
                }
            }
            SwiftRole::Heritage => {
                if let Ok(heritage_str) = node.utf8_text(source) {
                    m.heritage.push(heritage_str.to_string());
                }
            }
            SwiftRole::Property => m.property_root = Some(node),
            SwiftRole::PropertyNamePat => m.property_name = Some(node),
            SwiftRole::Constructor => m.constructor_node = Some(node),
            SwiftRole::Typealias => m.typealias_node = Some(node),
            SwiftRole::EnumCase => m.enum_case_root = Some(node),
            SwiftRole::EnumCaseName => m.enum_case_names.push(node),
        }
    }

    fn end_match<'t>(
        d: DeclSlot<'t>,
        m: &mut SwiftMatch<'t>,
        f: &mut SwiftFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        let SwiftMatch {
            import_name,
            import_src,
            is_exported,
            mut decorators,
            heritage,
            property_root,
            property_name,
            constructor_node,
            typealias_node,
            enum_case_root,
            enum_case_names,
        } = std::mem::take(m);

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
                cx.nodes.push(RawNode {
                    is_exported,
                    ..node::decl(
                        lhs.clone(),
                        NodeKind::Typedef,
                        node::span(ta_node),
                        &source[ta_node.start_byte()..ta_node.end_byte()],
                    )
                });
                let alias = Some(lhs.clone());
                cx.imports.push(imports::raw(rhs, lhs, alias, None));
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
            let span = node::span(ec_root);
            for name_node in &enum_case_names {
                if let Ok(name_str) = name_node.utf8_text(source) {
                    cx.nodes.push(RawNode {
                        is_exported: true, // enum cases follow enum visibility
                        ..node::decl(
                            name_str.to_string(),
                            NodeKind::EnumVariant,
                            span,
                            &source[ec_root.start_byte()..ec_root.end_byte()],
                        )
                    });
                }
            }
        }

        // Swift `init(...)` → Constructor. Emitted here before the
        // function_declaration path so `init` never falls through to Function.
        if let Some(ctor_node) = constructor_node {
            cx.nodes.push(RawNode {
                is_exported,
                ..node::decl(
                    "init".to_string(),
                    NodeKind::Constructor,
                    node::span(ctor_node),
                    &source[ctor_node.start_byte()..ctor_node.end_byte()],
                )
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
            // A local binding ends the whole match: the declaration and
            // import sections below must not run for it.
            let Some(node_kind) = emit_kind else { return };

            // Walk the property_declaration's direct children to find
            // type_annotation (if any). Text is `: <type>` — drop the colon.
            let type_ann = property_type_from_decl(pr_root, source);

            // Collect (name, byte_offset) pairs from the pattern node.
            // Handles both `var x` (one leaf) and `let (a, b)` (multiple).
            let names = collect_pattern_names(pat_node, source);

            let span = node::span(pr_root);

            for (name_str, name_byte) in names {
                if !f.seen_properties.insert((pr_root.id(), name_byte)) {
                    continue;
                }
                cx.nodes.push(RawNode {
                    is_exported: true,
                    type_annotation: type_ann.clone(),
                    ..node::decl(
                        name_str,
                        node_kind,
                        span,
                        &source[pr_root.start_byte()..pr_root.end_byte()],
                    )
                });
            }
        }

        if let (Some(n), Some(k), Some(root)) = (d.name, d.kind, d.root) {
            // Disambiguate class_declaration into Class/Struct/Enum via leading keyword.
            // `extension TypeName { ... }` shares the same class_declaration CST node in
            // tree-sitter-swift. Extensions are additive continuations of an existing type —
            // emitting a duplicate Class/Struct/Enum node for them is the root cause of
            // ~700 uid collisions in the Swift cluster. Skip the type-level emit; members
            // declared inside the extension body still emit individually via the
            // function_declaration / property_declaration / init_declaration paths.
            // The `return` also skips the import section: an extension match emits nothing else.
            let k = if k == NodeKind::Class {
                match swift_decl_keyword(root) {
                    "extension" => return,
                    "struct" => NodeKind::Struct,
                    "enum" => NodeKind::Enum,
                    _ => NodeKind::Class,
                }
            } else if k == NodeKind::Function && is_class_method(root) {
                NodeKind::Method
            } else {
                k
            };
            if let Ok(name_str) = n.utf8_text(source) {
                // `@objc(extName)` exposes a Swift symbol under an
                // Obj-C-visible alias. Emit an alias-only RawImport so the
                // rename binding shows up in the named-binding dimension
                // alongside Java static-import aliases. The attribute node
                // is nested under `(modifiers)` (not a direct
                // `function_declaration` child), so walk the subtree.
                if k == NodeKind::Method && has_override_modifier(root, source) {
                    decorators.push(OVERRIDE_SENTINEL.to_string());
                }
                if k == NodeKind::Function || k == NodeKind::Method {
                    if let Some(ext) = find_objc_rename_attribute(root, source) {
                        let alias = Some(ext.clone());
                        cx.imports
                            .push(imports::raw(name_str.to_string(), ext, alias, None));
                    }
                }
                cx.nodes.push(RawNode {
                    decorators,
                    is_exported,
                    heritage,
                    ..node::decl(
                        name_str.to_string(),
                        k,
                        node::span(root),
                        &source[root.start_byte()..root.end_byte()],
                    )
                });
            }
        }

        if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
            if let (Ok(name_str), Ok(src_str)) = (i_name.utf8_text(source), i_src.utf8_text(source))
            {
                cx.imports.push(imports::raw(
                    src_str.to_string(),
                    name_str.to_string(),
                    None,
                    None,
                ));
            }
        }
    }

    fn hook<'t>(h: SwiftHook, _c: &Compiled<Self>, _f: &mut SwiftFile, cx: &mut FileCx<'t>) {
        let root = cx.tree.root_node();
        match h {
            SwiftHook::Calls => {
                // Extract call sites with receiver-type binding: `self.method()` →
                // `Class.method`, `super.method()` → `Super.method`, typed-var
                // `obj.method()` → `Type.method` (P0 of Constructor Inference, mirrors
                // Python's `4e4fb1b` for the resolver's Tier 2.5 qualifier lookup).
                let bindings = collect_bindings(root, cx.source);
                let (raw_path_literals, raw_sql_refs) = extract_swift_calls_and_path_literals(
                    root,
                    cx.source,
                    &mut cx.nodes,
                    &bindings,
                );
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
            }
            SwiftHook::TextFrameworks => {
                cx.framework_refs = detect_ast_framework_patterns(cx.source, SWIFT_FRAMEWORKS);
            }
            SwiftHook::MemberOwners => stamp_member_owners(root, cx.source, &mut cx.nodes),
        }
    }

    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta> {
        crate::function_meta::swift::extract(root, source, nodes, category)
    }
}

/// Stamp every member with the type whose body declares it, before the
/// shared span stamp runs. That stamp sees neither an `extension` (it emits
/// no type node) nor an `enum` (its span stamps only variants), so their
/// members came out ownerless and every same-file pick took them for free
/// functions. The tightest enclosing type or extension wins: a type nested
/// in an extension keeps its own members.
fn stamp_member_owners(root: tree_sitter::Node<'_>, source: &[u8], nodes: &mut [RawNode]) {
    let mut containers: Vec<(Span, String)> = nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Class
                    | NodeKind::Struct
                    | NodeKind::Enum
                    | NodeKind::Interface
                    | NodeKind::Trait
            )
        })
        .map(|n| (n.span, n.name.clone()))
        .collect();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "function_body" | "lambda_literal" => continue,
            "class_declaration" if swift_decl_keyword(n) == "extension" => {
                let extended = n
                    .child_by_field_name("name")
                    .and_then(|name| extended_type_name(name, source));
                if let Some(extended) = extended {
                    containers.push((node_span(&n), extended.to_string()));
                }
            }
            _ => {}
        }
        let mut c = n.walk();
        stack.extend(n.named_children(&mut c));
    }
    if containers.is_empty() {
        return;
    }
    for node in nodes.iter_mut().filter(|n| {
        n.owner_class.is_none()
            && matches!(
                n.kind,
                NodeKind::Method | NodeKind::Function | NodeKind::Constructor | NodeKind::Property
            )
    }) {
        node.owner_class = containers
            .iter()
            .filter(|(span, _)| *span != node.span && span_contains(*span, node.span))
            .min_by_key(|(span, _)| span_area(*span))
            .map(|(_, name)| name.clone());
    }
}

/// Return the leading keyword of a `class_declaration` node: "class", "struct", "enum",
/// "extension", or "actor". tree-sitter-swift uses `class_declaration` for all of these;
/// the `declaration_kind` field is the first non-modifier keyword child.
fn swift_decl_keyword(class_decl: tree_sitter::Node<'_>) -> &'static str {
    for i in 0..class_decl.child_count() {
        if let Some(c) = class_decl.child(i) {
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

/// True when the `modifiers` of `decl` hold `override`. Swift requires the
/// modifier on a class member that overrides a superclass member, so the
/// `Overrides` pass reads it as the marker, as it reads Kotlin's.
fn has_override_modifier(decl: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let mut cursor = decl.walk();
    let found = decl
        .children(&mut cursor)
        .filter(|child| child.kind() == "modifiers")
        .any(|modifiers| {
            let mut inner = modifiers.walk();
            let found = modifiers.children(&mut inner).any(|m| {
                m.kind() == "member_modifier" && m.utf8_text(source).is_ok_and(|t| t == "override")
            });
            found
        });
    found
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
