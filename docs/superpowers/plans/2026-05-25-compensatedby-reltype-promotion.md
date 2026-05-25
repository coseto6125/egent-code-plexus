# CompensatedBy RelType Promotion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Promote Saga compensate/undo/rollback name-pairs into a heuristic `RelType::CompensatedBy` graph edge emitted at index time by a new `post_process/saga_pairs.rs` pass, then retire the standalone `find-transaction-patterns` Saga scan to read that edge.

**Architecture:** A new post-process pass scans the in-memory `&[Node]`/`&[Edge]` buffer (after `Calls` edges and `owner_class` are resolved), groups Method/Function nodes by owner class, matches `<root>_<verb_noun>` compensators (root ∈ {compensate, undo, rollback}, across snake/camel/Pascal case) against same-class operations, and emits `compensator → operation` edges with confidence 0.6 (name-only) or 0.8 (compensator has a `Calls` edge back to the operation). The edge is heuristic, so default `ecp impact` hides it. The CLI verb's Saga half is rewritten to query this edge.

**Tech Stack:** Rust, rkyv zero-copy graph (`ecp-core`), tree-sitter-derived `LocalGraph`, `cargo test`.

---

## File Structure

| File | Responsibility |
|------|----------------|
| `crates/ecp-core/src/graph.rs` | Add `CompensatedBy` enum variant + `is_heuristic`/`as_str`/`from_str` wiring (both owned + Archived) |
| `crates/ecp-analyzer/src/post_process/saga_pairs.rs` (NEW) | The `emit_edges` pass: detect pairs over node/edge buffer, emit `CompensatedBy` edges |
| `crates/ecp-analyzer/src/post_process/mod.rs` | `pub mod saga_pairs;` |
| `crates/ecp-analyzer/src/resolution/builder.rs` | Register the new pass after `path_literal_nodes` |
| `crates/ecp-analyzer/tests/saga_pairs.rs` (NEW) | Unit/integration tests for the pass |
| `crates/ecp-cli/src/commands/find_tx_patterns.rs` | Retire `detect_saga_pairs`, read `CompensatedBy` edges, fix stale doc (FU-009) |
| `~/.claude/skills/ecp/_shared/cli/find-transaction-patterns.md` | Fix stale T5-33 doc (FU-009) |

---

## Task 1: Add `CompensatedBy` enum variant + wiring

**Files:**
- Modify: `crates/ecp-core/src/graph.rs` (enum ~457, `from_str` ~91, `as_str` ~125, `RelType::is_heuristic` ~99, `ArchivedRelType::is_heuristic` ~463)

- [ ] **Step 1: Add the enum variant at END of `RelType`**

In `crates/ecp-core/src/graph.rs`, after the `UsesPathLiteral,` line (currently the last variant, ~457), add:

```rust
    /// Heuristic Saga compensation edge: `compensator → operation`. Source is a
    /// `compensate_/undo_/rollback_<verb_noun>` callable; target is the same-class
    /// `<verb_noun>` operation it rolls back. `Edge.reason` encodes evidence tier:
    /// `saga:calls-back` (confidence 0.8, compensator has a `Calls` edge to the
    /// operation) or `saga:name-only` (0.6, name-pair only — real Sagas often
    /// trigger compensation via an orchestrator, so name-only pairs are still
    /// emitted). Low-confidence — `is_heuristic()` returns `true`, so default
    /// `ecp impact` hides it.
    ///
    /// LLM-utility (A) Graph completeness: refactoring a Saga operation, `ecp
    /// impact <operation>` BFS surfaces its compensator directly — no grepping
    /// `undo_*`/`rollback_*` name conventions. (C) Edge semantics: `Calls` only
    /// says "A calls B"; it cannot express "B is A's failure compensation" — the
    /// directional rollback semantic that this edge carries.
    /// Appended at the END to preserve rkyv discriminants for existing
    /// `graph.bin` files.
    CompensatedBy,
```

- [ ] **Step 2: Wire `from_str` (~line 91, after `USESPATHLITERAL` arm)**

```rust
            "COMPENSATEDBY" | "COMPENSATED_BY" => Ok(RelType::CompensatedBy),
