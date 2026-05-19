# Follow-ups After Index Layout Redesign (PR #55)

| Field | Value |
|---|---|
| Date | 2026-05-17 |
| Status | Draft — items waiting to be picked up |
| Source | PR #55 (squash commit `724efa91`) deferred work + observed pre-existing issues |
| Parent spec | `docs/superpowers/specs/2026-05-17-index-layout-redesign-design.md` |
| Parent plan | `docs/superpowers/plans/2026-05-17-index-layout-redesign.md` |

PR #55 shipped the v2 storage layout (L2 commit-content-addressed + L1 session-local) but consciously deferred some pieces and surfaced some pre-existing issues. This doc enumerates them so they don't decay into folklore.

---

## P1 — Pre-existing CI failures on `main` (not introduced by #55)

These tests were already failing on `main` before #55 landed. Visible during the #55 acceptance sweep but out of scope for that PR. They should be either fixed or `#[ignore]`'d with documented rationale so CI red doesn't mask real regressions in subsequent PRs.

### Files
- `crates/cgn-cli/tests/search_batch.rs` — 3 failing tests. Cause hypothesis: expect a pre-indexed graph that the test setup doesn't create.
- `crates/cgn-core/tests/cypher_aggregation.rs` — 2 failing tests: `count_distinct_callers`, `distinct_callees`. Identical test file hash to `origin/main` before #55 → confirmed pre-existing.
- `crates/cgn-cli/tests/tsconfig_paths.rs::alias_specifier_resolves_to_aliased_file_e2e` — 1 failing test. Discovered during 2026-05-17 force-rebuild work; verified pre-existing by running against `498d1ce` (parent of force-rebuild branch). Cause hypothesis: TypeScript path-alias resolver depends on fixture setup that doesn't match current analyzer behavior.

### Action
1. Reproduce: `cargo test --test search_batch` / `cargo test --test cypher_aggregation` on a fresh `main` checkout.
2. Decide per test: (a) fix the assertion / fixture, or (b) mark `#[ignore = "<concrete reason + tracking issue link>"]`.
3. One PR covering both files; commit message format: `test: triage pre-existing failures in <file>`.

### Why P1
CI hygiene baseline. Every subsequent PR that runs the full test suite sees these red. Future "did my change cause this?" debugging gets contaminated. Cheap to fix or formally exclude.

---

## P2 — Task 5.4: Engine overlay merge (design gap, real product value)

L1 fragments are currently **written** to `~/.cgn/<repo>/sessions/<sid>/graph_overlay/` but **not read** by any query path. So edits without commit are tracked on disk but invisible to `inspect / cypher / search / impact / scan`.

### What's missing
1. **Fragment shape design** — pick the rkyv-archived container shape that the merge view will consume. Candidates:
   - `(Vec<NodeUid_removed>, Vec<Node_added>, Vec<Edge_added>, Vec<Edge_removed>)` — full delta
   - `Vec<FileGraphFragment>` where each fragment is a complete per-file subgraph replacing the L2 base's view of that file
   - Something else informed by what the query layer needs (lookups by UID? by file_path? by name?)
