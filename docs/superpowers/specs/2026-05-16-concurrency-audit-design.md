# Sub-project 0 — Concurrency Audit (deep)

**Date**: 2026-05-16
**Status**: Design (approved skeleton, awaiting spec review)
**Parent roadmap**: cgn ↔ gitnexus parity & exceed (7 sub-projects)
**Blocks**: Sub-projects 1–6

## 0. Why this exists

The parity roadmap will add ~10 new `Edge` emit sites (Imports / Defines / Implements / CONTAINS / MEMBER_OF / INHERITS / METHOD_OVERRIDES / METHOD_IMPLEMENTS / DECORATES / USES / WRAPS / QUERIES / HANDLES_TOOL). The existing parallel emit path (`pass2_emit_node_edges`, dispatched by rayon in `resolution/builder.rs`) is guarded by exactly **one** equivalence test —
`pass2_parallel_and_serial_emit_identical_edges` at `crates/cgn-analyzer/src/resolution/builder.rs:1819` — and it asserts only on `(source, target, rel_type)` tuples without per-rel-type stratification.

Adding emit sites without first auditing the concurrency surface risks:
- Non-determinism that only fires under specific thread counts or input shapes
- Lost edges under contention (silently dropped in `FxHashMap` insert race)
- Race-induced inconsistent string interning (same content → two `StrRef`)
- Hook reindex storms when `flock -n` semantics are subtly wrong

Per project CLAUDE.md §Performance non-negotiables, performance and correctness in the parallel hot path are first-class invariants. This audit pins both before the parity work amplifies the surface.

## 1. Scope

### 1.1 In scope (crate-level)

- `cgn-core` (graph types, algorithms, cypher executor, registry, pool)
- `cgn-analyzer` (parsers, resolver, builder, post-process)
- `cgn-cli` (commands, hook spawn, background reindex)

### 1.2 Out of scope

- `cgn-mcp` — single-process stdio; no shared state with peer processes
- `vendor/` — third-party copies, governed by upstream
- The MCP `rmcp_tools` cache restructure (separate refactor, tracked via eywa hint)
- Hand-tuning Rayon thread counts in `search.rs` / `admin/index.rs` — workload is known too small to benefit (eywa-confirmed convention)

## 2. Methodology — B-primary, C-augmented, A-bounded

Three complementary lanes, deliberately overlapping so each finding is corroborated by ≥2 sources (user-preferred triangulation, per eywa).

| Lane | Tool | Catches | Verdict |
|------|------|---------|---------|
| **A. Inspection (bounded)** | grep + manual read | Design-level risk only — used only to populate the inventory tables in §3 | Bounded by §3 lists; not free-form |
| **B. Test-driven (primary)** | Equivalence tests under `--test-threads=1` and `--test-threads=N` | Concrete non-determinism — the bugs that change user-visible output | Authoritative for correctness |
| **C. Tooling-driven (augment)** | TSan (`-Z sanitizer=thread`, nightly), miri where viable, loom for any new sync primitive | Subtle races, UB, undefined ordering | Authoritative for race-class bugs |

A finding promoted to "bug" only if it shows up in B **or** C. Inspection-only suspicions go to §6 follow-up, not §5 bug list.

## 3. Inventory pass — 6 axes

For each axis we produce a table in the findings doc with one row per callsite:

| Axis | grep seed | Risk classification |
|------|-----------|---------------------|
| **3.1 Rayon parallel iterators** | `par_iter\|into_par_iter\|par_bridge\|par_chunks\|par_extend` | safe / unclear / racy |
| **3.2 Interior mutability** | `RefCell\|Cell\|UnsafeCell` | thread-local-only / crosses-threads-but-protected / crosses-threads-unprotected |
| **3.3 `unsafe` blocks** | `unsafe\s*{` | UB-free / requires-invariant-doc / suspect |
| **3.4 Shared mutex / atomic state** | `Arc<Mutex\|Arc<RwLock\|AtomicU\|AtomicI\|AtomicBool\|AtomicUsize` | bounded / unbounded contention / lock-order risk |
| **3.5 File locks** | `flock\|fs2::\|fd_lock\|FileExt::lock` | non-blocking / blocking / not-released-on-panic |
| **3.6 Process / thread spawn** | `Command::spawn\|std::thread::spawn\|tokio::spawn` | fire-forget OK / needs-join / lifetime-tied |

