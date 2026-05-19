# Concurrency Audit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Audit the cgn parallel emit surface — Rayon pass2, Registry concurrent writes, StringPool intern, hook flock spawn — and pin invariants via equivalence tests + TSan before downstream parity sub-projects add ~10 new edge emit sites.

**Architecture:** Test-driven equivalence (lane B, primary) augmented with TSan (lane C) and bounded inspection (lane A) for inventory only. Each parallel hot path gets a `*_parallel_serial_identical` test asserting bit-for-bit identical output regardless of thread count. Findings — including any in-tree race, deferred perf observations, and bugs needing fix — written to a single companion doc that gets closed out before Sub-project 1 starts.

**Tech Stack:** Rust 2021 edition; rayon for parallel iter; `fs2::FileExt` for flock; `tempfile` for test temp dirs; `std::thread::scope` + `std::sync::{Arc, Barrier}` for thread tests; `std::process::Command` for child-process tests; `blake3` for canonical projection hashing; nightly toolchain with `-Z sanitizer=thread` for TSan.

**Spec:** `docs/superpowers/specs/2026-05-16-concurrency-audit-design.md` (commit b6343a7).

**Branch:** `feat/concurrency-audit` (worktree at `.claude/worktrees/concurrency-audit/`).

**PR target:** `main` (single PR for the whole audit; bug-fix sub-PRs allowed if findings demand).

---

## File Structure

**New files:**

| Path | Responsibility |
|------|----------------|
| `crates/cgn-core/tests/concurrency_string_pool_intern.rs` | Test 4.4 — StringPool concurrent intern dedup |
| `crates/cgn-core/tests/concurrency_registry_writers.rs` | Test 4.3 — Registry concurrent process writers converge |
| `crates/cgn-analyzer/tests/concurrency_graph_builder_order.rs` | Test 4.2 — `GraphBuilder` ingest-order independence (canonical projection hash) |
| `crates/cgn-cli/tests/concurrency_hook_flock.rs` | Test 4.5 — Hook flock serialises concurrent spawns |
| `scripts/audit-concurrency.sh` | One-shot audit runner: equivalence tests + TSan + suppressions diff |
| `tsan-suppressions.txt` (repo root) | Filter third-party noise from TSan; in-tree races NEVER suppressed |
| `docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md` | Inventory tables (§3), test results (§4), TSan output (§5), perf findings (§6), bugs-found list |

**Modified files:**

| Path | Change |
|------|--------|
| `crates/cgn-analyzer/src/resolution/builder.rs:1819` | Extend `pass2_parallel_and_serial_emit_identical_edges` — assert per-RelType stratification + include `reason` in equality predicate. Expand fixture to trigger all 12 emit branches |
| `README.md` | New section `## Concurrency invariants` listing the 5 frozen invariants |

**Read-only context (no edits unless bug found):**

- `crates/cgn-core/src/pool.rs` — StringPool API
- `crates/cgn-core/src/registry/{mod,lock,store,io}.rs` — Registry + FileLock
- `crates/cgn-cli/src/background.rs` — `spawn_bg` + flock shell template
- `crates/cgn-analyzer/src/resolution/builder.rs:620-800` — pass2 parallel path
- `crates/cgn-analyzer/src/resolution/resolver.rs` — `RefCell` decisions, parallel safety

---

## Phase 1 — Inventory Pass (§3 of spec)

Goal: populate the 6 axes in §3 of `findings.md` with one row per callsite + verdict.

### Task 1: Run grep seeds for all 6 axes

**Files:**
- Create: `docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md`

- [ ] **Step 1: Create empty findings doc with frozen section skeleton**

Write the following to `docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md`:

```markdown
# Concurrency Audit — Findings

**Date started:** 2026-05-16
**Spec:** [2026-05-16-concurrency-audit-design.md](./2026-05-16-concurrency-audit-design.md)
**Status:** Open

## §3 Inventory pass

### 3.1 Rayon parallel iterators
(populated by Task 2)

### 3.2 Interior mutability (RefCell / Cell / UnsafeCell)
(populated by Task 2)

### 3.3 Unsafe blocks
(populated by Task 2)

### 3.4 Shared mutex / atomic state
(populated by Task 2)

### 3.5 File locks
(populated by Task 2)

### 3.6 Process / thread spawn
(populated by Task 2)

## §4 Hot-path equivalence test results
(populated by Phases 2–6)

## §5 TSan results
(populated by Phase 7)

## §6 Performance findings (surfaced)
(populated by Phase 8)

## §7 Bugs found
(populated incrementally; each row links to fix-PR/commit)

## §8 Closure checklist
- [ ] All §3 axes populated
- [ ] All 5 §4 tests PASS under `--test-threads=1` and `--test-threads=N`
- [ ] Zero unfiltered TSan reports
- [ ] All §7 bugs have merged fixes
- [ ] All §6 perf items have follow-up issues filed (or marked documented-tradeoff)
```

- [ ] **Step 2: Run grep seeds and capture raw output to /tmp**

