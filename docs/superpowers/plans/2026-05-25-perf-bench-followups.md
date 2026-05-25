# Perf / Bench Follow-ups Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close 6 deferred perf/bench follow-ups (FU-002/003/004/005/007/008) in one PR — replace the `dedup_rows` Debug-string key with a structural byte key, expand cypher bench coverage, add `--analyze-runs`, pin parity clone commits, and record the #432+#433 stacked delta; FU-003 is profile-gated (parallelize only if it clears the ROI bar, else wontfix).

**Architecture:** FU-004 adds `Value::write_dedup_key` (structural byte serialization) and rewires the existing module-private `dedup_rows`. The three bench changes are pure Python edits to `benchmark_ecp.py`. FU-007 edits `benchmark_repos.py` clone logic. FU-005/FU-003 are measurement-first: run the bench / `--prof`, then act on the result.

**Tech Stack:** Rust (ecp-core cypher executor), Python 3 (benchmark scripts), `cargo test -p ecp-core`.

---

## File Structure

| File | Responsibility | FU |
|---|---|---|
| `crates/ecp-core/src/cypher/value.rs` | `Value::write_dedup_key` method | 004 |
| `crates/ecp-core/src/cypher/executor.rs` | `dedup_rows` rewire + inline tests | 004 |
| `scripts/benchmark/benchmark_ecp.py` | +4 cypher queries, `--analyze-runs` | 002, 008 |
| `scripts/parity/benchmark_repos.py` | pinned-commit clone | 007 |
| `scripts/parity/baselines.md` | recaptured numbers + pinned-shas header | 007 |
| `docs/perf-notes.md` | stacked-delta record (new) | 005 |
| `crates/ecp-analyzer/src/resolution/builder.rs` | pass16 par_iter (only if profile clears bar) | 003 |

---

## Task 1: FU-004 — `Value::write_dedup_key` structural key

**Files:**
- Modify: `crates/ecp-core/src/cypher/value.rs`
- Modify: `crates/ecp-core/src/cypher/executor.rs:337-340` (dedup_rows), tests in `executor.rs` mod tests (~line 1695+)

- [ ] **Step 1: Write the failing tests**

Append to the `mod tests { ... }` block in `crates/ecp-core/src/cypher/executor.rs` (the module already has `use super::*;`):

```rust
    #[test]
    fn dedup_rows_collapses_identical() {
        let mut rows = vec![
            vec![Value::Int(1), Value::Str("a".into())],
            vec![Value::Int(1), Value::Str("a".into())],
            vec![Value::Int(2), Value::Str("a".into())],
        ];
        dedup_rows(&mut rows);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn dedup_rows_distinguishes_floats() {
        let mut rows = vec![
            vec![Value::Float(1.0)],
            vec![Value::Float(1.5)],
            vec![Value::Float(1.0)],
        ];
        dedup_rows(&mut rows);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn dedup_rows_list_length_prefix_guard() {
        // ["a","b"] must not collide with ["ab"] — length-prefixing the
        // Str bytes is what prevents the boundary ambiguity.
        let mut rows = vec![
            vec![Value::List(vec![Value::Str("a".into()), Value::Str("b".into())])],
            vec![Value::List(vec![Value::Str("ab".into())])],
        ];
        dedup_rows(&mut rows);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn dedup_rows_distinguishes_int_float_same_value() {
        // Int(1) and Float(1.0) carry different tags → distinct rows.
        let mut rows = vec![vec![Value::Int(1)], vec![Value::Float(1.0)]];
        dedup_rows(&mut rows);
        assert_eq!(rows.len(), 2);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ecp-core --lib cypher::executor::tests::dedup_rows 2>&1 | tail -20`
