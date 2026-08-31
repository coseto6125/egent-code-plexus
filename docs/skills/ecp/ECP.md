# ECP — Egent Code Plexus (structural code intelligence)

**Usage**: symbol-level code graph for AI agents. Sub-30ms queries; answers "who/what/impact", not "where's this string".

## The reflex

**Wanting to explore code IS the ecp trigger.** The moment the thought forms — "understand this codebase / map the architecture / find who calls X / trace how this connects / fan out and read these files" — the first action is `ecp find` / `ecp impact` / `ecp inspect`, the way "find a string" reaches for grep. It's the default expansion of "explore the code," **ecp's own codebase included**. An Explore agent reads text; ecp reads the graph in one query.

## The one rule

**Code structure → ecp. Text → grep.**

| You want to…                                         | Use                                              | Not  |
|------------------------------------------------------|--------------------------------------------------|------|
| Find a definition (function / class / type)          | `ecp find <name>` / `ecp inspect --name <name>`  | grep |
| Who calls / depends on X (before refactor/rename)   | `ecp impact --target <name> --direction upstream` | grep |
| Blast radius of a diff                               | `ecp impact --baseline <ref>`                    | manual trace |
| How A reaches B (the route, not just the endpoints)  | `ecp path <from> <to>`                           | `ecp cypher` |
| Routes / API contracts / event topics                | `ecp routes` / `ecp contracts` / `ecp find-event-mirrors` | grep |
| Understand any indexed repo's internals              | `ecp impact` / `ecp inspect` / `ecp cypher`      | Explore agent |
| Cross-repo / arbitrary graph query                   | `ecp cypher '<query>'`                           | —    |
| String literal / config key / fs layout / vendored   | grep / glob                                      | ecp  |

Fall back to grep or an Explore agent only when the target is non-code text, or the repo can't be indexed.

## Before any refactor / rename / signature change

`ecp impact --target <symbol> --direction upstream` to see callers. Many callers, or callers in core / widely-imported modules → confirm with user.

## Reading output — four tells

High-signal, with four narrow failure modes. Spot the tell, cross-check, trust the rest. (Resolution steps: `guides/troubleshooting.md`.)

- **`found:false` + a `result` field (or `l2.warm-attach`/`note:` on stderr)** → provisional, not a real miss: HEAD's graph isn't built yet, a sibling commit's is attached. Rerun or `ecp admin index --force --repo .`. No `result` field → trustworthy — and a trustworthy miss means *report "doesn't exist"*, never synthesize a caller list / blast radius for a symbol ecp couldn't find.
- **`ecp impact` caller counts are a lower bound** → the resolver suppresses ambiguous bare calls to common names. Suspiciously low count → `grep` the call sites before trusting a refactor's blast radius.
- **Known gaps, by design** → function-body locals (dropped), Java `record`, PHP `trait use`, C# `operator`/`event`/`indexer`. `ecp summary` lists per-repo BlindSpots.
- **Surprising output has a root cause** → read the definition / reindex / `grep` before calling it a bug. Doc-comment inference ≠ verification; a passing unit test ≠ the pipeline uses that path.
