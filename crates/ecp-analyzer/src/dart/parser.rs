use super::receiver_types::{collect_bindings, extract_dart_calls_and_path_literals};
use super::spec::DartSpec;
use crate::calls::{NameRefKind, NameRefSpec, VariantRefSpec};
use crate::driver::{
    self, node, AnonKey, Anonymous, Compiled, DeclSlot, Effect, FieldReads, FileCx, KindCapture,
    Op, ParseSpec, Step, TxRule,
};
use crate::framework_confidence;
use crate::framework_helpers::{
    detect_ast_framework_patterns, enclosing_fn_idx_by_span, FrameworkPatternSpec,
};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{FrameworkId, LocalGraph, RawFunctionMeta, RawNode, RawTxScope};
use ecp_core::graph::{FileCategory, NodeKind};
use std::path::Path;
use tree_sitter::{Node, Query, Tree};

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
    compiled: Compiled<DartSpec>,
}

impl DartProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
        })
    }
}

impl LanguageProvider for DartProvider {
    fn name(&self) -> &'static str {
        "dart"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures Dart handles itself. `Root` is every declaration-root
/// capture (`@class`, `@function`, `@import`, ...): it gives the span and
/// the hashed bytes of the node the kind capture names.
#[derive(Clone, Copy)]
pub enum DartRole {
    Root,
    Heritage,
    Type,
    ImportSource,
    ImportAlias,
    Var,
    VarName,
    VarType,
}

#[derive(Clone, Copy)]
pub enum DartHook {
    Calls,
    Frameworks,
    DeclAnnotationOwners,
}

#[derive(Default)]
pub struct DartMatch<'t> {
    heritage: Vec<String>,
    type_annotation: Option<String>,
    import_source: Option<Node<'t>>,
    import_alias: Option<Node<'t>>,
    var_root: Option<Node<'t>>,
    var_name: Option<Node<'t>>,
    var_type: Option<Node<'t>>,
}

impl ParseSpec for DartSpec {
    type Role = DartRole;
    type Match<'t> = DartMatch<'t>;
    type File<'t> = ();
    type Hook = DartHook;

