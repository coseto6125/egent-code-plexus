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
| Diff blast radius (pre-commit / pre-edit) | `gnx impact --baseline origin/main --repo .` |
| Find symbol by name / concept | `gnx search "term" --repo .` (auto-picks bm25 / hybrid / vector; force via `--mode`) |
| Arbitrary graph query / source body via Cypher | `gnx cypher "MATCH (m:Method) WHERE m.name='X' RETURN m,m" --repo .` (positional; `--query "..."` alias works. Single-repo only. Minimal grammar — see Cypher subset below) |
| AST-aware multi-file rename | `gnx rename --symbol old --new-name new --dry-run --repo .` then drop `--dry-run`. **Never find-replace.** |
| HTTP route → handler → upstream callers | `gnx routes <path?> --repo .` (no path = list all) |
| Cross-repo API contracts (routes / queue / RPC) | `gnx contracts --repo @all` (needs ≥2 repos in group) |
| Verify references in a changed file resolve in the graph | `gnx scan <file> --repo .` |
| Registry health / freshness / frameworks / blind spots | `gnx coverage` (registry-wide) or `gnx coverage --repo @all --detailed` |
| String literals / config keys / vendored / generated / fs layout | grep / glob |
| MCP host integration / install hooks / config TUI | `gnx admin` (hidden namespace) |

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

## Cypher minimal grammar

The cypher subset only accepts:

```
MATCH (a:Kind)-[r:Rel]->(b:Kind) [WHERE a.name='Val'] RETURN a,b
```

- `Kind` is required on both sides: `Function / Method / Class / Property / Const / Variable / Route / File / Process`.
- `Rel` types: `CALLS / IMPORTS / EXTENDS / HAS_METHOD / HANDLES_ROUTE / FETCHES / METHOD_OVERRIDES / ACCESSES / MEMBER_OF / CONTAINS / DEFINES`.
- `WHERE` accepts equality on `a.name` or `b.name` only — no `STARTS WITH`, `count(*)`, multi-clause, aggregations.
- `m.content` returns the symbol body (the only non-name field exposed).
- For richer queries: `gnx search` (BM25 / embedding), `gnx inspect` (full edge view), or post-process JSON output downstream.

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
