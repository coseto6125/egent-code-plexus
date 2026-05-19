# Force Rebuild + Session Reference Model Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire `cgn admin index --force` to actually drop + rebuild L2 (currently warn-no-op), introduce `SessionState` as a derived view so query hot-path can skip overlay merge for clean sessions, remove three dead flags, and add a minimal `admin sessions list` so the new state shows up in the CLI.

**Architecture:** New cross-cutting type `SessionState` classifies each `<repo>/sessions/<sid>/` as `PureReference | AugmentedReference | Stale` based on dirty_files + session_meta + L2 lookup. Three callers consume it: (1) `force_rebuild_l2` selectively invalidates Augmented sessions (Pure kept); (2) `Engine::open` takes a fast L2-only path for Pure; (3) `admin sessions list` shows STATE column. Force-rebuild order is L1-then-L2 to keep crash recovery self-consistent. Attach contention falls through to existing `wait_for_completion` then re-locks.

**Tech Stack:** Rust, clap, fs2 file locks, rkyv mmap, tantivy, tempfile (tests), tracing.

**Spec:** `docs/superpowers/specs/2026-05-17-force-rebuild-and-session-reference-design.md`

**Total tasks:** 7. Estimated PR: del ~50 / add ~300 LOC.

---

## File Structure

### New files

| Path | Responsibility |
|---|---|
| `crates/cgn-core/src/session/state.rs` | `SessionState` enum + `StaleReason` enum. Pure types, no I/O. |
| `crates/cgn-cli/src/session/state.rs` | `pub fn classify(repo_root, sid) -> SessionState`. Reads session_meta + dirty_files, resolves L2 via `commit_lookup::CommitIndex`. |
| `crates/cgn-cli/src/build/force.rs` | `force_rebuild_l2` + `invalidate_matching_l1` + `InvalidateReport` + `ForceRebuildResult`. Reuses build internals via `build_l2_with_options`. |
| `crates/cgn-cli/src/commands/admin/sessions.rs` | `Sessions { command: SessionsCommand }` clap subcommand. Initially only `List` variant. |
| `crates/cgn-cli/tests/session_state_test.rs` | Unit/integration tests for `classify`. |
| `crates/cgn-cli/tests/force_rebuild_test.rs` | Integration tests for `force_rebuild_l2` happy + L1 interaction. |
| `crates/cgn-cli/tests/admin_index_force_test.rs` | CLI integration tests for `cgn admin index --force` and idempotent skip. |
| `crates/cgn-cli/tests/admin_sessions_list_test.rs` | CLI integration tests for `cgn admin sessions list` STATE column + JSON. |
| `crates/cgn-cli/tests/force_rebuild_concurrent_test.rs` | Two-thread concurrent --force test. |

### Modified files

| Path | What changes |
|---|---|
| `crates/cgn-core/src/session/mod.rs` | `pub mod state;` + `pub use state::{SessionState, StaleReason};` |
| `crates/cgn-cli/src/session/mod.rs` | `pub mod state;` (promotion.rs already there) |
| `crates/cgn-cli/src/build/mod.rs` | `pub mod force;` |
| `crates/cgn-cli/src/build/orchestrator.rs` | Extract reusable helpers (no signature change to `build_l2`). |
| `crates/cgn-cli/src/engine.rs` | Add `Engine::open(repo_root, sid)` constructor + `GraphView` enum; keep `Engine::load` for back-compat. |
| `crates/cgn-cli/src/commands/admin/index.rs` | Delete `no_cache / embeddings / drop_embeddings` + warn block; rewrite `run()` as 3-way match. |
| `crates/cgn-cli/src/commands/admin/mod.rs` | Add `Sessions` variant to `AdminCommands` + dispatch arm. |

---

## Task 1: SessionState enum + classify foundation

**Files:**
- Create: `crates/cgn-core/src/session/state.rs`
- Create: `crates/cgn-cli/src/session/state.rs`
- Modify: `crates/cgn-core/src/session/mod.rs`
- Modify: `crates/cgn-cli/src/session/mod.rs`
- Test: `crates/cgn-cli/tests/session_state_test.rs`

- [ ] **Step 1: Write the failing test file**

Create `crates/cgn-cli/tests/session_state_test.rs`:

```rust
use cgn_cli::session::state::classify;
use cgn_core::session::{
    DirtyEntry, DirtyFiles, SessionMeta, SessionState, StaleReason,
};
use std::fs;
use std::path::Path;

fn setup_repo(tmp: &Path, sha: &str, dirname: &str) {
    let commits = tmp.join("commits").join(dirname);
    fs::create_dir_all(&commits).unwrap();
    fs::write(commits.join("graph.bin"), b"stub").unwrap();
    let cm = cgn_core::registry::CommitBuildMeta {
        version: 1,
        sha: sha.to_string(),
        source_type: cgn_core::registry::SourceType::Branch,
        source_id: Some("main".into()),
        built_from_worktree: "/tmp/wt".into(),
        built_at: "2026-05-17T10:00:00Z".into(),
        parent_sha: None,
        node_count: 0,
        embedding_status: cgn_core::registry::EmbeddingStatus::None,
        refs_at_build: vec![],
        refs_seen_since: vec![],
    };
    cgn_core::registry::CommitBuildMeta::write_atomic(&commits.join("meta.json"), &cm)
        .unwrap();
}

fn setup_session(tmp: &Path, sid: &str, base_sha: &str, dirty: DirtyFiles) {
    let sd = tmp.join("sessions").join(sid);
    fs::create_dir_all(&sd).unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: sid.into(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: base_sha.into(),
        source_worktree: "/tmp/wt".into(),
        overlay_version: 0,
    };
    SessionMeta::write_atomic(&sd.join("session_meta.json"), &sm).unwrap();
    DirtyFiles::write_atomic(&sd.join("dirty_files.json"), &dirty).unwrap();
}

fn one_dirty_entry() -> DirtyFiles {
    let mut df = DirtyFiles::empty();
    df.entries.insert(
        "src/a.rs".into(),
        DirtyEntry {
            mtime_ns: 1,
            content_hash: "deadbeef".into(),
            fragment_id: "frag1".into(),
            tantivy_delta_segment: None,
            parse_failed: false,
        },
    );
    df
}

const SHA: &str = "abc123def456789012345678901234567890abcd";
const DIRNAME: &str = "branch_main__abc123def456789012345678901234567890abcd";

#[test]
fn classify_empty_dirty_returns_pure_reference() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo(tmp.path(), SHA, DIRNAME);
    setup_session(tmp.path(), "sid1", SHA, DirtyFiles::empty());
    match classify(tmp.path(), "sid1") {
        SessionState::PureReference { base_sha, l2_dirname } => {
            assert_eq!(base_sha, SHA);
            assert_eq!(l2_dirname, DIRNAME);
        }
        other => panic!("expected PureReference, got {other:?}"),
    }
}

#[test]
fn classify_nonempty_dirty_returns_augmented() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo(tmp.path(), SHA, DIRNAME);
    setup_session(tmp.path(), "sid1", SHA, one_dirty_entry());
    match classify(tmp.path(), "sid1") {
        SessionState::AugmentedReference { fragment_count, .. } => {
            assert_eq!(fragment_count, 1);
        }
        other => panic!("expected AugmentedReference, got {other:?}"),
    }
}

#[test]
fn classify_missing_dirty_file_returns_pure_reference() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo(tmp.path(), SHA, DIRNAME);
    let sd = tmp.path().join("sessions").join("sid1");
    fs::create_dir_all(&sd).unwrap();
    let sm = SessionMeta {
        version: 1, session_id: "sid1".into(), pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: SHA.into(),
        source_worktree: "/tmp/wt".into(),
        overlay_version: 0,
    };
    SessionMeta::write_atomic(&sd.join("session_meta.json"), &sm).unwrap();
    assert!(matches!(
        classify(tmp.path(), "sid1"),
        SessionState::PureReference { .. }
    ));
}

#[test]
fn classify_corrupt_dirty_returns_stale_dirtycorrupt() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo(tmp.path(), SHA, DIRNAME);
    setup_session(tmp.path(), "sid1", SHA, DirtyFiles::empty());
    fs::write(
        tmp.path().join("sessions/sid1/dirty_files.json"),
        b"{ not valid json",
    )
    .unwrap();
    assert!(matches!(
        classify(tmp.path(), "sid1"),
        SessionState::Stale { reason: StaleReason::DirtyFilesCorrupt }
    ));
}

#[test]
fn classify_missing_meta_returns_stale_metaunreadable() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo(tmp.path(), SHA, DIRNAME);
    fs::create_dir_all(tmp.path().join("sessions/sid1")).unwrap();
    assert!(matches!(
        classify(tmp.path(), "sid1"),
        SessionState::Stale { reason: StaleReason::MetaUnreadable }
    ));
}

#[test]
fn classify_missing_l2_returns_stale_l2missing() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("commits")).unwrap();
    setup_session(tmp.path(), "sid1", SHA, DirtyFiles::empty());
    assert!(matches!(
        classify(tmp.path(), "sid1"),
        SessionState::Stale { reason: StaleReason::L2Missing }
    ));
}
```

