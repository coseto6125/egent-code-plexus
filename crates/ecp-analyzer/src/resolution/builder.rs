use crate::fetch_shape::{
    consumer_keys, fetch_urls, format_reason, normalize_route_path, response_shapes,
};
use crate::framework_helpers::Span;
use crate::php::laravel_routes::LaravelRoutes;
use crate::post_process::path_literal_nodes::FILE_SOURCE_PENDING;
use crate::resolution::heuristics::{ResolutionTier, NEAREST_COPY_CONFIDENCE, NEAREST_COPY_REASON};
use crate::resolution::index::{DeclaredBase, FileMeta, Language, ResolveTarget, SymbolTable};
use crate::resolution::resolver::{ReexportIndex, Resolver};
use crate::rust::module_tree::RustWorkspaceModTree;
use ecp_core::analyzer::lexical::{
    file_identities, file_uids, lexical_scope_kinds, raw_decl, splits_by_arity, LexicalFunctions,
};
use ecp_core::analyzer::path_aliases::PathAliases;
use ecp_core::analyzer::reexport::{export_filter_applies, is_ecmascript, ImportRef};
use ecp_core::analyzer::type_ladder;
use ecp_core::analyzer::types::{
    owner_key, CallSite, LocalGraph, NameRef, RawImport, RawNode, RawRoute, VariantRef,
};
pub use ecp_core::analyzer::types::{
    FUNCTION_REFERENCE_REASON, VALUE_READ_REASON, VARIANT_PATTERN_REASON, VARIANT_USE_REASON,
};
use ecp_core::contracts::identity::{lexical_parents, UidSlot};
use ecp_core::contracts::raw_records::{FileOrd, RawIdx, RawIndexMap};
use ecp_core::contracts::routing::{FileRouter, Language as RouteLanguage};
use ecp_core::file_category::determine_category;
use ecp_core::graph::{
    BlindSpotRecord, CallMeta, Edge, ExportAliasFact, File, FileCategory, FunctionMeta, ImportFact,
    LexicalParentFact, Node, NodeKind, PathAliasFact, RelType, RouteMiddleware, RouteShape,
    SupertypeFact, ZeroCopyGraph, FILE_FLAG_ES_MODULE,
};
use ecp_core::graph_assembly::{GraphAssembly, ModuleFactTables};
use ecp_core::pool::{StrRef, StringPool};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

/// Classify a uid-hash collision based on `(kind, path)`. Returns the
/// BlindSpot `kind` string. See builder.rs near `uid_seen.get` for context.
/// Method / Constructor collisions never reach it: Pass 1 keeps them as
/// overloads ([`ecp_core::uid::keeps_overloads`]).
///
///   `ifdef-redef`    — Macro/Function/Struct/Enum/Typedef/Variable/Const
///       in C/C++ files. Headers commonly `#define X` or `static int foo()`
///       under different `#ifdef` branches (32/64-bit, GCC/MSVC,
///       portability shims). Tree-sitter parses both branches without
///       preprocessor evaluation, so both emit nodes that collide.
///
///   `field-reassign` — A non-C-family `Property` colliding with itself: the
///       same instance field assigned in more than one place (`self.x = …` in
///       `__init__` then again in another method; `this.x = …`; `@x = …`).
///       The uid `(kind, path, owner, name)` is identical across assignments
///       by design — they ARE the same field. Deduping to one node is correct
///       and loses no information, so this is not a parser bug and must not
///       surface as a `uid-collision` BlindSpot (it would falsely flag
///       `ecp impact`/`inspect` traversal as incomplete for ordinary OO code).
///
///   `uid-collision`  — Everything else: a true ambiguity in the corpus
///       worth investigating. After this reclassification, the count
///       drops from ~25K to <1K on .sample_repo, making the residual
///       set tractable.
pub(crate) fn classify_collision(kind: NodeKind, path: &str) -> &'static str {
    let is_c_family = path.ends_with(".c")
        || path.ends_with(".h")
        || path.ends_with(".cc")
        || path.ends_with(".cpp")
        || path.ends_with(".cxx")
        || path.ends_with(".hpp")
        || path.ends_with(".hh")
        || path.ends_with(".hxx");
    if is_c_family
        && matches!(
            kind,
            NodeKind::Macro
                | NodeKind::Function
                | NodeKind::Struct
                | NodeKind::Enum
                | NodeKind::Typedef
                | NodeKind::Variable
                | NodeKind::Const
                | NodeKind::Property
        )
    {
        return "ifdef-redef";
    }
    // Non-C-family Property reaching here is the same instance field assigned
    // in more than one place — normal OO, deduped without information loss.
    if matches!(kind, NodeKind::Property) {
        return "field-reassign";
    }
    "uid-collision"
}

use std::collections::HashMap;

/// Per-graph output from Pass 2 parallel map: `(edges, pending_call_metas,
/// file_edges)`. `pending_call_metas` entries are `(pre_sort_edge_idx, flags,
/// dispatch_type)`; `file_edges` start at the graph's `File` node, which the
/// builder appends later (C4 Open point 3, option b).
type PerGraphPass2 = (Vec<Edge>, Vec<(usize, u8, String)>, Vec<(FileOrd, Edge)>);

/// Pass 2's stitched whole-build output. Same shape as [`PerGraphPass2`], but
/// named apart because it is the aggregate across every graph, not one graph's
/// share — `pass2_resolve_edges` does whole-build setup (mod-tree build,
/// reason interning) before it fans out, so it is
/// called once, not once per file.
type Pass2Output = (Vec<Edge>, Vec<(usize, u8, String)>, Vec<(FileOrd, Edge)>);

/// Pass 1.5 output: `(route_node_idx, file_idx, route_method, route_path)`
/// per emitted Route node, consumed by Pass 1.6's fetch-shape route index.
type EmittedRoute = (u32, u32, String, String);

