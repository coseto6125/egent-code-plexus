# Provable Verdicts for Code Review

## Problem Statement

Code review against a diff requires answering structural questions at scale: "Who imports this symbol?", "Does this route removal break anything?", "Are there untrackable dispatches in the changed files?" These are expensive to compute per-review — every agent re-traverses the graph. A better model is to **pre-compute these verdicts once per diff**, package them as severity-tagged decision rows, and hand them to the agent's review workflow.

This spec defines `ecp review --verdicts`, a provable-only layer that answers *structural* changes only (no style, no complexity, no heuristics with confidence <0.7). Every verdict cites the exact graph fact that triggered it.

## Severity Model

**Three tiers, derived from caller reachability.**

| Severity | Rule | Signals |
|---|---|---|
| `RISK` | Cross-file callers exist (other modules import this symbol) | public symbol removed, route removed, blindspot in modified file |
| `WARN` | Intra-file callers only (refactor within one file) | public symbol changed, route modified |
| `INFO` | No known callers found (internal-only, new public surface, fully untracked) | new public symbol added, route added, internal-only change |

**Derivation logic:**

- Synthesize across sections: a verdict first checks `diff.symbols.certain.symbols_changed`, then looks up matching intra-file callers in `diff.symbols.certain.intra_file_callers` and cross-file candidates in `diff.symbols.heuristic.cross_file_callers`.
- **Risk escalation**: If cross-file callers exist → `RISK`. Else if intra-file callers exist → `WARN`. Else → `INFO`.
- **Removed public symbols**: Always `RISK` — removal without confirmation is the most common silent-break vector.
- **Blindspot in diff region**: Always `WARN` — indicates incomplete alias coverage, preventing full enumeration of downstream impact.

## Verdict Schema

```json
{
  "kind": "SIGNATURE_OR_BODY_CHANGED",
  "severity": "WARN",
  "path": "src/lib.rs",
  "line": 42,
  "symbol": "foo::bar",
  "detail": "Function foo::bar changed (hash abc1234 → def5678); 1 intra-file caller(s), 0 cross-file candidate(s)",
  "intra_callers": [
    {"path": "src/lib.rs", "name": "qux", "kind": "Function", "line": 95}
  ],
  "cross_callers": null
}
```

### Fields

- **`kind`** (required) — Discriminator; one of `VerdictKind` enum (see below).
- **`severity`** (required) — One of `RISK`, `WARN`, `INFO` per the model above.
- **`path`** (required) — File where the change occurred (relative to repo root).
- **`line`** (optional) — Line number within the file (0-based from AST).
- **`symbol`** (optional) — Fully qualified symbol name (e.g. `ClassName::method_name`); omitted for blindspots and routes.
- **`detail`** (required) — Human-readable summary of the change and caller counts.
- **`intra_callers`** (optional) — Array of `VerdictCaller` records (same file); omitted if empty.
- **`cross_callers`** (optional) — Array of `VerdictCaller` (different files, heuristic tier); omitted if empty.

### VerdictCaller

```json
{
  "path": "src/lib.rs",
  "name": "external_caller",
  "kind": "Function",
  "line": 50,
  "confidence": 0.92
}
```

- **`path`**, **`name`**, **`kind`**, **`line`** — Always present.
- **`confidence`** (optional) — Only on cross-file callers (heuristic tier, 0.0–1.0); intra-file callers omit this (certain tier).

## VerdictKind Catalog

### SIGNATURE_OR_BODY_CHANGED

**Source**: `diff.symbols.certain.symbols_changed`

A public-surface symbol's AST changed (any byte difference in the source span). Cannot distinguish signature vs. body without per-parser AST-diffing (v1 limitation — tracked for v2).

**Severity rule**: Caller-escalation via intra/cross-file lookup (see Severity Model).

**Example**:
```json
{
  "kind": "SIGNATURE_OR_BODY_CHANGED",
  "severity": "WARN",
  "path": "src/lib.rs",
  "symbol": "validateUser",
  "detail": "Function validateUser changed (hash aaaaaaa → bbbbbbb); 2 intra-file caller(s), 0 cross-file candidate(s)"
}
```

### NEW_PUBLIC_SURFACE

**Source**: `diff.symbols.certain.symbols_added` (filtered by `is_public_surface()`)

A public-surface symbol was added to the repo. Added symbols carry `INFO` severity — no callers yet exist (by definition of "new").

**Severity**: Always `INFO`.

**Example**:
```json
{
  "kind": "NEW_PUBLIC_SURFACE",
  "severity": "INFO",
  "path": "src/routes.rs",
  "symbol": "createUser",
  "detail": "new public surface: Function createUser"
}
```

### REMOVED_PUBLIC_SURFACE

**Source**: `diff.symbols.certain.symbols_removed` (filtered by `is_public_surface()`)

A public-surface symbol was deleted. Removal always carries `RISK` severity — callers outside the repo may depend on it, and verification is mandatory before merge.

**Severity**: Always `RISK`.

**Example**:
```json
{
  "kind": "REMOVED_PUBLIC_SURFACE",
  "severity": "RISK",
  "path": "src/lib.rs",
  "symbol": "legacyAuth",
  "detail": "removed public symbol — verify no external callers remain"
}
```

### ROUTE_CONTRACT_CHANGED

