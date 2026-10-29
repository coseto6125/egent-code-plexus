use super::receiver_types::{
    build_impl_map, collect_local_types, enclosing_enum_name, enclosing_function_name,
    enclosing_impl_or_trait_context, enclosing_impl_type, enclosing_struct_type,
    extract_rust_calls_and_path_literals, impl_self_type, impl_trait_name, LocalTypes,
};
use super::spec::RustSpec;
use crate::calls::{NameRefKind, NameRefSpec, VariantRefSpec};
use crate::driver::{
    self, node, AnonKey, Anonymous, Compiled, DeclSlot, Effect, FieldReads, FileCx, KindCapture,
    Op, ParseSpec, ScopeLanguage, Step, TxRule,
};
use crate::event_topic::{EventTopicConfig, KAFKA_RUST, RABBITMQ_RUST, REDIS_RUST, SQS_RUST};
use crate::framework_confidence;
use crate::framework_helpers::{
    collect_rust_transactional_scopes, has_import_from, node_span, MODULE_LEVEL_SOURCE,
};
use crate::indirect_dispatch::{collect_rust_indirect_param_types, detect_rust_indirect};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{
    LocalGraph, RawFrameworkRef, RawFunctionMeta, RawImport, RawNode, Span,
};
use ecp_core::file_category::Language;
use ecp_core::graph::{FileCategory, NodeKind};
use std::ops::Range;
use std::path::Path;
use tree_sitter::{Node, Query, Tree};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}

/// The trait an `impl Trait for T` names, as other languages store a base:
/// the path kept, generic arguments dropped (`fmt::Display`,
/// `From<u8>` → `From`, `Iterator<Item = u8>` → `Iterator`). The resolver
/// looks a base up as a type name, which carries no arguments.
fn trait_path(text: &str) -> String {
    let mut depth = 0u32;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

pub struct RustProvider {
    compiled: Compiled<RustSpec>,
}

impl RustProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
        })
    }
}

impl LanguageProvider for RustProvider {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures Rust handles itself; the root captures set the kind that a
/// `.name` capture of the same match only fills while it is empty.
#[derive(Clone, Copy)]
pub enum RustRole {
    ImportName,
    ImportSource,
    ImportAlias,
    Root(NodeKind),
    Export,
    Heritage,
    TypeAnnotation,
    Decorator,
    AxumHandler,
    ActixMethod,
    ActixHandler,
}

#[derive(Clone, Copy)]
pub enum RustHook {
    NestedUse,
    Calls,
    Indirect,
    Frameworks,
}

/// `(handler_ident, capture_start_byte, capture_end_byte, span)`
type AxumCapture = (String, usize, usize, Span);
/// `(method, handler_ident, span)`
type ActixCapture = (String, String, Span);

#[derive(Default)]
pub struct RustMatch<'t> {
    is_exported: bool,
    heritage: Vec<String>,
    type_annotation: Option<String>,
    decorators: Vec<String>,
    import_name: Option<Node<'t>>,
    import_src: Option<Node<'t>>,
    import_alias: Option<Node<'t>>,
    // One match = one fn + its attribute.
    actix_method: Option<Node<'t>>,
    actix_handler: Option<Node<'t>>,
}

#[derive(Default)]
pub struct RustFile {
    /// Function and method byte ranges + names, used to resolve the
    /// enclosing function of a framework-ref capture by byte-range containment.
    fn_spans: Vec<(String, Range<usize>)>,
    axum_handler_captures: Vec<AxumCapture>,
    actix_handler_captures: Vec<ActixCapture>,
    /// Written by the calls step, read by the indirect step.
    local_types: Option<LocalTypes>,
}

impl ParseSpec for RustSpec {
    type Role = RustRole;
    type Match<'t> = RustMatch<'t>;
    type File<'t> = RustFile;
    type Hook = RustHook;

