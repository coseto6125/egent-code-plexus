---
name: simplify
description: Review changed code for reuse, quality, and efficiency, then fix issues found. Each agent runs `ecp` first (when indexed) for graph-aware context before reading raw diffs.
---

# Simplify: Code Review and Cleanup (`ecp`-aware)

Review all changed files for reuse, quality, and efficiency; fix what you find. Three agents split the work; the orchestrator first runs an `ecp` pre-pass so each walks in with impact, blind spots, egress, shape drift, and resolver deltas already mapped.

## Phase 1: Identify Changes

Run `git diff` (or `git diff HEAD` for staged changes). No git changes: fall back to recently modified files the user mentioned or you edited earlier.

## Phase 2: Single `ecp` pre-pass (orchestrator)

1. **Probe whether indexed** — `ecp review --repo . --format toon`. On a missing/stale `.ecp/graph.bin`, run `ecp admin index --repo .` once, then retry. If `ecp` is unavailable or indexing fails, do raw-diff review and note the missing context.
2. **Choose the review scope:**
   - Working tree: `ecp review --repo . --format toon`
   - Since a baseline: `ecp review --repo . --since <ref> --format toon`
   - File list: `ecp review --repo . --files path/a.rs,path/b.rs --format toon`
3. **Capture four artefacts** for every agent:
   - `ecp_review`: `ecp review` output (impact, blind spots, egress/tool-map, shape-check, resolver diff)
   - `changed_files`: `git diff --name-only` or PR diff
   - `resolver_delta`: if needed, `ecp diff --repo . --section all --baseline <ref>`
   - `risk`: LOW / MEDIUM / HIGH / CRITICAL from the review output
4. If `ecp review`/`ecp diff` flags HIGH/CRITICAL risk, surface it before launching agents.

Hand the artefacts to each agent verbatim. They dig in with `ecp inspect --repo . --name "<symbol>"`, `ecp impact --repo . "<symbol>" --direction up`, `ecp find --repo . "<pattern>" --mode bm25`, or `ecp diff --repo . --section all --baseline <ref>`; the orchestrator maps broad strokes only.

## Phase 3: Launch three review agents in parallel

One message, three Agent tool uses. Each gets the full diff plus Phase-2 artefacts.

**Common preamble per agent prompt:**

> Repo at `<absolute path>`. Diff in `<location>`. The `ecp` pre-pass found:
> - ecp_review: `<summary or path>`
> - changed_files: `<list>`
> - resolver_delta: `<summary or not run>`
> - risk: `<level>`
>
> Focus on symbols / flows that actually changed. Skip mechanical sections (rename-only, formatting-only) when `ecp review` shows no execution impact. Depth on a symbol: `ecp inspect --repo . --name "<symbol>"`. Blast radius on a refactor: `ecp impact --repo . "<symbol>" --direction up`. "Duplicating an existing function?": `ecp find --repo . "<concept>" --mode bm25` before grep.

### Agent 1: Code Reuse Review

1. **Search for existing utilities/helpers** that could replace new code. The graph is primary — `ecp find --repo . "upsert bot" --mode bm25` or `ecp inspect --repo . --name "BotInfo"` finds matches grep misses. Grep only when the graph is empty.
2. **Flag any new function that duplicates existing functionality** — suggest the existing one (with file:line).
3. **Flag inline logic that could use an existing utility** — hand-rolled string manipulation, manual path handling, custom env checks, ad-hoc type guards.

### Agent 2: Code Quality Review

Same changes, hacky patterns:

1. **Redundant state** — duplicates existing state, cached values that could be derived, observers/effects that could be direct calls
2. **Parameter sprawl** — new parameters instead of generalising/restructuring existing ones
3. **Copy-paste with slight variation** — near-duplicate blocks that should share an abstraction (`ecp find --repo . "<concept>" --mode bm25` confirms it isn't canonical)
4. **Leaky abstractions** — exposing internals that should be encapsulated, or breaking boundaries (`ecp inspect --repo . --name "<symbol>"` shows it)
5. **Stringly-typed code** — raw strings where constants, enums, or branded types already exist
6. **Unnecessary JSX nesting** — wrapper Boxes/elements that add no layout value
7. **Nested conditionals 3+ levels deep** — flatten with early returns, guard clauses, lookup tables, or if/else-if cascades
8. **Unnecessary comments** — WHAT-comments (delete; identifiers do that), change narration, task/caller references — keep only non-obvious WHY

### Agent 3: Efficiency Review

Same changes, efficiency:

1. **Unnecessary work** — redundant computations, repeated file reads, duplicate API calls, N+1 patterns
2. **Missed concurrency** — independent operations run sequentially when they could parallelise
3. **Hot-path bloat** — new blocking work in startup or per-request/per-render hot paths. `ecp impact --repo . "<symbol>" --direction up` checks if the changed symbol sits in a hot path.
4. **Recurring no-op updates** — state/store updates in polling loops firing unconditionally; verify wrappers honour same-reference returns
5. **Unnecessary existence checks** — pre-checking file/resource existence (TOCTOU); operate directly, handle the error
6. **Memory** — unbounded structures, missing cleanup, event listener leaks
7. **Overly broad operations** — reading whole files when a portion suffices, loading all items to filter for one

## Phase 4: Aggregate and fix

Wait for all three agents, then fix each issue directly. False positives: note and skip.

After fixes:
- If indexed, rerun `ecp review --repo . --format toon` to confirm fixes touched only intended flows. Against a PR baseline, reuse the first pass's `--since <ref>` / `--files ...` scope.
- Summarise what was fixed (or confirm it was clean).

## Notes on cost

The pre-pass is one tool call. Per-agent `ecp` calls are bounded — start narrow, widen as needed. Graph-aware review is token-cheaper than large diffs: the agent skips mechanical noise the graph proved structure-preserving.
