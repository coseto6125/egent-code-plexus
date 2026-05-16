# gnx CLI Redesign — Agent-First Surface

**Date**: 2026-05-15
**Status**: Spec / awaiting implementation plan
**Author**: brainstormed with Claude Code (LLM agent perspective)

---

## 1. Motivation

The current `gnx` CLI exposes 27 top-level subcommands, mixing query/edit/refactor commands (for LLM agents) with registry/setup/destructive operations (for humans). For an LLM consuming `gnx --help`, this surface is noisy: 17 commands the agent rarely or never invokes, ambiguous verbs (`clean` / `remove`), and per-command flags that vary between commands. Empirically, the agent's first read of `--help` does not narrow its toolchain to "what to use right now".

**Goal**: rebuild the agent-facing surface to be the *minimum* set of commands an LLM needs to query, navigate, refactor, and verify code — with operations composed server-side so the agent never has to chain calls.

**Non-goals**:
- Backward compatibility with the current top-level paths (v0.1.0 is pre-release; hard break is acceptable).
- Re-implementing functionality beyond the CLI surface (graph engine, embeddings, registry storage stay as-is unless explicitly noted in §8).

---

## 2. Design Principles

| Principle | Concrete consequence |
|---|---|
| **P1. Agent never chains** | Each command returns everything the agent needs in one call; ambiguity returns rich matches, not opaque IDs. |
| **P2. Agent never manages plumbing** | No `--format`, no manual index lifecycle, no UID disambiguation in the agent CLI. Auto-ensure, auto-format. |
| **P3. Self-documenting verbs** | Every command name describes a single intent. Banned: `clean`, `analyze`, generic `init`/`list`. |
| **P4. Defaults are safe** | `rename` defaults to code-only (no comment/markdown changes); `impact` defaults to `--high-trust-only`. |
| **P5. No silent empties** | Empty result, error, or stale state always returns an explicit message + recovery hint. Token-disciplined: max 1–2 lines per hint. |
| **P6. Cross-repo is first-class** | `--repo` selector is global, supports path / name / `@group` / `@all` / ad-hoc CSV. No `multi_*` parallel commands. |
| **P7. Admin invisible from agent** | `gnx admin <cmd>` exists for human operators but is `hide = true` at the top level — `gnx --help` does not mention it. |

---

## 3. CLI Surface — Agent Side

`gnx --help` lists exactly **9 commands**:

```
GitNexus query engine for code intelligence.

Usage: gnx [OPTIONS] <COMMAND>

Commands:
  inspect    Show symbol's full context: signature, body, edges, callers,
             overrides, and 1-hop upstream impact summary
  search     Find symbols by name or concept; returns top matches with
             full inspect-style info per match
  impact     Blast radius — from <name> or git diff via --since <ref>
  rename     AST-aware multi-file rename
  cypher     Cypher query escape hatch
  coverage   Registry + repo health (indexed repos, freshness, frameworks,
             blind spots, contracts summary)
  routes     List HTTP routes; with path, show handler + full caller chain
  scan       Verify a file's symbol references exist; suggests near-matches
             for missing
  contracts  Cross-repo API contracts inventory (producer ↔ consumer matched)

Options:
      --repo <SELECTOR>   path | name | @group | @all | csv mix [default: cwd]
  -h, --help              Print help
  -V, --version           Print version
```

### 3.1 Global flag

| Flag | Type | Default | Notes |
|---|---|---|---|
| `--repo <selector>` | string | cwd | Selector grammar in §6 |

There is **no agent-visible `--format`** flag. Output format is decided server-side (§7).
There is **no agent-visible `--graph`** flag. Path resolution is internal.

### 3.2 Per-command spec

#### 3.2.1 `gnx inspect <name>`

Show symbol's full context.

| Arg / Flag | Type | Default | Purpose |
|---|---|---|---|
| `<name>` positional | string | — | Symbol name (case-sensitive) |
| `--file <substr>` | string | — | Disambiguate when name has multiple matches |
| `--kind <type>` | enum | — | Disambiguate by kind (`function` / `method` / `class` / `route` / etc.) |
| `--depth <N>` | int | 1 | Caller / override chain depth |
| `--include-tests` | bool | false | Include test files in edges |
| `--relation-types <list>` | csv | all | Limit to relation types (`calls`, `extends`, …) |

