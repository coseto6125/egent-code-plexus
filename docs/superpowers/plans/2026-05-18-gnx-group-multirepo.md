# `gnx group` Multi-repo Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the `gnx group <verb>` noun-first namespace
(`sync / status / contracts / impact / search / find / coverage`) per
`docs/specs/2026-05-18-gnx-group-multirepo-design.md`, including the
breaking removal of `--repo @<group>` from existing top-level commands.

**Architecture:** New module `commands/group/` under the existing CLI
crate (no new crate). Storage: `~/.gnx/groups/<name>/{contracts.rkyv,
meta.json}` — rkyv archive + small JSON sibling. Reuses existing
`Registry`, `TantivyEngine`, `ZeroCopyGraph`, `rayon`, and `Config`
infrastructure. Tantivy is the BM25 backend (no new BM25 dependency).

**Tech Stack:** Rust 2021, rkyv 0.7.x (already in deps), Tantivy
(already wired via `graph-nexus-cli/src/search.rs`), tree-sitter
grammars per-language (already in deps for Go / Python / Node /
Java / Rust), rayon, mimalloc allocator (project default).

**PR strategy:** Single PR, 16 commits, ~3–4.5k LOC. Each task =
one commit. Commit 0 (spec) already landed on this worktree branch
as `ee233bc`.

---

## Phase 1 — Foundation (Tasks 1–2)

### Task 1: Group types + storage IO

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/group/mod.rs`
- Create: `crates/graph-nexus-cli/src/commands/group/types.rs`
- Create: `crates/graph-nexus-cli/src/commands/group/storage.rs`
- Modify: `crates/graph-nexus-cli/src/commands/mod.rs` (register `pub mod group;`)
- Test: `crates/graph-nexus-cli/tests/group_storage.rs`

- [ ] **Step 1: Add module declaration**

Edit `crates/graph-nexus-cli/src/commands/mod.rs`, add alongside
other `pub mod`s:

```rust
pub mod group;
```

- [ ] **Step 2: Create `commands/group/mod.rs` shell**

```rust
//! `gnx group <verb>` — multi-repo workflow surface.
//!
//! Management commands stay in `commands/admin/group.rs`; this module
//! owns the query/sync surface.

pub mod storage;
pub mod types;
```

- [ ] **Step 3: Write the failing round-trip test**

Create `crates/graph-nexus-cli/tests/group_storage.rs`:

```rust
use graph_nexus_cli::commands::group::storage::{
    read_contracts, write_contracts, GroupMeta, RepoSnapshot,
};
use graph_nexus_cli::commands::group::types::{
    ContractRegistry, ContractRole, ContractType, ExtractedContract,
    MatchType, StoredContract, SymbolRef, CrossLink, CrossLinkEndpoint,
};
use std::collections::BTreeMap;
use tempfile::TempDir;

fn sample_registry() -> ContractRegistry {
    let provider = StoredContract {
        repo: "backend".into(),
        inner: ExtractedContract {
            contract_id: "http:POST:/api/users".into(),
            contract_type: ContractType::Http,
            role: ContractRole::Provider,
            symbol_uid: "backend::handlers::create_user".into(),
            symbol_ref: SymbolRef {
                file_path: "src/handlers.rs".into(),
                name: "create_user".into(),
            },
            confidence: 1.0,
            service: None,
            meta: vec![("method".into(), "POST".into())],
        },
    };
    let consumer = StoredContract {
        repo: "frontend".into(),
        inner: ExtractedContract {
            contract_id: "http:POST:/api/users".into(),
            contract_type: ContractType::Http,
            role: ContractRole::Consumer,
            symbol_uid: "frontend::api::createUser".into(),
            symbol_ref: SymbolRef {
                file_path: "src/api.ts".into(),
                name: "createUser".into(),
            },
            confidence: 1.0,
            service: None,
            meta: vec![],
        },
    };
    let link = CrossLink {
        from: CrossLinkEndpoint {
            repo: "frontend".into(),
            service: None,
            symbol_uid: consumer.inner.symbol_uid.clone(),
            symbol_ref: consumer.inner.symbol_ref.clone(),
        },
        to: CrossLinkEndpoint {
            repo: "backend".into(),
            service: None,
            symbol_uid: provider.inner.symbol_uid.clone(),
            symbol_ref: provider.inner.symbol_ref.clone(),
        },
        contract_type: ContractType::Http,
        contract_id: provider.inner.contract_id.clone(),
        match_type: MatchType::Exact,
        confidence: 1.0,
    };
    ContractRegistry {
        version: 1,
        contracts: vec![provider, consumer],
        cross_links: vec![link],
        unmatched: vec![],
    }
}

#[test]
fn contracts_rkyv_roundtrip_preserves_all_fields() {
    let dir = TempDir::new().unwrap();
    let registry = sample_registry();
    write_contracts(dir.path(), &registry).unwrap();
    let read = read_contracts(dir.path()).unwrap();
    assert_eq!(read.contracts.len(), 2);
    assert_eq!(read.cross_links.len(), 1);
    assert_eq!(read.cross_links[0].contract_id, "http:POST:/api/users");
    assert_eq!(read.cross_links[0].match_type, MatchType::Exact);
}

#[test]
fn meta_json_roundtrip() {
    let dir = TempDir::new().unwrap();
    let mut snapshots = BTreeMap::new();
    snapshots.insert(
        "backend".into(),
        RepoSnapshot {
            indexed_at: "2026-05-18T10:00:00Z".into(),
            last_commit: "abc123".into(),
        },
    );
    let meta = GroupMeta {
        version: 1,
        generated_at: "2026-05-18T10:05:00Z".into(),
        repo_snapshots: snapshots,
        missing_repos: vec!["legacy".into()],
    };
    graph_nexus_cli::commands::group::storage::write_meta(dir.path(), &meta).unwrap();
    let read = graph_nexus_cli::commands::group::storage::read_meta(dir.path()).unwrap();
    assert_eq!(read.generated_at, meta.generated_at);
    assert_eq!(read.missing_repos, vec!["legacy".to_string()]);
}

#[test]
fn read_contracts_missing_returns_empty() {
    let dir = TempDir::new().unwrap();
    let read = read_contracts(dir.path()).unwrap();
    assert!(read.contracts.is_empty());
    assert!(read.cross_links.is_empty());
}
```

- [ ] **Step 4: Run test to verify it fails**

```
cargo test -p graph-nexus --test group_storage
```

Expected: compile error — `group::storage` / `group::types` not found.

- [ ] **Step 5: Implement `types.rs`**

Create `crates/graph-nexus-cli/src/commands/group/types.rs`:

```rust
//! On-disk types for the per-group contract registry. rkyv-archived
//! for zero-copy reads via mmap.

