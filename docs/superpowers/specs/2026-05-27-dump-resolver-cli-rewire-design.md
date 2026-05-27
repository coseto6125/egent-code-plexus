# Design: Re-wire `--dump-resolver` in v2 layout

**Date:** 2026-05-27
**Branch:** `feat/dump-resolver-rewire`
**Status:** Approved (design)

## Problem

`ecp diff --section bindings` (and `--section all`) crashes 100% of the time:

```
Command failed: output encode error: read /tmp/ecp-diff-baseline-<SHA>.jsonl: No such file or directory (os error 2)
```

Surfaced via `ecp usage --failures` — the recurring ~550ms `diff` failures are this.

### Root cause

`ecp diff`'s bindings section produces a resolver-decision JSONL by shelling out
to `ecp admin index --repo <repo> --dump-resolver <out>` (see
`diff/bindings.rs::dump`). But in the v2 layout that subcommand only prints a
warning and returns success — it never writes the file:

```rust
// admin/index.rs:372
if args.dump_resolver.is_some() {
    eprintln!("warning: --dump-resolver accepted but not yet wired in v2 layout; ...");
}
```

So `diff` then tries to `load_jsonl` a file that was never written → read error,
mis-wrapped as `output encode error`.

`--section routes / contracts / symbols` are unaffected (they read `graph.bin`,
not the JSONL). Only `bindings` — and therefore `all` — is broken.

### Why it's a thin fix, not a cross-crate project

Investigation showed the analyzer layer is **already complete**:

- `GraphBuilder` has the `resolver_dump_path` field + `with_resolver_dump()` setter
  (`builder.rs:272, 345`).
- `GraphBuilder::build()` fully implements the dump path: serial resolver,
  `enable_dump()`, `take_decisions()`, `write_resolver_dump()` (`builder.rs:1138–1424`).
- Three **non-ignored** tests already cover it, including a serial-path assertion
  that the dump file exists (`builder.rs:3230–3308`).

The only broken link is the **CLI wiring**: the `--dump-resolver` path is parsed
into `IndexArgs.dump_resolver` but never threaded into `with_resolver_dump`. The
chain dies at `admin/index.rs:372`.

## Goal

Make `ecp diff --section bindings` and `--section all` work again by threading
the dump path from the CLI into the (already-working) analyzer dump path.

## Non-goals

- No change to the analyzer dump implementation (it works; tests pass).
- No change to `~/.ecp` storage or normal `admin index` performance.
- Not reviving the bindings feature's *value* debate — user decided it has value
  (tier-degradation / silent-target-change early warning); this is purely wiring.

## Chosen approach: A — bypass `build_l2`

`admin index`'s normal path (`build_l2` / `force_rebuild_l2`) has a **fast-path
attach**: for an already-built SHA it returns without re-running the analyzer
(`index.rs:390`, the `(false, Some(existing))` arm). `ecp diff` always checks out
a baseline SHA that is typically already built — so even with wiring, the
analyzer would be skipped and the dump never produced.

**Solution:** when `--dump-resolver` is present, `admin index` bypasses
`build_l2` entirely and calls `run_analyzer_for_paths` directly with the dump
path, writing only the requested JSONL.

### Why A (vs B: build_l2 thread-through, vs C: in-process diff)

| Concern | A (bypass) | B (build_l2 + skip fast-path) | C (in-process) |
|---|---|---|---|
| Normal `admin index` speed | unchanged | unchanged | unchanged |
| `~/.ecp` writes | **none** (only /tmp jsonl) | **new graph generation** | none |
| Dump-time cost | analyzer pass only | full publish + tantivy + registry | analyzer pass only |
| Semantic cleanliness | dump = side-output, no state change | conflates dump with publishing | couples to GitGuard checkout |
| Implementation surface | smallest | largest | medium, checkout-interaction risk |

A is lightest, does not feed the `~/.ecp` `.gen` generation-leak GC pain
(B would write a redundant graph generation per dump), and keeps dump as a pure
side-output that does not mutate resident state. `run_analyzer_for_paths` already
takes `src_root: &Path`, so it natively handles diff's GitGuard-checked-out
baseline tree.

### Implementation note: A exposed a latent diff bug (the real fix)

Approach A as first written (bypass build_l2, write only the JSONL, never publish
`graph.bin`) **broke all of `ecp diff` for never-before-indexed baselines** —
including `--section routes` alone. Root cause was NOT in approach A's wiring but
a latent defect in `diff/mod.rs` that A's removal of a side effect uncovered:

- The original code relied on `bindings::dump`'s subprocess (`admin index`,
  pre-A) **synchronously publishing `graph.bin`** as a side effect, so the later
  `ensure_fresh` always saw a `Ready` graph.
- `diff/mod.rs` called `ensure_fresh(...)` but **ignored its `EnsureFreshOutcome`
  return value**. When a SHA has no published graph, `ensure_fresh` returns
  `WarmAttach` — it borrows a *sibling* SHA's graph and spawns a *detached
  background* rebuild (`auto_ensure.rs:386–396`). Diff then `copy`/`extract`-ed a
  graph that was the wrong SHA and/or not yet written → "No such file" /
  background-writer race. A removed the synchronous publish, so this path now
  fired.

