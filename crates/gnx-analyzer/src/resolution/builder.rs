use crate::resolution::index::SymbolTable;
use crate::resolution::resolver::Resolver;
use gnx_core::analyzer::types::{LocalGraph, RawNode};
use gnx_core::graph::{Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph};
use gnx_core::pool::StringPool;

fn determine_category(path: &str) -> FileCategory {
    let lower_path = path.to_lowercase().replace('\\', "/");

    let is_test = lower_path.contains(".test.")
        || lower_path.contains(".spec.")
        || lower_path.contains("__tests__/")
        || lower_path.contains("__mocks__/")
        || lower_path.contains("/test/")
        || lower_path.contains("/tests/")
        || lower_path.contains("/testing/")
        || lower_path.contains("/fixtures/")
        || lower_path.ends_with("_test.go")
        || lower_path.ends_with("_test.py")
        || lower_path.ends_with("_spec.rb")
        || lower_path.ends_with("_test.rb")
        || lower_path.contains("/spec/")
        || lower_path.contains("/test_")
        || lower_path.contains("/conftest.");

    if is_test {
        return FileCategory::Test;
    }

    if lower_path.ends_with(".md") || lower_path.ends_with(".txt") || lower_path.ends_with(".rst") {
        return FileCategory::Document;
    }
    if lower_path.ends_with(".json")
        || lower_path.ends_with(".toml")
        || lower_path.ends_with(".yaml")
        || lower_path.ends_with(".yml")
        || lower_path.ends_with("dockerfile")
    {
        return FileCategory::Config;
    }
    FileCategory::Source
}

/// NodeKinds whose identifier names carry too little semantic signal to be
/// worth the BGE-M3 inference cost. Skipped nodes still occupy a slot in the
/// `embeddings` vector (zero-vec) so that `embeddings[i] ↔ nodes[i]` alignment
/// is preserved for downstream query code.
fn should_embed(kind: NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::Variable | NodeKind::Const | NodeKind::Import
    )
}

/// Build the per-node text fed to the embedding model. Combines structural
/// signals (kind, name, path, export flag, decorators, type annotation,
/// heritage) so that semantic search hits patterns like "express route" or
/// "scheduled job" via the decorator strings.
fn build_embed_text(raw_node: &RawNode, path_str: &str) -> String {
    let mut parts = vec![
        format!("{:?}: {}", raw_node.kind, raw_node.name),
        format!("Path: {}", path_str),
    ];
    if raw_node.is_exported {
        parts.push("Export: true".to_string());
    }
    if !raw_node.decorators.is_empty() {
        parts.push(raw_node.decorators.join(" "));
    }
    if let Some(ty) = &raw_node.type_annotation {
        parts.push(format!("Type: {}", ty));
    }
    if !raw_node.heritage.is_empty() {
        parts.push(format!("Heritage: {}", raw_node.heritage.join(", ")));
    }
    parts.join("\n")
}

use std::collections::HashMap;

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}

fn sanitize_id(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(20)
        .collect()
}

pub struct GraphBuilder {
    local_graphs: Vec<LocalGraph>,
    generate_embeddings: bool,
    old_file_hashes: HashMap<String, [u8; 32]>,
    old_embeddings_cache: HashMap<String, Vec<f32>>,
}