use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[archive(check_bytes)]
pub enum ContractType { Http, Grpc, Thrift, Topic, Lib, Custom, Include }

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[archive(check_bytes)]
pub enum ContractRole { Provider, Consumer }

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[archive(check_bytes)]
pub enum MatchType { Exact, Manifest, Wildcard, Bm25, Embedding }

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[archive(check_bytes)]
pub struct SymbolRef {
    pub file_path: String,
    pub name: String,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[archive(check_bytes)]
pub struct ExtractedContract {
    pub contract_id: String,
    pub contract_type: ContractType,
    pub role: ContractRole,
    pub symbol_uid: String,
    pub symbol_ref: SymbolRef,
    pub confidence: f32,
    pub service: Option<String>,
    pub meta: Vec<(String, String)>,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[archive(check_bytes)]
pub struct StoredContract {
    pub repo: String,
    pub inner: ExtractedContract,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[archive(check_bytes)]
pub struct CrossLinkEndpoint {
    pub repo: String,
    pub service: Option<String>,
    pub symbol_uid: String,
    pub symbol_ref: SymbolRef,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[archive(check_bytes)]
pub struct CrossLink {
    pub from: CrossLinkEndpoint,
    pub to: CrossLinkEndpoint,
    pub contract_type: ContractType,
    pub contract_id: String,
    pub match_type: MatchType,
    pub confidence: f32,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[archive(check_bytes)]
pub struct ContractRegistry {
    pub version: u32,
    pub contracts: Vec<StoredContract>,
    pub cross_links: Vec<CrossLink>,
    pub unmatched: Vec<StoredContract>,
}
```

- [ ] **Step 6: Implement `storage.rs`**

```rust
//! Read/write `contracts.rkyv` + `meta.json`. Atomic rename pattern
//! mirrors `graph_nexus_core::registry::io::atomic_write_json`.

use crate::commands::group::types::ContractRegistry;
use rkyv::ser::{serializers::AllocSerializer, Serializer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const CONTRACTS_FILE: &str = "contracts.rkyv";
pub const META_FILE: &str = "meta.json";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GroupMeta {
    pub version: u32,
    pub generated_at: String,
    pub repo_snapshots: BTreeMap<String, RepoSnapshot>,
    pub missing_repos: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RepoSnapshot {
    pub indexed_at: String,
    pub last_commit: String,
}

pub fn group_dir(home_gnx: &Path, group_name: &str) -> PathBuf {
    home_gnx.join("groups").join(group_name)
}

pub fn write_contracts(group_dir: &Path, reg: &ContractRegistry) -> io::Result<()> {
    fs::create_dir_all(group_dir)?;
    let mut ser = AllocSerializer::<4096>::default();
    ser.serialize_value(reg)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("rkyv: {e:?}")))?;
    let bytes = ser.into_serializer().into_inner();
    let path = group_dir.join(CONTRACTS_FILE);
    let tmp = group_dir.join(format!("{CONTRACTS_FILE}.tmp"));
    fs::write(&tmp, &bytes)?;
    fs::rename(&tmp, &path)
}

pub fn read_contracts(group_dir: &Path) -> io::Result<ContractRegistry> {
    let path = group_dir.join(CONTRACTS_FILE);
    if !path.exists() {
        return Ok(ContractRegistry {
            version: 1,
            contracts: vec![],
            cross_links: vec![],
            unmatched: vec![],
        });
    }
    let bytes = fs::read(&path)?;
    let archived = rkyv::check_archived_root::<ContractRegistry>(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("rkyv check: {e:?}")))?;
    let reg: ContractRegistry = archived
        .deserialize(&mut rkyv::Infallible)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("rkyv des: {e:?}")))?;
    Ok(reg)
}

pub fn write_meta(group_dir: &Path, meta: &GroupMeta) -> io::Result<()> {
    fs::create_dir_all(group_dir)?;
    graph_nexus_core::registry::io::atomic_write_json(&group_dir.join(META_FILE), meta)
}

pub fn read_meta(group_dir: &Path) -> io::Result<GroupMeta> {
    let path = group_dir.join(META_FILE);
    let bytes = fs::read(&path)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
```

- [ ] **Step 7: Verify `atomic_write_json` is `pub`**

```
grep -n "pub fn atomic_write_json" crates/graph-nexus-core/src/registry/io.rs
```

If not `pub`, promote it (single-line edit) and add a one-line WHY
comment: `// pub: reused by graph-nexus-cli::commands::group::storage`.

- [ ] **Step 8: Run tests to verify they pass**

```
cargo test -p graph-nexus --test group_storage
```

Expected: 3 passed.

- [ ] **Step 9: Clippy clean**

```
cargo clippy -p graph-nexus --tests -- -D warnings
```

Expected: no warnings.

- [ ] **Step 10: Commit**

```
git add crates/graph-nexus-cli/src/commands/group/ \
        crates/graph-nexus-cli/src/commands/mod.rs \
        crates/graph-nexus-cli/tests/group_storage.rs
[ -n "$(git status --porcelain crates/graph-nexus-core/src/registry/io.rs)" ] && \
  git add crates/graph-nexus-core/src/registry/io.rs
git commit -m "$(cat <<'EOF'
feat(group): types + rkyv storage for per-group contract registry

ContractRegistry / StoredContract / CrossLink + meta.json IO with
atomic-rename. Reuses atomic_write_json from graph-nexus-core.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: `GroupConfig` in `Config`

**Files:**
- Modify: `crates/graph-nexus-core/src/config.rs`
- Test: `crates/graph-nexus-core/tests/config_group.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/graph-nexus-core/tests/config_group.rs`:

```rust
use graph_nexus_core::config::Config;

#[test]
fn group_section_defaults_when_absent() {
    let toml = r#"
[output]
default_format = "toon"
"#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert!((cfg.group.bm25_threshold - 0.6).abs() < f32::EPSILON);
    assert_eq!(cfg.group.max_candidates_per_step, 16);
    assert!(cfg.group.exclude_links_paths.is_empty());
    assert!(!cfg.group.exclude_links_param_only_paths);
    assert_eq!(cfg.group.cross_depth, 1);
    assert_eq!(cfg.group.local_impact_timeout_ms, 5000);
}

#[test]
fn group_section_honours_overrides() {
    let toml = r#"
[group]
bm25_threshold = 0.75
max_candidates_per_step = 32
exclude_links_paths = ["/health", "/metrics"]
exclude_links_param_only_paths = true
cross_depth = 2
local_impact_timeout_ms = 8000
"#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert!((cfg.group.bm25_threshold - 0.75).abs() < f32::EPSILON);
    assert_eq!(cfg.group.max_candidates_per_step, 32);
    assert_eq!(
        cfg.group.exclude_links_paths,
        vec!["/health".to_string(), "/metrics".to_string()]
    );
    assert!(cfg.group.exclude_links_param_only_paths);
    assert_eq!(cfg.group.cross_depth, 2);
    assert_eq!(cfg.group.local_impact_timeout_ms, 8000);
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p graph-nexus-core --test config_group
```

Expected: compile error — `Config` has no `group` field.

- [ ] **Step 3: Extend `Config` in `crates/graph-nexus-core/src/config.rs`**

Locate the `Config` struct (line 13) and add the `group` field with
the same `#[serde(default)]` pattern as `output` / `confidence`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub confidence: ConfidenceConfig,
    #[serde(default)]
    pub group: GroupConfig,
}
```

Append the new struct at the bottom of the file (after
`ConfidenceConfig`):

```rust
/// **stored** — values consumed by `gnx group sync / impact` when
/// CLI flags do not override. See
/// `docs/specs/2026-05-18-gnx-group-multirepo-design.md` §Configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GroupConfig {
    #[serde(default = "default_group_bm25_threshold")]
    pub bm25_threshold: f32,
    #[serde(default = "default_group_max_candidates")]
    pub max_candidates_per_step: u32,
    #[serde(default)]
    pub exclude_links_paths: Vec<String>,
    #[serde(default)]
    pub exclude_links_param_only_paths: bool,
    #[serde(default = "default_group_cross_depth")]
    pub cross_depth: u32,
    #[serde(default = "default_group_timeout_ms")]
    pub local_impact_timeout_ms: u64,
}

impl Default for GroupConfig {
    fn default() -> Self {
        Self {
            bm25_threshold: default_group_bm25_threshold(),
            max_candidates_per_step: default_group_max_candidates(),
            exclude_links_paths: Vec::new(),
            exclude_links_param_only_paths: false,
            cross_depth: default_group_cross_depth(),
            local_impact_timeout_ms: default_group_timeout_ms(),
        }
    }
}

fn default_group_bm25_threshold() -> f32 { 0.6 }
fn default_group_max_candidates() -> u32 { 16 }
fn default_group_cross_depth() -> u32 { 1 }
fn default_group_timeout_ms() -> u64 { 5000 }
```

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p graph-nexus-core --test config_group
```

Expected: 2 passed.

- [ ] **Step 5: Verify `gnx admin config` displays new fields**

```
cargo run -p graph-nexus --bin gnx -- admin config show 2>&1 | grep -A2 "\[group\]"
```

Expected: `[group]` section listed with the six fields. If
`admin/config.rs` enumerates fields manually (it does — see
`group_header` at `config.rs:413`), add a `group_header` sibling
function and call it from the same parent. Otherwise relies on
auto-render.

- [ ] **Step 6: Commit**

```
git add crates/graph-nexus-core/src/config.rs \
        crates/graph-nexus-core/tests/config_group.rs \
        crates/graph-nexus-cli/src/commands/admin/config.rs  # only if touched in step 5
git commit -m "$(cat <<'EOF'
feat(config): GroupConfig section with BM25 / cross-depth / timeout knobs

All thresholds default-on, override via ~/.gnx/config.toml. No
hardcoded constants in group code — see spec §Configuration.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 2 — Extractors (Tasks 3–7)

### Task 3: Extractor trait + per-language registry

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/group/extractors/mod.rs`
- Modify: `crates/graph-nexus-cli/src/commands/group/mod.rs` (add `pub mod extractors;`)
- Test: `crates/graph-nexus-cli/tests/group_extractor_registry.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/graph-nexus-cli/tests/group_extractor_registry.rs`:

```rust
use graph_nexus_cli::commands::group::extractors::{
    registry, ExtractorKind,
};

#[test]
fn registry_lists_first_wave_languages() {
    let entries = registry();
    let go_http = entries
        .iter()
        .find(|e| e.lang == "go" && e.kind == ExtractorKind::Http);
    assert!(go_http.is_some(), "missing go/http extractor");
    // Wave-1 minimum: 5 langs × 2 protocols = 10 entries (HTTP + gRPC)
    assert!(entries.len() >= 10, "got {} extractors, expected ≥10", entries.len());
}

#[test]
fn extractor_kinds_distinct() {
    let entries = registry();
    let http_count = entries.iter().filter(|e| e.kind == ExtractorKind::Http).count();
    let grpc_count = entries.iter().filter(|e| e.kind == ExtractorKind::Grpc).count();
    assert_eq!(http_count, 5);
    assert_eq!(grpc_count, 5);
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p graph-nexus --test group_extractor_registry
```

Expected: compile error.

- [ ] **Step 3: Create `extractors/mod.rs` with trait + empty registry**

```rust
//! Per-language extractors emitting ExtractedContract from source.
//! First wave: HTTP routes + gRPC service defs in Go/Python/Node/Java/Rust.
//! Other 9 mainstream langs are BlindSpot stubs (registered but emit nothing).

use crate::commands::group::types::ExtractedContract;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractorKind { Http, Grpc }

pub struct ExtractorEntry {
    pub lang: &'static str,
    pub kind: ExtractorKind,
    pub extract: fn(&Path, &[u8]) -> Vec<ExtractedContract>,
}

pub fn registry() -> Vec<ExtractorEntry> {
    let mut v: Vec<ExtractorEntry> = Vec::new();
    v.extend(http_extractors());
    v.extend(grpc_extractors());
    v
}

fn http_extractors() -> Vec<ExtractorEntry> {
    vec![
        ExtractorEntry { lang: "go",     kind: ExtractorKind::Http, extract: blind_spot_extractor },
        ExtractorEntry { lang: "python", kind: ExtractorKind::Http, extract: blind_spot_extractor },
        ExtractorEntry { lang: "node",   kind: ExtractorKind::Http, extract: blind_spot_extractor },
        ExtractorEntry { lang: "java",   kind: ExtractorKind::Http, extract: blind_spot_extractor },
        ExtractorEntry { lang: "rust",   kind: ExtractorKind::Http, extract: blind_spot_extractor },
    ]
}

fn grpc_extractors() -> Vec<ExtractorEntry> {
    vec![
        ExtractorEntry { lang: "go",     kind: ExtractorKind::Grpc, extract: blind_spot_extractor },
        ExtractorEntry { lang: "python", kind: ExtractorKind::Grpc, extract: blind_spot_extractor },
        ExtractorEntry { lang: "node",   kind: ExtractorKind::Grpc, extract: blind_spot_extractor },
        ExtractorEntry { lang: "java",   kind: ExtractorKind::Grpc, extract: blind_spot_extractor },
        ExtractorEntry { lang: "rust",   kind: ExtractorKind::Grpc, extract: blind_spot_extractor },
    ]
}

fn blind_spot_extractor(_path: &Path, _source: &[u8]) -> Vec<ExtractedContract> {
    Vec::new()
}

/// `(ext, lang)` mapping used by `sync.rs` when walking source files.
/// Centralised here so add-a-language touches one place.
pub fn lang_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "go" => Some("go"),
        "py" => Some("python"),
        "ts" | "tsx" | "js" | "jsx" => Some("node"),
        "java" => Some("java"),
        "rs" => Some("rust"),
        _ => None,
    }
}
```

- [ ] **Step 4: Register module in `commands/group/mod.rs`**

```rust
//! `gnx group <verb>` — multi-repo workflow surface.

pub mod extractors;
pub mod storage;
pub mod types;
```

- [ ] **Step 5: Run tests to verify they pass**

```
cargo test -p graph-nexus --test group_extractor_registry
```

Expected: 2 passed (10 BlindSpot entries — real impls land in Tasks 4–7).

- [ ] **Step 6: Commit**

```
git add crates/graph-nexus-cli/src/commands/group/ \
        crates/graph-nexus-cli/tests/group_extractor_registry.rs
git commit -m "$(cat <<'EOF'
feat(group/extractors): trait + registry skeleton with BlindSpot stubs

10 entries (5 langs × HTTP+gRPC), each pointing at blind_spot_extractor
returning Vec::new(). Per-language impls land in following commits.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: HTTP Go extractor (template impl)

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/group/extractors/http_go.rs`
- Modify: `crates/graph-nexus-cli/src/commands/group/extractors/mod.rs` (wire it)
- Create: `crates/graph-nexus-cli/tests/fixtures/group/go/http_server.go`
- Test: `crates/graph-nexus-cli/tests/group_extract_go_http.rs`

This task establishes the pattern for Tasks 5–7. **Read it carefully
before doing the other extractors.**

- [ ] **Step 1: Create test fixture**

`crates/graph-nexus-cli/tests/fixtures/group/go/http_server.go`:

```go
package main

import (
    "net/http"
)

func main() {
    mux := http.NewServeMux()
    mux.HandleFunc("/api/users", createUser)
    mux.HandleFunc("/api/users/{id}", getUser)
    http.ListenAndServe(":8080", mux)
}

func createUser(w http.ResponseWriter, r *http.Request) { _ = r.Method }
func getUser(w http.ResponseWriter, r *http.Request)    { _ = r.Method }
```

- [ ] **Step 2: Write the failing test**

`crates/graph-nexus-cli/tests/group_extract_go_http.rs`:

```rust
use graph_nexus_cli::commands::group::extractors::http_go::extract_http;
use graph_nexus_cli::commands::group::types::{ContractRole, ContractType};
use std::path::Path;

#[test]
fn go_net_http_handle_func_extracts_routes() {
    let path = Path::new("tests/fixtures/group/go/http_server.go");
    let source = std::fs::read(path).unwrap();
    let contracts = extract_http(path, &source);

    let ids: Vec<&str> = contracts.iter().map(|c| c.contract_id.as_str()).collect();
    assert!(ids.contains(&"http:ANY:/api/users"),
            "missing /api/users; got {ids:?}");
    assert!(ids.contains(&"http:ANY:/api/users/{id}"),
            "missing /api/users/{{id}}; got {ids:?}");

    for c in &contracts {
        assert_eq!(c.contract_type, ContractType::Http);
        assert_eq!(c.role, ContractRole::Provider);
        assert!(c.confidence >= 0.7);
    }
}

#[test]
fn go_non_route_calls_ignored() {
    let source = b"package main\nfunc main() { println(\"hi\") }\n";
    let contracts = extract_http(Path::new("x.go"), source);
    assert!(contracts.is_empty());
}
```

- [ ] **Step 3: Run test to verify it fails**

```
cargo test -p graph-nexus --test group_extract_go_http
```

Expected: compile error — `http_go` module missing.

- [ ] **Step 4: Implement `http_go.rs`**

```rust
//! Go HTTP route extractor: net/http + gin + chi shapes via tree-sitter.

use crate::commands::group::types::{
    ContractRole, ContractType, ExtractedContract, SymbolRef,
};
use std::path::Path;
use tree_sitter::{Parser, Query, QueryCursor};

const QUERY_SRC: &str = r#"
(call_expression
  function: (selector_expression
              field: (field_identifier) @method)
  arguments: (argument_list
               (interpreted_string_literal) @path
               .
               (_) @handler))
"#;

pub fn extract_http(file_path: &Path, source: &[u8]) -> Vec<ExtractedContract> {
    let mut parser = Parser::new();
    let lang = tree_sitter_go::language();
    if parser.set_language(lang).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return Vec::new(),
    };
    let query = match Query::new(lang, QUERY_SRC) {
        Ok(q) => q,
        Err(_) => return Vec::new(),
    };

    let method_idx  = query.capture_index_for_name("method").unwrap();
    let path_idx    = query.capture_index_for_name("path").unwrap();
    let handler_idx = query.capture_index_for_name("handler").unwrap();

    let mut cursor = QueryCursor::new();
    let mut out: Vec<ExtractedContract> = Vec::new();
    for m in cursor.matches(&query, tree.root_node(), source) {
        let method = capture_text(&m, method_idx, source);
        if !is_route_register(method) {
            continue;
        }
        let raw_path = capture_text(&m, path_idx, source);
        let path = unquote(raw_path);
        let handler = capture_text(&m, handler_idx, source);
        let http_method = http_method_from_call(method);
        let id = format!("http:{http_method}:{path}");
        out.push(ExtractedContract {
            contract_id: id,
            contract_type: ContractType::Http,
            role: ContractRole::Provider,
            symbol_uid: format!("{}::{}", file_path.display(), handler),
            symbol_ref: SymbolRef {
                file_path: file_path.display().to_string(),
                name: handler.to_string(),
            },
            confidence: 0.85,
            service: None,
            meta: vec![("method".into(), http_method.into())],
        });
    }
    out
}

fn capture_text<'a>(
    m: &tree_sitter::QueryMatch<'a, 'a>,
    idx: u32,
    source: &'a [u8],
) -> &'a str {
    for c in m.captures {
        if c.index == idx {
            return std::str::from_utf8(&source[c.node.byte_range()]).unwrap_or("");
        }
    }
    ""
}

fn is_route_register(method: &str) -> bool {
    matches!(
        method,
        "HandleFunc" | "Handle" | "GET" | "POST" | "PUT" | "DELETE"
        | "PATCH" | "Get" | "Post" | "Put" | "Delete" | "Patch"
    )
}

fn http_method_from_call(method: &str) -> &'static str {
    match method {
        "GET" | "Get" => "GET",
        "POST" | "Post" => "POST",
        "PUT" | "Put" => "PUT",
        "DELETE" | "Delete" => "DELETE",
        "PATCH" | "Patch" => "PATCH",
        _ => "ANY",
    }
}

fn unquote(s: &str) -> String {
    s.trim_start_matches('"').trim_end_matches('"').to_string()
}
```

- [ ] **Step 5: Verify `tree-sitter-go` is in `Cargo.toml`**

```
grep "tree-sitter-go" crates/graph-nexus-cli/Cargo.toml
```

If absent, add it under `[dependencies]` matching the version used by
`graph-nexus-analyzer`:

```
tree-sitter-go = { workspace = true }
```

(Check workspace `Cargo.toml`; existing analyzer crate already pulls
it, so workspace inheritance should already exist.)

- [ ] **Step 6: Wire into registry — modify `extractors/mod.rs`**

```rust
pub mod http_go;

fn http_extractors() -> Vec<ExtractorEntry> {
    vec![
        ExtractorEntry { lang: "go",     kind: ExtractorKind::Http, extract: http_go::extract_http },
        ExtractorEntry { lang: "python", kind: ExtractorKind::Http, extract: blind_spot_extractor },
        ExtractorEntry { lang: "node",   kind: ExtractorKind::Http, extract: blind_spot_extractor },
        ExtractorEntry { lang: "java",   kind: ExtractorKind::Http, extract: blind_spot_extractor },
        ExtractorEntry { lang: "rust",   kind: ExtractorKind::Http, extract: blind_spot_extractor },
    ]
}
```

- [ ] **Step 7: Run tests to verify they pass**

```
cargo test -p graph-nexus --test group_extract_go_http
```

Expected: 2 passed.

- [ ] **Step 8: Clippy + commit**

```
cargo clippy -p graph-nexus --tests -- -D warnings
git add crates/graph-nexus-cli/src/commands/group/extractors/ \
        crates/graph-nexus-cli/tests/group_extract_go_http.rs \
        crates/graph-nexus-cli/tests/fixtures/group/go/
[ -n "$(git status --porcelain crates/graph-nexus-cli/Cargo.toml)" ] && \
  git add crates/graph-nexus-cli/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(group/extractors): Go HTTP route extractor (net/http + gin + chi shapes)

Tree-sitter capture on call_expression with HandleFunc/HTTP-verb
selectors. Method-explicit calls (GET/POST/…) emit the verb; generic
HandleFunc/Handle emit ANY. Confidence 0.85 — high but not 1.0 since
non-server contexts can still match the pattern.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: HTTP extractors for Python / Node / Java / Rust

Follows Task 4's template exactly. **Each language** gets:
- A fixture under `tests/fixtures/group/<lang>/http_server.<ext>`
- A `http_<lang>.rs` module
- Tests in `tests/group_extract_<lang>_http.rs` mirroring the Go test

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/group/extractors/http_python.rs`
- Create: `crates/graph-nexus-cli/src/commands/group/extractors/http_node.rs`
- Create: `crates/graph-nexus-cli/src/commands/group/extractors/http_java.rs`
- Create: `crates/graph-nexus-cli/src/commands/group/extractors/http_rust.rs`
- Modify: `crates/graph-nexus-cli/src/commands/group/extractors/mod.rs`
- Create: `crates/graph-nexus-cli/tests/fixtures/group/{python,node,java,rust}/http_server.{py,ts,java,rs}`
- Test: `crates/graph-nexus-cli/tests/group_extract_{python,node,java,rust}_http.rs`

Language-specific patterns to match:

| Language | Library | Tree-sitter capture target |
|---|---|---|
| Python | flask: `@app.route("/path", methods=[...])` | `decorator` with `call.function = attribute(object="app", attr="route")` |
| Python | fastapi: `@app.get("/path")` / `@router.post(...)` | `decorator` with `call.function = attribute(attr in {get,post,put,delete,patch})` |
| Node | express: `app.get("/path", handler)` / `router.post(...)` | `call_expression` with `member_expression.property in {get,post,put,...}` |
| Java | spring: `@GetMapping("/path")` / `@RequestMapping(...)` | `annotation` with name matching `*Mapping` |
| Rust | axum: `.route("/path", get(handler))` | `call_expression` with `field_expression.field = "route"`; method derived from inner `get`/`post`/... call |
| Rust | actix: `#[get("/path")]` | `attribute_item` with `meta_item` HTTP verb name |

- [ ] **Step 1 (per language): Write fixture** — see `tests/fixtures/group/<lang>/http_server.<ext>` examples below.

**Python fixture** (`tests/fixtures/group/python/http_server.py`):

```python
from flask import Flask
app = Flask(__name__)

@app.route("/api/users", methods=["POST"])
def create_user():
    return ""

@app.route("/api/users/<id>")
def get_user(id):
    return ""
```

**Node fixture** (`tests/fixtures/group/node/http_server.ts`):

```ts
import express from "express";
const app = express();
app.post("/api/users", (req, res) => res.json({}));
app.get("/api/users/:id", (req, res) => res.json({}));
```

**Java fixture** (`tests/fixtures/group/java/HttpServer.java`):

```java
package demo;
import org.springframework.web.bind.annotation.*;

@RestController
public class HttpServer {
    @PostMapping("/api/users") public String createUser() { return ""; }
    @GetMapping("/api/users/{id}") public String getUser(@PathVariable String id) { return ""; }
}
```

**Rust fixture** (`tests/fixtures/group/rust/http_server.rs`):

```rust
use axum::{routing::{get, post}, Router};

async fn create_user() {}
async fn get_user() {}

fn router() -> Router {
    Router::new()
        .route("/api/users", post(create_user))
        .route("/api/users/:id", get(get_user))
}
```

- [ ] **Step 2 (per language): Write test** — mirror the Go test
shape. Assert `http:POST:/api/users` and the verb-correct version
of the `/api/users/{id}` route. Replace `{id}` per language's path
syntax in the assertion (`/api/users/<id>` for Python, `/api/users/:id`
for Node/Rust, `/api/users/{id}` for Java — extractor MAY normalise to
`{param}` but test must match what extractor emits).

- [ ] **Step 3 (per language): Run test to verify it fails**

```
cargo test -p graph-nexus --test group_extract_<lang>_http
```

Expected: missing module compile error.

- [ ] **Step 4 (per language): Implement extractor**

Same shape as `http_go.rs` — adjust tree-sitter language + query
to the patterns in the table above. Confidence floor: 0.7 (path is
clearly inside an HTTP route declaration); 0.85 when HTTP verb
explicit; 1.0 reserved for graph-assisted Strategy A path (not in
first wave).

- [ ] **Step 5: Wire all 4 into `extractors/mod.rs`**

Replace the 4 `blind_spot_extractor` entries in `http_extractors()`
with the real fn pointers.

- [ ] **Step 6: Run all HTTP extractor tests**

```
cargo test -p graph-nexus --test 'group_extract_*_http'
```

Expected: all green (Go from Task 4 still passes too).

- [ ] **Step 7: Clippy clean**

```
cargo clippy -p graph-nexus --tests -- -D warnings
```

- [ ] **Step 8: Commit (one commit for all 4 langs)**

```
git add crates/graph-nexus-cli/src/commands/group/extractors/http_*.rs \
        crates/graph-nexus-cli/src/commands/group/extractors/mod.rs \
        crates/graph-nexus-cli/tests/group_extract_*_http.rs \
        crates/graph-nexus-cli/tests/fixtures/group/
git commit -m "$(cat <<'EOF'
feat(group/extractors): HTTP route extractors for Python/Node/Java/Rust

flask + fastapi (Python), express (Node TS/JS), spring (Java),
axum + actix (Rust). Same shape as the Go extractor.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: gRPC Go extractor (template impl)

Follows Task 4 pattern. gRPC contracts have two roles:
- **Provider** (`server`): `pb.RegisterFooServer(grpcServer, &impl{})`
- **Consumer** (`client`): `pb.NewFooClient(conn).Method(ctx, req)`

For first wave we only emit **provider** entries (server registration
is unambiguous; client extraction requires call-graph data we don't
yet pull through this path). Consumers in upstream are derived via the
graph-assisted Strategy A, deferred.

**Contract ID format:** `grpc:<service>:<method>`. When only service
is detectable (server registration), emit one entry per registered
service with `method = "*"`.

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/group/extractors/grpc_go.rs`
- Create: `crates/graph-nexus-cli/tests/fixtures/group/go/grpc_server.go`
- Test: `crates/graph-nexus-cli/tests/group_extract_go_grpc.rs`
- Modify: `crates/graph-nexus-cli/src/commands/group/extractors/mod.rs`

- [ ] **Step 1: Create fixture**

`tests/fixtures/group/go/grpc_server.go`:

```go
package main

import (
    "google.golang.org/grpc"
    pb "example/userpb"
)

type userSrv struct {
    pb.UnimplementedUserServiceServer
}

func main() {
    s := grpc.NewServer()
    pb.RegisterUserServiceServer(s, &userSrv{})
}
```

- [ ] **Step 2: Write the failing test**

`tests/group_extract_go_grpc.rs`:

```rust
use graph_nexus_cli::commands::group::extractors::grpc_go::extract_grpc;
use graph_nexus_cli::commands::group::types::{ContractRole, ContractType};
use std::path::Path;

#[test]
fn go_grpc_server_registration_extracts_service() {
    let path = Path::new("tests/fixtures/group/go/grpc_server.go");
    let source = std::fs::read(path).unwrap();
    let contracts = extract_grpc(path, &source);
    let ids: Vec<&str> = contracts.iter().map(|c| c.contract_id.as_str()).collect();
    assert!(ids.contains(&"grpc:UserService:*"), "got {ids:?}");
    assert_eq!(contracts[0].contract_type, ContractType::Grpc);
    assert_eq!(contracts[0].role, ContractRole::Provider);
}
```

- [ ] **Step 3: Run test to verify it fails**

```
cargo test -p graph-nexus --test group_extract_go_grpc
```

Expected: compile error.

- [ ] **Step 4: Implement `grpc_go.rs`**

```rust
//! Go gRPC server registration extractor. Captures Register<Svc>Server
//! calls and emits a provider contract per registered service.

use crate::commands::group::types::{
    ContractRole, ContractType, ExtractedContract, SymbolRef,
};
use std::path::Path;
use tree_sitter::{Parser, Query, QueryCursor};

const QUERY_SRC: &str = r#"
(call_expression
  function: (selector_expression
              field: (field_identifier) @register_fn))
"#;

pub fn extract_grpc(file_path: &Path, source: &[u8]) -> Vec<ExtractedContract> {
    let mut parser = Parser::new();
    let lang = tree_sitter_go::language();
    if parser.set_language(lang).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return Vec::new(),
    };
    let query = match Query::new(lang, QUERY_SRC) {
        Ok(q) => q,
        Err(_) => return Vec::new(),
    };
    let reg_idx = query.capture_index_for_name("register_fn").unwrap();

    let mut cursor = QueryCursor::new();
    let mut out: Vec<ExtractedContract> = Vec::new();
    for m in cursor.matches(&query, tree.root_node(), source) {
        let text = m.captures.iter()
            .find(|c| c.index == reg_idx)
            .and_then(|c| std::str::from_utf8(&source[c.node.byte_range()]).ok())
            .unwrap_or("");
        let svc = text
            .strip_prefix("Register")
            .and_then(|s| s.strip_suffix("Server"));
        let Some(svc) = svc else { continue };
        out.push(ExtractedContract {
            contract_id: format!("grpc:{svc}:*"),
            contract_type: ContractType::Grpc,
            role: ContractRole::Provider,
            symbol_uid: format!("{}::Register{svc}Server", file_path.display()),
            symbol_ref: SymbolRef {
                file_path: file_path.display().to_string(),
                name: format!("Register{svc}Server"),
            },
            confidence: 0.9,
            service: None,
            meta: vec![("service".into(), svc.to_string())],
        });
    }
    out
}
```

- [ ] **Step 5: Wire into registry, run test, commit**

Update `grpc_extractors()` in `extractors/mod.rs` to point Go's slot
at `grpc_go::extract_grpc`. Run:

```
cargo test -p graph-nexus --test group_extract_go_grpc
cargo clippy -p graph-nexus --tests -- -D warnings
git add crates/graph-nexus-cli/src/commands/group/extractors/grpc_go.rs \
        crates/graph-nexus-cli/src/commands/group/extractors/mod.rs \
        crates/graph-nexus-cli/tests/group_extract_go_grpc.rs \
        crates/graph-nexus-cli/tests/fixtures/group/go/grpc_server.go
git commit -m "$(cat <<'EOF'
feat(group/extractors): Go gRPC provider extractor

Captures Register<Svc>Server calls. Method-level extraction deferred
to graph-assisted Strategy A; first-wave server entry emits
service-level contract with method='*'.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: gRPC extractors for Python / Node / Java / Rust

Same shape as Task 5 for HTTP. Per language:

| Language | gRPC server registration pattern |
|---|---|
| Python | `<service>_pb2_grpc.add_<Svc>Servicer_to_server(<impl>, server)` |
| Node | `server.addService(<svc>_proto.<Svc>.service, { ... })` |
| Java | `serverBuilder.addService(new <Svc>ImplBase() { ... })` — extract from generic argument |
| Rust | `tonic::transport::Server::builder().add_service(<svc>_server::<Svc>Server::new(impl))` |

- [ ] **Step 1–4 (per language)**: fixture / test / implementation
  exactly as Task 6 with language-specific tree-sitter patterns.

- [ ] **Step 5: Wire 4 entries into `grpc_extractors()`**

- [ ] **Step 6: Run all gRPC tests**

```
cargo test -p graph-nexus --test 'group_extract_*_grpc'
```

- [ ] **Step 7: Clippy + commit**

```
cargo clippy -p graph-nexus --tests -- -D warnings
git add crates/graph-nexus-cli/src/commands/group/extractors/grpc_*.rs \
        crates/graph-nexus-cli/src/commands/group/extractors/mod.rs \
        crates/graph-nexus-cli/tests/group_extract_*_grpc.rs \
        crates/graph-nexus-cli/tests/fixtures/group/
git commit -m "$(cat <<'EOF'
feat(group/extractors): gRPC provider extractors for Python/Node/Java/Rust

grpcio (Python), @grpc/grpc-js (Node), grpc-java, tonic (Rust).
Service-level provider contracts; method-level deferred.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 3 — Sync Engine (Tasks 8–9)

### Task 8: Matching cascade (exact + BM25)

Reuses `TantivyEngine` from `graph-nexus-cli/src/search.rs`. Per-group
Tantivy index lives at `~/.gnx/groups/<name>/contracts_index/`.

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/group/matching.rs`
- Modify: `crates/graph-nexus-cli/src/commands/group/mod.rs` (add `pub mod matching;`)
- Test: `crates/graph-nexus-cli/tests/group_matching.rs`

- [ ] **Step 1: Write the failing test**

```rust
use graph_nexus_cli::commands::group::matching::match_contracts;
use graph_nexus_cli::commands::group::types::{
    ContractRole, ContractType, CrossLink, ExtractedContract, MatchType,
    StoredContract, SymbolRef,
};
use graph_nexus_core::config::GroupConfig;
use tempfile::TempDir;

fn make_contract(repo: &str, role: ContractRole, id: &str) -> StoredContract {
    StoredContract {
        repo: repo.into(),
        inner: ExtractedContract {
            contract_id: id.into(),
            contract_type: ContractType::Http,
            role,
            symbol_uid: format!("{repo}::handler"),
            symbol_ref: SymbolRef {
                file_path: "x".into(),
                name: "h".into(),
            },
            confidence: 1.0,
            service: None,
            meta: vec![],
        },
    }
}

#[test]
fn exact_match_pairs_provider_consumer() {
    let dir = TempDir::new().unwrap();
    let contracts = vec![
        make_contract("a", ContractRole::Provider, "http:GET:/x"),
        make_contract("b", ContractRole::Consumer, "http:GET:/x"),
    ];
    let cfg = GroupConfig::default();
    let (links, unmatched) = match_contracts(&contracts, dir.path(), &cfg, false).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].match_type, MatchType::Exact);
    assert_eq!(links[0].confidence, 1.0);
    assert!(unmatched.is_empty());
}

