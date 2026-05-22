use super::receiver_types::{
    build_receiver_map, collect_local_types, extract_go_calls, receiver_type_from_method_decl,
};
use super::spec::GoSpec;
use crate::framework_confidence;
use crate::framework_helpers::{
    enclosing_function_name, has_import_from, node_span, MODULE_LEVEL_SOURCE,
};
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::lang_spec::LangSpec;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{BlindSpot, LocalGraph, RawFrameworkRef, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

/// gin / echo / chi all share `r.METHOD("/path", handler)`. Gate the
/// framework_ref by the imported package — the route shape alone can't
/// distinguish gin from echo. Ported from upstream
/// `gitnexus/src/core/group/extractors/http-patterns/go.ts:23-39`.
const GIN_REQUIRED: &[&str] = &["github.com/gin-gonic/gin"];
const ECHO_REQUIRED: &[&str] = &["github.com/labstack/echo"];

/// Blind-spot kind/hint pairs. Order matches the capture-index dispatch
/// in `parse_file`.
const BLIND_SPEC: &[(&str, &str)] = &[
    (
        "go-reflect-method-by-name",
        "x.MethodByName(name) — runtime method resolution by string; the subsequent .Call(...) is dispatch through an unknown target",
    ),
    (
        "go-plugin-open",
        "plugin.Open(...) — dynamic library load (.so/.dll); symbols obtained via subsequent .Lookup(...) are resolved at runtime",
    ),
];

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_go::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
pub struct GoProvider {
    query: Query,
    /// Capture index → NodeKind mapping, pre-resolved from
    /// `GoSpec::CAPTURE_KIND` at provider construction. The hot loop
    /// looks up by integer index (cap.index as usize) — equivalent perf
    /// to the previous hard-coded if-chain, but the source of truth
    /// lives in `spec.rs` const tables.
    capture_kind_by_idx: Vec<Option<NodeKind>>,
}

/// Walk up the AST from `node` (a `field_identifier` inside a
/// `field_declaration`) to find the name of the enclosing named struct type.
///
/// Tree shape for a named struct:
///   `field_identifier` → `field_declaration` → `field_declaration_list`
///   → `struct_type` → `type_spec` (which has `name: type_identifier`)
///
/// For fields in anonymous inline structs there is no enclosing `type_spec`,
/// so this returns `None` (uid remains without owner_class — collisions inside
/// a single anonymous struct are already impossible because of line position).
fn enclosing_struct_type(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let mut current = node.parent()?;
    while current.kind() != "struct_type" {
        current = current.parent()?;
    }
    let parent = current.parent()?;
    if parent.kind() != "type_spec" {
        return None;
    }
    let name_node = parent.child_by_field_name("name")?;
    std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
        .ok()
        .map(String::from)
}

/// Walk up the AST from `node` until a `function_declaration` or
/// `method_declaration` is found. Returns the enclosing function name —
/// receiver methods are qualified as `RecvType.MethodName` so two methods
/// with the same name on different receivers (`(d *Dog) name()` and
/// `(c *Cat) name()`) don't collide.
///
/// Used to scope short-var (`:=`) locals; without this, every `pairs := ...`
/// across all funcs in a file uid-collides.
fn enclosing_func_or_method_name(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let mut current = node.parent()?;
    loop {
        match current.kind() {
            "function_declaration" => {
                let name_node = current.child_by_field_name("name")?;
                return std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                    .ok()
                    .map(String::from);
            }
            "method_declaration" => {
                let name_node = current.child_by_field_name("name")?;
                let method_name =
                    std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                        .ok()?
                        .to_string();
                return Some(
                    match super::receiver_types::receiver_type_from_method_decl(current, source) {
                        Some(recv) => format!("{recv}.{method_name}"),
                        None => method_name,
                    },
                );
            }
            _ => {}
        }
        current = current.parent()?;
    }
}

impl GoProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_go::LANGUAGE.into();
        let query_source = format!(
            "{}\n;; ---- framework queries ----\n{}",
            include_str!("queries.scm"),
            include_str!("frameworks.scm"),
        );
        let query = Query::new(&language, &query_source)?;

        // Pre-resolve capture-name → NodeKind from the spec table so the
        // hot loop stays an integer-index lookup (no per-capture string
        // compare). Capture names not in the spec map yield None and
        // fall through to the metadata-only branches below (heritage,
        // route, field, var, etc.).
        let capture_kind_by_idx: Vec<Option<NodeKind>> = query
            .capture_names()
            .iter()
            .map(|name| GoSpec::CAPTURE_KIND.get(name).copied())
            .collect();

        Ok(Self {
            query,
            capture_kind_by_idx,
        })
    }
}

