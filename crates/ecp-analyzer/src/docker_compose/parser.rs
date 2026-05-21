/// Docker Compose schema-aware YAML provider.
///
/// Reuses `tree-sitter-yaml` (no new grammar dep) and layers a schema
/// interpretation walk on top of the raw AST.
///
/// Semantic mapping:
///   services.<name>               → NodeKind::Class  (service = class-like entity)
///   services.<name>.image: …      → RawImport (image is an import-like dep)
///   services.<name>.build: …      → RawImport (Dockerfile dep)
///   services.<name>.depends_on[]  → RawNode call edge via `calls` field
///   services.<name>.environment.X → NodeKind::Const
///   top-level version/networks/volumes → ignored
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use tree_sitter::{Node, Parser, Query};

pub struct DockerComposeProvider {
    /// The query is only used to force the grammar to compile and to satisfy
    /// the pattern that every provider owns a Query. Actual extraction is done
    /// by the Rust AST walker below.
    #[allow(dead_code)]
    query: Query,
}

impl DockerComposeProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_yaml::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)
            .map_err(|e| anyhow::anyhow!("DockerCompose query compile: {}", e))?;
        Ok(Self { query })
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Extract the raw text slice for a node from the source bytes.
fn node_text<'s>(node: Node, source: &'s [u8]) -> &'s str {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).unwrap_or("")
}

/// Walk a `block_mapping` and collect (key_text, value_node) pairs.
/// Returns an empty vec if the node is not a block_mapping.
fn mapping_pairs<'tree>(node: Node<'tree>) -> Vec<(Node<'tree>, Node<'tree>)> {
    let target = if node.kind() == "block_mapping" {
        node
    } else if node.kind() == "block_node" {
        // block_node wraps a block_mapping
        match node.child(0) {
            Some(c) if c.kind() == "block_mapping" => c,
            _ => return vec![],
        }
    } else {
        return vec![];
    };

    let mut pairs = Vec::new();
    let mut cursor = target.walk();
    for child in target.children(&mut cursor) {
        if child.kind() == "block_mapping_pair" {
            let key = child.child_by_field_name("key");
            let val = child.child_by_field_name("value");
            if let (Some(k), Some(v)) = (key, val) {
                pairs.push((k, v));
            }
        }
    }
    pairs
}

/// Resolve a key node (flow_node > plain_scalar > string_scalar, or similar)
/// down to its plain string text.
fn key_str<'s>(key_node: Node, source: &'s [u8]) -> &'s str {
    // Key can be: flow_node → plain_scalar → string_scalar  (text lives there)
    // or just plain_scalar for simple keys.
    let mut n = key_node;
    while matches!(n.kind(), "flow_node" | "plain_scalar") {
        match n.child(0) {
            Some(c) => n = c,
            None => break,
        }
    }
    node_text(n, source).trim()
}

/// Resolve a scalar value node to its string content (strips quotes).
fn scalar_str<'s>(val_node: Node, source: &'s [u8]) -> Option<&'s str> {
    // value is a block_node or flow_node containing a scalar
    let mut n = val_node;
    loop {
        match n.kind() {
            "block_node"
            | "flow_node"
            | "plain_scalar"
            | "double_quote_scalar"
            | "single_quote_scalar" => {
                if let Some(c) = n.child(0) {
                    n = c;
                } else {
                    break;
                }
            }
            "string_scalar" => return Some(node_text(n, source).trim()),
            _ => break,
        }
    }
    // Fallback: if it's a leaf, return its text directly
    let t = node_text(n, source).trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// Collect items from a block_sequence (YAML list) as text strings.
fn sequence_items<'s>(val_node: Node, source: &'s [u8]) -> Vec<&'s str> {
    // Navigate: block_node → block_sequence → block_sequence_item → block_node → …
    let mut n = val_node;
    if n.kind() == "block_node" {
        n = match n.child(0) {
            Some(c) => c,
            None => return vec![],
        };
    }
    if n.kind() != "block_sequence" {
        return vec![];
    }
    let mut items = Vec::new();
    let mut cur = n.walk();
    for item in n.children(&mut cur) {
        if item.kind() == "block_sequence_item" {
            // block_sequence_item child 0 is the dash, child 1 is the value
            if let Some(v) = item.child(1) {
                if let Some(s) = scalar_str(v, source) {
                    items.push(s);
                }
            }
        }
    }
    items
}

// ── LanguageProvider impl ─────────────────────────────────────────────────────