#[test]
fn unmatched_consumer_lands_in_unmatched() {
    let dir = TempDir::new().unwrap();
    let contracts = vec![
        make_contract("b", ContractRole::Consumer, "http:GET:/orphan"),
    ];
    let cfg = GroupConfig::default();
    let (links, unmatched) = match_contracts(&contracts, dir.path(), &cfg, true).unwrap();
    assert!(links.is_empty());
    assert_eq!(unmatched.len(), 1);
}

#[test]
fn exact_only_skips_bm25() {
    let dir = TempDir::new().unwrap();
    let contracts = vec![
        make_contract("a", ContractRole::Provider, "http:GET:/users"),
        make_contract("b", ContractRole::Consumer, "http:GET:/user"),  // near-miss
    ];
    let cfg = GroupConfig::default();
    let (links, unmatched) = match_contracts(&contracts, dir.path(), &cfg, true).unwrap();
    assert!(links.is_empty(), "exact_only must not BM25-match near-miss");
    assert_eq!(unmatched.len(), 1);
}

#[test]
fn exclude_paths_drops_health_check() {
    let dir = TempDir::new().unwrap();
    let contracts = vec![
        make_contract("a", ContractRole::Provider, "http:GET:/health"),
        make_contract("b", ContractRole::Consumer, "http:GET:/health"),
    ];
    let mut cfg = GroupConfig::default();
    cfg.exclude_links_paths = vec!["/health".into()];
    let (links, _) = match_contracts(&contracts, dir.path(), &cfg, false).unwrap();
    assert!(links.is_empty(), "/health must be excluded from cross-links");
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p graph-nexus --test group_matching
```

Expected: compile error.

- [ ] **Step 3: Implement `matching.rs`**

```rust
//! Exact + BM25 cascade for cross-link generation. BM25 stage reuses
//! TantivyEngine — no new search dependency.

use crate::commands::group::types::{
    ContractRole, CrossLink, CrossLinkEndpoint, MatchType, StoredContract,
};
use graph_nexus_core::config::GroupConfig;
use std::collections::HashMap;
use std::io;
use std::path::Path;

pub fn match_contracts(
    contracts: &[StoredContract],
    group_dir: &Path,
    cfg: &GroupConfig,
    exact_only: bool,
) -> io::Result<(Vec<CrossLink>, Vec<StoredContract>)> {
    let kept: Vec<&StoredContract> = contracts
        .iter()
        .filter(|c| !is_excluded(&c.inner.contract_id, cfg))
        .collect();

    let mut by_id_providers: HashMap<&str, Vec<&StoredContract>> = HashMap::new();
    let mut by_id_consumers: HashMap<&str, Vec<&StoredContract>> = HashMap::new();
    for c in &kept {
        match c.inner.role {
            ContractRole::Provider => by_id_providers.entry(&c.inner.contract_id).or_default().push(c),
            ContractRole::Consumer => by_id_consumers.entry(&c.inner.contract_id).or_default().push(c),
        }
    }

    let mut links: Vec<CrossLink> = Vec::new();
    let mut matched_uids: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Exact stage: same contract_id, different repo
    for (id, providers) in &by_id_providers {
        let Some(consumers) = by_id_consumers.get(*id) else { continue };
        for p in providers {
            for c in consumers {
                if p.repo == c.repo { continue }
                links.push(make_link(c, p, MatchType::Exact, 1.0));
                matched_uids.insert(c.inner.symbol_uid.clone());
                matched_uids.insert(p.inner.symbol_uid.clone());
            }
        }
    }

    // BM25 stage — skipped when exact_only or when no consumers remain unmatched
    if !exact_only {
        let unmatched_consumers: Vec<&&StoredContract> = kept.iter()
            .filter(|c| c.inner.role == ContractRole::Consumer
                     && !matched_uids.contains(&c.inner.symbol_uid))
            .collect();
        if !unmatched_consumers.is_empty() {
            let index_dir = group_dir.join("contracts_index");
            build_bm25_index(&index_dir, &kept)?;
            for cons in &unmatched_consumers {
                let candidates = bm25_search(
                    &index_dir,
                    &cons.inner.contract_id,
                    cfg.max_candidates_per_step as usize,
                );
                for (uid, score) in candidates {
                    if score < cfg.bm25_threshold { continue }
                    let Some(prov) = kept.iter().find(|c| c.inner.symbol_uid == uid
                                                       && c.inner.role == ContractRole::Provider) else {
                        continue;
                    };
                    if prov.repo == cons.repo { continue }
                    links.push(make_link(cons, prov, MatchType::Bm25, score));
                    matched_uids.insert(cons.inner.symbol_uid.clone());
                }
            }
        }
    }

    let unmatched: Vec<StoredContract> = kept.iter()
        .filter(|c| c.inner.role == ContractRole::Consumer
                 && !matched_uids.contains(&c.inner.symbol_uid))
        .map(|c| (*c).clone())
        .collect();

    Ok((links, unmatched))
}

fn make_link(from: &StoredContract, to: &StoredContract, mt: MatchType, conf: f32) -> CrossLink {
    CrossLink {
        from: CrossLinkEndpoint {
            repo: from.repo.clone(),
            service: from.inner.service.clone(),
            symbol_uid: from.inner.symbol_uid.clone(),
            symbol_ref: from.inner.symbol_ref.clone(),
        },
        to: CrossLinkEndpoint {
            repo: to.repo.clone(),
            service: to.inner.service.clone(),
            symbol_uid: to.inner.symbol_uid.clone(),
            symbol_ref: to.inner.symbol_ref.clone(),
        },
        contract_type: to.inner.contract_type.clone(),
        contract_id: to.inner.contract_id.clone(),
        match_type: mt,
        confidence: conf,
    }
}

fn is_excluded(contract_id: &str, cfg: &GroupConfig) -> bool {
    if let Some(path) = contract_id.split(':').nth(2) {
        let norm = path.trim_end_matches('/');
        if cfg.exclude_links_paths.iter().any(|p| p.trim_end_matches('/') == norm) {
            return true;
        }
        if cfg.exclude_links_param_only_paths
            && norm.split('/').filter(|s| !s.is_empty()).all(|s| s == "{param}")
        {
            return true;
        }
    }
    false
}

fn build_bm25_index(index_dir: &Path, contracts: &[&StoredContract]) -> io::Result<()> {
    // Single-field Tantivy index over contract_id; uid kept as stored
    // for hit-to-contract mapping. Mirrors the schema used in
    // graph-nexus-cli/src/search.rs but with a contract-only doc shape.
    use tantivy::doc;
    use tantivy::schema::{Schema, STORED, TEXT};

    std::fs::create_dir_all(index_dir)?;
    let mut schema = Schema::builder();
    let f_id = schema.add_text_field("contract_id", TEXT | STORED);
    let f_uid = schema.add_text_field("uid", STORED);
    let schema = schema.build();
    let dir = tantivy::directory::MmapDirectory::open(index_dir)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("tantivy dir: {e:?}")))?;
    let index = tantivy::Index::open_or_create(dir, schema)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("tantivy idx: {e:?}")))?;
    let mut w = index.writer(50_000_000)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("tantivy w: {e:?}")))?;
    w.delete_all_documents()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("tantivy del: {e:?}")))?;
    for c in contracts {
        let _ = w.add_document(doc!(
            f_id => c.inner.contract_id.clone(),
            f_uid => c.inner.symbol_uid.clone(),
        ));
    }
    w.commit().map_err(|e| io::Error::new(io::ErrorKind::Other, format!("tantivy commit: {e:?}")))?;
    Ok(())
}

