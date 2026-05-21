use super::receiver_types::{build_impl_map, collect_local_types, extract_rust_calls};
use super::spec::RustSpec;
use crate::framework_confidence;
use crate::framework_helpers::{has_import_from, node_span, MODULE_LEVEL_SOURCE};
use crate::indirect_dispatch::{collect_rust_indirect_param_types, detect_rust_indirect};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct RustProvider {
    query: Query,
    indices: RustCaptureIndices,
    /// Capture index → NodeKind mapping, pre-resolved from
    /// `RustSpec::CAPTURE_KIND` at provider construction. The hot loop
    /// looks up by integer index (cap.index as usize) — equivalent perf
    /// to the previous hard-coded if-chain, but the source of truth
    /// lives in `spec.rs` const tables.
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

struct RustCaptureIndices {
    import_name: Option<u32>,
    import_source: Option<u32>,
    import_alias: Option<u32>,
    function: Option<u32>,
    struct_root: Option<u32>,
    enum_root: Option<u32>,
    trait_root: Option<u32>,
    method: Option<u32>,
    module_root: Option<u32>,
    type_alias_root: Option<u32>,
    const_root: Option<u32>,
    impl_root: Option<u32>,
    macro_root: Option<u32>,
    export: Option<u32>,
    heritage: Option<u32>,
    type_ann: Option<u32>,
    decorator: Option<u32>,
    property: Option<u32>,
    axum_handler: Option<u32>,
    actix_method: Option<u32>,
    actix_handler: Option<u32>,
}

impl RustProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_rust::LANGUAGE.into();
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        let query = Query::new(&language, &query_source)?;
        let indices = RustCaptureIndices {
            import_name: query.capture_index_for_name("import.name"),
            import_source: query.capture_index_for_name("import.source"),
            import_alias: query.capture_index_for_name("import.alias"),
            function: query.capture_index_for_name("function"),
            struct_root: query.capture_index_for_name("struct"),
            enum_root: query.capture_index_for_name("enum"),
            trait_root: query.capture_index_for_name("trait"),
            method: query.capture_index_for_name("method"),
            module_root: query.capture_index_for_name("module"),
            type_alias_root: query.capture_index_for_name("type_alias"),
            const_root: query.capture_index_for_name("const_decl"),
            impl_root: query.capture_index_for_name("impl_block"),
            macro_root: query.capture_index_for_name("macro_def"),
            export: query.capture_index_for_name("export"),
            heritage: query.capture_index_for_name("heritage"),
            type_ann: query.capture_index_for_name("type"),
            decorator: query.capture_index_for_name("decorator"),
            property: query.capture_index_for_name("property"),
            axum_handler: query.capture_index_for_name("axum.route.handler"),
            actix_method: query.capture_index_for_name("actix.route.method"),
            actix_handler: query.capture_index_for_name("actix.route.handler"),
        };

        // Pre-resolve capture-name → NodeKind from the spec table so the
        // hot loop stays an integer-index lookup (no per-capture string
        // compare). Capture names not in the spec map yield None and
        // fall through to the metadata-only branches (export, heritage, etc.).
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| RustSpec::CAPTURE_KIND.get(name).copied())
            .collect();

        Ok(Self {
            query,
            indices,
            capture_kind_by_idx,
        })
    }
}

impl LanguageProvider for RustProvider {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();

        let idx = &self.indices;

        // Side-table: top-level free `fn` byte ranges + names, used to resolve
        // the enclosing function for framework-ref captures via byte-range containment.
        let mut fn_spans: Vec<(String, std::ops::Range<usize>)> = Vec::new();
        // (handler_ident, capture_start_byte, capture_end_byte, span)
        type AxumCapture = (String, usize, usize, (u32, u32, u32, u32));
        // (method, handler_ident, span)
        type ActixCapture = (String, String, (u32, u32, u32, u32));
        let mut axum_handler_captures: Vec<AxumCapture> = Vec::new();
        let mut actix_handler_captures: Vec<ActixCapture> = Vec::new();

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut is_exported = false;
            let mut heritage = Vec::new();
            let mut type_annotation = None;
            let mut decorators = Vec::new();

            let mut import_name = None;
            let mut import_src = None;
            let mut import_alias = None;

