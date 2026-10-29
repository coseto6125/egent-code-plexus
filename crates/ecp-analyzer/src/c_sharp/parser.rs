use super::receiver_types::extract_csharp_calls_and_path_literals;
use super::spec::CSharpSpec;
use crate::calls::{NameRefKind, NameRefSpec, VariantRefSpec};
use crate::driver::{
    self, imports, node, AnonKey, Anonymous, Compiled, DeclSlot, Effect, FieldReads, FileCx,
    KindCapture, Op, ParseSpec, Step, TxRule,
};
use crate::framework_confidence;
use crate::framework_helpers::{
    collect_dotnet_transactional_scopes, detect_ast_framework_patterns, FrameworkPatternSpec,
};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawFunctionMeta, RawNode, OVERRIDE_SENTINEL};
use ecp_core::graph::{FileCategory, NodeKind};
use rustc_hash::FxHashMap;
use std::path::Path;
use tree_sitter::{Node, Query, Tree};

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

/// The number of type parameters a class, struct, interface, record or
/// delegate declaration lists (`class Conv<T, U>` is 2,
/// `delegate R Func<T, R>(T a)` is 2); 0 for any other node.
fn generic_arity(decl: tree_sitter::Node<'_>) -> u32 {
    let mut cursor = decl.walk();
    let Some(list) = decl
        .children(&mut cursor)
        .find(|child| child.kind() == "type_parameter_list")
    else {
        return 0;
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter(|param| param.kind() == "type_parameter")
        .count() as u32
}

/// Whether a type declaration carries the `partial` modifier.
fn is_partial(decl: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let mut cursor = decl.walk();
    let partial = decl
        .children(&mut cursor)
        .any(|child| child.kind() == "modifier" && child.utf8_text(source) == Ok("partial"));
    partial
}

/// A declaration's `name` field text with the whitespace removed
/// (`Lib . Json` is `Lib.Json`).
fn name_text(decl: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let name = decl.child_by_field_name("name")?.utf8_text(source).ok()?;
    Some(name.chars().filter(|c| !c.is_whitespace()).collect())
}

/// The file-scoped namespace (`namespace Lib.Json;`) of a compilation unit,
/// looked up through the preprocessor blocks that may wrap it. C# allows one
/// per file, and no block namespace beside it.
fn file_scoped_namespace(unit: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = unit.walk();
    let children: Vec<tree_sitter::Node<'_>> = unit.named_children(&mut cursor).collect();
    children.into_iter().find_map(|child| match child.kind() {
        "file_scoped_namespace_declaration" => name_text(child, source),
        kind if kind.starts_with("preproc_") => file_scoped_namespace(child, source),
        _ => None,
    })
}

/// The scope that declares the type `decl`, as
/// [`RawNode::partial_marker`] spells it: the namespace (block namespaces
/// joined with `.`, or the file-scoped one), then each enclosing type as
/// `+Name`, with a backtick and its generic arity when above 0. The
/// preprocessor blocks and declaration lists between them add nothing.
fn declaring_scope(decl: tree_sitter::Node<'_>, source: &[u8]) -> String {
    let mut namespaces: Vec<String> = Vec::new();
    let mut types: Vec<String> = Vec::new();
    let mut unit = None;
    let mut parent = decl.parent();
    while let Some(node) = parent {
        match node.kind() {
            "namespace_declaration" => namespaces.extend(name_text(node, source)),
            "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "record_declaration" => {
                if let Some(name) = name_text(node, source) {
                    types.push(match generic_arity(node) {
                        0 => name,
                        arity => format!("{name}`{arity}"),
                    });
                }
            }
            "compilation_unit" => unit = Some(node),
            _ => {}
        }
        parent = node.parent();
    }
    namespaces.extend(unit.and_then(|unit| file_scoped_namespace(unit, source)));
    namespaces.reverse();
    let mut scope = namespaces.join(".");
    for ty in types.iter().rev() {
        scope.push('+');
        scope.push_str(ty);
    }
    scope
}

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_c_sharp::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}

