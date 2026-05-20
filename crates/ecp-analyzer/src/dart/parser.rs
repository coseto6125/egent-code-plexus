use super::receiver_types::{collect_bindings, extract_dart_calls};
use super::spec::DartSpec;
use crate::framework_confidence;
use crate::framework_helpers::{detect_ast_framework_patterns, FrameworkPatternSpec};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

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
    query: Query,
    /// Capture index → NodeKind mapping, pre-resolved from
    /// `DartSpec::CAPTURE_KIND` at provider construction. The hot loop
    /// looks up by integer index — no per-capture string compare.
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

impl DartProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_dart::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| DartSpec::CAPTURE_KIND.get(name).copied())
            .collect();
        Ok(Self {
            query,
            capture_kind_by_idx,
        })
    }
}

impl LanguageProvider for DartProvider {
    fn name(&self) -> &'static str {
        "dart"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");
        let idx_decorator = self.query.capture_index_for_name("decorator");

        let idx_class = self.query.capture_index_for_name("class");
        let idx_function = self.query.capture_index_for_name("function");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_constructor = self.query.capture_index_for_name("constructor");
        let idx_typedef = self.query.capture_index_for_name("typedef");
        let idx_interface = self.query.capture_index_for_name("interface");
        let idx_trait = self.query.capture_index_for_name("trait");
        let idx_property = self.query.capture_index_for_name("property");
        let idx_import = self.query.capture_index_for_name("import");
        let idx_enum = self.query.capture_index_for_name("enum");
        let idx_annotation = self.query.capture_index_for_name("annotation");

        let idx_var = self.query.capture_index_for_name("var");
        let idx_var_name = self.query.capture_index_for_name("var.name");
        let idx_var_type = self.query.capture_index_for_name("var.type");

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut heritage = Vec::new();
            let mut type_annotation = None;
            let mut decorators = Vec::new();

            let mut import_source = None;
            let mut import_alias = None;

            let mut var_root: Option<tree_sitter::Node<'_>> = None;
            let mut var_name: Option<tree_sitter::Node<'_>> = None;
            let mut var_type: Option<tree_sitter::Node<'_>> = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap_idx as usize)
                    .copied()
                    .flatten()
                {
                    // Single config-driven dispatch replaces the eight explicit
                    // Class/Function/Method/Constructor/Typedef/Interface/Trait/Property arms.
                    // Source of truth: DartSpec::CAPTURE_KIND in spec.rs.
                    name_node = Some(cap.node);
                    kind = Some(k_from_spec);
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.trim().to_string());
                    }
                } else if Some(cap_idx) == idx_type {
                    if let Ok(t) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        type_annotation = Some(t.trim().to_string());
                    }
                } else if Some(cap_idx) == idx_import_source {
                    import_source = Some(cap.node);
                } else if Some(cap_idx) == idx_import_alias {
                    import_alias = Some(cap.node);
                } else if Some(cap_idx) == idx_decorator {
                    if let Ok(d_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d_str.to_string());
                    }
                } else if Some(cap_idx) == idx_var {
                    var_root = Some(cap.node);
                } else if Some(cap_idx) == idx_var_name {
                    var_name = Some(cap.node);
                } else if Some(cap_idx) == idx_var_type {
                    var_type = Some(cap.node);
                }

                if Some(cap_idx) == idx_function
                    || Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_method
                    || Some(cap_idx) == idx_constructor
                    || Some(cap_idx) == idx_typedef
                    || Some(cap_idx) == idx_interface
                    || Some(cap_idx) == idx_trait
                    || Some(cap_idx) == idx_property
                    || Some(cap_idx) == idx_import
                    || Some(cap_idx) == idx_enum
                    || Some(cap_idx) == idx_annotation
                {
                    root_span_node = Some(cap.node);
                }
            }

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
                        nodes.push(node);
                    }
                }
                continue;
            }
            if let (Some(v_root), Some(v_name)) = (var_root, var_name) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[v_name.start_byte()..v_name.end_byte()])
                {
                    let name_str = name_str.trim();
                    let start = v_root.start_position();
                    let end = v_root.end_position();
                    let type_ann = var_type.and_then(|t| {
                        std::str::from_utf8(&source[t.start_byte()..t.end_byte()])
                            .ok()
                            .map(|s| s.trim().to_string())
                    });
                    nodes.push(RawNode {
                        decorators: vec![],
                        is_exported: !name_str.starts_with('_'),
                        heritage: vec![],
                        type_annotation: type_ann,
                        name: name_str.to_string(),
                        kind: NodeKind::Variable,
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

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
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
                    })
                {
                    continue;
                }
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let name_str = name_str.trim();
                    // Dart visibility convention: identifiers starting with `_` are
                    // library-private regardless of `library` directive (per Dart
                    // language spec). Applies to all symbol kinds (Class, Function,
                    // Method, Interface, Property).
                    let is_exported = !name_str.starts_with('_');
                    let start = root.start_position();
                    let end = root.end_position();

                    nodes.push(RawNode {
                        decorators,
                        is_exported,
                        heritage: heritage.clone(),
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
                }
            }

            if let Some(i_src) = import_source {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
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

                    imports.push(RawImport {
                        alias: alias_str,
                        imported_name: clean_src.clone(),
                        source: clean_src,
                        binding_kind: None,
                    });
                }
            }
        }

        // Deduplicate simple identical node extractions
        nodes.dedup_by(|a, b| a.name == b.name && a.span == b.span && a.kind == b.kind);

        // Extract call sites with receiver-type binding: `this.method()` →
        // `Class.method`, `super.method()` → `Super.method`, typed-var
        // `obj.method()` → `Type.method`. Feeds the resolver's Tier 2.5
        // qualifier-scoped lookup.
        let bindings = collect_bindings(tree.root_node(), source);
        extract_dart_calls(tree.root_node(), source, &mut nodes, &bindings);

        let framework_refs = detect_ast_framework_patterns(source, DART_FRAMEWORKS);

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
            raw_function_metas: vec![],
        })
    }
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
    })
}