/// Pass 1 output: SymbolTable + StringPool own every name interned during
/// registration, threaded through as live state for every later pass.
struct Pass1Registration {
    symbol_table: SymbolTable,
    string_pool: StringPool,
    nodes: Vec<Node>,
    files: Vec<File>,
    collision_blind_spots: Vec<BlindSpotRecord>,
    /// `(tombstone slot, surviving slot)` for every collision tombstone.
    merged: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CallMetaKey {
    caller_span: (u32, u32, u32, u32),
    call_index: u32,
}

impl CallMetaKey {
    const fn new(caller_span: (u32, u32, u32, u32), call_index: u32) -> Self {
        Self {
            caller_span,
            call_index,
        }
    }
}

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
    old_file_hashes: HashMap<String, [u8; 8]>,
    /// When `Some`, the resolver pass 2 buffers every decision and writes a
    /// JSONL line per resolution attempt to this path. Used by the oracle
    /// verification harness (see specs/2026-05-15-resolver-oracle-harness.md).
    resolver_dump_path: Option<std::path::PathBuf>,
    /// Module-specifier aliases (TS `tsconfig.json` `compilerOptions.paths`,
    /// etc.) — forwarded to the resolver before Pass 2 starts.
    path_aliases: PathAliases,
    /// Repo root used to resolve `LocalGraph.file_path` (relative) to an
    /// absolute path when the fetch-shape pass needs to re-read the file
    /// content. Optional: when `None`, fetch-shape extraction is skipped
    /// (graph still builds; `route_shapes` is empty and no `Fetches`
    /// edges are emitted). Production callers (`ecp admin index`)
    /// always pass this; in-process tests opt in via tempdirs.
    repo_root: Option<std::path::PathBuf>,
    /// T7-6: per-file set of symbol UIDs whose body hash is UNCHANGED since
    /// the last full build. Symbols in this set skip `pass2_emit_node_edges`
    /// and the `class_membership` post-process. `None` = no diff data
    /// available → resolve everything (default, full-build path).
    ///
    /// Key: forward-slash relative path (matches `LocalGraph.file_path`
    /// normalisation used throughout the builder).
    /// Value: UIDs to SKIP. An empty inner set means all symbols changed;
    /// a full inner set means the file is entirely unchanged.
    symbol_skip_set: Option<FxHashMap<String, FxHashSet<u64>>>,
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
            old_file_hashes: HashMap::new(),
            resolver_dump_path: None,
            path_aliases: PathAliases::new(),
            repo_root: None,
            symbol_skip_set: None,
        }
    }

    pub fn with_path_aliases(mut self, aliases: PathAliases) -> Self {
        self.path_aliases = aliases;
        self
    }

    /// Provide the repo root so the fetch-shape pass can resolve each
    /// `LocalGraph.file_path` (relative) to an absolute path for re-read.
    /// Without this, `route_shapes` / `Fetches` edges are skipped.
    pub fn with_repo_root(mut self, root: std::path::PathBuf) -> Self {
        self.repo_root = Some(root);
        self
    }

    pub fn with_cache(mut self, hashes: HashMap<String, [u8; 8]>) -> Self {
        self.old_file_hashes = hashes;
        self
    }

    /// T7-6: supply per-symbol skip data from `symbol_hash_diff`. Symbols
    /// whose UID appears in the inner set for their file skip Pass 2 resolver
    /// work and the class_membership post-process entirely. Files absent from
    /// the outer map fall through to full reanalyze.
    ///
    /// The skip set is built by `ecp_cli::incremental::symbol_hash_diff` after
    /// all guards (import-set, shadow-candidate, SchemaField) have been
    /// evaluated. Callers MUST only pass this when the guard conditions are
    /// satisfied; the builder trusts the skip set unconditionally.
    pub fn with_symbol_skip_set(mut self, skip: FxHashMap<String, FxHashSet<u64>>) -> Self {
        self.symbol_skip_set = Some(skip);
        self
    }

    pub fn with_resolver_dump(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.resolver_dump_path = path;
        self
    }

    pub fn add_graph(&mut self, graph: LocalGraph) {
        self.local_graphs.push(graph);
    }

    pub fn build(mut self) -> ZeroCopyGraph {
        let prof = std::env::var("ECP_PROF").is_ok();
        let t_total = std::time::Instant::now();
        // Determinism: sort by file_path so node IDs and edge endpoints are
        // assigned in canonical order regardless of how the producer (scanner,
        // hook, manual add_graph) enumerated files. Without this, the same repo
        // indexed on two machines with different filesystem walk orders would
        // produce different graph.bin payloads — breaks the reproducibility
        // contract pinned by audit §4.2 (inv-003).
        // `file_path` is unique across LocalGraph entries (one ingest per file),
        // so unstable sort has no observable tie-breaking concern and avoids
        // the temporary allocation that stable sort needs.
        let t_sort = std::time::Instant::now();
        self.local_graphs
            .sort_unstable_by(|a, b| a.file_path.cmp(&b.file_path));
        if prof {
            eprintln!("prof build.sort: {:.3}s", t_sort.elapsed().as_secs_f32());
        }
        let _t_pass1 = std::time::Instant::now();

        let Pass1Registration {
            mut symbol_table,
            mut string_pool,
            mut nodes,
            files,
            collision_blind_spots,
            merged,
        } = pass1_register_nodes(&self.local_graphs);
        // Pass 1 gives every raw node a slot (a collision keeps a tombstone),
        // so the map reads node counts only and holds for the whole build.
        // A record of a tombstoned twin binds the node that kept its identity
        // (C4 Open point 2): an edge never starts at an empty-named tombstone.
        let raw_map = RawIndexMap::new(0, self.local_graphs.iter().map(|lg| lg.nodes.len()))
            .expect("total RawNode count exceeds u32::MAX — graph node ID scheme would overflow")
            .with_merged(merged)
            .expect("pass 1 pairs each tombstone with an earlier slot that is no tombstone");
        // Before Pass 2: the name tiers must drop a scoped node (I5) from
        // their first lookup on. The same rows become the v17 graph table.
        let lexical_table = record_lexical_parents(&self.local_graphs, &raw_map);
        for fact in &lexical_table {
            symbol_table.note_lexical_parent(fact.node_idx, fact.parent_idx);
        }
        // Edges from a record bound to `File`; the `File` nodes are appended
        // after the passes that emit them (C4 Open point 3, option b).
        let mut file_edges: Vec<(FileOrd, Edge)> = Vec::new();
        // Maps forward-slashed file_path → File node index. Populated by the
        // File-node loop after Pass 4, consumed by `post_process::imports_edges`
        // to wire (File)-[:Imports]->(symbol) edges. File nodes are deliberately
        // NOT registered in SymbolTable — File is metadata, not a symbol,
        // and a SymbolTable hit on a File node would create spurious
        // Calls/Accesses edges in the resolver tiers.
        let mut file_node_idx: FxHashMap<String, u32> =
            FxHashMap::with_capacity_and_hasher(self.local_graphs.len(), Default::default());

        let module_facts = record_module_facts(
            &self.local_graphs,
            &symbol_table,
            &self.path_aliases,
            nodes.len(),
            &mut string_pool,
        );

        if prof {
            eprintln!(
                "prof build.pass1_register: {:.3}s",
                _t_pass1.elapsed().as_secs_f32()
            );
        }
        let _t_pass15 = std::time::Instant::now();
        // Pass 1.5: Extract Routes
        let ExtractedRoutes {
            edges: route_edges,
            emitted: emitted_routes,
            middleware: mut route_middleware,
            actions: controller_actions,
            laravel,
            blind_spots: mut route_blind_spots,
        } = pass1_5_extract_routes(
            &self.local_graphs,
            &files,
            &symbol_table,
            &raw_map,
            &mut string_pool,
            &mut nodes,
        );

        if prof {
            eprintln!(
                "prof build.pass15_routes: {:.3}s",
                _t_pass15.elapsed().as_secs_f32()
            );
        }
        let _t_pass16 = std::time::Instant::now();
        // Pass 1.6: Fetch-shape extraction.
        let (route_shapes_out, fetches_edges) = pass1_6_fetch_shapes(
            &self.local_graphs,
            self.repo_root.as_deref(),
            &emitted_routes,
            &nodes,
            &mut string_pool,
        );
        file_edges.extend(fetches_edges);

        if prof {
            eprintln!(
                "prof build.pass16_fetch_shape: {:.3}s",
                _t_pass16.elapsed().as_secs_f32()
            );
        }
        let _t_pass17 = std::time::Instant::now();
        // Pass 1.7: Entry-point scoring (cross-language).
        let (entry_edges, entry_blind_spots) = pass1_7_entry_points(
            &self.local_graphs,
            &files,
            &raw_map,
            &mut string_pool,
            &mut nodes,
        );

        if prof {
            eprintln!(
                "prof build.pass17_entry_points: {:.3}s",
                _t_pass17.elapsed().as_secs_f32()
            );
        }
        let _t_pass18 = std::time::Instant::now();
        // Pass 1.8: FunctionMeta collection.
        let function_metas = pass1_8_function_metas(
            &self.local_graphs,
            &raw_map,
            &mut string_pool,
            &mut symbol_table,
        );
        if prof {
            eprintln!(
                "prof build.pass18_function_meta: {:.3}s  count={}",
                _t_pass18.elapsed().as_secs_f32(),
                function_metas.len()
            );
        }
        // Build the Rust workspace module tree once, for the supertypes
        // pre-pass and Pass 2. This is Tier 3.5: resolves `crate::a::b::fn`
        // FQN calls to concrete files by walking the filesystem mod tree from
        // each crate root. Gated on `repo_root` being set — test harnesses
        // that don't set a repo root simply skip module-tree resolution.
        let mod_tree: Option<RustWorkspaceModTree> =
            self.repo_root.as_deref().map(RustWorkspaceModTree::build);
        let mod_tree_ctx = mod_tree.as_ref().zip(self.repo_root.as_deref());
        // The overlay's crate heads: the rows the tree's manifest parse made.
        let workspace = mod_tree
            .as_ref()
            .map(|tree| tree.workspace_facts().clone())
            .unwrap_or_default();
        let reexports = ReexportIndex::build(&self.local_graphs, &symbol_table, &self.path_aliases);

        let _t_pass19 = std::time::Instant::now();
        let declared_supertypes = pass1_9_supertypes(
            &self.local_graphs,
            &symbol_table,
            &self.path_aliases,
            mod_tree_ctx,
            &reexports,
            &raw_map,
        );
        symbol_table.build_declared_bases(declared_supertypes);
        let supertype_table = record_supertypes(&symbol_table);
        let action_edges = pass1_95_controller_actions(
            &laravel,
            &controller_actions,
            &self.local_graphs,
            &files,
            &symbol_table,
            &raw_map,
            &entry_edges,
            &mut string_pool,
            &mut nodes,
            &mut route_middleware,
            &mut route_blind_spots,
        );
        let alias_table = record_path_aliases(&self.path_aliases, &mut string_pool);
        if prof {
            eprintln!(
                "prof build.pass19_supertypes: {:.3}s",
                _t_pass19.elapsed().as_secs_f32()
            );
        }

        let _t_pass2 = std::time::Instant::now();
        // Pass 2: resolve imports and build edges (rayon-vs-serial dual
        // path; see `pass2_resolve_edges` for the pre-computation +
        // dispatch logic).
        let (mut edges, pending_call_metas_global, framework_file_edges) = pass2_resolve_edges(
            &self.local_graphs,
            &symbol_table,
            &mut string_pool,
            &self.path_aliases,
            mod_tree_ctx,
            &reexports,
            &raw_map,
            self.resolver_dump_path.as_deref(),
            self.symbol_skip_set.as_ref(),
        );
        file_edges.extend(framework_file_edges);

        edges.extend(route_edges);
        edges.extend(entry_edges);
        edges.extend(action_edges);

        emit_closure_references(
            &self.local_graphs,
            &raw_map,
            &nodes,
            &mut string_pool,
            &mut edges,
        );
        emit_overload_references(&symbol_table, &mut string_pool, &mut edges);

        if prof {
            eprintln!(
                "prof build.pass2_imports_resolve: {:.3}s",
                _t_pass2.elapsed().as_secs_f32()
            );
        }
        let _t_blind = std::time::Instant::now();
        // Pass: blind spots — pure metadata passthrough, no edges created.
        // Each local_graph's blind_spots are interned and stored in the graph's
        // file-level metadata for `ecp context` / `ecp index` to surface to
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
                    is_test: bs.is_test,
                });
            }
        }
        // Merge D1 collision blind spots from Pass 1 (recorded before string_pool
        // was moved into ZeroCopyGraph). BlindSpotRecord StrRefs are already valid
        // because `string_pool` was borrowed mutably in Pass 1 when they were pushed.
        all_blind_spots.extend(collision_blind_spots);
        all_blind_spots.extend(route_blind_spots);
        all_blind_spots.extend(entry_blind_spots);

        if prof {
            eprintln!(
                "prof build.blind_spots: {:.3}s",
                _t_blind.elapsed().as_secs_f32()
            );
        }
        let _t_pass3 = std::time::Instant::now();
        // Pass 3: Community detection (Leiden) over Calls/Extends/Implements edges.
        // Leiden's refinement phase prevents the badly-connected-hub failure
        // mode where Louvain pins a hub to its first-touched chain.
        // Writes community_id back onto each Node in place.
        let assignments = ecp_core::algorithms::leiden::detect_communities(
            &nodes,
            &edges,
            &ecp_core::algorithms::leiden::LeidenConfig::default(),
        );
        for (node, &c) in nodes.iter_mut().zip(assignments.iter()) {
            node.community_id = c;
        }

        if prof {
            eprintln!(
                "prof build.pass3_community: {:.3}s",
                _t_pass3.elapsed().as_secs_f32()
            );
        }
        let _t_pass4 = std::time::Instant::now();
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

        let traces = ecp_core::algorithms::process_trace::detect_processes(
            &nodes,
            &edges,
            &file_paths,
            &ecp_core::algorithms::process_trace::ProcessConfig::default(),
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
            let proc_uid_name = format!(
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
            let process_file_path = nodes
                .get(entry_idx as usize)
                .map(|n| {
                    let fi = n.file_idx as usize;
                    if fi < files.len() {
                        let f = &files[fi];
                        let s = f.path.offset as usize;
                        let e = s + f.path.len as usize;
                        std::str::from_utf8(&string_pool.bytes[s..e])
                            .unwrap_or("")
                            .to_string()
                    } else {
                        String::new()
                    }
                })
                .unwrap_or_default();

            nodes.push(Node {
                uid: ecp_core::uid::compute(
                    NodeKind::Process,
                    &process_file_path,
                    None,
                    &proc_uid_name,
                ),
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
                owner_class: StrRef::default(),
                content_hash: 0,
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

        // Cross-language class membership post-process. Emits HasMethod /
        // HasProperty edges (Class → Function|Method|Property) for every
        // language. Pass 1 uses span containment; Pass 2 uses RawNode.owner_class
        // set directly by each parser. Must run BEFORE the CSR construction below
        // so new edges land in `out_offsets` / `in_offsets`.
        if prof {
            let n_intra = traces
                .iter()
                .filter(|t| {
                    matches!(
                        t.process_type,
                        ecp_core::algorithms::process_trace::ProcessType::IntraCommunity
                    )
                })
                .count();
            let n_cross = traces.len() - n_intra;
            let avg_steps = if traces.is_empty() {
                0.0
            } else {
                traces.iter().map(|t| t.trace.len()).sum::<usize>() as f32 / traces.len() as f32
            };
            let avg_spread = if traces.is_empty() {
                0.0
            } else {
                traces.iter().map(|t| t.communities.len()).sum::<usize>() as f32
                    / traces.len() as f32
            };
            eprintln!(
                "prof build.pass4_processes: {:.3}s count={} intra={n_intra} cross={n_cross} avg_steps={avg_steps:.2} avg_spread={avg_spread:.2}",
                _t_pass4.elapsed().as_secs_f32(),
                traces.len(),
            );
        }
        let _t_class_mem = std::time::Instant::now();
        crate::post_process::class_membership::emit_edges(
            &self.local_graphs,
            &symbol_table,
            &mut string_pool,
            &mut edges,
            self.symbol_skip_set.as_ref(),
        );

        // Enum → EnumVariant containment edges. Runs after class_membership
        // (same pipeline position) because EnumVariant owner_class is stamped
        // by stamp_owner_class_by_span at parse time; no dependency on HasMethod.
        crate::post_process::enum_variant_defines::emit_edges(
            &self.local_graphs,
            &symbol_table,
            &mut string_pool,
            &mut edges,
        );

        // Override resolution — emits `RelType::Overrides` edges (concrete
        // method → overridden supertype method). Runs after class_membership
        // so HasMethod edges are already in place; before CSR construction so
        // the new edges land in out_offsets / in_offsets.
        crate::post_process::overrides::emit_edges(
            &self.local_graphs,
            &files,
            &raw_map,
            &symbol_table,
            &mut string_pool,
            &mut edges,
            &mut all_blind_spots,
        );

        // T4-7: promote `RawSchemaField` → `SchemaField` Nodes + `HasProperty`
        // (Class → SchemaField) + `MirrorsField` heuristic edges across
        // detected cross-framework mirrors. Runs after overrides because it
        // appends new Nodes; before File-node loop so SchemaField nodes
        // occupy idx range BEFORE files[] (keeps file_node_idx contiguous).
        // SchemaField nodes are NOT registered in SymbolTable — nothing
        // references them by name; reverse traversal goes via HasProperty.
        crate::post_process::schema_field_mirrors::emit_edges(
            &self.local_graphs,
            &raw_map,
            &mut string_pool,
            &mut nodes,
            &mut edges,
        );

        // T5-33: promote `RawEventTopic`s → `EventTopic` Nodes +
        // `Publishes`/`Subscribes` edges (fn → EventTopic) + `EventTopicMirror`
        // heuristic edges between matched Publish↔Subscribe pairs. Runs after
        // schema_field_mirrors (sibling post-process) and before File-node loop
        // for the same idx-contiguity reason. EventTopic nodes are NOT
        // registered in SymbolTable — queries reach them via Publishes/Subscribes
        // traversal from function nodes.
        crate::post_process::event_topic_mirrors::emit_edges(
            &self.local_graphs,
            &raw_map,
            &mut string_pool,
            &mut nodes,
            &mut edges,
            &mut file_edges,
        );

        // T10: promote `RawTxScope`s → `TransactionScope` Nodes + `OpensTxScope`
        // edges (fn → TransactionScope). Runs after event_topic_mirrors (sibling
        // post-process) and before File-node loop for idx-contiguity. Nodes are
        // NOT registered in SymbolTable — queries reach them via OpensTxScope
        // traversal from function nodes.
        crate::post_process::tx_scope_edges::emit_edges(
            &self.local_graphs,
            &raw_map,
            &mut string_pool,
            &mut nodes,
            &mut edges,
        );

        // Decorates edge emission — promotes `RawNode.decorators` to first-class
        // `Decorates` edges from decorated symbols to `Annotation` nodes (resolved
        // or synthetic). Runs before File-node loop so synthetic Annotation nodes
        // land in the raw-symbol index range, consistent with EventTopic / SchemaField.
        let decorates_resolver = Resolver::new(&symbol_table)
            .with_path_aliases(self.path_aliases.clone())
            .with_reexports(&reexports);
        crate::post_process::decorates_edges::emit_edges(
            &self.local_graphs,
            &raw_map,
            &decorates_resolver,
            &mut string_pool,
            &mut nodes,
            &mut edges,
        );

        // PathLiteral promotion — emits `PathLiteral` Nodes + `UsesPathLiteral`
        // edges (bound endpoint → PathLiteral; a File endpoint waits in
        // `file_edges` for the File-node loop). Same idx-contiguity contract as
        // schema_field_mirrors / event_topic_mirrors above: appends Nodes
        // before the File-node loop runs.
        crate::post_process::path_literal_nodes::emit_edges(
            &self.local_graphs,
            &raw_map,
            &mut string_pool,
            &mut nodes,
            &mut edges,
            &mut file_edges,
        );

        crate::post_process::sql_table_edges::emit_edges(
            &self.local_graphs,
            &raw_map,
            &mut string_pool,
            &nodes,
            &mut edges,
            &mut file_edges,
        );

        // Saga compensation pairing — emits heuristic `CompensatedBy` edges
        // (compensator → operation) over the node/edge buffer. Runs AFTER all
        // Calls edges exist (so calls-back evidence is checkable) and AFTER
        // owner_class is resolved on nodes. Adds no nodes, so ordering vs the
        // File-node loop is unconstrained.
        crate::post_process::saga_pairs::emit_edges(&nodes, &mut string_pool, &mut edges);

        // Append one `NodeKind::File` node per LocalGraph at the tail of
        // `nodes` (idx >= raw-node count). Doing it here — AFTER all passes
        // that index symbols by SymbolTable + use raw node idx ranges —
        // keeps raw node ids dense [0..N) for the SymbolTable's monotonic-
        // dense invariant (`index.rs::register_node` debug_assert). File
        // nodes don't enter SymbolTable; they're metadata reachable only
        // via `file_node_idx` for module-level edges (Imports today).
        let first_file_node = nodes.len() as u32;
        for (i, local_graph) in self.local_graphs.iter().enumerate() {
            let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
            let basename = std::path::Path::new(&path_str)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path_str.clone());
            let name_ref = string_pool.add(&basename);
            // `Node.file_idx` points into `files: Vec<File>`. Pass 1 pushed
            // `files` in the same `self.local_graphs` enumeration order, so
            // the i-th LocalGraph's File node references `files[i]`.
            //
            // Use the iteration index directly — an earlier `file_node_idx.len()`
            // formulation silently lagged behind `i` when paths duplicated
            // (HashMap insert overwrites instead of growing), pointing later
            // File nodes at the wrong files[] entry.
            let node_file_idx = i as u32;
            // `nodes.len()` is the authoritative index because Passes 1.5
            // (Routes), 1.6/2 (EntryPoint), and Pass 4 (Process) all push
            // into `nodes` AFTER Pass 1 without keeping `current_node_idx`
            // in sync — using `current_node_idx` here pointed `file_node_idx`
            // at stale raw-node indices, which would silently make
            // `(File)-[:Imports]->()` edges originate from random Route /
            // EntryPoint / Process nodes instead of File.
            let file_node_id = nodes.len() as u32;
            nodes.push(Node {
                uid: ecp_core::uid::compute(NodeKind::File, &path_str, None, &path_str),
                name: name_ref,
                file_idx: node_file_idx,
                kind: NodeKind::File,
                span: (0, 0, 0, 0),
                community_id: 0,
                owner_class: StrRef::default(),
                content_hash: 0,
            });
            file_node_idx.insert(path_str, file_node_id);
        }
        // The loop pushes one `File` node per `LocalGraph`, in input order.
        edges.extend(file_edges.into_iter().map(|(file, mut edge)| {
            edge.source = first_file_node + file.0;
            edge
        }));

        // Cross-language Imports post-process. Emits (File)-[:Imports]->(symbol)
        // edges by resolving each `RawImport.imported_name` against the same
        // SymbolTable + path_aliases used in Pass 2. Resolver misses don't
        // emit — refuse gitnexus-style cross-language false positives
        // (`.mjs → Path.java`). Must run BEFORE CSR construction below so
        // new edges land in `out_offsets` / `in_offsets`.
        let imports_resolver = Resolver::new(&symbol_table)
            .with_path_aliases(self.path_aliases.clone())
            .with_reexports(&reexports);
        if prof {
            eprintln!(
                "prof build.class_membership: {:.3}s",
                _t_class_mem.elapsed().as_secs_f32()
            );
        }
        let _t_imports_edges = std::time::Instant::now();
        let _track_imports_call = ();
        crate::post_process::imports_edges::emit_edges(
            &self.local_graphs,
            &imports_resolver,
            &file_node_idx,
            &mut string_pool,
            &mut edges,
        );

        if prof {
            eprintln!(
                "prof build.imports_edges: {:.3}s",
                _t_imports_edges.elapsed().as_secs_f32()
            );
        }

        // Scope-containment Defines post-process. Emits:
        //   (File)-[:Defines]->(top-level symbol)
        //   (Namespace|Module)-[:Defines]->(child symbol)
        // Runs after file_node_idx is populated and after class_membership so
        // HasMethod/HasProperty edges are in place — the no-duplication invariant
        // (Defines never overlaps HasMethod/HasProperty) is verified by the
        // regression test `defines_emission::no_duplication_class_method`.
        crate::post_process::scope_defines::emit_edges(
            &self.local_graphs,
            &file_node_idx,
            &raw_map,
            &mut string_pool,
            &mut edges,
        );

        // Renamed from `build.csr_assembly`: the same timer now spans the whole
        // assembly — edge sort, both CSR directions, call-meta remap, name and
        // kind indexes — so a number compared against the old label reads as a
        // regression that never happened.
        let _t_assembly = std::time::Instant::now();
        // Intern dispatch types here — the pool is ours. Edge indices stay in
        // pre-sort space; `finish()` remaps them when it sorts the edges.
        let call_metas: Vec<CallMeta> = pending_call_metas_global
            .into_iter()
            .filter_map(|(pre_idx, flags, dispatch_type)| {
                // Reject rather than truncate: a wrapped index would name a
                // real, wrong edge, where `finish()` can only drop what is
                // out of range.
                Some(CallMeta {
                    edge_idx: u32::try_from(pre_idx).ok()?,
                    flags,
                    dispatch_type: string_pool.add(&dispatch_type),
                })
            })
            .collect();

        let graph = GraphAssembly {
            string_pool,
            files,
            nodes,
            edges,
            call_metas,
            function_metas,
            blind_spots: all_blind_spots,
            route_shapes: route_shapes_out,
            route_middleware,
            process_start: process_start_idx,
            traces_offsets,
            traces_data,
            fingerprint: [0; 32],
            module_facts,
            alias_table,
            supertype_table,
            workspace,
            lexical_parents: lexical_table,
        }
        .finish();

        if prof {
            eprintln!(
                "prof build.graph_assembly: {:.3}s  total_build: {:.3}s",
                _t_assembly.elapsed().as_secs_f32(),
                t_total.elapsed().as_secs_f32()
            );
        }

        graph
    }
}

/// The heritage the overlay walks (the v14 `supertype_table`): each type's
/// resolved bases and first-unresolved position, as the pre-pass left them in
/// `symbol_table`. SymbolTable ids are node indices: Pass 1 registers raw
/// nodes densely from 0 and later passes only append.
fn record_supertypes(symbol_table: &SymbolTable) -> Vec<SupertypeFact> {
    symbol_table
        .all_supertypes()
        .map(|(type_idx, supertypes)| SupertypeFact {
            type_idx,
            bases: supertypes.bases.to_vec(),
            first_unresolved: supertypes.first_unresolved,
        })
        .collect()
}

/// The path aliases the resolver ran with (the v14 `alias_table`), in
/// declaration order.
fn record_path_aliases(
    path_aliases: &PathAliases,
    string_pool: &mut StringPool,
) -> Vec<PathAliasFact> {
    path_aliases
        .entries()
        .map(|(pattern, replacements)| PathAliasFact {
            pattern: string_pool.add(pattern),
            replacements: replacements.iter().map(|r| string_pool.add(r)).collect(),
        })
        .collect()
}

/// The import / export facts the overlay replays the import tier from
/// (the v13 fields of `ZeroCopyGraph`): each file's
/// imports in source order, each TS / JS file's export alias pairs and
/// ES-module flag, and the exported declarations, as Pass 1 recorded them
/// in `symbol_table`. `raw_nodes` is the node count after Pass 1. A TS / JS
/// row the re-export hop follows keeps the file its specifier resolves to,
/// through `path_aliases` as the resolver probes it.
fn record_module_facts(
    local_graphs: &[LocalGraph],
    symbol_table: &SymbolTable,
    path_aliases: &PathAliases,
    raw_nodes: usize,
    string_pool: &mut StringPool,
) -> ModuleFactTables {
    let resolver = Resolver::new(symbol_table).with_path_aliases(path_aliases.clone());
    // Built on the first re-export row: most files re-export nothing.
    let mut file_idx: Option<FxHashMap<String, u32>> = None;
    let mut facts = ModuleFactTables {
        file_imports: Vec::with_capacity(local_graphs.len() + 1),
        file_export_aliases: Vec::with_capacity(local_graphs.len() + 1),
        file_flags: Vec::with_capacity(local_graphs.len()),
        exported_nodes: vec![0; raw_nodes.div_ceil(8)],
        ..ModuleFactTables::default()
    };
    facts.file_imports.push(0);
    facts.file_export_aliases.push(0);
    for local_graph in local_graphs {
        let path = local_graph.file_path.to_string_lossy().replace('\\', "/");
        let language = Language::from_normalized_path(&path);
        let ecmascript = is_ecmascript(language);
        for import in &local_graph.imports {
            let target_file = if ecmascript && followed_by_reexport(local_graph, import) {
                resolver
                    .ecmascript_module(&path, import)
                    .and_then(|module| {
                        file_idx
                            .get_or_insert_with(|| file_indices(local_graphs))
                            .get(&*module)
                            .copied()
                    })
            } else {
                None
            };
            facts.imports.push(ImportFact {
                source: string_pool.add(&import.source),
                imported_name: string_pool.add(&import.imported_name),
                alias: import.alias.as_deref().map(|alias| string_pool.add(alias)),
                binding_kind: import.binding_kind,
                target_file,
            });
        }
        facts.file_imports.push(facts.imports.len() as u32);
        if export_filter_applies(&path) {
            let aliases = local_graph.export_aliases.as_deref().unwrap_or(&[]);
            facts
                .export_aliases
                .extend(aliases.iter().map(|alias| ExportAliasFact {
                    exported: string_pool.add(&alias.exported),
                    local: string_pool.add(&alias.local),
                }));
        }
        facts
            .file_export_aliases
            .push(facts.export_aliases.len() as u32);
        facts.file_flags.push(if symbol_table.is_es_module(&path) {
            FILE_FLAG_ES_MODULE
        } else {
            0
        });
    }
    for id in symbol_table.exported_ids() {
        facts.exported_nodes[id as usize / 8] |= 1 << (id % 8);
    }
    facts
}

/// Whether the TS / JS re-export hop may follow `import` of `local_graph`
/// (`reexport::ecmascript_reexport`): an `export *`, a binding the file
/// exports under its own name, or the local of an export alias.
fn followed_by_reexport(local_graph: &LocalGraph, import: &RawImport) -> bool {
    let import = ImportRef::from(import);
    import.is_star_export()
        || import.is_es_reexport()
        || local_graph
            .export_aliases
            .as_deref()
            .into_iter()
            .flatten()
            .any(|alias| &*alias.local == import.local_name())
}

/// Forward-slash path → file index, as Pass 1 numbers the files.
/// First global node index of file `file_idx`. `file_idx` always comes from
/// enumerating the `local_graphs` the map was built from, so the `end()`
/// fallback is unreachable.
fn file_base(raw_map: &RawIndexMap, file_idx: u32) -> u32 {
    raw_map
        .range(FileOrd(file_idx))
        .map_or_else(|| raw_map.end(), |range| range.start)
}

/// The v17 lexical-parent table: one row per scoped node, from
/// [`lexical_parents`] over each file's `(kind, span)`, as
/// [`file_identities`] derives the parents for the uids. A parent that
/// Pass 1 tombstoned maps to its surviving twin. A tombstoned scoped node
/// gets no row: its twin owns the identity and gets its own. Empty while no
/// language opts in ([`lexical_scope_kinds`]).
fn record_lexical_parents(
    local_graphs: &[LocalGraph],
    raw_map: &RawIndexMap,
) -> Vec<LexicalParentFact> {
    let mut table = Vec::new();
    for (file, local_graph) in local_graphs.iter().enumerate() {
        let path = local_graph.file_path.to_string_lossy();
        let scoped = lexical_scope_kinds(Language::from_path(&path));
        if scoped.is_empty() {
            continue;
        }
        let file = FileOrd(file as u32);
        let decls: Vec<_> = local_graph.nodes.iter().map(|n| (n.kind, n.span)).collect();
        for (raw, parent) in lexical_parents(&decls, scoped).into_iter().enumerate() {
            let raw = RawIdx(raw as u32);
            let (Some(node_idx), Some(parent_idx)) = (
                raw_map.node(file, raw),
                parent.and_then(|parent| raw_map.node(file, RawIdx(parent))),
            ) else {
                continue;
            };
            if raw_map.slot(file, raw) == Some(node_idx) {
                table.push(LexicalParentFact {
                    node_idx,
                    parent_idx,
                });
            }
        }
    }
    table
}

fn file_indices(local_graphs: &[LocalGraph]) -> FxHashMap<String, u32> {
    local_graphs
        .iter()
        .enumerate()
        .map(|(idx, graph)| {
            (
                graph.file_path.to_string_lossy().replace('\\', "/"),
                idx as u32,
            )
        })
        .collect()
}

/// Pass 1: register every `LocalGraph`'s File + symbol nodes into the
/// SymbolTable/StringPool, assigning canonical UIDs, keeping overloads under
/// their own UIDs and tombstoning other D1 hash collisions (see
/// BlindSpotRecord `uid-collision`/`ifdef-redef` kinds).
fn pass1_register_nodes(local_graphs: &[LocalGraph]) -> Pass1Registration {
    // Pre-size accumulators from known input cardinalities. Each
    // LocalGraph contributes 1 File node + N symbol nodes. Plus a
    // small per-graph slack for Pass 1.5 Route nodes (one per
    // routing decorator). Vec growth from 0 → 297k goes through
    // ~17 reallocations + memcopies; sizing once avoids that.
    let total_symbol_nodes: usize = local_graphs.iter().map(|g| g.nodes.len()).sum();
    let total_files = local_graphs.len();
    // 10% slack for Pass 1.5 Route synthetics; over-shoot is cheap
    // (single tail growth), under-shoot just reverts to default.
    let mut symbol_table = SymbolTable::new();
    let mut string_pool = StringPool::new();
    let mut nodes = Vec::with_capacity(total_symbol_nodes + total_symbol_nodes / 10);
    let mut files = Vec::with_capacity(total_files);
    let mut current_node_idx = 0;
    // D1 collision set: uid_u64 → node_idx of the first occurrence. On a
    // collision (rare path), the previous node's kind/path/owner/name
    // are reconstructed from `nodes[prev_idx]` + `files` + `string_pool`
    // for the BlindSpotRecord hint. Storing only the index keeps the
    // hot path alloc-free (vs the prior 4-String tuple which allocated
    // ~1M Strings across 245k nodes on .sample_repo).
    let mut uid_seen: FxHashMap<u64, u32> =
        FxHashMap::with_capacity_and_hasher(total_symbol_nodes, Default::default());
    // Collision BlindSpots are accumulated here and merged into all_blind_spots
    // after the main blind-spot pass (below). Using a separate vec avoids
    // borrowing string_pool twice in the node loop.
    let mut collision_blind_spots: Vec<BlindSpotRecord> = Vec::new();
    let mut merged: Vec<(u32, u32)> = Vec::new();

    for (file_idx, local_graph) in local_graphs.iter().enumerate() {
        let file_idx = file_idx as u32;
        // Path → string 一律走 forward-slash，讓 UID / lookup / 顯示在 Windows
        // 上與 Linux/macOS 一致（與 resolver.rs / registry/path.rs 既有 idiom 對齊）。
        // Cow: `to_string_lossy()` returns Cow; `.replace()` always allocates.
        // Skip the replace + alloc on Linux/macOS where paths use `/` already.
        let raw_path = local_graph.file_path.to_string_lossy();
        let path_str: std::borrow::Cow<'_, str> = if raw_path.contains('\\') {
            std::borrow::Cow::Owned(raw_path.replace('\\', "/"))
        } else {
            raw_path
        };
        let path_ref = string_pool.add(&path_str);
        symbol_table.register_file(&path_str);
        // Hoisted once per file. `register_node` would otherwise call
        // `FileMeta::from_path` per node (~25x redundant on the
        // .sample_repo distribution), each allocating one `String` for the
        // `\\` -> `/` normalisation. `path_str` is already forward-slash.
        let category = determine_category(&path_str);
        let file_meta = crate::resolution::index::FileMeta::from_normalized_path(&path_str);
        if let Some(aliases) = &local_graph.export_aliases {
            symbol_table.note_export_aliases(&path_str, file_meta.language, aliases);
        }

        files.push(File {
            path: path_ref,
            mtime: 0, // In a real implementation, fetch actual mtime
            content_hash: local_graph.content_hash,
            category,
        });

        let graph_start = current_node_idx;
        let identities = file_identities(&path_str, &local_graph.nodes, raw_decl);
        // In-file first declaration → the node of another file whose uid it
        // repeats. Two `LocalGraph`s of one path do occur; their later
        // declarations then number against that earlier node, as one file.
        let mut repeats: FxHashMap<u32, u32> = FxHashMap::default();
        for (offset, (raw_node, identity)) in local_graph.nodes.iter().zip(identities).enumerate() {
            // The file's own numbering (`assign_file`), shared with incremental
            // mode and the overlay view.
            let uid_u64 = identity.slot.uid();
            let arity_splits = splits_by_arity(file_meta.language, raw_node.kind);
            // A C# type of another generic arity than the earlier ones of its
            // uid (`Conv<T>` after `Conv`): a node of its own under the
            // overload uid, with its name and owner, filed beside its first
            // instead of in a name table, so a lookup of the bare name still
            // returns the first, as before the split (codex ruling P-a).
            if let (UidSlot::Later { first, .. }, true) = (identity.slot, arity_splits) {
                let first_idx = repeats.get(&first).copied().unwrap_or(graph_start + first);
                symbol_table.register_arity_sibling(
                    first_idx,
                    &path_str,
                    file_meta,
                    raw_node.generic_arity(),
                );
                uid_seen.insert(uid_u64, current_node_idx);
                let first_node: &Node = &nodes[first_idx as usize];
                nodes.push(Node {
                    uid: uid_u64,
                    name: first_node.name,
                    file_idx,
                    kind: raw_node.kind,
                    span: raw_node.span,
                    community_id: 0,
                    owner_class: first_node.owner_class,
                    content_hash: raw_node.content_hash,
                });
                current_node_idx += 1;
                continue;
            }
            let earlier = match identity.slot {
                UidSlot::First(uid) => {
                    let earlier = uid_seen.get(&uid).copied();
                    if let Some(prev_idx) = earlier {
                        repeats.insert(offset as u32, prev_idx);
                    }
                    earlier
                }
                UidSlot::Later { first, .. } | UidSlot::Duplicate { first, .. } => {
                    Some(repeats.get(&first).copied().unwrap_or(graph_start + first))
                }
            };

            // D1 collision recovery: if two distinct symbol definitions hash to the
            // same u64, drop the second and record a BlindSpot for manual review.
            //
            // Collision nodes must NOT be registered in the SymbolTable — they
            // have no valid graph node backing them (we don't push them into
            // `nodes`). The SymbolTable maps name→node_id, and a phantom node_id
            // >= nodes.len() at CSR time causes an OOB panic in the out_offsets
            // loop (builder.rs:1549).
            //
            // However, `nodes` MUST still receive a placeholder entry to preserve
            // position alignment: the build's `RawIndexMap` counts
            // `lg.nodes.len()` (all raw nodes, including collision ones). If we
            // skip the `nodes.push` without also adjusting the map, every
            // subsequent file's node IDs in the map are off by the collision
            // count, producing wrong `current_node_idx` values and broken edges.
            //
            // Solution: push a tombstone Node (kind=File is reused as the most
            // benign placeholder; it doesn't enter SymbolTable so it's unreachable
            // from any query) to occupy the position, keeping nodes.len() ≡
            // current_node_idx throughout Pass 1.
            if let Some(prev_idx) = earlier {
                // An overload is a real declaration: it keeps its name and
                // owner (shared with the first, so already pooled) under a
                // uid of its own, and loses nothing a BlindSpot would report.
                // The slot decides, as in the overlay view, so a kind that
                // lexical scoping opts in numbers alike on both paths. A
                // `First` slot that repeats another `LocalGraph` of the same
                // path has no in-file slot to read; it numbers by kind, as
                // `assign_file` numbers an unscoped declaration.
                let keeps_overload = match identity.slot {
                    UidSlot::Later { .. } => true,
                    UidSlot::Duplicate { .. } => false,
                    UidSlot::First(_) => ecp_core::uid::keeps_overloads(raw_node.kind),
                };
                if keeps_overload {
                    let ordinal = symbol_table.register_overload(prev_idx, file_meta);
                    let first: &Node = &nodes[prev_idx as usize];
                    nodes.push(Node {
                        // Equal to the `UidSlot::Later` uid unless the file
                        // repeats another file's declarations.
                        uid: ecp_core::uid::overload(first.uid, ordinal),
                        name: first.name,
                        file_idx,
                        kind: raw_node.kind,
                        span: raw_node.span,
                        community_id: 0,
                        owner_class: first.owner_class,
                        content_hash: raw_node.content_hash,
                    });
                    current_node_idx += 1;
                    continue;
                }
                // Reclassify the collision based on (kind, lang) — most
                // collisions are not parser bugs but legitimate semantic
                // ambiguities the indexer can't resolve without semantic
                // analysis. Triaging them up front lets `uid-collision`
                // mean "true parser ambiguity worth investigating".
                //
                //   - ifdef-redef     : C/C++ Macro/Function/Struct/Enum/
                //     Typedef/Variable redefined under `#ifdef` branches
                //     (`#define X` for 32/64-bit, `static int foo() { ... }`
                //     repeated under different platform guards).
                //   - uid-collision   : everything else (real parser bug
                //     or unhandled corpus pattern).
                let bs_kind = classify_collision(raw_node.kind, &path_str);
                // `field-reassign` is the same instance field assigned in
                // multiple places; deduping to one node loses no information,
                // so it records NO BlindSpot — otherwise it would falsely flag
                // `ecp impact`/`inspect` traversal as incomplete for ordinary
                // OO code. The tombstone below still runs (the dedup is real).
                if bs_kind != "field-reassign" {
                    // Reconstruct prev info from the first occurrence's Node
                    // (rare path; the alloc savings on every other node pay
                    // for this lookup many times over).
                    let prev_node: &ecp_core::graph::Node = &nodes[prev_idx as usize];
                    let prev_kind = prev_node.kind.as_str();
                    let prev_path = string_pool.resolve(&files[prev_node.file_idx as usize].path);
                    let prev_name = string_pool.resolve(&prev_node.name);
                    let prev_owner = if prev_node.owner_class.len > 0 {
                        string_pool.resolve(&prev_node.owner_class)
                    } else {
                        ""
                    };
                    let hint = ecp_core::graph::format_hint(
                        bs_kind,
                        ecp_core::graph::HintFields {
                            kind: prev_kind,
                            path: prev_path,
                            owner: prev_owner,
                            name: prev_name,
                        },
                        ecp_core::graph::HintFields {
                            kind: raw_node.kind.as_str(),
                            path: &path_str,
                            owner: raw_node.owner_class.as_deref().unwrap_or(""),
                            name: &raw_node.name,
                        },
                    );
                    collision_blind_spots.push(BlindSpotRecord {
                        kind: string_pool.add(bs_kind),
                        file_path: string_pool.add(&path_str),
                        start_row: raw_node.span.0,
                        start_col: raw_node.span.1,
                        end_row: raw_node.span.2,
                        end_col: raw_node.span.3,
                        hint: string_pool.add(&hint),
                        // parser-metric BlindSpot (`DEV_METRIC_BS_KINDS`); not
                        // LLM-actionable, so is_test is irrelevant — fix false.
                        is_test: false,
                    });
                }
                // Push a tombstone Node + tombstone SymbolTable entry to keep
                // both `nodes.len()` and `node_kinds.len()` ≡ current_node_idx.
                // The tombstone is NOT registered in file_scoped / global_scoped,
                // so it is invisible to all name-based lookups and cannot produce
                // OOB edges via class_membership or overrides post-processes.
                // Tombstone uid+name MUST differ from the surviving node's;
                // otherwise `seen_uids.insert(node.uid)` would falsely flag a
                // duplicate and name-based queries (`n.name == X`) would still
                // hit the tombstone. The surviving node already owns `uid_u64`
                // and the original `raw_node.name`; the tombstone gets bit-
                // inverted uid (`!uid_u64`) plus empty name (StrRef::default,
                // which resolves to "" against any pool). The bit-inverse
                // never equals the survivor's uid, but it is not unique: every
                // later duplicate of one base uid gets the same tombstone uid.
                // Tombstones stay isolated by their empty name and their
                // absence from the symbol table, and "" never matches a real
                // symbol-name query.
                symbol_table.register_tombstone(raw_node.kind, file_meta);
                merged.push((current_node_idx, prev_idx));
                nodes.push(Node {
                    uid: ecp_core::uid::tombstone(uid_u64),
                    name: StrRef::default(),
                    file_idx,
                    kind: raw_node.kind,
                    span: raw_node.span,
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                });
                current_node_idx += 1;
                continue;
            }
            // Just the index — collision path will reconstruct details
            // from nodes[prev_idx] + files + string_pool if needed.
            uid_seen.insert(uid_u64, current_node_idx);

            symbol_table.register_node_with_meta(
                &path_str,
                file_meta,
                &raw_node.name,
                current_node_idx,
                raw_node.kind,
                raw_node.owner_class.as_deref(),
            );
            if arity_splits {
                symbol_table.note_type_arity(current_node_idx, raw_node.generic_arity());
            }
            if raw_node.is_exported {
                symbol_table.note_export(&path_str, file_meta.language, current_node_idx);
            }

            let name_ref = string_pool.add(&raw_node.name);
            // Skip hash-lookup when there is no owner: `StrRef::default()`
            // (offset=0, len=0) resolves to "" against any pool. Avoids
            // ~N hash-table lookups for top-level symbols at build time.
            let owner_class_ref = match raw_node.owner_class.as_deref() {
                Some(s) => string_pool.add(s),
                None => StrRef::default(),
            };

            nodes.push(Node {
                uid: uid_u64,
                name: name_ref,
                file_idx,
                kind: raw_node.kind,
                span: raw_node.span,
                community_id: 0,
                owner_class: owner_class_ref,
                content_hash: raw_node.content_hash,
            });

            current_node_idx += 1;
        }
        if file_meta.language == Language::Rust {
            note_rust_impl_members(&mut symbol_table, &local_graph.nodes, graph_start);
            symbol_table.note_rust_lexical_scopes(&local_graph.nodes, graph_start);
        }

        // NOTE: documents (markdown/yaml section/doc nodes) are parsed into
        // `local_graph.documents` but the graph.bin DocumentBlock storage is
        // not wired up yet. Skipped here intentionally — re-enable when the
        // `DocumentBlock` type lands in `ecp_core::graph`.
    }

    // Finalize the basename-stem → file paths view consumed by the
    // resolver's Tier-4 module-file fallback. Pass 1 is the only writer
    // of `file_scoped`, so finalizing here gives every subsequent pass
    // (and the resolver) an O(1) `files_by_stem` lookup instead of an
    // O(N_files) scan per qualified call.
    symbol_table.build_stem_index();
    symbol_table.build_constructor_index();
    symbol_table.build_owner_kinds();

    Pass1Registration {
        symbol_table,
        string_pool,
        nodes,
        files,
        collision_blind_spots,
        merged,
    }
}