- [ ] **Step 2: Run test to verify it fails (no impl yet)**

Run: `cargo test -p cgn-cli --test session_state_test 2>&1 | tail -20`

Expected: compile error, `session_state::classify` and `SessionState` not found.

- [ ] **Step 3: Write enum types in core**

Create `crates/cgn-core/src/session/state.rs`:

```rust
//! Derived view of session liveness, classifying each `<repo>/sessions/<sid>/`
//! as PureReference (clean, can short-circuit overlay merge), Augmented (has
//! dirty fragments), or Stale (cannot serve queries). Not persisted to disk —
//! always re-derived from session_meta + dirty_files + commits/.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    PureReference {
        base_sha: String,
        l2_dirname: String,
    },
    AugmentedReference {
        base_sha: String,
        l2_dirname: String,
        fragment_count: usize,
    },
    Stale {
        reason: StaleReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    MetaUnreadable,
    DirtyFilesCorrupt,
    L2Missing,
    Orphan,
}

impl StaleReason {
    /// Short text used by `admin sessions list` STATE column.
    pub fn short(&self) -> &'static str {
        match self {
            Self::MetaUnreadable => "meta",
            Self::DirtyFilesCorrupt => "dirty_corr",
            Self::L2Missing => "l2_missing",
            Self::Orphan => "orphan",
        }
    }
}
```

- [ ] **Step 4: Re-export from core::session::mod**

Modify `crates/cgn-core/src/session/mod.rs`:

```rust
pub mod meta;
pub mod overlay;
pub mod state;
pub use meta::SessionMeta;
pub use overlay::{DirtyEntry, DirtyFiles};
pub use state::{SessionState, StaleReason};
```

- [ ] **Step 5: Write classify in cli**

Create `crates/cgn-cli/src/session/state.rs`:

```rust
//! `classify`: pure function from filesystem state to `SessionState`.
//! Lives in cli (not core) because resolving `base_sha → l2_dirname` requires
//! `commit_lookup::CommitIndex`, which is a cli-side concern.

use crate::commit_lookup::CommitIndex;
use cgn_core::session::{DirtyFiles, SessionMeta, SessionState, StaleReason};
use std::fs;
use std::path::Path;

pub fn classify(repo_root: &Path, sid: &str) -> SessionState {
    let sid_dir = repo_root.join("sessions").join(sid);
    let sm_path = sid_dir.join("session_meta.json");
    let sm = match SessionMeta::read(&sm_path) {
        Ok(sm) => sm,
        Err(_) => return SessionState::Stale { reason: StaleReason::MetaUnreadable },
    };

    let l2_dirname = match resolve_l2_dirname(repo_root, &sm.base_sha) {
        Some(d) => d,
        None => return SessionState::Stale { reason: StaleReason::L2Missing },
    };

    let dirty_path = sid_dir.join("dirty_files.json");
    let dirty = match fs::read(&dirty_path) {
        Ok(bytes) => match serde_json::from_slice::<DirtyFiles>(&bytes) {
            Ok(df) => df,
            Err(_) => return SessionState::Stale { reason: StaleReason::DirtyFilesCorrupt },
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DirtyFiles::empty(),
        Err(_) => return SessionState::Stale { reason: StaleReason::DirtyFilesCorrupt },
    };

    if dirty.entries.is_empty() {
        SessionState::PureReference { base_sha: sm.base_sha, l2_dirname }
    } else {
        SessionState::AugmentedReference {
            base_sha: sm.base_sha,
            l2_dirname,
            fragment_count: dirty.entries.len(),
        }
    }
}

fn resolve_l2_dirname(repo_root: &Path, sha_hex: &str) -> Option<String> {
    let commits = repo_root.join("commits");
    let idx = CommitIndex::scan(&commits).ok()?;
    let sha_bytes = sha_hex_to_bytes(sha_hex)?;
    idx.find(&sha_bytes).map(|s| s.to_string())
}

fn sha_hex_to_bytes(hex: &str) -> Option<[u8; 20]> {
    if hex.len() != 40 { return None; }
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&hex[i*2..i*2+2], 16).ok()?;
    }
    Some(out)
}
```

- [ ] **Step 6: Re-export from cli::session::mod**

Check current contents of `crates/cgn-cli/src/session/mod.rs`:

```bash
cat crates/cgn-cli/src/session/mod.rs
```

It currently exposes `promotion` only. Add `state`:

```rust
pub mod promotion;
pub mod state;
```

- [ ] **Step 7: Verify CommitIndex::find returns &str**

```bash
grep -n "fn find\|pub fn scan" crates/cgn-cli/src/commit_lookup.rs | head
```

If `find` returns `Option<&str>` or `Option<&String>`, the `.map(|s| s.to_string())` in `resolve_l2_dirname` is correct. If it returns `Option<&Path>`, change to `.map(|p| p.file_name().unwrap().to_string_lossy().into_owned())`. Adjust accordingly.

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test -p cgn-cli --test session_state_test 2>&1 | tail -25`

Expected: 6 tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/cgn-core/src/session/state.rs \
        crates/cgn-core/src/session/mod.rs \
        crates/cgn-cli/src/session/state.rs \
        crates/cgn-cli/src/session/mod.rs \
        crates/cgn-cli/tests/session_state_test.rs
git commit -m "feat(session): SessionState derived view + classify (PureReference / Augmented / Stale)

Cross-cutting type that classifies each <repo>/sessions/<sid>/ as
PureReference (clean), AugmentedReference (has dirty overlays), or
Stale (unrecoverable). Foundation for --force selective invalidate,
hot-path L2-only early branch, and admin sessions list STATE column.

Enum lives in core (pure types); classify() lives in cli because it
needs commit_lookup::CommitIndex to resolve base_sha → l2_dirname."
```

---

## Task 2: invalidate_matching_l1 + InvalidateReport

**Files:**
- Create: `crates/cgn-cli/src/build/force.rs`
- Modify: `crates/cgn-cli/src/build/mod.rs`
- Test: `crates/cgn-cli/tests/force_rebuild_test.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/cgn-cli/tests/force_rebuild_test.rs`:

```rust
use cgn_cli::build::force::{invalidate_matching_l1, InvalidateReport};
use cgn_core::session::{
    DirtyEntry, DirtyFiles, SessionMeta,
};
use std::fs;
use std::path::Path;

const SHA: &str = "abc123def456789012345678901234567890abcd";
const SHA2: &str = "11ee22dd33cc44bb55aa66998877665544332211";
const DIRNAME: &str = "branch_main__abc123def456789012345678901234567890abcd";

fn setup_repo_with_l2(tmp: &Path) {
    let commits = tmp.join("commits").join(DIRNAME);
    fs::create_dir_all(&commits).unwrap();
    let cm = cgn_core::registry::CommitBuildMeta {
        version: 1, sha: SHA.into(),
        source_type: cgn_core::registry::SourceType::Branch,
        source_id: Some("main".into()),
        built_from_worktree: "/tmp/wt".into(),
        built_at: "2026-05-17T10:00:00Z".into(),
        parent_sha: None, node_count: 0,
        embedding_status: cgn_core::registry::EmbeddingStatus::None,
        refs_at_build: vec![], refs_seen_since: vec![],
    };
    cgn_core::registry::CommitBuildMeta::write_atomic(
        &commits.join("meta.json"), &cm).unwrap();
}

fn add_session(tmp: &Path, sid: &str, base_sha: &str, with_dirty: bool) {
    let sd = tmp.join("sessions").join(sid);
    fs::create_dir_all(sd.join("graph_overlay")).unwrap();
    let sm = SessionMeta {
        version: 1, session_id: sid.into(), pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: base_sha.into(),
        source_worktree: "/tmp/wt".into(),
        overlay_version: 0,
    };
    SessionMeta::write_atomic(&sd.join("session_meta.json"), &sm).unwrap();
    let df = if with_dirty {
        let mut d = DirtyFiles::empty();
        d.entries.insert(
            "src/a.rs".into(),
            DirtyEntry {
                mtime_ns: 1, content_hash: "x".into(),
                fragment_id: "frag1".into(),
                tantivy_delta_segment: None, parse_failed: false,
            },
        );
        d
    } else {
        DirtyFiles::empty()
    };
    DirtyFiles::write_atomic(&sd.join("dirty_files.json"), &df).unwrap();
}

#[test]
fn invalidate_keeps_pure_reference() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo_with_l2(tmp.path());
    add_session(tmp.path(), "sid_clean", SHA, false);
    let report = invalidate_matching_l1(tmp.path(), SHA).unwrap();
    assert_eq!(report.kept, 1);
    assert_eq!(report.invalidated, 0);
    assert!(tmp.path().join("sessions/sid_clean").exists());
}

#[test]
fn invalidate_renames_augmented_to_stale() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo_with_l2(tmp.path());
    add_session(tmp.path(), "sid_dirty", SHA, true);
    let report = invalidate_matching_l1(tmp.path(), SHA).unwrap();
    assert_eq!(report.kept, 0);
    assert_eq!(report.invalidated, 1);
    assert!(!tmp.path().join("sessions/sid_dirty").exists());
    let sha8 = &SHA[..8];
    let stale = tmp.path().join(format!("sessions/sid_dirty.stale-{sha8}"));
    assert!(stale.exists());
}

#[test]
fn invalidate_ignores_sessions_for_other_sha() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo_with_l2(tmp.path());
    add_session(tmp.path(), "sid_other", SHA2, true);
    let report = invalidate_matching_l1(tmp.path(), SHA).unwrap();
    assert_eq!(report.kept, 0);
    assert_eq!(report.invalidated, 0);
    assert!(tmp.path().join("sessions/sid_other").exists());
}

#[test]
fn invalidate_skips_already_stale_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo_with_l2(tmp.path());
    fs::create_dir_all(
        tmp.path().join(format!("sessions/sid_zombie.stale-{}", &SHA[..8])),
    ).unwrap();
    let report = invalidate_matching_l1(tmp.path(), SHA).unwrap();
    assert_eq!(report.kept, 0);
    assert_eq!(report.invalidated, 0);
}

#[test]
fn invalidate_classifies_corrupt_session_as_stale_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo_with_l2(tmp.path());
    add_session(tmp.path(), "sid_corrupt", SHA, false);
    fs::write(
        tmp.path().join("sessions/sid_corrupt/dirty_files.json"),
        b"{ broken",
    ).unwrap();
    let report = invalidate_matching_l1(tmp.path(), SHA).unwrap();
    assert_eq!(report.stale_skipped, 1);
    assert!(tmp.path().join("sessions/sid_corrupt").exists());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cgn-cli --test force_rebuild_test 2>&1 | tail -15`

