use super::receiver_types::extract_kotlin_calls;
use super::spec::KotlinSpec;
use crate::framework_confidence;
use crate::framework_helpers::{has_import_from, node_span, MODULE_LEVEL_SOURCE};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

// Framework-presence gate: only emit Ktor refs when the file imports io.ktor.*.
const KTOR_REQUIRED: &[&str] = &["io.ktor"];

/// Verb capture-index pairs. Indexed in lockstep with the per-verb captures in
/// `frameworks.scm` so the dispatch reads as a flat table — no alternation regex.
struct KtorVerbIndices {
    get: Option<u32>,
    post: Option<u32>,
    put: Option<u32>,
    delete: Option<u32>,
    patch: Option<u32>,
}

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_kotlin::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct KotlinProvider {
    query: Query,
    ktor: KtorVerbIndices,
    idx_ktor_path: Option<u32>,
    idx_constructor: Option<u32>,
    idx_property: Option<u32>,
    idx_variable: Option<u32>,
    /// Capture index → NodeKind mapping, pre-resolved from
    /// `KotlinSpec::CAPTURE_KIND` at provider construction. The hot loop
    /// looks up by integer index (cap.index as usize) — equivalent perf
    /// to the previous hard-coded if-chain, but the source of truth
    /// lives in `spec.rs` const tables.
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

/// True when `func` is a Kotlin `fun` declared directly inside a class body
/// (so its kind should be `Method`, not `Function`). Walks the tree-sitter
/// parent chain `function_declaration → class_body → class_declaration`.
fn is_class_method(func: tree_sitter::Node) -> bool {
    let Some(parent) = func.parent() else {
        return false;
    };
    if parent.kind() != "class_body" {
        return false;
    }
    parent
        .parent()
        .is_some_and(|p| p.kind() == "class_declaration")
}

/// True when the `class_declaration` is an `enum class Foo` — detected by the
/// presence of a direct `enum` keyword child (kind == `"enum"`). The grammar
/// places the `enum` token as a sibling of `class`, not inside `modifiers`.
fn is_enum_class(class_decl: tree_sitter::Node) -> bool {
    let mut cursor = class_decl.walk();
    for child in class_decl.children(&mut cursor) {
        if child.kind() == "enum" {
            return true;
        }
    }
    false
}

/// True when the `class_declaration` carries an `annotation` modifier — i.e.
/// `annotation class Foo`. Distinct from plain `class Foo`.
fn is_annotation_class(class_decl: tree_sitter::Node, source: &[u8]) -> bool {
    for i in 0..class_decl.child_count() {
        let Some(c) = class_decl.child(i as u32) else {
            continue;
        };
        if c.kind() == "modifiers" {
            for j in 0..c.child_count() {
                let Some(m) = c.child(j as u32) else { continue };
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
        let language = tree_sitter_kotlin::LANGUAGE.into();
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        let query = Query::new(&language, &query_source)?;
        let ktor = KtorVerbIndices {
            get: query.capture_index_for_name("ktor.route.get"),
            post: query.capture_index_for_name("ktor.route.post"),
            put: query.capture_index_for_name("ktor.route.put"),
            delete: query.capture_index_for_name("ktor.route.delete"),
            patch: query.capture_index_for_name("ktor.route.patch"),
        };
        let idx_ktor_path = query.capture_index_for_name("ktor.route.path");
        let idx_constructor = query.capture_index_for_name("constructor");
        let idx_property = query.capture_index_for_name("property");
        let idx_variable = query.capture_index_for_name("variable");

        // Pre-resolve capture-name → NodeKind from the spec table so the
        // hot loop stays an integer-index lookup (no per-capture string
        // compare). Capture names not in the spec map yield None and
        // fall through to the metadata-only branches below (heritage,
        // decorator, etc.).
        let capture_names = query.capture_names();
        let capture_kind_by_idx: Vec<Option<NodeKind>> = capture_names
            .iter()
            .map(|name| KotlinSpec::CAPTURE_KIND.get(name).copied())
            .collect();

        Ok(Self {
            query,
            ktor,
            idx_ktor_path,
            idx_constructor,
            idx_property,
            idx_variable,
            capture_kind_by_idx,
        })
    }
}

impl LanguageProvider for KotlinProvider {
    fn name(&self) -> &'static str {
        "kotlin"
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

        // Metadata-only capture indices — these don't carry a NodeKind
        // (handled in capture_kind_by_idx); they attach attributes to
        // the in-flight symbol. Kept as local indices for cheap compare.
        let idx_export = self.query.capture_index_for_name("export");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");
        let idx_alias = self.query.capture_index_for_name("alias");
        let idx_import_source = self.query.capture_index_for_name("import.source");
        let idx_decorator = self.query.capture_index_for_name("decorator");
        let idx_override_marker = self.query.capture_index_for_name("override_marker");

        // Root-span anchors (the @class / @function / @property / @variable
        // captures, not the .name variants). Their NodeKind is set via
        // capture_kind_by_idx for the .name captures; here we just track
        // the outer node so span/dedup keys point to the full declaration.
        let idx_class = self.query.capture_index_for_name("class");
        let idx_function = self.query.capture_index_for_name("function");
        let idx_enum_entry = self.query.capture_index_for_name("enum_entry");

        // Pending Ktor route refs: (verb, path_string, capture_span).
        // Emitted only if the file imports `io.ktor.*` — gate applied after the loop.
        type KtorRef = (&'static str, String, (u32, u32, u32, u32));
        let mut pending_ktor_refs: Vec<KtorRef> = Vec::new();

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_span_node = None;
            let mut is_exported = true;
            let mut heritage = Vec::new();
            let mut type_annotation = None;
            let mut decorators = Vec::new();

            let mut import_src = None;
            let mut import_alias = None;

            // Ktor route capture state for the current match — populated below
            // when the corresponding @ktor.route.<verb> + @ktor.route.path pair fires.
            let mut ktor_verb: Option<&'static str> = None;
            let mut ktor_route_span: Option<(u32, u32, u32, u32)> = None;
            let mut ktor_path_node: Option<tree_sitter::Node> = None;

            for cap in m.captures {
                let cap_idx = cap.index;
                if Some(cap_idx) == self.ktor.get {
                    ktor_verb = Some("get");
                    ktor_route_span = Some(node_span(&cap.node));
                } else if Some(cap_idx) == self.ktor.post {
                    ktor_verb = Some("post");
                    ktor_route_span = Some(node_span(&cap.node));
                } else if Some(cap_idx) == self.ktor.put {
                    ktor_verb = Some("put");
                    ktor_route_span = Some(node_span(&cap.node));
                } else if Some(cap_idx) == self.ktor.delete {
                    ktor_verb = Some("delete");
                    ktor_route_span = Some(node_span(&cap.node));
                } else if Some(cap_idx) == self.ktor.patch {
                    ktor_verb = Some("patch");
                    ktor_route_span = Some(node_span(&cap.node));
                } else if Some(cap_idx) == self.idx_ktor_path {
                    ktor_path_node = Some(cap.node);
                } else if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap_idx as usize)
                    .copied()
                    .flatten()
                {
                    // Single config-driven dispatch replaces the four
                    // explicit Class/Function/Property/Variable arms.
                    // Source of truth: KotlinSpec::CAPTURE_KIND in spec.rs.
                    name_node = Some(cap.node);
                    kind = Some(k_from_spec);
                } else if Some(cap_idx) == idx_export {
                    if let Ok(text) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        if text.contains("private") || text.contains("internal") {
                            is_exported = false;
                        }
                    }
                } else if Some(cap_idx) == idx_heritage {
                    if let Ok(h) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h.to_string());
                    }
                } else if Some(cap_idx) == idx_type {
                    if let Ok(t) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        type_annotation = Some(t.to_string());
                    }
                } else if Some(cap_idx) == idx_decorator {
                    if let Ok(d) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d.to_string());
                    }
                } else if Some(cap_idx) == idx_override_marker {
                    decorators.push("__override__".to_string());
                } else if Some(cap_idx) == idx_import_source {
                    import_src = Some(cap.node);
                } else if Some(cap_idx) == idx_alias {
                    import_alias = Some(cap.node);
                } else if (Some(cap_idx) == idx_class
                    || Some(cap_idx) == idx_function
                    || Some(cap_idx) == self.idx_constructor
                    || Some(cap_idx) == self.idx_property
                    || Some(cap_idx) == self.idx_variable
                    || Some(cap_idx) == idx_enum_entry)
                    && root_span_node.is_none()
                {
                    root_span_node = Some(cap.node);
                }
            }

            // Demote `Function` to `Method` when the `function_declaration` is
            // a direct child of `class_body`. Promote `Class` to `Annotation`
            // when the `class_declaration` carries the `annotation` modifier.
            // Mirrors the Python class-method fix landed in this PR (see
            // `python/parser.rs::is_class_method`).
            if let (Some(k_val), Some(root)) = (kind, root_span_node) {
                let new_kind = match k_val {
                    NodeKind::Function if is_class_method(root) => Some(NodeKind::Method),
                    NodeKind::Class if is_enum_class(root) => Some(NodeKind::Enum),
                    NodeKind::Class if is_annotation_class(root, source) => {
                        Some(NodeKind::Annotation)
                    }
                    _ => None,
                };
                if let Some(nk) = new_kind {
                    kind = Some(nk);
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_span_node) {
                // No pre-classification filter. The Variable query already
                // restricts to `(source_file (property_declaration ...))` via
                // the tree-sitter pattern; broader cases get whatever shape
                // the grammar offers. Downstream consumers decide.

                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let start = root.start_position();
                    let end = root.end_position();

                    // Property dedupe on name-node id so multi-declarator
                    // patterns each get their own entry; other kinds keep
                    // root-keyed dedupe (multi-decorator captures collapse).
                    let node_id = if k == NodeKind::Property {
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

                    if !is_exported {
                        entry.is_exported = false;
                    }
                    if type_annotation.is_some() {
                        entry.type_annotation = type_annotation;
                    }
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
                }
            }

            if let Some(i_src) = import_src {
                if let Ok(src_str) =
                    std::str::from_utf8(&source[i_src.start_byte()..i_src.end_byte()])
                {
                    let alias = if let Some(a_node) = import_alias {
                        std::str::from_utf8(&source[a_node.start_byte()..a_node.end_byte()])
                            .ok()
                            .map(|s| s.to_string())
                    } else {
                        None
                    };

                    imports.push(RawImport {
                        alias,
                        imported_name: src_str.to_string(),
                        source: src_str.to_string(),
                        binding_kind: None,
                    });
                }
            }

            // Stash Ktor route capture for post-loop gate. Path is from
            // `string_content` (already unquoted); verb is the per-pattern
            // capture name, so no regex alternation in Rust.
            if let (Some(verb), Some(span), Some(path_node)) =
                (ktor_verb, ktor_route_span, ktor_path_node)
            {
                if let Ok(path_str) =
                    std::str::from_utf8(&source[path_node.start_byte()..path_node.end_byte()])
                {
                    pending_ktor_refs.push((verb, path_str.to_string(), span));
                }
            }
        }

        // `nodes` already in source order — Vec + idx-map at parse-loop start.

        // Extract call sites with receiver-type binding for `this.foo()`,
        // `super.foo()`, and typed-variable `obj.foo()` patterns.
        extract_kotlin_calls(tree.root_node(), source, &mut nodes);

        // Ktor framework-presence gate: only emit refs when the file
        // imports `io.ktor.*`. The route DSL verbs (`get`/`post`/...) are
        // common identifiers, so without the gate we would over-claim.
        let has_ktor = has_import_from(&imports, KTOR_REQUIRED);
        let framework_refs: Vec<RawFrameworkRef> = if has_ktor {
            pending_ktor_refs
                .into_iter()
                .map(|(verb, path, span)| RawFrameworkRef {
                    source_name: MODULE_LEVEL_SOURCE.to_string(),
                    target_name: path,
                    confidence: framework_confidence::KTOR_ROUTE,
                    reason: format!("ktor-route-{}", verb),
                    span,
                })
                .collect()
        } else {
            Vec::new()
        };

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
            call_metas: vec![],
        })
    }
}