pub struct CSharpProvider {
    compiled: Compiled<CSharpSpec>,
}

impl CSharpProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
        })
    }
}

impl LanguageProvider for CSharpProvider {
    fn name(&self) -> &'static str {
        "c_sharp"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures C# handles itself. A root capture sets the declaration root
/// once per match; the kind captures in `CSharpSpec::CAPTURE_KIND` set name
/// and kind.
#[derive(Clone, Copy)]
pub enum CSharpRole {
    Root,
    /// `@destructor` and `@destructor.name` are both mandatory in the one
    /// pattern that uses them, so the root marks the `~` name prefix.
    DestructorRoot,
    /// `@event_field` and `@event_field.name` are both mandatory in the one
    /// pattern that uses them, so the root marks the per-declarator keying.
    EventFieldRoot,
    Export,
    Heritage,
    Type,
    Decorator,
    OverrideMarker,
    ImportName,
    ImportSource,
    ImportAlias,
    /// Special-member roots: their nodes lack a plain `name:` identifier,
    /// so `end_match` synthesises the name once metadata is collected.
    Operator,
    ConvOperator,
    Indexer,
}

#[derive(Clone, Copy)]
pub enum CSharpHook {
    Calls,
    Frameworks,
}

#[derive(Default)]
pub struct CSharpMatch<'t> {
    import_name: Option<Node<'t>>,
    import_src: Option<Node<'t>>,
    import_alias: Option<String>,
    is_exported: bool,
    heritage: Vec<String>,
    type_annotation: Option<String>,
    decorators: Vec<String>,
    is_destructor_name: bool,
    is_event_field_name: bool,
    deferred_operator: Option<Node<'t>>,
    deferred_conv_operator: Option<Node<'t>>,
    deferred_indexer: Option<Node<'t>>,
}

#[derive(Default)]
pub struct CSharpFile {
    /// Dedup lookup for multi-capture merge only; nodes are pushed in match
    /// order and the map is never iterated.
    node_id_to_idx: FxHashMap<usize, usize>,
}

impl ParseSpec for CSharpSpec {
    type Role = CSharpRole;
    type Match<'t> = CSharpMatch<'t>;
    type File<'t> = CSharpFile;
    type Hook = CSharpHook;

