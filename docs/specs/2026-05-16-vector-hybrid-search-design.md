# Vector + Hybrid Search Wire-Up — Design

**Status:** Draft
**Owner:** @coseto6125
**Date:** 2026-05-16
**Crate:** `graph-nexus-cli`

## Goal

Replace the three vector/hybrid stubs in `crates/graph-nexus-cli/src/commands/search.rs` — the `SearchMode::Vector` arm (L215 in `compute_single`), the `SearchMode::Hybrid` arm (L220), and the silently-dropped mode in `scan_repo` (L535) — with a real cosine-similarity path that uses the BGE-M3 embeddings already produced by `graph-nexus-analyzer::embeddings::Embedder` and persisted in `ZeroCopyGraph::embeddings`. Line numbers are anchors as of `06e426e`; refer to the match arms / `let _ = effective_mode;` site by name during implementation.

After this change:

- `gnx search "<phrase>" --mode vector` ranks symbols by cosine similarity between the query embedding and the per-node embedding stored in the graph.
- `gnx search "<phrase>" --mode hybrid` fuses BM25 + cosine via Reciprocal Rank Fusion (RRF, k=60).
- `gnx search "<phrase>" --mode auto` continues to route slug-like input to BM25 and phrase input to Hybrid when the graph has embeddings (existing `detect_mode` keeps working — no behavioural change, only the downstream path becomes real).
- Multi-repo fan-out (`scan_repo`) honours the mode instead of silently dropping it.

## Non-Goals

- **Build-time embedding generation** — already implemented in `resolution/builder.rs`; this change is query-side only.
- **SIMD-accelerated cosine** — corpus size (≤10k nodes × 1024 dims) is too small to justify; plain Rust + auto-vectorisation suffices.
- **Cross-process embedder cache** — hooks fork fresh processes; an in-process `OnceLock` cannot survive across invocations. No daemon work in this PR.
- **Query embedding cache** — single-shot queries; the dominant cost is `Embedder::new()` cold start, not the per-query embed call.
- **Model selection** — BGE-M3 stays hard-wired (same as build path).
- **`--hybrid-k` flag** — k=60 is well-tested in literature; add a flag only if quality issues surface.

## Architecture

### New module: `crates/graph-nexus-cli/src/embedder.rs`

Tiny accessor that wraps a process-local `OnceLock<Result<Embedder, String>>`. Returns `&'static Embedder` on success; otherwise an owned `GnxError` so callers can `?`-fallback to BM25 cleanly.

```rust
use graph_nexus_analyzer::embeddings::Embedder;
use graph_nexus_core::GnxError;
use std::sync::OnceLock;

pub fn get_embedder() -> Result<&'static Embedder, GnxError> {
    static CELL: OnceLock<Result<Embedder, String>> = OnceLock::new();
    let slot = CELL.get_or_init(|| Embedder::new().map_err(|e| e.to_string()));
    slot.as_ref().map_err(|e| GnxError::Rkyv(format!("embedder init: {e}")))
}
```

Rationale: `OnceLock` (std, stable) keeps a single Embedder per gnx process. For CLI single-shot use the gain is nil (one init either way); for multi-repo rayon fan-out it deduplicates ~1–2 s of cold start across N workers.

### Changes to `crates/graph-nexus-cli/src/commands/search.rs`

#### 1. Vector path (replaces the `SearchMode::Vector` arm in `compute_single`)

Add a private helper called from the `SearchMode::Vector` arm of `compute_single`:

```rust
fn vector_hits_from_graph(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
    index_dir: Option<&std::path::Path>,
) -> Vec<Hit> {
    let Some(embs) = graph.embeddings.as_ref() else {
        eprintln!("→ vector: graph has no embeddings — falling back to bm25 (rebuild with `gnx admin index --embeddings`)");
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

    cosine_top_k(graph, embs, &query_vec, kind_set, repo_label)
}
```

#### 2. Cosine compute