Expected: compile error, `build::force::invalidate_matching_l1` not found.

- [ ] **Step 3: Create force.rs with InvalidateReport + invalidate fn**

Create `crates/cgn-cli/src/build/force.rs`:

```rust
//! Force rebuild orchestration: drop existing L2 + selective L1 invalidation
//! before re-running the standard build pipeline.

use crate::session::state::classify;
use cgn_core::session::{SessionState, StaleReason};
use std::fs;
use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

#[derive(Debug, Default, Clone)]
pub struct InvalidateReport {
    pub kept: usize,
    pub invalidated: usize,
    pub stale_skipped: usize,
}

/// Rename each `sessions/<sid>/` whose `SessionState` is `AugmentedReference`
/// with `base_sha == target_sha` to `sessions/<sid>.stale-<sha8>/`, spawn a
/// 2s delayed `rm -rf`, and return counts. PureReference sessions for the
/// same SHA are kept. Stale sessions are left for the GC sweep.
pub fn invalidate_matching_l1(repo_root: &Path, target_sha: &str) -> io::Result<InvalidateReport> {
    let sessions_dir = repo_root.join("sessions");
    if !sessions_dir.exists() {
        return Ok(InvalidateReport::default());
    }
    let sha8 = &target_sha[..8];
    let mut report = InvalidateReport::default();

    for entry in fs::read_dir(&sessions_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if name.starts_with('.') || name.contains(".stale-") || name.contains(".dead") {
            continue;
        }

        match classify(repo_root, name) {
            SessionState::PureReference { base_sha, .. } if base_sha == target_sha => {
                report.kept += 1;
            }
            SessionState::AugmentedReference { base_sha, .. } if base_sha == target_sha => {
                let stale_path = sessions_dir.join(format!("{name}.stale-{sha8}"));
                fs::rename(&path, &stale_path)?;
                spawn_delayed_rm_rf(stale_path, Duration::from_secs(2));
                report.invalidated += 1;
            }
            SessionState::Stale { reason } if matches_sha_hint(repo_root, name, target_sha) => {
                tracing::warn!(
                    "session={} stale ({:?}) during force rebuild — skipping (use `admin sessions reset`)",
                    name, reason
                );
                report.stale_skipped += 1;
            }
            _ => {}
        }
    }
    Ok(report)
}

/// Stale-state sessions don't expose base_sha through `classify`. Read raw
/// `session_meta.json` to decide whether this stale session is even in scope
/// for the current `--force`. Read failure ⇒ count as in-scope (conservative).
fn matches_sha_hint(repo_root: &Path, sid: &str, target_sha: &str) -> bool {
    let path = repo_root.join("sessions").join(sid).join("session_meta.json");
    match cgn_core::session::SessionMeta::read(&path) {
        Ok(sm) => sm.base_sha == target_sha,
        Err(_) => true,
    }
}

fn spawn_delayed_rm_rf(path: std::path::PathBuf, delay: Duration) {
    thread::spawn(move || {
        thread::sleep(delay);
        let _ = fs::remove_dir_all(&path);
    });
}
```

- [ ] **Step 4: Re-export from build::mod**

Modify `crates/cgn-cli/src/build/mod.rs`:

```rust
pub mod dirname_picker;
pub mod force;
pub mod mode;
pub mod orchestrator;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p cgn-cli --test force_rebuild_test 2>&1 | tail -20`

Expected: 5 tests pass. The "rename to stale" test verifies the rename happened synchronously; the 2s GC delay means the stale dir may still be on disk when the test asserts existence — that's fine.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/build/force.rs \
        crates/cgn-cli/src/build/mod.rs \
        crates/cgn-cli/tests/force_rebuild_test.rs
git commit -m "feat(build): invalidate_matching_l1 — selective L1 session invalidation

