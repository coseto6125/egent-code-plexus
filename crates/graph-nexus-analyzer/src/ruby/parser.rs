use super::receiver_types::extract_ruby_calls;
use crate::framework_confidence;
use crate::framework_helpers::{detect_ast_framework_patterns, FrameworkPatternSpec};
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode, RawRoute};
use graph_nexus_core::graph::NodeKind;
use std::collections::HashMap;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor};

/// Per upstream `ruby.ts:156-178` `astFrameworkPatterns`.
const RUBY_FRAMEWORKS: &[FrameworkPatternSpec] = &[
    FrameworkPatternSpec {
        framework: "rails",
        reason: "rails-pattern",
        confidence: framework_confidence::RAILS_HINT,
        patterns: &[
            "ApplicationController",
            "ApplicationRecord",
            "ActiveRecord::Base",
            "before_action",
            "after_action",
            "has_many",
            "belongs_to",
            "has_one",
            "validates",
        ],
    },
    FrameworkPatternSpec {
        framework: "sinatra",
        reason: "sinatra-pattern",
        confidence: framework_confidence::SINATRA_HINT,
        patterns: &["Sinatra::Base", "Sinatra::Application"],
    },
];

thread_local! {
    static PARSER: std::cell::RefCell<tree_sitter::Parser> = std::cell::RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_ruby::LANGUAGE.into();
        parser.set_language(&language).expect("Failed to set language");
        parser
    });
}
/// Walks a `body_statement` (or any block node) and builds a map of
/// `start_row → is_exported` for every `method` / `singleton_method` child.
///
/// Ruby visibility rules: methods are `public` by default.  A bare call to
/// `private`, `protected`, or `public` (an `identifier` node in tree-sitter)
/// changes the visibility for every method that follows it within the same
/// `body_statement`, until the next visibility marker or end-of-scope.
fn build_visibility_map(node: Node<'_>, source: &[u8]) -> HashMap<u32, bool> {
    let mut map = HashMap::new();
    let mut is_public = true;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                if let Ok(text) = std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                {
                    match text {
                        "private" | "protected" => is_public = false,
                        "public" => is_public = true,
                        _ => {}
                    }
                }
            }
            "method" | "singleton_method" => {
                map.insert(child.start_position().row as u32, is_public);
                // Recurse into nested body_statements (nested classes/modules).
                let mut c2 = child.walk();
                for sub in child.children(&mut c2) {
                    if sub.kind() == "body_statement" {
                        map.extend(build_visibility_map(sub, source));
                    }
                }
            }
            "class" | "module" => {
                // Recurse into nested class/module body.
                let mut c2 = child.walk();
                for sub in child.children(&mut c2) {
                    if sub.kind() == "body_statement" {
                        map.extend(build_visibility_map(sub, source));
                    }
                }
            }
            _ => {}
        }
    }
    map
}

/// Append a Ruby named binding (`alias` keyword, `alias_method`, or constant
/// alias) as a `RawImport` with `alias = Some(new_name)`. De-duplicates on
/// (imported_name, source) to keep repeated parses idempotent.
fn push_alias_binding(imports: &mut Vec<RawImport>, new_name: &str, source: &str) {
    let exists = imports
        .iter()
        .any(|i| i.imported_name == new_name && i.source == source);
    if !exists {
        imports.push(RawImport {
            alias: Some(new_name.to_string()),
            imported_name: new_name.to_string(),
            source: source.to_string(),
            binding_kind: None,
        });
    }
}

/// Strip a leading `:` (symbol prefix) and a leading `@` (instance-var prefix)
/// from a `def_delegator` / `delegate` argument so the result is a plain
/// receiver / method name suitable for `RawImport.source` composition.
///
/// `:@songs` → `songs`, `:method` → `method`, `:customer` → `customer`.
fn strip_symbol_prefix(s: &str) -> &str {
    let after_colon = s.strip_prefix(':').unwrap_or(s);
    after_colon.strip_prefix('@').unwrap_or(after_colon)
}

pub struct RubyProvider {
    query: Query,
}

