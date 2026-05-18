# `gnx group` — Multi-repo Query Design

**Status**: Shipped 2026-05-18
**Date**: 2026-05-18
**Author**: Iteration with brainstorming skill
**Reference**: upstream `gitnexus` (`.source_code/gitnexus/src/cli/group.ts`, `src/core/group/`)

> **Amendment (2026-05-18, post-merge):** `gnx group search` was folded into
> `gnx group find` with `--merge none|rrf` + `--limit N` + `--batch`. The
> `search` verb no longer exists at the `group` level. Sections below that
> mention `gnx group search` should be read as `gnx group find --merge rrf`
> (RRF-merged top-K) or `gnx group find --merge none` (per-repo concat).
> Rationale: `find` is the noun-first verb at top-level (`gnx find`) and the
> two `group` verbs differed only in default merge mode — exposing that as a
> flag eliminated a duplicate CLI surface. Multi-query (batch) is `--batch`
> on `find`, never a new verb.

## Motivation

`gnx` already has the **selector layer** for groups (`@<group>` / `@all` in
`repo_selector.rs`, `RegistryFile.groups`, `admin group add/remove/list`) and
a handful of commands that accept `--repo @group` (`search`, `find`,
`contracts`, `coverage`). The **multi-repo workflow** layer — contract
extraction, cross-repo impact, staleness reporting — is missing. LLM agents
that already know upstream `gitnexus group sync / impact / query` cannot
transfer that knowledge to `gnx` today.

This spec brings the upstream workflow into `gnx` while keeping gnx's
performance non-negotiables (rkyv zero-copy, `<30 ms` per-query target) and
its surgical, signal-dense output discipline.

## Reused infrastructure (no rewrites)

Concrete existing pieces the new code MUST call instead of re-implementing.
Anything not listed here is genuinely net-new.

| Existing piece | Location | Where group code uses it |
|---|---|---|
| `RegistryFile` + `GroupEntry` schema | `graph-nexus-core/src/registry/store.rs` | Source of truth for group membership — **no new YAML/JSON file** |
| `Registry::resolve` | `graph-nexus-core/src/registry/` | Member identifier → graph path. Reused for `group impact --repo <member>` (resolves the same way as `--repo <name>` elsewhere) |
| `ZeroCopyGraph` mmap | `graph-nexus-core` | Per-repo graph reads in extractors and impact fan-out |
| `TantivyEngine` BM25 index | `graph-nexus-cli/src/search.rs` (used via `tantivy_hits` in `commands/find.rs:598`) | Cross-repo contract-id matching in the BM25 stage. Group sync builds a per-group Tantivy index over `contract_id`s and queries it for unmatched contracts — **no new BM25 dependency** |
| `bm25_hits_from_graph` + `substring_hits` fallback | `commands/find.rs` | Pattern same as `gnx find` — Tantivy when index exists, substring scan when missing |
| `atomic_write_json` | `graph-nexus-core/src/registry/io.rs` | `meta.json` writes |
| rkyv `Archive`/`Serialize` derive | already in `graph-nexus-core` deps | `contracts.rkyv` |
| `Config` (`~/.gnx/config.toml`) | `graph-nexus-core/src/config.rs:13` | Adds a `group` section instead of hardcoding thresholds (see "Configuration" below) |
| `commands/admin/config.rs` editor | `commands/admin/config.rs` | New group fields surface in `gnx admin config` without new UI |
| Existing `search` / `find` / `coverage` engines | `commands/{search,find,coverage}.rs` | `gnx group <verb>` are **thin parallel fan-outs**, not re-implementations |
| Existing `impact` engine | `commands/impact.rs` | Local phase of `gnx group impact` — cross fan-out only adds a wrapper |
| rayon for member fan-out | already in deps | Parallel extractor execution and search fan-out |
| `atomic_write` rename pattern | reused from `atomic_write_json` | `contracts.rkyv` write is `tmp → rename` |

The matching cascade (exact + BM25) is therefore **zero new search code** —
just per-group Tantivy index building plus the cascade glue.

## Non-goals