impl LanguageProvider for DockerComposeProvider {
    fn name(&self) -> &'static str {
        "docker-compose"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_yaml::LANGUAGE.into())
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let tree = parse_with_budget(&mut parser, source, ParseBudget::DEFAULT)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse docker-compose file"))?;

        let root = tree.root_node();
        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();

        // Navigate: stream → document → block_node → block_mapping
        let doc = {
            let mut found = None;
            let mut cur = root.walk();
            for child in root.children(&mut cur) {
                if child.kind() == "document" {
                    found = Some(child);
                    break;
                }
            }
            match found {
                Some(d) => d,
                None => {
                    return Ok(LocalGraph {
                        content_hash: [0; 8],
                        routes: vec![],
                        file_path: path.to_path_buf(),
                        nodes,
                        imports,
                        documents: vec![],
                        framework_refs: vec![],
                        fanout_refs: vec![],
                        blind_spots: vec![],
                        schema_fields: None,
                        event_topics: None,
                        tx_scopes: None,
                        call_metas: vec![],
                    });
                }
            }
        };

        // Top-level mapping pairs
        let top_pairs = {
            let mut found_mapping = None;
            let mut cur = doc.walk();
            for child in doc.children(&mut cur) {
                if child.kind() == "block_node" {
                    if let Some(m) = child.child(0) {
                        if m.kind() == "block_mapping" {
                            found_mapping = Some(child);
                            break;
                        }
                    }
                }
            }
            match found_mapping {
                Some(bn) => mapping_pairs(bn),
                None => vec![],
            }
        };

        for (top_key_node, top_val_node) in top_pairs {
            let top_key = key_str(top_key_node, source);
            if top_key != "services" {
                continue;
            }
            // top_val_node is the services mapping
            let service_pairs = mapping_pairs(top_val_node);
            for (svc_key_node, svc_val_node) in service_pairs {
                let svc_name = key_str(svc_key_node, source).to_string();
                if svc_name.is_empty() {
                    continue;
                }

                let svc_start = svc_key_node.start_position();
                let svc_end = svc_val_node.end_position();

                // Collect depends_on targets for the service's `calls` field.
                let mut calls: Vec<String> = Vec::new();
                // Collect environment vars and image/build imports from this service.
                let service_fields = mapping_pairs(svc_val_node);
                for (fld_key_node, fld_val_node) in &service_fields {
                    let field = key_str(*fld_key_node, source);
                    match field {
                        "image" => {
                            if let Some(img) = scalar_str(*fld_val_node, source) {
                                imports.push(RawImport {
                                    alias: Some(svc_name.clone()),
                                    imported_name: img.to_string(),
                                    source: img.to_string(),
                                    binding_kind: None,
                                });
                            }
                        }
                        "build" => {
                            if let Some(build_path) = scalar_str(*fld_val_node, source) {
                                imports.push(RawImport {
                                    alias: Some(svc_name.clone()),
                                    imported_name: build_path.to_string(),
                                    source: build_path.to_string(),
                                    binding_kind: None,
                                });
                            }
                        }
                        "depends_on" => {
                            for dep in sequence_items(*fld_val_node, source) {
                                calls.push(dep.to_string());
                            }
                        }
                        "environment" => {
                            // environment can be a mapping (KEY: val) or a sequence (KEY=val)
                            let env_pairs = mapping_pairs(*fld_val_node);
                            for (env_key_node, env_val_node) in env_pairs {
                                let env_name = key_str(env_key_node, source).to_string();
                                if env_name.is_empty() {
                                    continue;
                                }
                                let env_start = env_key_node.start_position();
                                let env_end = env_val_node.end_position();
                                nodes.push(RawNode {
                                    decorators: vec![],
                                    is_exported: false,
                                    heritage: vec![],
                                    type_annotation: None,
                                    name: env_name,
                                    kind: NodeKind::Const,
                                    span: (
                                        env_start.row as u32,
                                        env_start.column as u32,
                                        env_end.row as u32,
                                        env_end.column as u32,
                                    ),
                                    calls: vec![],
                                });
                            }
                        }
                        _ => {}
                    }
                }

                // Emit the service as a Class node.
                nodes.push(RawNode {
                    decorators: vec![],
                    is_exported: true,
                    heritage: vec![],
                    type_annotation: None,
                    name: svc_name,
                    kind: NodeKind::Class,
                    span: (
                        svc_start.row as u32,
                        svc_start.column as u32,
                        svc_end.row as u32,
                        svc_end.column as u32,
                    ),
                    calls,
                });
            }
        }

        Ok(LocalGraph {
            content_hash: [0; 8],
            routes: vec![],
            file_path: path.to_path_buf(),
            nodes,
            imports,
            documents: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            call_metas: vec![],
        })
    }
}