Expected: COMPILE ERROR — `write_dedup_key` not found (tests reference the new behavior via `dedup_rows`, which will be rewired in Step 3; before the rewire `dedup_rows` still uses `format!` so `dedup_rows_distinguishes_int_float_same_value` and the list guard would pass on Debug-string accident, but the structural method does not yet exist). If all 4 pass against the old `format!` impl, that's fine — they still pin the contract; proceed to make the impl structural.

Note: the tests pin observable `dedup_rows` behavior, so some may pass against the old impl. The point of Step 3 is to switch the *mechanism* to structural keys while keeping these green.

- [ ] **Step 3: Add `write_dedup_key` to value.rs**

In `crates/ecp-core/src/cypher/value.rs`, add an `impl Value` block after the enum:

```rust
impl Value {
    /// Append a self-describing byte key for DISTINCT/UNION dedup into `buf`.
    ///
    /// Replaces `format!("{self:?}")`: no per-row Debug-string allocation,
    /// and the key is collision-free (a leading discriminant tag per variant
    /// plus length-prefixed bytes — so `["a","b"]` cannot alias `["ab"]`, and
    /// `Int(1)` cannot alias `Float(1.0)`). `f64`/`f32` go through `to_bits`
    /// so NaN/-0.0 hash by exact bit pattern, matching `PartialEq` row identity
    /// closely enough for dedup (two NaN rows dedup together, which is the
    /// desired "drop the visually-identical duplicate" behavior).
    pub fn write_dedup_key(&self, buf: &mut Vec<u8>) {
        match self {
            Value::Null => buf.push(0),
            Value::Bool(b) => {
                buf.push(1);
                buf.push(*b as u8);
            }
            Value::Int(i) => {
                buf.push(2);
                buf.extend_from_slice(&i.to_le_bytes());
            }
            Value::Float(f) => {
                buf.push(3);
                buf.extend_from_slice(&f.to_bits().to_le_bytes());
            }
            Value::Str(s) => {
                buf.push(4);
                buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
                buf.extend_from_slice(s.as_bytes());
            }
            Value::List(items) => {
                buf.push(5);
                buf.extend_from_slice(&(items.len() as u32).to_le_bytes());
                for item in items {
                    item.write_dedup_key(buf);
                }
            }
            Value::NodeRef { idx, .. } => {
                buf.push(6);
                buf.extend_from_slice(&idx.to_le_bytes());
            }
            Value::EdgeRef {
                src,
                tgt,
                rel_type,
                confidence,
                ..
            } => {
                buf.push(7);
                buf.extend_from_slice(&src.to_le_bytes());
                buf.extend_from_slice(&tgt.to_le_bytes());
                buf.push(*rel_type as u8);
                buf.extend_from_slice(&confidence.to_bits().to_le_bytes());
            }
        }
    }
}
```

- [ ] **Step 4: Rewire `dedup_rows` in executor.rs**

Replace `crates/ecp-core/src/cypher/executor.rs:337-340`:

```rust
fn dedup_rows(rows: &mut Vec<Vec<Value>>) {
    let mut seen = HashSet::new();
    let mut key = Vec::new();
    rows.retain(|row| {
        key.clear();
        for v in row {
            v.write_dedup_key(&mut key);
        }
        seen.insert(key.clone())
    });
}
```

- [ ] **Step 5: Run tests + full cypher suite to verify pass**

Run: `cargo test -p ecp-core --lib cypher 2>&1 | tail -20`
Expected: all dedup_rows tests PASS, no regressions in existing cypher tests.

- [ ] **Step 6: Clippy + fmt touched files**

Run: `cargo clippy -p ecp-core --lib 2>&1 | tail -10 && rustfmt --edition 2021 crates/ecp-core/src/cypher/value.rs crates/ecp-core/src/cypher/executor.rs`
Expected: no clippy warnings on the changed lines.

- [ ] **Step 7: Commit**

```bash
git add crates/ecp-core/src/cypher/value.rs crates/ecp-core/src/cypher/executor.rs
git commit -m "perf(cypher): structural dedup key replaces format!(\"{row:?}\") (FU-2026-05-24-004)"
```

