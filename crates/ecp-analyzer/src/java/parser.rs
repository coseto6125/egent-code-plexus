use super::receiver_types::extract_java_calls_and_path_literals;
use super::spec::JavaSpec;
use crate::calls::{NameRefKind, NameRefSpec, VariantRefSpec};
use crate::driver::{
    self, imports, node, AnonKey, Anonymous, Compiled, DeclSlot, Effect, FieldReads, FileCx,
    KindCapture, Op, ParseSpec, Step, TxRule,
};
use crate::event_topic::{EventTopicConfig, KAFKA_JAVA, RABBITMQ_JAVA, REDIS_JAVA, SQS_JAVA};
use crate::framework_confidence;
use crate::framework_helpers::{collect_jvm_transactional_scopes, has_import_from, node_span};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::parse_prof::Pass;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawFunctionMeta, RawImport, RawNode};
use ecp_core::graph::{FileCategory, NodeKind};
use rustc_hash::FxHashMap;
use std::path::Path;
use tree_sitter::{Node, Query, Tree};

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_java::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct JavaProvider {
    compiled: Compiled<JavaSpec>,
}

impl JavaProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            compiled: Compiled::new()?,
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
        driver::parse(&self.compiled, path, source)
    }
}

/// The captures Java handles itself. A root capture sets the declaration
/// root once per match; the `.name` kind capture sets name and kind.
#[derive(Clone, Copy)]
pub enum JavaRole {
    Root,
    Export,
    Heritage,
    Type,
    Decorator,
    /// The `import_declaration` itself, read for the anonymous `static`.
    Import,
    ImportName,
    ImportSource,
    ImportWildcard,
    AutowiredClass,
    AutowiredTarget,
    RouteClass,
    RouteHandler,
}

#[derive(Clone, Copy)]
pub enum JavaHook {
    SpringGate,
    Calls,
}

#[derive(Default)]
pub struct JavaMatch<'t> {
    is_exported: bool,
    heritage: Vec<String>,
    type_annotation: Option<String>,
    decorators: Vec<String>,
    import_name: Option<Node<'t>>,
    import_src: Option<Node<'t>>,
    /// The asterisk node has no text the import needs; only its presence.
    is_import_wildcard: bool,
    import_decl_node: Option<Node<'t>>,
    autowired_class_node: Option<Node<'t>>,
    autowired_target_node: Option<Node<'t>>,
    route_class_node: Option<Node<'t>>,
    route_handler_node: Option<Node<'t>>,
}

#[derive(Default)]
pub struct JavaFile {
    /// Dedup lookup for multi-capture merge only; nodes are pushed in
    /// match order and the map is never iterated.
    node_id_to_idx: FxHashMap<usize, usize>,
    /// Emitted only if the file imports org.springframework.
    pending_spring_refs: Vec<RawFrameworkRef>,
}

impl ParseSpec for JavaSpec {
    type Role = JavaRole;
    type Match<'t> = JavaMatch<'t>;
    type File<'t> = JavaFile;
    type Hook = JavaHook;

