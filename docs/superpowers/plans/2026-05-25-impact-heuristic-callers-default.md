# Surface Heuristic Callers by Default — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ecp impact` and `ecp review` surface heuristic callers (MirrorsField / EventTopicMirror edges) by default — in a separate, clearly-tagged `heuristic_callers` bucket — instead of hiding them behind an opt-in `--include-heuristic` flag.

**Architecture:** The heuristic edges are already traversed by impact's BFS and returned as a separate `heuristic_results` array; risk/coverage already exclude them (`coverage_bfs_for_symbol` passes `include_heuristic: false`). This change flips the default-visibility: always emit a `heuristic_callers` payload key (tagged `requires_verification: true`), invert the CLI flag to `--no-heuristic`, and set `review`'s `run_impact` to include them. Because `impact` renders via the generic `emit()` (text = pretty-JSON of the same `Value`), no hand-rolled text renderer is needed — a new top-level key renders for free across text/json/toon.

**Tech Stack:** Rust, clap (CLI args), serde_json (`Value` payloads), the workspace's `egent-code-plexus` crate. Build: `cargo build -p egent-code-plexus --bin ecp`. Test: `cargo test -p egent-code-plexus --tests`.

---

## File Structure

- **Modify** `crates/ecp-cli/src/commands/impact.rs`
  - `ImpactArgs.include_heuristic` field (impact.rs:102-105) → rename to `no_heuristic` with inverted default + clap flag.
  - `attach_heuristic_fields` (impact.rs:1361-1382) → always emit `heuristic_callers` (tagged), drop the `include_heuristic` gate on emission.
  - All `args.include_heuristic` / literal `include_heuristic` call sites that drive `run_bfs` (impact.rs:280, 823, 975, 1024, 1277, 1284, 1301, 1325) → derive `include_heuristic = !args.no_heuristic`.
  - The stderr `note: N heuristic edges hidden` block (impact.rs:405-409) → only fires under `--no-heuristic`.
- **Modify** `crates/ecp-cli/src/commands/review/aggregate.rs:101` — `include_heuristic: false` → `true`.
- **Test** `crates/ecp-cli/tests/impact_heuristic_callers.rs` (new) — default-visible behavior, `--no-heuristic` suppression, empty-array presence, risk isolation.

**Naming locked for cross-task consistency:**
- New arg field: `no_heuristic: bool` (clap `--no-heuristic`, `default_value_t = false`).
- Derived local in each consuming fn: `let include_heuristic = !args.no_heuristic;`
- New payload key: `heuristic_callers` (was `heuristic_edges`).
- Per-entry tag key: `requires_verification` (bool, always `true` for these entries).

---

## Task 1: Confirm current heuristic-edge plumbing with a characterization test

This locks the current behavior before changing it, and gives us a fixture with a real heuristic edge to assert against.

**Files:**
- Test: `crates/ecp-cli/tests/impact_heuristic_callers.rs` (create)

- [ ] **Step 1: Find an existing test fixture that already produces a heuristic edge**

Run:
```bash
cd crates/ecp-cli
grep -rln "EventTopicMirror\|MirrorsField" tests/ | head
```
Expected: at least `tests/silent_drop_event_mirrors_threshold.rs` (it builds a graph with a Redis publish/subscribe "orders" EventTopicMirror at confidence 0.85). Read it to see how it constructs the indexed graph + Engine, and reuse that construction pattern.

- [ ] **Step 2: Write a characterization test asserting CURRENT (pre-change) behavior**

Create `crates/ecp-cli/tests/impact_heuristic_callers.rs`. Model the graph-build + `Engine` setup on `silent_drop_event_mirrors_threshold.rs`. Assert the CURRENT default behavior so we can see it flip:

