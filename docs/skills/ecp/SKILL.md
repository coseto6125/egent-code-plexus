---
name: ecp
description: Use for symbol-level code analysis, blast-radius impact, cross-repo API contracts, AST-aware rename, route map. Defer to grep for string literals, config keys, vendored / generated code, and fs layout.
---

# EgentCodePlexus (ecp) — Structural Analysis Entry

Directives = when to reach for ecp and when to distrust it. Quick Reference = command per task. (@ECP.md)

---

## 🧭 Layer 1: Core Principles

### Directive 1: ecp-first reflex (full rule in @ECP.md §"The reflex")
Code-structure queries go to ecp before grep or an Explore agent — definitions, callers, blast radius, traces, "fan out and read these files." Pick the verb: `find` (definitions), `impact` (callers / blast radius), `inspect` (full context), `routes` / `contracts` (API surfaces), `cypher` (else). Holds for ecp's own repo too.

### Directive 2: Blast Radius before Refactor — and it's a lower bound
Before modifying a function or class, run `ecp impact` for callers (HIGH / CRITICAL → confirm with user). The caller set is a **lower bound**: a bare call to a common name can be suppressed by the resolver's ambiguity cap. **Tell:** suspiciously low caller count → `grep` the call sites to cross-check.

### Directive 3: `found:false` is two-valued — read the `result` field
ecp auto-refreshes the index, but `found:false` can mean "doesn't exist" OR "graph is a warm-attach, HEAD not indexed yet". **Tell:** a `result` field in the payload or an `l2.warm-attach` / `note:` line on stderr → provisional; rerun or `ecp admin index --force --repo .` before concluding it's gone. For genuine misses, `ecp find <fragment> --mode fuzzy`. See [`guides/troubleshooting.md`](./guides/troubleshooting.md).

### Directive 4: Surprising output has a root cause; grep is right for text
Before concluding "ecp is broken", verify against source (definition, fresh reindex, grep cross-check) — doc-comment inference ≠ verification. **Tell:** non-code text — string literals, error messages, config keys, vendored / generated code, fs layout — belongs to grep / Read; ecp parses code, not text.

---

## ⚡ Quick Reference (command × use-case)

### Symbol lookup
| Command | Use for |
|---|---|
| `ecp find <name>` | Exact symbol match (default) |
| `ecp find <n> --mode fuzzy` | Substring match for partial names |
| `ecp find <n> --mode bm25` | BM25-ranked top-K |
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

**Literal mode** (path-string sink lookup):
| Command | Use for |
|---|---|
| `ecp impact --literal session_meta.json` | Read/write sites for that path string, classified (`sink:read` / `sink:write` / `sink:join` / `sink:free` / …). For split-brain bugs, query each literal alone |
| `ecp impact --literal-coherence` | Auto-detect filename split-brain pairs across PathLiteral nodes |

**Related (edge-level)**:
| Command | Use for |
|---|---|
| `ecp diff` | Edge-level resolver delta (binding tier-degradation, route / contract changes) |

### Architecture / cross-cutting
| Command | Use for |
|---|---|
| `ecp summary` | Repo health + frameworks + blind spots |
| `ecp routes <path>` | HTTP route → handler + caller chain |
| `ecp contracts` | Cross-repo API contracts |
| `ecp tool-map` | External HTTP / DB / Redis / queue calls |
| `ecp shape-check` | HTTP consumer ↔ Route response shape drift |
| `ecp processes` | List execution-flow Process nodes (Leiden + BFS at index time) |
| `ecp processes trace <pat>` | Full step sequence for a matching Process — actual execution order, cleaner than `impact --direction down` |
| `ecp review` | Full audit (impact + summary + tool-map + shape-check + diff) |
| `ecp rename <old> <new>` | AST-aware multi-file rename |
| `ecp admin doctor [check] [--fix]` | Environment health (skills / index / host / config / registry / version); `--fix` repairs fixable |

### Multi-repo / groups (cross-repo scope only)
Run in order: `sync` → `contracts` → `impact`.
| Command | Use for |
|---|---|
| `ecp group sync <name>` | Build cross-links + extract contracts for the group |
| `ecp group status <name>` | Check staleness of group members |
| `ecp group contracts <name> [--unmatched]` | Inspect contract registry; `--unmatched` finds orphaned consumers |
| `ecp group impact <name> --target <symbol> --repo <provider>` | Cross-repo blast radius — which repos call this symbol |
| `ecp group find <name>` | Search across all group members |
| `ecp contracts --repo @all` | Registry-wide contract view (no group) |

### Cypher escape hatch
| Command | Use for |
|---|---|
| `ecp cypher "<query>"` | Ad-hoc `MATCH ... RETURN ...` when no command fits |


### Schema introspection (graph-loadless)
| Command | Output |
|---|---|
| `ecp schema blindspots` | Per-lang BlindSpot coverage; "no dispatch in diff" vs "parser doesn't detect it" |
| `ecp schema reltypes` | All 20 RelType edges + LLM-utility category + heuristic flag |
| `ecp schema node-kinds` | All 29 NodeKind variants + same-name distinctions (Struct vs Class, Trait vs Interface) |
| `ecp schema graph-version` | rkyv `graph.bin` format version + bump history |

`schema` commands default to `--format json`; `--format text` for a table.

---

## 📚 On-Demand References

- [`guides/troubleshooting.md`](./guides/troubleshooting.md) — `found:false`, index staleness, resolver misses, the four output-trust tells.
- `_shared/cli/` — Per-command flag references (`inspect`, `impact`, `cypher`, `group`, `processes`, …).
- `_shared/refs/` — Cypher syntax, repo resolution.

