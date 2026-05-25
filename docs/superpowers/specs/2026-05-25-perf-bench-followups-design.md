# Perf / Bench Follow-ups Cleanup — Design

**Date:** 2026-05-25
**Branch:** `perf/followups-bench-cleanup`
**Scope:** Close out 6 deferred perf/bench follow-ups in one PR.

## Follow-ups covered

| FU | Change | File(s) | Risk |
|---|---|---|---|
| FU-2026-05-24-004 | `dedup_rows` structural key (replace `format!("{row:?}")`) | `cypher/value.rs`, `cypher/executor.rs` | hot path (DISTINCT/UNION only) |
| FU-2026-05-24-002 | bench: +4 cypher query patterns | `benchmark_ecp.py` | none (test script) |
| FU-2026-05-24-008 | bench: split `--analyze-runs N` | `benchmark_ecp.py` | none |
| FU-2026-05-24-005 | stacked-delta measurement for PRs #432+#433 | measurement → `docs/perf-notes.md` | result-dependent |
| FU-2026-05-24-007 | pin clone commits + release-binary recapture | `benchmark_repos.py`, `parity/baselines.md` | none |
| FU-2026-05-23-003 | pass16 / dir_size parallelization | `resolution/builder.rs` | parser core (14-lang gate) — **profile-gated** |

## Per-FU design

### FU-004 — structural dedup key

`Value` derives `PartialEq` but not `Eq`/`Hash` (it carries `Float(f64)` and
`f32` confidence). The current `dedup_rows` keys on `format!("{row:?}")` —
O(n) Debug-string allocation per row on every `RETURN DISTINCT` and non-ALL
`UNION`.

**Approach (chosen):** structural byte key, not xxh3.

Add `Value::write_dedup_key(&mut Vec<u8>)`:
- 1-byte discriminant tag per variant (stable, append-only ordering).
- `Bool` → 1 byte. `Int` → 8 LE bytes. `Float` → `f64::to_bits().to_le_bytes()`.
- `Str` → `len` (LE u32) prefix + raw bytes (length-prefixing prevents
  boundary ambiguity, e.g. `["a","b"]` vs `["ab"]`).
- `List` → element count prefix + recursive `write_dedup_key` per element.
- `NodeRef` → tag + `idx` (the u32 identity; name/kind/file_path are derived,
  so idx alone is the canonical identity).
- `EdgeRef` → tag + `src` + `tgt` + `rel_type as u8` + `confidence.to_bits()`.

`dedup_rows` becomes:
```rust
fn dedup_rows(rows: &mut Vec<Vec<Value>>) {
    let mut seen = HashSet::new();
    let mut key = Vec::new();
    rows.retain(|row| {
        key.clear();
        for v in row {
            v.write_dedup_key(&mut key);
        }
        seen.insert(key.clone())
    });
}
```

**Why structural over xxh3:** zero collision risk (a hash needs collision
handling or accepts silent wrong-dedup), and CLAUDE.md prefers structured keys
over string/hash heuristics. The `key` buffer is reused across rows (cleared,
not reallocated); only the inserted copy allocates.

**Verification:** new regression tests in `ecp-core` —
- DISTINCT collapses identical rows.
- DISTINCT keeps rows differing only in a `Float` (e.g. `1.0` vs `1.5`).
- DISTINCT keeps `["a","b"]` distinct from `["ab"]` (length-prefix guard).
- non-ALL `UNION` dedups across both sides.

### FU-002 — bench cypher coverage

Append 4 query rows to `queries[]` in `benchmark_ecp.py`, each with a WHY
comment matching the existing `count(*)` / `decorator IN` style:
1. `COLLECT()` projection — e.g. `MATCH (c:Class)-[:Contains]->(m:Method) RETURN c.name, collect(m.name)`.
2. `IN [literal,...]` literal-list filter — e.g. `MATCH (m:Method) WHERE m.name IN ['main','run','init'] RETURN count(*)`.
3. Multi-hop edge — `MATCH (a:Method)-[:Calls*1..3]->(b:Method) RETURN count(*)`.
4. Multi-aggregate GROUP BY — `MATCH (n) RETURN n.kind, count(*) ORDER BY count(*) DESC`.

