use super::receiver_types::extract_csharp_calls_and_path_literals;
use super::spec::CSharpSpec;
use crate::framework_confidence;
use crate::framework_helpers::{
    collect_dotnet_transactional_scopes, detect_ast_framework_patterns, node_span, push_blind_spot,
    FrameworkPatternSpec,
};
use crate::function_meta::subtree_contains_kind;
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::algorithms::process_trace::is_test_path;
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{BlindSpot, LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

/// Blind-spot kind/hint pairs. Order matches the capture-index dispatch
/// in `parse_file`.
const BLIND_SPEC: &[(&str, &str)] = &[
    (
        "cs-activator-create-instance",
        "Activator.CreateInstance(<expr>) — runtime type instantiation; created object's type/body is not statically determinable",
    ),
    (
        "cs-method-invoke",
        "<expr>.Invoke(...) — reflective method invocation; target method body resolved at runtime via MethodInfo",
    ),
];

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

/// Whether `node`'s subtree contains a C# `invocation_expression`. Gates
/// emission of `<anonymous>` callback nodes so empty closures (e.g.
/// `list.Select(x => x.Name)`) stay out of the graph.
fn body_has_call(node: tree_sitter::Node) -> bool {
    subtree_contains_kind(node, "invocation_expression")
}

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_c_sharp::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
struct CSharpCaptureIndices {
    import_name: Option<u32>,
    import_source: Option<u32>,
    import_alias: Option<u32>,
    export: Option<u32>,
    heritage: Option<u32>,
    type_: Option<u32>,
    decorator: Option<u32>,
    override_marker: Option<u32>,
    function: Option<u32>,
    class: Option<u32>,
    method: Option<u32>,
    interface: Option<u32>,
    property: Option<u32>,
    variable: Option<u32>,
    constructor: Option<u32>,
    namespace: Option<u32>,
    enum_: Option<u32>,
    enum_member_node: Option<u32>,
    struct_: Option<u32>,
    // BlindSpot captures (FU-001 P2c).
    blind_activator_create: Option<u32>,
    blind_method_invoke: Option<u32>,
    function_anonymous: Option<u32>,
}

pub struct CSharpProvider {
    query: Query,
    /// Capture index → NodeKind mapping, pre-resolved from
    /// `CSharpSpec::CAPTURE_KIND` at provider construction. The hot loop
    /// looks up by integer index (cap.index as usize) — equivalent perf
    /// to the previous hard-coded if-chain, but the source of truth
    /// lives in `spec.rs` const tables.
    capture_kind_by_idx: Vec<Option<NodeKind>>,
    /// All capture indices resolved once at provider construction.
    /// Cuts ~18 `query.capture_index_for_name()` calls per parse_file.
    indices: CSharpCaptureIndices,
}

impl CSharpProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_c_sharp::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;

        // Pre-resolve capture-name → NodeKind from the spec table so the
        // hot loop stays an integer-index lookup (no per-capture string
        // compare). Capture names not in the spec map yield None and
        // fall through to the metadata-only branches below (heritage,
        // decorator, etc.).
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| CSharpSpec::CAPTURE_KIND.get(name).copied())
            .collect();

        let indices = CSharpCaptureIndices {
            import_name: query.capture_index_for_name("import.name"),
            import_source: query.capture_index_for_name("import.source"),
            import_alias: query.capture_index_for_name("import.alias"),
            export: query.capture_index_for_name("export"),
            heritage: query.capture_index_for_name("heritage"),
            type_: query.capture_index_for_name("type"),
            decorator: query.capture_index_for_name("decorator"),
            override_marker: query.capture_index_for_name("override_marker"),
            function: query.capture_index_for_name("function"),
            class: query.capture_index_for_name("class"),
            method: query.capture_index_for_name("method"),
            interface: query.capture_index_for_name("interface"),
            property: query.capture_index_for_name("property"),
            variable: query.capture_index_for_name("variable"),
            constructor: query.capture_index_for_name("constructor"),
            namespace: query.capture_index_for_name("namespace"),
            enum_: query.capture_index_for_name("enum"),
            enum_member_node: query.capture_index_for_name("enum_member_node"),
            struct_: query.capture_index_for_name("struct"),
            blind_activator_create: query.capture_index_for_name("blind.activator_create"),
            blind_method_invoke: query.capture_index_for_name("blind.method_invoke"),
            function_anonymous: query.capture_index_for_name("function.anonymous"),
        };

        Ok(Self {
            query,
            capture_kind_by_idx,
            indices,
        })
    }
}