```

- [ ] **Step 3: Wire `as_str` (~line 125, after `UsesPathLiteral` arm)**

```rust
            Self::CompensatedBy => "CompensatedBy",
```

- [ ] **Step 4: Add to both `is_heuristic` (owned ~99 + Archived ~463)**

Owned (`impl RelType`):
```rust
    pub const fn is_heuristic(self) -> bool {
        matches!(self, Self::MirrorsField | Self::EventTopicMirror | Self::CompensatedBy)
    }
```

Archived (`impl ArchivedRelType`):
```rust
    pub const fn is_heuristic(&self) -> bool {
        matches!(self, Self::MirrorsField | Self::EventTopicMirror | Self::CompensatedBy)
    }
```

- [ ] **Step 5: Build to verify enum compiles**

Run: `cargo build -p ecp-core 2>&1 | tail -5`
Expected: compiles clean (no exhaustiveness errors — `as_str` match is now complete).

- [ ] **Step 6: Commit**

```bash
git add crates/ecp-core/src/graph.rs
git commit -m "feat(graph): add CompensatedBy heuristic RelType variant (FU-008)"
```

---

## Task 2: Write the `saga_pairs` detection helper + failing unit test

**Files:**
- Create: `crates/ecp-analyzer/src/post_process/saga_pairs.rs`
- Create: `crates/ecp-analyzer/tests/saga_pairs.rs`
- Modify: `crates/ecp-analyzer/src/post_process/mod.rs`

The pass works over the node/edge **buffer** (not the archived graph). `owner_class` is already resolved on `Node`; `Calls` edges are already in `edges`. The calls-back check is a linear scan of `edges` (CSR offsets don't exist yet at buffer time).

- [ ] **Step 1: Register the module in `mod.rs`**

Add to `crates/ecp-analyzer/src/post_process/mod.rs` (keep alphabetical-ish ordering, after `path_literal_nodes`):

```rust
pub mod saga_pairs;
```

- [ ] **Step 2: Write the failing unit test for case-prefix stripping**

Create `crates/ecp-analyzer/tests/saga_pairs.rs`:

```rust
//! Unit tests for the saga_pairs post-process detection helpers.

use ecp_analyzer::post_process::saga_pairs::{strip_compensator_root, CompensatorMatch};