- **Not** a rewrite of upstream's TypeScript implementation in Rust.
- **Not** an embedding-fallback matcher in the first cut (`exact` + `BM25`
  only — matches upstream's recommendation to ship without embeddings first).
- **Not** a `group.yaml` config file — gnx keeps the source of truth in
  `~/.gnx/registry.json` (`GroupEntry { name, members }`).
- **Not** the workspace/monorepo extractors (Go / Java / Python / Rust /
  Node / Elixir workspace detection) — those land in a follow-up once
  the HTTP+gRPC pipeline proves the storage model.

## Command topology

### Decision: noun-first under `gnx group`, management stays in `admin`

| Category | Command | Status |
|---|---|---|
| Management (writes registry) | `gnx admin group add / remove / list` | Existing — keep as-is |
| Cross-repo queries (new namespace) | `gnx group sync / status / impact / contracts / search / find / coverage` | **New top-level namespace** |
| Single-repo / all-repo queries | `gnx search / find / contracts / coverage --repo <name>|@all` | Existing — keep as-is |

**Removed (breaking)**: `--repo @<group>` on `search / find / contracts /
coverage`. Resolving `@<group>` from these commands returns an error with a
hint pointing to the `gnx group <verb>` equivalent. gnx is pre-1.0; the
callers are scripts and the MCP layer, both of which we control.

Rationale:
- `admin group add/remove` mutates `~/.gnx/registry.json` and belongs with
  the other config-mutation commands.
- Cross-repo *query* commands are noun-first because the group is the
  primary noun — fan-out semantics, cross-link traversal, and RRF merge are
  group-shaped operations, not single-repo operations with an extra flag.
- Splitting `search --repo @group` away from `group search` means
  command scope (single vs. group) is visible from the command name, not
  hidden in a flag — better for LLM discoverability via `--help`.

### Final command surface (post-change)

```
gnx admin group add <group> <repo>
gnx admin group remove <group> <repo>
gnx admin group list [name]

gnx group status <name>             # staleness audit (index + contracts)
gnx group sync <name> [flags]       # extract contracts + build cross-links
gnx group contracts <name> [flags]  # inspect contract registry
gnx group impact <name> --target <symbol> --repo <member> [flags]
gnx group search <name> <query> [flags]
gnx group find <name> <pattern> [flags]
gnx group coverage <name> [flags]
```

`group create` from upstream is **dropped** — `admin group add` already
creates the group if missing (see existing test
`group_add_creates_group_if_missing`).

## Data model

### Storage layout

```
~/.gnx/
  registry.json                  # existing — RepoAlias + GroupEntry
  groups/
    <name>/
      contracts.rkyv             # ContractRegistry (rkyv-archived)
      meta.json                  # small JSON: generatedAt, repoSnapshots, missingRepos
```

**Why split `meta.json` from `contracts.rkyv`**: `meta.json` is small,
human-readable, and read on every `gnx group status` call without needing to
mmap the contracts archive. `contracts.rkyv` is the hot path (impact /
contracts queries) and stays zero-copy.

**Why rkyv, not SQLite**: upstream's `bridge.lbug` is SQLite. gnx already has
`ZeroCopyGraph` (rkyv mmap) for per-repo graphs hitting `<30 ms`. A SQLite
bridge would be a step backward in the hot path. rkyv archives expose
`ArchivedContractRegistry` for zero-copy read; mutations rebuild the archive
atomically via `atomic_write` (same pattern as `registry::io::atomic_write_json`).

### Types (Rust, in new crate `graph-nexus-group` or under
`graph-nexus-cli::group::types`)

