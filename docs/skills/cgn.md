---
name: cgn
description: Use for symbol-level code analysis, blast-radius impact, cross-repo API contracts, AST-aware rename, route map. Defer to grep for string literals, config keys, vendored / generated code, and fs layout.
---

# cgn — code-graph-nexus-rs CLI

`cgn <cmd> [--repo <path>]`. `--repo` is **not** auto-injected; most commands fall back to cwd, a few require it explicitly. Run `cgn <cmd> --help` if unsure.

## Tool selection

| Goal | Command |
|---|---|
| ONE symbol → signature + body + 1-hop edges + callers + 1-hop impact | `cgn inspect --name X --repo .` |
| ONE symbol → blast radius | `cgn impact X --direction upstream --repo .` (positional; `--target X` alias works too. `--direction` accepts `up`/`down`/`both` or `upstream`/`downstream`. Filters: `--kind --file_path --relation_types --depth --min-confidence --include-tests`) |
| PR blast radius — symbol view (who breaks) | `cgn impact --baseline origin/main --repo .` |
| Find symbol by exact name | `cgn find "name" --repo .` (default mode = exact match; returns single top-ranked definition by category priority + caller count. `--all` for every exact match, `--include-tests` to include test files.) |
| Find symbol by name fragment / ranked search | `cgn find "fragment" --mode bm25 --repo .` (BM25 via tantivy; substring fallback when index absent. Output partitioned into 5 buckets: `source` / `tests` / `reference` / `document` / `config`. Read `.source` for production-code hits.) `--mode fuzzy` is a lighter alternative that shares the exact-mode flat output shape. |
| Arbitrary graph query / source body via Cypher | `cgn cypher "MATCH (m:Method) WHERE m.name='X' RETURN m,m" --repo .` (positional; `--query "..."` alias works. Single-repo only. Minimal grammar — see Cypher subset below) |
| AST-aware multi-file rename | `cgn rename --symbol old --new-name new --dry-run --repo .` then drop `--dry-run`. **Never find-replace.** |
| HTTP route → handler → upstream callers | `cgn routes <path?> --repo .` (no path = list all) |
| Cross-repo API contracts (routes / queue / RPC) | `cgn group contracts <name>` for a named group; `cgn contracts --repo @all` for every registered repo |
| HTTP consumer → Route shape drift detection | `cgn shape_check --route <path>? --repo .` (no `--route` = scan all routes; drift = consumer reads key not in Route's response/error keys) |
| Binding tier / route / contract delta — edge view | `cgn diff --section <bindings\|routes\|contracts\|all> --baseline <ref> --repo .` (`--baseline` required; accepts branch / tag / SHA / `HEAD~N` / `PR/<n>`. Multi-select via `,`. Formats: text / json / toon. Use `--verbose` for full lists.) |
| Registry health / freshness / frameworks / blind spots | `cgn coverage` (registry-wide) or `cgn coverage --repo @all --detailed` |
| String literals / config keys / vendored / generated / fs layout | grep / glob |
| MCP host integration / install hooks / config TUI / **MCP server (`mcp serve\|tools`)** / **resolver vs LSP oracle benchmark (`verify-resolver`)** | `cgn admin` (hidden namespace) |

## Repo + graph path resolution

Two access paths; pick one per command:

- **`--repo <abs-or-rel-path>`** → registry lookup → reads `~/.cgn/code-graph-nexus-<hash>/<branch-slug>/graph.bin`. Branch slug = current HEAD with `/` → `__`. **Preferred** day-to-day.
- **`--graph <abs-path-to-graph.bin>`** → bypass registry. Use when registry slug mismatch or testing a snapshot.
- **`--repo @all / csv` (`name1,name2`)** → multi-repo. Works for `find --mode bm25 / contracts / coverage`. `cypher / inspect` are single-repo (will error on multi).
- **`--repo @<group>` on top-level commands is rejected** with a hint pointing at `cgn group <verb>` — group queries are noun-first under the `cgn group` namespace (see §Multi-repo workflow).

### Indexing is automatic

Agent commands auto-detect stale/missing graphs and rebuild on demand,
emitting one stderr line `✓ Index refreshed (... in Xs)` and continuing.
No need to `cgn admin index` before querying — first query on a fresh
checkout pays the index cost once (~30s–2min depending on tree size).

`cgn admin index --repo <path>` is still available as an explicit form
for human-driven workflows (full re-index, `--force`).

### "Not found" but `grep` shows the symbol

Almost always stale — auto-ensure should have rebuilt. If it didn't, the
symbol genuinely isn't in the graph: check for typos, try `cgn find
<fragment> --mode fuzzy` (or `--mode bm25` for ranked variants), or
re-run the same command (auto-ensure walks the tree on each call and
re-indexes if mtime moved).

## Output formats

`--format` defaults vary by command:

| Command | Default | Other |
|---|---|---|
| `inspect / coverage / contracts / routes` | toon | json |
| `cypher` | json | toon, text |
| `find / rename / impact` | text | json, toon |

Rule of thumb: **toon** for agent → agent piping (compact key:value), **json** for parsing in scripts, **text** for human inspection.

## Cypher subset

```
MATCH (a:Kind)-[r:Rel]->(b:Kind) [WHERE ...] RETURN ...
```

Supports the openCypher read subset commonly used for graph queries: boolean WHERE (`AND / OR / NOT`), comparisons (`= != < <= > >=`), string ops (`STARTS WITH / ENDS WITH / CONTAINS / =~ / IN [...]`), aggregations (`COUNT(*)`, etc.), `DISTINCT`, `ORDER BY / SKIP / LIMIT`, `WITH`, `UNION`, variable-length paths (`[:Rel*1..2]`), and reverse arrows (`<-[r:Rel]-`). Convention: **keep queries minimal** — for richer needs use `cgn find` / `cgn inspect` / post-process JSON.

**NodeKind** (case-sensitive labels): `Function / Method / Class / Property / Constructor / Interface / Const / Variable / Import / Route / Process / Document / Section / EntryPoint / File`.

**RelType** (CamelCase only — `HAS_METHOD` fails with `unknown RelType` semantic error, use `HasMethod`): `Calls / Extends / Imports / Implements / HasMethod / HasProperty / Accesses / HandlesRoute / StepInProcess / References / Defines / Fetches`.

**Node properties** (in `WHERE` / `RETURN`): `a.name / a.uid / a.kind / a.filePath / a.content`. **Edge properties**: `r.rel_type / r.confidence / r.reason`.

**`HasMethod` target kind is parser-determined**: Python `def` and Rust associated fn surface as `Function`, true methods as `Method`. Use `MATCH (c:Class)-[:HasMethod]->(m) RETURN m` — **don't add `:Method` filter** or you'll miss those languages. `cgn inspect <Class>.contained_methods` keeps each entry's `kind` field if callers need to distinguish.

**`Imports` source is always `NodeKind::File`**. Target is the imported symbol when the import names one (TS/JS/Python/Java/PHP/Rust named imports → Function/Method/Class), or `NodeKind::File` for module-style imports (Ruby `require_relative`, C/C++ `#include`, Go `import "pkg"`, C# `using NS;`, Dart relative `import '*.dart'`, Rust `use crate::*`). Use `MATCH (f:File {name:'b.ts'})-[:Imports]->(t) RETURN t.name, t.kind` to find what a file imports. `r.reason` distinguishes `post_process:imports` (named) from `post_process:imports:module` (file-level fallback). External dependencies (Foundation, `package:flutter/...`, `std::io`, `jakarta.*`) **don't emit edges** — cgn refuses to fabricate edges to targets outside the indexed corpus, by design (avoid gitnexus-style `.mjs → Path.java` cross-language false positives).

## Common pitfalls

1. **`--repo` is required for cross-repo modes**. `@group / @all / csv` only work when explicit.
2. **Top-level commands reject `--repo @<group>`** — the error points at the matching `cgn group <verb>`. `cypher / inspect / rename` have no group analog; group queries live under `cgn group <verb>` only.
3. **Default `--graph .cgn/graph.bin`** is a cwd-relative legacy path. If you don't have a checked-in graph file, pass `--repo` (preferred) or absolute `--graph`.
4. **Auto-ensure on every agent command** — first query after a source change pays a brief re-index cost. The stderr `✓ Index refreshed` line is informational, not an error.
5. **`rename --markdown`** is OFF by default — code-only rename. Add the flag to sweep `.md / .rst / .txt`.

## PR-touching workflow

```bash
# Before editing a function: see blast radius
cgn impact Foo --direction upstream --repo .

# After staging a diff: see what changed + downstream/upstream callers
cgn impact --baseline origin/main --repo .

# Touched HTTP routing / handlers?
cgn routes /api/foo --repo .
```

HIGH / CRITICAL risk_level in impact output → **stop + confirm with user** before pushing. Cross-repo contract changes → check `cgn group contracts <name> --unmatched` for orphaned consumers in a known group, or `cgn contracts --repo @all --unmatched-only` registry-wide.

## Multi-repo workflow

Cross-repo queries live under `cgn group <verb>`. Management commands
(`add/remove/list`) stay under `cgn admin group`.

| Command | Purpose | Output (default) |
|---|---|---|
| `cgn group sync <name>` | Extract HTTP/gRPC contracts, build exact + BM25 cross-links, write `~/.cgn/groups/<name>/{contracts.rkyv, meta.json}` | TOON summary |
| `cgn group status <name>` | Per-member staleness (OK / STALE / MISSING / NO_META / NO_SNAPSHOT) via `git rev-parse` diff vs stored snapshot | Text/TOON |
| `cgn group contracts <name> [--type T] [--repo R] [--unmatched]` | Inspect contract registry with filters | Text/JSON |
| `cgn group impact <name> --target <sym> --repo <member>` | Local impact + cross-repo fan-out (cross_depth clamped to 1 first wave) | TOON/JSON |
| `cgn group find <name> <pattern> [--merge none\|rrf] [--limit N] [--batch]` | Default per-repo bucketed concat (`--merge none`). `--merge rrf --limit N` → unified top-K via RRF. `--batch` reads patterns from stdin (one per line, `#` for comments) and re-applies the merge mode per pattern. | Text/JSON |
| `cgn group coverage <name>` | Per-member health concat | Text/JSON |

**Selector layer**: `--repo @<group>` on top-level commands (`cgn search/find/contracts/coverage`) returns an error pointing at `cgn group <verb>` — the noun-first surface is canonical. `--repo @all` and single-repo selectors are unchanged.

First-wave extractors: Go / Python / Node / Java / Rust × HTTP + gRPC. Other 9 mainstream languages emit nothing (BlindSpot stubs).

Config knobs live in `~/.cgn/config.toml` under `[group]` (`bm25_threshold`, `max_candidates_per_step`, `exclude_links_paths`, `exclude_links_param_only_paths`, `cross_depth`, `local_impact_timeout_ms`).

See [spec](../specs/2026-05-18-cgn-group-multirepo-design.md) for the full design.
