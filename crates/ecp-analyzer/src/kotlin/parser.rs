use super::receiver_types::extract_kotlin_calls_and_path_literals;
use super::spec::KotlinSpec;
use crate::calls::{NameRefKind, NameRefSpec, VariantRefSpec};
use crate::driver::{
    self, imports, node, AnonKey, Anonymous, Compiled, DeclSlot, Effect, FieldReads, FileCx,
    KindCapture, Op, ParseSpec, Step, TxRule,
};
use crate::event_topic::{EventTopicConfig, KAFKA_KOTLIN};
use crate::framework_confidence;
use crate::framework_helpers::{
    collect_jvm_transactional_scopes, has_import_from, MODULE_LEVEL_SOURCE,
};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{
    LocalGraph, RawFrameworkRef, RawFunctionMeta, RawNode, Span, OVERRIDE_SENTINEL,
};
use ecp_core::graph::{FileCategory, NodeKind};
use rustc_hash::FxHashMap;
use std::path::Path;
use tree_sitter::{Node, Query, Tree};

// Framework-presence gate: only emit Ktor refs when the file imports io.ktor.*.
const KTOR_REQUIRED: &[&str] = &["io.ktor"];

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_kotlin::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct KotlinProvider {
    compiled: Compiled<KotlinSpec>,
}

/// tree-sitter-kotlin needs a separator between a class-body member and the
/// `}` that closes the body: `class A { fun f() {} }` (the last member and the
/// closer on one line) parses to an ERROR node, which drops the nested
/// classes and their owners. Overwriting the one blank before such a `}` with
/// `;` fixes that without moving any byte, so spans stay valid. `None` when no
/// blank qualifies.
fn separate_same_line_closers(source: &[u8]) -> Option<Vec<u8>> {
    let mut patched: Option<Vec<u8>> = None;
    for (i, &b) in source.iter().enumerate() {
        if b != b'}' || i == 0 || !matches!(source[i - 1], b' ' | b'\t') {
            continue;
        }
        let needs_separator = source[..i - 1]
            .iter()
            .rev()
            .find(|c| !matches!(c, b' ' | b'\t'))
            .is_some_and(|c| !matches!(c, b'{' | b';' | b'\n' | b'\r'));
        if needs_separator {
            patched.get_or_insert_with(|| source.to_vec())[i - 1] = b';';
        }
    }
    patched
}

/// `object O { fun a() {} }` on one line parses without an ERROR node, as a
/// top-level `infix_expression` led by an `object_literal`. A `.kt` file has no
/// top-level expressions, so that shape marks the same separator misparse.
fn has_top_level_object_misparse(root: tree_sitter::Node) -> bool {
    let mut cursor = root.walk();
    let found = root.named_children(&mut cursor).any(|child| {
        child.kind() == "infix_expression"
            && child
                .named_child(0)
                .is_some_and(|lhs| lhs.kind() == "object_literal")
    });
    found
}

/// Re-parses `source` with [`separate_same_line_closers`] applied when the
/// first parse has errors or the error-free object misparse, and keeps the
/// result only if it parses cleanly. Node offsets index the unpatched
/// `source` unchanged.
fn recover_same_line_closers(tree: tree_sitter::Tree, source: &[u8]) -> tree_sitter::Tree {
    let root = tree.root_node();
    if !root.has_error() && !has_top_level_object_misparse(root) {
        return tree;
    }
    separate_same_line_closers(source)
        .and_then(|patched| {
            PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), &patched, ParseBudget::DEFAULT))
        })
        .filter(|recovered| !recovered.root_node().has_error())
        .unwrap_or(tree)
}