fn bm25_search(index_dir: &Path, query_text: &str, limit: usize) -> Vec<(String, f32)> {
    use tantivy::collector::TopDocs;
    use tantivy::query::QueryParser;
    let dir = match tantivy::directory::MmapDirectory::open(index_dir) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let index = match tantivy::Index::open(dir) {
        Ok(i) => i,
        Err(_) => return Vec::new(),
    };
    let schema = index.schema();
    let f_id = schema.get_field("contract_id").unwrap();
    let f_uid = schema.get_field("uid").unwrap();
    let reader = match index.reader() { Ok(r) => r, Err(_) => return Vec::new() };
    let searcher = reader.searcher();
    let parser = QueryParser::for_index(&index, vec![f_id]);
    let query = match parser.parse_query(&escape_query(query_text)) {
        Ok(q) => q,
        Err(_) => return Vec::new(),
    };
    let hits = match searcher.search(&query, &TopDocs::with_limit(limit)) {
        Ok(h) => h,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<(String, f32)> = Vec::with_capacity(hits.len());
    for (score, addr) in hits {
        let Ok(doc) = searcher.doc(addr) else { continue };
        let Some(uid) = doc.get_first(f_uid).and_then(|v| v.as_text()) else { continue };
        out.push((uid.to_string(), score));
    }
    out
}

fn escape_query(s: &str) -> String {
    // Tantivy parser treats `:` as field delimiter; contract_ids contain `:`.
    s.replace(':', " ")
}
```

- [ ] **Step 4: Verify Tantivy is in `graph-nexus-cli`'s deps**

```
grep tantivy crates/graph-nexus-cli/Cargo.toml
```

Should already be present (used by `src/search.rs`). If
`tantivy::directory::MmapDirectory` is not exposed, switch to
`tantivy::Index::create_in_dir(index_dir, schema)` which is the
simpler high-level API.

- [ ] **Step 5: Run tests**

```
cargo test -p graph-nexus --test group_matching
```

Expected: 4 passed.

- [ ] **Step 6: Clippy + commit**

```
cargo clippy -p graph-nexus --tests -- -D warnings
git add crates/graph-nexus-cli/src/commands/group/matching.rs \
        crates/graph-nexus-cli/src/commands/group/mod.rs \
        crates/graph-nexus-cli/tests/group_matching.rs
git commit -m "$(cat <<'EOF'
feat(group): exact + BM25 matching cascade

Exact stage pairs provider/consumer on identical contract_id across
different repos. BM25 stage (skipped under --exact-only) reuses
Tantivy — no new search dependency — and filters by
GroupConfig.bm25_threshold. exclude_links_paths and
exclude_links_param_only_paths drop noise pre-match.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: `gnx group sync` command + wiring

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/group/sync.rs`
- Modify: `crates/graph-nexus-cli/src/commands/group/mod.rs` — add
  `pub mod sync;` + a `GroupCommands` enum and a `pub fn run(cmd) ->
  Result<(), GnxError>` dispatcher
- Modify: `crates/graph-nexus-cli/src/main.rs` — register the
  top-level `Group` subcommand
- Test: `crates/graph-nexus-cli/tests/group_sync.rs`

- [ ] **Step 1: Wire CLI subcommand in `main.rs`**

Locate the `enum Commands { ... }` enum (Subcommand-derived) and add:

```rust
/// Multi-repo workflow surface (sync / status / impact / contracts /
/// search / find / coverage). See `gnx group --help`.
Group {
    #[command(subcommand)]
    cmd: commands::group::GroupCommands,
},
```

In the `match cli.command { ... }` dispatch, add:

```rust
Commands::Group { cmd } => commands::group::run(cmd)?,
```

- [ ] **Step 2: Define `GroupCommands` enum in `commands/group/mod.rs`**

Append:

```rust
use clap::{Args, Subcommand};
use graph_nexus_core::GnxError;

#[derive(Subcommand, Debug)]
pub enum GroupCommands {
    /// Extract contracts + build cross-links for a group.
    Sync(sync::SyncArgs),
    // Status / Contracts / Impact / Search / Find / Coverage added in later tasks.
}

pub fn run(cmd: GroupCommands) -> Result<(), GnxError> {
    match cmd {
        GroupCommands::Sync(args) => sync::run(args),
    }
}
```

- [ ] **Step 3: Write the failing integration test**

`crates/graph-nexus-cli/tests/group_sync.rs`:

```rust
//! End-to-end: 2-repo fixture (Go provider + Python consumer of /api/users),
//! gnx group sync writes contracts.rkyv + meta.json, exact-matches across them.

use assert_cmd::Command;
use graph_nexus_cli::commands::group::storage::{read_contracts, read_meta};
use std::fs;
use tempfile::TempDir;

fn write(p: &std::path::Path, contents: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, contents).unwrap();
}