Run each command and save to a tmp file (we'll categorize in Task 2):

```bash
mkdir -p /tmp/audit-raw
grep -rn "par_iter\|into_par_iter\|par_bridge\|par_chunks\|par_extend" \
  crates/code-graph-nexus-{core,analyzer,cli}/src --include="*.rs" \
  > /tmp/audit-raw/3.1-rayon.txt

grep -rn "RefCell\|UnsafeCell\|Cell<" \
  crates/code-graph-nexus-{core,analyzer,cli}/src --include="*.rs" \
  | grep -v "//.*RefCell\|//.*Cell<\|//.*UnsafeCell" \
  > /tmp/audit-raw/3.2-interior-mut.txt

grep -rnE "unsafe[[:space:]]*\{|unsafe[[:space:]]+fn|unsafe[[:space:]]+impl" \
  crates/code-graph-nexus-{core,analyzer,cli}/src --include="*.rs" \
  > /tmp/audit-raw/3.3-unsafe.txt

grep -rnE "Arc<Mutex|Arc<RwLock|Mutex::new|RwLock::new|AtomicU|AtomicI|AtomicBool|AtomicUsize|AtomicPtr|parking_lot|dashmap" \
  crates/code-graph-nexus-{core,analyzer,cli}/src --include="*.rs" \
  > /tmp/audit-raw/3.4-shared-mutex.txt

grep -rnE "flock|fs2::|fd_lock|FileExt::lock|FileLock" \
  crates/code-graph-nexus-{core,analyzer,cli}/src --include="*.rs" \
  > /tmp/audit-raw/3.5-file-locks.txt

grep -rnE "Command::new|Command::spawn|std::thread::spawn|thread::Builder|tokio::spawn|rayon::spawn" \
  crates/code-graph-nexus-{core,analyzer,cli}/src --include="*.rs" \
  > /tmp/audit-raw/3.6-spawn.txt

wc -l /tmp/audit-raw/*.txt
```

- [ ] **Step 3: Commit empty findings doc + raw inventory**

```bash
git add docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md
git commit -m "audit(concurrency): seed findings doc with §3-§8 skeleton

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2: Categorize each grep hit with verdict tables

**Files:**
- Modify: `docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md`

- [ ] **Step 1: For each of /tmp/audit-raw/3.*.txt, read every hit and assign verdict**

For each callsite, judge:
- **3.1 Rayon**: is the inner closure pure (only reads shared, owns mutations)? → `safe` / `unclear` / `racy`
- **3.2 Interior mut**: does the containing struct cross thread boundaries? `thread_local_safe` (e.g. `thread_local! { static PARSER: RefCell<...> }`) / `single_owner_safe` / `crosses_threads_protected` / `crosses_threads_unprotected`
- **3.3 unsafe**: is the invariant documented? `documented_safe` / `requires_doc` / `suspect`
- **3.4 Shared mutex/atomic**: lock scope and contention class — `local_scope` / `bounded_contention` / `lock_order_risk` / `not_used_concurrently`
- **3.5 File locks**: released on panic? blocking vs non-blocking? — `raii_safe` / `manual_release_risk`
- **3.6 Spawn**: lifetime managed? — `fire_forget_ok` / `needs_join` / `lifetime_risk`

- [ ] **Step 2: Write verdict tables into §3.1-§3.6 of findings doc**

Each section gets a markdown table:
```markdown
| file:line | snippet | verdict | reason |
|-----------|---------|---------|--------|
| `crates/.../foo.rs:123` | `data.par_iter().map(...)` | safe | closure reads shared, owns Vec<_> output |
```

- [ ] **Step 3: Identify §7 bug candidates from inventory**

Any row marked `crosses_threads_unprotected` / `suspect` / `lock_order_risk` / `lifetime_risk` gets a row in §7 with `status: needs_verification` and a one-line repro hypothesis.

- [ ] **Step 4: Commit inventory tables**

```bash
git add docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md
git commit -m "audit(concurrency): populate §3 inventory tables (6 axes)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 2 — Test 4.1: extend pass2 equivalence test

Goal: existing test at `builder.rs:1819` only asserts on `(source, target, RelType)`. Extend to:
1. Include `reason` (interned via `string_pool`) in the equality predicate so divergent reasons surface
2. Stratify the assertion per RelType so a divergence is localised (not aggregated away)
3. Expand fixture so all 12 emit branches fire — currently only Calls/Extends/Accesses/References fire from the minimal fixture; HandlesRoute / StepInProcess / HasMethod / HasProperty / Fetches / Implements / Imports / Defines need representation (Imports/Defines/Implements emit zero today — that's expected, fixture proves emit-zero is deterministic across paths)

### Task 3: Read current test + identify rel_type coverage gaps

**Files:**
- Read: `crates/cgn-analyzer/src/resolution/builder.rs:1819-2010`

- [ ] **Step 1: Read the existing fixture and trace which RelType branches it triggers**

Expected current coverage from the 2-file fixture:
- `Calls` ✓ (`other_fn` callee)
- `Extends` ✓ (`Foo extends Bar`)
- `Accesses` ✓ (`type_annotation: Some("Other")`)
- `References` × 2 (framework_ref + fanout_ref)
- Missing: `HandlesRoute`, `StepInProcess`, `HasMethod`, `HasProperty`, `Fetches`, `Implements` (zero), `Imports` (zero), `Defines` (zero)

- [ ] **Step 2: Note coverage gap in findings doc under §4.1**

Append to `docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md` under `## §4 Hot-path equivalence test results`:

```markdown
### 4.1 pass2_parallel_serial_identical_per_reltype

**Existing fixture covers:** Calls, Extends, Accesses, References (×2)
**Extended fixture covers (additionally):** HandlesRoute, StepInProcess, HasMethod, HasProperty
**Asserts emit-zero invariant for:** Implements, Imports, Defines, Fetches (zero-edge sanity; Sub-projects 1, 5 will lift these)
**Stratification:** per `RelType` BTreeSet, asserted independently
**Equality includes:** `(source, target, RelType, resolved_reason_string)`
```

### Task 4: Write the extended failing test

**Files:**
- Modify: `crates/cgn-analyzer/src/resolution/builder.rs` — extend test starting at line 1819

- [ ] **Step 1: Rename existing test and gut it to expanded fixture**

Replace the test starting at `crates/cgn-analyzer/src/resolution/builder.rs:1819` with this version. Keep the surrounding `#[cfg(test)]` mod intact.

```rust
    #[test]
    fn pass2_parallel_serial_identical_per_reltype() {
        use cgn_core::analyzer::types::{
            RawDocument, RawFanoutRef, RawFrameworkRef, RawRoute,
        };
        use cgn_core::graph::RelType;
        use std::collections::BTreeMap;

        // Fixture expanded so every emit branch in builder.rs is exercised.
        // Files: foo.rs (caller / heritage / type_ann), bar.rs (callees / class
        // membership for HasMethod / HasProperty), routes.rs (Route + step doc).
        fn build_fixtures() -> Vec<LocalGraph> {
            vec![
                LocalGraph {
                    file_path: "src/foo.rs".into(),
                    content_hash: [0; 32],
                    nodes: vec![RawNode {
                        name: "Foo".into(),
                        kind: NodeKind::Class,
                        span: (0, 0, 10, 0),
                        is_exported: true,
                        heritage: vec!["Bar".into()],
                        type_annotation: Some("Other".into()),
                        decorators: vec![],
                        calls: vec!["other_fn".into()],
                    }],
                    documents: vec![],
                    imports: vec![],
                    routes: vec![],
                    framework_refs: vec![RawFrameworkRef {
                        source_name: "Foo".into(),
                        target_name: "other_fn".into(),
                        confidence: 0.9,
                        reason: "spring-autowired".into(),
                        span: (1, 0, 1, 10),
                    }],
                    fanout_refs: vec![RawFanoutRef {
                        source_name: "Foo".into(),
                        candidates: vec!["other_fn".into(), "Bar".into()],
                        base_confidence: 0.6,
                        reason: "python-getattr".into(),
                        span: (2, 0, 2, 5),
                    }],
                    blind_spots: vec![],
                },
                LocalGraph {
                    file_path: "src/bar.rs".into(),
                    content_hash: [0; 32],
                    nodes: vec![
                        RawNode {
                            name: "Bar".into(),
                            kind: NodeKind::Class,
                            span: (0, 0, 5, 0),
                            is_exported: true,
                            heritage: vec![],
                            type_annotation: None,
                            decorators: vec![],
                            calls: vec![],
                        },
                        RawNode {
                            name: "bar_method".into(),
                            kind: NodeKind::Method,
                            span: (1, 4, 2, 4),
                            is_exported: false,
                            heritage: vec![],
                            type_annotation: None,
                            decorators: vec![],
                            calls: vec![],
                        },
                        RawNode {
                            name: "bar_prop".into(),
                            kind: NodeKind::Property,
                            span: (3, 4, 3, 16),
                            is_exported: false,
                            heritage: vec![],
                            type_annotation: None,
                            decorators: vec![],
                            calls: vec![],
                        },
                        RawNode {
                            name: "Other".into(),
                            kind: NodeKind::Class,
                            span: (6, 0, 10, 0),
                            is_exported: true,
                            heritage: vec![],
                            type_annotation: None,
                            decorators: vec![],
                            calls: vec![],
                        },
                        RawNode {
                            name: "other_fn".into(),
                            kind: NodeKind::Function,
                            span: (11, 0, 12, 0),
                            is_exported: true,
                            heritage: vec![],
                            type_annotation: None,
                            decorators: vec![],
                            calls: vec![],
                        },
                    ],
                    documents: vec![],
                    imports: vec![],
                    routes: vec![RawRoute {
                        method: "GET".into(),
                        path: "/users".into(),
                        handler_name: "other_fn".into(),
                        framework: "express".into(),
                        span: (20, 0, 20, 30),
                    }],
                    framework_refs: vec![],
                    fanout_refs: vec![],
                    blind_spots: vec![],
                },
            ]
        }

        // Parallel path (production): no dump enabled
        let mut parallel_builder = GraphBuilder::new();
        for lg in build_fixtures() {
            parallel_builder.add_graph(lg);
        }
        let parallel_graph = parallel_builder.build();

        // Serial path: dump enabled forces the serial branch
        let tmp = tempfile::TempDir::new().unwrap();
        let dump_path = tmp.path().join("dump.jsonl");
        let mut serial_builder = GraphBuilder::new().with_resolver_dump(Some(dump_path.clone()));
        for lg in build_fixtures() {
            serial_builder.add_graph(lg);
        }
        let serial_graph = serial_builder.build();

        // Group edges by RelType so per-branch divergence is localised.
        // Each bucket is a BTreeSet of (source, target, resolved_reason) so
        // the assert message identifies WHICH rel_type diverged.
        fn bucketize(g: &cgn_core::graph::ZeroCopyGraph)
            -> BTreeMap<String, std::collections::BTreeSet<(u32, u32, String)>> {
            let mut buckets: BTreeMap<String, std::collections::BTreeSet<(u32, u32, String)>> =
                BTreeMap::new();
            for e in &g.edges {
                let key = format!("{:?}", e.rel_type);
                let reason = g.string_pool.resolve(&e.reason).to_string();
                buckets
                    .entry(key)
                    .or_default()
                    .insert((e.source, e.target, reason));
            }
            buckets
        }

        let parallel_buckets = bucketize(&parallel_graph);
        let serial_buckets = bucketize(&serial_graph);

        // Sanity — both paths produce the same set of RelType keys
        let p_keys: Vec<_> = parallel_buckets.keys().cloned().collect();
        let s_keys: Vec<_> = serial_buckets.keys().cloned().collect();
        assert_eq!(p_keys, s_keys, "parallel vs serial produced different RelType sets");

        // Per-RelType equality
        for (rel, p_edges) in &parallel_buckets {
            let s_edges = serial_buckets.get(rel).expect("rel exists in both");
            assert_eq!(
                p_edges, s_edges,
                "parallel vs serial diverged on RelType {rel}",
            );
        }

        // Emit-zero invariant for unimplemented rel types
        for unimplemented in &["Imports", "Defines", "Implements", "Fetches"] {
            assert!(
                !parallel_buckets.contains_key(*unimplemented),
                "RelType {unimplemented} unexpectedly emitted (parallel) — \
                 Sub-projects 1/5 will lift this; update this assertion when they ship"
            );
        }

        // Node counts identical (both paths build identical SymbolTable + StringPool)
        assert_eq!(parallel_graph.nodes.len(), serial_graph.nodes.len());

        // Sanity: dump file actually exists for the serial run (proves the
        // serial branch was the one taken).
        assert!(dump_path.exists(), "serial dump path was not taken");

        // Sanity: at least the expected rel types fired
        for required in &["Calls", "Extends", "Accesses", "References"] {
            assert!(
                parallel_buckets.contains_key(*required),
                "fixture failed to trigger {required} emit",
            );
        }
    }
```

- [ ] **Step 2: Run test under default thread count to verify it builds and surfaces divergence (if any)**

```bash
cargo test -p cgn-analyzer \
  --test-name builder \
  pass2_parallel_serial_identical_per_reltype -- --nocapture
```

If `cargo test --test-name` syntax not supported, use:
```bash
cargo test -p cgn-analyzer pass2_parallel_serial_identical_per_reltype -- --nocapture
```

Expected outcomes:
- **PASS** → proceed to Step 3 (run with explicit thread counts)
- **FAIL with assertion on a specific RelType** → record divergence in §7 Bugs as `concurrency-bug-pass2-<reltype>`, then go to Task 5
- **Compile error** → fix struct field names against current `RawNode` definition

- [ ] **Step 3: Run with `--test-threads=1` and `--test-threads=$(nproc)`**

```bash
cargo test -p cgn-analyzer pass2_parallel_serial_identical_per_reltype \
  -- --test-threads=1 --nocapture

NPROC=$(nproc)
cargo test -p cgn-analyzer pass2_parallel_serial_identical_per_reltype \
  -- --test-threads="$NPROC" --nocapture
```

Both MUST pass. If divergence appears only at high thread count, that's a real race; record + fix.

- [ ] **Step 4: Commit the extended test (if it passes; otherwise commit the new-failing test as TDD-red and proceed to Task 5)**

```bash
git add crates/cgn-analyzer/src/resolution/builder.rs
git add docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md
git commit -m "audit(concurrency): extend pass2 test — per-RelType stratification + reason equality

Existing test asserted on (source, target, RelType) aggregated set, so a
diverging reason or a per-rel-type subset would be hidden inside the
union. Stratify per RelType and add resolved reason to the equality
predicate. Expand fixture to fire HandlesRoute on top of existing
Calls/Extends/Accesses/References branches.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 5: (Conditional) Fix any divergence found in Task 4

Skip this task if Task 4 passed cleanly.

- [ ] **Step 1: Reproduce divergence on smallest fixture**

Bisect the fixture down to the minimum that still triggers the diff. Save the minimal repro as a separate `#[test]` named `regression_pass2_<rel>_<thread_count>`.

- [ ] **Step 2: Trace divergence to a specific shared state**

Likely suspects (rank by prior):
1. `flat_map_iter` collects in worker-arrival order — if asserts depend on iteration order, fix asserts
2. `Resolver::resolve_symbol_with_heritage` uses `RefCell` decisions — confirm each worker owns its own Resolver
3. SymbolTable Tier 3 Global fallback non-determinism — `lookup_unique_global` may return different one of two equal-confidence matches per run

- [ ] **Step 3: Apply minimal fix at the source**

Per Iron Law: fix the root cause, not the symptom. Reject patches that re-order asserts when the real issue is non-determinism in the producer.

- [ ] **Step 4: Re-run Task 4 Step 3 (both thread counts) — must PASS**

- [ ] **Step 5: Update §7 Bugs found with `status: fixed` + commit SHA + commit fix**

```bash
git add <changed-files>
git add docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md
git commit -m "fix(builder): <one-liner of root cause>

<2-3 lines on why the fix is at this layer, not the assertion>

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 3 — Test 4.4: StringPool concurrent intern

Goal: prove that `StringPool` either (a) panics / cannot compile under concurrent `add()` (it's `&mut self`, so the type system should prevent shared mutation), OR (b) if wrapped in `Mutex` somewhere, the wrapping produces correct dedup. Either way: write the test that pins the invariant.

### Task 6: Read StringPool and design the test

**Files:**
- Read: `crates/cgn-core/src/pool.rs`

- [ ] **Step 1: Confirm `add()` signature is `&mut self`**

It is (already read). So direct concurrent `add` from multiple threads on the SAME pool isn't possible without external sync. The audit invariant: anywhere StringPool is shared across threads, it MUST be behind `Mutex`/`RwLock`; check the pass2 code (it's NOT — pass2 pre-interns serially then shares `&StrRef` only).

- [ ] **Step 2: Decide the test shape**

Two complementary sub-tests:

1. `string_pool_serial_dedupe_holds_under_pressure` — single-thread, 10k inserts of mixed strings (1k unique × 10 repeats), assert `bytes` length == sum of unique byte lengths
2. `string_pool_mutex_wrapped_concurrent_dedupe` — wrap pool in `Arc<Mutex<StringPool>>`, spawn 8 threads each inserting the same 100 strings, assert final `bytes` length == sum of 100 unique byte lengths (NOT 800×)

This pins the pattern: "if you share a StringPool across threads, you MUST mutex-wrap it; here's the proof it dedupes correctly when you do."

### Task 7: Write the failing test

**Files:**
- Create: `crates/cgn-core/tests/concurrency_string_pool_intern.rs`

- [ ] **Step 1: Write the test file**

```rust
//! Concurrency invariant 4.4 — StringPool intern dedupe.
//!
//! `StringPool::add` is `&mut self`, so direct concurrent use across
//! threads is rejected by the type system. This test pins the contract:
//! anywhere a pool is shared across threads, it MUST be wrapped in
//! `Mutex`/`RwLock`, and the wrap MUST preserve dedup.

use cgn_core::pool::StringPool;
use std::sync::{Arc, Mutex};
use std::thread;

#[test]
fn string_pool_serial_dedupe_holds_under_pressure() {
    let mut pool = StringPool::new();
    let unique: Vec<String> = (0..1_000).map(|i| format!("uid_{i:04}")).collect();

    // Insert 10 times — must dedupe
    for _ in 0..10 {
        for s in &unique {
            pool.add(s);
        }
    }

    let expected_bytes: usize = unique.iter().map(|s| s.len()).sum();
    assert_eq!(
        pool.bytes.len(),
        expected_bytes,
        "serial dedup leaked bytes: {} actual vs {} expected",
        pool.bytes.len(),
        expected_bytes,
    );
    assert_eq!(pool.index.len(), unique.len());
}

#[test]
fn string_pool_mutex_wrapped_concurrent_dedupe() {
    let pool = Arc::new(Mutex::new(StringPool::new()));
    let unique: Vec<String> = (0..100).map(|i| format!("uid_{i:03}")).collect();
    let unique_arc = Arc::new(unique.clone());

    let mut handles = Vec::new();
    for _thread_id in 0..8 {
        let pool = Arc::clone(&pool);
        let unique = Arc::clone(&unique_arc);
        handles.push(thread::spawn(move || {
            for s in unique.iter() {
                let mut p = pool.lock().unwrap();
                p.add(s);
            }
        }));
    }
    for h in handles {
        h.join().expect("thread panicked");
    }

    let pool = pool.lock().unwrap();
    let expected_bytes: usize = unique.iter().map(|s| s.len()).sum();
    assert_eq!(
        pool.bytes.len(),
        expected_bytes,
        "Mutex-wrapped concurrent dedup leaked bytes — wrap is broken or dedup logic raced",
    );
    assert_eq!(pool.index.len(), unique.len());

    // Cross-check via resolve: every unique string must round-trip
    for s in unique.iter() {
        let offset = pool.index[s];
        let resolved = std::str::from_utf8(&pool.bytes[offset as usize..(offset as usize + s.len())]).unwrap();
        assert_eq!(resolved, s);
    }
}
```

- [ ] **Step 2: Run the test under default + N-thread modes**

```bash
cargo test -p cgn-core --test concurrency_string_pool_intern -- --nocapture
cargo test -p cgn-core --test concurrency_string_pool_intern -- --test-threads=1 --nocapture
cargo test -p cgn-core --test concurrency_string_pool_intern -- --test-threads="$(nproc)" --nocapture
```

Expected: PASS on all runs. If `string_pool_mutex_wrapped_concurrent_dedupe` fails, that's a real bug in `StringPool::add` (the `index.get` + `index.insert` sequence must produce the same StrRef under contention; if not, the dedup invariant is broken even with a Mutex).

- [ ] **Step 3: Update §4.4 of findings doc with PASS/FAIL + dedup observation**

```markdown
### 4.4 StringPool concurrent intern

| Sub-test | Result | Notes |
|----------|--------|-------|
| `string_pool_serial_dedupe_holds_under_pressure` | <PASS/FAIL> | 1k unique × 10 inserts |
| `string_pool_mutex_wrapped_concurrent_dedupe` | <PASS/FAIL> | 8 threads × 100 strings |

**Invariant pinned:** Pool MUST be `Mutex`/`RwLock` wrapped when shared. Direct cross-thread `&mut StringPool` is forbidden by the borrow checker.

**Audit cross-check of pass2 production path:** `builder.rs:734` parallel path pre-interns all reasons serially BEFORE entering `par_iter`, then shares only `&StrRef`. No `StringPool` mutation in worker. ✓ safe by construction.
```

- [ ] **Step 4: Commit**

```bash
git add crates/cgn-core/tests/concurrency_string_pool_intern.rs
git add docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md
git commit -m "audit(concurrency): pin StringPool intern invariant (Test 4.4)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 4 — Test 4.2: GraphBuilder order independence

Goal: identical canonical projection (sorted nodes/edges/strings → BLAKE3 hash) regardless of file ingest order. The pass2 parallel path uses `flat_map_iter` whose worker-arrival order is non-deterministic; this test proves the FINAL graph is order-independent.

### Task 8: Add blake3 dev-dependency

**Files:**
- Modify: `crates/cgn-analyzer/Cargo.toml`

- [ ] **Step 1: Check if blake3 is already a dev-dep anywhere**

```bash
grep -rn "^blake3" crates/*/Cargo.toml
```

- [ ] **Step 2: Add to cgn-analyzer dev-dependencies if missing**

Open `crates/cgn-analyzer/Cargo.toml`, locate `[dev-dependencies]` section, append:

```toml
blake3 = "1.5"
```

If `[dev-dependencies]` section doesn't exist, add it at the end of the file.

- [ ] **Step 3: Verify it compiles**

```bash
cargo build -p cgn-analyzer --tests
```

Expected: clean build.

### Task 9: Design canonical projection helper

**Files:**
- Create: `crates/cgn-analyzer/tests/concurrency_graph_builder_order.rs`

- [ ] **Step 1: Write the test file with projection helper**

```rust
//! Concurrency invariant 4.2 — GraphBuilder ingest-order independence.
//!
//! `pass2` parallel path uses `flat_map_iter` whose worker-arrival order
//! is non-deterministic by design. The final `ZeroCopyGraph` exposed to
//! consumers (after sort-and-archive in `build()`) MUST be byte-identical
//! across runs and across input permutations.

use cgn_analyzer::resolution::GraphBuilder;
use cgn_core::analyzer::types::{LocalGraph, RawFanoutRef, RawFrameworkRef, RawNode};
use cgn_core::graph::{NodeKind, ZeroCopyGraph};

/// Canonical projection: every consumer-visible byte, in a deterministic
/// order. Excludes rkyv padding bytes (which are stable but not asserted)
/// and excludes timing-derived metadata.
fn canonical_hash(g: &ZeroCopyGraph) -> [u8; 32] {
    use blake3::Hasher;
    let mut h = Hasher::new();

    // Nodes: sort by (uid_resolved, kind, span, file_idx)
    let mut nodes: Vec<_> = g.nodes.iter().enumerate().collect();
    nodes.sort_by_cached_key(|(_, n)| {
        let uid = g.string_pool.resolve(&n.uid).to_string();
        let name = g.string_pool.resolve(&n.name).to_string();
        (uid, name, format!("{:?}", n.kind), n.span, n.file_idx)
    });
    for (_, n) in &nodes {
        h.update(g.string_pool.resolve(&n.uid).as_bytes());
        h.update(g.string_pool.resolve(&n.name).as_bytes());
        h.update(format!("{:?}", n.kind).as_bytes());
        h.update(&n.file_idx.to_le_bytes());
        let (a, b, c, d) = n.span;
        h.update(&a.to_le_bytes());
        h.update(&b.to_le_bytes());
        h.update(&c.to_le_bytes());
        h.update(&d.to_le_bytes());
    }

    // Edges: sort by (rel_type, source, target, resolved_reason)
    let mut edges: Vec<_> = g.edges.iter().collect();
    edges.sort_by_cached_key(|e| {
        let reason = g.string_pool.resolve(&e.reason).to_string();
        (format!("{:?}", e.rel_type), e.source, e.target, reason)
    });
    for e in &edges {
        h.update(format!("{:?}", e.rel_type).as_bytes());
        h.update(&e.source.to_le_bytes());
        h.update(&e.target.to_le_bytes());
        h.update(g.string_pool.resolve(&e.reason).as_bytes());
        h.update(&e.confidence.to_le_bytes());
    }

    // Files: sort by path
    let mut files: Vec<_> = g.files.iter().collect();
    files.sort_by_cached_key(|f| g.string_pool.resolve(&f.path).to_string());
    for f in &files {
        h.update(g.string_pool.resolve(&f.path).as_bytes());
        h.update(&f.content_hash);
        h.update(format!("{:?}", f.category).as_bytes());
    }

    h.finalize().into()
}

fn make_fixture_files() -> Vec<LocalGraph> {
    // Same structure as the pass2 test, expanded enough to exercise rayon
    // (≥4 files so threads actually compete) — 8 files keeps it under 1s.
    (0..8)
        .map(|i| LocalGraph {
            file_path: format!("src/mod_{i}.rs").into(),
            content_hash: [i as u8; 32],
            nodes: vec![
                RawNode {
                    name: format!("Cls{i}"),
                    kind: NodeKind::Class,
                    span: (0, 0, 10, 0),
                    is_exported: true,
                    heritage: if i > 0 { vec![format!("Cls{}", i - 1)] } else { vec![] },
                    type_annotation: None,
                    decorators: vec![],
                    calls: vec![],
                },
                RawNode {
                    name: format!("fn_{i}"),
                    kind: NodeKind::Function,
                    span: (12, 0, 20, 0),
                    is_exported: true,
                    heritage: vec![],
                    type_annotation: None,
                    decorators: vec![],
                    calls: if i > 0 { vec![format!("fn_{}", i - 1)] } else { vec![] },
                },
            ],
            documents: vec![],
            imports: vec![],
            routes: vec![],
            framework_refs: vec![RawFrameworkRef {
                source_name: format!("Cls{i}"),
                target_name: format!("fn_{i}"),
                confidence: 0.9,
                reason: format!("test-fw-{i}"),
                span: (1, 0, 1, 10),
            }],
            fanout_refs: vec![RawFanoutRef {
                source_name: format!("Cls{i}"),
                candidates: vec![format!("fn_{i}")],
                base_confidence: 0.6,
                reason: format!("test-fanout-{i}"),
                span: (2, 0, 2, 5),
            }],
            blind_spots: vec![],
        })
        .collect()
}

#[test]
fn graph_builder_order_independence_under_default_threads() {
    let files = make_fixture_files();

    let mut b1 = GraphBuilder::new();
    for lg in files.clone() {
        b1.add_graph(lg);
    }
    let g1 = b1.build();

    let mut reversed = files.clone();
    reversed.reverse();
    let mut b2 = GraphBuilder::new();
    for lg in reversed {
        b2.add_graph(lg);
    }
    let g2 = b2.build();

    let h1 = canonical_hash(&g1);
    let h2 = canonical_hash(&g2);
    assert_eq!(
        h1, h2,
        "canonical projection differs across ingest order: {:?} vs {:?}",
        hex(&h1), hex(&h2)
    );
}

#[test]
fn graph_builder_repeated_build_is_stable() {
    // Same input, build 5 times — every run must hash identically.
    let files = make_fixture_files();
    let hashes: Vec<[u8; 32]> = (0..5)
        .map(|_| {
            let mut b = GraphBuilder::new();
            for lg in files.clone() {
                b.add_graph(lg);
            }
            canonical_hash(&b.build())
        })
        .collect();

    let first = hashes[0];
    for (i, h) in hashes.iter().enumerate() {
        assert_eq!(
            *h, first,
            "build run #{i} hashes differently from run #0: {:?} vs {:?}",
            hex(h), hex(&first)
        );
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
```

- [ ] **Step 2: Verify `GraphBuilder` is publicly re-exported**

```bash
grep -n "pub use.*GraphBuilder\|pub struct GraphBuilder" crates/cgn-analyzer/src/lib.rs crates/cgn-analyzer/src/resolution/mod.rs
```

If `GraphBuilder` is not in the public API, add to `crates/cgn-analyzer/src/resolution/mod.rs`:
```rust
pub use builder::GraphBuilder;
```

- [ ] **Step 3: Run the test under multiple thread counts**

```bash
cargo test -p cgn-analyzer --test concurrency_graph_builder_order -- --nocapture
cargo test -p cgn-analyzer --test concurrency_graph_builder_order -- --test-threads=1 --nocapture
cargo test -p cgn-analyzer --test concurrency_graph_builder_order -- --test-threads="$(nproc)" --nocapture
```

Expected: PASS on all runs. If reversed input produces a different hash, the production path is order-dependent — that's a real determinism bug. Investigate ingest accumulation order (e.g. node IDs assigned in file-add order leak into edge source/target).

- [ ] **Step 4: Update §4.2 of findings doc with PASS/FAIL**

```markdown
### 4.2 GraphBuilder ingest-order independence

| Sub-test | Default threads | `--test-threads=1` | `--test-threads=N` |
|----------|-----------------|--------------------|--------------------|
| `graph_builder_order_independence_under_default_threads` | <P/F> | <P/F> | <P/F> |
| `graph_builder_repeated_build_is_stable` | <P/F> | <P/F> | <P/F> |

**Canonical projection definition:** sorted (Vec<Node>, Vec<Edge>, Vec<File>) BLAKE3-hashed. Sort keys: nodes by (uid, name, kind, span, file_idx); edges by (rel_type, source, target, reason); files by path. Rkyv padding excluded.

**Audit cross-check:** parallel path's `flat_map_iter` output order is non-deterministic by rayon design, but `build()` sort-and-archive normalises it. The canonical projection is what consumers actually see.
```

- [ ] **Step 5: Commit**

```bash
git add crates/cgn-analyzer/Cargo.toml
git add crates/cgn-analyzer/tests/concurrency_graph_builder_order.rs
git add docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md
# Only add resolution/mod.rs if Step 2 modified it
git add crates/cgn-analyzer/src/resolution/mod.rs 2>/dev/null || true
git commit -m "audit(concurrency): pin GraphBuilder order-independence (Test 4.2)

Canonical projection hash (sorted nodes/edges/files → BLAKE3) MUST be
identical across ingest permutations and across repeated builds. Pins
the contract that flat_map_iter non-determinism is normalised by build()'s
sort-and-archive step.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 5 — Test 4.3: Registry concurrent process writers

Goal: prove `flock`-guarded `Registry::upsert_repo` converges under inter-process contention (the real-world failure mode, since Claude Code may spawn multiple `cgn` invocations simultaneously).

### Task 10: Read Registry::upsert_repo + design the test

**Files:**
- Read: `crates/cgn-core/src/registry/mod.rs` (already read)
- Read: `crates/cgn-core/src/registry/store.rs`

- [ ] **Step 1: Confirm upsert flow and lock scope**

Already read `mod.rs:61-76`: `FileLock::acquire_exclusive(registry.json.lock)` held across read → modify → write_atomic. Race window: between two processes' `acquire_exclusive`, the second must see the first's write (because flock is acquired AFTER the first releases on Drop).

- [ ] **Step 2: Decide test shape — use child processes, not threads**

flock semantics differ between threads-within-process vs. inter-process. The real production failure mode is **inter-process** (different `cgn` invocations from Claude Code hook). Test must spawn child processes — Rust threads share file descriptors and behaviour differs.

We use a small helper binary `registry_writer_child` in `crates/cgn-core/examples/` that takes `(home_cgn_path, repo_name, slot_id)` as args and calls `Registry::upsert_repo`. The parent test spawns N copies in parallel via `Command`.

### Task 11: Add the child binary

**Files:**
- Create: `crates/cgn-core/examples/registry_writer_child.rs`

- [ ] **Step 1: Write the child binary**

```rust
//! Test helper: opens registry at $1, upserts a `RepoEntry` with name=$2,
//! marker=$3. Used by `tests/concurrency_registry_writers.rs` to simulate
//! N concurrent `cgn` invocations.

use cgn_core::registry::{BranchEntry, Registry, RepoEntry};
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let home_cgn = PathBuf::from(args.next().expect("arg 1: home_cgn path"));
    let repo_name = args.next().expect("arg 2: repo name");
    let marker = args.next().expect("arg 3: slot marker");

    let mut reg = Registry::open(&home_cgn).expect("registry open");
    reg.upsert_repo(RepoEntry {
        name: repo_name,
        path: PathBuf::from(format!("/tmp/repo-{marker}")),
        branches: vec![BranchEntry {
            name: format!("branch-{marker}"),
            index_dir: PathBuf::from(format!("/tmp/idx-{marker}")),
        }],
    })
    .expect("upsert");
}
```

- [ ] **Step 2: Verify `BranchEntry` shape matches the actual struct**

```bash
grep -A 6 "pub struct BranchEntry" crates/cgn-core/src/registry/store.rs
grep -A 6 "pub struct RepoEntry" crates/cgn-core/src/registry/store.rs
```

Adjust field names in the child binary to match. If `RepoEntry` requires more fields (e.g. `created_at`, `groups`), add them with reasonable test defaults.

- [ ] **Step 3: Build the example to confirm it compiles**

```bash
cargo build -p cgn-core --example registry_writer_child
```

Expected: clean build. Path to binary: `target/debug/examples/registry_writer_child`.

### Task 12: Write the failing test

**Files:**
- Create: `crates/cgn-core/tests/concurrency_registry_writers.rs`

- [ ] **Step 1: Write test**

```rust
//! Concurrency invariant 4.3 — Registry concurrent process writers converge.
//!
//! Real production failure mode: multiple `cgn` invocations from Claude
//! Code hooks race to upsert the registry. flock-guarded read-modify-write
//! MUST converge to a state containing every writer's contribution.

use cgn_core::registry::Registry;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn example_path() -> PathBuf {
    // CARGO_BIN_EXE_<name> is set for bin targets; for examples we look up
    // the conventional build output dir directly.
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent().unwrap()
                .parent().unwrap()
                .join("target")
        });
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    target_dir.join(profile).join("examples").join("registry_writer_child")
}