impl RubyProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_ruby::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for RubyProvider {
    fn name(&self) -> &'static str {
        "ruby"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let tree = PARSER
            .with(|p| p.borrow_mut().parse(source, None))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        // Build method-row → is_exported map from visibility markers in class bodies.
        let visibility_map = build_visibility_map(tree.root_node(), source);

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();
        let mut routes: Vec<RawRoute> = Vec::new();
        // Mixin module additions, applied after primary node emission. Each
        // entry is (module_name, call_line) — we attach to the smallest
        // enclosing class node by span containment. Document-order traversal
        // of tree-sitter matches preserves source ordering (M1 before M2).
        let mut pending_mixins: Vec<(String, u32)> = Vec::new();

        let idx_name = self.query.capture_index_for_name("name");
        let idx_heritage = self.query.capture_index_for_name("heritage");
        let idx_class = self.query.capture_index_for_name("class");
        let idx_module = self.query.capture_index_for_name("module");
        let idx_method = self.query.capture_index_for_name("method");
        let idx_import_name = self.query.capture_index_for_name("import.name");
        let idx_decorator = self.query.capture_index_for_name("decorator");
        let idx_route_method = self.query.capture_index_for_name("route.method");
        let idx_route_path = self.query.capture_index_for_name("route.path");
        let idx_route = self.query.capture_index_for_name("route");
        let idx_attr_args = self.query.capture_index_for_name("attr_args");
        let idx_mixin_module = self.query.capture_index_for_name("mixin_module");
        let idx_alias_new = self.query.capture_index_for_name("alias.new");
        let idx_alias_old = self.query.capture_index_for_name("alias.old");
        let idx_alias_method_args = self.query.capture_index_for_name("alias_method.args");
        let idx_const_alias_new = self.query.capture_index_for_name("const_alias.new");
        let idx_const_alias_source = self.query.capture_index_for_name("const_alias.source");
        let idx_delegator_method = self.query.capture_index_for_name("delegator_method");
        let idx_delegator_args = self.query.capture_index_for_name("delegator_args");

        // Pending delegator emissions: (target, method, line). Applied after
        // the match loop so we can cross-check against `pending_mixins` to
        // require the enclosing class to `extend`/`include Forwardable`.
        // Without a Forwardable mixin we fall back to "low-confidence" emit
        // (still pushed, documented false-positive on user-defined methods
        // named `def_delegator` / `delegate`).
        let mut pending_delegators: Vec<(String, String, u32)> = Vec::new();