```rust
// Characterization: BEFORE the change, a default `impact` (no flag) must NOT
// include heuristic callers in the payload, and must report them as hidden.
// This test is INVERTED in Task 4 once the default flips.
#[test]
fn impact_default_hides_heuristic_callers_BEFORE() {
    let (engine, _tmp) = build_engine_with_event_mirror(); // helper modeled on the fixture file
    let args = default_impact_args_for("subscribe_orders"); // include_heuristic defaults false
    let payload = ecp_cli::commands::impact::build_payload(&args, &engine).unwrap();
    assert!(
        payload.get("heuristic_callers").is_none(),
        "pre-change: heuristic_callers must be absent by default"
    );
    assert!(
        payload["hidden_heuristic_edges"].as_u64().unwrap() >= 1,
        "pre-change: the mirror edge is counted as hidden"
    );
}
```

Note: check the exact public entry — `build_payload` vs `build_payload_with_hints` (impact.rs:~417). Use whichever returns the JSON `Value`. If only `build_payload_with_hints` is public, destructure `(payload, _hints)`.

- [ ] **Step 3: Run it to confirm current behavior holds**

Run: `cargo test -p egent-code-plexus --test impact_heuristic_callers BEFORE -- --nocapture`
Expected: PASS (proves the fixture + entry point work and current default hides heuristics).

- [ ] **Step 4: Commit**

```bash
git add crates/ecp-cli/tests/impact_heuristic_callers.rs
git commit -m "test: characterize current default-hidden heuristic-caller behavior"
```

---

## Task 2: Invert the CLI flag (`--include-heuristic` → `--no-heuristic`)

**Files:**
- Modify: `crates/ecp-cli/src/commands/impact.rs:102-105` (field def) and every `args.include_heuristic` / `include_heuristic` literal call site.

- [ ] **Step 1: Rename the arg field with inverted semantics**

Replace impact.rs:102-105:
```rust
    /// Include heuristic edges (MirrorsField, EventTopicMirror) in BFS.
    /// Default off keeps blast-radius results noise-free.
    #[arg(long, default_value_t = false)]
    pub include_heuristic: bool,
```
with:
```rust
    /// Suppress heuristic callers (MirrorsField, EventTopicMirror) from the
    /// blast radius. Default: heuristic callers ARE shown, in a separate
    /// `heuristic_callers` bucket tagged `requires_verification`. Pass this
    /// flag for a pure-deterministic blast radius.
    #[arg(long, default_value_t = false)]
    pub no_heuristic: bool,
```

- [ ] **Step 2: Update every consuming call site to derive from the inverted flag**

The BFS-driving call sites pass an `include_heuristic` bool. At impact.rs:975, 1277, 1301, 1325 (the `args.include_heuristic` reads), replace `args.include_heuristic` with `!args.no_heuristic`. For the literal-`false` internal calls (impact.rs:280 coverage-bfs, and the `coverage_bfs_for_symbol` call) **leave them as `false`** — coverage/risk must stay deterministic-only (that is the isolation we rely on).

Run to enumerate exact sites:
```bash
grep -n "include_heuristic" crates/ecp-cli/src/commands/impact.rs
```
For each line: if it READS `args.include_heuristic` → change to `!args.no_heuristic`. If it is a hardcoded `false` inside coverage BFS → leave it. If it is the `attach_heuristic_fields` arg → handled in Task 3. The `default_impact_args_for` helper / any `ImpactArgs { .. }` struct literal (impact.rs:823) → rename field `include_heuristic: false` to `no_heuristic: false`.

- [ ] **Step 3: Build to find every missed reference**

Run: `cargo build -p egent-code-plexus --bin ecp 2>&1 | grep -E "error\[|no_heuristic|include_heuristic"`
Expected: compile errors only at sites still naming `include_heuristic`. Fix each (struct literals in other files, tests). Re-run until clean.

- [ ] **Step 4: Commit**

```bash
git add crates/ecp-cli/src/commands/impact.rs
git commit -m "refactor: invert impact heuristic flag to --no-heuristic"
```

---

## Task 3: Always emit `heuristic_callers` (tagged), drop the visibility gate

**Files:**
- Modify: `crates/ecp-cli/src/commands/impact.rs` — `attach_heuristic_fields` (1361-1382) and its caller (1020-1027).

- [ ] **Step 1: Rewrite `attach_heuristic_fields` to always emit the tagged bucket**

Replace impact.rs:1361-1382 body. The signature's `include_heuristic` param now means "are heuristics included in BFS at all" (i.e. `!no_heuristic`); when suppressed we emit the hidden count, when shown we emit the bucket:

```rust
fn attach_heuristic_fields(
    result: &mut Value,
    hidden_heuristic_edges: u64,
    heuristic_results: Vec<Value>,
    include_heuristic: bool,
    explain_confidence: bool,
    confidence_threshold: f32,
) {
    result["hidden_heuristic_edges"] = json!(hidden_heuristic_edges);
    let heuristic_reached = heuristic_results.len() as u64;
    if include_heuristic {
        // Always present (empty array when none) so consumers can distinguish
        // "no heuristic caller" from "feature absent". Each entry tagged so an
        // LLM never mistakes a 0.85 lead for a 1.0 deterministic caller.
        let tagged: Vec<Value> = heuristic_results
            .into_iter()
            .map(|mut e| {
                if let Some(obj) = e.as_object_mut() {
                    obj.insert("requires_verification".to_string(), json!(true));
                }
                e
            })
            .collect();
        result["heuristic_callers"] = json!(tagged);
    }
    if explain_confidence {
        result["explain_confidence"] = json!({
            "threshold": confidence_threshold,
            "edges_filtered_by_tier": {
                "unknown_tier": heuristic_reached + hidden_heuristic_edges,
            },
        });
    }
}
```

- [ ] **Step 2: Update the caller to pass the inverted flag**

At impact.rs:1020-1027, change the `args.include_heuristic` argument to `!args.no_heuristic`:
```rust
    attach_heuristic_fields(
        &mut result_obj,
        hidden_heuristic_total,
        all_heuristic_results,
        !args.no_heuristic,
        args.explain_confidence,
        args.confidence_threshold,
    );
```
Also check the per-symbol path at impact.rs:1284 (`if args.include_heuristic && !heur_results.is_empty()`) — change to `if !args.no_heuristic` and rename the emitted key there from `heuristic_edges` to `heuristic_callers` with the same `requires_verification` tagging (extract a small `tag_heuristic(Vec<Value>) -> Vec<Value>` helper to avoid duplicating the map closure; place it next to `attach_heuristic_fields`).

- [ ] **Step 3: Gate the stderr "hidden" note on suppression only**

At impact.rs:405-409, the note should only fire when the user opted to suppress:
```rust
    if args.no_heuristic && hints.hidden_heuristic_edges > 0 {
        eprintln!(
            "note: {} heuristic callers suppressed (--no-heuristic); drop the flag to see them",
            hints.hidden_heuristic_edges
        );
    }
```
(Confirm `args` is in scope at this point in `run`; it is — `run(args, engine)` owns it.)

- [ ] **Step 4: Build**

Run: `cargo build -p egent-code-plexus --bin ecp 2>&1 | grep -E "error\[" || echo BUILD_OK`
Expected: BUILD_OK.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/src/commands/impact.rs
git commit -m "feat: emit heuristic_callers bucket by default, tagged requires_verification"
```

---

## Task 4: Flip the characterization test + add the new-behavior tests

**Files:**
- Modify: `crates/ecp-cli/tests/impact_heuristic_callers.rs`

- [ ] **Step 1: Replace the BEFORE test with the AFTER assertions**

Remove `impact_default_hides_heuristic_callers_BEFORE`. Add:

```rust
#[test]
fn impact_default_shows_heuristic_callers_tagged() {
    let (engine, _tmp) = build_engine_with_event_mirror();
    let args = default_impact_args_for("subscribe_orders"); // no_heuristic defaults false
    let payload = ecp_cli::commands::impact::build_payload(&args, &engine).unwrap();
    let callers = payload["heuristic_callers"]
        .as_array()
        .expect("heuristic_callers present by default");
    assert!(!callers.is_empty(), "the EventTopicMirror caller is shown");
    assert_eq!(
        callers[0]["requires_verification"], serde_json::json!(true),
        "each heuristic caller is tagged requires_verification"
    );
}

