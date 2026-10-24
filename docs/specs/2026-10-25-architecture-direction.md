# Architecture direction: structural changes

Status: direction, not started. No code from these programs is on `main` (`8868d659`): `GRAPH_FORMAT_VERSION` is 15, and `lexical_parents` and the resolution outcome type do not exist.

This page says where ecp's structure should go and how each target differs from today. Areas 1 to 4 came from the resolution and identity work of PR #861. Areas 5 to 11 came from a whole-repo review on 2026-10-25, one reviewer per subsystem. Its evidence, per subsystem and per finding, is in [`2026-10-25-architecture-review.md`](2026-10-25-architecture-review.md), with the small local fixes that need no restructuring. Two areas have full designs, kept beside this page:

| Area | Design | Size (realistic) |
|---|---|---|
| 1. One resolution rule set for the index and the overlay | [`2026-10-23-shared-resolution-design.md`](2026-10-23-shared-resolution-design.md) (design B) | ~7,000 changed lines |
| 2. Split `owner_class` into a type owner and a lexical parent | [`2026-10-23-owner-split-design.md`](2026-10-23-owner-split-design.md) (design A) | ~3,200 changed lines |
| 3. One ranking rule for "the current graph of a commit" | none yet; direction below | not estimated |
| 4. Resolve an import in the caller's workspace scope | none yet; direction below | not estimated |
| 5. One read path for every graph command | none yet; direction below | not estimated |
| 6. One routing fact: path, language, provider | none yet; direction below | not estimated |
| 7. Post-parse edges carry their endpoints and their evidence | none yet; direction below | not estimated |
| 8. One route and contract record | none yet; direction below | not estimated |
| 9. Cypher: answer exactly or refuse | none yet; direction below | not estimated |
| 10. `parse_file`: a shared driver and a per-language spec | none yet; direction below | not estimated |
| 11. Test architecture | none yet; direction below | not estimated |

[`2026-10-23-architecture-designs-crosscheck.md`](2026-10-23-architecture-designs-crosscheck.md) is the adversarial review of A and B, with the interleaved slice order and the effort estimates above.

On 2026-10-25 codex reviewed this page as a design peer. Its positions are folded into the sections below. Where this page departs from them, the section says why.

Every area changes what an agent receives from ecp. Each section states that consequence, because the agent is the user (see the repo `CLAUDE.md`). A reproduction that names a command ran on `main` at `8868d659` or with the installed 0.16.4 binary; the review page says which, and marks what was only read in code.

## Priority check

The repo ranks three priorities: per-query latency, answers that do not mislead the agent, and signal density. Every area exists for the second priority: each one removes an answer that is wrong, missing, or stale with no marker. Latency is a constraint on every area, not a priority any area trades away:

- A slice may move work from query time to index time. Cold index stays under 5 s per 25k files. This bound is provisional: the only profile covers 12,737 files (vscode: parse 3.16 s, global build 0.92 s, write 0.18 s), and it binds once a 25k-file run is measured.
- Clean-query latency of `find`, `impact`, `inspect` and `cypher` stays within benchmark noise. `scripts/benchmark/benchmark_ecp.py` runs before and after each slice. Measure newly supported dirty-file refreshes separately, and set their budgets before implementation.
- Where the correct answer costs query time, the area names the cost and its bound.

| Area | Answer it corrects | Query-time cost | Index-time cost | Signal density |
|---|---|---|---|---|
| 1 | index and overlay bind one call to one target | the overlay runs the shared ladder per dirty file; measure per slice | none | none |
| 2 | owner and parent of a nested symbol | none | not measured; storage: 8 B per scoped node | display names stay usable as targets |
| 3 | the build ecp reads is the build it means | none; the lock is taken at build time | one lock per build | none |
| 4 | an alias binds inside its own package | none; facts are built at index and overlay time | one config read per scope | none |
| 5 | every command sees uncommitted, deleted and renamed files, and marks a stale graph | commands that skip the overlay today pay the L1 refresh, cached per session (see §5 Cost) | none | one caveat field per stale answer |
| 6 | edits in 16 more languages reach the overlay | a parse per dirty file of those languages | none | none |
| 7 | an edge starts at the symbol that holds the code | none | one index per raw record; name lookups go away | none |
| 8 | route verb, full path and contracts | none | one record per route | duplicate route rows go away |
| 9 | a Cypher query returns the correct rows or an error | one check at parse time | none | an error replaces wrong rows |
| 10 | capabilities reach every language evenly | none | expected lower: one query pass per file instead of several; measure first | none |
| 11 | none at run time; it proves the other ten | none | none | none |

### Latency gates

Measured on vscode on 2026-10-25, one run each, with one session id, using the installed 0.16.4 binary and the default `ecp find` mode:

| Query | Today | Gate |
|---|---|---|
| `ecp find`, clean tree | 164 to 170 ms (the 12,338 untracked entries are stat'ed on every query) | under 30 ms |
| first `ecp find` after one edit | 363 ms | at most 363 ms |
| later `ecp find`, same dirty state | 164 ms | within 10% of the clean query |

These single runs are the starting targets. Lane 0a turns them into a protocol and fixes the final thresholds and the regression tolerance before any gate binds:

- Pin the corpus commit, the release build of the slice, the command and its find mode, and the edit (one exported function appended to `src/vs/base/common/strings.ts`).
- Record the machine, the exact commands, both binary revisions and the cache preparation.
- Each first-after-edit sample starts with a new session id, so its L1 state is fresh; the later-query samples reuse that session. Measure the no-session case once more with all five session variables cleared (`session/resolver.rs:19-23`).
- Discard one warm-up run, take 10 paired runs (baseline binary, slice binary), and compare medians against the declared tolerance.
- Add the dirty-file scenario to `scripts/benchmark/benchmark_ecp.py`; today it measures BM25 `find` on a clean tree only.

One rule applies to every slice: no regression beyond the declared tolerance against the previous slice. The final thresholds bind only after the C3 classification slice lands (phase 1), because no earlier slice removes the per-path syscalls that cost today's 164 ms.

## Certainty

A defect that is certain does not make its target sufficient. This table separates the two. codex reviewed the plan on 2026-10-25 and rated each area; the ratings below follow that review.

| Area | Defect | Target sufficient? | Proof of done |
|---|---|---|---|
| 1 | certain | no: a shared ladder does not supply missing facts, such as the Rust crate table | per call site: target identity, edge kind, exact confidence, edge multiplicity, for every stop and fallback row |
| 2 | certain | no: the identity of a trait impl, a signature and a sibling block must be defined first | swapped `Display`/`Debug` impls never pair wrongly; two blocks in one function stay distinct or report an ambiguity |
| 3 | certain | partly: old dirs can still tie, and `scan_cached` can miss a publish in one tick | an injected clock step, two refs at one SHA, and a crash before and after the rename: reader, waiter and `gc` select one build |
| 4 | certain | no design yet | one repo with several packages, several config roots and calls across packages |
| 5 | certain | partly: one read context does not refresh routes, path literals or `Defines` of a dirty file | every graph command, from the root and a subdirectory, on dirty, deleted and renamed fixtures |
| 6 | certain | partly: one routing definition must also hold path rules and the language after parse | cold, dirty and baseline runs use one provider set and one size cap |
| 7 | certain | partly: a raw index fixes the source node, not a wrong supertype or a file-wide name set | per edge: source, target and reason on same-name methods, trait declaration and impl, same-name base classes |
| 8 | certain | partly: prefix composition, handler binding and endpoint dedup need their own rules | `@PostMapping("/get-token")` is `POST`; framework prefixes compose; one endpoint gives one node |
| 9 | certain | yes, for the refusals | each listed query shape returns the correct rows or a named error |
| 10 | certain duplication | not shown: one driver must keep the phases that collect facts first and decide later | raw records, uids and edge multisets are identical before and after a pure refactor |
| 11 | certain | yes | an injected duplicate edge, wrong parent, wrong confidence, failed delete or missing provider fails the harness every time |

## Reading the designs

Designs A and B were written on 2026-10-23 against `ff75103a`, a commit of PR #861 before its squash merge (`9ff296a9`). Fetch it with `git fetch origin pull/861/head`; `ff75103a`, `829ee2fe` and `e6f56b8e` are its ancestors. PRs #861, #863 and #864 have since changed `resolver.rs`, `view.rs` and the parsers, so every `file:line` and every count in A and B is stale. Before a slice starts, re-run the inventory commands in A §1.1 and B §1.1, and B's pin script (§7), on the current `main`.

Both designs mention "Codex" and a "coordinator": the designs were reviewed while PR #861 was in progress, and those words name that review process. They carry no meaning for the code.

## 1. One resolution rule set for the index and the overlay

**Today.** A call site is resolved to its callee by a ladder of tiers: Lexical, SameFile, ImportScoped, QualifierScoped, ModuleTree, the receiver ladder, HeritageScoped, wildcard, then Global / NearestCopy, then three call-level steps (the Kotlin extension retry, the untyped-member post-filter and the constructor fallback). The ladder exists twice:

- **Index:** `crates/ecp-analyzer/src/resolution/resolver.rs` (`resolve_call` and the tier functions), fed by `resolution/builder.rs`. It runs when a commit is indexed.
- **Overlay:** `crates/ecp-core/src/session/view.rs` (`resolve_callee` and the build closure). It runs when a file in the worktree differs from the indexed commit.

At `ff75103a`, B counted 33 rules (a historical inventory, not re-measured on `main`): 22 implemented twice, 4 that diverge between the sides, and 67 leaf helpers already shared under `crates/ecp-core/src/analyzer/`. Both sides also return one value for two meanings: the index returns an empty `Vec`, the overlay returns `None`, and each can mean "no edge, stop" or "unknown, try the next tier".

**Consequence for the agent.** The same question gets a different answer before and after `git commit`. A fix in one ladder and not the other is invisible until someone edits a file. Known instance (B §1.4, confirmed at `ff75103a`, not re-checked since): while a Rust file is dirty, the overlay cannot resolve `other_crate::path::f()` into another workspace crate, so `ecp impact` drops those callers.

**Target.** One ladder in `ecp-core`, run by both sides. Each side supplies its facts through a trait (the index from `graph.bin` and the symbol table; the overlay from base plus dirty fragments). Each tier returns an explicit `Resolution` (B §3.1) instead of an empty value:

| `Resolution` | Meaning |
|---|---|
| `Bound(Hit)` | an edge, with its tier and confidence |
| `NoEdge(reason)` | no edge. Most reasons stop the ladder only, and the call-level fallbacks still run. Three reasons (`PackageMissing`, `DeclaredAttribute`, `ChainBroken`) also stop the fallbacks. |
| `Unknown` | this rule does not apply; the next tier runs |
| `Shadowed` | a lexical scope declares the name but none in reach binds it. Skip the name tiers; the import tiers and the call-level steps still run. |

| | Today | Target |
|---|---|---|
| Ladder code | two copies (`resolver.rs`, `view.rs`) | one, in `ecp-core` |
| "No edge" vs "not decided" | the same empty value | two named states |
| A new language rule | two edits | one edit |
| Parity check | per-bug tests | one 14-language parity harness, built before the first refactor slice |
| Stored data | — | refactor slices change nothing. B3b adds `Calls` edges for some chained calls. B7b adds a workspace crate table to `graph.bin` (format bump) so the overlay resolves cross-crate Rust calls (B §4.1). |

**Risk.** B8 rewrites the entry of every call edge for all 14 languages on both sides. The cross-check names it the riskiest slice.

**First round.** Commit to the shared ladder as the end state, not to all ~7,000 lines now. The first round is smaller:

1. **The parity harness.** It compares target identity, edge kind and confidence per call site, and it lists edges that must not exist. An edge count passes when both sides are wrong in the same way. Cases: a dirty file with no change, a deleted file, a renamed file, a cross-file import, and every stop and fallback row of B §3.2.
2. **`Resolution` on both sides, with one stop-and-fallback decision.** Today the index runs the constructor fallback when its result is empty (`resolver.rs:827`), and the overlay decides inside a `hit.or_else` closure (`view.rs:738`). A slice that turns `Resolution` back into an `Option` or an empty `Vec` inside a tier adds a wrapper and fixes nothing.
3. **The Rust cross-crate gap, as its own slice.** The overlay lacks the facts: `RustModules` (`view.rs:4234`) holds the graph, the dirty files and the roots, and no crate table. A shared ladder does not supply those facts. This slice is the Rust subset of contract C2: it stores the crate table in `graph.bin`, which is the persistence half of B7b, moved into the first round with its format change. B7b keeps the half that moves the module-tree rule into the shared ladder.

B9 and the R30 helper cleanup stay out of the first round. After each slice, decide whether the next one still pays. Watch every method of the facts trait: a method that hides a pick policy, such as `global_pick`, leaves two rule sets behind one ladder.

### Beyond calls: the incremental overlay

Area 1 shares the rules for call edges. Two kinds of answer stay stale in a dirty view even after it lands:

- The edges that the builder and the post-process passes make for a dirty file stay as indexed (`session/view.rs:60-70`): `Overrides`, `Imports`, `UsesPathLiteral`, `QueriesTable`, `Defines`.
- A clean file is never resolved again (same comment). A clean caller keeps its edge when a new same-name candidate in another file makes the call ambiguous, and it gains no edge when a candidate goes away.

GitNexus and codegraph avoid this with one pipeline and one store that they rewrite on change. ecp keeps its two layers: L2 is shared by every worktree and session at a SHA, so a session's edits must stay in L1. The target is therefore one pipeline that writes L1, and each global pass runs in one of three modes:

| Mode | Passes and generated nodes | Work per refresh |
|---|---|---|
| per file | raw extraction of every pass; `File` nodes; `EntryPoint` nodes of one file's own declarations and their `References` (`builder.rs:2073-2090`); path literals; function meta; scope `Defines`; enum containment; transaction scopes; route detection inside one file; `SchemaField` and `EventTopic` nodes with their `HasProperty` and publish or subscribe edges | the dirty files only |
| incremental | call resolution; `Extends` and `Implements` (`builder.rs:2559-2583`); `Accesses` from type annotations (`:2660-2675`); `ReadsField` and the non-call `References` to names, enum variants and fields (`:2688-2765`); `Imports`; `Overrides`; class membership; decorator targets (`decorates_edges.rs:105-123`); SQL table targets (`sql_table_edges.rs:30-36`); route prefix composition and handler binding; controller `EntryPoint` nodes that routes in another file create (`pass1_95_controller_actions`, `builder.rs:1603-1758`); `Fetches`, which a route added or renamed in another file rebinds (`builder.rs:1922-1971`) | the dirty files, plus the clean files whose candidates, exports, heritage, return types, tables, routes or scope config changed |
| index-time, with a caveat | communities, processes, and the name-paired heuristic edges: schema-field mirrors, event-topic mirrors, saga pairs | none; the answer names the pass as index-time |

Shared `Annotation` nodes (`decorates_edges.rs:133-155`) are deduplicated across files: such a node lives while one file still references it.

Every output of `GraphBuilder::build`, from pass 1 to the post-process passes (`resolution/builder.rs:313` to `:829`), and every generated node kind has one row. A pass added later gets a row in the same change. Lane F owns the integration; areas 1, 7 and 8 supply the facts each row replays.

The incremental passes read the global tables from L2 through the facts adapters of design B. They never rebuild them. The invalidation closure (which clean files to resolve again) is owned by area 5, and areas 1 and 4 supply the dependency facts. Where the closure is not bounded, the answer carries a caveat that names the stale edge kinds.

The incremental overlay needs areas 4, 7 and 8 for the facts it replays. Until each lands, the overlay keeps that edge kind index-time and says so in the caveat. L1 fragments carry symbols, imports and export aliases only today (`session/overlay_writer.rs:119-123`), so the overlay needs a new fragment encoding for the raw records it replays.

**Proof of done.** For every output in the per-file and incremental rows, the node and edge multisets of the dirty view equal those of a fresh build of the same tree, in these cases: a candidate added in another file, a candidate deleted, an export changed, a supertype changed, a return type changed, a table definition changed, a route added or renamed, a field renamed, and a scope config changed. Each case keeps the caller's bytes unchanged. The index-time outputs are listed apart, and the test asserts their caveat instead of their value. A temporary exclusion, such as an edge kind waiting for area 4, 7 or 8, names its owner and the slice that removes it.

**Measured on 2026-10-25** (`ECP_PROF=1 ecp admin index --force`, one run each):

| Stage | This repo (1,934 files, 37,664 nodes) | vscode (12,737 files, 491,992 nodes) |
|---|---|---|
| parse, all files | 0.33 s | 3.16 s |
| global build | 0.12 s | 0.92 s, of which register 0.235, community 0.167, imports and calls 0.156, class membership 0.111 |
| write `graph.bin` | 0.02 s | 0.18 s |

A refresh that runs the global build again costs about 0.9 s on vscode. That is the outcome to avoid; the incremental mode exists to avoid it. `ecp-cli/src/incremental.rs` (662 lines) has no production caller, and its import key ignores the alias (`incremental.rs:273`), so it is not a base to build on.

## 2. Split `owner_class` into a type owner and a lexical parent

**Today.** `owner_class` (a string on every node) carries five meanings (A §1.2):

| Id | Meaning | Examples |
|---|---|---|
| M1 | declaring type | Method, Constructor, Property, class Const, EnumVariant, nested type |
| M2 | namespace or module | Function, Const and types under `mod` / `namespace` |
| M3 | lexical scope | nested Function, Rust closure node, Rust fn-local Const / Macro, Go `:=` Variable, Python / JS non-callables in a function |
| M4 | uid tag only | Rust `Impl` (trait text), Rust associated `Typedef`, EntryPoint uid |
| M5 | display / FQN | every node, read back from `graph.bin` |

M3 is a string built by separate AST walks, for example `enclosing_function_path` in `rust/receiver_types.rs` for the callee and `rust_lexical_scopes` in `same_file_pick.rs` for the caller. The two strings must match, and they match by convention only.

**Consequence for the agent.** At `ff75103a`:

- Python `class A: def m(self): def h()` gives `h` the owner `A`. The graph then holds a fabricated `A -HasMethod-> h`.
- A local function inside `outer` and a top-level function of the same name get the same uid, and one of them is tombstoned, so `find` and `impact` see one node where the source has two. A names Kotlin, Swift, C# and Dart, and marks the Kotlin, Swift and Dart grammar captures as still to verify (A §5.2).
- The two Rust walks drifted apart once already (R-F2 in A).

**Target.** A Function whose innermost enclosing container is itself a callable gets a parent link instead of an M3 string: the index of that callable node. A class or module between the two breaks the link, and the function stays unscoped. Languages opt in one by one (A §5.2), and non-callable M3 nodes (local class, Variable, Const) keep their `owner_class` until a later slice. One pure function, `lexical_parents`, derives the links from a file's `(kind, span)` list. The parser, the index build, the overlay view, the overlay reader and incremental mode all call that one function. A scoped node's uid hashes its parent's uid. Unscoped nodes keep `uid::compute(kind, path, owner_class, name)`, and a later overload keeps `overload(first, ordinal)`.

| | Today | Target |
|---|---|---|
| Lexical scope of a nested function | a string, built by 5 writers | a node index, built by 1 function |
| Nested-function uid | can collide with a same-named outer function | distinct: hashes the parent uid |
| `graph.bin` | v15 | the version after B's crate-table version (see Stored formats in Order), with one sparse sorted table of parent links |
| Old data | — | a graph of the release before A rebuilds on the format bump; the previous-release fixture is that release's layout, not v15; overlay fragments of another builder are skipped and re-parsed; old cache dirs age out through gc (A §5.4) |
| Display (`Foo.validate`) | from `owner_class` | M1/M2 nodes unchanged; a scoped node prints through its parent, e.g. `A::fmt` for `pad` inside `impl Display for A { fn fmt }` |

**Risk.** Slice A-C combines a uid change, a stored-data migration and a semantic change of the Rust lexical tier. Its test must read a session written by the previous binary (repo `CLAUDE.md`, "What counts as having tested a change to stored data").

**Open identity problems.** A parent-hashed uid is only as stable as the parent's identity. Two cases break A as written:

- **Swapped impls.** A type has `impl Display` and `impl Debug`, each with a `fmt`. The second `fmt` gets an overload ordinal in encounter order (`view.rs:379`), and the overlay finds the base node to replace by uid (`view.rs:388`). Swap the two impls, and the nested functions of each `fmt` swap uids. The overlay then attaches base edges to the wrong node. The parent identity must carry the trait, or another semantic key, before a child uid hashes it.
- **Sibling blocks.** One function holds two blocks, and each block declares its own `h`. `scoped(parent, kind, name)` (A §4.1) gives both the same input. Until the facts tell blocks apart, mark such a node ambiguous. It is not a duplicate in one scope.

Each output surface needs its own contract:

| Surface | Contract |
|---|---|
| uid | equal across rebuilds of the same source and settings. The first slice may change scoped uids; a parent rename may change them too. |
| overlay pairing | pair a dirty node with a base node only on a unique identity. An ordinal that proves nothing pairs nothing. |
| `ecp diff` (`SymbolKey`, `diff/symbols.rs`) | today the key is owner, name and kind, and the first of a collision wins. The display label is not the identity; the trait or signature separates two parents, and an ordinal separates what remains. |
| `impact --target Foo.bar` (`commands/symbol_id.rs`) | a real member of `Foo` resolves as today. A nested callable needs a parent-aware target. Two matches with one display name report the ambiguity and never pick the first. |
| display | keep the trait of a Rust impl: `A::fmt` alone loses which `fmt`, and an agent cannot reuse it as a target. |

Stage A this way: A-A first, one `assign_file` for the full build, incremental mode, the overlay view and the overlay reader. Then the parent identity, with the two cases above as tests. Then A-B, A-C and A-D. The gate for old sessions lands with the first slice that changes a uid.

**Also in this area** (whole-repo review, 2026-10-25):

- Two non-method declarations with one identity, such as a function and its `#[cfg]` twin, become a tombstone with an empty name. This repo's graph holds 15 `Function` tombstones, and `ecp find with_windows_retry` returns `fs_safe.rs:21` only; the twin at `:43` is hidden.
- A `Service` and a `Deployment` with the same `metadata.name` in one YAML file share an identity.
- One owner has two spellings. A path literal's owner is `Walk<'s, 't>` while its function's owner is `Walk`. Sixteen rows in this repo have differing owner spellings, including `Walk<'s, 't>` versus `Walk`. The identity rule must state which spelling is canonical.

## 3. One ranking rule for "the current graph of a commit"

This area has no design yet. The facts below are from `main` at `8868d659`, and the target is a direction to design against.

**Today.** Several builds of one SHA can exist side by side, under one or more prefixes: a bare dir (`branch_main__<sha>`, `commit__<sha>`) and generation dirs (`…<sha>.gen.<ms>.<pid>.<n>`). For a lookup by SHA, the current build comes from these sources:

| Source | Written | Read by |
|---|---|---|
| Dir name: SHA plus an optional generation `(timestamp_ms, pid, counter)` | `publish_dir_for` (`build/orchestrator.rs:363`), at the rename | `CommitIndex::scan` (`commit_lookup.rs:88`), which `wait_for_completion` (`orchestrator.rs:816`) and the pre-build attach use; `sweep_generations_keeping` (`admin/gc.rs:621`, order at 686). A bare name (`None`) ranks below every generation. |
| mtime of `meta.json`, else `graph.bin`, else the dir | the filesystem | `CommitIndex::scan` only, when two candidates have equal generations (`commit_lookup.rs:91-98`, `167-173`). `gc` has no tie-break. |
| `meta.json` (`CommitBuildMeta`): `sha`, `built_at`, `builder_fingerprint` | inside `building/`, **before** the rename (`orchestrator.rs:197`) | the pre-build check `attach_if_fingerprint_matches`. Its contents rank nothing; only its mtime breaks ties. |
| Sidecars beside `graph.bin`: `graph.bin.head_sha`, `graph.bin.builder_fingerprint`, `graph.bin.compatible_version` | **after** the rename (`orchestrator.rs:269-277`); `.compatible_version` on a detached thread | `git_fingerprint_shortcut` (`auto_ensure.rs:409`) reads `.head_sha`; the freshness and attach checks read the others |
| `graph.bin.worktree` | back-filled from `meta.json` on the first freshness check that lacks it (`auto_ensure.rs:370-395`) | the worktree-root check |

A lookup across SHAs is a separate path: `find_latest_by_mtime` and `find_all_by_mtime_desc` (`commit_lookup.rs:217`, `236`; warm attach at `auto_ensure.rs:840`) sort by `graph.bin` mtime.

The build lock is `<dir>.building/.build.lock`. It is released before the rename (`orchestrator.rs:220`), because Windows refuses to rename a directory that holds an open handle.

**Consequence for the agent.** When these sources disagree, ecp answers from a graph other than the one it just built, and nothing in the output says so. Two such defects reached a release:

| Defect | Shipped in | Fixed in |
|---|---|---|
| `admin index` wrote `.head_sha` on a thread nothing joined. The process could exit first, the freshness check fell back to mtime, and edits made before the build stayed invisible. | 0.16.2 and earlier | 0.16.3 (#863) |
| A forced rebuild published under the bare name. Readers rank a bare name below every generation, so the older generation won, and `gc` deleted the new build. | 0.16.3 | 0.16.4 (#864) |

Still open (FU-2026-10-24-266b8622508e): the lock is released before the rename, and a new generation's timestamp is not compared with the newest existing generation, so a clock step can rank a new build below an old one.

**Target.** Every reader ranks same-SHA dirs by one key, the dir name. When every builder on the machine takes the new lock, a builder can only publish a name above every existing one. An old binary that builds at the same time is outside this guarantee (last row of the state table).

- **A lock per repo and SHA, held from the name choice through the rename.** The commits dir is per repo (`~/.ecp/<repo>/commits/`, `graph_path.rs:47`), so `<commits dir>/<sha>.build.lock` is keyed by repo and SHA. Today's lock `<dir>.building/.build.lock` is keyed by the ref-derived name, and `dirname_picker.rs:12` puts the branch in that name: two refs at one SHA take two locks. Outside `.building/`, Windows can rename while the lock stays held.
- **A strictly rising name.** Under the lock, the builder sets `timestamp_ms = max(now, newest same-SHA timestamp + 1)`. `has_same_sha_dir` (`orchestrator.rs:390`) already lists those dirs. A clock step no longer reorders builds.
- **One candidate reader and one comparator.** Two `.gen` names of one SHA can no longer tie, and the lock stops a second bare dir, so the mtime tie-break in `CommitIndex::scan` never decides for new data. `CommitIndex`, `wait_for_completion` and `gc` share the reader and the comparator. `gc` takes the SHA lock and checks again before it deletes.
- **Build-time sidecars inside `building/`.** `.builder_fingerprint` and `.compatible_version` are written before the rename, so they publish with `graph.bin`. A derived sidecar that is missing means "check again", never "compatible". It is rebuilt from the build's own data, never from the running binary.
- **A stated contract for `.head_sha`.** It records that one worktree confirmed one build at one HEAD. `git_fingerprint_shortcut` accepts a match of either the sidecar or the dir name with HEAD, and then trusts a clean `git status` (`auto_ensure.rs:418-426`). So the confirmation must name the build dir it confirms, and a failed rewrite must void it. A failed rewrite must not fall back to the mtime check, which can miss edits made before the build.

**Alternative not taken.** This page's first draft, and codex, put a `generation` counter in `meta.json` and ranked by it. The counter adds a second key beside the name. It needs `meta.json` v2 and an explicit rule for v1 dirs: mapping every v1 dir to 0 ties a bare dir with a `.gen` dir, where today the `.gen` dir wins. An old binary that runs beside the new one keeps ranking by name, so the two binaries disagree on the current build. A rising name keeps every format, and an old binary ranks new dirs the same way as the new binary.

| | Today | Target |
|---|---|---|
| "Current build of SHA X" | name generation, then mtime | name generation; new data cannot tie |
| Generation order | `(timestamp_ms, pid, counter)`; the clock decides | strictly rising per SHA, chosen under the lock |
| Lock | per dir name, released before the rename | per repo and SHA, held across the rename |
| Build-time sidecars | written after the rename, one on a detached thread | inside `building/`, published by the rename |
| Stored formats | — | unchanged: `graph.bin`, `meta.json`, dir names |
| Old dirs | — | no rule: old names sort by the same key, and every new name sorts above them |

The target must keep these results:

| State | Result |
|---|---|
| Crash before the rename | the old build stays current; a staging dir is never ranked |
| Crash after the rename | the new build is current; a retry under the lock finds it and does not publish again |
| Two builders, one SHA, two refs | one lock; the second finds the first build inside the lock |
| Windows, an open handle | the rename fails and the old build stays current; a failed `gc` delete waits for the next sweep |
| The clock steps back | names still rise |
| Dirs written by an old binary | ranked as today |
| An old and a new binary build at once | the old binary takes no new lock, so the pair can race as today |

**Questions the design must answer.**

1. `scan_cached` (`commit_lookup.rs:123-137`) reuses its scan while the commits dir mtime is unchanged. Each publish renames into that dir and updates the mtime, but a filesystem with coarse timestamps can hide a second publish in the same tick. State the granularity this needs, or add a second check.
2. Decide whether the cross-SHA lookup by `graph.bin` mtime stays as it is.
3. State the lock order between a builder and `gc`.
4. Decide whether the mixed-version window is allowed. An old binary that builds beside a new one can publish a name below the new build. If the window is not allowed, state how a rollout closes it.

**Also in this area: one source snapshot per commit.** A clean worktree is walked in place, and a dirty one goes through `git archive` (`build/orchestrator.rs:139-146`). The two apply different ignore rules. A tracked file that `.gitignore` matches is skipped in place but archived, and an `export-ignore` path is walked in place but not archived. The first builder's tree state then decides what the slot holds for every worktree at that SHA. The reviewer reproduced the archive policy with git and approximated the worktree walk with ripgrep, not with `ecp admin index`. ecp and ripgrep use the `ignore` crate. Target: one snapshot rule that does not depend on the builder's tree state. Related: `admin index` skips the rebuild when a slot exists (`commands/admin/index.rs:376-394`), while `build_l2` also checks the builder fingerprint, so after an upgrade the manual command reports "exists" where `auto_ensure` would rebuild.

## 4. Resolve an import in the caller's workspace scope

This area has no design yet. The facts below are from `main` at `8868d659`.

**Today.** `admin index` calls `parse_configs(src_root)` once (`commands/admin/index.rs:296`) and hands one `PathAliases` to the graph builder. `parse_configs` reads `tsconfig.json`, `package.json`, `go.mod`, `Cargo.toml`, `pyproject.toml` and `jsconfig.json` from the repo root only (`config_parser.rs:12-108`). It walks the tree for `.csproj`, `composer.json` and Swift packages, but merges every result into the same list. `PathAliases` is one list of pattern-to-replacement pairs (`path_aliases.rs:31`), and `expand` takes the specifier alone, with no caller (`path_aliases.rs:59`).

**Consequence for the agent.** In a monorepo, two packages can each define `@/*`, or each have their own module root. ecp applies one repo-wide reading to both, so a call through the alias binds to the wrong file or to none. Reproduced with ecp 0.16.4 on a repo with `packages/a` and `packages/b`. Each package has its own `tsconfig.json` with `@/*` → `src/*`, a `src/util.ts` that exports `helper`, and a `main.ts` that calls `helper` through `@/util`. With no root `tsconfig.json`, ecp writes no `IMPORTS` edge, and both calls reach their own package's file only through the path-proximity fallback, at confidence 0.6. With a root `tsconfig.json` that maps `@/*` → `src/*` and a root `src/util.ts`, both calls bind to the root file at confidence 0.95. `ecp impact --target helper --file packages/a/src/util.ts` then reports 0 callers. Areas 1 to 3 do not change this: two ladders that agree resolve the same wrong file.

**Target.** The index records workspace facts: the resolution scope of each file, the root of that scope, its effective configuration and its dependency map. A resolver looks up the caller's scope first. The overlay reads the same facts, and a changed configuration file marks its scope stale. The facts are built at index and overlay time, never walked at query time. B7b's Rust crate table is one instance of these facts, not a replacement for them.

| | Today | Target |
|---|---|---|
| Alias table | one per repo, from root files | one per scope, found by walking the tree |
| `expand` input | the specifier | the specifier and the caller's scope |
| Parity axis | 14 languages | 14 languages, plus one repo with several packages, several config roots and calls across packages |

**Also in this area.** Two more import resolvers read their own facts:

- `ecp flow` drops the level of a relative import. In `pkg/a/sub/b.py`, `from ..util import f` binds to the sibling `pkg/a/sub/util.py`, with 0 boundaries reported (`flow/module.rs:21-40`, reproduced). Flow resolves Python module names relative to the importing file, without Python absolute-import search semantics.
- The post-process `Imports` pass (`post_process/imports_edges.rs:238-397`) guesses by a basename or directory name that is unique in the repo, and emits those guesses at confidence 0.9, like exact matches. It holds a third Rust module-path implementation beside `ecp-core/src/analyzer/rust_paths.rs`.

Target: both read the workspace facts of this area, and a guessed match carries its own reason and a confidence below 0.7.

## 5. One read path for every graph command

**Today.** Each command decides on its own how it reads the graph:

| Step | Today | Reproduction |
|---|---|---|
| Name to node | seven resolvers, one per command family | `ecp find Engine.load` returns `found:false`. `ecp rename base_visible … --dry-run` reports no occurrences for a method at `merged.rs:136`. `ecp inspect load --kind method` still reports an ambiguity and suggests `--kind`. `ecp impact --target load --kind method` merges rows from 4 unrelated files into one blast radius. |
| Ambiguity | `rename` picks every match | `ecp rename parse parse_config` with `a.py:parse` and `b.py:parse` rewrites both files, with no ambiguity error |
| Overlay | attached in `main.rs` for some commands; `load_ensured` engines (bm25 `find`, `group`, `peers plan`) never attach it | with an uncommitted `beta_dirty`, `find` finds it and `rename beta_dirty … --dry-run` reports "No occurrences"; `pattern --callers-of alpha` scans 1 file and misses an untracked caller |
| Dirty set | `git status --porcelain` paths are relative to the repo top, but `auto_ensure.rs:504-515` joins them onto the cwd | after `def bar()` is added to `pkg/sub/a.py`, `ecp find bar --repo .` from `pkg/sub` returns `found:false`; from the root it returns `found:true` |
| Deleted and renamed files | no tombstone; the old half of a rename is dropped (`auto_ensure.rs:510`) | after `rm other/b.py`, `ecp find baz` returns `other/b.py` |
| Stale marker | `Engine::caveat()` (`engine.rs:144`) reaches 6 of about 15 graph commands; `routes`, `rename`, `pattern`, `processes` and `review` print none | on a warm-attached graph, `ecp rename zed zed2 --dry-run` reports `No occurrences` with no caveat (reviewer repro) |
| MCP | `ecp-mcp/src/spawn.rs:15-33` returns stdout only, so a caveat printed on stderr never reaches an MCP client | code read |
| Name index | a failed background tantivy build leaves no index, and `find` answers by substring for the life of that commit, with no marker | code read |

**Consequence for the agent.** One question gets different answers by command and by working directory. The worst case is `rename`: a command that writes files acts on a graph that lacks the symbol, or on two targets the agent did not choose.

**Target.** One module turns the cwd or `--repo`, the target text and its filters into a read context:

- the worktree top, found once through `git_cache::git_layout`;
- the graph, with the overlay attached;
- the dirty set, with deleted paths and both halves of a rename;
- one target resolver that returns one node or a typed ambiguity;
- the caveats.

The resolver accepts the parent-aware target of §2 once A lands, so the two areas build one target syntax. Every graph command, the hooks and the MCP server take that context. The caveat travels in the structured payload, never only on stderr. A command that writes files refuses an ambiguous target and a warm-attached graph. `rename` then edits only the occurrences bound to the chosen declaration: today its second stage scans each affected file for the name alone (`commands/rename.rs:410-411`), so a unique target still rewrites an unrelated same-name identifier.

| | Today | Target |
|---|---|---|
| Name resolvers | 7 | 1 |
| Commands that see the overlay | some | all |
| Root of the dirty set | the cwd | the worktree top |
| Deleted file | still answered | suppressed |
| Stale marker | 6 of about 15 commands, stderr only in MCP | every command, in the payload |

**Cost.** Commands that skip the overlay today pay the L1 refresh, which the L1 gate caches per session. Two fixes bound it. First, classify every porcelain path once (contract C3) and skip the per-path syscalls only for paths that are neither source nor resolution config: with 12,000 untracked `.csv` files, one `ecp find` costs 88 ms against 15 ms clean (reviewer measurement). Config files such as `tsconfig.json`, `go.mod` and `Cargo.toml` have no provider, so a filter on "has a provider" would hide their changes from area 4. Second, a host that exports no session id makes every query a new session, so nothing is cached (`session/resolver.rs:18-31`). Choosing a stable fallback key, such as the worktree path, is a product decision: two shells in one worktree would then share an overlay. The latency gates above bound the result.

**Also owned here: the invalidation closure.** Area 5 decides which clean files a dirty event makes stale; see "Beyond calls" in §1.

## 6. One routing fact: path, language, provider

**Today.** The facts that route a file live in five distinct hand-synced mappings: `EXTENSION_TABLE` (`ecp-core/src/analyzer/pipeline.rs:91`), `file_category::Language` (`ecp-core/src/file_category.rs:261-322`; `resolution::index` re-exports it), the identifier finder match (`identifier_finder/mod.rs:54-86`), the pattern finder, and `should_analyze_path` (`commands/admin/index.rs:537`, with its extension match at `:569-630`). The comment at `pipeline.rs:84-87` says the walk allowlist derives from `EXTENSION_TABLE`; it does not, and no test pins it. The tables disagree:

- The cold index constructs 37 providers. The overlay, reanalysis and `impact --baseline` run the 19 in `reanalyze.rs` `ALL_PROVIDER_NAMES`. The overlay lacks 18 registered providers, 16 of them routed (markdown and generic YAML are never routed). Bash, SQL, HCL, Dockerfile, Solidity, protobuf, GitHub Actions, Compose and 8 more never reach the overlay. Reproduced: in a committed `run.sh`, rename `old_fn` to `new_fn` and leave it uncommitted. `ecp find new_fn` returns `found:false`, and `ecp find old_fn` returns `found:true`. `tests/incremental_full_parity.rs:322-345` freezes this divergence.
- `Language` has no Vue, Svelte, Astro or protobuf variant, so those files are `Unknown`, and the Tier-3 barrier cuts them off from TS and JS targets (code read).
- The identifier finder returns an empty list for `.vue`, `.svelte` and `.astro` (`identifier_finder/mod.rs:87`), and `rename` drops an empty list (`commands/rename.rs:411`). A rename leaves those call sites unchanged, with no notice.
- OpenAPI YAML is routed to the Kubernetes provider. The Markdown and generic YAML providers are never routed.
- Two size caps disagree: 1 MiB (`ecp-core/src/config.rs:174`) and 16 MiB (`pipeline.rs:15-20`). A file between them is missing from the index but parsed in the overlay.

**Consequence for the agent.** In 16 languages, an uncommitted edit is invisible to every query. A rename across a Vue app reports success and misses the components.

**Target.** One routing definition covers extension defaults and ordered path-specific rules (basename and directory rules for Dockerfile, Compose, GitHub Actions and generic YAML). Each rule yields language, provider, size cap, identifier finder and capability set. Every reader takes its fact from the row. The overlay runs the same provider set as the cold index. A test checks that each routed extension has every capability, or a recorded `BlindSpot`.

## 7. Post-parse edges carry their endpoints and their evidence

**Today.** Several post-process passes find the enclosing symbol again by file and name, through `lookup_in_file` (`resolution/index.rs:847`). That function returns the first node with the name, and its own doc warns about it (`:840-846`). Path literals, SQL tables, event topics, scope defines and schema-field mirrors use it. Other passes in the same directory carry the raw index instead (`post_process/tx_scope_edges.rs:56-61`). The evidence and its siblings:

| Edge | Today | Evidence |
|---|---|---|
| `UsesPathLiteral` | starts at the first symbol with the name | in `commands/dev/pr_analyze.rs`, the literals inside `impl GhClient for RealGhClient` (`:541`, `:573`, `:591`) start at the bodiless trait declaration (`:430`, `:433`); 6 wrong rows in this repo (re-run) |
| `Overrides` | matches class name and method name only; merges same-named classes; needs a parser marker, which only Java, C#, Kotlin and C++ emit; confidence 1.0 and not heuristic | `post_process/overrides.rs:91-105`, `:146-180`; the doc at `:16-18` promises an arity check the code does not make |
| Indirect-dispatch flags | built from one name set per file; in Python, any parameter name anywhere in the file flags `x.m()` | `indirect_dispatch.rs:597-691`; read only by `diff` and `review` |
| `Fetches` | starts at the first node in the file, justified by a "no File nodes" comment that is no longer true | `crates/ecp-analyzer/src/resolution/builder.rs:1871-1883` |

**Consequence for the agent.** `ecp impact --literal` names a declaration with no body as the reader of a URL. "Who overrides `Base.f`" answers empty for Swift, Dart, TypeScript, PHP and Python, with no blind spot. `ecp review` warns "verify all impls" on a plain parameter call.

**Target.** Every raw record carries the raw index of its enclosing node, as `RawTxScope` does today. One helper maps that index to the global id. An edge built from a name match carries a confidence below 0.7 or a heuristic tag. `Overrides` resolves the supertype through the shared heritage facts (`ecp-core/src/analyzer/heritage.rs`). The raw records are stored in the parse cache, so the change bumps `BUILDER_FINGERPRINT` and ships with a test on data the previous binary wrote.

## 8. One route and contract record

**Today.** Routes and contracts come from several producers that nothing reconciles:

- Three route producers. NestJS gets two `Route` nodes with one uid, and a Gin or Express handler with no node prints as `inline_anonymous`.
- The Spring verb is a substring of the whole decorator text (`route_detector.rs:63-74`). `@PostMapping("/get-token")` becomes `GET /get-token` at confidence 1.0, and `@DeleteMapping("/budget/{id}")` becomes a `GET` (re-run).
- Only PHP and Ruby compose a route prefix. FastAPI, Gin, Express, NestJS and Spring routes lose their router prefix.
- `ecp contracts` is a stub. `extract_contracts_for_repo` returns empty lists as a success (`commands/contracts.rs:246-251`), and `--repo @all` over 178 repos returns `pairs: []` with no caveat.
- The group extractors emit only `ContractRole::Provider` (11 sites), so cross-repo links are always empty.
- Framework hints for C#, Swift, Ruby, C++ and Dart attach to `<module>` and are dropped.
- Five detector shapes exist. The `summary` framework catalog is a hand copy in three lists (`commands/summary.rs:342-440`). On a Laravel, Gin, Rails or ASP.NET repo, `frameworks.detected` is empty, so an agent concludes that ecp cannot see those routes.

**Consequence for the agent.** An agent asks which route a handler serves, and it gets the wrong verb and the wrong path at confidence 1.0. It asks for API contracts and gets "none" as a clean answer.

**Target.** One route record: verb, full path with composed prefixes, handler node, framework, evidence and confidence. One detector interface per framework produces it, and `routes`, `contracts`, `group` and `summary` read it. A surface that is not implemented fails with "not implemented", never an empty success. The Spring verb is a small fix that ships before the record. An event topic keeps its source spelling beside its canonical value; today extraction replaces the spelling (`event_topic/extract.rs:113-118`).

## 9. Cypher: answer exactly or refuse

**Today.** The engine accepts constructs that it then evaluates wrong. On this repo's graph:

| Query shape | Returns | Correct |
|---|---|---|
| two comma patterns, `(a {…}), (b {…})` | `[[compute_hits,None],[None,compute_single]]`: a union, not a join; `cypher_stream_aggregation.rs` pins it | one row with both |
| `EXISTS((a)-[:Calls]->(b))` with `a` bound, then `count(b)` | 18616 | 5 |
| `WITH a WHERE … WITH a RETURN count(a)` | 18616: the clauses are reordered | the filtered count |
| `count(DISTINCT f)` | 15718 against `count(f)` 18616: distinct by name | distinct by node |
| a regex with `\d` | 1333 against 42 for `[0-9]`: the backslash is lost | 42 |

The reviewer also reports, without a re-run: `ORDER BY` on a key not in the projection does nothing; unknown functions return null, and a label test inside an expression evaluates to false (`cypher/parser.rs:722`), while labels and relationship types in a pattern already fail with an error (`:333-347`); a variable-length pattern means reachability; `LIMIT` materialises every row.

**Consequence for the agent.** The agent writes a valid query and receives a confident wrong count. Cypher is the tool for questions that have no verb, so no other command shows the error.

**Target.** Each construct is on one of two lists. It is evaluated with openCypher semantics and covered by a conformance test, or the parser refuses it with an error that names the construct. An unknown function, label or relationship type is an error. The test that pins the comma-pattern union goes. Refusal comes first: each one is a small change, and it stops the wrong rows before the semantics are built.

## 10. `parse_file`: a shared driver and a per-language spec

**Today.** The 14 mainstream `parse_file` bodies hold 7,899 lines. A line-by-line classification on 2026-10-25, with a script recount, found the shares below. The labelled lines and the recount script are not in the repo; treat the shares as a reported measurement.

| Class | Share |
|---|---|
| the same concept in the same code shape, copied | 40.0% |
| the same concept, different only in syntax | 8.6% |
| language-specific | 35.6% |
| boilerplate | 15.8% |

The largest copies are the `RawNode` literal (43 copies, 793 lines), the `RawImport` push, anonymous-callback handling and the framework gate. 52 commits touched 5 or more parsers, and 26 touched all 14. Outside `parse_file`, `receiver_types.rs` exists in 14 copies, and `body_has_call` in 12 private copies. The capabilities are uneven: function meta 14 of 14, variant references 11, local types 7, indirect dispatch 6, `Overrides` markers 4. The 12 secondary languages emit no function meta, so `is_test` is never set for their callers, and `impact` counts a test caller as production. Python runs its combined query twice per file and walks the tree 8 more times. TypeScript runs the whole query again for schema fields on every file.

**Consequence for the agent.** A fix lands in some languages and not in others, so the answer quality depends on the language, with no marker. Parse is 51% of cold-index time (`docs/plans/2026-05-22-cold-ingest-opt.md`, May 2026).

**Target.** One driver owns the walk, the query pass, node construction, imports, call attachment and test classification from the file category. A per-language spec supplies the captures, the node-kind mapping and the rules that only that language has. One query pass feeds imports, schema fields, event topics and override markers. This replaces the earlier decision to keep the parser architecture out of scope. Before the first slice, add a per-pass timer: `ECP_PROF` reports per-provider totals only. Two kinds of slice stay separate. A driver extraction changes no output, and raw records, uids and edge multisets must stay identical. A capability addition changes output on purpose, and each difference is approved in review. Call extraction for Vue, Svelte and Astro is a capability addition: `sfc_common.rs:210` emits no calls, and sharing the 14 mainstream bodies does not add them.

## 11. Test architecture

**Today.** The tests that guard areas 1 to 10 are looser than the defects they must catch:

- `overlay_resolver_parity.rs` (143 tests) compares targets by file, name, owner and rounded confidence after a sort and dedup, and its main path finds the caller by name only. A closure helper in the same file compares uid and line (`:64`, `:108`). The default wrapper in `tests/common/parity.rs` supplies no aliases or repo root. The configurable harness accepts caller-supplied builders and already tests aliases.
- `overlay_differential_parity.rs` covers outgoing `Calls` edges only. Separate tests compare `References` edges (`overlay_variant_ref_parity.rs`). No test compares node sets, incoming edges, deleted files or renames, and a comprehensive node-and-edge matrix remains unestablished. This is how area 5's deleted-file defect stayed hidden.
- The JS and TS shadow cascade runs, but fragments whose recorded mtime is zero (`auto_ensure.rs:1232`) fail the reader's freshness check, which needs an exact mtime. No test noticed. Area 11 adds the regression fixture; lane F repairs the mtime capture and removes that fixture's baseline.
- `incremental_full_parity.rs` compares the overlay pipeline with itself, both with 19 providers, and pins the divergence of area 6.
- The stored-data fixtures only assert a rejection, and the newest `graph.bin` fixture is v13 while the format is 15.
- No mechanical gate enforces the 14-language rule; several test dimensions cover 3 to 5 languages.

**Target.**

- One parity harness with a strict key (source uid, target uid, kind, line, edge type, confidence). It reuses the structure of `tests/common/parity.rs` but compares edge multisets: that helper dedups today (`:88`, `:178`), which hides a doubled edge. Identity and parent correctness are separate assertions. It runs over node sets and every edge kind, for dirty, deleted, renamed and shadowed files. A known difference lives in a baseline file that must match exactly, as B §4.3 item 5 states for R14.
- Fixtures written by the previous release binary for every stored format, per the repo `CLAUDE.md`.
- A test that lists the languages each test dimension covers, and fails when one of the 14 is missing without an exemption line.
- A test that the routing table of area 6 agrees with every reader.

## Contracts before code

Five contracts are shared by several areas. Each is a short design note plus its Rust types, written and reviewed before any area that consumes it changes behaviour.

| Contract | Defines | Implemented against by |
|---|---|---|
| C1 identity | declaration identity, lexical parent, the target request syntax and a typed ambiguity result; route identity and handler identity as separate keys | 1, 2, 5, 8, 9 |
| C2 workspace facts | resolution scope per file, scope root, effective config, dependency map; stored once, read by index, overlay, flow and the `Imports` pass | 1 (the Rust subset in B's first round), 4, 5 (the invalidation closure) |
| C3 dirty event | one record per changed path: added, modified, deleted, renamed (both halves); class source, resolution config or other. Only "other" skips the per-path syscalls | 1, 4, 5, 6 |
| C4 raw records | the parser phases (collect captures, finalize nodes, bind raw records), the raw index to global node mapping, the endpoint of a raw record (node, file or unresolved), and the evidence classes exact, ambiguous, heuristic and unresolved | 2, 4, 5 (the records the overlay replays), 7, 8, 10 |
| C5 routing result | file format, provider, language after parse, identifier finder, capability set, effective size cap; extension defaults plus ordered path rules | 1, 2, 4 (`imports_edges.rs:152`), 5 (source or config class), 6, 7 (`scope_defines.rs:101`), 8 (response-shape language and consumer files, `builder.rs:1302`, `:1314`), 10 |

Area 11 verifies all five. A lane needs every contract that one of its areas uses, and all five reach phase 1's done state before phase 2 starts.

## Cross-area conflicts

Two areas conflict when they change one function, one stored format or one piece of data. A shared file alone only decides who rebases. codex checked all 55 pairs of the 11 areas; the pairs with a conflict are below.

| Pair | Shared point | Resolution |
|---|---|---|
| 1, 2 | global candidate filter (`resolution/index.rs:1073`), lexical input, uid pairing (`view.rs:388`) | the three ordered pairs in the Order section; C1 fixed first |
| 1, 4 | B7b's crate table and area 4's workspace facts | C2 first; B's first round stores the Rust subset with one `graph.bin` change; area 4 adds rows for other languages, not layout |
| 1, 5 | dirty symbols and call resolution in `OverlayView::build` (`view.rs:369`, `:587`) | C3 first; a deleted file is a whole-file tombstone, never "keep base" |
| 1, 6 and 2, 6 | `Language` decides call normalization (`resolver.rs:730`), global admission (`file_category.rs:400`) and Rust owners | C5 first; areas 1, 2 and 6 read one language value and one compatibility rule |
| 1, 7 | heritage: the overlay pre-pass (`view.rs:521`) and `Overrides` (`overrides.rs:153`) | `Overrides` reads the shared heritage facts |
| 2, 5 | the target resolver reads the owner (`symbol_id.rs:135`); rename treats an empty owner as top level (`rename.rs:333`) | C1 first; rename's scoped exclusion ships with A's opt-in |
| 2, 7 | raw slots kept in pass 1 (`builder.rs:1198`) | C4's index mapping; area 7 waits for that mapping, not for A-A |
| 2, 8 | Route uids made in two places (`builder.rs:1411`, `:1481`) | C1's route and handler identity |
| 3, 4 and 3, 6 | the source snapshot (`orchestrator.rs:139`) feeds config reads and file filters | area 3 defines the snapshot; areas 4 and 6 read from it |
| 3, 5 | freshness sidecars and the view cache key (`view_cache.rs:35`); one function, `git_fingerprint_shortcut` (`auto_ensure.rs:409-451`) | the read context takes the confirmed build handle and keeps the graph stamp in the cache key; area 3 lands first |
| 4, 5 and 4, 6 | config files have no provider (`pipeline.rs:146` routes only OpenAPI JSON) | C3's "resolution config" class; a config change widens the invalidation scope |
| 4, 7 | `Imports` guesses at 0.9 (`imports_edges.rs:391`) | C4's evidence classes |
| 5, 7 and 5, 8 | the overlay masks only some edge kinds (`view.rs:995`) and skips builder nodes (`:1297`) | the incremental overlay above; until then, a caveat that names the stale edge kinds |
| 7, 8 | `Fetches` source (`builder.rs:1871`) and `RawRoute.handler` as a string (`types.rs:507`) | C4's endpoint type, used by both |
| 7, 10 and 8, 10 | raw indexes (`types.rs:935`) and route producers rewritten by a later driver | C4's parser phases and index rules, fixed before 7 and 8 |
| every area, 11 | a hard zero-diff gate blocks the first slice; a moving expected file hides regressions | one baseline file per known difference, with an owner area and a removal slice |

## Lessons from peer tools

GitNexus 1.6.12 and codegraph 1.6.2 were read at their source on 2026-10-25. Both answer a deleted file as if it existed: `codegraph query alpha` returns `a.ts:1` after `rm a.ts`, and `gitnexus impact` returns the deleted file's caller. Both also ship name-matched edges at full confidence. These practices fit this plan:

| Practice | Source | Area | Query-time cost |
|---|---|---|---|
| routing tables that the compiler checks for every language (a `match` on the language enum with no wildcard arm) | GitNexus `language-detection.ts:24-52` | 6 | none |
| the alias map of the nearest config first, then the root map; nested `go.mod` and `go.work` rules | codegraph `import-resolver.ts:1293`; GitNexus | 4 | none: built at index time |
| one result type for every lookup: ok, ambiguous with the true count, not found | GitNexus | 5 | none |
| a caveat only for the files an answer cites: one `stat` per cited file, a hash only on a mismatch | codegraph `mcp/tools.ts:2713-2781` | 5 | one `stat` per cited file |
| golden capture fixtures per language; a full-against-incremental graph diff | GitNexus `test/fixtures/*-captures-golden` | 11 | none |
| a real Cypher engine as a test oracle for area 9's conformance cases | GitNexus uses LadybugDB | 9 | none: tests only |

Two practices do not fit. An embedded graph database costs 0.42 to 0.55 s per GitNexus CLI call, against a 30 ms target. A `label:name` node id (GitNexus `lib/utils.ts:1-3`) collides, and GitNexus issues #3041, #3499, #3569 and #3564 shipped those collisions.

## Order

Work runs in lanes. Two slices run at the same time only when their file lists share no file. Cargo runs in the main session, one process at a time; at most three slices are edited at once.

**Phase 0: start now, in parallel.**

| Lane | Work | Files |
|---|---|---|
| 0a | area 11: the strict harness, the baseline files with owners, the dirty-event fixtures | `ecp-cli/tests/`; `scripts/benchmark/benchmark_ecp.py` (the dirty-file scenario) |
| 0b | area 3: the SHA lock, the rising name, sidecars inside `building/`. The one snapshot rule is deferred to FU-2026-10-25-8aae9b180d81 | `build/orchestrator.rs`, `commit_lookup.rs`, `admin/gc.rs`, sidecar code in `auto_ensure.rs` |
| 0c | area 9: the refusals | `ecp-core/src/cypher/`; `ecp-core/tests/cypher_stream_aggregation.rs` |
| 0d | the small local fixes of the review page that use only today's identity and error semantics, such as cli F4's ambiguity error. flow F8 (handler-bound response shapes) waits for C1 and C4 and moves to lane P; freshness F6 moves to phase 1 | per fix; a fix in `auto_ensure.rs` runs after 0b's edits there |

**Phase 1: the five contracts.** Each one is a design note, a review, then its types. C1 and C2 come first, because the first rounds of A and B need them. The contract slices change no behaviour and land in any order, with one exception: C3 ships with one behaviour slice, the classification of porcelain paths that replaces freshness F6. That slice lands after C2 and C5, because it decides the source and config classes from them, and it is the slice that lets the latency thresholds bind.

**Phase 2: three implementation lanes.**

| Lane | Slices, in order | Needs |
|---|---|---|
| R (resolution) | B's first round; A-A; the parent identity; A-B, A-C, A-D interleaved with B5 and B8 as the pair table below says; B7b's ladder half; area 4 for the other languages on the C2 facts | C1 to C5, phase 0a, 0b |
| F (freshness) | area 5 (read context, worktree top, dirty events, caveat in the payload, the shadow-cascade mtime); then the incremental overlay | C1 to C5, phase 0b; the incremental overlay also needs B5 |
| P (parse output) | area 6, including the `Language` lookups in `session/view.rs:1770`, `:2828` and `resolver.rs:730`; area 7; area 8, with flow F8 | C1, C3, C4, C5; area 7 also needs the raw index mapping of C4 |

Before a slice is dispatched, its inventory lists the production files, the tests, the baseline files, the stored payloads and the compatibility identifiers it touches. Slices that share a file run one at a time. Slices that share stored data are coordinated even when their files differ. Known shared points between lanes:

| Lanes | Shared file or data |
|---|---|
| R, F | `session/view.rs`; `session/overlay_reader.rs` (A-A changes uid assignment, F changes which deleted and renamed inputs it accepts); the view cache key |
| R, P | `resolution/builder.rs`, `resolution/index.rs`; the `graph.bin` they both produce |
| F, P | the fragment payload and the manifest (F replays the raw records that P changes); `reanalyze.rs` |
| R, F, P | `BUILDER_FINGERPRINT`, the parse cache, the parity baselines; the C5 language lookups in `view.rs` and `resolver.rs` (P edits them one at a time with R and F, and the language after parse goes live only when every lookup reads C5) |

| Shared code | Order |
|---|---|
| Rust same-file lexical tier, `PickFacts`, overlay scope stores | A-C deletes the string-built scope before B5 moves the rule |
| Global candidate filter | A-B adds the scoped exclusion; B5's shared interface keeps it |
| `LexicalFunctions` input, the builder and overlay call closures | A-D changes the lexical facts before B8 moves them |

A-E needs a shared contract and tests for implicit `this` and lexical precedence, not the whole of B8.

**Phase 3: area 10.** The driver merges the parser code after A §5.2, area 6, area 7 and area 8 have changed it under C4. A per-pass timer comes first.

**Stored formats.** Area 3 lands before any format change. Five compatibility identifiers are tracked separately:

| Identifier | Changes when | Changes planned |
|---|---|---|
| `graph.bin` (`GRAPH_FORMAT_VERSION`) | the archived layout changes | B's first round: the C2 Rust crate table. A: lexical parents. Area 8: only if it adds a route table. Area 4's other languages add rows to the C2 facts, not layout, if C2 is designed for that |
| parse cache (`BUILDER_FINGERPRINT`, archived `LocalGraph`) | parser output, raw-record layout (C4), uid assignment (A-A), routing (C5), or graph-building semantics (B3b) | per slice |
| L1 fragment encoding (V5 today, `overlay_reader.rs:75`) | the overlay replays new raw records | the incremental overlay |
| dirty manifest | C3 adds the event class and deletions | C3's behaviour slice |
| view cache key (`view_cache.rs:40`) | any of the above | follows them |

Phases 0 to 3 ship in one release. One value of an identifier maps to one data contract:

- Each slice that makes stored data incompatible bumps every identifier that guards that data, also between two slices of one release.
- Each migration names its slice.
- Before the release, one test upgrades data that the previous release wrote. Further tests combine data that different slices left in one cache: a graph, a parse cache, a manifest and a fragment. Each test asserts what a query returns after the refusal or the rebuild, not only that the read succeeds.

**Gates for every slice:** the area's own proof of done (Certainty table), the latency rule (no regression; absolute thresholds after C3's slice), and the harness with no difference outside the baselines.

## Out of scope

These parts get no separate restructuring. Their defects are local and fix in one place:

- the append-only rkyv schema rule for `NodeKind` and `RelType`;
- the CLI output formats;
- the human-facing modules that the LLM-utility filter flags (admin menus, the config wizard, self-update, telemetry). They are a deletion discussion, listed in the review page, not a restructuring.
