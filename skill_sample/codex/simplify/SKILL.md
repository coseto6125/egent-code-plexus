---
name: simplify
description: Review changed code for reuse, quality, and efficiency, then fix any issues found. Each review agent leverages `ecp` first (when the repo is indexed) so it has graph-aware context — changed files, impact, blind spots, egress, shape drift, and resolver deltas — before reading raw diffs.
---

# Simplify: Code Review and Cleanup (`ecp`-aware)

Review all changed files for reuse, quality, and efficiency. Fix any issues found.

The flow is the original three-agent split. The difference is **the orchestrator runs a `ecp` pre-pass** so each agent walks into the review with changed-file impact, blind spots, egress, shape drift, and resolver deltas already mapped — instead of inferring everything from the raw diff.

## Phase 1: Identify Changes

Run `git diff` (or `git diff HEAD` if there are staged changes) to list what changed. If there are no git changes, fall back to the most recently modified files the user mentioned or that you edited earlier in this conversation.

## Phase 2: Run a single `ecp` pre-pass (orchestrator)

Once, before launching agents:

1. **Probe whether the repo is indexed.** Run `ecp review --repo . --format toon`. If it fails because `.ecp/graph.bin` is missing or stale, run `ecp admin index --repo .` once, then retry `ecp review --repo . --format toon`. If `ecp` is unavailable or indexing fails, continue with raw diff review and note the missing graph context.
2. **Choose the review scope.**
   - Working-tree changes: `ecp review --repo . --format toon`
   - Changes since a baseline: `ecp review --repo . --since <ref> --format toon`
   - Explicit file list: `ecp review --repo . --files path/a.rs,path/b.rs --format toon`
3. **Capture four artefacts** to hand to every agent:
   - `ecp_review`: output from `ecp review` (impact, blind spots, egress/tool-map, shape-check, resolver diff)
   - `changed_files`: file list from `git diff --name-only` or the PR diff
   - `resolver_delta`: if needed, `ecp diff --repo . --section all --baseline <ref>`
   - `risk`: LOW / MEDIUM / HIGH / CRITICAL summary inferred from the review output
4. If `ecp review` or `ecp diff` flags HIGH or CRITICAL risk, surface that to the user before the agents launch — don't bury it in the agent reports.

Hand the artefacts to each agent verbatim in their prompt so they can scope correctly. Tell them to use `ecp inspect --repo . --name "<symbol>"`, `ecp impact --repo . "<symbol>" --direction up`, `ecp find --repo . "<pattern>" --mode bm25`, or `ecp diff --repo . --section all --baseline <ref>` themselves on any symbol or flow they want to dig into; the orchestrator only does the broad-strokes mapping.

## Phase 3: Launch three review agents in parallel

Send a single message with three Agent tool uses so they run concurrently. Each agent gets the full diff plus the Phase-2 artefacts.

**Common preamble for every agent prompt:**

> The repo is at `<absolute path>`. The diff to review is in `<location>`. The `ecp` pre-pass found:
> - ecp_review: `<summary or path to captured output>`
> - changed_files: `<list>`
> - resolver_delta: `<summary or not run>`
> - risk: `<level>`
>
> Use this to focus your review on the symbols / flows that actually changed. Skip large mechanical sections (rename-only, formatting-only) when `ecp review` shows no execution impact. If you want depth on any symbol, run `ecp inspect --repo . --name "<symbol>"`. For blast-radius questions on a refactor, run `ecp impact --repo . "<symbol>" --direction up`. For "is this duplicating an existing function?", run `ecp find --repo . "<concept>" --mode bm25` before falling back to grep.

### Agent 1: Code Reuse Review

For each substantive change:

1. **Search for existing utilities and helpers** that could replace newly written code. The graph is the right primary source — `ecp find --repo . "upsert bot" --mode bm25` or `ecp inspect --repo . --name "BotInfo"` finds matches that grep can miss. Fall back to grep only when graph turns up empty.
2. **Flag any new function that duplicates existing functionality.** Suggest the existing function to use instead (with its file:line).
3. **Flag any inline logic that could use an existing utility** — hand-rolled string manipulation, manual path handling, custom environment checks, ad-hoc type guards.

### Agent 2: Code Quality Review

Review the same changes for hacky patterns:

1. **Redundant state** — duplicates existing state, cached values that could be derived, observers/effects that could be direct calls
2. **Parameter sprawl** — adding new parameters instead of generalising or restructuring existing ones
3. **Copy-paste with slight variation** — near-duplicate blocks that should share an abstraction (use `ecp find --repo . "<concept>" --mode bm25` to confirm the duplicate isn't already canonical somewhere)
4. **Leaky abstractions** — exposing internal details that should be encapsulated, or breaking existing abstraction boundaries (`ecp inspect --repo . --name "<symbol>"` shows the boundary)
5. **Stringly-typed code** — raw strings where constants, enums, or branded types already exist
6. **Unnecessary JSX nesting** — wrapper Boxes/elements that add no layout value
7. **Nested conditionals 3+ levels deep** — flatten with early returns, guard clauses, lookup tables, or if/else-if cascades
8. **Unnecessary comments** — comments explaining WHAT (delete; well-named identifiers do that), narrating the change, or referencing the task/caller — keep only non-obvious WHY

### Agent 3: Efficiency Review

Review the same changes for efficiency:

1. **Unnecessary work** — redundant computations, repeated file reads, duplicate API calls, N+1 patterns
2. **Missed concurrency** — independent operations run sequentially when they could run in parallel
3. **Hot-path bloat** — new blocking work added to startup or per-request/per-render hot paths. Use `ecp impact --repo . "<symbol>" --direction up` on the changed symbol to see if it sits inside a request hot path.
4. **Recurring no-op updates** — state/store updates inside polling loops that fire unconditionally; verify wrapper functions honour same-reference returns
5. **Unnecessary existence checks** — pre-checking file/resource existence before operating (TOCTOU) — operate directly and handle the error
6. **Memory** — unbounded data structures, missing cleanup, event listener leaks
7. **Overly broad operations** — reading entire files when only a portion is needed, loading all items when filtering for one

## Phase 4: Aggregate and fix

Wait for all three agents to complete. Aggregate findings and fix each issue directly. If a finding is a false positive or not worth addressing, note it and move on — do not argue, just skip.

After fixes:
- If the repo is indexed, run `ecp review --repo . --format toon` once more to confirm the fixes only touched the flows you intended. When reviewing against a PR baseline, use the same `--since <ref>` or `--files ...` scope as the first pass.
- Briefly summarise what was fixed (or confirm the code was already clean).

## Notes on cost

The orchestrator pre-pass is one tool call. The per-agent `ecp` calls are bounded — encourage agents to start narrow (one symbol, one process) and widen only if they need to. The graph-aware approach is **token-cheaper than reading large diffs** because the agent skips the mechanical noise (renames, formatting) the graph already proved is structure-preserving.