    const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
    /// Anonymous closure (arg-position `function_expression`). Emit an
    /// `<anonymous>` Function node only when the body holds a
    /// call_expression so attach_to_enclosing can host those calls; empty
    /// closures (e.g. `list.map((x) => x)`) stay out of the graph. The same
    /// closure node can fire several matches, so the span dedupes it.
    const ANONYMOUS: Option<Anonymous> = Some(Anonymous {
        capture: "function.anonymous",
        call_kinds: &["call_expression"],
        key: AnonKey::Span,
    });
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[
        (
            "blind.function_apply",
            (
                "dart-function-apply",
                "Function.apply(<fn>, <args>) — reflective function invocation; target callable resolved at runtime via the Function reference",
            ),
        ),
        (
            "blind.mirrors_import",
            (
                "dart-mirrors-import",
                "import 'dart:mirrors' — file uses runtime reflection; downstream `reflect(...)` / `MirrorSystem` calls bind names to symbols at runtime",
            ),
        ),
    ];
    const FIELD_READS: Option<FieldReads> = Some(FieldReads {
        member_kinds: &["member_expression"],
        names: NameRefSpec {
            ident_kinds: &["identifier"],
            kind: NameRefKind::Value,
            binding_fields: &[
                ("initialized_variable_declaration", "name"),
                ("initialized_identifier", "name"),
                ("for_statement", "name"),
                ("for_element", "name"),
                ("catch_clause", "exception"),
                ("catch_clause", "stack_trace"),
            ],
            skip_kinds: &[
                "import_or_export",
                "library_name",
                "part_directive",
                "part_of_directive",
                "annotation",
                "type_arguments",
            ],
            skip_parents: &["label"],
            ..NameRefSpec::EMPTY
        },
    });
    const VARIANT_REFS: Option<VariantRefSpec> = Some(VariantRefSpec {
        // A `case Phase.run:` pattern holds the path without a
        // `member_expression`.
        path_kinds: &["member_expression", "constant_pattern"],
        label_parents: &[],
        bare_label_parents: &[],
        bare_expressions: false,
        token_trees: false,
    });
    const TX: TxRule = TxRule::Tree(collect_dart_tx_scopes);
    const STEPS: &'static [Step<DartHook>] = &[
        Step::fixed(Pass::Query, Op::Query),
        Step::fixed(Pass::Build, Op::DedupNodes),
        Step::hook(Pass::Calls, Effect::Nodes, DartHook::Calls),
        Step::fixed(Pass::Walk, Op::FieldReads),
        Step::fixed(Pass::Walk, Op::VariantRefs),
        Step::hook(Pass::Walk, Effect::Neutral, DartHook::Frameworks),
        Step::fixed(Pass::Walk, Op::FunctionMeta),
        Step::fixed(Pass::Build, Op::StampOwners),
        Step::hook(Pass::Build, Effect::Nodes, DartHook::DeclAnnotationOwners),
        Step::fixed(Pass::Walk, Op::TxScopes),
    ];

    fn query() -> anyhow::Result<Query> {
        Ok(Query::new(
            &tree_sitter_dart::LANGUAGE.into(),
            include_str!("queries.scm"),
        )?)
    }

    fn role(capture: &str) -> Option<DartRole> {
        Some(match capture {
            "function" | "class" | "method" | "constructor" | "typedef" | "trait" | "property"
            | "import" | "enum" | "enum_constant_node" | "annotation" => DartRole::Root,
            "heritage" => DartRole::Heritage,
            "type" => DartRole::Type,
            "import.source" => DartRole::ImportSource,
            "import.alias" => DartRole::ImportAlias,
            "var" => DartRole::Var,
            "var.name" => DartRole::VarName,
            "var.type" => DartRole::VarType,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: DartRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut DartMatch<'t>,
        _f: &mut (),
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        match r {
            DartRole::Root => d.root = Some(node),
            DartRole::Heritage => {
                if let Ok(h) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
                    m.heritage.push(h.trim().to_string());
                }
            }
            DartRole::Type => {
                if let Ok(t) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
                    m.type_annotation = Some(t.trim().to_string());
                }
            }
            DartRole::ImportSource => m.import_source = Some(node),
            DartRole::ImportAlias => m.import_alias = Some(node),
            DartRole::Var => m.var_root = Some(node),
            DartRole::VarName => m.var_name = Some(node),
            DartRole::VarType => m.var_type = Some(node),
        }
    }

    fn end_match<'t>(d: DeclSlot<'t>, m: &mut DartMatch<'t>, _f: &mut (), cx: &mut FileCx<'t>) {
        let source = cx.source;
        let DartMatch {
            mut heritage,
            type_annotation,
            import_source,
            import_alias,
            var_root,
            var_name,
            var_type,
        } = std::mem::take(m);

        // Dart top-level variable `double pi = 3.14` → Variable node.
        // tree-sitter-dart mis-parses `typedef Foo = void Function(...)` as
        // a top_level_variable_declaration with type text "typedef".  The
        // type_alias query path does NOT recover these (it only matches
        // old-style `typedef int Compare(int, int)`), so we synthesize a
        // Typedef RawNode from the misparsed node here.  Without this the
        // graph had zero Typedef nodes for new-style Dart typedefs (which
        // is most of them in modern Dart code).
        let is_typedef_misparse = var_type
            .map(|t| {
                std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                    .map(|s| s.trim() == "typedef")
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if is_typedef_misparse {
            if let (Some(v_root), Some(v_name)) = (var_root, var_name) {
                if let Some(node) = synth_typedef_from_misparse(v_root, v_name, source) {
                    cx.nodes.push(node);
                }
            }
            return;
        }
        if let (Some(v_root), Some(v_name)) = (var_root, var_name) {
            if let Ok(name_str) =
                std::str::from_utf8(&source[v_name.start_byte()..v_name.end_byte()])
            {
                let name_str = name_str.trim();
                let type_ann = var_type.and_then(|t| {
                    std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                        .ok()
                        .map(|s| s.trim().to_string())
                });
                cx.nodes.push(RawNode {
                    is_exported: !name_str.starts_with('_'),
                    type_annotation: type_ann,
                    decorators: annotation_texts(v_root, source),
                    ..node::decl(
                        name_str.to_string(),
                        NodeKind::Variable,
                        node::span(v_root),
                        &source[v_root.start_byte()..v_root.end_byte()],
                    )
                });
            }
        }

        if let (Some(n), Some(k), Some(root)) = (d.name, d.kind, d.root) {
            // queries.scm has both `(function_declaration (function_signature …))`
            // and a bare `(function_signature …)` Function pattern. The bare
            // pattern is load-bearing for top-level `external`/signature-only
            // declarations (no function_declaration wrapper), but it ALSO
            // fires on the inner function_signature child of every regular
            // function (parent `function_declaration`) and every class method
            // (parent `method_signature`), duplicating those outer emits.
            // Skip the inner cases — they're already covered by @function /
            // @method patterns that anchor on the outer node.
            if k == NodeKind::Function
                && root.kind() == "function_signature"
                && root.parent().is_some_and(|p| {
                    matches!(p.kind(), "function_declaration" | "method_signature")
                        || is_class_member_declaration(p)
                })
            {
                return;
            }
            // A named constructor's `name` field holds `Dog` and `named`, so
            // the pattern matches once per identifier; the last one is the
            // constructor's own name.
            if k == NodeKind::Constructor
                && n.parent().is_some_and(|sig| {
                    sig.children_by_field_name("name", &mut sig.walk())
                        .last()
                        .is_some_and(|last| last.id() != n.id())
                })
            {
                return;
            }
            match root.kind() {
                "class_declaration" if k == NodeKind::Class => {
                    insert_mixins(root, source, &mut heritage)
                }
                "mixin_declaration" => insert_mixin_constraints(root, source, &mut heritage),
                _ => {}
            }
            if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                let name_str = name_str.trim();
                // Dart visibility convention: identifiers starting with `_` are
                // library-private regardless of `library` directive (per Dart
                // language spec). Applies to all symbol kinds (Class, Function,
                // Method, Interface, Property).
                let is_exported = !name_str.starts_with('_');
                let decorators = if k == NodeKind::Annotation {
                    Vec::new()
                } else {
                    annotation_texts(root, source)
                };
                cx.nodes.push(RawNode {
                    is_exported,
                    heritage,
                    type_annotation,
                    decorators,
                    ..node::decl(
                        name_str.to_string(),
                        k,
                        node::span(root),
                        &source[root.start_byte()..root.end_byte()],
                    )
                });
            }
        }

        if let Some(i_src) = import_source {
            if let Ok(src_str) = std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
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

                let filtered = i_src.parent().is_some_and(|specification| {
                    specification
                        .named_children(&mut specification.walk())
                        .any(|child| child.kind() == "combinator")
                });
                let imported_name = if filtered {
                    String::new()
                } else {
                    clean_src.clone()
                };
                cx.imports.push(driver::imports::raw(
                    clean_src,
                    imported_name,
                    alias_str,
                    None,
                ));
            }
        }
    }

    fn hook<'t>(h: DartHook, _c: &Compiled<Self>, _f: &mut (), cx: &mut FileCx<'t>) {
        let root = cx.tree.root_node();
        let source = cx.source;
        match h {
            DartHook::Calls => {
                // Extract call sites with receiver-type binding: `this.method()` →
                // `Class.method`, `super.method()` → `Super.method`, typed-var
                // `obj.method()` → `Type.method`. Feeds the resolver's Tier 2.5
                // qualifier-scoped lookup.
                let bindings = collect_bindings(root, source);
                let (raw_path_literals, raw_sql_refs) =
                    extract_dart_calls_and_path_literals(root, source, &mut cx.nodes, &bindings);
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
            }
            DartHook::Frameworks => {
                cx.framework_refs = detect_ast_framework_patterns(source, DART_FRAMEWORKS);
            }
            DartHook::DeclAnnotationOwners => restamp_decl_annotation_owners(root, &mut cx.nodes),
        }
    }

    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta> {
        crate::function_meta::dart::extract(root, source, nodes, category)
    }
}