**Output**: For each match — kind, file, signature, body, incoming edges (callers), outgoing edges (callees), method overrides (if applicable), 1-hop upstream impact summary, freshness flag.

**Ambiguity handling (P1)**: If multiple matches and no `--file` / `--kind` narrows them, return *all* matches with full inspect blocks per match — not a candidate list with UIDs.

**Empty / error (P5)**:
- Symbol not found → `No symbol "X". Did you mean: <fuzzy 3>? Or: gnx search X --mode bm25`
- Repo not indexed → auto-ensure (§5), then retry; if index fails, surface reason + recovery.

**Multi-repo**: yes — returns matches per repo with repo label.

#### 3.2.2 `gnx search <pattern>`

Find symbols by name or concept.

| Arg / Flag | Type | Default | Purpose |
|---|---|---|---|
| `<pattern>` positional | string | — | Name fragment or natural-language description |
| `--mode <mode>` | enum | `auto` | `bm25` / `vector` / `hybrid` / `auto` |
| `--kind <list>` | csv | all | Filter by node kinds |

**Mode auto-detection**:
- Input matches `^[A-Za-z0-9_]+$` (slug-like) → `bm25`
- Input contains whitespace or punctuation → `vector` if embeddings available, else fall back to `bm25` (stderr hint)
- Mixed signals → `hybrid`

**Output**: Top matches (default K=20, not user-configurable). **Each match includes inspect-style info** — name, kind, file, signature, 1-hop callers — so the agent does not need a follow-up `inspect` call (P1).

**Empty (P5)**: `0 matches for "X". Try --mode bm25 (name) / --mode vector (concept) / --kind any`

**Multi-repo**: yes — replaces the old `multi_query` command entirely.

#### 3.2.3 `gnx impact <name | --since <ref>>`

Blast radius analysis.

| Arg / Flag | Type | Default | Purpose |
|---|---|---|---|
| `<name>` positional | string | — | Target symbol (mutually exclusive with `--since`) |
| `--since <ref>` | git ref | — | Compute impact across all symbols that changed since `<ref>` (replaces standalone `diff` command) |
| `--direction <dir>` | enum | `up` | `up` (callers) / `down` (callees) / `both` |
| `--depth <N>` | int | 5 | BFS depth |
| `--file <substr>` | string | — | Disambiguate symbol target |
| `--kind <kind>` | enum | — | Disambiguate by kind |
| `--high-trust-only` | bool | **true** | Default ON (changed from current `false`) — only follow confidence ≥ 0.8 edges |
| `--min-confidence <f>` | f32 | 0.8 | Override threshold |
| `--include-tests` | bool | false | Include test files |
| `--relation-types <list>` | csv | all | Limit relation types |

**Output**: Affected symbols organized by hop depth, with risk classification:
- LOW (<5 callers), MEDIUM (5–20), HIGH (20–100), CRITICAL (>100)

**Two modes (P1 composition)**:
- `gnx impact validateUser` → blast radius from one symbol
- `gnx impact --since HEAD~1` → blast radius across all symbols changed since `HEAD~1` (collapses the old `detect-changes` / `diff` flow)

**Empty / error (P5)**:
- `X` has 0 callers → `X exists but has 0 incoming references. Possible: entry point, dead code, or recent rename. Try --direction both / --include-tests`
- Repo not indexed → auto-ensure → retry

**Multi-repo**: yes (per-repo independent computation).

#### 3.2.4 `gnx rename <old> <new>`

AST-aware multi-file rename, with built-in verification.

| Arg / Flag | Type | Default | Purpose |
|---|---|---|---|
| `<old>` positional | string | — | Existing symbol name |
| `<new>` positional | string | — | New symbol name |
| `--code` | bool | **true** | Rename in source code identifiers (safe default ON) |
| `--comment` | bool | false | Also rename in inline code comments |
| `--markdown` | bool | false | Also rename in markdown/RST docs |
| `--reference` | bool | false | Also rename in docstrings / cross-references |
| `--all` | bool | false | Shortcut: `--code --comment --markdown --reference` |
| `--dry-run` | bool | false | Preview without writing |

