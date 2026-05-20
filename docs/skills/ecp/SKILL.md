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

- `_shared/cli/` — Command-specific flag references (e.g., `inspect`, `impact`, `cypher`).
- `_shared/refs/` — Conceptual background (e.g., Cypher syntax, Repo resolution).