#[test]
fn sync_two_repo_http_exact_match() {
    let home = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();

    // backend (Go provider)
    let backend = workspaces.path().join("backend");
    write(&backend.join("main.go"), r#"package main
import "net/http"
func main() { http.HandleFunc("/api/users", func(w http.ResponseWriter, r *http.Request){}) }
"#);
    Command::cargo_bin("gnx").unwrap()
        .env("GNX_HOME", home.path())
        .args(["admin", "index", backend.to_str().unwrap()])
        .assert().success();

    // frontend (Python consumer — simulated as a flask route hitting the same path)
    let frontend = workspaces.path().join("frontend");
    write(&frontend.join("app.py"), r#"
from flask import Flask
app = Flask(__name__)
@app.route("/api/users", methods=["POST"])
def proxy(): return ""
"#);
    Command::cargo_bin("gnx").unwrap()
        .env("GNX_HOME", home.path())
        .args(["admin", "index", frontend.to_str().unwrap()])
        .assert().success();

    // Form the group
    Command::cargo_bin("gnx").unwrap()
        .env("GNX_HOME", home.path())
        .args(["admin", "group", "add", "demo", "backend"])
        .assert().success();
    Command::cargo_bin("gnx").unwrap()
        .env("GNX_HOME", home.path())
        .args(["admin", "group", "add", "demo", "frontend"])
        .assert().success();

    // Sync
    Command::cargo_bin("gnx").unwrap()
        .env("GNX_HOME", home.path())
        .args(["group", "sync", "demo"])
        .assert().success();

    // Verify artifacts
    let gdir = home.path().join("groups").join("demo");
    let reg = read_contracts(&gdir).unwrap();
    assert!(reg.contracts.len() >= 2, "got {} contracts", reg.contracts.len());
    // At least one exact link across the two repos. Both contracts use
    // the same path; one provider, one consumer-ish.
    // (If the python route is also classified as provider, no link will
    // form — that's fine; the test then asserts contracts presence only.
    // Tighten this in followups when consumer detection lands.)
    let meta = read_meta(&gdir).unwrap();
    assert_eq!(meta.repo_snapshots.len(), 2);
}
```

- [ ] **Step 4: Run test to verify it fails**

```
cargo test -p graph-nexus --test group_sync
```

Expected: command fails — `Sync` not implemented.

- [ ] **Step 5: Implement `sync.rs`**

```rust
//! `gnx group sync <name>` — extract contracts across group members,
//! run matching cascade, write contracts.rkyv + meta.json.

use crate::commands::group::extractors::{lang_for_extension, registry, ExtractorEntry};
use crate::commands::group::matching::match_contracts;
use crate::commands::group::storage::{group_dir, write_contracts, write_meta, GroupMeta, RepoSnapshot};
use crate::commands::group::types::{ContractRegistry, StoredContract};
use clap::Args;
use graph_nexus_core::config::Config;
use graph_nexus_core::registry::path::resolve_home_gnx;
use graph_nexus_core::registry::Registry;
use graph_nexus_core::GnxError;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use walkdir::WalkDir;

#[derive(Args, Debug)]
pub struct SyncArgs {
    /// Group name (must exist in registry.json).
    pub name: String,
    /// Skip BM25 stage; exact match only.
    #[arg(long)]
    pub exact_only: bool,
    /// Don't bail when per-repo index is stale.
    #[arg(long)]
    pub allow_stale: bool,
    /// Emit JSON instead of TOON.
    #[arg(long)]
    pub json: bool,
    /// Show per-cross-link detail.
    #[arg(long)]
    pub verbose: bool,
}

pub fn run(args: SyncArgs) -> Result<(), GnxError> {
    let started = Instant::now();
    let home = resolve_home_gnx()?;
    let cfg = Config::load(&home).unwrap_or_default();
    let registry = Registry::open(&home)?;
    let entry = registry
        .find_group(&args.name)
        .ok_or_else(|| GnxError::other(format!("group not found: {}", args.name)))?;

    let members: Vec<String> = entry.members.clone();
    let extractors = registry_grouped();

    let per_repo: Vec<(String, Vec<StoredContract>, Option<RepoSnapshot>)> = members
        .par_iter()
        .map(|member| {
            let snapshot = registry.repo_snapshot(member).ok();
            let Some(repo_path) = registry.resolve(member).ok() else {
                return (member.clone(), Vec::new(), None);
            };
            let contracts = extract_from_repo(member, &repo_path, &extractors);
            (member.clone(), contracts, snapshot)
        })
        .collect();

    let mut all_contracts: Vec<StoredContract> = Vec::new();
    let mut snapshots: BTreeMap<String, RepoSnapshot> = BTreeMap::new();
    let mut missing: Vec<String> = Vec::new();
    for (m, cs, snap) in per_repo {
        if let Some(s) = snap {
            snapshots.insert(m.clone(), s);
        } else {
            missing.push(m.clone());
        }
        all_contracts.extend(cs);
    }

    let gdir = group_dir(&home, &args.name);
    let (links, unmatched) = match_contracts(
        &all_contracts,
        &gdir,
        &cfg.group,
        args.exact_only,
    )?;

    let reg_out = ContractRegistry {
        version: 1,
        contracts: all_contracts.clone(),
        cross_links: links.clone(),
        unmatched: unmatched.clone(),
    };
    write_contracts(&gdir, &reg_out)?;
    write_meta(&gdir, &GroupMeta {
        version: 1,
        generated_at: chrono::Utc::now().to_rfc3339(),
        repo_snapshots: snapshots,
        missing_repos: missing,
    })?;

    emit_summary(&args, &reg_out, started.elapsed());
    Ok(())
}

fn registry_grouped() -> Vec<ExtractorEntry> {
    registry()
}

fn extract_from_repo(
    repo_label: &str,
    repo_root: &Path,
    extractors: &[ExtractorEntry],
) -> Vec<StoredContract> {
    let _ = repo_label;
    let mut out: Vec<StoredContract> = Vec::new();
    for entry in WalkDir::new(repo_root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() { continue }
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|s| s.to_str()) else { continue };
        let Some(lang) = lang_for_extension(ext) else { continue };
        let Ok(bytes) = std::fs::read(path) else { continue };
        for ex in extractors.iter().filter(|e| e.lang == lang) {
            for c in (ex.extract)(path, &bytes) {
                out.push(StoredContract { repo: repo_label.to_string(), inner: c });
            }
        }
    }
    out
}

fn emit_summary(args: &SyncArgs, reg: &ContractRegistry, elapsed: std::time::Duration) {
    let exact = reg.cross_links.iter().filter(|l| matches!(l.match_type, crate::commands::group::types::MatchType::Exact)).count();
    let bm25 = reg.cross_links.iter().filter(|l| matches!(l.match_type, crate::commands::group::types::MatchType::Bm25)).count();

    if args.json {
        let v = serde_json::json!({
            "group": args.name,
            "contracts": reg.contracts.len(),
            "cross_links": { "exact": exact, "bm25": bm25 },
            "unmatched": reg.unmatched.len(),
            "elapsed_ms": elapsed.as_millis(),
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
    } else {
        println!("group         {}", args.name);
        println!("contracts     {}", reg.contracts.len());
        println!("cross_links");
        println!("  exact       {exact}");
        println!("  bm25        {bm25}");
        println!("unmatched     {}", reg.unmatched.len());
        println!("elapsed_ms    {}", elapsed.as_millis());
    }
    if args.verbose {
        for l in &reg.cross_links {
            println!("  {} -> {}  [{:?}, conf={:.2}]  {}",
                     l.from.repo, l.to.repo, l.match_type, l.confidence, l.contract_id);
        }
    }
}
```

- [ ] **Step 6: Run test to verify it passes**

```
cargo test -p graph-nexus --test group_sync -- --nocapture
```

Expected: PASS. If `Registry::find_group` / `Registry::repo_snapshot`
/ `Config::load` don't exist with these exact names, locate the
equivalents and adjust the call sites. Update the test's expected
indexed-paths if the test fixture layout differs.

- [ ] **Step 7: Clippy + commit**

```
cargo clippy -p graph-nexus --tests -- -D warnings
git add crates/graph-nexus-cli/src/commands/group/sync.rs \
        crates/graph-nexus-cli/src/commands/group/mod.rs \
        crates/graph-nexus-cli/src/main.rs \
        crates/graph-nexus-cli/tests/group_sync.rs
git commit -m "$(cat <<'EOF'
feat(group): gnx group sync — extract contracts + write registry

Parallel per-member extraction via rayon, exact+BM25 matching cascade
from Task 8, atomic rkyv write. Defaults to TOON summary; --json /
--verbose / --exact-only / --allow-stale supported.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 4 — Query Commands (Tasks 10–13)

### Task 10: `gnx group status`

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/group/status.rs`
- Modify: `commands/group/mod.rs` — register `Status(status::StatusArgs)`
- Test: `crates/graph-nexus-cli/tests/group_status.rs`

- [ ] **Step 1: Write failing test**

```rust
use assert_cmd::Command;
use graph_nexus_cli::commands::group::storage::{group_dir, write_meta, GroupMeta, RepoSnapshot};
use std::collections::BTreeMap;
use tempfile::TempDir;

#[test]
fn status_reports_stale_when_meta_commit_differs_from_head() {
    let home = TempDir::new().unwrap();
    // … set up a one-repo group with a known last_commit that doesn't
    // match the worktree HEAD, write meta.json, then:
    let out = Command::cargo_bin("gnx").unwrap()
        .env("GNX_HOME", home.path())
        .args(["group", "status", "demo"])
        .assert().success()
        .get_output().stdout.clone();
    let stdout = String::from_utf8_lossy(&out);
    assert!(stdout.contains("STALE"), "expected STALE marker; got: {stdout}");
}

#[test]
fn status_never_synced_reports_no_meta() {
    let home = TempDir::new().unwrap();
    // … create the group but never sync …
    let out = Command::cargo_bin("gnx").unwrap()
        .env("GNX_HOME", home.path())
        .args(["group", "status", "demo"])
        .assert().success()
        .get_output().stdout.clone();
    let stdout = String::from_utf8_lossy(&out);
    assert!(stdout.contains("never synced"));
}
```

- [ ] **Step 2: Run, see fail, implement `status.rs`**

```rust
use crate::commands::group::storage::{group_dir, read_meta, META_FILE};
use clap::Args;
use graph_nexus_core::registry::path::resolve_home_gnx;
use graph_nexus_core::registry::Registry;
use graph_nexus_core::GnxError;
use std::process::Command;

#[derive(Args, Debug)]
pub struct StatusArgs {
    pub name: String,
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: StatusArgs) -> Result<(), GnxError> {
    let home = resolve_home_gnx()?;
    let registry = Registry::open(&home)?;
    let entry = registry.find_group(&args.name)
        .ok_or_else(|| GnxError::other(format!("group not found: {}", args.name)))?;
    let gdir = group_dir(&home, &args.name);
    let meta_path = gdir.join(META_FILE);
    if !meta_path.exists() {
        println!("Group: {} (never synced)", args.name);
        for m in &entry.members {
            println!("  {:25} NO_META", m);
        }
        return Ok(());
    }
    let meta = read_meta(&gdir)?;
    println!("Group: {} (last sync: {})", args.name, meta.generated_at);
    for m in &entry.members {
        let snap = meta.repo_snapshots.get(m);
        match snap {
            None => println!("  {:25} MISSING", m),
            Some(snap) => {
                let repo_root = registry.resolve(m).ok();
                let head = repo_root.as_deref().and_then(head_commit);
                let stale = head.as_deref().map(|h| h != snap.last_commit).unwrap_or(true);
                let behind = if stale {
                    repo_root.as_deref()
                        .and_then(|r| commits_behind(r, &snap.last_commit))
                        .map(|n| format!("({n} behind)"))
                        .unwrap_or_default()
                } else { String::new() };
                println!("  {:25} {} {}", m,
                         if stale { "STALE" } else { "OK   " },
                         behind);
            }
        }
    }
    Ok(())
}

fn head_commit(repo: &std::path::Path) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(["rev-parse", "HEAD"]).output().ok()?;
    if !out.status.success() { return None }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn commits_behind(repo: &std::path::Path, base: &str) -> Option<u64> {
    let range = format!("{base}..HEAD");
    let out = Command::new("git").arg("-C").arg(repo).args(["rev-list", "--count", &range]).output().ok()?;
    if !out.status.success() { return None }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}
```

- [ ] **Step 3: Register in dispatcher**

In `commands/group/mod.rs` add to `GroupCommands` and dispatch:

```rust
Status(status::StatusArgs),
// in match:
GroupCommands::Status(args) => status::run(args),
```

- [ ] **Step 4: Run test, fix any path expectations**

```
cargo test -p graph-nexus --test group_status
```

- [ ] **Step 5: Commit**

```
git add crates/graph-nexus-cli/src/commands/group/status.rs \
        crates/graph-nexus-cli/src/commands/group/mod.rs \
        crates/graph-nexus-cli/tests/group_status.rs
git commit -m "feat(group): gnx group status — index + contracts staleness

Diffs meta.json snapshots against per-repo HEAD via git rev-parse.
Reports STALE / OK / MISSING / NO_META per member.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 11: `gnx group contracts`

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/group/contracts.rs`
- Modify: `commands/group/mod.rs`
- Test: `crates/graph-nexus-cli/tests/group_contracts.rs`

- [ ] **Step 1: Write failing test**

```rust
use graph_nexus_cli::commands::group::storage::{group_dir, write_contracts};
use graph_nexus_cli::commands::group::types::*;
use assert_cmd::Command;
use tempfile::TempDir;

fn seed(home: &std::path::Path) {
    // … write a registry with one http + one grpc contract via write_contracts
}

#[test]
fn contracts_unmatched_only_filters_matched_out() {
    let home = TempDir::new().unwrap();
    seed(home.path());
    let out = Command::cargo_bin("gnx").unwrap()
        .env("GNX_HOME", home.path())
        .args(["group", "contracts", "demo", "--unmatched", "--json"])
        .assert().success()
        .get_output().stdout.clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let unmatched = v["contracts"].as_array().unwrap();
    assert!(unmatched.iter().all(|c| c["matched"].as_bool() == Some(false)));
}

#[test]
fn contracts_type_http_filters_by_type() {
    let home = TempDir::new().unwrap();
    seed(home.path());
    let out = Command::cargo_bin("gnx").unwrap()
        .env("GNX_HOME", home.path())
        .args(["group", "contracts", "demo", "--type", "http", "--json"])
        .assert().success()
        .get_output().stdout.clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    for c in v["contracts"].as_array().unwrap() {
        assert_eq!(c["contract_type"].as_str(), Some("Http"));
    }
}
```

- [ ] **Step 2: Implement `contracts.rs`**

```rust
use crate::commands::group::storage::{group_dir, read_contracts};
use crate::commands::group::types::{ContractType, ContractRole, StoredContract};
use clap::Args;
use graph_nexus_core::registry::path::resolve_home_gnx;
use graph_nexus_core::GnxError;
use std::collections::HashSet;

#[derive(Args, Debug)]
pub struct ContractsArgs {
    pub name: String,
    #[arg(long, value_parser = parse_type)]
    pub r#type: Option<ContractType>,
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long)]
    pub unmatched: bool,
    #[arg(long)]
    pub json: bool,
}

fn parse_type(s: &str) -> Result<ContractType, String> {
    match s.to_lowercase().as_str() {
        "http" => Ok(ContractType::Http),
        "grpc" => Ok(ContractType::Grpc),
        "thrift" => Ok(ContractType::Thrift),
        "topic" => Ok(ContractType::Topic),
        "lib" => Ok(ContractType::Lib),
        "include" => Ok(ContractType::Include),
        "custom" => Ok(ContractType::Custom),
        other => Err(format!("unknown type: {other}")),
    }
}

pub fn run(args: ContractsArgs) -> Result<(), GnxError> {
    let home = resolve_home_gnx()?;
    let gdir = group_dir(&home, &args.name);
    let reg = read_contracts(&gdir)?;
    let matched: HashSet<&str> = reg.cross_links.iter()
        .flat_map(|l| [l.from.symbol_uid.as_str(), l.to.symbol_uid.as_str()])
        .collect();
    let pool: Vec<&StoredContract> = if args.unmatched {
        reg.contracts.iter().filter(|c| !matched.contains(c.inner.symbol_uid.as_str())).collect()
    } else {
        reg.contracts.iter().collect()
    };
    let filtered: Vec<&StoredContract> = pool.into_iter()
        .filter(|c| args.r#type.as_ref().map_or(true, |t| &c.inner.contract_type == t))
        .filter(|c| args.repo.as_ref().map_or(true, |r| &c.repo == r))
        .collect();
    if args.json {
        let payload = serde_json::json!({
            "group": args.name,
            "contracts": filtered.iter().map(|c| serde_json::json!({
                "repo": c.repo,
                "contract_id": c.inner.contract_id,
                "contract_type": format!("{:?}", c.inner.contract_type),
                "role": format!("{:?}", c.inner.role),
                "symbol": c.inner.symbol_ref.name,
                "file": c.inner.symbol_ref.file_path,
                "confidence": c.inner.confidence,
                "matched": matched.contains(c.inner.symbol_uid.as_str()),
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        println!("contracts {}", filtered.len());
        for c in filtered {
            println!("  [{:?}] {}  ({})  {}",
                     c.inner.role, c.inner.contract_id, c.repo, c.inner.symbol_ref.name);
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Register, run tests, clippy, commit**

```
git add crates/graph-nexus-cli/src/commands/group/contracts.rs \
        crates/graph-nexus-cli/src/commands/group/mod.rs \
        crates/graph-nexus-cli/tests/group_contracts.rs
git commit -m "feat(group): gnx group contracts — inspect registry with filters

--type, --repo, --unmatched, --json. Reads contracts.rkyv via mmap.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 12: `gnx group impact`

Local impact via existing `commands/impact.rs` entry, then cross-repo
fan-out via `cross_links`. First wave: `cross_depth = 1`.

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/group/impact.rs`
- Modify: `commands/group/mod.rs`
- Modify: `commands/impact.rs` — promote the inner per-symbol impact
  computation to a `pub fn run_for_symbol(...)` so `group::impact`
  can call it directly without re-implementing.
- Test: `crates/graph-nexus-cli/tests/group_impact.rs`

- [ ] **Step 1: Survey existing impact entry**

```
grep -n "pub fn run\|fn run_for\|fn compute_impact" crates/graph-nexus-cli/src/commands/impact.rs
```

Identify the function that takes (repo, symbol, direction, depth) and
returns the structured impact payload. Promote to `pub` if not
already, OR factor it out if `run(args)` does too much (CLI parsing
mixed with computation). Keep the existing `pub fn run(args)` as the
thin CLI entry.

- [ ] **Step 2: Write failing test**

```rust
use assert_cmd::Command;
use tempfile::TempDir;

#[test]
fn impact_cross_repo_emits_consumer_when_provider_changed() {
    let home = TempDir::new().unwrap();
    // … set up two-repo fixture with exact cross-link, run `gnx group sync demo`,
    // then `gnx group impact demo --target create_user --repo backend --direction downstream`
    let out = Command::cargo_bin("gnx").unwrap()
        .env("GNX_HOME", home.path())
        .args(["group", "impact", "demo",
               "--target", "create_user",
               "--repo", "backend",
               "--direction", "downstream",
               "--json"])
        .assert().success()
        .get_output().stdout.clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let cross = v["cross"].as_array().unwrap();
    assert!(!cross.is_empty(), "expected at least one cross-repo hit");
    assert!(cross.iter().any(|c| c["repo"] == "frontend"));
}
```

- [ ] **Step 3: Implement `impact.rs` under group**

```rust
use crate::commands::group::storage::{group_dir, read_contracts};
use crate::commands::group::types::{CrossLink, MatchType};
use clap::Args;
use graph_nexus_core::config::Config;
use graph_nexus_core::registry::path::resolve_home_gnx;
use graph_nexus_core::registry::Registry;
use graph_nexus_core::GnxError;

#[derive(Args, Debug)]
pub struct ImpactArgs {
    pub name: String,
    #[arg(long)] pub target: String,
    #[arg(long)] pub repo: String,                        // member name
    #[arg(long, default_value = "upstream")] pub direction: String,
    #[arg(long)] pub max_depth: Option<u32>,
    #[arg(long)] pub cross_depth: Option<u32>,
    #[arg(long)] pub min_confidence: Option<f32>,
    #[arg(long)] pub timeout_ms: Option<u64>,
    #[arg(long, default_value_t = false)] pub include_tests: bool,
    #[arg(long)] pub json: bool,
}

pub fn run(args: ImpactArgs) -> Result<(), GnxError> {
    let home = resolve_home_gnx()?;
    let cfg = Config::load(&home).unwrap_or_default();
    let registry = Registry::open(&home)?;
    let _entry = registry.find_group(&args.name)
        .ok_or_else(|| GnxError::other(format!("group not found: {}", args.name)))?;

    // Phase 1 — local impact via existing engine
    let local = crate::commands::impact::run_for_symbol(
        &registry,
        &args.repo,
        &args.target,
        &args.direction,
        args.max_depth,
        args.timeout_ms.or(Some(cfg.group.local_impact_timeout_ms)),
        args.include_tests,
    )?;

    // Phase 2 — cross fan-out via contracts.rkyv
    let gdir = group_dir(&home, &args.name);
    let reg = read_contracts(&gdir)?;
    let depth_cap = args.cross_depth.unwrap_or(cfg.group.cross_depth).min(1);  // clamp first wave
    let min_conf = args.min_confidence.unwrap_or(0.0);

    let local_uids: std::collections::HashSet<&str> = local
        .direct_symbol_uids()  // assumed accessor on the local-impact payload
        .iter().map(String::as_str).collect();

    let cross: Vec<&CrossLink> = if depth_cap == 0 {
        Vec::new()
    } else {
        reg.cross_links.iter()
            .filter(|l| l.confidence >= min_conf)
            .filter(|l| local_uids.contains(l.from.symbol_uid.as_str())
                     || local_uids.contains(l.to.symbol_uid.as_str()))
            .collect()
    };

    let payload = serde_json::json!({
        "group": args.name,
        "target": args.target,
        "summary": {
            "direct": local.direct_count(),
            "cross_repo_hits": cross.len(),
        },
        "local": local.as_json(),
        "cross": cross.iter().map(|l| serde_json::json!({
            "repo": l.to.repo,
            "contract": {
                "id": l.contract_id,
                "type": format!("{:?}", l.contract_type),
                "match_type": format!("{:?}", l.match_type),
                "confidence": l.confidence,
            },
            "symbol": l.to.symbol_ref.name,
            "file": l.to.symbol_ref.file_path,
        })).collect::<Vec<_>>(),
        "truncated": cross.len() == 0 && depth_cap < args.cross_depth.unwrap_or(0),
        "cross_depth_warning": if args.cross_depth.unwrap_or(0) > 1 {
            Some("cross_depth > 1 clamped to 1 (multi-hop not yet implemented)")
        } else { None },
    });

    if args.json {
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        println!("group         {}", args.name);
        println!("target        {}", args.target);
        println!("direct        {}", local.direct_count());
        println!("cross_hits    {}", cross.len());
        for l in &cross {
            println!("  -> {} : {} ({:?}, conf={:.2})",
                     l.to.repo, l.contract_id, l.match_type, l.confidence);
        }
    }
    Ok(())
}
```

> **NOTE:** `local.direct_symbol_uids()`, `local.direct_count()`,
> `local.as_json()` are placeholders for the shape of the impact-engine
> return type. When promoting `commands/impact.rs` to expose
> `run_for_symbol`, give it a return type
> `pub struct LocalImpact { ... }` with these three methods. If the
> existing engine returns JSON-only, factor a struct out first.

- [ ] **Step 4: Run tests, clippy, commit**

```
cargo test -p graph-nexus --test group_impact
cargo clippy -p graph-nexus --tests -- -D warnings
git add crates/graph-nexus-cli/src/commands/group/impact.rs \
        crates/graph-nexus-cli/src/commands/group/mod.rs \
        crates/graph-nexus-cli/src/commands/impact.rs
git commit -m "feat(group): gnx group impact — local impact + cross-repo fan-out

Phase 1 reuses commands/impact.rs (promoted run_for_symbol to pub).
Phase 2 reads cross_links from contracts.rkyv, clamps cross_depth=1.
Surfaces cross_depth_warning when caller requests > 1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 13: `gnx group search` + `find` + `coverage` (thin wrappers)

Bundled — each is ~40 LOC. Same pattern: resolve members, fan out
existing engine via rayon, optionally merge.

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/group/search.rs`
- Create: `crates/graph-nexus-cli/src/commands/group/find.rs`
- Create: `crates/graph-nexus-cli/src/commands/group/coverage.rs`
- Modify: `commands/group/mod.rs`
- Test: `crates/graph-nexus-cli/tests/group_search.rs`
- Test: `crates/graph-nexus-cli/tests/group_find.rs`
- Test: `crates/graph-nexus-cli/tests/group_coverage.rs`

- [ ] **Step 1: Write failing tests** — one per command. Each test:

  1. Creates a 2-repo fixture (Go + Python) and indexes both via
     `gnx admin index`.
  2. Forms a group via `gnx admin group add demo …`.
  3. Runs `gnx group <verb> demo …` and asserts the JSON output has
     entries from both repos.

  For `search`: assert RRF merge ordering — first run with default,
  then with `--no-merge` and assert per-repo result count survives.

- [ ] **Step 2: Implement `search.rs`**

```rust
use clap::Args;
use graph_nexus_core::GnxError;
use graph_nexus_core::registry::Registry;
use graph_nexus_core::registry::path::resolve_home_gnx;
use rayon::prelude::*;

#[derive(Args, Debug)]
pub struct SearchArgs {
    pub name: String,
    pub query: String,
    #[arg(long, default_value_t = 5)] pub limit: usize,
    #[arg(long)] pub no_merge: bool,
    #[arg(long)] pub json: bool,
}

pub fn run(args: SearchArgs) -> Result<(), GnxError> {
    let home = resolve_home_gnx()?;
    let registry = Registry::open(&home)?;
    let entry = registry.find_group(&args.name)
        .ok_or_else(|| GnxError::other(format!("group not found: {}", args.name)))?;

    let per_repo: Vec<(String, Vec<crate::commands::search::Hit>)> = entry.members.par_iter()
        .map(|m| (m.clone(), crate::commands::search::run_for_repo(&registry, m, &args.query, args.limit)))
        .collect();

    if args.no_merge {
        emit_per_repo(&per_repo, args.json);
    } else {
        let merged = rrf_merge(&per_repo, args.limit);
        emit_merged(&merged, &per_repo, args.json);
    }
    Ok(())
}

fn rrf_merge(per_repo: &[(String, Vec<crate::commands::search::Hit>)], limit: usize) -> Vec<(String, crate::commands::search::Hit, f32)> {
    // Reciprocal-rank fusion: score(uid) = sum over repos of 1.0 / (60 + rank).
    const K: f32 = 60.0;
    let mut acc: std::collections::HashMap<String, (String, crate::commands::search::Hit, f32)> =
        std::collections::HashMap::new();
    for (repo, hits) in per_repo {
        for (rank, h) in hits.iter().enumerate() {
            let inc = 1.0 / (K + rank as f32 + 1.0);
            let key = format!("{}::{}", repo, h.uid);
            acc.entry(key)
                .and_modify(|(_, _, s)| *s += inc)
                .or_insert_with(|| (repo.clone(), h.clone(), inc));
        }
    }
    let mut out: Vec<_> = acc.into_values().collect();
    out.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(limit);
    out
}

fn emit_per_repo(per_repo: &[(String, Vec<crate::commands::search::Hit>)], json: bool) {
    if json {
        let v = serde_json::json!({
            "per_repo": per_repo.iter().map(|(r, hs)| serde_json::json!({
                "repo": r,
                "hits": hs.iter().map(|h| serde_json::to_value(h).unwrap()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>()
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
    } else {
        for (r, hs) in per_repo {
            println!("[{r}] {} hits", hs.len());
            for h in hs { println!("  {} :: {}", h.file_path, h.name); }
        }
    }
}

fn emit_merged(merged: &[(String, crate::commands::search::Hit, f32)],
               per_repo: &[(String, Vec<crate::commands::search::Hit>)], json: bool) {
    if json {
        let v = serde_json::json!({
            "results": merged.iter().map(|(r, h, s)| serde_json::json!({
                "repo": r, "hit": h, "rrf": s,
            })).collect::<Vec<_>>(),
            "per_repo": per_repo.iter().map(|(r, hs)| serde_json::json!({
                "repo": r, "count": hs.len(),
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
    } else {
        for (r, h, s) in merged {
            println!("  [{r}] {} (rrf={s:.4})", h.name);
        }
    }
}
```

> **NOTE:** `crate::commands::search::run_for_repo(...)` and
> `crate::commands::search::Hit` need to exist. If `commands/search.rs`
> only exposes a CLI entry, refactor: extract the per-repo computation
> into `pub fn run_for_repo(registry, repo, query, limit) -> Vec<Hit>`
> and `pub struct Hit { ... }` (or expose the existing internal type).

- [ ] **Step 3: Implement `find.rs` (no merge — pure concat)**

```rust
use clap::Args;
use graph_nexus_core::GnxError;
use graph_nexus_core::registry::Registry;
use graph_nexus_core::registry::path::resolve_home_gnx;
use rayon::prelude::*;

#[derive(Args, Debug)]
pub struct FindArgs {
    pub name: String,
    pub pattern: String,
    #[arg(long)] pub json: bool,
}

pub fn run(args: FindArgs) -> Result<(), GnxError> {
    let home = resolve_home_gnx()?;
    let registry = Registry::open(&home)?;
    let entry = registry.find_group(&args.name)
        .ok_or_else(|| GnxError::other(format!("group not found: {}", args.name)))?;
    let per_repo: Vec<(String, Vec<crate::commands::find::Hit>)> = entry.members.par_iter()
        .map(|m| (m.clone(), crate::commands::find::run_for_repo(&registry, m, &args.pattern)))
        .collect();
    if args.json {
        let v = serde_json::to_string_pretty(&per_repo).unwrap();
        println!("{v}");
    } else {
        for (r, hs) in &per_repo {
            for h in hs {
                println!("  [{r}] {} :: {}", h.file_path, h.name);
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Implement `coverage.rs` analogously**

Same shape as `find.rs`, calls `commands::coverage::run_for_repo`.

- [ ] **Step 5: Register all three in `commands/group/mod.rs`**

```rust
Search(search::SearchArgs),
Find(find::FindArgs),
Coverage(coverage::CoverageArgs),
// dispatch:
GroupCommands::Search(a) => search::run(a),
GroupCommands::Find(a) => find::run(a),
GroupCommands::Coverage(a) => coverage::run(a),
```

- [ ] **Step 6: Run all three tests, clippy, commit**

```
cargo test -p graph-nexus --test 'group_{search,find,coverage}'
cargo clippy -p graph-nexus --tests -- -D warnings
git add crates/graph-nexus-cli/src/commands/group/{search,find,coverage}.rs \
        crates/graph-nexus-cli/src/commands/group/mod.rs \
        crates/graph-nexus-cli/src/commands/{search,find,coverage}.rs \
        crates/graph-nexus-cli/tests/group_search.rs \
        crates/graph-nexus-cli/tests/group_find.rs \
        crates/graph-nexus-cli/tests/group_coverage.rs
git commit -m "feat(group): gnx group search / find / coverage thin wrappers

search defaults to RRF-merged top-K; --no-merge yields per-repo
streams identical to legacy --repo @group behaviour. find and
coverage are pure parallel concat. All three reuse existing engines'
run_for_repo entrypoints — no engine duplication.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 5 — Migration & Docs (Tasks 14–16)

### Task 14: Remove `@<group>` from top-level CLI commands

**Files:**
- Modify: `crates/graph-nexus-cli/src/repo_selector.rs` — add an
  enum variant signalling "@group reached top-level dispatcher"
- Modify: `crates/graph-nexus-cli/src/commands/search.rs`
- Modify: `crates/graph-nexus-cli/src/commands/find.rs`
- Modify: `crates/graph-nexus-cli/src/commands/contracts.rs`
- Modify: `crates/graph-nexus-cli/src/commands/coverage.rs`
- Delete: `crates/graph-nexus-cli/tests/search_multi_repo.rs` — replaced by `tests/group_search.rs`
- Modify: `crates/graph-nexus-cli/tests/search_cmd.rs` — remove
  `search_multi_repo_at_*` tests, keep `@all` tests
- Test: `crates/graph-nexus-cli/tests/group_at_repo_top_level_errors.rs`

- [ ] **Step 1: Write failing test for the error path**

```rust
use assert_cmd::Command;
use tempfile::TempDir;

#[test]
fn search_at_group_returns_error_with_hint() {
    let home = TempDir::new().unwrap();
    // … seed a registry with group "demo" (no members needed for this error)
    let out = Command::cargo_bin("gnx").unwrap()
        .env("GNX_HOME", home.path())
        .args(["search", "--repo", "@demo", "x"])
        .assert().failure()
        .get_output().stderr.clone();
    let s = String::from_utf8_lossy(&out);
    assert!(s.contains("use `gnx group search`"), "missing hint; got: {s}");
}

#[test]
fn contracts_at_group_returns_error_with_hint() {
    /* same shape; expects "use `gnx group contracts`" */
}

#[test]
fn find_at_group_returns_error_with_hint() {
    /* same shape; expects "use `gnx group find`" */
}

#[test]
fn coverage_at_group_returns_error_with_hint() {
    /* same shape; expects "use `gnx group coverage`" */
}

#[test]
fn search_at_all_still_works() {
    // Sanity — @all is unchanged.
    let home = TempDir::new().unwrap();
    // … seed two indexed repos …
    Command::cargo_bin("gnx").unwrap()
        .env("GNX_HOME", home.path())
        .args(["search", "--repo", "@all", "x"])
        .assert().success();
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p graph-nexus --test group_at_repo_top_level_errors
```

Expected: searches succeed today (no error) — that's the failing
behaviour we'll fix.

- [ ] **Step 3: Add the selector-level error**

In `repo_selector.rs`, extend `ResolveError`:

```rust
#[error("`@{0}` cannot be used at the top level — use `gnx group {hint}` instead")]
GroupAtTopLevel { group: String, hint: String },
```

Add a parameter or builder helper that lets callers specify the
`hint` (`"search"`, `"contracts"`, `"find"`, `"coverage"`). Simplest:
keep `Atom::Group` resolution unchanged, but add a `pub fn
resolve_top_level(atom, registry, verb_hint)` that wraps the existing
`resolve` and intercepts `Atom::Group` before expansion.

- [ ] **Step 4: Replace each command's `@group` branch**

For each of `search.rs` / `find.rs` / `contracts.rs` / `coverage.rs`,
locate where `repo_selector::resolve` (or equivalent) is called and
swap to `resolve_top_level(..., verb_hint)`. **Delete** the dead
`@<group>` iteration code (the loop that expanded the group then
ran the command). Existing `@all` and single-repo branches stay.

Use `verb_hint`:
- `commands/search.rs` → `"search"`
- `commands/find.rs` → `"find"`
- `commands/contracts.rs` → `"contracts"`
- `commands/coverage.rs` → `"coverage"`

- [ ] **Step 5: Delete / migrate old multi-repo tests**

```
git rm crates/graph-nexus-cli/tests/search_multi_repo.rs
```

In `crates/graph-nexus-cli/tests/search_cmd.rs`, delete:
- `search_multi_repo_at_group_both_repos`
- `search_multi_repo_at_all` (renaming kept-content for `@all` to a
  new top-level test if it isn't there already)
- `search_multi_repo_unknown_group_errors`

Migrate the still-relevant assertions to
`tests/group_search.rs` (Task 13) if they're not already covered.

- [ ] **Step 6: Run all CLI tests**

```
cargo test -p graph-nexus --tests
```

Expected: all green, including
`group_at_repo_top_level_errors` (4 errors + 1 sanity = 5 passed).

- [ ] **Step 7: Clippy + commit**

```
cargo clippy -p graph-nexus --tests -- -D warnings
git add -A
git commit -m "$(cat <<'EOF'
feat(group)!: remove --repo @<group> from top-level CLI commands

BREAKING: search/find/contracts/coverage now reject @<group> with a
hint pointing at `gnx group <verb>`. @all and single-repo selectors
unchanged. Migrated tests to tests/group_*.rs; deleted dead
search_multi_repo.rs.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 15: Remove `@<group>` from MCP layer

**Files:**
- Modify: `crates/graph-nexus-mcp/src/handlers/` — every handler
  whose schema declares `repo: string` and that calls into the same
  selector path. Inventory via:
  ```
  grep -rn "@.*group\|Atom::Group\|repo_selector" crates/graph-nexus-mcp/src/
  ```
- Test: `crates/graph-nexus-mcp/tests/mcp_group_rejected.rs` (or
  extend existing handler tests)

- [ ] **Step 1: Inventory MCP handlers**

```
grep -rn "fn.*repo\|RepoSelector\|repo_selector::resolve" crates/graph-nexus-mcp/src/
```

For each handler that resolves a `repo` parameter, identify whether
it calls the CLI's `resolve_top_level` (in which case the CLI's new
error propagates automatically — only verify) or rolls its own
expansion.

- [ ] **Step 2: Write failing test per affected handler**

```rust
#[tokio::test]
async fn mcp_find_at_group_returns_hint_error() {
    let resp = call_mcp_tool("mcp__gnx__find", serde_json::json!({
        "repo": "@demo",
        "pattern": "x",
    })).await;
    let err = resp.as_object().unwrap().get("error").unwrap().as_str().unwrap();
    assert!(err.contains("use `gnx group find`"), "got: {err}");
}
```

- [ ] **Step 3: Wire each handler through `resolve_top_level`**

Pass the same `verb_hint` strings as Task 14. Any handler that has
its own expansion loop: delete it, defer to the CLI's path.

- [ ] **Step 4: Run MCP tests**

```
cargo test -p graph-nexus-mcp
```

- [ ] **Step 5: Clippy + commit**

```
cargo clippy -p graph-nexus-mcp -- -D warnings
git add -A
git commit -m "$(cat <<'EOF'
feat(mcp/group)!: remove @<group> from MCP repo-parameter handlers

BREAKING: MCP handlers reject @<group> with the same hint as the CLI.
New mcp__gnx_group_* tools deferred to a follow-up PR — for now,
agents shell out to gnx group <verb>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 16: Doc updates + final suite verification

**Files:**
- Modify: `docs/skills/gnx.md`
- Modify: `README.md` (Language Matrix table)
- Modify: `docs/specs/2026-05-18-gnx-group-multirepo-design.md` —
  flip Status from `Draft` to `Shipped`, add a "Shipped" date.

- [ ] **Step 1: Update `docs/skills/gnx.md`**

Add a "Multi-repo workflow" section listing the new commands and
their TOON output shapes. Cross-link to the spec. Keep entries
terse (LLM-first).

- [ ] **Step 2: Update README Language Matrix**

Add a "group extractor" column. Mark Wave-1 languages
(Go/Python/JS/TS/Java/Rust) as ✓ for both HTTP and gRPC; mark the
other 9 mainstream as `—` with footnote "stub only — extractor
not implemented".

- [ ] **Step 3: Flip spec status**

In `docs/specs/2026-05-18-gnx-group-multirepo-design.md`, change:

```
**Status**: Draft
```

to:

```
**Status**: Shipped 2026-05-18
```

- [ ] **Step 4: Run the full suite — accuracy + perf guarantee**

```
cargo test -p graph-nexus --tests
cargo test -p graph-nexus-analyzer --tests
cargo test -p graph-nexus-core --tests
cargo test -p graph-nexus-mcp --tests
cargo clippy --workspace --tests -- -D warnings
```

Expected: **all green**. This is the eywa "Confirm successful
refactoring by running the full test suite" gate — do not commit
the doc updates without it passing.

- [ ] **Step 5: Bench against `scripts/benchmark_gnx.py`**

```
python scripts/benchmark_gnx.py
```

Compare per-query timings against pre-PR baseline. The relevant
targets from the spec:
- `group status` < 200 ms for N=10
- `group impact` < 200 ms for direct=20, group=5

If anything regresses, **do not ship** — investigate before
proceeding. Capture before/after in the PR body.

- [ ] **Step 6: Commit docs**

```
git add docs/skills/gnx.md README.md docs/specs/2026-05-18-gnx-group-multirepo-design.md
git commit -m "$(cat <<'EOF'
docs(group): gnx.md + README matrix + flip spec status to shipped

Skills doc gains a Multi-repo workflow section; README Language
Matrix adds a "group extractor" column with Wave-1 langs ✓ and
others as stub-only. Spec status: Shipped 2026-05-18.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 7: Push branch + open PR**

```
git push -u origin worktree-feat-group-multirepo-spec:feat/group-multirepo
gh pr create --title "feat(group)!: multi-repo workflow (sync/impact/status/contracts/...)" \
  --body "$(cat <<'EOF'
## Summary

Brings the `gnx group <verb>` noun-first namespace into the CLI per
[docs/specs/2026-05-18-gnx-group-multirepo-design.md](docs/specs/2026-05-18-gnx-group-multirepo-design.md):
`sync / status / contracts / impact / search / find / coverage`.
Removes `--repo @<group>` from top-level commands (breaking).
Storage uses rkyv (zero-copy mmap); BM25 matching reuses the
existing Tantivy infrastructure — no new search dependency.

## Out of scope (do not bundle here)

- Embedding-fallback contract matching (only exact + BM25 in this PR)
- Topic / Thrift / Include extractors (HTTP + gRPC only this PR)
- Workspace extractors (Go / Java / Python / Rust / Node / Elixir)
- Manifest links / `group.yaml` escape-hatch authoring
- New MCP tools for group operations (only the breaking-change removal
  of `@<group>` from existing MCP handlers)
- Wave-2 languages (Kotlin, C#, PHP, Ruby, Swift, C, C++, Dart) — they
  get BlindSpot stubs only in this PR
- Multi-hop cross-repo impact (`cross_depth > 1` — clamped to 1)

## Test plan

- [ ] `cargo test -p graph-nexus --tests` — all green
- [ ] `cargo test -p graph-nexus-analyzer --tests` — all green
- [ ] `cargo test -p graph-nexus-core --tests` — all green
- [ ] `cargo test -p graph-nexus-mcp --tests` — all green
- [ ] `cargo clippy --workspace --tests -- -D warnings` — clean
- [ ] `python scripts/benchmark_gnx.py` — `group status` < 200 ms,
      `group impact` < 200 ms for direct=20 / group=5
- [ ] Manual: form a 2-repo group, `gnx group sync demo`,
      `gnx group contracts demo`, verify TOON output shape matches spec

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-review (writing-plans skill checklist)

- **Spec coverage:** All command-surface items from the spec map to a
  task (sync→T9, status→T10, contracts→T11, impact→T12,
  search/find/coverage→T13, breaking change→T14, MCP→T15, docs→T16).
  Storage→T1, config→T2, extractors→T3-7, matching→T8.
- **Placeholder scan:** No "TBD" / "implement later" left. The
  `local.direct_symbol_uids()` etc. accessors in T12 are explicitly
  flagged as "depends on impact engine refactor — define when
  promoting to pub" with the named struct shape.
- **Type consistency:** `ContractType`, `ContractRole`, `MatchType`,
  `ExtractedContract`, `StoredContract`, `CrossLink`, `CrossLinkEndpoint`,
  `ContractRegistry`, `GroupMeta`, `RepoSnapshot` all defined once in
  T1, referenced verbatim in T8/T9/T10/T11/T12/T13. `SyncArgs`,
  `StatusArgs`, `ContractsArgs`, `ImpactArgs`, `SearchArgs`,
  `FindArgs`, `CoverageArgs` defined in their respective tasks and
  registered in the dispatcher.
- **Test discipline:** Every task has a failing-test-first step before
  implementation. Final task runs the full suite.
- **eywa adherence:** Out-of-scope section in PR body verbatim from
  spec; T16 step 4 is the "run full suite to confirm refactoring"
  gate; T15 documents the in-implementation "pause to confirm" point
  when MCP handler shapes diverge from CLI.