```rust
#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub enum ContractType { Http, Grpc, Thrift, Topic, Lib, Custom, Include }

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub enum ContractRole { Provider, Consumer }

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub enum MatchType { Exact, Manifest, Wildcard, Bm25, Embedding }

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct SymbolRef {
    pub file_path: String,
    pub name: String,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct ExtractedContract {
    pub contract_id: String,         // e.g. "http:POST:/api/users"
    pub contract_type: ContractType,
    pub role: ContractRole,
    pub symbol_uid: String,
    pub symbol_ref: SymbolRef,
    pub confidence: f32,             // [0.0, 1.0]
    pub service: Option<String>,     // monorepo sub-path
    pub meta: Vec<(String, String)>, // flat key/value, not nested JSON
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct StoredContract {
    pub repo: String,                // registry name of containing repo
    #[rkyv(omit_bounds)]
    pub inner: ExtractedContract,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct CrossLinkEndpoint {
    pub repo: String,
    pub service: Option<String>,
    pub symbol_uid: String,
    pub symbol_ref: SymbolRef,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct CrossLink {
    pub from: CrossLinkEndpoint,
    pub to: CrossLinkEndpoint,
    pub contract_type: ContractType,
    pub contract_id: String,
    pub match_type: MatchType,
    pub confidence: f32,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct ContractRegistry {
    pub version: u32,                // start at 1
    pub contracts: Vec<StoredContract>,
    pub cross_links: Vec<CrossLink>,
    pub unmatched: Vec<StoredContract>,
}
```

`meta.json` is plain serde JSON (small, evolves more often than the archive
schema):

```rust
#[derive(Serialize, Deserialize)]
pub struct GroupMeta {
    pub version: u32,
    pub generated_at: String,                       // RFC3339
    pub repo_snapshots: BTreeMap<String, RepoSnapshot>,
    pub missing_repos: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct RepoSnapshot {
    pub indexed_at: String,
    pub last_commit: String,
}
```

### Configuration (no hardcoded knobs)

Threshold and behavioral knobs live in `Config` (existing
`graph-nexus-core/src/config.rs`), not as `const` literals in group code.

```rust
// Added to Config struct
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GroupConfig {
    /// BM25 score floor for cross-link matching. Below this, contracts
    /// land in `unmatched` rather than producing a low-confidence link.
    #[serde(default = "default_group_bm25_threshold")]
    pub bm25_threshold: f32,                  // default 0.6

    /// Per-contract max BM25 candidates fetched before threshold filter.
    #[serde(default = "default_group_max_candidates")]
    pub max_candidates_per_step: u32,         // default 16

    /// HTTP paths excluded from cross-link matching (health checks,
    /// /ping, /metrics — would otherwise produce N×M false links).
    /// Trailing slashes normalized before comparison.
    #[serde(default)]
    pub exclude_links_paths: Vec<String>,     // default []

    /// When true, exclude HTTP routes that are entirely {param} segments
    /// (e.g. `/{param}`, `/{param}/{param}`). Mixed routes like
    /// `/users/{param}` are unaffected.
    #[serde(default)]
    pub exclude_links_param_only_paths: bool, // default false

    /// Cross-repo impact: max additional hops after the local impact phase.
    /// Clamped to 1 for first wave (multi-hop deferred).
    #[serde(default = "default_group_cross_depth")]
    pub cross_depth: u32,                     // default 1

    /// Local impact wall-clock budget for `gnx group impact`.
    #[serde(default = "default_group_timeout_ms")]
    pub local_impact_timeout_ms: u64,         // default 5000
}
```

CLI flags (`--bm25-threshold`, `--exclude-path`, `--cross-depth`,
`--timeout-ms`, ...) **override** the config for a single invocation but
do not mutate the stored value. Pattern matches existing `Config` usage:
constants disappear from group code entirely.

`exclude_links_paths` defaults to `[]` and **the first wave does not ship
a vendored deny list** — health-check noise is real but every team's
deny list looks different. We don't hardcode `/ping`, `/health`,
`/metrics`; users add them once via `gnx admin config` if needed.

### Manifest links — deferred

Upstream supports user-authored `links[]` in `group.yaml` as an escape hatch
when extractors miss a connection. **Not in this spec.** Reason: the
extractors haven't been built yet; we can't know what they miss. After
HTTP+gRPC ship and we see real false-negatives, we add a thin `links` field
to `GroupEntry` in `registry.json` (no separate file).

## Workflows

### `gnx group sync <name>`

Pipeline:

1. Load `GroupEntry` from `registry.json`. Resolve each member to its indexed
   graph path via `Registry::resolve`.
