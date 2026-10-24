# C3: dirty event

Status: phase-1 contract. Types: `crates/ecp-core/src/contracts/dirty_event.rs`. No production code calls them yet.

Plan row (`docs/specs/2026-10-25-architecture-direction.md`, "Contracts before code"): one record per changed path: added, modified, deleted, renamed (both halves); class source, resolution config or other. Only "other" skips the per-path syscalls. Consumers: areas 1, 4, 5, 6. Area 5 owns the dirty-event flow.

The plan's Order section ships C3 with one behaviour slice: the classification of porcelain paths that replaces freshness F6. This note describes that slice (section "The C3 behaviour slice"). This contract slice does not implement it.

Every `file:line` below was read on branch `feat/arch-p1-contracts` at `1c69c974`.

## Defines

### Path rule

A dirty path is relative to the worktree top. It follows the path rule of C2: `/`-separated, not empty, no leading `./`, no trailing `/`, no `.` or `..` segment, no `\`. `RepoPath::new` rejects every other form. Git prints porcelain paths relative to the repository top, whatever the working directory is.

### Types

| Type | Kind | Purpose |
|---|---|---|
| `RepoPath` | value | One path in the path rule. |
| `NotRepoRelative` | error | A path that breaks the path rule. |
| `PathClass` | stored value (serde) | The class of one path: a set of two flags, `source` and `resolution_config`. The empty set is "other". |
| `PathChange` | stored enum (serde) | The state of one path against the base commit: `added`, `modified`, `deleted`. |
| `DirtyEvent<P>` | record | One change: `Added(P)`, `Modified(P)`, `Deleted(P)`, `Renamed { from: P, to: P }`. `P` is `RepoPath` before classification, `ClassifiedPath` after it. |
| `ClassifiedPath` | value | A `RepoPath` with its `PathClass`. |
| `DirtySet<P>` | collection | The dirty set of one worktree, plus the entries it could not represent. |
| `SkippedEntry`, `SkipReason` | record, enum | A porcelain path that is not UTF-8 or breaks the path rule. |
| `DuplicatePath` | error | Two events name one path. |
| `PorcelainError` | error | The bytes are not porcelain v1 `-z` output. |

### `DirtyEvent` variants

The base commit is HEAD at the time of `git status`.

| Variant | Invariant | What a reader does |
|---|---|---|
| `Added(p)` | The worktree holds `p`. Porcelain proves that HEAD does not. | Parse `p`. |
| `Modified(p)` | The worktree holds `p`. HEAD holds it, or porcelain cannot tell (an unmerged entry). | Parse `p`. Its nodes replace the base nodes of `p`, if any. |
| `Deleted(p)` | The worktree lacks `p`. HEAD holds it, or porcelain cannot tell. | Tombstone the whole file: drop every base node of `p` and every base edge that starts or ends there. Never "keep base". |
| `Renamed { from, to }` | `from` as in `Deleted`, `to` as in `Added`, `from != to`, and git paired them. | Tombstone `from`, parse `to`. |

`worktree_path()` gives the half to parse (`Added`, `Modified`, `Renamed.to`). `removed_path()` gives the half to tombstone (`Deleted`, `Renamed.from`). `halves()` gives each path with its own `PathChange`: a rename gives `(Deleted, from)`, then `(Added, to)`.

### `PathClass`

| Flag | Set when | Source of truth |
|---|---|---|
| `source` | C5 routes the path: `FileRouter::route(path)` is `RouteOutcome::Routed`. An `Excluded` or `Unrouted` path is not source. | C5 |
| `resolution_config` | C2 names the path. With a table: `WorkspaceFacts::config_ecosystem(path)`. Without one: `Ecosystem::classify_config(file_name)`. | C2 |

Rules:

- The class is a set. `tsconfig.json`, `package.json` and `jest.config.ts` are source and resolution config.
- The class reads the path only. A deleted path gets the same class as a present one, with no syscall.
- A config path that a scope tried to read and did not find (C2 `ConfigPresence::Missing`) is resolution config, so its appearance is a config change (test `test_classify_missing_extends_target_with_facts_is_config`).
- `DirtyEvent::class()` is the union of the halves' classes. `is_other()` is true only when every half is "other".
- Only an event whose halves are all "other" may skip a per-path syscall (`DirtySet::without_other`). A rename with one "other" half stays, and the reader checks each half.
- The stored form is `{"source": bool, "resolution_config": bool}`. `PathClass::default()` is "other".

### `DirtySet`

| Rule | How it holds |
|---|---|
| One record per path: a path is in one event, in one half. | `from_events` rejects a repeat with `DuplicatePath`. `from_porcelain_z` merges entries per path. |
| Events are sorted by the path of their first half. | Both constructors sort. |
| An unrepresentable path is recorded, never guessed. | `skipped` holds it with `NotUtf8` or `NotRepoRelative`. `is_empty()` is false while `skipped` has an entry. |
| Classification keeps order and skipped entries. | `classify(router, facts)` maps each path. |

### `DirtySet::from_porcelain_z`

The input is the stdout of `git status --porcelain -z --untracked-files=all` (format v1). The parser reads entries in order. A rename or copy entry (`R` or `C` in either column) is followed by one source segment. Each entry adds two facts to its path: what it says about HEAD, and whether it proves that the worktree holds the path.

| Status `XY` | HEAD | Worktree holds the path |
|---|---|---|
| `??` | lacks | yes |
| `!!` | no record | no record |
| unmerged (`U` in a column, `AA`, `DD`) | unknown | yes, except `DD` |
| `R` or `C` in a column: destination | lacks | yes, unless `Y` is `D` |
| `R` in a column: source | holds | no |
| `C` in a column: source | no record | no record |
| `A` in `X` (other cases) | lacks | yes, unless `Y` is `D` |
| other (`M`, `T`, `D`, space in `X`) | holds | yes, unless `X` or `Y` is `D` |

Facts merge per path. "Holds" beats "lacks" beats "unknown", and any entry that proves presence wins. `D  a` followed by `?? a` (after `git rm --cached a`) is therefore `Modified`.

| HEAD | Worktree holds | Record |
|---|---|---|
| lacks | yes | `Added` |
| holds or unknown | yes | `Modified` |
| holds or unknown | no | `Deleted` |
| lacks | no | none (`AD`: added, then deleted) |

A rename becomes `Renamed` only when its source is `Deleted` and its destination is `Added` after the merge. Otherwise its halves stay separate records: `RD` gives `Deleted(from)`, and a source recreated as untracked gives `Modified(from)` plus `Added(to)`.

`PorcelainError` cases: an entry shorter than `XY p` or without the space at byte 2 (`Malformed`); a missing final NUL (`Unterminated`); a status letter outside ` MTADRCU?!`, or `?` / `!` paired with another letter (`UnknownStatus`); a rename or copy without its source segment (`MissingSource`). Each carries the byte offset of the entry.

## Today

### Where each fact is produced and read

| Fact | Produced today | Read today |
|---|---|---|
| Porcelain bytes | `porcelain_all` (`ecp-cli/src/git/status.rs:63-65`) with `STATUS_ARGS` (`:34`): `status --porcelain -z --untracked-files=all`. The plain form is `plain` (`:82-84`). | `git_fingerprint_shortcut` (`auto_ensure.rs:426`); `worktree_clean_and_head_matches` (`build/orchestrator.rs:811`). |
| Changed paths | `parse_porcelain_paths` (`auto_ensure.rs:500-511`). It keeps each segment with a space at byte 2 (`:506`), decodes it lossily (`:507`) and joins it onto `worktree_root` (`:509`). | `git_fingerprint_shortcut` (`:441`); `worktree_clean_and_head_matches` (`orchestrator.rs:818-820`). |
| Artifact filter | `is_ecp_artifact` (`auto_ensure.rs:466-488`): one `fs::canonicalize` per path (`:471`). | `:443`; `orchestrator.rs:820`. |
| Dirty set handed on | `EnsureResult::Stale.dirty_files: Option<Vec<PathBuf>>`, absolute paths (`auto_ensure.rs:121-125`), set at `:452-456`. `None` when the mtime walk decided. | `ensure_fresh_with_rebuild` (`:614-623`), which falls back to `collect_dirty_files` (`:1465-1505`). |
| Repo-relative paths | `strip_prefix(worktree_root)` (`:627-631`). | The freshness gate and the parse. |
| Freshness gate | `retain_stale_fragments` (`:1163-1230`). A path with no manifest entry stays without a stat (`:1196-1198`). A path with an entry costs one stat (`:1206`). A failed stat keeps the path (`:1221`). | `:639`. An empty result returns `Ready` (`:640-644`). |
| Pre-parse mtime | One stat per path the gate did not stat (`:655-661`). | Manifest `mtime_ns` (`:1280-1288`). |
| Parse | `reanalyze_files` (`reanalyze.rs:150-216`): one `git ls-files` per call for the shadow cascade (`:157`, `:273-289`); a provider filter (`:186-192`); two stats per path, `exists` and `is_file` (`:198-209`). | `apply_l1_overlay_updates` (`auto_ensure.rs:675-681`). |
| Dirty manifest | `DirtyFiles`, `DirtyEntry` (`ecp-core/src/session/overlay.rs:8-41`). `commit_fragment_results` inserts entries and never removes one (`ecp-cli/src/session/overlay_writer.rs:373-376`). `append_dirty` (`overlay_writer.rs:575-604`) has test callers only. | `retain_stale_fragments`; `accepted_fragments` (`session/overlay_reader.rs:58-97`). |
| Deleted file in the view | No record. `accepted_fragments` skips an entry whose file is missing or whose mtime differs (`overlay_reader.rs:78-81`). The base nodes of the file stay visible. | `Engine::overlay_view` (`ecp-cli/src/engine.rs:216-240`). |
| Dirty file in the view | `OverlayView::build` (`ecp-core/src/session/view.rs:326`) takes the dirty file set from its inputs (`:332-340`). It collects base nodes only of the kinds `fragment_emits` names (`:342-357`, `:1297`), and suppresses the ones the parse did not re-emit (`:409-413`). Builder-made nodes stay visible (`:20-26`). | Every overlay reader. |
| View cache key | `view_cache::key` (`ecp-cli/src/session/view_cache.rs:35-48`). `inputs_hash` hashes each accepted fragment's path and bytes (`overlay_reader.rs:89-90`). | `engine.rs:227-230`. |

### What an "other" path costs today

An untracked `.csv` file takes this route on every query:

1. `is_ecp_artifact` canonicalizes it (`auto_ensure.rs:443`, `:471`).
2. The gate keeps it: it never gets a manifest entry, because only parsed files do (`overlay_writer.rs:373-376`).
3. The pre-parse loop stats it (`auto_ensure.rs:655-661`).
4. `reanalyze_files` runs `git ls-files` (`reanalyze.rs:157`). It drops the path at the provider filter (`:186-189`). If a source path is also dirty, it stats the `.csv` path twice (`:203`).
5. `apply_l1_overlay_updates` runs again and rewrites the HEAD sidecar (`auto_ensure.rs:675-686`).

Freshness F6 (`docs/specs/2026-10-25-architecture-review.md:163`) measured the result: 15 ms clean, 88 ms with 12,000 untracked `.csv` files.

### Mapping to the new types

| Today | Contract | Difference |
|---|---|---|
| ` M p`, `MM p`, ` T p` → `p` kept | `Modified(p)` | none |
| `?? p`, `A  p` → `p` kept | `Added(p)` | today has no added/modified distinction |
| ` D p`, `D  p` → `p` kept; `reanalyze_files` skips it (`reanalyze.rs:203`) | `Deleted(p)` | today has no tombstone |
| `R  to\0from` → `to` kept, `from` dropped (`auto_ensure.rs:506`) | `Renamed { from, to }` | the old half is kept |
| `D  p` + `?? p` → `p` twice | `Modified(p)` once | one record per path |
| `AD p` → `p` kept | no record | |
| `C  to\0from` → `to` kept | `Added(to)` | none |
| a rename source with a space at byte 2, such as `ab c.py` | the rename source | today reads it as an entry for `c.py` (`auto_ensure.rs:506-509`) |
| a non-UTF-8 path → a lossy path that does not exist | `SkippedEntry { NotUtf8 }` | recorded, not guessed |
| a path with `\` | `SkippedEntry { NotRepoRelative }` | today the manifest key replaces `\` with `/` (`auto_ensure.rs:630`), which names another file |
| absolute `PathBuf`, joined onto `worktree_root` | `RepoPath`, relative to the worktree top | the join belongs to the reader |
| no class | `PathClass` | new |

Values with no mapping:

| Today | Why |
|---|---|
| `collect_dirty_files` output (mtime walk) | It has no change kind and sees no deletion. Its producer wraps each path as `Modified` through `DirtySet::from_events`. |
| `EnsureResult::Stale.dirty_files: None` | "No dirty set": the mtime walk decided, or a full rebuild is due. It stays an `Option`. |
| `!!` entries | `STATUS_ARGS` has no `--ignored`, so git prints none. The parser accepts and ignores them. |

## Consumers

### Area 1: one rule set, and the incremental overlay

Reads:

- `removed_path()` of a source half as a whole-file tombstone, and `worktree_path()` of a source half as a parse input to `OverlayView::build`.
- `Added` and `Deleted` halves as candidate changes for the invalidation closure that area 5 owns. A rename gives new uids: the uid holds the path (`view.rs:372-377`).

Must stop:

- Building the dirty file set only from files that have a fragment (`view.rs:332-340`). A tombstone has no fragment.
- Keeping the builder-made nodes of a tombstoned file (`view.rs:20-26`, `fragment_emits` `:1297`). A tombstone removes every node of the file.
- Serving base nodes for a missing file (`overlay_reader.rs:78-81`).

### Area 4: resolve an import in the caller's workspace scope

Reads: each half with `is_resolution_config()`, of any change kind, deletions included. For each one it calls C2 `invalidated_by(path)`.

Must stop: treating "has a provider" as "is a change". `Cargo.toml` and `go.mod` have no provider (`reanalyze.rs:186-192`); C3 keeps them as resolution config.

### Area 5: one read path (owner of the dirty-event flow)

Reads: `DirtySet::from_porcelain_z` once per query, from the worktree top. Then `classify`, then `without_other` before any per-path syscall. `skipped` goes into the payload caveat. A `PorcelainError` makes the dirty set unknown, as a failed status does today (`auto_ensure.rs:427-429`).

Must stop:

- `parse_porcelain_paths` (`auto_ensure.rs:500-511`): the dropped rename source (`:506`), the lossy decode (`:507`), the join onto a cwd-derived root (`:509`, freshness F1).
- Passing absolute `PathBuf`s in `EnsureResult::Stale` (`:124`).
- Answering a deleted or renamed-away file from the base graph (freshness F2).
- The mtime walk's silent miss of deletions (`:1465-1505`): the answer says that deletions are not seen.

### Area 6: one routing fact

Reads: `PathClass::source` is C5's `RouteOutcome::Routed`. After area 6, the overlay runs the cold index's provider set, so "source" means "parsed".

Must stop: `reanalyze_files` deciding which paths count from `provider_name_for_path` (`reanalyze.rs:186-189`).

### Area 11: tests

Lane 0a's dirty-event fixtures (added, modified, deleted, renamed, unmerged, untracked "other" files) build on `DirtySet::from_porcelain_z` and `from_events`.

### Cross-area conflict rows that name C3

| Pair | Requirement | Status |
|---|---|---|
| 1, 5 | C3 first; a deleted file is a whole-file tombstone, never "keep base". | Satisfied by the types: `Deleted` and `Renamed.from` are `removed_path()`, and the variant docs fix the tombstone semantics. The view change is lane F's (area 5) or lane R's; C3 changes no view code. |
| 4, 5 | A config change widens the invalidation scope; C3's "resolution config" class. | Satisfied: `PathClass::resolution_config` from C2. A config path is never "other", so `without_other` never drops it. |
| 4, 6 | Config files have no provider (`ecp-core/src/analyzer/pipeline.rs:146` routes only OpenAPI JSON). | Satisfied: the class comes from C2 names and C5 routes, never from a provider. |

### C2 and C5 proposals

| Proposal | Decision |
|---|---|
| C2 open point 7: C3 classifies a "resolution config" path by calling `config_ecosystem`. | Accept. `PathClass::classify` calls `WorkspaceFacts::config_ecosystem` when it has the table. Without the table it calls `Ecosystem::classify_config`, the name half of the same function (`workspace.rs:583-591`). See open point 3. |
| C5 open point 12: a file can be config and source; the class is a set. | Accept. `PathClass` holds two flags. `tsconfig.json` is both (test `test_classify_tsconfig_json_is_source_and_config`). |

## Stored data

| Identifier | This contract slice | The C3 behaviour slice | The first slice that reads tombstones (area 5) |
|---|---|---|---|
| `GRAPH_FORMAT_VERSION` | none | none | none |
| `BUILDER_FINGERPRINT` | none | none, when "source" keeps today's provider term (open point 2) | none |
| L1 fragment encoding | none | none | none |
| Dirty manifest | none | none (open point 1) | `DirtyEntry` gains `change: PathChange` (serde default `modified`) and `class: PathClass` (serde default source). A tombstone entry has no fragment bin. |
| View cache key | none | none | `inputs_hash` must hash every tombstone path. Else a cached view survives a deletion. |

Old manifests: `DirtyFiles` has no `deny_unknown_fields` (`overlay.rs:8-19`), so an older binary of the same fingerprint ignores the new fields. A tombstone entry needs a `format` that today's reader skips (`overlay_reader.rs:75-77` skips every format but V5). The area-5 slice proves the migration with a manifest that 0.16.4 wrote: `crates/ecp-cli/tests/stored_formats_prev_release.rs` already loads one (`DIRTY_FILES` `:50`, fixture `tests/fixtures/stored-by-0.16.4`).

## The C3 behaviour slice

Goal: the freshness gate classifies each porcelain path once and drops "other" events before any per-path syscall. This replaces freshness F6. After it lands, the absolute latency thresholds bind (plan line 67).

Needs: C2 and C5 committed and reviewed (C2 `294320e3`, C5 `00bb6b82`).

### What changes, and where

| File | Change |
|---|---|
| `ecp-cli/src/auto_ensure.rs` `git_fingerprint_shortcut` (`:405-457`) | Replace `parse_porcelain_paths` with `DirtySet::from_porcelain_z`. On `PorcelainError`, return `None` (the inconclusive branch). Classify with `FileRouter` and `facts = None`. Add the legacy source term (open point 2). Call `without_other`. Run `is_ecp_artifact` only on the remaining worktree halves. No event left gives `Ready`, as `:445-447` does. Print a count of `skipped` entries on stderr. |
| `auto_ensure.rs` `EnsureResult::Stale` (`:121-125`) | `dirty_files` becomes `Option<DirtySet<ClassifiedPath>>`. |
| `auto_ensure.rs` `ensure_fresh_with_rebuild` (`:614-631`) | Take the repo-relative paths from the set; drop the `strip_prefix`. The gate and `reanalyze_files` receive every **source** half: worktree halves and removed halves, as today's ` D` entries already reach them. Config-only halves stay in the set for areas 4 and 5, and do not reach `reanalyze_files`, which drops them today anyway (`reanalyze.rs:186-192`). Wrap the mtime-walk paths (`:617-621`) as `Modified` events through `from_events` and classify them the same way. |
| `ecp-cli/src/build/orchestrator.rs` `worktree_clean_and_head_matches` (`:787-821`) | Use `from_porcelain_z` with no classification. Any event or skipped entry that is not an ecp artifact means "not clean". A `PorcelainError` means "not clean". The build reads more than source (config, ignore files), so "other" must not count as clean here. |
| `auto_ensure.rs:500-511`, `:1748-1767` | Delete `parse_porcelain_paths` and its two unit tests. The contract tests cover the parse. |

Unchanged: the gate, the manifest, `reanalyze_files`, the overlay writer and reader, the view and its cache key. The slice changes no stored data and no parse output.

### Behaviour that changes

| Case | Before | After |
|---|---|---|
| Only "other" files dirty | `Stale`; every query runs the refresh path (gate, `git ls-files`, session meta, sidecar write) and parses nothing | `Ready` |
| "Other" and source files dirty | every path is canonicalized, statted and passed to `reanalyze_files` | only the source paths |
| A rename `R  to\0from` | `to` only | both halves; `from` also enters the shadow cascade (`reanalyze.rs:157-176`) |
| A rename source with a space at byte 2 | a spurious path | the source |
| Malformed porcelain | every segment with a space at byte 2 counts | the inconclusive branch: the mtime walk |

### Tests

In `crates/ecp-cli/tests/auto_ensure.rs`, with a git repo and a HEAD sidecar, as `shortcut_returns_stale_when_tree_dirty_even_with_matching_sidecar` (`:186`) builds them:

| Test | Asserts |
|---|---|
| `test_ensure_index_only_untracked_csv_returns_ready` | 50 untracked `.csv` files give `Ready`. |
| `test_ensure_index_untracked_csv_and_py_returns_stale_with_py_only` | The set holds the `.py` file only. |
| `test_ensure_index_modified_cargo_toml_returns_stale_with_config_path` | `Cargo.toml` stays, class resolution config: the 4-5 row. |
| `test_ensure_index_modified_tsconfig_returns_source_and_config` | The class is a set. |
| `test_ensure_index_git_mv_returns_renamed_both_halves` | `Renamed { from, to }`. |
| `test_ensure_index_deleted_source_returns_deleted_event` | `Deleted(p)`, with no stat of `p`. |
| `test_ensure_index_head_sha_sidecar_other_sha_graph_at_head_reports_untracked_stale` (`:279`) | Rewritten for the new `dirty_files` type; same assertion. |

In the `auto_ensure.rs` unit tests, where `test_counters` is reachable:

| Test | Asserts |
|---|---|
| `test_ensure_fresh_only_untracked_csv_skips_reanalyze` | `REANALYZE_CALL_COUNT` (`:672`) does not move. |
| `test_ensure_fresh_csv_beside_py_reanalyzes_py_only` | `reanalyze_files` receives one path. |

In `crates/ecp-cli/src/build/orchestrator.rs` tests:

| Test | Asserts |
|---|---|
| `test_worktree_clean_and_head_matches_untracked_csv_returns_false` | An "other" file still makes the tree not clean for the build. |
| `test_worktree_clean_and_head_matches_malformed_status_returns_false` | Only reachable with a fake status; else note it as infeasible. |

Latency: lane 0a's dirty scenario in `scripts/benchmark/benchmark_ecp.py`, plus 12,000 untracked `.csv` files on the 10k-file corpus of freshness F6. Pass criterion before the run: the later-query median is within 10% of the clean query.

## Open points

| # | Decision | Options | Recommendation |
|---|---|---|---|
| 1 | When the dirty manifest gains the change and the class | (a) the C3 behaviour slice writes tombstone and config entries now; (b) the first area-5 slice that reads tombstones adds them, with the reader | (b). The plan's Stored formats row puts it in C3's slice, but F6 needs no stored change: each query classifies its own porcelain before the gate. Data that no reader reads cannot be tested for what the reader gets (repo `CLAUDE.md`, stored data). The serde types are ready. |
| 2 | "Source" before area 6 | (a) C5 `Routed` only; (b) C5 `Routed` or `AnalyzerPipeline::provider_name_for_path` is `Some` | (b) in the C3 slice; area 6 deletes the second term. The two differ on the template paths only (C5 note, value mapping): C5 excludes `__brick__` and `{{…}}`, and today's overlay parses them. (b) keeps the overlay input set, so no fingerprint bump. |
| 3 | Workspace table at the gate | (a) `facts = None`: names only; (b) map `graph.bin` before the gate and pass `WorkspaceFacts` | (a) until B's first round stores the table and area 5's read context maps the graph before the gate. Gap: a config file that only a scope lists and whose extension C5 does not route (test `test_classify_extends_target_without_facts_is_other`) is "other". Most extends targets are `.json`, which C5 routes, so they stay source. |
| 4 | Unmerged entries | `Modified`, or a separate conflict kind | `Modified`: the worktree holds the file, and a reader parses it. `DD` is `Deleted`. |
| 5 | Paths git prints that the path rule rejects (non-UTF-8; `\` on Unix) | lossy path, or skipped entry | Skipped entry, reported in the caveat. The lossy path names no file today. |
| 6 | Index-scope files: `.gitignore`, `.ecpignore`, `[index] max_file_bytes` | a fourth class, or "other" | "Other". An edit to them changes the cold index's file set; the overlay ignores them today too. Area 3's snapshot owns them. |
| 7 | The base of the delta | porcelain compares the worktree with HEAD; the overlay's base graph can be a warm-attached sibling or behind HEAD (`auto_ensure.rs:720`) | Area 5's read context applies tombstones only when the base graph is HEAD's, else it adds a caveat. Area 3 owns the build handle. |
| 8 | Per-query cost of a deleted source file | keep (each query: a failed canonicalize, a failed stat, `git ls-files`), or gate it | Keep in the C3 slice: the cost is not "other" cost. The area-5 tombstone slice records the tombstone and lets the gate skip it. |
| 9 | The path rule in two places | Settled: `DirtySet` calls C2's `pub(crate) is_normal_path` (`workspace.rs:675`). | Done. |
