use super::receiver_types::extract_csharp_calls;
use super::spec::CSharpSpec;
use crate::framework_confidence;
use crate::framework_helpers::{detect_ast_framework_patterns, FrameworkPatternSpec};
use graph_nexus_core::analyzer::lang_spec::LangSpec;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

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

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_c_sharp::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct CSharpProvider {
    query: Query,
    /// Capture index → NodeKind mapping, pre-resolved from
    /// `CSharpSpec::CAPTURE_KIND` at provider construction. The hot loop
    /// looks up by integer index (cap.index as usize) — equivalent perf
    /// to the previous hard-coded if-chain, but the source of truth
    /// lives in `spec.rs` const tables.
    capture_kind_by_idx: Vec<Option<NodeKind>>,
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

        Ok(Self {
            query,
            capture_kind_by_idx,
        })
    }
}

impl LanguageProvider for CSharpProvider {
    fn name(&self) -> &'static str {
        "c_sharp"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        // Vec + idx-map pattern — see java/parser.rs same-site note.
        let mut nodes: Vec<RawNode> = Vec::new();
        let mut node_id_to_idx: rustc_hash::FxHashMap<usize, usize> =
            rustc_hash::FxHashMap::default();
        let mut imports = Vec::new();

        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");

        let idx_export = self.query.capture_index_for_name("export");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");
        let idx_decorator = self.query.capture_index_for_name("decorator");

        let idx_function = self.query.capture_index_for_name("function");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_interface = self.query.capture_index_for_name("interface");
        let idx_property = self.query.capture_index_for_name("property");
        let idx_variable = self.query.capture_index_for_name("variable");
        let idx_constructor = self.query.capture_index_for_name("constructor");
        let idx_namespace = self.query.capture_index_for_name("namespace");
        let idx_enum = self.query.capture_index_for_name("enum");
        let idx_struct = self.query.capture_index_for_name("struct");

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
                } else if (Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_method
                    || Some(cap_idx) == idx_interface
                    || Some(cap_idx) == idx_property
                    || Some(cap_idx) == idx_variable
                    || Some(cap_idx) == idx_constructor
                    || Some(cap_idx) == idx_namespace
                    || Some(cap_idx) == idx_enum
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
        // `base.Foo()`, and typed-variable `obj.Foo()` patterns.
        extract_csharp_calls(tree.root_node(), source, &mut nodes);

        let framework_refs = detect_ast_framework_patterns(source, CSHARP_FRAMEWORKS);

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
