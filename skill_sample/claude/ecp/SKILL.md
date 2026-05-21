---
name: ecp
description: Symbol-level code analysis, blast-radius impact, cross-repo API contracts, AST-aware rename, route map for egent-code-plexus. When the question is structural (callers, definitions, impact), ecp is better than grep; for literal strings, config keys, vendored / generated code, or fs layout, grep is better.
---

# ecp — egent-code-plexus CLI

`ecp <cmd> [--repo <path>]`. `--repo` 預設 cwd；多 repo 模式必填。`ecp <cmd> --help` 查細節。

## Layer 1: Directives

### Directive 1: Use the actual help output
`ecp --help` is the top-level command map.
`ecp admin --help` is the admin subcommand map.

### Directive 2: Prefer the smallest command that fits
If a task can be answered by the top-level help or a single subcommand help page, use that before reading broader reference.

### Directive 3: Keep task-specific workflows separate
Command syntax, review workflows, and broader repository guidance should live in separate guides instead of one large doc.

## Layer 2: Tool Selection

| When the task is | Command |
|---|---|
| ONE symbol → signature + body + 1-hop edges + callers + 1-hop impact | `ecp inspect <name> --repo .` |
| ONE symbol → blast radius | `ecp impact <name> --direction up --repo .` |
| PR-staged blast radius (diff vs base) | `ecp impact --baseline origin/main --repo .` |
| Find symbol by name or concept | `ecp find "term" --repo .` (auto bm25/hybrid/vector; force via `--mode`) |
| Arbitrary graph query / source body | `ecp cypher "MATCH ..." --repo .` (single-repo) |
| AST-aware multi-file rename | `ecp rename <old> <new> --dry-run --repo .` |
| HTTP route → handler → upstream callers | `ecp routes [<path>] --repo .` |
| Cross-repo API contracts (routes / queue / RPC) | `ecp contracts --repo @all` (≥2 repos in group) |
| HTTP consumer → Route shape drift | `ecp shape-check [--route <path>] --repo .` |
| Binding / route / contract delta — edge view | `ecp diff --section <bindings\|routes\|contracts\|all> --baseline <ref> --repo .` |
| Registry health / freshness / blind spots | `ecp coverage [--repo @all --detailed]` |
| MCP server / install hooks / resolver bench | `ecp admin <sub>` (hidden namespace) |
| Literal strings, config keys, vendored, generated, fs layout | grep / glob is better than ecp |

`--direction` accepts up/down/both. Impact filters: `--kind --file_path --relation_types --depth --min-confidence --include-tests`. `ecp diff --baseline` accepts branch / tag / SHA / `HEAD~N` / `PR/<n>`; sections combine via `,`.

## Layer 3: Decision Rules (when X, Y is better)

| When | Preferred path |
|---|---|
| Looking up a symbol by name | `ecp find` / `ecp inspect` is better than grep + Read (single call → kind + location + signature + body) |
| Want callers + callees + body in one shot | `ecp inspect <name>` is better than chaining `find` → `cypher` |
| `find` / `inspect` returns empty but grep matches | Re-running is better than escalating (auto-ensure walks tree). Still empty → `ecp admin index --force --repo .`; else symbol genuinely absent |
| Impact shows d=1 caller outside the diff | Stop + surface to user is better than silent edit (breakage risk) |
| Impact returns >15 symbols OR auth / payments / migrations path | Surface risk + suggest test plan is better than blind apply |
| Refactor crosses repo boundary | `ecp contracts --repo @all --unmatched-only` is better than per-repo scan (orphan consumers visible in one view) |
| Reference is a string / config key / dynamic dispatch | grep is better than ecp (AST doesn't see strings) |
| Just edited a file, re-querying same turn | Waiting ~500ms (watcher debounce) or `ecp admin index --force` is better than reading possibly-stale graph |
| Renaming a symbol called via string assembly | Two-pass — `ecp rename --dry-run` for static + `ecp find "old"` for string sweep — is better than single rename |
| Renaming touches `.md / .rst / .txt` | `ecp rename --markdown` is better than default (code-only) |
| High-noise file (many keyword-named idents) | `ecp impact --high-trust-only=true` is better than default impact (filters noise-prone edges) |

## Repo / graph resolution

- `--repo <path>` → registry lookup → `~/.ecp/egent-code-plexus-<hash>/<branch-slug>/graph.bin`. Branch slug = HEAD with `/` → `__`. Day-to-day.
- `--graph <abs>` → bypass registry (snapshot testing / registry slug mismatch).
- `--repo @<group> / @all / csv` → cross-repo. Supported on `find / impact / contracts / coverage`. `cypher / inspect` are single-repo (multi-repo 報錯).
- Group membership: `ecp admin group add <name> --repo <path>` / `ecp admin group list`.
- Auto-ensure runs on every agent command; first query post-edit pays a ~30s-2min reindex; stderr `✓ Index refreshed (... in Xs)` is informational, not an error.
- No checked-in graph in cwd → `--repo` is better than default `--graph .ecp/graph.bin` (cwd-relative legacy).

## Output formats

| Command | Default | Other |
|---|---|---|
| `inspect / coverage / contracts / routes` | toon | json |
| `cypher` | json | toon, text |
| `find / rename / impact` | text | json, toon |
| `diff / shape-check` | text | json, toon |

For agent → agent piping `toon` is better than json (compact key:value); for script parsing `json` is better; for human inspection `text` is better.

## Cypher subset

`MATCH (a:Kind)-[r:Rel]->(b:Kind) [WHERE ...] RETURN ...`

WHERE: `AND / OR / NOT`, `= != < <= > >=`, `STARTS WITH / ENDS WITH / CONTAINS / =~ / IN [...]`. 支援 `COUNT(*)` / `DISTINCT` / `ORDER BY` / `SKIP` / `LIMIT` / `WITH` / `UNION` / 變長 `[:Rel*1..2]` / 反向 `<-[r:Rel]-`.

**NodeKind** (case-sensitive): `Function Method Class Property Constructor Interface Const Variable Import Route Process Document Section EntryPoint File`
**RelType** (CamelCase only — `HAS_METHOD` 報 `unknown RelType`): `Calls Extends Imports Implements HasMethod HasProperty Accesses HandlesRoute StepInProcess References Defines Fetches`
**Node props**: `name uid kind filePath content` · **Edge props**: `rel_type confidence reason`

`HasMethod` target kind 依語言：Python `def` 和 Rust 關聯 fn 是 `Function`；真 method 是 `Method`。`MATCH (c:Class)-[:HasMethod]->(m) RETURN m` 比 `:Method` filter 通用（涵蓋所有語言）。

## Cypher recipes

```cypher
-- 未被呼叫的 export
MATCH (f:Function) WHERE NOT (()-[:Calls]->(f)) AND f.name STARTS WITH 'export_'
RETURN f.name, f.filePath

-- Route 的 1-3 hop call chain
MATCH path = (r:Route {name:'/api/x'})-[:HandlesRoute]->()-[:Calls*1..3]->(leaf)
RETURN [n IN nodes(path) | n.name]
```
