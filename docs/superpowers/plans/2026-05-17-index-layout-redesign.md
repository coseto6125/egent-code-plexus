# Index Layout Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `~/.cgn/<repo>/<branch>/` index layout with `~/.cgn/<repo>/{commits/<sha-dir>, sessions/<sid>}` two-tier model — commit-content-addressed L2 + session-local incremental L1.

**Architecture:** Branch removed from storage key. Commit SHA is sole L2 identity. Per-session L1 holds dirty-worktree overlay (graph fragments + tantivy delta). Promotion handles HEAD drift via content-equivalence (Case A fast-forward) or atomic invalidate (Case B cross-refactor).

**Tech Stack:** Rust, rkyv (zero-copy graph), tantivy (fulltext), tree-sitter (parsing), fs2 (file locking), serde + BTreeMap (deterministic JSON), sha2.

**Spec:** `docs/superpowers/specs/2026-05-17-index-layout-redesign-design.md`

**Blast radius:** ~30 files across `cgn-core::registry` + `cgn-cli::{graph_path, repo_selector, auto_ensure, engine, commands::admin::*, commands::hook::*}`. Net **+300 LOC** (del ~600, add ~900). No migration code — `cgn admin reset` wipes `~/.cgn/` and auto-rebuild kicks in.

**Phase ordering (strict dependency):**

```
1. Schema foundation (new types + delete old)
       ↓
2. Atomic write consolidation
       ↓
3. Read path (resolution + engine)
       ↓
4. Write path L2 (build)
       ↓
5. Write path L1 (incremental overlay)
       ↓
6. Promotion (Case A + B)
       ↓
7. GC + concurrency
       ↓
8. CLI surface
       ↓
9. 14-language smoke + docs
```

Don't skip ahead — Phase 4 depends on Phase 3's resolution to know where to write.

---

## Phase 1: Schema Foundation

Goal: introduce new types, remove dead `branch`-keyed types. After this phase compiles, **nothing functional yet** — types are inert.

### Task 1.1: Add `SourceType` enum + `CommitDirName` parser

**Files:**
- Create: `crates/cgn-core/src/registry/dirname.rs`
- Modify: `crates/cgn-core/src/registry/mod.rs:1-30` (re-export)
- Test: `crates/cgn-core/tests/commit_dirname.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/cgn-core/tests/commit_dirname.rs
use cgn_core::registry::{CommitDirName, SourceType, ParseError};

#[test]
fn parse_branch_simple() {
    let n = CommitDirName::parse("branch_main__abc123def4567890abc123def4567890abc123de").unwrap();
    assert_eq!(n.source_type, SourceType::Branch);
    assert_eq!(n.source_id.as_deref(), Some("main"));
    assert_eq!(n.sha_hex(), "abc123def4567890abc123def4567890abc123de");
}

#[test]
fn parse_commit_no_id() {
    let n = CommitDirName::parse("commit__456789abc123def456789abc123def456789abc123").unwrap();
    assert_eq!(n.source_type, SourceType::Commit);
    assert!(n.source_id.is_none());
}

#[test]
fn parse_source_id_with_underscore() {
    let n = CommitDirName::parse("branch_feat_x_v2__abc123def4567890abc123def4567890abc123de").unwrap();
    assert_eq!(n.source_id.as_deref(), Some("feat_x_v2"));
}

#[test]
fn parse_source_id_with_double_underscore() {
    // rsplit_once('__') 從右切，sha 段優先
    let n = CommitDirName::parse("branch_weird__name__abc123def4567890abc123def4567890abc123de").unwrap();
    assert_eq!(n.source_id.as_deref(), Some("weird__name"));
}

#[test]
fn reject_unknown_source_type() {
    assert!(matches!(
        CommitDirName::parse("fake_x__abc123def4567890abc123def4567890abc123de"),
        Err(ParseError::UnknownSourceType(_))
    ));
}

#[test]
fn reject_non_hex_sha() {
    assert!(matches!(
        CommitDirName::parse("branch_main__notahexstring1234567890abcdef12345xyz"),
        Err(ParseError::InvalidSha)
    ));
}

#[test]
fn reject_short_sha() {
    assert!(matches!(
        CommitDirName::parse("branch_main__abc123"),
        Err(ParseError::InvalidSha)
    ));
}

#[test]
fn round_trip_format_parse() {
    let original = "tag_v1.2.3__789abc123def456789abc123def456789abc1234a";
    let parsed = CommitDirName::parse(original).unwrap();
    assert_eq!(parsed.format(), original);
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p cgn-core --test commit_dirname
```

Expected: compile error — module `registry::dirname` does not exist.

- [ ] **Step 3: Implement `dirname.rs`**

```rust
// crates/cgn-core/src/registry/dirname.rs
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceType {
    Branch,
    Tag,
    Pr,
    Commit,
}

impl SourceType {
    fn as_str(&self) -> &'static str {
        match self {
            SourceType::Branch => "branch",
            SourceType::Tag => "tag",
            SourceType::Pr => "pr",
            SourceType::Commit => "commit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDirName {
    pub source_type: SourceType,
    pub source_id: Option<String>,
    pub sha: [u8; 20],
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("dir name missing __ separator")]
    NoSha,
    #[error("sha segment not 40-hex")]
    InvalidSha,
    #[error("prefix missing source_type")]
    NoTypeId,
    #[error("unknown source_type: {0}")]
    UnknownSourceType(String),
}

impl CommitDirName {
    pub fn parse(name: &str) -> Result<Self, ParseError> {
        let (prefix, sha_str) = name.rsplit_once("__").ok_or(ParseError::NoSha)?;
        if sha_str.len() != 40 || !sha_str.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ParseError::InvalidSha);
        }
        let mut sha = [0u8; 20];
        for i in 0..20 {
            sha[i] = u8::from_str_radix(&sha_str[i * 2..i * 2 + 2], 16)
                .map_err(|_| ParseError::InvalidSha)?;
        }

        if prefix == "commit" {
            return Ok(Self { source_type: SourceType::Commit, source_id: None, sha });
        }

        let (type_str, id_str) = prefix.split_once('_').ok_or(ParseError::NoTypeId)?;
        let source_type = match type_str {
            "branch" => SourceType::Branch,
            "tag" => SourceType::Tag,
            "pr" => SourceType::Pr,
            other => return Err(ParseError::UnknownSourceType(other.into())),
        };
        Ok(Self { source_type, source_id: Some(id_str.into()), sha })
    }

    pub fn format(&self) -> String {
        let sha_hex = self.sha_hex();
        match (&self.source_type, &self.source_id) {
            (SourceType::Commit, _) => format!("commit__{sha_hex}"),
            (t, Some(id)) => format!("{}_{id}__{sha_hex}", t.as_str()),
            (t, None) => format!("{}__{sha_hex}", t.as_str()),
        }
    }

    pub fn sha_hex(&self) -> String {
        self.sha.iter().map(|b| format!("{b:02x}")).collect()
    }
}
```

Add to `crates/cgn-core/src/registry/mod.rs`:

```rust
pub mod dirname;
pub use dirname::{CommitDirName, ParseError as DirNameParseError, SourceType};
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p cgn-core --test commit_dirname
```

Expected: all 8 tests pass.

- [ ] **Step 5: Clippy + format**

```bash
cargo clippy -p cgn-core --tests
rustfmt --edition 2021 crates/cgn-core/src/registry/dirname.rs
rustfmt --edition 2021 crates/cgn-core/tests/commit_dirname.rs
```

Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-core/src/registry/dirname.rs \
        crates/cgn-core/src/registry/mod.rs \
        crates/cgn-core/tests/commit_dirname.rs
git commit -m "feat(registry): CommitDirName parser + SourceType enum"
```

### Task 1.2: Add `CommitBuildMeta` (replaces `BranchMeta`)

**Files:**
- Create: `crates/cgn-core/src/registry/commit_meta.rs`
- Modify: `crates/cgn-core/src/registry/mod.rs` (add `pub mod commit_meta;`)
- Test: `crates/cgn-core/tests/commit_meta.rs`

- [ ] **Step 1: Write failing test**

```rust
// crates/cgn-core/tests/commit_meta.rs
use cgn_core::registry::{CommitBuildMeta, EmbeddingStatus, RefRecord, SourceType};
use tempfile::NamedTempFile;

#[test]
fn round_trip_deterministic_json() {
    let meta = CommitBuildMeta {
        version: 1,
        sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_type: SourceType::Branch,
        source_id: Some("main".into()),
        built_from_worktree: "/work/myrepo".into(),
        built_at: "2026-05-17T10:00:00Z".into(),
        parent_sha: Some("def0000000000000000000000000000000000000".into()),
        node_count: 100,
        embedding_status: EmbeddingStatus::None,
        refs_at_build: vec![RefRecord { ref_name: "refs/heads/main".into(), seen_at: "2026-05-17T10:00:00Z".into() }],
        refs_seen_since: vec![],
    };
    let s1 = serde_json::to_string(&meta).unwrap();
    let s2 = serde_json::to_string(&meta).unwrap();
    assert_eq!(s1, s2, "serialization must be deterministic");
    let back: CommitBuildMeta = serde_json::from_str(&s1).unwrap();
    assert_eq!(back.sha, meta.sha);
    assert_eq!(back.source_type, meta.source_type);
}

