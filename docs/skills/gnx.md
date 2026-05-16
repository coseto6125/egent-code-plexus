---
name: gnx
description: Use for symbol-level code analysis, blast-radius impact, cross-repo API contracts, AST-aware rename, route map. Defer to grep for string literals, config keys, vendored / generated code, and fs layout.
---

# gnx — graph-nexus-rs CLI

`gnx <cmd> [--repo <path>]`. `--repo` is **not** auto-injected; most commands fall back to cwd, a few require it explicitly. Run `gnx <cmd> --help` if unsure.

## Tool selection

| Goal | Command |
|---|---|
| ONE symbol → signature + body + 1-hop edges + callers + 1-hop impact | `gnx inspect --name X --repo .` |
| ONE symbol → blast radius | `gnx impact X --direction upstream --repo .` (positional; `--target X` alias works too. `--direction` accepts `up`/`down`/`both` or `upstream`/`downstream`. Filters: `--kind --file_path --relation_types --depth --min-confidence --include-tests`) |
| PR blast radius — symbol view (who breaks) | `gnx impact --baseline origin/main --repo .` |
| Find symbol by name / concept | `gnx search "term" --repo .` (auto-picks bm25 / hybrid / vector; force via `--mode`) |
| Arbitrary graph query / source body via Cypher | `gnx cypher "MATCH (m:Method) WHERE m.name='X' RETURN m,m" --repo .` (positional; `--query "..."` alias works. Single-repo only. Minimal grammar — see Cypher subset below) |
| AST-aware multi-file rename | `gnx rename --symbol old --new-name new --dry-run --repo .` then drop `--dry-run`. **Never find-replace.** |
| HTTP route → handler → upstream callers | `gnx routes <path?> --repo .` (no path = list all) |
| Cross-repo API contracts (routes / queue / RPC) | `gnx contracts --repo @all` (needs ≥2 repos in group) |
| Verify references in a changed file resolve in the graph | `gnx scan <file> --repo .` |
| HTTP consumer → Route shape drift detection | `gnx shape_check --route <path>? --repo .` (no `--route` = scan all routes; drift = consumer reads key not in Route's response/error keys) |
| Binding tier / route / contract delta — edge view | `gnx diff --section <bindings\|routes\|contracts\|all> --baseline <ref> --repo .` (`--baseline` required; accepts branch / tag / SHA / `HEAD~N` / `PR/<n>`. Multi-select via `,`. Formats: text / json / toon. Use `--verbose` for full lists.) |
| Registry health / freshness / frameworks / blind spots | `gnx coverage` (registry-wide) or `gnx coverage --repo @all --detailed` |
| String literals / config keys / vendored / generated / fs layout | grep / glob |
| MCP host integration / install hooks / config TUI / **MCP server (`mcp serve\|tools`)** / **resolver vs LSP oracle benchmark (`verify-resolver`)** | `gnx admin` (hidden namespace) |

## Repo + graph path resolution

Two access paths; pick one per command:

- **`--repo <abs-or-rel-path>`** → registry lookup → reads `~/.gnx/graph-nexus-<hash>/<branch-slug>/graph.bin`. Branch slug = current HEAD with `/` → `__`. **Preferred** day-to-day.
- **`--graph <abs-path-to-graph.bin>`** → bypass registry. Use when registry slug mismatch or testing a snapshot.
- **`--repo @<group> / @all / csv` (`name1,name2`)** → multi-repo. Works for `search / impact / contracts / coverage`. `cypher / inspect` are single-repo (will error on multi).

### Indexing is automatic

Agent commands auto-detect stale/missing graphs and rebuild on demand,
emitting one stderr line `✓ Index refreshed (... in Xs)` and continuing.
No need to `gnx admin index` before querying — first query on a fresh
checkout pays the index cost once (~30s–2min depending on tree size).

`gnx admin index --repo <path>` is still available as an explicit form
for human-driven workflows (full re-index, `--embeddings`, `--force`).

### "Not found" but `grep` shows the symbol

Almost always stale — auto-ensure should have rebuilt. If it didn't, the
symbol genuinely isn't in the graph: check for typos, try `gnx search`
for fuzzy matches, or re-run the same command (auto-ensure walks the
tree on each call and re-indexes if mtime moved).

## Output formats

`--format` defaults vary by command:

| Command | Default | Other |
|---|---|---|
| `inspect / coverage / contracts / routes` | toon | json |
| `cypher` | json | toon, text |
| `search / scan / rename / impact` | text | json, toon |

Rule of thumb: **toon** for agent → agent piping (compact key:value), **json** for parsing in scripts, **text** for human inspection.

## Cypher subset

```
MATCH (a:Kind)-[r:Rel]->(b:Kind) [WHERE ...] RETURN ...
```

Supports the openCypher read subset commonly used for graph queries: boolean WHERE (`AND / OR / NOT`), comparisons (`= != < <= > >=`), string ops (`STARTS WITH / ENDS WITH / CONTAINS / =~ / IN [...]`), aggregations (`COUNT(*)`, etc.), `DISTINCT`, `ORDER BY / SKIP / LIMIT`, `WITH`, `UNION`, variable-length paths (`[:Rel*1..2]`), and reverse arrows (`<-[r:Rel]-`). Convention: **keep queries minimal** — for richer needs use `gnx search` / `gnx inspect` / post-process JSON.

**NodeKind** (case-sensitive labels): `Function / Method / Class / Property / Constructor / Interface / Const / Variable / Import / Route / Process / Document / Section / EntryPoint / File`.

**RelType** (CamelCase only — `HAS_METHOD` fails with `unknown RelType` semantic error, use `HasMethod`): `Calls / Extends / Imports / Implements / HasMethod / HasProperty / Accesses / HandlesRoute / StepInProcess / References / Defines / Fetches`.

**Node properties** (in `WHERE` / `RETURN`): `a.name / a.uid / a.kind / a.filePath / a.content`. **Edge properties**: `r.rel_type / r.confidence / r.reason`.

**`HasMethod` target kind is parser-determined**: Python `def` and Rust associated fn surface as `Function`, true methods as `Method`. Use `MATCH (c:Class)-[:HasMethod]->(m) RETURN m` — **don't add `:Method` filter** or you'll miss those languages. `gnx inspect <Class>.contained_methods` keeps each entry's `kind` field if callers need to distinguish.

## Common pitfalls

1. **`--repo` is required for cross-repo modes**. `@group / @all / csv` only work when explicit.
2. **`cypher --repo @group` errors** — single-repo only.
3. **Default `--graph .gitnexus-rs/graph.bin`** is a cwd-relative legacy path. If you don't have a checked-in graph file, pass `--repo` (preferred) or absolute `--graph`.
4. **Auto-ensure on every agent command** — first query after a source change pays a brief re-index cost. The stderr `✓ Index refreshed` line is informational, not an error.
5. **`scan --strict`** flags identifiers that match language keywords / builtins. Off by default; turn on for high-noise files.
6. **`rename --markdown`** is OFF by default — code-only rename. Add the flag to sweep `.md / .rst / .txt`.

## PR-touching workflow

```bash
# Before editing a function: see blast radius
gnx impact Foo --direction upstream --repo .

# After staging a diff: see what changed + downstream/upstream callers
gnx impact --baseline origin/main --repo .

# After edits: verify changed files' references still resolve
gnx scan crates/.../changed_file.rs --repo .

# Touched HTTP routing / handlers?
gnx routes /api/foo --repo .
```

HIGH / CRITICAL risk_level in impact output → **stop + confirm with user** before pushing. Cross-repo contract changes → check `gnx contracts --repo @all --unmatched-only` for orphaned consumers.

## Group / multi-repo

- Membership: `gnx admin group add <name> --repo <path>` / `gnx admin group list`.
- Query across group: `--repo @<group-name>` on supported commands.
- `--repo @all` = all registered repos.
- gnx-rs has no standalone `group_status / group_query / group_impact` commands — use `--repo @group` on the relevant agent command.