/// Map a file path to the language hint accepted by
/// `response_shapes::extract` / `consumer_keys::extract`. `None` for
/// languages with no fetch-shape extractor, so the pass skips those files
/// cheaply.
///
/// Routing goes through the canonical provider table rather than a private
/// extension list: which extensions are TypeScript is a fact the analyzer
/// already owns, and this repo has twice shipped drift from a hand-copied
/// dispatch table (PR #141/#142, PR #595).
fn lang_for_path(path: &str) -> Option<response_shapes::Lang> {
    // A sniffed route (generic YAML) has no fetch-shape extractor.
    let route = FileRouter::PATH_ONLY.route(path).route().copied()?;
    match route.fixed_language()? {
        RouteLanguage::TypeScript => Some(response_shapes::Lang::TypeScript),
        RouteLanguage::JavaScript => Some(response_shapes::Lang::JavaScript),
        RouteLanguage::Php => Some(response_shapes::Lang::Php),
        _ => None,
    }
}

/// True for file extensions where `fetch_urls::extract` can find HTTP
/// client call sites. PHP is a server-side language; Python is included
/// because `requests` / `httpx` are common HTTP client libraries.
fn is_consumer_extension(path: &str) -> bool {
    let dot = match path.rfind('.') {
        Some(d) => d,
        None => return false,
    };
    matches!(
        &path[dot + 1..],
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py"
    )
}

/// Record the `impl` block of every Rust method in `nodes`, the raw nodes
/// of one file whose ids start at `start`
/// ([`SymbolTable::note_impl_member`]), by the rule the session overlay
/// shares ([`ecp_core::analyzer::same_file_pick::rust_impl_members`]).
fn note_rust_impl_members(symbol_table: &mut SymbolTable, nodes: &[RawNode], start: u32) {
    ecp_core::analyzer::same_file_pick::rust_impl_members(nodes, |member, block, implemented| {
        symbol_table.note_impl_member(start + member as u32, start + block as u32, implemented)
    });
}

/// A Laravel route that names a controller action (`'C@m'`).
struct PendingAction {
    route: u32,
    /// `METHOD path`, the Route node's name.
    route_name: String,
    action: String,
    namespace: Option<String>,
    /// The route-file middleware the Route node carries, and the names its
    /// groups remove, which the controller's middleware joins.
    middleware: Vec<String>,
    excluded: Vec<String>,
}

/// What Pass 1.5 hands on.
struct ExtractedRoutes {
    edges: Vec<Edge>,
    /// Consumed by Pass 1.6 to build the fetch-shape route index.
    emitted: Vec<EmittedRoute>,
    middleware: Vec<RouteMiddleware>,
    /// Bound after Pass 1.9 by [`pass1_95_controller_actions`].
    actions: Vec<PendingAction>,
    laravel: LaravelRoutes,
    /// Route files whose routes lost Laravel include scopes to the cap.
    blind_spots: Vec<BlindSpotRecord>,
}

/// Pass 1.5: Extract Routes.
fn pass1_5_extract_routes(
    local_graphs: &[LocalGraph],
    files: &[File],
    symbol_table: &SymbolTable,
    raw_map: &RawIndexMap,
    string_pool: &mut StringPool,
    nodes: &mut Vec<Node>,
) -> ExtractedRoutes {
    let mut route_edges = Vec::new();
    let mut emitted_routes: Vec<EmittedRoute> = Vec::new();
    let mut route_middleware: Vec<RouteMiddleware> = Vec::new();
    let mut actions: Vec<PendingAction> = Vec::new();
    // Route groups reach across files (a provider's `->group(base_path(
    // 'routes/admin.php'))` prefixes every route of `admin.php`), so the
    // per-file view alone cannot name a Laravel route's path or handler.
    let laravel = LaravelRoutes::build(local_graphs, raw_map);
    for (file_idx, local_graph) in local_graphs.iter().enumerate() {
        let file_idx = file_idx as u32;
        let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
        // Skip Route emission for genuinely non-production files
        // (Test/Reference). `Example` is INTENTIONALLY NOT skipped —
        // framework example apps (Express `examples/auth/`, Flask
        // `examples/tutorial/`) are canonical "how to wire routes"
        // content that LLM consumers explicitly want to navigate.
        // Tests (`/tests/`, `.spec.`, `Test.java` …) stay skipped
        // because their `@app.route('/test_setup')` fixture routes
        // would pollute the production-route surface. Reads the
        // category already computed in Pass 1 (line 229) — avoids
        // re-running `determine_category` (~36 string scans per file).
        let is_non_production = matches!(
            files[file_idx as usize].category,
            FileCategory::Test | FileCategory::Reference
        );
        if is_non_production {
            continue;
        }

        for (raw_idx, raw_node) in local_graph.nodes.iter().enumerate() {
            let Some(handler_idx) = raw_map.node(FileOrd(file_idx), RawIdx(raw_idx as u32)) else {
                continue;
            };

            for dec in &raw_node.decorators {
                for detected in crate::route_detector::detect_all_from_decorator(dec) {
                    let route_name = format!("{} {}", detected.method, detected.path);

                    let route_idx = nodes.len() as u32;
                    nodes.push(Node {
                        uid: ecp_core::uid::compute(NodeKind::Route, &path_str, None, &route_name),
                        name: string_pool.add(&route_name),
                        file_idx,
                        kind: ecp_core::graph::NodeKind::Route,
                        span: raw_node.span,
                        community_id: 0,
                        owner_class: StrRef::default(),
                        content_hash: 0,
                    });

                    route_edges.push(Edge {
                        source: handler_idx,
                        target: route_idx,
                        rel_type: RelType::HandlesRoute,
                        confidence: 1.0,
                        reason: string_pool.add("decorator"),
                    });

                    emitted_routes.push((
                        route_idx,
                        file_idx,
                        detected.method.clone(),
                        detected.path.clone(),
                    ));
                }
            }
        }

        for raw_route in &local_graph.routes {
            if raw_route.method == RawRoute::INCLUDE
                || raw_route.method == RawRoute::CONTROLLER_MIDDLEWARE
            {
                continue;
            }
            let Some(detected) = crate::route_detector::detect_from_call(raw_route) else {
                continue;
            };
            let promoted = match raw_route.scope.as_deref() {
                Some(scope) => laravel.promote(
                    file_idx,
                    scope,
                    &detected.path,
                    raw_route.handler.as_deref(),
                ),
                None => vec![crate::php::laravel_routes::PromotedRoute {
                    path: detected.path.clone(),
                    middleware: Vec::new(),
                    excluded: Vec::new(),
                    namespace: None,
                    action: raw_route.handler.clone(),
                }],
            };
            // Two including groups can give one route the same path. Laravel's
            // `RouteCollection` keeps the route registered last for a method
            // and URI, so one Route node stands for both with the last
            // group's middleware. Include scopes come in the source order of
            // the including calls, which is the registration order within one
            // provider file only.
            let last_of_path: Vec<bool> = (0..promoted.len())
                .map(|i| promoted[i + 1..].iter().all(|r| r.path != promoted[i].path))
                .collect();
            for (route, last) in promoted.into_iter().zip(last_of_path) {
                if !last {
                    continue;
                }
                let route_name = format!("{} {}", detected.method, route.path);

                let route_idx = nodes.len() as u32;
                nodes.push(Node {
                    uid: ecp_core::uid::compute(NodeKind::Route, &path_str, None, &route_name),
                    name: string_pool.add(&route_name),
                    file_idx,
                    kind: ecp_core::graph::NodeKind::Route,
                    span: raw_route.span,
                    community_id: 0,
                    owner_class: StrRef::default(),
                    content_hash: 0,
                });

                // Resolve the imperative-route handler, if the parser captured
                // a named handler (e.g. `app.get("/x", loginHandler)`). The
                // handler must be a function/method registered in the same
                // file; inline arrow functions are not captured. A route with
                // a group scope names a controller action (`Ctrl@method`) in
                // any file instead, bound once heritage is resolved.
                match (route.action.as_deref(), raw_route.scope.is_some()) {
                    (None, _) => {}
                    (Some(action), true) => actions.push(PendingAction {
                        route: route_idx,
                        route_name: route_name.clone(),
                        action: action.to_string(),
                        namespace: route.namespace.clone(),
                        middleware: route.middleware.clone(),
                        excluded: route.excluded.clone(),
                    }),
                    (Some(handler_name), false) => {
                        if let Some(handler) = symbol_table.lookup_in_file(&path_str, handler_name)
                        {
                            route_edges.push(Edge {
                                source: handler,
                                target: route_idx,
                                rel_type: RelType::HandlesRoute,
                                confidence: 1.0,
                                reason: string_pool.add("call-arg"),
                            });
                        }
                    }
                }

                if !route.middleware.is_empty() {
                    route_middleware.push(RouteMiddleware {
                        node_idx: route_idx,
                        middleware: route
                            .middleware
                            .iter()
                            .map(|name| string_pool.add(name))
                            .collect(),
                    });
                }

                emitted_routes.push((route_idx, file_idx, detected.method.clone(), route.path));
            }
        }
    }
    let blind_spots: Vec<BlindSpotRecord> = laravel
        .capped_includes()
        .iter()
        .map(|capped| {
            let graph = &local_graphs[capped.file_idx as usize];
            BlindSpotRecord {
                kind: string_pool.add(crate::php::laravel_routes::BS_KIND_INCLUDE_SCOPE_CAP),
                file_path: string_pool.add(&graph.file_path.to_string_lossy().replace('\\', "/")),
                start_row: 0,
                start_col: 0,
                end_row: 0,
                end_col: 0,
                hint: string_pool.add(&capped.hint()),
                is_test: matches!(files[capped.file_idx as usize].category, FileCategory::Test),
            }
        })
        .collect();
    ExtractedRoutes {
        edges: route_edges,
        emitted: emitted_routes,
        middleware: route_middleware,
        actions,
        laravel,
        blind_spots,
    }
}