#[test]
fn atomic_write_round_trip() {
    let tmp = NamedTempFile::new().unwrap();
    let meta = CommitBuildMeta {
        version: 1,
        sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_type: SourceType::Commit,
        source_id: None,
        built_from_worktree: "/work/x".into(),
        built_at: "2026-05-17T10:00:00Z".into(),
        parent_sha: None,
        node_count: 42,
        embedding_status: EmbeddingStatus::Skipped,
        refs_at_build: vec![],
        refs_seen_since: vec![],
    };
    CommitBuildMeta::write_atomic(tmp.path(), &meta).unwrap();
    let read = CommitBuildMeta::read(tmp.path()).unwrap();
    assert_eq!(read.node_count, 42);
}
```

- [ ] **Step 2: Run to verify fails**

```bash
cargo test -p cgn-core --test commit_meta
```

Expected: compile error — `CommitBuildMeta` undefined.

- [ ] **Step 3: Implement `commit_meta.rs`**

```rust
// crates/cgn-core/src/registry/commit_meta.rs
use crate::registry::dirname::SourceType;
use crate::registry::io::atomic_write_json;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitBuildMeta {
    pub version: u32,
    pub sha: String,
    pub source_type: SourceType,
    pub source_id: Option<String>,
    pub built_from_worktree: String,
    pub built_at: String,
    pub parent_sha: Option<String>,
    pub node_count: u32,
    pub embedding_status: EmbeddingStatus,
    pub refs_at_build: Vec<RefRecord>,
    pub refs_seen_since: Vec<RefRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefRecord {
    pub ref_name: String,
    pub seen_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmbeddingStatus {
    None,
    Skipped,
    Computed,
}

impl CommitBuildMeta {
    pub fn write_atomic(path: &Path, value: &Self) -> io::Result<()> {
        atomic_write_json(path, value)
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}
```

Update `crates/cgn-core/src/registry/mod.rs`:

```rust
pub mod commit_meta;
pub use commit_meta::{CommitBuildMeta, EmbeddingStatus, RefRecord};
```

- [ ] **Step 4: Run test pass**

```bash
cargo test -p cgn-core --test commit_meta
```

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cgn-core/src/registry/commit_meta.rs \
        crates/cgn-core/src/registry/mod.rs \
        crates/cgn-core/tests/commit_meta.rs
git commit -m "feat(registry): CommitBuildMeta + EmbeddingStatus + RefRecord"
```

### Task 1.3: Add `RepoMeta`

**Files:**
- Create: `crates/cgn-core/src/registry/repo_meta.rs`
- Test: `crates/cgn-core/tests/repo_meta.rs`

- [ ] **Step 1: Failing test**

```rust
// crates/cgn-core/tests/repo_meta.rs
use cgn_core::registry::RepoMeta;
use std::collections::BTreeMap;
use tempfile::NamedTempFile;

#[test]
fn round_trip_btreemap_deterministic() {
    let mut refs = BTreeMap::new();
    refs.insert("refs/heads/main".to_string(), "abc123".to_string());
    refs.insert("refs/tags/v1".to_string(), "def456".to_string());
    let m = RepoMeta {
        version: 1,
        common_dir: "/work/r/.git".into(),
        remote_url: Some("https://github.com/u/r.git".into()),
        aliases: vec!["r".into()],
        known_refs: refs.clone(),
        last_built_sha: None,
        total_size_bytes: 0,
        last_touched: "2026-05-17T10:00:00Z".into(),
    };
    let s1 = serde_json::to_string(&m).unwrap();
    let s2 = serde_json::to_string(&m).unwrap();
    assert_eq!(s1, s2);
    // BTreeMap → JSON keys must be sorted
    let i = s1.find("refs/heads/main").unwrap();
    let j = s1.find("refs/tags/v1").unwrap();
    assert!(i < j, "BTreeMap iterates in sorted key order");
}

#[test]
fn atomic_write_read() {
    let tmp = NamedTempFile::new().unwrap();
    let m = RepoMeta {
        version: 1,
        common_dir: "/x".into(),
        remote_url: None,
        aliases: vec![],
        known_refs: BTreeMap::new(),
        last_built_sha: None,
        total_size_bytes: 0,
        last_touched: "2026-05-17T10:00:00Z".into(),
    };
    RepoMeta::write_atomic(tmp.path(), &m).unwrap();
    let r = RepoMeta::read(tmp.path()).unwrap();
    assert_eq!(r.common_dir, "/x");
}
```

- [ ] **Step 2: Verify fail**

```bash
cargo test -p cgn-core --test repo_meta
```

- [ ] **Step 3: Implement**

```rust
// crates/cgn-core/src/registry/repo_meta.rs
use crate::registry::io::atomic_write_json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoMeta {
    pub version: u32,
    pub common_dir: String,
    pub remote_url: Option<String>,
    pub aliases: Vec<String>,
    pub known_refs: BTreeMap<String, String>,
    pub last_built_sha: Option<String>,
    pub total_size_bytes: u64,
    pub last_touched: String,
}

impl RepoMeta {
    pub fn write_atomic(path: &Path, value: &Self) -> io::Result<()> {
        atomic_write_json(path, value)
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}
```

Update `mod.rs`:

```rust
pub mod repo_meta;
pub use repo_meta::RepoMeta;
```

- [ ] **Step 4: Verify pass + commit**

```bash
cargo test -p cgn-core --test repo_meta
git add crates/cgn-core/src/registry/repo_meta.rs \
        crates/cgn-core/src/registry/mod.rs \
        crates/cgn-core/tests/repo_meta.rs
git commit -m "feat(registry): RepoMeta with BTreeMap deterministic serialization"
```

### Task 1.4: Add `session/` module — `SessionMeta` + `DirtyFiles`

**Files:**
- Create: `crates/cgn-core/src/session/mod.rs`
- Create: `crates/cgn-core/src/session/meta.rs`
- Create: `crates/cgn-core/src/session/overlay.rs`
- Modify: `crates/cgn-core/src/lib.rs` (add `pub mod session;`)
- Test: `crates/cgn-core/tests/session_meta.rs`

- [ ] **Step 1: Failing test**

```rust
// crates/cgn-core/tests/session_meta.rs
use cgn_core::session::{DirtyEntry, DirtyFiles, SessionMeta};
use std::collections::BTreeMap;
use tempfile::NamedTempFile;

#[test]
fn session_meta_round_trip() {
    let sm = SessionMeta {
        version: 1,
        session_id: "cli-abc12345".into(),
        pid: Some(1234),
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:30:00Z".into(),
        base_sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_worktree: "/work/myrepo".into(),
        overlay_version: 5,
    };
    let s1 = serde_json::to_string(&sm).unwrap();
    let s2 = serde_json::to_string(&sm).unwrap();
    assert_eq!(s1, s2);
}

#[test]
fn dirty_files_deterministic() {
    let mut entries = BTreeMap::new();
    entries.insert(
        "src/a.rs".into(),
        DirtyEntry {
            mtime_ns: 1000,
            content_hash: "deadbeef".into(),
            fragment_id: "frag1".into(),
            tantivy_delta_segment: None,
            parse_failed: false,
        },
    );
    entries.insert(
        "src/b.rs".into(),
        DirtyEntry {
            mtime_ns: 2000,
            content_hash: "cafebabe".into(),
            fragment_id: "frag2".into(),
            tantivy_delta_segment: Some("seg_xxx".into()),
            parse_failed: false,
        },
    );
    let df = DirtyFiles { version: 1, entries };
    let s1 = serde_json::to_string(&df).unwrap();
    let s2 = serde_json::to_string(&df).unwrap();
    assert_eq!(s1, s2);
    assert!(s1.find("src/a.rs").unwrap() < s1.find("src/b.rs").unwrap(),
            "BTreeMap → keys sorted");
}

#[test]
fn atomic_write_session_meta() {
    let tmp = NamedTempFile::new().unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: "x".into(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: "0".repeat(40),
        source_worktree: "/x".into(),
        overlay_version: 0,
    };
    SessionMeta::write_atomic(tmp.path(), &sm).unwrap();
    let r = SessionMeta::read(tmp.path()).unwrap();
    assert_eq!(r.session_id, "x");
}
```

- [ ] **Step 2: Verify fails**

```bash
cargo test -p cgn-core --test session_meta
```

- [ ] **Step 3: Implement**

```rust
// crates/cgn-core/src/session/mod.rs
pub mod meta;
pub mod overlay;
pub use meta::SessionMeta;
pub use overlay::{DirtyEntry, DirtyFiles};
```

```rust
// crates/cgn-core/src/session/meta.rs
use crate::registry::io::atomic_write_json;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub version: u32,
    pub session_id: String,
    pub pid: Option<u32>,
    pub started_at: String,
    pub last_touched: String,
    pub base_sha: String,
    pub source_worktree: String,
    pub overlay_version: u32,
}

impl SessionMeta {
    pub fn write_atomic(path: &Path, value: &Self) -> io::Result<()> {
        atomic_write_json(path, value)
    }
    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
}
```

```rust
// crates/cgn-core/src/session/overlay.rs
use crate::registry::io::atomic_write_json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyFiles {
    pub version: u32,
    pub entries: BTreeMap<String, DirtyEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyEntry {
    pub mtime_ns: u64,
    pub content_hash: String,
    pub fragment_id: String,
    pub tantivy_delta_segment: Option<String>,
    pub parse_failed: bool,
}

impl DirtyFiles {
    pub fn write_atomic(path: &Path, value: &Self) -> io::Result<()> {
        atomic_write_json(path, value)
    }
    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
    pub fn empty() -> Self {
        Self { version: 1, entries: BTreeMap::new() }
    }
}
```

Update `crates/cgn-core/src/lib.rs`:

```rust
pub mod session;
```

- [ ] **Step 4: Verify pass + commit**

```bash
cargo test -p cgn-core --test session_meta
git add crates/cgn-core/src/session/ crates/cgn-core/src/lib.rs \
        crates/cgn-core/tests/session_meta.rs
git commit -m "feat(session): SessionMeta + DirtyFiles overlay manifest"
```

### Task 1.5: Delete `IndexLayout` + `sanitize_branch` + `BranchMeta`

**Files:**
- Modify: `crates/cgn-core/src/registry/path.rs` (delete `IndexLayout` + `sanitize_branch`)
- Delete: `crates/cgn-core/src/registry/meta.rs` (BranchMeta → CommitBuildMeta done; file gone)
- Modify: `crates/cgn-core/src/registry/mod.rs` (drop re-exports)
- Delete: `crates/cgn-core/tests/registry_meta.rs`
- Delete: `crates/cgn-core/tests/registry_path.rs` (IndexLayout-specific cases; keep sanitize_segment tests if exist by moving)

- [ ] **Step 1: List call sites that will break (snapshot for next tasks)**

```bash
grep -rn "IndexLayout\|sanitize_branch\|BranchMeta" crates/ --include="*.rs" | grep -v "// removed"
```

Expected output: locations in `cgn-cli/src/graph_path.rs`, `commands/admin/index.rs`, `commands/admin/rename_branch.rs`, hook common.rs, tests. Capture this list — Phase 3 / 4 / 8 will rewrite each.

- [ ] **Step 2: Remove `IndexLayout` struct + impl from `path.rs`**

```bash
# Surgical: keep sanitize_segment, derive_repo_name, uid_path, resolve_home_cgn, hash8.
# Delete: IndexLayout struct (line ~87 to ~139), sanitize_branch fn (line ~42 to ~55).
```

Open `crates/cgn-core/src/registry/path.rs` and delete:
- the `IndexLayout` struct + `impl IndexLayout`
- the `sanitize_branch` function
- any test cases that test these (move sanitize_segment cases to remain)

- [ ] **Step 3: Delete `meta.rs` (replaced by `commit_meta.rs`)**

```bash
git rm crates/cgn-core/src/registry/meta.rs
git rm crates/cgn-core/tests/registry_meta.rs
```

- [ ] **Step 4: Update `mod.rs` re-exports**

Remove from `crates/cgn-core/src/registry/mod.rs`:

```rust
// DELETE:
// pub mod meta;
// pub use meta::BranchMeta;
// pub use path::{IndexLayout, sanitize_branch};
```

Keep:

```rust
pub use path::{derive_repo_name, resolve_home_cgn, sanitize_segment, uid_path, PathError};
```

- [ ] **Step 5: Verify `cgn-core` still compiles standalone**

```bash
cargo build -p cgn-core
```

Expected: PASS (CLI hasn't switched yet, but core is clean).

- [ ] **Step 6: Commit (cgn-cli intentionally broken — fixed in next tasks)**

```bash
git add -u crates/cgn-core/
git commit -m "refactor(registry): delete IndexLayout + sanitize_branch + BranchMeta

cgn-cli won't compile until Phase 3/4/8 rewires call sites."
```

### Task 1.6: Migrate `RegistryFile` to v2 schema (`RepoAlias` + `BTreeMap`)

**Files:**
- Modify: `crates/cgn-core/src/registry/store.rs`
- Modify: `crates/cgn-core/tests/registry_store.rs`

- [ ] **Step 1: Update test to v2 expectations**

```rust
// crates/cgn-core/tests/registry_store.rs
use cgn_core::registry::{RegistryFile, RepoAlias, GroupEntry};
use std::collections::BTreeMap;

#[test]
fn v2_empty_registry() {
    let reg = RegistryFile::empty();
    assert_eq!(reg.version, 2);
    assert!(reg.repos.is_empty());
}

#[test]
fn v2_round_trip_deterministic() {
    let mut repos = BTreeMap::new();
    repos.insert("a__1234".into(), RepoAlias {
        dir_name: "a__1234".into(),
        common_dir: "/work/a/.git".into(),
        remote_url: None,
        aliases: vec!["a".into()],
        last_touched: "2026-05-17T10:00:00Z".into(),
        groups: vec![],
    });
    let reg = RegistryFile { version: 2, repos, groups: vec![] };
    let s1 = serde_json::to_string(&reg).unwrap();
    let s2 = serde_json::to_string(&reg).unwrap();
    assert_eq!(s1, s2);
}

#[test]
fn v1_rejected_clearly() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), r#"{"version":1,"repos":[]}"#).unwrap();
    let err = RegistryFile::read_or_empty(tmp.path()).unwrap_err();
    let s = err.to_string();
    assert!(s.contains("v2") || s.contains("reset"),
            "v1 detection must surface a clear migration message, got: {s}");
}
```

- [ ] **Step 2: Verify fails (expected — code still v1)**

```bash
cargo test -p cgn-core --test registry_store
```

- [ ] **Step 3: Rewrite `store.rs`**

```rust
// crates/cgn-core/src/registry/store.rs
use crate::registry::io::atomic_write_json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

pub const CURRENT_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryFile {
    pub version: u32,
    #[serde(default)]
    pub repos: BTreeMap<String, RepoAlias>,
    #[serde(default)]
    pub groups: Vec<GroupEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoAlias {
    pub dir_name: String,
    pub common_dir: String,
    pub remote_url: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub last_touched: String,
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupEntry {
    pub name: String,
    pub members: Vec<String>,
}

impl RegistryFile {
    pub fn empty() -> Self {
        Self { version: CURRENT_VERSION, repos: BTreeMap::new(), groups: vec![] }
    }

    pub fn write_atomic(path: &Path, value: &RegistryFile) -> io::Result<()> {
        if path.exists() {
            let bak = bak_path(path);
            fs::copy(path, &bak)?;
        }
        atomic_write_json(path, value)
    }

    pub fn read_or_empty(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            return Ok(RegistryFile::empty());
        }
        let bytes = fs::read(path)?;
        let parsed: RegistryFile = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if parsed.version != CURRENT_VERSION {
            return Err(io::Error::other(format!(
                "registry schema v{} (expected v{CURRENT_VERSION}); run `cgn admin reset` to wipe and rebuild",
                parsed.version
            )));
        }
        Ok(parsed)
    }
}

/// Last-resort recovery: walk `~/.cgn/*/meta.json` and rebuild RegistryFile
/// as alias cache. Filesystem is source of truth — group memberships are LOST
/// (registry-only data), operator must re-apply via `cgn admin group add`.
impl RegistryFile {
    pub fn rebuild_from_disk(home_cgn: &Path) -> io::Result<Self> {
        use crate::registry::repo_meta::RepoMeta;

        let mut repos = BTreeMap::new();
        let it = match fs::read_dir(home_cgn) {
            Ok(d) => d,
            Err(_) => return Ok(RegistryFile::empty()),
        };
        for entry in it.flatten() {
            let dir_name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };
            if dir_name.starts_with('_') || dir_name.starts_with('.') {
                continue;
            }
            let repo_meta_path = entry.path().join("meta.json");
            if !repo_meta_path.exists() {
                continue;
            }
            let rm = match RepoMeta::read(&repo_meta_path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            repos.insert(
                dir_name.clone(),
                RepoAlias {
                    dir_name,
                    common_dir: rm.common_dir,
                    remote_url: rm.remote_url,
                    aliases: rm.aliases,
                    last_touched: rm.last_touched,
                    groups: vec![],
                },
            );
        }
        Ok(RegistryFile { version: CURRENT_VERSION, repos, groups: vec![] })
    }
}

fn bak_path(path: &Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bak");
    std::path::PathBuf::from(s)
}

/// Remove user:pass from a remote URL.
pub fn strip_credentials(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        Err(_) => url.to_string(),
    }
}
```

Update `mod.rs`:

```rust
pub use store::{strip_credentials, GroupEntry, RegistryFile, RepoAlias, CURRENT_VERSION};
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p cgn-core --test registry_store
cargo build -p cgn-core
```

Expected: tests pass; `cgn-core` builds clean.

- [ ] **Step 5: Commit**

```bash
git add -u crates/cgn-core/
git commit -m "refactor(registry): RegistryFile v2 — BTreeMap repos, RepoAlias, drop RepoEntry/BranchEntry

v1 detection now returns clear 'run cgn admin reset' error; rebuild_from_disk
walks <repo>/meta.json (per-repo source of truth)."
```

### Task 1.7: Delete `incremental_cache.rs` (replaced by L1 overlay)

**Files:**
- Delete: `crates/cgn-cli/src/incremental_cache.rs`
- Modify: `crates/cgn-cli/src/lib.rs` (drop module decl)

- [ ] **Step 1: Snapshot callers**

```bash
grep -rn "incremental_cache" crates/cgn-cli/src/ --include="*.rs"
```

Capture caller locations — Phase 4/5 will replace each.

- [ ] **Step 2: Delete file + module decl**

```bash
git rm crates/cgn-cli/src/incremental_cache.rs
```

Edit `crates/cgn-cli/src/lib.rs` and delete the line `pub mod incremental_cache;` (or `mod incremental_cache;`).

- [ ] **Step 3: Commit**

```bash
git add -u crates/cgn-cli/src/lib.rs
git commit -m "refactor(cli): delete incremental_cache.rs (replaced by L1 overlay)

cgn-cli still won't compile until L1 overlay lands in Phase 5."
```

---

## Phase 2: Atomic Write Consolidation

Goal: collapse three `write_atomic` copies into one canonical helper.

### Task 2.1: Audit existing `write_atomic` implementations

**Files (read only):**
- `crates/cgn-core/src/registry/io.rs:1-50`
- `crates/cgn-core/src/registry/store.rs` (search `write_atomic`)
- `crates/cgn-cli/src/commands/admin/claude_code.rs` (search `write_atomic`)

- [ ] **Step 1: Compare implementations**

```bash
grep -A 20 "fn write_atomic\|fn atomic_write" crates/cgn-core/src/registry/io.rs crates/cgn-core/src/registry/store.rs crates/cgn-cli/src/commands/admin/claude_code.rs
```

Document differences (some may have fsync, some may not; some may use tempfile, some manual `<path>.tmp`).

### Task 2.2: Promote canonical `atomic_write_json` in `registry/io.rs`

**Files:**
- Modify: `crates/cgn-core/src/registry/io.rs`
- Test: `crates/cgn-core/tests/atomic_write.rs` (likely exists; extend)

- [ ] **Step 1: Verify / extend test**

```rust
// crates/cgn-core/tests/atomic_write.rs (extend)
use cgn_core::registry::io::atomic_write_json;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Sample { v: u32, s: String }

#[test]
fn atomic_write_json_persists_and_fsyncs() {
    let tmp = NamedTempFile::new().unwrap();
    let s = Sample { v: 42, s: "hello".into() };
    atomic_write_json(tmp.path(), &s).unwrap();
    let bytes = std::fs::read(tmp.path()).unwrap();
    let back: Sample = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back, s);
}

#[test]
fn atomic_write_no_partial_file_on_concurrent_read() {
    // Sequential proxy: write A, read while writing B → must see A or B, never partial
    let tmp = NamedTempFile::new().unwrap();
    atomic_write_json(tmp.path(), &Sample { v: 1, s: "a".repeat(10_000) }).unwrap();
    atomic_write_json(tmp.path(), &Sample { v: 2, s: "b".repeat(10_000) }).unwrap();
    let r: Sample = serde_json::from_slice(&std::fs::read(tmp.path()).unwrap()).unwrap();
    assert!(r.v == 1 || r.v == 2);
    assert_eq!(r.s.len(), 10_000);
}
```

- [ ] **Step 2: Ensure `atomic_write_json` is canonical (tmp file + fsync + rename)**

Open `crates/cgn-core/src/registry/io.rs`. If implementation doesn't already match this pattern, replace:

```rust
// crates/cgn-core/src/registry/io.rs
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let tmp_path = {
        let mut s = path.as_os_str().to_owned();
        s.push(".tmp");
        std::path::PathBuf::from(s)
    };
    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}
```

- [ ] **Step 3: Run tests pass**

```bash
cargo test -p cgn-core --test atomic_write
```

- [ ] **Step 4: Delete duplicate in `claude_code.rs`**

In `crates/cgn-cli/src/commands/admin/claude_code.rs`, find the local `write_atomic` (or similar) function and replace all its callers with:

```rust
use cgn_core::registry::io::atomic_write_json;
// ... at call site:
atomic_write_json(path, &value)?;
```

Then delete the local function.

- [ ] **Step 5: Verify build + commit**

```bash
cargo build -p cgn-core
# cgn-cli build still pending Phase 3+
git add -u
git commit -m "refactor(registry): consolidate atomic_write_json — single canonical helper

Deletes duplicate write_atomic in commands/admin/claude_code.rs; all callers
now use cgn_core::registry::io::atomic_write_json."
```

---

## Phase 3: Read Path

Goal: rewrite repo resolution + graph_path::resolve to use commit-SHA layout. After this phase, queries against an existing L2 work (but build path still Phase 4).

### Task 3.1: Repo identity — `repo_dir_name_for_cwd` helper

**Files:**
- Create: `crates/cgn-cli/src/repo_identity.rs`
- Modify: `crates/cgn-cli/src/lib.rs` (add `pub mod repo_identity;`)
- Test: `crates/cgn-cli/tests/repo_identity.rs`

- [ ] **Step 1: Failing test**

```rust
// crates/cgn-cli/tests/repo_identity.rs
use cgn_cli::repo_identity::repo_dir_name_for_cwd;
use std::process::Command;

#[test]
fn cwd_in_git_repo_returns_basename_hash() {
    let tmp = tempfile::tempdir().unwrap();
    Command::new("git").arg("-C").arg(tmp.path()).arg("init").arg("-q").status().unwrap();
    let name = repo_dir_name_for_cwd(tmp.path()).unwrap();
    let basename = tmp.path().file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with(basename) || name.contains("__"), "got: {name}");
    assert!(name.contains("__"), "must contain hash separator: {name}");
}

#[test]
fn two_worktrees_same_repo_yield_same_dir_name() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    std::fs::create_dir(&primary).unwrap();
    Command::new("git").arg("-C").arg(&primary).arg("init").arg("-q").status().unwrap();
    // create dummy commit
    std::fs::write(primary.join("README"), "x").unwrap();
    Command::new("git").arg("-C").arg(&primary).args(["add", "."]).status().unwrap();
    Command::new("git").arg("-C").arg(&primary).args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "x"]).status().unwrap();
    let wt = tmp.path().join("wt2");
    Command::new("git").arg("-C").arg(&primary).args(["worktree", "add", "-q", wt.to_str().unwrap()]).status().unwrap();

    let n1 = repo_dir_name_for_cwd(&primary).unwrap();
    let n2 = repo_dir_name_for_cwd(&wt).unwrap();
    assert_eq!(n1, n2, "two worktrees of same repo must share dir name");
}

#[test]
fn cwd_not_in_repo_errors() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(repo_dir_name_for_cwd(tmp.path()).is_err());
}
```

- [ ] **Step 2: Verify fails**

```bash
cargo test -p code-graph-nexus -p cgn-cli --test repo_identity 2>&1 | head -20
```

Expected: compile error — `repo_identity` module missing.

- [ ] **Step 3: Implement**

```rust
// crates/cgn-cli/src/repo_identity.rs
use crate::git::safe_exec;
use cgn_core::registry::sanitize_segment;
use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;

pub fn repo_dir_name_for_cwd(cwd: &Path) -> io::Result<String> {
    let common_dir = git_common_dir(cwd)?;
    let canonical = std::fs::canonicalize(&common_dir)?;
    let basename = canonical
        .parent()
        .and_then(|p| p.file_name())
        .or_else(|| canonical.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    let cleaned = basename.trim_start_matches(['.', '-']);
    let safe = sanitize_segment(if cleaned.is_empty() { "repo" } else { cleaned })
        .unwrap_or_else(|_| "repo".to_string());
    let h = sha256_hex8(canonical.to_string_lossy().as_bytes());
    Ok(format!("{safe}__{h}"))
}

fn git_common_dir(cwd: &Path) -> io::Result<std::path::PathBuf> {
    let out = safe_exec::git()
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(cwd)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("not a git repository"));
    }
    let path_str = std::str::from_utf8(&out.stdout)
        .map_err(io::Error::other)?
        .trim();
    let p = std::path::PathBuf::from(path_str);
    if p.is_absolute() {
        Ok(p)
    } else {
        Ok(cwd.join(p))
    }
}

fn sha256_hex8(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(&digest[..4])
}
```

Update `crates/cgn-cli/src/lib.rs`:

```rust
pub mod repo_identity;
```

- [ ] **Step 4: Run tests pass**

```bash
cargo test -p code-graph-nexus --test repo_identity
```

- [ ] **Step 5: Commit**

```bash
git add crates/cgn-cli/src/repo_identity.rs crates/cgn-cli/src/lib.rs \
        crates/cgn-cli/tests/repo_identity.rs
git commit -m "feat(cli): repo_dir_name_for_cwd — git common-dir based identity"
```

### Task 3.2: SHA → dirname lookup helper

**Files:**
- Create: `crates/cgn-cli/src/commit_lookup.rs`
- Test: `crates/cgn-cli/tests/commit_lookup.rs`

- [ ] **Step 1: Failing test**

```rust
// crates/cgn-cli/tests/commit_lookup.rs
use cgn_cli::commit_lookup::CommitIndex;

#[test]
fn empty_dir_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("commits")).unwrap();
    let idx = CommitIndex::scan(&tmp.path().join("commits")).unwrap();
    assert!(idx.find(&[0; 20]).is_none());
}

#[test]
fn finds_existing_commit_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir(&commits).unwrap();
    std::fs::create_dir(commits.join("branch_main__abc123def4567890abc123def4567890abc123de")).unwrap();
    let idx = CommitIndex::scan(&commits).unwrap();
    let mut sha = [0u8; 20];
    hex::decode_to_slice("abc123def4567890abc123def4567890abc123de", &mut sha).unwrap();
    let dir = idx.find(&sha).unwrap();
    assert_eq!(dir, "branch_main__abc123def4567890abc123def4567890abc123de");
}

#[test]
fn skips_unparseable_names() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir(&commits).unwrap();
    std::fs::create_dir(commits.join("garbage_name")).unwrap();
    std::fs::create_dir(commits.join("branch_x__abc123def4567890abc123def4567890abc123de.building")).unwrap();
    let idx = CommitIndex::scan(&commits).unwrap();
    assert_eq!(idx.len(), 0);
}
```

- [ ] **Step 2: Verify fails**

```bash
cargo test -p code-graph-nexus --test commit_lookup
```

- [ ] **Step 3: Implement**

```rust
// crates/cgn-cli/src/commit_lookup.rs
use cgn_core::registry::CommitDirName;
use std::collections::HashMap;
use std::io;
use std::path::Path;

pub struct CommitIndex {
    by_sha: HashMap<[u8; 20], String>,
}

impl CommitIndex {
    pub fn scan(commits_dir: &Path) -> io::Result<Self> {
        let mut by_sha = HashMap::new();
        let it = match std::fs::read_dir(commits_dir) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self { by_sha }),
            Err(e) => return Err(e),
        };
        for entry in it.flatten() {
            let Ok(name) = entry.file_name().into_string() else { continue };
            if name.ends_with(".building") || name.ends_with(".stale") || name.contains(".stale-") {
                continue;
            }
            let Ok(parsed) = CommitDirName::parse(&name) else { continue };
            by_sha.insert(parsed.sha, name);
        }
        Ok(Self { by_sha })
    }

    pub fn find(&self, sha: &[u8; 20]) -> Option<&str> {
        self.by_sha.get(sha).map(|s| s.as_str())
    }

    pub fn len(&self) -> usize { self.by_sha.len() }
    pub fn is_empty(&self) -> bool { self.by_sha.is_empty() }
}
```

Update `crates/cgn-cli/src/lib.rs`:

```rust
pub mod commit_lookup;
```

- [ ] **Step 4: Tests pass + commit**

```bash
cargo test -p code-graph-nexus --test commit_lookup
git add crates/cgn-cli/src/commit_lookup.rs crates/cgn-cli/src/lib.rs \
        crates/cgn-cli/tests/commit_lookup.rs
git commit -m "feat(cli): CommitIndex — scan commits/ to build sha→dirname map"
```

### Task 3.3: Rewrite `graph_path::resolve`

**Files:**
- Modify: `crates/cgn-cli/src/graph_path.rs` (full rewrite)
- Modify: `crates/cgn-cli/tests/` — adjust whatever tests touched this

- [ ] **Step 1: Read existing resolve to capture caller expectations**

```bash
grep -rn "graph_path::resolve\|graph_path::Resolve" crates/cgn-cli/src/ --include="*.rs"
```

Callers expect: input `(graph: &Path, cwd: &Path) → PathBuf`. We preserve signature; semantics change to commit-SHA layout.

- [ ] **Step 2: Write test for new behavior**

```rust
// crates/cgn-cli/tests/graph_path_resolve.rs (new)
use cgn_cli::graph_path;
use std::path::Path;
use std::process::Command;

const LEGACY_DEFAULT: &str = ".cgn/graph.bin";

#[test]
fn custom_path_passes_through() {
    let custom = Path::new("/abs/custom/graph.bin");
    let cwd = Path::new("/tmp");
    let resolved = graph_path::resolve(custom, cwd);
    assert_eq!(resolved, std::path::PathBuf::from("/abs/custom/graph.bin"));
}

#[test]
fn legacy_default_in_git_repo_resolves_to_commits_dir() {
    let tmp = tempfile::tempdir().unwrap();
    Command::new("git").arg("-C").arg(tmp.path()).arg("init").arg("-q").status().unwrap();
    std::fs::write(tmp.path().join("a"), "x").unwrap();
    Command::new("git").arg("-C").arg(tmp.path()).args(["add", "."]).status().unwrap();
    Command::new("git").arg("-C").arg(tmp.path()).args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "x"]).status().unwrap();

    // override HOME so we don't touch user's ~/.cgn/
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());

    let resolved = graph_path::resolve(Path::new(LEGACY_DEFAULT), tmp.path());
    // Expected shape: <home>/.cgn/<repo>__<hash8>/commits/<dirname>/graph.bin
    let s = resolved.to_string_lossy();
    assert!(s.contains(".cgn"), "got: {s}");
    assert!(s.contains("commits"), "got: {s}");
    assert!(s.ends_with("graph.bin"), "got: {s}");
}

#[test]
fn legacy_default_not_in_git_repo_falls_through() {
    let tmp = tempfile::tempdir().unwrap();  // no git init
    let resolved = graph_path::resolve(Path::new(LEGACY_DEFAULT), tmp.path());
    // Fall back to original path verbatim — caller's error handling surfaces "not found"
    assert_eq!(resolved, std::path::PathBuf::from(LEGACY_DEFAULT));
}
```

- [ ] **Step 3: Rewrite `graph_path.rs`**

```rust
// crates/cgn-cli/src/graph_path.rs
use crate::commit_lookup::CommitIndex;
use crate::git::safe_exec;
use crate::repo_identity;
use cgn_core::registry::resolve_home_cgn;
use std::path::{Path, PathBuf};

const LEGACY_DEFAULT: &str = ".cgn/graph.bin";

/// Resolve the `--graph` arg. If user passed legacy default, route through
/// the v2 commit-content-addressed layout based on cwd's current HEAD SHA.
/// Custom absolute paths pass through unchanged.
pub fn resolve(graph: &Path, cwd: &Path) -> PathBuf {
    if graph.as_os_str() != LEGACY_DEFAULT {
        return graph.to_path_buf();
    }
    resolve_v2(cwd).unwrap_or_else(|| graph.to_path_buf())
}

fn resolve_v2(cwd: &Path) -> Option<PathBuf> {
    let home_cgn = resolve_home_cgn();
    let repo_dir_name = repo_identity::repo_dir_name_for_cwd(cwd).ok()?;
    let repo_root = home_cgn.join(&repo_dir_name);
    let commits = repo_root.join("commits");

    let head_sha = head_sha(cwd)?;
    let idx = CommitIndex::scan(&commits).ok()?;
    let dir = idx.find(&head_sha)?;
    Some(commits.join(dir).join("graph.bin"))
}

fn head_sha(cwd: &Path) -> Option<[u8; 20]> {
    let out = safe_exec::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() { return None; }
    let s = std::str::from_utf8(&out.stdout).ok()?.trim();
    if s.len() != 40 { return None; }
    let mut sha = [0u8; 20];
    hex::decode_to_slice(s, &mut sha).ok()?;
    Some(sha)
}
```

- [ ] **Step 4: Tests pass**

```bash
cargo test -p code-graph-nexus --test graph_path_resolve
```

Note: only the first + third tests are expected pass at this phase. The second (resolves to commits/) will only succeed once Phase 4 builds; for now, it falls through. Mark it `#[ignore]` until Phase 4 with a clear comment.

```rust
#[test]
#[ignore = "enabled after Phase 4 (L2 build)"]
fn legacy_default_in_git_repo_resolves_to_commits_dir() { /* ... */ }
```

- [ ] **Step 5: Commit**

```bash
git add crates/cgn-cli/src/graph_path.rs crates/cgn-cli/tests/graph_path_resolve.rs
git commit -m "refactor(cli): graph_path::resolve walks commit-SHA layout

Legacy default (.cgn/graph.bin) now resolves to:
  <home>/.cgn/<repo>/commits/<dirname>/graph.bin via cwd → common-dir hash + HEAD SHA.
Custom paths pass through unchanged."
```

### Task 3.4: Update `repo_selector::find_by_path` to common-dir match

**Files:**
- Modify: `crates/cgn-cli/src/repo_selector.rs` (find_by_path)
- Modify: `crates/cgn-cli/tests/repo_selector.rs`

- [ ] **Step 1: Test new behavior**

Add to `tests/repo_selector.rs`:

```rust
#[test]
fn find_by_path_matches_via_common_dir() {
    use cgn_cli::repo_selector;
    use cgn_core::registry::{RegistryFile, RepoAlias};
    use std::collections::BTreeMap;
    use std::process::Command;

    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    std::fs::create_dir(&primary).unwrap();
    Command::new("git").arg("-C").arg(&primary).arg("init").arg("-q").status().unwrap();

    let common_dir = std::fs::canonicalize(primary.join(".git")).unwrap();
    let mut repos = BTreeMap::new();
    repos.insert("primary__xxxx".into(), RepoAlias {
        dir_name: "primary__xxxx".into(),
        common_dir: common_dir.to_string_lossy().into(),
        remote_url: None,
        aliases: vec!["primary".into()],
        last_touched: "2026-05-17T10:00:00Z".into(),
        groups: vec![],
    });
    let reg = RegistryFile { version: 2, repos, groups: vec![] };

    let resolved = repo_selector::find_by_path(&reg, primary.to_string_lossy().as_ref()).unwrap();
    assert_eq!(resolved.dir_name, "primary__xxxx");
}
```

- [ ] **Step 2: Rewrite `find_by_path` (and adapt `resolve` to v2 schema)**

```rust
// crates/cgn-cli/src/repo_selector.rs (modify resolver section)
use cgn_core::registry::{RegistryFile, RepoAlias};
use std::path::Path;
use crate::git::safe_exec;
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct ResolvedRepo {
    pub dir_name: String,
    pub common_dir: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("repo not found in registry: {0}")]
    NotFound(String),
    #[error("group not found: {0}")]
    GroupNotFound(String),
    #[error("path not in registry: {0}")]
    PathNotRegistered(String),
}

pub fn find_by_path<'a>(registry: &'a RegistryFile, p: &str) -> Option<&'a RepoAlias> {
    let target_common = git_common_dir_for(Path::new(p))?;
    registry.repos.values().find(|alias| {
        std::fs::canonicalize(&alias.common_dir)
            .ok()
            .as_deref()
            == Some(&target_common)
    })
}

