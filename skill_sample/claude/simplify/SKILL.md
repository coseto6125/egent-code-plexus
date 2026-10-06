---
name: simplify
description: "This is the code review skill on this machine. Use it to review any changed code before it goes anywhere: an uncommitted diff, a branch about to be pushed, or a GitHub PR you must judge merge-ready. It checks spec conformance, bugs, security, reuse, quality, and efficiency, then fixes what it finds — tiered, so most diffs cost zero Claude sub-agents, and every one of them gets a second read from another model family. Reach here when the user says review this, 審一下, look at my diff, is this ready to merge, 可以 merge 了嗎, or check this PR. Other skills reach here for `CHECKLIST.md`, the shared review checklist."
---

# Simplify — tiered, ecp-aware diff review

## Phase 1: Identify changes

`git diff` (or `git diff HEAD` for staged changes) to list what changed. With no git changes, fall back to the files the user named or you edited earlier. Record file count, LOC changed, and whether the diff is docs/comments-only or tests-only.

Then pin the **spec source**, in this order: issue references in the commit messages (`#123`, `Closes #45`) fetched with `gh issue view`; a path the user passed; a spec or PRD under `docs/`, `specs/`, or `.scratch/` matching the branch name. None resolves → the Spec section reports "no spec available", which is an explicit outcome, not a silent skip.

## Phase 2: ecp pre-pass (orchestrator, once)

1. `ecp impact --baseline HEAD~1 --repo . --format json` — use the merge-base for PR reviews (`--baseline origin/main`). Not installed or not indexed → `ecp admin index --repo .`, or skip silently; the skill works without graph context.
2. Capture `changed_symbols` (which symbols the hunks resolve to) and `impact_by_symbol` (upstream callers per changed symbol).
3. Risk floor, per CLAUDE.md Dispatch *Risk is inferred*: a changed symbol with >10 upstream callers, or one on an auth / payment / schema-migration / concurrency / external-API path, is **HIGH**. Tell the user before you review, not at the end of the report.

## Phase 3: Tier the dispatch

Pick the LOWEST tier the diff qualifies for; Phase-2 risk moves it up.

| Tier | When | Dispatch |
|------|------|----------|
| 0 | docs / comments / lockfile-only | No review. Say so and stop. |
| 1 | <3 files **or** <100 LOC | **Zero Claude agents** — the orchestrator self-reviews against [`CHECKLIST.md`](CHECKLIST.md), plus the free third reader below. |
| 2 | ≤10 files **and** ≤400 LOC | **One Claude agent** carrying every checklist section, plus the free third reader below. |
| 3 | bigger, or cross-crate/cross-package, or Phase-2 HIGH risk | Parallel Claude agents, one per dimension the diff can violate. |

Every tier from 1 up also runs codex — see **Cross-family** below. The tier sets how many Claude agents read the diff. A second model family reads it either way.

**Reviewer agent** — Tier 2 and Tier 3 both dispatch `subagent_type: deep-review` with `model: sonnet`: read-only, ecp-aware, already carrying the confidence protocol. At Phase-2 HIGH risk, drop the model override so Correctness runs on its native opus. When the agent list has no `deep-review`, dispatch `general-purpose` with the same model. The Phase-4 preamble carries the same rules.

**HIGH runs Tier 3.** HIGH is the top level Phase 2 assigns, so it never qualifies a diff out of the tier it just earned.

**Cross-family — always.** Every tier from 1 up launches `codex` alongside its own agents: a different model family is the only reader whose mistakes are uncorrelated with yours. Tier 0 launches nothing.

Without the `peer-agent` skill, use the detached form below. With it, pick the mode from its *Two ways to run a peer*: supervised through
Orca when Orca is up, so the report arrives as a `worker_done` message you wait on;
detached otherwise. The detached form runs from the repo root at the same time as the
tier's agents. Write the brief to a file, then start this as a background Bash call:

```bash
bash ~/.claude/skills/simplify/codex-review.sh "<scratchpad>/codex-brief.md" "<scratchpad>/codex-review.md"
```