```rust
// Pseudocode signature — the real call site iterates `graph.embeddings`
// (an `Option<ArchivedVec<ArchivedVec<f32>>>`) directly with no copy.
// Unit tests inject a `&[Vec<f32>]` via a thin wrapper trait so we don't
// need a rkyv-archived graph in test fixtures.
fn cosine_top_k(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    embeddings: impl IntoParIter<Item = &[f32]>,
    query: &[f32],
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
) -> Vec<Hit> {
    let q_norm = l2_norm(query);
    if q_norm == 0.0 { return Vec::new(); }

    // Parallel scan: enumerate node embeddings, score, collect (idx, score) tuples.
    // BinaryHeap top-K avoids full sort.
    let scored: Vec<(usize, f32)> = embeddings
        .par_iter()
        .enumerate()
        .filter_map(|(idx, emb)| {
            let dot: f32 = emb.iter().zip(query.iter()).map(|(a, b)| a * b).sum();
            let denom = l2_norm(emb) * q_norm;
            if denom == 0.0 { return None; }     // skip-marker zero vec
            let sim = dot / denom;
            (sim > 0.0).then_some((idx, sim))
        })
        .collect();

    // Top-K heap merge
    let mut heap: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::with_capacity(TOP_K + 1);
    for (idx, sim) in scored {
        heap.push(Reverse((sim.to_bits(), idx)));
        if heap.len() > TOP_K { heap.pop(); }
    }

    let mut ordered: Vec<(usize, f32)> = heap
        .into_iter()
        .map(|r| (r.0.1, f32::from_bits(r.0.0)))
        .collect();
    ordered.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    ordered
        .into_iter()
        .filter_map(|(idx, score)| build_hit(graph, idx, score, kind_set, repo_label))
        .collect()
}

fn l2_norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}
```

The exact `impl IntoParIter` form will be settled during implementation — likely two free functions (`cosine_top_k_owned` for tests, `cosine_top_k_archived` for prod) sharing a common scoring inner loop, rather than a generic trait object, to avoid rayon trait-bound noise.

#### 3. Hybrid path (replaces the `SearchMode::Hybrid` arm in `compute_single`)

```rust
fn hybrid_hits_from_graph(
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    repo_label: &Option<String>,
    index_dir: Option<&std::path::Path>,
) -> Vec<Hit> {
    if graph.embeddings.is_none() {
        eprintln!("→ hybrid: graph has no embeddings — falling back to bm25");
        return bm25_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
    }
    let bm25 = bm25_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
    let vec = vector_hits_from_graph(graph, pattern, kind_set, repo_label, index_dir);
    rrf_merge(bm25, vec)
}

const RRF_K: f32 = 60.0;

fn rrf_merge(bm25: Vec<Hit>, vec: Vec<Hit>) -> Vec<Hit> {
    // uid surrogate = (file, line, name) — Hit has no uid field but the triple is unique enough.
    // Building a stable key from those three is cheap and avoids changing Hit.
    use std::collections::HashMap;
    let key = |h: &Hit| (h.file.clone(), h.line, h.name.clone());

    let mut scores: HashMap<(String, u32, String), (f32, Hit)> = HashMap::new();

    for (rank, h) in bm25.into_iter().enumerate() {
        let s = 1.0 / (RRF_K + rank as f32 + 1.0);
        scores.entry(key(&h))
            .and_modify(|e| e.0 += s)
            .or_insert_with(|| (s, h));
    }
    for (rank, h) in vec.into_iter().enumerate() {
        let s = 1.0 / (RRF_K + rank as f32 + 1.0);
        scores.entry(key(&h))
            .and_modify(|e| e.0 += s)
            .or_insert_with(|| (s, h));
    }

    let mut combined: Vec<(f32, Hit)> = scores.into_values().collect();
    combined.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    combined.into_iter()
        .take(TOP_K)
        .map(|(score, mut h)| { h.score = score; h })
        .collect()
}
```

#### 4. Multi-repo fan-out (replaces the `let _ = effective_mode;` block in `scan_repo`)

`scan_repo` currently has `let _ = effective_mode;` and dispatches everything to BM25. Replace with the same match used in `compute_single`:

```rust
fn scan_repo(
    repo_name: &str,
    graph_path: &str,
    pattern: &str,
    kind_set: &Option<Vec<String>>,
    mode: &SearchMode,
) -> Result<Vec<Hit>, String> {
    let engine = Engine::load(std::path::PathBuf::from(graph_path))
        .map_err(|e| format!("{repo_name}: load {graph_path}: {e}"))?;
    let graph = engine.graph().map_err(|e| format!("{repo_name}: access: {e}"))?;
    let index_dir = engine.index_dir();
    let repo_label = Some(repo_name.to_string());

    let effective_mode = match mode {
        SearchMode::Auto => detect_mode(pattern, embeddings_available_for(graph)),
        m => m.clone(),
    };

    let hits = match effective_mode {
        SearchMode::Bm25 | SearchMode::Auto =>
            bm25_hits_from_graph(graph, pattern, kind_set, &repo_label, index_dir),
        SearchMode::Vector =>
            vector_hits_from_graph(graph, pattern, kind_set, &repo_label, index_dir),
        SearchMode::Hybrid =>
            hybrid_hits_from_graph(graph, pattern, kind_set, &repo_label, index_dir),
    };

    Ok(hits)
}
```

