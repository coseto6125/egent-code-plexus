---
name: ecp
description: Tracing who-calls-X or a data flow — mid-debug, not only before a refactor — or exploring code structure: where a symbol is defined, who calls it, blast radius, routes/contracts. Reach here before grep. Command by question: definition→`ecp find`, who-calls/blast-radius→`ecp impact`, how-A-reaches-B→`ecp path <from> <to>`, full context→`ecp inspect`, filename-read-vs-written→`ecp impact --literal`, routes/contracts→`ecp routes`/`ecp contracts`, trace execution flow→`ecp processes`, statement shape inside code (swallowed exception, missing timeout)→`ecp pattern`, event-topic / saga / schema-field pairings→`ecp heuristics`, graph question with no verb (orphans, all-impls)→`ecp cypher`. Grep only for non-code text: config values, log strings, fs layout.
---

# EgentCodePlexus (ecp) — Structural Analysis Entry

## Core rules

1. **ecp-first.** The moment you'd fan out to read files or grep a symbol to understand structure, that IS the ecp trigger — any indexed repo, ecp's own included. "Who calls X" → `ecp impact` (returns the caller *list*); `ecp find` only locates the definition.
2. **Blast radius before refactor — and it's a lower bound.** Before changing a function or class, run `ecp impact <name>`; many callers, or callers in core / widely-imported modules → confirm with the user first. The resolver suppresses ambiguous bare calls to common names, so the caller set is a lower bound: a suspiciously low count for a common name → `grep` the call sites to cross-check.
3. **Honest miss.** `found:false` carrying a `result` caveat field is provisional — do what the caveat says (rerun, or `ecp admin index --force --repo .`). `found:false` with no caveat is trustworthy: try `ecp find <fragment> --mode fuzzy` for name drift, then report "doesn't exist" — never synthesize a caller list or blast radius for a symbol ecp couldn't find.
4. **Text → grep.** String literals, log messages, config keys, fs layout, vendored / generated code: grep / Read. ecp parses code, not text. Between the two sits statement *shape* (a `try` that swallows, a call missing an argument): that is `ecp pattern`, not grep. For any other surprising output, find the root cause before calling it a bug: [`guides/troubleshooting.md`](./guides/troubleshooting.md).

## Quick Reference

### Symbol lookup
| Command | Use for |
|---|---|
| `ecp find <name>` | Exact symbol match (default) |
| `ecp find <n> --mode fuzzy\|bm25` | Substring match / BM25-ranked top-K |
| `ecp find <n> --kind function,method` | Filter by symbol kind |
| `ecp inspect --name <n>` | Full context: signature + body + edges + callers |

### Impact / blast radius — three **mutually exclusive** modes, pick by what you have

**Symbol mode** (you know the symbol name):
| Command | Use for |
|---|---|
| `ecp impact <name>` | Upstream callers with file:line + counts (default depth 5, dir `up`) |
| `ecp impact <n> --direction down --depth N` | Custom traversal (`up` / `down` / `both`) |

**Baseline mode** (no symbol — derive from git diff):
| Command | Use for |
|---|---|
| `ecp impact --baseline origin/main` | All symbols changed baseline → HEAD |
| `ecp review --baseline origin/main` | Post-edit audit: impact + route drift + egress, one pass |

**Two symbols, not one** — `ecp path <from> <to>` returns the ordered chain plus the edge per hop, which `impact` (one endpoint) and cypher's `-[:Calls*1..N]->` (endpoints only, route dropped) cannot. Default `--direction down` follows callees; a miss names the direction that works. Heuristic edges are off by default here, the reverse of `impact`: a path has no second bucket to put an unverified step in.

**Literal mode**: `ecp impact --literal session_meta.json` → each site classified `sink:read`/`write`/`join`/`free` (grep can't tell read from write); `--literal-coherence` finds split-brain filename pairs.

`ecp diff` — edge-level resolver delta (route/contract changes).

### Architecture / cross-cutting
| Command | Use for |
|---|---|
| `ecp summary` | Repo health + frameworks + blind spots |
| `ecp routes <path>` | HTTP route → handler + caller chain |
| `ecp contracts` | Cross-repo API contracts |
| `ecp tool-map` | External HTTP / DB / Redis / queue calls |
| `ecp shape-check` | HTTP consumer ↔ Route response shape drift |
| `ecp processes` / `processes trace <pat>` | Execution-flow steps in real order — cleaner than `impact --direction down` |
| `ecp review` | Full audit (impact + summary + tool-map + shape-check + diff) |
| `ecp rename <old> <new>` | AST-aware multi-file rename |
| `ecp pattern -p '<pat>'` | Statement shapes the graph has no node for; `--callers-of` scopes by reachability |
| `ecp heuristics <kind>` | Confidence-tagged pairings no edge proves: `saga`, `schema-bindings`, `event-mirrors` |
| `ecp admin doctor [check] [--fix]` | Environment health; `--fix` repairs |

### Multi-repo / groups
Run in order `sync` → `contracts` → `impact`: `ecp group sync <name>` (cross-links + contracts), `group status` (staleness), `group contracts <name> [--unmatched]` (`--unmatched` = orphaned consumers), `group impact <name> --target <sym> --repo <provider>` (which repos call it), `group find <name>`. Without a group: `ecp contracts --repo @all`.

### Cypher escape hatch
`ecp cypher "MATCH ... RETURN ..."` for graph questions with no dedicated verb (orphans, all-impls, edges-of-type). One query beats looping `impact`.
- Orphans: `MATCH (f:Callable) WHERE NOT EXISTS((c)-[:Calls]->(f)) RETURN f.name` — `:Callable` = Function|Method|Constructor (also `:Type`, `:Data`); bare `:Function` silently skips methods. `IS [NOT] NULL` and `EXISTS((pat))` are supported; SQL shapes (LEFT JOIN / CALL) error with a hint.
- Absence-of-Calls over-reports (lower bound) — heed the `result` caveat before declaring dead code.

### Schema introspection (no graph load)
`ecp schema blindspots` (per-lang BlindSpot coverage), `reltypes` (RelType edges + LLM-utility + heuristic flag), `node-kinds` (NodeKind variants + Struct-vs-Class etc.), `graph-version` (graph.bin format version).

## On-Demand References

- [`guides/troubleshooting.md`](./guides/troubleshooting.md) — `found:false`, index staleness, resolver misses, output-trust tells.
- `_shared/cli/` — Per-command flag references (`inspect`, `impact`, `cypher`, `group`, `processes`, …).
- `_shared/refs/` — Cypher syntax, repo resolution.
