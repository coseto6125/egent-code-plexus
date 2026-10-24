# Architecture direction: four structural changes

Status: direction, not started. No code from these programs is on `main` (`8868d659`): `GRAPH_FORMAT_VERSION` is 15, and `lexical_parents` and the resolution outcome type do not exist.

This page says where ecp's structure should go and how each target differs from today. Two of the four areas have full designs, kept beside this page:

| Area | Design | Size (realistic) |
|---|---|---|
| 1. One resolution rule set for the index and the overlay | [`2026-10-23-shared-resolution-design.md`](2026-10-23-shared-resolution-design.md) (design B) | ~7,000 changed lines |
| 2. Split `owner_class` into a type owner and a lexical parent | [`2026-10-23-owner-split-design.md`](2026-10-23-owner-split-design.md) (design A) | ~3,200 changed lines |
| 3. One ranking rule for "the current graph of a commit" | none yet; direction below | not estimated |
| 4. Resolve an import in the caller's workspace scope | none yet; direction below | not estimated |

[`2026-10-23-architecture-designs-crosscheck.md`](2026-10-23-architecture-designs-crosscheck.md) is the adversarial review of A and B, with the interleaved slice order and the effort estimates above.

On 2026-10-25 codex reviewed this page as a design peer. Its positions are folded into the sections below. Where this page departs from them, the section says why.

Areas 1, 2 and 4 change what `ecp impact`, `find` and `inspect` answer. Area 3 changes whether ecp answers from the graph it should. Each section below states the consequence for the agent that reads ecp's output, since that agent is the user (see the repo `CLAUDE.md`).

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
3. **The Rust cross-crate gap, as its own slice.** The overlay lacks the facts: `RustModules` (`view.rs:4234`) holds the graph, the dirty files and the roots, and no crate table. A shared ladder does not supply those facts, so this fix need not wait for B7.

B9 and the R30 helper cleanup stay out of the first round. After each slice, decide whether the next one still pays. Watch every method of the facts trait: a method that hides a pick policy, such as `global_pick`, leaves two rule sets behind one ladder.

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
| `graph.bin` | v15 | v16, with one sparse sorted table of parent links |
| Old data | — | a v15 graph rebuilds on the format bump; overlay fragments of another builder are skipped and re-parsed; old cache dirs age out through gc (A §5.4) |
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

**Target.** Every reader ranks same-SHA dirs by one key, the dir name, and the builder can only publish a name above every existing one.

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

## Order

1. **Start now, with no code dependency between them:** the area 3 change and its failure tests, the parity harness of §1, the identity cases of §2, and tests that read data written by the previous binary.
2. **Area 3 lands before A or B ships a graph change.** This is an order for verification, not a code dependency: a test of a new graph must read the build it means to read. Its boundary is `build/orchestrator.rs`, `registry/`, `commit_lookup.rs`, `admin/gc.rs` and the sidecar code in `auto_ensure.rs`.
3. **Then the first rounds of A and B:** B's first round (§1), A-A, the parent identity (§2), A-B, A-C and A-D. Defect fixes for Go selectors, chains and Rust cross-crate calls go in as they come. The shared Tier 1, global, import and call-entry slices follow, one at a time.
4. **Three pairs fix the order between A and B.** The other overlaps in B §4.2 are shared files, not shared data, and only decide who rebases.

   | Shared code | Order |
   |---|---|
   | Rust same-file lexical tier, `PickFacts`, overlay scope stores | A-C deletes the string-built scope before B5 moves the rule |
   | Global candidate filter | A-B adds the scoped exclusion; B5's shared interface keeps it |
   | `LexicalFunctions` input, the builder and overlay call closures | A-D changes the lexical facts before B8 moves them |

   A-E needs a shared contract and tests for implicit `this` and lexical precedence, not the whole of B8.
5. **A and B bump `graph.bin` in their own releases.** One v16 for both ties two programs to one release.
6. **Area 4 after the first round of B**, so its facts feed one ladder.

## Out of scope

These parts get no separate restructuring. Their defects are local and fix in one place:

- the architecture of the per-language tree-sitter parsers (`crates/ecp-analyzer/src/<lang>/`). The parser wiring that area 2 needs (A §5.2) is part of area 2;
- the append-only rkyv schema rule for `NodeKind` and `RelType`;
- the CLI output formats.