impl LanguageProvider for GoProvider {
    fn name(&self) -> &'static str {
        "go"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| parse_with_budget(&mut p.borrow_mut(), source, ParseBudget::DEFAULT))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();
        let mut blind_spots: Vec<BlindSpot> = Vec::new();

        let idx_struct = self.query.capture_index_for_name("struct");
        let idx_interface = self.query.capture_index_for_name("interface");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_function = self.query.capture_index_for_name("function");
        let idx_const = self.query.capture_index_for_name("const");

        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_type = self.query.capture_index_for_name("type");

        let idx_import = self.query.capture_index_for_name("import");
        let idx_import_alias = self.query.capture_index_for_name("import.alias");
        let idx_import_source = self.query.capture_index_for_name("import.source");

        let idx_route_call = self.query.capture_index_for_name("route.call");
        let idx_route_method = self.query.capture_index_for_name("route.method");
        let idx_route_path = self.query.capture_index_for_name("route.path");
        let idx_route_handler = self.query.capture_index_for_name("route.handler");

        let idx_blind_method_by_name = self
            .query
            .capture_index_for_name("blind.reflect_method_by_name");
        let idx_blind_plugin_open = self.query.capture_index_for_name("blind.plugin_open");

        // Pending framework refs for gin / echo. Collected during the
        // match loop; only emitted after we confirm an import gate match
        // (so we don't pollute framework_refs when net/http or chi is used).
        let mut pending_gin: Vec<(String, (u32, u32, u32, u32))> = Vec::new();
        let mut pending_echo: Vec<(String, (u32, u32, u32, u32))> = Vec::new();

        let idx_field_name = self.query.capture_index_for_name("field.name");
        let idx_field_type = self.query.capture_index_for_name("field.type");

        let idx_var = self.query.capture_index_for_name("var");
        let idx_var_name = self.query.capture_index_for_name("var.name");
        let idx_var_type = self.query.capture_index_for_name("var.type");

        // File-scope var declarations (with or without explicit type annotation).
        // `@variable` anchors to the source_file so function-local vars are excluded.
        let idx_variable = self.query.capture_index_for_name("variable");
        let idx_variable_name = self.query.capture_index_for_name("variable.name");

        // Short var declarations: `x := expr`, `x, y := a, b`.
        let idx_local = self.query.capture_index_for_name("local");
        let idx_local_name = self.query.capture_index_for_name("local.name");

        let mut routes = Vec::new();
        // Buffer for file-scope `@variable` path emissions. Merged into `nodes`
        // after the match loop so the typed `@var` path (which runs within the
        // same loop) takes precedence: if `@var` already emitted a Variable with
        // a type annotation, the `@variable` entry for the same name is dropped.
        let mut file_var_pending: Vec<RawNode> = Vec::new();

        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut kind = None;
            let mut root_node = None;
            let mut heritage = Vec::new();
            let mut type_annotation: Option<String> = None;

            let mut is_import = false;
            let mut import_alias = None;
            let mut import_source = None;

            let mut is_route = false;
            let mut route_method_node = None;
            let mut route_path_node = None;
            let mut route_span_node = None;
            let mut route_handler_node: Option<tree_sitter::Node> = None;

            // Buffers for per-name struct-field emission. `X, Y int` produces
            // multiple `@field.name` captures + one `@field.type`; we collect
            // both then emit one Property per name sharing the type text.
            let mut field_name_nodes: Vec<tree_sitter::Node> = Vec::new();
            let mut field_type_text: Option<String> = None;

            // Per-match buffers for var declarations. Names are buffered as
            // Vec because Go allows multi-name decls in one node: `var x, y
            // int` parses as a single `var_spec` with multiple `name:`
            // children. Pre-fix this used `Option<Node>` and silently
            // dropped all but the last.
            let mut is_var = false;
            let mut var_name_nodes: Vec<tree_sitter::Node> = Vec::new();
            let mut var_type_text: Option<String> = None;
            let mut var_root_node = None;

            // File-scope `var X [T] = ...` — typed or untyped. Anchored at
            // source_file so body-local `var` blocks don't match here.
            let mut is_file_var = false;
            let mut file_var_name_nodes: Vec<tree_sitter::Node> = Vec::new();

            // Short-var `x := expr` — no type field in the grammar.
            let mut is_local = false;
            let mut local_name_nodes: Vec<tree_sitter::Node> = Vec::new();
            let mut local_root_node = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if let Some(k_from_spec) = self
                    .capture_kind_by_idx
                    .get(cap.index as usize)
                    .copied()
                    .flatten()
                {
                    // Single config-driven dispatch replaces the four
                    // explicit Struct/Interface/Method/Function arms.
                    // Source of truth: GoSpec::CAPTURE_KIND in spec.rs.
                    name_node = Some(cap.node);
                    kind = Some(k_from_spec);
                } else if cap_idx == idx_struct
                    || cap_idx == idx_interface
                    || cap_idx == idx_method
                    || cap_idx == idx_function
                    || cap_idx == idx_const
                {
                    root_node = Some(cap.node);
                } else if cap_idx == idx_heritage {
                    if let Ok(h_name) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h_name.to_string());
                    }
                } else if cap_idx == idx_type {
                    if let Ok(t_name) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        if let Some(ref mut existing) = type_annotation {
                            existing.push(' ');
                            existing.push_str(t_name);
                        } else {
                            type_annotation = Some(t_name.to_string());
                        }
                    }
                } else if cap_idx == idx_import {
                    is_import = true;
                } else if cap_idx == idx_import_alias {
                    import_alias = Some(cap.node);
                } else if cap_idx == idx_import_source {
                    import_source = Some(cap.node);
                } else if cap_idx == idx_route_call {
                    is_route = true;
                    route_span_node = Some(cap.node);
                } else if cap_idx == idx_route_method {
                    route_method_node = Some(cap.node);
                } else if cap_idx == idx_route_path {
                    route_path_node = Some(cap.node);
                } else if cap_idx == idx_route_handler {
                    route_handler_node = Some(cap.node);
                } else if cap_idx == idx_field_name {
                    // One `field_declaration` can declare multiple names
                    // (`X, Y int`), so buffer name nodes here and emit one
                    // Property per name after the loop — that way every
                    // name picks up the shared `@field.type` text.
                    field_name_nodes.push(cap.node);
                } else if cap_idx == idx_field_type {
                    if let Ok(t_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        field_type_text = Some(t_str.to_string());
                    }
                } else if cap_idx == idx_var {
                    is_var = true;
                    var_root_node = Some(cap.node);
                } else if cap_idx == idx_var_name {
                    var_name_nodes.push(cap.node);
                } else if cap_idx == idx_var_type {
                    if let Ok(t_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        var_type_text = Some(t_str.to_string());
                    }
                } else if cap_idx == idx_variable {
                    is_file_var = true;
                } else if cap_idx == idx_variable_name {
                    file_var_name_nodes.push(cap.node);
                } else if cap_idx == idx_local {
                    is_local = true;
                    local_root_node = Some(cap.node);
                } else if cap_idx == idx_local_name {
                    // Only accept identifiers whose direct parent is the
                    // expression_list (left side of the declaration). The
                    // tree-sitter query descends into sub-expressions (e.g.
                    // `n` in `n.children`), so we guard here.
                    if cap
                        .node
                        .parent()
                        .is_some_and(|p| p.kind() == "expression_list")
                    {
                        local_name_nodes.push(cap.node);
                    }
                } else if cap_idx == idx_blind_method_by_name {
                    let (kind, hint) = BLIND_SPEC[0];
                    blind_spots.push(BlindSpot {
                        kind: kind.to_string(),
                        file_path: path.to_path_buf(),
                        span: node_span(&cap.node),
                        hint: hint.to_string(),
                    });
                } else if cap_idx == idx_blind_plugin_open {
                    let (kind, hint) = BLIND_SPEC[1];
                    blind_spots.push(BlindSpot {
                        kind: kind.to_string(),
                        file_path: path.to_path_buf(),
                        span: node_span(&cap.node),
                        hint: hint.to_string(),
                    });
                }
            }

            // Emit one Property per struct-field name, sharing the field's
            // type text. Span is the name token (keeps multi-name decls
            // distinct). owner_class = enclosing named struct type so two
            // structs with a same-named field (e.g. `FooBarFileStruct::File`
            // and `FooBarFileFailStruct::File`) get distinct uids.
            for name_node in &field_name_nodes {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                {
                    let name = name_str.to_string();
                    let is_exported = name.chars().next().is_some_and(|c| c.is_uppercase());
                    let start = name_node.start_position();
                    let end = name_node.end_position();
                    let owner = enclosing_struct_type(*name_node, source);
                    nodes.push(RawNode {
                        decorators: vec![],
                        name,
                        kind: NodeKind::Property,
                        is_exported,
                        heritage: vec![],
                        type_annotation: field_type_text.clone(),
                        span: (
                            start.row as u32,
                            start.column as u32,
                            end.row as u32,
                            end.column as u32,
                        ),
                        calls: Vec::new(),
                        owner_class: owner,
                        content_hash: ecp_core::uid::xxh3_64_bytes(
                            &source[name_node.start_byte()..name_node.end_byte()],
                        ),
                    });
                }
            }

            // Top-level `var n int = ...` → emit a Variable node with type.
            // Multi-name `var x, y int` emits one Variable per name.
            if is_var {
                if let Some(root) = var_root_node {
                    for n in &var_name_nodes {
                        if let Ok(name_str) =
                            std::str::from_utf8(&source[n.start_byte()..n.end_byte()])
                        {
                            if name_str == "_" {
                                continue;
                            }
                            let name = name_str.to_string();
                            let is_exported = name.chars().next().is_some_and(|c| c.is_uppercase());
                            let start = n.start_position();
                            let end = root.end_position();
                            nodes.push(RawNode {
                                decorators: vec![],
                                name,
                                kind: NodeKind::Variable,
                                is_exported,
                                heritage: vec![],
                                type_annotation: var_type_text.clone(),
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
                        }
                    }
                }
            }

            // File-scope `var X [T] = ...` — buffer one Variable per name.
            // Pushed to `file_var_pending` here; merged into `nodes` after the
            // match loop so the typed `@var` path (which runs in the same loop
            // but as a separate match) always takes precedence over this path.
            //
            // The `@variable` capture anchors at `source_file` (the whole file
            // node), so `file_var_root_node` is the source_file node. Using its
            // end_position() for span would mismatch the `@var` path's span
            // (which uses var_spec end), defeating the dedup check. We walk up
            // from the name identifier to find its parent `var_spec` instead,
            // so both paths produce the same (name_start, var_spec_end) span.
            if is_file_var {
                for n in &file_var_name_nodes {
                    if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()])
                    {
                        if name_str == "_" {
                            continue;
                        }
                        // Walk up: identifier → var_spec (could be
                        // identifier → var_spec directly, or through
                        // var_spec_list). Stop at the first var_spec ancestor.
                        let var_spec_root = {
                            let mut cur = n.parent();
                            loop {
                                match cur {
                                    None => break None,
                                    Some(p) if p.kind() == "var_spec" => break Some(p),
                                    Some(p) => cur = p.parent(),
                                }
                            }
                        };
                        let Some(var_spec) = var_spec_root else {
                            continue;
                        };
                        let name = name_str.to_string();
                        let is_exported = name.chars().next().is_some_and(|c| c.is_uppercase());
                        let start = n.start_position();
                        let end = var_spec.end_position();
                        file_var_pending.push(RawNode {
                            decorators: vec![],
                            name,
                            kind: NodeKind::Variable,
                            is_exported,
                            heritage: vec![],
                            type_annotation: None,
                            span: (
                                start.row as u32,
                                start.column as u32,
                                end.row as u32,
                                end.column as u32,
                            ),
                            calls: Vec::new(),
                            owner_class: None,
                            content_hash: ecp_core::uid::xxh3_64_bytes(
                                &source[var_spec.start_byte()..var_spec.end_byte()],
                            ),
                        });
                    }
                }
            }

            // Short-var `x := expr` — emit one Variable per identifier on the left.
            // type_annotation is always None (no type field in the grammar).
            //
            // owner_class = enclosing function name: `:=` is statement-level so
            // every short-var is inside a function or method. Without this, every
            // `pairs := ...` across all funcs in a file uid-collides (1,798 hits
            // on `Go/auth.go::pairs` on the .sample_repo corpus). Scoping to the
            // enclosing function/method makes each uid distinct while preserving
            // receiver-type call resolution (go_ctor.rs::test_go_short_var_*).
            if is_local {
                if let Some(root) = local_root_node {
                    let enclosing = enclosing_func_or_method_name(root, source);
                    for n in &local_name_nodes {
                        if let Ok(name_str) =
                            std::str::from_utf8(&source[n.start_byte()..n.end_byte()])
                        {
                            if name_str == "_" {
                                continue;
                            }
                            let name = name_str.to_string();
                            let is_exported = name.chars().next().is_some_and(|c| c.is_uppercase());
                            let start = n.start_position();
                            let end = root.end_position();
                            nodes.push(RawNode {
                                decorators: vec![],
                                name,
                                kind: NodeKind::Variable,
                                is_exported,
                                heritage: vec![],
                                type_annotation: None,
                                span: (
                                    start.row as u32,
                                    start.column as u32,
                                    end.row as u32,
                                    end.column as u32,
                                ),
                                calls: Vec::new(),
                                owner_class: enclosing.clone(),
                                content_hash: ecp_core::uid::xxh3_64_bytes(
                                    &source[root.start_byte()..root.end_byte()],
                                ),
                            });
                        }
                    }
                }
            }

            if let (Some(n), Some(k), Some(root)) = (name_node, kind, root_node) {
                if let Ok(name_str) = std::str::from_utf8(&source[n.start_byte()..n.end_byte()]) {
                    let name = name_str.to_string();
                    let is_exported = name.chars().next().is_some_and(|c| c.is_uppercase());
                    let start = root.start_position();
                    let end = root.end_position();

                    // At emit time: extract receiver type from method_declaration.
                    // Each match carries the full declaration node, so two methods
                    // with the same name on different receiver types are correctly
                    // distinguished without a HashMap collision.
                    let owner = if k == NodeKind::Method {
                        receiver_type_from_method_decl(root, source)
                    } else {
                        None
                    };

                    nodes.push(RawNode {
                        decorators: vec![],
                        name,
                        kind: k,
                        is_exported,
                        heritage,
                        type_annotation,
                        span: (
                            start.row as u32,
                            start.column as u32,
                            end.row as u32,
                            end.column as u32,
                        ),
                        calls: Vec::new(),
                        owner_class: owner,
                        content_hash: ecp_core::uid::xxh3_64_bytes(
                            &source[root.start_byte()..root.end_byte()],
                        ),
                    });
                }
            }

            if is_route {
                if let (Some(m_node), Some(p_node), Some(span_node)) =
                    (route_method_node, route_path_node, route_span_node)
                {
                    let method_str =
                        std::str::from_utf8(&source[m_node.start_byte()..m_node.end_byte()])
                            .unwrap_or("")
                            .to_string();
                    let path_raw =
                        std::str::from_utf8(&source[p_node.start_byte()..p_node.end_byte()])
                            .unwrap_or("");
                    let path_str = path_raw.trim_matches(|c| c == '"' || c == '`').to_string();
                    let start = span_node.start_position();
                    let end = span_node.end_position();

                    routes.push(ecp_core::analyzer::types::RawRoute {
                        method: method_str,
                        path: path_str,
                        handler: None,
                        span: (
                            start.row as u32,
                            start.column as u32,
                            end.row as u32,
                            end.column as u32,
                        ),
                    });

                    // Stage a framework_ref for gin / echo. We can't tell
                    // them apart from the route shape alone, so push to
                    // both pending lists; the import-gate pass below picks
                    // the matching one (or drops both if neither imported).
                    if let Some(h_node) = route_handler_node {
                        if let Ok(handler_name) =
                            std::str::from_utf8(&source[h_node.start_byte()..h_node.end_byte()])
                        {
                            let span = node_span(&h_node);
                            pending_gin.push((handler_name.to_string(), span));
                            pending_echo.push((handler_name.to_string(), span));
                        }
                    }
                }
            }

            if is_import {
                if let Some(src_node) = import_source {
                    if let Ok(src_quoted) =
                        std::str::from_utf8(&source[src_node.start_byte()..src_node.end_byte()])
                    {
                        let source_path = src_quoted
                            .trim_matches(|c| c == '"' || c == '`')
                            .to_string();

                        let alias = if let Some(alias_node) = import_alias {
                            std::str::from_utf8(
                                &source[alias_node.start_byte()..alias_node.end_byte()],
                            )
                            .ok()
                            .map(|s| s.to_string())
                        } else {
                            None
                        };

                        let imported_name = if let Some(ref a) = alias {
                            a.clone()
                        } else if let Some(last_part) = source_path.split('/').next_back() {
                            last_part.to_string()
                        } else {
                            source_path.clone()
                        };

                        imports.push(RawImport {
                            source: source_path,
                            alias,
                            imported_name,
                            binding_kind: None,
                        });
                    }
                }
            }
        }

        // Merge file-scope var candidates: only add names not already covered by
        // the typed `@var` path (which emits Variable nodes with type_annotation).
        // Span check prevents suppressing package-level vars when a local `:=`
        // shadows them by name — only skip when both name AND span match.
        for pending in file_var_pending {
            if !nodes.iter().any(|n| {
                n.name == pending.name && n.span == pending.span && n.kind == NodeKind::Variable
            }) {
                nodes.push(pending);
            }
        }

        // Deduplicate imports
        imports.sort_by(|a, b| {
            a.source
                .cmp(&b.source)
                .then(a.imported_name.cmp(&b.imported_name))
        });
        imports.dedup_by(|a, b| a.source == b.source && a.imported_name == b.imported_name);

        // Extract call sites with receiver-type binding. Replaces the shared
        // `extract_calls` for Go so `obj.Method()` can be recorded as
        // `Type.Method` when `obj`'s type is locally known.
        let recv_map = build_receiver_map(tree.root_node(), source);
        let local_types = collect_local_types(tree.root_node(), source, &recv_map);
        extract_go_calls(tree.root_node(), source, &mut nodes, &local_types);

        // owner_class is set at emit time via receiver_type_from_method_decl().
        // No post-loop needed; recv_map is still used for call-site resolution.

        // Gate-and-emit pending gin / echo framework refs. Both lists hold
        // the same handlers (we couldn't tell them apart at capture time);
        // only one matches an import gate, so at most one set is emitted.
        let mut framework_refs: Vec<RawFrameworkRef> = Vec::new();
        if has_import_from(&imports, GIN_REQUIRED) {
            for (target_name, span) in &pending_gin {
                let source_name = enclosing_function_name(&nodes, *span)
                    .unwrap_or_else(|| MODULE_LEVEL_SOURCE.to_string());
                framework_refs.push(RawFrameworkRef {
                    source_name,
                    target_name: target_name.clone(),
                    confidence: framework_confidence::GIN_ROUTE,
                    reason: "gin-route".to_string(),
                    span: *span,
                });
            }
        } else if has_import_from(&imports, ECHO_REQUIRED) {
            for (target_name, span) in &pending_echo {
                let source_name = enclosing_function_name(&nodes, *span)
                    .unwrap_or_else(|| MODULE_LEVEL_SOURCE.to_string());
                framework_refs.push(RawFrameworkRef {
                    source_name,
                    target_name: target_name.clone(),
                    confidence: framework_confidence::ECHO_ROUTE,
                    reason: "echo-route".to_string(),
                    span: *span,
                });
            }
        }

        let file_category = if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with("_test.go"))
            .unwrap_or(false)
        {
            ecp_core::graph::FileCategory::Test
        } else {
            ecp_core::graph::FileCategory::Source
        };
        let raw_function_metas =
            crate::function_meta::go::extract(tree.root_node(), source, &nodes, file_category);

        let event_topics = {
            let topics = crate::event_topic::extract_event_topics(
                &tree,
                source,
                &self.query,
                &[
                    crate::event_topic::REDIS_GO,
                    crate::event_topic::KAFKA_GO,
                    crate::event_topic::RABBITMQ_GO,
                    crate::event_topic::SQS_GO,
                ],
                &imports,
            );
            (!topics.is_empty()).then(|| topics.into_boxed_slice())
        };

        Ok(LocalGraph {
            content_hash: [0; 8],
            routes,
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs,
            fanout_refs: vec![],
            blind_spots,
            schema_fields: None,
            event_topics,
            tx_scopes: None,
            call_metas: vec![],
            raw_function_metas,
        })
    }
}
