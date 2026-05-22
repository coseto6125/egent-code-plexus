# ecp Multi-Layer Performance Optimization Roadmap

**Date:** 2026-05-22
**Status:** Shipped — single PR, 10 commits
**Scope:** 10 fixes spanning architecture → process → module → function layers
**Source:** 4 parallel Explore-agent audits cross-referenced against `~/code-graph-nexus` self-scan benchmark

## 1. Context

The symbol-graph core roadmap (`docs/plans/archive/2026-05-21-symbol-graph-core-roadmap.md`)
closed all 54 atomic dev tasks (T0/T1/T4/T5/T7/T10 + hybrid plumbing). This
follow-up roadmap captures perf gaps surfaced when running `ecp` against its
own repo — gaps that did NOT block the dev roadmap (10-15ms warm queries hit
target) but that:

- **Violate priority #1** (`per-query <30ms`): `tool-map` measures 33ms
- **Defeat zero-copy invariant**: BFS hot path materializes RelType via
  `rkyv::deserialize` every edge
- **Leave declared-but-empty index** (`name_index: Vec<u32>` always
  `Vec::new()`), forcing O(N) scans on every name lookup
- **Re-do startup work** (3-4 `git rev-parse HEAD` subprocesses per warm query,
  `graph_path::resolve` called twice in `main.rs`)
- **Mark T7-6 guard(c) as a conservative fallback** that triggers full
  reanalyze on every Python/TS model edit

## 2. Scope decisions

- **One PR, one feature branch** (`perf/ecp-multilayer-opt`). Stepwise commits
  per fix so each commit is reviewable in isolation; bundling avoids
  reshuffling test/lint cycles across 10 individual PRs.
- **Two schema bumps fused** (`name_index` v8→v9, `kind_offsets` v9→v10) so
  the 14-lang parity baselines only regenerate once.
- **No drive-by refactors.** Each commit's diff is the minimum for its line
  item. Anything noticed mid-flight gets logged here, not silently bundled.

## 3. Locked design decisions

- `name_index` layout: sorted `Vec<(u64 name_hash, u32 node_idx)>` in archived
  graph; binary search lookup. `xxh3_64(name_bytes)` for the hash so we share
  the existing UID hasher. Collisions disambiguated by re-resolving from
  `string_pool` post-lookup.
- `kind_offsets` layout: `Vec<u32>` of length `NUM_KIND_VARIANTS+1`,
  identical CSR shape to `out_offsets`. `kind_node_idx: Vec<u32>` flat array
  of node indices grouped by kind. Build cost ≈ one `O(N)` pass at end of
  Pass 1.
- Both new indices are append-only schema additions per the existing rkyv
  discriminant-stability invariant (graph.rs:96-100).
- Guard (c) fingerprint: per-file `schema_fields_blake3` sidecar
  alongside `content_hash`; bucket-set delta computed in O(field_count) per
  file. No new schema bump — sidecar lives in `.ecp/<commit>/`.

## 4. Status table

| # | Fix | Layer | Severity | Status | Commit | Evidence |
|---|---|---|---|---|---|---|
| P1 | cypher: scalar functions not aggregate | module/exec | 🔴 correctness | shipped | 9e47ab93 | `is_aggregate_fn` helper + `eval_scalar_funcall` for type/id/labels; 4 regression tests |
| P2 | resolve_owner_class direct field read | function | 🔴 hot | shipped | 8c90a94c | `symbol_id.rs` now reads `Node.owner_class` directly — O(in_degree) → O(1) |
| P3 | BFS matches! on Archived enums | function | 🔴 hot | shipped | cd889aee | `From<&ArchivedRelType>` + `From<&ArchivedNodeKind>` zero-cost; 5 call sites migrated |
| P4 | git subprocess memoize | process | 🔴 startup | shipped | 3b49981d | new `git_cache` module; HEAD invalidation via `<common_dir>/HEAD` mtime |
| P5 | auto_ensure: thread dirty_files | process | 🔴 incr | shipped | 7b65af26 | `apply_l1_overlay_updates` takes `dirty_files`; second `collect_dirty_files` walk removed |
| P6 | make_pipeline OnceLock | module | 🟡 incr | shipped | 08b9a2ce | `reanalyze::pipeline()` is the single source; `overlay_writer` consumes it |
| P7 | tool_map par_iter | module | 🔴 30ms+ | shipped | 8c082cef | rayon `par_iter` + `flat_map_iter`; `classify_source` drops `format!` alloc |
| P8 | name_index populate (v9) | architecture | 🔴 sys | shipped | 4285bd57 | `NameIndexEntry { name_hash: u64, node_idx: u32 }` sorted; `nodes_by_name` binary search |
| P9 | kind_offsets CSR (v10) | architecture | 🔴 sys | shipped | 12abb77f | `kind_offsets` + `kind_node_idx` CSR; `nodes_by_kind`; routes + find-event-mirrors migrated |
| P10 | Guard (c) name-set diff | process | 🟡 incr | shipped | 12abb77f (bundled w/ P9) | `schema_field_names_per_file` + new `symbol_hash_diff` param; 2 regression tests |

## 5. Layer mapping