---

## Task 2: FU-002 — bench cypher coverage (+4 queries)

**Files:**
- Modify: `scripts/benchmark/benchmark_ecp.py:382-435` (queries[] list)

- [ ] **Step 1: Append 4 query rows**

In `scripts/benchmark/benchmark_ecp.py`, inside the `queries: list[...] = [ ... ]` literal (after the `cypher decorator IN` row, before the closing `]` at line ~435), add:

```python
        # COLLECT() projection: aggregates child names into a list per parent.
        # Exercises the grouping accumulator's list-collection path, distinct
        # from scalar count(*). Without it, COLLECT regressions go undetected.
        (
            "cypher COLLECT",
            [
                str(args.binary),
                "cypher",
                "MATCH (c:Class)-[:Contains]->(m:Method) RETURN c.name, collect(m.name)",
                "--repo",
                str(args.repo),
            ],
            args.repo,
        ),
        # IN [literal,...] literal-list filter: distinct from `<lit> IN prop`
        # (decorator IN row). Exercises the literal-list membership predicate.
        (
            "cypher IN literal-list",
            [
                str(args.binary),
                "cypher",
                "MATCH (m:Method) WHERE m.name IN ['main','run','init'] RETURN count(*)",
                "--repo",
                str(args.repo),
            ],
            args.repo,
        ),
        # Multi-hop variable-length edge: exercises the frontier-expansion path
        # over 1..3 Calls hops, the most allocation-heavy traversal shape.
        (
            "cypher multi-hop Calls*1..3",
            [
                str(args.binary),
                "cypher",
                "MATCH (a:Method)-[:Calls*1..3]->(b:Method) RETURN count(*)",
                "--repo",
                str(args.repo),
            ],
            args.repo,
        ),
        # Multi-aggregate GROUP BY: groups all nodes by kind with a count,
        # exercising the grouped-accumulator + ORDER BY on an aggregate.
        (
            "cypher GROUP BY kind",
            [
                str(args.binary),
                "cypher",
                "MATCH (n) RETURN n.kind, count(*) ORDER BY count(*) DESC",
                "--repo",
                str(args.repo),
            ],
            args.repo,
        ),
```

- [ ] **Step 2: Run bench to verify the new queries execute and exceed 5 ms**

Run (build release binary first if needed):
```bash
cargo build -p egent-code-plexus --bin ecp --release 2>&1 | tail -2
python scripts/benchmark/benchmark_ecp.py --repo .sample_repo --skip-cold --runs 3 2>&1 | grep -A1 -iE "COLLECT|IN literal|multi-hop|GROUP BY"
```
Expected: each of the 4 new rows prints a median time. Confirm each median is >5 ms. If a row is sub-5ms, note it in the commit body (it still guards correctness even if cheap) OR swap the query for a heavier shape on `.sample_repo`.

- [ ] **Step 3: Commit**

```bash
git add scripts/benchmark/benchmark_ecp.py
git commit -m "test(bench): cover COLLECT / IN-list / multi-hop / GROUP BY cypher patterns (FU-2026-05-24-002)"
```

---

## Task 3: FU-008 — `--analyze-runs N`

**Files:**
- Modify: `scripts/benchmark/benchmark_ecp.py:291` (argparse), `:339-357` (analyze _bench calls), docstring near top

- [ ] **Step 1: Add the `--analyze-runs` argument**

After the existing `--runs` arg definition (line ~291), add:

```python
    ap.add_argument(
        "--analyze-runs",
        type=int,
        default=0,
        help="Repeats for the analyze (cold/baseline + incremental) phases. "
        "Default 0 means: imply --runs when --runs>1, else 1. Single-sample "
        "analyze has ~±20%% wall-time noise; use --analyze-runs 10 for CI "
        "regression detection.",
    )
```