/// True when `func` is a Kotlin `fun` declared directly inside a class body
/// (so its kind should be `Method`, not `Function`). Walks the tree-sitter
/// parent chain `function_declaration → class_body → class_declaration`.
fn is_class_method(func: tree_sitter::Node) -> bool {
    let Some(parent) = func.parent() else {
        return false;
    };
    // An `enum class` declares its members in `enum_class_body`, after the
    // entries; an entry with a body (`RED { override fun f() }`) overrides
    // them in its own `class_body`.
    if parent.kind() == "enum_class_body" {
        return true;
    }
    if parent.kind() != "class_body" {
        return false;
    }
    // class_body may be owned by class_declaration OR companion_object.
    // companion_object methods are class-level (static-equivalent) and should
    // be Method, not Function — the companion_object node is a class-body
    // sibling of regular members, so its parent is also class_declaration.
    parent.parent().is_some_and(|p| {
        matches!(
            p.kind(),
            "class_declaration" | "companion_object" | "object_declaration" | "enum_entry"
        )
    })
}

/// True when `class_decl` has a direct child whose kind matches `keyword`.
/// fwcd/tree-sitter-kotlin uses `class_declaration` for `class`, `interface`,
/// and `enum class` forms; the leading keyword appears as a sibling-of-name
/// child rather than inside `modifiers`. Callers below pin it to a specific
/// keyword to decide the emitted NodeKind.
fn class_decl_has_keyword_child(class_decl: tree_sitter::Node, keyword: &str) -> bool {
    let mut cursor = class_decl.walk();
    for child in class_decl.children(&mut cursor) {
        if child.kind() == keyword {
            return true;
        }
    }
    false
}

fn is_enum_class(class_decl: tree_sitter::Node) -> bool {
    class_decl_has_keyword_child(class_decl, "enum")
}

/// `class Bar : Foo` where `Foo` is an interface needs the demotion so PR #358's
/// kind-based dispatch emits `Implements` instead of `Extends`.
fn is_interface_class(class_decl: tree_sitter::Node) -> bool {
    class_decl_has_keyword_child(class_decl, "interface")
}