        while let Some(m) = matches.next() {
            let mut node_name = None;
            let mut kind = None;
            let mut root_node = None;
            let mut heritage = Vec::new();
            let mut import_name = None;
            let mut decorators = Vec::new();

            let mut route_method = None;
            let mut route_path = None;
            let mut route_root = None;

            let mut attr_args_node: Option<tree_sitter::Node<'_>> = None;
            let mut mixin_module_node: Option<tree_sitter::Node<'_>> = None;
            let mut alias_new_node: Option<tree_sitter::Node<'_>> = None;
            let mut alias_old_node: Option<tree_sitter::Node<'_>> = None;
            let mut alias_method_args: Option<tree_sitter::Node<'_>> = None;
            let mut const_alias_new_node: Option<tree_sitter::Node<'_>> = None;
            let mut const_alias_source_node: Option<tree_sitter::Node<'_>> = None;
            let mut delegator_method_node: Option<tree_sitter::Node<'_>> = None;
            let mut delegator_args_node: Option<tree_sitter::Node<'_>> = None;

            for cap in m.captures {
                let cap_idx = Some(cap.index);
                if cap_idx == idx_name {
                    node_name = Some(cap.node);
                } else if cap_idx == idx_heritage {
                    if let Ok(h_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        heritage.push(h_str.to_string());
                    }
                } else if cap_idx == idx_class {
                    kind = Some(NodeKind::Class);
                    root_node = Some(cap.node);
                } else if cap_idx == idx_module {
                    kind = Some(NodeKind::Class); // Modules are treated as Class for graph
                    root_node = Some(cap.node);
                } else if cap_idx == idx_method {
                    kind = Some(NodeKind::Method);
                    root_node = Some(cap.node);
                } else if cap_idx == idx_import_name {
                    import_name = Some(cap.node);
                } else if cap_idx == idx_decorator {
                    if let Ok(d_str) =
                        std::str::from_utf8(&source[cap.node.start_byte()..cap.node.end_byte()])
                    {
                        decorators.push(d_str.to_string());
                    }
                } else if cap_idx == idx_route_method {
                    route_method = Some(cap.node);
                } else if cap_idx == idx_route_path {
                    route_path = Some(cap.node);
                } else if cap_idx == idx_route {
                    route_root = Some(cap.node);
                } else if cap_idx == idx_attr_args {
                    attr_args_node = Some(cap.node);
                } else if cap_idx == idx_mixin_module {
                    mixin_module_node = Some(cap.node);
                } else if cap_idx == idx_alias_new {
                    alias_new_node = Some(cap.node);
                } else if cap_idx == idx_alias_old {
                    alias_old_node = Some(cap.node);
                } else if cap_idx == idx_alias_method_args {
                    alias_method_args = Some(cap.node);
                } else if cap_idx == idx_const_alias_new {
                    const_alias_new_node = Some(cap.node);
                } else if cap_idx == idx_const_alias_source {
                    const_alias_source_node = Some(cap.node);
                } else if cap_idx == idx_delegator_method {
                    delegator_method_node = Some(cap.node);
                } else if cap_idx == idx_delegator_args {
                    delegator_args_node = Some(cap.node);
                }
            }

            if let (Some(name_node), Some(k), Some(root)) = (node_name, kind, root_node) {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[name_node.start_byte()..name_node.end_byte()])
                {
                    let start = root.start_position();
                    let end = root.end_position();
                    // Methods: respect visibility markers. Classes/modules are always exported.
                    let is_exported = if k == NodeKind::Method {
                        *visibility_map.get(&(start.row as u32)).unwrap_or(&true)
                    } else {
                        true
                    };
                    nodes.push(RawNode {
                        decorators: decorators.clone(),
                        is_exported,
                        heritage,
                        type_annotation: None,
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

            if let Some(i_node) = import_name {
                if let Ok(name_str) =
                    std::str::from_utf8(&source[i_node.start_byte()..i_node.end_byte()])
                {
                    imports.push(RawImport {
                        alias: None,
                        imported_name: name_str.to_string(),
                        source: name_str.to_string(),
                        binding_kind: None,
                    });
                }
            }

            if let (Some(r_method), Some(r_path), Some(r_root)) =
                (route_method, route_path, route_root)
            {
                if let (Ok(method_str), Ok(path_str)) = (
                    std::str::from_utf8(&source[r_method.start_byte()..r_method.end_byte()]),
                    std::str::from_utf8(&source[r_path.start_byte()..r_path.end_byte()]),
                ) {
                    let start = r_root.start_position();
                    let end = r_root.end_position();
                    routes.push(RawRoute {
                        method: method_str.to_string(),
                        path: path_str.to_string(),
                        handler: None,
                        span: (
                            start.row as u32,
                            start.column as u32,
                            end.row as u32,
                            end.column as u32,
                        ),
                    });
                }
            }

            // attr_reader / attr_writer / attr_accessor → emit one Property per symbol.
            // is_exported=true unconditionally; private-block detection is punted for MVP
            // because tree-sitter parses `private` as just another bareword call without
            // a structural block — distinguishing "below a private call" from "above"
            // requires a stateful AST sweep that's out of scope for this pass.
            if let Some(args) = attr_args_node {
                let mut walker = args.walk();
                for child in args.named_children(&mut walker) {
                    if child.kind() != "simple_symbol" {
                        continue;
                    }
                    let Ok(sym_text) =
                        std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
                    else {
                        continue;
                    };
                    let prop_name = sym_text.strip_prefix(':').unwrap_or(sym_text);
                    if prop_name.is_empty() {
                        continue;
                    }
                    let start = child.start_position();
                    let end = child.end_position();
                    nodes.push(RawNode {
                        name: prop_name.to_string(),
                        kind: NodeKind::Property,
                        span: (
                            start.row as u32,
                            start.column as u32,
                            end.row as u32,
                            end.column as u32,
                        ),
                        is_exported: true,
                        heritage: Vec::new(),
                        type_annotation: None,
                        decorators: Vec::new(),
                        calls: Vec::new(),
                    });
                }
            }

            // include / extend → queue the module name for attachment to the
            // enclosing class's heritage after all class nodes are emitted.
            if let Some(mm) = mixin_module_node {
                if let Ok(mm_str) = std::str::from_utf8(&source[mm.start_byte()..mm.end_byte()]) {
                    let line = mm.start_position().row as u32;
                    pending_mixins.push((mm_str.to_string(), line));
                }
            }

            // `alias new old` keyword → named binding.
            if let (Some(n), Some(o)) = (alias_new_node, alias_old_node) {
                if let (Ok(new_name), Ok(old_name)) = (
                    std::str::from_utf8(&source[n.start_byte()..n.end_byte()]),
                    std::str::from_utf8(&source[o.start_byte()..o.end_byte()]),
                ) {
                    push_alias_binding(&mut imports, new_name, old_name);
                }
            }

            // `alias_method :new, :old` metaprogramming → named binding.
            // Walk the argument_list and grab the first two simple_symbols.
            if let Some(args) = alias_method_args {
                let mut walker = args.walk();
                let symbols: Vec<&str> = args
                    .named_children(&mut walker)
                    .filter(|c| c.kind() == "simple_symbol")
                    .filter_map(|c| std::str::from_utf8(&source[c.start_byte()..c.end_byte()]).ok())
                    .map(|s| s.strip_prefix(':').unwrap_or(s))
                    .filter(|s| !s.is_empty())
                    .take(2)
                    .collect();
                if let [new_name, old_name] = symbols.as_slice() {
                    push_alias_binding(&mut imports, new_name, old_name);
                }
            }

            // `MyConst = OtherModule::Const` → named binding.
            // lhs is guaranteed `(constant)` by the query, so the assignment
            // is a constant alias (not a local variable assignment).
            if let (Some(lhs), Some(rhs)) = (const_alias_new_node, const_alias_source_node) {
                if let (Ok(new_name), Ok(source_path)) = (
                    std::str::from_utf8(&source[lhs.start_byte()..lhs.end_byte()]),
                    std::str::from_utf8(&source[rhs.start_byte()..rhs.end_byte()]),
                ) {
                    push_alias_binding(&mut imports, new_name, source_path);
                }
            }

            // `def_delegator :target, :method` / `def_delegators :target, :m1, ...` /
            // `delegate :m1, :m2, to: :target` — parse argument list shape per
            // method name. Each delegated method is queued for emission; the
            // Forwardable-mixin check runs after the match loop because the
            // enclosing class span and `pending_mixins` aren't both finalised
            // until then.
            if let (Some(method_node), Some(args)) = (delegator_method_node, delegator_args_node) {
                let method_name =
                    std::str::from_utf8(&source[method_node.start_byte()..method_node.end_byte()])
                        .unwrap_or("");
                let call_line = method_node.start_position().row as u32;
                let mut walker = args.walk();
                let children: Vec<tree_sitter::Node<'_>> =
                    args.named_children(&mut walker).collect();

                match method_name {
                    "def_delegator" | "def_delegators" => {
                        // First simple_symbol = target; rest = delegated methods.
                        let mut symbols = children
                            .iter()
                            .filter(|c| c.kind() == "simple_symbol")
                            .filter_map(|c| {
                                std::str::from_utf8(&source[c.start_byte()..c.end_byte()]).ok()
                            });
                        if let Some(target_raw) = symbols.next() {
                            let target = strip_symbol_prefix(target_raw).to_string();
                            if !target.is_empty() {
                                for m_raw in symbols {
                                    let m = strip_symbol_prefix(m_raw);
                                    if !m.is_empty() {
                                        pending_delegators.push((
                                            target.clone(),
                                            m.to_string(),
                                            call_line,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    "delegate" => {
                        // simple_symbol* (methods), then `pair` with key=`to`,
                        // value=simple_symbol (target). Walk in order, collect
                        // method names until the `to:` pair is hit.
                        let mut methods: Vec<String> = Vec::new();
                        let mut target: Option<String> = None;
                        for child in &children {
                            match child.kind() {
                                "simple_symbol" => {
                                    if let Ok(s) = std::str::from_utf8(
                                        &source[child.start_byte()..child.end_byte()],
                                    ) {
                                        let stripped = strip_symbol_prefix(s);
                                        if !stripped.is_empty() {
                                            methods.push(stripped.to_string());
                                        }
                                    }
                                }
                                "pair" => {
                                    // Look for key=hash_key_symbol with text "to"
                                    // and value=simple_symbol/string.
                                    let key = child.child_by_field_name("key");
                                    let value = child.child_by_field_name("value");
                                    let key_text = key.and_then(|k| {
                                        std::str::from_utf8(&source[k.start_byte()..k.end_byte()])
                                            .ok()
                                    });
                                    if key_text == Some("to") {
                                        if let Some(v) = value {
                                            if let Ok(t) = std::str::from_utf8(
                                                &source[v.start_byte()..v.end_byte()],
                                            ) {
                                                target = Some(strip_symbol_prefix(t).to_string());
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        if let Some(t) = target {
                            if !t.is_empty() {
                                for m in methods {
                                    pending_delegators.push((t.clone(), m, call_line));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Helper: locate the smallest-span class RawNode whose body contains
        // `line`. Returns its index in `nodes`. Shared between mixin
        // application and the delegator Forwardable-scope check below.
        let enclosing_class_idx = |nodes: &[RawNode], line: u32| -> Option<usize> {
            let mut best: Option<usize> = None;
            let mut best_span: u32 = u32::MAX;
            for (i, n) in nodes.iter().enumerate() {
                if n.kind != NodeKind::Class {
                    continue;
                }
                if n.span.0 <= line && n.span.2 >= line {
                    let width = n.span.2 - n.span.0;
                    if width < best_span {
                        best_span = width;
                        best = Some(i);
                    }
                }
            }
            best
        };

        // Apply mixins: for each (module, line), find the smallest enclosing
        // class RawNode by span containment and append the module to its
        // heritage. Mixins outside any class are dropped (matches Ruby
        // semantics — bare top-level `include` is rare and out of scope).
        for (module_name, line) in &pending_mixins {
            if let Some(i) = enclosing_class_idx(&nodes, *line) {
                nodes[i].heritage.push(module_name.clone());
            }
        }

        // Apply delegators: `def_delegator/s` / `delegate` add a method
        // binding on the enclosing class. We require `extend Forwardable`
        // (or `include Forwardable`) in the same enclosing class as a
        // sanity check; without it we still emit (Option-A fallback per
        // spec §4) at the cost of a known false positive when the user
        // defines their own method named `def_delegator`.
        //
        // The emitted RawImport mirrors the alias-keyword shape:
        // `{ alias: Some(method), imported_name: method, source: "target.method" }`
        // so downstream rename / resolution code reuses the existing path.
        for (target, method, line) in pending_delegators {
            let enclosing = enclosing_class_idx(&nodes, line);
            let _has_forwardable = enclosing.is_some_and(|idx| {
                let span = nodes[idx].span;
                pending_mixins
                    .iter()
                    .any(|(m, ml)| m == "Forwardable" && *ml >= span.0 && *ml <= span.2)
            });
            // Emit regardless of `_has_forwardable` — Option-A low-confidence
            // fallback per docs/specs/2026-05-16-ruby-receiver-aware-resolver.md.
            // The flag is retained as a future hook for telemetry / a
            // BindingKind discriminant when one becomes available.
            let source_path = format!("{target}.{method}");
            push_alias_binding(&mut imports, &method, &source_path);
            // Also materialise the delegator as a Method RawNode so cross-file
            // mixin chains resolve via Tier 2.75 (HeritageScoped): without a
            // real node in the originating module the alias is only visible
            // to same-file lookups, leaving `class Bar; include Foo; end`
            // callers unable to reach `Foo`'s delegated methods.
            nodes.push(RawNode {
                decorators: vec![],
                is_exported: true,
                heritage: vec![],
                type_annotation: None,
                name: method.clone(),
                kind: NodeKind::Method,
                span: (line, 0, line, 0),
                calls: vec![],
            });
        }

        // Extract call sites with receiver-type binding.
        // Handles self.method → EnclosingClass.method, Constant.method → Constant.method.
        extract_ruby_calls(tree.root_node(), source, &mut nodes);

        let framework_refs = detect_ast_framework_patterns(source, RUBY_FRAMEWORKS);

        // Path-shape filter — drop generic route captures whose first
        // string arg doesn't look like an HTTP route. Same rationale as
        // the JS/TS/Python parsers; spec:
        // `docs/superpowers/specs/2026-05-17-route-precision-design.md`.
        routes.retain_mut(|r| match crate::route_detector::clean_route_path(&r.path) {
            Some(clean) => {
                r.path = clean;
                true
            }
            None => false,
        });

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes,
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
