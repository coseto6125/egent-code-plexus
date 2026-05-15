use crate::resolution::index::{ResolveTarget, SymbolTable};
use crate::resolution::path_aliases::PathAliases;
use crate::resolution::resolver::Resolver;
use graph_nexus_core::analyzer::types::{LocalGraph, RawNode};
use graph_nexus_core::graph::{
    BlindSpotRecord, Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph,
};
use graph_nexus_core::pool::{StrRef, StringPool};
use rayon::prelude::*;
use rustc_hash::FxHashMap;

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
    /// When `Some`, the resolver pass 2 buffers every decision and writes a
    /// JSONL line per resolution attempt to this path. Used by the oracle
    /// verification harness (see specs/2026-05-15-resolver-oracle-harness.md).
    resolver_dump_path: Option<std::path::PathBuf>,
    /// Module-specifier aliases (TS `tsconfig.json` `compilerOptions.paths`,
    /// etc.) — forwarded to the resolver before Pass 2 starts.
    path_aliases: PathAliases,
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
            resolver_dump_path: None,
            path_aliases: PathAliases::new(),
        }
    }

    pub fn with_path_aliases(mut self, aliases: PathAliases) -> Self {
        self.path_aliases = aliases;
        self
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

    pub fn with_resolver_dump(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.resolver_dump_path = path;
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
            // Path → string 一律走 forward-slash，讓 UID / lookup / 顯示在 Windows
            // 上與 Linux/macOS 一致（與 resolver.rs / registry/path.rs 既有 idiom 對齊）。
            let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
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
                symbol_table.register_node(
                    &path_str,
                    &raw_node.name,
                    current_node_idx,
                    raw_node.kind,
                );

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
            // `DocumentBlock` type lands in `graph_nexus_core::graph`.
        }

        // Pass 1.5: Extract Routes
        let mut route_edges = Vec::new();
        let mut current_handler_idx = 0;
        for (file_idx, local_graph) in self.local_graphs.iter().enumerate() {
            let file_idx = file_idx as u32;
            let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");

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
                            kind: graph_nexus_core::graph::NodeKind::Route,
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
                        kind: graph_nexus_core::graph::NodeKind::Route,
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

                    // Resolve the imperative-route handler, if the parser captured
                    // a named handler (e.g. `app.get("/x", loginHandler)`). The
                    // handler must be a function/method registered in the same
                    // file; inline arrow functions are not captured.
                    if let Some(handler_name) = raw_route.handler.as_deref() {
                        if let Some(handler_node_id) =
                            symbol_table.lookup_in_file(&path_str, handler_name)
                        {
                            route_edges.push(Edge {
                                source: handler_node_id,
                                target: route_idx,
                                rel_type: RelType::HandlesRoute,
                                confidence: 1.0,
                                reason: string_pool.add("call-arg"),
                            });
                        }
                    }
                }
            }
        }

        // Pass 1.7: Entry-point scoring (cross-language).
        //
        // Pure consumer of `RawRoute` + `RawFrameworkRef` + `main()`
        // detection — see `crate::entry_points` for the scoring matrix.
        // Closes the ⚠️ Entry column for Java / Kotlin / C# / Go / Rust /
        // Swift / C / C++ / Dart in the README Language Matrix.
        //
        // Emits one `NodeKind::EntryPoint` marker node per scored entry
        // point and a `References` edge from the marker to the underlying
        // handler (looked up by name in the same file's SymbolTable). The
        // edge's `reason` carries the scoring provenance so downstream
        // LLM tooling can render "this is an HTTP route handler at
        // confidence 1.0" without re-running the scorer.
        let mut entry_edges: Vec<Edge> = Vec::new();
        for (file_idx, local_graph) in self.local_graphs.iter().enumerate() {
            let file_idx = file_idx as u32;
            let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
            let entries = crate::entry_points::score_entry_points(
                &local_graph.routes,
                &local_graph.framework_refs,
                &local_graph.nodes,
            );
            for ep in entries {
                let handler_idx = symbol_table.lookup_in_file(&path_str, &ep.uid);
                let Some(handler_idx) = handler_idx else {
                    // Handler not found in this file — happens when a
                    // RawRoute's handler name is a string literal that
                    // doesn't match any parsed symbol (e.g. an external
                    // reference). Skip silently; the EntryPoint without
                    // a target would be a dangling marker.
                    continue;
                };
                let entry_uid = format!("EntryPoint:{}:{}:{}", path_str, ep.kind.tag(), ep.uid);
                let entry_name = format!("{}@{}", ep.kind.tag(), ep.uid);
                let entry_idx = nodes.len() as u32;
                nodes.push(Node {
                    uid: string_pool.add(&entry_uid),
                    name: string_pool.add(&entry_name),
                    file_idx,
                    kind: NodeKind::EntryPoint,
                    span: (0, 0, 0, 0),
                    community_id: 0,
                });

                if self.generate_embeddings {
                    // EntryPoint markers are synthetic; skip embedding
                    // and preserve the embeddings[i] ↔ nodes[i] alignment
                    // by pushing a sentinel zero-vec slot. `should_embed`
                    // already handles structurally-noisy kinds the same
                    // way (Variable/Const/Import).
                    final_embeddings.push(Some(vec![0.0; 1024]));
                }

                // Encode score in the edge reason: "{tag}:{score}:{reason}".
                // Downstream parsing is trivial (split on first ':') and
                // the reason text is preserved as-is for LLM rendering.
                let edge_reason = format!("{}:{:.2}:{}", ep.kind.tag(), ep.score, ep.reason);
                entry_edges.push(Edge {
                    source: entry_idx,
                    target: handler_idx,
                    rel_type: RelType::References,
                    confidence: ep.score,
                    reason: string_pool.add(&edge_reason),
                });
            }
        }

        // Pass 2: Resolve imports and build edges
        //
        // Pass 2 strategy: dump-disabled path (production hot path) runs in
        // parallel over `local_graphs` via rayon. Dump-enabled path (oracle
        // harness, off by default) falls back to serial because
        // `Resolver.decisions` is `RefCell<Vec<_>>` and not `Sync`.
        //
        // To enable parallelism we pre-compute two artifacts serially before
        // the par_iter so the inner closure only needs read-only access to
        // the resolver + symbol_table:
        //   1. `start_indices[graph_idx]` — base `current_node_idx` for each
        //      `local_graph` (prefix-sum of node counts). Replaces the
        //      `current_node_idx += 1` accumulator that previously coupled
        //      graphs sequentially.
        //   2. `reason_cache` — every unique `framework_refs.reason` /
        //      `fanout_refs.reason` interned into `string_pool` up front.
        //      `string_pool.add` is `&mut self` so the inner loop can't
        //      touch it; pre-interning + lookup-by-cache is `&StrRef`-only.

        let mut start_indices: Vec<u32> = Vec::with_capacity(self.local_graphs.len());
        {
            // Precompute as u64 so we detect overflow before the lossy cast
            // would corrupt indices. Hitting this means >4.29B total RawNodes
            // — not currently observed in any real repo, but a single int
            // truncation would silently misalign every downstream index.
            let total: u64 = self
                .local_graphs
                .iter()
                .map(|lg| lg.nodes.len() as u64)
                .sum();
            assert!(
                total <= u32::MAX as u64,
                "total RawNode count {} exceeds u32::MAX — graph node ID scheme would overflow",
                total
            );
            let mut acc: u32 = 0;
            for lg in &self.local_graphs {
                start_indices.push(acc);
                acc += lg.nodes.len() as u32;
            }
        }

        let reason_heritage = string_pool.add("heritage");
        let reason_type = string_pool.add("type_annotation");
        let reason_call = string_pool.add("call");

        let mut reason_cache: FxHashMap<String, StrRef> = FxHashMap::default();
        for lg in &self.local_graphs {
            for fw_ref in &lg.framework_refs {
                reason_cache
                    .entry(fw_ref.reason.clone())
                    .or_insert_with(|| string_pool.add(&fw_ref.reason));
            }
            for fanout_ref in &lg.fanout_refs {
                reason_cache
                    .entry(fanout_ref.reason.clone())
                    .or_insert_with(|| string_pool.add(&fanout_ref.reason));
            }
        }

        let dump_enabled = self.resolver_dump_path.is_some();
        let path_aliases = self.path_aliases.clone();

        // Resolver carries decisions: RefCell<...> which is `!Sync`, so when
        // dumping is enabled we run the serial path. When disabled (the
        // production case) we create a fresh `Resolver` *inside* each
        // par_iter worker so each thread owns its own state.
        let mut resolver_for_dump = if dump_enabled {
            let mut r = Resolver::new(&symbol_table).with_path_aliases(path_aliases.clone());
            r.enable_dump();
            Some(r)
        } else {
            None
        };

        let local_graphs = &self.local_graphs;
        let symbol_table_ref = &symbol_table;
        let reason_cache_ref = &reason_cache;

        let edges: Vec<Edge> = if let Some(resolver) = resolver_for_dump.as_mut() {
            // Serial dump path — original loop, with reason lookups going
            // through `reason_cache` (filled above) instead of inline
            // `string_pool.add`.
            let mut edges = Vec::new();
            let mut current_node_idx = 0u32;
            for local_graph in local_graphs {
                for raw_node in &local_graph.nodes {
                    pass2_emit_node_edges(
                        resolver,
                        local_graph,
                        raw_node,
                        current_node_idx,
                        reason_heritage,
                        reason_type,
                        reason_call,
                        &mut edges,
                    );
                    current_node_idx += 1;
                }
                pass2_emit_framework_and_fanout(
                    resolver,
                    symbol_table_ref,
                    local_graph,
                    reason_cache_ref,
                    &mut edges,
                );
            }
            edges
        } else {
            // Parallel path. Each rayon worker drives a `flat_map` chunk;
            // we pay one Resolver construction per local_graph (cheap —
            // borrows symbol_table, clones path_aliases). For ~14k files
            // on .sample_repo that's ~14k path_aliases.clone() calls
            // totalling a few ms — far below the parallelism gain.
            local_graphs
                .par_iter()
                .enumerate()
                .flat_map_iter(|(graph_idx, local_graph)| {
                    let resolver =
                        Resolver::new(symbol_table_ref).with_path_aliases(path_aliases.clone());
                    let start_idx = start_indices[graph_idx];
                    let mut local_edges: Vec<Edge> = Vec::new();
                    for (node_offset, raw_node) in local_graph.nodes.iter().enumerate() {
                        let current_node_idx = start_idx + node_offset as u32;
                        pass2_emit_node_edges(
                            &resolver,
                            local_graph,
                            raw_node,
                            current_node_idx,
                            reason_heritage,
                            reason_type,
                            reason_call,
                            &mut local_edges,
                        );
                    }
                    pass2_emit_framework_and_fanout(
                        &resolver,
                        symbol_table_ref,
                        local_graph,
                        reason_cache_ref,
                        &mut local_edges,
                    );
                    local_edges
                })
                .collect()
        };
        let mut edges = edges;
        let resolver_dump_drain = resolver_for_dump.as_mut();

        edges.extend(route_edges);
        edges.extend(entry_edges);

        // Pass: blind spots — pure metadata passthrough, no edges created.
        // Each local_graph's blind_spots are interned and stored in the graph's
        // file-level metadata for `gnx context` / `gnx index` to surface to
        // the LLM (truly unresolvable patterns like eval/dynamic-import).
        let mut all_blind_spots: Vec<BlindSpotRecord> = Vec::new();
        for local_graph in &self.local_graphs {
            for bs in &local_graph.blind_spots {
                all_blind_spots.push(BlindSpotRecord {
                    kind: string_pool.add(&bs.kind),
                    file_path: string_pool.add(&bs.file_path.to_string_lossy().replace('\\', "/")),
                    start_row: bs.span.0,
                    start_col: bs.span.1,
                    end_row: bs.span.2,
                    end_col: bs.span.3,
                    hint: string_pool.add(&bs.hint),
                });
            }
        }

        // Optional: flush the resolver decision dump now that pass 2 is done.
        // Spec: docs/specs/2026-05-15-resolver-oracle-harness.md
        // Only the serial dump-enabled path keeps a `Resolver` alive past
        // Pass 2; the parallel path constructs ephemeral per-graph resolvers
        // with `decisions: None`, so a dump-disabled run has nothing to flush.
        if let Some(dump_path) = self.resolver_dump_path.as_ref() {
            if let Some(resolver) = resolver_dump_drain {
                if let Some(decisions) = resolver.take_decisions() {
                    if let Err(e) = write_resolver_dump(dump_path, &decisions, &symbol_table) {
                        tracing::warn!("Failed to write resolver dump to {:?}: {}", dump_path, e);
                    }
                }
            }
        }

        // Pass 3: Community detection (Leiden) over Calls/Extends/Implements edges.
        // Leiden's refinement phase prevents the badly-connected-hub failure
        // mode where Louvain pins a hub to its first-touched chain.
        // Writes community_id back onto each Node in place.
        let assignments = graph_nexus_core::algorithms::leiden::detect_communities(
            &nodes,
            &edges,
            &graph_nexus_core::algorithms::leiden::LeidenConfig::default(),
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

        let traces = graph_nexus_core::algorithms::process_trace::detect_processes(
            &nodes,
            &edges,
            &file_paths,
            &graph_nexus_core::algorithms::process_trace::ProcessConfig::default(),
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

        // Build in_edge_idx (indices of edges sorted by target).
        // Same overflow guard as the node accumulator: precompute total in
        // u64 and assert before the lossy cast would corrupt the index range.
        assert!(
            edges.len() as u64 <= u32::MAX as u64,
            "total edge count {} exceeds u32::MAX — edge index scheme would overflow",
            edges.len()
        );
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
            magic: graph_nexus_core::graph::GRAPH_MAGIC,
            version: graph_nexus_core::graph::GRAPH_FORMAT_VERSION,
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
            blind_spots: all_blind_spots,
            route_shapes: Vec::new(),
        }
    }
}