impl LanguageProvider for CSharpProvider {
    fn name(&self) -> &'static str {
        "c_sharp"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        // Vec + idx-map pattern — see java/parser.rs same-site note.
        let mut nodes: Vec<RawNode> = Vec::new();
        let mut node_id_to_idx: rustc_hash::FxHashMap<usize, usize> =
            rustc_hash::FxHashMap::default();
        let mut imports = Vec::new();
        let mut blind_spots: Vec<BlindSpot> = Vec::new();
        let is_test_file = is_test_path(path.to_str().unwrap_or(""));

        // Dedup: the same lambda/closure can be captured by multiple matches.
        // Span set guards against pushing the same `<anonymous>` node twice.
        let mut anon_emitted_spans: std::collections::HashSet<(u32, u32, u32, u32)> =
            std::collections::HashSet::new();

        let idx = &self.indices;
        let idx_import_name = idx.import_name;
        let idx_import_source = idx.import_source;
        let idx_import_alias = idx.import_alias;

        let idx_export = idx.export;
        let idx_heritage = idx.heritage;
        let idx_type = idx.type_;
        let idx_decorator = idx.decorator;
        let idx_override_marker = idx.override_marker;

        let idx_function = idx.function;
        let idx_class = idx.class;
        let idx_method = idx.method;
        let idx_interface = idx.interface;
        let idx_property = idx.property;
        let idx_variable = idx.variable;
        let idx_constructor = idx.constructor;
        let idx_namespace = idx.namespace;
        let idx_enum = idx.enum_;
        let idx_enum_member_node = idx.enum_member_node;
        let idx_struct = idx.struct_;

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;

            let mut import_name = None;
            let mut import_src = None;
            let mut import_alias = None;

            let mut is_exported = false;
            let mut heritage_list = Vec::new();
            let mut type_annotation = None;
            let mut decorators = Vec::new();