`OnceLock` in `embedder.rs` ensures all rayon workers in a single CLI invocation share one Embedder.

### Auto-mode behaviour (no logic change)

`detect_mode` and `embeddings_available_for` are unchanged. The fallback `eprintln!` already tells users to build with `gnx admin index --embeddings`. Net effect:

- **Slug input + any state** → BM25 (hook fast path stays cheap).
- **Phrase input + embeddings absent** → BM25 + stderr hint (current behaviour).
- **Phrase input + embeddings present** → Hybrid (this PR makes that path real).

## Data Flow

```
gnx search "validate user input" --mode hybrid
  └── compute_single
       ├── effective_mode = Hybrid (explicit)
       ├── hybrid_hits_from_graph
       │    ├── bm25_hits_from_graph  → top-K Hits ranked by BM25
       │    ├── vector_hits_from_graph
       │    │    ├── get_embedder() → &'static Embedder (OnceLock)
       │    │    ├── embedder.embed(["validate user input"]) → [1024-dim f32]
       │    │    └── cosine_top_k(graph.embeddings, query) → top-K Hits ranked by cosine
       │    └── rrf_merge(bm25, vec) → top-K Hits ranked by RRF (k=60)
       ├── sort by score, truncate TOP_K
       └── emit
```

## Error Handling

All failure modes degrade to BM25 + stderr hint. The hook path MUST NOT error out — preserves the existing contract that hooks return empty rather than crash.

| Failure | Behaviour |
|---|---|
| `graph.embeddings == None` | stderr warn, BM25 |
| `Embedder::new()` fails (no model, offline) | stderr warn, BM25 |
| `embedder.embed(query)` fails | stderr warn, BM25 |
| Query L2-norm = 0 (empty/whitespace input) | empty Vec (caller already filters short queries) |
| Node embedding is all-zero (skip marker) | excluded from ranking |

## Testing

All tests live in `crates/graph-nexus-cli/tests/`. They must NOT trigger model download — that means no `Embedder::new()` in tests. Strategy: split `cosine_top_k` and `rrf_merge` so they accept pre-computed embeddings / pre-ranked Hit lists, then test those directly with hand-crafted data. Wire-up code that *does* call `Embedder::new()` is covered by an end-to-end smoke test gated behind a feature flag or `#[ignore]`.

### Unit tests (no network)

1. **`vector_falls_back_when_embeddings_missing`** — build minimal `ZeroCopyGraph` with `embeddings: None`, call `vector_hits_from_graph`, assert: (a) result equals `bm25_hits_from_graph` output, (b) stderr matches `"vector: graph has no embeddings"`.

2. **`cosine_top_k_ranks_by_similarity`** — hand-craft 5 nodes with known embeddings, query vec close to node 2 → assert node 2 ranks first, all-zero node excluded.

3. **`cosine_top_k_respects_kind_filter`** — same graph, kind_set=["function"] → assert non-matching kinds dropped.

4. **`hybrid_falls_back_when_embeddings_missing`** — symmetric to test 1.

5. **`rrf_merge_combines_two_ranked_lists`** — hand-craft two top-K Vec<Hit>, assert: (a) items in both gain combined score, (b) items in only one keep their unilateral rank score, (c) result sorted descending, (d) result truncated to TOP_K.

6. **`rrf_merge_dedupes_by_file_line_name`** — same symbol from both sides → single entry with summed score.

### Integration test (network-gated)

7. **`vector_end_to_end_with_real_embedder`** (`#[ignore]` by default) — build a tiny graph with `--embeddings`, run `gnx search "<phrase>" --mode vector`, assert at least one hit and that ordering changes vs BM25 on the same query. Marked `#[ignore]` because the first run downloads ~1.2 GB.

## Risks & Open Questions

- **Q: Are BGE-M3 outputs from fastembed already L2-normalised?**
  A: We do *not* depend on the answer. `cosine_top_k` always divides by `||a|| · ||b||`, so an unnormalised input gives the same ranking as a normalised one. The first vector query logs a one-line `tracing::debug!` reporting `query_norm` so we can confirm empirically post-merge; if norm is reliably ~1.0 we can drop per-pair normalisation in a follow-up for a minor speedup.

- **Risk: Embedder cold start (~1–2 s) hits any CLI invocation that asks for `--mode vector` or `--mode hybrid` with embeddings present.**
  Mitigation: documented in `gnx search --help`. Auto mode keeps hooks on BM25 unless input is phrase-like.

