use super::receiver_types::extract_java_calls;
use super::spec::JavaSpec;
use crate::framework_confidence;
use crate::framework_helpers::{collect_jvm_transactional_scopes, has_import_from, node_span};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use ecp_core::pool::StringPool;
use rustc_hash::FxHashMap;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_java::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct JavaProvider {
    query: Query,
    indices: JavaCaptureIndices,
    /// Capture index → NodeKind mapping, pre-resolved from
    /// `JavaSpec::CAPTURE_KIND` at provider construction. The hot loop
    /// looks up by integer index — equivalent perf to the previous
    /// hard-coded if-chain, but the source of truth lives in `spec.rs`.
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

struct JavaCaptureIndices {
    import_name: Option<u32>,
    import_source: Option<u32>,
    /// Captured `asterisk` node present when the import is a wildcard (`.*`).
    import_wildcard: Option<u32>,
    class: Option<u32>,
    interface: Option<u32>,
    method: Option<u32>,
    constructor: Option<u32>,
    property: Option<u32>,
    variable: Option<u32>,
    export: Option<u32>,
    heritage: Option<u32>,
    type_ann: Option<u32>,
    decorator: Option<u32>,
    enum_: Option<u32>,
    annotation: Option<u32>,
    // Spring @Autowired field injection.
    spring_autowired_class: Option<u32>,
    spring_autowired_target: Option<u32>,
    // Spring @RestController / @Controller route methods.
    spring_route_class: Option<u32>,
    spring_route_handler: Option<u32>,
}

impl JavaProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_java::LANGUAGE.into();
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        let query = Query::new(&language, &query_source)?;
        let indices = JavaCaptureIndices {
            import_name: query.capture_index_for_name("import.name"),
            import_source: query.capture_index_for_name("import.source"),
            import_wildcard: query.capture_index_for_name("import.wildcard"),
            class: query.capture_index_for_name("class"),
            interface: query.capture_index_for_name("interface"),
            method: query.capture_index_for_name("method"),
            constructor: query.capture_index_for_name("constructor"),
            property: query.capture_index_for_name("property"),
            variable: query.capture_index_for_name("variable"),
            export: query.capture_index_for_name("export"),
            heritage: query.capture_index_for_name("heritage"),
            type_ann: query.capture_index_for_name("type"),
            decorator: query.capture_index_for_name("decorator"),
            enum_: query.capture_index_for_name("enum"),
            annotation: query.capture_index_for_name("annotation"),
            spring_autowired_class: query.capture_index_for_name("spring.autowired.class"),
            spring_autowired_target: query.capture_index_for_name("spring.autowired.target"),
            spring_route_class: query.capture_index_for_name("spring.route.class"),
            spring_route_handler: query.capture_index_for_name("spring.route.handler"),
        };

        // Pre-resolve capture-name → NodeKind from the spec table so the
        // hot loop stays an integer-index lookup (no per-capture string
        // compare). Capture names not in the spec map yield None and
        // fall through to the metadata-only branches (heritage, decorator,
        // spring, etc.).
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| JavaSpec::CAPTURE_KIND.get(name).copied())
            .collect();

        Ok(Self {
            query,
            indices,
            capture_kind_by_idx,
        })
    }
}

/// Returns true if the bytes of the `import_declaration` node contain the
/// anonymous keyword `static` — used to distinguish `import static X.y` from
/// plain `import X.y` without adding a named capture for every anonymous node.
fn import_decl_is_static(source: &[u8], node: tree_sitter::Node) -> bool {
    let slice = &source[node.start_byte()..node.end_byte()];
    // The layout is always:  `import` WS (`static` WS)? ...
    // We check the first ~20 bytes so we never scan the whole file.
    let window = &slice[..slice.len().min(20)];
    window.windows(6).any(|w| w == b"static")
}

impl LanguageProvider for JavaProvider {
    fn name(&self) -> &'static str {
        "java"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        // Vec + idx-map pattern: push to `nodes` in tree-sitter match order
        // (source-position deterministic). `node_id_to_idx` only acts as a
        // dedup lookup for multi-capture merge; iteration never visits the
        // map, so no downstream sort is needed.
        let mut nodes: Vec<RawNode> = Vec::new();
        let mut node_id_to_idx: FxHashMap<usize, usize> = FxHashMap::default();
        let mut imports = Vec::new();
        // Buffer Spring refs and emit only if the file imports org.springframework.
        let mut pending_spring_refs: Vec<RawFrameworkRef> = Vec::new();

        let idx = &self.indices;

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
            let mut import_wildcard_node: Option<tree_sitter::Node> = None;
            // Track the enclosing `import_declaration` node so we can inspect
            // the raw source text for the anonymous `static` keyword.
            let mut import_decl_node: Option<tree_sitter::Node> = None;

