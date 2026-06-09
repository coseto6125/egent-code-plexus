# Changelog

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