    const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
    /// An anonymous callback (lambda / delegate in argument position) gets an
    /// `<anonymous>` Function node only when its body holds a call, so
    /// attach_to_enclosing can host those calls instead of dropping them when
    /// no named enclosing scope exists. Empty closures (e.g.
    /// `list.Select(x => x.Name)`) stay out of the graph. The same lambda can
    /// be captured by several matches, so the dedupe keys on its span.
    const ANONYMOUS: Option<Anonymous> = Some(Anonymous {
        capture: "function.anonymous",
        call_kinds: &["invocation_expression"],
        key: AnonKey::Span,
    });
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[
        (
            "blind.activator_create",
            (
                "cs-activator-create-instance",
                "Activator.CreateInstance(<expr>) — runtime type instantiation; created object's type/body is not statically determinable",
            ),
        ),
        (
            "blind.method_invoke",
            (
                "cs-method-invoke",
                "<expr>.Invoke(...) — reflective method invocation; target method body resolved at runtime via MethodInfo",
            ),
        ),
    ];
    const FIELD_READS: Option<FieldReads> = Some(FieldReads {
        member_kinds: &["member_access_expression"],
        names: NameRefSpec {
            ident_kinds: &["identifier", "implicit_parameter"],
            kind: NameRefKind::Value,
            binding_fields: &[
                ("foreach_statement", "left"),
                ("catch_declaration", "name"),
                ("declaration_expression", "name"),
                ("declaration_pattern", "name"),
                ("from_clause", "name"),
                ("tuple_pattern", "name"),
                ("parenthesized_variable_designation", "name"),
            ],
            binding_parents: &["let_clause"],
            skip_kinds: &[
                "using_directive",
                "attribute_list",
                "type_argument_list",
                "qualified_name",
                "generic_name",
            ],
            skip_parents: &["labeled_statement", "goto_statement"],
            ..NameRefSpec::EMPTY
        },
    });
    const VARIANT_REFS: Option<VariantRefSpec> = Some(VariantRefSpec {
        path_kinds: &["member_access_expression"],
        label_parents: &[],
        bare_label_parents: &[],
        bare_expressions: false,
        token_trees: false,
    });
    const TX: TxRule = TxRule::Nodes {
        collect: collect_dotnet_transactional_scopes,
        kinds: &[NodeKind::Method, NodeKind::Constructor, NodeKind::Function],
    };
    const STEPS: &'static [Step<CSharpHook>] = &[
        Step::fixed(Pass::Query, Op::Query),
        Step::hook(Pass::Calls, Effect::Nodes, CSharpHook::Calls),
        Step::fixed(Pass::Walk, Op::FieldReads),
        Step::fixed(Pass::Walk, Op::VariantRefs),
        Step::hook(Pass::Walk, Effect::Neutral, CSharpHook::Frameworks),
        Step::fixed(Pass::Walk, Op::FunctionMeta),
        Step::fixed(Pass::Build, Op::StampOwners),
        Step::fixed(Pass::Build, Op::TxScopes),
    ];

    fn query() -> anyhow::Result<Query> {
        Ok(Query::new(
            &tree_sitter_c_sharp::LANGUAGE.into(),
            include_str!("queries.scm"),
        )?)
    }

    fn role(capture: &str) -> Option<CSharpRole> {
        Some(match capture {
            "function" | "class" | "method" | "interface" | "property" | "constructor"
            | "namespace" | "enum" | "enum_member_node" | "struct" | "event_decl" => {
                CSharpRole::Root
            }
            "destructor" => CSharpRole::DestructorRoot,
            "event_field" => CSharpRole::EventFieldRoot,
            "export" => CSharpRole::Export,
            "heritage" => CSharpRole::Heritage,
            "type" => CSharpRole::Type,
            "decorator" => CSharpRole::Decorator,
            "override_marker" => CSharpRole::OverrideMarker,
            "import.name" => CSharpRole::ImportName,
            "import.source" => CSharpRole::ImportSource,
            "import.alias" => CSharpRole::ImportAlias,
            "operator_decl" => CSharpRole::Operator,
            "conv_operator_decl" => CSharpRole::ConvOperator,
            "indexer_decl" => CSharpRole::Indexer,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: CSharpRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut CSharpMatch<'t>,
        _f: &mut CSharpFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        match r {
            CSharpRole::Root => {
                if d.root.is_none() {
                    d.root = Some(node);
                }
            }
            CSharpRole::DestructorRoot => {
                m.is_destructor_name = true;
                if d.root.is_none() {
                    d.root = Some(node);
                }
            }
            CSharpRole::EventFieldRoot => {
                m.is_event_field_name = true;
                if d.root.is_none() {
                    d.root = Some(node);
                }
            }
            CSharpRole::ImportName => m.import_name = Some(node),
            CSharpRole::ImportSource => m.import_src = Some(node),
            CSharpRole::ImportAlias => {
                if let Ok(text) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
                    m.import_alias = Some(text.to_string());
                }
            }
            CSharpRole::Export => {
                if let Ok(text) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
                    if text == "public" {
                        m.is_exported = true;
                    }
                }
            }
            CSharpRole::Heritage => {
                // `(base_list (_)*)` also matches the `#pragma` lines
                // that bracket an obsolete base.
                if node.kind().starts_with("preproc_") {
                    return;
                }
                if let Ok(text) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
                    m.heritage.push(text.to_string());
                }
            }
            CSharpRole::Type => {
                if let Ok(text) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
                    m.type_annotation = Some(text.to_string());
                }
            }
            CSharpRole::Decorator => {
                if let Ok(text) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()]) {
                    m.decorators.push(text.to_string());
                }
            }
            CSharpRole::OverrideMarker => m.decorators.push(OVERRIDE_SENTINEL.to_string()),
            CSharpRole::Operator => m.deferred_operator = Some(node),
            CSharpRole::ConvOperator => m.deferred_conv_operator = Some(node),
            CSharpRole::Indexer => m.deferred_indexer = Some(node),
        }
    }

    fn end_match<'t>(
        d: DeclSlot<'t>,
        m: &mut CSharpMatch<'t>,
        f: &mut CSharpFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        let CSharpMatch {
            import_name,
            import_src,
            import_alias,
            is_exported,
            heritage: heritage_list,
            type_annotation,
            decorators,
            is_destructor_name,
            is_event_field_name,
            deferred_operator,
            deferred_conv_operator,
            deferred_indexer,
        } = std::mem::take(m);
        let (name_node, mut kind, root_span_node) = (d.name, d.kind, d.root);

        // Emit deferred special-member nodes (operator/conv-operator/indexer).
        // These are processed AFTER the capture loop so that decorator/export/type
        // metadata is fully accumulated before the RawNode is pushed.
        let emit_special = |n: Node<'_>,
                            name: String,
                            is_exp: bool,
                            ty: Option<String>,
                            decs: Vec<String>,
                            nodes: &mut Vec<RawNode>,
                            id_map: &mut FxHashMap<usize, usize>| {
            id_map.entry(n.id()).or_insert_with(|| {
                let i = nodes.len();
                nodes.push(RawNode {
                    decorators: decs,
                    is_exported: is_exp,
                    type_annotation: ty,
                    ..node::decl(
                        name,
                        NodeKind::Method,
                        node::span(n),
                        &source[n.start_byte()..n.end_byte()],
                    )
                });
                i
            });
        };
        if let Some(op_node) = deferred_operator {
            // operator_declaration: name = "op_<token>" from the `operator:` field.
            let op_name = op_node
                .child_by_field_name("operator")
                .and_then(|tok| std::str::from_utf8(&source[tok.start_byte()..tok.end_byte()]).ok())
                .map(|tok| format!("op_{tok}"))
                .unwrap_or_else(|| "op_unknown".to_string());
            emit_special(
                op_node,
                op_name,
                is_exported,
                type_annotation.clone(),
                decorators.clone(),
                &mut cx.nodes,
                &mut f.node_id_to_idx,
            );
        }
        if let Some(cv_node) = deferred_conv_operator {
            // conversion_operator_declaration: name = "op_Implicit" / "op_Explicit".
            let conv_name = {
                let mut c = cv_node.walk();
                let found = cv_node.children(&mut c).find_map(|ch| match ch.kind() {
                    "implicit" => Some("op_Implicit"),
                    "explicit" => Some("op_Explicit"),
                    _ => None,
                });
                found.unwrap_or("op_Conversion").to_string()
            };
            emit_special(
                cv_node,
                conv_name,
                is_exported,
                type_annotation.clone(),
                decorators.clone(),
                &mut cx.nodes,
                &mut f.node_id_to_idx,
            );
        }
        if let Some(idx_node) = deferred_indexer {
            // indexer_declaration: canonical name "this[...]".
            emit_special(
                idx_node,
                "this[...]".to_string(),
                is_exported,
                type_annotation.clone(),
                decorators.clone(),
                &mut cx.nodes,
                &mut f.node_id_to_idx,
            );
        }

        // Reclassify class declarations inheriting from `Attribute` (or any
        // base whose name ends in `Attribute`) as NodeKind::Annotation —
        // C# attribute-class convention. Heritage check alone is sufficient
        // because `Attribute` suffix in a base type is the load-bearing
        // signal; regular classes don't inherit from `*Attribute` types.
        if kind == Some(NodeKind::Class) && heritage_list.iter().any(|h| h.ends_with("Attribute")) {
            kind = Some(NodeKind::Annotation);
        }

        if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
            // tree-sitter-c-sharp recovery can bind the wrong identifier
            // to a type's `name:` field when a preproc directive sits
            // between the type keyword and the real name:
            //   class JsonWriter
            //   #if HAVE_ASYNC_DISPOSABLE
            //     : IAsyncDisposable
            // Recovery wraps the real name in an ERROR sibling and binds
            // the post-`#if` identifier to `name:`. When `name` has a
            // preceding ERROR sibling AND the kind is type-like, extract
            // the leading identifier from that ERROR node's text instead.
            let real_name = if matches!(
                k,
                NodeKind::Class | NodeKind::Interface | NodeKind::Annotation
            ) {
                n.prev_sibling().and_then(|s| {
                    if s.kind() != "ERROR" {
                        return None;
                    }
                    let txt = std::str::from_utf8(&source[s.start_byte()..s.end_byte()]).ok()?;
                    let id: String = txt
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if id.is_empty() {
                        None
                    } else {
                        Some(id)
                    }
                })
            } else {
                None
            };
            let name_bytes = real_name.as_deref().map(str::as_bytes);
            let name_result = name_bytes
                .map(|b| std::str::from_utf8(b))
                .unwrap_or_else(|| std::str::from_utf8(&source[n.start_byte()..n.end_byte()]));
            if let Ok(name_str) = name_result {
                // For Property + Variable nodes, multiple declarators
                // share the same root node id (`int x, y, z;`); key on
                // the identifier node so each declarator is distinct.
                // event_field_declaration has the same multiple-declarator
                // pattern (`event EventHandler E, F;`), so same keying.
                let node_id = if matches!(k, NodeKind::Property | NodeKind::Variable)
                    || is_event_field_name
                {
                    n.id()
                } else {
                    root.id()
                };
                let nodes = &mut cx.nodes;
                let idx = *f.node_id_to_idx.entry(node_id).or_insert_with(|| {
                    // Destructors: prefix the bare class name with `~`.
                    let name = if is_destructor_name {
                        format!("~{name_str}")
                    } else {
                        name_str.to_string()
                    };
                    let i = nodes.len();
                    // `Conv<T>` beside `Conv` is a type of its own, and
                    // `Func<T, R>` beside `Func<R>` a delegate of its
                    // own; the identity numbering reads the arity off
                    // this marker. A delegate is the one `Function` that
                    // carries it (`lexical::splits_by_arity`).
                    let arity = if k.declares_type() || root.kind() == "delegate_declaration" {
                        generic_arity(root)
                    } else {
                        0
                    };
                    let mut decorators = match arity {
                        0 => vec![],
                        arity => vec![RawNode::generic_arity_marker(arity)],
                    };
                    // The parts of one `partial` type merge by this
                    // scope (`c_sharp::partial`).
                    if k.declares_type() && is_partial(root, source) {
                        decorators.push(RawNode::partial_marker(&declaring_scope(root, source)));
                    }
                    nodes.push(RawNode {
                        decorators,
                        is_exported,
                        type_annotation: type_annotation.clone(),
                        ..node::decl(
                            name,
                            k,
                            node::span(root),
                            &source[root.start_byte()..root.end_byte()],
                        )
                    });
                    i
                });
                let entry = &mut nodes[idx];

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
                cx.imports.push(imports::raw(
                    src_str.to_string(),
                    name_str.to_string(),
                    import_alias,
                    None,
                ));
            }
        }
    }

    fn hook<'t>(h: CSharpHook, _c: &Compiled<Self>, _f: &mut CSharpFile, cx: &mut FileCx<'t>) {
        match h {
            // Extract call sites with receiver-type binding for `this.Foo()`,
            // `base.Foo()`, and typed-variable `obj.Foo()` patterns; same DFS
            // also collects path-shaped string literals.
            CSharpHook::Calls => {
                let (raw_path_literals, raw_sql_refs) = extract_csharp_calls_and_path_literals(
                    cx.tree.root_node(),
                    cx.source,
                    &mut cx.nodes,
                );
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
            }
            CSharpHook::Frameworks => {
                cx.framework_refs = detect_ast_framework_patterns(cx.source, CSHARP_FRAMEWORKS);
            }
        }
    }

    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta> {
        crate::function_meta::csharp::extract(root, source, nodes, category)
    }
}