            for cap in m.captures {
                let cap_idx = cap.index;
                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap_idx as usize)
                    .copied()
                    .flatten()
                {
                    // Single config-driven dispatch replaces the nine
                    // explicit Class/Method/Interface/Function/Property/
                    // Variable/Constructor/Namespace/Enum arms.
                    // Source of truth: CSharpSpec::CAPTURE_KIND in spec.rs.
                    name_node = Some(cap.node);
                    kind = Some(k_from_spec);
                } else if Some(cap_idx) == idx_import_name {
                    import_name = Some(cap.node);
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_import_alias {
                    if let Ok(text) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        import_alias = Some(text.to_string());
                    }
                } else if Some(cap_idx) == idx_export {
                    if let Ok(text) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        if text == "public" {
                            is_exported = true;
                        }
                    }
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(text) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage_list.push(text.to_string());
                    }
                } else if Some(cap_idx) == idx_type {
                    if let Ok(text) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        type_annotation = Some(text.to_string());
                    }
                } else if Some(cap_idx) == idx_decorator {
                    if let Ok(text) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(text.to_string());
                    }
                } else if Some(cap_idx) == idx_override_marker {
                    decorators.push("__override__".to_string());
                } else if Some(cap_idx) == idx.blind_activator_create {
                    push_blind_spot(
                        &mut blind_spots,
                        BLIND_SPEC[0],
                        &cap.node,
                        path,
                        is_test_file,
                    );
                } else if Some(cap_idx) == idx.blind_method_invoke {
                    push_blind_spot(
                        &mut blind_spots,
                        BLIND_SPEC[1],
                        &cap.node,
                        path,
                        is_test_file,
                    );
                } else if Some(cap_idx) == idx.function_anonymous {
                    // Anonymous callback (lambda / delegate in argument position).
                    // Emit an `<anonymous>` Function node only when the body holds
                    // a call — attach_to_enclosing can then host those calls instead
                    // of dropping them when no named enclosing scope exists.
                    // Empty closures (e.g. `list.Select(x => x.Name)`) stay out of
                    // the graph.
                    if body_has_call(cap.node) {
                        let span = node_span(&cap.node);
                        if anon_emitted_spans.insert(span) {
                            nodes.push(RawNode {
                                decorators: Vec::new(),
                                is_exported: false,
                                heritage: Vec::new(),
                                type_annotation: None,
                                name: format!("<anonymous:{}:{}>", span.0 + 1, span.1),
                                kind: NodeKind::Function,
                                span,
                                calls: Vec::new(),
                                owner_class: None,
                                content_hash: ecp_core::uid::xxh3_64_bytes(
                                    &source[cap.node.start_byte()..cap.node.end_byte()],
                                ),
                            });
                        }
                    }
                } else if (Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_method
                    || Some(cap_idx) == idx_interface
                    || Some(cap_idx) == idx_property
                    || Some(cap_idx) == idx_variable
                    || Some(cap_idx) == idx_constructor
                    || Some(cap_idx) == idx_namespace
                    || Some(cap_idx) == idx_enum
                    || Some(cap_idx) == idx_enum_member_node
                    || Some(cap_idx) == idx_struct)
                    && root_span_node.is_none()
                {
                    root_span_node = Some(cap.node);
                }
            }

            // Reclassify class declarations inheriting from `Attribute` (or any
            // base whose name ends in `Attribute`) as NodeKind::Annotation —
            // C# attribute-class convention. Heritage check alone is sufficient
            // because `Attribute` suffix in a base type is the load-bearing
            // signal; regular classes don't inherit from `*Attribute` types.
            if kind == Some(NodeKind::Class)
                && heritage_list.iter().any(|h| h.ends_with("Attribute"))
            {
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
                        let txt =
                            std::str::from_utf8(&source[s.start_byte()..s.end_byte()]).ok()?;
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
                    let start = root.start_position();
                    let end = root.end_position();

                    // For Property + Variable nodes, multiple declarators
                    // share the same root node id (`int x, y, z;`); key on
                    // the identifier node so each declarator is distinct.
                    let node_id = if matches!(k, NodeKind::Property | NodeKind::Variable) {
                        n.id()
                    } else {
                        root.id()
                    };
                    let idx = *node_id_to_idx.entry(node_id).or_insert_with(|| {
                        let i = nodes.len();
                        nodes.push(RawNode {
                            decorators: vec![],
                            is_exported,
                            heritage: Vec::new(),
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
                            owner_class: None,
                            content_hash: ecp_core::uid::xxh3_64_bytes(
                                &source[root.start_byte()..root.end_byte()],
                            ),
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
                    imports.push(RawImport {
                        alias: import_alias,
                        imported_name: name_str.to_string(),
                        source: src_str.to_string(),
                        binding_kind: None,
                    });
                }
            }
        }

        // `nodes` already in source order — Vec + idx-map at parse-loop start.

        // Extract call sites with receiver-type binding for `this.Foo()`,
        // `base.Foo()`, and typed-variable `obj.Foo()` patterns; same DFS
        // also collects path-shaped string literals.
        let raw_path_literals =
            extract_csharp_calls_and_path_literals(tree.root_node(), source, &mut nodes);

        let framework_refs = detect_ast_framework_patterns(source, CSHARP_FRAMEWORKS);

        let file_category =
            crate::resolution::builder::determine_category(path.to_str().unwrap_or(""));
        let raw_function_metas =
            crate::function_meta::csharp::extract(tree.root_node(), source, &nodes, file_category);

        crate::framework_helpers::stamp_owner_class_by_span(&mut nodes);
        let tx_scopes = collect_dotnet_transactional_scopes(
            &nodes,
            &[NodeKind::Method, NodeKind::Constructor, NodeKind::Function],
        );
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
            tx_scopes,
            path_literals: (!raw_path_literals.is_empty())
                .then(|| raw_path_literals.into_boxed_slice()),
            call_metas: vec![],
            raw_function_metas,
        })
    }
}