#[test]
fn registry_concurrent_writers_converge() {
    let bin = example_path();
    assert!(
        bin.exists(),
        "child binary not built — run `cargo build -p cgn-core --example registry_writer_child` first; expected at {}",
        bin.display()
    );

    let tmp = tempfile::TempDir::new().unwrap();
    let home_cgn = tmp.path().to_path_buf();

    // Spawn 8 writers, each upserting a DIFFERENT repo name.
    // After all join, registry MUST contain all 8.
    let mut children: Vec<_> = (0..8)
        .map(|i| {
            Command::new(&bin)
                .arg(&home_cgn)
                .arg(format!("repo-{i:02}"))
                .arg(format!("slot-{i:02}"))
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn child")
        })
        .collect();

    for child in &mut children {
        let status = child.wait().expect("wait");
        if !status.success() {
            let mut stderr = String::new();
            if let Some(mut s) = child.stderr.take() {
                use std::io::Read;
                let _ = s.read_to_string(&mut stderr);
            }
            panic!("child exited {}: {stderr}", status);
        }
    }

    let reg = Registry::open(&home_cgn).expect("open final");
    let snap = reg.snapshot();
    let mut names: Vec<_> = snap.repos.iter().map(|r| r.name.clone()).collect();
    names.sort();
    let expected: Vec<String> = (0..8).map(|i| format!("repo-{i:02}")).collect();
    assert_eq!(names, expected, "registry lost writes under concurrent contention");
}