2. For each member, in parallel via rayon (members are independent):
   - Strategy A (graph-assisted): query the per-repo `ZeroCopyGraph` for
     edges/nodes that already encode contracts (e.g. `Route` nodes,
     `HANDLES_ROUTE` edges from the existing route detector).
   - Strategy B (source-scan fallback): run per-language tree-sitter
     extractors on `*.{go,py,ts,js,java,rs,php,...}`. **First wave: HTTP
     routes + gRPC service definitions only.** Topic / Thrift / Include come
     later.
3. Collect `ExtractedContract` arrays into a flat `Vec<StoredContract>`.
4. Matching cascade (single pass over indexed contracts):
   - **Exact**: same `contract_id` provider/consumer → `confidence = 1.0`.
   - **BM25** (if not `--exact-only`): index contract IDs into a per-group
     bm25s-style ranker, match below-1.0 candidates with score above
     `bm25_threshold` (default `0.6`).
   - Anything left lands in `unmatched`.
5. Write `contracts.rkyv` atomically (`tmp → rename`). Update `meta.json`
   with `generated_at` and per-member commit hashes.
6. Emit summary (TOON):

```
group         <name>
contracts     <n>
cross_links
  exact       <n>
  bm25        <n>
unmatched     <n>
missing_repos [...]
elapsed_ms    <n>
```

Flags (initial set, matching upstream verbs):

| Flag | Default | Effect |
|---|---|---|
| `--exact-only` | false | Skip BM25 stage |
| `--skip-embeddings` | true | Always true in first cut (no embeddings) |
| `--allow-stale` | false | Don't bail when per-repo index is stale |
| `--verbose` | false | Per-cross-link detail in output |
| `--json` | false | JSON instead of TOON |

### `gnx group status <name>`

Read `meta.json` + each member's `index_meta.json` + each member's HEAD via
`git rev-parse`. Report per-repo:

| Column | Source |
|---|---|
| `index_stale` | `meta.repo_snapshots[m].last_commit != HEAD` |
| `commits_behind` | `git rev-list --count <stored>..HEAD` |
| `contracts_stale` | `meta.generated_at < per-repo index time` |
| `missing` | not in registry or unreadable |

Default output: TOON table. No mutation.

### `gnx group impact <name> --target <symbol> --repo <member>`

Two-phase:

1. **Local impact** — run existing `gnx impact` engine on the named member
   (`--repo` is the member name from `GroupEntry.members`, not an arbitrary
   registry name). Wall-clock budget honored via `--timeout-ms` (default
   from existing `impact` config).
2. **Cross-repo fan-out** — for each direct hit symbol whose UID appears as
   a `from.symbol_uid` in `cross_links`, follow the link to `to.repo`,
   re-enter that repo's graph, walk one hop (initially), collect affected
   processes.

Output (TOON):

```
group         <name>
target        <symbol>
risk          high|medium|low|none
summary
  direct                 <n>
  processes_affected     <n>
  cross_repo_hits        <n>
  modules_affected       <n>
local
  ...                                # existing impact payload, untouched
cross
  - repo                <r>
    contract            <id>
    match_type          exact|bm25
    confidence          <f>
    affected_processes  [...]
out_of_scope
  - from / to / contract_id / confidence    # cross-links pointing outside group
truncated     true|false
truncation_reason  timeout|partial|null
```

Flags: `--direction upstream|downstream`, `--max-depth`, `--cross-depth`
(initial: clamp to 1, surface `crossDepthWarning` per upstream),
`--min-confidence`, `--service`, `--subgroup`, `--include-tests`,
`--timeout-ms`, `--json`.

### `gnx group contracts <name>`

Pure read of `contracts.rkyv`. Filters: `--type http|grpc|...`, `--repo
<member>`, `--unmatched`, `--json`.

### `gnx group search / find / coverage <name>`

Thin orchestration wrappers:

1. Resolve group to ordered member list.
2. Fan out the existing single-repo engine (`search` / `find` / `coverage`)
   via rayon across members.
3. For `search`: optional **RRF merge** (Reciprocal Rank Fusion) of per-repo
   results, then output top-K. Per-repo result counts surfaced in a
   `per_repo` sibling array (matches upstream `groupQuery` shape).
4. For `find` / `coverage`: concatenate per-repo outputs with a `repo`
   column prefix. No merge logic needed — these are listing operations.