/// Serialize captured resolver decisions to a JSONL file. Schema matches
/// the oracle harness contract: one decision per line, fields ordered for
/// readable diffs. Each line is a flattened `ResolverDecision` plus the
/// resolved `target_file` (looked up from `target_id` via the symbol
/// table). Delegating to `serde_json` keeps escaping (Unicode controls,
/// surrogates, line separators) compliant with RFC 8259.
/// Emit Pass-2 edges for a single `raw_node`'s heritage / calls / type
/// annotation. Factored out so the serial dump path and the parallel
/// hot path can share the same per-node logic.
#[allow(clippy::too_many_arguments)]
fn pass2_emit_node_edges(
    resolver: &Resolver<'_>,
    local_graph: &LocalGraph,
    raw_node: &RawNode,
    current_node_idx: u32,
    reason_heritage: StrRef,
    reason_type: StrRef,
    reason_call: StrRef,
    edges: &mut Vec<Edge>,
) {
    for base in &raw_node.heritage {
        let targets = resolver.resolve_symbol(
            &local_graph.file_path,
            base,
            &local_graph.imports,
            ResolveTarget::Type,
        );
        for (target_id, confidence) in targets {
            edges.push(Edge {
                source: current_node_idx,
                target: target_id,
                rel_type: RelType::Extends,
                confidence,
                reason: reason_heritage,
            });
        }
    }

    for callee in &raw_node.calls {
        let targets = resolver.resolve_symbol(
            &local_graph.file_path,
            callee,
            &local_graph.imports,
            ResolveTarget::Callable,
        );
        for (target_id, confidence) in targets {
            if target_id == current_node_idx {
                continue; // self-recursion edges are Louvain / process noise
            }
            edges.push(Edge {
                source: current_node_idx,
                target: target_id,
                rel_type: RelType::Calls,
                confidence,
                reason: reason_call,
            });
        }
    }

    if let Some(type_ann) = &raw_node.type_annotation {
        let targets = resolver.resolve_symbol(
            &local_graph.file_path,
            type_ann,
            &local_graph.imports,
            ResolveTarget::Type,
        );
        for (target_id, confidence) in targets {
            edges.push(Edge {
                source: current_node_idx,
                target: target_id,
                rel_type: RelType::Accesses,
                confidence,
                reason: reason_type,
            });
        }
    }
}