```
ARCHITECTURE   ── P8 (name_index)  P9 (kind_offsets)
       ↓
PROCESS        ── P4 (startup dedup)  P5 (dirty walk dedup)  P10 (guard-c fingerprint)
       ↓
MODULE         ── P1 (cypher exec)  P6 (pipeline cache)  P7 (tool_map par)
       ↓
FUNCTION       ── P2 (owner_class)  P3 (BFS matches)
```

## 6. Acceptance

- All 10 commits land in one PR; each commit message names the layer + item ID.
- `cargo test -p egent-code-plexus --tests` and `cargo test -p ecp-analyzer`
  green.
- `cargo clippy -p egent-code-plexus --tests -p ecp-analyzer` clean.
- 14-lang parity baselines regenerated after P8/P9 schema bump; diff matches
  `scripts/parity/round*_baseline.txt` invariants (inclusive emission preserved).
- Warm-query benchmarks on ecp self-scan show:
  - `tool-map` < 15ms (from 33ms)
  - `cypher 'RETURN type(r)'` succeeds
  - `find <name>` < 5ms on 5k-node graph (from O(N))
- A "Things to highlight" section is added to this doc at end-of-PR documenting
  any deferred sub-items, surprising findings, or design changes vs. this spec.

## 7. Things to highlight (post-implementation)

- **P9 + P10 fused into one commit**. The original spec scoped them as
  separate commits but they share the v9→v10 schema bump and ~28 test-fixture
  migrations. Splitting them would have churned the same files twice with
  no review benefit. Combined commit message names both layer/item IDs.
- **P1 cypher fix surfaced a parser-layer correctness bug, not just perf**.
  Every FunCall in RETURN was routed through `apply_aggregate`, so
  `RETURN type(r)`, `id(n)`, `labels(n)` (standard OpenCypher scalars)
  returned a hard error. The fix is logic-only at `executor.rs:96-98` plus
  `is_aggregate_fn` and `eval_scalar_funcall` helpers. 4 regression tests
  pin the new behaviour including mixed scalar + aggregate
  (`RETURN type(r), count(*)`).
- **P3 added `From<&ArchivedRelType>` + `From<&ArchivedNodeKind>`** with
  the same `ptr::read` pattern as the pre-existing `From<&ArchivedNodeKind>`.
  This unblocks zero-cost discriminant reads across BFS hot paths and
  Cypher executor — 5 `rkyv::deserialize::<RelType/NodeKind>` call sites
  collapsed. The unsafe blocks share the rationale documented in
  graph.rs:262-264 (rustc 27-arm match SIGSEGV workaround); no new safety
  surface added.
- **P4 git_cache uses `<common_dir>/HEAD` mtime as invalidation sentinel**,
  not a `clear()` method. This is safer than explicit invalidation because
  it survives `ecp diff`'s `GitGuard` mid-process checkout — the
  diff_contracts_two_commit_added_fetch test failed without this and
  passes with it. Caller code never needs to think about cache freshness.
- **P5 also dropped the now-unused `graph_path` param** from
  `apply_l1_overlay_updates`. It was only used to read `graph_mtime`
  inside the second `collect_dirty_files` call, which is removed by P5.
  The SKIP_DIRS `.ecp` duplication noted by the audit (`auto_ensure.rs:376`)
  is preserved as a follow-up — fixing it is cosmetic and out of P5's scope.
- **P7 tool_map preserves output order** despite parallelism. Rayon's
  `flat_map_iter` is order-preserving; within-file hit order (line, col)
  is unchanged because each file's inner loop runs serially.
- **P8 name_index struct change broke ~28 test fixtures**. Tests that
  constructed `name_index: vec![]` or `(0..n).collect()` were updated to
  `Vec::new()` of the new `NameIndexEntry` type. Production reads of
  `name_index` were zero before this commit (confirmed via grep) so no
  consumer migration was needed for v9 itself; rename.rs was the one
  consumer migrated to use the lookup helper.
- **P10 narrows guard (c) to name-set comparison, not full bucket
  fingerprint**. The original spec called for cross-file bucket-fingerprint
  sidecar, but archived nodes don't persist `SchemaType` (T4-8's
  documented architectural gap). Name-set comparison is sufficient for
  the common case (field add / remove / rename) because
  `schema_field_mirrors` clusters by name first. Type-only renames are
  caught by guard (a) (import set changes alongside type imports) or
  per-symbol content_hash diff. The conservative pre-v10 fallback is
  preserved when the caller passes `&FxHashMap::default()`.
- **`symbol_hash_diff` is not yet wired into production**. P10's new
  argument lands as ready-to-use API. The integration point is in
  `auto_ensure::ensure_fresh`'s Stale branch where `reanalyze_files`
  returns the new `LocalGraph`s — currently the result is intentionally
  `_fresh_graphs` (unused) per the T7-4 deliverable comment. Wiring
  `symbol_hash_diff` into the dispatch is a separate scope.
- **Schema versions bumped twice (v8→v9→v10), not fused.** The two
  schema additions are append-only and logically independent;
  bundling would have made the v9 commit reviewer-hostile (~50 fixture
  changes in one commit). Users reindex twice on upgrade — acceptable
  for a pre-1.0 internal tool. CI's `header_compatible` check transparently
  triggers the rebuild.
