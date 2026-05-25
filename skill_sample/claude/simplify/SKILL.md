---
name: simplify
description: Review changed code for reuse, quality, and efficiency, then fix any issues found. Each review agent leverages gnx first (when the repo is indexed) so it has graph-aware context — affected symbols, blast radius — before reading raw diffs.
---

# Simplify: Code Review and Cleanup (gnx-aware)

Review all changed files for reuse, quality, and efficiency. Fix any issues found.

The flow is the original three-agent split. The difference is **each agent runs a gnx pre-pass** so it walks into the review with affected symbols, callers, and blast radius already mapped — instead of inferring everything from the raw diff.

## Phase 1: Identify Changes

Run `git diff` (or `git diff HEAD` if there are staged changes) to list what changed. If there are no git changes, fall back to the most recently modified files the user mentioned or that you edited earlier in this conversation.

## Phase 2: Run a single gnx pre-pass (orchestrator)

Once, before launching agents:

1. **Probe whether the repo is gnx-indexed.** Try `gnx impact --baseline HEAD~1 --repo . --format json` (use the merge-base ref for PR reviews, e.g. `--baseline origin/main`). If gnx isn't installed or the repo isn't indexed (`gnx admin index --repo .` to fix), skip silently — the rest of this skill still works without graph context.
2. **Capture two artefacts** to hand to every agent:
   - `changed_symbols`: which symbols the diff hunks resolve to (function / class / module) — from `impact --baseline`
   - `impact_by_symbol`: upstream callers per changed symbol — same call
3. If d=1 upstream callers count is high (>10) or hits auth / payment / external-API paths, surface that as **HIGH risk** to the user before the agents launch — don't bury it in the agent reports.

Hand the artefacts to each agent verbatim in their prompt so they can scope correctly. Tell them to call `gnx inspect --name X --repo .` or `gnx impact --target X --direction upstream --repo .` themselves on any symbol they want to dig into; the orchestrator only does the broad-strokes mapping.

## Phase 3: Launch three review agents in parallel

Send a single message with three Agent tool uses so they run concurrently. Each agent gets the full diff plus the Phase-2 artefacts.

**Common preamble for every agent prompt:**

> The repo is at `<absolute path>`. The diff to review is in `<location>`. The gnx pre-pass found:
> - changed_symbols: `<list>`
> - impact_by_symbol: `<map symbol → upstream callers>`
> - risk: `<level>`
>
> Use this to focus your review on the symbols that actually changed. Skip large mechanical sections (rename-only, formatting-only) — the graph already confirms those don't alter execution. If you want depth on any symbol, call `gnx inspect --name X --repo .`. For blast-radius questions on a refactor, call `gnx impact --target X --direction upstream --repo .`. For "is this duplicating an existing function?" call `gnx search "<concept>" --repo .`.

### Agent 1: Code Reuse Review

For each substantive change:

1. **Search for existing utilities and helpers** that could replace newly written code. The graph is the right primary source — `gnx search "upsert bot" --repo .` or `gnx inspect --name BotInfo --repo .` finds matches the grep won't. Fall back to grep only when graph turns up empty.
2. **Flag any new function that duplicates existing functionality.** Suggest the existing function to use instead (with its file:line).
3. **Flag any inline logic that could use an existing utility** — hand-rolled string manipulation, manual path handling, custom environment checks, ad-hoc type guards.

### Agent 2: Code Quality Review

Review the same changes for hacky patterns:

1. **Redundant state** — duplicates existing state, cached values that could be derived, observers/effects that could be direct calls
2. **Parameter sprawl** — adding new parameters instead of generalising or restructuring existing ones
3. **Copy-paste with slight variation** — near-duplicate blocks that should share an abstraction (use `gnx search` to confirm the duplicate isn't already canonical somewhere)
4. **Leaky abstractions** — exposing internal details that should be encapsulated, or breaking existing abstraction boundaries (`gnx inspect` shows the boundary)
5. **Stringly-typed code** — raw strings where constants, enums, or branded types already exist
6. **Unnecessary JSX nesting** — wrapper Boxes/elements that add no layout value
7. **Nested conditionals 3+ levels deep** — flatten with early returns, guard clauses, lookup tables, or if/else-if cascades
8. **Unnecessary comments** — comments explaining WHAT (delete; well-named identifiers do that), narrating the change, or referencing the task/caller — keep only non-obvious WHY

### Agent 3: Efficiency Review

Review the same changes for efficiency:

1. **Unnecessary work** — redundant computations, repeated file reads, duplicate API calls, N+1 patterns
2. **Missed concurrency** — independent operations run sequentially when they could run in parallel
3. **Hot-path bloat** — new blocking work added to startup or per-request/per-render hot paths. Use `gnx impact --target X --direction upstream --repo .` on the changed symbol to see if it sits inside a request hot path.
4. **Recurring no-op updates** — state/store updates inside polling loops that fire unconditionally; verify wrapper functions honour same-reference returns
5. **Unnecessary existence checks** — pre-checking file/resource existence before operating (TOCTOU) — operate directly and handle the error
6. **Memory** — unbounded data structures, missing cleanup, event listener leaks
7. **Overly broad operations** — reading entire files when only a portion is needed, loading all items when filtering for one

## Phase 4: Aggregate and fix

Wait for all three agents to complete. Aggregate findings and fix each issue directly. If a finding is a false positive or not worth addressing, note it and move on — do not argue, just skip.

After fixes:
- If the repo is ecp-indexed, run `ecp find <changed-symbol> --repo .` (or `ecp inspect --name <symbol> --repo .`) to confirm fixed symbols still resolve in the graph.
- Briefly summarise what was fixed (or confirm the code was already clean).

## Notes on cost

The orchestrator pre-pass is one CLI call. The per-agent gnx calls are bounded — encourage agents to start narrow (one symbol) and widen only if they need to. The graph-aware approach is **token-cheaper than reading large diffs** because the agent skips the mechanical noise (renames, formatting) the graph already proved is structure-preserving.