/// Emit Pass-2 framework-ref + fanout-ref edges for one `local_graph`.
/// Reason interning is pre-baked into `reason_cache` (see Pass 2 setup);
/// every entry that reaches this function is guaranteed to be in the map.
fn pass2_emit_framework_and_fanout(
    resolver: &Resolver<'_>,
    symbol_table: &SymbolTable,
    local_graph: &LocalGraph,
    reason_cache: &FxHashMap<String, StrRef>,
    edges: &mut Vec<Edge>,
) {
    let file_path_lossy = local_graph.file_path.to_string_lossy().replace('\\', "/");

    for fw_ref in &local_graph.framework_refs {
        let source_id = symbol_table.lookup_in_file(&file_path_lossy, &fw_ref.source_name);
        let Some(source_id) = source_id else { continue };
        let targets = resolver.resolve_symbol(
            &local_graph.file_path,
            &fw_ref.target_name,
            &local_graph.imports,
            ResolveTarget::Callable,
        );
        // `reason_cache` is filled by the same caller that walked
        // `self.local_graphs` to seed it, so every reason should be
        // present. Use `.get(...)` rather than `[]` indexing so a
        // future caller that forgets to pre-seed degrades to "skip
        // this batch of edges" instead of panicking mid-analyze on
        // a rayon worker (consistent with the best-effort policy of
        // every other Pass 2 error path).
        let Some(&reason_ref) = reason_cache.get(&fw_ref.reason) else {
            continue;
        };
        for (target_id, _) in targets {
            edges.push(Edge {
                source: source_id,
                target: target_id,
                rel_type: RelType::References,
                confidence: fw_ref.confidence,
                reason: reason_ref,
            });
        }
    }

    for fanout_ref in &local_graph.fanout_refs {
        let source_id = symbol_table.lookup_in_file(&file_path_lossy, &fanout_ref.source_name);
        let Some(source_id) = source_id else { continue };
        let n = fanout_ref.candidates.len() as f32;
        if n < 1.0 {
            continue;
        }
        let confidence = (fanout_ref.base_confidence / n.sqrt()).max(0.1);
        let Some(&reason_ref) = reason_cache.get(&fanout_ref.reason) else {
            continue;
        };
        for candidate_name in &fanout_ref.candidates {
            let targets = resolver.resolve_symbol(
                &local_graph.file_path,
                candidate_name,
                &local_graph.imports,
                ResolveTarget::Callable,
            );
            for (target_id, _) in targets {
                edges.push(Edge {
                    source: source_id,
                    target: target_id,
                    rel_type: RelType::References,
                    confidence,
                    reason: reason_ref,
                });
            }
        }
    }
}