/// Reason strings an `EntryPoint` folds in before it counts the rest: a
/// base-controller method can handle every route of an application.
const MAX_ENTRY_REASONS: usize = 8;

/// Pass 1.95: Laravel controller actions.
///
/// Binds each [`PendingAction`] to its controller method with a
/// `HandlesRoute` edge. It runs after Pass 1.9 because a controller can
/// inherit the action from a used trait or a parent class, and only the
/// resolved supertypes say which one.
///
/// Each bound method also gets the `EntryPoint` marker Pass 1.7 gives a
/// same-file route handler, in the method's own file. Pass 1.7 cannot see
/// it: the route and the method are in different files.
///
/// The middleware the controller registers for the bound method joins the
/// route's `route_middleware` after the route-file middleware, each name
/// once and none the route's groups remove, as Laravel's
/// `Route::gatherMiddleware` and `Router::resolveMiddleware` compose them.
/// Controller middleware that is not statically certain joins nothing; a
/// BlindSpot in the controller's file says why.
#[allow(clippy::too_many_arguments)]
fn pass1_95_controller_actions(
    laravel: &LaravelRoutes,
    actions: &[PendingAction],
    local_graphs: &[LocalGraph],
    files: &[File],
    symbol_table: &SymbolTable,
    raw_map: &RawIndexMap,
    entry_edges: &[Edge],
    string_pool: &mut StringPool,
    nodes: &mut Vec<Node>,
    route_middleware: &mut Vec<RouteMiddleware>,
    blind_spots: &mut Vec<BlindSpotRecord>,
) -> Vec<Edge> {
    let mut edges = Vec::new();
    let mut joined: Vec<RouteMiddleware> = Vec::new();
    let mut unread: Vec<crate::php::laravel_routes::UnreadMiddleware> = Vec::new();
    // Handler → (best confidence, reasons), in first-bound order.
    let mut entries: Vec<(u32, f32, Vec<&str>)> = Vec::new();
    let mut entry_of: FxHashMap<u32, usize> = FxHashMap::default();
    for pending in actions {
        let Some(found) =
            laravel.controller_action(&pending.action, pending.namespace.as_deref(), symbol_table)
        else {
            // The route names an action, not a closure: say so, so a reader
            // of the edge-less Route node does not take it for an inline
            // handler.
            let route = &nodes[pending.route as usize];
            let graph = &local_graphs[route.file_idx as usize];
            blind_spots.push(BlindSpotRecord {
                kind: string_pool.add(crate::php::laravel_routes::BS_KIND_UNBOUND_ACTION),
                file_path: string_pool.add(&graph.file_path.to_string_lossy().replace('\\', "/")),
                start_row: route.span.0,
                start_col: route.span.1,
                end_row: route.span.2,
                end_col: route.span.3,
                hint: string_pool.add(&crate::php::laravel_routes::unbound_action_hint(
                    &pending.route_name,
                    &pending.action,
                )),
                is_test: matches!(files[route.file_idx as usize].category, FileCategory::Test),
            });
            continue;
        };
        let confidence = if found.by_namespace { 1.0 } else { 0.9 };
        let reason = match (found.by_namespace, found.inherited) {
            (true, false) => "controller-action",
            (true, true) => "controller-action:inherited",
            (false, false) => "controller-action:unique-name",
            (false, true) => "controller-action:unique-name:inherited",
        };
        edges.push(Edge {
            source: found.node,
            target: pending.route,
            rel_type: RelType::HandlesRoute,
            confidence,
            reason: string_pool.add(reason),
        });
        let mut middleware = pending.middleware.clone();
        match &found.middleware {
            Ok(names) => {
                for name in names {
                    if !middleware.contains(name) && !pending.excluded.contains(name) {
                        middleware.push(name.clone());
                    }
                }
            }
            Err(spots) => {
                for spot in spots {
                    if !unread.contains(spot) {
                        unread.push(spot.clone());
                    }
                }
            }
        }
        if middleware.len() > pending.middleware.len() {
            joined.push(RouteMiddleware {
                node_idx: pending.route,
                middleware: middleware
                    .iter()
                    .map(|name| string_pool.add(name))
                    .collect(),
            });
        }
        match entry_of.get(&found.node) {
            Some(&slot) => {
                let (_, best, reasons) = &mut entries[slot];
                *best = best.max(confidence);
                reasons.push(pending.route_name.as_str());
            }
            None => {
                entry_of.insert(found.node, entries.len());
                entries.push((found.node, confidence, vec![pending.route_name.as_str()]));
            }
        }
    }
    if !joined.is_empty() {
        // An entry replaces the route's route-file-only one. Sorted by node
        // index once: `route_middleware_of` binary-searches it.
        let replaced: FxHashSet<u32> = joined.iter().map(|entry| entry.node_idx).collect();
        route_middleware.retain(|entry| !replaced.contains(&entry.node_idx));
        route_middleware.append(&mut joined);
        route_middleware.sort_unstable_by_key(|entry| entry.node_idx);
    }
    for spot in unread {
        let graph = &local_graphs[spot.file_idx as usize];
        blind_spots.push(BlindSpotRecord {
            kind: string_pool.add(crate::php::laravel_routes::BS_KIND_CONTROLLER_MIDDLEWARE),
            file_path: string_pool.add(&graph.file_path.to_string_lossy().replace('\\', "/")),
            start_row: spot.span.0,
            start_col: spot.span.1,
            end_row: spot.span.2,
            end_col: spot.span.3,
            hint: string_pool.add(&format!(
                "{}; routes this controller handles carry none of its middleware",
                spot.hint
            )),
            is_test: matches!(files[spot.file_idx as usize].category, FileCategory::Test),
        });
    }
    if entries.is_empty() {
        return edges;
    }
    let marked: FxHashSet<u32> = entry_edges.iter().map(|edge| edge.target).collect();
    let kind = crate::entry_points::EntryKind::HttpRoute;
    for (handler, score, routes) in entries {
        if marked.contains(&handler) {
            continue;
        }
        let file_idx = nodes[handler as usize].file_idx;
        let graph = &local_graphs[file_idx as usize];
        let raw = &graph.nodes[(handler - file_base(raw_map, file_idx)) as usize];
        let path_str = graph.file_path.to_string_lossy().replace('\\', "/");
        let mut reason = routes
            .iter()
            .take(MAX_ENTRY_REASONS)
            .map(|route| format!("{}:{route}", kind.tag()))
            .collect::<Vec<_>>()
            .join("; also: ");
        if routes.len() > MAX_ENTRY_REASONS {
            reason.push_str(&format!(
                "; also: {} more routes",
                routes.len() - MAX_ENTRY_REASONS
            ));
        }
        // The owner keeps two classes' same-named actions in one file apart.
        let entry_uid_name = format!("{}:{}", kind.tag(), raw.name);
        let entry_idx = nodes.len() as u32;
        nodes.push(Node {
            uid: ecp_core::uid::compute(
                NodeKind::EntryPoint,
                &path_str,
                raw.owner_class.as_deref(),
                &entry_uid_name,
            ),
            name: string_pool.add(&format!("{}@{}", kind.tag(), raw.name)),
            file_idx,
            kind: NodeKind::EntryPoint,
            span: (0, 0, 0, 0),
            community_id: 0,
            owner_class: StrRef::default(),
            content_hash: 0,
        });
        edges.push(Edge {
            source: entry_idx,
            target: handler,
            rel_type: RelType::References,
            confidence: score,
            reason: string_pool.add(&format!("{}:{score:.2}:{reason}", kind.tag())),
        });
    }
    edges
}

/// Pass 1.6: Fetch-shape extraction.
///
/// Two sub-passes — both gated on `repo_root` being available (it
/// is the only way to resolve `LocalGraph.file_path` back to an
/// absolute path so we can re-read the source). Test harnesses that
/// don't set a repo root simply opt out of fetch-shape data.
///
/// 1.6a — for each Route node, run `response_shapes::extract` on
///        its handler file and stash a `RouteShape` if it produced
///        any keys (sparse — empty payloads do not appear).
/// 1.6b — build a path→route_idx map, then for every non-handler
///        file in `local_graphs` scan for `fetch(url) /
///        axios.get(url)` literals and emit `RelType::Fetches`
///        edges (file_node → route_idx) with the
///        `format_reason(keys, fetch_count)` reason payload. The edges go
///        out as `file_edges`: the call sites are text with no position
///        (`fetch_urls::extract`), so the edge starts at the consumer's
///        `File` node (C4 Open point 4, option a), also in a file with no
///        declaration.
///
/// Assumption (MVP): exact path match — `fetch('/users')` only
/// hits a route whose path is exactly `/users`. Dynamic-segment
/// normalisation (upstream `normalizeFetchURL` + `routeMatches`)
/// is intentionally NOT ported here; it can land later without
/// touching the wire format.
fn pass1_6_fetch_shapes(
    local_graphs: &[LocalGraph],
    repo_root: Option<&std::path::Path>,
    emitted_routes: &[EmittedRoute],
    nodes: &[Node],
    string_pool: &mut StringPool,
) -> (Vec<RouteShape>, Vec<(FileOrd, Edge)>) {
    let mut route_shapes_out: Vec<RouteShape> = Vec::new();
    let mut fetches_edges: Vec<(FileOrd, Edge)> = Vec::new();

    if let Some(repo_root) = repo_root {
        // Per-file content cache — multiple routes may share a handler file
        // (Express-style `router.get('/a')`/`router.get('/b')` in one file),
        // and consumer files re-read once even if they fetch many URLs.
        let mut content_cache: FxHashMap<u32, Option<String>> = FxHashMap::default();
        let read_content = |file_idx: u32,
                            cache: &mut FxHashMap<u32, Option<String>>,
                            local_graphs: &[LocalGraph]|
         -> Option<String> {
            if let Some(slot) = cache.get(&file_idx) {
                return slot.clone();
            }
            let lg = &local_graphs[file_idx as usize];
            let abs = repo_root.join(&lg.file_path);
            let content = std::fs::read_to_string(&abs).ok();
            cache.insert(file_idx, content.clone());
            content
        };

        // 1.6a — RouteShape per Route.
        for (route_idx, file_idx, _route_method, _route_path) in emitted_routes {
            let lg = &local_graphs[*file_idx as usize];
            let Some(lang) = lang_for_path(&lg.file_path.to_string_lossy()) else {
                continue;
            };
            let Some(content) = read_content(*file_idx, &mut content_cache, local_graphs) else {
                continue;
            };
            let shape = response_shapes::extract(&content, lang);
            if shape.response_keys.is_empty() && shape.error_keys.is_empty() {
                continue;
            }
            let response_keys: Vec<StrRef> = shape
                .response_keys
                .iter()
                .map(|k| string_pool.add(k))
                .collect();
            let error_keys: Vec<StrRef> = shape
                .error_keys
                .iter()
                .map(|k| string_pool.add(k))
                .collect();
            route_shapes_out.push(RouteShape {
                node_idx: *route_idx,
                response_keys,
                error_keys,
            });
        }

        // 1.6b — Fetches edges.
        //
        // RouteIndex: (UPPERCASED_METHOD, normalized_path) → Vec<route_idx>.
        // `normalize_route_path` coalesces `:param` / `{param}` / `<param>`
        // to `:*` so `app.get('/api/users/:id')` and
        // `fetch('/api/users/42')` both resolve to `("GET", "api/users/:*")`
        // and produce a Fetches edge.
        // Each route keeps its path as registered: Laravel's overwrite rule
        // compares URIs, which the normalized key no longer tells apart.
        let mut route_index: FxHashMap<(String, String), Vec<(u32, &str)>> = FxHashMap::default();
        for (route_idx, _file_idx, route_method, route_path) in emitted_routes {
            let (norm_path, _) = normalize_route_path(route_path);
            route_index
                .entry((route_method.to_uppercase(), norm_path))
                .or_default()
                .push((*route_idx, route_path.as_str()));
        }

        // Files that themselves emit a Route are handlers, not
        // consumers — skip them on the consumer pass to avoid
        // self-loops where a file `fetch()`es its own route.
        let handler_files: rustc_hash::FxHashSet<u32> =
            emitted_routes.iter().map(|(_, fi, _, _)| *fi).collect();

        // Parallel phase: pure read-only work per file.
        // Each worker reads the file directly (no shared cache) and
        // returns (file_idx, matched, reason_str).
        // string_pool.add and Edge construction happen serially below.
        #[allow(clippy::type_complexity)]
        let parallel_results: Vec<(u32, Vec<(u32, bool)>, String)> = local_graphs
            .par_iter()
            .enumerate()
            .filter_map(|(file_idx_usize, lg)| {
                let file_idx = file_idx_usize as u32;
                if handler_files.contains(&file_idx) {
                    return None;
                }
                let path_str = lg.file_path.to_string_lossy();
                // Allowlist: extensions where `fetch_urls::extract` produces
                // useful hits. PHP is a route-server language, not a consumer
                // here; all other unknown extensions are skipped cheaply.
                if !is_consumer_extension(&path_str) {
                    return None;
                }
                // Read directly — the cache is serial-only (1.6a) and each
                // consumer file is visited at most once in this subpass.
                let abs = repo_root.join(&lg.file_path);
                let content = std::fs::read_to_string(&abs).ok()?;
                let client_calls = fetch_urls::extract(&content);
                if client_calls.is_empty() {
                    return None;
                }
                let keys = consumer_keys::extract(&content);

                // Resolve each (method, url) pair against the RouteIndex.
                // Cross-repo misses (no matching Route node) are silently
                // skipped — ecp contracts handles those separately.
                let mut matched: Vec<(u32, bool)> = Vec::new(); // (route_idx, is_templated)
                for (client_method, client_url) in &client_calls {
                    let (norm_url, client_templated) = normalize_route_path(client_url);
                    // Route side also normalized via `:*`; the index key carries
                    // `:*` segments whenever the route had a param placeholder.
                    let route_templated = norm_url.contains(":*");
                    // A Laravel `ANY` route answers each verb `Route::any` registers.
                    let any = if client_method != "ANY"
                        && crate::route_detector::any_route_answers(client_method)
                    {
                        route_index.get(&("ANY".to_string(), norm_url.clone()))
                    } else {
                        None
                    };
                    let exact = route_index.get(&(client_method.clone(), norm_url));
                    for route_idx in answering_routes(
                        exact.map_or(&[][..], Vec::as_slice),
                        any.map_or(&[][..], Vec::as_slice),
                        nodes,
                    ) {
                        matched.push((route_idx, client_templated || route_templated));
                    }
                }
                if matched.is_empty() {
                    return None;
                }
                let fetch_count = matched.len() as u32;
                let reason_str = format_reason(&keys, fetch_count);
                Some((file_idx, matched, reason_str))
            })
            .collect();

        // Serial phase: intern strings and push edges in file_idx order.
        // rayon's `collect::<Vec<_>>()` preserves source order even though
        // `filter_map` drops the IndexedParallelIterator bound — so
        // parallel_results stays in file_idx order, identical to the old
        // serial loop's edge order; no extra sort needed.
        for (file_idx, matched, reason_str) in parallel_results {
            let reason_ref = string_pool.add(&reason_str);
            for (route_idx, is_templated) in matched {
                // Confidence: 0.8 for exact-path matches, 0.6 for any
                // templated segment — caller or callee side.
                let confidence = if is_templated { 0.6 } else { 0.8 };
                fetches_edges.push((
                    FileOrd(file_idx),
                    Edge {
                        source: FILE_SOURCE_PENDING,
                        target: route_idx,
                        rel_type: RelType::Fetches,
                        confidence,
                        reason: reason_ref,
                    },
                ));
            }
        }
    }
    (route_shapes_out, fetches_edges)
}