- [ ] **Step 2: Resolve effective analyze_runs after parse**

Immediately after `args = ap.parse_args()` (find it; search `parse_args`), add:

```python
    analyze_runs = args.analyze_runs or (args.runs if args.runs > 1 else 1)
```

- [ ] **Step 3: Wire both analyze `_bench` calls**

Change the cold/baseline `_bench(...)` call (line ~341) `runs=1` → `runs=analyze_runs`, and the incremental `_bench(...)` call (line ~356) `runs=1` → `runs=analyze_runs`.

- [ ] **Step 4: Document the noise floor in the module docstring**

In the top-of-file docstring (the `"""..."""` block with the usage examples near line 7), add a line:

```
    # analyze phases default to 1 sample (~±20% wall noise); --runs N>1 or
    # --analyze-runs N raises analyze repeats for CI regression detection.
```

- [ ] **Step 5: Verify back-compat + multi-sample paths**

Run:
```bash
python scripts/benchmark/benchmark_ecp.py --repo .sample_repo --skip-cold --runs 1 2>&1 | grep -i "analyze (baseline)"
python scripts/benchmark/benchmark_ecp.py --repo .sample_repo --skip-cold --analyze-runs 2 2>&1 | grep -i "analyze"
```
Expected: first run does single-sample analyze (back-compat); second does 2 analyze samples without error.

- [ ] **Step 6: Commit**

```bash
git add scripts/benchmark/benchmark_ecp.py
git commit -m "test(bench): --analyze-runs N for multi-sample analyze phase (FU-2026-05-24-008)"
```

---

## Task 4: FU-007 — pin parity clone commits + recapture baselines

**Files:**
- Modify: `scripts/parity/benchmark_repos.py:25-48` (clone_repo + repo list)
- Modify: `scripts/parity/baselines.md`

- [ ] **Step 1: Read the current repo list to find which langs/URLs are cloned**

Run: `grep -nE "clone_repo|REPOS|http|github" scripts/parity/benchmark_repos.py | head -40`
Expected: a dict/list of `(name, url)` wave-1 lang repos. Note each URL.

- [ ] **Step 2: Resolve a pinned sha per repo**

For each repo URL found, resolve current upstream HEAD sha to pin against:
```bash
git ls-remote <url> HEAD
```
Record `(name, url, sha)` for each. (These shas become the pinned snapshot.)

- [ ] **Step 3: Change `clone_repo` to accept and checkout a pinned sha**

Replace `clone_repo` in `scripts/parity/benchmark_repos.py`:

```python
def clone_repo(name: str, url: str, sha: str) -> Path:
    repo_path = SAMPLE_DIR / name
    if not repo_path.exists():
        print(f"[↓] Cloning {name} ({url} @ {sha[:8]})...")
        subprocess.run(
            ["git", "clone", "--filter=blob:none", "--no-checkout", url, str(repo_path)],
            check=True,
            capture_output=True,
        )
        subprocess.run(
            ["git", "-C", str(repo_path), "checkout", sha],
            check=True,
            capture_output=True,
        )
    return repo_path
```

Update the repo registry to carry `sha` per entry and the call site to pass it. (Exact structure depends on Step 1 — if it is a list of `(name, url)` tuples, extend to `(name, url, sha)` and update the loop `for name, url in REPOS` → `for name, url, sha in REPOS` and the `clone_repo(name, url)` → `clone_repo(name, url, sha)`.)

- [ ] **Step 4: Recapture baselines with a release binary**

Run:
```bash
cargo build -p egent-code-plexus --bin ecp --release 2>&1 | tail -2
python scripts/parity/benchmark_repos.py 2>&1 | tee /tmp/baselines_recap.txt
```
Expected: cold-index timings per pinned wave-1 lang against the release binary.

- [ ] **Step 5: Update `baselines.md` with recaptured numbers + pinned-sha header**