    const KIND_CAPTURE: KindCapture = KindCapture::NameOverwrite;
    /// A lambda passed as a call argument gets an `<anonymous>` Function node
    /// only when its body holds a call, so attach_to_enclosing can host those
    /// calls instead of dropping them; pure-transform lambdas (`x -> x + 1`)
    /// stay out of the graph. The same lambda node can appear in several
    /// matches, so the dedupe keys on its tree-sitter node id.
    const ANONYMOUS: Option<Anonymous> = Some(Anonymous {
        capture: "function.anonymous",
        call_kinds: &["method_invocation", "object_creation_expression"],
        key: AnonKey::NodeId,
    });
    const BLIND: &'static [(&'static str, (&'static str, &'static str))] = &[
        (
            "blind.class_forname",
            (
                "java-class-forname",
                "Class.forName(<expr>) — runtime class loading by name; loaded class body is not statically determinable",
            ),
        ),
        (
            "blind.method_invoke",
            (
                "java-method-invoke",
                "<expr>.invoke(...) — reflective method invocation; target method body resolved at runtime via Method handle",
            ),
        ),
    ];
    const FIELD_READS: Option<FieldReads> = Some(FieldReads {
        member_kinds: &["field_access"],
        names: NameRefSpec {
            ident_kinds: &["identifier"],
            kind: NameRefKind::Value,
            binding_fields: &[
                ("enhanced_for_statement", "name"),
                ("resource", "name"),
                ("instanceof_expression", "name"),
            ],
            binding_parents: &["type_pattern", "record_pattern_component"],
            skip_kinds: &[
                "marker_annotation",
                "annotation",
                "scoped_identifier",
                "type_arguments",
            ],
            skip_parents: &["break_statement", "continue_statement", "labeled_statement"],
            method_refs: &["method_reference"],
            ..NameRefSpec::EMPTY
        },
    });
    const VARIANT_REFS: Option<VariantRefSpec> = Some(VariantRefSpec {
        path_kinds: &["field_access"],
        label_parents: &["switch_label"],
        // `case MONDAY:` names the constant without its enum.
        bare_label_parents: &["switch_label"],
        bare_expressions: false,
        token_trees: false,
    });
    const EVENT_TOPICS: &'static [EventTopicConfig] =
        &[REDIS_JAVA, KAFKA_JAVA, RABBITMQ_JAVA, SQS_JAVA];
    const TX: TxRule = TxRule::Nodes {
        collect: collect_jvm_transactional_scopes,
        kinds: &[NodeKind::Method, NodeKind::Constructor],
    };
    // The tx bind follows owner stamping: stamping writes `owner_class`
    // only, which the binder does not read.
    const STEPS: &'static [Step<JavaHook>] = &[
        Step::fixed(Pass::Query, Op::Query),
        Step::hook(Pass::Build, Effect::Neutral, JavaHook::SpringGate),
        Step::hook(Pass::Calls, Effect::Nodes, JavaHook::Calls),
        Step::fixed(Pass::Walk, Op::FieldReads),
        Step::fixed(Pass::Walk, Op::VariantRefs),
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
            &tree_sitter_java::LANGUAGE.into(),
            &query_source,
        )?)
    }

    fn role(capture: &str) -> Option<JavaRole> {
        Some(match capture {
            "namespace" | "class" | "interface" | "method" | "constructor" | "property"
            | "enum" | "enum_constant_node" | "annotation" => JavaRole::Root,
            "export" => JavaRole::Export,
            "heritage" => JavaRole::Heritage,
            "type" => JavaRole::Type,
            "decorator" => JavaRole::Decorator,
            "import" => JavaRole::Import,
            "import.name" => JavaRole::ImportName,
            "import.source" => JavaRole::ImportSource,
            "import.wildcard" => JavaRole::ImportWildcard,
            "spring.autowired.class" => JavaRole::AutowiredClass,
            "spring.autowired.target" => JavaRole::AutowiredTarget,
            "spring.route.class" => JavaRole::RouteClass,
            "spring.route.handler" => JavaRole::RouteHandler,
            _ => return None,
        })
    }

    fn parse_tree(source: &[u8]) -> Option<Tree> {
        PARSER.with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
    }

    fn capture<'t>(
        r: JavaRole,
        node: Node<'t>,
        d: &mut DeclSlot<'t>,
        m: &mut JavaMatch<'t>,
        _f: &mut JavaFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        match r {
            JavaRole::Root => {
                if d.root.is_none() {
                    d.root = Some(node);
                }
            }
            JavaRole::Export => m.is_exported = true,
            JavaRole::Heritage => {
                if let Ok(h_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    m.heritage.push(h_str.to_string());
                }
            }
            JavaRole::Type => {
                if let Ok(t_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    m.type_annotation = Some(t_str.to_string());
                }
            }
            JavaRole::Decorator => {
                if let Ok(d_str) = std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
                {
                    m.decorators.push(d_str.to_string());
                }
            }
            // The `@import` capture uses the same index in both query
            // patterns, so whichever fires populates it.
            JavaRole::Import => m.import_decl_node = Some(node),
            JavaRole::ImportName => m.import_name = Some(node),
            JavaRole::ImportSource => m.import_src = Some(node),
            JavaRole::ImportWildcard => m.is_import_wildcard = true,
            JavaRole::AutowiredClass => m.autowired_class_node = Some(node),
            JavaRole::AutowiredTarget => m.autowired_target_node = Some(node),
            JavaRole::RouteClass => m.route_class_node = Some(node),
            JavaRole::RouteHandler => m.route_handler_node = Some(node),
        }
    }

    fn end_match<'t>(
        d: DeclSlot<'t>,
        m: &mut JavaMatch<'t>,
        f: &mut JavaFile,
        cx: &mut FileCx<'t>,
    ) {
        let source = cx.source;
        let JavaMatch {
            is_exported,
            heritage,
            type_annotation,
            decorators,
            import_name,
            import_src,
            is_import_wildcard,
            import_decl_node,
            autowired_class_node,
            autowired_target_node,
            route_class_node,
            route_handler_node,
        } = std::mem::take(m);

        if let (Some(n), Some(k), Some(root)) = (d.name, d.kind, d.root) {
            if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
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

                let nodes = &mut cx.nodes;
                let idx = *f.node_id_to_idx.entry(node_id).or_insert_with(|| {
                    let i = nodes.len();
                    nodes.push(RawNode {
                        is_exported,
                        type_annotation: type_annotation.clone(),
                        ..node::decl(
                            name_str.to_string(),
                            k,
                            node::span(root),
                            &source[root.start_byte()..root.end_byte()],
                        )
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

                let exists = cx
                    .imports
                    .iter()
                    .any(|i: &RawImport| i.imported_name == name_str && i.source == src_str);
                if !exists {
                    cx.imports.push(imports::raw(
                        src_str.to_string(),
                        name_str.to_string(),
                        alias,
                        None,
                    ));
                }
            }
        }

        // --- Wildcard import (import X.* or import static X.*) ---
        if let (true, Some(i_src)) = (is_import_wildcard, import_src) {
            if let Ok(src_str) = std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
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

                let exists = cx
                    .imports
                    .iter()
                    .any(|i: &RawImport| i.imported_name == "*" && i.source == src_str);
                if !exists {
                    cx.imports.push(imports::raw(
                        src_str.to_string(),
                        "*".to_string(),
                        alias,
                        None,
                    ));
                }
            }
        }

        // Spring @Autowired: enclosing class -> injected type.
        if let (Some(cls), Some(tgt)) = (autowired_class_node, autowired_target_node) {
            if let (Ok(class_name), Ok(target_name)) = (
                std::str::from_utf8(&source[cls.start_byte()..cls.end_byte()]),
                std::str::from_utf8(&source[tgt.start_byte()..tgt.end_byte()]),
            ) {
                f.pending_spring_refs.push(RawFrameworkRef {
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
                f.pending_spring_refs.push(RawFrameworkRef {
                    source_name: class_name.to_string(),
                    target_name: method_name.to_string(),
                    confidence: framework_confidence::SPRING_ROUTE,
                    reason: "spring-route-handler".to_string(),
                    span: node_span(&mth),
                });
            }
        }
    }

    fn hook<'t>(h: JavaHook, _c: &Compiled<Self>, f: &mut JavaFile, cx: &mut FileCx<'t>) {
        match h {
            // Framework-presence gate: emit Spring refs only when the file imports
            // anything under `org.springframework`. Annotations alone aren't proof.
            JavaHook::SpringGate => {
                const SPRING_REQUIRED: &[&str] = &["org.springframework"];
                cx.framework_refs = if has_import_from(&cx.imports, SPRING_REQUIRED) {
                    std::mem::take(&mut f.pending_spring_refs)
                } else {
                    Vec::new()
                };
            }
            // Extract call sites with receiver-type binding for `this.foo()`,
            // `super.foo()`, and typed-variable `obj.foo()` patterns; same DFS
            // also collects path-shaped string literals.
            JavaHook::Calls => {
                let (raw_path_literals, raw_sql_refs) = extract_java_calls_and_path_literals(
                    cx.tree.root_node(),
                    cx.source,
                    &mut cx.nodes,
                );
                cx.path_literals = raw_path_literals;
                cx.sql_refs = raw_sql_refs;
            }
        }
    }

    fn function_meta(
        root: Node<'_>,
        source: &[u8],
        nodes: &[RawNode],
        category: FileCategory,
    ) -> Vec<RawFunctionMeta> {
        crate::function_meta::java::extract(root, source, nodes, category)
    }
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
    fn test_parse_file_single_segment_wildcard_preserves_import() {
        let graph = parse("import models.*;\n");
        assert!(
            graph.imports.iter().any(|import| {
                import.source == "models"
                    && import.imported_name == "*"
                    && import.alias.as_deref() == Some("*")
            }),
            "{:?}",
            graph.imports
        );
    }

    #[test]
    fn test_parse_file_package_declaration_preserves_namespace() {
        for name in ["models", "app.models"] {
            let graph = parse(&format!("package {name};\nclass Foo {{}}\n"));
            let namespaces: Vec<_> = graph
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKind::Namespace)
                .map(|node| (node.name.as_str(), node.span.0, node.span.2))
                .collect();
            assert_eq!(namespaces, [(name, 0, 0)]);
        }
        assert!(parse("class Foo {}\n")
            .nodes
            .iter()
            .all(|node| node.kind != NodeKind::Namespace));
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
