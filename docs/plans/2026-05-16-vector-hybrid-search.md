# Vector + Hybrid Search Wire-Up — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the three vector/hybrid stubs in `crates/graph-nexus-cli/src/commands/search.rs` with a real cosine-similarity path against `ZeroCopyGraph::embeddings`, fused with BM25 via Reciprocal Rank Fusion (k=60), shared across rayon workers via a process-local `OnceLock<Embedder>`.

**Architecture:** New `crates/graph-nexus-cli/src/embedder.rs` exposes `get_embedder() -> Result<&'static Embedder, GnxError>`. `search.rs` gets three private helpers (`cosine_top_k_indices`, `vector_hits_from_graph`, `hybrid_hits_from_graph`) plus a tiny `l2_norm` + `rrf_merge`. All failure modes fall back to BM25 + stderr hint so the hook contract stays intact.

**Tech Stack:** Rust, rayon, std::sync::OnceLock, fastembed (BGE-M3 INT8 via `graph-nexus-analyzer::embeddings`), rkyv for archived graph access.

**Spec:** `docs/specs/2026-05-16-vector-hybrid-search-design.md` (committed 2b233da)

---

### Task 1: Add `embedder.rs` accessor module

**Files:**
- Create: `crates/graph-nexus-cli/src/embedder.rs`
- Modify: `crates/graph-nexus-cli/src/lib.rs:1-15`

- [ ] **Step 1: Create the embedder module**

Create `crates/graph-nexus-cli/src/embedder.rs` with content:

```rust
//! Process-shared embedder accessor.
//!
//! `Embedder::new()` is a 1–2s cold start (after the ~1.2 GB model is
//! cached on disk). Hooks fork a fresh process per Claude Code tool
//! call, so this `OnceLock` does NOT help the hook path — but it does
//! deduplicate cold-start across rayon workers in a single multi-repo
//! `gnx search` invocation.

use graph_nexus_analyzer::embeddings::Embedder;
use graph_nexus_core::GnxError;
use std::sync::OnceLock;

/// Returns a process-shared `&Embedder` initialised on first call.
/// On init failure (no model + offline, ONNX runtime hiccup) returns
/// `GnxError::Rkyv` carrying the underlying error string so callers
/// can `?`-fallback to BM25 cleanly.
pub fn get_embedder() -> Result<&'static Embedder, GnxError> {
    static CELL: OnceLock<Result<Embedder, String>> = OnceLock::new();
    let slot = CELL.get_or_init(|| Embedder::new().map_err(|e| e.to_string()));
    slot.as_ref()
        .map_err(|e| GnxError::Rkyv(format!("embedder init: {e}")))
}
```

- [ ] **Step 2: Register the module in `lib.rs`**

Edit `crates/graph-nexus-cli/src/lib.rs` to add `pub mod embedder;` in alphabetical order with the other module declarations (between `pub mod commands;` and `pub mod engine;`):

```rust
pub mod admin;
pub mod auto_ensure;
pub mod background;
pub mod commands;
pub mod config_parser;
pub mod embedder;
pub mod engine;
pub mod git;
pub mod git_state;
pub mod graph_path;
pub mod hint;
pub mod incremental_cache;
pub mod output;
pub mod reanalyze;
pub mod repo_selector;
pub mod search;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p graph-nexus-cli`
Expected: PASS with no warnings related to `embedder`.

- [ ] **Step 4: Commit**

```bash
git add crates/graph-nexus-cli/src/embedder.rs crates/graph-nexus-cli/src/lib.rs
git commit -m "feat(search): add process-shared Embedder accessor"
```

---

### Task 2: Add `l2_norm` + `cosine_top_k_indices` with unit tests

**Rationale:** Splitting the ranking step from `build_hit` materialisation makes the scoring testable without constructing an archived `ZeroCopyGraph`.

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/search.rs` (add helpers before the `// ── Multi-repo fan-out` section, around L398)

- [ ] **Step 1: Write the failing test for `l2_norm`**

Append to `crates/graph-nexus-cli/src/commands/search.rs` inside the existing `#[cfg(test)] mod tests` block (find it near the bottom of the file, after the `detect_mode_*` tests):