Rewrite `scripts/parity/baselines.md`: add a header block recording (a) the date of recapture, (b) `release` binary profile, (c) the pinned `(name → sha)` table; replace the numeric rows with the Step 4 output. Mark the prior 2026-05-14 dev-profile numbers as superseded.

- [ ] **Step 6: Commit**

```bash
git add scripts/parity/benchmark_repos.py scripts/parity/baselines.md
git commit -m "test(parity): pin wave-1 clone commits + release-binary baseline recapture (FU-2026-05-24-007)"
```

---

## Task 5: FU-005 — stacked-delta measurement (PRs #432 + #433)

**Files:**
- Create: `docs/perf-notes.md`

- [ ] **Step 1: Confirm a pre-#432 baseline binary is reachable**

PRs #432/#433 are merged (`5bda7884`, `64c7cfd2`). Need a pre-#432 binary to compare. Check for a pre-#432 worktree binary, else build one:
```bash
git log --oneline 5bda7884~1 -1   # the pre-#432 commit
```
If no held-aside binary exists, build the baseline in a temp checkout:
```bash
git worktree add /tmp/ecp-pre432 5bda7884~1
cargo build --manifest-path /tmp/ecp-pre432/Cargo.toml -p egent-code-plexus --bin ecp --release 2>&1 | tail -2
```

- [ ] **Step 2: Build current-main release binary**

```bash
cargo build -p egent-code-plexus --bin ecp --release 2>&1 | tail -2
```

- [ ] **Step 3: Run both bench binaries, capture count(*) + decorator IN**

```bash
python scripts/benchmark/benchmark_ecp.py --repo .sample_repo --skip-cold --runs 10 \
  --binary /tmp/ecp-pre432/target/release/ecp 2>&1 | grep -iE "count\(\*\)|decorator IN" | tee /tmp/pre432.txt
python scripts/benchmark/benchmark_ecp.py --repo .sample_repo --skip-cold --runs 10 \
  --binary ./target/release/ecp 2>&1 | grep -iE "count\(\*\)|decorator IN" | tee /tmp/post433.txt
```
Expected: a median for each pattern on each binary.

- [ ] **Step 4: Compute deltas + decide**

Compute `(post - pre)/pre` for count(*) and decorator IN. Predicted: count(*) ~-67%, decorator IN ~-33%.
- If the deltas are roughly in line (compounding holds) → record in `docs/perf-notes.md`, done.
- If a delta is *positive* (regression) or stacking is clearly non-linear (one win cancels another) → STOP, surface the finding to the user with the numbers, and propose an optimization before writing anything as final. Do not silently optimize.

- [ ] **Step 5: Write `docs/perf-notes.md`**

Create `docs/perf-notes.md` recording: the two binaries compared (shas), `.sample_repo` corpus, `--runs 10`, the per-pattern medians, the computed stacked deltas, and a one-line conclusion (compounding confirmed / interference found). Reference PRs #432, #433, FU-2026-05-23-006.

- [ ] **Step 6: Clean up the temp worktree + commit**

```bash
git worktree remove /tmp/ecp-pre432 2>/dev/null || true
git add docs/perf-notes.md
git commit -m "docs(perf): stacked-delta record for PRs #432 + #433 (FU-2026-05-24-005)"
```

---

## Task 6: FU-003 — pass16 / dir_size parallelization (PROFILE-GATED)

**Files:**
- Possibly modify: `crates/ecp-analyzer/src/resolution/builder.rs` (pass16 loop ~line 849-905)

- [ ] **Step 1: Measure pass16 wall with --prof**

```bash
cargo build -p egent-code-plexus --bin ecp --release 2>&1 | tail -2
./target/release/ecp admin drop --repo .sample_repo
./target/release/ecp admin index --repo .sample_repo --prof 2>&1 | grep -i "pass16"
```
Expected: a `prof build.pass16_fetch_shape: X.XXXs` line.