Implementation note: existing engines (`compute_hits`, `dispatch_by_mode`)
stay untouched. The fan-out lives in `commands/group/mod.rs` and only calls
the existing engine entry points.

## Migration of `--repo @group` callers — delete, don't gate

Inventory of files touching `@<group>` selector today
(`grep -l "@group" crates/graph-nexus-cli/src/commands/`):

- `commands/coverage.rs`
- `commands/contracts.rs` — currently gates `@<group>` / `@all`
- `commands/find.rs`
- `commands/search.rs` (multi-repo via `search_multi_repo` tests)
- `commands/admin/host_integration/` — help text references `--repo @group`

**Each command's `@<group>` branch is deleted, not gated.** Reasons:
1. Keeping a dead branch behind a runtime check is dead code that rots —
   future maintainers wonder why two paths exist.
2. The selector path (`repo_selector.rs::Atom::Group`) stays — it's
   reused by `gnx group <verb>` internals to expand the member list.
   What changes is the **dispatch site**: top-level commands now refuse
   `Atom::Group` before resolution, returning
   `SelectorError::GroupAtTopLevel { hint: "use `gnx group <verb>` instead" }`.
3. `Atom::All` (`@all`) stays usable on top-level commands — it's not a
   group, just every indexed repo, and has no `gnx group` equivalent.

Concretely per command:

| Command | Before | After |
|---|---|---|
| `gnx search --repo @hr` | iterates members serially | error → `gnx group search hr` |
| `gnx contracts --repo @hr` | iterates and concatenates | error → `gnx group contracts hr` |
| `gnx find --repo @hr` | iterates | error → `gnx group find hr` |
| `gnx coverage --repo @hr` | iterates | error → `gnx group coverage hr` |
| `gnx search --repo @all` | unchanged | unchanged |
| `gnx contracts --repo <single>` | unchanged | unchanged |
| MCP `repo: "@hr"` | iterates | error with same hint |

Affected tests get **renamed and moved**, not duplicated:
`search_multi_repo_at_group_both_repos` → `group_search_two_repos`,
relocated to `tests/group_search_*.rs`. Old test files (`search_multi_repo.rs`)
shrink to cover only `@all` paths or get deleted if nothing remains.

## Output discipline

Per CLAUDE.md, defaults:

| Command | Default format |
|---|---|
| `group status` | TOON |
| `group sync` | TOON (with `--json` switch) |
| `group contracts` | TOON |
| `group impact` | TOON |
| `group search` | text (mirrors existing `search` default) |
| `group find` | text |
| `group coverage` | TOON |

No prose, no trailing summary inside structured payloads. `summary` /
`risk` / `truncated` go in sibling fields, never interleaved.

## Accuracy guarantee — no regressions, only additions

**Today**'s `gnx contracts --repo @hr` and `gnx search --repo @hr`
behavior:
- `contracts` concatenates per-repo contract lists. **No matching, no
  cross-links.**
- `search` runs the per-repo engine on each member and concatenates.
  No merge, no de-dupe.

**After this change**:
- `gnx group contracts` returns the same raw contract list **plus** an
  optional `cross_links` array. Filtering with `--unmatched` returns the
  subset that the old command would have returned (just relabelled).
  *Precision floor*: every contract the old command listed remains
  listed.
- `gnx group search` defaults to RRF-merged top-K but `--no-merge`
  reproduces the old "concatenate per-repo streams" behavior exactly.
  *Recall floor*: every result the old command surfaced is still
  reachable via `--no-merge`, ordered the same way.

Matching cascade (`exact` → `BM25`) can only **add** cross-links, never
remove contracts. BM25-derived links carry their `confidence` and
`match_type: bm25` so consumers can filter them out if needed — gnx's
"honest 'no data' beats a guess" rule applies in reverse here too:
heuristic cross-links are tagged, never promoted.

`group impact` introduces cross-repo edges that **don't exist today** —
so there's no prior behaviour to regress against, only new signal.

## Performance budget

- `group status`: stat 1 small JSON + N `git rev-parse` + N `git rev-list
  --count`. Target `<200 ms` for N=10.
- `group sync` (first cut, exact + BM25, HTTP+gRPC only): bounded by
  rayon-parallel extractor cost. No hard target — measured against
  `scripts/benchmark_gnx.py` extension before merge.