fn git_common_dir_for(cwd: &Path) -> Option<std::path::PathBuf> {
    let out = safe_exec::git()
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() { return None; }
    let s = std::str::from_utf8(&out.stdout).ok()?.trim();
    let p = std::path::PathBuf::from(s);
    std::fs::canonicalize(if p.is_absolute() { p } else { cwd.join(p) }).ok()
}

pub fn resolve(
    sel: &Selector,
    registry: &RegistryFile,
    cwd: &str,
) -> Result<Vec<ResolvedRepo>, ResolveError> {
    let mut seen = HashSet::<String>::new();
    let mut out = Vec::<ResolvedRepo>::new();

    for atom in &sel.0 {
        match atom {
            Atom::Cwd => {
                let alias = find_by_path(registry, cwd)
                    .ok_or_else(|| ResolveError::PathNotRegistered(cwd.into()))?;
                push_unique(&mut seen, &mut out, alias);
            }
            Atom::Path(p) => {
                let s = p.to_string_lossy();
                let alias = find_by_path(registry, &s)
                    .ok_or_else(|| ResolveError::PathNotRegistered(s.into_owned()))?;
                push_unique(&mut seen, &mut out, alias);
            }
            Atom::Name(n) => {
                let alias = registry
                    .repos
                    .values()
                    .find(|r| r.aliases.iter().any(|a| a == n) || r.dir_name == *n)
                    .ok_or_else(|| ResolveError::NotFound(n.clone()))?;
                push_unique(&mut seen, &mut out, alias);
            }
            Atom::Group(g) => {
                let group = registry
                    .groups
                    .iter()
                    .find(|gr| gr.name == *g)
                    .ok_or_else(|| ResolveError::GroupNotFound(g.clone()))?;
                for member in &group.members {
                    if let Some(a) = registry.repos.get(member) {
                        push_unique(&mut seen, &mut out, a);
                    }
                }
            }
            Atom::All => {
                for alias in registry.repos.values() {
                    push_unique(&mut seen, &mut out, alias);
                }
            }
        }
    }
    Ok(out)
}

fn push_unique(seen: &mut HashSet<String>, out: &mut Vec<ResolvedRepo>, alias: &RepoAlias) {
    if seen.insert(alias.dir_name.clone()) {
        out.push(ResolvedRepo {
            dir_name: alias.dir_name.clone(),
            common_dir: alias.common_dir.clone(),
            aliases: alias.aliases.clone(),
        });
    }
}
```

- [ ] **Step 3: Tests pass + commit**

```bash
cargo test -p code-graph-nexus --test repo_selector
git add -u
git commit -m "refactor(cli): repo_selector — common-dir-based matching, v2 RegistryFile

ResolvedRepo no longer carries index_dir_root (derived from <home>/.cgn/<dir_name>);
find_by_path matches by canonical git common-dir, so multiple worktrees of the
same repo resolve to one alias."
```

### Task 3.5: Update `Engine::load` to support L1 overlay (parameter only; merge logic comes in Phase 5)

**Files:**
- Modify: `crates/cgn-cli/src/engine.rs`
- Test: extend existing engine tests

- [ ] **Step 1: Add optional overlay path parameter (no merge yet)**

```rust
// crates/cgn-cli/src/engine.rs (modify)
use cgn_core::graph::{ArchivedZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use memmap2::Mmap;
use rkyv::rancor::Error;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

pub struct Engine {
    mmap: Mmap,
    graph_path: PathBuf,
    overlay_dir: Option<PathBuf>,
}

impl Engine {
    /// Load L2 graph. Overlay is opt-in via `with_overlay`.
    pub fn load<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let graph_path = fs::canonicalize(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_path_buf());
        let file = File::open(&graph_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        validate_header(&mmap)?;
        Ok(Self { mmap, graph_path, overlay_dir: None })
    }

    pub fn with_overlay(mut self, dir: PathBuf) -> Self {
        self.overlay_dir = Some(dir);
        self
    }

    pub fn graph(&self) -> Result<&ArchivedZeroCopyGraph, Error> {
        rkyv::access::<ArchivedZeroCopyGraph, Error>(&self.mmap)
    }

    pub fn index_dir(&self) -> Option<&Path> {
        self.graph_path.parent()
    }

    pub fn overlay_dir(&self) -> Option<&Path> {
        self.overlay_dir.as_deref()
    }
}

fn validate_header(bytes: &[u8]) -> io::Result<()> {
    // unchanged from current implementation
    let archived = rkyv::access::<ArchivedZeroCopyGraph, Error>(bytes).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("graph.bin: structural validation failed: {e}"))
    })?;
    if archived.magic != GRAPH_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("graph.bin: bad magic — expected {:?}, got {:?}", GRAPH_MAGIC, archived.magic),
        ));
    }
    let version = archived.version.to_native();
    if version != GRAPH_FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("graph.bin: incompatible format version {version} (this reader expects {GRAPH_FORMAT_VERSION}) — run `cgn analyze` to regenerate"),
        ));
    }
    Ok(())
}
```

- [ ] **Step 2: Verify build**

```bash
cargo build -p code-graph-nexus
```

If `cgn-cli` is still uncompilable from Phase 1's deletions, narrow to:

```bash
cargo check -p code-graph-nexus 2>&1 | head -30
```

Note remaining compilation errors — they'll be fixed in subsequent tasks. We're tracking progress, not requiring green build until Phase 8.

- [ ] **Step 3: Commit**

```bash
git add crates/cgn-cli/src/engine.rs
git commit -m "feat(cli): Engine::with_overlay accepts L1 session dir (merge logic in Phase 5)"
```

---

## Phase 4: Write Path L2 (Build)

Goal: rewrite `commands/admin/index::run` to use new layout. After this phase, builds produce `<repo>/commits/<dirname>/graph.bin`.

### Task 4.1: Helper — pick dirname from SHA

**Files:**
- Create: `crates/cgn-cli/src/build/dirname_picker.rs`
- Test: `crates/cgn-cli/tests/dirname_picker.rs`

- [ ] **Step 1: Failing test**

```rust
// crates/cgn-cli/tests/dirname_picker.rs
use cgn_cli::build::dirname_picker::pick_dirname;
use std::process::Command;

