---
name: simplify
description: Review changed code for reuse, quality, and efficiency, then fix any issues found. Each review agent leverages ecp first (when the repo is indexed) so it has graph-aware context — affected symbols, blast radius — before reading raw diffs.
---

# Simplify: Code Review and Cleanup (ecp-aware)

Review all changed files for reuse, quality, and efficiency; fix any issues found. The flow is the original three-agent split, but **each agent runs an ecp pre-pass** so it enters the review with affected symbols, callers, and blast radius already mapped instead of inferring them from the raw diff.

## Phase 1: Identify changes

`git diff` (or `git diff HEAD` for staged changes) to list what changed. No git changes → fall back to the most recently modified files the user mentioned or you edited earlier.

## Phase 2: One ecp pre-pass (orchestrator)

Once, before launching agents:

1. **Probe the index.** `ecp impact --baseline HEAD~1 --repo . --format json` (use the merge-base for PR reviews, e.g. `--baseline origin/main`). If ecp isn't installed or the repo isn't indexed (`ecp admin index --repo .` to fix), skip silently — the skill still works without graph context.
2. **Capture for every agent:** `changed_symbols` (which symbols the diff hunks resolve to) and `impact_by_symbol` (upstream callers per changed symbol) — both from `impact --baseline`.
3. If a changed symbol's upstream caller count is high (>10) or it hits auth / payment / external-API paths, surface that as **HIGH risk** before the agents launch — don't bury it in their reports.

Hand these artefacts to each agent verbatim, and tell them to call `ecp inspect --name X --repo .` or `ecp impact --target X --direction upstream --repo .` on any symbol they want to dig into.

## Phase 3: Three review agents in parallel

One message, three Agent tool uses. Each gets the full diff plus the Phase-2 artefacts.

**Common preamble:**

> Repo at `<absolute path>`. Diff in `<location>`. ecp pre-pass found:
> - changed_symbols: `<list>`
> - impact_by_symbol: `<symbol → upstream callers>`
> - risk: `<level>`
>
> Focus on the symbols that actually changed; skip rename-only / formatting-only sections (the graph confirms those don't alter execution). Dig in with `ecp inspect --name X --repo .`; blast radius with `ecp impact --target X --direction upstream --repo .`; "does this duplicate an existing function?" with `ecp find "<concept>" --repo .`.

### Agent 1: Reuse

1. **Existing utilities that replace new code.** Graph first — `ecp find "upsert bot" --repo .` / `ecp inspect --name BotInfo --repo .` finds matches grep won't; fall back to grep only when the graph is empty.
2. **New function duplicating existing functionality** — suggest the existing one (file:line).
3. **Inline logic that an existing utility covers** — hand-rolled string manipulation, manual path handling, custom env checks, ad-hoc type guards.

### Agent 2: Quality

1. **Redundant state** — duplicates existing state, cacheable-derivable values, observers that could be direct calls
2. **Parameter sprawl** — new params instead of restructuring existing ones
3. **Copy-paste with variation** — near-duplicate blocks needing a shared abstraction (`ecp find` to confirm it isn't already canonical somewhere)
4. **Leaky abstractions** — exposing internals or breaking abstraction boundaries (`ecp inspect` shows the boundary)
5. **Stringly-typed code** — raw strings where constants / enums / branded types exist
6. **Unnecessary JSX nesting** — wrapper elements adding no layout value
7. **Nested conditionals 3+ deep** — flatten with early returns, guard clauses, lookup tables
8. **WHAT-comments** — delete (identifiers say it); keep only non-obvious WHY

### Agent 3: Efficiency

1. **Unnecessary work** — redundant computation, repeated reads, duplicate API calls, N+1
2. **Missed concurrency** — independent operations run sequentially
3. **Hot-path bloat** — new blocking work in startup / per-request / per-render paths; `ecp impact --target X --direction upstream --repo .` shows if it sits in a hot path
4. **Recurring no-op updates** — unconditional store updates in polling loops; verify wrappers honour same-reference returns
5. **TOCTOU existence checks** — operate directly and handle the error instead of pre-checking
6. **Memory** — unbounded structures, missing cleanup, listener leaks
7. **Overly broad ops** — reading whole files / loading all items when a portion suffices

## Phase 4: Aggregate and fix

Wait for all three, aggregate, fix each directly. False positive or not worth it → note and skip, don't argue. After fixing, if the repo is ecp-indexed, `ecp find <changed-symbol> --repo .` to confirm fixed symbols still resolve. Summarise what was fixed (or confirm it was already clean).

## Cost

The pre-pass is one CLI call; per-agent ecp calls are bounded — start narrow (one symbol), widen only if needed. Graph-aware review is **token-cheaper than reading large diffs**: the agent skips mechanical noise (renames, formatting) the graph already proved is structure-preserving.