Each must show >5ms on `.sample_repo` (verified by running the bench). If a
pattern is sub-5ms it isn't exercising a distinct hot path → swap for a
heavier shape or drop with a note.

### FU-008 — `--analyze-runs N`

- New arg `--analyze-runs`, `type=int`, `default=1` (back-compat: existing
  single-sample behavior unchanged).
- When `--runs N` is passed with `N>1` and `--analyze-runs` is left default,
  auto-imply `--analyze-runs = args.runs` so CI `--runs 10` callers get
  multi-sample analyze without a second flag.
- Both analyze `_bench(...)` calls change `runs=1` → `runs=args.analyze_runs`.
- Docstring documents the single-sample ±20% noise floor.

### FU-005 — stacked-delta measurement

PRs #432 (kind-CSR + walk_rel) and #433 (VarMap) are both merged to main
(verified: commits `5bda7884`, `64c7cfd2`). Predicted compounding:
count(*) ~-67%, decorator IN ~-33% vs pre-#432 baseline.

**Procedure:** build current-main release binary, run
`benchmark_ecp.py --runs 10` vs a pre-#432 baseline binary. Record the stacked
deltas in a new `docs/perf-notes.md`.

**Decision branch (per user directive "measure, then optimize if it speeds
things up"):** if measurement shows the expected compounding, just record it.
If it shows a non-linear interference (one PR's win cancels another's) or an
unexpected regression on any query, surface the finding and propose an
optimization before acting — do not silently optimize.

### FU-007 — pin clone commits + recapture

`benchmark_repos.py:31` clones wave-1 langs with `git clone --depth 1` against
upstream master, so fixture composition drifts (lua 15→11, solidity 727→403,
move 486→367, zig 1→31). Per user directive, take the **Pin** path:
- Change `clone_repo` to accept a per-repo pinned commit sha and check it out
  after a shallow fetch (`git clone --depth 1`, then
  `git fetch --depth 1 origin <sha>` + `git checkout <sha>` — or
  `--revision <sha>` where the git version supports it).
- Recapture `parity/baselines.md` against the pinned snapshot using a **release**
  binary (not dev — current docs are dev-profile, ~3× slower).
- Add a header to `baselines.md` recording the pinned commit shas + binary
  profile so the numbers are reproducible.

### FU-003 — pass16 / dir_size parallelization (PROFILE-GATED)

The original next-action set an explicit ROI gate: *">1.5s 才動，目前 1.87s
median 已達 -40% 目標"*. pass16 is ~0.075s = 4% of wall.

**Procedure (per user directive "measure, wontfix if no win"):**
1. Run `--prof` to measure current pass16 wall on `.sample_repo`.
2. If pass16 is already very fast (<~0.05s), rayon fan-out overhead likely
   negates or reverses any gain → **archive FU-003 as wontfix** recording the
   measured number + rationale (mirrors the FU-2026-05-23-032 wontfix
   precedent: measured-below-threshold → wontfix with the number).
3. If measurably worth it, parallelize the pass16 main loop
   (`for (file_idx, lg) in self.local_graphs.iter().enumerate()`) via
   `par_iter().filter_map()` collecting per-file edges into thread-local Vecs,
   then merge — preserving deterministic edge ordering (sort by
   `(source, target)` after collect if the parallel order differs). dir_size
   walkdir parallelization has even lower ROI (ms-level advisory stats) and is
   only done if pass16 parallelization itself clears the bar.

**14-language gate:** if pass16 IS parallelized, the change touches parser-core
edge construction → must pass the 14-language coverage requirement and a
`.sample_repo` reindex confirming Fetches edge count is unchanged
(parallelization must not drop or duplicate edges).

## FOLLOWUPS bookkeeping

All 6 entries move to `FOLLOWUPS_DONE.md` with `✅ done in PR #N` prefix; the
Open file keeps a one-line stub each. FU-003, if measured-out, is archived as
`🚫 wontfix` with the measured pass16 number. **Per repo protocol, the
FOLLOWUPS update is its own `chore/followups-…` commit/PR, not bundled with this
feature PR** — this PR's description references the FU ids only.

## Out of scope

- FU-2026-05-23-021 (nextest archive CI) — M-sized, separate.
- FU-2026-05-24-009 (auto-merge timing) — M, design spike.
- Any cypher planner change beyond the dedup key.