**Source**: `diff.routes.added`, `diff.routes.removed`, `diff.routes.modified`

HTTP route (or declarative route node) was added, removed, or had its shape modified (method/path/handler changed).

**Severity rules**:
- Added route → `INFO`
- Removed route → `RISK` (consumers may still call it)
- Modified route → `WARN` (method/path shift may break callers)

**Example**:
```json
{
  "kind": "ROUTE_CONTRACT_CHANGED",
  "severity": "RISK",
  "path": "src/handlers.rs",
  "symbol": "POST /api/users",
  "detail": "route removed: POST /api/users — verify all consumers migrated",
  "line": 42
}
```

### BLINDSPOT_IN_DIFF_REGION

**Source**: `diff.symbols.unknown.blindspots_in_diff_region`

A `BlindSpot` record (eval, dynamic dispatch, reflection, unresolved import) exists inside one of the modified files. Callers downstream of that site cannot be fully enumerated.

**Severity**: Always `WARN` — signals incomplete static coverage; requires manual verification.

**Example**:
```json
{
  "kind": "BLINDSPOT_IN_DIFF_REGION",
  "severity": "WARN",
  "path": "src/plugins.rs",
  "line": 88,
  "detail": "blindspot (dynamic_call) inside modified file: callback registration"
}
```

## Public Surface Definition

`is_public_surface()` matches these NodeKind strings:

- `Function`, `Method` — callable entries
- `Constructor` — object initialization
- `Class`, `Struct`, `Enum`, `Trait`, `Interface` — type definitions
- `Route` — HTTP endpoint
- `EventTopic` — async event channel
- `SchemaField` — API field exposure

**Intentionally excluded**: `Variable`, `Property`, `Const` (internal data deltas generate noise without semantic value at the review layer).

## Provability Invariant

Every verdict is **provably derivable** from the graph. No verdict fires without:

1. An exact AST-derived symbol change (hashes, line numbers)
2. A deterministic caller enumeration (intra-file = certain, cross-file = heuristic tier ≥0.7)
3. A route shape delta from the route detector

Heuristic edges with confidence <0.7 are **not** emitted (they feed the agent's fallback grep, not structured verdicts).

## Non-Goals

- **Style linting** — indentation, naming, formatting. Graph has no style data.
- **Complexity scoring** — cyclomatic, cognitive, A-F grades. LLMs read source directly.
- **Duplicate detection** — copy-paste, code clones. Not AST-accessible without per-language IR diffing.
- **Test coverage** — coverage metrics, uncovered branches. Not in the graph layer.

Verdicts are **structural only**.

## Integration with ecp diff

`ecp review --verdicts` internally calls `ecp diff --section all`, which returns:

```json
{
  "baseline_ref": "main",
  "baseline_sha": "abc1234...",
  "current_ref": "HEAD",
  "current_sha": "def5678...",
  "symbols": { ... },
  "routes": { ... },
  "contracts": { ... }
}
```

The verdicts layer filters this payload:

1. Iterate `symbols.certain.symbols_changed` → match against intra + cross caller maps → emit `SIGNATURE_OR_BODY_CHANGED` with severity.
2. Iterate `symbols.certain.symbols_added` (filtered by `is_public_surface()`) → emit `NEW_PUBLIC_SURFACE`.
3. Iterate `symbols.certain.symbols_removed` (filtered by `is_public_surface()`) → emit `REMOVED_PUBLIC_SURFACE`.
4. Iterate `symbols.unknown.blindspots_in_diff_region` → emit `BLINDSPOT_IN_DIFF_REGION`.
5. Iterate `routes.added` / `routes.removed` / `routes.modified` → emit `ROUTE_CONTRACT_CHANGED` with appropriate severity.

Output is sorted by severity (RISK first) then by file path, then by line number. All verdicts are collected into a single flat JSON array under the `verdicts` field.

## Output Format

```json
{
  "baseline": {"ref": "main", "sha": "abc1234..."},
  "current":  {"ref": "HEAD", "sha": "def5678..."},
  "verdicts": [
    {
      "kind": "SIGNATURE_OR_BODY_CHANGED",
      "severity": "WARN",
      "path": "src/lib.rs",
      ...
    }
  ],
  "summary": {
    "total": 5,
    "risk": 1,
    "warn": 2,
    "info": 2
  },
  "elapsed_ms": 142
}
```

The summary counts verdicts by severity for quick triage. Elapsed time includes diff computation, caller enumeration, and verdict derivation.

## Implementation Reference

- **Source**: `crates/ecp-cli/src/commands/review/verdicts.rs`
- **Integration**: `crates/ecp-cli/src/commands/review/mod.rs` (lines 64–102, `run_verdicts()`)
- **Tests**: `crates/ecp-cli/tests/review_verdicts_test.rs`

## Deferred Features

- **Per-symbol v1 → v2 signature diff** (tracks semantic parameter/return type changes separately from body) — requires per-language AST-diffing IR. Deferred to v2.
- **Heuristic cross-call resolution** — caller detection for dynamic dispatch (vtable, trait objects, callbacks) currently falls under blindspot. Confidence tier ≥0.85 heuristic edges are included in cross-file caller enumeration; <0.7 are omitted. Closing the gap requires language-specific hint libraries.