The script launches codex read-only at `medium`, waits for its report, and ends the codex process. It returns when the review ends, so the harness's completion notification is the signal: wait for it, then read the report file. A background call you start this way is the only watcher it needs.

The brief carries the diff location and what the change is for. It also carries the decisions that were settled by argument rather than measurement, what any earlier round already found, and the finding format the agent preamble names. Ask it for a **challenge list**: every way a reader could legitimately attack this diff. Tell it that "clean, no findings" is a welcome result. The challenge list is the part same-family reviewers cannot give you.

Every tier launches at `medium`; the `peer-agent` skill's *Model and effort* holds that rule. A brief wider than one reader holds is split across several peers at `medium`, one per dimension, each carrying only its own dimension: one brief file and one report file per peer.

A non-zero exit is a downgrade. Exit 2 means codex ended without a report, 3 means no report before the timeout, and 4 means codex is missing or not logged in. Report the stderr reason in the summary as `cross-family skipped: <reason>`. A failed run keeps its log at `<report>.log`: read the end of that log before you call the run skipped.

**Free third reader — Tiers 1 and 2.** Both tiers also send the whole diff to `nvidia/nemotron-3-ultra-550b-a55b:free`, a third model family whose capacity costs nothing. Start it as a background Bash call, as for codex. Read its report after the completion notification arrives.

```bash
python3 ~/.claude/skills/simplify/free-reader.py "<scratchpad>/free-brief.md" "<scratchpad>/free-review.md"
```

Write the brief to a file first. It is the Phase-4 agent preamble with the diff pasted in full and the ecp lines dropped: this reader has no tools and no repo access, so a path it cannot open is a section it cannot review. Drop the absolute repo path with them, and say what the code is for instead — this brief leaves the machine, and the path names the user, the client and the project. Read the diff before you send it: a hunk that REMOVES a credential still carries that credential in its `-` lines.

**Read its report as leads, not as findings.** Take the file:line and the claim, verify the location yourself, and discard its failure_scenario and confidence: they never enter the Phase-5 ladder. Treat concurrency and lock scope as uncovered by this reader, whatever its report says.

A third argument naming a context file (the full text of the touched files, plus the callers `ecp impact --direction upstream` names) makes it re-report against that file. It is optional; skip it when you verify the locations yourself anyway.

Launch exactly one, and only at Tier 1 or Tier 2: its recall falls as the diff grows, and parallel calls hang. A `TRUNCATED` line at the end of its report means the tail is missing, not clean. A non-zero exit prints its reason; report it as `free reader skipped: <reason>` and carry on.

> Measured 2026-09-11 on the earlier `nemotron-3-super-120b-a12b:free`, not yet on ultra: 4 of 5 planted defects located per trial, every failure_scenario invented, a lock held across a network call missed in every read; 4 of 4 on a 52-line diff, 2.3 of 4 on a 2034-line one.

**Dispatching is part of the invocation.** The agents the tier names need no separate approval; launch them.

Walk the dimensions yourself only when the `Agent` tool is absent from your tool set, and open the summary with `Tier <n>, run inline: Agent tool unavailable`. That is a downgrade you report, not a judgement call you justify.

**Tier-3 dimensions** — launch only the ones the diff can violate, typically 2–4:

- **Correctness** and **Quality** — always. The Quality agent carries the Conventions section too, so project rules cost no extra agent.
- **Spec** — only when Phase 1 resolved a spec source. Its own agent, so intent findings are never reranked against style findings.
- **Reuse** — only when the diff ADDS functions or utilities (deletions, renames, and edits inside existing bodies duplicate nothing new). Mechanical graph lookup, so `subagent_type: lite-scan` instead, or `general-purpose` with `model: haiku` when `lite-scan` is not defined.
- **Security** — only when the diff touches a route table, an auth or session path, a tenancy check, a permission, a credential, a webhook handler, a tool the model can call, or a server-side fetch of a caller-supplied URL. The Security section routes into `~/.claude/skills/simplify/security/SURFACES.md`, which probes the repo for surfaces and loads depth only for those it finds. Phase 2 already raises those paths to HIGH, so this dimension and Tier 3 arrive together. Runs on `deep-review` at its native opus.
- **Efficiency** — only when the diff touches non-test code.

