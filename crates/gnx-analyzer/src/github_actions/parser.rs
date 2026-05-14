use gnx_core::analyzer::provider::LanguageProvider;
use gnx_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use gnx_core::graph::NodeKind;
use std::path::Path;
use tree_sitter::{Node, Parser, Query};

pub struct GitHubActionsProvider {
    query: Query,
}

impl GitHubActionsProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_yaml::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)
            .map_err(|e| anyhow::anyhow!("Failed to create GHA query: {}", e))?;
        Ok(Self { query })
    }
}

/// Extract text content from a `flow_node > plain_scalar > string_scalar` subtree.
fn node_text<'a>(node: Node<'a>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).ok()
}

/// Return the scalar text if `node` is a `flow_node` wrapping a plain scalar.
fn scalar_text<'a>(node: Node<'a>, source: &'a [u8]) -> Option<&'a str> {
    if node.kind() == "flow_node" {
        let child = node.named_child(0)?;
        if child.kind() == "plain_scalar" {
            let inner = child.named_child(0)?;
            return node_text(inner, source);
        }
    }
    // Also handle double/single-quoted scalars directly under flow_node
    if node.kind() == "flow_node" {
        let child = node.named_child(0)?;
        if child.kind() == "double_quote_scalar" || child.kind() == "single_quote_scalar" {
            return node_text(child, source).map(|s| s.trim_matches(|c| c == '"' || c == '\''));
        }
        // block scalar strings used in run: |
        if child.kind() == "string_scalar" {
            return node_text(child, source);
        }
    }
    None
}

/// Given a `block_mapping` node, iterate key-value pairs and return the value
/// node for the given key name, if found.
fn mapping_value<'a>(mapping: Node<'a>, key_name: &str, source: &'a [u8]) -> Option<Node<'a>> {
    let mut cursor = mapping.walk();
    for child in mapping.named_children(&mut cursor) {
        if child.kind() != "block_mapping_pair" {
            continue;
        }
        let key_node = child.child_by_field_name("key")?;
        if let Some(k) = scalar_text(key_node, source) {
            if k == key_name {
                return child.child_by_field_name("value");
            }
        }
    }
    None
}

/// Collect all key names in a block_mapping as (key_text, value_node) pairs.
fn mapping_pairs<'a>(mapping: Node<'a>, source: &'a [u8]) -> Vec<(&'a str, Node<'a>)> {
    let mut pairs = Vec::new();
    let mut cursor = mapping.walk();
    for child in mapping.named_children(&mut cursor) {
        if child.kind() != "block_mapping_pair" {
            continue;
        }
        let Some(key_node) = child.child_by_field_name("key") else { continue };
        let Some(val_node) = child.child_by_field_name("value") else { continue };
        if let Some(k) = scalar_text(key_node, source) {
            pairs.push((k, val_node));
        }
    }
    pairs
}

/// Descend into a value node to get the first `block_mapping` inside it.
fn unwrap_block_mapping(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() == "block_node" {
        let child = node.named_child(0)?;
        if child.kind() == "block_mapping" {
            return Some(child);
        }
    }
    if node.kind() == "block_mapping" {
        return Some(node);
    }
    None
}

