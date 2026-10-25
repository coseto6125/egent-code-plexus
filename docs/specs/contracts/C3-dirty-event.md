# C3: dirty event

Status: phase-1 contract. Types: `crates/ecp-core/src/contracts/dirty_event.rs`. Landed in the C3 behaviour slice (`96b877e8`, `91f65ef0`): the freshness gate and the build-clean check call `DirtySet::from_porcelain_z`. The area-5 tombstone slice has not landed.

Plan row (`docs/specs/2026-10-25-architecture-direction.md`, "Contracts before code"): one record per changed path: added, modified, deleted, renamed (both halves); class source, resolution config or other. Only "other" skips the per-path syscalls. Consumers: areas 1, 4, 5, 6. Area 5 owns the dirty-event flow.

The plan's Order section ships C3 with one behaviour slice: the classification of porcelain paths that replaces freshness F6. This note describes that slice (section "The C3 behaviour slice"). This contract slice does not implement it.

Every `file:line` below is at the head of branch `feat/arch-p1-contracts`.

## Defines

### Path rule

A dirty path is relative to the worktree top. It follows the path rule of C2: `/`-separated, not empty, no leading `./`, no trailing `/`, no `.` or `..` segment, no `\`. `RepoPath::new` rejects every other form. Git prints porcelain paths relative to the repository top, whatever the working directory is.

### Types

| Type | Kind | Purpose |
|---|---|---|
| `RepoPath` | value | One path in the path rule. |
| `NotRepoRelative` | error | A path that breaks the path rule. |
| `PathClass` | stored value (serde) | The class of one path: a set of two flags, `source` and `resolution_config`. The empty set is "other". |
| `PathChange` | stored enum (serde) | The state of one path against the base commit: `added`, `modified`, `deleted`. `unmerged` exists only before the reader's stat; a writer stores the confirmed change. |
| `DirtyEvent<P>` | record | One change: `Added(P)`, `Modified(P)`, `Deleted(P)`, `Renamed { from: P, to: P }`, `Unmerged(P)`. `P` is `RepoPath` before classification, `ClassifiedPath` after it. |
| `ClassifiedPath` | value | A `RepoPath` with its `PathClass`. |
| `DirtySet<P>` | collection | The dirty set of one worktree, plus the entries it could not represent. |
| `SkippedEntry`, `SkipReason` | record, enum | A path that the set cannot name: `NotUtf8`, `NotRepoRelative`, or `Directory` (`dirty_event.rs:251-266`). |
| `DuplicatePath` | error | Two events name one path. |
| `PorcelainError` | error | The bytes are not porcelain v1 `-z` output. |

### `DirtyEvent` variants

The base commit is HEAD at the time of `git status`.

| Variant | Invariant | What a reader does |
|---|---|---|
| `Added(p)` | The worktree holds `p`. Porcelain proves that HEAD does not. | Parse `p`. |
| `Modified(p)` | The worktree holds `p`. HEAD holds it. | Parse `p`. Its nodes replace the base nodes of `p`. |
| `Deleted(p)` | The worktree lacks `p`. HEAD holds it. | Tombstone the whole file: drop every base node of `p` and every base edge that starts or ends there. Never "keep base". |
| `Renamed { from, to }` | `from` as in `Deleted`, `to` as in `Added`, `from != to`, and git paired them. | Tombstone `from`, parse `to`. |
| `Unmerged(p)` | An unmerged entry (`U` in a column, `AA`, `DD`). Its letters describe index stages, not the file: a `UU` file can be deleted by hand, and a `DD` path can be recreated without staging. HEAD and the worktree are both unknown. | Stat `p` once, then `confirm(exists)`: present gives `Modified(p)`, absent gives `Deleted(p)`. Read no half before that. |

`worktree_path()` gives the half to parse (`Added`, `Modified`, `Renamed.to`). `removed_path()` gives the half to tombstone (`Deleted`, `Renamed.from`). Both return `None` for `Unmerged` until `confirm` resolves it. `halves()` gives each path with its own `PathChange`: a rename gives `(Deleted, from)`, then `(Added, to)`.

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
| An unrepresentable path is recorded, never guessed. | `skipped` holds it with `NotUtf8`, `NotRepoRelative` or `Directory`. `is_empty()` is false while `skipped` has an entry. |
| `SkipReason::Directory` marks a porcelain path that ends in `/`: git names an untracked nested repository as a directory (`PorcelainParse::note`, `dirty_event.rs:534-539`; test `test_from_porcelain_z_untracked_nested_repo_records_directory_skip`). | The freshness gate does not count it toward the caveat (`auto_ensure.rs:478`, `read_context.rs:154-162`). The build-clean check counts it as not clean, because `porcelain_clean` requires every skipped entry to be an ecp artifact (`build/orchestrator.rs:841-844`). |
| The mtime walk records what it cannot name. | `DirtySet::from_modified(paths, skipped)` (`dirty_event.rs:399`) takes the walk's paths as `Modified` events plus its skipped entries. `walked_repo_path` (`auto_ensure.rs:530-550`) records a non-UTF-8 name as `NotUtf8` and a path that breaks the path rule as `NotRepoRelative`. |
| Classification keeps order and skipped entries. | `classify(router, facts)` maps each path. |

### `DirtySet::from_porcelain_z`

The input is the stdout of `git status --porcelain -z --untracked-files=all` (format v1). The parser reads entries in order. A rename or copy entry (`R` or `C` in either column) is followed by one source segment. Each entry adds two facts to its path: what it says about HEAD, and what it says about the worktree file (present, absent or unknown).

| Status `XY` | HEAD | Worktree file |
|---|---|---|
| `??` | lacks | present |
| `!!` | no record | no record |
| unmerged (`U` in a column, `AA`, `DD`) | unknown | unknown |
| `R` or `C` in a column: destination | lacks | present, unless `Y` is `D` |
| `R` in a column: source | holds | absent |
| `C` in a column: source | no record | no record |
| `A` in `X` (other cases) | lacks | present, unless `Y` is `D` |
| other (`M`, `T`, `D`, space in `X`) | holds | present, unless `X` or `Y` is `D` |

Facts merge per path. For HEAD, "holds" beats "lacks" beats "unknown". For the worktree, "present" beats "absent" beats "unknown". `D  a` followed by `?? a` (after `git rm --cached a`) is therefore `Modified`.

| HEAD | Worktree file | Record |
|---|---|---|
| any | unknown | `Unmerged` |
| lacks | present | `Added` |
| holds or unknown | present | `Modified` |
| holds or unknown | absent | `Deleted` |
| lacks | absent | none (`AD`: added, then deleted) |

Only an unmerged entry leaves the worktree unknown.

A rename becomes `Renamed` only when its source is `Deleted` and its destination is `Added` after the merge. Otherwise its halves stay separate records: `RD` gives `Deleted(from)`, and a source recreated as untracked gives `Modified(from)` plus `Added(to)`.

`PorcelainError` cases: an entry shorter than `XY p` or without the space at byte 2 (`Malformed`); a missing final NUL (`Unterminated`); a status letter outside ` MTADRCU?!`, or `?` / `!` paired with another letter (`UnknownStatus`); a rename or copy without its source segment (`MissingSource`). Each carries the byte offset of the entry.

## Landed and remaining

Landed in the C3 behaviour slice: the rows marked "landed". The other rows describe code that this slice does not change. Before the slice, `parse_porcelain_paths` read the porcelain bytes; `DirtySet::from_porcelain_z` replaced it, and no code named `parse_porcelain_paths` remains.

### Where each fact is produced and read

| Fact | Produced now | Read now |
|---|---|---|
| Porcelain bytes | `porcelain_all` (`ecp-cli/src/git/status.rs:63`) with `STATUS_ARGS` (`:34`): `status --porcelain -z --untracked-files=all`. The plain form is `plain` (`:82`). | `git_fingerprint_shortcut` (`auto_ensure.rs:465`); `worktree_clean_and_head_matches` (`build/orchestrator.rs:812`). |
| Changed paths (landed) | `DirtySet::from_porcelain_z`. It keeps a rename source, decodes no path lossily and records a path it cannot name as a `SkippedEntry`. | `git_fingerprint_shortcut` (`auto_ensure.rs:474`); `porcelain_clean` (`orchestrator.rs:830`). |
| Artifact filter | `is_ecp_artifact` (`auto_ensure.rs:559`): one `fs::canonicalize` per path (`:564`). | Landed: it runs only on the halves left after `without_other` (`auto_ensure.rs:487`, `:498-505`); `orchestrator.rs:833-835` runs it on every event. |
| Dirty set handed on (landed) | `EnsureResult::Stale.dirty_files: Option<DirtySet<ClassifiedPath>>` (`auto_ensure.rs:137`), set at `:517`. `None` when the mtime walk decided. | `ensure_fresh_with_rebuild` (`:642`), which falls back to `collect_dirty_files` (`:1593`) and wraps the result with `DirtySet::from_modified` (`:740`). |
| Repo-relative paths (landed) | `RepoPath`, relative to the worktree top. No `strip_prefix` of an absolute path remains in the porcelain branch. | The freshness gate and the parse (`auto_ensure.rs:747-753`). |
| Freshness gate | `retain_stale_fragments` (`:1291-1356`). A path with no manifest entry stays without a stat (`:1324-1326`). A path with an entry costs one stat (`:1334`). A failed stat keeps the path (`:1349`). | `:761`. An empty result returns `Ready` (`:762-773`). |
| Pre-parse mtime | One stat per path the gate did not stat (`:784-790`). | Manifest `mtime_ns` (`apply_l1_overlay_updates`, `:1360`). |
| Parse | `reanalyze_files` (`reanalyze.rs:141-206`): one `git ls-files` per call for the shadow cascade (`:148`, `tracked_files` `:263`). Landed in P1: the provider filter is `provider_names_for_paths` (`:174-177`). Two stats per path, `exists` and `is_file` (`:193`). | `apply_l1_overlay_updates` (`auto_ensure.rs:806`). |
| Dirty manifest | `DirtyFiles` (`ecp-core/src/session/overlay.rs:9`), `DirtyEntry` (`:22`). `commit_fragment_results` inserts entries and never removes one (`ecp-cli/src/session/overlay_writer.rs:356`, `:383`). `append_dirty` (`overlay_writer.rs:586`) has test callers only. | `retain_stale_fragments`; `accepted_fragments` (`session/overlay_reader.rs:58-97`). |
| Deleted file in the view | No record. `accepted_fragments` skips an entry whose file is missing or whose mtime differs (`overlay_reader.rs:76-80`). The base nodes of the file stay visible. | `Engine::overlay_view` (`ecp-cli/src/engine.rs:244`). |
| Dirty file in the view | `OverlayView::build` (`ecp-core/src/session/view.rs:327`) takes the dirty file set from its inputs (`:333-340`). It collects base nodes only of the kinds `fragment_emits` names (`:1406`). Builder-made nodes stay visible (`:66`). | Every overlay reader. |
| View cache key | `view_cache::key` (`ecp-cli/src/session/view_cache.rs:35-48`). `inputs_hash` hashes each accepted fragment's path and bytes (`overlay_reader.rs:89-90`). | `engine.rs:255`. |
| Skipped count (landed in F1 and F6) | `unreadable_changes` counts the skipped entries except `Directory` (`read_context.rs:154-162`). | `Engine::caveat()` and `unreadable_changes_caveat()` (`engine.rs:149`, `:177`) reach every graph command through the payload `result` field. |

### What an "other" path cost before the C3 slice

An untracked `.csv` file took this route on every query before the slice. The slice removes steps 1 to 5 for it:

1. `is_ecp_artifact` canonicalized it.
2. The gate kept it: it never gets a manifest entry, because only parsed files do (`overlay_writer.rs:383`).
3. The pre-parse loop statted it.
4. `reanalyze_files` ran `git ls-files` and dropped the path at the provider filter. If a source path was also dirty, it statted the `.csv` path twice.
5. `apply_l1_overlay_updates` ran again and rewrote the HEAD sidecar.

Landed: `without_other` drops the path before any syscall (`auto_ensure.rs:487`), and an all-"other" set returns `Ready` (`:488-490`).

Freshness F6 (`docs/specs/2026-10-25-architecture-review.md:163`) measured the result: 15 ms clean, 88 ms with 12,000 untracked `.csv` files.

### Mapping to the new types

| Before the slice | Contract | Difference |
|---|---|---|
| ` M p`, `MM p`, ` T p` → `p` kept | `Modified(p)` | none |
| `?? p`, `A  p` → `p` kept | `Added(p)` | the old parse had no added/modified distinction |
| ` D p`, `D  p` → `p` kept; `reanalyze_files` skips it (`reanalyze.rs:192-196`) | `Deleted(p)` | the old parse had no tombstone |
| `R  to\0from` → `to` kept, `from` dropped (`parse_porcelain_paths`, deleted by the slice) | `Renamed { from, to }` | the old half is kept |
| `D  p` + `?? p` → `p` twice | `Modified(p)` once | one record per path |
| `AD p` → `p` kept | no record | |
| `C  to\0from` → `to` kept | `Added(to)` | none |
| `UU p`, `AA p`, `DD p` and the other unmerged pairs → `p` kept; `reanalyze_files` parses it when it exists (`reanalyze.rs:192-196`) | `Unmerged(p)` | the reader's stat decides `Modified` or `Deleted`; the old parse dropped a missing file and its base nodes stay |
| a rename source with a space at byte 2, such as `ab c.py` | the rename source | `parse_porcelain_paths` read it as an entry for `c.py` |
| a non-UTF-8 path → a lossy path that does not exist | `SkippedEntry { NotUtf8 }` | recorded, not guessed. The mtime walk records it the same way (`DirtySet::from_modified`). |
| `?? sub/` (an untracked nested repository) → a path ending in `/` | `SkippedEntry { Directory }` | recorded; the caveat leaves it out, and the build-clean check counts it as not clean |
| a path with `\` | `SkippedEntry { NotRepoRelative }` | before the slice, the manifest key replaced `\` with `/`, which names another file. Now only the Windows walk maps it (`walked_repo_path`, `auto_ensure.rs:541-545`). |
| absolute `PathBuf`, joined onto `worktree_root` | `RepoPath`, relative to the worktree top | the join belongs to the reader |
| no class | `PathClass` | new |

Values with no mapping:

| Before the slice | Why |
|---|---|
| `collect_dirty_files` output (mtime walk) | It has no change kind and sees no deletion. Landed: its producer wraps each path as `Modified` through `DirtySet::from_modified` (`auto_ensure.rs:740`), with the names it cannot read as skipped entries. |
| `EnsureResult::Stale.dirty_files: None` | "No dirty set": the mtime walk decided, or a full rebuild is due. It stays an `Option`. |
| `!!` entries | `STATUS_ARGS` has no `--ignored`, so git prints none. The parser accepts and ignores them. |

## Consumers

### Area 1: one rule set, and the incremental overlay

Reads:

- `removed_path()` of a source half as a whole-file tombstone, and `worktree_path()` of a source half as a parse input to `OverlayView::build`.
- `Added` and `Deleted` halves as candidate changes for the invalidation closure that area 5 owns. A rename gives new uids: the uid holds the path (`view.rs:372-377`, not re-verified at HEAD).

Must stop:

- Building the dirty file set only from files that have a fragment (`view.rs:333-340`). A tombstone has no fragment.
- Keeping the builder-made nodes of a tombstoned file (`view.rs:66`, `fragment_emits` `:1406`). A tombstone removes every node of the file.
- Serving base nodes for a missing file (`overlay_reader.rs:76-80`).

### Area 4: resolve an import in the caller's workspace scope

Reads: each half with `is_resolution_config()`, of any change kind, deletions included. For each one it calls C2 `invalidated_by(path)`.

Must stop: treating "has a provider" as "is a change". `Cargo.toml` and `go.mod` have no provider (`reanalyze_files` returns no graph for them, `reanalyze.rs:174-177`); C3 keeps them as resolution config.

### Area 5: one read path (owner of the dirty-event flow)

Reads: `DirtySet::from_porcelain_z` once per query, from the worktree top. Then `classify`, then `without_other` before any per-path syscall. Each remaining `Unmerged` path gets one stat and `confirm` before any other reader sees it. `skipped` goes into the payload caveat. A `PorcelainError` makes the dirty set unknown, as a failed status does (`auto_ensure.rs:474`: `.ok()?` returns the inconclusive branch). Landed in F1 and F6: `skipped` reaches every graph command through `Engine::caveat()` (`engine.rs:149`).

Must stop:

- Done in the C3 slice and F1: `parse_porcelain_paths` (deleted) with its dropped rename source, lossy decode and join onto a cwd-derived root; absolute `PathBuf`s in `EnsureResult::Stale` (now `Option<DirtySet<ClassifiedPath>>`, `auto_ensure.rs:137`). The read context roots the worktree at its top (`read_context.rs`).
- Answering a deleted or renamed-away file from the base graph (freshness F2).
- The mtime walk's silent miss of deletions (`collect_dirty_files`, `auto_ensure.rs:1593`): the answer says that deletions are not seen.

### Area 6: one routing fact

Reads: `PathClass::source` is C5's `RouteOutcome::Routed`. After area 6, the overlay runs the cold index's provider set, so "source" means "parsed".

Landed in P1 and the C3 slice: `reanalyze_files` decides from `provider_names_for_paths`, the route's provider list (`reanalyze.rs:174-177`), and the freshness gate classifies by C5 routing alone (`auto_ensure.rs:523-526`). `provider_name_for_path` no longer exists.

### Area 11: tests

Lane 0a's dirty-event fixtures (added, modified, deleted, renamed, unmerged, untracked "other" files) build on `DirtySet::from_porcelain_z` and `from_events`.

### Cross-area conflict rows that name C3

| Pair | Requirement | Status |
|---|---|---|
| 1, 5 | C3 first; a deleted file is a whole-file tombstone, never "keep base". | Satisfied by the types: `Deleted` and `Renamed.from` are `removed_path()`, and the variant docs fix the tombstone semantics. The view change is lane F's (area 5) or lane R's; C3 changes no view code. |
| 4, 5 | A config change widens the invalidation scope; C3's "resolution config" class. | Satisfied: `PathClass::resolution_config` from C2. A config path is never "other", so `without_other` never drops it. |
| 4, 6 | Config files have no provider (C5 routes a `.json` file by `PathRule::JsonContent` to the OpenAPI provider only, `PathRule::JsonContent` in `routing.rs`). | Satisfied: the class comes from C2 names and C5 routes, never from a provider. |

### C2 and C5 proposals

| Proposal | Decision |
|---|---|
| C2 open point 7: C3 classifies a "resolution config" path by calling `config_ecosystem`. | Accept. `PathClass::classify` calls `WorkspaceFacts::config_ecosystem` when it has the table. Without the table it calls `Ecosystem::classify_config`, the name half of the same function (`workspace.rs:583-591`). See open point 3. |
| C5 open point 12: a file can be config and source; the class is a set. | Accept. `PathClass` holds two flags. `tsconfig.json` is both (test `test_classify_tsconfig_json_is_source_and_config`). |

## Stored data

| Identifier | This contract slice | The C3 behaviour slice | The first slice that reads tombstones (area 5) |
|---|---|---|---|
| `GRAPH_FORMAT_VERSION` | none | none | none |
| `BUILDER_FINGERPRINT` | none | none: "source" is C5 `Routed` alone (open point 2) | none: the new manifest namespace isolates the change (below) |
| L1 fragment encoding | none | none | none: the bins keep the V5 encoding, in the new directory |
| Dirty manifest | none | none (open point 1) | a new namespace: `<session>/l1/dirty_events.json`, versioned by `DIRTY_EVENTS_FORMAT = 1`, with bins in `<session>/l1/fragments/`. `dirty_files.json` and `graph_overlay/` are no longer read. |
| View cache key | none | none | `VIEW_CACHE_FORMAT` bump, and `inputs_hash` hashes every tombstone path. Else a cached view survives a deletion. |

### Manifest isolation (the area-5 tombstone slice)

Status: landed in F2a (`fc1f663c`). The types are `DirtyEvents` and `DirtyEventEntry` (`ecp-core/src/session/overlay.rs`); the paths come from `ecp-core/src/session/layout.rs`. The header also carries `size_cap_bytes` (C5 "Cache validity"). gc removes the old namespace once its newest file is older than the session idle limit. The view reads tombstones in F2b.

A new field in today's `dirty_files.json` is not safe while an older binary of the same fingerprint can write the file. The older writer reads the whole manifest into its own `DirtyEntry`, which has no catch-all field (`ecp-core/src/session/overlay.rs:22-41`). It then writes the whole manifest back (`ecp-cli/src/session/overlay_writer.rs:371-387`). The unknown `change` and `class` fields are gone, and a new reader applies their defaults: a tombstone reads as `modified`, an "other" path reads as source. A format check that skips non-V5 entries (`overlay_reader.rs:75-77`) protects only the reader, not the data.

| Option | Cost | Decision |
|---|---|---|
| Bump `BUILDER_FINGERPRINT` | Every repo pays a full L2 rebuild on upgrade (`auto_ensure.rs:219-232`), for a change to session state only. Two binaries of one fingerprint (two dev builds of one release) still share the file. | Reject. |
| A new namespace: `<session>/l1/dirty_events.json` and `<session>/l1/fragments/` | Older binaries never open it: today's writer, reader and gate use `dirty_files.json` and `graph_overlay/` only (`overlay_writer.rs:371`, `overlay_reader.rs:63`, `auto_ensure.rs:1311`, `:1319`), and today's `admin gc` sweeps only `graph_overlay/` (`admin/gc.rs:544`, `:564`), so it cannot delete a bin that only the new manifest names. An older binary's peer watcher sees a new session as clean (`peer/watcher.rs:165`, `commands/peers.rs:238`). | Accept. |

Rules of the new namespace:

- `dirty_events.json` holds `format: u32`. The reader accepts only `DIRTY_EVENTS_FORMAT` (1). Any other value, or no value, makes every dirty file a re-parse. That is the gate's existing "any doubt re-parses" rule (`auto_ensure.rs:1277-1286`).
- `change` and `class` are required fields with no serde default. There is no older data in the namespace to default for.
- The manifest stores confirmed changes only: never `unmerged`.
- A tombstone entry has a path, `change: deleted`, a class and no bin.
- The new binary ignores `dirty_files.json` and `graph_overlay/`. Its `admin gc` sweeps both namespaces and retires the old one once it is idle.
- Session promotion (`promotion.rs`) and the peer watcher move to the new namespace in the same slice.

Tests of that slice, written before the run with their pass criterion:

| Test | File | Pass criterion |
|---|---|---|
| `test_dirty_events_old_writer_manifest_beside_new_namespace_keeps_tombstone` | `crates/ecp-cli/tests/stored_formats_prev_release.rs` | A session holds the 0.16.4 `dirty_files.json` and `graph_overlay/` of the existing fixture (`:50`, `tests/fixtures/stored-by-0.16.4`), rewritten after the new binary wrote a `lib.rs` tombstone to `l1/`. `ecp find` of a `lib.rs` symbol answers `found: false`; the old manifest's `lib.rs` entry is never served. |
| `test_dirty_events_missing_format_reparses_every_dirty_file` | `crates/ecp-cli/tests/overlay_view_cache.rs` | `REANALYZE_CALL_COUNT` covers every dirty file; no entry of that manifest is served. |
| `test_sweep_stale_fragments_bin_named_only_by_new_namespace_survives` | `crates/ecp-cli/src/admin/gc.rs` tests | The bin is still on disk after the sweep. |
| `test_overlay_view_tombstone_changes_view_cache_key` (deferred to F2b: the view does not read tombstones yet) | `crates/ecp-cli/tests/overlay_view_cache.rs` | Two views, before and after `rm`, have different keys. |

## The C3 behaviour slice

Status: landed in `96b877e8` (classification) and `91f65ef0` (C5 routing alone). Goal: the freshness gate classifies each porcelain path once and drops "other" events before any per-path syscall. This replaces freshness F6. The absolute latency thresholds now bind (plan line 67).

Needs: C2 and C5 committed and reviewed (C2 `294320e3`, C5 `00bb6b82`).

### What changed, and where

| File | Change |
|---|---|
| `ecp-cli/src/auto_ensure.rs` `git_fingerprint_shortcut` (`:444-521`) | `DirtySet::from_porcelain_z` replaced `parse_porcelain_paths` (`:474`). A `PorcelainError` returns `None`, the inconclusive branch. `classify_dirty` classifies with `FileRouter::PATH_ONLY` and `facts = None` (`:523-526`). The source term is C5 `Routed` alone (open point 2). `without_other` runs first (`:487`). `is_ecp_artifact` runs on the remaining worktree halves (`:498-505`). No event left gives `Ready` (`:488-490`). A count of skipped entries, `Directory` excluded, goes to stderr (`:475-484`). |
| `auto_ensure.rs` `EnsureResult::Stale` (`:137`) | `dirty_files` is `Option<DirtySet<ClassifiedPath>>`. |
| `auto_ensure.rs` `ensure_fresh_with_rebuild` (`:642`) | It takes the repo-relative paths from the set (`:747-753`) and has no `strip_prefix`. The gate and `reanalyze_files` receive every **source** half: worktree halves and removed halves. Config-only halves stay in the set for areas 4 and 5, and do not reach `reanalyze_files`. An `Unmerged` source path reaches them; the existence check in `reanalyze_files` (`reanalyze.rs:193`) is its one stat. The slice writes no tombstone for it. The mtime-walk paths become `Modified` events through `DirtySet::from_modified` and the same classification (`:740`). |
| `ecp-cli/src/build/orchestrator.rs` `worktree_clean_and_head_matches` (`:788`) and `porcelain_clean` (`:829`) | They use `from_porcelain_z` with no classification. Any event or skipped entry that is not an ecp artifact means "not clean", and so does a `PorcelainError`. The build reads more than source (config, ignore files), so "other" must not count as clean here. |
| `auto_ensure.rs` | `parse_porcelain_paths` and its two unit tests are deleted. The contract tests cover the parse. |

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
| `test_ensure_index_conflicted_file_deleted_by_hand_returns_unmerged` | A `UU` file removed with `rm` gives `Unmerged(p)`, not `Modified(p)`. |
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
| `test_porcelain_clean_malformed_status_returns_false` (`orchestrator.rs:2113`) | A malformed status is "not clean". It calls `porcelain_clean` on fixed bytes, because the real status cannot be malformed. |

Latency: lane 0a's dirty scenario in `scripts/benchmark/benchmark_ecp.py`, plus 12,000 untracked `.csv` files on the 10k-file corpus of freshness F6.

The first pass criterion failed, and the criterion changed after review. The record:

| Item | Value |
|---|---|
| First criterion, fixed before the run | The later-query median is within 10% of the clean query. |
| Run | This repo cloned locally (2,393 tracked files), `ecp find parse_sha`, 30 interleaved runs, a separate `ECP_HOME` per binary, median. |
| 0.16.4 | clean 12.3 ms, 12k `.csv` 184.2 ms |
| C3 slice | clean 15.9 ms, 12k `.csv` 28.4 ms (ratio 1.79: fail) |
| Cause | `git status --porcelain -z -uall` lists 12,000 paths: 17.8 ms, against 9.6 ms for `-unormal`. The classification makes no syscall. |
| Clean regression | +3.6 ms from the filter-driver guard's `git config` read (FU-2026-10-25-b39d68734257). |

Current criterion: on the named corpus, the later-query median is under 30 ms, and ecp does no per-path filesystem operation for an "other" path. Re-verify with a fixed binary SHA, a separate `ECP_HOME` per binary kept across runs, and the first query (cache build) reported apart from the later queries. Track the clean regression and the filter-guard cost apart from this criterion.

Rejected alternatives:

| Option | Failure |
|---|---|
| `-unormal` | A new untracked directory prints as `dir/` only, so its `.py` files never reach the overlay. A walk of our own must reproduce the ignore rules and brings back the per-path cost. |
| `core.untrackedCache` | Already on: `private_index.rs:339` reuses the private index copy while the index and config identity hold. The cache does not remove the cost of printing 12,000 paths. |
| `-uno`, or ignoring `.csv` | `-uno` misses a new source file. Ignoring `.csv` changes the user's ignore semantics. |

## Open points

| # | Decision | Options | Recommendation |
|---|---|---|---|
| 1 | When the dirty manifest gains the change and the class | (a) the C3 behaviour slice writes tombstone and config entries now; (b) the first area-5 slice that reads tombstones adds them, with the reader | (b), in the new namespace of "Manifest isolation". F6 needs no stored change: each query classifies its own porcelain before the gate. Data that no reader reads cannot be tested for what the reader gets (repo `CLAUDE.md`, stored data). Writer and reader ship together, and the namespace keeps an older writer from stripping the fields. The plan's Stored formats row changes to match (replacement text in the C3 fix-round return). |
| 2 | "Source" before area 6. Decided: (a), in `91f65ef0` and P1. | (a) C5 `Routed` only; (b) C5 `Routed` or the legacy `provider_name_for_path` is `Some` | Before P1, (b) was the plan: the two differed on the template paths only, because C5 excludes `__brick__` and `{{…}}` and the pre-P1 overlay parsed them. P1 made the overlay route through C5, so the second term had no reader and `provider_name_for_path` was deleted. `DirtySet::classify` uses C5 alone (`auto_ensure.rs:523-526`). |
| 3 | Workspace table at the gate | (a) `facts = None`: names only; (b) map `graph.bin` before the gate and pass `WorkspaceFacts` | (a) only as a transition, in the C3 behaviour slice, while no consumer reads the config class. Gap: a config path that only a scope lists and whose extension C5 does not route is "other" (test `test_classify_extends_target_without_facts_is_other`). A missing `extends` target such as `build/x.custom` that appears is then dropped before any consumer sees it. So (b) is required before area 4 ships: from that slice on, classification reads the workspace facts (test `test_classify_missing_extends_target_with_facts_is_config`). |
| 4 | Unmerged entries | (a) `Modified`, and `DD` as `Deleted`; (b) `Unmerged`, existence unknown, confirmed by one stat | (b). Git's unmerged `XY` describes the index stages, not the current file: a `UU` file can be deleted by hand after the conflict, and a `DD` path can be recreated and not staged. (a) would parse a missing file, or tombstone a file that exists. The reader stats the path once and calls `DirtyEvent::confirm`. An unmerged "other" path is dropped by `without_other` with no stat. Tests: `test_from_porcelain_z_unmerged_codes_return_unmerged`, `test_dirty_event_confirm_unmerged_file_present_returns_modified`, `test_dirty_event_confirm_unmerged_file_deleted_by_hand_returns_deleted`. |
| 5 | Paths git prints that the path rule rejects (non-UTF-8; `\` on Unix) | lossy path, or skipped entry | Skipped entry, reported in the caveat. Done: `SkipReason::{NotUtf8, NotRepoRelative}` for both producers (`from_porcelain_z`, `from_modified`), and `Directory` for an untracked nested repository, which the caveat leaves out and the build-clean check counts as not clean. The count reaches every graph command through `Engine::caveat()` (`engine.rs:149`). The lossy path names no file. |
| 6 | Index-scope files: `.gitignore`, `.ecpignore`, `[index] max_file_bytes` | a fourth class, or "other" | "Other". An edit to them changes the cold index's file set; the overlay ignores them today too. Area 3's snapshot owns them. |
| 7 | The base of the delta | porcelain compares the worktree with HEAD; the overlay's base graph can be a warm-attached sibling or behind HEAD (`auto_ensure.rs:720`) | Area 5's read context applies tombstones only when the base graph is HEAD's, else it adds a caveat. Area 3 owns the build handle. |
| 8 | Per-query cost of a deleted source file | keep (each query: a failed canonicalize, a failed stat, `git ls-files`), or gate it | Keep in the C3 slice: the cost is not "other" cost. The area-5 tombstone slice records the tombstone and lets the gate skip it. |
| 9 | The path rule in two places | Settled: `DirtySet` calls C2's `pub(crate) is_normal_path` (`workspace.rs:675`). | Done. |