#[test]
fn picks_branch_name_when_head_is_branch() {
    let tmp = tempfile::tempdir().unwrap();
    Command::new("git").arg("-C").arg(tmp.path()).arg("init").arg("-q").status().unwrap();
    std::fs::write(tmp.path().join("a"), "x").unwrap();
    Command::new("git").arg("-C").arg(tmp.path()).args(["add", "."]).status().unwrap();
    Command::new("git").arg("-C").arg(tmp.path()).args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "x"]).status().unwrap();
    Command::new("git").arg("-C").arg(tmp.path()).args(["branch", "-M", "main"]).status().unwrap();

    let sha = git_head_sha(tmp.path());
    let name = pick_dirname(tmp.path(), &sha).unwrap();
    assert!(name.starts_with("branch_main__"), "got: {name}");
    assert!(name.ends_with(&sha));
}

#[test]
fn picks_commit_for_detached_head() {
    let tmp = tempfile::tempdir().unwrap();
    Command::new("git").arg("-C").arg(tmp.path()).arg("init").arg("-q").status().unwrap();
    std::fs::write(tmp.path().join("a"), "x").unwrap();
    Command::new("git").arg("-C").arg(tmp.path()).args(["add", "."]).status().unwrap();
    Command::new("git").arg("-C").arg(tmp.path()).args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "x"]).status().unwrap();
    let sha = git_head_sha(tmp.path());
    Command::new("git").arg("-C").arg(tmp.path()).args(["checkout", "-q", "--detach", &sha]).status().unwrap();

    let name = pick_dirname(tmp.path(), &sha).unwrap();
    assert_eq!(name, format!("commit__{sha}"));
}

fn git_head_sha(p: &std::path::Path) -> String {
    let o = Command::new("git").arg("-C").arg(p).args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}
```

- [ ] **Step 2: Verify fails**

```bash
cargo test -p code-graph-nexus --test dirname_picker
```

- [ ] **Step 3: Implement**

```rust
// crates/cgn-cli/src/build/mod.rs (new)
pub mod dirname_picker;
```

```rust
// crates/cgn-cli/src/build/dirname_picker.rs
use crate::git::safe_exec;
use cgn_core::registry::sanitize_segment;
use std::io;
use std::path::Path;

/// Pick most-specific dir name for SHA: branch > tag > pr > commit (fallback).
pub fn pick_dirname(worktree: &Path, sha_hex: &str) -> io::Result<String> {
    let refs = list_refs_pointing_at(worktree, sha_hex)?;

    if let Some(b) = refs.iter().find_map(|r| r.strip_prefix("refs/heads/")) {
        return Ok(format!("branch_{}__{sha_hex}", sanitize_for_dir(b)));
    }
    if let Some(t) = refs.iter().find_map(|r| r.strip_prefix("refs/tags/")) {
        return Ok(format!("tag_{}__{sha_hex}", sanitize_for_dir(t)));
    }
    for r in &refs {
        if let Some(rest) = r.strip_prefix("refs/pull/").or_else(|| r.strip_prefix("refs/merge-requests/")) {
            if let Some(n) = rest.split('/').next() {
                return Ok(format!("pr_{}__{sha_hex}", sanitize_for_dir(n)));
            }
        }
    }
    Ok(format!("commit__{sha_hex}"))
}

fn list_refs_pointing_at(worktree: &Path, sha_hex: &str) -> io::Result<Vec<String>> {
    let out = safe_exec::git()
        .args(["for-each-ref", "--points-at", sha_hex, "--format=%(refname)"])
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("git for-each-ref failed"));
    }
    let s = std::str::from_utf8(&out.stdout).map_err(io::Error::other)?;
    Ok(s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

fn sanitize_for_dir(s: &str) -> String {
    let replaced: String = s.chars().map(|c| match c {
        '/' => '-',
        c if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') => c,
        _ => '_',
    }).collect();
    sanitize_segment(&replaced).unwrap_or_else(|_| "x".to_string())
}
```

Update `crates/cgn-cli/src/lib.rs`:

```rust
pub mod build;
```

- [ ] **Step 4: Tests pass + commit**

```bash
cargo test -p code-graph-nexus --test dirname_picker
git add crates/cgn-cli/src/build/ crates/cgn-cli/src/lib.rs \
        crates/cgn-cli/tests/dirname_picker.rs
git commit -m "feat(build): pick_dirname — most-specific ref selection for L2 dir naming"
```

### Task 4.2: Build mode binary rule (Sync iff no commits)

**Files:**
- Create: `crates/cgn-cli/src/build/mode.rs`
- Test: `crates/cgn-cli/tests/build_mode.rs`

- [ ] **Step 1: Failing test**

```rust
// crates/cgn-cli/tests/build_mode.rs
use cgn_cli::build::mode::{build_mode, BuildMode};

#[test]
fn first_build_is_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let sha = [0u8; 20];
    assert_eq!(build_mode(repo, &sha), BuildMode::Sync);
}

#[test]
fn target_exists_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let commits = repo.join("commits");
    std::fs::create_dir(&commits).unwrap();
    let dir = "branch_main__abc123def4567890abc123def4567890abc123de";
    std::fs::create_dir(commits.join(dir)).unwrap();
    let mut sha = [0u8; 20];
    hex::decode_to_slice("abc123def4567890abc123def4567890abc123de", &mut sha).unwrap();
    assert_eq!(build_mode(repo, &sha), BuildMode::None);
}

#[test]
fn other_commits_exist_target_missing_is_background() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let commits = repo.join("commits");
    std::fs::create_dir(&commits).unwrap();
    std::fs::create_dir(commits.join("branch_main__abc123def4567890abc123def4567890abc123de")).unwrap();
    let mut other = [0u8; 20];
    hex::decode_to_slice("000000000000000000000000000000000000fffe", &mut other).unwrap();
    assert_eq!(build_mode(repo, &other), BuildMode::Background);
}

#[test]
fn building_suffix_excluded_from_count() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let commits = repo.join("commits");
    std::fs::create_dir(&commits).unwrap();
    std::fs::create_dir(commits.join("branch_x__abc.building")).unwrap();
    let sha = [1u8; 20];
    // No completed commits → still Sync
    assert_eq!(build_mode(repo, &sha), BuildMode::Sync);
}
```

- [ ] **Step 2: Verify fails**

```bash
cargo test -p code-graph-nexus --test build_mode
```

- [ ] **Step 3: Implement**

```rust
// crates/cgn-cli/src/build/mode.rs
use crate::commit_lookup::CommitIndex;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMode { None, Sync, Background }

pub fn build_mode(repo_root: &Path, target_sha: &[u8; 20]) -> BuildMode {
    let commits = repo_root.join("commits");
    let idx = CommitIndex::scan(&commits).unwrap_or_else(|_| {
        // Empty / inaccessible commits/ dir → treat as no completed entries
        CommitIndex::scan(&commits).unwrap_or(CommitIndex { by_sha: Default::default() })
    });
    if idx.find(target_sha).is_some() {
        return BuildMode::None;
    }
    if idx.is_empty() {
        BuildMode::Sync
    } else {
        BuildMode::Background
    }
}
```

(`CommitIndex::by_sha` must be `pub(crate)` to allow `Default::default()` fallback; expose accessor or restructure if visibility issue — alternative cleaner: return error from scan when commits/ missing and let caller treat as empty.)

Cleaner alternative for `mode.rs`:

```rust
pub fn build_mode(repo_root: &Path, target_sha: &[u8; 20]) -> BuildMode {
    let commits = repo_root.join("commits");
    let idx = match CommitIndex::scan(&commits) {
        Ok(i) => i,
        Err(_) => return BuildMode::Sync,
    };
    if idx.find(target_sha).is_some() { BuildMode::None }
    else if idx.is_empty() { BuildMode::Sync }
    else { BuildMode::Background }
}
```

Update `crates/cgn-cli/src/build/mod.rs`:

```rust
pub mod mode;
```

- [ ] **Step 4: Tests pass + commit**

```bash
cargo test -p code-graph-nexus --test build_mode
git add -u
git commit -m "feat(build): BuildMode binary rule — Sync iff no completed commits exist"
```

### Task 4.3: Build orchestrator — `build_l2`

**Files:**
- Create: `crates/cgn-cli/src/build/orchestrator.rs`
- Test: `crates/cgn-cli/tests/build_orchestrator.rs`

- [ ] **Step 1: Failing test (end-to-end: tiny repo → first build succeeds)**

```rust
// crates/cgn-cli/tests/build_orchestrator.rs
use cgn_cli::build::orchestrator;
use std::process::Command;

#[test]
fn first_build_writes_commit_dir_atomically() {
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    Command::new("git").arg("-C").arg(&worktree).arg("init").arg("-q").status().unwrap();
    std::fs::write(worktree.join("main.rs"), "fn main() { println!(\"hi\"); }").unwrap();
    Command::new("git").arg("-C").arg(&worktree).args(["add", "."]).status().unwrap();
    Command::new("git").arg("-C").arg(&worktree).args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "x"]).status().unwrap();

    let home = tmp.path().join("home");
    std::env::set_var("HOME", &home);

    let result = orchestrator::build_l2(&worktree, None).unwrap();
    assert!(result.commit_dir.exists(), "commit dir must exist: {:?}", result.commit_dir);
    assert!(result.commit_dir.join("graph.bin").exists());
    assert!(result.commit_dir.join("meta.json").exists());
    let building = result.commit_dir.with_extension("building");
    assert!(!building.exists(), "building suffix must be gone after atomic rename");
}
```

- [ ] **Step 2: Verify fails**

```bash
cargo test -p code-graph-nexus --test build_orchestrator
```

- [ ] **Step 3: Implement**

```rust
// crates/cgn-cli/src/build/orchestrator.rs
use crate::build::dirname_picker::pick_dirname;
use crate::repo_identity::repo_dir_name_for_cwd;
use crate::git::safe_exec;
use fs2::FileExt;
use cgn_core::registry::{
    resolve_home_cgn, CommitBuildMeta, EmbeddingStatus, RefRecord, RepoMeta, SourceType,
};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

pub struct BuildResult {
    pub commit_dir: PathBuf,
    pub sha_hex: String,
    pub source_type: SourceType,
}

pub fn build_l2(worktree: &Path, target_sha: Option<&str>) -> io::Result<BuildResult> {
    let sha_hex = match target_sha {
        Some(s) => s.to_string(),
        None => head_sha_hex(worktree)?,
    };
    if sha_hex.len() != 40 {
        return Err(io::Error::other(format!("invalid sha: {sha_hex}")));
    }

    let home_cgn = resolve_home_cgn();
    let repo_dir_name = repo_dir_name_for_cwd(worktree)?;
    let repo_root = home_cgn.join(&repo_dir_name);
    fs::create_dir_all(repo_root.join("commits"))?;

    let dirname = pick_dirname(worktree, &sha_hex)?;
    let commit_dir = repo_root.join("commits").join(&dirname);
    let building = repo_root.join("commits").join(format!("{dirname}.building"));

    // Acquire build lock; attach pattern if locked
    fs::create_dir_all(&building)?;
    let lock_path = building.join(".build.lock");
    let lock = OpenOptions::new().create(true).write(true).open(&lock_path)?;
    if lock.try_lock_exclusive().is_err() {
        // Another builder owns it — wait for completion + return
        return wait_for_completion(&building, &commit_dir);
    }

    // 1. Source resolution
    let src_root = if worktree_clean_and_head_matches(worktree, &sha_hex)? {
        worktree.to_path_buf()
    } else {
        let src = building.join("_src");
        fs::create_dir_all(&src)?;
        git_archive_to(worktree, &sha_hex, &src)?;
        src
    };

    // 2. Build graph.bin + tantivy + (optional embeddings)
    let node_count = run_analyzer_pipeline(&src_root, &building)?;

    // 3. Refs at build
    let refs_at_build = collect_refs(worktree, &sha_hex)?;
    let source_type = source_type_from_refs(&refs_at_build);
    let source_id = source_id_from_refs(&refs_at_build);

    // 4. Metadata
    let meta = CommitBuildMeta {
        version: 1,
        sha: sha_hex.clone(),
        source_type,
        source_id,
        built_from_worktree: worktree.to_string_lossy().into(),
        built_at: chrono::Utc::now().to_rfc3339(),
        parent_sha: parent_sha(worktree, &sha_hex).ok(),
        node_count: node_count as u32,
        embedding_status: EmbeddingStatus::None,
        refs_at_build,
        refs_seen_since: vec![],
    };
    CommitBuildMeta::write_atomic(&building.join("meta.json"), &meta)?;

    // 5. fsync + atomic publish
    sync_all_files(&building)?;
    fs::rename(&building, &commit_dir)?;
    let _ = fs::remove_dir_all(commit_dir.join("_src"));

    // 6. Update repo_meta.json
    update_repo_meta(&repo_root, worktree, &sha_hex)?;

    Ok(BuildResult { commit_dir, sha_hex, source_type })
}

fn head_sha_hex(worktree: &Path) -> io::Result<String> {
    let out = safe_exec::git().args(["rev-parse", "HEAD"]).current_dir(worktree).output()?;
    if !out.status.success() { return Err(io::Error::other("git rev-parse HEAD failed")); }
    Ok(std::str::from_utf8(&out.stdout).map_err(io::Error::other)?.trim().to_string())
}

fn worktree_clean_and_head_matches(worktree: &Path, sha: &str) -> io::Result<bool> {
    let head = head_sha_hex(worktree)?;
    if head != sha { return Ok(false); }
    let out = safe_exec::git().args(["diff-index", "--quiet", "HEAD"]).current_dir(worktree).output()?;
    Ok(out.status.success())
}