2. **`parse_single_file_to_fragment` real implementation** in `commands/scan.rs` — currently returns empty `Vec<u8>` stub. Needs analyzer integration to produce the chosen shape per dirty file.
3. **`OverlayView` wrapper** — `Engine::graph()` returns a wrapper when `overlay_dir.is_some()` that:
   - Holds base `&ArchivedZeroCopyGraph` (mmap'd L2)
   - Loads + merges all `<overlay_dir>/graph_overlay/*.bin` fragments
   - Dispatches lookups: overrides first, base second; edge enumeration: union(overrides) ∖ (edges_to_overridden_nodes from base)
4. **Update 5 query commands** — `inspect / cypher / search / impact / scan` — to accept `OverlayView` shape instead of raw `ArchivedZeroCopyGraph`.

### Process
- **Brainstorm first** (`superpowers:brainstorming`): co-design fragment shape with real workload examples (which queries hit which override classes most). Don't pick shape in a vacuum.
- **Spec doc**: `docs/superpowers/specs/YYYY-MM-DD-l1-overlay-merge-design.md`.
- **Plan doc**: stagger into 4 sub-tasks per the parent plan's note on Task 5.4 ("split into 4 sub-tasks during implementation if needed").

### Why P2
This is the actual feature value of the v2 redesign. Without it, the spec's "edits no longer trigger full reindex" promise is half-fulfilled: writes are cheap, reads are still L2-only. Users who edit then immediately query won't see their edits.

---

## P3 — v2 fixture cleanup (small bundled work)

A cluster of small items unlocked by writing one shared v2 test fixture helper:

### Items
- 5 `#[ignore]`'d integration tests need v2 fixture helper to unignore:
  - `tests/diff_bindings_test.rs::diff_bindings_two_commit_resolution_change`
  - `tests/impact_cmd.rs::impact_baseline_ref_runs_diff_mode`
  - `tests/hook_pre_tool_use_test.rs::with_index_emits_legacy_block_via_subprocess`
  - `tests/hook_session_start_test.rs::template_placeholders_get_rendered_when_meta_present`
  - `tests/search_cmd.rs::search_multi_repo_at_group_both_repos` / `search_multi_repo_csv_single`
- Re-wire `--dump-resolver` flag in `build_l2` (needed by `cgn diff` baseline path)
- ~~Re-wire `--force` / `--embeddings` / `--drop-embeddings` / `--no-cache` flags OR remove them with deprecation notice~~ **Shipped** — `--force` now drives `force_rebuild_l2` (drop L2 + selective L1 invalidate + rebuild); `--no-cache` removed (warn-no-op had no v2 semantic); `--embeddings` / `--drop-embeddings` were already gone in PR #51. See `docs/superpowers/specs/2026-05-17-force-rebuild-and-session-reference-design.md`.
- `cgn admin prune --branch` stub → either implement v2 semantic (LRU-by-SHA-range?) or remove the `--branch` flag and update help text

### Shared helper to write first
```rust
// crates/cgn-cli/tests/common/v2_fixture.rs (new)
pub struct V2Repo {
    pub home: TempDir,
    pub worktree: TempDir,
    pub repo_dir: PathBuf,         // <home>/.cgn/<dir_name>/
    pub commit_dir: PathBuf,       // <repo_dir>/commits/<dirname>/
    pub head_sha: String,
}

impl V2Repo {
    /// Init git repo, commit, run `cgn admin index`, return paths.
    pub fn new(files: &[(&str, &str)]) -> V2Repo { ... }

    /// Add a session under this repo with an empty L1.
    pub fn add_session(&self, sid: &str) -> PathBuf { ... }

    /// Build a v2 RegistryFile referencing this repo.
    pub fn registry(&self) -> RegistryFile { ... }
}
```

Once `V2Repo` exists, each of the 5 ignored tests is a 10-20 line rewrite. Bundle into one PR.

### Why P3
Each item alone is too small for its own brainstorm cycle, but they share infrastructure (V2Repo helper). Doing them in one bundled PR avoids redoing fixture work 5 times.

---

## P4 — Force-rebuild follow-ups (post-2026-05-17 spec)

These items were explicit non-goals in the force-rebuild + session-reference design (`docs/superpowers/specs/2026-05-17-force-rebuild-and-session-reference-design.md`) but should still happen:

- **F5 fd-level test**: spec invariant F5 ("PureReference query 不讀 graph_overlay") is currently asserted via `engine.view() == GraphView::L2Only`, not via strace/lsof. If future P2 work adds overlay probing inside `Engine::load`, F5 could silently regress. Add a wrapper that records every `fs::File::open` path and runs the standard query smoke against it.
- **`admin sessions reset <id>` / `admin sessions sweep`**: parent spec §11.2 lists these alongside `sessions list`. Now that the subcommand exists with `list`, the other two are mechanical additions over `SessionState` + atomic rename.
- **AugmentedReference overlay merge** (parent spec §11.2 P2): `Engine::open` records `overlay_dir` for Augmented sessions but doesn't merge fragments yet. P2 implementer should hook into the `GraphView::L2WithOverlay` branch.
- **`admin index --rev <ref>` flag**: currently implicit HEAD. Add when the broader `--rev` rollout per parent spec §11.1 happens.

---

## Out of scope (don't pick up unless you have a reason)

- **`session_resolver` env-var race under parallel cargo test** — flaky in default parallel mode, deterministic with `--test-threads=1`. Real fix is to use a process-wide mutex in tests that touch `CLAUDE_CODE_SESSION_ID`. Low ROI — workaround documented.
- **Cross-fork same-SHA L2 sharing** — `<repo>/commits/<sha>/` could symlink to `_shared/objects/<sha>/` if disk pressure becomes real. YAGNI today; current users don't have this workload.
- **Background `cgn daemon`** — would let `cgn admin gc` run automatically on a schedule, attach pattern via socket instead of fork-per-call, etc. Spec §16 lists this; only worth doing if users hit ops pain.

---

## Notes on the PR itself (for retrospective)

- Squash merge consolidated 45 commits into one (`724efa91`). Original commit history exists on the worktree branch `worktree-spec-index-layout-redesign` if someone needs to trace evolution; the feat branch `feat/index-layout-redesign` was deleted from `origin` per `gh pr merge --delete-branch`.
- The 6 review-driven fix commits (`b613592`, `841c459`, `88861f1`, `ed8b872`, `cf57a7b`) are also baked into the squash; their fix rationale is captured in PR #55 comments.
- Merge commit `e33a1b8` resolved 15 conflicts with `origin/main` (mostly intersection with PR #51 hard-delete embedding pipeline). Decisions documented in that commit message.