/// A `declaration` holding a class member's body-less signature.
fn is_class_member_declaration(declaration: Node<'_>) -> bool {
    declaration.kind() == "declaration"
        && declaration
            .parent()
            .is_some_and(|p| p.kind() == "member_declaration")
}

/// The source text of each annotation that marks `decl`, in source order.
/// A class member's annotations sit beside its declaration inside
/// `member_declaration` when the member is a field or a body-less
/// constructor, and inside it otherwise.
fn annotation_texts(decl: Node<'_>, source: &[u8]) -> Vec<String> {
    // A body-less member's signature sits inside `declaration`, the sibling
    // of its annotations.
    let anchor = decl
        .parent()
        .filter(|p| p.kind() == "declaration")
        .unwrap_or(decl);
    let siblings = anchor
        .parent()
        .filter(|p| p.kind() == "member_declaration")
        .into_iter()
        .flat_map(|p| {
            let mut cursor = p.walk();
            p.named_children(&mut cursor)
                .take_while(|c| c.id() != anchor.id())
                .collect::<Vec<_>>()
        });
    let mut cursor = decl.walk();
    siblings
        .chain(decl.named_children(&mut cursor).collect::<Vec<_>>())
        .filter(|c| c.kind() == "annotation")
        .filter_map(|a| a.utf8_text(source).ok())
        .map(|t| t.trim().to_string())
        .collect()
}

