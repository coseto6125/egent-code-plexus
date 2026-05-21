# Symbol-Graph Core Roadmap — #1 / #4 / #5 / #7 / #10

**Date:** 2026-05-21
**Original status:** Draft — awaiting user review (Phase 6)
**Total PRs:** 54 atomic, test-first commits

> **Restoration note (2026-05-21):** the original draft of this document
> lived in worktree `feat-symbol-graph-roadmap` on branch
> `feat/symbol-graph-core-roadmap`. The worktree was deleted before the
> branch was pushed to origin, and the branch was pruned from local refs,
> so the file was lost from git history entirely. It has been
> reconstructed by replaying the original `Write` tool call + 23
> subsequent `Edit` operations captured in a Claude Code session jsonl
> (`9289733a-d3f9-4333-88f0-42190a9e88df`). One `Edit` (line 1231,
> 2026-05-20T23:31:12) was skipped because its `old_string` was already
> not present at restore time — likely a hunk that was reverted later in
> the session. Body content below is preserved verbatim from the
> reconstruction; only this note and the status table were added.

## Status snapshot

Last refreshed 2026-05-22 (evening) — closes the roadmap dev side
(T5-3+ event detectors and T10-4 Outbox half remain out of scope, see
below). Source-of-truth columns:

- `grep evidence` cites the actual symbol / file the verification looked
  at — so future drift can re-check the same anchor instead of guessing
- `status` distinguishes **shipped on main** from **PR open** from
  **truly pending** (no branch, no PR)

| Phase | Task | Status | Evidence on `origin/main` |
|---|---|---|---|
| 0 | T0-1 (schema variants) | shipped #261 | `NodeKind::{SchemaField,EventTopic,TransactionScope}` in `graph.rs` |
| 0 | T0-2 (LocalGraph raw vecs) | shipped #263 | `LocalGraph.schema_fields/event_topics/tx_scopes` in `types.rs` |
| 1 | T1-1 (= T1-3 per D5) owner_class 14-lang | shipped #267 | `RawNode.owner_class` + `stamp_owner_class_by_span` in `framework_helpers.rs` |
| 1 | T1-2 streaming xxh3_64 helper | shipped #262 | `crates/ecp-core/src/uid.rs` |
| 1 | T1-4 `Node.owner_class` (struct field) | shipped #285 (bundled w/ T1-11) | `Node.owner_class: StrRef` + builder Pass-1 wiring + 14-lang parity test |
| 1 | T1-5 `Node.uid: u64` | shipped #285 (stacked via #293) | u64 uid via `ecp_core::uid::compute`; D1 collision recovery emits BlindSpot `kind: "uid-collision"` |
| 1 | T1-6 Resolver uid lookup | shipped #315 — Option B picked (real win) | Investigation found O(N) scan in `impact::classify_symbol`; new `graph_query::build_uid_index` builds `FxHashMap<u64,u32>` once. Bench: 18 628× speedup (399ms → 0.02ms @ 10k lookups / 3k nodes). 5 tests incl. local-var-not-leaked negative |
| 1 | T1-7 `GRAPH_FORMAT_VERSION` rolling bumps | shipped (multiple) | Schema crossed 4 → 5 → 6 → 7 → 8 as T1-4 / T1-5 / T7-2 landed; auto-reindex + `.bak` rollback path lives at `engine.rs:122-170` + `auto_ensure.rs:37-42`. Re-audited as part of T7-4 |
| 1 | T1-8 FQN in `inspect` | shipped #284 (bundled w/ T1-9) | `commands/symbol_id.rs` (`resolve_owner_class` edge-walk + `split_fqn_target`) + `ownerClass` JSON output |
| 1 | T1-9 FQN in `impact` | shipped #284 (bundled w/ T1-8) | owner_filter in `impact_by_name` + BFS results carry `ownerClass` |
| 1 | T1-10 Cypher uid migration | shipped #309 | `n.uid` projection → `Value::Int` (no per-row alloc); `WHERE n.uid = <numeric>` works; `WHERE n.uid = "<str>"` returns clear typed error |
| 1 | T1-11 `ecp rename` owner_class isolation | shipped #285 (bundled w/ T1-4) — closes LOAD-BEARING accuracy bug | `rename.rs` filters by both `n.name` AND `n.owner_class`; bare-name now strict top-level only; u32-len fast-reject in hot path |
| 1 | T1-12 sentinel cleanup | shipped #308 | `IMPL_TARGET_PREFIX` + `__impl_target__:` legacy fallback removed from `post_process/class_membership.rs`; tests updated to use direct owner_class path |
| 4 (hybrid) | T-H1 impact filter | shipped #264 | `is_heuristic()` filter in BFS edge loop |
| 4 (hybrid) | T-H2 rename hard-exclude + count surface | shipped #265 | `heuristic_mirror_count` in `rename.rs` |
| 4 (hybrid) | T-H3 inspect separate section | shipped #266 | `heuristic_outgoing`/`heuristic_note` in `inspect.rs` |
| 7 | T7-1 `parse_to_fragment` real impl | shipped #268 | `parse_to_fragment` in `overlay_writer.rs` |
| 7 | T7-2 per-symbol content hash | shipped #292 | `RawNode.content_hash: u64` + `Node.content_hash: u64`; 14-lang real hash via `xxh3_64_bytes(tree-sitter root span)` |
| 7 | T7-3 shadow-candidates | shipped #269 | `crates/ecp-analyzer/src/incremental/shadow_candidates.rs` |
| 7 | T7-4 wire `reanalyze_files` into `auto_ensure` | shipped #313 | Dirty-stale branch with known rel_paths dispatches `reanalyze_files`; `apply_l1_overlay_updates` retained as fallback; `pre_tool_use::handle` deliberately untouched, locked by `test_pre_tool_use_hook_unchanged_path` |
| 7 | T7-5 overlay zero-copy merge | shipped #317 | `crates/ecp-core/src/session/overlay.rs` ships `merge_archived(base, overlay) -> impl Iterator<&ArchivedNode>` (overlay wins on uid); FxHashSet<u64> built once. `inspect.rs` wired through it. **Note**: `unsafe ptr::read` for `ArchivedNodeKind → NodeKind` conversion required to dodge rustc-stable SIGSEGV on 27-arm pattern match — safety invariants documented in module. No tombstone support in v1 (deletion test `#[ignore]`'d with rationale) |
| 7 | T7-6 skip unchanged bodies | shipped #316 | Per-symbol content_hash diff in `crates/ecp-cli/src/incremental.rs::symbol_hash_diff`; resolver / class_membership skipped for unchanged uids. Guards (a) import-set, (b) shadow-candidates, (c) SchemaFieldIndex bucket. **Guard (c) conservative fallback**: any file with non-empty `schema_fields` forces full reanalyze. Proper incremental bucket-diff API deferred to T7-7's parity gate |
| 7 | T7-7 incremental vs full-reindex parity gate | shipped #319 | `crates/ecp-cli/tests/incremental_full_parity.rs` — proptest 20 cases × 5 edit types × 14-lang fixture (56 files). 42 s smoke runtime; heavy 200-case `#[ignore]`'d for nightly. Compares nodes by `(uid,file,name,kind)`, edges by `(src,dst,rel)`, bucket fingerprint (T7-6 guard (c) verification). 0 parity violations |
| 4 (schema) | T4-1 SchemaFieldExtractor skeleton | shipped #270 | `crates/ecp-analyzer/src/schema_field/{config,extract,mod}.rs` |
| 4 (schema) | T4-2 Pydantic | shipped #279 | `python/schema_extractors.rs::PYDANTIC_CONFIG` + `python_pydantic_schema.rs` tests |
| 4 (schema) | T4-3 SQLAlchemy | shipped #281 | `python/schema_extractors.rs::SQLALCHEMY_CONFIG` (Idiom A `Column` + Idiom B `Mapped[T]`) + `python_sqlalchemy_schema.rs` tests |
| 4 (schema) | T4-4 TS interface | shipped #283 | TS interface property extraction wired through `typescript/queries.scm` + dispatcher |
| 4 (schema) | T4-5 protobuf | shipped #290 | hand-rolled `.proto` lexer (no tree-sitter-protobuf dep); `FrameworkId::Protobuf` discriminant; 16 scalar types + proto2/3 scalars + repeated/optional/required; nested-message / oneof / map<K,V> deferred to v2. 13 tests pass |
| 4 (schema) | T4-6 OpenAPI | shipped #312 | `crates/ecp-analyzer/src/openapi/schema_scan.rs` walks `components.schemas` (OpenAPI 3.x) + `definitions` (Swagger 2.0). 200-byte prefix gate keeps non-OpenAPI YAML zero-cost. `serde_yaml` added as analyzer dep. 7 tests incl. negative (k8s manifest emits 0 fields) + `paths.*` inline schemas excluded v1 |
| 4 (schema) | T4-7 SchemaFieldIndex + `MirrorsField` | shipped #291 | new `post_process/schema_field_mirrors.rs`: emits `SchemaField` nodes + `HasProperty` (Class→SchemaField) + `MirrorsField` heuristic edges. Bucket by `(name.to_lowercase(), SchemaType)`; D3 cluster semantics for k≥3 uniform triples |
| 4 (schema) | T4-8 `find-schema-bindings` CLI | shipped #314 | `crates/ecp-cli/src/commands/find_schema_bindings.rs`. Supports `Class.field` (owner-scoped) + bare `field` (cross-owner). Output carries `tier`, per-check breakdown (name/type/class/bidir), `requires_verification: true`. 5 tests. **Documented gaps** (architectural — not bugs): `framework` field surfaces `null` because `FrameworkId` isn't persisted on `Node`; check booleans are inferred from connected nodes (not stored per edge); `BLIND_SPOT` tier only reachable when emitter stores partial-match edges (currently always 0.9 confidence) |
| 5 (event) | T5-0 normalize | shipped #271 | `event_topic/normalize.rs` with `split_camel_case` consecutive-caps fix |
| 5 (event) | T5-1 `RawEventTopic` dispatcher skeleton | shipped #280 | `event_topic/mod.rs` (dispatcher present) |
| 5 (event) | T5-2 Kafka Python | shipped #289 | first concrete event-topic detector; validates T5-1 dispatcher pattern |
| 5 (event) | T5-3..T5-31 detectors (5 Celery SKIP) | **out of scope this round** | T5 series owned by separate parallel session; do not touch |
| 5 (event) | T5-32 coverage matrix doc | out of scope | depends on T5 detectors above |
| 5 (event) | T5-33 `EventTopicMirror` heuristic | out of scope | depends on T5-2..31 subset gate (D7) |
| 5 (event) | T5-34 `find-event-mirrors` CLI | out of scope | depends on T5-33 |
| 10 | T10-1 + T10-2 + T10-3 (collapsed) | shipped #275 | `RawTxScope` packed + `NodeKind::TransactionScope` + `OpensTxScope` edge |
| 10 | T10-4 `find-transaction-patterns` CLI (Saga half) | shipped #311 — Saga half only | `crates/ecp-cli/src/commands/find_tx_patterns.rs`. Detects `<verb>_<noun>` ↔ `compensate/undo/rollback_<verb>_<noun>` on same class. Confidence 0.6 base, bumped to 0.8 when compensator calls operation (Calls edge); cap 0.85. `outbox_patterns` field always present but empty with `outbox_status: "blocked_on_t5_33"` — Outbox half intentionally deferred (depends on `EventTopicMirror`) |
| 10 | T10-4 Outbox half | **out of scope this round** | depends on T5-33 EventTopicMirror; revisit when T5 detectors stabilize |
| Phase 5 | T-P1 parity baselines refresh | **in-flight** | dump-script half shipped #288; baseline regeneration tracking PR opening this turn |
| Phase 5 | T-P2 user-doc updates | shipped #320 | `find-schema-bindings` + `find-transaction-patterns` documented in skill files + README + `install_hook.rs` embedded skill text |
| CI | Docs-only PR short-circuit | shipped #287 | `detect-changes` job + step-level `if:` gating |

