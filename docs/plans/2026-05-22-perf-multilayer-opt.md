# ecp Multi-Layer Performance Optimization Roadmap

**Date:** 2026-05-22
**Status:** In flight — single PR, stepwise commits
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
| P1 | cypher: scalar functions not aggregate | module/exec | 🔴 correctness | — | — | `executor.rs:96-98` flags every FunCall as agg |
| P2 | resolve_owner_class direct field read | function | 🔴 hot | — | — | `symbol_id.rs:23` edge-walks despite `Node.owner_class: StrRef` field |
| P3 | BFS matches! on Archived enums | function | 🔴 hot | — | — | `graph_query.rs:140-169` deserialize per edge |
| P4 | git subprocess memoize | process | 🔴 startup | — | — | `main.rs:214/220` + 3 git rev-parse HEAD sites |
| P5 | auto_ensure: thread dirty_files | process | 🔴 incr | — | — | `auto_ensure.rs:218→280` double walk |
| P6 | make_pipeline OnceLock | module | 🟡 incr | — | — | `reanalyze.rs:103` no cache; pattern from `overlay_writer.rs:226` |
| P7 | tool_map par_iter | module | 🔴 30ms+ | — | — | `tool_map.rs:213` serial fs::read |
| P8 | name_index populate (v9) | architecture | 🔴 sys | — | — | `builder.rs:1670` TODO; field declared in `graph.rs:587` |
| P9 | kind_offsets CSR (v10) | architecture | 🔴 sys | — | — | no kind→indices reverse map; 4 detectors pay O(N) |
| P10 | Guard (c) bucket fingerprint | process | 🟡 incr | — | — | `incremental.rs:113` documented conservative fallback |

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
