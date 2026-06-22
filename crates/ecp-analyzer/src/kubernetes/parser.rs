/// Kubernetes manifest schema-aware YAML provider.
///
/// Reuses `tree-sitter-yaml` (no new grammar dep) and layers a schema
/// interpretation walk on top of the raw AST — modeled on
/// `DockerComposeProvider`.
///
/// Semantic mapping (mirrors compose's `service=Class`, `image=RawImport`):
///   <doc>.metadata.name        → NodeKind::Class  (a k8s resource is a
///                                service-like entity); `kind` (Deployment /
///                                Service / …) lands in `type_annotation` so an
///                                LLM can disambiguate two same-named resources.
///   <doc>…containers[].image   → RawImport (the container image is an
///                                import-like dep — THE load-bearing edge that
///                                links the manifest to the Dockerfile / registry
///                                artifact, so "what runs this service" resolves
///                                instead of dead-ending at the YAML).
///   container `name`           → ignored (noise; matches compose restraint).
///   apiVersion / status / spec scalars → ignored.
///
/// Multi-document files (`---`-separated) are common in k8s (Service +
/// Deployment bundled); every `document` child is walked, not just the first.
///
/// Dispatch: `.yaml`/`.yml` cannot be identified as k8s by path alone (shared
/// with arbitrary YAML), so the provider content-sniffs `apiVersion:` + `kind:`
/// in the first ~1KB before any parse work — non-k8s YAML costs near-zero.
use crate::parse_budget::{parse_with_budget, ParseBudget};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;
use tree_sitter::{Node, Parser};

pub struct KubernetesProvider;

impl KubernetesProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self)
    }
}

// ── content sniff ─────────────────────────────────────────────────────────────

/// Cheap byte-level gate: a k8s manifest has top-level `apiVersion:` AND
/// `kind:` keys. Checks the first ≤1KB for both markers at column 0 (start of
/// file or after a newline, optionally after a `---` document separator).
///
/// Runs before UTF-8 validation and before any tree-sitter parse — no
/// allocation, no parse. `pub` so the routing/registration layer (and tests)
/// can reuse the exact gate.
pub fn has_k8s_markers(probe: &[u8]) -> bool {
    line_key_present(probe, b"apiVersion:") && line_key_present(probe, b"kind:")
}

/// True when `needle` appears at the start of a line within `hay` (column 0,
/// i.e. file start or immediately after `\n`). k8s top-level keys are
/// unindented, so a column-0 match avoids false positives on nested keys like
/// `spec.template.metadata.kind`-shaped paths in arbitrary YAML.
fn line_key_present(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len())
        .enumerate()
        .any(|(i, w)| w == needle && (i == 0 || hay[i - 1] == b'\n'))
}

// ── AST helpers (shared shape with docker_compose) ────────────────────────────

fn node_text<'s>(node: Node, source: &'s [u8]) -> &'s str {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).unwrap_or("")
}

/// Walk a `block_mapping` (directly or wrapped in a `block_node`) and collect
/// `(key_node, value_node)` pairs. Empty for non-mapping nodes.
fn mapping_pairs<'tree>(node: Node<'tree>) -> Vec<(Node<'tree>, Node<'tree>)> {
    let target = if node.kind() == "block_mapping" {
        node
    } else if node.kind() == "block_node" {
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

fn key_str<'s>(key_node: Node, source: &'s [u8]) -> &'s str {
    let mut n = key_node;
    while matches!(n.kind(), "flow_node" | "plain_scalar") {
        match n.child(0) {
            Some(c) => n = c,
            None => break,
        }
    }
    node_text(n, source).trim()
}

fn scalar_str<'s>(val_node: Node, source: &'s [u8]) -> Option<&'s str> {
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
    let t = node_text(n, source).trim();
    (!t.is_empty()).then_some(t)
}

/// Iterate the items of a `block_sequence` (YAML list), yielding each item's
/// value node. Used to walk `containers:` / `initContainers:` lists.
fn sequence_value_nodes<'tree>(val_node: Node<'tree>) -> Vec<Node<'tree>> {
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
            if let Some(v) = item.child(1) {
                items.push(v);
            }
        }
    }
    items
}

/// Find the value node of the first top-level mapping key equal to `key`.
fn field<'tree>(map_node: Node<'tree>, key: &str, source: &[u8]) -> Option<Node<'tree>> {
    mapping_pairs(map_node)
        .into_iter()
        .find(|(k, _)| key_str(*k, source) == key)
        .map(|(_, v)| v)
}

// ── container/image extraction ────────────────────────────────────────────────