Verdict labels are committed alongside each row so a future reader can audit the auditor.

## 4. Hot-path equivalence tests — 5 conditions to prove

Each becomes a `#[test]` in `crates/<crate>/tests/concurrency_<name>.rs`, run under both single-thread and multi-thread executor settings. Each MUST be invariant to thread count.

| # | Property | Test name | Crate |
|---|----------|-----------|-------|
| 4.1 | `pass2_emit_node_edges` produces identical `(source, target, rel_type, reason)` set in serial vs parallel — **per rel_type, not just aggregate** | `pass2_parallel_serial_identical_per_reltype` | analyzer |
| 4.2 | `GraphBuilder::add_graph(...)+build()` end-to-end produces an identical *canonical projection* — `(sorted Vec<Node>, sorted Vec<Edge>, sorted Vec<(StrRef, content)>)` BLAKE3-hashed — regardless of file ingest order. Rkyv field-padding bytes are not asserted; the projection is what consumers actually see | `graph_builder_order_independence` | analyzer |
| 4.3 | `Registry` concurrent writers from N processes converge to a single valid state with all entries present (uses `tempfile` + spawned processes to mirror real hook contention) | `registry_concurrent_writers_converge` | core |
| 4.4 | `StringPool::add` called concurrently with the same string returns the same `StrRef` (no shadow entries) | `string_pool_concurrent_intern_dedupes` | core |
| 4.5 | Two simultaneous PreToolUse hook spawns: exactly one acquires `flock -n` and proceeds with reindex, the other returns immediately. Both exit code 0. Test asserts: (a) the lock file exists during reindex and is released after, (b) `wait()` on both child PIDs reaps cleanly (no `EAGAIN`/zombie), (c) only one reindex side-effect (e.g. `graph.bin` mtime bump) is observed | `hook_concurrent_spawn_flock_serializes` | cli |