#[test]
fn impact_no_heuristic_suppresses_bucket() {
    let (engine, _tmp) = build_engine_with_event_mirror();
    let mut args = default_impact_args_for("subscribe_orders");
    args.no_heuristic = true;
    let payload = ecp_cli::commands::impact::build_payload(&args, &engine).unwrap();
    assert!(
        payload.get("heuristic_callers").is_none(),
        "--no-heuristic removes the bucket"
    );
    assert!(
        payload["hidden_heuristic_edges"].as_u64().unwrap() >= 1,
        "--no-heuristic restores the hidden count"
    );
}

#[test]
fn impact_deterministic_only_symbol_has_empty_heuristic_bucket() {
    let (engine, _tmp) = build_engine_with_event_mirror();
    // A symbol with NO mirror edge still gets the key, as an empty array.
    let args = default_impact_args_for("unrelated_plain_fn");
    let payload = ecp_cli::commands::impact::build_payload(&args, &engine).unwrap();
    assert_eq!(
        payload["heuristic_callers"], serde_json::json!([]),
        "present-but-empty distinguishes no-data from feature-absent"
    );
}
```

Add `unrelated_plain_fn` (a function with no mirror edge) to the `build_engine_with_event_mirror` fixture if not already present.

- [ ] **Step 2: Run all three**

Run: `cargo test -p egent-code-plexus --test impact_heuristic_callers -- --nocapture`
Expected: 3 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/ecp-cli/tests/impact_heuristic_callers.rs
git commit -m "test: heuristic_callers shown+tagged by default, suppressed by --no-heuristic"
```

---

## Task 5: Risk-isolation regression test

Proves the new default visibility does NOT inflate `risk_level` / coverage — the load-bearing invariant from the spec.

**Files:**
- Modify: `crates/ecp-cli/tests/impact_heuristic_callers.rs`

- [ ] **Step 1: Write the isolation test**

```rust
#[test]
fn heuristic_callers_do_not_affect_risk_or_coverage() {
    let (engine, _tmp) = build_engine_with_event_mirror();

    let shown = default_impact_args_for("subscribe_orders"); // heuristics shown
    let mut hidden = default_impact_args_for("subscribe_orders");
    hidden.no_heuristic = true;                               // heuristics suppressed

    let p_shown = ecp_cli::commands::impact::build_payload(&shown, &engine).unwrap();
    let p_hidden = ecp_cli::commands::impact::build_payload(&hidden, &engine).unwrap();

    // risk_level (and the deterministic `impact` array) must be byte-identical
    // regardless of heuristic visibility — they are computed from deterministic
    // callers only (coverage_bfs_for_symbol passes include_heuristic=false).
    assert_eq!(p_shown.get("risk_level"), p_hidden.get("risk_level"));
    assert_eq!(p_shown["impact"], p_hidden["impact"]);
}
```
If the payload has no top-level `risk_level` (it may live per-symbol or under coverage), assert on whatever the deterministic risk/coverage field actually is — inspect `build_payload` output first with `--nocapture` and a `dbg!(&payload)` if unsure, then assert the real field. Do NOT assert a field that does not exist.

- [ ] **Step 2: Run**

Run: `cargo test -p egent-code-plexus --test impact_heuristic_callers risk -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/ecp-cli/tests/impact_heuristic_callers.rs
git commit -m "test: prove heuristic visibility does not affect risk/coverage"
```

---

## Task 6: Wire `review` to include heuristic callers

**Files:**
- Modify: `crates/ecp-cli/src/commands/review/aggregate.rs:101`

- [ ] **Step 1: Inspect the ImpactArgs literal in run_impact**

Run: `grep -n "include_heuristic\|no_heuristic\|ImpactArgs" crates/ecp-cli/src/commands/review/aggregate.rs`
Expected: line ~101 sets `include_heuristic: false`. Since Task 2 renamed the field, this line currently FAILS to compile after Task 2 — confirm by building.

- [ ] **Step 2: Update the field to enable heuristics**

Change the struct-literal field from the old `include_heuristic: false` to:
```rust
        no_heuristic: false,
```
This makes review's `run_impact` surface heuristic callers (default-on) in its findings. (`false` = do not suppress.)

- [ ] **Step 3: Build the whole CLI + run review tests**