/// Recursively collect every `image:` ref reachable under a resource `spec`.
///
/// Workload kinds nest the container list at different depths — Pod uses
/// `spec.containers`, Deployment/StatefulSet/DaemonSet/Job use
/// `spec.template.spec.containers`, CronJob adds another `jobTemplate` level.
/// Rather than table each kind's path (brittle, misses new kinds), we walk any
/// mapping for a `containers` / `initContainers` sequence and descend through
/// nested mappings/sequences. The image ref is the load-bearing edge regardless
/// of where the container list sits.
fn collect_images(node: Node, source: &[u8], out: &mut Vec<String>) {
    for (key_node, val_node) in mapping_pairs(node) {
        let key = key_str(key_node, source);
        if matches!(key, "containers" | "initContainers") {
            for container in sequence_value_nodes(val_node) {
                if let Some(img_node) = field(container, "image", source) {
                    if let Some(img) = scalar_str(img_node, source) {
                        out.push(img.to_string());
                    }
                }
            }
            continue;
        }
        // Descend into nested mappings (template, jobTemplate, spec, …) and
        // sequences so deeply-nested container lists are still reached.
        descend(val_node, source, out);
    }
}

fn descend(val_node: Node, source: &[u8], out: &mut Vec<String>) {
    let mut n = val_node;
    if n.kind() == "block_node" {
        n = match n.child(0) {
            Some(c) => c,
            None => return,
        };
    }
    match n.kind() {
        "block_mapping" => collect_images(n, source, out),
        "block_sequence" => {
            for item in sequence_value_nodes(val_node) {
                descend(item, source, out);
            }
        }
        _ => {}
    }
}

// ── document-level extraction ─────────────────────────────────────────────────

/// Extract one k8s resource (`apiVersion`+`kind`+`metadata.name`) from a
/// document's top-level mapping into `nodes` + `imports`.
fn extract_resource(
    doc_map: Node,
    source: &[u8],
    nodes: &mut Vec<RawNode>,
    imports: &mut Vec<RawImport>,
) {
    let kind = field(doc_map, "kind", source)
        .and_then(|n| scalar_str(n, source))
        .unwrap_or("")
        .to_string();

    let metadata = field(doc_map, "metadata", source);
    let name = metadata
        .and_then(|m| field(m, "name", source))
        .and_then(|n| scalar_str(n, source))
        .map(str::to_string);
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return;
    };

    let name_node = metadata
        .and_then(|m| field(m, "name", source))
        .unwrap_or(doc_map);
    let start = name_node.start_position();
    let end = name_node.end_position();

    if let Some(spec) = field(doc_map, "spec", source) {
        let mut images = Vec::new();
        descend(spec, source, &mut images);
        for img in images {
            imports.push(RawImport {
                alias: Some(name.clone()),
                imported_name: img.clone(),
                source: img,
                binding_kind: None,
            });
        }
    }

    nodes.push(RawNode {
        name,
        kind: NodeKind::Class,
        span: (
            start.row as u32,
            start.column as u32,
            end.row as u32,
            end.column as u32,
        ),
        is_exported: true,
        heritage: vec![],
        type_annotation: (!kind.is_empty()).then_some(kind),
        decorators: vec![],
        calls: vec![],
        field_reads: vec![],
        owner_class: None,
        content_hash: 0,
    });
}

impl LanguageProvider for KubernetesProvider {
    fn name(&self) -> &'static str {
        "kubernetes"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        // Content-sniff gate: reject non-k8s YAML before any parse work.
        let probe = &source[..source.len().min(1024)];
        if !has_k8s_markers(probe) {
            return Ok(LocalGraph {
                file_path: path.to_path_buf(),
                ..Default::default()
            });
        }

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_yaml::LANGUAGE.into())
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let tree = parse_with_budget(&mut parser, source, ParseBudget::DEFAULT)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse kubernetes manifest"))?;

        let root = tree.root_node();
        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();

        // Walk EVERY document (`---`-separated); k8s bundles multiple resources
        // per file (Service + Deployment is the canonical pair).
        let mut cur = root.walk();
        for doc in root.children(&mut cur) {
            if doc.kind() != "document" {
                continue;
            }
            let mut dcur = doc.walk();
            for child in doc.children(&mut dcur) {
                if child.kind() == "block_node" {
                    if let Some(m) = child.child(0) {
                        if m.kind() == "block_mapping" {
                            extract_resource(m, source, &mut nodes, &mut imports);
                            break;
                        }
                    }
                }
            }
        }

        Ok(LocalGraph {
            file_path: path.to_path_buf(),
            nodes,
            imports,
            ..Default::default()
        })
    }
}