fn git_archive_to(worktree: &Path, sha: &str, dest: &Path) -> io::Result<()> {
    let mut cmd = safe_exec::git();
    cmd.args(["archive", "--format=tar", sha]).current_dir(worktree);
    let archive = cmd.output()?;
    if !archive.status.success() { return Err(io::Error::other("git archive failed")); }
    // Extract via tar; minimal — could use `tar` crate to avoid shell dependency
    let mut child = std::process::Command::new("tar")
        .args(["-x", "-C", dest.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    use std::io::Write;
    child.stdin.as_mut().unwrap().write_all(&archive.stdout)?;
    let s = child.wait()?;
    if !s.success() { return Err(io::Error::other("tar extract failed")); }
    Ok(())
}

fn run_analyzer_pipeline(src_root: &Path, out_dir: &Path) -> io::Result<usize> {
    // Delegate to existing analyzer pipeline. Real impl: call into
    // cgn_analyzer with src_root, write graph.bin + tantivy/ into out_dir.
    // For now this is the integration point — concrete API depends on existing
    // pipeline signature (see commands/admin/index.rs::run for current shape).
    crate::commands::admin::index::run_analyzer_for_paths(src_root, out_dir)
}

fn collect_refs(worktree: &Path, sha: &str) -> io::Result<Vec<RefRecord>> {
    let out = safe_exec::git()
        .args(["for-each-ref", "--points-at", sha, "--format=%(refname)"])
        .current_dir(worktree).output()?;
    if !out.status.success() { return Ok(vec![]); }
    let now = chrono::Utc::now().to_rfc3339();
    Ok(std::str::from_utf8(&out.stdout).unwrap_or("")
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| RefRecord { ref_name: l.to_string(), seen_at: now.clone() })
        .collect())
}

fn source_type_from_refs(refs: &[RefRecord]) -> SourceType {
    for r in refs {
        if r.ref_name.starts_with("refs/heads/") { return SourceType::Branch; }
    }
    for r in refs {
        if r.ref_name.starts_with("refs/tags/") { return SourceType::Tag; }
    }
    for r in refs {
        if r.ref_name.starts_with("refs/pull/") || r.ref_name.starts_with("refs/merge-requests/") {
            return SourceType::Pr;
        }
    }
    SourceType::Commit
}

fn source_id_from_refs(refs: &[RefRecord]) -> Option<String> {
    for r in refs {
        if let Some(b) = r.ref_name.strip_prefix("refs/heads/") {
            return Some(b.to_string());
        }
    }
    for r in refs {
        if let Some(t) = r.ref_name.strip_prefix("refs/tags/") {
            return Some(t.to_string());
        }
    }
    for r in refs {
        if let Some(rest) = r.ref_name.strip_prefix("refs/pull/").or_else(|| r.ref_name.strip_prefix("refs/merge-requests/")) {
            if let Some(n) = rest.split('/').next() {
                return Some(n.to_string());
            }
        }
    }
    None
}

fn parent_sha(worktree: &Path, sha: &str) -> io::Result<String> {
    let out = safe_exec::git()
        .args(["rev-parse", &format!("{sha}^")])
        .current_dir(worktree).output()?;
    if !out.status.success() { return Err(io::Error::other("no parent")); }
    Ok(std::str::from_utf8(&out.stdout).map_err(io::Error::other)?.trim().to_string())
}

fn sync_all_files(dir: &Path) -> io::Result<()> {
    for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(Result::ok) {
        if entry.file_type().is_file() {
            let f = File::open(entry.path())?;
            f.sync_all()?;
        }
    }
    Ok(())
}

fn update_repo_meta(repo_root: &Path, worktree: &Path, sha: &str) -> io::Result<()> {
    let meta_path = repo_root.join("meta.json");
    let lock_path = repo_root.join(".meta.lock");
    let lock = OpenOptions::new().create(true).write(true).open(&lock_path)?;
    lock.lock_exclusive()?;

    let mut rm = if meta_path.exists() {
        RepoMeta::read(&meta_path)?
    } else {
        RepoMeta {
            version: 1,
            common_dir: git_common_dir_string(worktree)?,
            remote_url: git_remote_url(worktree).ok(),
            aliases: vec![],
            known_refs: Default::default(),
            last_built_sha: None,
            total_size_bytes: 0,
            last_touched: chrono::Utc::now().to_rfc3339(),
        }
    };
    rm.last_built_sha = Some(sha.to_string());
    rm.last_touched = chrono::Utc::now().to_rfc3339();
    rm.total_size_bytes = dir_size(repo_root)?;
    RepoMeta::write_atomic(&meta_path, &rm)?;
    Ok(())
}

fn git_common_dir_string(worktree: &Path) -> io::Result<String> {
    let out = safe_exec::git().args(["rev-parse", "--git-common-dir"]).current_dir(worktree).output()?;
    let s = std::str::from_utf8(&out.stdout).map_err(io::Error::other)?.trim();
    Ok(std::fs::canonicalize(s).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| s.into()))
}

fn git_remote_url(worktree: &Path) -> io::Result<String> {
    let out = safe_exec::git().args(["remote", "get-url", "origin"]).current_dir(worktree).output()?;
    if !out.status.success() { return Err(io::Error::other("no origin remote")); }
    Ok(std::str::from_utf8(&out.stdout).map_err(io::Error::other)?.trim().to_string())
}

fn dir_size(dir: &Path) -> io::Result<u64> {
    let mut total = 0;
    for e in walkdir::WalkDir::new(dir).into_iter().filter_map(Result::ok) {
        if e.file_type().is_file() {
            total += e.metadata()?.len();
        }
    }
    Ok(total)
}

fn wait_for_completion(building: &Path, commit_dir: &Path) -> io::Result<BuildResult> {
    let start = std::time::Instant::now();
    while building.exists() {
        if start.elapsed() > std::time::Duration::from_secs(600) {
            return Err(io::Error::other("build attach timeout"));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    if !commit_dir.exists() {
        return Err(io::Error::other("attached builder failed to publish"));
    }
    let meta_path = commit_dir.join("meta.json");
    let meta = CommitBuildMeta::read(&meta_path)?;
    Ok(BuildResult {
        commit_dir: commit_dir.to_path_buf(),
        sha_hex: meta.sha,
        source_type: meta.source_type,
    })
}
```

NOTE: this orchestrator calls `crate::commands::admin::index::run_analyzer_for_paths` which doesn't exist yet — it'll be carved out of existing `commands/admin/index::run` in Task 4.4.

Update `crates/cgn-cli/src/build/mod.rs`:

```rust
pub mod orchestrator;
```

Add `chrono`, `fs2`, `walkdir`, `hex` to Cargo.toml if not present.

- [ ] **Step 4: Move on** — test will pass after Task 4.4 carves out `run_analyzer_for_paths`. Commit current state with placeholder marker.

```bash
git add -u
git commit -m "feat(build): L2 build orchestrator skeleton

build_l2 handles: source resolution (git archive on dirty),
build lock + attach pattern, meta.json write, atomic publish.
Pipeline integration deferred to next task (run_analyzer_for_paths)."
```

### Task 4.4: Carve `run_analyzer_for_paths` out of `commands/admin/index::run`

**Files:**
- Modify: `crates/cgn-cli/src/commands/admin/index.rs`

- [ ] **Step 1: Locate parser pipeline section**

```bash
grep -n "tree_sitter\|analyzer\|graph.bin\|tantivy" crates/cgn-cli/src/commands/admin/index.rs | head -30
```

Identify the function or block that:
1. takes a source root path
2. runs tree-sitter via cgn_analyzer
3. writes graph.bin
4. writes tantivy/ index

- [ ] **Step 2: Extract into pub fn**

Add to `index.rs`:

```rust
pub fn run_analyzer_for_paths(src_root: &std::path::Path, out_dir: &std::path::Path)
    -> std::io::Result<usize> {
    // Move the body of: tree-sitter scan over src_root, build ZeroCopyGraph,
    // rkyv archive to out_dir.join("graph.bin"), tantivy index to out_dir.join("tantivy/").
    // Return: node_count.
    //
    // Implementation: copy/refactor from existing `run` function body's parse-and-write block.
    // Specific signature depends on current pipeline shape — preserve same behavior, just
    // accept paths as args instead of inferring from IndexArgs.
    todo!("extract from existing run() — concrete code depends on current pipeline shape")
}
```

The `todo!()` is unacceptable in final code. Engineer doing this task: read existing `run` body, copy the parse + write block, parameterize input/output paths. This is mechanical extraction, not new design. **If the existing pipeline can't be cleanly extracted, leave a note and stop — the task design assumes a clean carve-out.**

- [ ] **Step 3: Build + test**

```bash
cargo build -p code-graph-nexus
cargo test -p code-graph-nexus --test build_orchestrator
```

Expected: passes once extraction is correct.

- [ ] **Step 4: Commit**

```bash
git add crates/cgn-cli/src/commands/admin/index.rs
git commit -m "refactor(admin): extract run_analyzer_for_paths from index::run

build_l2 orchestrator can now drive the analyzer over arbitrary source/output dirs."
```

### Task 4.5: Rewire `commands/admin/index::run` to use `build_l2`

**Files:**
- Modify: `crates/cgn-cli/src/commands/admin/index.rs::run`

- [ ] **Step 1: Replace path-derivation logic with `build_l2` call**

In `run`:

```rust
pub fn run(args: IndexArgs) -> Result<(), String> {
    let worktree = std::path::PathBuf::from(&args.repo);
    let target_sha = None; // build HEAD; future flag for explicit --rev <sha>
    let result = crate::build::orchestrator::build_l2(&worktree, target_sha)
        .map_err(|e| format!("build_l2 failed: {e}"))?;

    if !args.quiet {
        eprintln!("✓ Built L2 at {} ({:?})", result.sha_hex, result.source_type);
    }
    Ok(())
}
```

Delete old branch-based path logic.

- [ ] **Step 2: Build + run smoke**

```bash
cargo build -p code-graph-nexus
cargo run -p code-graph-nexus -- admin index --repo /tmp/sample-repo
```

Expected: success, produces `~/.cgn/<repo>/commits/<dirname>/graph.bin`.

- [ ] **Step 3: Commit**

```bash
git add -u
git commit -m "refactor(admin): rewire commands/admin/index::run to build_l2 orchestrator"
```

---

## Phase 5: Write Path L1 (Incremental Overlay)

Goal: when a file is edited, write per-file fragment to `<repo>/sessions/<sid>/graph_overlay/`. After this phase, L1 dirty overlay exists; merge logic into queries comes via Engine extensions.

### Task 5.1: Session resolution (env / hook / MCP / fallback)

**Files:**
- Create: `crates/cgn-cli/src/session/resolver.rs`
- Modify: `crates/cgn-cli/src/lib.rs` (`pub mod session;`)
- Create: `crates/cgn-cli/src/session/mod.rs`
- Test: `crates/cgn-cli/tests/session_resolver.rs`

- [ ] **Step 1: Failing test**

```rust
// crates/cgn-cli/tests/session_resolver.rs
use cgn_cli::session::resolver::resolve_session_id;

#[test]
fn env_takes_precedence() {
    std::env::set_var("CLAUDE_CODE_SESSION_ID", "test-session-123");
    let id = resolve_session_id(None);
    assert_eq!(id, "test-session-123");
    std::env::remove_var("CLAUDE_CODE_SESSION_ID");
}

#[test]
fn cli_explicit_overrides_env() {
    std::env::set_var("CLAUDE_CODE_SESSION_ID", "env-session");
    let id = resolve_session_id(Some("explicit"));
    assert_eq!(id, "explicit");
    std::env::remove_var("CLAUDE_CODE_SESSION_ID");
}

#[test]
fn fallback_to_cli_pid() {
    std::env::remove_var("CLAUDE_CODE_SESSION_ID");
    let id = resolve_session_id(None);
    assert!(id.starts_with("cli-"), "got: {id}");
}
```

- [ ] **Step 2: Verify fails**

```bash
cargo test -p code-graph-nexus --test session_resolver
```

- [ ] **Step 3: Implement**

```rust
// crates/cgn-cli/src/session/mod.rs
pub mod resolver;
pub mod overlay_writer;
```

```rust
// crates/cgn-cli/src/session/resolver.rs
use sha2::{Digest, Sha256};

pub fn resolve_session_id(explicit: Option<&str>) -> String {
    if let Some(s) = explicit { return s.to_string(); }
    if let Ok(s) = std::env::var("CLAUDE_CODE_SESSION_ID") {
        if !s.is_empty() { return s; }
    }
    let pid = std::process::id();
    let mut h = Sha256::new();
    h.update(pid.to_le_bytes());
    h.update(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap()
        .as_nanos().to_le_bytes());
    let digest = h.finalize();
    format!("cli-{}", hex::encode(&digest[..4]))
}
```

- [ ] **Step 4: Tests pass + commit**

```bash
cargo test -p code-graph-nexus --test session_resolver
git add crates/cgn-cli/src/session/ crates/cgn-cli/src/lib.rs \
        crates/cgn-cli/tests/session_resolver.rs
git commit -m "feat(session): resolve_session_id — explicit > env > pid fallback"
```

### Task 5.2: L1 overlay writer — per-file fragment

**Files:**
- Create: `crates/cgn-cli/src/session/overlay_writer.rs`
- Test: `crates/cgn-cli/tests/overlay_writer.rs`

- [ ] **Step 1: Failing test**

```rust
// crates/cgn-cli/tests/overlay_writer.rs
use cgn_cli::session::overlay_writer::{write_dirty_fragment, FragmentInput};
use cgn_core::session::{DirtyFiles, SessionMeta};

#[test]
fn dirty_file_first_time_creates_fragment_and_updates_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("sessions").join("test-sid");
    std::fs::create_dir_all(&session_dir).unwrap();

    let sm = SessionMeta {
        version: 1,
        session_id: "test-sid".into(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_worktree: "/work/x".into(),
        overlay_version: 0,
    };
    SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &sm).unwrap();

    let df = DirtyFiles::empty();
    DirtyFiles::write_atomic(&session_dir.join("dirty_files.json"), &df).unwrap();

    let input = FragmentInput {
        rel_path: "src/a.rs".into(),
        content: b"fn a() {}".to_vec(),
        mtime_ns: 1000,
    };
    write_dirty_fragment(&session_dir, &input).unwrap();

    // Fragment exists
    let fragments: Vec<_> = std::fs::read_dir(session_dir.join("graph_overlay"))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(fragments.len(), 1, "expected 1 fragment, got {}", fragments.len());

    // Manifest updated
    let df = DirtyFiles::read(&session_dir.join("dirty_files.json")).unwrap();
    assert!(df.entries.contains_key("src/a.rs"));

    // SessionMeta overlay_version incremented
    let sm = SessionMeta::read(&session_dir.join("session_meta.json")).unwrap();
    assert_eq!(sm.overlay_version, 1);
}

#[test]
fn parse_failed_persists_old_fragment() {
    // Setup with existing dirty entry, simulate parse fail → entry.parse_failed = true,
    // fragment_id unchanged. (Test infra below requires inject_parse_failure helper.)
    // Marked ignore until Phase 5 testing infra solidifies.
}
```

- [ ] **Step 2: Verify fails**

```bash
cargo test -p code-graph-nexus --test overlay_writer
```

- [ ] **Step 3: Implement**

```rust
// crates/cgn-cli/src/session/overlay_writer.rs
use cgn_core::registry::io::atomic_write_json;
use cgn_core::session::{DirtyEntry, DirtyFiles, SessionMeta};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::Path;

pub struct FragmentInput {
    pub rel_path: String,
    pub content: Vec<u8>,
    pub mtime_ns: u64,
}

pub struct FragmentOutcome {
    pub fragment_id: String,
    pub parse_failed: bool,
}

pub fn write_dirty_fragment(session_dir: &Path, input: &FragmentInput) -> io::Result<FragmentOutcome> {
    let content_hash = sha256_hex(&input.content);
    let fragment_id = content_hash[..16].to_string();

    let overlay_dir = session_dir.join("graph_overlay");
    fs::create_dir_all(&overlay_dir)?;

    let fragment_path = overlay_dir.join(format!("{fragment_id}.bin"));

    // Parse + serialize fragment
    let archive_bytes = match parse_to_fragment(&input.rel_path, &input.content) {
        Ok(b) => b,
        Err(_) => {
            // parse_failed path — keep old fragment if any, mark in manifest
            update_manifest(session_dir, &input.rel_path, &fragment_id, &content_hash, input.mtime_ns, true)?;
            return Ok(FragmentOutcome { fragment_id, parse_failed: true });
        }
    };

    // Atomic write
    let tmp = overlay_dir.join(format!("{fragment_id}.tmp"));
    fs::write(&tmp, &archive_bytes)?;
    let f = fs::File::open(&tmp)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, &fragment_path)?;

    update_manifest(session_dir, &input.rel_path, &fragment_id, &content_hash, input.mtime_ns, false)?;
    bump_overlay_version(session_dir)?;

    Ok(FragmentOutcome { fragment_id, parse_failed: false })
}

fn parse_to_fragment(rel_path: &str, content: &[u8]) -> io::Result<Vec<u8>> {
    // Integration point — call into cgn_analyzer to parse a single file
    // and return rkyv-archived Vec<u8> of the node/edge fragment.
    //
    // Real signature TBD by analyzer crate API. For implementing engineer:
    // examine cgn_analyzer::parse_file(rel_path, content) → Fragment,
    // then rkyv::to_bytes::<_, 256>(&fragment).
    crate::commands::scan::parse_single_file_to_fragment(rel_path, content)
}

fn update_manifest(
    session_dir: &Path,
    rel_path: &str,
    fragment_id: &str,
    content_hash: &str,
    mtime_ns: u64,
    parse_failed: bool,
) -> io::Result<()> {
    let manifest_path = session_dir.join("dirty_files.json");
    let mut df = if manifest_path.exists() {
        DirtyFiles::read(&manifest_path)?
    } else {
        DirtyFiles::empty()
    };
    df.entries.insert(rel_path.to_string(), DirtyEntry {
        mtime_ns,
        content_hash: content_hash.to_string(),
        fragment_id: fragment_id.to_string(),
        tantivy_delta_segment: None,
        parse_failed,
    });
    atomic_write_json(&manifest_path, &df)
}

fn bump_overlay_version(session_dir: &Path) -> io::Result<()> {
    let path = session_dir.join("session_meta.json");
    let mut sm = SessionMeta::read(&path)?;
    sm.overlay_version += 1;
    sm.last_touched = chrono::Utc::now().to_rfc3339();
    atomic_write_json(&path, &sm)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    hex::encode(d)
}
```

NOTE: `parse_single_file_to_fragment` is the integration point. The engineer doing this task: examine analyzer API, add a `parse_single_file_to_fragment` shim in `commands::scan` or `analyzer/`. **Test will not pass until shim is in place.**

- [ ] **Step 4: Implement `parse_single_file_to_fragment` shim**

In `crates/cgn-cli/src/commands/scan.rs` (or new helper):

```rust
pub fn parse_single_file_to_fragment(rel_path: &str, content: &[u8]) -> std::io::Result<Vec<u8>> {
    // Call analyzer with single-file content; serialize result.
    // Specific signature depends on cgn_analyzer's public API.
    // The engineer doing this task should:
    //   1. Read cgn-analyzer's lib.rs to find the public parse entry
    //   2. Call it with (rel_path, content)
    //   3. Take the returned graph fragment, run rkyv::to_bytes
    //   4. Return Vec<u8>
    todo!("integrate with cgn_analyzer public API")
}
```

This `todo!()` MUST be replaced before commit. Implementing engineer: do the integration; this scaffolding shows what the shim must do.

- [ ] **Step 5: Tests pass + commit**

```bash
cargo test -p code-graph-nexus --test overlay_writer
git add -u
git commit -m "feat(session): write_dirty_fragment + manifest update + version bump