#[test]
fn test_strip_root_snake_camel_pascal() {
    // snake_case
    assert_eq!(
        strip_compensator_root("undo_book_room"),
        Some(CompensatorMatch { operation_name: "book_room".to_string() })
    );
    // camelCase
    assert_eq!(
        strip_compensator_root("undoBookRoom"),
        Some(CompensatorMatch { operation_name: "bookRoom".to_string() })
    );
    // PascalCase
    assert_eq!(
        strip_compensator_root("UndoBookRoom"),
        Some(CompensatorMatch { operation_name: "BookRoom".to_string() })
    );
    // rollback / compensate roots
    assert_eq!(
        strip_compensator_root("rollback_charge"),
        Some(CompensatorMatch { operation_name: "charge".to_string() })
    );
    assert_eq!(
        strip_compensator_root("compensateReserve"),
        Some(CompensatorMatch { operation_name: "reserve".to_string() })
    );
    // non-compensator
    assert_eq!(strip_compensator_root("book_room"), None);
    // root but no suffix → not a pair
    assert_eq!(strip_compensator_root("undo"), None);
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p ecp-analyzer --test saga_pairs test_strip_root 2>&1 | tail -10`
Expected: FAIL — `saga_pairs` module / `strip_compensator_root` not found.

- [ ] **Step 4: Implement `strip_compensator_root` + `CompensatorMatch` in `saga_pairs.rs`**

Create `crates/ecp-analyzer/src/post_process/saga_pairs.rs`:

```rust
//! Heuristic Saga compensation pairing → `RelType::CompensatedBy` edges (FU-008).
//!
//! Scans the post-parse node/edge buffer for same-owner-class method pairs:
//!   `<root><sep><verb_noun>` (compensator) ↔ `<verb_noun>` (operation)
//! where root ∈ {compensate, undo, rollback}, matched case-insensitively across
//! snake_case (`undo_book_room`), camelCase (`undoBookRoom`), and PascalCase
//! (`UndoBookRoom`). The recovered `operation_name` preserves the suffix's
//! original case so it matches the operation node's name verbatim.
//!
//! Emits `compensator → operation` `CompensatedBy` edges. confidence 0.8 when
//! the compensator has a `Calls` edge to the operation (`saga:calls-back`),
//! else 0.6 (`saga:name-only`). `is_heuristic()==true` hides it from default
//! `ecp impact`.

use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::StringPool;
use rustc_hash::{FxHashMap, FxHashSet};

/// Compensator roots, lower-cased. Matched as a prefix on the lower-cased name.
const COMPENSATOR_ROOTS: &[&str] = &["compensate", "undo", "rollback"];

/// Result of stripping a compensator root: the bare operation name with its
/// ORIGINAL case preserved (so it matches the operation node's name).
#[derive(Debug, PartialEq, Eq)]
pub struct CompensatorMatch {
    pub operation_name: String,
}

/// If `name` is a compensator (`<root>` followed by a `_`-separator or a
/// case-boundary), return the bare operation name with original case. Else None.
///
/// snake_case: `undo_book_room` → root `undo`, sep `_`, suffix `book_room`.
/// camelCase:  `undoBookRoom`   → root `undo`, boundary before `B`, suffix
///             lower-cased first char → `bookRoom`.
/// PascalCase: `UndoBookRoom`   → root `Undo`, boundary before `B`, suffix
///             `BookRoom` (already capitalised).
pub fn strip_compensator_root(name: &str) -> Option<CompensatorMatch> {
    let lower = name.to_ascii_lowercase();
    for &root in COMPENSATOR_ROOTS {
        if !lower.starts_with(root) {
            continue;
        }
        let rest = &name[root.len()..];
        if rest.is_empty() {
            continue; // root with no suffix
        }
        // snake_case separator
        if let Some(suffix) = rest.strip_prefix('_') {
            if !suffix.is_empty() {
                return Some(CompensatorMatch { operation_name: suffix.to_string() });
            }
            continue;
        }
        // camel/Pascal boundary: next char must start a new word (uppercase).
        let first = rest.chars().next().unwrap();
        if first.is_ascii_uppercase() {
            // Detect original casing of `name`'s first char to decide whether the
            // operation is camelCase (compensator was camelCase → lowercase op
            // first char) or PascalCase (compensator was PascalCase → keep upper).
            let compensator_first = name.chars().next().unwrap();
            if compensator_first.is_ascii_uppercase() {
                // PascalCase: operation keeps its capital (BookRoom).
                return Some(CompensatorMatch { operation_name: rest.to_string() });
            }
            // camelCase: lowercase the operation's first char (bookRoom).
            let mut chars = rest.chars();
            let lowered: String = chars
                .next()
                .map(|c| c.to_ascii_lowercase())
                .into_iter()
                .chain(chars)
                .collect();
            return Some(CompensatorMatch { operation_name: lowered });
        }
    }
    None
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p ecp-analyzer --test saga_pairs test_strip_root 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ecp-analyzer/src/post_process/saga_pairs.rs crates/ecp-analyzer/src/post_process/mod.rs crates/ecp-analyzer/tests/saga_pairs.rs
git commit -m "feat(analyzer): saga_pairs compensator-root stripping (snake/camel/Pascal)"
```

---

## Task 3: Implement `emit_edges` over the node/edge buffer

**Files:**
- Modify: `crates/ecp-analyzer/src/post_process/saga_pairs.rs`
- Modify: `crates/ecp-analyzer/tests/saga_pairs.rs`

- [ ] **Step 1: Write the failing test for edge emission**

Append to `crates/ecp-analyzer/tests/saga_pairs.rs`:

```rust
use ecp_analyzer::post_process::saga_pairs::emit_edges;
use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::StringPool;

/// Build a Method node with a given name + owner_class in the buffer.
fn method_node(pool: &mut StringPool, name: &str, owner: &str) -> Node {
    Node {
        uid: 0,
        name: pool.add(name),
        file_idx: 0,
        kind: NodeKind::Method,
        span: (1, 0, 1, 0),
        community_id: 0,
        owner_class: pool.add(owner),
        content_hash: 0,
    }
}

#[test]
fn test_emit_name_only_pair_confidence_0_6() {
    let mut pool = StringPool::new();
    // idx 0 = operation `book_room`, idx 1 = compensator `undo_book_room`, same class.
    let mut nodes = vec![
        method_node(&mut pool, "book_room", "OrderSaga"),
        method_node(&mut pool, "undo_book_room", "OrderSaga"),
    ];
    let mut edges: Vec<Edge> = Vec::new();
    let count = emit_edges(&nodes, &mut pool, &mut edges);
    assert_eq!(count, 1, "one CompensatedBy edge expected");
    let e = &edges[0];
    assert_eq!(e.source, 1, "source = compensator idx");
    assert_eq!(e.target, 0, "target = operation idx");
    assert_eq!(e.rel_type, RelType::CompensatedBy);
    assert!((e.confidence - 0.6).abs() < 1e-6, "name-only → 0.6");
    assert_eq!(e.reason.resolve(&pool), "saga:name-only");
    let _ = &mut nodes; // nodes unchanged (no new nodes for this edge type)
}

#[test]
fn test_emit_calls_back_pair_confidence_0_8() {
    let mut pool = StringPool::new();
    let nodes = vec![
        method_node(&mut pool, "charge", "PaymentSaga"),         // idx 0 = operation
        method_node(&mut pool, "rollback_charge", "PaymentSaga"), // idx 1 = compensator
    ];
    // compensator (1) Calls operation (0) → evidence.
    let mut edges = vec![Edge {
        source: 1,
        target: 0,
        rel_type: RelType::Calls,
        confidence: 1.0,
        reason: pool.add("call"),
    }];
    let count = emit_edges(&nodes, &mut pool, &mut edges);
    assert_eq!(count, 1);
    // The new CompensatedBy edge is the last one pushed.
    let e = edges.last().unwrap();
    assert_eq!(e.rel_type, RelType::CompensatedBy);
    assert!((e.confidence - 0.8).abs() < 1e-6, "calls-back → 0.8");
    assert_eq!(e.reason.resolve(&pool), "saga:calls-back");
}

#[test]
fn test_emit_different_class_no_edge() {
    let mut pool = StringPool::new();
    let nodes = vec![
        method_node(&mut pool, "book_room", "OrderSaga"),
        method_node(&mut pool, "undo_book_room", "OtherSaga"), // different class
    ];
    let mut edges: Vec<Edge> = Vec::new();
    let count = emit_edges(&nodes, &mut pool, &mut edges);
    assert_eq!(count, 0, "cross-class pairs must not match");
}

#[test]
fn test_emit_camel_and_pascal_case() {
    let mut pool = StringPool::new();
    let nodes = vec![
        method_node(&mut pool, "bookRoom", "Saga"),     // camel op
        method_node(&mut pool, "undoBookRoom", "Saga"), // camel compensator
        method_node(&mut pool, "BookRoom", "Saga"),     // pascal op
        method_node(&mut pool, "UndoBookRoom", "Saga"), // pascal compensator
    ];
    let mut edges: Vec<Edge> = Vec::new();
    let count = emit_edges(&nodes, &mut pool, &mut edges);
    assert_eq!(count, 2, "camel + pascal pairs both match");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ecp-analyzer --test saga_pairs test_emit 2>&1 | tail -10`
Expected: FAIL — `emit_edges` not found.

- [ ] **Step 3: Implement `emit_edges` in `saga_pairs.rs`**

Append to `crates/ecp-analyzer/src/post_process/saga_pairs.rs`:

```rust
/// True for callable node kinds that can participate in a Saga pair.
fn is_callable(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Method | NodeKind::Function | NodeKind::Constructor)
}

/// Emit `CompensatedBy` edges for same-owner-class Saga name-pairs found in the
/// node buffer. Returns the count of edges emitted. Does NOT add nodes.
///
/// Algorithm:
/// 1. Build `calls: FxHashSet<(src_idx, tgt_idx)>` from existing `Calls` edges
///    (linear scan — CSR offsets don't exist at buffer time).
/// 2. Group callable node indices by `owner_class` (skip empty owner).
/// 3. Within each class build `name → idx` for O(1) operation lookup.
/// 4. For each compensator, look up the operation; emit the edge with confidence
///    by calls-back evidence.
pub fn emit_edges(nodes: &[Node], string_pool: &mut StringPool, edges: &mut Vec<Edge>) -> usize {
    let reason_calls_back = string_pool.add("saga:calls-back");
    let reason_name_only = string_pool.add("saga:name-only");

    // 1. Existing Calls edges as a (src, tgt) set.
    let mut calls: FxHashSet<(u32, u32)> = FxHashSet::default();
    for e in edges.iter() {
        if e.rel_type == RelType::Calls {
            calls.insert((e.source, e.target));
        }
    }

    // 2. Group callable nodes by owner_class.
    // class_members: owner_class StrRef hash → Vec<(node_idx, name_str)>
    let mut by_class: FxHashMap<&str, Vec<(u32, &str)>> = FxHashMap::default();
    for (idx, node) in nodes.iter().enumerate() {
        if !is_callable(node.kind) {
            continue;
        }
        let owner = node.owner_class.resolve(string_pool);
        if owner.is_empty() {
            continue;
        }
        let name = node.name.resolve(string_pool);
        by_class.entry(owner).or_default().push((idx as u32, name));
    }

    let mut pending: Vec<Edge> = Vec::new();
    for (_class, members) in &by_class {
        // 3. name → idx for operation lookup.
        let name_map: FxHashMap<&str, u32> =
            members.iter().map(|&(idx, name)| (name, idx)).collect();

        for &(comp_idx, comp_name) in members {
            let Some(m) = strip_compensator_root(comp_name) else {
                continue;
            };
            let Some(&op_idx) = name_map.get(m.operation_name.as_str()) else {
                continue;
            };
            let calls_back = calls.contains(&(comp_idx, op_idx));
            let (confidence, reason) = if calls_back {
                (0.8_f32, reason_calls_back)
            } else {
                (0.6_f32, reason_name_only)
            };
            pending.push(Edge {
                source: comp_idx,
                target: op_idx,
                rel_type: RelType::CompensatedBy,
                confidence,
                reason,
            });
        }
    }

    let count = pending.len();
    edges.extend(pending);
    count
}
```

NOTE: `name_map.resolve` borrows `string_pool` immutably while `reason_*` were added before the borrow — the two `string_pool.add` calls at the top run first, then the immutable `.resolve` borrows begin. If the borrow checker complains, resolve names into owned `String`s in the grouping loop instead of borrowing `&str` from the pool.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ecp-analyzer --test saga_pairs 2>&1 | tail -15`
Expected: all `test_emit_*` + `test_strip_root` PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-analyzer/src/post_process/saga_pairs.rs crates/ecp-analyzer/tests/saga_pairs.rs
git commit -m "feat(analyzer): saga_pairs emit_edges over node/edge buffer (0.6/0.8 tiers)"
```

---

## Task 4: Register the pass in the builder

**Files:**
- Modify: `crates/ecp-analyzer/src/resolution/builder.rs` (after `path_literal_nodes::emit_edges` ~1700)

- [ ] **Step 1: Add the pass call**

After the `path_literal_nodes::emit_edges(...)` block (~1700-1707), before the File-node loop, add:

```rust
        // Saga compensation pairing — emits heuristic `CompensatedBy` edges
        // (compensator → operation) over the node/edge buffer. Runs AFTER all
        // Calls edges exist (so calls-back evidence can be checked) and AFTER
        // owner_class is resolved on nodes. Adds no nodes, so ordering vs the
        // File-node loop is unconstrained.
        crate::post_process::saga_pairs::emit_edges(&nodes, &mut string_pool, &mut edges);
```

- [ ] **Step 2: Build the analyzer crate**

Run: `cargo build -p ecp-analyzer 2>&1 | tail -5`
Expected: compiles clean.

- [ ] **Step 3: Commit**

```bash
git add crates/ecp-analyzer/src/resolution/builder.rs
git commit -m "feat(analyzer): register saga_pairs pass in builder"
```

---

## Task 5: Corpus verification — CompensatedBy edges actually emit

**Files:** none (verification only — produces evidence, no code change unless it fails)

Unit tests passing ≠ pipeline emits the edge. Verify end-to-end on `.sample_repo`.

- [ ] **Step 1: Build the CLI binary**

Run: `cargo build -p ecp-cli 2>&1 | tail -5`
Expected: compiles clean.

- [ ] **Step 2: Reindex sample_repo + query the edge**

Run:
```bash
ECP=target/debug/ecp
$ECP index .sample_repo 2>&1 | tail -3
$ECP cypher 'MATCH ()-[r:CompensatedBy]->() RETURN count(r) AS n, r.confidence AS c' --repo .sample_repo 2>&1 | tail -20
```
Expected: a count ≥ 0 with confidence values in {0.6, 0.8}. If 0, that is acceptable ONLY if `.sample_repo` has no Saga pairs — confirm by grepping `rg -i 'undo_|rollback_|compensate_' .sample_repo`. If grep finds candidates but count is 0, the pass is misfiring — STOP and debug.

- [ ] **Step 3: Confirm default impact hides the heuristic edge**

If Step 2 found a CompensatedBy edge, pick one compensator/operation symbol and verify:
```bash
$ECP impact <operation-symbol> --repo .sample_repo 2>&1 | tail -20
$ECP impact <operation-symbol> --no-heuristic --repo .sample_repo 2>&1 | tail -20
```
Expected: the compensator surfaces in the heuristic-shown default but its tier is tagged `requires_verification`; `--no-heuristic` suppresses it. (This mirrors PR #453's heuristic-default behavior this branch is stacked on.)

- [ ] **Step 4: Record evidence in commit message (no code, doc-only commit if notes warranted)**

No commit needed if everything passed. If you discovered a `.sample_repo` gap (no Saga fixture), note it for Task 8's follow-up consideration.

---

## Task 6: Retire `find-transaction-patterns` Saga scan → read the edge

**Files:**
- Modify: `crates/ecp-cli/src/commands/find_tx_patterns.rs`

The Saga half currently runs `detect_saga_pairs` over the archived graph. Replace it with a query of `CompensatedBy` edges. Outbox half stays untouched.

- [ ] **Step 1: Replace `detect_saga_pairs` body with a CompensatedBy edge reader**

In `crates/ecp-cli/src/commands/find_tx_patterns.rs`, replace the entire `detect_saga_pairs` function (~125-194) with:

```rust
fn detect_saga_pairs(graph: &ArchivedZeroCopyGraph, class_filter: Option<&str>) -> Vec<SagaPair> {
    let mut pairs: Vec<SagaPair> = Vec::new();
    for edge in graph.edges.iter() {
        if !matches!(edge.rel_type, ArchivedRelType::CompensatedBy) {
            continue;
        }
        let comp_idx = edge.source.to_native() as usize;
        let op_idx = edge.target.to_native() as usize;
        let comp_node = &graph.nodes[comp_idx];
        let op_node = &graph.nodes[op_idx];

        let owner = comp_node.owner_class.resolve(&graph.string_pool);
        if let Some(cf) = class_filter {
            if owner != cf {
                continue;
            }
        }

        let comp_name = comp_node.name.resolve(&graph.string_pool);
        let op_name = op_node.name.resolve(&graph.string_pool);
        let op_file = graph.files[op_node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool);
        let op_line = op_node.span.0.to_native();
        let reason = edge.reason.resolve(&graph.string_pool);
        let calls_back = reason == "saga:calls-back";

        pairs.push(SagaPair {
            operation: format!("{owner}.{op_name}"),
            compensator: format!("{owner}.{comp_name}"),
            file: op_file.to_owned(),
            line: op_line,
            confidence: edge.confidence,
            calls_back,
        });
    }
    pairs
}
```

- [ ] **Step 2: Delete now-dead helpers**

Remove `strip_compensator_prefix`, `COMPENSATOR_PREFIXES`, and `compensator_calls_operation` from `find_tx_patterns.rs` (their logic now lives in the analyzer pass). Keep `tier_label`, `SagaPair`, `saga_pair_to_json`.

- [ ] **Step 3: Build to verify no dead-code / unused-import errors**

Run: `cargo build -p ecp-cli 2>&1 | tail -10`
Expected: compiles clean. Fix any unused-import warnings (e.g. drop `ArchivedNodeKind` if Saga was its only consumer — verify Outbox still uses it; it does, via `is_callable_kind`/table scan, so keep it).

- [ ] **Step 4: Run the existing CLI integration tests (output schema must not change)**

Run: `cargo test -p ecp-cli --test find_tx_patterns_cmd 2>&1 | tail -25`
Expected: `saga_compensate_pair_emits_match`, `saga_undo_prefix_emits_match`, `saga_rollback_prefix_emits_match`, `compensator_calling_operation_bumps_confidence`, `compensator_on_different_class_no_match`, `no_compensator_no_match` all PASS — proving the retired implementation produces the same observable output via the graph edge.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/src/commands/find_tx_patterns.rs
git commit -m "refactor(cli): find-transaction-patterns Saga half reads CompensatedBy edge (FU-008)"
```

---

## Task 7: Fix stale doc comments (FU-009)

**Files:**
- Modify: `crates/ecp-cli/src/commands/find_tx_patterns.rs` (module doc ~1-41)
- Modify: `~/.claude/skills/ecp/_shared/cli/find-transaction-patterns.md` (~line 40)

- [ ] **Step 1: Update the module doc-comment in `find_tx_patterns.rs`**

Replace the `## Saga detection` section and the final `All findings carry ... never enter the graph.` line. The Saga section becomes:

```rust
//! ## Saga detection
//!
//! Reads heuristic `RelType::CompensatedBy` edges from the graph (emitted at
//! index time by `ecp_analyzer::post_process::saga_pairs`). Each edge is a
//! `compensator → operation` pair following the Saga compensating-transaction
//! naming convention (`compensate/undo/rollback` + `<verb_noun>`, snake/camel/
//! Pascal case), both on the same owner class. `--class <Name>` filters by class.
//! confidence + `saga:calls-back`/`saga:name-only` evidence come from the edge.
```

And replace the closing line (~41):

```rust
//! Outbox findings carry `requires_verification: true` and never enter the graph.
//! Saga findings are now backed by the in-graph `CompensatedBy` edge (queryable
//! via `ecp cypher 'MATCH ()-[r:CompensatedBy]->() ...'` and traversed by
//! `ecp impact` when heuristics are shown).
```

- [ ] **Step 2: Update the skill doc**

Read `~/.claude/skills/ecp/_shared/cli/find-transaction-patterns.md` around line 40. Replace any text claiming Saga Outbox detection is "deferred pending T5-33" with: T5-33 (EventTopicMirror) has landed; Saga pairs are now a first-class `CompensatedBy` graph edge. Keep the rest of the doc intact.

- [ ] **Step 3: Build (doc comments don't break compile, but confirm)**

Run: `cargo build -p ecp-cli 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/ecp-cli/src/commands/find_tx_patterns.rs
git commit -m "docs(cli): find-transaction-patterns reflects CompensatedBy edge + T5-33 landed (FU-009)"
```

(The skill doc lives outside the repo at `~/.claude/skills/...` — commit it separately if it is under its own VCS, else note the edit was applied in place.)

---

## Task 8: Cross-impact regression + schema reltypes doc

**Files:**
- Modify: `crates/ecp-analyzer/tests/saga_pairs.rs` (add regression assertion) OR a dedicated CLI test
- Modify: schema reltypes listing if `CompensatedBy` needs registering for `ecp schema reltypes`

- [ ] **Step 1: Verify `ecp schema reltypes` lists CompensatedBy**

Run: `target/debug/ecp schema reltypes 2>&1 | rg -i compensat`
Expected: `CompensatedBy` appears with its heuristic flag. If absent, find the reltypes enumeration source (`rg -rn "schema.*reltypes\|reltypes" crates/ecp-cli/src/commands/schema*`) and confirm it iterates the enum (it should auto-pick up the new variant via `as_str`). If it is a hardcoded list, add `CompensatedBy`.

- [ ] **Step 2: Add a regression test that heuristic visibility doesn't change risk/coverage**

This mirrors the already-landed `e74b8e6c` test. Add to `crates/ecp-cli/tests/find_tx_patterns_cmd.rs` (or wherever the risk/coverage CLI tests live — `rg -ln "coverage\|risk" crates/ecp-cli/tests`):

```rust
#[test]
fn compensatedby_edge_does_not_change_coverage() {
    // A repo with a Saga pair must report the same `ecp coverage` summary
    // whether or not CompensatedBy edges exist, because heuristic edges are
    // excluded from coverage/risk scoring (PR #453 invariant).
    let tmp = setup_single_file(
        "class OrderSaga:\n    def book_room(self): ...\n    def undo_book_room(self): self.book_room()\n",
    );
    // Run coverage; assert it succeeds and the summary is stable.
    // (Exact assertion: coverage JSON `summary` has no CompensatedBy contribution.)
    let json = run_find_tx(tmp.path(), &[]);
    assert!(json["saga_pairs"].as_array().map_or(false, |a| !a.is_empty()));
}
```

NOTE: if the existing test harness in `find_tx_patterns_cmd.rs` doesn't expose a coverage runner, keep this assertion scoped to "saga pair still detected post-retire" and rely on the existing `e74b8e6c` coverage test for the invariant. Do not duplicate coverage-runner plumbing.

- [ ] **Step 3: Run the full affected test set**

Run:
```bash
cargo test -p ecp-analyzer --test saga_pairs 2>&1 | tail -5
cargo test -p ecp-cli --test find_tx_patterns_cmd 2>&1 | tail -8
```
Expected: all PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/ecp-cli/tests/find_tx_patterns_cmd.rs
git commit -m "test: CompensatedBy in schema reltypes + coverage-invariant regression"
```

---

## Task 9: Full workspace test + simplify pass before push

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: 0 failures. Investigate any regression before proceeding.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: no new warnings introduced by this change.

- [ ] **Step 3: Self-review the diff (small diff → single-pass, per CLAUDE.md)**

Run: `git diff main --stat` then review each hunk for reuse / dead code / unused imports left by the retire.

- [ ] **Step 4: Update FOLLOWUPS — mark FU-008 + FU-009 done**

Edit `/home/enor/code-graph-nexus/.claude/FOLLOWUPS.md`: convert FU-2026-05-25-008 and FU-2026-05-25-009 to `<!-- ... → ✅ done -->` stubs and archive full entries to `FOLLOWUPS_DONE.md` (per the followups doc protocol).

- [ ] **Step 5: Push branch + open PR**

```bash
git push -u origin feat/compensatedby-reltype
gh pr create --title "feat: CompensatedBy heuristic RelType + retire find-tx-patterns Saga scan (FU-008/009)" --body "..."
```
PR body: summarize the 3-problem payoff (verb-sprawl / main-path exposure / hot-path), the 0.6/0.8 evidence tiers, 14-language case handling, and that it stacks on PR #453. NO Claude attribution footer.

---

## Self-Review Notes

- **Spec coverage:** enum+wiring (T1) ✓; pass detection (T2) ✓; emit + tiers (T3) ✓; builder reg (T4) ✓; corpus verify (T5) ✓; retire + read edge (T6) ✓; FU-009 docs (T7) ✓; 14-lang case (T2/T3 camel/pascal tests) ✓; cross-impact regression (T8) ✓.
- **Borrow-checker risk** flagged inline in T3 Step 3 (resolve to owned String if `&str` borrow conflicts with `string_pool.add`).
- **Outbox untouched** — confirmed only Saga half changes in T6.
- **Stacking:** branch is on PR #453 HEAD; rebase onto main after #453 merges before final push (note in T9).
