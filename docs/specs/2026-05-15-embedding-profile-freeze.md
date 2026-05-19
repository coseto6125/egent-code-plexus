# Spec — Embedding Profile Freeze + First-Time Wizard

**Date**: 2026-05-15
**Status**: Draft (handoff from OOM-fix conversation; implementation pending)
**Related commits**:
- `3c1123a fix(analyzer): chunk fastembed batch (default 32) to bound peak embedding memory` — landed prerequisite
- `175c260 feat(cli): cgn config — interactive TUI wizard for repo-local config` — wizard infra
- Existing `crates/cgn-core/src/config.rs` (EmbeddingConfig: stored-only)
- Existing `crates/cgn-core/src/registry/meta.rs` (BranchMeta v1)

---

## 1. Motivation

`cgn admin index --embeddings` currently hard-wires `BAAI/bge-m3` (1024-dim, ~540 MB INT8). Three problems:

1. **Memory pressure on smaller hosts**: peak 3.1 GB resident on 22k-symbol corpora. WSL2 / 8 GB laptops sit dangerously close to OOM (mitigated by `3c1123a` batch chunking, but model size is still the floor).
2. **No way to opt out / down-shift**: users on constrained hardware have no way to pick a smaller model or skip embeddings entirely without editing source.
3. **No cross-machine identity**: `graph.bin` carries vector data but `meta.json` doesn't record which model produced it. Loading a graph indexed on machine A and querying it on machine B silently uses whatever model machine B happens to load (today: always bge-m3 — but the moment we ship a second model, this is a footgun).

The fix is to make the **embedding model + batch a first-class profile**, frozen per-graph at first analyze and validated on every subsequent load.

---

## 2. Goals & non-goals

### Goals