Walks <repo>/sessions/*, classifies each via SessionState, renames
AugmentedReference sessions matching target_sha to .stale-<sha8>/
with a 2s delayed rm. PureReference sessions are kept (clean = new
L2 directly compatible). Stale sessions are skipped with a warning."
```

---

## Task 3: force_rebuild_l2 main flow

**Files:**
- Modify: `crates/cgn-cli/src/build/force.rs` (extend with `force_rebuild_l2`)
- Modify: `crates/cgn-cli/src/build/orchestrator.rs` (expose `build_l2` plus a `force` variant internally, OR factor common helpers — see Step 2)
- Test: `crates/cgn-cli/tests/force_rebuild_test.rs` (extend)

- [ ] **Step 1: Read orchestrator.rs in full to identify reusable helpers**

```bash
wc -l crates/cgn-cli/src/build/orchestrator.rs
```

Look for: `pick_dirname`, `head_sha_hex`, `worktree_clean_and_head_matches`, `git_archive_to`, `collect_refs`, `source_type_from_refs`, `source_id_from_refs`, `parent_sha`, `sync_all_files`, `update_repo_meta`, `wait_for_completion`. Some are `pub(crate)`, some `fn`. We need `force_rebuild_l2` to reuse them.

- [ ] **Step 2: Refactor build_l2 to split into pub(crate) helpers (no behavior change)**

Modify `crates/cgn-cli/src/build/orchestrator.rs` — expose internals needed by `force.rs`. Make `head_sha_hex`, `pick_dirname` reuse, `worktree_clean_and_head_matches`, `git_archive_to`, `collect_refs`, `source_type_from_refs`, `source_id_from_refs`, `parent_sha`, `sync_all_files`, `update_repo_meta`, `wait_for_completion` all `pub(crate)`.

For every private fn currently used inside `build_l2`, change to `pub(crate) fn`. Don't change behavior.

Verify with:
```bash
cargo build -p cgn-cli 2>&1 | tail -20
```

Expected: clean build.

- [ ] **Step 3: Write failing tests for force_rebuild_l2**

Append to `crates/cgn-cli/tests/force_rebuild_test.rs`:

```rust
use cgn_cli::build::force::force_rebuild_l2;
use std::process::Command;

fn git_init(p: &Path) -> String {
    Command::new("git").arg("-C").arg(p).args(["init", "-q"]).status().unwrap();
    Command::new("git").arg("-C").arg(p)
        .args(["config", "user.email", "t@t"]).status().unwrap();
    Command::new("git").arg("-C").arg(p)
        .args(["config", "user.name", "t"]).status().unwrap();
    fs::write(p.join("hello.rs"), "fn hello() {}").unwrap();
    Command::new("git").arg("-C").arg(p).args(["add", "."]).status().unwrap();
    Command::new("git").arg("-C").arg(p)
        .args(["commit", "-qm", "init"]).status().unwrap();
    let o = Command::new("git").arg("-C").arg(p)
        .args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

#[test]
fn force_rebuild_l2_when_l2_absent_builds_fresh() {
    let wt = tempfile::tempdir().unwrap();
    let sha = git_init(wt.path());
    // Point HOME at a temp dir so ~/.cgn lands in the test scratch.
    std::env::set_var("HOME", wt.path());
    let r = force_rebuild_l2(wt.path(), &sha).unwrap();
    assert_eq!(r.sha_hex, sha);
    assert!(r.rebuilt);
    assert!(r.commit_dir.join("graph.bin").exists());
    assert!(r.commit_dir.join("meta.json").exists());
}

#[test]
fn force_rebuild_l2_drops_existing_dir_and_rebuilds() {
    let wt = tempfile::tempdir().unwrap();
    let sha = git_init(wt.path());
    std::env::set_var("HOME", wt.path());

    // First build (no force) — use build_l2 directly
    let initial = cgn_cli::build::orchestrator::build_l2(wt.path(), None).unwrap();
    let first_mtime = fs::metadata(initial.commit_dir.join("graph.bin"))
        .unwrap().modified().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Now force-rebuild same SHA — commit_dir should be replaced with new mtime
    let r = force_rebuild_l2(wt.path(), &sha).unwrap();
    let second_mtime = fs::metadata(r.commit_dir.join("graph.bin"))
        .unwrap().modified().unwrap();
    assert!(second_mtime > first_mtime, "graph.bin should have newer mtime after force rebuild");
}

#[test]
fn force_rebuild_l2_invalidates_dirty_session_with_same_base_sha() {
    let wt = tempfile::tempdir().unwrap();
    let sha = git_init(wt.path());
    std::env::set_var("HOME", wt.path());

    // First build to establish L2
    let initial = cgn_cli::build::orchestrator::build_l2(wt.path(), None).unwrap();
    let repo_root = initial.commit_dir.parent().unwrap().parent().unwrap();
    add_session(repo_root, "sid_dirty", &sha, true);
    add_session(repo_root, "sid_clean", &sha, false);

    force_rebuild_l2(wt.path(), &sha).unwrap();

    assert!(!repo_root.join("sessions/sid_dirty").exists(),
            "dirty session should be renamed .stale-*");
    assert!(repo_root.join("sessions/sid_clean").exists(),
            "clean session should be kept");
}
```

Notes:
- These tests mutate `HOME` env var — they MUST run with `--test-threads=1` to avoid cross-test bleed, OR use a process-wide mutex (see `tests/promotion.rs` pattern if it has one).
- If `HOME` env mutation is brittle, refactor `force_rebuild_l2` to accept an explicit `home_cgn: Option<&Path>` parameter (preferred), then pass `wt.path()` directly. This avoids the env race entirely.

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test -p cgn-cli --test force_rebuild_test 2>&1 | tail -10`

Expected: compile error, `force_rebuild_l2` not found.

- [ ] **Step 5: Implement force_rebuild_l2**

Append to `crates/cgn-cli/src/build/force.rs`:

```rust
use crate::build::dirname_picker::pick_dirname;
use crate::build::orchestrator::{
    self as orch, BuildResult, head_sha_hex, worktree_clean_and_head_matches,
    git_archive_to, collect_refs, source_type_from_refs, source_id_from_refs,
    parent_sha, sync_all_files, update_repo_meta, wait_for_completion,
};
use crate::repo_identity::repo_dir_name_for_cwd;
use fs2::FileExt;
use cgn_core::registry::{
    resolve_home_cgn, CommitBuildMeta, EmbeddingStatus, RepoMeta,
};
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;

#[derive(Debug)]
pub struct ForceRebuildResult {
    pub sha_hex: String,
    pub source_type: cgn_core::registry::SourceType,
    pub commit_dir: PathBuf,
    pub rebuilt: bool,
    pub invalidate_report: InvalidateReport,
}

/// --force orchestration. See spec §4.2.
pub fn force_rebuild_l2(worktree: &std::path::Path, target_sha: &str) -> io::Result<ForceRebuildResult> {
    let sha_hex = target_sha.to_string();
    if sha_hex.len() != 40 || !sha_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(io::Error::other(format!("invalid sha: {sha_hex}")));
    }

    let home_cgn = resolve_home_cgn();
    let repo_dir_name = repo_dir_name_for_cwd(worktree)?;
    let repo_root = home_cgn.join(&repo_dir_name);
    fs::create_dir_all(repo_root.join("commits"))?;

    let dirname = pick_dirname(worktree, &sha_hex)?;
    let commit_dir = repo_root.join("commits").join(&dirname);
    let building = repo_root.join("commits").join(format!("{dirname}.building"));

    // 1. Acquire lock (attach-and-retake pattern if contended)
    fs::create_dir_all(&building)?;
    let lock_path = building.join(".build.lock");
    let lock = OpenOptions::new().create(true).write(true).open(&lock_path)?;
    if lock.try_lock_exclusive().is_err() {
        wait_for_completion(&building, &commit_dir)?;
        fs::create_dir_all(&building)?;
        let lock = OpenOptions::new().create(true).write(true).open(&lock_path)?;
        lock.try_lock_exclusive()
            .map_err(|e| io::Error::other(format!("re-lock after attach failed: {e}")))?;
    }

    // 2. Invalidate matching L1 BEFORE dropping L2 (spec §4.4)
    let invalidate_report = invalidate_matching_l1(&repo_root, &sha_hex)?;

    // 3. Drop existing L2
    if commit_dir.exists() {
        fs::remove_dir_all(&commit_dir)?;
    }

    // 4-7. Re-run build pipeline (mirrors orchestrator::build_l2 step 1+)
    let src_root = if worktree_clean_and_head_matches(worktree, &sha_hex)? {
        worktree.to_path_buf()
    } else {
        let src = building.join("_src");
        fs::create_dir_all(&src)?;
        git_archive_to(worktree, &sha_hex, &src)?;
        src
    };
    let node_count =
        crate::commands::admin::index::run_analyzer_for_paths(&src_root, &building)?;

    let refs_at_build = collect_refs(worktree, &sha_hex)?;
    let source_type = source_type_from_refs(&refs_at_build);
    let source_id = source_id_from_refs(&refs_at_build);
    let parent = parent_sha(worktree, &sha_hex).ok();

    let meta = CommitBuildMeta {
        version: 1,
        sha: sha_hex.clone(),
        source_type,
        source_id,
        built_from_worktree: worktree.to_string_lossy().into(),
        built_at: chrono::Utc::now().to_rfc3339(),
        parent_sha: parent,
        node_count: node_count as u32,
        embedding_status: EmbeddingStatus::None,
        refs_at_build,
        refs_seen_since: vec![],
    };
    CommitBuildMeta::write_atomic(&building.join("meta.json"), &meta)?;
    sync_all_files(&building)?;
    fs::rename(&building, &commit_dir)?;
    let _ = fs::remove_dir_all(commit_dir.join("_src"));

    update_repo_meta(&repo_root, worktree, &sha_hex)?;

    Ok(ForceRebuildResult {
        sha_hex,
        source_type,
        commit_dir,
        rebuilt: true,
        invalidate_report,
    })
}
```

If you find that orchestrator helpers (`head_sha_hex`, `pick_dirname`, etc.) aren't `pub(crate)`, edit `orchestrator.rs` to expose them. Each one should keep its current body — only the visibility modifier changes.

- [ ] **Step 6: Run tests**

Run: `cargo test -p cgn-cli --test force_rebuild_test 2>&1 | tail -30`

Expected: 8 tests pass (5 from Task 2 + 3 new).

If `HOME` env var tests are flaky, the cleanest fix is to pass `home_cgn_override: Option<PathBuf>` into `force_rebuild_l2` (and have a wrapper that defaults to `resolve_home_cgn()`). Land that before continuing.

- [ ] **Step 7: Commit**

```bash
git add crates/cgn-cli/src/build/force.rs \
        crates/cgn-cli/src/build/orchestrator.rs \
        crates/cgn-cli/tests/force_rebuild_test.rs
git commit -m "feat(build): force_rebuild_l2 — drop existing L2 + rebuild with L1 invalidation

Acquires build lock (attach-and-retake if contended), invalidates
matching L1 sessions, drops the existing commit_dir, then re-runs
the standard build pipeline. Order is L1-then-L2 so any crash
leaves a self-consistent state recoverable by the next query.

Exposes orchestrator helpers as pub(crate) for reuse."
```

---

## Task 4: admin index --force wiring + clap cleanup + idempotent skip

**Files:**
- Modify: `crates/cgn-cli/src/commands/admin/index.rs`
- Create: `crates/cgn-cli/tests/admin_index_force_test.rs`

- [ ] **Step 1: Write failing CLI integration tests**

Create `crates/cgn-cli/tests/admin_index_force_test.rs`:

```rust
use std::process::Command;

fn cgn_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") { p.pop(); }
    p.join("cgn")
}

fn git_init(p: &std::path::Path) -> String {
    Command::new("git").arg("-C").arg(p).args(["init", "-q"]).status().unwrap();
    Command::new("git").arg("-C").arg(p)
        .args(["config", "user.email", "t@t"]).status().unwrap();
    Command::new("git").arg("-C").arg(p)
        .args(["config", "user.name", "t"]).status().unwrap();
    std::fs::write(p.join("hello.rs"), "fn hello() {}").unwrap();
    Command::new("git").arg("-C").arg(p).args(["add", "."]).status().unwrap();
    Command::new("git").arg("-C").arg(p)
        .args(["commit", "-qm", "init"]).status().unwrap();
    let o = Command::new("git").arg("-C").arg(p)
        .args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

#[test]
fn admin_index_without_force_builds_when_l2_absent() {
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());

    let out = Command::new(cgn_bin())
        .env("HOME", home.path())
        .args(["admin", "index", "--repo"]).arg(wt.path())
        .output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("l2.built"), "expected l2.built in stderr: {stderr}");
}

#[test]
fn admin_index_without_force_skips_when_l2_exists() {
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());

    // First build
    Command::new(cgn_bin())
        .env("HOME", home.path())
        .args(["admin", "index", "--repo"]).arg(wt.path())
        .status().unwrap();
    // Second run — should skip
    let out = Command::new(cgn_bin())
        .env("HOME", home.path())
        .args(["admin", "index", "--repo"]).arg(wt.path())
        .output().unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("l2.exists"), "expected l2.exists in stderr: {stderr}");
    assert!(stderr.contains("--force to rebuild"));
}

#[test]
fn admin_index_with_force_rebuilds_existing_l2() {
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());

    Command::new(cgn_bin())
        .env("HOME", home.path())
        .args(["admin", "index", "--repo"]).arg(wt.path())
        .status().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let out = Command::new(cgn_bin())
        .env("HOME", home.path())
        .args(["admin", "index", "--repo"]).arg(wt.path()).arg("--force")
        .output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("l2.rebuilt"), "expected l2.rebuilt: {stderr}");
}

#[test]
fn admin_index_rejects_no_cache_flag() {
    let out = Command::new(cgn_bin())
        .args(["admin", "index", "--repo", "/tmp/x", "--no-cache"])
        .output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unexpected argument") || stderr.contains("--no-cache"),
            "expected clap rejection: {stderr}");
}

#[test]
fn admin_index_rejects_embeddings_flag() {
    let out = Command::new(cgn_bin())
        .args(["admin", "index", "--repo", "/tmp/x", "--embeddings"])
        .output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn admin_index_rejects_drop_embeddings_flag() {
    let out = Command::new(cgn_bin())
        .args(["admin", "index", "--repo", "/tmp/x", "--drop-embeddings"])
        .output().unwrap();
    assert!(!out.status.success());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cgn-cli --test admin_index_force_test -- --test-threads=1 2>&1 | tail -15`

Expected: build first (`cargo build -p cgn-cli`). Tests should fail because:
- `--force` is still warn-no-op (no `l2.rebuilt` output)
- L2-exists case still does a full rebuild instead of `l2.exists` message
- `--no-cache / --embeddings / --drop-embeddings` are still accepted

- [ ] **Step 3: Rewrite IndexArgs (remove dead flags)**

Modify `crates/cgn-cli/src/commands/admin/index.rs` lines 20-49 — replace `IndexArgs` struct:

```rust
#[derive(Args, Debug, Clone)]
pub struct IndexArgs {
    #[arg(long)]
    pub repo: String,

    /// Force-rebuild L2 at the target SHA. Drops the existing L2 dir
    /// and any orphan `.building/`, invalidates L1 sessions that have
    /// overlays for this SHA (clean sessions kept), then rebuilds.
    /// Without `--force`, an existing L2 is reused.
    #[arg(long, default_value_t = false)]
    pub force: bool,

    /// Optional path to write a JSONL dump of every resolver decision.
    /// Used by the oracle verification harness; off by default.
    /// Spec: docs/specs/2026-05-15-resolver-oracle-harness.md
    #[arg(long)]
    pub dump_resolver: Option<std::path::PathBuf>,

    /// Suppress progress output (timings, "Graph saved", etc.). Used by
    /// auto_ensure when an agent command transparently rebuilds; the
    /// agent's stdout must stay clean and the user sees only the single
    /// "Index refreshed" notice from the wrapper.
    #[arg(skip)]
    pub quiet: bool,
}
```

Three flags deleted: `no_cache`, `embeddings`, `drop_embeddings`. If any of these field names appear elsewhere in the codebase (constructed with `IndexArgs { no_cache: ..., ... }`), grep and fix:

```bash
grep -rn "IndexArgs {" crates/cgn-cli/src/ | head
grep -rn "no_cache\b" crates/cgn-cli/src/ | head
```

- [ ] **Step 4: Rewrite run() with 3-way match**

Replace `pub fn run(args: IndexArgs) -> Result<(), String>` body at `crates/cgn-cli/src/commands/admin/index.rs:303-325`:

```rust
pub fn run(args: IndexArgs) -> Result<(), String> {
    let worktree = std::path::PathBuf::from(&args.repo);
    if !worktree.exists() {
        return Err(format!("repo path does not exist: {}", worktree.display()));
    }

    let start = std::time::Instant::now();
    let sha = head_sha_hex(&worktree).map_err(|e| format!("git rev-parse HEAD: {e}"))?;
    let commit_dir = locate_commit_dir(&worktree, &sha)
        .map_err(|e| format!("locate commit dir: {e}"))?;

    match (args.force, commit_dir) {
        (false, Some(existing)) => {
            if !args.quiet {
                let st = detect_source_type(&existing);
                eprintln!(
                    "l2.exists sha={} type={:?} elapsed={:.2}s (use --force to rebuild)",
                    &sha[..8], st, start.elapsed().as_secs_f32(),
                );
            }
            Ok(())
        }
        (false, None) => {
            let r = crate::build::orchestrator::build_l2(&worktree, None)
                .map_err(|e| format!("build_l2 failed: {e}"))?;
            if !args.quiet {
                eprintln!(
                    "l2.built sha={} type={:?} elapsed={:.2}s",
                    &r.sha_hex[..8], r.source_type, start.elapsed().as_secs_f32(),
                );
            }
            Ok(())
        }
        (true, _) => {
            let r = crate::build::force::force_rebuild_l2(&worktree, &sha)
                .map_err(|e| format!("force rebuild failed: {e}"))?;
            if !args.quiet {
                eprintln!(
                    "l2.rebuilt sha={} type={:?} elapsed={:.2}s l1_kept={} l1_invalidated={}",
                    &r.sha_hex[..8], r.source_type, start.elapsed().as_secs_f32(),
                    r.invalidate_report.kept, r.invalidate_report.invalidated,
                );
            }
            Ok(())
        }
    }
}

fn head_sha_hex(worktree: &std::path::Path) -> std::io::Result<String> {
    let out = std::process::Command::new("git")
        .arg("-C").arg(worktree)
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other("git rev-parse HEAD failed"));
    }
    Ok(std::str::from_utf8(&out.stdout)
        .map_err(std::io::Error::other)?
        .trim().to_string())
}

fn locate_commit_dir(
    worktree: &std::path::Path,
    sha: &str,
) -> std::io::Result<Option<std::path::PathBuf>> {
    let home_cgn = cgn_core::registry::resolve_home_cgn();
    let repo_dir_name = crate::repo_identity::repo_dir_name_for_cwd(worktree)?;
    let commits = home_cgn.join(&repo_dir_name).join("commits");
    if !commits.exists() {
        return Ok(None);
    }
    let idx = crate::commit_lookup::CommitIndex::scan(&commits)?;
    let sha_bytes = sha_hex_to_bytes(sha).ok_or_else(|| std::io::Error::other("invalid sha hex"))?;
    Ok(idx.find(&sha_bytes).map(|name| commits.join(name)))
}

fn sha_hex_to_bytes(hex: &str) -> Option<[u8; 20]> {
    if hex.len() != 40 { return None; }
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&hex[i*2..i*2+2], 16).ok()?;
    }
    Some(out)
}

fn detect_source_type(commit_dir: &std::path::Path) -> cgn_core::registry::SourceType {
    cgn_core::registry::CommitBuildMeta::read(&commit_dir.join("meta.json"))
        .map(|m| m.source_type)
        .unwrap_or(cgn_core::registry::SourceType::Commit)
}
```

(If `head_sha_hex` already exists as a private fn higher in the file, you can reuse it; otherwise add the helpers above.)

Also delete lines 303-309 (the `warn` block) — replaced by the new run() body.

- [ ] **Step 5: Run tests**

Run: `cargo test -p cgn-cli --test admin_index_force_test -- --test-threads=1 2>&1 | tail -20`

Expected: 6 tests pass.

- [ ] **Step 6: Run the broader test suite to catch regressions**

Run: `cargo test -p cgn-cli 2>&1 | tail -30`

Watch for failures in any test that constructed `IndexArgs` with the deleted fields. If any, update those tests to drop the fields.

- [ ] **Step 7: Commit**

```bash
git add crates/cgn-cli/src/commands/admin/index.rs \
        crates/cgn-cli/tests/admin_index_force_test.rs
git commit -m "feat(admin/index): wire --force to force_rebuild_l2; idempotent skip; drop dead flags

--force now actually drops the L2 dir + invalidates matching L1
sessions before rebuilding. Without --force, an existing L2 at the
target SHA triggers a silent skip with hint instead of attempting
a wasteful re-build that would fail at the atomic rename.

Removes --no-cache, --embeddings, --drop-embeddings — all warn-no-op
in v2 with no recoverable semantics (embedding pipeline was hard-
deleted in #51; --no-cache referred to a v1 per-file cache that
content-addressed L2 dirs already subsume)."
```

---

## Task 5: Engine::open SessionState dispatch + GraphView

**Files:**
- Modify: `crates/cgn-cli/src/engine.rs`
- Test: `crates/cgn-cli/tests/engine_session_state_test.rs` (new)

- [ ] **Step 1: Write failing tests**

Create `crates/cgn-cli/tests/engine_session_state_test.rs`:

```rust
use cgn_cli::engine::Engine;
use std::fs;
use std::path::Path;

// Reuse setup from session_state_test.rs — copy the helpers here OR move them
// into crates/cgn-cli/tests/common/ and share. Plan-writer choice:
// inline for now since shared mod requires Cargo.toml integration test plumbing.

const SHA: &str = "abc123def456789012345678901234567890abcd";

fn write_minimal_l2(commit_dir: &Path) {
    fs::create_dir_all(commit_dir).unwrap();
    // Write a minimal but valid graph.bin: empty ZeroCopyGraph rkyv archive.
    // Use the same path that build_l2 produces — easiest is to invoke build_l2
    // directly. For unit test isolation, use a real `cargo build`-produced
    // graph.bin fixture from another test's tempdir; alternatively, invoke
    // build_l2 in this test's setup.
    let g = cgn_core::graph::ZeroCopyGraph::default();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap();
    fs::write(commit_dir.join("graph.bin"), &bytes).unwrap();
    let cm = cgn_core::registry::CommitBuildMeta {
        version: 1, sha: SHA.into(),
        source_type: cgn_core::registry::SourceType::Branch,
        source_id: Some("main".into()),
        built_from_worktree: "/tmp/wt".into(),
        built_at: "2026-05-17T10:00:00Z".into(),
        parent_sha: None, node_count: 0,
        embedding_status: cgn_core::registry::EmbeddingStatus::None,
        refs_at_build: vec![], refs_seen_since: vec![],
    };
    cgn_core::registry::CommitBuildMeta::write_atomic(
        &commit_dir.join("meta.json"), &cm).unwrap();
}

#[test]
fn engine_open_pure_reference_loads_l2only() {
    let tmp = tempfile::tempdir().unwrap();
    let dirname = format!("branch_main__{}", SHA);
    let commit_dir = tmp.path().join("commits").join(&dirname);
    write_minimal_l2(&commit_dir);

    // Set up session
    let sd = tmp.path().join("sessions").join("sid_pure");
    fs::create_dir_all(&sd).unwrap();
    let sm = cgn_core::session::SessionMeta {
        version: 1, session_id: "sid_pure".into(), pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: SHA.into(),
        source_worktree: "/tmp/wt".into(),
        overlay_version: 0,
    };
    cgn_core::session::SessionMeta::write_atomic(
        &sd.join("session_meta.json"), &sm).unwrap();
    cgn_core::session::DirtyFiles::write_atomic(
        &sd.join("dirty_files.json"),
        &cgn_core::session::DirtyFiles::empty(),
    ).unwrap();

    let engine = Engine::open(tmp.path(), "sid_pure").unwrap();
    assert!(matches!(
        engine.view(),
        cgn_cli::engine::GraphView::L2Only(_)
    ));
}

#[test]
fn engine_open_stale_session_returns_err() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("sessions/sid_broken")).unwrap();
    let r = Engine::open(tmp.path(), "sid_broken");
    assert!(r.is_err(), "Stale session should fail to open");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cgn-cli --test engine_session_state_test 2>&1 | tail -15`

Expected: compile error — `Engine::open`, `Engine::view`, `GraphView::L2Only` don't exist.

- [ ] **Step 3: Add GraphView + Engine::open**

Modify `crates/cgn-cli/src/engine.rs`. After the existing `Engine` impl block, add:

```rust
/// Discriminated view over the L2 graph plus an optional L1 overlay.
/// `L2Only` is the PureReference fast-path: the engine guarantees no
/// `graph_overlay/` access (invariant F5).
pub enum GraphView {
    L2Only,
    L2WithOverlay,
}

impl Engine {
    /// New v2 constructor: classify the session and pick the right load path.
    /// PureReference loads L2 only and skips overlay merge entirely.
    /// AugmentedReference loads L2 + records the overlay dir so query layers
    /// can merge (merge implementation deferred to P2 of the follow-up tracker).
    /// Stale sessions are rejected; callers should fall back to a fresh session.
    pub fn open(repo_root: &Path, sid: &str) -> io::Result<Self> {
        let state = crate::session::state::classify(repo_root, sid);
        match state {
            cgn_core::session::SessionState::PureReference { l2_dirname, .. } => {
                let l2_dir = repo_root.join("commits").join(&l2_dirname);
                let mut eng = Self::load(l2_dir.join("graph.bin"))?;
                eng.view = GraphView::L2Only;
                Ok(eng)
            }
            cgn_core::session::SessionState::AugmentedReference { l2_dirname, .. } => {
                let l2_dir = repo_root.join("commits").join(&l2_dirname);
                let overlay_dir = repo_root.join("sessions").join(sid);
                let mut eng = Self::load(l2_dir.join("graph.bin"))?;
                eng.overlay_dir = Some(overlay_dir);
                eng.view = GraphView::L2WithOverlay;
                Ok(eng)
            }
            cgn_core::session::SessionState::Stale { reason } => {
                Err(io::Error::other(format!(
                    "session stale: {reason:?}; remove via `cgn admin sessions reset <id>`"
                )))
            }
        }
    }

    pub fn view(&self) -> &GraphView {
        &self.view
    }
}
```

Also add a `view: GraphView` field to the `Engine` struct (defaulting to `L2WithOverlay` in the existing `load` path to preserve current behavior):

```rust
pub struct Engine {
    mmap: Mmap,
    graph_path: PathBuf,
    overlay_dir: Option<PathBuf>,
    view: GraphView,
}
```

Initialize `view: GraphView::L2WithOverlay` in `Engine::load` (back-compat — callers using `load` go through whatever path they were on before).

- [ ] **Step 4: Run tests**

Run: `cargo test -p cgn-cli --test engine_session_state_test 2>&1 | tail -15`

Expected: 2 tests pass.

- [ ] **Step 5: Run broader suite for regressions**

Run: `cargo test -p cgn-cli 2>&1 | tail -25`

Watch for any test that pattern-matches on the `Engine` struct fields — they may need to construct `GraphView`.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/engine.rs \
        crates/cgn-cli/tests/engine_session_state_test.rs
git commit -m "feat(engine): SessionState-driven Engine::open + GraphView fast-path

Engine::open dispatches on SessionState: PureReference yields a
L2Only view (no overlay touch), AugmentedReference records the
overlay dir for later merge (P2), Stale errors out. Engine::load
remains as the back-compat constructor for callers that don't
have a session id.

Real overlay merge for AugmentedReference still deferred to P2."
```

---

## Task 6: admin sessions list (minimal) with STATE column

**Files:**
- Create: `crates/cgn-cli/src/commands/admin/sessions.rs`
- Modify: `crates/cgn-cli/src/commands/admin/mod.rs`
- Test: `crates/cgn-cli/tests/admin_sessions_list_test.rs`

- [ ] **Step 1: Write failing test**

Create `crates/cgn-cli/tests/admin_sessions_list_test.rs`:

```rust
use std::process::Command;

fn cgn_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") { p.pop(); }
    p.join("cgn")
}

#[test]
fn admin_sessions_list_runs_with_empty_home() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(cgn_bin())
        .env("HOME", home.path())
        .args(["admin", "sessions", "list"])
        .output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn admin_sessions_list_json_emits_empty_array() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(cgn_bin())
        .env("HOME", home.path())
        .args(["admin", "sessions", "list", "--json"])
        .output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(parsed.is_array());
    assert_eq!(parsed.as_array().unwrap().len(), 0);
}

#[test]
fn admin_sessions_list_shows_pure_reference_state() {
    let home = tempfile::tempdir().unwrap();
    // Build an L2 + 1 clean session by hand under $HOME/.cgn/
    let repo_root = home.path().join(".cgn/myrepo__deadbeef");
    let commit_dir = repo_root.join("commits/branch_main__abc123def456789012345678901234567890abcd");
    std::fs::create_dir_all(&commit_dir).unwrap();
    let cm = cgn_core::registry::CommitBuildMeta {
        version: 1,
        sha: "abc123def456789012345678901234567890abcd".into(),
        source_type: cgn_core::registry::SourceType::Branch,
        source_id: Some("main".into()),
        built_from_worktree: "/tmp/wt".into(),
        built_at: "2026-05-17T10:00:00Z".into(),
        parent_sha: None, node_count: 0,
        embedding_status: cgn_core::registry::EmbeddingStatus::None,
        refs_at_build: vec![], refs_seen_since: vec![],
    };
    cgn_core::registry::CommitBuildMeta::write_atomic(
        &commit_dir.join("meta.json"), &cm).unwrap();

    let sd = repo_root.join("sessions/sid_a");
    std::fs::create_dir_all(&sd).unwrap();
    let sm = cgn_core::session::SessionMeta {
        version: 1, session_id: "sid_a".into(), pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: "abc123def456789012345678901234567890abcd".into(),
        source_worktree: "/tmp/wt".into(),
        overlay_version: 0,
    };
    cgn_core::session::SessionMeta::write_atomic(
        &sd.join("session_meta.json"), &sm).unwrap();
    cgn_core::session::DirtyFiles::write_atomic(
        &sd.join("dirty_files.json"),
        &cgn_core::session::DirtyFiles::empty(),
    ).unwrap();

    let out = Command::new(cgn_bin())
        .env("HOME", home.path())
        .args(["admin", "sessions", "list", "--json"])
        .output().unwrap();
    assert!(out.status.success());
    let parsed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["state"]["kind"], "pure_reference");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cgn-cli --test admin_sessions_list_test -- --test-threads=1 2>&1 | tail -15`

Expected: clap rejects the `sessions` subcommand (it doesn't exist yet).

- [ ] **Step 3: Add Sessions variant to AdminCommands**

Modify `crates/cgn-cli/src/commands/admin/mod.rs`:

```rust
//! `cgn admin` subcommand namespace — registry / hooks / destructive ops.
//! Hidden from top-level `cgn --help` per spec §4.

use clap::Subcommand;

pub mod claude_code;
pub mod config;
pub mod drop;
pub mod group;
pub mod index;
pub mod install_hook;
pub mod prune;
pub mod sessions;

#[derive(Subcommand, Debug)]
pub enum AdminCommands {
    /// Install git ref-transaction hook for branch tracking (or Claude Code hooks with --claude-code)
    InstallHook(install_hook::InstallHookArgs),
    /// Remove Claude Code hook entries from settings.json
    UninstallHook(claude_code::UninstallHookArgs),
    /// Show Claude Code hook install status
    Status(claude_code::StatusArgs),
    /// Delete a repo's index data + registry entry
    Drop(drop::DropArgs),
    /// Remove orphan index dirs not in registry
    Prune(prune::PruneArgs),
    /// Interactive TOML config editor
    Config(config::ConfigArgs),
    /// Manage repo group membership
    Group {
        #[command(subcommand)]
        command: group::GroupCommands,
    },
    /// Build or refresh the graph (explicit / bulk)
    Index(index::IndexArgs),
    /// List / inspect L1 sessions
    Sessions {
        #[command(subcommand)]
        command: sessions::SessionsCommand,
    },
    /// Run MCP server (serve) or list exposed tools (tools).
    Mcp(crate::commands::mcp::McpArgs),
    /// Diff resolver dump against language oracle (cgn-dev QA)
    VerifyResolver(crate::commands::verify_resolver::VerifyResolverArgs),
}

pub fn run(cmd: AdminCommands, root_cmd: clap::Command) -> Result<(), cgn_core::CgnError> {
    match cmd {
        AdminCommands::InstallHook(args) => install_hook::run(args),
        AdminCommands::UninstallHook(args) => claude_code::run_uninstall(args),
        AdminCommands::Status(args) => claude_code::run_status(args),
        AdminCommands::Drop(args) => drop::run(args),
        AdminCommands::Prune(args) => prune::run(args),
        AdminCommands::Config(args) => config::run(args),
        AdminCommands::Group { command } => group::run(command),
        AdminCommands::Index(args) => index::run(args).map_err(cgn_core::CgnError::Output),
        AdminCommands::Sessions { command } => sessions::run(command).map_err(cgn_core::CgnError::Output),
        AdminCommands::Mcp(args) => crate::commands::mcp::run(args, root_cmd),
        AdminCommands::VerifyResolver(args) => crate::commands::verify_resolver::run(args),
    }
}
```

- [ ] **Step 4: Create sessions.rs subcommand**

Create `crates/cgn-cli/src/commands/admin/sessions.rs`:

```rust
//! `cgn admin sessions list` — inspect L1 sessions under all repos.
//! reset / sweep variants deferred (parent spec §11.2 follow-up).

use clap::{Args, Subcommand};
use cgn_core::registry::resolve_home_cgn;
use cgn_core::session::{SessionMeta, SessionState, StaleReason};
use std::fs;
use std::io;

#[derive(Subcommand, Debug)]
pub enum SessionsCommand {
    /// List active L1 sessions across all repos under ~/.cgn/
    List(ListArgs),
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Emit JSON instead of the human table.
    #[arg(long, default_value_t = false)]
    pub json: bool,
}

pub fn run(cmd: SessionsCommand) -> Result<(), String> {
    match cmd {
        SessionsCommand::List(args) => run_list(args).map_err(|e| e.to_string()),
    }
}

#[derive(serde::Serialize)]
struct ListRow {
    session_id: String,
    repo: String,
    base_sha: String,
    state: StateView,
    last_touched: String,
}

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StateView {
    PureReference { l2_dirname: String },
    AugmentedReference { l2_dirname: String, fragment_count: usize },
    Stale { reason: String },
}

fn run_list(args: ListArgs) -> io::Result<()> {
    let home_cgn = resolve_home_cgn();
    let rows = collect_rows(&home_cgn)?;

    if args.json {
        println!("{}", serde_json::to_string(&rows).map_err(io::Error::other)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("(no sessions)");
        return Ok(());
    }
    println!("{:<24} {:<24} {:<8} {:<22} {}",
             "SESSION", "REPO", "BASE_SHA", "STATE", "LAST_TOUCHED");
    for r in &rows {
        let state_text = match &r.state {
            StateView::PureReference { .. } => "PureReference".to_string(),
            StateView::AugmentedReference { fragment_count, .. } =>
                format!("Augmented ({})", fragment_count),
            StateView::Stale { reason } => format!("Stale({})", reason),
        };
        let base = if r.base_sha.is_empty() {
            "--------".to_string()
        } else {
            r.base_sha[..8.min(r.base_sha.len())].to_string()
        };
        println!("{:<24} {:<24} {:<8} {:<22} {}",
                 r.session_id, r.repo, base, state_text, r.last_touched);
    }
    Ok(())
}

fn collect_rows(home_cgn: &std::path::Path) -> io::Result<Vec<ListRow>> {
    let mut out = vec![];
    if !home_cgn.exists() {
        return Ok(out);
    }
    for repo_entry in fs::read_dir(home_cgn)? {
        let repo_entry = repo_entry?;
        let repo_dir = repo_entry.path();
        if !repo_dir.is_dir() {
            continue;
        }
        let repo_name = match repo_dir.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let sessions_dir = repo_dir.join("sessions");
        if !sessions_dir.exists() {
            continue;
        }
        for s_entry in fs::read_dir(&sessions_dir)? {
            let s_entry = s_entry?;
            let s_path = s_entry.path();
            if !s_path.is_dir() {
                continue;
            }
            let sid = match s_path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            if sid.starts_with('.') || sid.contains(".stale-") || sid.contains(".dead") {
                continue;
            }
            let state = crate::session::state::classify(&repo_dir, sid);
            let (base_sha, state_view) = match &state {
                SessionState::PureReference { base_sha, l2_dirname } => (
                    base_sha.clone(),
                    StateView::PureReference { l2_dirname: l2_dirname.clone() },
                ),
                SessionState::AugmentedReference { base_sha, l2_dirname, fragment_count } => (
                    base_sha.clone(),
                    StateView::AugmentedReference {
                        l2_dirname: l2_dirname.clone(),
                        fragment_count: *fragment_count,
                    },
                ),
                SessionState::Stale { reason } => (
                    SessionMeta::read(&s_path.join("session_meta.json"))
                        .map(|m| m.base_sha)
                        .unwrap_or_default(),
                    StateView::Stale { reason: reason.short().to_string() },
                ),
            };
            let last_touched = SessionMeta::read(&s_path.join("session_meta.json"))
                .map(|m| m.last_touched)
                .unwrap_or_else(|_| "?".to_string());
            out.push(ListRow {
                session_id: sid.to_string(),
                repo: repo_name.clone(),
                base_sha,
                state: state_view,
                last_touched,
            });
        }
    }
    Ok(out)
}
```

- [ ] **Step 5: Verify StaleReason exposes short()**

Already added in Task 1 Step 3 (`StaleReason::short()`). Confirm:

```bash
grep -n "fn short" crates/cgn-core/src/session/state.rs
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p cgn-cli --test admin_sessions_list_test -- --test-threads=1 2>&1 | tail -15`

Expected: 3 tests pass.

- [ ] **Step 7: Sanity check `cgn admin --help` shows sessions**

```bash
cargo run -p cgn-cli --bin cgn -- admin --help 2>&1 | grep -i session
```

Expected: a line like `sessions   List / inspect L1 sessions`.

- [ ] **Step 8: Commit**

```bash
git add crates/cgn-cli/src/commands/admin/sessions.rs \
        crates/cgn-cli/src/commands/admin/mod.rs \
        crates/cgn-cli/tests/admin_sessions_list_test.rs
git commit -m "feat(admin/sessions): list subcommand with STATE column (PureReference / Augmented / Stale)

Walks ~/.cgn/<repo>/sessions/<sid>/ across all repos, classifies via
SessionState, emits table or --json. PureReference shows 'PureReference',
AugmentedReference shows 'Augmented (N)' with fragment count, Stale
shows 'Stale(<short-reason>)'. reset / sweep subcommands deferred to
parent spec §11.2 follow-up."
```

---

## Task 7: Concurrent --force integration test

**Files:**
- Test: `crates/cgn-cli/tests/force_rebuild_concurrent_test.rs`

- [ ] **Step 1: Write the concurrency test**

Create `crates/cgn-cli/tests/force_rebuild_concurrent_test.rs`:

```rust
use std::process::Command;
use std::thread;

fn cgn_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") { p.pop(); }
    p.join("cgn")
}

fn git_init(p: &std::path::Path) -> String {
    Command::new("git").arg("-C").arg(p).args(["init", "-q"]).status().unwrap();
    Command::new("git").arg("-C").arg(p)
        .args(["config", "user.email", "t@t"]).status().unwrap();
    Command::new("git").arg("-C").arg(p)
        .args(["config", "user.name", "t"]).status().unwrap();
    std::fs::write(p.join("hello.rs"), "fn hello() {}").unwrap();
    Command::new("git").arg("-C").arg(p).args(["add", "."]).status().unwrap();
    Command::new("git").arg("-C").arg(p)
        .args(["commit", "-qm", "init"]).status().unwrap();
    let o = Command::new("git").arg("-C").arg(p)
        .args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

#[test]
fn two_concurrent_force_rebuilds_both_succeed_with_one_final_commit_dir() {
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let _sha = git_init(wt.path());
    let home_path = home.path().to_path_buf();
    let wt_path = wt.path().to_path_buf();

    // Seed an initial L2 so we exercise the drop-existing path
    Command::new(cgn_bin())
        .env("HOME", &home_path)
        .args(["admin", "index", "--repo"]).arg(&wt_path)
        .status().unwrap();

    let h1 = {
        let home_path = home_path.clone();
        let wt_path = wt_path.clone();
        thread::spawn(move || {
            Command::new(cgn_bin())
                .env("HOME", &home_path)
                .args(["admin", "index", "--repo"]).arg(&wt_path).arg("--force")
                .output().unwrap()
        })
    };
    let h2 = {
        let home_path = home_path.clone();
        let wt_path = wt_path.clone();
        thread::spawn(move || {
            Command::new(cgn_bin())
                .env("HOME", &home_path)
                .args(["admin", "index", "--repo"]).arg(&wt_path).arg("--force")
                .output().unwrap()
        })
    };
    let o1 = h1.join().unwrap();
    let o2 = h2.join().unwrap();

    assert!(o1.status.success(), "process 1 failed: {}",
            String::from_utf8_lossy(&o1.stderr));
    assert!(o2.status.success(), "process 2 failed: {}",
            String::from_utf8_lossy(&o2.stderr));

    // Only one commit_dir for this SHA + no leftover .building
    let commits = home_path.join(".cgn");
    let mut dir_count = 0;
    let mut building_count = 0;
    fn walk(p: &std::path::Path, c: &mut usize, b: &mut usize) {
        for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".building") { *b += 1; }
            if e.file_type().unwrap().is_dir() && name.starts_with("branch_") {
                *c += 1;
            } else if e.file_type().unwrap().is_dir() {
                walk(&e.path(), c, b);
            }
        }
    }
    walk(&commits, &mut dir_count, &mut building_count);
    assert_eq!(dir_count, 1, "expected exactly 1 commit dir, found {dir_count}");
    assert_eq!(building_count, 0, "expected no .building leftovers, found {building_count}");
}
```

- [ ] **Step 2: Run test**

Run: `cargo test -p cgn-cli --test force_rebuild_concurrent_test -- --test-threads=1 2>&1 | tail -15`

Expected: pass.

If it's flaky (timing-dependent), do NOT mark `#[ignore]` and move on — the spec promises this works. Debug: usually it's the second process not seeing the first's lock release. Add a 50ms stagger between thread spawns if needed.

- [ ] **Step 3: Commit**

```bash
git add crates/cgn-cli/tests/force_rebuild_concurrent_test.rs
git commit -m "test(force-rebuild): two concurrent --force on same SHA converge to one final L2

Verifies the attach-and-retake pattern: when two processes race to
--force the same SHA, both exit 0, no .building leftovers remain,
and exactly one commit_dir exists in commits/. Validates spec §4.5
attach semantics."
```

---

## Cross-Task Verification

After all 7 tasks:

- [ ] **Run full test suite**

```bash
cargo test -p cgn-cli -p cgn-core -- --test-threads=1 2>&1 | tail -40
```

Expected: all tests pass. Watch for:
- pre-existing failures in `search_batch.rs` / `cypher_aggregation.rs` — these are P1 in the follow-ups doc, NOT our regressions; leave them
- new failures in any test that referenced `IndexArgs.no_cache` etc.

- [ ] **Clippy**

```bash
cargo clippy -p cgn-cli -p cgn-core --tests 2>&1 | tail -30
```

Expected: no new warnings.

- [ ] **Manual smoke**

```bash
cd $(mktemp -d) && git init -q && echo 'fn x() {}' > a.rs && git add . && git commit -qm init
# Build (should land in $HOME/.cgn)
cargo run -q --bin cgn -- admin index --repo .
# Should skip
cargo run -q --bin cgn -- admin index --repo .
# Should force-rebuild
cargo run -q --bin cgn -- admin index --repo . --force
# Sessions list (likely empty)
cargo run -q --bin cgn -- admin sessions list
```

Expected output sequence: `l2.built ...` → `l2.exists ... (use --force to rebuild)` → `l2.rebuilt ...` → `(no sessions)`.

- [ ] **Update follow-ups doc**

Modify `docs/feat/2026-05-17-index-layout-followups.md` — strike `--force / --embeddings / --drop-embeddings / --no-cache` from P3 (mark done; reference this spec/plan).

```bash
git add docs/feat/2026-05-17-index-layout-followups.md
git commit -m "docs(followups): mark --force / cleanup flags as shipped (see force-rebuild spec)"
```

---

## Self-Review Checklist (done)

- [x] **Spec coverage:**
  - §3 SessionState — Task 1
  - §4 Force rebuild flow — Tasks 2 + 3 + 4
  - §4.5 Attach — Task 7 (concurrent test) + Task 3 impl
  - §5 Hot-path GraphView — Task 5
  - §6 Sessions list STATE — Task 6
  - §7 CLI surface changes — Task 4
  - §9 Invariants F1-F4 — Tasks 1-3 tests; F5 — Task 5 (text-level, lsof check is verification not enforcement); F6 — Task 7 partial (crash injection deferred — see below); F7 — Task 4 test
  - §10 Test plan — covered per task

- [x] **Placeholder scan:** no TBD / TODO / "implement later" — every step has exact code.

- [x] **Type consistency:** `SessionState`, `StaleReason`, `InvalidateReport`, `ForceRebuildResult`, `GraphView`, `SessionsCommand`, `ListArgs`, `ListRow`, `StateView` defined once each, names match across tasks.

- [x] **Gaps acknowledged:**
  - F5 strace/lsof enforcement not added — text-level invariant only, would need a separate verification harness. Listed as out-of-scope-for-plan.
  - F6 crash injection beyond "concurrent + idempotent" — deferred. The orchestrator already leaves `.building/` for GC sweep; adding kill-mid-step tests requires a test harness we don't have. Out of scope for this plan.
  - `admin sessions reset` / `admin sessions sweep` — explicitly deferred per parent spec §11.2 and user decision during brainstorming.
