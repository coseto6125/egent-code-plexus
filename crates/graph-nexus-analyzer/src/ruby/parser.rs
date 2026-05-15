use super::receiver_types::extract_ruby_calls;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode, RawRoute};
use graph_nexus_core::graph::NodeKind;
use std::collections::HashMap;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor};

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
                if let Ok(text) =
                    std::str::from_utf8(&source[child.start_byte()..child.end_byte()])
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
                        *visibility_map
                            .get(&(start.row as u32))
                            .unwrap_or(&true)
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
        }

        // Apply mixins: for each (module, line), find the smallest enclosing
        // class RawNode by span containment and append the module to its
        // heritage. Mixins outside any class are dropped (matches Ruby
        // semantics — bare top-level `include` is rare and out of scope).
        for (module_name, line) in pending_mixins {
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
            if let Some(i) = best {
                nodes[i].heritage.push(module_name);
            }
        }

        // Extract call sites with receiver-type binding.
        // Handles self.method → EnclosingClass.method, Constant.method → Constant.method.
        extract_ruby_calls(tree.root_node(), source, &mut nodes);

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes,
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
        })
    }
}