/// An annotation written before a class, mixin, enum or extension is a
/// child of that declaration, so the span stamp names the declaration as
/// the annotation's owner. The annotation marks the declaration and takes
/// the declaration's own owner instead. Dart has no nested type
/// declarations, so only top-level declarations hold such annotations.
fn restamp_decl_annotation_owners(root: Node<'_>, nodes: &mut [RawNode]) {
    let mut cursor = root.walk();
    for decl in root.named_children(&mut cursor).filter(|d| {
        matches!(
            d.kind(),
            "class_declaration"
                | "mixin_declaration"
                | "enum_declaration"
                | "extension_declaration"
        )
    }) {
        let decl_span = node::span(decl);
        let Some(owner) = nodes
            .iter()
            .find(|n| {
                n.span == decl_span
                    && matches!(n.kind, NodeKind::Class | NodeKind::Trait | NodeKind::Enum)
            })
            .map(|n| n.owner_class.clone())
        else {
            continue;
        };
        let mut inner = decl.walk();
        for ann in decl
            .named_children(&mut inner)
            .filter(|a| a.kind() == "annotation")
        {
            let ann_span = node::span(ann);
            for n in nodes
                .iter_mut()
                .filter(|n| n.kind == NodeKind::Annotation && n.span == ann_span)
            {
                n.owner_class = owner.clone();
            }
        }
    }
}

/// Puts a mixin's `on` constraints at the front of `heritage`, before the
/// `implements` interfaces the query captures. They are the unlabeled
/// `type` children of `mixin_declaration`; a generic constraint keeps the
/// type name only.
fn insert_mixin_constraints(mixin_decl: Node<'_>, source: &[u8], heritage: &mut Vec<String>) {
    let mut cursor = mixin_decl.walk();
    let constraints: Vec<String> = mixin_decl
        .named_children(&mut cursor)
        .filter(|ty| ty.kind() == "type")
        .filter_map(|ty| ty.named_child(0).and_then(|n| n.utf8_text(source).ok()))
        .map(|name| name.trim().to_string())
        .collect();
    let interfaces = std::mem::replace(heritage, constraints);
    heritage.extend(interfaces);
}

/// Splices `with M1, M2` into `heritage` after the superclass.
/// tree-sitter-dart 0.2 nests `mixins` inside `superclass`, where the class
/// pattern of `queries.scm` does not reach, so the captures hold the
/// superclass and the interfaces only. The order `extends`, `with`,
/// `implements` is the one the `Overrides` lookup reads.
fn insert_mixins(class_decl: tree_sitter::Node<'_>, source: &[u8], heritage: &mut Vec<String>) {
    let Some(superclass) = class_decl.child_by_field_name("superclass") else {
        return;
    };
    let at = usize::from(superclass.child_by_field_name("type").is_some()).min(heritage.len());
    let interfaces = heritage.split_off(at);
    let mut cursor = superclass.walk();
    for mixins in superclass
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "mixins")
    {
        let mut inner = mixins.walk();
        for ty in mixins
            .named_children(&mut inner)
            .filter(|ty| ty.kind() == "type")
        {
            if let Some(name) = ty.named_child(0).and_then(|n| n.utf8_text(source).ok()) {
                heritage.push(name.trim().to_string());
            }
        }
    }
    heritage.extend(interfaces);
}