## Phase 4: Run the review

Every reviewer — the orchestrator itself at Tier 1 — walks **every rung of every checklist section it owns**, and is done only once each rung has ended as a finding or as explicitly clear.

The preamble below carries the review rules, not just the dispatch text: scope, confidence floor, and blind spots bind an inline pass exactly as they bind an agent.

Agent preamble:

> Repo at `<absolute path>`. Diff in `<location>`. Spec at `<path or fetched issue, else "none">`. Apply every rung of the `<sections>` sections of `~/.claude/skills/simplify/CHECKLIST.md`.
> ecp pre-pass — changed_symbols: `<list>` · impact_by_symbol: `<symbol → upstream callers>` · risk: `<level>` · commands: one `ecp impact --target <symbol> --direction upstream --repo .` per changed symbol, listed here.
> Review the symbols that actually changed; the graph already proved the rename-only and formatting-only sections structure-preserving. That proof covers code only: *Prose drift* still searches the text for every renamed name. Read the enclosing function of every hunk: a bug on an unchanged line of a touched function is in scope, because the diff re-exposes it. Run every command the pre-pass lists and paste each output under the finding it supports; a definition question goes to `ecp inspect --name X --repo .` and a reuse question to `ecp find "<concept>" --repo .`, output pasted the same way.
> Open with the rung ledger that CHECKLIST.md defines. Report each finding as file:line, what and why, suggested fix, a **failure_scenario** (concrete inputs or state, and the wrong output or crash they produce), and **confidence 0–100**. A finding you cannot give a failure_scenario for is not a finding — drop it rather than lowering its confidence. Realistic-but-rare state keeps its confidence: a race, a nil on a cold-cache path, a falsy zero, a boundary the code does not exclude. Score a finding below 50 only when the code refutes it — quote the line that makes it impossible, or the guard that already handles it. Carry the command you ran and its raw output so the orchestrator re-checks without redoing your search. Close with your blind spots: what you did not read, run, or verify. Report the findings you would defend at 50 or above.

**Sweep — Phase-2 HIGH risk only.** Once the reviewers return, take one more pass yourself over the diff and its enclosing functions, holding their finding list. Look only for what the list misses: moved code that dropped a guard or an anchor, a default evaluated once at definition, a lock scope that shrank, setup/teardown asymmetry in tests, a config default flipped. An empty sweep is a valid result.

## Phase 5: Aggregate and fix

Wait for every reviewer, codex and the free reader included. Act by confidence, scored against the anchors in [`CHECKLIST.md`](CHECKLIST.md#confidence); the free reader's own scores do not count toward these bands, per its section above:

- **≥50, the fix gate** — re-check the finding against the evidence it carries, then fix. Re-checking is one jump to the cited line or command output, not a repeat of the reviewer's search. The 50 anchor already means the finding is real. A finding whose evidence the re-check cannot confirm is not fixed: it goes under `## Scanned, not acted on`.
- **<50** — drop. A finding you spent a command investigating and then rejected goes under a `## Scanned, not acted on` heading instead: one line each, carrying its score and why it stays.

Fix everything you can reach. Three classes stay unapplied and go to the summary as proposals, each carrying the concrete change it proposes and one line saying why it stays unapplied:

- a fix that changes intended behaviour
- a fix that reaches outside the files the diff touches, except a *Prose drift* fix: rewrite that stale sentence in any file of the repo
- a fix big enough to be its own change: a refactor, an API change, a migration

A false positive at any confidence → note it and move on. After fixing, `ecp find <changed-symbol> --repo .` confirms the fixed symbols still resolve.

Summarise what was fixed (or that the diff was already clean), and which tier ran and why. Report the cross-family findings separately from the tier's. A finding two model families reach independently outranks its confidence score. A finding only codex reached is the reason it runs. Rank Correctness findings above Reuse, Quality, Conventions, and Efficiency findings.

Spec findings sit under their own `## Spec` heading above the ranked list, and keep their own worst-issue line — a diff can be clean on every other axis and still build the wrong thing, so the two are never ranked against each other.