Multi-flag combinable: `--code --markdown` = "code + markdown only".

**Built-in post-rename verification (P1)**:

After execution (or in dry-run mode), the server automatically runs two queries and embeds the results:

1. **Old-name residual check** — search for `<old>` after the rename; ideal residual = 0. Non-zero residuals are itemized with their context (string literal / markdown / test fixture / etc.).
2. **New-name distribution** — search for `<new>`; expected count = rename count. Excess count = collision detected.

**Example output**:
```
✓ Renamed 12 occurrences in 8 files
  src/auth/user.ts          3
  src/auth/middleware.ts    5
  src/api/handlers/auth.ts  2
  src/types/auth.ts         1
  src/services/session.ts   1

⚠ Old name "validateUser" still present in 2 places:
  - tests/fixtures/legacy_data.json:42   (string literal)
  - docs/api.md:108                       (markdown — opt-in with --markdown)

✓ New name "checkUser" distribution:
  - 12 code references (matches, no unexpected collisions)

→ To also rename in markdown: gnx rename validateUser checkUser --markdown
→ To inspect new symbol:      gnx inspect checkUser
```

**Pre-flight collision detection (dry-run)**: if `<new>` already exists as a different symbol, the command fails the dry-run with a `COLLISION` warning + suggestions.

**Empty case (P5)**: 0 occurrences → explicit "No occurrences of `<old>` found" + fuzzy-match suggestions + opt-in flag hints (e.g., `--markdown` if string-literal matches were detected).

**Multi-repo**: no (single repo per invocation — modifies source).

#### 3.2.5 `gnx cypher <query>`

Cypher escape hatch for queries not covered by canned commands.

| Arg / Flag | Type | Default | Purpose |
|---|---|---|---|
| `<query>` positional | string | — | Cypher query |
| `--params <json>` | json | `{}` | Bound parameters |
| `--limit <N>` | int | 100 | Result row limit |

**Multi-repo**: no (graph identity is single-repo).

**Empty / error (P5)**:
- 0 rows → `Query returned 0 rows.`
- Parse error → `Cypher parse error at col <N>: <token>` + tip pointing to `gnx cypher --help` for grammar.

#### 3.2.6 `gnx coverage`