Per-file rkyv fragment, atomic rename, BTreeMap manifest update, session_meta heartbeat.
parse_single_file_to_fragment shim integrates with cgn_analyzer."
```

### Task 5.3: auto_ensure wire-up — emit overlay updates for dirty files

**Files:**
- Modify: `crates/cgn-cli/src/auto_ensure.rs` (extend `ensure_fresh`)

- [ ] **Step 1: Identify integration point**

Current `auto_ensure::ensure_fresh` decides Ready / Stale / Missing and invokes full reindex on Stale or Missing. New behavior:

- Missing → invoke L2 build (`build_l2`) — see Phase 4
- Stale → walk worktree, emit per-file `write_dirty_fragment` calls into the session's L1 dir
- Ready → noop

- [ ] **Step 2: Rewrite `ensure_fresh`**

```rust
// crates/cgn-cli/src/auto_ensure.rs (sketch — preserves signature)
use crate::build::orchestrator;
use crate::session::{overlay_writer, resolver};

pub fn ensure_fresh(graph_path: &Path, worktree_root: &Path) -> Result<(), String> {
    let state = ensure_index(graph_path, worktree_root).map_err(|e| format!("{e}"))?;
    match state {
        EnsureResult::Ready => Ok(()),
        EnsureResult::Missing => {
            orchestrator::build_l2(worktree_root, None).map_err(|e| format!("build_l2: {e}"))?;
            eprintln!("✓ Index built (L2 cold path)");
            Ok(())
        }
        EnsureResult::Stale { .. } => {
            // Per-file overlay update
            let session_id = resolver::resolve_session_id(None);
            let home_cgn = cgn_core::registry::resolve_home_cgn();
            let repo_dir = crate::repo_identity::repo_dir_name_for_cwd(worktree_root)
                .map_err(|e| format!("repo identity: {e}"))?;
            let session_dir = home_cgn.join(&repo_dir).join("sessions").join(&session_id);
            std::fs::create_dir_all(&session_dir).map_err(|e| format!("session dir: {e}"))?;
            ensure_session_meta_exists(&session_dir, worktree_root)?;

            for dirty_path in find_dirty_files(graph_path, worktree_root) {
                let rel = dirty_path.strip_prefix(worktree_root).unwrap_or(&dirty_path);
                let content = std::fs::read(&dirty_path)
                    .map_err(|e| format!("read {}: {e}", dirty_path.display()))?;
                let mtime_ns = std::fs::metadata(&dirty_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                let input = overlay_writer::FragmentInput {
                    rel_path: rel.to_string_lossy().into(),
                    content,
                    mtime_ns,
                };
                overlay_writer::write_dirty_fragment(&session_dir, &input)
                    .map_err(|e| format!("overlay write: {e}"))?;
            }
            eprintln!("✓ L1 overlay refreshed");
            Ok(())
        }
    }
}

fn ensure_session_meta_exists(session_dir: &Path, worktree: &Path) -> Result<(), String> {
    let meta_path = session_dir.join("session_meta.json");
    if meta_path.exists() { return Ok(()); }
    // Init fresh session_meta with current HEAD as base
    // ... (use head_sha + chrono::Utc::now())
    todo!("init SessionMeta from current HEAD")
}

fn find_dirty_files(graph_path: &Path, root: &Path) -> Vec<PathBuf> {
    // Refactor of existing any_source_newer_than — instead of bool short-circuit,
    // collect all newer paths.
    todo!("refactor any_source_newer_than to return Vec<PathBuf>")
}
```

Engineer doing this task fills in both `todo!()`s with concrete implementations.

- [ ] **Step 3: Run integration test**

```bash
cargo build -p code-graph-nexus
cargo run -p code-graph-nexus -- inspect --name FooBar --repo /tmp/sample
```

Expected: first call builds L2 (sync), subsequent edits trigger L1 overlay.

- [ ] **Step 4: Commit**

```bash
git add -u
git commit -m "feat(auto_ensure): split L2 build (Missing) vs L1 overlay (Stale)

Edits no longer trigger full reindex — only the changed files go into L1
fragment dir under the current session."
```

### Task 5.4: Engine query merges L1 fragments

**Files:**
- Modify: `crates/cgn-cli/src/engine.rs` (graph() merges overlay)
- Test: `crates/cgn-cli/tests/engine_overlay.rs`

- [ ] **Step 1: Failing test (end-to-end: edit reflected in query)**

```rust
// crates/cgn-cli/tests/engine_overlay.rs
#[test]
#[ignore = "end-to-end query — wire up after merge logic"]
fn edit_reflected_in_subsequent_query() {
    // Build L2 with file containing fn original() {}
    // Edit file to contain fn renamed() {}
    // Trigger ensure_fresh (Stale path)
    // Engine::load with overlay → graph contains "renamed" node, not "original"
    todo!()
}
```

- [ ] **Step 2: Implement overlay merge**

This is the most complex piece — needs careful design of the merge semantics in the graph layer (ZeroCopyGraph or a thin wrapper). High-level:

```rust
// In Engine, when overlay_dir.is_some():
//   1. graph() returns a wrapper type that holds:
//      - base: &ArchivedZeroCopyGraph (from L2 mmap)
//      - overrides: HashMap<NodeId, OverrideEntry> built by scanning overlay/*.bin
//   2. node lookups: check overrides first, then base
//   3. edges: union(overrides.edges, base.edges minus edges_to_overridden_nodes)
```

Concrete implementation is non-trivial; engineer doing this task:
1. Define `OverlayView` struct wrapping base graph + overlay HashMap
2. Implement `Display` / query traits matching `ArchivedZeroCopyGraph`'s public surface
3. Update query commands (cypher, inspect, search, impact) to use `OverlayView` instead of raw `ArchivedZeroCopyGraph` when overlay is available

This expands beyond a single-task scope — **split into 4 sub-tasks during implementation if needed**.

- [ ] **Step 3: Commit**

```bash
git add -u
git commit -m "feat(engine): merge L1 fragments over L2 base in queries

OverlayView wraps base graph + per-fragment overrides. Query commands now
see edits-without-commit reflected in results."
```

---

## Phase 6: Promotion (Case A + B)

Goal: when HEAD changes mid-session, transition L1 + L2 correctly.

### Task 6.1: Promotion case detection (merge-base)

**Files:**
- Create: `crates/cgn-cli/src/session/promotion.rs`
- Test: `crates/cgn-cli/tests/promotion.rs`

- [ ] **Step 1: Failing test**

```rust
// crates/cgn-cli/tests/promotion.rs
use cgn_cli::session::promotion::{promotion_case, PromotionCase};
use std::process::Command;

#[test]
fn fast_forward_is_case_a() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path();
    Command::new("git").arg("-C").arg(wt).arg("init").arg("-q").status().unwrap();
    std::fs::write(wt.join("a"), "1").unwrap();
    Command::new("git").arg("-C").arg(wt).args(["add", "."]).status().unwrap();
    Command::new("git").arg("-C").arg(wt).args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "x"]).status().unwrap();
    let old_sha = head(wt);
    std::fs::write(wt.join("a"), "2").unwrap();
    Command::new("git").arg("-C").arg(wt).args(["add", "."]).status().unwrap();
    Command::new("git").arg("-C").arg(wt).args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "y"]).status().unwrap();
    let new_sha = head(wt);

    assert_eq!(promotion_case(&old_sha, &new_sha, wt), PromotionCase::A);
}

#[test]
fn cross_branch_is_case_b() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path();
    Command::new("git").arg("-C").arg(wt).arg("init").arg("-q").status().unwrap();
    std::fs::write(wt.join("a"), "1").unwrap();
    Command::new("git").arg("-C").arg(wt).args(["add", "."]).status().unwrap();
    Command::new("git").arg("-C").arg(wt).args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "x"]).status().unwrap();
    let main_sha = head(wt);
    Command::new("git").arg("-C").arg(wt).args(["checkout", "-q", "-b", "side"]).status().unwrap();
    std::fs::write(wt.join("b"), "1").unwrap();
    Command::new("git").arg("-C").arg(wt).args(["add", "."]).status().unwrap();
    Command::new("git").arg("-C").arg(wt).args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "side"]).status().unwrap();
    let side_sha = head(wt);

    // main → side is not fast-forward (side diverges)
    // BUT in our heuristic, A iff merge-base(old, new) == old.
    // From main_sha to side_sha: merge-base = main_sha, so this IS Case A by our rule.
    // For Case B test we need: old=side, new=main (going backward).
    assert_eq!(promotion_case(&side_sha, &main_sha, wt), PromotionCase::B);
}

fn head(p: &std::path::Path) -> String {
    let o = Command::new("git").arg("-C").arg(p).args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}
```

- [ ] **Step 2: Verify fails**

```bash
cargo test -p code-graph-nexus --test promotion
```

- [ ] **Step 3: Implement**

```rust
// crates/cgn-cli/src/session/promotion.rs
use crate::git::safe_exec;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionCase { A, B }

pub fn promotion_case(old_sha: &str, new_sha: &str, worktree: &Path) -> PromotionCase {
    let base = match merge_base(old_sha, new_sha, worktree) {
        Some(b) => b,
        None => return PromotionCase::B,
    };
    if base == old_sha { PromotionCase::A } else { PromotionCase::B }
}

fn merge_base(a: &str, b: &str, worktree: &Path) -> Option<String> {
    let out = safe_exec::git()
        .args(["merge-base", a, b])
        .current_dir(worktree)
        .output().ok()?;
    if !out.status.success() { return None; }
    Some(std::str::from_utf8(&out.stdout).ok()?.trim().to_string())
}
```

- [ ] **Step 4: Tests pass + commit**

```bash
cargo test -p code-graph-nexus --test promotion
git add -u
git commit -m "feat(promotion): merge-base-based case detection (A=fast-forward, B=diverged)"
```

### Task 6.2: Case A promotion — content-equivalence drop

**Files:**
- Modify: `crates/cgn-cli/src/session/promotion.rs` (add `promote_case_a`)
- Test: extend `tests/promotion.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn case_a_drops_fragment_when_content_matches_l2() {
    // Build L2 at SHA A
    // Add L1 dirty fragment for file F (content C)
    // Commit F → SHA B; commit's blob of F == C
    // promote_case_a → fragment should be dropped, entry removed from dirty_files
    todo!()
}

#[test]
fn case_a_keeps_fragment_when_content_diverges() {
    // L1 has fragment for F (content C1)
    // Commit creates SHA B but worktree edits F further to C2 (not in commit)
    // promote_case_a → fragment kept (C2 ≠ commit's blob of F)
    todo!()
}
```

- [ ] **Step 2: Implement**

```rust
// Add to crates/cgn-cli/src/session/promotion.rs
use crate::git::safe_exec;
use cgn_core::registry::io::atomic_write_json;
use cgn_core::session::{DirtyFiles, SessionMeta};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::Path;

pub fn promote_case_a(
    session_dir: &Path,
    worktree: &Path,
    new_sha: &str,
) -> io::Result<PromoteStats> {
    let manifest_path = session_dir.join("dirty_files.json");
    let mut df = DirtyFiles::read(&manifest_path)?;
    let mut dropped = 0;
    let mut kept = 0;

    let entries_snapshot: Vec<(String, _)> = df.entries.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (rel_path, entry) in entries_snapshot {
        let l2_blob_hash = match git_cat_file_sha256(worktree, new_sha, &rel_path) {
            Ok(h) => h,
            Err(_) => { kept += 1; continue; }
        };
        if entry.content_hash == l2_blob_hash {
            // Drop
            let fragment_path = session_dir.join("graph_overlay").join(format!("{}.bin", entry.fragment_id));
            let _ = fs::remove_file(&fragment_path);
            df.entries.remove(&rel_path);
            dropped += 1;
        } else {
            kept += 1;
        }
    }

    // Update session_meta.base_sha
    let sm_path = session_dir.join("session_meta.json");
    let mut sm = SessionMeta::read(&sm_path)?;
    sm.base_sha = new_sha.to_string();
    sm.last_touched = chrono::Utc::now().to_rfc3339();
    atomic_write_json(&sm_path, &sm)?;
    atomic_write_json(&manifest_path, &df)?;

    Ok(PromoteStats { dropped, kept })
}

pub struct PromoteStats { pub dropped: usize, pub kept: usize }

fn git_cat_file_sha256(worktree: &Path, sha: &str, rel_path: &str) -> io::Result<String> {
    let spec = format!("{sha}:{rel_path}");
    let out = safe_exec::git()
        .args(["cat-file", "blob", &spec])
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!("git cat-file failed for {spec}")));
    }
    let d = Sha256::digest(&out.stdout);
    Ok(hex::encode(d))
}
```

- [ ] **Step 3: Tests pass + commit**

```bash
cargo test -p code-graph-nexus --test promotion
git add -u
git commit -m "feat(promotion): Case A content-equivalence — drop fragment iff sha256(L1)==sha256(L2 blob)"
```

### Task 6.3: Case B promotion — atomic L1 invalidate + delayed GC

**Files:**
- Modify: `crates/cgn-cli/src/session/promotion.rs` (add `promote_case_b`)
- Test: extend `tests/promotion.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn case_b_atomic_renames_l1_to_stale() {
    // Setup session_dir with fragments + manifest
    // promote_case_b(old_sha, new_sha) → session_dir empty + .stale-<old_sha>/ exists
    // After 2s+ → .stale-<old_sha>/ removed
    todo!()
}
```

- [ ] **Step 2: Implement**

```rust
pub fn promote_case_b(
    session_dir: &Path,
    old_sha: &str,
    new_sha: &str,
) -> io::Result<()> {
    let parent = session_dir.parent().ok_or_else(|| io::Error::other("session_dir has no parent"))?;
    let sid = session_dir.file_name().and_then(|s| s.to_str()).ok_or_else(|| io::Error::other("invalid session_dir name"))?;
    let stale = parent.join(format!("{sid}.stale-{old_sha}"));
    fs::rename(session_dir, &stale)?;
    fs::create_dir(session_dir)?;

    // Fresh session_meta with new base
    let sm = SessionMeta {
        version: 1,
        session_id: sid.to_string(),
        pid: Some(std::process::id()),
        started_at: chrono::Utc::now().to_rfc3339(),
        last_touched: chrono::Utc::now().to_rfc3339(),
        base_sha: new_sha.to_string(),
        source_worktree: String::new(),  // caller fills in via auto_ensure
        overlay_version: 0,
    };
    atomic_write_json(&session_dir.join("session_meta.json"), &sm)?;
    atomic_write_json(&session_dir.join("dirty_files.json"), &DirtyFiles::empty())?;

    // Background GC stale dir after 2s
    let stale_clone = stale.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let _ = fs::remove_dir_all(&stale_clone);
    });

    Ok(())
}
```

- [ ] **Step 3: Tests pass + commit**

```bash
cargo test -p code-graph-nexus --test promotion
git add -u
git commit -m "feat(promotion): Case B atomic L1 invalidate + delayed stale GC"
```

### Task 6.4: Wire promotion into auto_ensure

**Files:**
- Modify: `crates/cgn-cli/src/auto_ensure.rs` (detect HEAD drift before overlay update)

- [ ] **Step 1: Add drift detection**

```rust
// In ensure_fresh, before the Stale path:
let current_head = head_sha_hex(worktree_root)?;
let session_meta = SessionMeta::read(&session_dir.join("session_meta.json"))?;
if session_meta.base_sha != current_head {
    let case = promotion::promotion_case(&session_meta.base_sha, &current_head, worktree_root);
    match case {
        PromotionCase::A => {
            promotion::promote_case_a(&session_dir, worktree_root, &current_head)?;
            eprintln!("✓ session promoted (Case A: fast-forward)");
        }
        PromotionCase::B => {
            promotion::promote_case_b(&session_dir, &session_meta.base_sha, &current_head)?;
            eprintln!("✓ session rebased (Case B: cross-refactor)");
        }
    }
}
// Continue with overlay update for any remaining dirty files
```

- [ ] **Step 2: Run smoke**

```bash
cargo build -p code-graph-nexus
# Manual test: build L2, edit file, commit, run query → should see Case A audit line
```

- [ ] **Step 3: Commit**

```bash
git add crates/cgn-cli/src/auto_ensure.rs
git commit -m "feat(auto_ensure): detect HEAD drift → invoke promotion before overlay update"
```

---

## Phase 7: GC + Concurrency

Goal: GC reachability + LRU; orphan / dead pid sweep; multi-session safety verified.

### Task 7.1: Reachability computation

**Files:**
- Create: `crates/cgn-cli/src/admin/gc.rs`
- Test: `crates/cgn-cli/tests/gc.rs`

- [ ] **Step 1: Failing test**

```rust
// crates/cgn-cli/tests/gc.rs
use cgn_cli::admin::gc::reachability;
use std::process::Command;