```rust
    #[test]
    fn l2_norm_handles_zero_vec() {
        assert_eq!(super::l2_norm(&[]), 0.0);
        assert_eq!(super::l2_norm(&[0.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn l2_norm_unit_vec_is_one() {
        let v = [1.0_f32 / 3.0_f32.sqrt(); 3];
        assert!((super::l2_norm(&v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_top_k_indices_ranks_by_similarity() {
        // 4 nodes, 3-dim embeddings. Query points at node 1.
        let embs = vec![
            vec![1.0, 0.0, 0.0], // node 0 — orthogonal to query
            vec![0.0, 1.0, 0.0], // node 1 — identical direction
            vec![0.0, 0.7, 0.7], // node 2 — partially aligned
            vec![0.0, 0.0, 0.0], // node 3 — skip-marker zero vec
        ];
        let query = vec![0.0, 1.0, 0.0];
        let ranked = super::cosine_top_k_indices(&embs, &query, 3);
        assert_eq!(ranked[0].0, 1, "node 1 should rank first");
        assert_eq!(ranked[1].0, 2, "node 2 should rank second");
        // node 3 must be excluded (zero norm), node 0 has sim=0 so dropped.
        assert!(ranked.iter().all(|(idx, _)| *idx != 3));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p graph-nexus-cli --lib search::tests::l2_norm_handles_zero_vec`
Expected: FAIL with `cannot find function l2_norm in module super`

- [ ] **Step 3: Implement `l2_norm` and `cosine_top_k_indices`**

Add to `crates/graph-nexus-cli/src/commands/search.rs` just before the `// ── Multi-repo fan-out ─` divider (search for that comment, around L399):

```rust
// ── Vector scoring primitives ────────────────────────────────────────────────

/// Plain L2 norm. Returns 0.0 for empty input or an all-zero vector.
pub(crate) fn l2_norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// Score `embeddings[i]` against `query` via cosine similarity, drop
/// zero-norm and non-positive entries, and return the top-`k` as
/// `(node_idx, similarity)` sorted descending by similarity.
///
/// Used by tests (with `&[Vec<f32>]`) and by `vector_hits_from_graph`
/// via an archived-view wrapper. Skip-marker zero embeddings produced
/// at build time get filtered here.
pub(crate) fn cosine_top_k_indices(
    embeddings: &[Vec<f32>],
    query: &[f32],
    k: usize,
) -> Vec<(usize, f32)> {
    let q_norm = l2_norm(query);
    if q_norm == 0.0 {
        return Vec::new();
    }

    let scored: Vec<(usize, f32)> = embeddings
        .par_iter()
        .enumerate()
        .filter_map(|(idx, emb)| {
            let dot: f32 = emb.iter().zip(query.iter()).map(|(a, b)| a * b).sum();
            let denom = l2_norm(emb) * q_norm;
            if denom == 0.0 {
                return None;
            }
            let sim = dot / denom;
            (sim > 0.0).then_some((idx, sim))
        })
        .collect();

    // Top-K heap merge using f32-as-u32 surrogate key.
    let mut heap: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::with_capacity(k + 1);
    for (idx, sim) in scored {
        heap.push(Reverse((sim.to_bits(), idx)));
        if heap.len() > k {
            heap.pop();
        }
    }

    let mut out: Vec<(usize, f32)> = heap
        .into_iter()
        .map(|r| (r.0 .1, f32::from_bits(r.0 .0)))
        .collect();
    out.sort_by(|a, b| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p graph-nexus-cli --lib search::tests::l2_norm`
Expected: PASS (both `l2_norm_*` tests)