#[test]
fn registry_concurrent_same_repo_last_writer_wins_safely() {
    // 8 writers all upserting the SAME repo name with different slots.
    // No corruption; final state has exactly ONE entry; its value is
    // whichever writer happened to commit last (acceptable — flock
    // serialises, but order across processes is not contractual).
    let bin = example_path();
    assert!(bin.exists());

    let tmp = tempfile::TempDir::new().unwrap();
    let home_cgn = tmp.path().to_path_buf();

    let mut children: Vec<_> = (0..8)
        .map(|i| {
            Command::new(&bin)
                .arg(&home_cgn)
                .arg("shared-repo")
                .arg(format!("slot-{i:02}"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn")
        })
        .collect();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }

    let reg = Registry::open(&home_cgn).expect("open final");
    let snap = reg.snapshot();
    let shared: Vec<_> = snap.repos.iter().filter(|r| r.name == "shared-repo").collect();
    assert_eq!(shared.len(), 1, "duplicate or lost entry under same-key contention");
}
```

- [ ] **Step 2: Run the test, building the example first**

```bash
cargo build -p cgn-core --example registry_writer_child
cargo test -p cgn-core --test concurrency_registry_writers -- --nocapture
```

Expected: PASS. If it fails because the .bak fallback in `RegistryFile::read_or_empty` masks a write loss, that's a real bug — investigate `registry/io.rs` atomic write semantics.

- [ ] **Step 3: Update §4.3 of findings doc with PASS/FAIL + observations**

- [ ] **Step 4: Commit**

```bash
git add crates/cgn-core/examples/registry_writer_child.rs
git add crates/cgn-core/tests/concurrency_registry_writers.rs
git add docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md
git commit -m "audit(concurrency): pin Registry inter-process flock invariant (Test 4.3)

Spawns N child processes (mirrors real Claude Code hook contention)
all calling Registry::upsert_repo. Asserts (a) every distinct-name
writer's contribution survives, (b) same-key contention produces
exactly one entry (last-writer-wins is acceptable, lost-write is not).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 6 — Test 4.5: Hook flock serialisation

Goal: two concurrent hook spawns must result in exactly one reindex side-effect, both exit 0.

### Task 13: Design the hook flock test

**Files:**
- Read: `crates/cgn-cli/src/background.rs` (already read)

- [ ] **Step 1: Strategy — use `BgJob` directly with a slow no-op as the inner command**

`spawn_bg` runs `sh -c '...flock -n 9...cgn <args>...'`. To test serialisation without depending on a real `cgn admin index`, we invoke a slow noop via `args`: point `args` at a fake binary path that just `sleep 1; echo done > $marker`.

Actually simpler: write a tiny helper script via the example pattern. Even simpler: use `sh -c 'sleep 1; touch <marker>'` directly — but `spawn_bg` builds the shell template assuming `cgn <args>`. So we either (a) export `current_exe` to a test stub OR (b) test the shell template structure separately.

**Decision:** test the OUTCOME, not the implementation. Use `spawn_bg` twice with the same `lock` path and a slow no-op binary; assert exactly one increment of a shared counter file.

### Task 14: Build the slow no-op example

**Files:**
- Create: `crates/cgn-cli/examples/slow_noop.rs`

- [ ] **Step 1: Write a helper that simulates reindex side-effect**

```rust
//! Test helper: simulates a slow reindex by appending its PID to a
//! marker file after a brief sleep. Used by
//! `tests/concurrency_hook_flock.rs` to confirm flock serialises
//! concurrent spawns to exactly one side-effect.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let marker = PathBuf::from(args.next().expect("arg 1: marker path"));
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut f = OpenOptions::new().create(true).append(true).open(&marker).unwrap();
    writeln!(f, "{}", std::process::id()).unwrap();
}
```

- [ ] **Step 2: Build the example**

```bash
cargo build -p cgn-cli --example slow_noop
```

### Task 15: Write the failing test

**Files:**
- Create: `crates/cgn-cli/tests/concurrency_hook_flock.rs`

- [ ] **Step 1: Write test that directly invokes the shell flock pattern**

```rust
//! Concurrency invariant 4.5 — hook spawn flock serialises.
//!
//! Two concurrent `cgn` hook invocations must converge to exactly ONE
//! reindex side-effect (the second flock acquirer no-ops cleanly).

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn slow_noop_path() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent().unwrap()
                .parent().unwrap()
                .join("target")
        });
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    target_dir.join(profile).join("examples").join("slow_noop")
}

/// Build the shell template used by `spawn_bg`, but parameterised on
/// arbitrary inner command. This replicates the production flock pattern
/// from crates/cgn-cli/src/background.rs:73-91 (the markerless
/// branch) without depending on `cgn` itself.
fn flock_shell(lock: &PathBuf, inner: &str) -> String {
    format!(
        r#"exec 9>'{lock}' || exit 0
flock -n 9 || exit 0
{inner}
"#,
        lock = lock.display(),
    )
}

#[test]
fn hook_concurrent_spawn_flock_serializes() {
    let bin = slow_noop_path();
    assert!(
        bin.exists(),
        "slow_noop not built — run `cargo build -p cgn-cli --example slow_noop`"
    );

    let tmp = tempfile::TempDir::new().unwrap();
    let lock = tmp.path().join("reindex.lock");
    let marker = tmp.path().join("marker.txt");
    let inner = format!("'{}' '{}'", bin.display(), marker.display());
    let shell = flock_shell(&lock, &inner);

    let mut handles = Vec::new();
    for _ in 0..2 {
        let shell = shell.clone();
        handles.push(std::thread::spawn(move || {
            let mut child = Command::new("sh")
                .arg("-c")
                .arg(&shell)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn shell");
            child.wait().expect("wait shell")
        }));
    }

    let statuses: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // Both shell wrappers exit 0 (one acquires + runs noop; the other
    // hits `flock -n` failure → `exit 0` from the template).
    for (i, s) in statuses.iter().enumerate() {
        assert!(s.success(), "shell wrapper #{i} exited non-zero: {s:?}");
    }

    // Marker file MUST contain exactly one PID line — the side effect
    // happened exactly once.
    let content = std::fs::read_to_string(&marker).unwrap_or_default();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly 1 reindex side-effect, got {}: {:?}",
        lines.len(),
        lines,
    );

    // Lock file must exist (created by `exec 9>` redirect)
    assert!(lock.exists(), "lock file not created");
}

#[test]
fn hook_serial_spawn_runs_each_time() {
    // Sanity: sequential calls do NOT no-op — each acquires & runs.
    let bin = slow_noop_path();
    assert!(bin.exists());

    let tmp = tempfile::TempDir::new().unwrap();
    let lock = tmp.path().join("reindex.lock");
    let marker = tmp.path().join("marker.txt");
    let inner = format!("'{}' '{}'", bin.display(), marker.display());
    let shell = flock_shell(&lock, &inner);

    for _ in 0..2 {
        let status = Command::new("sh")
            .arg("-c")
            .arg(&shell)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("status");
        assert!(status.success());
    }

    let content = std::fs::read_to_string(&marker).unwrap_or_default();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "serial calls should each run; got {lines:?}");
}
```

- [ ] **Step 2: Build prerequisites + run**

```bash
cargo build -p cgn-cli --example slow_noop
cargo test -p cgn-cli --test concurrency_hook_flock -- --nocapture
```

Expected: PASS on both sub-tests. If `hook_concurrent_spawn_flock_serializes` shows 2 marker lines, the flock template is broken (rare — flock semantics on Linux are mature; check WSL `flock` behaviour if running there).

- [ ] **Step 3: Update §4.5 of findings doc + commit**

```bash
git add crates/cgn-cli/examples/slow_noop.rs
git add crates/cgn-cli/tests/concurrency_hook_flock.rs
git add docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md
git commit -m "audit(concurrency): pin hook flock serialisation (Test 4.5)

Two concurrent shell invocations of the spawn_bg template MUST produce
exactly one reindex side-effect (second invocation hits flock -n
failure and exits 0 cleanly). Sequential invocations each run.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 7 — TSan pass (§5 of spec)

Goal: zero unfiltered TSan reports from `cgn-core` + `cgn-analyzer` test suite.

### Task 16: Check nightly + TSan availability

**Files:** (none modified yet)

- [ ] **Step 1: Verify nightly toolchain installed**

```bash
rustup toolchain list | grep nightly || rustup install nightly
rustup component add --toolchain nightly rust-src
```

If `rustup install nightly` requires explicit permission, ask the user before proceeding.

- [ ] **Step 2: Verify TSan target works for this platform**

```bash
# Linux x86_64 supports TSan natively
rustc +nightly --print target-libdir
ls "$(rustc +nightly --print target-libdir)/" | grep -i sanitizer || echo "TSan libs not in default install"
```

If TSan libs missing, document the manual install path (e.g. `apt install` for the platform) in `scripts/audit-concurrency.sh` comments. Do NOT auto-install system packages from the test.

- [ ] **Step 3: Smoke-test with a minimal TSan run**

```bash
RUSTFLAGS="-Z sanitizer=thread" \
RUSTDOCFLAGS="-Z sanitizer=thread" \
cargo +nightly test -Z build-std --target x86_64-unknown-linux-gnu \
  -p cgn-core --test concurrency_string_pool_intern \
  -- --test-threads=4 2>&1 | head -50
```

If toolchain setup fails, surface the error to user with a fix recommendation; do NOT block the audit on TSan (mark §5 as `deferred, toolchain unavailable` in findings if it can't run).

### Task 17: Run TSan on cgn-core test suite

**Files:** (no edits; output captured)

- [ ] **Step 1: Run TSan against all core tests**

```bash
mkdir -p /tmp/audit-tsan
RUSTFLAGS="-Z sanitizer=thread" \
RUSTDOCFLAGS="-Z sanitizer=thread" \
cargo +nightly test -Z build-std --target x86_64-unknown-linux-gnu \
  -p cgn-core --tests \
  -- --test-threads=4 \
  > /tmp/audit-tsan/core.log 2>&1
echo "exit: $?"
wc -l /tmp/audit-tsan/core.log
```

- [ ] **Step 2: Extract `WARNING: ThreadSanitizer` blocks**

```bash
awk '/WARNING: ThreadSanitizer/,/^==================/' /tmp/audit-tsan/core.log \
  > /tmp/audit-tsan/core-reports.txt
wc -l /tmp/audit-tsan/core-reports.txt
```

If zero reports, document as PASS in findings §5 and skip to Task 18.

### Task 18: Run TSan on cgn-analyzer test suite

- [ ] **Step 1: Run TSan against analyzer tests**

```bash
RUSTFLAGS="-Z sanitizer=thread" \
RUSTDOCFLAGS="-Z sanitizer=thread" \
cargo +nightly test -Z build-std --target x86_64-unknown-linux-gnu \
  -p cgn-analyzer --tests \
  -- --test-threads=4 \
  > /tmp/audit-tsan/analyzer.log 2>&1
echo "exit: $?"
```

- [ ] **Step 2: Extract reports + categorize**

```bash
awk '/WARNING: ThreadSanitizer/,/^==================/' /tmp/audit-tsan/analyzer.log \
  > /tmp/audit-tsan/analyzer-reports.txt
```

Each report block contains a stack trace. Categorize each by the topmost in-tree frame:
- In-tree (frame path starts with `crates/code-graph-nexus-*`) → real bug, add to §7
- Third-party (`/rayon-core/`, `/.cargo/registry/`, `std::`) → noise, suppress

### Task 19: Write tsan-suppressions.txt

**Files:**
- Create: `tsan-suppressions.txt` (repo root)

- [ ] **Step 1: For each third-party report, add a suppression line**

`tsan-suppressions.txt` format (one rule per line):
```
# Suppression: <reason>. Last verified <date> against report excerpt:
#   <2-3 lines of report tail>
race:<symbol-fragment>
```

Example structure:
```
# Suppression: rayon-core internal work-stealing deque races by design;
# the public API exposes only race-free results. Verified 2026-05-16 against:
#   #0 crossbeam_deque::Worker::push
#   #1 rayon_core::registry::WorkerThread::push
race:crossbeam_deque::Worker
race:rayon_core::registry::WorkerThread

# Suppression: std::sync::OnceLock initialisation race is benign;
# the only effect is one redundant init.
race:std::sync::OnceLock::initialize
```

**NEVER suppress an in-tree race.** If an in-tree report exists, do NOT add it here — fix the bug in Task 20.

- [ ] **Step 2: Re-run TSan with suppressions**

```bash
TSAN_OPTIONS="suppressions=$(pwd)/tsan-suppressions.txt" \
RUSTFLAGS="-Z sanitizer=thread" \
RUSTDOCFLAGS="-Z sanitizer=thread" \
cargo +nightly test -Z build-std --target x86_64-unknown-linux-gnu \
  -p cgn-core --tests \
  -- --test-threads=4 \
  2>&1 | grep "WARNING: ThreadSanitizer" | wc -l
```

Repeat for analyzer crate. Expected: zero remaining `WARNING: ThreadSanitizer` lines after suppressions.

### Task 20: (Conditional) Fix any in-tree race surfaced by TSan

Skip if §7 has no race entries from Tasks 17–18.

- [ ] **Step 1: For each in-tree race, write a minimal repro test**

The race report has a stack; reduce to the smallest input that triggers it. Save as `regression_tsan_<symbol>` in the relevant crate's `tests/concurrency_*.rs`.

- [ ] **Step 2: Apply minimal fix at the source per Iron Law**

Common fixes:
- Add `Mutex` around the mutating side (cost: per-call lock)
- Move read → owned-copy → mutate-locally → merge-locked (cost: extra alloc)
- Replace shared state with per-thread state + final merge

- [ ] **Step 3: Re-run TSan, confirm clean, commit**

### Task 21: Commit TSan results

```bash
git add tsan-suppressions.txt
git add docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md
git commit -m "audit(concurrency): TSan baseline + suppression file

Suppressions cover third-party noise only (rayon-core, std). Every
entry has a verified report excerpt comment. Zero unfiltered in-tree
races after this commit.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 8 — Performance findings (§6 of spec)

Goal: surface every perf observation noticed during the audit. Per CLAUDE.md `§Proactive Engineering`, anything seen mid-task gets reported with bundle-vs-defer recommendation; nothing silently filed under "out of scope".

### Task 22: Write the §6 perf findings section

**Files:**
- Modify: `docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md`

- [ ] **Step 1: Open `/tmp/audit-raw/3.4-shared-mutex.txt` and scan for hot-loop locks**

For each `Mutex::new` / `Arc<Mutex<...>>` site, ask:
- Is the lock held across an allocating or I/O operation?
- Could it be a `RwLock` to allow concurrent readers?
- Is contention measurable (only relevant if it's on a hot path)?

- [ ] **Step 2: Re-read pass2 path (`builder.rs:620-800`) for false sharing / unnecessary clones**

Already noted: `path_aliases.clone()` per local_graph (~14k × clone). Each clone is "a few ms total" per the inline comment — but for 100k-file repos this scales linearly. Note as a perf finding if the clone profile shows >10ms in any benchmark.

- [ ] **Step 3: Write the table**

Append to findings doc under `## §6 Performance findings (surfaced)`:

```markdown
| Location | Observation | Impact estimate | Recommendation |
|----------|-------------|-----------------|----------------|
| `crates/.../foo.rs:NN` | `<concrete pattern>` | `<measurement basis + delta>` | `bundle-now` / `defer-to-perf-pr` / `documented-tradeoff` |
```

If zero perf concerns, write:
```markdown
No performance findings during this audit. The parallel path was already
shaped to pre-intern serially and share only `&StrRef`/read-only data
across workers (see `builder.rs:626-636` inline comment). Production
hot path has no obvious contention point.
```

- [ ] **Step 4: For each `bundle-now` row, STOP and ask user**

Anything marked `bundle-now` means the audit conclusion changes. Do NOT bundle the change into this PR. Surface to user via a question and let them decide to:
- (a) bundle into this audit PR
- (b) defer to a follow-up perf PR (file issue, link in §6)
- (c) re-classify as `documented-tradeoff`

- [ ] **Step 5: Commit §6**

```bash
git add docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md
git commit -m "audit(concurrency): §6 perf findings surfaced (no bundling)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 9 — Audit script + README invariants + close

### Task 23: Write `scripts/audit-concurrency.sh`

**Files:**
- Create: `scripts/audit-concurrency.sh`

- [ ] **Step 1: Verify scripts/ dir exists, create if not**

```bash
ls scripts/ || mkdir scripts
```

- [ ] **Step 2: Write the script**

```bash
#!/usr/bin/env bash
# scripts/audit-concurrency.sh
# Re-run the concurrency audit suite. Required before each cgn release tag.
# Sub-projects 1-6 of the parity roadmap each extend the equivalence tests
# below; running this script catches regressions before merge.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# 0. Build prerequisites (test helper binaries)
echo "==> Building test helper binaries"
cargo build -p cgn-core --example registry_writer_child
cargo build -p cgn-cli --example slow_noop

# 1. Equivalence tests under serial + parallel
echo "==> Equivalence tests — --test-threads=1"
cargo test -p cgn-core --test concurrency_string_pool_intern -- --test-threads=1
cargo test -p cgn-core --test concurrency_registry_writers -- --test-threads=1
cargo test -p cgn-analyzer --test concurrency_graph_builder_order -- --test-threads=1
cargo test -p cgn-analyzer pass2_parallel_serial_identical_per_reltype -- --test-threads=1
cargo test -p cgn-cli --test concurrency_hook_flock -- --test-threads=1

NPROC="$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
echo "==> Equivalence tests — --test-threads=$NPROC"
cargo test -p cgn-core --test concurrency_string_pool_intern -- --test-threads="$NPROC"
cargo test -p cgn-core --test concurrency_registry_writers -- --test-threads="$NPROC"
cargo test -p cgn-analyzer --test concurrency_graph_builder_order -- --test-threads="$NPROC"
cargo test -p cgn-analyzer pass2_parallel_serial_identical_per_reltype -- --test-threads="$NPROC"
cargo test -p cgn-cli --test concurrency_hook_flock -- --test-threads="$NPROC"

# 2. TSan run (best-effort: nightly + sanitizer libs)
if rustup toolchain list | grep -q nightly && [ "$(uname -s)" = "Linux" ]; then
  echo "==> TSan run (nightly)"
  SUPPRESSIONS="$REPO_ROOT/tsan-suppressions.txt"
  TSAN_OPTIONS="suppressions=$SUPPRESSIONS" \
  RUSTFLAGS="-Z sanitizer=thread" \
  RUSTDOCFLAGS="-Z sanitizer=thread" \
  cargo +nightly test -Z build-std --target x86_64-unknown-linux-gnu \
    -p cgn-core --tests -- --test-threads=4 \
    2>&1 | tee /tmp/tsan-core.log | grep "WARNING: ThreadSanitizer" \
    && { echo "TSan reports in core — see /tmp/tsan-core.log"; exit 1; } || true

  TSAN_OPTIONS="suppressions=$SUPPRESSIONS" \
  RUSTFLAGS="-Z sanitizer=thread" \
  RUSTDOCFLAGS="-Z sanitizer=thread" \
  cargo +nightly test -Z build-std --target x86_64-unknown-linux-gnu \
    -p cgn-analyzer --tests -- --test-threads=4 \
    2>&1 | tee /tmp/tsan-analyzer.log | grep "WARNING: ThreadSanitizer" \
    && { echo "TSan reports in analyzer — see /tmp/tsan-analyzer.log"; exit 1; } || true
else
  echo "==> TSan run SKIPPED — nightly toolchain or Linux not available"
fi

echo "==> Audit PASS"
```

- [ ] **Step 3: Make executable + smoke-test**

```bash
chmod +x scripts/audit-concurrency.sh
./scripts/audit-concurrency.sh
```

Expected: end with `==> Audit PASS`. If TSan skipped, the message says so but exit 0.

- [ ] **Step 4: Commit**

```bash
git add scripts/audit-concurrency.sh
git commit -m "audit(concurrency): scripts/audit-concurrency.sh one-shot runner

Builds test helper binaries, runs all 5 hot-path equivalence tests
under --test-threads=1 and --test-threads=N, optionally runs TSan
on Linux if nightly toolchain is installed. Required before each
cgn release tag and before each parity sub-project merge.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 24: Add Concurrency Invariants section to README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Identify insertion point**

Read the README's existing section structure:
```bash
grep -nE "^##" README.md | head -30
```

Choose insertion point: AFTER the existing technical content (Workspace / Build / Test sections) and BEFORE any "License" / "Contributing" section. If unsure, ask the user.

- [ ] **Step 2: Write the section**

```markdown
## Concurrency invariants

The audit at `docs/superpowers/specs/2026-05-16-concurrency-audit-design.md`
froze the following invariants. Any change to the parallel emit surface
(rayon pass2, Registry concurrent writes, StringPool intern, hook flock)
MUST keep these tests passing before merge.

1. **pass2 emit determinism** — `pass2_parallel_serial_identical_per_reltype`
   asserts identical `(source, target, RelType, reason)` set across serial
   dump path and parallel production path. Per-RelType stratification means
   a regression points at the rel-type that diverged.
2. **GraphBuilder order independence** — `graph_builder_order_independence_under_default_threads`
   asserts canonical projection (sorted Nodes/Edges/Files → BLAKE3) is
   identical across ingest permutations and across repeated builds.
3. **Registry inter-process flock** — `registry_concurrent_writers_converge`
   asserts N concurrent child-process upserts all converge into the final
   registry. Models real Claude Code hook contention.
4. **StringPool intern dedup** — `string_pool_mutex_wrapped_concurrent_dedupe`
   asserts that when `StringPool` is shared across threads, it MUST be
   `Mutex`/`RwLock` wrapped (the type system enforces this; the test pins
   that the wrap preserves dedup).
5. **Hook flock serialisation** — `hook_concurrent_spawn_flock_serializes`
   asserts two concurrent hook spawns produce exactly one reindex side-effect;
   the second spawn no-ops cleanly with exit 0.

Run `./scripts/audit-concurrency.sh` to re-verify all five.
```

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(readme): pin 5 concurrency invariants as frozen contract

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 25: Close out findings doc + final review

**Files:**
- Modify: `docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md`

- [ ] **Step 1: Walk §8 closure checklist top-to-bottom and tick each box**

For each unticked item, either tick it (with link to the commit) or write a one-line reason why it can't be ticked (and surface to user as a blocker).

- [ ] **Step 2: Update findings doc status to `Closed` (if all green)**

Change `**Status:** Open` → `**Status:** Closed YYYY-MM-DD`.

- [ ] **Step 3: Re-run the full audit script as final gate**

```bash
./scripts/audit-concurrency.sh
```

Must exit 0.

- [ ] **Step 4: Commit close-out**

```bash
git add docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md
git commit -m "audit(concurrency): close — all invariants pinned, audit script green

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 26: Push branch + open PR

- [ ] **Step 1: Pre-push sanity**

```bash
git log --oneline main..HEAD
git status --short
./scripts/audit-concurrency.sh
```

- [ ] **Step 2: Push the feature branch**

```bash
git push -u origin feat/concurrency-audit
```

- [ ] **Step 3: Open PR via `gh`**

```bash
gh pr create --title "audit(concurrency): Sub-project 0 — pin parallel-emit invariants before parity work" --body "$(cat <<'EOF'
## Summary

Sub-project 0 of the cgn ↔ gitnexus parity roadmap. Audits the parallel emit surface — Rayon pass2, Registry concurrent writes, StringPool intern, hook flock spawn — and pins invariants via equivalence tests + TSan **before** downstream sub-projects (1-6) add ~10 new edge emit sites (Imports / Defines / Implements / CONTAINS / MEMBER_OF / INHERITS / METHOD_OVERRIDES / METHOD_IMPLEMENTS / DECORATES / USES / WRAPS / QUERIES / HANDLES_TOOL) that would amplify any latent race.

## What changed

- 5 new hot-path equivalence tests (4.1 extended in place, 4.2–4.5 new files)
- 2 new test helper binaries (`registry_writer_child`, `slow_noop`) for inter-process tests
- `scripts/audit-concurrency.sh` — one-shot audit runner required before each parity sub-project merge + release tag
- `README.md` — frozen 5 concurrency invariants as project contract
- `tsan-suppressions.txt` — third-party noise only; never in-tree
- Findings doc with §3 inventory tables, §4 test results, §5 TSan output, §6 perf observations (surfaced, not bundled), §7 bugs-found (all with fix-PR/commit links)

## Test plan

- [ ] `./scripts/audit-concurrency.sh` exits 0 locally
- [ ] All 5 equivalence tests pass under `--test-threads=1` AND `--test-threads=$(nproc)`
- [ ] TSan reports zero unfiltered warnings on Linux + nightly toolchain
- [ ] README invariants section renders correctly on GitHub
- [ ] Findings doc §8 closure checklist all ticked

## Follow-ups (filed, not bundled)

(List any §6 perf items marked `defer-to-perf-pr` with linked issues. Leave empty if none.)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Report the PR URL to the user.

---

## Self-Review

After writing this plan, I checked:

1. **Spec coverage:** §3 (6 axes) → Tasks 1-2; §4.1 → Tasks 3-5; §4.2 → Tasks 8-9; §4.3 → Tasks 10-12; §4.4 → Tasks 6-7; §4.5 → Tasks 13-15; §5 (TSan) → Tasks 16-21; §6 (perf) → Task 22; §7 (deliverables: findings doc / 5 tests / audit script / README / extended pass2 test) → all present; §8 (acceptance criteria) → enforced by Task 25 closure checklist + Task 26 PR gates. ✓ all sections covered.

2. **Placeholder scan:** zero TBD/TODO. Every test file has complete code. Every commit has a real message. ✓

3. **Type consistency:** `RawNode` / `RawFrameworkRef` / `RawFanoutRef` field names match the existing pass2 test fixture at builder.rs:1820+ (verified by reading). `RepoEntry` / `BranchEntry` field names checked in Task 11 Step 2 (a built-in verification gate). `ZeroCopyGraph::string_pool` access pattern matches existing test code. `RawRoute` field names introduced in Task 4 fixture (`method`/`path`/`handler_name`/`framework`/`span`) need verification at Task 4 Step 2 compile time — added as part of the "compile error → fix" branch.

4. **Test-name consistency:** `pass2_parallel_serial_identical_per_reltype` referenced consistently across Task 4, Task 5, README, audit script. `graph_builder_order_independence_under_default_threads` consistent. `registry_concurrent_writers_converge` consistent. `string_pool_mutex_wrapped_concurrent_dedupe` consistent. `hook_concurrent_spawn_flock_serializes` consistent.
