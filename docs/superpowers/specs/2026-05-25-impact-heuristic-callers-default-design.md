# Design: surface heuristic callers by default in `impact` / `review`

**Date:** 2026-05-25
**Status:** approved (pending spec review)
**Size:** S–M (presentation layer only — no graph schema, no risk-algorithm change)

## Problem

`MirrorsField` / `EventTopicMirror` are real heuristic graph edges (confidence
~0.85) representing implicit relationships AST cannot see: a DTO field mirroring
an entity field, an event publisher mirroring a subscriber by topic. They are an
**(A) Graph-completeness** signal — an LLM refactoring one side must be told the
other side exists, or `ecp impact` silently under-reports the blast radius.

These edges are **already fully wired** into `impact` (BFS traverses them
up/down/both), `review` (`run_impact` calls impact), and `rename` (handles
`is_heuristic()`). The gap is **exposure**: `--include-heuristic` defaults OFF,
so a default `ecp impact <sym>` shows only a one-line `note: N heuristic edges
hidden (pass --include-heuristic to see them)`. An LLM building context must
notice the note, then re-run with the flag — the "LLM won't think to ask"
trap. Saga pairs (`find-transaction-patterns`) are NOT graph edges (pure
runtime computation) and are out of scope here.

## Current state (verified via code read)

`run_for_symbol` already returns heuristic results in a **separate array**:
- `run_bfs` returns `(det_results, heur_results, hidden_conf, hidden_heur)` —
  deterministic and heuristic callers are already segregated.
- JSON: `"impact"` = deterministic callers; `attach_heuristic_fields` adds
  `"heuristic_edges"` **but only when `include_heuristic == true`**
  (`impact.rs:1028`); `"hidden_heuristic_edges"` count always emitted.
- `coverage_bfs_for_symbol` passes `include_heuristic: false` deliberately
  (`impact.rs:272-280`) — **risk_level / coverage class already exclude
  heuristic callers**. No change needed there.

So the segregation exists; only the default visibility and the text rendering
are missing.

## Design

### Decision 1 — heuristic callers shown by default, clearly tagged

`heuristic_edges` is emitted **regardless of the flag**, each entry tagged
`requires_verification: true` + its `confidence`. (`requires_verification` is
added by this presentation layer keyed on `is_heuristic()` — it is not currently
a per-edge graph property; implementation confirms whether the existing
`find-*` JSON already sets it and reuses that key for consistency.)
Deterministic callers stay in `"impact"`. Two distinct buckets, never merged — the LLM reads the top bucket as
ground truth and the bottom as leads to verify. Mirrors the existing `find
--mode bm25` source/tests/reference bucketing precedent.

### Decision 2 — JSON shape (LLM consumer)

```jsonc
{
  "status": "success",
  "target": "...",
  "direction": "upstream",
  "impact": [ /* deterministic callers, confidence 1.0 */ ],
  "heuristic_callers": [        // renamed from heuristic_edges for caller semantics
    { "name": "...", "confidence": 0.85, "rel_type": "EventTopicMirror",
      "requires_verification": true, "reason": "..." }
  ],
  "hidden_heuristic_edges": 0   // now ~always 0 since they're shown; kept for --no-heuristic
}
```

`heuristic_callers` is **always present** (empty array when none — honest
no-data, distinguishable from "feature absent"). Field renamed from
`heuristic_edges` → `heuristic_callers` for symmetry with the `impact` (caller)
list; `heuristic_edges` was never a stable MCP surface.

### Decision 3 — text shape (human-debug path)

```
impact: bookFlight  (upstream)
[deterministic callers]
  reserveSeat        services/booking.rs:88
  checkoutHandler    api/checkout.rs:142

heuristic_callers[1] (requires_verification):
  cancelFlight  EventTopicMirror  conf=0.85  services/booking.rs:201
```

The `heuristic_callers[N] (requires_verification)` header preserves the
count + verification signal even at a glance. When N=0 the header is omitted
(no empty-section noise in the human path).

### Decision 4 — flag inversion

`--include-heuristic` (default false) → `--no-heuristic` (default: shown).
Keeps the ability to suppress for a pure-deterministic blast radius. Update the
4 call sites + the `note:` text (no longer "pass --include-heuristic"; instead
the suppressed-count note only appears under `--no-heuristic`).

### Decision 5 — review aggregator

`review/aggregate.rs::run_impact` sets `include_heuristic: true` so the
automatic change-review path surfaces heuristic callers in its findings,
tagged the same way. risk/verdict logic unaffected (already deterministic-only).

## Out of scope

- Saga pairs (`find-transaction-patterns`): not graph edges; wiring them into
  impact would require per-query runtime computation — separate effort.
- Verb consolidation of the three `find-*` commands: orthogonal; the value is
  in the main-path exposure done here, not the standalone verbs.
- Any change to `risk_level` / coverage classification (already isolated).

## Testing

- Unit: `impact` on a symbol with a known `EventTopicMirror` edge emits
  `heuristic_callers` with `requires_verification: true` **without** any flag.
- Unit: `--no-heuristic` suppresses `heuristic_callers` (empty) and restores the
  `hidden_heuristic_edges` count.
- Unit: deterministic-only symbol → `heuristic_callers: []` (present, empty).
- Regression: `risk_level` / coverage class identical with and without
  heuristic callers present (proves isolation).
- text-format snapshot: two-bucket rendering; header omitted when N=0.
- review: `run_impact` findings include a heuristic caller for a changed file
  that has a mirror edge.
- 14-language note: heuristic edges are emitted by the analyzer, not this
  presentation change; this PR adds no per-language parser logic, so the
  14-language matrix is not triggered. Tests use existing fixtures that already
  carry mirror/event edges.
```
