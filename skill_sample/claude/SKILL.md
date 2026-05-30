---
name: ecp
description: Symbol-level code analysis, blast-radius impact, cross-repo API contracts, AST-aware rename, route map. Defer to grep for string literals, config keys, vendored/generated code, fs layout.
---

# ecp — egent-code-plexus-rs CLI

`ecp <cmd> [--repo <path>]`. `--repo` is **not** auto-injected: most commands fall back to cwd, a few require it. `--help` if unsure.

## Tool selection

| Goal | Command |
|---|---|
| ONE symbol → signature + body + 1-hop edges + callers + impact | `ecp inspect --name X --repo .` |
| ONE symbol → blast radius | `ecp impact X --direction upstream --repo .` (positional; `--target X` alias. `--direction`: `up`/`down`/`both`. Filters: `--kind --file_path --relation_types --depth --min-confidence --include-tests`) |
| PR blast radius — who breaks | `ecp impact --baseline origin/main --repo .` |
| Find symbol by name / concept | `ecp find "term" --repo .` (auto bm25/hybrid/vector; force `--mode`) |
| Schema mirrors (cross-service field alignment) | `ecp find-schema-bindings User.email --repo .` (bare = all classes). `--format json`. |
| Saga compensate/undo/rollback pairs | `ecp find-transaction-patterns [--class OrderService] --repo .` (bare = all classes). JSON; `POSSIBLY_RELATED` (≥0.75) or `BLIND_SPOT` (<0.75). Outbox half deferred (T5-33). |
| Arbitrary graph query / source body | `ecp cypher "MATCH (m:Method) WHERE m.name='X' RETURN m,m" --repo .` (positional; `--query` alias. Single-repo. Grammar below) |
| AST-aware multi-file rename | `ecp rename --symbol old --new-name new --dry-run --repo .` then drop `--dry-run`. **Never find-replace.** |
| HTTP route → handler → upstream callers | `ecp routes <path?> --repo .` (no path = list all) |
| Cross-repo API contracts (routes / queue / RPC) | `ecp contracts --repo @all` (needs ≥2 repos in group) |
| HTTP consumer → Route shape drift | `ecp shape-check [--route <path>] --repo .` (no `--route` = scan all; drift = consumer reads key absent from Route's response/error keys) |
| Binding / route / contract delta — edge view | `ecp diff --section <bindings\|routes\|contracts\|all> --baseline <ref> --repo .` (`--baseline` required: branch / tag / SHA / `HEAD~N` / `PR/<n>`; multi via `,`; `--verbose` full lists) |
| Registry health / freshness / frameworks / blind spots | `ecp summary [--repo @all --detailed]` (was `ecp coverage`, aliased one release) |
| String literals / config keys / vendored / generated / fs layout | grep / glob |
| MCP host integration / install hooks / config TUI / **MCP server (`mcp serve\|tools`)** / **resolver-vs-LSP benchmark (`verify-resolver`)** | `ecp admin` (hidden namespace) |

## Repo + graph path resolution

One access path per command:

- **`--repo <path>`** (**preferred**) → registry → `~/.ecp/egent-code-plexus-<hash>/<branch-slug>/graph.bin` (slug = HEAD, `/` → `__`).
- **`--graph <abs-path>`** → bypass registry; on slug mismatch or snapshot test.
- **`--repo @<group> / @all / csv`** → multi-repo for `find / impact / contracts / coverage`; `cypher / inspect` single-repo (error on multi).

Indexing is automatic: agent commands auto-rebuild stale/missing graphs (`✓ Index refreshed (... in Xs)` on stderr); a fresh checkout pays once (~30s–2min). Explicit: `ecp admin index --repo <path>` (`--embeddings`, `--force`).

**"Not found" but `grep` shows the symbol**: usually stale; re-run (auto-ensure re-indexes on mtime change). Still absent → check typos or `ecp find`.

## Output formats

`--format` defaults: **toon** (`inspect / coverage / contracts / routes`); **json** (`cypher`); **text** (`find / rename / impact`). toon = agent→agent, json = scripts, text = humans.

## Cypher subset

```
MATCH (a:Kind)-[r:Rel]->(b:Kind) [WHERE ...] RETURN ...
```

openCypher read subset: `AND / OR / NOT`, `= != < <= > >=`, `STARTS WITH / ENDS WITH / CONTAINS / =~ / IN [...]`, `COUNT(*)`, `DISTINCT`, `ORDER BY / SKIP / LIMIT`, `WITH`, `UNION`, var-length `[:Rel*1..2]`, reverse `<-[r:Rel]-`. Keep minimal — richer → `ecp find` / `ecp inspect` / post-process JSON.

**NodeKind** (case-sensitive): `Function / Method / Class / Property / Constructor / Interface / Const / Variable / Import / Route / Process / Document / Section / EntryPoint / File`.

**RelType** (CamelCase only — `HAS_METHOD` → `unknown RelType`): `Calls / Extends / Imports / Implements / HasMethod / HasProperty / Accesses / HandlesRoute / StepInProcess / References / Defines / Fetches`.

**Props**: nodes `a.name / a.uid / a.kind / a.filePath / a.content`; edges `r.rel_type / r.confidence / r.reason`.

**`HasMethod` target kind is parser-determined** — Python `def` / Rust associated fn surface as `Function`, true methods as `Method`. Use `MATCH (c:Class)-[:HasMethod]->(m) RETURN m`; **no `:Method` filter** or you'll miss those languages.

## Common pitfalls

1. **`--repo` required for cross-repo modes** — `@group / @all / csv` only work when explicit.
2. **`cypher --repo @group` errors** — single-repo only.
3. **Default `--graph .ecp-rs/graph.bin`** is a cwd-relative legacy path. Without a checked-in graph, pass `--repo` or absolute `--graph`.
4. **Auto-ensure runs every agent command** — first query after a source change pays a brief re-index; `✓ Index refreshed` is informational, not an error.
5. **`rename --markdown`** is OFF by default (code-only). Add it to sweep `.md / .rst / .txt`.

## PR-touching workflow

```bash
# Before editing a function — blast radius
ecp impact Foo --direction upstream --repo .
# After staging a diff — what changed + callers
ecp impact --baseline origin/main --repo .
# After edits — binding / route / contract delta
ecp diff --section bindings --baseline origin/main --repo .
# Touched HTTP routing / handlers
ecp routes /api/foo --repo .
```

HIGH / CRITICAL risk_level → **stop + confirm with user** before pushing. Cross-repo contract changes → `ecp contracts --repo @all --unmatched-only` for orphaned consumers.

## Group / multi-repo

- Membership: `ecp admin group add <name> --repo <path>` / `ecp admin group list`.
- Query: `--repo @<group-name>` (`@all` = all registered repos) on supported commands.
- No standalone `group_status / group_query / group_impact` — use `--repo @group`.
