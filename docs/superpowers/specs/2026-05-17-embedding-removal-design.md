# Embedding Hard-Delete — Design

**Status**: design approved, implementation pending
**Date**: 2026-05-17
**Scope**: remove all embedding-related design from `code-graph-nexus-rs`, return to BM25-only code-intelligence graph.

## Motivation

`fastembed = "5"` pulls ONNX runtime into both `cgn-analyzer` and `cgn-cli`, bloating binary size and cold-start. The embedding pipeline adds 199 lines (`embeddings.rs`) + 53 builder integration points + 32 search-command branches, yet the design has not proven its value: vector / hybrid search modes are rarely the right answer for code-intelligence queries (BM25 + symbol-graph traversal already covers the high-signal cases), and the cold-start cost (~1–2 s per CLI invocation when embeddings are on) directly violates the per-query <30 ms target listed in `CLAUDE.md`. Cutting the feature reclaims the perf headroom and removes a large surface that needed parallel-quality coverage.

## Scope

**Remove:**

- `crates/cgn-analyzer/src/embeddings.rs` (entire module, 199 lines)
- `crates/cgn-cli/src/embedder.rs` (entire module, 22 lines)
- `fastembed = "5"` dependency from both `cgn-analyzer` and `cgn-cli` `Cargo.toml`
- `ZeroCopyGraph.embeddings: Option<Vec<Vec<f32>>>` field (rkyv schema)
- `EmbeddingConfig` struct (`cgn-core::config`) + default fns + TUI Embedding group
- `BranchEntry.embedding_status` field (`cgn-core::registry::store`)
- Search modes `Vector` / `Hybrid` / `Auto` (retain `Bm25` only; `--mode bm25` becomes a no-op alias, other values fail clap validation)
- `cgn admin index --embeddings` / `--drop-embeddings` flags
- `auto_ensure::embeddings_present` + the rebuild-state preservation logic that calls it
- All 53 builder integration points in `resolution/builder.rs`
- Dedicated tests: `auto_ensure_embeddings.rs`, `search_vector_fallback.rs`
- Stale plan doc: `docs/plans/2026-05-16-vector-hybrid-search.md`

**Keep:**

- `tantivy` / BM25 lexical index — unrelated to embeddings, remains the search backbone
- `ScoreSource` enum variants `Bm25` / `Substring`; remove only `Cosine` / `Rrf`
- Historical audit records (e.g. `docs/superpowers/specs/2026-05-16-concurrency-audit-findings.md`) — do not rewrite history

## Change strategy: single atomic PR

Approach evaluated against a two-phase split (user-facing first, internal cleanup second). Single PR wins on:

| Dimension | Single PR (chosen) | Two-phase |
|---|---|---|
| Atomicity | Schema bump + design removal synchronous | Phase 1 leaves `ZeroCopyGraph.embeddings = None` dead field |
| Review narrative | One story: "embedding is gone" | Phase 1 cannot self-explain why user-facing is removed but internals remain |
| Surgical-changes principle | Every line traces to removal | Phase 1 must preserve meaningless fields |
| User-visible churn | One reindex | Phase 1 still requires reindex; Phase 2 requires another |

Trade-off: ~39 files touched in one PR. Mitigated by `cgn diff --baseline main --section all` for blast-radius review and a `simplify` skill pass before push.

### Execution order

1. Bump `GRAPH_FORMAT_VERSION: 2 → 3` in `crates/cgn-core/src/graph.rs`
2. Add magic + version pre-check in `auto_ensure::ensure_index`: mismatch is treated as `Stale { age_seconds: 0 }`, so `ensure_fresh` triggers a clean rebuild instead of surfacing `engine::Engine::load`'s `InvalidData` error to the user
3. Remove `ZeroCopyGraph.embeddings` field (schema change happens with the bump)
4. Cascade-remove: `embeddings.rs`, `embedder.rs`, `EmbeddingConfig`, TUI fields, search modes, index flags, auto-ensure preservation, builder integration, tests, stale plan doc
5. Remove `fastembed = "5"` from both `Cargo.toml`s; refresh `Cargo.lock`

## Layer-by-layer changes

### `cgn-core`

- `graph.rs`
  - Remove `pub embeddings: Option<Vec<Vec<f32>>>` (line 209)
  - `GRAPH_FORMAT_VERSION: 2 → 3` (line 14)
  - Strip "embeddings table" mentions from doc comments
  - Update `test_serialize_deserialize_graph` to drop the `embeddings: None` initializer and assert `version == 3`