The existing test at `builder.rs:1819` (4.1's predecessor) only asserts on `(u32, u32, String)` tuples — we extend it to stratify per `RelType` and add `reason` to the equality predicate so a future divergence is localised, not aggregated away.

## 5. TSan pass

```bash
RUSTFLAGS="-Z sanitizer=thread" \
RUSTDOCFLAGS="-Z sanitizer=thread" \
cargo +nightly test --tests \
  -p cgn-core \
  -p cgn-analyzer \
  --target x86_64-unknown-linux-gnu \
  -- --test-threads=4
```

Two phases:
1. **Baseline run** — capture every report
2. **Filtering** — strip noise from `std`, `rayon-core`, third-party crates we don't own; what remains is in-tree

Acceptance: zero unfiltered TSan reports.

If TSan toolchain unavailable in CI, the audit script falls back to running B-tests under `MIRIFLAGS="-Zmiri-strict-provenance"` on `cgn-core` only (analyzer has tree-sitter FFI which miri can't run).

## 6. Performance findings — surfaced, not bundled

Per CLAUDE.md §Proactive Engineering, any perf issue found during the audit is surfaced explicitly with bundle / defer recommendation. The findings doc gets a dedicated `## Performance findings (surfaced)` section, one row per item:

| Field | Meaning |
|-------|---------|
| `location` | `file:line` |
| `observation` | Concrete pattern (e.g. "`Mutex` held across hot loop allocates"; "rayon `par_iter` on N<32 collection, overhead > benefit"; "`StringPool` exclusive lock 12% of build time"; "false sharing on `AtomicUsize` array") |
| `impact_estimate` | Rough latency / throughput delta with measurement basis |
| `recommendation` | `bundle-now` (changes audit conclusion) / `defer-to-perf-pr` (clean optimisation) / `documented-tradeoff` (eywa-confirmed not to touch) |

eywa-confirmed not-to-touch list (start of audit, expand as needed):
- `search.rs` cross-repo parallel fan-out — workload too small
- `admin/index.rs` file-level parallel — same
- MCP `rmcp_tools` cache structure — separate refactor

## 7. Deliverables

1. **`docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md`** — inventory tables (§3), test results (§4), TSan output (§5), perf findings (§6), and a final §Bugs-found section with each bug → fix PR linked
2. **New tests** under `crates/{core,analyzer,cli}/tests/concurrency_*.rs` — five files, one per §4 condition
3. **`scripts/audit-concurrency.sh`** — one-shot runner: builds with TSan, runs equivalence test suite, diffs serial vs parallel output, reports pass/fail. Wired as a manual `cargo xtask`-style helper, not CI-gated yet (CI gating tracked separately)
4. **`README.md` section** `## Concurrency invariants` — 5 invariants from §4, frozen as project contract
5. **Updated `crates/cgn-analyzer/src/resolution/builder.rs:1819` test** — extended per-rel-type stratification (see §4.1)

## 8. Acceptance criteria

All four MUST hold for audit closure:

1. Inventory tables in §3 list every callsite matched by the grep seeds; each row carries a verdict label
2. All 5 equivalence tests in §4 PASS under `--test-threads=1` and `--test-threads=$(nproc)` (CI runs both via `cargo test -- --test-threads=...`)
3. TSan run in §5 produces zero unfiltered reports on `cgn-core` + `cgn-analyzer`
4. Every bug surfaced in §4 or §5 has a merged fix PR before this audit's design doc is marked `Closed`. Perf findings in §6 may be deferred to follow-up issues, **but must be filed before closure** — they cannot disappear into "we'll see"

## 9. Effort estimate

| Phase | Best case | Expected | Worst case |
|-------|-----------|----------|------------|
| Inventory grep + table fill | 2 hr | 3 hr | 1 day |
| Write 5 equivalence tests | 1 day | 1.5 day | 2 days |
| TSan setup + baseline + filter | 4 hr | 1 day | 2 days (if toolchain quirks) |
| Bug fix work | 0 (no bugs) | 1 day | 5 days |
| Findings doc + README invariants | 2 hr | 4 hr | 1 day |
| **Total** | **~2 day** | **~3.5 day** | **~8 day** |

The expected case is the planning anchor; the worst case is when fixes are needed.

## 10. Risks

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| Nightly toolchain TSan flake | Medium | Pin nightly to a known-working revision in `rust-toolchain-tsan.toml`; document in script |
| `flock` semantics differ on macOS vs Linux vs WSL | Medium | Test 4.5 runs in container-pinned Linux CI; manual macOS verification documented |
| Equivalence test 4.2 sensitive to rkyv internal ordering | Low-medium | Normalise rkyv output: sort edges / nodes before hashing; document the canonical sort key |
| TSan reports masked by `rayon-core` internal use of `crossbeam-deque` | Medium | Suppression file `tsan-suppressions.txt` checked in with explicit comments per entry; never suppress in-tree code |
| Audit finds large-scale rework (e.g. `RefCell` used widely in shared structures) | Low (one quick grep just done — none found cross-thread) | If hit, scope down: fix the one path that the parity work touches, document rest as follow-up |

## 11. Open questions for spec review

1. **CI gating**: should `scripts/audit-concurrency.sh` block PR merge from Sub-project 1 onwards, or stay manual? Default in this spec: manual + documented + run by maintainer before each `cgn` release tag.
2. **TSan in CI**: nightly toolchain in CI is expensive (3-5× build time). Default: don't run in CI yet; run locally per release. Revisit if a race bug ships to users.
3. **Test 4.3 process model**: `registry_concurrent_writers_converge` mirrors real hook contention via spawned processes — that's heavier than thread-based simulation. Acceptable cost for one test?

## 12. Out of scope (explicit)

- Architectural refactor of `pass2` to use a different parallel model (work-stealing queue, etc.)
- Replacing `rayon` with `tokio::task::spawn_blocking` or any other executor
- Async-ifying the analyzer (it's CPU-bound, async wouldn't help)
- `cgn-mcp` audit
- `vendor/` audit
- Bundling perf refactors into the audit PR (surface to findings, defer to follow-up — see §6)

## 13. Dependencies / blocks

- **Blocks**: Sub-projects 1, 2, 3, 4, 5, 6 cannot merge until §8 acceptance criteria pass. Each downstream sub-project's own equivalence test (per its new emit sites) builds on the harness this audit lands.
- **Depends on**: nightly Rust toolchain on the development machine; CI Linux runner with `tsan` libs available (or local-only TSan as fallback per §11.2).

## 14. Transition

After spec approval and findings publication:
- If bugs found → fix-PR thread, audit re-runs, sign-off
- If clean → audit doc moves to `Closed`, Sub-project 1 brainstorming starts (Dead-enum activation: Defines / Imports / Implements)