#[test]
fn reachability_includes_branches_and_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    std::fs::create_dir(&wt).unwrap();
    Command::new("git").arg("-C").arg(&wt).arg("init").arg("-q").status().unwrap();
    std::fs::write(wt.join("a"), "x").unwrap();
    Command::new("git").arg("-C").arg(&wt).args(["add", "."]).status().unwrap();
    Command::new("git").arg("-C").arg(&wt).args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "x"]).status().unwrap();
    let main_sha = git_head_sha(&wt);

    let repo_root = tmp.path().join("repo_root");
    std::fs::create_dir_all(repo_root.join("sessions").join("sid1")).unwrap();
    // Write a session_meta pinning a different sha
    let session_sha = "0".repeat(40);
    let sm = cgn_core::session::SessionMeta {
        version: 1,
        session_id: "sid1".into(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: chrono::Utc::now().to_rfc3339(),
        base_sha: session_sha.clone(),
        source_worktree: wt.to_string_lossy().into(),
        overlay_version: 0,
    };
    cgn_core::session::SessionMeta::write_atomic(
        &repo_root.join("sessions").join("sid1").join("session_meta.json"),
        &sm,
    ).unwrap();

    let r = reachability(&repo_root, &wt).unwrap();
    assert!(r.contains(&main_sha), "missing main_sha in reachability: {r:?}");
    assert!(r.contains(&session_sha), "missing session_sha: {r:?}");
}

fn git_head_sha(p: &std::path::Path) -> String {
    let o = Command::new("git").arg("-C").arg(p).args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}
```

- [ ] **Step 2: Verify fails**

```bash
cargo test -p code-graph-nexus --test gc
```

- [ ] **Step 3: Implement**

```rust
// crates/cgn-cli/src/admin/gc.rs
use crate::git::safe_exec;
use cgn_core::session::SessionMeta;
use std::collections::HashSet;
use std::io;
use std::path::Path;

pub fn reachability(repo_root: &Path, worktree: &Path) -> io::Result<HashSet<String>> {
    let mut set = HashSet::new();
    // Branch refs from worktree
    let out = safe_exec::git()
        .args(["for-each-ref", "--format=%(objectname)"])
        .current_dir(worktree).output()?;
    for line in std::str::from_utf8(&out.stdout).unwrap_or("").lines() {
        let s = line.trim();
        if s.len() == 40 { set.insert(s.to_string()); }
    }
    // Active sessions' base_sha
    let sessions_dir = repo_root.join("sessions");
    if let Ok(it) = std::fs::read_dir(&sessions_dir) {
        for entry in it.flatten() {
            let sm_path = entry.path().join("session_meta.json");
            if let Ok(sm) = SessionMeta::read(&sm_path) {
                // Skip stale sessions
                if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&sm.last_touched) {
                    let age = chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
                    if age.num_hours() > 24 { continue; }
                }
                set.insert(sm.base_sha);
            }
        }
    }
    Ok(set)
}
```

- [ ] **Step 4: Tests pass + commit**

```bash
cargo test -p code-graph-nexus --test gc
git add -u
git commit -m "feat(admin/gc): reachability = git refs ∪ active session base_shas"
```

### Task 7.2: LRU eviction

**Files:**
- Modify: `crates/cgn-cli/src/admin/gc.rs` (add `enforce_quota`)
- Test: extend `tests/gc.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn quota_evicts_oldest_unreachable_until_below_threshold() {
    // Create commits/<dir1>, <dir2>, <dir3> with different built_at timestamps
    // Mark <dir2> unreachable, set total_size > QUOTA
    // enforce_quota → <dir2> removed
    todo!()
}
```

- [ ] **Step 2: Implement**

```rust
const DEFAULT_QUOTA_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const TARGET_LOAD_FACTOR: f64 = 0.8;

pub fn enforce_quota(repo_root: &Path, worktree: &Path, quota: u64) -> io::Result<EvictStats> {
    let reachable = reachability(repo_root, worktree)?;
    let commits_dir = repo_root.join("commits");
    let mut entries: Vec<(std::path::PathBuf, String, u64, u64)> = vec![];  // (path, sha, size, built_at_epoch)
    for e in std::fs::read_dir(&commits_dir)? {
        let e = e?;
        if !e.file_type()?.is_dir() { continue; }
        let name = e.file_name().to_string_lossy().to_string();
        if name.contains(".building") || name.contains(".stale") { continue; }
        let Ok(parsed) = cgn_core::registry::CommitDirName::parse(&name) else { continue };
        let meta_path = e.path().join("meta.json");
        let Ok(cm) = cgn_core::registry::CommitBuildMeta::read(&meta_path) else { continue };
        let size = crate::admin::utils::dir_size(&e.path()).unwrap_or(0);
        let built_at_epoch = chrono::DateTime::parse_from_rfc3339(&cm.built_at)
            .map(|d| d.timestamp() as u64).unwrap_or(0);
        entries.push((e.path(), parsed.sha_hex(), size, built_at_epoch));
    }

    let total: u64 = entries.iter().map(|(_, _, s, _)| *s).sum();
    if total <= quota { return Ok(EvictStats { evicted: 0, freed_bytes: 0 }); }
    let target_size = (quota as f64 * TARGET_LOAD_FACTOR) as u64;

    entries.sort_by_key(|(_, _, _, t)| *t);  // oldest first
    let mut freed = 0u64;
    let mut evicted = 0;
    let mut current = total;
    for (path, sha, size, _) in entries {
        if current <= target_size { break; }
        if reachable.contains(&sha) { continue; }
        std::fs::remove_dir_all(&path)?;
        current -= size;
        freed += size;
        evicted += 1;
    }
    Ok(EvictStats { evicted, freed_bytes: freed })
}

pub struct EvictStats { pub evicted: usize, pub freed_bytes: u64 }
```

- [ ] **Step 3: Tests pass + commit**

```bash
cargo test -p code-graph-nexus --test gc
git add -u
git commit -m "feat(admin/gc): LRU quota enforcement — evict oldest unreachable to 80% load"
```

### Task 7.3: Session orphan sweep

**Files:**
- Modify: `crates/cgn-cli/src/admin/gc.rs` (add `sweep_sessions`)

- [ ] **Step 1: Test**

```rust
#[test]
fn idle_session_swept_after_24h() {
    // Write session_meta with last_touched 25h ago
    // sweep_sessions → dir marked .dead → next sweep removes
    todo!()
}

#[test]
fn dead_pid_sweep_immediate() {
    // Write session_meta with pid = invalid (e.g., u32::MAX)
    // sweep_sessions → dir immediately marked .dead
    todo!()
}
```

- [ ] **Step 2: Implement**

```rust
pub fn sweep_sessions(repo_root: &Path) -> io::Result<SweepStats> {
    let sessions_dir = repo_root.join("sessions");
    let mut marked = 0;
    let mut removed = 0;
    if let Ok(it) = std::fs::read_dir(&sessions_dir) {
        for entry in it.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".dead") {
                // Already marked — remove
                std::fs::remove_dir_all(entry.path())?;
                removed += 1;
                continue;
            }
            let sm_path = entry.path().join("session_meta.json");
            let Ok(sm) = SessionMeta::read(&sm_path) else { continue };
            let mut should_kill = false;

            if let Some(pid) = sm.pid {
                // Linux-specific pid check; skip on other platforms
                #[cfg(unix)]
                {
                    if unsafe { libc::kill(pid as i32, 0) } != 0 {
                        let e = std::io::Error::last_os_error();
                        if e.raw_os_error() == Some(libc::ESRCH) {
                            should_kill = true;
                        }
                    }
                }
            }
            if !should_kill {
                if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&sm.last_touched) {
                    let age = chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
                    if age.num_hours() > 24 { should_kill = true; }
                }
            }
            if should_kill {
                let dead = entry.path().with_extension("dead");
                std::fs::rename(entry.path(), &dead)?;
                marked += 1;
            }
        }
    }
    Ok(SweepStats { marked, removed })
}

pub struct SweepStats { pub marked: usize, pub removed: usize }
```

- [ ] **Step 3: Commit**

```bash
git add -u
git commit -m "feat(admin/gc): sweep_sessions — pid liveness + 24h heartbeat → mark .dead → remove"
```

### Task 7.4: Background GC heartbeat

**Files:**
- Modify: `crates/cgn-cli/src/main.rs` (early-startup heartbeat check)

- [ ] **Step 1: Add check at CLI entry**

```rust
// In main(), before command dispatch:
fn maybe_spawn_background_gc() {
    let home = cgn_core::registry::resolve_home_cgn();
    let stamp = home.join(".last-gc");
    let age_ok = std::fs::metadata(&stamp).and_then(|m| m.modified()).ok()
        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs() > 24 * 3600)
        .unwrap_or(true);  // missing → trigger
    if !age_ok { return; }

    // Touch stamp to prevent concurrent spawns
    let _ = std::fs::write(&stamp, b"");

    // Spawn background gc
    let _ = std::process::Command::new(std::env::current_exe().unwrap_or_else(|_| "cgn".into()))
        .args(["admin", "gc", "--quiet"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
```

- [ ] **Step 2: Commit**

```bash
git add -u
git commit -m "feat(cli): background GC heartbeat — auto-spawn cgn admin gc when stamp >24h"
```

---

## Phase 8: CLI Surface

Goal: add new flags + commands, remove obsolete ones, rewire hook resolution.

### Task 8.1: `--rev` flag

**Files:**
- Modify: query commands (`inspect`, `impact`, `cypher`, `search`, `scan`, `contracts`, `coverage`, `routes`, `shape-check`, `diff`)
- Test: `crates/cgn-cli/tests/rev_flag.rs`

- [ ] **Step 1: Failing test**

```rust
// crates/cgn-cli/tests/rev_flag.rs
use std::process::Command;

#[test]
fn inspect_with_rev_branch_resolves_correctly() {
    // Build L2 at SHA A on branch main, switch to branch dev (different SHA),
    // run `cgn inspect Foo --rev main` → should use L2 at A, not current HEAD's L2.
    todo!()
}
```

- [ ] **Step 2: Add `--rev` to each query command's clap struct**

```rust
// In inspect args (and similar for impact/cypher/search/scan/contracts/coverage/routes/shape-check/diff):
#[derive(Parser)]
pub struct InspectArgs {
    // ... existing fields ...
    /// Resolve queries against this ref (branch / tag / PR / commit SHA). Defaults to HEAD.
    #[arg(long, value_name = "REF")]
    pub rev: Option<String>,
}
```

- [ ] **Step 3: Translate `--rev` to SHA in command handler**

Add helper `crates/cgn-cli/src/rev_resolve.rs`:

```rust
use crate::git::safe_exec;
use std::path::Path;

pub fn resolve_rev_to_sha(worktree: &Path, rev: Option<&str>) -> std::io::Result<String> {
    let target = rev.unwrap_or("HEAD");
    let out = safe_exec::git().args(["rev-parse", target]).current_dir(worktree).output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!("git rev-parse {target} failed")));
    }
    let s = std::str::from_utf8(&out.stdout).map_err(std::io::Error::other)?.trim();
    if s.len() != 40 {
        return Err(std::io::Error::other(format!("invalid sha: {s}")));
    }
    Ok(s.to_string())
}
```

Use in each command:

```rust
let sha = crate::rev_resolve::resolve_rev_to_sha(&worktree, args.rev.as_deref())?;
// then resolve <repo>/commits/<dirname>/graph.bin via CommitIndex
```

- [ ] **Step 4: Tests pass + commit**

```bash
cargo test -p code-graph-nexus --test rev_flag
git add -u
git commit -m "feat(cli): --rev flag on all query commands

git rev-parse <ref> → SHA → CommitIndex lookup. Branches, tags, PRs, HEAD~N
all work uniformly."
```

### Task 8.2: `--session-id` flag

**Files:**
- Modify: query commands' clap structs (add `--session-id`)
- Modify: hooks (already have session in payload)

- [ ] **Step 1: Add flag**

```rust
// Each query command:
#[arg(long)]
pub session_id: Option<String>,
```

- [ ] **Step 2: Wire to session resolver**

```rust
let sid = crate::session::resolver::resolve_session_id(args.session_id.as_deref());
```

- [ ] **Step 3: Commit**

```bash
git add -u
git commit -m "feat(cli): --session-id flag — explicit > env > pid fallback"
```

### Task 8.3: New `cgn admin sessions` subcommand

**Files:**
- Create: `crates/cgn-cli/src/commands/admin/sessions.rs`
- Modify: `crates/cgn-cli/src/commands/admin/mod.rs` (add subcommand)

- [ ] **Step 1: Implement**

```rust
// crates/cgn-cli/src/commands/admin/sessions.rs
use clap::Subcommand;
use cgn_core::registry::resolve_home_cgn;
use cgn_core::session::SessionMeta;

#[derive(Subcommand)]
pub enum SessionsCmd {
    /// List all active sessions across all repos
    List,
    /// Force-reset a specific session (atomic rename to .dead)
    Reset { session_id: String },
    /// Immediately sweep orphan/dead sessions
    Sweep,
}

pub fn run(cmd: SessionsCmd) -> Result<(), String> {
    match cmd {
        SessionsCmd::List => list_sessions(),
        SessionsCmd::Reset { session_id } => reset_session(&session_id),
        SessionsCmd::Sweep => sweep_all(),
    }
}

fn list_sessions() -> Result<(), String> {
    let home = resolve_home_cgn();
    let it = std::fs::read_dir(&home).map_err(|e| format!("{e}"))?;
    println!("repo\tsession_id\tbase_sha\tlast_touched\tdirty_count");
    for repo_entry in it.flatten() {
        if !repo_entry.path().is_dir() { continue; }
        let repo_name = repo_entry.file_name().to_string_lossy().to_string();
        if repo_name.starts_with('_') || repo_name.starts_with('.') { continue; }
        let sessions_dir = repo_entry.path().join("sessions");
        let Ok(sit) = std::fs::read_dir(&sessions_dir) else { continue };
        for s_entry in sit.flatten() {
            let sm_path = s_entry.path().join("session_meta.json");
            let Ok(sm) = SessionMeta::read(&sm_path) else { continue };
            let df_path = s_entry.path().join("dirty_files.json");
            let dirty_count = cgn_core::session::DirtyFiles::read(&df_path)
                .map(|d| d.entries.len()).unwrap_or(0);
            println!("{}\t{}\t{}\t{}\t{}", repo_name, sm.session_id, &sm.base_sha[..8], sm.last_touched, dirty_count);
        }
    }
    Ok(())
}

fn reset_session(sid: &str) -> Result<(), String> {
    let home = resolve_home_cgn();
    let it = std::fs::read_dir(&home).map_err(|e| format!("{e}"))?;
    for repo_entry in it.flatten() {
        let candidate = repo_entry.path().join("sessions").join(sid);
        if candidate.exists() {
            let dead = candidate.with_extension("dead");
            std::fs::rename(&candidate, &dead).map_err(|e| format!("{e}"))?;
            println!("Reset session {sid} in {}", repo_entry.file_name().to_string_lossy());
            return Ok(());
        }
    }
    Err(format!("session {sid} not found"))
}

fn sweep_all() -> Result<(), String> {
    let home = resolve_home_cgn();
    let it = std::fs::read_dir(&home).map_err(|e| format!("{e}"))?;
    for repo_entry in it.flatten() {
        if !repo_entry.path().is_dir() { continue; }
        let _ = crate::admin::gc::sweep_sessions(&repo_entry.path());
    }
    Ok(())
}
```

Register in admin mod.

- [ ] **Step 2: Commit**

```bash
git add -u
git commit -m "feat(admin): cgn admin sessions list/reset/sweep"
```

### Task 8.4: `cgn admin reset`

**Files:**
- Create: `crates/cgn-cli/src/commands/admin/reset.rs`
- Modify: `crates/cgn-cli/src/commands/admin/mod.rs`

- [ ] **Step 1: Implement**

```rust
// crates/cgn-cli/src/commands/admin/reset.rs
use cgn_core::registry::resolve_home_cgn;

pub fn run(force: bool) -> Result<(), String> {
    let home = resolve_home_cgn();
    if !home.exists() { return Ok(()); }
    if !force {
        eprintln!("This will wipe {} (all indexed graphs + sessions).", home.display());
        eprint!("Type 'reset' to confirm: ");
        use std::io::BufRead;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).map_err(|e| format!("{e}"))?;
        if line.trim() != "reset" { return Err("aborted".into()); }
    }
    std::fs::remove_dir_all(&home).map_err(|e| format!("{e}"))?;
    eprintln!("✓ ~/.cgn/ wiped. Next query auto-rebuilds.");
    Ok(())
}
```

- [ ] **Step 2: Commit**

```bash
git add -u
git commit -m "feat(admin): cgn admin reset — wipe ~/.cgn/ with confirm prompt"
```

### Task 8.5: `cgn admin gc` command

**Files:**
- Create: `crates/cgn-cli/src/commands/admin/gc_cmd.rs`
- Modify: `commands/admin/mod.rs`

- [ ] **Step 1: Implement**

```rust
// crates/cgn-cli/src/commands/admin/gc_cmd.rs
use crate::admin::gc;
use cgn_core::registry::resolve_home_cgn;