- `config.rs`
  - Delete `EmbeddingConfig` struct, the `embedding: EmbeddingConfig` field on `Config`, all four `default_embedding_*` fns, the `Default` impl entry, and the three related test assertions (lines 161, 176, etc.)
- `registry/store.rs`
  - Delete `BranchEntry.embedding_status` field and both `"unknown"` / `"none"` initializers (lines 77, 240, 384)
- `cypher/executor.rs`
  - Clean up 7 incidental references (field copies, comments)

### `cgn-analyzer`

- Delete `src/embeddings.rs` (199 lines, entire file)
- `src/lib.rs`: remove `pub mod embeddings;` and re-exports
- `src/resolution/builder.rs`: 53 sites — remove `Embedder` field, embedding cache collection, `with_embeddings(bool)`, change `with_cache(file_hashes, embeddings_cache)` to single-arg `with_cache(file_hashes)`, drop Pass 2 embedding-generation block
- `build.rs`: drop any fastembed-related build logic (if present after re-read)
- `tests/entry_points.rs`: drop residual embedding references
- `Cargo.toml`: drop `fastembed = "5"` and any related features

### `cgn-cli`

- Delete `src/embedder.rs` (22 lines)
- `commands/search.rs`
  - Simplify `SearchMode` enum to `{ Bm25 }` only (single variant — clap still emits "possible values: [bm25]" so users see what's accepted)
  - Remove `Vector`, `Hybrid`, `Auto` variants, `detect_mode`, `embeddings_available_for`, `vector_hits`, `hybrid_hits`, `cosine_top_k_indices`
  - Remove `ScoreSource::Cosine` and `ScoreSource::Rrf`
  - Strip the stdin-batch-dispatch comment about embedder cold-start amortization (lines 110–112)
  - Rewrite the module doc-comment (lines 1–14) to reflect BM25-only routing
  - `--mode bm25` becomes a no-op alias; `--mode vector|hybrid|auto` fails at clap validation
- `commands/admin/index.rs`
  - Delete `IndexArgs.embeddings` and `IndexArgs.drop_embeddings` fields
  - Drop `embeddings_flag`, embedding cache branches in step 3b, `with_embeddings(...)` call, `embedding_status` write
- `commands/admin/config.rs`
  - Delete 4 `FieldId::Embedding*` variants, their entries in `FIELD_ORDER`, the 4 match arms in `field_value` / `field_edit_value` / `apply_edit`
  - Delete the `group_header("Embedding")` + 4 `field_line` calls in render (lines 436–440)
- `admin/indexes.rs`
  - Remove the `Confirm` prompt for "Build embeddings", `IndexArgs.embeddings` / `drop_embeddings` assignments, the status-print fields
- `admin/diagnostics.rs`
  - Drop the registry-backfill `embedding_status` initializer (line 252)
- `auto_ensure.rs`
  - Delete `embeddings_present` fn (lines 44–61) entirely
  - In `ensure_fresh`, drop `keep_embeddings`, `IndexArgs.embeddings`, `IndexArgs.drop_embeddings` (lines 76–89)
  - Add magic + version pre-check in `ensure_index`: between the metadata check and `any_source_newer_than`, read the first 8 bytes of `graph.bin` (4-byte magic + 4-byte little-endian version); if magic mismatches or version != `GRAPH_FORMAT_VERSION`, return `Stale { age_seconds: 0 }`. Failure to read 8 bytes (truncated file) is also Stale.
- `commands/coverage.rs`: drop the one `embeddings: None` initializer (line 339)
- `commands/hook/post_tool_use.rs`: clean 4 incidental references
- `lib.rs` / `main.rs`: remove module exports

### Tests

- Delete `tests/auto_ensure_embeddings.rs` and `tests/search_vector_fallback.rs` (entire files)
- For incidental-reference tests, remove `embeddings: None` / `embedding_status: "none"` from fixtures and drop now-irrelevant assertions:
  - `tests/search_cmd.rs`, `tests/config_cmd.rs`, `tests/score_source_tagging.rs`
  - `tests/hook_pre_tool_use_test.rs`, `tests/compute_hits_tantivy.rs`, `tests/engine_header.rs`
  - `tests/search_multi_repo.rs`, `tests/tantivy_build.rs`, `tests/repo_selector.rs`
  - `crates/cgn-core/tests/{registry_store,registry_lifecycle,mmap_test}.rs`
- **Add** `tests/version_mismatch_triggers_reindex.rs`: write a v2 fixture (8 bytes: magic + `2u32.to_le_bytes()` + padding) to a temp path, assert `auto_ensure::ensure_index` returns `Stale`

### Docs

- Delete `docs/plans/2026-05-16-vector-hybrid-search.md`
- `docs/skills/cgn.md` line 45: remove the `--embeddings` example
- `crates/cgn-mcp/`: grep for search-mode docs and reconcile (likely a short README mention only)

## Error handling

| Scenario | Behavior |
|---|---|
| `cgn search --mode vector\|hybrid\|auto` | clap `value_enum` fails: `error: invalid value '<x>' for '--mode <MODE>' (possible values: [bm25])` |
| `cgn search --mode bm25` | accepted, identical to omitting the flag |
| `cgn admin index --embeddings` / `--drop-embeddings` | clap fails: unknown flag |
| Old `graph.bin` (v2, contains `embeddings` field) | `ensure_index` pre-check detects version mismatch → returns `Stale {age_seconds: 0}` → `ensure_fresh` invokes `admin index` synchronously → one-line stderr notice → transparent to user |
| User's existing `~/.cgn/config.toml` contains `[embedding]` | serde defaults to ignoring unknown fields → no user-visible impact |
| Cypher query touches a removed embedding property | property does not exist in schema → executor returns null, consistent with any other missing property |

## Testing strategy

Tests shipped in the same PR:

1. **Version migration** — `tests/version_mismatch_triggers_reindex.rs`: v2 fixture must produce `Stale`, never panic, never `Ready`
2. **Search hard error** — `tests/search_cmd.rs` adds cases for `--mode vector` / `--mode hybrid` / `--mode auto` returning non-zero exit + `invalid value` substring
3. **Search `--mode bm25` no-op** — `tests/search_cmd.rs` asserts identical output with and without the flag
4. **Admin index flag removal** — assert non-zero exit on `--embeddings` and `--drop-embeddings`
5. **Config TUI** — `tests/config_cmd.rs` asserts rendered output does not contain "Embedding" group header
6. **Registry round-trip** — `crates/cgn-core/tests/registry_store.rs`, `registry_lifecycle.rs` confirm `BranchEntry` serialises without `embedding_status`
7. **rkyv schema** — `graph.rs::test_serialize_deserialize_graph` asserts `archived.version.to_native() == 3` and no embeddings field
8. **Builder smoke** — `cargo test -p cgn-analyzer` green: parsers / framework detection / resolution must work with no `Embedder`
9. **14-language parity** — `crates/cgn-analyzer/tests/<lang>_*.rs` all green (no parser-behavior change expected, but required by `CLAUDE.md`)

## Performance verification

Run `python scripts/benchmark_cgn.py` against `main` baseline. Expected:

- Cold-index time on `.sample_repo` drops noticeably (eliminate fastembed cold-start ~1–2 s plus per-node embedding generation)
- Per-query latency unchanged (BM25 path untouched)
- Release binary size shrinks substantially (ONNX runtime is heavy)

Capture the diff in the PR description; the comparison is part of the verification, not optional.

## Risk register

| Risk | Mitigation |
|---|---|
| Builder cleanup misses a path in the 53-site cascade, breaking the cache pipeline silently | Run `cargo test -p cgn-analyzer` after each removal pass; final acceptance is a clean `cgn admin index --force` on `.sample_repo` producing a v3 `graph.bin` with no embedding traces |
| TUI keyboard nav indices drift when Embedding group is removed | Existing `config_cmd.rs` coverage + manual walk through `cgn admin config` |
| Hidden MCP / hook callers reference vector mode | Pre-flight grep `rg -n "Vector\|Hybrid\|Auto" crates/cgn-mcp crates/cgn-cli/src/commands/hook`; reconcile before tagging the PR |
| Unrelated test fixtures contain stale `embeddings: None` and need touching, surfacing scope creep | Acceptable: any fixture initializer that mentions the removed field traces directly to the removal, not drive-by cleanup |

## Out of scope

- Re-introducing semantic search via a different backend (OpenAI embeddings, etc.) — separate design if ever revived
- Migration tool for old graphs — auto-ensure rebuild covers it; no need for a one-shot migrator
- Renaming `ScoreSource::Bm25` or `Substring` to anything else — leave as-is
- Touching the `embed-profile` worktree (`feat/embedding-profile-freeze`) — independent stream, not blocked by this work
