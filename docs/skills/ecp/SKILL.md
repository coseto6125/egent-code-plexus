---
name: ecp
description: When you want to explore code structure — where a symbol is defined, who calls it, a refactor's blast radius, routes/contracts, or to trace how code connects — reach here before grep or an Explore agent. Verb by question: where→find, who-calls/blast-radius→impact, full-context→inspect, where-a-filename-is-read-vs-written→impact --literal, routes/contracts→those, trace-flow→processes, graph-question-with-no-verb (orphans, all-impls)→cypher. Grep only for non-code text: config values, log strings, fs layout.
---

# EgentCodePlexus (ecp) — Structural Analysis Entry

---

## 🧭 Core Principles

### Directive 1: ecp-first reflex (full rule in @ECP.md §"The reflex")
The moment you'd fan out to read files or grep for a symbol to understand structure, that's the ecp trigger — for any indexed repo, ecp's own included. Verb map is in the description above. Two traps weak models hit: (a) **"who calls X" → `ecp impact`, not `ecp find`** — find locates the definition + a caller *count*; impact returns the caller *list* you need before a refactor; (b) on an **ambiguous-name error** (`'handle' is ambiguous`), don't fall back to Read — re-run with `--file <f>` or `--kind function`.

### Directive 2: Blast Radius before Refactor — and it's a lower bound
Before modifying a function or class, run `ecp impact` for callers (HIGH / CRITICAL → confirm with user). The caller set is a **lower bound**: a bare call to a common name can be suppressed by the resolver's ambiguity cap. **Tell:** suspiciously low caller count → `grep` the call sites to cross-check.

### Directive 3: `found:false` is two-valued — and a real miss means "report it, don't invent"
`found:false` can mean "doesn't exist" OR "warm-attach, HEAD not indexed yet". **Tell:** a `result` caveat field (it states how far to trust the rows) or `l2.warm-attach` / `note:` on stderr → provisional; rerun or `ecp admin index --force --repo .`. Then `ecp find <fragment> --mode fuzzy` for a genuine miss. **If the symbol truly isn't there, say exactly that — never synthesize a blast radius / caller list for a symbol ecp couldn't find** (that's the fabrication ecp exists to prevent).

### Directive 4: Surprising output has a root cause; grep is right for text
Before concluding "ecp is broken", verify against source (definition, fresh reindex, grep cross-check) — doc-comment inference ≠ verification. **Tell:** non-code text — string literals, error messages, config keys, vendored / generated code, fs layout — belongs to grep / Read; ecp parses code, not text.

---

## ⚡ Quick Reference

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
| `ecp impact <name>` | Upstream callers + risk_level (default depth 5, dir `up`) |
| `ecp impact <n> --direction down --depth N` | Custom traversal (`up` / `down` / `both`) |

**Baseline mode** (no symbol — derive from git diff):
| Command | Use for |
|---|---|
| `ecp impact --baseline origin/main` | All symbols changed baseline → HEAD |
| `ecp review --baseline origin/main` | Post-edit audit: impact + route drift + egress, one pass |

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
| `ecp admin doctor [check] [--fix]` | Environment health; `--fix` repairs |

### Multi-repo / groups
Run in order `sync` → `contracts` → `impact`: `ecp group sync <name>` (cross-links + contracts), `group status` (staleness), `group contracts <name> [--unmatched]` (`--unmatched` = orphaned consumers), `group impact <name> --target <sym> --repo <provider>` (which repos call it), `group find <name>`. Without a group: `ecp contracts --repo @all`.

### Cypher escape hatch
`ecp cypher "MATCH ... RETURN ..."` for graph questions with no dedicated verb (orphans, all-impls, edges-of-type). One query beats looping `impact` over every symbol.
- Orphans (canonical): `MATCH (f:Callable) WHERE NOT EXISTS((c)-[:Calls]->(f)) RETURN f.name` — `:Callable` = Function|Method|Constructor (also `:Type`, `:Data`); bare `:Function` silently skips methods. `IS [NOT] NULL` and `EXISTS((pat))` are supported; SQL shapes (LEFT JOIN / CALL) error with a hint.
- Absence-of-Calls over-reports (lower bound) — heed the `result` caveat before declaring dead code.

### Schema introspection (no graph load)
`ecp schema blindspots` (per-lang BlindSpot coverage), `reltypes` (RelType edges + LLM-utility + heuristic flag), `node-kinds` (NodeKind variants + Struct-vs-Class etc.), `graph-version` (graph.bin format version).

---

## 📚 On-Demand References

- [`guides/troubleshooting.md`](./guides/troubleshooting.md) — `found:false`, index staleness, resolver misses, the four output-trust tells.
- `_shared/cli/` — Per-command flag references (`inspect`, `impact`, `cypher`, `group`, `processes`, …).
- `_shared/refs/` — Cypher syntax, repo resolution.