Run:
```bash
cargo build -p egent-code-plexus --bin ecp 2>&1 | grep -E "error\[" || echo BUILD_OK
cargo test -p egent-code-plexus --test '*review*' 2>&1 | grep -E "test result|FAILED"
```
Expected: BUILD_OK and no review-test regressions. If a review snapshot test asserts the absence of a `heuristic_callers` field, update that expectation to include the now-present bucket.

- [ ] **Step 4: Commit**

```bash
git add crates/ecp-cli/src/commands/review/aggregate.rs
git commit -m "feat: review surfaces heuristic callers by default"
```

---

## Task 7: Full-suite regression + lint + clippy

**Files:** none (verification only).

- [ ] **Step 1: Run the impact + auto-ensure + review suites**

Run:
```bash
cargo test -p egent-code-plexus --test impact_heuristic_callers \
  --test silent_drop_event_mirrors_threshold 2>&1 | grep -E "test result|FAILED"
cargo test -p egent-code-plexus --tests 2>&1 | grep -E "test result: FAILED|error\[" || echo ALL_GREEN
```
Expected: ALL_GREEN (no FAILED). Investigate any failure before proceeding.

- [ ] **Step 2: clippy on the touched crate**

Run: `cargo clippy -p egent-code-plexus --tests 2>&1 | grep -E "warning:|error:" || echo CLIPPY_CLEAN`
Expected: CLIPPY_CLEAN.

- [ ] **Step 3: rustfmt the touched files**

Run:
```bash
rustfmt --edition 2021 \
  crates/ecp-cli/src/commands/impact.rs \
  crates/ecp-cli/src/commands/review/aggregate.rs \
  crates/ecp-cli/tests/impact_heuristic_callers.rs
```

- [ ] **Step 4: End-to-end smoke on this repo**

Run:
```bash
cargo build -p egent-code-plexus --bin ecp 2>&1 | tail -1
./target/debug/ecp impact --target subscribe --repo . 2>&1 | head -30
```
Expected: a `heuristic_callers` key appears in the output (or an empty array if this repo has no mirror edge on that symbol) — no `--no-heuristic` needed. Try `--no-heuristic` and confirm the bucket disappears and the stderr suppression note appears.

- [ ] **Step 5: Commit any fmt changes**

```bash
git add -A
git commit -m "chore: rustfmt touched files" --allow-empty
```

---

## Task 8: Update the FOLLOWUPS open-item references (local, gitignored)

The spec deferred Saga promotion to FU-2026-05-25-008 and a doc fix to FU-009. No code FU is resolved by this PR, so this task only confirms no Open item is silently closed.

- [ ] **Step 1: Confirm no Open FU is resolved by this change**

Run: `grep -n "heuristic_callers\|include_heuristic\|impact.*heuristic" /home/enor/code-graph-nexus/.claude/FOLLOWUPS.md`
Expected: no Open entry matches (FU-008/009 are about Saga promotion + stale docs, not this presentation change). If a match exists, move it to DONE per protocol. Otherwise, no action.

---

## Self-Review Notes (completed by plan author)

- **Spec coverage:** Decision 1 (tagged default-visible) → Task 3+4. Decision 2 (json key `heuristic_callers`) → Task 3. Decision 3 (text via generic emit) → no renderer task needed; verified `emit` falls to pretty-JSON for the `impact` key. Decision 4 (flag inversion) → Task 2. Decision 5 (review) → Task 6. Risk isolation → Task 5. All covered.
- **Placeholder scan:** test bodies are concrete; the one soft spot is the fixture helper `build_engine_with_event_mirror` / `default_impact_args_for`, which Task 1 Step 1 explicitly derives from `silent_drop_event_mirrors_threshold.rs` rather than inventing — acceptable because the exact construction is repo-specific and must be read, not guessed.
- **Type consistency:** `no_heuristic: bool` field, `!args.no_heuristic` derived bool, `heuristic_callers` payload key, `requires_verification` tag — used identically across Tasks 2/3/4/5/6.
- **Risk:** the only correctness-sensitive change is Task 3's emission gate; Task 5 pins the risk-isolation invariant. The `build_payload` vs `build_payload_with_hints` entry-point ambiguity is flagged in Task 1 Step 2 for the implementer to resolve by reading.
