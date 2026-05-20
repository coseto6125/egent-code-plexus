# Symbol-Graph Core Roadmap — #1 / #4 / #5 / #7 / #10

**Date:** 2026-05-21
**Status:** Draft — awaiting user review (Phase 6)
**Target:** `docs/plans/` per repo convention (`docs/plans/<date>-<name>.md`)
**Total PRs:** 54 atomic, test-first commits

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
| Collision policy | `<10⁷ symbols` → ~2.7e-6 global risk. Builder triple-check `(uid, name, owner_class)` on insert; panic+log on real collision |

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

**Why `rename` hard-exclude is safe (not signal loss):**

Agents using `ecp rename --symbol users.email --new-name email_address` do NOT lose visibility on possible mirrors — they have multiple deterministic fallback paths:

1. **`grep`** — `grep -rn "users\.email" .` finds string-literal references the AST can't see
2. **`ecp find-schema-bindings users.email`** — explicit pull-CLI returns LIKELY_RELATED + BLIND_SPOT candidates with check breakdown
3. **`ecp inspect users.email`** — shows `Possible mirrors` section (heuristic visible, just not actioned)

Rename hard-excludes heuristic edges because **mutation cannot be undone**, not because heuristic info is unavailable. Agent workflow: rename → verify scope via grep / find-schema-bindings → propagate manually if confirmed.

### 2.3 Tier model

Internal confidence computed but never surfaced. Maps to tier label:

| Internal confidence | Tier | Default visibility |
|---|---|---|
| `≥ 0.85` (all 4 strict checks) | `LIKELY_RELATED` | shown |
| `0.65 – 0.85` (3/4 checks) | `BLIND_SPOT` | hidden unless `--include-blindspot` |
| `< 0.65` (≤2/4) | not emitted | — |

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

### T0-1: Append schema variants + heuristic classifier

**Touches:**
- `crates/ecp-core/src/graph.rs:75-132` — append to `NodeKind`: `SchemaField`, `EventTopic`, `TransactionScope`
- `crates/ecp-core/src/graph.rs:206-224` — append to `RelType`: `MirrorsField`, `Publishes`, `Subscribes`, `EventTopicMirror`, `OpensTxScope`
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
**Test:** `crates/ecp-core/tests/graph_schema.rs` — `from_str` round-trip per variant; hard-code discriminant indices to lock append-only
**Perf:** enum widening only
**Surface:** internal

### T0-2: Extend `LocalGraph` with new raw-ref vectors

**Touches:**
- `crates/ecp-core/src/analyzer/types.rs:115-126` — add `schema_fields: Vec<RawSchemaField>`, `event_topics: Vec<RawEventTopic>`, `tx_scopes: Vec<RawTxScope>`
- Same file ~line 92 — three new `Raw*` structs with rkyv derives