    const KIND_CAPTURE: KindCapture = KindCapture::NameKeepFirst;
    /// A closure gets an `<anonymous>` node only when its body holds a call,
    /// so attach_to_enclosing can host those calls instead of dropping them;
    /// empty closures (`v.iter().map(|x| x * 2)`) stay out of the graph.
    const ANONYMOUS: Option<Anonymous> = Some(Anonymous {
        capture: "function.anonymous",
        call_kinds: &["call_expression"],
        key: AnonKey::Span,
    });
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[
        (
            "blind.transmute_fn",
            (
                "rs-transmute-fn",
                "transmute::<_, fn(...)>(ptr) — bit-cast to a function pointer; target function not statically determinable",
            ),
        ),
        (
            "blind.libloading_get",
            (
                "rs-libloading-get",
                "libloading::Library::get(...) — dynamic symbol load from dlopen'd library; target function resolved at runtime",
            ),
        ),
    ];
    const SCOPE_LANGUAGE: ScopeLanguage = ScopeLanguage::Fixed(Language::Rust);
    const FIELD_READS: Option<FieldReads> = Some(FieldReads {
        member_kinds: &["field_expression"],
        names: NameRefSpec {
            ident_kinds: &["identifier", "shorthand_field_identifier"],
            kind: NameRefKind::Value,
            binding_fields: &[("const_item", "name"), ("static_item", "name")],
            skip_kinds: &[
                "use_declaration",
                "attribute_item",
                "inner_attribute_item",
                "type_arguments",
            ],
            ..NameRefSpec::EMPTY
        },
    });
    const VARIANT_REFS: Option<VariantRefSpec> = Some(VariantRefSpec {
        // `E::B(..)` / `E::C` / tuple patterns, and the name of a
        // struct-like `E::A { .. }` expression or pattern.
        path_kinds: &["scoped_identifier", "scoped_type_identifier"],
        label_parents: &[],
        bare_label_parents: &[],
        bare_expressions: false,
        token_trees: true,
    });
    const EVENT_TOPICS: &'static [EventTopicConfig] =
        &[REDIS_RUST, KAFKA_RUST, RABBITMQ_RUST, SQS_RUST];
    const TX: TxRule = TxRule::Nodes {
        collect: collect_rust_transactional_scopes,
        kinds: &[NodeKind::Function, NodeKind::Method],
    };
    const STEPS: &'static [Step<RustHook>] = &[
        Step::fixed(Pass::Query, Op::Query),
        Step::hook(Pass::Imports, Effect::Neutral, RustHook::NestedUse),
        Step::hook(Pass::Calls, Effect::Nodes, RustHook::Calls),
        Step::fixed(Pass::Walk, Op::FieldReads),
        Step::fixed(Pass::Walk, Op::VariantRefs),
        Step::hook(Pass::Walk, Effect::Neutral, RustHook::Indirect),
        Step::hook(Pass::Build, Effect::Neutral, RustHook::Frameworks),
        Step::fixed(Pass::Walk, Op::FunctionMeta),
        Step::fixed(Pass::Walk, Op::EventTopicsRequery),
        // FU-016: fill module ownership for items inside `mod foo { ... }`.
        // `enclosing_impl_type` already populated `impl Foo` method ownership;
        // `stamp_owner_class_by_span` preserves those (see the `is_none()` guard
        // in framework_helpers) and only fills the gap for module-resident
        // free items, so `scope_defines::Pass2` can emit `Module(foo) → bar`.
        Step::fixed(Pass::Build, Op::StampOwners),
        // Last owner step: a nested `fn` or closure node has a lexical
        // parent and no owner (invariant I1), whatever the steps above set.
        Step::fixed(Pass::Build, Op::ClearScopedOwners),
        Step::fixed(Pass::Build, Op::TxScopes),
    ];

    fn query() -> anyhow::Result<Query> {
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        Ok(Query::new(
            &tree_sitter_rust::LANGUAGE.into(),
            &query_source,
        )?)
    }

    fn role(capture: &str) -> Option<RustRole> {
        Some(match capture {
            "import.name" => RustRole::ImportName,
            "import.source" => RustRole::ImportSource,
            "import.alias" => RustRole::ImportAlias,
            "function" => RustRole::Root(NodeKind::Function),
            "struct" => RustRole::Root(NodeKind::Struct),
            "enum" => RustRole::Root(NodeKind::Enum),
            "enum_variant_node" => RustRole::Root(NodeKind::EnumVariant),
            "trait" => RustRole::Root(NodeKind::Trait),
            "module" => RustRole::Root(NodeKind::Module),
            "type_alias" => RustRole::Root(NodeKind::Typedef),
            "const_decl" => RustRole::Root(NodeKind::Const),
            "impl_block" => RustRole::Root(NodeKind::Impl),
            "macro_def" => RustRole::Root(NodeKind::Macro),
            "property" => RustRole::Root(NodeKind::Property),
            "method" => RustRole::Root(NodeKind::Method),
            "export" => RustRole::Export,
            "heritage" => RustRole::Heritage,
            "type" => RustRole::TypeAnnotation,
            "decorator" => RustRole::Decorator,
            "axum.route.handler" => RustRole::AxumHandler,
            "actix.route.method" => RustRole::ActixMethod,
            "actix.route.handler" => RustRole::ActixHandler,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: RustRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut RustMatch<'t>,
        f: &mut RustFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        match r {
            RustRole::ImportName => m.import_name = Some(node),
            RustRole::ImportSource => m.import_src = Some(node),
            RustRole::ImportAlias => m.import_alias = Some(node),
            RustRole::Root(kind) => {
                d.root = Some(node);
                d.kind = Some(kind);
            }
            RustRole::Export => m.is_exported = true,
            RustRole::Heritage => {
                if let Ok(h_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    m.heritage.push(trait_path(h_str));
                }
            }
            RustRole::TypeAnnotation => {
                if let Ok(t_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    m.type_annotation = Some(t_str.to_string());
                }
            }
            RustRole::Decorator => {
                if let Ok(d_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    m.decorators.push(d_str.to_string());
                }
            }
            RustRole::AxumHandler => {
                if let Ok(h_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    f.axum_handler_captures.push((
                        h_str.to_string(),
                        node.start_byte(),
                        node.end_byte(),
                        node_span(&node),
                    ));
                }
            }
            RustRole::ActixMethod => m.actix_method = Some(node),
            RustRole::ActixHandler => m.actix_handler = Some(node),
        }
    }

    fn end_match<'t>(
        d: DeclSlot<'t>,
        m: &mut RustMatch<'t>,
        f: &mut RustFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        let RustMatch {
            is_exported,
            heritage,
            type_annotation,
            decorators,
            import_name,
            import_src,
            import_alias,
            actix_method,
            actix_handler,
        } = std::mem::take(m);

        if let (Some(method_node), Some(handler_node)) = (actix_method, actix_handler) {
            if let (Ok(method_str), Ok(handler_str)) = (
                std::str::from_utf8(&source[method_node.start_byte()..method_node.end_byte()]),
                std::str::from_utf8(&source[handler_node.start_byte()..handler_node.end_byte()]),
            ) {
                f.actix_handler_captures.push((
                    method_str.to_string(),
                    handler_str.to_string(),
                    node_span(&handler_node),
                ));
            }
        }

        if let (Some(n), Some(k), Some(root)) = (d.name, d.kind, d.root) {
            if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                let impl_name;
                let name_str = if k == NodeKind::Impl {
                    // An impl whose self type has no name emits nothing,
                    // the import of this match included.
                    let Some(name) = impl_self_type(&root, source) else {
                        return;
                    };
                    impl_name = name;
                    impl_name.as_str()
                } else {
                    name_str
                };
                if matches!(k, NodeKind::Function | NodeKind::Method) {
                    f.fn_spans
                        .push((name_str.to_string(), root.start_byte()..root.end_byte()));
                }
                // Dedup by (name, span): inherent-impl + trait-impl queries
                // both fire on `impl Trait for Type { fn m() {} }`. Keep the
                // higher-priority kind (Method/Constructor > Function).
                let span = node_span(&root);
                let new_priority = match k {
                    NodeKind::Constructor => 3,
                    NodeKind::Method => 2,
                    NodeKind::Function => 1,
                    _ => 0,
                };

                // At-emit-time parent-chain walk to set owner_class, which
                // is one of the four UID inputs.  Each kind has different
                // ancestry semantics:
                //
                // • Method/Constructor/Function inside an impl block → the
                //   impl's concrete type (e.g. "Dog").  A function nested in
                //   a callable body is scoped: `clear_scoped_owners` drops
                //   this owner, and its uid hashes its lexical parent, so a
                //   local `fn helper()` doesn't collide with a top-level
                //   `fn helper()`.
                //
                // • Impl block itself → trait name for trait-impls
                //   (e.g. "Display"), empty string for inherent impls.
                //   This distinguishes `impl Dog {}` from
                //   `impl Display for Dog {}` in the same file.
                //
                // • Property (struct/enum field) → enclosing struct/enum name
                //   so that `field: T` in two different structs does not
                //   collide.
                //
                // • Typedef (associated type in impl) → impl self-type, so
                //   `type Error` in two different impls in the same file
                //   does not collide.
                //
                // • Const/Macro nested inside a function → enclosing function
                //   name, so a function-local `const X` does not collide with
                //   a file-level `const X`.
                let owner = match k {
                    NodeKind::Method | NodeKind::Constructor | NodeKind::Function => {
                        enclosing_impl_type(root, source)
                    }
                    NodeKind::Impl => {
                        // Trait name for trait-impls; empty string for
                        // inherent impls (ensures `impl Dog` ≠ `impl Trait
                        // for Dog` in UID space while keeping inherent-impl
                        // nodes distinct from trait-impl nodes).
                        Some(impl_trait_name(&root, source).unwrap_or_default())
                    }
                    NodeKind::Property => enclosing_struct_type(root, source),
                    NodeKind::Typedef => {
                        // Associated types can appear in impl blocks or
                        // trait definitions.  Use the trait name (for trait-
                        // impls) or trait definition name so that `type Error`
                        // in `impl Encoder for Http` and `type Error` in
                        // `impl Decoder for Http` get distinct owner_class.
                        // Falls back to None for top-level type aliases.
                        enclosing_impl_or_trait_context(root, source)
                    }
                    NodeKind::Const => enclosing_function_name(root, source)
                        .or_else(|| enclosing_impl_type(root, source)),
                    NodeKind::Macro => {
                        // Const/macro inside a function → use function name.
                        enclosing_function_name(root, source)
                    }
                    NodeKind::EnumVariant => {
                        // Variant → enclosing enum name. Walk parent chain
                        // to find enum_item, then extract its name.
                        enclosing_enum_name(root, source)
                    }
                    _ => None,
                };

                let mut merged = false;
                if new_priority > 0 {
                    for existing in cx.nodes.iter_mut() {
                        if existing.name == name_str && existing.span == span {
                            let existing_priority = match existing.kind {
                                NodeKind::Constructor => 3,
                                NodeKind::Method => 2,
                                NodeKind::Function => 1,
                                _ => 0,
                            };
                            if existing_priority > 0 && new_priority > existing_priority {
                                existing.kind = k;
                            }
                            if existing_priority > 0 {
                                // Preserve owner_class if not already set.
                                if existing.owner_class.is_none() {
                                    existing.owner_class = owner.clone();
                                }
                                // Merge decorators from this match (attribute-aware
                                // patterns in frameworks.scm fire after the base
                                // queries.scm pattern; the base push has empty
                                // decorators, the attribute pattern carries them).
                                for d in &decorators {
                                    if !existing.decorators.contains(d) {
                                        existing.decorators.push(d.clone());
                                    }
                                }
                                merged = true;
                            }
                            break;
                        }
                    }
                }
                if !merged {
                    cx.nodes.push(RawNode {
                        decorators,
                        is_exported,
                        heritage,
                        type_annotation,
                        owner_class: owner,
                        ..node::decl(
                            name_str.to_string(),
                            k,
                            span,
                            &source[root.start_byte()..root.end_byte()],
                        )
                    });
                }
            }
        }

        if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
            if let (Ok(name_str), Ok(src_str)) = (
                std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]),
                std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
            ) {
                let alias = if let Some(a_node) = import_alias {
                    std::str::from_utf8(&source[a_node.start_byte()..a_node.end_byte()])
                        .ok()
                        .map(|s| s.to_string())
                } else {
                    None
                };

                cx.imports.push(driver::imports::raw(
                    src_str.to_string(),
                    name_str.to_string(),
                    alias,
                    None,
                ));
            }
        }
    }

    fn hook<'t>(h: RustHook, _c: &Compiled<Self>, f: &mut RustFile, cx: &mut FileCx<'t>) {
        let root = cx.tree.root_node();
        let source = cx.source;
        match h {
            RustHook::NestedUse => push_nested_use_imports(root, source, &mut cx.imports),
            RustHook::Calls => {
                // Extract call sites with receiver-type binding. Replaces the shared
                // `extract_calls` for Rust so `self.method()` inside an impl block and
                // `obj.method()` with locally-typed `obj` are recorded as `Type.method`
                // for the resolver's qualifier-scoped (Tier 2.5) lookup.
                let impl_map = build_impl_map(root, source);
                let local_types = collect_local_types(root, source, &impl_map);
                let (raw_path_literals, raw_sql_refs) = extract_rust_calls_and_path_literals(
                    root,
                    source,
                    &mut cx.nodes,
                    &local_types,
                    &mut cx.call_sites,
                );
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
                f.local_types = Some(local_types);
            }
            RustHook::Indirect => {
                // `collect_rust_indirect_param_types` captures fn(...)  and &dyn Trait
                // types that `bare_type_name` (used by LocalTypes) discards.
                let mut param_types = collect_rust_indirect_param_types(root, source);
                // Merge LocalTypes bindings (for typed let bindings already collected).
                if let Some(local_types) = &f.local_types {
                    for scope in local_types.scopes() {
                        for (var, ty) in &scope.bindings {
                            param_types.entry(var.clone()).or_insert_with(|| ty.clone());
                        }
                        if let Some(ref st) = scope.self_type {
                            param_types
                                .entry("self".to_string())
                                .or_insert_with(|| st.clone());
                        }
                    }
                }
                cx.call_metas =
                    detect_rust_indirect(root, source, &cx.nodes, &param_types, &cx.call_sites);
            }
            RustHook::Frameworks => {
                // Framework-presence gates: only claim Axum/Actix refs when the file
                // actually `use`s the matching crate. Saves us from false positives in
                // files that happen to define a `get()` fn or use `#[get]` from another
                // crate.
                const AXUM_REQUIRED: &[&str] = &["axum"];
                const ACTIX_REQUIRED: &[&str] = &["actix_web", "actix"];
                let has_axum = has_import_from(&cx.imports, AXUM_REQUIRED);
                let has_actix = has_import_from(&cx.imports, ACTIX_REQUIRED);

                // Pick the innermost (smallest) function that contains the capture.
                let axum_handler_captures = std::mem::take(&mut f.axum_handler_captures);
                cx.framework_refs.reserve(axum_handler_captures.len());
                if has_axum {
                    for (handler_name, cap_start, cap_end, span) in axum_handler_captures {
                        let enclosing = f
                            .fn_spans
                            .iter()
                            .filter(|(_, range)| range.start <= cap_start && cap_end <= range.end)
                            .min_by_key(|(_, range)| range.end - range.start);
                        if let Some((fn_name, _)) = enclosing {
                            cx.framework_refs.push(RawFrameworkRef {
                                source_name: fn_name.clone(),
                                target_name: handler_name,
                                confidence: framework_confidence::AXUM_ROUTE,
                                reason: "axum-route-handler".to_string(),
                                span,
                            });
                        }
                    }
                }

                // Actix attribute-macro routes: emit one ref per #[verb] → fn pair.
                // No natural source — use module-level sentinel; confidence 0.9 (syntactic, unambiguous).
                if has_actix {
                    for (method, handler, span) in std::mem::take(&mut f.actix_handler_captures) {
                        cx.framework_refs.push(RawFrameworkRef {
                            source_name: MODULE_LEVEL_SOURCE.to_string(),
                            target_name: handler,
                            confidence: framework_confidence::ACTIX_ROUTE,
                            reason: format!("actix-route-{}", method),
                            span,
                        });
                    }
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
        crate::function_meta::rust_lang::extract(root, source, nodes, category)
    }
}

/// The `use` forms the import query does not reach: a path inside a list
/// (`use a::{b::C}`), a nested list, `self` in a list, a glob (`use a::*`)
/// and a list with no path (`use {a::B}`). One `RawImport` per item, with
/// the module path as `source`. The query keeps the flat forms
/// (`use a::b::C`, `use a::{C, D as E}`, `use a as b`), so this skips them.
fn push_nested_use_imports(root: tree_sitter::Node, source: &[u8], out: &mut Vec<RawImport>) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() != "use_declaration" {
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
            continue;
        }
        let Some(arg) = node.child_by_field_name("argument") else {
            continue;
        };
        match arg.kind() {
            // The query reads the list of `use a::{..}` only when it has a
            // path; `use ::{a, b}` has none.
            "scoped_use_list" => {
                let covered = arg.child_by_field_name("path").is_some();
                use_list_items(arg, "", covered, source, out)
            }
            "use_list" => list_items(arg, "", false, source, out),
            "use_wildcard" => push_wildcard(arg, "", source, out),
            _ => {}
        }
    }
}