Single entry point for "what do I have / how healthy is it". Folds the old `doctor` + `status` + `list` + `overview`. External-client (HTTP/DB/Redis/queue) usage detail is **not** folded here — see the standalone `gnx tool-map` command (its per-callsite binding analysis sits beyond a health summary's granularity).

| Arg / Flag | Type | Default | Purpose |
|---|---|---|---|
| `--frameworks` | bool | true (incl) | Framework detection coverage |
| `--freshness` | bool | true (incl) | Index vs working-tree mtime check |
| `--blind-spots` | bool | true (incl) | Unsupported file types, missing grammars |
| `--detailed` | bool | false | Verbose per-section breakdown |

Without `--repo`: registry-level overview (indexed repos, groups, global health).
With `--repo`: per-repo health.
With `--repo @group`: aggregated group health.

**Empty**: never (always reports framework coverage; if 0 frameworks detected, says so + suggests `gnx admin index --reembed`).

**Multi-repo**: yes.

#### 3.2.7 `gnx routes [<path>]`

HTTP routes navigation.

| Arg / Flag | Type | Default | Purpose |
|---|---|---|---|
| `<path>` positional | string | — | If given, show handler + caller chain for this route |
| `--method <m>` | string | all | Filter (`GET`/`POST`/…) |

Without `<path>`: list all routes.
With `<path>`: full route detail (handler symbol + signature + caller chain).

**Empty (P5)**: `No HTTP routes detected. Frameworks scanned: [list]. Possible: framework not supported / no route declarations / coverage gap (see gnx coverage --blind-spots)`

**Multi-repo**: yes.

#### 3.2.8 `gnx scan <file>`

File-level hallucination check — verify all symbol references in a file actually exist in the graph.

| Arg / Flag | Type | Default | Purpose |
|---|---|---|---|
| `<file>` positional | path | — | File to scan |
| `--strict` | bool | false | Also flag uncertain references (heuristic-resolved) |

**Output**: list of unresolved references with location + fuzzy suggestions.

**Composed (P1)**: each unresolved reference is paired with up to 3 near-match suggestions ("did you mean ...?").

**Empty (P5)**: `File OK, 0 unresolved references ✓`

**Multi-repo**: no (file is in a specific repo).

#### 3.2.9 `gnx contracts`

Cross-repo API contracts inventory: producer (server-side route / queue producer / RPC endpoint) ↔ consumer (HTTP client / queue consumer / RPC caller).

| Arg / Flag | Type | Default | Purpose |
|---|---|---|---|
| `--kind <k>` | enum | `all` | `routes` / `queue` / `rpc` / `all` |
| `--unmatched-only` | bool | false | Only contracts without a paired consumer/producer |

**Multi-repo**: yes (core use case). Designed primarily for `--repo @group` / `@all`.

**Empty (P5)**:
- Group has only 1 repo → `Group "<g>" has 1 member; cross-repo contracts need ≥2 repos`
- No matches in multi-repo selector → `No cross-repo contracts found. Possible: no @route + @client pairings, or framework coverage gap (see gnx coverage)`

---

## 4. CLI Surface — Admin Side

Admin is a **nested subcommand**, hidden from the top-level `gnx --help`:

```rust
Commands::Admin(AdminCommands)  // #[command(hide = true)]
```

`gnx admin --help` lists:

```
GitNexus administrative operations (registry, hooks, destructive ops).

Usage: gnx admin <COMMAND>

Commands:
  install-hook     Install git ref-transaction hook for branch tracking
  drop             Delete a repo's index data + registry entry
  prune            Remove orphan index dirs not in registry
  rename-branch    Rename a branch's index dir
  config           Interactive TOML config editor
  group            Manage repo group membership
  index            Build or refresh the graph for a repo (explicit / bulk / embeddings)
```

### 4.1 Admin commands

| Command | Args / Flags | Purpose |
|---|---|---|
| `install-hook` | — | Install `.git/hooks/reference-transaction` hook |
| `drop <repo>` | `--yes` (skip confirm) | Delete index data + registry entry (destructive) |
| `prune` | `--dry-run` | Remove orphan index dirs not in registry |
| `rename-branch <old> <new>` | — | Rename a branch's index dir + registry entry |
| `config` | — | Interactive TUI for `.gitnexus-rs/config.toml` |
| `index <path>` | `--embeddings` `--patch <file>` `--branch <name>` `--format <fmt>` | Explicit build/refresh (agent uses auto-ensure §5 instead) |

### 4.2 Admin nested group ops

```
gnx admin group <COMMAND>

Commands:
  add <repo> <group>     Add a repo to a group (auto-creates group)
  remove <repo> <group>  Remove a repo from a group (auto-deletes empty group)
```

Multi-group membership is supported (schema migration §8).

### 4.3 Hidden internal commands

Not in any `--help` output, but still runnable:

| Command | Purpose |
|---|---|
| `hook-handle` | Git ref-transaction event handler (called by hook) |
| `hook-watcher` | Background watcher (forked by `hook-handle`) |
| `verify-resolver` | gnx-developer QA tool (resolver dump vs language oracle) |

---

## 5. Auto-Ensure Index

Agent CLI does not expose `index` as an explicit command. Instead, any query command (inspect / search / impact / cypher / coverage / routes / scan / contracts) follows this protocol:

```
1. Resolve --repo selector → repo path(s).
2. For each repo:
   a. Check whether graph.bin exists at expected path.
   b. If missing:
      - Print: `Repo "<name>" not indexed yet — auto-indexing (est. ~<N>s)...`
      - Run index synchronously (same code path as `gnx admin index`).
      - On success: continue with query.
      - On failure: surface error with diagnostic (see §7 Output Contract).
   c. If present but stale (mtime < newest source file):
      - Emit `⚠ Index for "<name>" is stale (last built <duration> ago).` to stderr.
      - Continue with query (do not auto-rebuild — too expensive for every call).
      - Agent / hook may proactively run `gnx admin index` after large changes.
```

Auto-ensure is **single-repo per invocation** unless multi-repo selector forces fan-out, in which case each repo is ensured independently (parallel via rayon, mirrors current `multi_query` pattern).

---

## 6. `--repo` Selector Grammar

Global flag. Grammar:

```
selector := atom | atom,atom,...
atom     := <path> | <name> | @<group> | @all
```

Forms accepted:

| Form | Resolution |
|---|---|
| (omitted) | cwd repo (looked up in registry by canonicalized path) |
| `.` / `./rel` / `/abs/path` | Filesystem path → registry lookup |
| `<name>` (no `@` prefix) | Registry name lookup |
| `<name1>,<name2>` | Multiple repos by name (ad-hoc, no pre-existing group required) |
| `@<group>` | Group expansion via `RepoEntry.groups` (§8 schema) |
| `@<g1>,@<g2>` | Multi-group union (deduplicated) |
| `<name>,@<g>` | Mix: name + group expansion |
| `@all` | All registered repos |

**Resolution failure**: `--repo "x,y"` where `y` doesn't resolve → fail with itemized list of which atoms didn't resolve + suggestion (`gnx list` or `gnx coverage`).

**Per-command multi-repo support**:

| Command | Multi-repo | Notes |
|---|---|---|
| `inspect` | ✓ | Returns matches per repo with repo label |
| `search` | ✓ | Replaces `multi_query`; rayon-parallel; top-K merged |
| `impact` | ✓ | Per-repo independent (cross-repo via `contracts` once linked) |
| `cypher` | ✗ | Graph identity is single-repo; multi-repo selector → error |
| `rename` | ✗ | Modifies source; single-repo per invocation |
| `coverage` | ✓ | Aggregated for groups |
| `routes` | ✓ | Cross-repo route inventory |
| `scan` | ✗ | File is in one repo |
| `contracts` | ✓ | Core use case |

---

## 7. Output Contract

Cross-cutting rules for all agent commands.

### 7.1 Server-side format selection

Agent has no `--format` flag. Server selects per command and per result shape:

| Result shape | Server output |
|---|---|
| Single record (e.g., `inspect` single match) | Plain compact text |
| Tabular multi-row (e.g., `search`, `routes` list) | TOON / eToon |
| Tree (e.g., `impact` multi-hop) | eToon |
| List with few columns (e.g., `list` inside coverage) | Compact text or TOON |

Internally backed by **etoon v0.1.4+** auto-detect (JSON/log mode) — agent sees the cooked output, never the raw shape choice.

### 7.2 No silent empties

Every empty result returns a one-line reason + one-line next-step suggestion. See per-command empty-case tables (§3.2.*).

### 7.3 Error transparency + recovery

Errors return:
1. What failed (one line)
2. Cause (one line, if known)
3. Recovery hint (one line)

Example:
```
✗ Index build failed for "/path/x":
  cause: framework not recognized — language detection inconclusive
  next:  gnx coverage --repo /path/x --blind-spots
```

### 7.4 Hint discipline

Server appends contextual hints **only** in these situations:

| Trigger | Add hint? |
|---|---|
| Empty / not-found | Required |
| Ambiguous multi-match | Required (narrowing hint) |
| Tool-side failure | Required (recovery hint) |
| Stale index detected | Required (one-line warning) |
| Pre-flight collision | Required |
| Normal multi-row result | None (data speaks) |
| Single clear result | None |

**Hint format**:
- Max 1–2 lines after main output
- Prefix markers: `⚠` warning / `✓` confirm / `→` suggestion / `✗` error
- Never duplicate information already in main output
- Never decorative ("This is a search command")

---

## 8. Schema Migration: Multi-Group Membership

### 8.1 Old schema

```rust
RepoEntry {
    name: String,
    remote_url: String,
    worktree_path: String,
    index_dir_root: String,
    branches: Vec<BranchEntry>,
    group: Option<String>,    // ← single group
}
```

### 8.2 New schema

```rust
RepoEntry {
    name: String,
    remote_url: String,
    worktree_path: String,
    index_dir_root: String,
    branches: Vec<BranchEntry>,
    groups: Vec<String>,      // ← multi-group, may be empty
}
```

### 8.3 Migration

`RegistryFile::read_or_empty` performs auto-migration on first read:
- `group: Some("x")` → `groups: vec!["x"]`
- `group: None` → `groups: vec![]`
- Subsequent writes use the new schema. `.bak` retention ensures rollback safety.

`GroupEntry { name, members }` unchanged.

### 8.4 Group CRUD via admin

- `gnx admin group add <repo> <group>` — appends `<group>` to `RepoEntry.groups` (if not already present); auto-creates `GroupEntry` if needed; adds `<repo>` to `GroupEntry.members`.
- `gnx admin group remove <repo> <group>` — removes `<group>` from `RepoEntry.groups` and `<repo>` from `GroupEntry.members`; if `GroupEntry.members` becomes empty, deletes the `GroupEntry`.

---

## 9. Backward Compatibility

**Hard break — no aliases.** v0.1.0 is pre-release, and the design's value depends on a clean surface. Old top-level commands (`analyze`, `clean`, `remove`, `list`, etc.) become unrecognized subcommands.

**Required updates as part of this work**:
- `README.md` (9 references to old commands, see `git grep` of original audit)
- `CLAUDE.md` (gnx workflow section in user's global instructions)
- Any test scripts in `crates/graph-nexus-cli/tests/` that invoke commands by name
- Any references in `scripts/parity/` and `scripts/benchmark_gnx.py`

`multi_query` command (#5, recently merged) is **superseded by `search` with `--repo` multi-repo selector**. The `multi_query` source code (`commands/multi_query.rs`) is removed; its rayon parallel-load + top-K heap logic moves into `commands/search.rs`.

---

## 10. Command Count Summary

| | Before | After |
|---|---|---|
| Top-level visible (agent) | 25 | **9** |
| Admin (under `gnx admin`) | (mixed) | **7 entries at `gnx admin --help`** (6 leaves + 1 `group` parent), with 2 nested subcommands under `gnx admin group` |
| Hidden internal | 2 | **3** |
| Global flags | 1 (`--graph`) | **1** (`--repo`) |
| Total binary surface | 27 | **19** |

Dropped (folded or removed): `analyze` (→ `admin index`), `analyze-here` (→ auto-ensure / `admin index .`), `register`, `unregister`/`remove`, `index` (recovery, → registry self-heals via `admin index`), `clean` (→ `admin drop`), `init` (→ `admin install-hook`), `list` (→ `coverage`), `summarize` (→ `coverage`), `doctor` (→ `coverage`), `status` (→ `coverage` + auto stale warnings), `route-map` + `api-impact` (→ `routes`), `detect-changes` (→ `impact --since`), `cluster`, `process`, `multi_query` (→ `search` multi-repo), `context` (→ `inspect`), `query` (→ `search`). `tool-map` was initially folded into `coverage --externals` but later restored as a standalone command — its per-callsite binding analysis is too granular for a health summary.

Added: `scan`, `contracts`, nested `admin group add/remove`, multi-group `RepoEntry.groups`, auto-ensure indexing, server-side rename verification, hybrid search modes.

---

## 11. Open Questions

1. **`coverage` without `--repo`**: should it report on every registered repo (potentially slow with N repos) or on `cwd` only with a hint? Current spec says registry-level overview; refine during implementation.

2. **`scan --strict` heuristic**: what counts as "uncertain"? Likely candidates: resolver tier-3 global matches (per recent eywa hint: "Cap Tier 3 global matching to unambiguous cases only when global_matches length is exactly one"). Spec leaves implementation latitude.

3. **`auto` mode for search**: regex `^[A-Za-z0-9_]+$` is heuristic. If real-world inputs hit edge cases (e.g., `User_ID` slug treated as bm25 but user meant concept), revisit. Initial implementation: stick with regex; instrument to log auto-mode decisions for offline tuning.

4. **`impact --since` overlap with `git diff`**: should we mirror git's `..` / `...` ref syntax? Initial: `--since HEAD~1` (single ref means "compared to current HEAD"); add `--base..--head` later if needed.

5. **etoon vs TOON default**: §7.1 says server selects. Implementation decision: route through etoon as the universal serializer (per eywa hint: "all final output segments | etoon"), but suppress columnar layout for single-record outputs (auto-detect text mode).

---

## 12. Testing Strategy

- **CLI surface tests**: integration tests verify `gnx --help` exposes exactly the 9 agent commands; `gnx admin --help` exposes exactly the 7 admin commands; hidden commands are runnable but absent from `--help` output.
- **Per-command tests**: each command has at least: happy path, empty result, ambiguous result, error case (e.g., no index), multi-repo case (where applicable).
- **Rename verification tests**: post-rename old-name search returns 0 (with `--all`); residual detection works (mixed `--code` only run leaves markdown intact and reports it).
- **Schema migration tests**: old `group: Some(x)` → `groups: vec![x]`; `group: None` → `groups: vec![]`; write-back uses new schema; `.bak` preserves old.
- **Selector grammar tests**: each atom form resolves correctly; malformed selectors fail with actionable error.
- **Auto-ensure tests**: missing index triggers build; failed build surfaces error; stale index emits warning but does not block.

Existing test infrastructure under `crates/graph-nexus-cli/tests/` is the home base.

---

## 13. Out of Scope

Deferred to future work, explicitly NOT in this redesign:

- **`find` smell recipes** (orphans / god-classes / circular-imports) — cypher covers; revisit if usage shows agents struggling to compose queries.
- **`pre-edit` combo command** — server-side composition in `inspect` (auto-includes 1-hop upstream + freshness) eliminates the need; hook integration can chain `inspect` + `impact` directly if needed.
- **Cross-repo `impact`** beyond per-repo independent computation — requires contract-graph linking; deferred until `contracts` is populated and proven in production.
- **`gnx update`** / self-update CLI — out of CLI redesign scope.
- **Telemetry / usage analytics** — out of scope.

---

## 14. Decision Trajectory (for posterity)

Brainstorm sequence that produced this spec:

1. Started from 25 visible commands, agent uses ~7 heavily, rest are admin / niche.
2. Considered four grouping strategies (display_order + after_help / `gnx admin` namespace / hide-by-default / docs-only). Picked nested `gnx admin` with `hide = true` at top.
3. User insight: pre-release status (v0.1.0) means hard break is acceptable; no aliases.
4. User insight: rename for clarity goes deeper than admin — `analyze` should be `index` (commit `c08b3e9` already partial). Drove full rename pass.
5. User insight: many commands are obsolete because Python-era constraints disappeared (e.g., `register` exists because Python `analyze` was slow; gnx-rs is fast → `register` unnecessary).
6. User insight: collapse name-based variants into one command with flags (no separate `signature`, `exists`, `callers`, `overrides` — all flags of `inspect`).
7. User insight: don't worry about tokens — return rich info by default; agent never types flags.
8. User insight: agent CLI should never see admin (not even hint at its existence).
9. User insight: `index` keeps an agent face via auto-ensure; explicit `gnx admin index` remains for human/bulk operations.
10. User insight: cross-repo needs ad-hoc multi-repo + multi-group; single `--repo` selector unifies upstream's `group_*` commands.
11. User insight: server-side composition — agent never chains; ambiguity returns rich matches, not opaque UIDs.
12. User insight: drop `--format` from agent surface; server decides.
13. User insight: `rename` defaults must be safe (code-only); opt-in for comment/markdown.
14. User insight: every command needs explicit empty/error/recovery messaging (no silent empties).

---

*End of spec. Implementation plan to be produced via the `superpowers:writing-plans` skill.*