impl Default for GraphBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self {
            local_graphs: Vec::new(),
            generate_embeddings: false,
            old_file_hashes: HashMap::new(),
            old_embeddings_cache: HashMap::new(),
        }
    }

    pub fn with_embeddings(mut self, generate: bool) -> Self {
        self.generate_embeddings = generate;
        self
    }

    pub fn with_cache(
        mut self,
        hashes: HashMap<String, [u8; 32]>,
        embs: HashMap<String, Vec<f32>>,
    ) -> Self {
        self.old_file_hashes = hashes;
        self.old_embeddings_cache = embs;
        self
    }

    pub fn add_graph(&mut self, graph: LocalGraph) {
        self.local_graphs.push(graph);
    }

    pub fn build(self) -> ZeroCopyGraph {
        let mut symbol_table = SymbolTable::new();
        let mut string_pool = StringPool::new();
        let mut nodes = Vec::new();
        let mut files = Vec::new();

        // Pass 1: Register all nodes into SymbolTable and StringPool
        let mut current_node_idx = 0;
        let mut embed_texts = Vec::new();

        let mut final_embeddings: Vec<Option<Vec<f32>>> = Vec::new();

        for (file_idx, local_graph) in self.local_graphs.iter().enumerate() {
            let file_idx = file_idx as u32;
            let path_str = local_graph.file_path.to_string_lossy().to_string();
            let path_ref = string_pool.add(&path_str);

            let file_unchanged =
                self.old_file_hashes.get(&path_str) == Some(&local_graph.content_hash);

            files.push(File {
                path: path_ref,
                mtime: 0, // In a real implementation, fetch actual mtime
                content_hash: local_graph.content_hash,
                category: determine_category(&path_str),
            });

            for raw_node in &local_graph.nodes {
                symbol_table.register_node(&path_str, &raw_node.name, current_node_idx);

                let uid_str = format!("{:?}:{}:{}", raw_node.kind, path_str, raw_node.name);
                let uid_ref = string_pool.add(&uid_str);
                let name_ref = string_pool.add(&raw_node.name);

                nodes.push(Node {
                    uid: uid_ref,
                    name: name_ref,
                    file_idx,
                    kind: raw_node.kind,
                    span: raw_node.span,
                    community_id: 0,
                });

                if self.generate_embeddings {
                    let mut reused = false;
                    if file_unchanged {
                        if let Some(old_emb) = self.old_embeddings_cache.get(&uid_str) {
                            final_embeddings.push(Some(old_emb.clone()));
                            reused = true;
                        }
                    }

                    if !reused {
                        final_embeddings.push(None); // Will be filled later
                        let text = if should_embed(raw_node.kind) {
                            build_embed_text(raw_node, &path_str)
                        } else {
                            String::new()
                        };
                        embed_texts.push((current_node_idx, text));
                    }
                }

                current_node_idx += 1;
            }

            // NOTE: documents (markdown/yaml section/doc nodes) are parsed into
            // `local_graph.documents` but the graph.bin DocumentBlock storage is
            // not wired up yet. Skipped here intentionally — re-enable when the
            // `DocumentBlock` type lands in `gnx_core::graph`.
        }

        // Pass 1.5: Extract Routes
        let mut route_edges = Vec::new();
        let mut current_handler_idx = 0;
        for (file_idx, local_graph) in self.local_graphs.iter().enumerate() {
            let file_idx = file_idx as u32;
            let path_str = local_graph.file_path.to_string_lossy().to_string();

            for raw_node in &local_graph.nodes {
                let handler_idx = current_handler_idx;

                for dec in &raw_node.decorators {
                    if let Some(detected) = crate::route_detector::detect_from_decorator(dec) {
                        let route_name = format!("{} {}", detected.method, detected.path);
                        let uid_str = format!("Route:{}:{}", path_str, route_name);

                        let route_idx = nodes.len() as u32;
                        nodes.push(Node {
                            uid: string_pool.add(&uid_str),
                            name: string_pool.add(&route_name),
                            file_idx,
                            kind: gnx_core::graph::NodeKind::Route,
                            span: raw_node.span,
                            community_id: 0,
                        });

                        if self.generate_embeddings {
                            let mut reused = false;
                            let file_unchanged = self.old_file_hashes.get(&path_str)
                                == Some(&local_graph.content_hash);
                            if file_unchanged {
                                if let Some(old_emb) = self.old_embeddings_cache.get(&uid_str) {
                                    final_embeddings.push(Some(old_emb.clone()));
                                    reused = true;
                                }
                            }
                            if !reused {
                                final_embeddings.push(None);
                                embed_texts.push((
                                    route_idx,
                                    format!("Route: {}\nPath: {}", route_name, path_str),
                                ));
                            }
                        }

                        route_edges.push(Edge {
                            source: handler_idx,
                            target: route_idx,
                            rel_type: RelType::HandlesRoute,
                            confidence: 1.0,
                            reason: string_pool.add("decorator"),
                        });
                    }
                }
                current_handler_idx += 1;
            }

            for raw_route in &local_graph.routes {
                if let Some(detected) = crate::route_detector::detect_from_call(raw_route) {
                    let route_name = format!("{} {}", detected.method, detected.path);
                    let uid_str = format!("Route:{}:{}", path_str, route_name);

                    let route_idx = nodes.len() as u32;
                    nodes.push(Node {
                        uid: string_pool.add(&uid_str),
                        name: string_pool.add(&route_name),
                        file_idx,
                        kind: gnx_core::graph::NodeKind::Route,
                        span: raw_route.span,
                        community_id: 0,
                    });

                    if self.generate_embeddings {
                        let mut reused = false;
                        let file_unchanged =
                            self.old_file_hashes.get(&path_str) == Some(&local_graph.content_hash);
                        if file_unchanged {
                            if let Some(old_emb) = self.old_embeddings_cache.get(&uid_str) {
                                final_embeddings.push(Some(old_emb.clone()));
                                reused = true;
                            }
                        }
                        if !reused {
                            final_embeddings.push(None);
                            embed_texts.push((
                                route_idx,
                                format!("Route: {}\nPath: {}", route_name, path_str),
                            ));
                        }
                    }

                    // Imperative routes might not easily map to a specific handler.
                    // We just register the Route node here.
                }
            }
        }

        // Pass 2: Resolve imports and build edges
        let resolver = Resolver::new(&symbol_table);
        let mut edges = Vec::new();
        let mut current_node_idx = 0;

        let reason_heritage = string_pool.add("heritage");
        let reason_type = string_pool.add("type_annotation");
        let reason_call = string_pool.add("call");

        for local_graph in &self.local_graphs {
            for raw_node in &local_graph.nodes {
                // Resolve heritage (base classes, traits) — emit Extends edge.
                for base in &raw_node.heritage {
                    let targets =
                        resolver.resolve_symbol(&local_graph.file_path, base, &local_graph.imports);
                    for (target_id, confidence) in targets {
                        edges.push(Edge {
                            source: current_node_idx,
                            target: target_id,
                            rel_type: RelType::Extends,
                            confidence,
                            reason: reason_heritage.clone(),
                        });
                    }
                }

                // Resolve calls (function invocations from this node's body).
                for callee in &raw_node.calls {
                    let targets = resolver.resolve_symbol(
                        &local_graph.file_path,
                        callee,
                        &local_graph.imports,
                    );
                    for (target_id, confidence) in targets {
                        if target_id == current_node_idx {
                            continue; // skip self-recursion edges (Louvain/process noise)
                        }
                        edges.push(Edge {
                            source: current_node_idx,
                            target: target_id,
                            rel_type: RelType::Calls,
                            confidence,
                            reason: reason_call.clone(),
                        });
                    }
                }

                // Resolve type annotation
                if let Some(type_ann) = &raw_node.type_annotation {
                    let targets = resolver.resolve_symbol(
                        &local_graph.file_path,
                        type_ann,
                        &local_graph.imports,
                    );
                    for (target_id, confidence) in targets {
                        edges.push(Edge {
                            source: current_node_idx,
                            target: target_id,
                            rel_type: RelType::Accesses,
                            confidence,
                            reason: reason_type.clone(),
                        });
                    }
                }

                current_node_idx += 1;
            }

            // Resolve framework refs (confidence-weighted edges with custom reasons)
            for fw_ref in &local_graph.framework_refs {
                // Resolve source node in current file
                let source_id = symbol_table.lookup_in_file(
                    &local_graph.file_path.to_string_lossy(),
                    &fw_ref.source_name,
                );

                if let Some(source_id) = source_id {
                    // Resolve target: same-file → import-scoped → global
                    let targets = resolver.resolve_symbol(
                        &local_graph.file_path,
                        &fw_ref.target_name,
                        &local_graph.imports,
                    );

                    for (target_id, _) in targets {
                        let reason_ref = string_pool.add(&fw_ref.reason);
                        edges.push(Edge {
                            source: source_id,
                            target: target_id,
                            rel_type: RelType::References,
                            confidence: fw_ref.confidence,
                            reason: reason_ref,
                        });
                    }
                }
            }
        }

        edges.extend(route_edges);

        // Pass 3: Community detection (Leiden) over Calls/Extends/Implements edges.
        // Leiden's refinement phase prevents the badly-connected-hub failure
        // mode where Louvain pins a hub to its first-touched chain.
        // Writes community_id back onto each Node in place.
        let assignments = gnx_core::algorithms::leiden::detect_communities(
            &nodes,
            &edges,
            &gnx_core::algorithms::leiden::LeidenConfig::default(),
        );
        for (node, &c) in nodes.iter_mut().zip(assignments.iter()) {
            node.community_id = c;
        }

        // Pass 4: Process detection (BFS forward via CALLS).
        // Produces traces; each trace becomes a NodeKind::Process node + N
        // StepInProcess edges. Process nodes are appended to `nodes` so they
        // sit at the tail — `process_start` marks the boundary.
        let file_paths: Vec<String> = files
            .iter()
            .map(|f| {
                let start = f.path.offset as usize;
                let end = start + f.path.len as usize;
                std::str::from_utf8(&string_pool.bytes[start..end])
                    .unwrap_or("")
                    .to_string()
            })
            .collect();

        let traces = gnx_core::algorithms::process_trace::detect_processes(
            &nodes,
            &edges,
            &file_paths,
            &gnx_core::algorithms::process_trace::ProcessConfig::default(),
        );

        let process_start_idx = nodes.len() as u32;
        let mut traces_offsets: Vec<u32> = Vec::with_capacity(traces.len() + 1);
        let mut traces_data: Vec<u32> = Vec::new();
        traces_offsets.push(0);

        for (k, tr) in traces.iter().enumerate() {
            let entry_idx = tr.trace.first().copied().unwrap_or(0);
            let terminal_idx = tr.trace.last().copied().unwrap_or(0);
            let entry_name = nodes
                .get(entry_idx as usize)
                .map(|n| {
                    std::str::from_utf8(
                        &string_pool.bytes
                            [n.name.offset as usize..n.name.offset as usize + n.name.len as usize],
                    )
                    .unwrap_or("")
                    .to_string()
                })
                .unwrap_or_default();
            let terminal_name = nodes
                .get(terminal_idx as usize)
                .map(|n| {
                    std::str::from_utf8(
                        &string_pool.bytes
                            [n.name.offset as usize..n.name.offset as usize + n.name.len as usize],
                    )
                    .unwrap_or("")
                    .to_string()
                })
                .unwrap_or_default();

            let label = format!(
                "{} → {}",
                capitalize(&entry_name),
                capitalize(&terminal_name)
            );
            let uid_str = format!(
                "proc_{}_{}_{}",
                k,
                sanitize_id(&entry_name),
                sanitize_id(&terminal_name)
            );

            let process_node_idx = nodes.len() as u32;
            let process_node_community = nodes
                .get(entry_idx as usize)
                .map(|n| n.community_id)
                .unwrap_or(0);

            nodes.push(Node {
                uid: string_pool.add(&uid_str),
                name: string_pool.add(&label),
                file_idx: nodes
                    .get(entry_idx as usize)
                    .map(|n| n.file_idx)
                    .unwrap_or(0),
                kind: NodeKind::Process,
                span: nodes
                    .get(entry_idx as usize)
                    .map(|n| n.span)
                    .unwrap_or((0, 0, 0, 0)),
                community_id: process_node_community,
            });

            for (step_idx, &member_idx) in tr.trace.iter().enumerate() {
                let reason_str = format!("step:{}", step_idx + 1);
                edges.push(Edge {
                    source: member_idx,
                    target: process_node_idx,
                    rel_type: RelType::StepInProcess,
                    confidence: 1.0,
                    reason: string_pool.add(&reason_str),
                });
                traces_data.push(member_idx);
            }
            traces_offsets.push(traces_data.len() as u32);
        }

        // Final pass: Construct CSR (out_offsets and in_offsets)
        // Sort edges by source to build out_offsets easily
        edges.sort_by_key(|e| e.source);

        let num_nodes = nodes.len();
        let mut out_offsets = vec![0; num_nodes + 1];
        for edge in &edges {
            out_offsets[edge.source as usize + 1] += 1;
        }
        for i in 0..num_nodes {
            out_offsets[i + 1] += out_offsets[i];
        }

        // Build in_edge_idx (indices of edges sorted by target)
        let mut in_edge_idx: Vec<u32> = (0..edges.len() as u32).collect();
        in_edge_idx.sort_by_key(|&idx| edges[idx as usize].target);

        let mut in_offsets = vec![0; num_nodes + 1];
        for &idx in &in_edge_idx {
            let edge = &edges[idx as usize];
            in_offsets[edge.target as usize + 1] += 1;
        }
        for i in 0..num_nodes {
            in_offsets[i + 1] += in_offsets[i];
        }

        let embeddings = if self.generate_embeddings {
            if !embed_texts.is_empty() {
                tracing::info!(
                    "Generating embeddings for {} nodes ({} reused)...",
                    embed_texts.len(),
                    final_embeddings.len() - embed_texts.len()
                );
                match crate::embeddings::Embedder::new() {
                    Ok(embedder) => {
                        let texts: Vec<String> = embed_texts.into_iter().map(|(_, t)| t).collect();
                        if let Ok(new_embs) = embedder.embed(texts) {
                            // Find all None in final_embeddings and fill them
                            let mut new_embs_iter = new_embs.into_iter();
                            for emb in final_embeddings.iter_mut() {
                                if emb.is_none() {
                                    if let Some(new_emb) = new_embs_iter.next() {
                                        *emb = Some(new_emb);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!("Failed to initialize embedder: {}", e),
                }
            } else {
                tracing::info!(
                    "Reused all {} embeddings from cache.",
                    final_embeddings.len()
                );
            }

            let mut final_embs_unwrapped = Vec::with_capacity(final_embeddings.len());
            let mut all_some = true;
            for emb in final_embeddings {
                if let Some(e) = emb {
                    final_embs_unwrapped.push(e);
                } else {
                    final_embs_unwrapped.push(vec![0.0; 1024]);
                    all_some = false;
                }
            }
            if !all_some {
                tracing::warn!("Some embeddings failed to generate, filled with zeros.");
            }
            Some(final_embs_unwrapped)
        } else {
            None
        };

        ZeroCopyGraph {
            magic: *b"GNX-RS\0\0",
            fingerprint: [0; 32],
            string_pool: string_pool.bytes,
            nodes,
            edges,
            out_offsets,
            in_offsets,
            in_edge_idx,
            name_index: Vec::new(), // To be implemented if name indexing is needed
            embeddings,
            process_start: process_start_idx,
            traces_offsets,
            traces_data,
            files,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gnx_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawNode};
    use gnx_core::graph::NodeKind;

    #[test]
    fn framework_ref_produces_edge_with_confidence_and_reason() {
        let g = LocalGraph {
            file_path: "test.py".into(),
            content_hash: [0; 32],
            nodes: vec![
                RawNode {
                    name: "handler".into(),
                    kind: NodeKind::Function,
                    span: (0, 0, 0, 0),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                },
                RawNode {
                    name: "get_db".into(),
                    kind: NodeKind::Function,
                    span: (0, 0, 0, 0),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                },
            ],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![RawFrameworkRef {
                source_name: "handler".into(),
                target_name: "get_db".into(),
                confidence: 0.6,
                reason: "fastapi-depends".into(),
                span: (0, 0, 0, 0),
            }],
        };

        let mut builder = GraphBuilder::new();
        builder.add_graph(g);
        let graph = builder.build();

        let fw_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::References)
            .collect();
        assert_eq!(
            fw_edges.len(),
            1,
            "expected 1 References edge, got {}",
            fw_edges.len()
        );
        assert!((fw_edges[0].confidence - 0.6).abs() < 1e-6);
    }
}