Run: `cargo test -p graph-nexus-cli --lib search::tests::cosine_top_k_indices_ranks_by_similarity`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/search.rs
git commit -m "feat(search): cosine_top_k_indices + l2_norm primitives"
```

---

### Task 3: Add `rrf_merge` with unit tests

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/search.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block:

```rust
    fn make_test_hit(name: &str, file: &str, line: u32, score: f32) -> super::Hit {
        super::Hit {
            repo: None,
            score,
            kind: "function".into(),
            file: file.into(),
            line,
            name: name.into(),
            signature: format!("function {name}"),
            caller_count: 0,
            callers: vec![],
            callees: vec![],
        }
    }

    #[test]
    fn rrf_merge_combines_two_ranked_lists() {
        // bm25 ranks: [A, B, C], vec ranks: [B, A, D]
        let bm25 = vec![
            make_test_hit("A", "a.rs", 1, 10.0),
            make_test_hit("B", "b.rs", 2, 8.0),
            make_test_hit("C", "c.rs", 3, 6.0),
        ];
        let vec = vec![
            make_test_hit("B", "b.rs", 2, 0.9),
            make_test_hit("A", "a.rs", 1, 0.8),
            make_test_hit("D", "d.rs", 4, 0.7),
        ];
        let merged = super::rrf_merge(bm25, vec);

        // A in both: 1/(60+1) + 1/(60+2) = ~0.0327
        // B in both: 1/(60+2) + 1/(60+1) = ~0.0327
        // C bm25 only at rank 3: 1/(60+3) = ~0.0159
        // D vec only at rank 3: 1/(60+3) = ~0.0159
        // → expected order: A and B tied at top, then C and D (order between
        //   tied entries is unspecified; just assert membership of top-2).
        let top_names: Vec<&str> =
            merged.iter().take(2).map(|h| h.name.as_str()).collect();
        assert!(top_names.contains(&"A") && top_names.contains(&"B"));
        assert_eq!(merged.len(), 4);
    }

    #[test]
    fn rrf_merge_dedupes_by_file_line_name() {
        let bm25 = vec![make_test_hit("A", "a.rs", 1, 5.0)];
        let vec = vec![make_test_hit("A", "a.rs", 1, 0.9)];
        let merged = super::rrf_merge(bm25, vec);
        assert_eq!(merged.len(), 1);
        // 1/(60+1) + 1/(60+1) = 2/61 ≈ 0.0328
        assert!((merged[0].score - (2.0 / 61.0)).abs() < 1e-6);
    }

    #[test]
    fn rrf_merge_truncates_to_top_k() {
        // Generate 30 distinct hits in bm25 only; merge should return TOP_K (20).
        let bm25: Vec<super::Hit> = (0..30)
            .map(|i| make_test_hit(&format!("n{i}"), "x.rs", i, 1.0))
            .collect();
        let merged = super::rrf_merge(bm25, vec![]);
        assert_eq!(merged.len(), super::TOP_K);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p graph-nexus-cli --lib search::tests::rrf_merge`
Expected: FAIL with `cannot find function rrf_merge`

- [ ] **Step 3: Implement `rrf_merge`**

Add to `crates/graph-nexus-cli/src/commands/search.rs` immediately after `cosine_top_k_indices` (before the `// ── Multi-repo fan-out ─` divider):

```rust
/// Reciprocal Rank Fusion constant. k=60 is the Cormack et al. 2009
/// default and is the parameter used by Elasticsearch / Vespa /
/// Weaviate for hybrid retrieval. Hard-wired for now; add a flag if
/// we ever need to tune per query type.
const RRF_K: f32 = 60.0;

/// Fuse two ranked `Vec<Hit>` lists by Reciprocal Rank Fusion:
/// `score(uid) = Σ 1/(RRF_K + rank_i + 1)` over the lists that contain
/// `uid`. Output sorted descending by combined score, truncated to
/// `TOP_K`. The merged Hit's `score` field is overwritten with the RRF
/// score so emit / serialise layers see the fused number.
///
/// Dedup key: `(file, line, name)`. Stable within a single graph,
/// which is the only context this helper runs in. Multi-repo merge
/// happens later in `compute_multi`, which keys on the full
/// `OrderedHit` including `repo`.
pub(crate) fn rrf_merge(bm25: Vec<Hit>, vec: Vec<Hit>) -> Vec<Hit> {
    type Key = (String, u32, String);
    let key = |h: &Hit| -> Key { (h.file.clone(), h.line, h.name.clone()) };

    let mut scores: HashMap<Key, (f32, Hit)> = HashMap::new();

    for (rank, h) in bm25.into_iter().enumerate() {
        let s = 1.0 / (RRF_K + rank as f32 + 1.0);
        scores
            .entry(key(&h))
            .and_modify(|e| e.0 += s)
            .or_insert_with(|| (s, h));
    }
    for (rank, h) in vec.into_iter().enumerate() {
        let s = 1.0 / (RRF_K + rank as f32 + 1.0);
        scores
            .entry(key(&h))
            .and_modify(|e| e.0 += s)
            .or_insert_with(|| (s, h));
    }

    let mut combined: Vec<(f32, Hit)> = scores.into_values().collect();
    combined.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    combined
        .into_iter()
        .take(TOP_K)
        .map(|(score, mut h)| {
            h.score = score;
            h
        })
        .collect()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p graph-nexus-cli --lib search::tests::rrf_merge`
Expected: PASS (all three rrf_merge tests)

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/search.rs
git commit -m "feat(search): rrf_merge for hybrid score fusion"
```

---

### Task 4: Add `vector_hits_from_graph` (production wrapper)

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/search.rs`

- [ ] **Step 1: Add the helper**

Insert immediately before the `// ── Multi-repo fan-out ─` divider (after `rrf_merge` from Task 3):

```rust
/// Vector path: embed the query, score every node embedding by cosine,
/// materialise `Hit` rows for the top-K survivors. All failure modes
/// degrade to BM25 + a stderr hint — the hook contract requires that
/// search NEVER errors out.
fn vector_hits_from_graph(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
    index_dir: Option<&std::path::Path>,
) -> Vec<Hit> {
    let Some(archived_embs) = graph.embeddings.as_ref() else {
        eprintln!(
            "→ vector: graph has no embeddings — falling back to bm25 \
             (rebuild with `gnx admin index --embeddings`)"
        );
        return bm25_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
    };

    let embedder = match crate::embedder::get_embedder() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("→ vector: embedder unavailable ({e}) — falling back to bm25");
            return bm25_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
        }
    };

    let query_vec = match embedder.embed(vec![pattern.to_string()]) {
        Ok(mut vs) if !vs.is_empty() => vs.swap_remove(0),
        _ => {
            eprintln!("→ vector: query embed failed — falling back to bm25");
            return bm25_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
        }
    };

    // One-off debug log so we can confirm BGE-M3 / fastembed output
    // norm assumption post-merge. Cheap (single sum), only fires once
    // per CLI invocation in practice.
    tracing::debug!(query_norm = l2_norm(&query_vec), "vector query embedded");

    // Materialise archived → Vec<Vec<f32>> for the ranking helper.
    // Cost: ~5k × 1024 × 4 bytes ≈ 20 MB allocation per call. We could
    // iterate the archived view directly via a thin wrapper but the
    // ranking step is allocator-light and this keeps the test seam
    // (`cosine_top_k_indices`) clean.
    let owned_embs: Vec<Vec<f32>> = archived_embs
        .iter()
        .map(|v| v.iter().map(|x| x.to_native()).collect())
        .collect();

    let ranked = cosine_top_k_indices(&owned_embs, &query_vec, TOP_K);

    ranked
        .into_iter()
        .filter_map(|(idx, score)| build_hit(graph, idx, score, kind_set, repo_label))
        .collect()
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p graph-nexus-cli`
Expected: PASS. If a `tracing` import warning fires, add `use tracing;` at the file's import block or scope it inline.

- [ ] **Step 3: Add the fallback regression test**

Create new test file `crates/graph-nexus-cli/tests/search_vector_fallback.rs`:

```rust
//! Regression: vector mode must degrade to BM25 (not crash) when
//! `graph.embeddings == None`. Mirrors the hook contract that search
//! never errors out — the cost of an unindexed graph is a stderr
//! warning, not a missing tool result.

use graph_nexus_cli::commands::search::{compute_hits, SearchArgs, SearchMode};
use graph_nexus_cli::engine::Engine;
use graph_nexus_core::graph::{
    Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph, GRAPH_FORMAT_VERSION,
    GRAPH_MAGIC,
};
use graph_nexus_core::pool::StringPool;
use rkyv::rancor::Error;
use std::fs;
use tempfile::tempdir;

fn make_minimal_graph(with_embeddings: bool) -> ZeroCopyGraph {
    let mut pool = StringPool::new();
    let file_ref = pool.add("src/lib.rs");
    let name_ref = pool.add("validateUser");
    let uid_ref = pool.add("Function:src/lib.rs:validateUser");
    let nodes = vec![Node {
        uid: uid_ref,
        name: name_ref,
        file_idx: 0,
        kind: NodeKind::Function,
        span: (0, 0, 1, 0),
        community_id: 0,
    }];
    ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![File {
            path: file_ref,
            mtime: 0,
            content_hash: [0; 32],
            category: FileCategory::Source,
        }],
        nodes,
        edges: vec![],
        out_offsets: vec![0, 0],
        in_offsets: vec![0, 0],
        in_edge_idx: vec![],
        name_index: vec![0],
        embeddings: if with_embeddings {
            Some(vec![vec![0.5; 1024]])
        } else {
            None
        },
        process_start: 1,
        traces_offsets: vec![],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
    }
}

#[test]
fn vector_falls_back_to_bm25_when_embeddings_missing() {
    let dir = tempdir().unwrap();
    let graph_path = dir.path().join("graph.bin");
    let graph = make_minimal_graph(/* with_embeddings = */ false);
    let bytes = rkyv::to_bytes::<Error>(&graph).unwrap();
    fs::write(&graph_path, bytes).unwrap();

    let engine = Engine::load(graph_path).unwrap();
    let args = SearchArgs {
        pattern: "validateUser".into(),
        mode: SearchMode::Vector,
        kind: None,
        repo: None,
        format: None,
    };
    // MUST NOT panic and MUST return the substring/BM25 hit for "validateUser".
    let hits = compute_hits(args, &engine).expect("compute_hits Err");
    assert!(
        hits.iter().any(|h| h.name == "validateUser"),
        "expected BM25 fallback to surface validateUser, got {:?}",
        hits.iter().map(|h| &h.name).collect::<Vec<_>>()
    );
}

#[test]
fn hybrid_falls_back_to_bm25_when_embeddings_missing() {
    let dir = tempdir().unwrap();
    let graph_path = dir.path().join("graph.bin");
    let graph = make_minimal_graph(/* with_embeddings = */ false);
    let bytes = rkyv::to_bytes::<Error>(&graph).unwrap();
    fs::write(&graph_path, bytes).unwrap();

    let engine = Engine::load(graph_path).unwrap();
    let args = SearchArgs {
        pattern: "validateUser".into(),
        mode: SearchMode::Hybrid,
        kind: None,
        repo: None,
        format: None,
    };
    let hits = compute_hits(args, &engine).expect("compute_hits Err");
    assert!(
        hits.iter().any(|h| h.name == "validateUser"),
        "expected hybrid → BM25 fallback to surface validateUser"
    );
}
```

- [ ] **Step 4: Run the fallback tests**

Run: `cargo test -p graph-nexus-cli --test search_vector_fallback`
Expected: FAIL — the test compiles but `SearchMode::Vector` still routes to the stub. We're about to wire it. (If it passes already by coincidence — i.e. the stub falls back to BM25 too — note that and the test will still be correct post-wiring.)

- [ ] **Step 5: Commit (helper only, not yet wired)**

```bash
git add crates/graph-nexus-cli/src/commands/search.rs crates/graph-nexus-cli/tests/search_vector_fallback.rs
git commit -m "feat(search): vector_hits_from_graph helper + fallback tests"
```

---

### Task 5: Add `hybrid_hits_from_graph`

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/search.rs`

- [ ] **Step 1: Add the helper**

Insert immediately after `vector_hits_from_graph` (still before the `// ── Multi-repo fan-out ─` divider):

```rust
/// Hybrid path: run BM25 and vector, then fuse via RRF. Short-circuits
/// to BM25 when the graph has no embeddings — vector_hits_from_graph
/// would do this anyway, but the short-circuit avoids running BM25
/// twice for the same query.
fn hybrid_hits_from_graph(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
    index_dir: Option<&std::path::Path>,
) -> Vec<Hit> {
    if graph.embeddings.is_none() {
        eprintln!(
            "→ hybrid: graph has no embeddings — falling back to bm25 \
             (rebuild with `gnx admin index --embeddings`)"
        );
        return bm25_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
    }

    let bm25 = bm25_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
    let vec = vector_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
    rrf_merge(bm25, vec)
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check -p graph-nexus-cli`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/search.rs
git commit -m "feat(search): hybrid_hits_from_graph via RRF fusion"
```

---

### Task 6: Wire the match arms in `compute_single`

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/search.rs:211-225`

- [ ] **Step 1: Replace the stubs**

Open `crates/graph-nexus-cli/src/commands/search.rs`. Find the block:

```rust
    let mut hits = match effective_mode {
        SearchMode::Bm25 | SearchMode::Auto => {
            bm25_hits_from_graph(graph, pattern, &kind_set, &repo_label, index_dir)
        }
        SearchMode::Vector => {
            // TODO: wire to real cosine path (graph_nexus_analyzer::embeddings)
            eprintln!("→ vector mode not yet wired — falling back to bm25");
            bm25_hits_from_graph(graph, pattern, &kind_set, &repo_label, index_dir)
        }
        SearchMode::Hybrid => {
            // TODO: fold bm25 + cosine scores when embeddings are wired
            eprintln!("→ hybrid: embeddings not wired — using bm25");
            bm25_hits_from_graph(graph, pattern, &kind_set, &repo_label, index_dir)
        }
    };
```

Replace with:

```rust
    let mut hits = match effective_mode {
        SearchMode::Bm25 | SearchMode::Auto => {
            bm25_hits_from_graph(graph, pattern, &kind_set, &repo_label, index_dir)
        }
        SearchMode::Vector => {
            vector_hits_from_graph(graph, pattern, &kind_set, &repo_label, index_dir)
        }
        SearchMode::Hybrid => {
            hybrid_hits_from_graph(graph, pattern, &kind_set, &repo_label, index_dir)
        }
    };
```

- [ ] **Step 2: Run the fallback tests**

Run: `cargo test -p graph-nexus-cli --test search_vector_fallback`
Expected: PASS (both `vector_falls_back_to_bm25_when_embeddings_missing` and `hybrid_falls_back_to_bm25_when_embeddings_missing`).

- [ ] **Step 3: Run the rest of the crate's tests to catch regressions**

Run: `cargo test -p graph-nexus-cli --lib search`
Expected: PASS (all existing `detect_mode_*` and other search-related tests).

- [ ] **Step 4: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/search.rs
git commit -m "feat(search): wire SearchMode::Vector + Hybrid in compute_single"
```

---

### Task 7: Wire `scan_repo` (multi-repo fan-out)

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/search.rs:515-544`

- [ ] **Step 1: Replace the dispatch block**

Open `crates/graph-nexus-cli/src/commands/search.rs`. Find `fn scan_repo` and locate the block:

```rust
    let effective_mode = match mode {
        SearchMode::Auto => detect_mode(pattern, embeddings_available_for(graph)),
        m => m.clone(),
    };

    // All modes except a real vector path fall through to bm25 for now.
    // TODO: wire vector/hybrid to graph_nexus_analyzer::embeddings.
    let _ = effective_mode;
    Ok(bm25_hits_from_graph(
        graph,
        pattern,
        kind_set,
        &Some(repo_name.to_string()),
        index_dir,
    ))
```

Replace with:

```rust
    let effective_mode = match mode {
        SearchMode::Auto => detect_mode(pattern, embeddings_available_for(graph)),
        m => m.clone(),
    };
    let repo_label = Some(repo_name.to_string());

    let hits = match effective_mode {
        SearchMode::Bm25 | SearchMode::Auto => {
            bm25_hits_from_graph(graph, pattern, kind_set, &repo_label, index_dir)
        }
        SearchMode::Vector => {
            vector_hits_from_graph(graph, pattern, kind_set, &repo_label, index_dir)
        }
        SearchMode::Hybrid => {
            hybrid_hits_from_graph(graph, pattern, kind_set, &repo_label, index_dir)
        }
    };
    Ok(hits)
```

- [ ] **Step 2: Verify compilation + tests**

Run: `cargo test -p graph-nexus-cli`
Expected: PASS — no test regressions (the change is dispatch-only).

- [ ] **Step 3: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/search.rs
git commit -m "feat(search): honour mode in multi-repo scan_repo dispatch"
```

---

### Task 8: Lint + full test sweep

**Files:** none (verification only)

- [ ] **Step 1: Format**

Run: `cargo fmt -p graph-nexus-cli`
Expected: no diff (or trivial whitespace). If formatting changes were made, commit them.

- [ ] **Step 2: Clippy**

Run: `cargo clippy -p graph-nexus-cli --all-targets -- -D warnings`
Expected: PASS with no warnings. Common things to fix if they fire:
- Unused imports (`tracing` if not pulled in elsewhere)
- `needless_lifetimes`, `redundant_closure` — fix per clippy suggestion

- [ ] **Step 3: Full test run for the cli crate**

Run: `cargo test -p graph-nexus-cli`
Expected: PASS. Specifically check:
- `search_vector_fallback::vector_falls_back_to_bm25_when_embeddings_missing`
- `search_vector_fallback::hybrid_falls_back_to_bm25_when_embeddings_missing`
- All `compute_hits_tantivy` tests (no regression)
- All `search::tests::*` (l2_norm, cosine_top_k_indices, rrf_merge)

- [ ] **Step 4: Commit any lint fixups**

```bash
git add -A
git diff --cached --stat   # confirm only fmt/clippy fixes
git commit -m "chore(search): cargo fmt + clippy fixups" || echo "no fixups needed"
```

---

### Task 9: Manual smoke test against a real embedded graph

**Files:** none (manual verification)

This step downloads the BGE-M3 model (~1.2 GB the first time) and exercises the full pipeline end-to-end. Run from the worktree directory.

- [ ] **Step 1: Pick a target repo**

Use an existing local repo with code (any language). Example: the worktree itself.

```bash
TARGET=/home/enor/gitnexus-rs/.claude/worktrees/feat-vector-hybrid-search
```

- [ ] **Step 2: Build a fresh embedded graph**

Run (will take a few minutes the first time; the model download is one-shot):

```bash
cargo run --release -p graph-nexus-cli -- admin index --embeddings --repo "$TARGET"
```
Expected: stderr shows `🧠 [graph-nexus] Initializing BGE-M3 INT8 ...` then `Generating embeddings for N nodes ...`, ending with `✓ Index refreshed`.

- [ ] **Step 3: Run vector search on a phrase query**

```bash
cargo run --release -p graph-nexus-cli -- search "compute cosine similarity between vectors" --mode vector --repo "$TARGET"
```
Expected: top hits should include the symbols `cosine_top_k_indices` and `l2_norm` (newly added in this PR), ranked above unrelated functions. Note the stderr line — should NOT include any "falling back to bm25" warning.

- [ ] **Step 4: Run hybrid search on a query that mixes slug + phrase**

```bash
cargo run --release -p graph-nexus-cli -- search "validateUser auth check" --mode hybrid --repo "$TARGET"
```
Expected: results blend BM25 lexical matches (anything literally named `validateUser`) with cosine semantic matches (auth-adjacent functions).

- [ ] **Step 5: Verify fallback warning**

Pick a repo that does NOT have embeddings (e.g. just `gnx admin index` without `--embeddings`):

```bash
cargo run --release -p graph-nexus-cli -- search "anything semantic" --mode vector --repo "$TARGET"
```
Expected: stderr `→ vector: graph has no embeddings — falling back to bm25 ...`, stdout shows BM25 results (not empty).

- [ ] **Step 6: Confirm hook path unchanged**

Trigger a Claude Code hook (or simulate via a Bash command containing `rg`) and confirm the hook still completes quickly. Pre-tool-use uses `SearchMode::Auto`, which `detect_mode` routes to BM25 for slug-like input, so the Embedder must NOT initialise.

Quick sanity check via the existing hook-dispatch test:

```bash
cargo test -p graph-nexus-cli --test hook_dispatch_test
```
Expected: PASS, no Embedder init in stderr.

- [ ] **Step 7: Commit a note if anything needed adjusting**

If smoke test surfaced an issue worth pinning (e.g. the BGE-M3 norm assumption), capture it. Otherwise no commit.

---

### Task 10: PR

**Files:** none

- [ ] **Step 1: Push the branch**

```bash
git push -u origin worktree-feat-vector-hybrid-search:feat/vector-hybrid-search
```

- [ ] **Step 2: Open the PR**

```bash
gh pr create --title "feat(search): wire SearchMode::Vector + Hybrid (RRF fusion)" --body "$(cat <<'EOF'
## Summary
- Replace the three vector/hybrid stubs in `crates/graph-nexus-cli/src/commands/search.rs` with a real BGE-M3 cosine path + RRF (k=60) fusion.
- Multi-repo fan-out (`scan_repo`) now honours `--mode`; previously it silently dropped everything to BM25.
- All failure modes (no embeddings, embedder init failure, embed call failure) fall back to BM25 + stderr hint, preserving the hook contract.

Spec: `docs/specs/2026-05-16-vector-hybrid-search-design.md`
Plan: `docs/plans/2026-05-16-vector-hybrid-search.md`

## Test plan
- [x] Unit: `l2_norm`, `cosine_top_k_indices` ranking + skip-marker exclusion
- [x] Unit: `rrf_merge` combine / dedup / top-K truncate
- [x] Integration: `search_vector_fallback` — vector + hybrid both degrade to BM25 when `embeddings == None`
- [x] No regression in `compute_hits_tantivy`, `hook_dispatch_test`, `detect_mode_*`
- [x] Manual smoke: `--mode vector` against an embedded local graph surfaces semantic hits; `--mode hybrid` blends BM25 + cosine

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 3: Capture the PR URL**

The `gh pr create` output is the PR URL — share it back to the user.

---

## Self-Review

Cross-checking against `docs/specs/2026-05-16-vector-hybrid-search-design.md`:

- ✅ Goal (3 stubs replaced) → Tasks 6 + 7
- ✅ `embedder.rs` accessor → Task 1
- ✅ Vector path → Task 4
- ✅ Cosine compute → Task 2
- ✅ Hybrid path (RRF) → Tasks 3 + 5
- ✅ Multi-repo `scan_repo` → Task 7
- ✅ `detect_mode` unchanged → no task needed (existing behaviour preserved)
- ✅ Error handling: graph None / embedder fail / embed fail → Task 4 + 5 (matches the spec's failure-mode table)
- ✅ Unit tests for cosine + RRF → Tasks 2 + 3
- ✅ Fallback tests → Task 4
- ⚠ Spec lists 6 unit tests + 1 ignored E2E test. Plan delivers 6 unit tests but folds the "ignored E2E" into a manual smoke step (Task 9) rather than a `#[ignore]` Rust test. Rationale: an `#[ignore]` test that's never run in CI is dead code; the manual smoke instructions are clearer and don't drift.
- ✅ Sequencing matches the spec's ordering.

No placeholder text remains. Types are consistent: `Hit`, `SearchArgs`, `SearchMode`, `ZeroCopyGraph`, `Engine`, `Embedder`, `OnceLock` used identically across tasks.

## Tasks 11–12 (added mid-PR for scope extension)

### Task 11: Preserve embeddings across auto-reindex

**Files:**
- Modify: `crates/graph-nexus-cli/src/auto_ensure.rs` — add `pub fn embeddings_present(graph_path: &Path) -> bool`; wire it into `ensure_fresh` so the synchronous rebuild keeps the previous state
- Modify: `crates/graph-nexus-cli/src/commands/hook/post_tool_use.rs` — pass `graph_path` to `spawn_background_reindex`; conditionally append `"--embeddings"` to the spawned-process args
- Create: `crates/graph-nexus-cli/tests/auto_ensure_embeddings.rs` — 3 unit tests (with embeddings, without, missing file)

Executed in commit `9d2203f`.

### Task 12: `gnx search --batch`

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/search.rs` — `SearchArgs::pattern: Option<String>` with `required_unless_present = "batch"`, new `batch: bool` flag, new `run_batch` dispatch; `compute_hits` rejects None pattern with `GnxError::InvalidArgument`
- Modify: `crates/graph-nexus-cli/src/commands/hook/pre_tool_use.rs` — pass `pattern: Some(pattern), batch: false`
- Modify: `crates/graph-nexus-cli/tests/compute_hits_tantivy.rs` + `crates/graph-nexus-cli/tests/search_vector_fallback.rs` — same `Some(...)` / `batch: false` updates for fixtures
- Create: `crates/graph-nexus-cli/tests/search_batch.rs` — 3 integration tests covering divider emission, blank/comment skip, empty-stdin contract

Executed in commit `de599de`.