            // Per-match Actix capture pair (one match = one fn + its attribute).
            let mut actix_method: Option<tree_sitter::Node> = None;
            let mut actix_handler: Option<tree_sitter::Node> = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap.index as usize)
                    .copied()
                    .flatten()
                {
                    // Single config-driven dispatch replaces the ten explicit
                    // name_struct/name_enum/… arms. Source of truth: RustSpec::CAPTURE_KIND.
                    name_node = Some(cap.node);
                    if kind.is_none() {
                        kind = Some(k_from_spec);
                    }
                } else if cap_idx == idx.import_name {
                    import_name = Some(cap.node);
                } else if cap_idx == idx.import_source {
                    import_src = Some(cap.node);
                } else if cap_idx == idx.import_alias {
                    import_alias = Some(cap.node);
                } else if cap_idx == idx.function {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Function);
                } else if cap_idx == idx.struct_root {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Struct);
                } else if cap_idx == idx.enum_root {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Enum);
                } else if cap_idx == idx.trait_root {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Trait);
                } else if cap_idx == idx.module_root {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Module);
                } else if cap_idx == idx.type_alias_root {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Typedef);
                } else if cap_idx == idx.const_root {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Const);
                } else if cap_idx == idx.impl_root {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Impl);
                } else if cap_idx == idx.macro_root {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Macro);
                } else if cap_idx == idx.property {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Property);
                } else if cap_idx == idx.method {
                    root_span_node = Some(cap.node);
                    kind = Some(NodeKind::Method);
                } else if cap_idx == idx.export {
                    is_exported = true;
                } else if cap_idx == idx.heritage {
                    if let Ok(h_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h_str.to_string());
                    }
                } else if cap_idx == idx.type_ann {
                    if let Ok(t_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        type_annotation = Some(t_str.to_string());
                    }
                } else if cap_idx == idx.decorator {
                    if let Ok(d_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d_str.to_string());
                    }
                } else if cap_idx == idx.axum_handler {
                    if let Ok(h_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        axum_handler_captures.push((
                            h_str.to_string(),
                            cap.node.start_byte(),
                            cap.node.end_byte(),
                            node_span(&cap.node),
                        ));
                    }
                } else if cap_idx == idx.actix_method {
                    actix_method = Some(cap.node);
                } else if cap_idx == idx.actix_handler {
                    actix_handler = Some(cap.node);
                }
            }

            if let (Some(method_node), Some(handler_node)) = (actix_method, actix_handler) {
                if let (Ok(method_str), Ok(handler_str)) = (
                    std::str::from_utf8(&source[method_node.start_byte()..method_node.end_byte()]),
                    std::str::from_utf8(
                        &source[handler_node.start_byte()..handler_node.end_byte()],
                    ),
                ) {
                    actix_handler_captures.push((
                        method_str.to_string(),
                        handler_str.to_string(),
                        node_span(&handler_node),
                    ));
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    if matches!(k, NodeKind::Function | NodeKind::Method) {
                        fn_spans.push((name_str.to_string(), root.start_byte()..root.end_byte()));
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
                    let mut merged = false;
                    if new_priority > 0 {
                        for existing in nodes.iter_mut() {
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
                                    merged = true;
                                }
                                break;
                            }
                        }
                    }
                    if !merged {
                        nodes.push(RawNode {
                            decorators,
                            is_exported,
                            heritage,
                            type_annotation,
                            name: name_str.to_string(),
                            kind: k,
                            span,
                            calls: Vec::new(),
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

                    imports.push(RawImport {
                        alias,
                        imported_name: name_str.to_string(),
                        source: src_str.to_string(),
                        binding_kind: None,
                    });
                }
            }
        }

        // Extract call sites with receiver-type binding. Replaces the shared
        // `extract_calls` for Rust so `self.method()` inside an impl block and
        // `obj.method()` with locally-typed `obj` are recorded as `Type.method`
        // for the resolver's qualifier-scoped (Tier 2.5) lookup.
        let impl_map = build_impl_map(tree.root_node(), source);
        let local_types = collect_local_types(tree.root_node(), source, &impl_map);
        extract_rust_calls(tree.root_node(), source, &mut nodes, &local_types);

        // Build param type map for indirect-call detection.
        // `collect_rust_indirect_param_types` captures fn(...)  and &dyn Trait
        // types that `bare_type_name` (used by LocalTypes) discards.
        let mut param_types = collect_rust_indirect_param_types(tree.root_node(), source);
        // Merge LocalTypes bindings (for typed let bindings already collected).
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
        let call_metas = detect_rust_indirect(tree.root_node(), source, &nodes, &param_types);

        // Stamp impl-target sentinel onto each impl method's heritage so the
        // class-membership post-process can bridge `struct Foo` ↔ `impl Foo
        // { fn bar() {} }` — the method's span lies OUTSIDE the struct span,
        // so pure span containment misses.
        let prefix = crate::post_process::class_membership::IMPL_TARGET_PREFIX;
        for raw in nodes.iter_mut() {
            if matches!(raw.kind, NodeKind::Function | NodeKind::Method) {
                if let Some(impl_ty) = impl_map.entries.get(&raw.name) {
                    raw.heritage.push(format!("{}{}", prefix, impl_ty));
                }
            }
        }

        // Framework-presence gates: only claim Axum/Actix refs when the file
        // actually `use`s the matching crate. Saves us from false positives in
        // files that happen to define a `get()` fn or use `#[get]` from another
        // crate.
        const AXUM_REQUIRED: &[&str] = &["axum"];
        const ACTIX_REQUIRED: &[&str] = &["actix_web", "actix"];
        let has_axum = has_import_from(&imports, AXUM_REQUIRED);
        let has_actix = has_import_from(&imports, ACTIX_REQUIRED);

        // Resolve enclosing function for each framework-ref capture via byte-range
        // containment; pick the innermost (smallest) function that contains the capture.
        let mut framework_refs = Vec::with_capacity(axum_handler_captures.len());
        if has_axum {
            for (handler_name, cap_start, cap_end, span) in axum_handler_captures {
                let enclosing = fn_spans
                    .iter()
                    .filter(|(_, range)| range.start <= cap_start && cap_end <= range.end)
                    .min_by_key(|(_, range)| range.end - range.start);
                if let Some((fn_name, _)) = enclosing {
                    framework_refs.push(RawFrameworkRef {
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
            for (method, handler, span) in actix_handler_captures {
                framework_refs.push(RawFrameworkRef {
                    source_name: MODULE_LEVEL_SOURCE.to_string(),
                    target_name: handler,
                    confidence: framework_confidence::ACTIX_ROUTE,
                    reason: format!("actix-route-{}", method),
                    span,
                });
            }
        }

        let file_category = crate::resolution::builder::determine_category(&path.to_string_lossy());
        let raw_function_metas = crate::function_meta::rust_lang::extract(
            tree.root_node(),
            source,
            &nodes,
            file_category,
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
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
<<<<<<< HEAD
            call_metas,
=======
            raw_function_metas,
>>>>>>> 9367228 (feat(meta): per-language FunctionMeta extraction — Python, Rust, TS, JS)
        })
    }
}