/// True when the `class_declaration` carries an `annotation` modifier — i.e.
/// `annotation class Foo`. Distinct from plain `class Foo`.
fn is_annotation_class(class_decl: tree_sitter::Node, source: &[u8]) -> bool {
    for i in 0..class_decl.child_count() {
        let Some(c) = class_decl.child(i) else {
            continue;
        };
        if c.kind() == "modifiers" {
            for j in 0..c.child_count() {
                let Some(m) = c.child(j) else { continue };
                if m.kind() == "class_modifier" || m.kind() == "modifier" {
                    if let Ok(t) = std::str::from_utf8(&source[m.start_byte()..m.end_byte()]) {
                        if t == "annotation" {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

impl KotlinProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
        })
    }
}

impl LanguageProvider for KotlinProvider {
    fn name(&self) -> &'static str {
        "kotlin"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures Kotlin handles itself. A root capture sets the declaration
/// root once per match; the `.name` kind capture sets name and kind.
#[derive(Clone, Copy)]
pub enum KotlinRole {
    Root,
    Export,
    Heritage,
    Type,
    Decorator,
    OverrideMarker,
    ImportSource,
    ImportAlias,
    KtorVerb(&'static str),
    KtorPath,
}

#[derive(Clone, Copy)]
pub enum KotlinHook {
    Calls,
    Ktor,
}

pub struct KotlinMatch<'t> {
    is_exported: bool,
    heritage: Vec<String>,
    type_annotation: Option<String>,
    decorators: Vec<String>,
    import_source: Option<Node<'t>>,
    import_alias: Option<Node<'t>>,
    ktor_verb: Option<&'static str>,
    ktor_route_span: Option<Span>,
    ktor_path_node: Option<Node<'t>>,
}

impl Default for KotlinMatch<'_> {
    /// A declaration is exported unless its `@export` modifiers say otherwise.
    fn default() -> Self {
        Self {
            is_exported: true,
            heritage: Vec::new(),
            type_annotation: None,
            decorators: Vec::new(),
            import_source: None,
            import_alias: None,
            ktor_verb: None,
            ktor_route_span: None,
            ktor_path_node: None,
        }
    }
}

/// Pending Ktor route ref: (verb, path_string, capture_span).
type KtorRef = (&'static str, String, Span);

#[derive(Default)]
pub struct KotlinFile {
    /// Dedup lookup for multi-capture merge only; nodes are pushed in match order and the map is never iterated.
    node_id_to_idx: FxHashMap<usize, usize>,
    /// Emitted only if the file imports `io.ktor.*`; the gate reads the
    /// imports after the query.
    pending_ktor_refs: Vec<KtorRef>,
}

impl ParseSpec for KotlinSpec {
    type Role = KotlinRole;
    type Match<'t> = KotlinMatch<'t>;
    type File<'t> = KotlinFile;
    type Hook = KotlinHook;

    const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
    /// Trailing / paren-position lambda. Emit an `<anonymous>` Function node
    /// only when the body holds a call, so attach_to_enclosing hosts those
    /// calls instead of dropping them; the trailing and paren-position
    /// queries can both match one lambda_literal, hence the span dedupe.
    const ANONYMOUS: Option<Anonymous> = Some(Anonymous {
        capture: "function.anonymous",
        call_kinds: &["call_expression"],
        key: AnonKey::Span,
    });
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[
        (
            "blind.class_forname",
            (
                "kt-class-forname",
                "Class.forName(<expr>) — runtime class loading via Java reflection bridge; loaded class body is not statically determinable",
            ),
        ),
        (
            "blind.method_invoke",
            (
                "kt-method-invoke",
                "<expr>.invoke(...) — reflective method invocation; target method body resolved at runtime via Java Method handle or KCallable",
            ),
        ),
    ];
    const FIELD_READS: Option<FieldReads> = Some(FieldReads {
        member_kinds: &["navigation_expression"],
        names: NameRefSpec {
            ident_kinds: &["simple_identifier"],
            kind: NameRefKind::Value,
            binding_parents: &["variable_declaration", "catch_block"],
            skip_kinds: &[
                "import_header",
                "package_header",
                "annotation",
                "type_arguments",
                "user_type",
            ],
            skip_parents: &["navigation_suffix"],
            call_suffixes: &["call_suffix"],
            infix_calls: &["infix_expression"],
            method_refs: &["callable_reference"],
            named_arg_parents: &["value_argument"],
            ..NameRefSpec::EMPTY
        },
    });
    const VARIANT_REFS: Option<VariantRefSpec> = Some(VariantRefSpec {
        path_kinds: &["navigation_expression"],
        label_parents: &["when_condition"],
        bare_label_parents: &[],
        bare_expressions: false,
        token_trees: false,
    });
    const EVENT_TOPICS: &'static [EventTopicConfig] = &[KAFKA_KOTLIN];
    const TX: TxRule = TxRule::Nodes {
        collect: collect_jvm_transactional_scopes,
        kinds: &[NodeKind::Method, NodeKind::Function, NodeKind::Constructor],
    };
    const POST_TREE: Option<fn(Tree, &[u8]) -> Tree> = Some(recover_same_line_closers);
    // Binding after the owner stamp is safe: `tx_scopes` reads kind and
    // decorators, and the stamp writes only `owner_class`.
    const STEPS: &'static [Step<KotlinHook>] = &[
        Step::fixed(Pass::Query, Op::Query),
        Step::hook(Pass::Calls, Effect::Nodes, KotlinHook::Calls),
        Step::fixed(Pass::Walk, Op::FieldReads),
        Step::fixed(Pass::Walk, Op::VariantRefs),
        Step::hook(Pass::Build, Effect::Neutral, KotlinHook::Ktor),
        Step::fixed(Pass::Walk, Op::FunctionMeta),
        Step::fixed(Pass::Build, Op::StampOwners),
        Step::fixed(Pass::Build, Op::TxScopes),
        Step::fixed(Pass::Walk, Op::EventTopicsRequery),
    ];

    fn query() -> anyhow::Result<Query> {
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        Ok(Query::new(
            &tree_sitter_kotlin::LANGUAGE.into(),
            &query_source,
        )?)
    }

    fn role(capture: &str) -> Option<KotlinRole> {
        Some(match capture {
            "class" | "function" | "constructor" | "property" | "variable" | "namespace"
            | "enum_entry" | "typedef" => KotlinRole::Root,
            "export" => KotlinRole::Export,
            "heritage" => KotlinRole::Heritage,
            "type" => KotlinRole::Type,
            "decorator" => KotlinRole::Decorator,
            "override_marker" => KotlinRole::OverrideMarker,
            "import.source" => KotlinRole::ImportSource,
            "alias" => KotlinRole::ImportAlias,
            "ktor.route.get" => KotlinRole::KtorVerb("get"),
            "ktor.route.post" => KotlinRole::KtorVerb("post"),
            "ktor.route.put" => KotlinRole::KtorVerb("put"),
            "ktor.route.delete" => KotlinRole::KtorVerb("delete"),
            "ktor.route.patch" => KotlinRole::KtorVerb("patch"),
            "ktor.route.path" => KotlinRole::KtorPath,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: KotlinRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut KotlinMatch<'t>,
        _f: &mut KotlinFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        match r {
            KotlinRole::Root => {
                if d.root.is_none() {
                    d.root = Some(node);
                }
            }
            KotlinRole::Export => {
                if let Some(modifiers) = node.utf8_text(source).ok() {
                    if modifiers.contains("private") || modifiers.contains("internal") {
                        m.is_exported = false;
                    }
                }
            }
            KotlinRole::Heritage => {
                if let Some(h) = node.utf8_text(source).ok() {
                    m.heritage.push(h.to_string());
                }
            }
            KotlinRole::Type => {
                if let Some(t) = node.utf8_text(source).ok() {
                    m.type_annotation = Some(t.to_string());
                }
            }
            KotlinRole::Decorator => {
                if let Some(dec) = node.utf8_text(source).ok() {
                    m.decorators.push(dec.to_string());
                }
            }
            KotlinRole::OverrideMarker => m.decorators.push(OVERRIDE_SENTINEL.to_string()),
            KotlinRole::ImportSource => m.import_source = Some(node),
            KotlinRole::ImportAlias => m.import_alias = Some(node),
            KotlinRole::KtorVerb(verb) => {
                m.ktor_verb = Some(verb);
                m.ktor_route_span = Some(node::span(node));
            }
            KotlinRole::KtorPath => m.ktor_path_node = Some(node),
        }
    }

    fn end_match<'t>(
        d: DeclSlot<'t>,
        m: &mut KotlinMatch<'t>,
        f: &mut KotlinFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        let mut kind = d.kind;

        // Demote `Function` to `Method` when the `function_declaration` is
        // a direct child of `class_body`. Promote `Class` to `Annotation`
        // when the `class_declaration` carries the `annotation` modifier.
        // Mirrors the Python class-method fix landed in this PR (see
        // `python/parser.rs::is_class_method`).
        if let (Some(k_val), Some(root)) = (kind, d.root) {
            let new_kind = match k_val {
                NodeKind::Function if is_class_method(root) => Some(NodeKind::Method),
                NodeKind::Class if is_enum_class(root) => Some(NodeKind::Enum),
                NodeKind::Class if is_annotation_class(root, source) => Some(NodeKind::Annotation),
                NodeKind::Class if is_interface_class(root) => Some(NodeKind::Interface),
                _ => None,
            };
            if let Some(nk) = new_kind {
                kind = Some(nk);
            }
        }

        if let (Some(n), Some(k), Some(root)) = (d.name, kind, d.root) {
            // No pre-classification filter. The Variable query already
            // restricts to `(source_file (property_declaration ...))` via
            // the tree-sitter pattern; broader cases get whatever shape
            // the grammar offers. Downstream consumers decide.

            if let Some(name_str) = n.utf8_text(source).ok() {
                // Property dedupe on name-node id so multi-declarator
                // patterns each get their own entry; other kinds keep
                // root-keyed dedupe (multi-decorator captures collapse).
                let node_id = if k == NodeKind::Property {
                    n.id()
                } else {
                    root.id()
                };
                let nodes = &mut cx.nodes;
                let idx = *f.node_id_to_idx.entry(node_id).or_insert_with(|| {
                    let i = nodes.len();
                    nodes.push(RawNode {
                        is_exported: m.is_exported,
                        type_annotation: m.type_annotation.clone(),
                        ..node::decl(
                            name_str.to_string(),
                            k,
                            node::span(root),
                            &source[root.start_byte()..root.end_byte()],
                        )
                    });
                    i
                });
                let entry = &mut cx.nodes[idx];

                if !m.is_exported {
                    entry.is_exported = false;
                }
                if m.type_annotation.is_some() {
                    entry.type_annotation = m.type_annotation.take();
                }
                for h in std::mem::take(&mut m.heritage) {
                    if !entry.heritage.contains(&h) {
                        entry.heritage.push(h);
                    }
                }
                for dec in std::mem::take(&mut m.decorators) {
                    if !entry.decorators.contains(&dec) {
                        entry.decorators.push(dec);
                    }
                }
            }
        }

        if let Some(i_src) = m.import_source {
            if let Some(src_str) = i_src.utf8_text(source).ok() {
                let alias = m
                    .import_alias
                    .and_then(|a_node| a_node.utf8_text(source).ok())
                    .map(|s| s.to_string());
                let imported_name = if i_src.parent().is_some_and(|parent| {
                    parent
                        .named_children(&mut parent.walk())
                        .any(|child| child.kind() == "wildcard_import")
                }) {
                    "*".to_string()
                } else {
                    src_str.rsplit('.').next().unwrap_or(src_str).to_string()
                };
                cx.imports.push(imports::raw(
                    src_str.to_string(),
                    imported_name,
                    alias,
                    None,
                ));
            }
        }

        // Stash Ktor route capture for post-loop gate. Path is from
        // `string_content` (already unquoted); verb is the per-pattern
        // capture name, so no regex alternation in Rust.
        if let (Some(verb), Some(span), Some(path_node)) =
            (m.ktor_verb, m.ktor_route_span, m.ktor_path_node)
        {
            if let Some(path_str) = path_node.utf8_text(source).ok() {
                f.pending_ktor_refs.push((verb, path_str.to_string(), span));
            }
        }
    }

    fn hook<'t>(h: KotlinHook, _c: &Compiled<Self>, f: &mut KotlinFile, cx: &mut FileCx<'t>) {
        match h {
            KotlinHook::Calls => {
                // Extract call sites with receiver-type binding for `this.foo()`,
                // `super.foo()`, and typed-variable `obj.foo()` patterns; same DFS
                // also collects path-shaped and SQL-shaped string literals.
                let (raw_path_literals, raw_sql_refs) = extract_kotlin_calls_and_path_literals(
                    cx.tree.root_node(),
                    cx.source,
                    &mut cx.nodes,
                );
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
            }
            KotlinHook::Ktor => {
                // The route DSL verbs (`get`/`post`/...) are common
                // identifiers, so without the gate we would over-claim.
                if has_import_from(&cx.imports, KTOR_REQUIRED) {
                    cx.framework_refs = std::mem::take(&mut f.pending_ktor_refs)
                        .into_iter()
                        .map(|(verb, path, span)| RawFrameworkRef {
                            source_name: MODULE_LEVEL_SOURCE.to_string(),
                            target_name: path,
                            confidence: framework_confidence::KTOR_ROUTE,
                            reason: format!("ktor-route-{}", verb),
                            span,
                        })
                        .collect();
                }
            }
        }
    }

    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta> {
        crate::function_meta::kotlin::extract(root, source, nodes, category)
    }
}