fn node_text<'s>(node: tree_sitter::Node, source: &'s [u8]) -> &'s str {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).unwrap_or("")
}

fn join_path(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        path.to_string()
    } else {
        format!("{prefix}::{path}")
    }
}

/// `prefix::path::{...}`: the items of the list under `prefix::path`.
/// `top` marks the list the import query already reads item by item.
fn use_list_items(
    node: tree_sitter::Node,
    prefix: &str,
    top: bool,
    source: &[u8],
    out: &mut Vec<RawImport>,
) {
    let path = node
        .child_by_field_name("path")
        .map(|p| join_path(prefix, node_text(p, source)))
        .unwrap_or_else(|| prefix.to_string());
    if let Some(list) = node.child_by_field_name("list") {
        list_items(list, &path, top, source, out);
    }
}

fn list_items(
    list: tree_sitter::Node,
    prefix: &str,
    covered: bool,
    source: &[u8],
    out: &mut Vec<RawImport>,
) {
    let mut cursor = list.walk();
    let items: Vec<_> = list.named_children(&mut cursor).collect();
    for item in items {
        match item.kind() {
            "identifier" if !covered => push_import(out, prefix, node_text(item, source), None),
            "self" => push_self(out, prefix, None),
            "scoped_identifier" => {
                let (path, name) = split_last(node_text(item, source));
                push_import(out, &join_path(prefix, path), name, None);
            }
            "use_as_clause" => {
                let (Some(path), Some(alias)) = (
                    item.child_by_field_name("path"),
                    item.child_by_field_name("alias"),
                ) else {
                    continue;
                };
                if path.kind() == "identifier" && covered {
                    continue;
                }
                if path.kind() == "self" {
                    push_self(out, prefix, Some(node_text(alias, source)));
                    continue;
                }
                let (module, name) = split_last(node_text(path, source));
                push_import(
                    out,
                    &join_path(prefix, module),
                    name,
                    Some(node_text(alias, source)),
                );
            }
            "scoped_use_list" => use_list_items(item, prefix, false, source, out),
            "use_list" => list_items(item, prefix, false, source, out),
            "use_wildcard" => push_wildcard(item, prefix, source, out),
            _ => {}
        }
    }
}

