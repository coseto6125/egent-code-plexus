# ecp Cold-Ingest Pipeline Optimization

**Date:** 2026-05-22
**Status:** In flight — depends on `perf/ecp-multilayer-opt` (PR #333)
**Scope:** Cold reindex hot-path fixes; complements warm-query roadmap.

## 1. Context

The multi-layer warm-query PR (#333, `perf/ecp-multilayer-opt`) cut warm
queries from ~50ms → ~27ms on ecp self-scan. Cold reindex on the same
corpus (10k files, 13.6k nodes) finishes in 0.61-0.92s — already inside
the project's `<5s / 25k files` budget.

ECP_PROF on a fresh cache reveals the time distribution:

| Phase | Duration | Share |
|---|---|---|
| step1 scan files (1389) | 0.01s | 1% |
| step2 init_providers | 0.13s | 14% |
| step3 parse | 0.01s | 1% |
| **step4 build_global_graph** | **0.65s** | **71%** |
| step5 write graph.bin | 0.01s | 1% |
| step6 tantivy index | 0.08s | 9% |

Step 4 internal sub-totals (`total_build: 0.049s`) account for only 50ms
of the 650ms wall time. The unaccounted **~0.6s lives in the serial
`for graph in local_graphs { builder.add_graph(graph) }` ingest loop**
at `admin/index.rs:285` — string-pool intern + RawNode→Node materialization
runs single-threaded.

This roadmap targets that ingest gap plus two low-risk supporting fixes
identified by the Explore-agent audit.

## 2. Locked design decisions

- **Branched off `perf/ecp-multilayer-opt`, not `main`.** Cold-ingest work
  touches `builder.rs` heavily and PR #333 already extended it (name_index,
  kind_offsets). Branching off the latest perf branch avoids reverse-cherrypick
  pain. After #333 merges, this branch rebases to `main` before final
  push.
- **Three items shipped, four deferred.** Scope is intentionally narrow:
  - **C1** (par_iter add_graph) — biggest single win, requires careful
    builder API rework
  - **C3** (Arc path_aliases) — small per-worker alloc, trivial fix
  - **C7** (file-node loop batch) — single serial pass, Cow-friendly
  - Deferred: **C2** (heritage O(N_file²)) — bounded inside 30ms Pass 2;
    **C4** (post-process serial island) — sub-2ms on current corpus;
    **C5** (CSR sort 2x) — sub-10ms at current edge count.
- **No schema bump.** All changes are internal to the build pipeline.
  `graph.bin` layout is unchanged; v10 caches keep working.

## 3. Status table

| # | Fix | Severity | Status | Commit | Evidence |
|---|---|---|---|---|---|
| C1 | par_iter add_graph + thread-local merge | 🔴 -300ms | — | — | `admin/index.rs:285` serial loop |
| C3 | Arc path_aliases (no clone per worker) | 🔴 14k clones | — | — | `builder.rs:1148` per-worker clone |
| C7 | File-node loop batch alloc | 🟡 F allocs | — | — | `builder.rs:1517` per-file String alloc |

## 4. Deferred (with rationale)

- **C2** `enclosing_class_heritage` O(N_file²) at `builder.rs:1712` —
  Pass 2 wall is 30ms total; even an O(N²) fix saves &lt;30ms on the
  current corpus. Worth fixing when a single Java/Kotlin file with
  200+ nodes appears.
- **C4** post-process serial island (class_membership + overrides +
  schema_field_mirrors + event_topic_mirrors + file-node loop) —
  individual passes run at 1-2ms each on ecp self. Parallelization
  cost (mutex-protected edge vecs / thread-local reduce) likely
  exceeds savings at this scale.
- **C5** CSR sort 2x + 2 perm vecs (`builder.rs:1595`) — 8MB extra
  alloc at 1M edges, but ecp self has ~30k edges so well under 1MB.
- **C6** `make_pipeline` no cache — already shipped in PR #333 (P6).

## 5. Acceptance

- All 3 commits land in one PR; commits name the item ID.
- `cargo test --workspace --tests` green.
- `cargo clippy --workspace --tests` clean.
- Cold reindex benchmark on ecp self-scan:
  - Target: step4 (build_global_graph) wall time drops by ≥30%
  - Total cold reindex ≤ 0.5s on the 13.6k-node corpus
- A "Things to highlight" section gets added at end-of-PR.