- [ ] **Step 2: Decide gate**

- If pass16 is **<~0.05s**: rayon fan-out overhead would likely negate or reverse the gain. **Do NOT parallelize.** Mark FU-003 as `🚫 wontfix` with the measured number. Skip Steps 3-6. Record the decision (the measured number goes into the FOLLOWUPS_DONE archive in the separate chore PR, Task 7).
- If pass16 is **measurably worth parallelizing** (well above 0.05s and a clear fraction of wall): proceed to Step 3.

- [ ] **Step 3 (only if gate passes): Parallelize the pass16 loop**

Convert the `for (file_idx, lg) in self.local_graphs.iter().enumerate()` loop (builder.rs ~849) into a `par_iter().enumerate().filter_map(...)` that returns per-file `Vec<Edge>`, then flatten + sort by `(source, target)` to preserve deterministic edge order. Keep `content_cache` thread-local (each rayon task reads its own file). The `string_pool.add(&reason_str)` calls need a thread-safe interning strategy — collect `(route_idx, is_templated, reason_str)` per file in parallel, then do the `string_pool.add` + `Edge` construction serially after the join (string pool is not `Sync` for mutation).

- [ ] **Step 4 (only if gate passes): Verify edge count unchanged**

```bash
./target/release/ecp admin drop --repo .sample_repo
./target/release/ecp admin index --repo .sample_repo
./target/release/ecp cypher "MATCH ()-[r:Fetches]->() RETURN count(*)" --repo .sample_repo
```
Expected: identical Fetches count to a pre-change run (capture the baseline count BEFORE Step 3).

- [ ] **Step 5 (only if gate passes): 14-language parser coverage**

pass16 is parser-core edge construction. Run the analyzer test suite:
```bash
cargo test -p ecp-analyzer 2>&1 | tail -15
```
Expected: all green. If any Fetches-related per-language test exists, confirm it still passes.

- [ ] **Step 6 (only if gate passes): Commit**

```bash
rustfmt --edition 2021 crates/ecp-analyzer/src/resolution/builder.rs
git add crates/ecp-analyzer/src/resolution/builder.rs
git commit -m "perf(build): parallelize pass16 fetch-shape per-file scan (FU-2026-05-23-003)"
```

---

## Task 7: FOLLOWUPS bookkeeping (SEPARATE chore PR)

**Per repo protocol, this is NOT part of the feature PR.** The feature PR
references FU ids in its description only.

- [ ] **Step 1: After the feature PR has a number, in a separate `chore/followups-bench-cleanup` worktree**, move the 6 entries from `/home/enor/code-graph-nexus/.claude/FOLLOWUPS.md` `## Open` to `FOLLOWUPS_DONE.md`:
  - FU-2026-05-24-002, -004, -005, -007, -008 → `✅ done in PR #N`.
  - FU-2026-05-23-003 → `✅ done in PR #N` if parallelized, else `🚫 wontfix` with the measured pass16 number.
  - Leave a one-line stub for each in the Open file.

- [ ] **Step 2: Commit in the chore worktree** (not this feature branch).

---

## Self-Review Notes

- **Spec coverage:** all 6 FU map to Tasks 1-6; FOLLOWUPS bookkeeping → Task 7. ✓
- **FU-003 gate:** Task 6 Step 2 makes the wontfix path explicit with the measured number, matching the spec's FU-032 precedent. ✓
- **FU-005 decision branch:** Task 5 Step 4 STOPs and surfaces on regression/non-linearity instead of silently optimizing, matching the spec + user directive. ✓
- **No placeholders:** every code step shows full code; FU-007 Step 3 notes the registry-shape dependency on Step 1's discovery (legitimate — the exact tuple arity is read at execution, both branches specified). ✓
- **Type consistency:** `write_dedup_key(&self, buf: &mut Vec<u8>)` signature identical across value.rs definition and executor.rs call site. ✓