```rust
pub struct RawSchemaField {
    pub name: String,
    pub type_class: SchemaType,         // String / Int / Float / Bool / Datetime / Json / Other
    pub owner_class: String,
    pub framework: &'static str,
    pub span: (u32, u32, u32, u32),
}
pub struct RawEventTopic {
    pub topic_literal: Option<String>,  // None = dynamic; emit BlindSpot
    pub direction: PubSub,
    pub lib: &'static str,
    pub enclosing_fn: String,
    pub span: (u32, u32, u32, u32),
}
pub struct RawTxScope {
    pub enclosing_fn: String,
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

### T1-1: Add `owner_class` to `RawNode` IR + scaffold

**Touches:** `crates/ecp-core/src/analyzer/types.rs` (RawNode), `crates/ecp-analyzer/src/post_process/class_membership.rs`
**Pre:** none
**Test:** `tests/owner_class_scaffold.rs` — `RawNode.owner_class` field exists, defaults `None`; `class_membership` post-pass populates for Python/TS/JS (3-lang min; full 14 in T1-3)
**Perf:** IR-only, `Option<String>` heap-pointer; promoted to `StrRef` in T1-4
**Accuracy:** No semantic change yet — owner_class read but not hashed

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

### T1-3: 14-lang parser owner_class extraction

**Touches:** per-parser owner_class emission for Rust (`rust/parser.rs:336-351`), Python (`python/parser.rs:368-380`), TS, JS, Java, Kotlin, C#, Go, PHP, Ruby, Swift, C, C++, Dart
**Pre:** T1-1, T1-2
**Test:** `tests/owner_class_<lang>.rs` × 14 + aggregate `owner_class_parity_14lang.rs`
- Per-lang: two methods of same name on different classes → owner_class distinguishes
- C special: `static struct foo_ops = { .open = my_open }` → `my_open.owner_class = Some("foo_ops_t")` (struct TYPE, see OQ-2)
**Perf:** Parser hot path; reuse tree-sitter capture buffers, no extra walks
**Accuracy:** 14-lang parity per CLAUDE.md mandate

### T1-4: Promote `owner_class` to `Node` struct (StrRef)

**Touches:** `crates/ecp-core/src/graph.rs:228-235` — add `pub owner_class: Option<StrRef>`; builder interns via `string_pool.add()`
**Pre:** T1-3
**Test:** `tests/node_owner_class_field.rs` — rkyv round-trip
**Perf:** `Option<StrRef>` = `Option<u32>` = 8B with niche. ~9k symbols × 8 = 72 KB negligible
**Accuracy:** rkyv layout change → format bump in T1-7

### T1-5: Switch `Node.uid` from `StrRef` to `u64`

**Touches:** `crates/ecp-core/src/graph.rs:228-235` (uid type); `crates/ecp-analyzer/src/resolution/builder.rs:344-368` (drop `uid_buf` + StringPool insert; call `uid::compute(kind, path, owner_class, name)`)
**Pre:** T1-2, T1-4
**Test:** `tests/uid_u64_builder.rs`:
- `test_builder_uid_matches_helper` for every Node
- `test_real_collisions_resolved_in_ecp_self` — index ecp itself, `default×3` in config.rs now 3 distinct u64s
**Perf:** Eliminates 1 string-pool insert + 1 StrRef lookup per node per query — load-bearing win
**Accuracy:** Collision risk 2.7e-6 acceptable; contingency triple-check in T1-11

### T1-6: Resolver `HashMap<String, NodeId>` → `FxHashMap<u64, NodeId>`

**Touches:** `crates/ecp-analyzer/src/resolution/resolver.rs:62`, `builder.rs:1477`, symbol-table internals
**Pre:** T1-5
**Test:** `tests/resolver_fxhash_uid.rs` + `benches/resolver_lookup.rs` asserting ≥2× speedup vs baseline
**Perf:** Hot path for `compute_hits` (find.rs:964). u64 key = zero string hash, zero String alloc

### T1-7: Bump `GRAPH_FORMAT_VERSION` 4 → 5 + auto-reindex

**Touches:** `crates/ecp-core/src/graph.rs` const; `crates/ecp-cli/src/engine.rs:122-170` version-check branch (inline blocking reindex on v4 detect)
**Pre:** T1-5, T1-6
**Test:** `tests/format_upgrade_v4_to_v5.rs` — synthetic v4 graph.bin → `ecp inspect foo` → reindex spawned, result `format_version == 5`
**Perf:** One-time post-upgrade cost
**⚠️ FORMAT BUMP**

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
**Pre:** T1-3
**Test:** All 14 `owner_class_<lang>.rs` still pass + `no_impl_target_sentinel_regression.rs`
**Accuracy:** Single source of truth

---

## 6. Feature #7 — Incremental Indexing First-Class (7 tasks)

### T7-1: `parse_to_fragment()` real implementation

**Touches:** `crates/ecp-cli/src/session/overlay_writer.rs:163-166` (stub returning `vec![]`); reuse `extract_symbols()` line 276-299
**Pre:** none
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

### T7-4: Wire `reanalyze_files()` into agent commands + hook

**Touches:** call sites in `commands/{impact,inspect,find}.rs`; hook `pre_tool_use.rs:23`
**Pre:** T7-1, T7-2, T7-3
**Test:** `tests/incremental_wired.rs`:
- `test_edit_file_then_impact_sees_new_symbol_without_full_reindex`
- `test_pre_tool_use_hook_no_alloc_hot_path` — `dhat` asserts zero new allocs ⚠️ **CLAUDE.md hot-path**
**Perf:** Dirty-set check = single FxHashSet lookup, no I/O, no String build. `compute_hits` reads overlay via rkyv archived access only

### T7-5: Working-tree overlay zero-copy merge

**Touches:** `crates/ecp-core/src/session/overlay.rs` — remove `#![allow(dead_code)]`; add `merge_archived(...) -> impl Iterator<Item=&ArchivedNode>` (overlay wins on uid match)
**Pre:** T7-4
**Test:** `tests/overlay_merge_zero_copy.rs` — override / addition / deletion; `dhat` zero-alloc
**Perf:** rkyv archived only; overlay-uid FxHashSet<u64> built once per query

### T7-6: Skip class_membership/resolver on unchanged symbol bodies

**Touches:** `crates/ecp-cli/src/reanalyze.rs:67` — diff per-symbol hashes (T7-2); re-run only changed-hash subset
**Pre:** T7-2, T7-4
**Test:** `tests/incremental_skips_unchanged_symbols.rs` — mtime touch skips resolver; 1-of-5 edit processes 1; AtomicUsize counter
**Perf:** Largest incremental win
**Accuracy:** Must NOT skip when (a) file's import set changed OR (b) shadow-candidates set changed — explicit guards

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
**Pre:** T0-1, T0-2, T4-2..T4-6
**Test:** `tests/schema_field_mirror.rs` — Pydantic `User.email: str` + SQLA `User.email = Column(String)` → MirrorsField 0.9; Pydantic `User.email` + protobuf `User.user_email` (3/4: name differs) → BlindSpot
**Perf:** O(N) bucket build + O(k²) per bucket (k<10). Offline only, never on hot paths
**Accuracy:** Four-point strict rubric is the entire confidence model — no learned weights, fully deterministic
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
**Test:** `tests/event_topic_normalize.rs` — 30-row table-driven covering all 6 transformations
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
**Pre:** T5-1, T5-2..T5-31 subset
**Test:** `tests/event_topic_mirror.rs` — Kafka producer `"order.created"` + RabbitMQ consumer `"OrderCreated"` → both normalize to `order/created` → one mirror edge
**Perf:** O(N) group + O(k²) intra-group (k<5 typical)
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