- G1. User can choose embedding model on first `admin index --embeddings` via the existing `cgn admin config` wizard. RAM is probed to suggest a sensible default.
- G2. Choice is **frozen** into `meta.json` (schema v2). Subsequent `admin index` / query operations on the same graph use the frozen profile; ignoring `config.toml` if the two disagree (config edits don't silently invalidate a graph).
- G3. Cross-machine load: if the frozen model isn't available on the loading host, fail-loud with a clear remediation hint (re-analyze with `--drop-embeddings`).
- G4. Multi-model loader: at minimum `bge-m3-int8` (current) + `off` (no embeddings). `multilingual-e5-small` (118 MB, 384 dim) optional but recommended for memory-constrained users.
- G5. Non-TTY environments (CI, Docker build, pipes) bypass the wizard and use the RAM-tier auto-default deterministically.
- G6. v1 graphs (no `embedding_profile` in meta) migrate cleanly: assume `bge-m3-int8` with default batch when loaded under v2 code.

### Non-goals

- N1. Remote embedding API (`EmbeddingConfig.endpoint` / `api_key`) — already stored, still not wired. Out of scope; separate spec.
- N2. Model fine-tuning / custom ONNX upload — registry is hard-coded.
- N3. Multiple embedding models in the same graph (e.g. one for code, one for docs). Single model per graph.

---

## 3. Schema changes

### 3.1 `crates/cgn-core/src/registry/meta.rs`

Bump `schema_version` from `1` → `2`. Add optional `embedding_profile` (None for v1 graphs, Some(...) for v2 written-by-this-code graphs).

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchMeta {
    pub indexed_at: String,
    pub node_count: u32,
    #[serde(default)]
    pub delta_size: u64,
    #[serde(default)]
    pub last_compact_at: Option<String>,
    pub worktree_path: String,
    pub remote_url: String,
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// **NEW v2** — frozen at first `analyze --embeddings`; absent on v1
    /// graphs and on graphs analyzed without `--embeddings`.
    #[serde(default)]
    pub embedding_profile: Option<EmbeddingProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingProfile {
    /// Stable slug, NOT the HuggingFace repo path. See §4.
    pub model_id: String,
    pub dim: u32,
    pub batch_size: u32,
    /// What the host reported when this profile was frozen. Diagnostic
    /// only — NOT consulted for validation; the model_id is the contract.
    #[serde(default)]
    pub detected_ram_gb: Option<u32>,
    /// ISO-8601 of the analyze run that froze this profile.
    pub frozen_at: String,
}

fn default_schema_version() -> u32 { 2 }   // bumped from 1
```

### 3.2 Migration

- v1 meta loaded under v2 code → `embedding_profile = None`. If the on-disk graph has any vectors (detect by probing graph.bin for non-zero embedding count), treat as `Some(EmbeddingProfile { model_id: "bge-m3-int8", dim: 1024, batch_size: 32, detected_ram_gb: None, frozen_at: indexed_at.clone() })` on first read and rewrite the meta. Pure inference; no user prompt.

---

## 4. Multi-model registry

### 4.1 `crates/cgn-analyzer/src/embeddings.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbedModelId {
    /// Default. BAAI/bge-m3 INT8 — 1024 dim, multilingual (100+ langs),
    /// ~540 MB resident. Recommended for ≥12 GB hosts.
    BgeM3Int8,
    /// Multilingual fallback for constrained hosts. Qdrant/multilingual-e5-
    /// small-onnx — 384 dim, multilingual (100+ langs), ~118 MB resident.
    /// Recommended for 6–12 GB hosts. **MUST verify ONNX file naming
    /// before shipping** — Qdrant repo layout differs from MahradHosseini.
    E5SmallMultilingual,
    /// No embedding. analyze --embeddings becomes a no-op for vectors;
    /// graph.bin contains zero-vector placeholders; query falls back to
    /// BM25 lexical only. For ≤6 GB hosts or batch jobs that don't need
    /// semantic search.
    Off,
}

impl EmbedModelId {
    pub fn slug(self) -> &'static str { ... }       // "bge-m3-int8" / "e5-small-multilingual" / "off"
    pub fn parse(s: &str) -> Option<Self> { ... }   // accepts aliases: "bge-m3", "e5-small", "none", ...
    pub fn dim(self) -> usize { 1024 / 384 / 0 }
    pub fn approx_resident_mb(self) -> u64 { 540 / 118 / 0 }
    pub fn display_name(self) -> &'static str { ... }
    pub fn is_multilingual(self) -> bool { ... }    // both BgeM3 & E5Small true; Off n/a
    fn hf_model_repo(self) -> &'static str { ... }
    fn hf_model_file(self) -> &'static str { ... }
    fn hf_tokenizer_repo(self) -> &'static str { ... }
}

impl Default for EmbedModelId {
    fn default() -> Self { Self::BgeM3Int8 }
}
```

### 4.2 Embedder factory

Change `Embedder::new()` → `Embedder::new(model_id: EmbedModelId, batch: usize) -> Result<Option<Self>>` (returns `None` for `Off`). All call sites need adjustment.

Implementation per variant:
- `BgeM3Int8`: existing path (already in `embeddings.rs`)
- `E5SmallMultilingual`: same `try_new_from_user_defined` shape, different HF repo + file names. **Pre-flight check**: verify Qdrant's `multilingual-e5-small-onnx` ships `model.onnx` + `tokenizer.json` in repo root. If not, swap to `intfloat/multilingual-e5-small` (PyTorch weights only — would need separate ONNX conversion step or vendoring).
- `Off`: no model, no HF download, no tokenizer.

---

## 5. CLI surface

### 5.1 `crates/cgn-cli/src/commands/analyze.rs`

New flags:
- `--embed-model <MODEL>` — explicit override. Validated against registry. **Forces wizard skip** (non-interactive intent).
- `--embed-batch <N>` — explicit override. Validated `>0`.
- `--yes` / `-y` — skip wizard, use RAM-tier auto-default.

Resolution order at analyze start:
1. If `--embed-model` provided → use it. If `--embed-batch` provided → use it. Skip wizard.
2. Else load `.cgn/config.toml`:
   - If `embedding.model` is set AND `meta.json` already has `embedding_profile` → use frozen profile (don't re-prompt).
   - If `embedding.model` is set AND no meta yet → freeze it.
   - If `embedding.model` absent (fresh config) AND TTY → launch wizard with RAM-probed defaults pre-filled.
   - If `embedding.model` absent AND non-TTY → use RAM-tier auto-default, write meta, log a one-liner.

### 5.2 `crates/cgn-cli/src/commands/config.rs` (extend existing wizard)

Existing wizard has 6 fields. Two need to swap behavior:

- `embedding.model` field becomes a **select picker** (was free-text):
  - `[*] bge-m3-int8 (1024 dim, multilingual, ~540 MB) — recommended`
  - `[ ] multilingual-e5-small (384 dim, multilingual, ~118 MB)`
  - `[ ] off (BM25 lexical only)`
  - Recommendation marker (`*`) shifts based on RAM probe (§6).
- `embedding.batch_size` field stays free-text but **default** is RAM-tier value, not hard-coded 32.

Add a "Detected RAM: X GB → recommend Y" hint line above the embedding section.

### 5.3 Query-path commands (`context`, `query`, `cypher`, `impact`, `route-map`, ...)

On load, read `meta.json` → `embedding_profile`. If profile exists and the loading host has the model available (i.e., `EmbedModelId::parse(slug)` returns `Some`), use it for query encoding. If not (e.g., new code drops a model), fail-loud:

```
error: graph.bin was indexed with embedding model "experimental-foo" which
       isn't compiled into this cgn build. Re-analyze with --drop-embeddings
       and pick a supported model, or upgrade cgn.
```

If `embedding_profile.model_id != "off"` but the query command doesn't actually need embeddings (e.g., pure `MATCH (a)-[r]->(b) RETURN a,b` cypher), the load can proceed without model init — lazy load.

---

## 6. RAM probe & auto-default

### 6.1 Detection

Linux/WSL2 only. Read `/proc/meminfo` → `MemTotal:` line → kB → GiB. macOS/Windows: skip probe, fall back to `BgeM3Int8` + batch=32 (matches today's hard-coded default; users on those platforms can override via wizard or flag).

```rust
fn detect_total_ram_gb() -> Option<u32> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = content.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some((kb / 1024 / 1024) as u32)
}
```

### 6.2 Tier table

| RAM tier | Model | Batch | Peak RSS estimate | Headroom on min RAM |
|---|---|---|---|---|
| ≤6 GB | `Off` | n/a | 0 | n/a (no embeddings) |
| 7–10 GB | `E5SmallMultilingual` | 16 | ~1.0 GB | 90% |
| 11–14 GB | `E5SmallMultilingual` | 32 OR `BgeM3Int8` + 16 | ~1.4 GB / ~2.0 GB | 80% |
| 15–24 GB | `BgeM3Int8` | 32 | ~3.1 GB (measured) | 80% |
| 25+ GB | `BgeM3Int8` | 64 | ~5.5 GB | 80%+ |

11–14 GB tier (your WSL2 case) is the ambiguous middle. Default to `BgeM3Int8` + 16 because the multilingual + 1024 dim quality difference matters more than the +500 MB peak. Wizard makes both options visible.

### 6.3 macOS/Windows

Without `/proc/meminfo`, `detect_total_ram_gb()` returns `None`. Wizard default falls to `BgeM3Int8` + 32 (today's behavior). Users on those platforms see the same UI but no auto-recommendation marker — they pick explicitly or accept the default.

(Adding cross-platform RAM probe via `sysinfo` crate is a separate decision — current direction is to defer it because Linux/WSL2 covers ~90% of dev users.)

---

## 7. Cross-machine load validation

When any read command loads `graph.bin`:

```rust
let meta = BranchMeta::read(&meta_path)?;
match &meta.embedding_profile {
    None => {
        // v1 graph or analyze-without-embeddings graph; nothing to validate.
        // If graph has embeddings (probe), apply migration §3.2 inline.
    }
    Some(profile) => {
        let model_id = EmbedModelId::parse(&profile.model_id)
            .ok_or_else(|| format!(
                "graph indexed with unknown embedding model '{}'. \
                 Available: {}. Re-analyze with --drop-embeddings.",
                profile.model_id,
                EmbedModelId::known_slugs().join(", ")
            ))?;
        if profile.dim != model_id.dim() as u32 {
            // Should never happen; consistency check.
            return Err(format!("meta dim {} != model {} dim {}",
                profile.dim, profile.model_id, model_id.dim()));
        }
        // Lazy: defer model load until a query path actually needs it.
    }
}
```

---

## 8. Implementation order (suggested commit sequence)

| # | Commit | Crate(s) | LOC | Risk |
|---|---|---|---|---|
| 1 | `feat(core): add EmbeddingProfile to BranchMeta (schema v2) + migration` | core | ~60 | low (additive) |
| 2 | `feat(analyzer): introduce EmbedModelId enum + factory; keep bge-m3-int8 as only impl` | analyzer | ~80 | low (no behavior change yet) |
| 3 | `feat(analyzer): add multilingual-e5-small loader behind EmbedModelId::E5SmallMultilingual` | analyzer | ~50 | medium (new HF repo, verify file layout) |
| 4 | `feat(core): RAM-tier auto-default for embedding model + batch (Linux/WSL2)` | core | ~50 | low |
| 5 | `feat(cli): wire EmbeddingConfig → analyze; freeze profile into meta on first run` | cli | ~80 | medium |
| 6 | `feat(cli): wizard select picker for embedding model; show RAM recommendation` | cli | ~100 | medium (ratatui surface change) |
| 7 | `feat(cli): cross-machine validation on graph load; fail-loud on unknown model` | cli | ~40 | low |
| 8 | `feat(cli): --embed-model / --embed-batch / --yes flags on analyze` | cli | ~40 | low |
| 9 | `test: multi-model round-trip + migration + validation paths` | all | ~150 | low |

Total ~650 LOC across 9 commits. Each commit independently reviewable and revert-able.

---

## 9. Test plan

### Unit
- `EmbedModelId::parse` / `slug` round-trip for all variants + aliases
- `BranchMeta` v1 ↔ v2 JSON round-trip (older `cgn` reads v2 meta by dropping unknown field; newer reads v1 by defaulting `embedding_profile = None`)
- RAM probe: mock `/proc/meminfo` with various MemTotal values, verify tier mapping
- Migration §3.2: v1 meta + non-empty embeddings → rewrites meta with `BgeM3Int8` profile

### Integration
- `admin index --embeddings --embed-model off` → graph has zero-vector placeholders, query falls back to BM25
- `admin index --embeddings` (TTY) → wizard shown, selection persisted to both config.toml and meta.json
- `admin index --embeddings` (non-TTY, no config.toml) → uses RAM-tier auto-default, writes meta, exits with status info
- Two-machine simulation: indexing on host with `BgeM3Int8`, loading on host where only `E5SmallMultilingual` is registered → fail-loud with hint
- `cgn admin config` edit model after index → meta still wins; user gets a one-liner warning that the change applies only to next `--drop-embeddings` re-index

### Manual
- Wizard UX on actual TTY: select picker keyboard nav, RAM recommendation visible, `^S` persists
- 8 GB WSL2 / 16 GB MBA / 32 GB workstation each see appropriate auto-default

---

## 10. Open questions (decide during implementation)

- Q1. `multilingual-e5-small` ONNX repo selection: Qdrant's vs another community port. Test download + load before committing.
- Q2. Wizard select picker: ratatui has no built-in select widget. Either roll a 5-line one (radio list with arrow keys) or pull `dialoguer` (already not a dep — might cost a fresh dep tree). Likely roll-your-own to keep dep count down.
- Q3. Where to surface RAM tier table in user-facing docs (`README.md`? `docs/embeddings.md` new file?). Spec defers to implementer.
- Q4. `--drop-embeddings` already exists in analyze; verify it also wipes `embedding_profile` from meta. Add test.

---

## 11. Backwards compatibility

- v1 `meta.json` (no `embedding_profile`): loaded fine via `#[serde(default)]`, migration §3.2 applies on first read under v2 code.
- v1 `config.toml` (no embedding fields or partial): loaded fine via existing per-field defaults.
- Older `cgn` binary reading a v2 `meta.json`: unknown `embedding_profile` field tolerated (`#[serde(default)]` in old struct — verify by inspecting `meta.rs` of the older release; if old version uses `deny_unknown_fields`, need a transition release first).
- Older `cgn` binary loading a graph with new `model_id = "e5-small-multilingual"` embeddings: today's code is hard-wired to bge-m3, so it'll try to encode queries with bge-m3 — silent wrong-model encoding. **Risk**: anyone on stale `cgn` querying a fresh-indexed graph gets garbage results. Mitigation: bump `BranchMeta::schema_version` to 2; old code that reads `schema_version > 1` should refuse to load (verify the old code does this; if not, an intermediate "version-aware refusal" release is needed).

---

## 12. Out of scope for this spec (intentionally deferred)

- Remote embedding API wiring (`EmbeddingConfig.endpoint` / `api_key`) — already stored, separate spec when needed
- Custom ONNX upload
- Per-language embedding model (e.g., code-specific model for Rust source, prose model for markdown)
- Embedding model performance comparison / benchmark suite (the `benchmark_cgn.py` script in worktree-vendor-dlbuild can be extended in a follow-up)
- GPU inference path

---

## Appendix A — Why not just keep one model and call it good?

- Memory: bge-m3 INT8 is 540 MB resident + activation. On 8 GB hosts it's a no-go even with batch chunking.
- Choice signaling: README's positioning is "code intelligence for AI agents". Telling users "you need 16 GB or stay BM25-only" is anti-positioning. Offering e5-small as a multilingual fallback keeps the cross-lingual story alive at half the model size.
- Future-proofing: when bge-m4 or similar lands, having `EmbedModelId` registry means adding a variant + HF metadata, not refactoring the whole pipeline.

## Appendix B — Why freeze in meta vs always read config?

- Two analyze runs with different config.toml between them today silently invalidate any cached embeddings (because hashes don't match across model changes). Loud-fail on mismatch beats silent corruption.
- Cross-machine sync: someone shares `.cgn/<repo>/<branch>/` via tarball. Receiver needs to know which model to encode queries with. config.toml on the receiver's side doesn't have that info; meta.json travels with the graph.
- Trust: user runs the wizard once, picks a profile, expects it to stick. config.toml is a tunables surface (batch size adjustment, etc.); meta.json is the contract.