/// The routes that answer a request when `exact` routes of its verb and
/// `any` Laravel `ANY` routes share its normalized path, each with its
/// path as registered. Laravel's `RouteCollection` files an `ANY` route
/// under every verb, keyed by URI, so within one route file the route
/// registered last replaces the others of the same URI for that verb;
/// `/items/{id}` and `/items/{slug}` are two URIs and both answer. The
/// order of registrations across files is not modelled, so every route
/// of another file stays.
fn answering_routes<'p>(
    exact: &[(u32, &'p str)],
    any: &[(u32, &'p str)],
    nodes: &[Node],
) -> Vec<u32> {
    if any.is_empty() {
        return exact.iter().map(|&(route, _)| route).collect();
    }
    // Laravel trims the slashes around a route's URI.
    let key = |(route, path): (u32, &'p str)| -> (u32, &'p str) {
        (nodes[route as usize].file_idx, path.trim_matches('/'))
    };
    let start = |route: u32| {
        let span = nodes[route as usize].span;
        (span.0, span.1)
    };
    let any_uris: FxHashSet<(u32, &str)> = any.iter().map(|&r| key(r)).collect();
    let mut answering: Vec<u32> = Vec::new();
    // (Route file, URI) → the route it registers last, in first-seen order.
    let mut last: Vec<u32> = Vec::new();
    let mut slot: FxHashMap<(u32, &str), usize> = FxHashMap::default();
    for &(route, path) in exact.iter().chain(any) {
        let uri = key((route, path));
        if !any_uris.contains(&uri) {
            answering.push(route);
            continue;
        }
        match slot.get(&uri) {
            Some(&i) if start(route) >= start(last[i]) => last[i] = route,
            Some(_) => {}
            None => {
                slot.insert(uri, last.len());
                last.push(route);
            }
        }
    }
    answering.extend(last);
    answering
}

/// Pass 1.7: Entry-point scoring (cross-language).
///
/// Pure consumer of `RawRoute` + `RawFrameworkRef` + `main()`
/// detection — see `crate::entry_points` for the scoring matrix.
/// Closes the ⚠️ Entry column for Java / Kotlin / C# / Go / Rust /
/// Swift / C / C++ / Dart in the README Language Matrix.
///
/// Emits one `NodeKind::EntryPoint` marker node per scored entry
/// point and a `References` edge from the marker to the underlying
/// handler. The scorer keeps only the handler's name, so the handler is
/// the one node of the file so named that no same-named node encloses
/// ([`outermost_named_slot`]); several give no marker and an
/// [`BS_KIND_ENTRY_POINT_AMBIGUOUS`] BlindSpot. The edge's `reason`
/// carries the scoring provenance so downstream LLM tooling can render
/// "this is an HTTP route handler at confidence 1.0" without re-running
/// the scorer.
fn pass1_7_entry_points(
    local_graphs: &[LocalGraph],
    files: &[File],
    raw_map: &RawIndexMap,
    string_pool: &mut StringPool,
    nodes: &mut Vec<Node>,
) -> (Vec<Edge>, Vec<BlindSpotRecord>) {
    let mut entry_edges: Vec<Edge> = Vec::new();
    let mut blind_spots: Vec<BlindSpotRecord> = Vec::new();
    for (file_idx, local_graph) in local_graphs.iter().enumerate() {
        let file_idx = file_idx as u32;
        let path_str = local_graph.file_path.to_string_lossy().replace('\\', "/");
        let entries = crate::entry_points::score_entry_points(
            &local_graph.routes,
            &local_graph.framework_refs,
            &local_graph.nodes,
        );
        for ep in entries {
            let handler_idx = match outermost_named_slot(
                &local_graph.nodes,
                &ep.uid,
                raw_map,
                FileOrd(file_idx),
            ) {
                Ok(slot) => slot,
                // Handler not found in this file — happens when a
                // RawRoute's handler name is a string literal that
                // doesn't match any parsed symbol (e.g. an external
                // reference). The EntryPoint without a target would be
                // a dangling marker.
                Err(0) => continue,
                Err(candidates) => {
                    let first = local_graph
                        .nodes
                        .iter()
                        .find(|n| n.name == ep.uid)
                        .map_or((0, 0, 0, 0), |n| n.span);
                    blind_spots.push(BlindSpotRecord {
                        kind: string_pool.add(BS_KIND_ENTRY_POINT_AMBIGUOUS),
                        file_path: string_pool.add(&path_str),
                        start_row: first.0,
                        start_col: first.1,
                        end_row: first.2,
                        end_col: first.3,
                        hint: string_pool.add(&format!(
                            "{candidates} declarations are named `{}`; the {} entry point binds none of them",
                            ep.uid,
                            ep.kind.tag()
                        )),
                        is_test: matches!(files[file_idx as usize].category, FileCategory::Test),
                    });
                    continue;
                }
            };
            let entry_name = format!("{}@{}", ep.kind.tag(), ep.uid);
            // EntryPoint UID encodes kind-tag + handler-uid in the name segment
            // so two entry points for the same handler (route + main) don't collide.
            let entry_uid_name = format!("{}:{}", ep.kind.tag(), ep.uid);
            let entry_idx = nodes.len() as u32;
            nodes.push(Node {
                uid: ecp_core::uid::compute(NodeKind::EntryPoint, &path_str, None, &entry_uid_name),
                name: string_pool.add(&entry_name),
                file_idx,
                kind: NodeKind::EntryPoint,
                span: (0, 0, 0, 0),
                community_id: 0,
                owner_class: StrRef::default(),
                content_hash: 0,
            });

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
    (entry_edges, blind_spots)
}

/// BlindSpot kind: an entry point's handler name matches several
/// declarations of its file, so no marker is emitted.
pub const BS_KIND_ENTRY_POINT_AMBIGUOUS: &str = "entry-point-ambiguous";

/// The slot of the one node of `nodes` named `name` that no other node so
/// named encloses: a class, not its own same-named constructor. Between two
/// identical spans the lower index stays, as in `innermost_enclosing`.
/// Only a declaration a handler can be counts: Go's `package main` is no
/// rival to `func main`.
/// `Err(n)` when `n` nodes remain, `Err(0)` for an absent or blank name.
/// Never the first of several (C4 `bind_name`).
fn outermost_named_slot(
    nodes: &[RawNode],
    name: &str,
    raw_map: &RawIndexMap,
    file: FileOrd,
) -> Result<u32, u32> {
    if name.trim().is_empty() {
        return Err(0);
    }
    let named: Vec<usize> = (0..nodes.len())
        .filter(|&i| nodes[i].name == name && can_handle(nodes[i].kind))
        .collect();
    let mut slots: Vec<u32> = Vec::new();
    for &i in &named {
        let span = nodes[i].span;
        let enclosed = named.iter().any(|&j| {
            j != i
                && ecp_core::analyzer::types::span_contains(nodes[j].span, span)
                && (nodes[j].span != span || j < i)
        });
        if enclosed {
            continue;
        }
        // A tombstoned twin maps to its survivor (C4 Open point 2).
        if let Some(slot) = raw_map.node(file, RawIdx(i as u32)) {
            if !slots.contains(&slot) {
                slots.push(slot);
            }
        }
    }
    match slots.as_slice() {
        [only] => Ok(*only),
        several => Err(several.len() as u32),
    }
}

/// Whether a node of `kind` can be the handler of an entry point or the
/// source of a framework ref: a declaration, not a namespace, an import or
/// a synthetic marker.
fn can_handle(kind: NodeKind) -> bool {
    !matches!(
        kind,
        NodeKind::File
            | NodeKind::Namespace
            | NodeKind::Module
            | NodeKind::Import
            | NodeKind::Document
            | NodeKind::Section
            | NodeKind::Route
            | NodeKind::Process
            | NodeKind::EntryPoint
            | NodeKind::Annotation
            | NodeKind::EventTopic
            | NodeKind::TransactionScope
            | NodeKind::SchemaField
            | NodeKind::PathLiteral
    )
}

/// Pass 1.8: FunctionMeta collection.
///
/// For each LocalGraph that has populated `raw_function_metas`, pair each
/// entry with the corresponding graph node by span, then intern the strings
/// into the pool and produce a `FunctionMeta`.
///
/// Returned unsorted; `GraphAssembly::finish` sorts by `node_idx` — which is
/// what `ZeroCopyGraph::function_meta()`'s binary search needs — and derives
/// the dense `node_flags` mirror from the result.
/// Also records each function's return type in `symbol_table`, where Pass 2
/// types the receiver of a chained call (`make().bar()`) by it.
fn pass1_8_function_metas(
    local_graphs: &[LocalGraph],
    raw_map: &RawIndexMap,
    string_pool: &mut StringPool,
    symbol_table: &mut SymbolTable,
) -> Vec<FunctionMeta> {
    let mut function_metas: Vec<FunctionMeta> = Vec::new();
    {
        for (file_idx, local_graph) in local_graphs.iter().enumerate() {
            if !local_graph.raw_function_metas.is_empty() {
                let base = file_base(raw_map, file_idx as u32);
                // Sorted index → binary_search per node, so the inner loop
                // is O(N log F) instead of O(N*F).
                let mut meta_idx: Vec<(Span, usize)> = local_graph
                    .raw_function_metas
                    .iter()
                    .enumerate()
                    .map(|(i, m)| (m.span, i))
                    .collect();
                meta_idx.sort_by_key(|(s, _)| *s);
                for (raw_idx, raw_node) in local_graph.nodes.iter().enumerate() {
                    if !matches!(
                        raw_node.kind,
                        NodeKind::Function | NodeKind::Method | NodeKind::Constructor
                    ) {
                        continue;
                    }
                    let node_idx = base + raw_idx as u32;
                    let Ok(slot) = meta_idx.binary_search_by_key(&raw_node.span, |(s, _)| *s)
                    else {
                        continue;
                    };
                    let rfm = &local_graph.raw_function_metas[meta_idx[slot].1];
                    let params: Vec<StrRef> =
                        rfm.params.iter().map(|s| string_pool.add(s)).collect();
                    if !rfm.return_type.is_empty() {
                        symbol_table.note_return_type(node_idx, &rfm.return_type);
                    }
                    let return_type = string_pool.add(&rfm.return_type);
                    let decorators: Vec<StrRef> =
                        rfm.decorators.iter().map(|s| string_pool.add(s)).collect();
                    function_metas.push(FunctionMeta {
                        node_idx,
                        flags: rfm.flags,
                        params,
                        return_type,
                        decorators,
                    });
                }
            }
        }
    }
    function_metas
}

fn callable_parents(graph: &LocalGraph) -> (Vec<(usize, &RawNode)>, Vec<Option<usize>>) {
    let functions: Vec<_> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            matches!(
                node.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Constructor
            )
        })
        .collect();
    let spans: Vec<_> = functions.iter().map(|(_, node)| node.span).collect();
    let parents = ecp_core::analyzer::types::innermost_enclosing(&spans);
    (functions, parents)
}

fn is_anonymous_closure(node: &RawNode) -> bool {
    node.kind == NodeKind::Function && node.name.starts_with("<anonymous:")
}

/// `References` from the first of each overload set to every later one. A
/// call binds the first by name: the argument types that pick an overload
/// are not parsed. These edges let upstream impact of a later overload reach
/// the set's callers through the first one, at k - 1 edges per set instead
/// of one per call site and overload.
fn emit_overload_references(
    symbol_table: &SymbolTable,
    string_pool: &mut StringPool,
    edges: &mut Vec<Edge>,
) {
    let reason = string_pool.add(ecp_core::analyzer::types::OVERLOAD_SET_REASON);
    for (first, later) in symbol_table.overload_sets() {
        edges.extend(later.iter().map(|&target| Edge {
            source: first,
            target,
            rel_type: RelType::References,
            confidence: 1.0,
            reason,
        }));
    }
}

fn emit_closure_references(
    local_graphs: &[LocalGraph],
    raw_map: &RawIndexMap,
    nodes: &[Node],
    string_pool: &mut StringPool,
    edges: &mut Vec<Edge>,
) {
    let reason = string_pool.add(ecp_core::analyzer::types::CLOSURE_REFERENCE_REASON);
    for (file_idx, graph) in local_graphs.iter().enumerate() {
        let start_index = file_base(raw_map, file_idx as u32);
        emit_file_closure_references(graph, start_index, nodes, reason, edges);
    }
}

fn emit_file_closure_references(
    graph: &LocalGraph,
    start_index: u32,
    nodes: &[Node],
    reason: StrRef,
    edges: &mut Vec<Edge>,
) {
    if !graph.nodes.iter().any(is_anonymous_closure) {
        return;
    }
    let (functions, parents) = callable_parents(graph);
    let mut live_by_uid: Option<FxHashMap<u64, u32>> = None;
    for (i, &(offset, node)) in functions.iter().enumerate() {
        if !is_anonymous_closure(node) {
            continue;
        }
        let Some(parent) = parents[i] else {
            continue;
        };
        let mut source = start_index + functions[parent].0 as u32;
        let target = start_index + offset as u32;
        if nodes[target as usize].name.len == 0 {
            continue;
        }
        // A redefined function (an `#ifdef` twin) shares the first one's
        // identity; its tombstone clears the name and inverts the
        // survivor's UID. Preserve both callbacks.
        if nodes[source as usize].name.len == 0 {
            let live_by_uid = live_by_uid.get_or_insert_with(|| {
                nodes[start_index as usize..start_index as usize + graph.nodes.len()]
                    .iter()
                    .enumerate()
                    .filter(|(_, node)| node.name.len != 0)
                    .map(|(offset, node)| (node.uid, start_index + offset as u32))
                    .collect()
            });
            let Some(&live_source) = live_by_uid.get(&!nodes[source as usize].uid) else {
                continue;
            };
            source = live_source;
        }
        // Creating a callback references its body without proving execution.
        // References preserves upstream impact without inventing a direct call.
        edges.push(Edge {
            source,
            target,
            rel_type: RelType::References,
            confidence: 1.0,
            reason,
        });
    }
}

/// The file's [`LexicalFunctions`]; a file without calls needs none.
fn lexical_functions(graph: &LocalGraph, start_index: u32) -> LexicalFunctions {
    if graph.nodes.iter().all(|node| node.calls.is_empty()) {
        return LexicalFunctions::default();
    }
    LexicalFunctions::new(
        &graph.file_path,
        graph.nodes.iter().map(|node| {
            (
                node.kind,
                node.name.as_str(),
                node.owner_class.is_some(),
                node.span,
            )
        }),
        start_index,
    )
}

/// Pass 1.9: declared supertypes, as `(type_id, base)` rows in declared
/// order, each base with every project type it resolved to and the
/// resolver's confidence (none = the base resolved to no project type), for
/// [`SymbolTable::build_declared_bases`].
///
/// Each base goes through the same [`resolve_base`] call Pass 2 makes for
/// `Extends` / `Implements`, with the same path aliases
/// and module tree, so a resolved supertype here is exactly a heritage edge
/// Pass 2 emits. The resolver has no dump enabled, so the decision dump is
/// unchanged.
///
/// The subject is the type the heritage belongs to:
/// - a type-kind node: itself;
/// - an `Impl` node: the type it names;
/// - a Rust trait-impl method (`impl Trait for O`, `rust/queries.scm`): its
///   `owner_class` `O`. Rust records heritage on the methods, not the struct.
///
/// The `Impl` and Rust subjects resolve as a type from the declaring file;
/// an ambiguous or missing type drops that heritage rather than guess.
fn pass1_9_supertypes(
    local_graphs: &[LocalGraph],
    symbol_table: &SymbolTable,
    path_aliases: &PathAliases,
    mod_tree_ctx: Option<(&RustWorkspaceModTree, &std::path::Path)>,
    reexports: &ReexportIndex<'_>,
    raw_map: &RawIndexMap,
) -> Vec<(u32, DeclaredBase)> {
    let per_graph: Vec<Vec<(u32, DeclaredBase)>> = local_graphs
        .par_iter()
        .enumerate()
        .map(|(file_idx, lg)| {
            let start = file_base(raw_map, file_idx as u32);
            if lg.nodes.iter().all(|n| n.heritage.is_empty()) {
                return Vec::new();
            }
            let mut resolver = Resolver::new(symbol_table)
                .with_path_aliases(path_aliases.clone())
                .with_reexports(reexports);
            if let Some((mt, root)) = mod_tree_ctx {
                resolver = resolver.with_mod_tree(mt, root.to_path_buf());
            }
            let resolve_type = |name: &str| {
                resolver.resolve_symbol(&lg.file_path, name, &lg.imports, ResolveTarget::Type)
            };
            let language = Language::from_path(&lg.file_path.to_string_lossy());
            let is_rust = language == Language::Rust;
            // One `impl Trait for O` block repeats its heritage on every
            // method; the cache keeps that to one resolve per name per file.
            let mut bases_cache: FxHashMap<&str, Box<[(u32, f32)]>> = FxHashMap::default();
            let mut subject_cache: FxHashMap<&str, Option<u32>> = FxHashMap::default();
            let mut out = Vec::new();
            for (offset, raw) in lg.nodes.iter().enumerate() {
                if raw.heritage.is_empty() {
                    continue;
                }
                let node_id = start + offset as u32;
                let subject = if raw.kind.is_type() {
                    // A UID-collision tombstone holds the id but no name.
                    symbol_table.file_of(node_id).map(|_| node_id)
                } else {
                    let type_name = match raw.kind {
                        NodeKind::Impl => Some(raw.name.as_str()),
                        _ if is_rust && raw.kind.is_callable() => raw.owner_class.as_deref(),
                        _ => None,
                    };
                    let Some(type_name) = type_name.filter(|n| !n.is_empty()) else {
                        continue;
                    };
                    *subject_cache.entry(type_name).or_insert_with(|| {
                        match resolve_type(type_name).as_slice() {
                            [(id, _)] => Some(*id),
                            _ => None,
                        }
                    })
                };
                let Some(subject) = subject else {
                    continue;
                };
                for base in &raw.heritage {
                    let candidates = bases_cache.entry(base.as_str()).or_insert_with(|| {
                        resolve_base(&resolver, symbol_table, lg, language, base).into_boxed_slice()
                    });
                    out.push((
                        subject,
                        DeclaredBase {
                            spelling: base.as_str().into(),
                            candidates: candidates.clone(),
                        },
                    ));
                }
            }
            out
        })
        .collect();
    per_graph.into_iter().flatten().collect()
}

/// The types the declared base `spelling` of a type in `local_graph` names,
/// with resolver confidence: what Pass 1.9 records and Pass 2 emits as
/// `Extends` / `Implements`. Outside C# it is the resolver's answer as is.
///
/// A C# spelling names a type by its bare name and its type-argument count
/// ([`csharp_base_arity`]): `Conv` is `Conv` of arity 0, `Conv<Version>` is
/// `Conv` of arity 1. The bare name resolves through the ordinary tiers, and
/// each candidate becomes the type of that arity
/// ([`SymbolTable::with_arity`]). The arity keeps one candidate at most per
/// declaring file, so candidates in two files stay ambiguous.
fn resolve_base(
    resolver: &Resolver<'_>,
    symbol_table: &SymbolTable,
    local_graph: &LocalGraph,
    language: Language,
    spelling: &str,
) -> Vec<(u32, f32)> {
    let resolve = |name: &str| {
        resolver.resolve_symbol(
            &local_graph.file_path,
            name,
            &local_graph.imports,
            ResolveTarget::Type,
        )
    };
    if language != Language::CSharp {
        return resolve(spelling);
    }
    let (name, arity) = csharp_base_arity(spelling);
    let mut targets = resolve(name);
    targets.retain_mut(|(id, _)| match symbol_table.with_arity(*id, arity) {
        Some(typed) => {
            *id = typed;
            true
        }
        None => false,
    });
    targets
}

/// A C# base spelling as the bare type it names and the number of type
/// arguments of its last segment: `Conv<int, string>` is (`Conv`, 2),
/// `N.Conv<List<int>>` is (`N.Conv`, 1), `Conv` is (`Conv`, 0). A spelling
/// whose argument list belongs to an outer segment (`Outer<int>.Inner`) or
/// does not close stays whole, with arity 0.
fn csharp_base_arity(spelling: &str) -> (&str, u32) {
    let whole = (spelling, 0);
    let Some(open) = spelling.find('<') else {
        return whole;
    };
    let Some(args) = spelling
        .trim_end()
        .strip_suffix('>')
        .and_then(|s| s.get(open + 1..))
    else {
        return whole;
    };
    let mut depth = 0u32;
    let mut arity = 1;
    for c in args.chars() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' if depth == 0 => return whole,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => arity += 1,
            _ => {}
        }
    }
    (spelling[..open].trim_end(), arity)
}

/// The `reason` of a resolved `Calls` edge, by the tier that resolved it.
#[derive(Clone, Copy)]
struct CallReasons {
    call: StrRef,
    nearest_copy: StrRef,
}

impl CallReasons {
    fn of(self, confidence: f32) -> StrRef {
        if confidence == NEAREST_COPY_CONFIDENCE {
            self.nearest_copy
        } else {
            self.call
        }
    }
}

/// The `reason` of an enum-variant `References` edge. Data-carrying variants
/// (Rust, Swift) change shape for producers and destructuring matches in
/// different ways, and "who sets state X" differs from "who handles state
/// X", so a pattern or case label keeps its own reason.
#[derive(Clone, Copy)]
struct VariantReasons {
    used: StrRef,
    matched: StrRef,
}

/// The `EnumVariant` a [`VariantRef`] names, with the edge confidence.
///
/// A qualified reference resolves its qualifier as a type through the
/// ordinary tiers, then takes that type's variant, so `A` of two enums never
/// mixes. Rust `Self::A` names the caller's own impl type. A bare name (C
/// enumerators, Java case labels) binds a variant declared in the file, else
/// the one project-wide variant of that name.
fn resolve_variant_ref(
    resolver: &Resolver<'_>,
    symbol_table: &SymbolTable,
    local_graph: &LocalGraph,
    raw_node: &RawNode,
    variant_ref: VariantRef<'_>,
) -> Option<(u32, f32)> {
    let split = variant_ref.split();
    if !symbol_table.has_enum_variant(split.map_or(variant_ref.path, |(_, variant)| variant)) {
        return None;
    }
    let file = local_graph.file_path.to_string_lossy().replace('\\', "/");
    let Some((qualifier, variant)) = split else {
        return symbol_table
            .bare_enum_variant(
                &file,
                variant_ref.path,
                FileMeta::from_normalized_path(&file),
            )
            .map(|(id, same_file)| {
                let tier = if same_file {
                    ResolutionTier::SameFile
                } else {
                    ResolutionTier::Global
                };
                (id, tier.base_confidence())
            });
    };
    let qualifier = match (Language::from_normalized_path(&file), qualifier) {
        (Language::Rust, "Self") => raw_node.owner_class.as_deref().and_then(owner_key)?,
        _ => qualifier,
    };
    let types = resolver.resolve_symbol(
        &local_graph.file_path,
        qualifier,
        &local_graph.imports,
        ResolveTarget::Type,
    );
    let &[(type_id, confidence)] = types.as_slice() else {
        return None;
    };
    symbol_table
        .enum_variant_of(type_id, qualifier, variant)
        .map(|id| (id, confidence))
}

/// The reasons of the edges a [`NameRef`] yields: `ReadsField` to a
/// constant or variable, `References` to a function.
#[derive(Clone, Copy)]
struct NameRefReasons {
    value: StrRef,
    function: StrRef,
}

/// The declaration a [`NameRef`] names, with the edge confidence.
///
/// In a language where a bare name reaches a member of the enclosing type
/// (Java `TABLE`, C# method group `Print`), the reader's own type is tried
/// first, then the types it inherits from. A bare function name in PHP,
/// Python, Rust or C never names a method (`array_map('fmt', $xs)`), so
/// those skip the type. Then the ordinary tiers: same file, import, and the
/// unique project-wide declaration ([`ResolutionTier::Global`]). The global tier is
/// dropped where a module must import a name before it can use it bare, as
/// there it can only stand for a local the parser did not see. An
/// ambiguous name binds nothing.
fn resolve_name_ref(
    resolver: &Resolver<'_>,
    symbol_table: &SymbolTable,
    local_graph: &LocalGraph,
    (file, language): (&str, Language),
    raw_node: &RawNode,
    name_ref: NameRef<'_>,
) -> Option<(u32, f32)> {
    ecp_core::analyzer::name_ref::resolve_name_ref(
        symbol_table,
        file,
        language,
        &local_graph.imports,
        raw_node.owner_class.as_deref(),
        name_ref,
        ecp_core::analyzer::name_ref::NameRefTiers {
            same_file: ResolutionTier::SameFile.base_confidence(),
            heritage: ResolutionTier::HeritageScoped.base_confidence(),
            global: ResolutionTier::Global.base_confidence(),
        },
        |name| symbol_table.global_match_count(name) > 0,
        |name, target| {
            resolver.resolve_symbol(&local_graph.file_path, name, &local_graph.imports, target)
        },
        |name, target| {
            resolver.resolve_imported_symbol(
                &local_graph.file_path,
                name,
                &local_graph.imports,
                target,
            )
        },
    )
}

/// Emit Pass-2 edges for a single `raw_node`'s heritage / calls / type
/// annotation. Factored out so the serial dump path and the parallel
/// hot path can share the same per-node logic.
///
/// `indirect_lookup` maps `(caller_span, call_index)` to
/// `(flags, dispatch_type_string)` for calls that are non-direct. When a Calls
/// edge is emitted from such a call site, a corresponding entry is pushed to
/// `pending_call_metas` using the pre-sort edge index. The caller is responsible
/// for converting pre-sort indices to final sorted indices before placing
/// entries in `ZeroCopyGraph.call_metas`.
#[allow(clippy::too_many_arguments)]
fn pass2_emit_node_edges(
    resolver: &Resolver<'_>,
    local_graph: &LocalGraph,
    raw_node: &RawNode,
    current_node_idx: u32,
    reason_heritage: StrRef,
    reason_implements: StrRef,
    reason_type: StrRef,
    reason_call: CallReasons,
    reason_reads_field: StrRef,
    reason_variant: VariantReasons,
    reason_name_ref: NameRefReasons,
    symbol_table: &SymbolTable,
    edges: &mut Vec<Edge>,
    indirect_lookup: &FxHashMap<CallMetaKey, (u8, String)>,
    lexical_lookup: &LexicalFunctions,
    pending_call_metas: &mut Vec<(usize, u8, String)>,
) {
    let language = Language::from_path(&local_graph.file_path.to_string_lossy());
    for base in &raw_node.heritage {
        let targets = resolve_base(resolver, symbol_table, local_graph, language, base);
        for (target_id, confidence) in targets {
            // Kind-based dispatch: Interface/Trait targets → Implements,
            // concrete class targets → Extends. The resolved target's NodeKind
            // IS the relationship definition — no per-language parser change
            // needed (Java `implements`, Kotlin `:`, Go structural, Rust
            // `impl For` all converge on the same heritage list).
            let (rel_type, reason) = match symbol_table.node_kind(target_id) {
                NodeKind::Interface | NodeKind::Trait => (RelType::Implements, reason_implements),
                _ => (RelType::Extends, reason_heritage),
            };
            edges.push(Edge {
                source: current_node_idx,
                target: target_id,
                rel_type,
                confidence,
                reason,
            });
        }
    }

    let call_heritage =
        ecp_core::analyzer::heritage::enclosing_class_heritage(raw_node, &local_graph.nodes);
    for (call_idx, raw_callee) in raw_node.calls.iter().enumerate() {
        let lookup_key = CallMetaKey::new(raw_node.span, call_idx as u32);
        let meta = indirect_lookup.get(&lookup_key);
        let resolve = |site: CallSite<'_>| match lexical_lookup.targets(raw_node.span, site.name())
        {
            Some(targets) if targets.is_empty() => {
                resolver.resolve_imported_call(&local_graph.file_path, site, &local_graph.imports)
            }
            Some(targets) => targets,
            None => resolver.resolve_call(
                &local_graph.file_path,
                site,
                &local_graph.imports,
                call_heritage,
                raw_node.owner_class.as_deref(),
                Some(current_node_idx),
            ),
        };
        // `Foo.create().bar()`: `bar` on the type the one bound `create`
        // returns. Only an unknown return type permits the parser's fallback,
        // except Kotlin's existing top-level extension-function retry.
        let mut chain_failed = false;
        let targets = CallSite::parse(raw_callee).resolve_chain(resolve, |inner, member| {
            if chain_failed {
                return Some(Vec::new());
            }
            let &[inner] = inner.as_slice() else {
                return None;
            };
            match resolver.resolve_chained_member_outcome(&local_graph.file_path, inner, member) {
                type_ladder::ChainOutcome::Unknown => None,
                type_ladder::ChainOutcome::Bound(target) => Some(vec![target]),
                type_ladder::ChainOutcome::Conclusive => {
                    let targets = if language == Language::Kotlin {
                        resolver
                            .extension_fallback(
                                &local_graph.file_path,
                                member,
                                &local_graph.imports,
                                ResolveTarget::Callable,
                                call_heritage,
                            )
                            .into_iter()
                            .map(|(id, confidence)| (id, confidence.min(inner.1)))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    chain_failed = targets.is_empty();
                    Some(targets)
                }
            }
        });
        for (target_id, confidence) in targets {
            if target_id == current_node_idx {
                continue; // self-recursion edges are Louvain / process noise
            }
            let edge_pre_sort_idx = edges.len();
            edges.push(Edge {
                source: current_node_idx,
                target: target_id,
                rel_type: RelType::Calls,
                confidence,
                reason: reason_call.of(confidence),
            });
            if let Some((flags, dispatch_type)) = meta {
                pending_call_metas.push((edge_pre_sort_idx, *flags, dispatch_type.clone()));
            }
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

    // ReadsField: each field-read site resolves to a Property target. The
    // `ResolveTarget::Field` predicate (is_property) admits only Property
    // nodes, so locals / Variables never match — the read edge stays on the
    // cross-function field contract without reintroducing local fan-out.
    // A `NameRef` (a bare name the parser kept after dropping parameters and
    // locals) binds a constant or variable as ReadsField, a function as
    // References: one edge per target per body.
    let mut variant_edges: Vec<(u32, bool)> = Vec::new();
    let mut name_edges: Vec<u32> = Vec::new();
    let mut name_file: Option<(String, Language)> = None;
    for field_name in &raw_node.field_reads {
        if let Some(name_ref) = NameRef::parse(field_name) {
            let (file, language) = name_file.get_or_insert_with(|| {
                let file = local_graph.file_path.to_string_lossy().replace('\\', "/");
                let language = Language::from_normalized_path(&file);
                (file, language)
            });
            let Some((target_id, confidence)) = resolve_name_ref(
                resolver,
                symbol_table,
                local_graph,
                (file.as_str(), *language),
                raw_node,
                name_ref,
            ) else {
                continue;
            };
            if target_id == current_node_idx || name_edges.contains(&target_id) {
                continue;
            }
            name_edges.push(target_id);
            let (rel_type, reason) = if symbol_table.node_kind(target_id).is_callable() {
                (RelType::References, reason_name_ref.function)
            } else {
                (RelType::ReadsField, reason_name_ref.value)
            };
            edges.push(Edge {
                source: current_node_idx,
                target: target_id,
                rel_type,
                confidence,
                reason,
            });
            continue;
        }
        if let Some(variant_ref) = VariantRef::parse(field_name) {
            let Some((target_id, confidence)) =
                resolve_variant_ref(resolver, symbol_table, local_graph, raw_node, variant_ref)
            else {
                continue;
            };
            // `E::A` and `crate::m::E::A` in one body are one dependency.
            if target_id == current_node_idx
                || variant_edges.contains(&(target_id, variant_ref.pattern))
            {
                continue;
            }
            variant_edges.push((target_id, variant_ref.pattern));
            edges.push(Edge {
                source: current_node_idx,
                target: target_id,
                rel_type: RelType::References,
                confidence,
                reason: if variant_ref.pattern {
                    reason_variant.matched
                } else {
                    reason_variant.used
                },
            });
            continue;
        }
        let targets = resolver.resolve_symbol(
            &local_graph.file_path,
            field_name,
            &local_graph.imports,
            ResolveTarget::Field,
        );
        for (target_id, confidence) in targets {
            if target_id == current_node_idx {
                continue;
            }
            edges.push(Edge {
                source: current_node_idx,
                target: target_id,
                rel_type: RelType::ReadsField,
                confidence,
                reason: reason_reads_field,
            });
        }
    }
}

/// Emit Pass-2 framework-ref + fanout-ref edges for one `local_graph`.
/// Reason interning is pre-baked into `reason_cache` (see Pass 2 setup);
/// every entry that reaches this function is guaranteed to be in the map.
///
/// A framework ref binds its source by endpoint (C4
/// `RecordKind::FrameworkRef`): a module-level ref (`<module>` or no name)
/// binds the file, and its edges go to `file_edges` for the `File` node; a
/// named one binds [`framework_ref_source`].
#[allow(clippy::too_many_arguments)]
fn pass2_emit_framework_and_fanout(
    resolver: &Resolver<'_>,
    symbol_table: &SymbolTable,
    local_graph: &LocalGraph,
    raw_map: &RawIndexMap,
    file: FileOrd,
    reason_cache: &FxHashMap<String, StrRef>,
    edges: &mut Vec<Edge>,
    file_edges: &mut Vec<(FileOrd, Edge)>,
) {
    let file_path_lossy = local_graph.file_path.to_string_lossy().replace('\\', "/");
    let start_idx = file_base(raw_map, file.0);

    for fw_ref in &local_graph.framework_refs {
        let module_level = fw_ref.source_name.trim().is_empty()
            || fw_ref.source_name == crate::framework_helpers::MODULE_LEVEL_SOURCE;
        let source_id = if module_level {
            None
        } else {
            let Some(slot) = framework_ref_source(&local_graph.nodes, fw_ref, raw_map, file) else {
                continue;
            };
            Some(slot)
        };
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
            let edge = Edge {
                source: source_id.unwrap_or(FILE_SOURCE_PENDING),
                target: target_id,
                rel_type: RelType::References,
                confidence: fw_ref.confidence,
                reason: reason_ref,
            };
            match source_id {
                Some(_) => edges.push(edge),
                None => file_edges.push((file, edge)),
            }
        }
    }

    for fanout_ref in &local_graph.fanout_refs {
        // A name lookup takes the file's first node so named, which can be a
        // property or a same-named method of another class; the callable whose
        // span holds the call site is the one the parser named.
        let (row, col, _, _) = fanout_ref.span;
        let source_id =
            crate::framework_helpers::enclosing_fn_idx_by_span(&local_graph.nodes, row, col)
                .filter(|&i| local_graph.nodes[i as usize].name == fanout_ref.source_name)
                .map(|i| start_idx + i)
                .or_else(|| {
                    symbol_table.lookup_in_file_with_kind(
                        &file_path_lossy,
                        &fanout_ref.source_name,
                        ResolveTarget::Callable,
                    )
                });
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

/// The source slot of a framework ref the parser named: the smallest node so
/// named whose span holds the ref's site (Spring names the class that holds
/// the handler), else the one node of the file so named
/// ([`outermost_named_slot`]; FastAPI names the module-level `app`). `None`
/// when no node or several have the name: the ref gets no edge, never the
/// first of several.
fn framework_ref_source(
    nodes: &[RawNode],
    fw_ref: &ecp_core::analyzer::types::RawFrameworkRef,
    raw_map: &RawIndexMap,
    file: FileOrd,
) -> Option<u32> {
    let enclosing = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            n.name == fw_ref.source_name
                && ecp_core::analyzer::types::span_contains(n.span, fw_ref.span)
        })
        .min_by_key(|(i, n)| (crate::framework_helpers::span_area(n.span), *i))
        .map(|(i, _)| i);
    match enclosing {
        Some(i) => raw_map.node(file, RawIdx(i as u32)),
        None => outermost_named_slot(nodes, &fw_ref.source_name, raw_map, file).ok(),
    }
}

/// T7-6: per raw node of `local_graph`, whether its unchanged body skips
/// resolver work. `None` when the skip data has no entry for the file: no
/// diff data, or the file needs a full reanalyze. An empty inner set means
/// every symbol changed. The uids are Pass 1's, overloads numbered.
fn skipped_nodes(
    local_graph: &LocalGraph,
    symbol_skip: Option<&FxHashMap<String, FxHashSet<u64>>>,
) -> Option<Vec<bool>> {
    let symbol_skip = symbol_skip?;
    let raw = local_graph.file_path.to_string_lossy();
    let path: std::borrow::Cow<'_, str> = if raw.contains('\\') {
        std::borrow::Cow::Owned(raw.replace('\\', "/"))
    } else {
        raw
    };
    let skip_uids = symbol_skip.get(path.as_ref())?;
    Some(
        file_uids(&path, &local_graph.nodes, raw_decl)
            .iter()
            .map(|uid| skip_uids.contains(uid))
            .collect(),
    )
}

/// Pass 2: Resolve imports and build edges
///
/// Pass 2 strategy: dump-disabled path (production hot path) runs in
/// parallel over `local_graphs` via rayon. Dump-enabled path (oracle
/// harness, off by default) stays serial so one resolver owns the
/// decision stream and preserves deterministic dump order.
///
/// To enable parallelism we pre-compute two artifacts serially before
/// the par_iter so the inner closure only needs read-only access to
/// the resolver + symbol_table:
///   1. `file_base(raw_map, graph_idx)` — base `current_node_idx` for each
///      `local_graph`, read from the build's one `RawIndexMap`. Replaces
///      the `current_node_idx += 1` accumulator that previously coupled
///      graphs sequentially.
///   2. `reason_cache` — every unique `framework_refs.reason` /
///      `fanout_refs.reason` interned into `string_pool` up front.
///      `string_pool.add` is `&mut self` so the inner loop can't
///      touch it; pre-interning + lookup-by-cache is `&StrRef`-only.
#[allow(clippy::too_many_arguments)]
fn pass2_resolve_edges(
    local_graphs: &[LocalGraph],
    symbol_table: &SymbolTable,
    string_pool: &mut StringPool,
    path_aliases: &PathAliases,
    mod_tree_ctx: Option<(&RustWorkspaceModTree, &std::path::Path)>,
    reexports: &ReexportIndex<'_>,
    raw_map: &RawIndexMap,
    resolver_dump_path: Option<&std::path::Path>,
    symbol_skip_set: Option<&FxHashMap<String, FxHashSet<u64>>>,
) -> Pass2Output {
    let reason_heritage = string_pool.add("heritage");
    let reason_implements = string_pool.add("pass2:implements");
    let reason_type = string_pool.add("type_annotation");
    let reason_call = CallReasons {
        call: string_pool.add("call"),
        nearest_copy: string_pool.add(NEAREST_COPY_REASON),
    };
    let reason_reads_field = string_pool.add("field_read");
    let reason_variant = VariantReasons {
        used: string_pool.add(VARIANT_USE_REASON),
        matched: string_pool.add(VARIANT_PATTERN_REASON),
    };
    let reason_name_ref = NameRefReasons {
        value: string_pool.add(VALUE_READ_REASON),
        function: string_pool.add(FUNCTION_REFERENCE_REASON),
    };

    let mut reason_cache: FxHashMap<String, StrRef> = FxHashMap::default();
    for lg in local_graphs {
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

    let dump_enabled = resolver_dump_path.is_some();
    let path_aliases = path_aliases.clone();

    // When dumping is enabled we run the serial path so a single resolver
    // owns the decision stream. When disabled (the production case) we
    // create a fresh `Resolver` *inside* each par_iter worker so each
    // thread owns its own state.
    let mut resolver_for_dump = if dump_enabled {
        let mut r = Resolver::new(symbol_table)
            .with_path_aliases(path_aliases.clone())
            .with_reexports(reexports);
        if let Some((mt, root)) = mod_tree_ctx {
            r = r.with_mod_tree(mt, root.to_path_buf());
        }
        r.enable_dump();
        Some(r)
    } else {
        None
    };

    let symbol_table_ref = symbol_table;
    let reason_cache_ref = &reason_cache;
    // T7-6: reference to skip-set (None = full reanalyze for all files).
    let symbol_skip_ref = symbol_skip_set;

    // Pre-build per-graph indirect-call lookup keyed by caller span and
    // call index so same-name functions/methods in one file do not collide.
    let indirect_lookups: Vec<FxHashMap<CallMetaKey, (u8, String)>> = local_graphs
        .iter()
        .map(|lg| {
            lg.call_metas
                .iter()
                .map(|m| {
                    (
                        CallMetaKey::new(m.caller_span, m.call_index),
                        (m.flags, m.dispatch_type.clone()),
                    )
                })
                .collect()
        })
        .collect();

    // `pending_call_metas`: pairs of (pre-sort edge index, RawCallMeta ref).
    // Collected alongside edges and promoted to ZeroCopyGraph.call_metas after
    // the final edge sort remaps pre-sort indices to sorted positions.
    let mut pending_call_metas_global: Vec<(usize, u8, String)> = Vec::new();
    let mut file_edges: Vec<(FileOrd, Edge)> = Vec::new();

    let edges: Vec<Edge> = if let Some(resolver) = resolver_for_dump.as_mut() {
        // Serial dump path — original loop, with reason lookups going
        // through `reason_cache` (filled above) instead of inline
        // `string_pool.add`.
        let mut edges = Vec::new();
        let mut current_node_idx = 0u32;
        for (graph_idx, local_graph) in local_graphs.iter().enumerate() {
            let lookup = &indirect_lookups[graph_idx];
            let lexical_lookup =
                lexical_functions(local_graph, file_base(raw_map, graph_idx as u32));
            let skipped = skipped_nodes(local_graph, symbol_skip_ref);
            for (node_offset, raw_node) in local_graph.nodes.iter().enumerate() {
                if skipped.as_ref().is_some_and(|skipped| skipped[node_offset]) {
                    current_node_idx += 1;
                    continue;
                }
                pass2_emit_node_edges(
                    resolver,
                    local_graph,
                    raw_node,
                    current_node_idx,
                    reason_heritage,
                    reason_implements,
                    reason_type,
                    reason_call,
                    reason_reads_field,
                    reason_variant,
                    reason_name_ref,
                    symbol_table,
                    &mut edges,
                    lookup,
                    &lexical_lookup,
                    &mut pending_call_metas_global,
                );
                current_node_idx += 1;
            }
            // T7-6: skip framework/fanout edges when all symbols in this
            // file are in the skip set (import set unchanged → no new
            // resolution targets for framework refs either).
            if !skipped.is_some_and(|skipped| skipped.iter().all(|&s| s)) {
                pass2_emit_framework_and_fanout(
                    resolver,
                    symbol_table_ref,
                    local_graph,
                    raw_map,
                    FileOrd(graph_idx as u32),
                    reason_cache_ref,
                    &mut edges,
                    &mut file_edges,
                );
            }
        }
        edges
    } else {
        // Parallel path. Each rayon worker drives a `flat_map` chunk;
        // we pay one Resolver construction per local_graph (cheap —
        // borrows symbol_table, clones path_aliases). For ~14k files
        // on .sample_repo that's ~14k path_aliases.clone() calls
        // totalling a few ms — far below the parallelism gain.
        //
        // The mod_tree borrow is `&RustWorkspaceModTree` (read-only,
        // `Sync`), so sharing it across rayon workers is safe.
        // Collect (local_edges, local_pending) per graph, then stitch.
        let per_graph: Vec<PerGraphPass2> = local_graphs
            .par_iter()
            .enumerate()
            .map(|(graph_idx, local_graph)| {
                let mut resolver = Resolver::new(symbol_table_ref)
                    .with_path_aliases(path_aliases.clone())
                    .with_reexports(reexports);
                if let Some((mt, root)) = mod_tree_ctx {
                    resolver = resolver.with_mod_tree(mt, root.to_path_buf());
                }
                let start_idx = file_base(raw_map, graph_idx as u32);
                let lookup = &indirect_lookups[graph_idx];
                let lexical_lookup = lexical_functions(local_graph, start_idx);
                let mut local_edges: Vec<Edge> = Vec::new();
                let mut local_pending: Vec<(usize, u8, String)> = Vec::new();
                let mut local_file_edges: Vec<(FileOrd, Edge)> = Vec::new();
                let skipped = skipped_nodes(local_graph, symbol_skip_ref);
                for (node_offset, raw_node) in local_graph.nodes.iter().enumerate() {
                    let current_node_idx = start_idx + node_offset as u32;
                    if skipped.as_ref().is_some_and(|skipped| skipped[node_offset]) {
                        continue;
                    }
                    pass2_emit_node_edges(
                        &resolver,
                        local_graph,
                        raw_node,
                        current_node_idx,
                        reason_heritage,
                        reason_implements,
                        reason_type,
                        reason_call,
                        reason_reads_field,
                        reason_variant,
                        reason_name_ref,
                        symbol_table_ref,
                        &mut local_edges,
                        lookup,
                        &lexical_lookup,
                        &mut local_pending,
                    );
                }
                // T7-6: skip framework/fanout edges only when ALL symbols
                // in this file are unchanged.
                if !skipped.is_some_and(|skipped| skipped.iter().all(|&s| s)) {
                    pass2_emit_framework_and_fanout(
                        &resolver,
                        symbol_table_ref,
                        local_graph,
                        raw_map,
                        FileOrd(graph_idx as u32),
                        reason_cache_ref,
                        &mut local_edges,
                        &mut local_file_edges,
                    );
                }
                (local_edges, local_pending, local_file_edges)
            })
            .collect();

        // Stitch per-graph results: compute global edge offset for each graph's
        // local_pending indices, then merge into the global pending vec.
        let mut all_edges: Vec<Edge> = Vec::new();
        for (local_edges, local_pending, local_file_edges) in per_graph {
            let edge_offset = all_edges.len();
            for (local_idx, flags, dispatch_type) in local_pending {
                pending_call_metas_global.push((edge_offset + local_idx, flags, dispatch_type));
            }
            all_edges.extend(local_edges);
            file_edges.extend(local_file_edges);
        }
        all_edges
    };
    let resolver_dump_drain = resolver_for_dump.as_mut();

    // Optional: flush the resolver decision dump now that pass 2 is done.
    // Spec: docs/specs/2026-05-15-resolver-oracle-harness.md
    // Only the serial dump-enabled path keeps a `Resolver` alive past
    // Pass 2; the parallel path constructs ephemeral per-graph resolvers
    // with `decisions: None`, so a dump-disabled run has nothing to flush.
    if let Some(dump_path) = resolver_dump_path {
        if let Some(resolver) = resolver_dump_drain {
            if let Some(decisions) = resolver.take_decisions() {
                if let Err(e) = write_resolver_dump(dump_path, &decisions, symbol_table) {
                    tracing::warn!("Failed to write resolver dump to {:?}: {}", dump_path, e);
                }
            }
        }
    }

    (edges, pending_call_metas_global, file_edges)
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
    use ecp_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawImport, RawNode};
    use ecp_core::graph::NodeKind;

    // ---- classify_collision (uid-collision reclassification) ----

    #[test]
    fn classify_c_macro_function_struct_is_ifdef_redef() {
        for kind in [
            NodeKind::Macro,
            NodeKind::Function,
            NodeKind::Struct,
            NodeKind::Enum,
            NodeKind::Typedef,
            NodeKind::Variable,
            NodeKind::Const,
            NodeKind::Property,
        ] {
            for path in [
                "C/deps/hiredis/hiredis.h",
                "C/foo.c",
                "Cpp/include/nlohmann/json.hpp",
                "src/foo.cc",
                "src/foo.cxx",
                "src/foo.hh",
                "src/foo.hxx",
            ] {
                assert_eq!(
                    classify_collision(kind, path),
                    "ifdef-redef",
                    "kind={kind:?} path={path}"
                );
            }
        }
    }

    #[test]
    fn classify_non_c_non_method_falls_through() {
        // Go Variable, Rust Impl, Bash Const, etc. stay as `uid-collision`
        // until per-lang fixes land (or until they get their own kind).
        for (kind, path) in [
            (NodeKind::Variable, "Go/auth.go"),
            (NodeKind::Impl, "Rust/lib.rs"),
            (NodeKind::Const, "bash/aliases.sh"),
            (NodeKind::Class, "Swift/AppDelegate.swift"),
            // C/C++ Class is not in the ifdef-redef list (class is C++-only
            // and uses body anchoring after the cpp scm tighten).
            (NodeKind::Class, "Cpp/foo.cpp"),
        ] {
            assert_eq!(
                classify_collision(kind, path),
                "uid-collision",
                "kind={kind:?} path={path}"
            );
        }
    }

    #[test]
    fn classify_non_c_property_is_field_reassign() {
        // An instance field assigned in multiple places (self.x / this.x / @x)
        // collides with itself by design — not a parser bug.
        for path in ["py/manager.py", "js/store.js", "Rust/benches/copy.rs"] {
            assert_eq!(
                classify_collision(NodeKind::Property, path),
                "field-reassign",
                "non-C-family Property must classify as field-reassign: {path}"
            );
        }
        // C-family Property stays ifdef-redef (header redefinition, not OO field).
        assert_eq!(
            classify_collision(NodeKind::Property, "c/foo.h"),
            "ifdef-redef"
        );
    }

    /// L0 end-to-end: caller imports `./b`, defining file lives at
    /// `src/b.ts`. Tier 2 ImportScoped must fire and emit a `Calls` edge
    /// at confidence 0.95. Locks in the 173-hit win measured on NestJS so
    /// it can't silently regress.
    #[test]
    fn l0_relative_import_produces_import_scoped_edge() {
        let caller = LocalGraph {
            file_path: "src/a.ts".into(),
            content_hash: [0; 8],
            nodes: vec![RawNode {
                name: "useThing".into(),
                kind: NodeKind::Function,
                span: (0, 0, 0, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec!["thing".into()],
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
            }],
            documents: vec![],
            imports: vec![RawImport {
                source: "./b".into(),
                imported_name: "thing".into(),
                alias: None,
                binding_kind: None,
            }],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: vec![],
            raw_function_metas: vec![],
            export_aliases: None,
        };
        let target = LocalGraph {
            file_path: "src/b.ts".into(),
            content_hash: [0; 8],
            nodes: vec![RawNode {
                name: "thing".into(),
                kind: NodeKind::Function,
                span: (0, 0, 0, 0),
                is_exported: true,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
            }],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: vec![],
            raw_function_metas: vec![],
            export_aliases: None,
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

        let tmp = std::env::temp_dir().join(format!("ecp-dump-test-{}.jsonl", std::process::id()));
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
                DecisionTier::SameFile
                | DecisionTier::QualifierScoped
                | DecisionTier::HeritageScoped
                | DecisionTier::Global
                | DecisionTier::AmbiguousGlobal
                | DecisionTier::ModuleTree
                | DecisionTier::TypeOwned
                | DecisionTier::TypeHeritage
                | DecisionTier::TypeCandidates
                | DecisionTier::NearestCopy => {
                    panic!(
                        "fixture should only produce ImportScoped/Unresolved, got {:?}",
                        original.tier
                    )
                }
            }
        }
    }

    /// `pass1_register_nodes` became reachable from a test when it was lifted
    /// out of `build()`. Before that, node ordering — which UIDs hash from —
    /// could only be observed through a full graph build.
    #[test]
    fn pass1_keeps_node_order_and_count_aligned_with_input() {
        let g = LocalGraph {
            file_path: "order.py".into(),
            content_hash: [0; 8],
            nodes: vec![raw_fn("alpha", 0), raw_fn("beta", 10), raw_fn("gamma", 20)],
            ..Default::default()
        };

        let out = pass1_register_nodes(std::slice::from_ref(&g));

        assert_eq!(out.nodes.len(), 3, "one graph node per raw node");
        assert_eq!(out.files.len(), 1);
        assert!(
            out.collision_blind_spots.is_empty(),
            "distinct names must not collide"
        );
        let names: Vec<&str> = out
            .nodes
            .iter()
            .map(|n| out.string_pool.resolve(&n.name))
            .collect();
        assert_eq!(
            names,
            vec!["alpha", "beta", "gamma"],
            "input order is load-bearing: UIDs and the RawIndexMap are derived from it"
        );
    }

    /// `uid::compute` hashes (kind, path, owner, name) and deliberately omits
    /// the span, so two same-named symbols in one file collide by construction
    /// — C `#ifdef` redefinitions and overloads reach this path for real.
    ///
    /// The second one is dropped, but a tombstone takes its slot: `nodes.len()`
    /// has to stay equal to the raw-node count or every later `RawIndexMap`
    /// offset shifts and edges land on the wrong symbols.
    #[test]
    fn pass1_tombstones_a_uid_collision_without_shifting_indices() {
        let g = LocalGraph {
            file_path: "dup.c".into(),
            content_hash: [0; 8],
            nodes: vec![raw_fn("twice", 0), raw_fn("twice", 30)],
            ..Default::default()
        };

        let out = pass1_register_nodes(std::slice::from_ref(&g));

        assert_eq!(
            out.nodes.len(),
            2,
            "the tombstone must occupy the dropped node's slot"
        );
        assert_eq!(
            out.nodes[1].uid, !out.nodes[0].uid,
            "tombstone uid is the bit-inverse, so it never shadows the survivor"
        );
        assert!(
            out.string_pool.resolve(&out.nodes[1].name).is_empty(),
            "an empty name keeps the tombstone out of name-based lookups"
        );
        assert_eq!(
            out.collision_blind_spots.len(),
            1,
            "the collision is reported, not silently swallowed"
        );
    }

    #[test]
    fn test_pass1_register_nodes_three_duplicates_share_one_tombstone_uid() {
        let g = graph_of(
            "dup.c",
            vec![raw_fn("twice", 0), raw_fn("twice", 30), raw_fn("twice", 60)],
        );
        let out = pass1_register_nodes(std::slice::from_ref(&g));
        let survivor = out.nodes[0].uid;
        assert_eq!(
            [out.nodes[1].uid, out.nodes[2].uid],
            [!survivor, !survivor],
            "every later duplicate of one base uid stores the same tombstone uid"
        );
        assert!(out.nodes[1..]
            .iter()
            .all(|n| out.string_pool.resolve(&n.name).is_empty()));
        assert_eq!(
            out.symbol_table.lookup_in_file("dup.c", "twice"),
            Some(0),
            "a name lookup reaches the survivor, never a tombstone"
        );
    }

    #[test]
    fn test_pass1_register_nodes_later_method_slot_keeps_overload_uid() {
        let g = graph_of(
            "A.java",
            vec![
                raw_decl("save", NodeKind::Method, 1, &[], Some("A")),
                raw_decl("save", NodeKind::Method, 5, &[], Some("A")),
            ],
        );
        let out = pass1_register_nodes(std::slice::from_ref(&g));
        let first = ecp_core::uid::compute(NodeKind::Method, "A.java", Some("A"), "save");
        assert_eq!(
            [out.nodes[0].uid, out.nodes[1].uid],
            [first, ecp_core::uid::overload(first, 1)],
            "a `UidSlot::Later` slot becomes overload 1, not a tombstone"
        );
        assert_eq!(out.string_pool.resolve(&out.nodes[1].name), "save");
        assert!(out.collision_blind_spots.is_empty());
    }

    #[test]
    fn test_pass1_register_nodes_two_graphs_one_path_number_as_one_file() {
        let graphs = [
            graph_of(
                "A.java",
                vec![
                    raw_decl("save", NodeKind::Method, 1, &[], Some("A")),
                    raw_decl("helper", NodeKind::Function, 9, &[], None),
                ],
            ),
            graph_of(
                "A.java",
                vec![
                    raw_decl("save", NodeKind::Method, 1, &[], Some("A")),
                    raw_decl("helper", NodeKind::Function, 9, &[], None),
                ],
            ),
        ];
        let out = pass1_register_nodes(&graphs);
        let save = ecp_core::uid::compute(NodeKind::Method, "A.java", Some("A"), "save");
        let helper = ecp_core::uid::compute(NodeKind::Function, "A.java", None, "helper");
        assert_eq!(
            out.nodes.iter().map(|n| n.uid).collect::<Vec<_>>(),
            vec![save, helper, ecp_core::uid::overload(save, 1), !helper],
            "the second graph's `First` slots repeat the first graph's nodes: \
             the method becomes an overload and the function a tombstone"
        );
    }

    fn raw_decl(
        name: &str,
        kind: NodeKind,
        start_row: u32,
        heritage: &[&str],
        owner: Option<&str>,
    ) -> RawNode {
        RawNode {
            kind,
            heritage: heritage.iter().map(|h| h.to_string()).collect(),
            owner_class: owner.map(str::to_string),
            ..raw_fn(name, start_row)
        }
    }

    fn graph_of(path: &str, nodes: Vec<RawNode>) -> LocalGraph {
        LocalGraph {
            file_path: path.into(),
            content_hash: [0; 8],
            nodes,
            ..Default::default()
        }
    }

    fn raw_map_of(graphs: &[LocalGraph]) -> RawIndexMap {
        RawIndexMap::new(0, graphs.iter().map(|g| g.nodes.len())).unwrap()
    }

    /// Pass 1 plus the supertypes pre-pass. `graphs` must already be sorted
    /// by path, as `build()` sorts them before Pass 1.
    fn table_with_supertypes(graphs: &[LocalGraph]) -> SymbolTable {
        let mut st = pass1_register_nodes(graphs).symbol_table;
        let declared = pass1_9_supertypes(
            graphs,
            &st,
            &PathAliases::default(),
            None,
            &ReexportIndex::default(),
            &raw_map_of(graphs),
        );
        st.build_declared_bases(declared);
        st
    }

    #[test]
    fn test_pass1_register_nodes_owner_class_feeds_owner_index() {
        let g = graph_of(
            "db.py",
            vec![
                raw_decl("Tx", NodeKind::Class, 0, &[], None),
                raw_decl("execute", NodeKind::Method, 1, &[], Some("Tx")),
                raw_decl("BaseDBPool", NodeKind::Class, 10, &[], None),
                raw_decl("execute", NodeKind::Method, 11, &[], Some("BaseDBPool")),
                raw_decl("helper", NodeKind::Function, 20, &[], None),
            ],
        );
        let st = pass1_register_nodes(std::slice::from_ref(&g)).symbol_table;
        let call = ResolveTarget::Callable;
        assert_eq!(st.owned_in_file("db.py", "execute", "Tx", call), vec![1]);
        assert_eq!(
            st.owned_in_file("db.py", "execute", "BaseDBPool", call),
            vec![3]
        );
        assert!(st.owned_in_file("db.py", "helper", "Tx", call).is_empty());
    }

    /// Ids: base.py AnimalBase=0; child.py Puppy=1, Kennel=2, bark=3,
    /// Crate=4; mixin.py WagMixin=5.
    fn python_heritage_graphs() -> Vec<LocalGraph> {
        vec![
            graph_of(
                "base.py",
                vec![raw_decl("AnimalBase", NodeKind::Class, 0, &[], None)],
            ),
            graph_of(
                "child.py",
                vec![
                    raw_decl(
                        "Puppy",
                        NodeKind::Class,
                        0,
                        &["ExternalMixin", "AnimalBase", "WagMixin"],
                        None,
                    ),
                    raw_decl(
                        "Kennel",
                        NodeKind::Class,
                        10,
                        &["Puppy", "AnimalBase"],
                        None,
                    ),
                    // A non-Rust method carrying heritage is not attributed
                    // to its owner.
                    raw_decl("bark", NodeKind::Method, 11, &["WagMixin"], Some("Kennel")),
                    raw_decl("Crate", NodeKind::Class, 20, &["AnimalBase[int]"], None),
                ],
            ),
            graph_of(
                "mixin.py",
                vec![raw_decl("WagMixin", NodeKind::Class, 0, &[], None)],
            ),
        ]
    }

    #[test]
    fn test_pass1_9_supertypes_external_base_first_keeps_declared_order_and_position() {
        let st = table_with_supertypes(&python_heritage_graphs());
        let puppy = st.supertypes(1).expect("Puppy declares heritage");
        assert_eq!(
            &*puppy.bases,
            &[0, 5],
            "declared order, external base skipped"
        );
        assert_eq!(
            puppy.first_unresolved,
            Some(0),
            "the external mixin is declared before every project base"
        );
        assert_eq!(&*st.supertypes(2).unwrap().bases, &[1, 0]);
        assert_eq!(st.ancestors(2), vec![1, 0, 5], "Kennel -> Puppy -> bases");
        assert!(st.supertypes(0).is_none(), "no heritage, no entry");
        assert!(
            st.supertypes(3).is_none(),
            "a Python method is not a subject"
        );
    }

    /// Stands for: the `Overrides` pass reads each declared base's spelling,
    /// slot and resolver confidence from Pass 1.9, which `Supertypes` drops.
    #[test]
    fn test_pass1_9_supertypes_declared_bases_keep_spelling_and_confidence() {
        let st = table_with_supertypes(&python_heritage_graphs());
        let puppy = st.declared_bases(1);
        let spellings: Vec<&str> = puppy.iter().map(|b| &*b.spelling).collect();
        assert_eq!(spellings, ["ExternalMixin", "AnimalBase", "WagMixin"]);
        assert!(
            puppy[0].candidates.is_empty(),
            "the external mixin keeps its slot"
        );
        let kennel = st.declared_bases(2);
        assert_eq!(
            &*kennel[0].candidates,
            &[(1u32, 1.0f32)],
            "a same-file base binds at the exact tier"
        );
        assert!(
            kennel[1].candidates[0].1 < 1.0,
            "a cross-file base with no import binds below it: {kennel:?}"
        );
    }

    /// A supertype is exactly a heritage edge Pass 2 emits from the same
    /// type node: the pre-pass reuses Pass 2's resolution, so neither can
    /// know a base the other does not (generic `AnimalBase[int]` included).
    #[test]
    fn test_pass1_9_supertypes_match_pass2_heritage_edges_from_type_nodes() {
        let graphs = python_heritage_graphs();
        let st = table_with_supertypes(&graphs);
        let mut from_table: Vec<(u32, u32)> = (0..6u32)
            .flat_map(|t| {
                st.supertypes(t)
                    .map(|s| s.bases.iter().map(|&b| (t, b)).collect::<Vec<_>>())
                    .unwrap_or_default()
            })
            .collect();
        from_table.sort_unstable();

        let mut builder = GraphBuilder::new();
        for g in graphs {
            builder.add_graph(g);
        }
        let graph = builder.build();
        let type_ids = [0u32, 1, 2, 4, 5];
        let mut from_edges: Vec<(u32, u32)> = graph
            .edges
            .iter()
            .filter(|e| matches!(e.rel_type, RelType::Extends | RelType::Implements))
            .filter(|e| type_ids.contains(&e.source))
            .map(|e| (e.source, e.target))
            .collect();
        from_edges.sort_unstable();
        from_edges.dedup();

        assert_eq!(from_table, from_edges);
    }

    #[test]
    fn test_pass1_9_supertypes_rust_trait_impl_methods_attribute_to_owner_struct() {
        let g = graph_of(
            "src/lib.rs",
            vec![
                raw_decl("Dog", NodeKind::Struct, 0, &[], None),
                raw_decl("Speak", NodeKind::Trait, 2, &[], None),
                raw_decl("Walk", NodeKind::Trait, 4, &[], None),
                raw_decl("speak", NodeKind::Method, 6, &["Speak"], Some("Dog")),
                raw_decl("shout", NodeKind::Method, 8, &["Speak"], Some("Dog")),
                raw_decl("fmt", NodeKind::Method, 10, &["Display"], Some("Dog")),
                raw_decl("Dog", NodeKind::Impl, 12, &["Walk"], Some("Walk")),
                raw_decl("helper", NodeKind::Function, 20, &["Speak"], None),
            ],
        );
        let st = table_with_supertypes(std::slice::from_ref(&g));
        let dog = st
            .supertypes(0)
            .expect("Dog gains the heritage of its impl blocks");
        assert_eq!(&*dog.bases, &[1, 2], "one entry per trait, in source order");
        assert_eq!(
            dog.first_unresolved,
            Some(1),
            "the external Display impl sits after Speak"
        );
        for not_a_type in [3, 4, 5, 6, 7] {
            assert!(st.supertypes(not_a_type).is_none());
        }
    }

    #[test]
    fn test_pass1_9_supertypes_uid_collision_tombstone_skipped() {
        // Ids: bases.py A=0, B=1; dup.py Dup=2, Dup tombstone=3.
        let graphs = vec![
            graph_of(
                "bases.py",
                vec![
                    raw_decl("A", NodeKind::Class, 0, &[], None),
                    raw_decl("B", NodeKind::Class, 5, &[], None),
                ],
            ),
            graph_of(
                "dup.py",
                vec![
                    raw_decl("Dup", NodeKind::Class, 0, &["A"], None),
                    raw_decl("Dup", NodeKind::Class, 10, &["B"], None),
                ],
            ),
        ];
        let st = table_with_supertypes(&graphs);
        assert_eq!(&*st.supertypes(2).unwrap().bases, &[0]);
        assert!(
            st.supertypes(3).is_none(),
            "a tombstone is unreachable by name and gets no supertypes"
        );
    }

    fn raw_fn(name: &str, start_row: u32) -> RawNode {
        RawNode {
            name: name.into(),
            kind: NodeKind::Function,
            span: (start_row, 0, start_row + 2, 0),
            is_exported: false,
            heritage: vec![],
            type_annotation: None,
            decorators: vec![],
            calls: vec![],
            field_reads: Vec::new(),
            owner_class: None,
            content_hash: 0,
        }
    }

    /// Contract: a raw node's global index is the sum of the node counts of
    /// the files before it in path order. The expected indices below come
    /// from that definition, stated by hand for fixtures of 2, 0 and 2 nodes
    /// (file `c.py` starts at 2), not from `RawIndexMap`. A route, a
    /// `Decorates` source, an `Overrides` pair and a `Calls` edge are not in
    /// the fixture; the two passes it covers read the map through
    /// pass 1.9 (`Extends`) and the tx-scope post-process (`OpensTxScope`).
    #[test]
    fn test_build_edge_multiset_unchanged_after_raw_index_map() {
        use ecp_core::analyzer::types::{FrameworkId, RawTxScope};
        use std::collections::BTreeSet;

        let class = |name: &str, row: u32, heritage: &[&str]| RawNode {
            kind: NodeKind::Class,
            heritage: heritage.iter().map(|h| h.to_string()).collect(),
            ..raw_fn(name, row)
        };
        let a = LocalGraph {
            file_path: "a.py".into(),
            content_hash: [0; 8],
            nodes: vec![raw_fn("a_one", 0), raw_fn("a_two", 4)],
            ..Default::default()
        };
        let empty = LocalGraph {
            file_path: "b.py".into(),
            content_hash: [0; 8],
            ..Default::default()
        };
        let c = LocalGraph {
            file_path: "c.py".into(),
            content_hash: [0; 8],
            nodes: vec![class("Base", 0, &[]), class("Child", 4, &["Base"])],
            tx_scopes: Some(Box::new([RawTxScope::new(
                1,
                FrameworkId::SpringTransactional,
            )])),
            ..Default::default()
        };
        let mut builder = GraphBuilder::new();
        // Added out of path order: `build()` sorts before Pass 1.
        builder.add_graph(c);
        builder.add_graph(empty);
        builder.add_graph(a);
        let graph = builder.build();

        let pairs = |rel: RelType| -> BTreeSet<(u32, u32)> {
            graph
                .edges
                .iter()
                .filter(|e| e.rel_type == rel)
                .map(|e| (e.source, e.target))
                .collect()
        };
        let child_global = 3;
        let base_global = 2;
        assert_eq!(
            pairs(RelType::Extends),
            BTreeSet::from([(child_global, base_global)]),
            "Child (raw 1 of c.py) extends Base (raw 0): c.py starts at 2 after a.py's 2 nodes and the empty b.py"
        );
        let tx: Vec<_> = pairs(RelType::OpensTxScope).into_iter().collect();
        assert_eq!(tx.len(), 1);
        assert_eq!(
            tx[0].0, child_global,
            "the tx scope opens at Child, raw 1 of c.py"
        );
        assert_eq!(
            graph.nodes[tx[0].1 as usize].kind,
            NodeKind::TransactionScope
        );
    }

    #[test]
    fn fanout_ref_emits_n_edges_with_confidence_decay() {
        use ecp_core::analyzer::types::RawFanoutRef;

        let g = LocalGraph {
            file_path: "test.py".into(),
            content_hash: [0; 8],
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
                    field_reads: Vec::new(),
                    owner_class: None,
                    content_hash: 0,
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
                    field_reads: Vec::new(),
                    owner_class: None,
                    content_hash: 0,
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
                    field_reads: Vec::new(),
                    owner_class: None,
                    content_hash: 0,
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
                    field_reads: Vec::new(),
                    owner_class: None,
                    content_hash: 0,
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
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: vec![],
            raw_function_metas: vec![],
            export_aliases: None,
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
        use ecp_core::analyzer::types::RawFanoutRef;

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
            field_reads: Vec::new(),
            owner_class: None,
            content_hash: 0,
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
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
            });
        }
        let g = LocalGraph {
            file_path: "test.py".into(),
            content_hash: [0; 8],
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
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: vec![],
            raw_function_metas: vec![],
            export_aliases: None,
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
            content_hash: [0; 8],
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
                    field_reads: Vec::new(),
                    owner_class: None,
                    content_hash: 0,
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
                    field_reads: Vec::new(),
                    owner_class: None,
                    content_hash: 0,
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
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: vec![],
            raw_function_metas: vec![],
            export_aliases: None,
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
            content_hash: [0; 8],
            nodes: vec![RawNode {
                field_reads: Vec::new(),
                name: name.into(),
                kind,
                span: (0, 0, 0, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls,
                owner_class: None,
                content_hash: 0,
            }],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: vec![],
            raw_function_metas: vec![],
            export_aliases: None,
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

    fn calls_edge_count(graphs: Vec<LocalGraph>) -> usize {
        let mut builder = GraphBuilder::new();
        for g in graphs {
            builder.add_graph(g);
        }
        let graph = builder.build();
        graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::Calls)
            .count()
    }

    /// The constructor fallback follows the Tier-3 rule: a construction of a
    /// class declared in two files picks neither.
    #[test]
    fn test_resolve_call_type_in_two_files_emits_no_calls_edge() {
        let count = calls_edge_count(vec![
            mk_file("app.py", "make", NodeKind::Function, vec!["Widget".into()]),
            mk_file("a.py", "Widget", NodeKind::Class, vec![]),
            mk_file("b.py", "Widget", NodeKind::Class, vec![]),
        ]);
        assert_eq!(count, 0, "an ambiguous type must not get a Calls edge");
    }

    #[test]
    fn test_resolve_call_name_neither_callable_nor_type_emits_no_calls_edge() {
        let count = calls_edge_count(vec![
            mk_file("app.py", "make", NodeKind::Function, vec!["Widget".into()]),
            mk_file("a.py", "Widget", NodeKind::Variable, vec![]),
        ]);
        assert_eq!(count, 0, "a Variable is never constructed");
    }

    /// Rust has no constructors: a tuple-struct call `Point(1, 2)` keeps no
    /// Calls edge into the struct.
    #[test]
    fn test_resolve_call_rust_tuple_struct_call_emits_no_calls_edge() {
        let count = calls_edge_count(vec![
            mk_file(
                "src/app.rs",
                "make",
                NodeKind::Function,
                vec!["Point".into()],
            ),
            mk_file("src/point.rs", "Point", NodeKind::Struct, vec![]),
        ]);
        assert_eq!(count, 0, "Rust is outside the constructor fallback");
    }

    /// Task A acceptance: `LocalGraph.blind_spots` survive the builder pass
    /// and land in `ZeroCopyGraph.blind_spots` with all fields (kind /
    /// file_path / span / hint) preserved via the string pool. Locks in
    /// the contract that Task B (Python detector) and Task C (CLI
    /// surface) rely on.
    #[test]
    fn blind_spots_pass_through_to_graph() {
        use ecp_core::analyzer::types::BlindSpot;

        let g = LocalGraph {
            file_path: "test.py".into(),
            content_hash: [0; 8],
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
                    is_test: false,
                },
                BlindSpot {
                    kind: "python-dynamic-import".into(),
                    file_path: "test.py".into(),
                    span: (15, 0, 15, 40),
                    hint: "importlib.import_module(...) — dynamic loading".into(),
                    is_test: false,
                },
            ],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: vec![],
            raw_function_metas: vec![],
            export_aliases: None,
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

        let resolve = |sref: &ecp_core::pool::StrRef| -> &str {
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
    ///
    /// Extended from the original aggregated-set assertion to:
    ///   (a) stratify per `RelType` so a divergence on one type doesn't hide
    ///       behind equality in another;
    ///   (b) include the resolved `reason` string in the equality predicate;
    ///   (c) add a `HandlesRoute` fixture to fire that emit branch;
    ///   (d) pin emit-zero invariant for Sub-projects 1/5 types
    ///       (Imports, Defines, Fetches); Implements lifted here (§8.2).
    ///
    /// The fixtures cover these edge-emission categories:
    ///   * heritage (`Extends`) — Class Foo extends concrete Class Bar
    ///   * heritage (`Implements`) — Class Implementor implements Interface IThing
    ///   * calls (`Calls`) — Function with callee
    ///   * type_annotation (`Accesses`)
    ///   * framework_refs (`References` via Spring fixture)
    ///   * fanout_refs (`References` via reflection fixture)
    ///   * routes (`HandlesRoute`) — bar.rs exposes GET /users → other_fn
    #[test]
    fn pass2_parallel_serial_identical_per_reltype() {
        use ecp_core::analyzer::types::{RawFanoutRef, RawFrameworkRef, RawRoute};
        use std::collections::{BTreeMap, BTreeSet};

        fn build_fixtures() -> Vec<LocalGraph> {
            vec![
                LocalGraph {
                    file_path: "src/foo.rs".into(),
                    content_hash: [0; 8],
                    nodes: vec![
                        RawNode {
                            name: "Foo".into(),
                            kind: NodeKind::Class,
                            span: (0, 0, 10, 0),
                            is_exported: true,
                            heritage: vec!["Bar".into()],
                            type_annotation: Some("Other".into()),
                            decorators: vec![],
                            calls: vec!["other_fn".into()],
                            field_reads: Vec::new(),
                            owner_class: None,
                            content_hash: 0,
                        },
                        // Implementor → IThing exercises the Implements branch
                        // of the kind-based heritage dispatch (§8.2).
                        RawNode {
                            name: "Implementor".into(),
                            kind: NodeKind::Class,
                            span: (12, 0, 20, 0),
                            is_exported: true,
                            heritage: vec!["IThing".into()],
                            type_annotation: None,
                            decorators: vec![],
                            calls: vec![],
                            field_reads: Vec::new(),
                            owner_class: None,
                            content_hash: 0,
                        },
                    ],
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
                    schema_fields: None,
                    event_topics: None,
                    tx_scopes: None,
                    path_literals: None,
                    sql_refs: None,
                    call_metas: vec![],
                    raw_function_metas: vec![],
                    export_aliases: None,
                },
                LocalGraph {
                    file_path: "src/bar.rs".into(),
                    content_hash: [0; 8],
                    nodes: vec![
                        RawNode {
                            name: "Bar".into(),
                            kind: NodeKind::Class,
                            span: (0, 0, 5, 0),
                            is_exported: true,
                            heritage: vec![],
                            type_annotation: None,
                            // Fixture: provides a Decorates-emit data point so
                            // the coverage assertion below can guard against
                            // regression when decorates_edges.rs is modified.
                            decorators: vec!["@MyAnnotation".into()],
                            calls: vec![],
                            field_reads: Vec::new(),
                            owner_class: None,
                            content_hash: 0,
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
                            field_reads: Vec::new(),
                            owner_class: None,
                            content_hash: 0,
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
                            field_reads: Vec::new(),
                            owner_class: None,
                            content_hash: 0,
                        },
                    ],
                    documents: vec![],
                    imports: vec![],
                    routes: vec![RawRoute {
                        method: "GET".into(),
                        path: "/users".into(),
                        handler: Some("other_fn".into()),
                        span: (20, 0, 20, 30),
                        scope: None,
                    }],
                    framework_refs: vec![],
                    fanout_refs: vec![],
                    blind_spots: vec![],
                    schema_fields: None,
                    event_topics: None,
                    tx_scopes: None,
                    path_literals: None,
                    sql_refs: None,
                    call_metas: vec![],
                    raw_function_metas: vec![],
                    export_aliases: None,
                },
                // iface.rs: defines Interface IThing so the Implementor →
                // IThing heritage edge dispatches to Implements (not Extends).
                LocalGraph {
                    file_path: "src/iface.rs".into(),
                    content_hash: [0; 8],
                    nodes: vec![RawNode {
                        name: "IThing".into(),
                        kind: NodeKind::Interface,
                        span: (0, 0, 3, 0),
                        is_exported: true,
                        heritage: vec![],
                        type_annotation: None,
                        decorators: vec![],
                        calls: vec![],
                        field_reads: Vec::new(),
                        owner_class: None,
                        content_hash: 0,
                    }],
                    documents: vec![],
                    imports: vec![],
                    routes: vec![],
                    framework_refs: vec![],
                    fanout_refs: vec![],
                    blind_spots: vec![],
                    schema_fields: None,
                    event_topics: None,
                    tx_scopes: None,
                    path_literals: None,
                    sql_refs: None,
                    call_metas: vec![],
                    raw_function_metas: vec![],
                    export_aliases: None,
                },
            ]
        }

        // Shared tmp dir: Pass 1.6b needs consumer file content on disk
        // to emit Fetches edges (route data comes from LocalGraph, not disk).
        let tmp = tempfile::TempDir::new().unwrap();
        // Consumer file fetches GET /users → matches the RawRoute in bar.rs fixture.
        std::fs::write(
            tmp.path().join("consumer.ts"),
            "async function load() { const { id } = await fetch('/users').json(); }",
        )
        .unwrap();

        // Parallel path (production): no dump enabled
        let mut parallel_builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
        for lg in build_fixtures() {
            parallel_builder.add_graph(lg);
        }
        // Consumer LocalGraph for the consumer.ts file written above.
        parallel_builder.add_graph(consumer_local_graph("consumer.ts"));
        let parallel_graph = parallel_builder.build();

        // Serial path: dump enabled forces the serial branch
        let dump_path = tmp.path().join("dump.jsonl");
        let mut serial_builder = GraphBuilder::new()
            .with_repo_root(tmp.path().to_path_buf())
            .with_resolver_dump(Some(dump_path.clone()));
        for lg in build_fixtures() {
            serial_builder.add_graph(lg);
        }
        serial_builder.add_graph(consumer_local_graph("consumer.ts"));
        let serial_graph = serial_builder.build();

        // Bucketize edges per RelType; include resolved reason in the key so a
        // diverging reason on the same (source, target) pair is caught.
        // `RelType` doesn't derive `Ord`, so `format!("{:?}", …)` is used as a
        // stable string key for the BTreeMap — the Debug repr of each variant
        // is its identifier name and is not subject to drift.
        let bucketize =
            |g: &ecp_core::graph::ZeroCopyGraph| -> BTreeMap<String, BTreeSet<(u32, u32, String)>> {
                let mut buckets: BTreeMap<String, BTreeSet<(u32, u32, String)>> = BTreeMap::new();
                for e in &g.edges {
                    let key = format!("{:?}", e.rel_type);
                    let reason = e.reason.resolve(&g.string_pool).to_string();
                    buckets
                        .entry(key)
                        .or_default()
                        .insert((e.source, e.target, reason));
                }
                buckets
            };

        let parallel_buckets = bucketize(&parallel_graph);
        let serial_buckets = bucketize(&serial_graph);

        // RelType key sets must match before per-bucket comparison.
        let p_keys: Vec<_> = parallel_buckets.keys().cloned().collect();
        let s_keys: Vec<_> = serial_buckets.keys().cloned().collect();
        assert_eq!(
            p_keys, s_keys,
            "parallel vs serial produced different RelType sets"
        );

        // Per-RelType equality — divergence is localised to the failing bucket.
        for (rel, p_edges) in &parallel_buckets {
            let s_edges = serial_buckets.get(rel).expect("rel exists in both");
            assert_eq!(
                p_edges, s_edges,
                "parallel vs serial diverged on RelType {rel}",
            );
        }

        // Emit-zero invariant: only `Imports` remains intentionally post-process
        // only — see `crates/ecp-analyzer/src/post_process/imports_edges.rs`
        // Tier-1-2-3 resolver for cross-file basename / dir-component indexing
        // that can't be moved to Pass-2 without breaking its streaming design.
        // `Implements` lifted §8.2, `Defines` §8.4, `Fetches` (this PR) §8.5.

        // Imports is intentionally post-process only (imports_edges.rs Tier-1-2-3
        // resolver); Pass-2 must never emit it.
        assert!(
            !parallel_buckets.contains_key("Imports"),
            "RelType Imports unexpectedly emitted by Pass-2 — Imports is intentionally \
             post-process only; see imports_edges.rs. Did you accidentally add a \
             Pass-2 emit site?",
        );

        // Node counts identical (both paths build identical SymbolTable + StringPool)
        assert_eq!(parallel_graph.nodes.len(), serial_graph.nodes.len());

        // Sanity: dump file actually exists for the serial run (proves the
        // serial branch was the one taken).
        assert!(dump_path.exists(), "serial dump path was not taken");

        // Fixture coverage: assert each expected category fired at least once.
        // Implements + Defines + Fetches + Decorates all required — covers
        // IThing Interface fixture, scope_defines post-process, RouteIndex
        // post-Pass-2 join, and Decorates post-process.
        for required in &[
            "Calls",
            "Extends",
            "Implements",
            "Accesses",
            "References",
            "HandlesRoute",
            "Defines",
            "Fetches",
            "Decorates",
        ] {
            assert!(
                parallel_buckets.contains_key(*required),
                "fixture failed to trigger {required} emit",
            );
        }
    }

    // ─── Pass 1.6: fetch-shape extraction ──────────────────────────────────

    /// Helper: materialise a file under `repo` and return a `LocalGraph` whose
    /// `file_path` is the relative form (matches the production
    /// `analyze.rs` flow). The graph carries an imperative-style
    /// `RawRoute` so Pass 1.5 emits a Route node we can attach a shape to.
    fn route_local_graph(
        rel_path: &str,
        method: &str,
        route_path: &str,
        handler: &str,
    ) -> LocalGraph {
        LocalGraph {
            file_path: rel_path.into(),
            content_hash: [0; 8],
            nodes: vec![RawNode {
                name: handler.into(),
                kind: NodeKind::Function,
                span: (0, 0, 0, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
            }],
            documents: vec![],
            imports: vec![],
            routes: vec![ecp_core::analyzer::types::RawRoute {
                method: method.into(),
                path: route_path.into(),
                handler: Some(handler.into()),
                span: (0, 0, 0, 0),
                scope: None,
            }],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: vec![],
            raw_function_metas: vec![],
            export_aliases: None,
        }
    }

    fn consumer_local_graph(rel_path: &str) -> LocalGraph {
        LocalGraph {
            file_path: rel_path.into(),
            content_hash: [0; 8],
            nodes: vec![RawNode {
                name: "loadUsers".into(),
                kind: NodeKind::Function,
                span: (0, 0, 0, 0),
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
            }],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![],
            fanout_refs: vec![],
            blind_spots: vec![],
            schema_fields: None,
            event_topics: None,
            tx_scopes: None,
            path_literals: None,
            sql_refs: None,
            call_metas: vec![],
            raw_function_metas: vec![],
            export_aliases: None,
        }
    }

    /// Resolve a `StrRef` against the graph's archived string pool to a
    /// `String` so test assertions don't need to juggle byte offsets.
    fn s(graph: &ZeroCopyGraph, sref: StrRef) -> String {
        let start = sref.offset as usize;
        let end = start + sref.len as usize;
        std::str::from_utf8(&graph.string_pool[start..end])
            .expect("utf-8 in pool")
            .to_string()
    }

    /// gRPC end-to-end: a `RawRoute { method: "GRPC", path: "/pkg.Svc/M" }`
    /// (as the protobuf provider emits from a `service { rpc }` block) must
    /// survive `detect_from_call` and land as a `NodeKind::Route`. Regression
    /// for the HTTP-only gatekeeper that previously dropped every gRPC route.
    #[test]
    fn grpc_raw_route_promotes_to_route_node() {
        let mut builder = GraphBuilder::new();
        builder.add_graph(route_local_graph(
            "svc.proto",
            "GRPC",
            "/helloworld.Greeter/SayHello",
            "SayHello",
        ));
        let graph = builder.build();

        let route_nodes: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Route)
            .collect();
        assert_eq!(route_nodes.len(), 1, "exactly one gRPC Route node");
        assert_eq!(
            s(&graph, route_nodes[0].name),
            "GRPC /helloworld.Greeter/SayHello"
        );
    }

    /// TS route emitting `res.json({ id, name })` → RouteShape with
    /// response_keys `["id", "name"]` (sorted). Locks in that Pass 1.6a
    /// reads the source via `repo_root` and runs `response_shapes::extract`.
    #[test]
    fn ts_route_handler_emits_route_shape_response_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let route_file = "api/users.ts";
        std::fs::create_dir_all(tmp.path().join("api")).unwrap();
        std::fs::write(
            tmp.path().join(route_file),
            "function getUsers(req, res) { res.json({ id, name }); }",
        )
        .unwrap();

        let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
        builder.add_graph(route_local_graph(route_file, "get", "/users", "getUsers"));
        let graph = builder.build();

        assert_eq!(
            graph.route_shapes.len(),
            1,
            "expected exactly one RouteShape; got {}",
            graph.route_shapes.len()
        );
        let shape = &graph.route_shapes[0];
        // The Route node must be the one this shape points at.
        let route_node = &graph.nodes[shape.node_idx as usize];
        assert_eq!(route_node.kind, NodeKind::Route);

        let response_keys: Vec<String> =
            shape.response_keys.iter().map(|r| s(&graph, *r)).collect();
        assert_eq!(response_keys, vec!["id".to_string(), "name".to_string()]);
        assert!(
            shape.error_keys.is_empty(),
            "no error keys on 2xx handler; got {:?}",
            shape
                .error_keys
                .iter()
                .map(|r| s(&graph, *r))
                .collect::<Vec<_>>()
        );
    }

    /// Consumer file with `fetch('/users')` + `data.id` access → a
    /// `RelType::Fetches` edge whose reason is `fetch-url-match|keys:id`.
    /// Covers the full 1.6b flow: URL extraction, route-table match,
    /// consumer-key extraction, reason formatting.
    #[test]
    fn ts_consumer_emits_fetches_edge_with_keyed_reason() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("api.ts"),
            "function getUsers(req, res) { res.json({ id, name }); }",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("client.ts"),
            "async function loadUsers() { const { id } = await fetch('/users').json(); }",
        )
        .unwrap();

        let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
        builder.add_graph(route_local_graph("api.ts", "get", "/users", "getUsers"));
        builder.add_graph(consumer_local_graph("client.ts"));
        let graph = builder.build();

        let fetches: Vec<_> = graph
            .edges
            .iter()
            .filter(|e| e.rel_type == RelType::Fetches)
            .collect();
        assert_eq!(
            fetches.len(),
            1,
            "expected exactly one Fetches edge; got {} (edges: {:?})",
            fetches.len(),
            graph
                .edges
                .iter()
                .map(|e| (e.source, e.target, format!("{:?}", e.rel_type)))
                .collect::<Vec<_>>()
        );
        let edge = fetches[0];
        assert!(
            (edge.confidence - 0.8).abs() < 1e-6,
            "Fetches confidence must be 0.8 for exact-path matches; got {}",
            edge.confidence
        );
        // Target must be the Route node.
        assert_eq!(graph.nodes[edge.target as usize].kind, NodeKind::Route);

        let reason = s(&graph, edge.reason);
        assert_eq!(reason, "fetch-url-match|keys:id");
    }

    /// PHP route emitting `json_encode(['id' => $x])` → RouteShape
    /// with response_keys `["id"]`. Covers the Lang::Php branch of
    /// `response_shapes::extract` end-to-end through the builder.
    #[test]
    fn php_route_handler_emits_route_shape_response_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let route_file = "api/show.php";
        std::fs::create_dir_all(tmp.path().join("api")).unwrap();
        std::fs::write(
            tmp.path().join(route_file),
            "<?php $x = 1; echo json_encode(['id' => $x]); ?>",
        )
        .unwrap();

        let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
        builder.add_graph(route_local_graph(route_file, "get", "/show", "showHandler"));
        let graph = builder.build();

        assert_eq!(
            graph.route_shapes.len(),
            1,
            "expected exactly one RouteShape; got {}",
            graph.route_shapes.len()
        );
        let shape = &graph.route_shapes[0];
        let response_keys: Vec<String> =
            shape.response_keys.iter().map(|r| s(&graph, *r)).collect();
        assert_eq!(response_keys, vec!["id".to_string()]);
    }
}