            // Spring @Autowired captures.
            let mut autowired_class_node: Option<tree_sitter::Node> = None;
            let mut autowired_target_node: Option<tree_sitter::Node> = None;
            // Spring route handler captures.
            let mut route_class_node: Option<tree_sitter::Node> = None;
            let mut route_handler_node: Option<tree_sitter::Node> = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap.index as usize)
                    .copied()
                    .flatten()
                {
                    // Single table-driven dispatch replaces the eight explicit
                    // Class/Interface/Method/Constructor/Property/Variable/Enum/Annotation arms.
                    // Source of truth: JavaSpec::CAPTURE_KIND in spec.rs.
                    name_node = Some(cap.node);
                    kind = Some(k_from_spec);
                } else if cap_idx == idx.import_name {
                    import_name = Some(cap.node);
                } else if cap_idx == idx.import_source {
                    import_src = Some(cap.node);
                } else if cap_idx == idx.import_wildcard {
                    import_wildcard_node = Some(cap.node);
                } else if cap_idx == idx.class
                    || cap_idx == idx.interface
                    || cap_idx == idx.method
                    || cap_idx == idx.constructor
                    || cap_idx == idx.property
                    || cap_idx == idx.variable
                    || cap_idx == idx.enum_
                    || cap_idx == idx.annotation
                {
                    if root_span_node.is_none() {
                        root_span_node = Some(cap.node);
                    }
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
                } else if cap_idx == idx.spring_autowired_class {
                    autowired_class_node = Some(cap.node);
                } else if cap_idx == idx.spring_autowired_target {
                    autowired_target_node = Some(cap.node);
                } else if cap_idx == idx.spring_route_class {
                    route_class_node = Some(cap.node);
                } else if cap_idx == idx.spring_route_handler {
                    route_handler_node = Some(cap.node);
                }

                // Track the `@import` pattern node (the import_declaration itself).
                // The `@import` capture uses the same index in both query patterns,
                // so whichever fires populates import_decl_node.
                if let Some(import_idx) = query_capture_index_named(&self.query, "import") {
                    if cap.index == import_idx {
                        import_decl_node = Some(cap.node);
                    }
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();

                    // Multi-declarator declarations (`int x, y, z;` for
                    // fields OR locals) share one declaration root. Keying
                    // dedupe on `n.id()` (the per-name identifier node)
                    // for Property + Variable emits one node per declarator.
                    // Other kinds (Class/Interface/Method/Constructor) keep
                    // root-keyed dedupe so multi-decorator captures still
                    // collapse to one node.
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

                    for h in heritage {
                        if !entry.heritage.contains(&h) {
                            entry.heritage.push(h);
                        }
                    }
                    for d in decorators {
                        if !entry.decorators.contains(&d) {
                            entry.decorators.push(d);
                        }
                    }
                    if is_exported {
                        entry.is_exported = true;
                    }
                    if type_annotation.is_some() {
                        entry.type_annotation = type_annotation.clone();
                    }
                }
            }

            // --- Named import (regular or static) ---
            if let (Some(i_name), Some(i_src)) = (import_name, import_src) {
                if let (Ok(name_str), Ok(src_str)) = (
                    std::str::from_utf8(&source[i_name.start_byte()..i_name.end_byte()]),
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()]),
                ) {
                    // `alias` carries the short binding name for static imports
                    // (the identifier that call sites use without a qualifier).
                    // For `import static com.foo.Bar.method`, alias = Some("method").
                    // For regular `import com.foo.Bar`,       alias = None.
                    let is_static = import_decl_node
                        .map(|n| import_decl_is_static(source, n))
                        .unwrap_or(false);
                    let alias = if is_static {
                        Some(name_str.to_string())
                    } else {
                        None
                    };

                    let exists = imports
                        .iter()
                        .any(|i: &RawImport| i.imported_name == name_str && i.source == src_str);
                    if !exists {
                        imports.push(RawImport {
                            alias,
                            imported_name: name_str.to_string(),
                            source: src_str.to_string(),
                            binding_kind: None,
                        });
                    }
                }
            }

            // --- Wildcard import (import X.* or import static X.*) ---
            if let (Some(wildcard_node), Some(i_src)) = (import_wildcard_node, import_src) {
                let _ = wildcard_node; // asterisk node itself has no text we need
                if let Ok(src_str) =
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
                {
                    let is_static = import_decl_node
                        .map(|n| import_decl_is_static(source, n))
                        .unwrap_or(false);

                    // imported_name = "*" marks an on-demand / wildcard import.
                    // alias = Some("*") for non-static wildcard,
                    //         Some("static:*") for static wildcard — lets
                    //         downstream tools distinguish the two without a
                    //         separate field.
                    let alias = if is_static {
                        Some("static:*".to_string())
                    } else {
                        Some("*".to_string())
                    };

                    let exists = imports
                        .iter()
                        .any(|i: &RawImport| i.imported_name == "*" && i.source == src_str);
                    if !exists {
                        imports.push(RawImport {
                            alias,
                            imported_name: "*".to_string(),
                            source: src_str.to_string(),
                            binding_kind: None,
                        });
                    }
                }
            }

            // Spring @Autowired: enclosing class -> injected type.
            if let (Some(cls), Some(tgt)) = (autowired_class_node, autowired_target_node) {
                if let (Ok(class_name), Ok(target_name)) = (
                    std::str::from_utf8(&source[cls.start_byte()..cls.end_byte()]),
                    std::str::from_utf8(&source[tgt.start_byte()..tgt.end_byte()]),
                ) {
                    pending_spring_refs.push(RawFrameworkRef {
                        source_name: class_name.to_string(),
                        target_name: target_name.to_string(),
                        confidence: framework_confidence::SPRING_AUTOWIRED,
                        reason: "spring-autowired".to_string(),
                        span: node_span(&tgt),
                    });
                }
            }

            // Spring @RestController/@Controller: class -> route handler method.
            if let (Some(cls), Some(mth)) = (route_class_node, route_handler_node) {
                if let (Ok(class_name), Ok(method_name)) = (
                    std::str::from_utf8(&source[cls.start_byte()..cls.end_byte()]),
                    std::str::from_utf8(&source[mth.start_byte()..mth.end_byte()]),
                ) {
                    pending_spring_refs.push(RawFrameworkRef {
                        source_name: class_name.to_string(),
                        target_name: method_name.to_string(),
                        confidence: framework_confidence::SPRING_ROUTE,
                        reason: "spring-route-handler".to_string(),
                        span: node_span(&mth),
                    });
                }
            }
        }

        // Framework-presence gate: emit Spring refs only when the file imports
        // anything under `org.springframework`. Annotations alone aren't proof.
        const SPRING_REQUIRED: &[&str] = &["org.springframework"];
        let framework_refs: Vec<RawFrameworkRef> = if has_import_from(&imports, SPRING_REQUIRED) {
            pending_spring_refs
        } else {
            Vec::new()
        };

        // `nodes` already in tree-sitter match order (= source order) per
        // the Vec + idx-map pattern at parse-loop start; no sort needed.

        // Extract call sites with receiver-type binding for `this.foo()`,
        // `super.foo()`, and typed-variable `obj.foo()` patterns.
        extract_java_calls(tree.root_node(), source, &mut nodes);

        let file_category =
            crate::resolution::builder::determine_category(path.to_str().unwrap_or(""));
        let raw_function_metas =
            crate::function_meta::java::extract(tree.root_node(), source, &nodes, file_category);
        let tx_scopes =
            collect_jvm_transactional_scopes(&nodes, &[NodeKind::Method, NodeKind::Constructor]);

        let event_topics = {
            let mut pool = StringPool::new();
            let topics = crate::event_topic::extract_event_topics(
                &tree,
                source,
                &self.query,
                &[crate::event_topic::REDIS_JAVA],
                &imports,
                &mut pool,
            );
            (!topics.is_empty()).then(|| topics.into_boxed_slice())
        };

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
            blind_spots: vec![],
            schema_fields: None,
            event_topics,
            tx_scopes,
            call_metas: vec![],
            raw_function_metas,
        })
    }
}

