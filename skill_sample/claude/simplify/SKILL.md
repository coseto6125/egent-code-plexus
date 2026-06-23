---
name: simplify
description: Review changed code for bugs, reuse, quality, and efficiency, then fix any issues found. Dispatch is tiered by diff size — small diffs get a single-pass self-review (zero agents), only large or risky diffs fan out to parallel agents. Reviewers leverage ecp first (when the repo is indexed) for graph-aware context.
---

# Simplify: Code Review and Cleanup (ecp-aware, tiered)

Review all changed files for correctness bugs, reuse, quality, and efficiency; fix any issues found. Agent fan-out is the exception, not the default — the tier ladder below decides how much machinery the diff actually earns. Reviewers run an ecp pre-pass so they enter with affected symbols, callers, and blast radius already mapped instead of inferring them from the raw diff.

## Phase 1: Identify changes

`git diff` (or `git diff HEAD` for staged changes) to list what changed. No git changes → fall back to the most recently modified files the user mentioned or you edited earlier. Record: file count, LOC changed, and whether the diff is docs/comments-only or tests-only.

## Phase 2: One ecp pre-pass (orchestrator)

Once, before any review:

1. **Probe the index.** `ecp impact --baseline HEAD~1 --repo . --format json` (use the merge-base for PR reviews, e.g. `--baseline origin/main`). If ecp isn't installed or the repo isn't indexed (`ecp admin index --repo .` to fix), skip silently — the skill still works without graph context.
2. **Capture for every reviewer:** `changed_symbols` (which symbols the diff hunks resolve to) and `impact_by_symbol` (upstream callers per changed symbol) — both from `impact --baseline`.
3. If a changed symbol's upstream caller count is high (>10) or it hits auth / payment / external-API paths, surface that as **HIGH risk** before reviewing — don't bury it in reports.

## Phase 2.5: Tier the dispatch

Pick the LOWEST tier the diff qualifies for. Size alone never rounds up; only the risk signals listed in Tier 3 do.

| Tier | When | Dispatch |
|------|------|----------|
| 0 | docs / comments / lockfile-only diff | No review. Say so and stop. |
| 1 | <3 files **or** <100 LOC | **Zero agents.** Single-pass self-review by the orchestrator against the merged checklist below (~50–70k tokens saved vs fan-out). |
| 2 | ≤10 files **and** ≤400 LOC | **One agent** carrying the merged checklist (all dimensions in one prompt). |
| 3 | bigger, or cross-crate/cross-package, or Phase-2 flagged HIGH risk | Parallel agents — but only the dimensions the diff earns (next section). |

**Dimension selection for Tier 3** — drop dimensions the diff can't violate instead of always launching all of them:

- **Correctness (bugs)** — always. Prefer `subagent_type: feature-dev:code-reviewer` when available (its built-in system prompt is tuned for bug/security hunting with confidence filtering); else general-purpose with the checklist below.
- **Quality** — always.
- **Reuse** — only when the diff ADDS new functions/utilities (pure deletions, renames, or edits inside existing bodies can't duplicate anything new).
- **Efficiency** — only when the diff touches non-test code (a tests-only diff gets Correctness + Quality only).

Typical Tier 3 is therefore 2–4 agents, not an unconditional full fan-out.

## Phase 3: Run the review

Reviewers (or the orchestrator at Tier 1) get the diff plus the Phase-2 artefacts.

**Common preamble (agents only):**

> Repo at `<absolute path>`. Diff in `<location>`. ecp pre-pass found:
> - changed_symbols: `<list>`
> - impact_by_symbol: `<symbol → upstream callers>`
> - risk: `<level>`
>
> Focus on the symbols that actually changed; skip rename-only / formatting-only sections (the graph confirms those don't alter execution). Dig in with `ecp inspect --name X --repo .`; blast radius with `ecp impact --target X --direction upstream --repo .`; "does this duplicate an existing function?" with `ecp find "<concept>" --repo .`.
> Report each finding as: file:line, what and why, suggested fix, **confidence 0–100**. Report only findings you'd defend at 50+; do not pad with nitpicks.

### Checklist — Correctness (bugs)

1. **Logic errors** — inverted conditions, off-by-one, wrong operator, dead branches that should be live
2. **Boundary/empty cases** — empty input, zero/one element, max sizes, saturating vs wrapping arithmetic
3. **Error-handling gaps** — swallowed errors, unwrap/expect on fallible paths reachable in production, partial-failure states left inconsistent
4. **Null/None/undefined flows** — optional values dereferenced on paths where absence is possible
5. **Concurrency** — racy check-then-act, shared state without synchronization, lock ordering, await points invalidating earlier reads
6. **Resource lifecycle** — leaks (files, sockets, listeners), double-free/double-close, missing cleanup on early return
7. **Contract breakage** — callers relying on the OLD behavior of a changed function (`ecp impact --target X --direction upstream` enumerates them; verify each caller survives the change)
8. **Security** — injection (SQL/shell/path), unvalidated external input crossing a trust boundary, secrets in logs

### Checklist — Reuse

1. **Existing utilities that replace new code.** Graph first — `ecp find "upsert bot" --repo .` / `ecp inspect --name BotInfo --repo .` finds matches grep won't; fall back to grep only when the graph is empty.
2. **New function duplicating existing functionality** — suggest the existing one (file:line).
3. **Inline logic that an existing utility covers** — hand-rolled string manipulation, manual path handling, custom env checks, ad-hoc type guards.

### Checklist — Quality

1. **Redundant state** — duplicates existing state, cacheable-derivable values, observers that could be direct calls
2. **Parameter sprawl** — new params instead of restructuring existing ones
3. **Copy-paste with variation** — near-duplicate blocks needing a shared abstraction (`ecp find` to confirm it isn't already canonical somewhere)
4. **Leaky abstractions** — exposing internals or breaking abstraction boundaries (`ecp inspect` shows the boundary)
5. **Stringly-typed code** — raw strings where constants / enums / branded types exist
6. **Unnecessary JSX nesting** — wrapper elements adding no layout value
7. **Nested conditionals 3+ deep** — flatten with early returns, guard clauses, lookup tables
8. **WHAT-comments** — delete (identifiers say it); keep only non-obvious WHY

### Checklist — Efficiency

1. **Unnecessary work** — redundant computation, repeated reads, duplicate API calls, N+1
2. **Missed concurrency** — independent operations run sequentially
3. **Hot-path bloat** — new blocking work in startup / per-request / per-render paths; `ecp impact --target X --direction upstream --repo .` shows if it sits in a hot path
4. **Recurring no-op updates** — unconditional store updates in polling loops; verify wrappers honour same-reference returns
5. **TOCTOU existence checks** — operate directly and handle the error instead of pre-checking
6. **Memory** — unbounded structures, missing cleanup, listener leaks
7. **Overly broad ops** — reading whole files / loading all items when a portion suffices

## Phase 4: Aggregate and fix (confidence-gated)

Wait for all reviewers, aggregate, then act by confidence — mirrors the built-in code-reviewer's high-priority-only filtering:

- **≥70**: fix directly.
- **50–69**: list in the summary as "worth a look", don't fix unprompted.
- **<50**: drop silently.

False positive at any confidence → note and skip, don't argue. After fixing, if the repo is ecp-indexed, `ecp find <changed-symbol> --repo .` to confirm fixed symbols still resolve. Summarise what was fixed (or confirm it was already clean), including the tier chosen and why.

## Cost

The tier ladder is the main cost control: most pre-push diffs are Tier 1 and cost zero agents. The pre-pass is one CLI call; per-reviewer ecp calls are bounded — start narrow (one symbol), widen only if needed. Graph-aware review is **token-cheaper than reading large diffs**: reviewers skip mechanical noise (renames, formatting) the graph already proved is structure-preserving.
