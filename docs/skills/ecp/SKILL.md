---
name: ecp
description: Use for symbol-level code analysis, blast-radius impact, cross-repo API contracts, AST-aware rename, route map. Defer to grep for string literals, config keys, vendored / generated code, and fs layout.
---

# EgentCodePlexus (ecp) — Structural Analysis Entry

This is the **single entry point** for all structural code analysis, impact assessment, and cross-repo contract verification using EgentCodePlexus.

When you need to understand code connectivity, **DO NOT** just grep for strings. Identify your goal first, then jump to the matching Layer-2 guide.

---

## 🧭 Layer 1: Core Principles

### Directive 1: Symbols over Strings
Always prefer `ecp find` or `ecp inspect` over `grep` when searching for code definitions. `ecp` understands scope, types, and heritage; `grep` only sees text.

### Directive 2: Blast Radius before Refactor
Before modifying a function or class, always run `ecp impact` to see who calls it. If the risk is HIGH or CRITICAL, stop and confirm with the user.

### Directive 3: Automatic Indexing
`ecp` auto-detects changes and rebuilds the index on demand. You rarely need to run `ecp admin index` manually. If a symbol is missing, try `ecp find --mode fuzzy`.

### Directive 4: Grep When It's Actually Right
Use grep / Read for: string literals, error messages, config keys (toml / yaml / json), vendored / generated code, file-system layout (`find . -name ...`). `ecp` parses code, not text — for non-code text, grep is the correct tool.

---

## ⚡ Quick Reference (command × use-case)

### Symbol lookup
| Command | Use for |
|---|---|
| `ecp find <name>` | Exact symbol match (default) |
| `ecp find <n> --mode fuzzy` | Substring match for partial names |
| `ecp find <n> --mode bm25` | BM25-ranked, bucketed top-K |
| `ecp find <n> --kind function,method` | Filter by symbol kind |
| `ecp inspect --name <n>` | Full context: signature + body + edges + callers |

### Impact / blast radius

`ecp impact` has three **mutually exclusive** modes — pick by what you have:

**Symbol mode** (you know the symbol name):
| Command | Use for |
|---|---|
| `ecp impact <name>` | Upstream callers + risk_level (default depth 5, direction `up`) |
| `ecp impact <n> --direction down --depth N` | Custom traversal (`up` / `down` / `both`) |

**Baseline mode** (no symbol — derive from git diff):
| Command | Use for |
|---|---|
| `ecp impact --baseline origin/main` | All symbols changed between baseline and HEAD |

**Literal mode** (path-string sink lookup):
| Command | Use for |
|---|---|
| `ecp impact --literal session_meta.json` | Every read/write site of a path string, classified (`sink:read` / `sink:write` / `sink:join` / `sink:free` / …). For split-brain bugs (one part writes `meta.json`, another reads `session_meta.json`) |

**Related (edge-level, not symbol-level)**:
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
| `ecp processes` | List execution-flow Process nodes (Leiden community + BFS detection at index time) |
| `ecp processes trace <pat>` | Dump full Function / Method step sequence for a matching Process — cleaner than `impact --direction down` when you want the actual execution order |
| `ecp review` | Full audit (impact + summary + tool-map + shape-check + diff) |
| `ecp rename <old> <new>` | AST-aware multi-file rename |

### Cypher escape hatch
| Command | Use for |
|---|---|
| `ecp cypher "<query>"` | Ad-hoc `MATCH ... RETURN ...` when no command fits |

### Schema introspection (graph-loadless)
| Command | Output |
|---|---|
| `ecp schema blindspots` | Per-lang BlindSpot coverage; disambiguates "no dispatch in diff" vs "parser doesn't detect it" |
| `ecp schema reltypes` | All 19 RelType edges + LLM-utility category + heuristic flag |
| `ecp schema node-kinds` | All 28 NodeKind variants + same-name distinctions (Struct vs Class, Trait vs Interface) |
| `ecp schema graph-version` | rkyv `graph.bin` format version + bump history |

All `schema` commands default to `--format json` (agent-consumable); pass `--format text` for a human table.

---

## 🧭 Layer 2: Workflow Guides

Match your current task to a guide.

| Task | Open Guide |
|---|---|
| Deep dive into a single function, class, or variable context | [`guides/symbol-analysis.md`](./guides/symbol-analysis.md) |
| Assess risk of a PR or a planned modification (Blast Radius) | [`guides/pr-impact.md`](./guides/pr-impact.md) |
| Analyze cross-repo dependencies, HTTP contracts, and gRPC links | [`guides/multi-repo.md`](./guides/multi-repo.md) |
| "Not found" issues, index staleness, or resolver misses | [`guides/troubleshooting.md`](./guides/troubleshooting.md) |

---

## 📚 Layer 3: On-Demand References

These are detailed manuals for specific commands and concepts.

- `_shared/cli/` — Command-specific flag references (e.g., `inspect`, `impact`, `cypher`, `processes`).
- `_shared/refs/` — Conceptual background (e.g., Cypher syntax, Repo resolution).