/// Helper to look up a capture index by name from the compiled query.
/// Returns `None` if the name does not appear in the query.
#[inline]
fn query_capture_index_named(query: &Query, name: &str) -> Option<u32> {
    query.capture_index_for_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn parse(source: &str) -> LocalGraph {
        let provider = JavaProvider::new().expect("JavaProvider::new failed");
        provider
            .parse_file(&PathBuf::from("Test.java"), source.as_bytes())
            .expect("parse_file failed")
    }

    // ── Task G: Java Named Bindings ──────────────────────────────────────────

    #[test]
    fn java_static_import_sets_alias() {
        let graph = parse(
            r#"
import static com.example.MathUtils.square;

public class App {
    public void run() {
        int x = square(5);
    }
}
"#,
        );
        let imp = graph
            .imports
            .iter()
            .find(|i| i.imported_name == "square")
            .expect("static import of `square` not found");
        assert_eq!(
            imp.source, "com.example.MathUtils.square",
            "source should be the full qualified path"
        );
        assert_eq!(
            imp.alias,
            Some("square".to_string()),
            "alias must carry the short binding name for static imports"
        );
    }

    #[test]
    fn java_wildcard_import_alias_star() {
        let graph = parse("import com.example.utils.*;\n");
        let imp = graph
            .imports
            .iter()
            .find(|i| i.imported_name == "*")
            .expect("wildcard import not found");
        assert_eq!(imp.source, "com.example.utils");
        assert_eq!(
            imp.alias,
            Some("*".to_string()),
            "non-static wildcard alias must be `*`"
        );
    }

    #[test]
    fn java_static_wildcard_import_alias() {
        let graph = parse("import static com.example.Constants.*;\n");
        let imp = graph
            .imports
            .iter()
            .find(|i| i.imported_name == "*")
            .expect("static wildcard import not found");
        assert_eq!(imp.source, "com.example.Constants");
        assert_eq!(
            imp.alias,
            Some("static:*".to_string()),
            "static wildcard alias must be `static:*`"
        );
    }

    #[test]
    fn java_regular_import_no_alias() {
        let graph = parse("import com.example.Foo;\n");
        let imp = graph
            .imports
            .iter()
            .find(|i| i.imported_name == "Foo")
            .expect("regular import not found");
        assert_eq!(imp.alias, None, "regular imports must have no alias");
    }
}