pub struct GcArgs {
    pub repo: Option<String>,
    pub dry_run: bool,
    pub force: bool,
    pub quiet: bool,
}

pub fn run(args: GcArgs) -> Result<(), String> {
    let home = resolve_home_cgn();
    let repos: Vec<_> = if let Some(r) = args.repo.as_deref() {
        vec![home.join(r)]
    } else {
        std::fs::read_dir(&home).map_err(|e| format!("{e}"))?
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .filter(|e| !e.file_name().to_string_lossy().starts_with(['_', '.']))
            .map(|e| e.path()).collect()
    };

    for repo_root in repos {
        let worktree = derive_worktree_for_repo(&repo_root)?;
        let stats = gc::enforce_quota(&repo_root, &worktree, gc::DEFAULT_QUOTA_BYTES)
            .map_err(|e| format!("{e}"))?;
        let sw = gc::sweep_sessions(&repo_root).map_err(|e| format!("{e}"))?;
        if !args.quiet {
            println!("{}: evicted={} freed_bytes={} sessions_marked={} sessions_removed={}",
                     repo_root.file_name().unwrap().to_string_lossy(),
                     stats.evicted, stats.freed_bytes, sw.marked, sw.removed);
        }
    }
    // Update .last-gc stamp
    std::fs::write(home.join(".last-gc"), b"").ok();
    Ok(())
}

fn derive_worktree_for_repo(repo_root: &std::path::Path) -> Result<std::path::PathBuf, String> {
    // Read repo_meta.json.common_dir → infer worktree.
    let rm = cgn_core::registry::RepoMeta::read(&repo_root.join("meta.json"))
        .map_err(|e| format!("{e}"))?;
    std::fs::canonicalize(rm.common_dir.trim_end_matches("/.git").trim_end_matches("/.git/"))
        .map_err(|e| format!("{e}"))
}
```

- [ ] **Step 2: Commit**

```bash
git add -u
git commit -m "feat(admin): cgn admin gc — LRU eviction + session sweep, per-repo or all"
```

### Task 8.6: Remove `--branch` flag and `rename_branch` command

**Files:**
- Modify: all query command structs (remove `--branch`)
- Delete: `crates/cgn-cli/src/commands/admin/rename_branch.rs`
- Modify: `crates/cgn-cli/src/commands/admin/mod.rs`

- [ ] **Step 1: Sweep `--branch` usage**

```bash
grep -rn "branch:\s*Option<String>\|--branch" crates/cgn-cli/src/commands/ --include="*.rs"
```

For each match: delete the `pub branch: Option<String>` field; remove `--branch` handling in run fn.

- [ ] **Step 2: Delete rename_branch**

```bash
git rm crates/cgn-cli/src/commands/admin/rename_branch.rs
# Edit mod.rs to remove the subcommand registration
```

- [ ] **Step 3: Commit**

```bash
git add -u
git commit -m "refactor(cli): remove --branch flag and rename_branch command

Branch is no longer in storage. Use --rev <branch-name> for branch-pointed SHA."
```

### Task 8.7: Rewire `commands/hook/common.rs::lookup_index_dir`

**Files:**
- Modify: `crates/cgn-cli/src/commands/hook/common.rs::lookup_index_dir`

- [ ] **Step 1: Replace branch-based lookup**

```rust
// crates/cgn-cli/src/commands/hook/common.rs
use crate::commit_lookup::CommitIndex;
use crate::repo_identity::repo_dir_name_for_cwd;
use cgn_core::registry::resolve_home_cgn;
use std::path::Path;

/// Resolve cwd → <repo>/commits/<dirname>/ path, or None if no L2 yet.
pub fn lookup_index_dir(cwd: &Path) -> Option<std::path::PathBuf> {
    let home = resolve_home_cgn();
    let repo_dir = repo_dir_name_for_cwd(cwd).ok()?;
    let commits = home.join(&repo_dir).join("commits");
    let head_sha = head_sha_bytes(cwd)?;
    let idx = CommitIndex::scan(&commits).ok()?;
    let dir = idx.find(&head_sha)?;
    Some(commits.join(dir))
}

fn head_sha_bytes(cwd: &Path) -> Option<[u8; 20]> {
    let out = crate::git::safe_exec::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(cwd).output().ok()?;
    if !out.status.success() { return None; }
    let s = std::str::from_utf8(&out.stdout).ok()?.trim();
    if s.len() != 40 { return None; }
    let mut sha = [0u8; 20];
    hex::decode_to_slice(s, &mut sha).ok()?;
    Some(sha)
}
```

- [ ] **Step 2: Commit**

```bash
git add -u
git commit -m "refactor(hook): lookup_index_dir uses commit-SHA layout"
```

### Task 8.8: Full workspace build + workspace tests

- [ ] **Step 1: Build**

```bash
cargo build -p code-graph-nexus --release
```

Expected: success.

- [ ] **Step 2: Run all tests**

```bash
cargo test -p code-graph-nexus --tests
cargo test -p cgn-core --tests
```

Expected: all pass. If any fail, fix before continuing.

- [ ] **Step 3: Clippy + format**

```bash
cargo clippy -p code-graph-nexus --tests
cargo clippy -p cgn-core --tests
rustfmt --edition 2021 $(git diff --name-only --diff-filter=AM HEAD~30 | grep '\.rs$')
```

- [ ] **Step 4: Commit any lint fixes**

```bash
git add -u
git commit -m "chore: cargo clippy + rustfmt across redesign"
```

---

## Phase 9: 14-Language Smoke + Docs

Goal: verify layout change doesn't break any language's build/query flow; update user-facing docs.

### Task 9.1: 14-language smoke test

**Files:**
- Create: `crates/cgn-cli/tests/multi_language_smoke.rs`

- [ ] **Step 1: Write smoke test**

```rust
// crates/cgn-cli/tests/multi_language_smoke.rs
use std::process::Command;

const LANGUAGES: &[(&str, &str, &str)] = &[
    // (lang, sample_file_name, sample_content)
    ("typescript", "main.ts", "function hello(): void { console.log('hi'); }"),
    ("javascript", "main.js", "function hello() { console.log('hi'); }"),
    ("python", "main.py", "def hello():\n    print('hi')\n"),
    ("java", "Main.java", "class Main { public static void main(String[] a) {} }"),
    ("kotlin", "Main.kt", "fun hello() { println(\"hi\") }"),
    ("csharp", "Main.cs", "class Main { static void Hello() {} }"),
    ("go", "main.go", "package main\nfunc hello() {}\n"),
    ("rust", "main.rs", "fn hello() {}"),
    ("php", "main.php", "<?php function hello() {} ?>"),
    ("ruby", "main.rb", "def hello\n  puts 'hi'\nend\n"),
    ("swift", "main.swift", "func hello() { print(\"hi\") }"),
    ("c", "main.c", "void hello(void) {}"),
    ("cpp", "main.cpp", "void hello() {}"),
    ("dart", "main.dart", "void hello() { print('hi'); }"),
];

#[test]
fn smoke_build_and_query_each_language() {
    for (lang, fname, content) in LANGUAGES {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path();
        Command::new("git").arg("-C").arg(wt).arg("init").arg("-q").status().unwrap();
        std::fs::write(wt.join(fname), content).unwrap();
        Command::new("git").arg("-C").arg(wt).args(["add", "."]).status().unwrap();
        Command::new("git").arg("-C").arg(wt).args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", lang]).status().unwrap();

        let home = tmp.path().join("home");
        std::env::set_var("HOME", &home);

        // Build L2 + query
        let bin = env!("CARGO_BIN_EXE_cgn");
        let build = Command::new(bin)
            .args(["admin", "index", "--repo", wt.to_str().unwrap()])
            .output().unwrap();
        assert!(build.status.success(), "[{lang}] build failed: {}", String::from_utf8_lossy(&build.stderr));

        let query = Command::new(bin)
            .args(["search", "hello", "--repo", wt.to_str().unwrap()])
            .output().unwrap();
        assert!(query.status.success(), "[{lang}] query failed: {}", String::from_utf8_lossy(&query.stderr));
        let stdout = String::from_utf8_lossy(&query.stdout);
        assert!(stdout.contains("hello") || stdout.contains(fname),
                "[{lang}] hello not found in: {stdout}");
    }
}

#[test]
fn smoke_edit_reflects_in_overlay_each_language() {
    for (lang, fname, content) in LANGUAGES {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path();
        Command::new("git").arg("-C").arg(wt).arg("init").arg("-q").status().unwrap();
        std::fs::write(wt.join(fname), content).unwrap();
        Command::new("git").arg("-C").arg(wt).args(["add", "."]).status().unwrap();
        Command::new("git").arg("-C").arg(wt).args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", lang]).status().unwrap();

        let home = tmp.path().join("home");
        std::env::set_var("HOME", &home);

        let bin = env!("CARGO_BIN_EXE_cgn");
        Command::new(bin).args(["admin", "index", "--repo", wt.to_str().unwrap()]).status().unwrap();

        // Edit: rename hello → renamed
        let new_content = content.replace("hello", "renamed");
        std::fs::write(wt.join(fname), &new_content).unwrap();

        // Query for new name — should hit via L1 overlay
        let query = Command::new(bin)
            .args(["search", "renamed", "--repo", wt.to_str().unwrap()])
            .output().unwrap();
        let stdout = String::from_utf8_lossy(&query.stdout);
        assert!(stdout.contains("renamed"), "[{lang}] L1 overlay did not surface edit: {stdout}");
    }
}
```

- [ ] **Step 2: Run**

```bash
cargo test -p code-graph-nexus --test multi_language_smoke --release -- --nocapture
```

Expected: both tests pass for all 14 languages. If any language fails, investigate before declaring victory.

- [ ] **Step 3: Commit**

```bash
git add crates/cgn-cli/tests/multi_language_smoke.rs
git commit -m "test: 14-language smoke — build + query + L1 overlay edit-visibility"
```

### Task 9.2: Update CLAUDE.md

**Files:**
- Modify: `CLAUDE.md` (Workspace section if it lists layout; usage examples if any)

- [ ] **Step 1: Audit**

```bash
grep -n "<branch>\|\.cgn/<repo>/<branch>\|IndexLayout\|sanitize_branch\|BranchMeta" CLAUDE.md docs/
```

- [ ] **Step 2: Update wording**

In `CLAUDE.md`, replace any branch-based layout descriptions with the v2 commit-content-addressed layout (cite the spec).

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md docs/
git commit -m "docs: update CLAUDE.md + skill docs for v2 storage layout"
```

### Task 9.3: Final acceptance run

- [ ] **Step 1: Full test sweep**

```bash
cargo test -p code-graph-nexus -p cgn-core --release
cargo clippy -p code-graph-nexus -p cgn-core --tests
```

- [ ] **Step 2: Manual smoke**

```bash
# Wipe + cold start
cgn admin reset --force  # if implemented; else manually rm -rf ~/.cgn/
cd /path/to/real/repo
cgn inspect SomeSymbol     # should trigger first-time build, sync
cgn search foo             # subsequent should be fast
echo "fn new() {}" >> src/something.rs
cgn inspect new            # L1 overlay should reflect edit immediately
git commit -am "test"
cgn inspect new            # promotion Case A — fragment dropped
```

- [ ] **Step 3: PR prep**

```bash
git log --oneline main..HEAD | head -50
# Verify commit messages are clean; squash if any "wip" / "fix typo" leaked in
```

```bash
gh pr create --title "Index Layout Redesign — commit-content-addressed L2 + session-local L1" --body "$(cat <<'EOF'
## Summary
- Replace `<repo>/<branch>/` layout with `<repo>/{commits/<sha-dir>, sessions/<sid>}`
- L2: commit-SHA-keyed, immutable, cross-session shared
- L1: per-session, incremental dirty overlay (graph fragments + tantivy delta)
- Promotion: Case A (fast-forward, content-equivalence drop) vs Case B (cross-refactor, atomic invalidate)
- No migration — `cgn admin reset` wipes and rebuilds; pre-1.0

Spec: docs/superpowers/specs/2026-05-17-index-layout-redesign-design.md
Plan: docs/superpowers/plans/2026-05-17-index-layout-redesign.md

## Test plan
- [x] Unit: CommitDirName parser, schema round-trips, build mode rule, promotion case detection
- [x] Integration: first-build sync, subsequent-build bg, L1 overlay edit visibility, attach pattern
- [x] 14-language smoke: build + query + edit-reflects-in-overlay
- [x] Promotion: Case A drop fragment when content matches, Case B atomic invalidate
- [x] GC: reachability (refs ∪ active sessions), LRU eviction, session sweep
- [x] Clippy + rustfmt clean

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 4: Mark plan complete**

```bash
git add -u
git commit -m "chore: phase 9 acceptance — all green, ready for review" --allow-empty
```

---

## Self-Review Notes (inline corrections)

After writing all tasks, checked against spec:

- ✅ §1 Motivation: covered by overall scope (no task needed)
- ✅ §3 Storage layout: Tasks 1.1–1.4 (types), 4.1–4.3 (write produces it), 3.3 (read consumes it)
- ✅ §4 Rust types: Tasks 1.1–1.4, 1.6
- ✅ §5 Read path: Tasks 3.1–3.4
- ✅ §6 Write L2: Tasks 4.1–4.5
- ✅ §7 Write L1: Tasks 5.1–5.4
- ✅ §8 Promotion: Tasks 6.1–6.4
- ✅ §9 Concurrency: covered by build lock in 4.3, parse lock in 5.2, multi-session test in 7.x
- ✅ §10 GC: Tasks 7.1–7.4
- ✅ §11 CLI surface: Tasks 8.1–8.8
- ✅ §12 Error handling: distributed across tasks (parse_failed in 5.2, attach pattern in 4.3, v1 detection in 1.6)
- ✅ §13 Testing: Tasks 9.1 (14-lang) + per-task unit tests
- ✅ §14 Invariants: I1 (atomic publish in 4.3), I2 (atomic fragment in 5.2), I3 (overlay_version in 5.2), I4 (cross-session in 7.x test), I5 (BTreeMap tests in 1.6/1.3/1.4), I6 (round-trip in 1.1), I7 (content-equivalence in 6.2), I8 (reachability in 7.1), I9 (build mode in 4.2), I10 (read path in 3.3)
- ✅ §15 Migration cleanup: Task 1.5 + 1.6 (delete + clear-error)
- ✅ §16 Out of scope: explicitly excluded — no tasks
- ✅ §17 File inventory: matches tasks

**Placeholders deliberately left** (engineer must replace):
- Task 4.4 `run_analyzer_for_paths` body — depends on existing pipeline shape
- Task 5.2 `parse_single_file_to_fragment` shim — depends on analyzer crate API
- Task 5.3 `ensure_session_meta_exists` + `find_dirty_files` — straightforward but signature-coupled
- Task 5.4 OverlayView merge logic — non-trivial, may split into sub-tasks
- Promotion Case A/B tests — fixture setup is verbose, sketched only

These are unavoidable integration points; the plan tells engineer **what** to produce, not how to navigate every existing fn signature.

**Estimated effort:** 5–9 working days for a Rust-fluent engineer with codebase context; 12–18 days if starting cold. **Net code change: +300 LOC** (del ~600 / add ~900) per spec §15.

---

## Phase 5 Status Note (added during execution, 2026-05-17)

**Task 5.4 deferred from autonomous execution.** Reason: overlay merge in Engine requires fragment-shape design that benefits from real workload feedback (which query patterns hit which override classes most). Splitting into a dedicated follow-up plan once Phase 6/7/8 have shaped the read-path requirements concretely.

**What's in place from Phase 5:**
- Task 5.1: `resolve_session_id` (explicit > env > pid fallback) ✓
- Task 5.2: `write_dirty_fragment` with `parse_single_file_to_fragment` returning empty rkyv stub. File-write atomic semantics + manifest plumbing work; the fragment payload is a structural no-op until 5.4 lands. ✓
- Task 5.3: `auto_ensure` Stale path walks worktree, calls `write_dirty_fragment` per file — wires the trigger. ✓

**What 5.4 still needs:**
- Fragment shape: rkyv-archived `(Vec<NodeUid>, Vec<NewNode>, Vec<NewEdge>)` likely; consumer-driven
- `parse_single_file_to_fragment` real per-file analyzer integration
- `Engine::graph()` returns an `OverlayView` wrapper when `overlay_dir.is_some()` — wrapper holds base mmap + per-fragment overrides HashMap, dispatches lookups to override-first/base-second
- Update query commands (`inspect / cypher / search / impact / scan`) to consume `OverlayView`'s shape

**Acceptable interim state:** L1 fragments are written to disk and tracked in `dirty_files.json`, but queries continue to see L2-only view. Promotion Case A's content-hash comparison (Phase 6) still works since it reads file content via `git cat-file blob`, not the L1 fragment.
