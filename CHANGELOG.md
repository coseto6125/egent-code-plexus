# Changelog

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