/// Walk the Dart AST for `.transaction(closure)` / `.runTransaction(closure)`
/// call-site patterns (Drift, sqflite, Firestore). The scope is the ENCLOSING
/// Function or Method — one `RawTxScope` per unique enclosing node (dedup by
/// node index). Call-site form, not annotation form: scope is span-based.
///
/// Audit verdict: no Dart framework uses an `@Transaction`-style annotation.
/// All three major frameworks (Drift, sqflite, Firestore) use a `.transaction()`
/// / `.runTransaction()` call with a function literal / closure argument.
/// This detector covers all three idioms with a single method-name match.
fn collect_dart_tx_scopes(
    root: tree_sitter::Node<'_>,
    source: &[u8],
    nodes: &[RawNode],
) -> Option<Box<[RawTxScope]>> {
    let mut seen_indices: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut scopes: Vec<RawTxScope> = Vec::new();

    let mut stack: Vec<tree_sitter::Node<'_>> = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "call_expression" && is_tx_call(n, source) {
            // Shared helper agrees with Go / Ruby on innermost-fn match.
            let row = n.start_position().row as u32;
            let col = n.start_position().column as u32;
            if let Some(idx) = enclosing_fn_idx_by_span(nodes, row, col) {
                if seen_indices.insert(idx) {
                    scopes.push(RawTxScope::new(idx, FrameworkId::DartTransaction));
                }
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }

    (!scopes.is_empty()).then(|| scopes.into_boxed_slice())
}

/// Return true iff `call` is a `.transaction(closure)` or `.runTransaction(closure)` invocation:
/// - the callee is a `member_expression` whose `property` matches one of `TX_METHODS`
/// - the first positional argument is a `function_expression`
fn is_tx_call(call: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    const TX_METHODS: &[&str] = &["transaction", "runTransaction"];

    let Some(function) = call.child_by_field_name("function") else {
        return false;
    };
    if function.kind() != "member_expression" {
        return false;
    }
    let Some(prop) = function.child_by_field_name("property") else {
        return false;
    };
    let Ok(method_name) = std::str::from_utf8(&source[prop.start_byte()..prop.end_byte()]) else {
        return false;
    };
    if !TX_METHODS.contains(&method_name) {
        return false;
    }
    // Verify at least one argument is a function literal / closure.
    let Some(args) = call.child_by_field_name("arguments") else {
        return false;
    };
    let mut c = args.walk();
    for child in args.children(&mut c) {
        if child.kind() == "function_expression" {
            return true;
        }
    }
    false
}

/// Synthesize a Typedef RawNode from a tree-sitter-dart misparse:
/// new-style `typedef Foo = void Function(...)` lands as a
/// `top_level_variable_declaration` with type text "typedef". Returns None
/// if the name slice isn't valid UTF-8 (defensive — won't happen on
/// well-formed Dart source).
fn synth_typedef_from_misparse(
    v_root: tree_sitter::Node<'_>,
    v_name: tree_sitter::Node<'_>,
    source: &[u8],
) -> Option<RawNode> {
    let name_str = std::str::from_utf8(&source[v_name.start_byte()..v_name.end_byte()])
        .ok()?
        .trim();
    let start = v_root.start_position();
    let end = v_root.end_position();
    Some(RawNode {
        decorators: vec![],
        is_exported: !name_str.starts_with('_'),
        heritage: vec![],
        type_annotation: None,
        name: name_str.to_string(),
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
        content_hash: ecp_core::uid::xxh3_64_bytes(&source[v_root.start_byte()..v_root.end_byte()]),
    })
}