**The fix** (commit on this branch) keeps A's clean bypass and repairs the diff
defect at its source: a shared helper `ensure_graph_synchronously(repo, sha,
label)` used by **both** the current and baseline sides. On `WarmAttach` it forces
a foreground `build_l2(repo, sha)` for the exact SHA, then resolves the path
*after* the build so the fresh commit dir is used (not the legacy fallback). This
also subsumes an earlier per-site "resolve-after-ensure" workaround. Net effect:
diff no longer depends on the dump's graph-publish side effect at all — the two
concerns (resolver JSONL vs. baseline graph readiness) are now properly separated.

## Data flow (after fix)

```
ecp admin index --repo <r> --dump-resolver <out>
  │  IndexArgs.dump_resolver = Some(<out>)
  ▼
admin/index.rs::run
  │  if dump_resolver.is_some():
  │     run_analyzer_for_paths(<r>, <tmp_out_dir>, parse_cache, dump_path=Some(<out>))
  │     return  (skip build_l2 entirely)
  ▼
run_analyzer_for_paths(..., dump_path)
  │  GraphBuilder::new().with_resolver_dump(dump_path) ...
  ▼
GraphBuilder::build()  ── unchanged ──▶ write_resolver_dump(out, decisions, table)
```

## Changes

### 1. `run_analyzer_for_paths` — add optional dump param

`crates/ecp-cli/src/commands/admin/index.rs:69`

Add a fourth parameter `dump_resolver: Option<&Path>`. At the `GraphBuilder::new()`
site (line 327), conditionally call `.with_resolver_dump(dump_resolver.map(Path::to_path_buf))`.

Update the existing caller in `orchestrator.rs:150` to pass `None` (normal build
never dumps).

### 2. `admin index::run` — bypass branch when dumping

`crates/ecp-cli/src/commands/admin/index.rs:371`

Replace the warning-only block. When `args.dump_resolver.is_some()`:
- Resolve a scratch out-dir for the analyzer's `graph.bin` (a temp dir — the
  graph itself is discarded; only the dump JSONL is wanted). The dump JSONL goes
  to the caller-specified `--dump-resolver` path.
- Call `run_analyzer_for_paths(&worktree, &scratch_out, parse_cache, Some(&dump_path))`.
- Return early (do not touch `build_l2`).

Scratch out-dir: use `std::env::temp_dir().join(format!("ecp-dumponly-{pid}"))`,
create it, and `remove_dir_all` it on the way out (best-effort). The dump JSONL
path is outside it (caller owns that path), so it survives.

### 3. Restore the ignored diff bindings integration test

`crates/ecp-cli/tests/diff_bindings_test.rs:28`

Remove `#[ignore = "requires --dump-resolver which is deferred in v2 ..."]` on
`diff_bindings_two_commit_resolution_change`. The test is already written in v2
terms (`--section bindings --format json`, asserts `sections.bindings.*`) — it
was authored for exactly this fix and should pass once wiring lands. No
expectation rewrite expected; verify by running.

**Pre-existing bug found while un-ignoring (fixed in same commit):** the test
exposed a path-resolution race in `diff/mod.rs`. The baseline graph path was
resolved via `graph_path::resolve` *before* `ensure_fresh` built the graph. For a
baseline SHA that was never indexed before (e.g. a fresh TempDir repo, or any
first-time diff of a never-seen baseline), `resolve_v2`'s `CommitIndex.find`
returns `None` → falls back to the non-existent legacy `.ecp/graph.bin` → the
later `std::fs::copy` fails. Fix: resolve the path *after* `ensure_fresh`, so the
freshly-built commit dir is in the index. Safe because `CommitIndex::scan_cached`
keys its process cache on `commits/` mtime, which `ensure_fresh` bumps. This bug
was latent because earlier manual `--section routes --baseline` tests used
already-indexed baselines, so the first `resolve` already hit the v2 path.

Note: the sibling test `diff_bindings_against_head_yields_empty` (line 159) is
*not* ignored and passes today because HEAD-vs-HEAD hits the
`baseline_sha == current_sha` fast-path (`builder.rs:161`) and returns empty
without invoking the dump subprocess. This confirms only the non-fast-path
bindings flow is broken.

### 4. Un-skip bindings in `review`

`crates/ecp-cli/src/commands/review/mod.rs:70`

The comment says bindings is skipped because "--dump-resolver path is deferred in
v2". Re-enable the bindings section in `review` now that it works.

## Testing

- **Restore + pass** `diff_bindings_test.rs` (integration, real subprocess +
  GitGuard checkout). Primary regression guard.
- **New unit test**: `run_analyzer_for_paths` with `dump_resolver = Some(tmp)`
  writes a non-empty JSONL whose lines deserialize into `BindingDecision`. (The
  GraphBuilder-level round-trip is already covered at `builder.rs:2545`; this new
  test pins the *CLI wiring* specifically.)
- **Manual verification**: `ecp diff --section bindings --baseline HEAD~3` and
  `--section all` both exit 0 with structured output (currently exit 1).
- **No-regression**: `ecp admin index` without `--dump-resolver` still goes
  through `build_l2` (confirm the bypass branch is `is_some()`-gated).

## Error-handling

- If the analyzer pass fails, surface the analyzer error (not a mis-wrapped
  "output encode error"). The diff caller's `load_jsonl` error message
  (`read {path}: {e}`) is already correct; the real issue was the file never
  being written, which this fixes.
- Scratch out-dir cleanup is best-effort (`let _ = remove_dir_all`); a leaked
  temp dir is not a correctness issue.

## Risk

Low. Analyzer dump path is already tested and unchanged. The change is additive
CLI wiring + one early-return branch, gated on a flag that is off for all normal
traffic.