/// One import of `name` from `module`; a bare top-level item (`use {c}`)
/// is its own module, as the query records `use c`.
fn push_import(out: &mut Vec<RawImport>, module: &str, name: &str, alias: Option<&str>) {
    if name.is_empty() {
        return;
    }
    out.push(RawImport {
        source: if module.is_empty() { name } else { module }.to_string(),
        imported_name: name.to_string(),
        alias: alias.map(str::to_string),
        binding_kind: None,
    });
}

/// `use a::m::{self}` imports the module `m` itself: recorded as
/// `use a::m;` is, source `a`, name `m`.
fn push_self(out: &mut Vec<RawImport>, prefix: &str, alias: Option<&str>) {
    match prefix.rsplit_once("::") {
        Some((parent, module)) => push_import(out, parent, module, alias),
        None => push_import(out, prefix, prefix, alias),
    }
}

/// `a::b::C` → (`a::b`, `C`); a single segment has an empty module path.
fn split_last(path: &str) -> (&str, &str) {
    path.rsplit_once("::").unwrap_or(("", path))
}

fn push_wildcard(node: tree_sitter::Node, prefix: &str, source: &[u8], out: &mut Vec<RawImport>) {
    let module = node
        .named_child(0)
        .map(|p| join_path(prefix, node_text(p, source)))
        .unwrap_or_else(|| prefix.to_string());
    if !module.is_empty() {
        push_import(out, &module, "*", None);
    }
}