/// Collect string scalars from a flow_sequence or block_sequence value node.
fn collect_sequence_scalars<'a>(node: Node<'a>, source: &'a [u8]) -> Vec<&'a str> {
    let mut out = Vec::new();
    // flow_sequence: (flow_node...)
    if node.kind() == "flow_node" {
        // single-item sequence wrapped in flow_node — check for flow_sequence
        if let Some(seq) = node.named_child(0) {
            if seq.kind() == "flow_sequence" {
                let mut cur = seq.walk();
                for item in seq.named_children(&mut cur) {
                    if let Some(t) = scalar_text(item, source) {
                        out.push(t);
                    }
                }
                return out;
            }
        }
        // single scalar
        if let Some(t) = scalar_text(node, source) {
            out.push(t);
        }
        return out;
    }
    if node.kind() == "block_node" {
        if let Some(seq) = node.named_child(0) {
            if seq.kind() == "block_sequence" {
                let mut cur = seq.walk();
                for item in seq.named_children(&mut cur) {
                    // block_sequence_item > block_node > plain_scalar or flow_node
                    if item.kind() == "block_sequence_item" {
                        if let Some(inner) = item.named_child(0) {
                            if let Some(t) = scalar_text(inner, source) {
                                out.push(t);
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// Extract the action name portion from `uses: owner/action@ref` — strips `@ref`.
fn action_name(uses_value: &str) -> String {
    match uses_value.split_once('@') {
        Some((name, _)) => name.to_string(),
        None => uses_value.to_string(),
    }
}

/// Parse the first word of a `run:` command as the "command name".
fn run_command_name(run_value: &str) -> Option<String> {
    let first_word = run_value.trim().split_whitespace().next()?;
    if first_word.is_empty() { None } else { Some(first_word.to_string()) }
}

impl LanguageProvider for GitHubActionsProvider {
    fn name(&self) -> &'static str {
        "github-actions"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let language = tree_sitter_yaml::LANGUAGE.into();
        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse GitHub Actions YAML"))?;

        let root = tree.root_node();
        let mut nodes: Vec<RawNode> = Vec::new();
        let mut imports: Vec<RawImport> = Vec::new();

        // Root is `stream > document > block_node > block_mapping`
        let doc = match root.named_child(0) {
            Some(d) => d,
            None => {
                return Ok(LocalGraph {
                    content_hash: [0; 32],
                    routes: vec![],
                    file_path: path.to_path_buf(),
                    nodes,
                    documents: vec![],
                    imports,
                });
            }
        };

        let top_mapping = doc
            .named_child(0)
            .and_then(|bn| if bn.kind() == "block_node" { bn.named_child(0) } else { Some(bn) })
            .filter(|n| n.kind() == "block_mapping");

        let Some(top_mapping) = top_mapping else {
            return Ok(LocalGraph {
                content_hash: [0; 32],
                routes: vec![],
                file_path: path.to_path_buf(),
                nodes,
                documents: vec![],
                imports,
            });
        };

        // Extract workflow `name:` → top-level Class node
        if let Some(name_val) = mapping_value(top_mapping, "name", source) {
            if let Some(wf_name) = scalar_text(name_val, source) {
                let start = top_mapping.start_position();
                let end = top_mapping.end_position();
                nodes.push(RawNode {
                    name: wf_name.to_string(),
                    kind: NodeKind::Class,
                    span: (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    ),
                    is_exported: true,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec!["workflow".to_string()],
                    calls: vec![],
                });
            }
        }

        // Extract jobs map
        let jobs_block = mapping_value(top_mapping, "jobs", source)
            .and_then(unwrap_block_mapping);

        if let Some(jobs_mapping) = jobs_block {
            for (job_key, job_val_node) in mapping_pairs(jobs_mapping, source) {
                let job_val_mapping = unwrap_block_mapping(job_val_node);
                let start = job_val_node.start_position();
                let end = job_val_node.end_position();

                // Collect `needs:` as call dependencies
                let mut job_calls: Vec<String> = Vec::new();
                if let Some(jm) = job_val_mapping {
                    if let Some(needs_val) = mapping_value(jm, "needs", source) {
                        for dep in collect_sequence_scalars(needs_val, source) {
                            job_calls.push(dep.to_string());
                        }
                    }
                }

                // Each job → Class node
                nodes.push(RawNode {
                    name: job_key.to_string(),
                    kind: NodeKind::Class,
                    span: (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    ),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec!["job".to_string()],
                    calls: job_calls,
                });

                // Walk steps of this job
                if let Some(jm) = job_val_mapping {
                    if let Some(steps_val) = mapping_value(jm, "steps", source) {
                        walk_steps(steps_val, source, job_key, &mut nodes, &mut imports);
                    }
                }
            }
        }

        Ok(LocalGraph {
            content_hash: [0; 32],
            routes: vec![],
            file_path: path.to_path_buf(),
            nodes,
            documents: vec![],
            imports,
        })
    }
}

fn walk_steps(
    steps_node: Node<'_>,
    source: &[u8],
    job_key: &str,
    nodes: &mut Vec<RawNode>,
    imports: &mut Vec<RawImport>,
) {
    // steps_node is a block_node containing a block_sequence
    let seq = match steps_node.kind() {
        "block_node" => steps_node
            .named_child(0)
            .filter(|n| n.kind() == "block_sequence"),
        "block_sequence" => Some(steps_node),
        _ => None,
    };

    let Some(seq) = seq else { return };
    let mut step_idx = 0u32;
    let mut cur = seq.walk();

    for item in seq.named_children(&mut cur) {
        if item.kind() != "block_sequence_item" {
            continue;
        }
        let Some(step_block) = item.named_child(0) else { continue };
        let Some(step_mapping) = unwrap_block_mapping(step_block) else { continue };

        let pairs = mapping_pairs(step_mapping, source);
        let start = item.start_position();
        let end = item.end_position();

        let mut step_name: Option<String> = None;
        let mut uses_val: Option<&str> = None;
        let mut run_val: Option<&str> = None;

        for (k, v) in &pairs {
            match *k {
                "name" => {
                    step_name = scalar_text(*v, source).map(|s| s.to_string());
                }
                "uses" => {
                    uses_val = scalar_text(*v, source);
                }
                "run" => {
                    run_val = node_text(*v, source);
                }
                _ => {}
            }
        }

        // `uses:` → RawImport (action dependency)
        if let Some(uses) = uses_val {
            let action = action_name(uses);
            imports.push(RawImport {
                source: action.clone(),
                imported_name: action,
                alias: None,
            });
        }

        // `run:` → Function node named by first command word
        if let Some(run) = run_val {
            if let Some(cmd_name) = run_command_name(run) {
                let func_name = match &step_name {
                    Some(n) => format!("{}/{}", job_key, n),
                    None => format!("{}/step{}", job_key, step_idx),
                };
                nodes.push(RawNode {
                    name: func_name,
                    kind: NodeKind::Function,
                    span: (
                        start.row as u32,
                        start.column as u32,
                        end.row as u32,
                        end.column as u32,
                    ),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![format!("run:{}", cmd_name)],
                    calls: vec![],
                });
            }
        }

        step_idx += 1;
    }
}