### T-H2: `rename` hard-exclude heuristic

**Touches:** `crates/ecp-cli/src/commands/rename.rs` — when planner walks inbound edges, skip `rel_type.is_heuristic()`. Add assertion test that fails if heuristic edge ever reaches the file-collection set.

Output post-rename includes a one-line guidance: `// post-rename hint: grep "<old-name>" or 'ecp find-schema-bindings <old-name>' to verify heuristic mirrors`. Keeps agent workflow obvious without making rename do guesswork.

**Pre:** T0-1
**Test:** `tests/rename_excludes_heuristic.rs` — `MirrorsField` from `User.email` Pydantic → `User.email` SQLAlchemy; renaming Pydantic does NOT touch SQLAlchemy file. Output contains grep/find-schema-bindings hint line
**Accuracy:** **Rename is 100% deterministic. Heuristic edges represent guesses; rename mutates files and cannot guess.** Agent workflow for cross-schema rename: (1) `ecp rename` deterministic core, (2) `ecp find-schema-bindings` or `grep` to enumerate heuristic mirrors, (3) per-mirror manual rename decision

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
| **OQ-1** | xxh3_64 vs xxh3_128 for `Node.uid` | **64-bit** + T1-11 contingency triple-check. 2.7e-6 collision risk acceptable; 128-bit doubles uid memory with no real-world payoff |
| **OQ-2** | C function-pointer vtables — owner_class = struct type or instance? | **Struct type** (`foo_ops_t` not `foo_ops`). LLM queries "what implements foo_ops_t" more common |
| **OQ-3** | C++/Java/C# method overloads (same name, owner, different signatures) | **Defer.** UID inputs don't include parameter types. If parity tests hit collisions, dedicated mini-spec post-T1-10 |
| **OQ-4** | Overlay durability — persist or rebuild per CLI call? | **Rebuild v1**, persist as follow-up. Simpler; rebuild cost ~ms per edited file |
| **OQ-5** | Format v4→v5 reindex strategy | **Inline blocking** on first query post-upgrade. v4 schema unreadable under v5; cannot degrade-mode |
| **OQ-6** | Tier granularity — 2 (LIKELY/BLIND) or 3 (+ POSSIBLY)? | **2 tiers**. Haiku review consensus: granularity via per-candidate check breakdown, not more tiers |
| **OQ-7** | TS `number` → `SchemaType::Int` or `Float`? | **Float**. TS has no integer/float split. Float avoids silent type-mismatch when bound to Java `int` |
| **OQ-8** | OpenAPI: scan inline `paths.*.responses.*` schemas? | **`components.schemas` only v1**. Add `--include-inline` follow-up. Inline schemas 3× node count, mostly redundant |
| **OQ-9** | `EVENT_TOPIC_PACKAGES` shared with `tool_map.rs:40-89 PACKAGE_CATEGORY`? | **Yes** — hoist into `ecp-core/src/event_libs.rs`. Single source of truth, pre-T5-1 refactor task |
| **OQ-10** | Celery in Java/Go/Rust (`celery-java`, `gocelery`) | **Skip in v1**, document in T5-32. Revisit on user-repo adoption signal |
| **OQ-11** | C# `[Transaction]` (Spring.NET) | **Defer**. No canonical attribute; EF uses `using` (closer to T10-2 model). Half-implementing risks confusion |
| **OQ-12** | TransactionScope node vs `Function.is_transactional` bool | **Node**, not bool. ~1% transactional functions → sparse edge wins over per-Node byte overhead |

---

## 14. Dependency graph + PR ordering

```
T0-1 ──┬──→ T0-2 ──→ T-H1, T-H2, T-H3 (hybrid surface, MUST land before any heuristic edge does)
       │
       └──→ T1-1, T1-2 ──→ T1-3 ──→ T1-4 ──→ T1-5 ──┬──→ T1-6 ──→ T1-11
                                                     ├──→ T1-7 (format bump)
                                                     ├──→ T1-8, T1-9
                                                     ├──→ T1-10
                                                     └──→ T1-12

T7-1 ──→ T7-2 ──┬──→ T7-4 ──→ T7-5 ──→ T7-6 ──→ T7-7
T7-3 ───────────┘

T0-2 ──→ T4-1 ──→ T4-2..T4-6 (parallel) ──→ T4-7 ──→ T4-8
T0-2 ──→ T5-0 ──→ T5-1 ──→ T5-2..T5-31 (25 parallel) ──→ T5-32 → T5-33 → T5-34
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