### Things to highlight (vs. literal reading of body below)

- **Dev side closed**: T1 / T4 / T7 / T10 (Saga half) / Phase 5 docs all
  on `origin/main`. Only T-P1 baseline regeneration remains tracking on
  this roadmap.
- **T1-6 chose Option B (real win)**, not the no-op the original spec
  feared. Investigation surfaced an unrelated O(N) hot spot in
  `impact::classify_symbol` (linear scan reverse-lookup `uid → node_idx`
  per BFS caller entry). `graph_query::build_uid_index` builds a
  `FxHashMap<u64,u32>` once at coverage_analyses entry; lookup goes
  O(N) → O(1). Bench `crates/ecp-analyzer/benches/resolver_lookup.rs`
  records 18 628× speedup at 3 k nodes / 10 k lookups. The original
  spec's resolver-side `FxHashMap<u64,NodeId>` was indeed moot post-
  SymbolTable; the real win lived one layer up.
- **T1-8 + T1-9 (#284) edge-walk owner resolution is now collapsable**.
  After #285 landed, `commands/symbol_id.rs::resolve_owner_class` can
  collapse from `HasMethod`/`HasProperty` inbound-edge walk to a single
  `n.owner_class` field read — O(in_degree) → O(1). Tracked as a
  follow-up; current code is correct, just suboptimal.
- **T7-5 shipped a workaround for a rustc-stable codegen bug**.
  `merge_archived` returns a concrete `MergeIter<'_>` (not
  `impl Iterator<Item = &ArchivedNode>`) and uses `unsafe ptr::read` for
  the `ArchivedNodeKind → NodeKind` conversion because a 27-arm pattern
  match through `rustc_pattern_analysis::check_unsafety` triggered
  stable SIGSEGV. Safety invariants documented inline.
- **T7-6 guard (c) is a conservative fallback, not the spec's incremental
  bucket-diff**. `SchemaFieldIndex` doesn't expose a per-file
  bucket-membership API at the right layer for incremental diffing; T7-6
  falls back to "any file with non-empty `schema_fields` forces full
  reanalyze". T7-7's parity gate (#319) verifies set-equality across
  20 random edits × 14 langs and finds zero divergence, so the
  conservative fallback is correctness-preserving. A bucket-fingerprint
  sidecar to enable proper incremental guard (c) is a future-work item.
- **T4-8 (#314) is honest about three architectural gaps**: `framework`
  field always `null` (`FrameworkId` isn't persisted on `Node`); check
  booleans inferred from connected nodes (not stored per edge);
  `BLIND_SPOT` tier currently unreachable (T4-7 emitter always stores
  confidence 0.9). Documented in the command's module doc rather than
  faked. Promoting any of these out of "documented gap" requires a
  Node-schema bump + emitter rework — out of scope for this batch.
- **T10-4 (#311) shipped Saga-only**. The Outbox half depends on
  `EventTopicMirror` (T5-33) which is out of scope this round. The
  CLI's `outbox_patterns` field is always present but empty with
  `outbox_status: "blocked_on_t5_33"` — honest no-data per CLAUDE.md
  output discipline, gives consuming LLMs a structured trigger to
  retry once T5-33 ships.
- **GRAPH_FORMAT_VERSION rolling bumps stabilized**. The version raced
  through v5 → v6 (T1-4) → v7 (T1-5 stacked + T7-2) → v8 (current).
  Each bump cascaded `.bak` rollback audit through `engine.rs:122-170`
  and `auto_ensure.rs:37-42`. T7-4's wiring re-audited the path again
  during the dirty-stale dispatch refactor; no new gaps.

The snapshot is intentionally NOT woven into the body — the body stays
verbatim as the canonical planning artefact. Status drift gets tracked
by overwriting just this table in follow-up commits.

---

## 1. Goal

**Highest performance + highest accuracy** for LLM agents using ecp at edit-time. Five features close real gaps confirmed against ref-gitnexus (`~/code-graph-nexus/.gitnexus/gitnexus`) and ecp's own self-index collisions:

| # | Feature | Real bite today |
|---|---|---|
| #1 | Symbol Identity with FQN | Rust impl-block method UID collisions in ecp self-index (`from_str×2 / from_path×2 / default×3`) |
| #4 | Schema cross-binding | Agent refactors `users.email` blind to `UserResponse.email` / `user.proto`; DB-migration breakage |
| #5 | Event Flow Graph | Event-driven backends opaque; producer/consumer rename silently breaks |
| #7 | Incremental Indexing | Working-tree (unstaged) invisible; mtime touch triggers full reanalyze |
| #10 | Transaction Boundary (P2) | Cross-tx refactors silently break atomicity |

---

## 2. Locked design decisions

### 2.1 Identity hashing

| Choice | Rationale |
|---|---|
| `Node.uid: u64` (was `StrRef`) | 8B inline; `FxHashMap<u64, NodeId>` 1-cycle lookup; resolver ~15× faster |
| `xxh3_64` streaming, no alloc | Already in deps (parse_cache); deterministic, cross-version stable |
| Canonical bytes | `[kind_tag \0 path \0 owner_class_or_empty \0 name]` — locked by golden test |
| Collision risk at scale (D2) | `10⁷ symbols → 2.7e-6` / `5×10⁷ → 6.9e-5` / `10⁸ → 2.8e-4`. Acceptable to ecp's deployment scale ceiling |
| Collision recovery (D1) | Builder triple-check `(name, owner_class, path)` on insert. On detected collision → emit `BlindSpot { kind: "uid-collision", offending_nodes: [...] }` + continue indexing. **No panic in background indexer**; user sees "N symbols couldn't be uniquely identified, run `ecp blindspots`" in CLI summary |

### 2.2 Surface model — hybrid by command + structural verification

Per CLAUDE.md `Heuristic edges with <0.7 confidence must be tagged, not promoted`. Per 5-Haiku LLM-consumer review consensus.

**Heuristic visibility per command:**

| Command | Heuristic? | Format |
|---|---|---|
| `impact` (default) | NO | single section, deterministic only |
| `impact --include-heuristic` | YES | **two sections never merged**: ① Confirmed blast radius ② Possible mirror cascades. Hidden-count surfaced as `hidden_heuristic_edges: N` when default-suppressed |
| `impact --confidence-threshold <0.0-1.0>` | filter | sets internal threshold; tier label still used for display |
| `impact --explain-confidence` | YES | adds per-candidate check matrix |
| `inspect` | YES | two sections: ① Confirmed members/callers ② Possible mirrors (top-level `heuristic_note` field) |
| `find` | YES | two sections: ① exact/fuzzy matches ② heuristic candidates |
| `rename` / `refactor` | **NEVER** | hard-exclude heuristic — action commands cannot mutate based on guesses |
| `find-schema-bindings` / `find-event-mirrors` / `find-transaction-patterns` | YES | pure heuristic, primary content with full evidence list |

**Per-candidate output format (all commands surfacing heuristic):**

```text
UserResponse.email   [LIKELY_RELATED]    checks: name✓ type✓ class✓ bidir✓
                                          requires_verification: true
AdminResponse.email  [BLIND_SPOT]         checks: name✓ type✓ class✗ bidir✗
                                          requires_verification: true
```

Two key revisions from Haiku review (5/5 agreement):

1. **Check breakdown shown structurally** — `checks: name✓ type✓ class✗ bidir✓` per candidate. Tier label alone is insufficient; LLMs need to see WHICH checks passed to calibrate trust.
2. **`requires_verification: true` is a structural field** (JSON/TOON), NOT prose. Agent execution engines can gate programmatically on this field; prose labels get ignored under loop pressure.

**Why `rename` action excludes heuristic, but count surfaces (revised):**

Rename hard-excludes heuristic edges from the **mutation set** because mutation cannot be undone. However, the rename **output** must surface the COUNT of heuristic mirrors via structural field `heuristic_mirrors_not_touched: N` — without this, the LLM has no trigger to investigate and assumes rename is complete.

The split:
- **Action**: 100% deterministic. `rename` only mutates files reachable via non-heuristic edges
- **Output**: surfaces `heuristic_mirrors_not_touched: N` count when N>0, with hint to `ecp find-schema-bindings` or `--show-heuristic-mirrors`
- **Flag** `--show-heuristic-mirrors`: opt-in expansion to include full candidate list in output (same format as `find-schema-bindings`)

Agents have multiple deterministic fallback paths regardless:

1. **`grep`** — `grep -rn "users\.email" .` finds string-literal references the AST can't see
2. **`ecp find-schema-bindings users.email`** — explicit pull-CLI returns LIKELY_RELATED + BLIND_SPOT candidates
3. **`ecp inspect users.email`** — shows `Possible mirrors` section (heuristic visible, not actioned)

The count surface is the trigger; the follow-up commands are the means.

### 2.3 Tier model

Internal confidence computed but never surfaced. Maps to tier label:

| Internal confidence | Tier | Default visibility |
|---|---|---|
| `≥ 0.85` (all 4 strict checks) | `LIKELY_RELATED` | shown |
| `0.70 – 0.85` (3/4 checks) | `BLIND_SPOT` | hidden unless `--include-blindspot` |
| `< 0.70` (≤2/4) | not emitted | — |

Floor at `0.70` is set by CLAUDE.md `Heuristic edges with <0.7 confidence must be tagged, not promoted` — anything sub-0.70 must drop, not surface. Confidence band `0.65 – 0.70` is intentionally empty; if the four-check scoring math ever produces a value in that range, the candidate is treated as "not emitted" rather than BlindSpot.

Strict checks differ per edge type:

- **MirrorsField (#4)**: (1) exact field name, (2) same type-class, (3) same class name, (4) bidirectional top-1
- **EventTopicMirror (#5)**: (1) normalized topic name match, (2) same direction-pair (Publish↔Subscribe), (3) same lib OR cross-lib explicit, (4) bidirectional top-1
- **SagaCompensates (#10)**: heuristic name-pair only, no graph edge (pull CLI only)

### 2.4 Format-version migration

- `GRAPH_FORMAT_VERSION`: 4 → 5 (forced once by Phase 0 schema changes)
- Auto-ensure detects version mismatch → **inline blocking** reindex on first query post-upgrade (CLAUDE.md "first query pays the cost once")
- No background degraded-mode; v4 schema unreadable under v5

---

## 3. Crate-name note

Architect outputs initially used legacy `cgn-*` crate names. The rename `cgn → ecp` landed in PR #228 (2026-05-19). All paths below use current `ecp-*` names:

| Legacy | Current |
|---|---|
| `cgn-core` | `ecp-core` |
| `cgn-analyzer` | `ecp-analyzer` |
| `cgn-cli` | `ecp-cli` |
| `cgn-mcp` | `ecp-mcp` |

---

## 4. Phase 0 — Schema preamble (blocks everything)

### T0-1: Append schema variants + heuristic classifier + structural ordering test

**Touches:**
- `crates/ecp-core/src/graph.rs` — **append AFTER `Impl` (currently discriminant 23, last variant of `NodeKind`)**: `SchemaField` (24), `EventTopic` (25), `TransactionScope` (26). Do NOT insert mid-enum (rkyv discriminants are append-only per CLAUDE.md).
- `crates/ecp-core/src/graph.rs` — **append AFTER `Fetches` (currently discriminant 11, last variant of `RelType`)**: `MirrorsField` (12), `Publishes` (13), `Subscribes` (14), `EventTopicMirror` (15), `OpensTxScope` (16)
- Extend `NodeKind::as_str` + `RelType::from_str` for new variants
- Add `RelType::is_heuristic` accessor:

```rust
impl RelType {
    pub const fn is_heuristic(self) -> bool {
        matches!(self, Self::MirrorsField | Self::EventTopicMirror)
    }
}
```

Each new variant carries doc comment naming its LLM-query benefit (per CLAUDE.md `graph.rs:101-103` precedent).

**Pre:** none
**Test:** `crates/ecp-core/tests/graph_schema.rs`:
- `test_from_str_roundtrip_all_new_variants`
- `test_node_kind_discriminants_locked` — hard-code expected `as u8` value for every variant (SchemaField=24, EventTopic=25, TransactionScope=26); locks append-only
- `test_rel_type_discriminants_locked` — same (MirrorsField=12, Publishes=13, Subscribes=14, EventTopicMirror=15, OpensTxScope=16)
- `test_is_heuristic_classification` — `MirrorsField.is_heuristic()` and `EventTopicMirror.is_heuristic()` return true; all others false

**Plus structural ordering gate** (`crates/ecp-cli/tests/heuristic_filter_structural.rs`):
- `test_impact_default_hides_mirrors_field` — build a synthetic graph containing a `MirrorsField` edge → `ecp impact <node>` default output MUST NOT contain that edge
- **This test fails until T-H1's filter exists** — making the T-H1 → T4-7 ordering structurally enforced rather than procedural. PRs merging T4-7 before T-H1 will fail CI

**Perf:** enum widening only
**Surface:** internal

### T0-2: Extend `LocalGraph` with new raw-ref vectors

**Touches:**
- `crates/ecp-core/src/analyzer/types.rs:115-126` — add `schema_fields: Vec<RawSchemaField>`, `event_topics: Vec<RawEventTopic>`, `tx_scopes: Vec<RawTxScope>`
- Same file ~line 92 — three new `Raw*` structs with rkyv derives

```rust
// D4: All identifier-bearing fields use StrRef (string-pool indirect, 4-byte
// offset+len) from day-1 to avoid per-parse heap allocs on the hot path.
// `framework` / `source_pattern` / `lib` stay &'static str (compile-time constants).
pub struct RawSchemaField {
    pub name: StrRef,
    pub type_class: SchemaType,         // String / Int / Float / Bool / Datetime / Json / Other
    pub owner_class: StrRef,
    pub framework: &'static str,
    pub span: (u32, u32, u32, u32),
}
pub struct RawEventTopic {
    pub topic_literal: Option<StrRef>,  // None = dynamic; emit BlindSpot
    pub direction: PubSub,
    pub lib: &'static str,
    pub enclosing_fn: StrRef,
    pub span: (u32, u32, u32, u32),
}
pub struct RawTxScope {
    pub enclosing_fn: StrRef,
    pub source_pattern: &'static str,   // "java-transactional" / "django-atomic" / ...
    pub span: (u32, u32, u32, u32),
}
```

**Pre:** T0-1
**Test:** `cargo build -p ecp-core` clean; existing fixtures still serialize
**Perf:** 3 empty Vecs per LocalGraph (cap=0) — negligible
**Surface:** internal

---

## 5. Feature #1 — Symbol Identity with FQN (12 tasks)

### T1-1: Add `owner_class` to `RawNode` IR + 14-lang plumbing (merged with former T1-3)

**D5 resolution:** T1-1 and former T1-3 collapsed into single PR. CLAUDE.md "Single-language tests for a multi-language change get rejected" applies to `RawNode` (shared parser IR). Cannot split owner_class addition across two PRs.

**Touches:**
- `crates/ecp-core/src/analyzer/types.rs` — add `RawNode.owner_class: Option<StrRef>` (StrRef per D4; no `String` intermediate stage)
- 14 parsers each emit `owner_class` for methods/properties:
  - Rust: `rust/parser.rs:336-351` — replace `__impl_target__:Type` sentinel with direct field
  - Python: `python/parser.rs:368-380` — return class name from `is_class_method()` (or new helper)
  - TypeScript: `typescript/parser.rs` — capture class-name at emit, not via post-pass span containment
  - JavaScript, Java, Kotlin, C#, Go (receiver type), PHP, Ruby, Swift, C (struct via function-pointer assignment), C++, Dart — same shape
- `crates/ecp-analyzer/src/post_process/class_membership.rs` — keep as fallback only for langs without direct parser emission (none expected after this lands)
**Pre:** none
**Test:** `tests/owner_class_<lang>.rs` × 14 + aggregate `owner_class_parity_14lang.rs`
- Per-lang: two methods of same name on different classes → owner_class distinguishes
- C special (OQ-2 → struct type): `static struct foo_ops = { .open = my_open }` → `my_open.owner_class = Some("foo_ops_t")`
- Aggregate: `from_str` on `NodeKind` vs `RelType` (Rust corpus) both present, owner_class differs
- Negative: module-level functions get `owner_class = None`
**Perf:** Parser hot path; reuse existing tree-sitter capture buffers, no extra walks. StrRef interning amortized via `string_pool.add()`
**Accuracy:** 14-lang parity per CLAUDE.md mandate

### ~~T1-3~~ (merged into T1-1)

### T1-2: Streaming xxh3 UID helper (zero-alloc)

**Touches:** new `crates/ecp-core/src/uid.rs`; re-export from `lib.rs`
**Pre:** none (parallel with T1-1)
**Test:** `tests/uid_canonical.rs`:
- `test_uid_streaming_matches_concat_hash` → `xxh3_64(b"Function\0src/a.rs\0\0foo")`
- `test_uid_owner_class_disambiguates_collision`
- `test_uid_stable_across_1000_invocations`
- `test_uid_zero_alloc_verified` via `dhat`
**Perf:** `Xxh3::new().update(...).digest()` streaming. `\0` separator (cannot appear in any valid input)
**Accuracy:** Canonical byte order locked by golden test

### ~~T1-3~~ — Merged into T1-1 per D5

### T1-4: Promote `owner_class` to `Node` struct (StrRef)

**Touches:** `crates/ecp-core/src/graph.rs:228-235` — add `pub owner_class: Option<StrRef>`; builder interns via `string_pool.add()`
**Pre:** T1-1 (14-lang owner_class plumbing — was T1-3 before D5 merge)
**Test:** `tests/node_owner_class_field.rs` — rkyv round-trip
**Perf:** `Option<StrRef>` = `Option<u32>` = 8B with niche. ~9k symbols × 8 = 72 KB negligible
**Accuracy:** rkyv layout change → format bump in T1-7

### T1-5: Switch `Node.uid` from `StrRef` to `u64`

**Touches:** `crates/ecp-core/src/graph.rs:228-235` (uid type); `crates/ecp-analyzer/src/resolution/builder.rs:344-368` (drop `uid_buf` + StringPool insert; call `uid::compute(kind, path, owner_class, name)`); builder gains `(name, owner_class, path) → uid` triple-check `FxHashMap`; on detected collision → emit `BlindSpot { kind: "uid-collision" }` + log, **do NOT panic** (D1)
**Pre:** T1-2, T1-4 (T1-1 already 14-lang per D5 merge)
**Test:** `tests/uid_u64_builder.rs`:
- `test_builder_uid_matches_helper` for every Node
- `test_real_collisions_resolved_in_ecp_self` — index ecp itself, `default×3` in config.rs now 3 distinct u64s
- `test_assert_unique_uid_in_self_index` — index ecp itself, walk every Node, assert `FxHashMap<u64, NodeId>::insert` never reports collision (guards window before T1-11 wires the triple-check map)
- `test_synthetic_collision_emits_blindspot_not_panic` — force hash collision via test harness, assert BlindSpot record + indexer completes (no panic, no abort)
**Perf:** Eliminates 1 string-pool insert + 1 StrRef lookup per node per query — load-bearing win. Triple-check map insert+lookup amortized O(1)
**Accuracy:** Collision risk 2.7e-6 @ 10⁷ / 6.9e-5 @ 5×10⁷ / 2.8e-4 @ 10⁸ (D2 keeps u64). Graceful BlindSpot recovery (D1)

### T1-6: Resolver `HashMap<String, NodeId>` → `FxHashMap<u64, NodeId>`

**Touches:** `crates/ecp-analyzer/src/resolution/resolver.rs:62`, `builder.rs:1477`, symbol-table internals
**Pre:** T1-5
**Test:** `tests/resolver_fxhash_uid.rs` + `benches/resolver_lookup.rs` asserting ≥2× speedup vs baseline
**Perf:** Hot path for `compute_hits` (find.rs:964). u64 key = zero string hash, zero String alloc

### T1-7: Bump `GRAPH_FORMAT_VERSION` 4 → 5 + auto-reindex + rollback safety

**Touches:**
- `crates/ecp-core/src/graph.rs` const bump
- `crates/ecp-cli/src/engine.rs:122-170` — distinguish "stale v5" (overlay path OK) from "version-incompatible v4" (full rebuild required)
- `crates/ecp-cli/src/auto_ensure.rs:37-42` — when `header_compatible == false`, must call `build_l2`, NOT `apply_l1_overlay_updates` (overlay against v4-incompatible base = corruption)
- Rollback safety: before triggering reindex, atomically rename `graph.bin` → `graph.bin.v4.bak`. If reindex exits non-zero, surface hard CLI error with reindex stderr — do NOT loop into another auto-ensure on the same broken state. Keep `.v4.bak` until next successful reindex completes (manual recovery path)

**Pre:** T1-4 (Node struct layout change already breaks format), T1-5, T1-6
**Test:** `tests/format_upgrade_v4_to_v5.rs`:
- `test_v4_graph_triggers_full_rebuild_not_overlay` — synthetic v4 graph.bin → `ecp inspect foo` → `build_l2` invoked, NOT overlay
- `test_v5_graph_no_reindex` — fresh v5, no reindex
- `test_reindex_failure_keeps_backup_and_errors` — simulated reindex exit-1 → `.v4.bak` exists, CLI returns non-zero with stderr, no auto-ensure loop
**Perf:** One-time post-upgrade cost; no degraded-mode fallback. Backup file kept until next successful reindex
**⚠️ FORMAT BUMP** — note T1-4 alone (adding owner_class field) already changes rkyv Node layout, so T1-7 must land in a PR-pair with T1-4 OR T1-7 must precede T1-4 in merge order

### T1-8: FQN render in `inspect`

**Touches:** `crates/ecp-cli/src/commands/inspect.rs:185-248`
**Pre:** T1-4
**Test:** `tests/inspect_fqn_render.rs` — `Foo.bar` vs `baz`; TOON `fqn` field
**Accuracy:** `(Some(c), n) => format!("{c}.{n}") | (None, n) => n`

### T1-9: FQN render in `impact`

**Touches:** `crates/ecp-cli/src/commands/impact.rs`
**Pre:** T1-4
**Test:** `tests/impact_fqn_render.rs` — callers show `ClassName.method`; `ecp impact ClassName.method` resolves disambiguated Method
**Accuracy:** Without this, impact on collided names returns wrong blast radius

### T1-10: Cypher executor — uid migration

**Touches:** `crates/ecp-core/src/cypher/executor.rs`, `cypher/value.rs:20`
**Pre:** T1-5
**Test:** `tests/cypher_uid_migration.rs` — `WHERE n.uid = <u64>`, `WHERE n.name='X' AND n.owner_class='Y'`; legacy string form returns clear error
**Accuracy:** Hard-fail with guidance > silent miss

### T1-11: `ecp rename` owner_class awareness

**Touches:** `crates/ecp-cli/src/commands/rename.rs`
**Pre:** T1-6, T1-9
**Test:** `tests/rename_owner_class_scoped.rs` — two classes with `validate()`; rename `Foo.validate → Foo.check`; `Bar.validate` untouched
**Accuracy:** **Load-bearing user-visible accuracy claim of Feature #1**

### T1-12: Cleanup — remove sentinel + bool flags

**Touches:** `rust/parser.rs:336-351` (`__impl_target__:Type`), `python/parser.rs:368-380` (`is_class_method` bool), class_membership fallback
**Pre:** T1-1 (was T1-3 before D5 merge)
**Test:** All 14 `owner_class_<lang>.rs` still pass + `no_impl_target_sentinel_regression.rs`
**Accuracy:** Single source of truth

---

## 6. Feature #7 — Incremental Indexing First-Class (7 tasks)

### T7-1: `parse_to_fragment()` real implementation

**Touches:** `crates/ecp-cli/src/session/overlay_writer.rs:163-166` (stub returning `vec![]`); reuse `extract_symbols()` line 276-299
**Pre:** T0-2 (R1-F3: T7-1's fragment format must include the new `schema_fields`/`event_topics`/`tx_scopes` vectors from T0-2, otherwise T7-7 parity gate fails on struct shape mismatch between incremental and full-reindex paths)
**Test:** `tests/parse_to_fragment.rs` — Python 3-def file → 3 fragments with correct byte spans; empty file → empty; syntax error → partial; 14-lang fixture coverage
**Perf:** Reuse existing parser instance
**Accuracy:** Fragment boundaries byte-equal to full-reindex symbol boundaries

### T7-2: Per-symbol content hash

**Touches:** `crates/ecp-core/src/analyzer/types.rs:118` — add `pub symbol_hashes: Vec<[u8; 8]>` aligned with `nodes`; builder populates after Pass 1
**Pre:** T7-1
**Test:** `tests/per_symbol_hash.rs` — unchanged stable, whitespace-only file-hash changes but symbol-hash doesn't, body-edit changes symbol-hash
**Perf:** xxh3_64 over symbol body. Negligible vs full reindex

### T7-3: Port `shadow-candidates.ts` to Rust

**Touches:** new `crates/ecp-analyzer/src/incremental/shadow_candidates.rs`; integrate into `reanalyze_files()` at `crates/ecp-cli/src/reanalyze.rs:67`
**Pre:** none (parallel with T7-1/T7-2)
**Test:** `tests/shadow_candidates.rs` — new `.ts` file shadows sibling `.js` import resolution; distinct basenames no shadow
**Perf:** Once per incremental batch, not per query
**Accuracy:** Without this, per-file incremental produces stale Calls edges (proven by ref-gitnexus PR #1479 review)

### T7-4: Wire `reanalyze_files()` into `auto_ensure` (centralized refresh path)

**Touches:** `crates/ecp-cli/src/auto_ensure.rs:37-42` `ensure_index` / `ensure_fresh` — **NOT** `pre_tool_use::handle`. The hook does BM25 search per tool-use; reanalyze must hook at the per-CLI-invocation refresh layer (auto_ensure), not per-tool-use.

The path: `main.rs:203` calls `ensure_fresh` once per CLI command. When `header_compatible == false` OR overlay says dirty, `ensure_fresh` currently calls `apply_l1_overlay_updates`. T7-4 changes the dirty-Stale branch to invoke `reanalyze_files(repo, scope, rel_paths)` for the changed-file set when (a) the change is incremental (overlay knows), or (b) fall through to full `build_l2` when version-incompatible. **Per CLAUDE.md hot-path rule: `pre_tool_use::handle` stays untouched.**

**Pre:** T7-1, T7-2, T7-3
**Test:** `tests/incremental_wired.rs`:
- `test_edit_file_then_impact_sees_new_symbol_without_full_reindex` — touch file, run `ecp impact`, new symbol visible, no full-reindex marker fires
- `test_auto_ensure_dispatches_incremental_for_overlay_dirty` — assert `reanalyze_files` was called (AtomicUsize counter under `#[cfg(test)]`), `build_l2` was not
- `test_pre_tool_use_hook_unchanged_path` — verify `pre_tool_use::handle` does not gain new code in this PR
**Perf:** All work happens inside `auto_ensure::ensure_fresh`, called at most once per CLI invocation. `pre_tool_use::handle` hot-path untouched

### T7-5: Working-tree overlay zero-copy merge

**Touches:** `crates/ecp-core/src/session/overlay.rs` — remove `#![allow(dead_code)]`; add `merge_archived(...) -> impl Iterator<Item=&ArchivedNode>` (overlay wins on uid match)
**Pre:** T7-4
**Test:** `tests/overlay_merge_zero_copy.rs` — override / addition / deletion; `dhat` zero-alloc
**Perf:** rkyv archived only; overlay-uid FxHashSet<u64> built once per query

### T7-6: Skip class_membership/resolver on unchanged symbol bodies

**Touches:** `crates/ecp-cli/src/reanalyze.rs:67` — diff per-symbol hashes (T7-2); re-run only changed-hash subset
**Pre:** T7-2, T7-4
**Test:** `tests/incremental_skips_unchanged_symbols.rs`:
- `test_mtime_touch_skips_resolver` — `touch file.py`, AtomicUsize counter confirms resolver not invoked
- `test_one_of_five_edit_only_resolves_one`
- `test_skip_guarded_when_import_set_changes` (a)
- `test_skip_guarded_when_shadow_candidates_change` (b)
- `test_skip_guarded_when_schemafield_bucket_membership_changes` (c, R3-F7) — when `UserResponse.email` added in unchanged file's bucket, peer `UserRequest.email`'s MirrorsField re-emission must trigger even though `UserRequest.email`'s body hash didn't move
**Perf:** Largest incremental win
**Accuracy:** Must NOT skip when (a) file's import set changed OR (b) shadow-candidates set changed OR **(c, R3-F7) SchemaFieldIndex / EventTopicIndex bucket gains or loses members** — re-emit mirrors for affected buckets only (O(k²) k<10), not full N² re-bind

### T7-7: Incremental vs full-reindex parity gate (CI)

**Touches:** new `tests/incremental_full_parity.rs`; CI workflow
**Pre:** T7-4, T7-5, T7-6
**Test:** 50-file polyglot fixture, 20 random edits, maintain incremental parallel with full-reindex; assert `(nodes, edges, resolver_table)` equal as sets; `proptest` ≥200 sequences; 14-lang fixture mix
**Accuracy:** **Gate that proves "incremental = first-class"**

---

## 7. Feature #4 — Schema cross-binding (8 tasks)

**Architectural choice (per Architect B + ref-gitnexus precedent):** table-driven `FieldExtractionConfig` over five separate hardcoded detectors. Mirrors ref-gitnexus `field-extractors/generic.ts` (192 lines proves the pattern collapses cleanly).

### T4-1: `SchemaFieldExtractor` config table + trait

**Touches:** new `crates/ecp-analyzer/src/schema_field/{mod,config,extract}.rs`
- `config.rs` — `SchemaFieldConfig { framework, owner_capture, name_capture, type_capture, import_gate: &'static [&'static str], type_classifier: fn(&str) -> SchemaType }`
- `extract.rs` — `extract_schema_fields(&Tree, &[u8], &Query, &[SchemaFieldConfig], imports: &[RawImport]) -> Vec<RawSchemaField>`
**Pre:** T0-1, T0-2
**Test:** `tests/schema_field_extract.rs` — config-driven dispatch picks right framework label
**Perf:** Lazy-compiled per-language queries; `&'static` configs; no per-file alloc beyond output Vec

### T4-2: Pydantic detector (Python)

**Touches:**
- `crates/ecp-analyzer/src/python/queries.scm:42-58` — extend Property pattern to capture annotation type as `@property.type`
- `crates/ecp-analyzer/src/python/parser.rs:537` — annotated class-body assignment + `has_import_from(&imports, &["pydantic"])` + heritage contains `BaseModel` → push `RawSchemaField { framework: "pydantic", ... }`
**Pre:** T4-1
**Test:** `tests/python_schema_fields.rs::pydantic_basemodel_emits_fields` — `class User(BaseModel): email: str` → SchemaField `type_class=String owner_class="User"`
**Perf:** Same `QueryCursor` pass as existing captures — no extra walk
**Accuracy:** Strict gate (import + heritage). No false positives on plain annotated class attrs

### T4-3: SQLAlchemy detector (Python)

**Touches:**
- `python/queries.scm` — capture `assignment: (call function: (identifier) @sa.column_func arguments: (...))` filtered to `Column` / `mapped_column` / `Mapped`
- `python/parser.rs` — gate on `sqlalchemy` import; resolve type-class from first positional arg
**Pre:** T4-1, T4-2 (shares plumbing)
**Test:** `python_schema_fields.rs::sqlalchemy_column_emits_fields` — `id = Column(Integer, primary_key=True)`
**Accuracy:** `mapped_column` (2.0) + `Column` (1.x) both covered; `Mapped[int]` via T4-2 type annotation path

### T4-4: TypeScript interface detector

**Touches:**
- `typescript/queries.scm:148-152` — walk `interface_body (property_signature name: (...) @field.name type: (type_annotation (_) @field.type))`
- `typescript/parser.rs` — emit `RawSchemaField { framework: "typescript-interface", owner_class: <interface_name> }`. No import gate (interfaces unambiguous)
**Pre:** T4-1
**Test:** `tests/typescript_schema_fields.rs::interface_emits_fields`
**Accuracy:** Type-class for TS: `string`→String, `number`→Float (see OQ-7), `boolean`→Bool, `Date`→Datetime, `Record<...>`/`unknown`/`object`→Json

### T4-5: protobuf detector (`.proto`)

**Touches:**
- Pipeline `pipeline.rs:91` — add `"proto" => ...` arm
- New minimal provider `crates/ecp-analyzer/src/protobuf/{mod,provider,queries.scm}` — query `message_definition name: ... body: (message_body (field name: ... type: ...))`. Uses `tree-sitter-proto`
- Type-class table: `string`→String, `int32`/`int64`/`uint*`→Int, `float`/`double`→Float, `bool`→Bool, `google.protobuf.Timestamp`→Datetime, message/Any/Struct→Json
**Pre:** T4-1
**Test:** `tests/protobuf_schema_fields.rs::message_emits_fields`
**Accuracy:** Message-body fields only; no Service/RPC out-of-scope for #4

### T4-6: OpenAPI detector (`.yaml`/`.yml`/`.json`)

**Touches:**
- `crates/ecp-analyzer/src/yaml/parser.rs` — OpenAPI trigger: file contains `openapi: ` or `swagger: ` at col 0 within first 200 bytes
- New `crates/ecp-analyzer/src/openapi/schema_scan.rs` — walks `components.schemas.<Name>.properties.<field>.type` via `serde_yaml::Value` / `serde_json::Value`
- Type-class: `string` w/ `format: date-time`→Datetime else String; `integer`→Int; `number`→Float; `boolean`→Bool; `object`/`array`→Json
**Pre:** T4-1
**Test:** `tests/openapi_schema_fields.rs::yaml_and_json_components_schemas`
**Perf:** Pre-check is 200-byte string scan — zero cost on non-OpenAPI YAML (k8s manifests, CI configs)
**Accuracy:** `components.schemas` only; inline schemas under `paths.*` deferred (OQ-8)

### T4-7: `SchemaFieldIndex` + `MirrorsField` edge emission

**Touches:**
- `crates/ecp-analyzer/src/resolution/builder.rs` — new Pass-2 sub-pass `pass2_emit_schema_field_mirrors` after framework+fanout (~line 1440)
- Bucketing: `FxHashMap<(name_lowercase, SchemaType), SmallVec<[NodeId; 4]>>` (inline cap=4 covers >90% buckets)
- Per pair `(a, b)` in bucket: score 4 strict checks; ≥4 → MirrorsField confidence 0.9; 3/4 → BlindSpot record `kind: "schema-field-mirror-candidate"`; ≤2 → drop silently
- **Cluster semantics (D3)** — when k ≥ 3 fields share the same `(name, type, class)` triple and all pair-checks pass, the bidirectional-top-1 check is considered satisfied for **every pair in the cluster**, not just k=2. Implementation: if bucket subset has uniform `(name, type, owner_class)`, emit MirrorsField pairwise (k×(k-1)/2 edges) at 0.9. Without this, k=3+ same-class same-name fields all drop to BLIND_SPOT (silent accuracy loss)
**Pre:** T0-1, T0-2, T4-2..T4-6
**Test:** `tests/schema_field_mirror.rs`:
- `test_pair_strict_match_emits_mirrorsfield` — Pydantic `User.email: str` + SQLA `User.email = Column(String)` → MirrorsField 0.9
- `test_three_way_cluster_all_pairs_emit_mirrorsfield` (D3) — Pydantic `User.email` + SQLA `User.email` + protobuf `User.email` → 3 pairs, each 0.9
- `test_partial_match_emits_blindspot` — Pydantic `User.email` + protobuf `User.user_email` (3/4: name differs) → BlindSpot
- `test_different_class_name_blindspot` — `User.email` + `Admin.email` same type → BlindSpot
**Perf:** O(N) bucket build + O(k²) per bucket (k<10). Cluster check adds one extra pass over bucket for uniform-triple detection: still O(k²). Offline only, never on hot paths
**Accuracy:** Four-point strict rubric + cluster semantics for k≥3. Fully deterministic
**Surface:** edge stored; hidden by default in `impact`/`rename`; shown in `inspect` and `find-schema-bindings`

### T4-8: `ecp find-schema-bindings` CLI

**Touches:**
- new `crates/ecp-cli/src/commands/find_schema_bindings.rs`
- `commands/mod.rs` + `main.rs` — register subcommand
- Default format `toon`; output:
```json
{
  "field": "User.email",
  "mirrors": [
    {"name", "owner", "framework", "filePath", "line",
     "tier": "LIKELY_RELATED",
     "checks": {"name": true, "type": true, "class": true, "bidir": true},
     "requires_verification": true}
  ],
  "blind_spot_candidates": [...]
}
```
**Pre:** T4-7
**Test:** `tests/find_schema_bindings_cmd.rs::pydantic_to_sqlalchemy_surface`
**Perf:** Single mmap traversal, same cost class as `inspect`
**Accuracy:** Every entry carries evidence + verification flag — LLM consumer can re-rank

---

## 8. Feature #5 — Event Flow Graph (33 tasks)

### T5-0: Topic-normalization spec lock

**Touches:** new `docs/specs/2026-05-21-event-topic-normalization.md` + `crates/ecp-analyzer/src/event_topic/normalize.rs::canonicalize(&str) -> String`

Normalization rules (locked):
1. Strip prefixes from static list (`prod.`, `dev.`, `staging.`, `<env>.`)
2. Strip suffix `.v[0-9]+`
3. Lowercase
4. Replace `.` `_` `-` `:` `/` with `/`
5. Trim leading/trailing `/`
6. Camel→snake per segment (`OrderCreated` → `order/created`)

**Pre:** none
**Test:** `tests/event_topic_normalize.rs` — 30-row table-driven covering all 6 transformations. **Include negative documentation cases** (R3-F6):
- `order-created` (hyphens) and `order/created` (slashes) BOTH normalize to `order/created` — **this is intentional**; consumers using different separators ARE expected to mirror. Locked by `test_hyphen_and_slash_collapse_to_same_canonical`
- `eu-west-1.order.created` → `eu-west-1/order/created`, `eu-west-2.order.created` → `eu-west-2/order/created` — distinct (correct, region prefixes preserved)
- `tenant-123.order.created` and `tenant-456.order.created` — distinct (correct, tenant IDs preserved)
**Perf:** Pure function

### T5-1: `RawEventTopic` collector + flush

**Touches:**
- new `crates/ecp-analyzer/src/event_topic/mod.rs` — `EventTopicCapture` helper + `flush_event_topics(&mut LocalGraph)`
- Pattern mirrors Celery `pending_celery_refs` flush at `python/parser.rs:527`
- Shared constants table — see OQ-4 about hoisting `EVENT_TOPIC_PACKAGES` into `ecp-core/src/event_libs.rs`
**Pre:** T0-1, T0-2, T5-0
**Test:** `tests/event_topic_collector.rs` — fake captures → flush → enclosing-fn resolution
**Surface:** internal

### T5-2 to T5-31: 30 (lib, lang) detector PRs

Format identical, varies only in (lib, lang) tuple. Each is **one PR**. Coverage matrix:

| Task | Lib | Lang | Status | Package gate |
|---|---|---|---|---|
| T5-2 | Kafka | Python | impl | `kafka, aiokafka, confluent_kafka, faust` |
| T5-3 | Kafka | TypeScript | impl | `kafkajs, node-rdkafka` |
| T5-4 | Kafka | JavaScript | impl | same as T5-3 |
| T5-5 | Kafka | Java | impl | `org.apache.kafka, org.springframework.kafka` |
| T5-6 | Kafka | Go | impl | `segmentio/kafka-go, Shopify/sarama, confluentinc/confluent-kafka-go` |
| T5-7 | Kafka | Rust | impl | `rdkafka` |
| T5-8 | RabbitMQ | Python | impl | `pika, aio_pika, kombu` |
| T5-9 | RabbitMQ | TS | impl | `amqplib, amqp-connection-manager` |
| T5-10 | RabbitMQ | JS | impl | same as T5-9 |
| T5-11 | RabbitMQ | Java | impl | `springframework.amqp, rabbitmq.client` |
| T5-12 | RabbitMQ | Go | impl | `rabbitmq/amqp091-go, streadway/amqp` |
| T5-13 | RabbitMQ | Rust | impl | `lapin, amiquip` |
| T5-14 | SQS | Python | impl | `boto3, aioboto3` — topic = QueueUrl |
| T5-15 | SQS | TS | impl | `@aws-sdk/client-sqs` |
| T5-16 | SQS | JS | impl | same as T5-15 |
| T5-17 | SQS | Java | impl | `software.amazon.awssdk.services.sqs` |
| T5-18 | SQS | Go | impl | `aws/aws-sdk-go-v2/service/sqs` |
| T5-19 | SQS | Rust | impl | `aws-sdk-sqs` |
| T5-20 | Celery | Python | impl (extend existing detection at `python/parser.rs:663`) | already gated by `CELERY_REQUIRED` |
| T5-21 | Celery | TypeScript | **SKIP** | no first-class TS Celery client |
| T5-22 | Celery | JavaScript | **SKIP** | same |
| T5-23 | Celery | Java | **SKIP** | `celery-java` exists but <1% adoption (OQ-10) |
| T5-24 | Celery | Go | **SKIP** | same |
| T5-25 | Celery | Rust | **SKIP** | same |
| T5-26 | Redis pub/sub | Python | impl | `redis, aioredis` |
| T5-27 | Redis pub/sub | TS | impl | `redis, ioredis` |
| T5-28 | Redis pub/sub | JS | impl | same as T5-27 |
| T5-29 | Redis pub/sub | Java | impl | `springframework.data.redis, redis.clients.jedis, io.lettuce.core` |
| T5-30 | Redis pub/sub | Go | impl | `redis/go-redis, gomodule/redigo` |
| T5-31 | Redis pub/sub | Rust | impl | `redis` crate |

**Per-task spec (each non-SKIP):**

```
**Touches:** crates/ecp-analyzer/src/<lang>/parser.rs (push to pending_event_topics);
            crates/ecp-analyzer/src/<lang>/queries.scm (add producer/consumer capture);
            crates/ecp-analyzer/src/event_topic/<lib>.rs (lib-specific arg-pattern matcher)
**Pre:** T5-1
**Test:** tests/<lang>_events_<lib>.rs covering:
  - literal-string topic → RawEventTopic confidence 1.0
  - variable-arg topic → BlindSpot kind: "<lib>-dynamic-topic"
  - import gate negative: no <lib> import → zero captures
**Perf:** Existing QueryCursor pass; lib-specific arg matcher reads kwargs from same node already in scope — no re-parse
**Accuracy:** topic_literal None whenever analyzer can't statically prove a literal — never fabricate
**Surface:** RawEventTopic → EventTopic + Publishes/Subscribes (deterministic 1.0); visible in default impact/inspect
```

### T5-32: Coverage matrix doc

**Touches:** new `docs/specs/2026-05-21-event-detector-coverage.md` — all 30 tuples with SKIP reasons
**Pre:** T5-31
**Test:** doc only
**Accuracy:** Documents "honest no-data" SKIPs explicitly

### T5-33: `EventTopicMirror` heuristic edges

**Touches:** `crates/ecp-analyzer/src/resolution/builder.rs` — new Pass-2 sub-pass `pass2_emit_event_topic_mirrors` after T4-7. Group `EventTopic` by `canonicalize(topic_literal)`; within group, Publisher↔Subscriber pairs with differing raw literals get `EventTopicMirror` confidence 0.9. Cross-lib pairs explicit (Kafka↔RabbitMQ same normalized name → mirror)

**Cluster semantics (D3 parity with T4-7)**: when k≥3 EventTopic nodes share canonical key + direction-pair, emit pairwise (k×(k-1)/2 edges) at 0.9; do NOT silently drop to BLIND_SPOT just because top-1 is ambiguous in larger cluster
**Pre:** T-H1 (per §10 sequencing — heuristic filter must exist); T5-1; **T5-33 subset gate (D7)**: at least 1 Publish detector + 1 Subscribe detector merged for each lib that the test fixture exercises. Concrete: Kafka needs T5-2 (Python Publish) AND any Kafka Subscribe detector; same for RabbitMQ/SQS/Celery/Redis. Does NOT require all 25 detectors merged
**Test:** `tests/event_topic_mirror.rs`:
- `test_kafka_to_rabbitmq_cross_lib_mirror` — Kafka producer `"order.created"` + RabbitMQ consumer `"OrderCreated"` → both normalize to `order/created` → one mirror edge
- `test_three_way_event_cluster_emits_all_pairs` (D3) — 3 systems publishing/subscribing `order.created` → 3 mirror edges
- `test_subset_gate_kafka_only` — only Kafka detectors merged; Kafka↔Kafka mirrors emit; no RabbitMQ mirrors expected
**Perf:** O(N) group + O(k²) intra-group (k<5 typical). Pass runs once per offline reindex
**Accuracy:** Edge `reason` carries normalized key + lib pair for verification
**Surface:** heuristic, hybrid-routed per surface rules

### T5-34: `ecp find-event-mirrors` CLI

**Touches:** new `crates/ecp-cli/src/commands/find_event_mirrors.rs`; args `topic` (string) or `--canonical <key>`
**Pre:** T5-33
**Test:** `tests/find_event_mirrors_cmd.rs`
**Surface:** primary content (explicit-opt-in)

---

## 9. Feature #10 — Transaction Boundary (4 tasks, P2)

### T10-1: Annotation-based detection (Java/Kotlin/Python decorators)

**Touches:**
- `crates/ecp-analyzer/src/java/parser.rs` — `@Transactional` in `decorators` → push `RawTxScope { source_pattern: "java-transactional" }`
- `crates/ecp-analyzer/src/kotlin/parser.rs` — same
- `crates/ecp-analyzer/src/python/parser.rs` — `@transaction.atomic` / `@db_session` → push appropriate source_pattern
- C# `[Transaction]` — deferred (OQ-11)
**Pre:** T0-1, T0-2
**Test:** `tests/{java,kotlin,python}_tx_scope_annotation.rs`
**Perf:** Zero extra work — reads existing `decorators: Vec<String>` (`types.rs:22`)
**Accuracy:** Exact decorator-text match (post-strip `@` / `#[`). No false positives on custom-named decorators

### T10-2: Context-manager detection (Python `with`)

**Touches:**
- `python/queries.scm` — capture `with_statement` whose call resolves to `transaction.atomic` / `db.transaction` / `conn.begin` / `session.begin` / `engine.begin` as `@tx.with_context_target`
- `python/parser.rs` — process capture; flush mirrors `pending_depends` pattern at line 519
**Pre:** T0-2
**Test:** `tests/python_tx_scope_with.rs` — `with transaction.atomic():` inside function → tx_scope anchored
**Accuracy:** Call-text suffix match is whitelist-based; arbitrary `with foo.atomic():` doesn't fire

### T10-3: Builder — `TransactionScope` node + `OpensTxScope` edge

**Touches:** `crates/ecp-analyzer/src/resolution/builder.rs` — new sub-pass `pass2_emit_tx_scopes`. For each `RawTxScope`, materialize:
- New `NodeKind::TransactionScope` node (one per detected scope, with span)
- `OpensTxScope` edge from enclosing Function → TransactionScope
- A function with 2 `with transaction.atomic():` blocks → 2 TransactionScope nodes, 2 OpensTxScope edges

**NOT a `Function.is_transactional: bool`** — adding bool to every Node regresses memory (~1% of functions are transactional). Sparse edge representation wins.

**Pre:** T0-1, T0-2, T10-1, T10-2
**Test:** `tests/tx_scope_edges.rs` — Python function with two nested `with transaction.atomic():` blocks → 2 TransactionScope nodes, 2 OpensTxScope edges
**Surface:** deterministic — visible in default impact/inspect

### T10-4: `find-transaction-patterns` CLI (heuristic — Saga + Outbox)

**Touches:** new `crates/ecp-cli/src/commands/find_tx_patterns.rs`. Does NOT push to graph — pull-time query:
- **Outbox detection:** tables/structs/classes named `outbox_event*` / `event_outbox` / `message_outbox` cross-referenced with `EventTopic Publish` in functions reachable from outbox-writing functions
- **Saga detection:** name-pair `<verb>_<noun>` ↔ `compensate_<verb>_<noun>` / `undo_<verb>_<noun>` / `rollback_<verb>_<noun>` on same class
- All findings tagged `confidence < 0.9`, marked `requires_verification: true` — never enters graph
**Pre:** T5-33, T10-3
**Test:** `tests/find_tx_patterns_cmd.rs` — fixture with `OutboxEvent` table + Kafka producer + Saga compensate methods
**Perf:** Single graph traversal + name-pattern scan. Bounded by N(Class) + N(Method); 25k-file index <200ms
**Accuracy:** Heuristic by design; well-known naming patterns; confidence reflects naming ambiguity
**Surface:** primary content (explicit-opt-in)

---

## 10. Hybrid surface plumbing (3 tasks)

**Critical sequence note:** These tasks must land **BEFORE** any of T4-7 / T5-33 reaches `main`, otherwise heuristic edges leak into `impact`/`rename` before filters exist. Sequence: Phase 0 → Phase 4 (these 3 tasks) → Phase 1-3.

### T-H1: `impact` filter

**Touches:** `crates/ecp-cli/src/commands/impact.rs:31-91` — add `#[arg(long, default_value_t = false)] pub include_heuristic: bool` + `--confidence-threshold` + `--explain-confidence`. BFS edge-traversal filters by `!edge.rel_type.is_heuristic() || args.include_heuristic`. Hidden-count attached via `hidden_heuristic_edges: N` field in output
**Pre:** T0-1
**Test:** `tests/impact_heuristic_filter.rs` — default does not traverse; `--include-heuristic` traverses with two sections never merged
**Perf:** One extra `is_heuristic()` branch per edge in BFS — `const fn`, zero alloc

### T-H2: `rename` hard-exclude heuristic (action) + structural count surface

**Touches:**
- `crates/ecp-cli/src/commands/rename.rs` — when planner walks inbound edges, skip `rel_type.is_heuristic()`. Add assertion test that fails if heuristic edge ever reaches the file-collection set
- Compute (do NOT traverse for action) the count of heuristic edges touching the renamed symbol; emit as structural field `heuristic_mirrors_not_touched: <N>` in output
- New flag `--show-heuristic-mirrors` — opt-in expansion to embed full candidate list (same format as `find-schema-bindings`) in rename output

**Why count must surface (revised from prior draft):**

Earlier draft had rename output stay silent on heuristic mirrors, relying on agent to remember to call `ecp find-schema-bindings`. **This is wrong** — silent output gives LLM no trigger, so it assumes rename is complete. Surfacing the count as a structural field:

- Costs ~1 graph-edge filter pass (no action, no traversal of heuristic cascade)
- Triggers agent investigation when `count > 0`
- Action stays 100% deterministic (count is informational, never used to mutate)
- Symmetric with hidden-edges pattern already established in T-H1's `hidden_heuristic_edges` field

Default output:
```
Renamed:
  - models/user.py:42
  - tests/test_user.py:18
heuristic_mirrors_not_touched: 3
hint: "ecp find-schema-bindings User.email" or rerun with --show-heuristic-mirrors
```

With `--show-heuristic-mirrors`:
```
Renamed: [...]
heuristic_mirrors:
  - UserResponse.email   [LIKELY_RELATED]   checks: name✓ type✓ class✓ bidir✓
                                              requires_verification: true
  - Admin.email          [BLIND_SPOT]       checks: name✓ type✓ class✗ bidir✗
                                              requires_verification: true
```

**Pre:** T0-1
**Test:** `tests/rename_excludes_heuristic.rs`:
- `test_rename_does_not_touch_heuristic_files` — `MirrorsField` from `User.email` Pydantic → `User.email` SQLAlchemy; renaming Pydantic does NOT touch SQLAlchemy file
- `test_rename_output_surfaces_count_default` — output has `heuristic_mirrors_not_touched: 1` structural field
- `test_rename_show_flag_embeds_candidate_list` — `--show-heuristic-mirrors` output has full candidate list with check breakdown
- `test_rename_zero_count_omits_hint_line` — when no heuristic mirrors exist, count=0 field shown but no hint line (avoid noise)
**Accuracy:** **Rename mutation is 100% deterministic. Heuristic count is informational — it never participates in the file-collection set.**

### T-H3: `inspect` separate heuristic section

**Touches:** `crates/ecp-cli/src/commands/inspect.rs:79-217` — split `build_inspect_block` outgoing/incoming into `heuristic_incoming` / `heuristic_outgoing` (separate maps). Top-level `heuristic_note: "verify before acting — candidate edges, may have false positives"` when non-empty. Per-candidate check breakdown rendered
**Pre:** T0-1
**Test:** `tests/inspect_heuristic_section.rs` — deterministic edges in `outgoing`, MirrorsField in `heuristic_outgoing`, note present, checks visible
**Surface:** shown, structurally labeled

---

## 11. Documentation + parity (2 tasks)

### T-P1: 14-lang parity baselines refresh

**Touches:** `scripts/parity/round*_baseline.txt` regenerate (covers SchemaField/EventTopic/TransactionScope counts); `scripts/parity/dump_ref.py` extend dump query
**Pre:** all Phases 1-3
**Test:** `python scripts/benchmark/benchmark_ecp.py` cold-cache stays within ±5% of pre-change baseline
**Accuracy:** Locks new schema so regressions are caught

### T-P2: User-doc updates

**Touches:**
- `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` — document 3 new subcommands
- `crates/ecp-cli/src/commands/admin/install_hook.rs` skill text — register new commands in auto-injected CLAUDE.md
- `README.md` — one-paragraph blurb per feature
**Pre:** T4-8, T5-34, T10-4
**Test:** doc PR review only

---

## 12. Defense lines (CI gates — separate sub-spec)

Per CLAUDE.md priority-1 "per-query latency <30ms" + "hot path no new alloc / file I/O":

1. **Bench baseline + CI gate** — `scripts/benchmark/benchmark_ecp.py` produces baseline JSON committed to repo; CI fails on >5% regression
2. **Hot-path no-alloc gate** — `dhat` profile of `pre_tool_use::handle`, `compute_hits`, `dispatch_by_mode` in CI; PR fails on any new allocation
3. **#4 / #5 index normalization spec locked first** — T4-7 SchemaFieldIndex bucketing and T5-33 EventTopicMirror normalization rules settled at sub-spec before implementation
4. **Incremental cross-binding invalidation** — when SchemaField/EventTopic node added/removed, only its bucket re-binds (O(k²) k<10), not full N². Locked at index design time

---

## 13. Open design questions

| # | Question | Recommendation |
|---|---|---|
| **OQ-1** | xxh3_64 vs xxh3_128 for `Node.uid` | **RESOLVED (D2): 64-bit** + D1 graceful collision → BlindSpot recovery. Doubling to 128-bit doesn't justify 2× memory + 2× compare cycles at ecp's scale ceiling |
| **OQ-2** | C function-pointer vtables — owner_class = struct type or instance? | **Struct type** (`foo_ops_t` not `foo_ops`). LLM queries "what implements foo_ops_t" more common |
| **OQ-3** | C++/Java/C# method overloads (same name, owner, different signatures) | **Defer.** UID inputs don't include parameter types. If parity tests hit collisions, dedicated mini-spec post-T1-10 |
| **OQ-4** | Overlay durability — persist or rebuild per CLI call? | **RESOLVED (D6): Persist + zero-copy merge** (T7-5 as written). Aligns with perf-first; rebuild-per-CLI is wasteful |
| **OQ-5** | Format v4→v5 reindex strategy | **Inline blocking** on first query post-upgrade + atomic backup `graph.bin.v4.bak` + hard error on reindex failure (no auto-ensure loop) per R3-F1 |
| **OQ-6** | Tier granularity — 2 (LIKELY/BLIND) or 3 (+ POSSIBLY)? | **2 tiers**. Haiku review consensus: granularity via per-candidate check breakdown, not more tiers |
| **OQ-7** | TS `number` → `SchemaType::Int` or `Float`? | **Float**. TS has no integer/float split. Float avoids silent type-mismatch when bound to Java `int` |
| **OQ-8** | OpenAPI: scan inline `paths.*.responses.*` schemas? | **`components.schemas` only v1**. Add `--include-inline` follow-up. Inline schemas 3× node count, mostly redundant |
| **OQ-9** | `EVENT_TOPIC_PACKAGES` shared with `tool_map.rs:40-89 PACKAGE_CATEGORY`? | **Yes** — hoist into `ecp-core/src/event_libs.rs`. Single source of truth, pre-T5-1 refactor task |
| **OQ-10** | Celery in Java/Go/Rust (`celery-java`, `gocelery`) | **Skip in v1**, document in T5-32. Revisit on user-repo adoption signal |
| **OQ-11** | C# `[Transaction]` (Spring.NET) | **Defer**. No canonical attribute; EF uses `using` (closer to T10-2 model). Half-implementing risks confusion |
| **OQ-12** | TransactionScope node vs `Function.is_transactional` bool | **Node**, not bool. ~1% transactional functions → sparse edge wins over per-Node byte overhead |

**Reviewer-correlated decisions applied (D1-D7):**

- **D1 (R3-F3)** UID collision recovery → graceful BlindSpot, no panic (applied in §2.1 + T1-5)
- **D2 (OQ-1)** Hash width → 64-bit (saves 8B/node + 1 cycle per compare; D1 handles 50M+ scale)
- **D3 (R3-F5)** MirrorsField k≥3 cluster semantics → pairwise emit at 0.9 (applied in T4-7 + T5-33)
- **D4 (R2-F3)** RawSchemaField → StrRef from day-1 (applied in T0-2)
- **D5 (R2-F4)** T1-1+T1-3 merged into single 14-lang PR (applied in §5)
- **D6 (OQ-4)** Overlay durability → persist + zero-copy (T7-5 as written, no scope reduction)
- **D7 (R1-F5)** T5-33 subset → ≥1 Publish + ≥1 Subscribe per lib used in fixture (applied in T5-33)

---

## 14. Dependency graph + PR ordering

```
T0-1 ──┬──→ T0-2 ──→ T-H1 ──┬──→ T4-7        (T-H1 → T4-7 enforces hybrid filter exists before heuristic edge enters graph)
       │            ├──→ T-H2 ──→ T5-33      (T-H1 → T5-33 same enforcement)
       │            └──→ T-H3
       │
       │      Note: T0-1 ships with structural CI gate (`test_impact_default_hides_mirrors_field`)
       │      that FAILS until T-H1 lands. PR merging T4-7 before T-H1 fails CI mechanically,
       │      not procedurally.
       │
       └──→ T1-1 (14-lang, was T1-1+T1-3 merged per D5) ─┐
            T1-2 ──→ T1-4 ──→ T1-5 ──┬──→ T1-6 ──→ T1-11
                                      ├──→ T1-7 (format bump; also dep T1-4)
                                      ├──→ T1-8, T1-9
                                      ├──→ T1-10
                                      └──→ T1-12

T0-2 ──→ T7-1 ──→ T7-2 ──┬──→ T7-4 ──→ T7-5 ──→ T7-6 ──→ T7-7  (T7-1 dep T0-2 per R1-F3: LocalGraph new vecs)
        T7-3 ─────────────┘

T0-2 ──→ T4-1 ──→ T4-2..T4-6 (5 parallel) ──→ T4-7 ──→ T4-8
T0-2 ──→ T5-0 ──→ T5-1 ──→ T5-2..T5-31 (25 parallel) ──→ T5-33 (subset gate per D7: at least 1 Publish + 1 Subscribe detector per lib) ──→ T5-34
                                                          └──→ T5-32 (coverage doc)
T0-2 ──→ T10-1, T10-2 (parallel) ──→ T10-3 ──→ T10-4

Phase 5: T-P1, T-P2 (after all phases done)
```

**Total: 2 + 12 + 7 + 8 + 33 + 4 + 3 + 2 = 71 tasks → 54 PRs** (25 detector PRs in Phase 2 parallelizable; 5 Celery SKIPs not PR'd).

---

## 15. Out of scope (future roadmaps)

- Vector embedding semantic layer
- Runtime trace / OpenTelemetry integration
- Ownership / CODEOWNERS social layer
- Agent memory back-feed to graph
- CI result / production failure ingestion

---

## 16. Acceptance for this spec

User reviews Phase 6 (this section's existence). Sign-off blocks code work.

After sign-off:
1. Open issue per Phase (Phase 0 / Phase 4 hybrid surface / Phase 1 #1 / Phase 2 #7 / Phase 3 #4 / Phase 3 #5 / Phase 3 #10 / Phase 5)
2. PR-per-task, atomic, test-first, 14-lang parity where applicable
3. Each task references this doc anchor (e.g., "Implements T1-3 from `docs/plans/2026-05-21-symbol-graph-core-roadmap.md`")