- **Risk: HashMap key in `rrf_merge` uses `(file, line, name)`.**
  Hit currently has no `uid` field. The triple is unique within a single graph but might collide across repos in multi-repo merge. Acceptable for now because `rrf_merge` runs *inside* a single repo's compute path — multi-repo merge happens later in `compute_multi`'s top-K heap, which keys on the full OrderedHit including `repo`.

## File Touch List

```
crates/graph-nexus-cli/src/embedder.rs        # NEW — OnceLock<Embedder> accessor
crates/graph-nexus-cli/src/lib.rs             # add `pub mod embedder;`
crates/graph-nexus-cli/src/commands/search.rs # wire 3 stubs, add 4 private helpers
crates/graph-nexus-cli/Cargo.toml             # (no change — graph-nexus-analyzer already a dep)
crates/graph-nexus-cli/tests/search_vector.rs # NEW — 6 unit tests + 1 ignored E2E
docs/specs/2026-05-16-vector-hybrid-search-design.md  # THIS DOC
```

Estimated total: ~280 LOC added (+ ~220 prod, ~60 tests), 0 LOC removed except the three TODO comments and the `let _ = effective_mode;` line.

## Sequencing

1. Add `embedder.rs` accessor + `pub mod embedder` in lib.
2. Add `cosine_top_k` + `l2_norm` + unit tests (no embedder needed).
3. Add `rrf_merge` + unit tests.
4. Wire `vector_hits_from_graph` calling (2).
5. Wire `hybrid_hits_from_graph` calling (3) + (4).
6. Wire the three match-arm sites: `SearchMode::Vector` and `SearchMode::Hybrid` in `compute_single`, and the dispatch block in `scan_repo`.
7. Add `vector_falls_back_when_embeddings_missing` + `hybrid_falls_back_when_embeddings_missing`.
8. Add ignored end-to-end test.
9. `cargo fmt` + `cargo clippy` + `cargo test`.
10. Manual smoke: `gnx admin index --embeddings` on a small repo, then `gnx search "<phrase>" --mode vector` / `--mode hybrid`.

## Scope Extension — Phase 1 & 2 (added mid-PR)

Two follow-ups originally planned for separate PRs were folded into this branch after concurrent-execution analysis (see PR conversation) confirmed they address the highest-impact UX gaps. Both fit the same architecture and share tests with the core wire-up.

### Phase 1 — Preserve embeddings across auto-reindex

**Problem.** Every `git commit` triggered `post_tool_use` to spawn `gnx admin index --repo .` without `--embeddings`, silently demoting a vector-capable graph to BM25-only on the next query. `auto_ensure::ensure_fresh` had the same bug for synchronous rebuilds.

**Fix.** New `pub fn auto_ensure::embeddings_present(graph_path: &Path) -> bool` inspects the previous graph. Both reindex spawn sites (`auto_ensure::ensure_fresh` and `post_tool_use::spawn_background_reindex`) call it and conditionally append `--embeddings` to the rebuild args. Any failure (missing / corrupt graph) collapses to `false` so the hook contract is preserved.

**Tests.** `crates/graph-nexus-cli/tests/auto_ensure_embeddings.rs` covers the three branches (with embeddings / without / missing file).

### Phase 2 — `gnx search --batch` (stdin amortisation)

**Problem.** Every `gnx search --mode vector` is a fresh process paying ~1.1 s of BGE-M3 cold start (~1.7 GB RSS for the ONNX session). Scripted batch workloads multiply this linearly.

**Fix.** New `--batch` flag on `SearchArgs` reads patterns from stdin (one per line, `#` / blank lines skipped). All queries share the OnceLock-cached Embedder so cold start is paid once. Each query block is preceded by `=== pattern: <pattern> ===` on stdout so downstream scripts can split per-query regardless of `--format`.

**Internal API change.** `SearchArgs::pattern` is now `Option<String>` with `#[arg(required_unless_present = "batch")]`. Internal callers (`pre_tool_use::handle`, integration tests) updated to pass `Some(...)`. `compute_hits()` rejects `pattern == None` with `GnxError::InvalidArgument` — batch is CLI-only, hooks always run one pattern at a time.

**Measured speedup.** 3 vector queries on the worktree's own graph:
- Sequential (3× fresh CLI): 3315 ms
- `--batch` (single process): 1167 ms — **2.8× speedup, 65 % wall-time saved**

The win scales linearly with N: at N=10 expect ~7× speedup.

**Tests.** `crates/graph-nexus-cli/tests/search_batch.rs` covers divider emission, blank/comment line skipping, and the empty-stdin contract (one-line stderr hint, no spurious dividers).