- `group impact`: local impact uses existing path. Cross fan-out is
  `O(direct_hits × avg_links_per_uid)` rkyv index lookups. Target
  `<200 ms` for direct=20, group=5 repos.
- `group search` / `find` / `coverage`: parallel-fan-out of existing
  engines. Per-repo cost unchanged; wall-clock dominated by slowest member.

`contracts.rkyv` mmap is zero-copy — opening costs are O(file map), not
deserialization.

## Testing strategy

Per CLAUDE.md, parser/core changes need 14-language coverage. This spec is
**not** a parser change — extractors are net-new and live in the group
module. Coverage requirement scales to "every extractor language ships with
a `crates/graph-nexus-group/tests/<lang>_<extractor>.rs` fixture".

First-wave extractors (HTTP routes + gRPC):

| Language | HTTP | gRPC |
|---|---|---|
| Go | gin / chi / net-http | grpc-go server / client |
| Python | flask / fastapi | grpc-python servicer / stub |
| TS / JS / Node | express / fastify | @grpc/grpc-js |
| Java | spring-mvc | grpc-java |
| Rust | axum / actix | tonic |

Other 9 mainstream languages (Kotlin, C#, PHP, Ruby, Swift, C, C++, Dart)
get extractor stubs marked `BlindSpot` in this spec's first wave — they
emit nothing (no false positives) and surface "extractor not implemented"
in `group contracts --json` for the affected member.

Group-level tests live in `crates/graph-nexus-cli/tests/group_*.rs` and
cover:

- `group_sync_two_repo_http.rs` — Go provider + Python consumer, exact match.
- `group_sync_grpc_service.rs` — proto-less BM25 fallback path.
- `group_status_stale_index.rs` — staleness detection.
- `group_impact_cross_repo.rs` — upstream/downstream traversal.
- `group_contracts_filter.rs` — `--type`, `--repo`, `--unmatched`.
- `group_search_rrf_merge.rs` — verify per-repo + merged result shape.
- `group_at_repo_top_level_errors.rs` — `search --repo @group` returns
  the documented error.

## Crate layout

Two options. Recommendation: **(b)** — minimize new top-level crates.

(a) New crate `crates/graph-nexus-group/` containing types + extractors +
    sync engine. CLI calls into it.

(b) Module `graph_nexus_cli::group` under existing CLI crate, with types in
    `graph_nexus_cli::group::types`. Extractors as sibling modules.

Going with **(b)** because:
- No new crate boundary to enforce; group operations only need CLI-level
  types (paths, registry handles), no leaked downstream consumers.
- One fewer `Cargo.toml` to maintain, faster compile feedback loop.
- Easy promotion to its own crate later if MCP or other binaries need to
  link the same types.

Initial file tree:

```
crates/graph-nexus-cli/src/
  commands/
    group/
      mod.rs              # subcommand dispatch
      sync.rs             # contract extraction pipeline
      status.rs
      impact.rs
      contracts.rs        # NEW — distinct from existing top-level contracts.rs
      search.rs           # thin wrapper around existing search engine
      find.rs
      coverage.rs
      types.rs            # ContractType, CrossLink, ContractRegistry, ...
      storage.rs          # contracts.rkyv read/write, meta.json IO
      matching.rs         # exact + BM25 cascade
      extractors/
        mod.rs            # trait Extractor, language registry
        http_go.rs
        http_python.rs
        http_node.rs
        http_java.rs
        http_rust.rs
        grpc_go.rs
        grpc_python.rs
        grpc_node.rs
        grpc_java.rs
        grpc_rust.rs
```

Existing `commands/contracts.rs` (top-level) **stays** but loses its
`@group` / `@all` branches — those return the migration error.

## Implementation order (commits within the single PR)

**Constraint**: spec + implementation ship in **one PR** with multiple
commits. The spec is the first commit so review can start before the code
lands. Estimated total: ~3,000–4,500 LOC across ~13 commits — large for a
single PR; reviewer should expect to review per-commit, not as a single
unified diff.

0. **This spec** committed as `docs(spec): gnx group multi-repo design`.
1. `commands/group/types.rs` + `storage.rs` + meta.json IO. No CLI surface
   yet. Round-trip test in `tests/group_storage.rs`.
2. `commands/group/extractors/mod.rs` + first HTTP extractor (Go, axum-ish).
   Unit tests in `crates/graph-nexus-cli/tests/group_extract_go_http.rs`.
3. Remaining 4 HTTP extractors (Python, Node, Java, Rust). One commit each
   if any need non-trivial work, otherwise bundled.
4. gRPC extractors (5 langs, same pattern).
5. `commands/group/matching.rs` — exact + BM25 cascade.
6. `commands/group/sync.rs` + `gnx group sync` wiring. End-to-end test
   `tests/group_sync_two_repo_http.rs`.
7. `commands/group/status.rs` + wiring. Test for stale-index reporting.
8. `commands/group/contracts.rs` + wiring. Filter tests.
9. `commands/group/impact.rs` + wiring. Cross-repo fan-out test.
10. `commands/group/{search,find,coverage}.rs` — thin wrappers.
11. **Breaking-change commit**: remove `@group` from top-level
    `search/find/contracts/coverage`, replace with error pointing to
    `gnx group <verb>`. Update CLI tests + MCP layer (`mcp__gnx_*`
    handlers that accept `repo` parameter — surface the same hint
    message at the MCP boundary). MCP cannot be deferred; the breaking
    change would otherwise crash MCP-driven workflows mid-version.
12. Doc updates: `docs/skills/gnx.md` mentions the new namespace; README
    Language Matrix gains a "group extractor" column (Wave-1 langs only).

Each commit must pass `cargo test -p graph-nexus --tests` and
`cargo clippy -p graph-nexus --tests`. Format touched files only with
`rustfmt --edition 2021 <file>...`.

## Design decisions previously open

Resolved per "重用既有 + 不硬寫 + 準確度不降 + 效能第一" + eywa
principles. Listed here for traceability; flag any of these for
re-discussion before implementation starts.

1. **Member identifier for `group impact --repo <member>`** — **uses
   registry name**. Reused `Registry::resolve` semantics, no new
   identifier scheme. Upstream's `groupPath` (`hr/hiring/backend`) is a
   yaml-key artifact gnx doesn't need; existing `Registry` already
   handles name + path + alias lookup. *(Principle: 整併, don't invent.)*

2. **`group search` RRF merge** — **default-on**, `--no-merge` opts out.
   Matches upstream. The accuracy-first principle wins: merged top-K
   surfaces the highest-signal results across repos, which is what the
   LLM agent typically wants. `--no-merge` reproduces today's raw
   per-repo concatenation exactly (see "Accuracy guarantee" section).

3. **MCP layer** — **in scope** (implementation step 11). All MCP
   handlers accepting `repo: "@<group>"` reject with the same hint as
   CLI. New MCP equivalents (`mcp__gnx_group_sync`,
   `mcp__gnx_group_impact`, etc.) are *deferred to a follow-up PR* —
   this PR only removes the broken `@group` path at the MCP boundary so
   MCP-driven workflows don't crash mid-version; agents call CLI via
   shell until the new MCP tools land.

## PR scope discipline (eywa)

The PR description **must** include an explicit "Out of scope" section
mirroring the Non-goals section of this spec, so reviewers (human and
LLM) don't lobby for embedding fallback, Topic / Thrift / Include
extractors, workspace detectors, manifest links, or new MCP tools to be
bundled in. Each follow-up gets its own issue + PR.

Out-of-scope items (verbatim for the PR body):

- Embedding-fallback contract matching (only exact + BM25 in this PR)
- Topic / Thrift / Include extractors (HTTP + gRPC only this PR)
- Workspace extractors (Go / Java / Python / Rust / Node / Elixir)
- Manifest links / `group.yaml` escape-hatch authoring
- New MCP tools for group operations (only the breaking-change removal
  of `@<group>` from existing MCP handlers)
- Languages outside the Wave-1 five (Kotlin, C#, PHP, Ruby, Swift, C,
  C++, Dart get `BlindSpot` stubs only)
- Multi-hop cross-repo impact (`cross_depth > 1`)