fn write_resolver_dump(
    path: &std::path::Path,
    decisions: &[crate::resolution::resolver::ResolverDecision],
    symbol_table: &SymbolTable,
) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    for d in decisions {
        let line = DumpLine {
            src_file: &d.src_file,
            name: &d.name,
            specifier: d.specifier.as_deref(),
            tier: d.tier,
            target_file: d.target_id.and_then(|id| symbol_table.file_of(id)),
            alt_count: d.alt_count,
            confidence: d.confidence,
        };
        // serde_json into a Vec<u8> keeps the return type io::Result. The
        // alloc cost is per-decision; for dumps this only fires when the
        // user passed `--dump-resolver`, so production traffic is unaffected.
        let buf = serde_json::to_vec(&line).map_err(std::io::Error::other)?;
        f.write_all(&buf)?;
        f.write_all(b"\n")?;
    }
    f.flush()?;
    Ok(())
}

#[derive(serde::Serialize)]
struct DumpLine<'a> {
    src_file: &'a str,
    name: &'a str,
    specifier: Option<&'a str>,
    tier: crate::resolution::resolver::DecisionTier,
    target_file: Option<&'a str>,
    alt_count: u32,
    confidence: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use graph_nexus_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawImport, RawNode};
    use graph_nexus_core::graph::NodeKind;

    /// L0 end-to-end: caller imports `./b`, defining file lives at
    /// `src/b.ts`. Tier 2 ImportScoped must fire and emit a `Calls` edge
    /// at confidence 0.95. Locks in the 173-hit win measured on NestJS so
    /// it can't silently regress.
    #[test]
    fn l0_relative_import_produces_import_scoped_edge() {
        let caller = LocalGraph {
            file_path: "src/a.ts".into(),
            content_hash: [0; 32],
            nodes: vec![RawNode {
                name: "useThing".into(),
                kind: NodeKind::Function,
                span: (0, 0, 0, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec!["thing".into()],
            }],
            documents: vec![],
            imports: vec![RawImport {
                source: "./b".into(),
                imported_name: "thing".into(),
                alias: None,
            }],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
        };
        let target = LocalGraph {
            file_path: "src/b.ts".into(),
            content_hash: [0; 32],
            nodes: vec![RawNode {
                name: "thing".into(),
                kind: NodeKind::Function,
                span: (0, 0, 0, 0),
                is_exported: true,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
            }],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
        };

        let mut builder = GraphBuilder::new();
        builder.add_graph(caller);
        builder.add_graph(target);
        let graph = builder.build();

        let calls: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::Calls)
            .collect();
        assert_eq!(
            calls.len(),
            1,
            "exactly one Calls edge expected (./b → thing), got {}",
            calls.len()
        );
        // ImportScoped confidence = 0.95 — locks in that L0 promoted the
        // resolution past Tier 3 Global (0.7).
        assert!(
            (calls[0].confidence - 0.95).abs() < 1e-6,
            "Calls edge should be ImportScoped (0.95), got {}",
            calls[0].confidence
        );
    }

    /// `write_resolver_dump` round-trip: produce a dump containing
    /// boundary characters (quote, backslash, newline, control byte,
    /// non-ASCII), parse it back as JSON, assert fidelity. Locks in the
    /// serde-based serializer against silent escape regressions if we
    /// ever revert to a hand-rolled writer.
    #[test]
    fn resolver_dump_round_trips_through_serde_json() {
        use crate::resolution::resolver::{DecisionTier, ResolverDecision};

        let symbol_table = SymbolTable::new();
        let decisions = vec![
            ResolverDecision {
                src_file: "weird \"name\".ts".into(),
                name: "fn\\with\nbreak".into(),
                specifier: Some("./bar".into()),
                tier: DecisionTier::ImportScoped,
                target_id: None,
                alt_count: 0,
                confidence: Some(0.95),
            },
            ResolverDecision {
                src_file: "中文/檔名.py".into(),
                name: "你好".into(),
                specifier: None,
                tier: DecisionTier::Unresolved,
                target_id: None,
                alt_count: 0,
                confidence: None,
            },
        ];

        let tmp = std::env::temp_dir().join(format!("gnx-dump-test-{}.jsonl", std::process::id()));
        write_resolver_dump(&tmp, &decisions, &symbol_table).expect("write dump");
        let text = std::fs::read_to_string(&tmp).expect("read dump");
        let _ = std::fs::remove_file(&tmp);

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        for (line, original) in lines.iter().zip(decisions.iter()) {
            let v: serde_json::Value = serde_json::from_str(line).expect("valid JSONL");
            assert_eq!(v["src_file"], original.src_file);
            assert_eq!(v["name"], original.name);
            assert_eq!(v["alt_count"], 0);
            match original.tier {
                DecisionTier::ImportScoped => assert_eq!(v["tier"], "ImportScoped"),
                DecisionTier::Unresolved => assert_eq!(v["tier"], "Unresolved"),
                DecisionTier::SameFile | DecisionTier::QualifierScoped | DecisionTier::Global => {
                    panic!(
                        "fixture should only produce ImportScoped/Unresolved, got {:?}",
                        original.tier
                    )
                }
            }
        }
    }

    #[test]
    fn fanout_ref_emits_n_edges_with_confidence_decay() {
        use graph_nexus_core::analyzer::types::RawFanoutRef;

        let g = LocalGraph {
            file_path: "test.py".into(),
            content_hash: [0; 32],
            nodes: vec![
                RawNode {
                    name: "dispatch".into(),
                    kind: NodeKind::Method,
                    span: (0, 0, 5, 0),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                },
                RawNode {
                    name: "handle_a".into(),
                    kind: NodeKind::Method,
                    span: (10, 0, 12, 0),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                },
                RawNode {
                    name: "handle_b".into(),
                    kind: NodeKind::Method,
                    span: (14, 0, 16, 0),
                    is_exported: false,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                },
                RawNode {
                    name: "handle_c".into(),
                    kind: NodeKind::Method,
                    span: (18, 0, 20, 0),
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
            framework_refs: vec![],
            fanout_refs: vec![RawFanoutRef {
                source_name: "dispatch".into(),
                candidates: vec!["handle_a".into(), "handle_b".into(), "handle_c".into()],
                base_confidence: 0.5,
                reason: "reflection-getattr-fanout".into(),
                span: (0, 0, 0, 0),
            }],
            blind_spots: vec![],
        };

        let mut builder = GraphBuilder::new();
        builder.add_graph(g);
        let graph = builder.build();

        let fanout_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::References)
            .collect();

        // Expect 3 edges (one per candidate), each with confidence ≈ 0.5 / sqrt(3) ≈ 0.29.
        assert_eq!(
            fanout_edges.len(),
            3,
            "expected 3 fan-out edges, got {}",
            fanout_edges.len()
        );

        let expected_conf = 0.5_f32 / (3.0_f32).sqrt();
        for e in &fanout_edges {
            assert!(
                (e.confidence - expected_conf).abs() < 0.01,
                "expected conf ≈ {}, got {}",
                expected_conf,
                e.confidence
            );
            let reason_start = e.reason.offset as usize;
            let reason_end = reason_start + e.reason.len as usize;
            let reason_str = std::str::from_utf8(&graph.string_pool[reason_start..reason_end])
                .expect("reason utf-8");
            assert_eq!(reason_str, "reflection-getattr-fanout");
        }
    }

    #[test]
    fn fanout_ref_minimum_confidence_cap() {
        use graph_nexus_core::analyzer::types::RawFanoutRef;

        // 60 candidates → 0.5/sqrt(60) ≈ 0.0645，應 cap 到 0.1
        let mut nodes = vec![RawNode {
            name: "dispatch".into(),
            kind: NodeKind::Method,
            span: (0, 0, 5, 0),
            is_exported: false,
            heritage: vec![],
            type_annotation: None,
            decorators: vec![],
            calls: vec![],
        }];
        let mut candidates = vec![];
        for i in 0..60u32 {
            let name = format!("h{}", i);
            candidates.push(name.clone());
            nodes.push(RawNode {
                name,
                kind: NodeKind::Method,
                span: (10 + i, 0, 10 + i + 1, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
            });
        }
        let g = LocalGraph {
            file_path: "test.py".into(),
            content_hash: [0; 32],
            nodes,
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![RawFanoutRef {
                source_name: "dispatch".into(),
                candidates,
                base_confidence: 0.5,
                reason: "reflection-getattr-fanout".into(),
                span: (0, 0, 0, 0),
            }],
            blind_spots: vec![],
        };
        let mut builder = GraphBuilder::new();
        builder.add_graph(g);
        let graph = builder.build();

        let fanout_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::References)
            .collect();

        assert_eq!(fanout_edges.len(), 60);
        for e in &fanout_edges {
            assert!(
                (e.confidence - 0.1).abs() < 1e-5,
                "expected cap 0.1, got {}",
                e.confidence
            );
        }
    }

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
            fanout_refs: vec![],
            framework_refs: vec![RawFrameworkRef {
                source_name: "handler".into(),
                target_name: "get_db".into(),
                confidence: 0.6,
                reason: "fastapi-depends".into(),
                span: (0, 0, 0, 0),
            }],
            blind_spots: vec![],
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

    /// Build a single-node `LocalGraph` for end-to-end resolver tests.
    fn mk_file(path: &str, name: &str, kind: NodeKind, calls: Vec<String>) -> LocalGraph {
        LocalGraph {
            file_path: path.into(),
            content_hash: [0; 32],
            nodes: vec![RawNode {
                name: name.into(),
                kind,
                span: (0, 0, 0, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls,
            }],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
        }
    }

    /// Two same-named callables in different files must NOT both receive a
    /// CALLS edge from an ambiguous bare call site. Pin against fan-out
    /// regression for common names (`new` / `format` / `default` / ...).
    #[test]
    fn ambiguous_bare_callee_emits_no_calls_edge() {
        let mut builder = GraphBuilder::new();
        builder.add_graph(mk_file(
            "caller.rs",
            "caller_fn",
            NodeKind::Function,
            vec!["new".into()],
        ));
        builder.add_graph(mk_file("a.rs", "new", NodeKind::Method, vec![]));
        builder.add_graph(mk_file("b.rs", "new", NodeKind::Method, vec![]));
        let graph = builder.build();

        let calls_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::Calls)
            .collect();
        assert_eq!(
            calls_edges.len(),
            0,
            "ambiguous bare callee must produce zero CALLS edges, got {}: {:?}",
            calls_edges.len(),
            calls_edges
        );
    }

    /// Sibling: a uniquely-named callable still resolves via Tier 3 — the cap
    /// suppresses fan-out, not all cross-file resolution.
    #[test]
    fn unique_global_callable_still_emits_calls_edge() {
        let mut builder = GraphBuilder::new();
        builder.add_graph(mk_file(
            "caller.rs",
            "caller_fn",
            NodeKind::Function,
            vec!["uniquely_named_helper".into()],
        ));
        builder.add_graph(mk_file(
            "lib.rs",
            "uniquely_named_helper",
            NodeKind::Function,
            vec![],
        ));
        let graph = builder.build();

        let calls_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::Calls)
            .collect();
        assert_eq!(
            calls_edges.len(),
            1,
            "unique callable must emit exactly one CALLS edge"
        );
    }

    /// Task A acceptance: `LocalGraph.blind_spots` survive the builder pass
    /// and land in `ZeroCopyGraph.blind_spots` with all fields (kind /
    /// file_path / span / hint) preserved via the string pool. Locks in
    /// the contract that Task B (Python detector) and Task C (CLI
    /// surface) rely on.
    #[test]
    fn blind_spots_pass_through_to_graph() {
        use graph_nexus_core::analyzer::types::BlindSpot;

        let g = LocalGraph {
            file_path: "test.py".into(),
            content_hash: [0; 32],
            nodes: vec![],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![
                BlindSpot {
                    kind: "python-eval".into(),
                    file_path: "test.py".into(),
                    span: (10, 4, 10, 25),
                    hint: "eval(arg) — runtime code execution".into(),
                },
                BlindSpot {
                    kind: "python-dynamic-import".into(),
                    file_path: "test.py".into(),
                    span: (15, 0, 15, 40),
                    hint: "importlib.import_module(...) — dynamic loading".into(),
                },
            ],
        };

        let mut builder = GraphBuilder::new();
        builder.add_graph(g);
        let graph = builder.build();

        assert_eq!(
            graph.blind_spots.len(),
            2,
            "expected 2 blind spots in graph, got {}",
            graph.blind_spots.len()
        );

        let resolve = |sref: &graph_nexus_core::pool::StrRef| -> &str {
            let start = sref.offset as usize;
            let end = start + sref.len as usize;
            std::str::from_utf8(&graph.string_pool[start..end]).expect("utf-8")
        };

        let kinds: Vec<&str> = graph
            .blind_spots
            .iter()
            .map(|bs| resolve(&bs.kind))
            .collect();
        assert!(kinds.contains(&"python-eval"));
        assert!(kinds.contains(&"python-dynamic-import"));

        // Spot-check the first record's span + file_path + hint round-trip.
        let bs0 = &graph.blind_spots[0];
        assert_eq!(resolve(&bs0.file_path), "test.py");
        assert_eq!(bs0.start_row, 10);
        assert_eq!(bs0.start_col, 4);
        assert_eq!(bs0.end_row, 10);
        assert_eq!(bs0.end_col, 25);
        assert_eq!(resolve(&bs0.hint), "eval(arg) — runtime code execution");
    }

    /// Pins the contract that Pass-2 emits the same edge set whether the
    /// dump-enabled serial path or the dump-disabled parallel path runs.
    /// Without this test, all 8 existing builder tests exercise only the
    /// parallel path (none set `with_resolver_dump`), so a divergence
    /// between the two `pass2_emit_*` call sites would slip through
    /// until a user enables the oracle harness.
    ///
    /// The fixtures cover all five edge-emission categories so the test
    /// would catch:
    ///   * heritage (`Extends`) — Class with base
    ///   * calls (`Calls`) — Function with callee
    ///   * type_annotation (`Accesses`)
    ///   * framework_refs (`References` via Spring fixture)
    ///   * fanout_refs (`References` via reflection fixture)
    #[test]
    fn pass2_parallel_and_serial_emit_identical_edges() {
        use graph_nexus_core::analyzer::types::{RawFanoutRef, RawFrameworkRef};

        fn build_fixtures() -> Vec<LocalGraph> {
            vec![
                LocalGraph {
                    file_path: "src/foo.rs".into(),
                    content_hash: [0; 32],
                    nodes: vec![RawNode {
                        name: "Foo".into(),
                        kind: NodeKind::Class,
                        span: (0, 0, 10, 0),
                        is_exported: true,
                        heritage: vec!["Bar".into()],
                        type_annotation: Some("Other".into()),
                        decorators: vec![],
                        calls: vec!["other_fn".into()],
                    }],
                    documents: vec![],
                    imports: vec![],
                    routes: vec![],
                    framework_refs: vec![RawFrameworkRef {
                        source_name: "Foo".into(),
                        target_name: "other_fn".into(),
                        confidence: 0.9,
                        reason: "spring-autowired".into(),
                        span: (1, 0, 1, 10),
                    }],
                    fanout_refs: vec![RawFanoutRef {
                        source_name: "Foo".into(),
                        candidates: vec!["other_fn".into(), "Bar".into()],
                        base_confidence: 0.6,
                        reason: "python-getattr".into(),
                        span: (2, 0, 2, 5),
                    }],
                    blind_spots: vec![],
                },
                LocalGraph {
                    file_path: "src/bar.rs".into(),
                    content_hash: [0; 32],
                    nodes: vec![
                        RawNode {
                            name: "Bar".into(),
                            kind: NodeKind::Class,
                            span: (0, 0, 5, 0),
                            is_exported: true,
                            heritage: vec![],
                            type_annotation: None,
                            decorators: vec![],
                            calls: vec![],
                        },
                        RawNode {
                            name: "Other".into(),
                            kind: NodeKind::Class,
                            span: (6, 0, 10, 0),
                            is_exported: true,
                            heritage: vec![],
                            type_annotation: None,
                            decorators: vec![],
                            calls: vec![],
                        },
                        RawNode {
                            name: "other_fn".into(),
                            kind: NodeKind::Function,
                            span: (11, 0, 12, 0),
                            is_exported: true,
                            heritage: vec![],
                            type_annotation: None,
                            decorators: vec![],
                            calls: vec![],
                        },
                    ],
                    documents: vec![],
                    imports: vec![],
                    routes: vec![],
                    framework_refs: vec![],
                    fanout_refs: vec![],
                    blind_spots: vec![],
                },
            ]
        }

        // Parallel path (production): no dump enabled
        let mut parallel_builder = GraphBuilder::new();
        for lg in build_fixtures() {
            parallel_builder.add_graph(lg);
        }
        let parallel_graph = parallel_builder.build();

        // Serial path: dump enabled forces the serial branch
        let tmp = tempfile::TempDir::new().unwrap();
        let dump_path = tmp.path().join("dump.jsonl");
        let mut serial_builder = GraphBuilder::new().with_resolver_dump(Some(dump_path.clone()));
        for lg in build_fixtures() {
            serial_builder.add_graph(lg);
        }
        let serial_graph = serial_builder.build();

        // Compare edges as multisets — flat_map_iter ordering across rayon
        // workers can differ from the serial nested loop, but the SET of
        // (source, target, rel_type) tuples must match. `RelType` doesn't
        // derive Ord, so we use `{:?}` formatting as a stable key.
        let parallel_edges: std::collections::BTreeSet<(u32, u32, String)> = parallel_graph
            .edges
            .iter()
            .map(|e| (e.source, e.target, format!("{:?}", e.rel_type)))
            .collect();
        let serial_edges: std::collections::BTreeSet<(u32, u32, String)> = serial_graph
            .edges
            .iter()
            .map(|e| (e.source, e.target, format!("{:?}", e.rel_type)))
            .collect();
        assert_eq!(
            parallel_edges, serial_edges,
            "parallel Pass 2 must emit the same edges as the serial dump path",
        );

        // Node counts identical (both paths build identical SymbolTable + StringPool)
        assert_eq!(parallel_graph.nodes.len(), serial_graph.nodes.len());

        // Sanity: dump file actually exists for the serial run (proves the
        // serial branch was the one taken).
        assert!(
            dump_path.exists(),
            "serial branch must have produced a resolver dump file",
        );
    }
}
