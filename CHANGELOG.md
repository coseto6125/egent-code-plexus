# Changelog

## v0.10.0 - 2026-08-23

### Features

- add `ecp pattern` for statement-shape search (#711)
- five more locales, pt-BR ru hi fr de (#701)
- comparison and integration pages, six locales, 12 URLs to 60 (#700)

### Performance

- parallelize full-repo scans (#713)
## v0.9.3 - 2026-08-10

### Bug Fixes

- recover the file from a pre-upgrade entry instead of blanking it (#695)
## v0.9.2 - 2026-08-10

### Features

- prerender one page per locale and give crawlers something to read (#691)

### Bug Fixes

- stop naming a declaration HARD cannot have observed (#693)
## v0.9.1 - 2026-08-10

### Bug Fixes

- make session liveness mean the agent is still there (#689)
## v0.9.0 - 2026-08-09

### Bug Fixes

- identify a symbol by (file, name), not by name alone (#687)
- make HARD and SOFT concerns actually fire (#686)
- emit the cache hints 2026-07-28 requires on tools/list (#685)
## v0.8.9 - 2026-08-06

Two fixes you can observe, and an internals pass with no behaviour change.

### Changed behaviour

- **`ecp summary --repo <path>` / `ecp contracts --repo <path>` now index a
  path that was never indexed**, instead of failing with
  `path not in registry`. The graph-loading commands (`find`, `impact`,
  `inspect`) already did this through `auto_ensure`; the registry-backed
  readers went down a different code path and did not. A `--repo <name>` or
  `--repo @group` that resolves to nothing still fails — there is no directory
  to build from, and picking one would answer a directed query against the
  wrong repo. (#679)
- **`ecp usage --clear` now clears the MCP log too.** It used to remove
  `cli-calls.jsonl` and print `MCP calls.jsonl kept`, while `ecp usage` reads
  both by default — so clearing left the dashboard reporting the history you
  had just asked it to drop. Retention (`ecp admin gc`) reaches both files as
  well; `calls.jsonl` previously grew unbounded. (#681)

### Fixes

- **CLI telemetry records could interleave on disk.** The writer's own comment
  said a single `O_APPEND` write is atomic and needs no lock — correct, but it
  used `writeln!`, which issues one write for the record and another for the
  newline. A concurrent `ecp` between the two produced `{a}{b}` on one line.
  Measured before the fix: 45 of 1747 lines unparseable locally, 8% in the
  busiest repo. Such lines were skipped by `ecp usage` (undercounting) and kept
  forever by retention (they carry no readable timestamp, so nothing could age
  them out). The newline now travels in the same write, and unreadable lines
  are pruned. (#681)
- Windows absolute paths (`C:\repo`) parsed as registry names rather than
  paths, so `--repo C:\...` could never resolve. (#679)

### Internals

Six deepening refactors from an architecture review of the repo's churn hot
spots — no behaviour change intended, and none observed across 4187 tests:
the cypher pipeline gained per-stage seams (`execute_inner` 231 → 40 lines),
`GraphBuilder::build()` dropped 1106 → 580 lines with passes 1 and 2 extracted,
index-readiness policy moved behind a single `IndexNeed` seam that `main` and
`diff` had been encoding differently, two duplicated `NodeKind` name tables
were deleted in favour of the one `uid::compute` already hashes, `ecp impact`'s
symbol-mode envelope got the typed treatment its `--baseline` twin already had,
and Pass 1's node-ordering and UID-collision invariants became unit-testable.
(#680)

## v0.8.8 - 2026-08-05

A dependency release. The MCP server moves to the 2026-07-28 protocol SDK; no
commands, flags or output formats change.

### Dependencies

- **rmcp 2.2 → 3.1.0.** rmcp 3.0 tracks MCP 2026-07-28, whose one breaking
  change reaching this crate is SEP-2322 MRTR: `ServerHandler::call_tool`
  returns a `CallToolResponse` enum (`Complete` / `InputRequired` / `Task`)
  instead of `CallToolResult`. `ecp admin mcp serve` is spawn-mode only, so
  every call runs its subcommand to completion and resolves as `Complete`.

  **Existing MCP clients need no change.** Checked against a real
  2025-06-18 stdio session: a `tools/call` still comes back as plain
  `{"content":[…]}` with no `resultType` field, byte-identical to rmcp 2.2.
  Three dependabot PRs (#668, #670, #673) stalled on this bump because a
  lockfile-only update cannot carry the source migration. (#676)
- Full `cargo update`, including etoon 0.7.1, whose encoder collapses nested
  field groups and keyed tabular forms, so `--format toon` output gets smaller
  on its own. (#675)

### Refactor

- The two hand-rolled `OnceLock<Regex>` accessors now use `regex!` from
  regex 1.13, which expands to the same `static Lazy<Regex>` shape. Identical
  behaviour, less boilerplate. (#676)

## v0.8.7 - 2026-07-26

An internals release: six refactors that each collapse a rule the codebase was
keeping in several places by hand. No new commands or flags.

### Changed behaviour

- `ecp impact --baseline` output is now deterministic. Two runs of the same
  binary over the same tree used to order `changed_symbols` differently
  (the collection loops iterate hash maps), so diffing two `ecp` outputs was
  unreliable and an agent asking the same question twice got different answers.
  Same content, stable order. (#659)
- The `--prof` timing label `build.csr_assembly` is now `build.graph_assembly`.
  It spans more of the build than it used to, so a number compared against the
  old label would read as a regression that never happened. (#657)

### Refactor

- **Graph assembly** — `out_offsets`, `in_offsets`, `in_edge_idx`,
  `name_index`, `kind_offsets`, `kind_node_idx` and `node_flags` are derived:
  given the nodes and edges there is one correct value for each. That
  derivation lived only inside `GraphBuilder::build()`, so every test fixture
  reimplemented it by hand. `GraphAssembly::finish()` owns it now, and 48
  fixtures across 28 files declare symbols and edges instead of laying out
  index arrays. Fixtures built this way carry the indices a real graph has —
  several were previously exercising fallback paths production never takes,
  including the cypher benchmark. (#657)
- **Overlay traversal** — applying the L1 overlay (uncommitted edits) to a
  traversal was a three-step protocol written out five times across cypher and
  impact; missing a step answered from a graph nobody edited, silently.
  `MergedGraph` performs it once, behind `out_edges` / `in_edges` /
  `all_edges`. (#658, #661)
- **`impact --baseline` payload** — was a `serde_json::Value` that `ecp review`
  and `ecp dev pr-analyze` navigated by string key, the latter through its own
  hand-synced mirror structs. One typed declaration now serves both, so a
  renamed field is a compile error rather than a silent `unwrap_or("?")` in a
  risk calculation. (#659)
- **Language dispatch** — the cold-index path decided which parsers to build
  through a 37-field bool struct plus two match tables kept in sync by comment;
  a provider name with no arm was silently never constructed. Now a set of
  registry names, 141 lines lighter. The resolver's private extension list for
  fetch-shape extraction routes through the canonical table too. (#660)
- **CLI dispatch** — "does this command need a graph" was written three times
  in `main.rs`, and the first copy ended in a catch-all, so a new command that
  forgot an arm silently fell into graph loading. Two exhaustive accessors on
  `Commands` now hold it, and a new variant does not compile until it declares
  itself. (#662)

### Performance

- `MergedEdge::reason` is `#[inline]`: `run_bfs` calls it once per result node
  from the other crate, and only `release-dist` builds with LTO. (#661)
## v0.8.6 - 2026-07-24

### Refactor

- single provider registry — collapse 3 hand-synced dispatch tables (#649)
- single scalar/aggregate dispatch — unify the 3-way evaluator split (#653)
- extract build() passes 1.5-1.8 into named functions (#652)
- split five modes into impact/ submodules + fixture-based unit tests (#648)
## v0.8.5 - 2026-07-18

### Bug Fixes

- invalidate parser caches when framework tree-sitter queries change
- reject query shapes affected by the tree-sitter 0.26.11 sibling-anchor regression

### Performance

- add an end-to-end Cypher short-string projection benchmark for `compact_str` updates

## v0.8.4 - 2026-07-08

- (no user-facing changes)
## v0.8.3 - 2026-07-03

### Bug Fixes

- emit schema as Struct node so schema fields reach graph (#616)
- trust third-party tap before brew install in release smoke (#611)

### Performance

- scope --baseline pipeline to the diff's languages (#618)
## v0.8.2 - 2026-06-30

- (no user-facing changes)
## v0.8.1 - 2026-06-23

- (no user-facing changes)
## v0.8.0 - 2026-06-22

### Performance

- skip redundant rkyv re-validation + tighten contracts BM25 schema (#590)
## v0.7.1 - 2026-06-16

### Grammar

- Swift: re-vendored tree-sitter-swift 0.7.2 → 0.7.3 (#580). New syntax now
  parses cleanly (previously produced ERROR nodes that dropped the enclosing
  symbol): consume/discard operators, typed throws (`do throws(E)` /
  `func f() throws(E)`), `#if` directives inside type bodies,
  `nonisolated(unsafe)`/`nonisolated(nonsending)`, bracket-qualified nested
  type access, and double-optional `Type??` in lambda parameters.

### CI / Internal

- Make the `safe_exec` timeout tests portable to the new `windows-2025-vs2026`
  GitHub runner image (#579).
- Scope Dependabot to the root workspace; stop churn PRs against vendored
  grammar snapshots (#581).
## v0.7.0 - 2026-06-10

### Features

- pre-spawn plan query, lead pairs view, scale-audit fixes (P2-P4) (#562)
- agent_name identity bridge for agent-team peer-sync (#561)

### Bug Fixes

- audit batch — verb consolidation, --batch impact, self-heal gate, output slimming (#563)

### Performance

- L1 freshness gate — skip re-parse of unchanged dirty files (#567)

### Refactor

- simplify pass over the 0.7.0 diff — gate dedupe, Windows keys, heuristic-set lock (#568)
## v0.6.7 - 2026-06-10

### Features

- agent-dispatch tripwire — redirect structural queries to ecp (#559)
## v0.6.6 - 2026-06-10

### Features

- execute against the merged graph (root-cure 3/3) (#557)
- traverse the query-time OverlayView (root-cure 2/3) (#556)
- Fragment v2 + query-time OverlayView foundation (root-cure 1/3) (#555)
- self-flag incomplete caller sets on name-collision targets (#551)

### Bug Fixes

- untracked files enter the L1 overlay dirty set (#554)
- surface behind-HEAD staleness caveat on group verbs (#553)
- unbreak cross-repo selectors + thread staleness caveat through bm25 paths (#552)
## v0.6.5 - 2026-06-09

### Features

- Callable/Type/Data category labels + Calls-absence caveat (#548)
- EXISTS pattern + IS [NOT] NULL predicates, SQL-shape hints, exec_pattern fixes (#543)

### Bug Fixes

- classify unresolvable baseline/PR as user input (#544)

### Performance

- push WHERE prop-equality conjuncts into MATCH node patterns (#547)
- find-first DFS behind EXISTS + WillNeed readahead on graph mmap (#545)
## v0.6.4 - 2026-06-03

### Features

- **Build concurrency cap**: bound machine-wide simultaneous L2 rebuilds (`ECP_MAX_CONCURRENT_BUILDS`, env-aware default) so concurrent builds across repos can't saturate disk I/O and hang the host — most relevant on WSL2's vhdx. Gates only rebuilds; cache hits, warm-attach, and queries never queue.

### Bug Fixes

- usage_cmd aggregation fixtures used a fixed past date that aged out of the telemetry retention window, failing every PR's Test run (#534).

### Docs

- A/B-tuned the embedded `ecp` skill (`docs/skills/ecp/`) for agent usability (#532): question→verb map, anchors for `impact --literal` / `cypher`, no-fabrication rule on `found:false`. Ships via `ecp admin claude install skills`.

## v0.6.3 - 2026-06-01

### Bug Fixes

- strip Windows verbatim \?\ prefix from registry common_dir (#528)
## v0.6.2 - 2026-05-31

### Bug Fixes

- classify multi-assigned instance field as field-reassign, not uid-collision (#525)
- try next-best sibling instead of giving up on the newest (#522)
- close symbol-extraction gaps across 8 languages (#520)
- warn on unknown property name instead of silent 0 rows (#521)

### Refactor

- filter unknown cypher props before sort/dedup (#523)

### Chore

- make `ecp impact --file` the primary flag, keep `--file_path` as alias (#526)

## v0.6.1 - 2026-05-30

### Features

- SQL-in-string → code→table edges (QueriesTable) (#515)
- list-repos as narrowed alias of `ecp summary` (#495)

### Bug Fixes

- drop stale gnx name + trim product skills (#518)
- capture namespace import `import * as ns` as RawImport (#513)
- re-wire --dump-resolver + fix WarmAttach race breaking diff on fresh baselines (#488)

### Performance

- use ReloadPolicy::Manual for cold-open BM25 query (#517)

## v0.6.0 - 2026-05-29

### Features

- raise ecp adoption — reflex-first skill, daily update probe, Windows home, doctor color, slimmer help (#505)
- optional `result` caveat field for unreliable query answers (#504)

### Bug Fixes

- expand Rust crate::/self::/super:: import paths in Tier 2 (#503)
- inline node-property map filters all known properties (#502)
- gate warm-attach on sibling commit distance (#501)
## v0.5.4 - 2026-05-28

### Features

- capture class fields as Property nodes (#499)
- emit C enum constants + Go defined types as graph nodes (#498)
- clean wipe — kill daemons, drop empty shells, warn on stale backups (#494)
- user-input error classification (#493)
- backtick-quoted identifier (Neo4j-compat) (#492)
- diagnosable failures + ephemeral cwd bucket (#491)
- --clear flag to delete CLI telemetry log (#486)
- self-delete binary + rename --host to --agent + README install/uninstall (#485)

### Bug Fixes

- skip index --fix in non-git dirs to prevent OOM (#487)

### Performance

- build tantivy uid→idx map for matched subset only (#497)
## v0.5.3 - 2026-05-27

### Features

- ecp usage — CLI usage dashboard + telemetry instrumentation (#481)
- surface L1 overlay symbols so uncommitted edits are findable (#480)

### Bug Fixes

- bound the registry flock so a dead holder can't freeze the machine (#475)
- cross-repo graph loads skip version check; PathLiteral precision; schema impact_traversal (#473)
- sweep orphan .tmp + prune ghost registry entries (#474)
- npm trusted publishing (OIDC) + README + idempotent re-runs (#472)

### Performance

- parse dirty files once, build only the needed providers (#479)
