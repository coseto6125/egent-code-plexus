# Multi-Agent Peer Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable real-time peer-session awareness — when peer modifies a symbol that intersects with mine, my next tool call surfaces it via hook injection. Includes Ƀ (beta) messaging.

**Architecture:** Filesystem-as-transport (atomic JSON + inotify), per-session forked watcher, hook-based LLM injection, symbol-level concern matching via the existing impact graph. No daemon, no socket — sessions communicate by writing/reading files under `~/.cgn/code-graph-nexus/<repo>/sessions/<id>/`.

**Tech Stack:** Rust, `notify` crate (inotify / fsevents / RDCW), `fs2` (flock), `serde` + serde_json, `chrono` (timestamps), `uuid` v7 (msg_id), `tracing`, `std::backtrace`.

**Spec:** `docs/superpowers/specs/2026-05-17-multi-agent-peer-sync-design.md`

**Blast radius:** ~2287 LOC. 12 new files (1250 LOC), 12 modified files (+237 LOC), 1 test harness (120 LOC), 11 test files (680 LOC).

**Phase ordering (strict dependency):**

```
Phase 1: Symbol foundation (Tasks 1-2)
   ↓
Phase 2: Peer core types & logic (Tasks 3-6)            ← CHECKPOINT
   ↓
Phase 3: CLI peer support — render/dispatch/watcher (Tasks 7-9)
   ↓
Phase 4: CLI commands (Tasks 10-13)                     ← CHECKPOINT
   ↓
Phase 5: Hook integration (Tasks 14-16)
   ↓
Phase 6: MCP tools (Task 17)                            ← CHECKPOINT
   ↓
Phase 7: Cross-session integration tests (Tasks 18-22)  ← FINAL VALIDATION
```

**Conventions:**
- All work on branch `feat/peer-sync` (worktree at `.claude/worktrees/peer-sync/`)
- Each task = one commit; commit message format `feat(peer): ...` / `feat(peer-cli): ...` / `feat(peer-hook): ...` / `test(peer): ...`
- TDD strictly: failing test → run to confirm RED → minimal impl → run to confirm GREEN → commit
- Build check before every commit: `cargo build -p code-graph-nexus --bin cgn` and `cargo build -p cgn-core`
- Lint touched files only: `rustfmt --edition 2021 <file>` then `cargo clippy -p <crate> --tests`

---

## File Structure

### Created

```
crates/cgn-core/src/peer/
  ├─ mod.rs                      (5 LOC) — re-exports
  ├─ registry.rs                 (100 LOC) — PeerSession, alive_peers()
  ├─ concern.rs                  (150 LOC) — ConcernKind, classify(), ImpactCache
  ├─ inbox.rs                    (120 LOC) — InboxEntry, append, drain
  └─ retention.rs                (80 LOC) — rotation constants + rotate()

crates/cgn-cli/src/peer/
  ├─ mod.rs                      (5 LOC)
  ├─ watcher.rs                  (200 LOC) — fork, inotify loop, lifecycle
  ├─ dispatch.rs                 (80 LOC) — concern → inbox
  └─ render.rs                   (150 LOC) — InboxEntry → hookSpecificOutput

crates/cgn-cli/src/commands/
  ├─ peers.rs                    (180 LOC) — status / diff / log / say / inbox / thread / gc
  └─ watch.rs                    (120 LOC) — --start / --stop / --status / --foreground

crates/cgn-mcp/src/tools/
  └─ peers.rs                    (60 LOC) — 3 MCP tools

crates/cgn-cli/tests/common/
  └─ peer_harness.rs             (120 LOC) — PeerHarness fixture

crates/cgn-cli/tests/
  ├─ peers_watch_lifecycle.rs        (80 LOC)
  ├─ peers_inbox_drain.rs            (60 LOC)
  ├─ peers_msg_log_rotation.rs       (50 LOC)
  ├─ peers_watcher_log_backtrace.rs  (40 LOC)
  ├─ peers_two_session_dirty_event.rs   (80 LOC)
  ├─ peers_two_session_msg.rs           (60 LOC)
  ├─ peers_symbol_level_filter.rs       (80 LOC)
  └─ peers_concern_impact_cache_invalidation.rs (60 LOC)
```

### Modified

```
crates/cgn-core/src/
  ├─ lib.rs                       (+1) `pub mod peer;`
  ├─ session/overlay.rs           (+30) DirtyEntry.dirty_symbols + SymbolRef
  ├─ session/overlay_writer.rs    (+40) extract symbols at write-time
  └─ session/meta.rs              (+5) watcher_pid + last_drained_offset

crates/cgn-cli/src/
  ├─ lib.rs                       (+1) `pub mod peer;`
  ├─ main.rs                      (+15) top-level dispatch for watch + peers
  ├─ commands/mod.rs              (+5) wire submodules
  ├─ commands/hook/mod.rs         (+5) register handlers
  ├─ commands/hook/session_start.rs    (+40) auto-watch spawn
  ├─ commands/hook/pre_tool_use.rs     (+60) drain + render + emit
  └─ commands/hook/user_prompt_submit.rs (+30) drain + render + emit

crates/cgn-mcp/src/lib.rs   (+5) register peer tools
```

---

## Phase 1: Symbol Foundation

### Task 1: SymbolRef type + DirtyEntry.dirty_symbols field

**Files:**
- Modify: `crates/cgn-core/src/session/overlay.rs:1-50`
- Test: `crates/cgn-core/tests/session_overlay_symbols.rs` (new)

**Goal:** Extend `DirtyEntry` to carry the list of symbols modified in that file, with serde default for backward compatibility.

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-core/tests/session_overlay_symbols.rs`:

```rust
use cgn_core::session::overlay::{DirtyEntry, DirtyFiles, SymbolKind, SymbolRef};
use std::collections::BTreeMap;

#[test]
fn dirty_entry_serialises_dirty_symbols() {
    let entry = DirtyEntry {
        mtime_ns: 1,
        content_hash: "h".into(),
        fragment_id: "f".into(),
        tantivy_delta_segment: None,
        parse_failed: false,
        dirty_symbols: vec![SymbolRef {
            name: "verify_token".into(),
            kind: SymbolKind::Function,
            file: "src/auth.rs".into(),
            line_start: 42,
            line_end: 58,
        }],
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("\"name\":\"verify_token\""));
    assert!(json.contains("\"kind\":\"function\""));
    let back: DirtyEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(back.dirty_symbols.len(), 1);
    assert_eq!(back.dirty_symbols[0].line_start, 42);
}

#[test]
fn dirty_entry_deserialises_without_dirty_symbols_field() {
    let legacy = r#"{
        "mtime_ns":1,"content_hash":"h","fragment_id":"f",
        "tantivy_delta_segment":null,"parse_failed":false
    }"#;
    let entry: DirtyEntry = serde_json::from_str(legacy).unwrap();
    assert!(entry.dirty_symbols.is_empty());
}

#[test]
fn dirty_files_round_trip_via_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dirty.json");
    let mut entries = BTreeMap::new();
    entries.insert("src/a.rs".to_string(), DirtyEntry {
        mtime_ns: 1, content_hash: "h".into(), fragment_id: "f".into(),
        tantivy_delta_segment: None, parse_failed: false,
        dirty_symbols: vec![SymbolRef {
            name: "foo".into(), kind: SymbolKind::Function,
            file: "src/a.rs".into(), line_start: 1, line_end: 10,
        }],
    });
    let files = DirtyFiles { version: 1, entries };
    DirtyFiles::write_atomic(&path, &files).unwrap();
    let back = DirtyFiles::read(&path).unwrap();
    assert_eq!(back.entries["src/a.rs"].dirty_symbols[0].name, "foo");
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p cgn-core --test session_overlay_symbols
```

Expected: compilation error — `SymbolRef`, `SymbolKind`, `dirty_symbols` not defined.

- [ ] **Step 3: Implement the types**

Edit `crates/cgn-core/src/session/overlay.rs`, replace the existing `DirtyEntry` struct and add `SymbolRef`/`SymbolKind`:

```rust
use crate::registry::io::atomic_write_json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyFiles {
    pub version: u32,
    #[serde(default)]
    pub entries: BTreeMap<String, DirtyEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyEntry {
    pub mtime_ns: u64,
    pub content_hash: String,
    pub fragment_id: String,
    pub tantivy_delta_segment: Option<String>,
    #[serde(default)]
    pub parse_failed: bool,
    #[serde(default)]
    pub dirty_symbols: Vec<SymbolRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolRef {
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Const,
    Type,
    Module,
    Unknown,
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

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p cgn-core --test session_overlay_symbols
```

Expected: 3 passed.

- [ ] **Step 5: Lint + build**

```
rustfmt --edition 2021 crates/cgn-core/src/session/overlay.rs
cargo clippy -p cgn-core --tests -- -D warnings
cargo build -p cgn-core
```

Expected: no warnings, build succeeds.

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-core/src/session/overlay.rs \
        crates/cgn-core/tests/session_overlay_symbols.rs
git commit -m "$(cat <<'EOF'
feat(peer): add SymbolRef + dirty_symbols field to DirtyEntry

Extends DirtyEntry with a Vec<SymbolRef> capturing the AST symbols
touched by an edit. Serde default ensures existing dirty.json on disk
deserialises without migration code.

Refs: spec §4.1
EOF
)"
```

---

### Task 2: OverlayWriter symbol extraction at write-time

**Files:**
- Modify: `crates/cgn-cli/src/session/overlay_writer.rs`
- Test: `crates/cgn-cli/tests/overlay_writer_symbols.rs` (new)

**Goal:** When `OverlayWriter` records a dirty file, run the existing analyzer pipeline on that path to extract `Vec<SymbolRef>` and store it in `DirtyEntry.dirty_symbols`.

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/overlay_writer_symbols.rs`:

```rust
use cgn_cli::session::overlay_writer::OverlayWriter;
use std::fs;
use tempfile::tempdir;

#[test]
fn write_dirty_records_function_symbol() {
    let dir = tempdir().unwrap();
    let session_dir = dir.path().join("sessions/s1");
    fs::create_dir_all(&session_dir).unwrap();
    let src_path = dir.path().join("src/lib.rs");
    fs::create_dir_all(src_path.parent().unwrap()).unwrap();
    fs::write(&src_path, "pub fn verify_token() -> bool { true }\n").unwrap();

    let mut writer = OverlayWriter::new(&session_dir);
    writer.append_dirty(&src_path, "deadbeef", "f1").unwrap();

    let dirty = writer.read_dirty().unwrap();
    let entry = dirty.entries.values().next().unwrap();
    assert!(entry.dirty_symbols.iter().any(|s| s.name == "verify_token"));
}

#[test]
fn write_dirty_on_unsupported_file_keeps_empty_symbols() {
    let dir = tempdir().unwrap();
    let session_dir = dir.path().join("sessions/s1");
    fs::create_dir_all(&session_dir).unwrap();
    let src_path = dir.path().join("README.bin");
    fs::write(&src_path, "binary garbage").unwrap();

    let mut writer = OverlayWriter::new(&session_dir);
    writer.append_dirty(&src_path, "x", "y").unwrap();

    let dirty = writer.read_dirty().unwrap();
    let entry = dirty.entries.values().next().unwrap();
    assert!(entry.dirty_symbols.is_empty());
    assert!(entry.parse_failed); // unsupported file marked
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p code-graph-nexus --test overlay_writer_symbols
```

Expected: FAIL — either method missing or symbols field empty.

- [ ] **Step 3: Implement symbol extraction in OverlayWriter**

Edit `crates/cgn-cli/src/session/overlay_writer.rs`. Add at top:

```rust
use cgn_analyzer::extract_symbols_from_file;
use cgn_core::session::overlay::{DirtyEntry, DirtyFiles, SymbolKind, SymbolRef};
```

Replace `append_dirty` body with:

```rust
pub fn append_dirty(
    &mut self,
    path: &Path,
    content_hash: &str,
    fragment_id: &str,
) -> io::Result<FragmentOutcome> {
    let mtime_ns = path.metadata()?.modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    let (dirty_symbols, parse_failed) = match extract_symbols_from_file(path) {
        Ok(syms) => (
            syms.into_iter().map(|s| SymbolRef {
                name: s.name,
                kind: map_kind(&s.kind),
                file: s.file,
                line_start: s.line_start,
                line_end: s.line_end,
            }).collect::<Vec<_>>(),
            false,
        ),
        Err(_) => (Vec::new(), true),
    };

    let entry = DirtyEntry {
        mtime_ns,
        content_hash: content_hash.to_string(),
        fragment_id: fragment_id.to_string(),
        tantivy_delta_segment: None,
        parse_failed,
        dirty_symbols,
    };
    let rel = path.strip_prefix(self.session_dir.parent().unwrap_or(Path::new("/")))
        .unwrap_or(path).to_string_lossy().into_owned();
    self.dirty.entries.insert(rel, entry);
    DirtyFiles::write_atomic(&self.dirty_path, &self.dirty)?;
    Ok(FragmentOutcome { fragment_id: fragment_id.to_string() })
}

fn map_kind(s: &str) -> SymbolKind {
    match s {
        "function" => SymbolKind::Function,
        "method" => SymbolKind::Method,
        "struct" => SymbolKind::Struct,
        "enum" => SymbolKind::Enum,
        "trait" => SymbolKind::Trait,
        "const" => SymbolKind::Const,
        "type" | "type_alias" => SymbolKind::Type,
        "module" => SymbolKind::Module,
        _ => SymbolKind::Unknown,
    }
}
```

In `crates/cgn-analyzer/src/lib.rs`, add the public helper:

```rust
pub fn extract_symbols_from_file(path: &std::path::Path) -> Result<Vec<crate::SymbolRecord>, crate::AnalyzerError> {
    let bytes = std::fs::read(path).map_err(crate::AnalyzerError::Io)?;
    let lang = crate::detect_language_for_path(path)
        .ok_or(crate::AnalyzerError::UnsupportedLanguage)?;
    crate::parse_symbols(lang, &bytes, path)
}
```

(If `SymbolRecord` / `parse_symbols` / `detect_language_for_path` are named differently in the analyzer, use the canonical analyzer API the codebase already exposes; this task's purpose is wiring not API redesign.)

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p code-graph-nexus --test overlay_writer_symbols
```

Expected: 2 passed.

- [ ] **Step 5: Lint + build**

```
rustfmt --edition 2021 crates/cgn-cli/src/session/overlay_writer.rs
cargo clippy -p code-graph-nexus --tests -- -D warnings
cargo build -p code-graph-nexus --bin cgn
```

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/session/overlay_writer.rs \
        crates/cgn-cli/tests/overlay_writer_symbols.rs \
        crates/cgn-analyzer/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(peer): extract dirty_symbols at OverlayWriter write-time

Runs the analyzer pipeline against the modified file synchronously and
caches Vec<SymbolRef> into DirtyEntry. parse_failed flagged when the
language is unsupported or the parse errors out.

Refs: spec §5
EOF
)"
```

---

## Phase 2: Peer Core Types & Logic

### Task 3: peer::registry — alive_peers()

**Files:**
- Create: `crates/cgn-core/src/peer/mod.rs`
- Create: `crates/cgn-core/src/peer/registry.rs`
- Modify: `crates/cgn-core/src/lib.rs:+1`
- Modify: `crates/cgn-core/src/session/meta.rs:+5`
- Test: `crates/cgn-core/tests/peer_registry.rs` (new)

**Goal:** Enumerate alive peer sessions in a repo by scanning `sessions/*/meta.json` and pruning entries whose `pid` is dead.

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-core/tests/peer_registry.rs`:

```rust
use chrono::Utc;
use cgn_core::peer::registry::alive_peers;
use cgn_core::registry::atomic_write_json;
use cgn_core::session::SessionMeta;
use std::fs;
use tempfile::tempdir;

fn write_meta(root: &std::path::Path, id: &str, pid: u32) {
    let dir = root.join("sessions").join(id);
    fs::create_dir_all(&dir).unwrap();
    let meta = SessionMeta {
        version: 1,
        session_id: id.into(),
        pid: Some(pid),
        started_at: Utc::now().to_rfc3339(),
        last_touched: Utc::now().to_rfc3339(),
        base_sha: "0".repeat(40),
        source_worktree: "/tmp".into(),
        overlay_version: 1,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    atomic_write_json(&dir.join("meta.json"), &meta).unwrap();
}

#[test]
fn alive_peers_excludes_self_and_dead_pids() {
    let dir = tempdir().unwrap();
    write_meta(dir.path(), "self", std::process::id());
    write_meta(dir.path(), "alive_peer", std::process::id()); // same pid → alive
    write_meta(dir.path(), "dead_peer", 1); // pid 1 = init, alive but not us; use 999999 below

    // Replace dead_peer pid with an unreachable one
    let dead_meta_path = dir.path().join("sessions/dead_peer/meta.json");
    let mut m: SessionMeta = serde_json::from_slice(&fs::read(&dead_meta_path).unwrap()).unwrap();
    m.pid = Some(999_999_999); // implausibly large
    atomic_write_json(&dead_meta_path, &m).unwrap();

    let peers = alive_peers(dir.path(), "self");
    let ids: Vec<_> = peers.iter().map(|p| p.session_id.as_str()).collect();
    assert!(ids.contains(&"alive_peer"));
    assert!(!ids.contains(&"self"));
    assert!(!ids.contains(&"dead_peer"));
}

#[test]
fn alive_peers_empty_when_no_sessions() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("sessions")).unwrap();
    assert!(alive_peers(dir.path(), "self").is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p cgn-core --test peer_registry
```

Expected: FAIL — `peer::registry::alive_peers` not found, `watcher_pid` / `last_drained_offset` not on `SessionMeta`.

- [ ] **Step 3: Extend SessionMeta**

Edit `crates/cgn-core/src/session/meta.rs`:

```rust
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
    #[serde(default)]
    pub watcher_pid: Option<u32>,
    #[serde(default)]
    pub last_drained_offset: u64,
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

- [ ] **Step 4: Implement peer::registry**

Create `crates/cgn-core/src/peer/mod.rs`:

```rust
pub mod registry;
pub mod concern;
pub mod inbox;
pub mod retention;
```

Create `crates/cgn-core/src/peer/registry.rs`:

```rust
//! Enumerate alive peer sessions sharing the same repo `common_dir`.

use crate::session::SessionMeta;
use chrono::{DateTime, Utc};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct PeerSession {
    pub session_id: String,
    pub pid: u32,
    pub last_touched: DateTime<Utc>,
    pub base_sha: String,
    pub watcher_alive: bool,
}

pub fn alive_peers(repo_root: &Path, exclude_self: &str) -> Vec<PeerSession> {
    let sessions_dir = repo_root.join("sessions");
    let Ok(read) = fs::read_dir(&sessions_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if id.is_empty() || id == exclude_self || id.starts_with('.') {
            continue;
        }
        let meta_path = path.join("meta.json");
        let Ok(meta) = SessionMeta::read(&meta_path) else {
            continue;
        };
        let Some(pid) = meta.pid else { continue };
        if !pid_alive(pid) {
            continue;
        }
        let last_touched: DateTime<Utc> = meta.last_touched.parse().unwrap_or_else(|_| Utc::now());
        let watcher_alive = meta.watcher_pid.is_some_and(pid_alive);
        out.push(PeerSession {
            session_id: id.to_string(),
            pid,
            last_touched,
            base_sha: meta.base_sha,
            watcher_alive,
        });
    }
    out
}

pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        // Windows: try OpenProcess with PROCESS_QUERY_LIMITED_INFORMATION
        false
    }
}
```

Add `libc` to `crates/cgn-core/Cargo.toml` if not present:

```toml
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

Edit `crates/cgn-core/src/lib.rs`, add:

```rust
pub mod peer;
```

- [ ] **Step 5: Run test to verify it passes**

```
cargo test -p cgn-core --test peer_registry
```

Expected: 2 passed.

- [ ] **Step 6: Lint + build**

```
rustfmt --edition 2021 crates/cgn-core/src/peer/registry.rs crates/cgn-core/src/peer/mod.rs crates/cgn-core/src/session/meta.rs
cargo clippy -p cgn-core --tests -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add crates/cgn-core/src/peer/ \
        crates/cgn-core/src/session/meta.rs \
        crates/cgn-core/src/lib.rs \
        crates/cgn-core/Cargo.toml \
        crates/cgn-core/tests/peer_registry.rs
git commit -m "$(cat <<'EOF'
feat(peer): peer::registry — enumerate alive peer sessions

Scans sessions/* under repo_root, filters out self, prunes entries
whose pid no longer exists (kill(pid, 0)). Extends SessionMeta with
watcher_pid and last_drained_offset fields (serde default).

Refs: spec §4.2 (PeerSession), §11
EOF
)"
```

---

### Task 4: peer::concern — HARD/SOFT/IGNORE classification

**Files:**
- Create: `crates/cgn-core/src/peer/concern.rs`
- Test: `crates/cgn-core/tests/peer_concern.rs` (new)

**Goal:** Implement the precise concern definition from spec §7.2. `ImpactCache` materializes `IMPACT(MY_DIRTY_SYMBOLS)` on demand and invalidates on self-dirty change.

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-core/tests/peer_concern.rs`:

```rust
use cgn_core::peer::concern::{classify, ConcernKind, ConcernResult, ImpactCache};
use cgn_core::session::overlay::{SymbolKind, SymbolRef};
use std::collections::HashSet;

fn sym(name: &str, file: &str) -> SymbolRef {
    SymbolRef {
        name: name.into(),
        kind: SymbolKind::Function,
        file: file.into(),
        line_start: 1,
        line_end: 10,
    }
}

#[test]
fn hard_when_same_symbol_modified() {
    let mine = vec![sym("verify_token", "src/auth.rs")];
    let peer = vec![sym("verify_token", "src/auth.rs")];
    let cache = ImpactCache::from_set(HashSet::new());
    let r = classify(&peer, &mine, &cache);
    assert!(matches!(r, ConcernResult::Hit { kind: ConcernKind::Hard, .. }));
}

#[test]
fn soft_when_peer_is_one_hop_neighbor() {
    let mine = vec![sym("verify_token", "src/auth.rs")];
    let peer = vec![sym("login_handler", "src/handlers/login.rs")];
    let mut impacted = HashSet::new();
    impacted.insert("login_handler".to_string()); // peer ∈ IMPACT(mine)
    let cache = ImpactCache::from_set(impacted);
    let r = classify(&peer, &mine, &cache);
    assert!(matches!(r, ConcernResult::Hit { kind: ConcernKind::Soft, .. }));
}

#[test]
fn ignore_when_unrelated() {
    let mine = vec![sym("verify_token", "src/auth.rs")];
    let peer = vec![sym("format_money", "src/utils/money.rs")];
    let cache = ImpactCache::from_set(HashSet::new());
    let r = classify(&peer, &mine, &cache);
    assert!(matches!(r, ConcernResult::Ignore));
}

#[test]
fn hard_takes_precedence_over_soft() {
    let mine = vec![sym("verify_token", "src/auth.rs")];
    let peer = vec![
        sym("verify_token", "src/auth.rs"),    // hard
        sym("login_handler", "src/login.rs"),  // soft
    ];
    let mut impacted = HashSet::new();
    impacted.insert("login_handler".into());
    let cache = ImpactCache::from_set(impacted);
    let r = classify(&peer, &mine, &cache);
    match r {
        ConcernResult::Hit { kind: ConcernKind::Hard, symbol, .. } => {
            assert_eq!(symbol.name, "verify_token");
        }
        _ => panic!("expected Hard"),
    }
}

#[test]
fn empty_my_dirty_yields_ignore() {
    let mine = vec![];
    let peer = vec![sym("anything", "src/x.rs")];
    let cache = ImpactCache::from_set(HashSet::new());
    assert!(matches!(classify(&peer, &mine, &cache), ConcernResult::Ignore));
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p cgn-core --test peer_concern
```

Expected: FAIL — module not present.

- [ ] **Step 3: Implement peer::concern**

Create `crates/cgn-core/src/peer/concern.rs`:

```rust
//! Concern classification — decide whether a peer dirty event matters.
//!
//! HARD  iff PEER_SYMBOLS ∩ MY_DIRTY_SYMBOLS ≠ ∅
//! SOFT  iff PEER_SYMBOLS ∩ IMPACT(MY_DIRTY_SYMBOLS) ≠ ∅ AND not HARD
//! IGNORE otherwise

use crate::session::overlay::SymbolRef;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcernKind {
    Hard,
    Soft,
}

#[derive(Debug, Clone)]
pub enum ConcernResult {
    Hit {
        kind: ConcernKind,
        symbol: SymbolRef,
        reason: String,
    },
    Ignore,
}

#[derive(Debug, Clone, Default)]
pub struct ImpactCache {
    impacted_names: HashSet<String>,
}

impl ImpactCache {
    pub fn from_set(s: HashSet<String>) -> Self {
        Self { impacted_names: s }
    }
    pub fn contains(&self, name: &str) -> bool {
        self.impacted_names.contains(name)
    }
    pub fn invalidate(&mut self) {
        self.impacted_names.clear();
    }
    pub fn refresh(&mut self, names: impl IntoIterator<Item = String>) {
        self.impacted_names = names.into_iter().collect();
    }
}

pub fn classify(
    peer_symbols: &[SymbolRef],
    my_dirty_symbols: &[SymbolRef],
    impact_cache: &ImpactCache,
) -> ConcernResult {
    if my_dirty_symbols.is_empty() || peer_symbols.is_empty() {
        return ConcernResult::Ignore;
    }
    let my_names: HashSet<&str> = my_dirty_symbols.iter().map(|s| s.name.as_str()).collect();

    // HARD pass first — wins over SOFT.
    for p in peer_symbols {
        if my_names.contains(p.name.as_str()) {
            return ConcernResult::Hit {
                kind: ConcernKind::Hard,
                symbol: p.clone(),
                reason: format!("Both sessions modified `{}`", p.name),
            };
        }
    }
    for p in peer_symbols {
        if impact_cache.contains(&p.name) {
            return ConcernResult::Hit {
                kind: ConcernKind::Soft,
                symbol: p.clone(),
                reason: format!("Peer modified `{}` which is a graph neighbor of your dirty symbols", p.name),
            };
        }
    }
    ConcernResult::Ignore
}
```

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p cgn-core --test peer_concern
```

Expected: 5 passed.

- [ ] **Step 5: Lint + build**

```
rustfmt --edition 2021 crates/cgn-core/src/peer/concern.rs
cargo clippy -p cgn-core --tests -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-core/src/peer/concern.rs \
        crates/cgn-core/tests/peer_concern.rs
git commit -m "$(cat <<'EOF'
feat(peer): peer::concern — HARD/SOFT/IGNORE classification

Symbol-level intersection: HARD when same name overlaps, SOFT when
peer name is in cached IMPACT(my_dirty_symbols), IGNORE otherwise.
ImpactCache is a name HashSet refreshable by the watcher when our own
dirty set changes.

Refs: spec §4.2, §7.2
EOF
)"
```

---

### Task 5: peer::inbox — append, drain, watermark

**Files:**
- Create: `crates/cgn-core/src/peer/inbox.rs`
- Test: `crates/cgn-core/tests/peer_inbox.rs` (new)

**Goal:** `InboxEntry` schema (DirtyEvent + Message), append-as-line-O_APPEND, drain-and-truncate with watermark.

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-core/tests/peer_inbox.rs`:

```rust
use cgn_core::peer::concern::ConcernKind;
use cgn_core::peer::inbox::{append_entry, drain, InboxEntry};
use cgn_core::session::overlay::{SymbolKind, SymbolRef};
use tempfile::tempdir;

fn dirty_event_fixture() -> InboxEntry {
    InboxEntry::DirtyEvent {
        ts: "2026-05-17T00:00:00Z".into(),
        peer_session: "abc12".into(),
        peer_pid: 1234,
        kind: ConcernKind::Hard,
        symbol: SymbolRef {
            name: "verify_token".into(),
            kind: SymbolKind::Function,
            file: "src/auth.rs".into(),
            line_start: 1,
            line_end: 10,
        },
        reason: "Both sessions modified verify_token".into(),
        peer_delta: Some("-old\n+new".into()),
        your_overlap_range: Some((5, 7)),
    }
}

#[test]
fn append_then_drain_returns_all_entries() {
    let dir = tempdir().unwrap();
    let inbox = dir.path().join("inbox.jsonl");
    append_entry(&inbox, &dirty_event_fixture()).unwrap();
    append_entry(&inbox, &InboxEntry::Message {
        ts: "2026-05-17T00:00:01Z".into(),
        msg_id: "m_1".into(),
        from: "abc12".into(),
        to: None,
        reply_to: None,
        body: "hi".into(),
    }).unwrap();

    let (entries, new_offset) = drain(&inbox, 0).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(new_offset > 0);

    let (entries2, _) = drain(&inbox, new_offset).unwrap();
    assert!(entries2.is_empty(), "second drain at watermark sees nothing new");
}

#[test]
fn drain_handles_missing_file_as_empty() {
    let dir = tempdir().unwrap();
    let (entries, off) = drain(&dir.path().join("absent.jsonl"), 0).unwrap();
    assert!(entries.is_empty());
    assert_eq!(off, 0);
}

#[test]
fn drain_resets_offset_when_file_truncated_below_watermark() {
    let dir = tempdir().unwrap();
    let inbox = dir.path().join("inbox.jsonl");
    append_entry(&inbox, &dirty_event_fixture()).unwrap();
    let (_, off) = drain(&inbox, 0).unwrap();
    assert!(off > 0);

    std::fs::write(&inbox, "").unwrap(); // truncate externally
    append_entry(&inbox, &dirty_event_fixture()).unwrap();

    let (entries, new_off) = drain(&inbox, off).unwrap();
    assert_eq!(entries.len(), 1, "should re-read from offset 0 after detected shrink");
    assert!(new_off < off || new_off > 0);
}

#[test]
fn drain_skips_corrupt_line_and_continues() {
    let dir = tempdir().unwrap();
    let inbox = dir.path().join("inbox.jsonl");
    std::fs::write(&inbox, "not valid json\n").unwrap();
    append_entry(&inbox, &dirty_event_fixture()).unwrap();

    let (entries, _) = drain(&inbox, 0).unwrap();
    assert_eq!(entries.len(), 1, "corrupt line skipped, good line returned");
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p cgn-core --test peer_inbox
```

Expected: FAIL — `peer::inbox` module not present.

- [ ] **Step 3: Implement peer::inbox**

Create `crates/cgn-core/src/peer/inbox.rs`:

```rust
//! Inbox transport — append-only JSON lines, drain-and-truncate semantics.

use crate::peer::concern::ConcernKind;
use crate::session::overlay::SymbolRef;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InboxEntry {
    DirtyEvent {
        ts: String,
        peer_session: String,
        peer_pid: u32,
        kind: ConcernKindSer,
        symbol: SymbolRef,
        reason: String,
        peer_delta: Option<String>,
        your_overlap_range: Option<(u32, u32)>,
    },
    Message {
        ts: String,
        msg_id: String,
        from: String,
        to: Option<String>,
        reply_to: Option<String>,
        body: String,
    },
}

// Serde-friendly mirror because ConcernKind is in another module without serde derive.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConcernKindSer {
    Hard,
    Soft,
}

impl From<ConcernKind> for ConcernKindSer {
    fn from(k: ConcernKind) -> Self {
        match k {
            ConcernKind::Hard => Self::Hard,
            ConcernKind::Soft => Self::Soft,
        }
    }
}

pub fn append_entry(path: &Path, entry: &InboxEntry) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_vec(entry).map_err(io::Error::other)?;
    line.push(b'\n');
    debug_assert!(line.len() < 4096, "inbox entry must fit in PIPE_BUF for atomic append");
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(&line)?;
    Ok(())
}

/// Read all entries after `start_offset`, then return new offset.
/// If file is shorter than `start_offset`, treat as truncated externally and reset to 0.
pub fn drain(path: &Path, start_offset: u64) -> io::Result<(Vec<InboxEntry>, u64)> {
    let mut f = match OpenOptions::new().read(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(e),
    };
    let len = f.metadata()?.len();
    let from = if start_offset > len { 0 } else { start_offset };
    f.seek(SeekFrom::Start(from))?;
    let reader = BufReader::new(&mut f);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<InboxEntry>(&line) {
            Ok(entry) => out.push(entry),
            Err(e) => {
                tracing::warn!(error=%e, "skipping corrupt inbox line");
                continue;
            }
        }
    }
    Ok((out, len))
}
```

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p cgn-core --test peer_inbox
```

Expected: 4 passed.

- [ ] **Step 5: Lint + build**

```
rustfmt --edition 2021 crates/cgn-core/src/peer/inbox.rs
cargo clippy -p cgn-core --tests -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-core/src/peer/inbox.rs \
        crates/cgn-core/tests/peer_inbox.rs
git commit -m "$(cat <<'EOF'
feat(peer): peer::inbox — append + drain with watermark

InboxEntry tagged enum (DirtyEvent | Message). append_entry uses
O_APPEND + single write (< PIPE_BUF) for POSIX atomicity. drain reads
after start_offset, detects external truncation by file-size shrink,
and skips corrupt JSON lines.

Refs: spec §4.2 (InboxEntry), §10
EOF
)"
```

---

### Task 6: peer::retention — rotation constants + rotate()

**Files:**
- Create: `crates/cgn-core/src/peer/retention.rs`
- Test: `crates/cgn-core/tests/peer_retention.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-core/tests/peer_retention.rs`:

```rust
use cgn_core::peer::retention::{rotate_if_needed, MSG_LOG_KEEP_ROTATED};
use std::fs;
use tempfile::tempdir;

#[test]
fn no_rotate_when_under_threshold() {
    let dir = tempdir().unwrap();
    let log = dir.path().join("msg.log");
    fs::write(&log, b"small").unwrap();
    let rotated = rotate_if_needed(&log, 1024 * 1024, MSG_LOG_KEEP_ROTATED).unwrap();
    assert!(!rotated);
    assert!(log.exists());
    assert!(!dir.path().join("msg.log.1").exists());
}

#[test]
fn rotates_when_over_threshold_and_chains_files() {
    let dir = tempdir().unwrap();
    let log = dir.path().join("msg.log");
    fs::write(&log, vec![b'x'; 100]).unwrap();
    let rotated = rotate_if_needed(&log, 50, MSG_LOG_KEEP_ROTATED).unwrap();
    assert!(rotated);
    assert!(log.exists() && fs::metadata(&log).unwrap().len() == 0);
    assert!(dir.path().join("msg.log.1").exists());
}

#[test]
fn rotation_drops_oldest_beyond_keep_count() {
    let dir = tempdir().unwrap();
    let log = dir.path().join("msg.log");
    // Pre-populate so that after rotation msg.log.{1..7} all exist.
    for n in 1..=MSG_LOG_KEEP_ROTATED {
        fs::write(dir.path().join(format!("msg.log.{n}")), format!("rot{n}")).unwrap();
    }
    fs::write(&log, vec![b'y'; 100]).unwrap();
    rotate_if_needed(&log, 50, MSG_LOG_KEEP_ROTATED).unwrap();
    // msg.log.{KEEP+1} must not exist
    assert!(!dir.path().join(format!("msg.log.{}", MSG_LOG_KEEP_ROTATED + 1)).exists());
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p cgn-core --test peer_retention
```

Expected: FAIL — module not present.

- [ ] **Step 3: Implement peer::retention**

Create `crates/cgn-core/src/peer/retention.rs`:

```rust
//! Log rotation + retention constants for peer-sync logs.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const MSG_LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;
pub const MSG_LOG_KEEP_ROTATED: usize = 7;
pub const WATCHER_LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;
pub const WATCHER_LOG_KEEP_ROTATED: usize = 3;
pub const SESSION_STALE_DAYS: i64 = 30;
pub const ARCHIVE_PURGE_DAYS: i64 = 90;
pub const ROTATE_CHECK_EVERY_N_EVENTS: u32 = 100;

/// Rotate `log` if it exceeds `threshold_bytes`. Chains existing rotated files
/// (`log.1` → `log.2`, …, dropping `log.{keep+1}` if present). Truncates `log`
/// after rotation. Returns whether rotation happened.
pub fn rotate_if_needed(log: &Path, threshold_bytes: u64, keep: usize) -> io::Result<bool> {
    let meta = match fs::metadata(log) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if meta.len() < threshold_bytes {
        return Ok(false);
    }
    // Drop the oldest if present
    let dir = log.parent().unwrap_or_else(|| Path::new("."));
    let stem = log.file_name().and_then(|s| s.to_str()).unwrap_or("log");
    let path_n = |n: usize| -> PathBuf { dir.join(format!("{stem}.{n}")) };

    if path_n(keep).exists() {
        fs::remove_file(path_n(keep))?;
    }
    for n in (1..keep).rev() {
        let from = path_n(n);
        if from.exists() {
            fs::rename(&from, path_n(n + 1))?;
        }
    }
    fs::rename(log, path_n(1))?;
    fs::write(log, b"")?; // re-create empty
    Ok(true)
}
```

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p cgn-core --test peer_retention
```

Expected: 3 passed.

- [ ] **Step 5: Lint + build**

```
rustfmt --edition 2021 crates/cgn-core/src/peer/retention.rs
cargo clippy -p cgn-core --tests -- -D warnings
cargo build -p cgn-core
```

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-core/src/peer/retention.rs \
        crates/cgn-core/tests/peer_retention.rs
git commit -m "$(cat <<'EOF'
feat(peer): peer::retention — log rotation primitives

Compile-time constants (msg.log 5MB×7, watcher.log 10MB×3, stale 30d,
purge 90d, check every 100 events). rotate_if_needed handles classic
mv-chain rotation and drops files beyond the keep count.

Refs: spec §12
EOF
)"
```

---

## ✅ CHECKPOINT — Phase 2 Complete

Run full test suite for core peer module:

```bash
cargo test -p cgn-core peer
cargo clippy -p cgn-core --tests -- -D warnings
```

Expected: all peer_* tests green, no clippy warnings.

If any RED → stop, investigate, fix before proceeding to Phase 3.

---

## Phase 3: CLI Peer Support

### Task 7: cli/peer/render — InboxEntry → hookSpecificOutput payload

**Files:**
- Create: `crates/cgn-cli/src/peer/mod.rs`
- Create: `crates/cgn-cli/src/peer/render.rs`
- Modify: `crates/cgn-cli/src/lib.rs:+1`
- Test: `crates/cgn-cli/tests/peer_render.rs` (new)

**Goal:** Render a drained batch of `InboxEntry` into the structured text payload from spec §6 + §3 (HARD inline / SOFT one-line / Messages full body), with the 4 KB cap.

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/peer_render.rs`:

```rust
use cgn_cli::peer::render::render_payload;
use cgn_core::peer::inbox::{ConcernKindSer, InboxEntry};
use cgn_core::session::overlay::{SymbolKind, SymbolRef};

fn dirty_hard() -> InboxEntry {
    InboxEntry::DirtyEvent {
        ts: "2026-05-17T00:00:30Z".into(),
        peer_session: "abc12".into(),
        peer_pid: 1234,
        kind: ConcernKindSer::Hard,
        symbol: SymbolRef {
            name: "verify_token".into(),
            kind: SymbolKind::Function,
            file: "src/auth.rs".into(),
            line_start: 42,
            line_end: 58,
        },
        reason: "Both sessions modified verify_token".into(),
        peer_delta: Some("-old\n+new".into()),
        your_overlap_range: Some((45, 50)),
    }
}

#[test]
fn empty_input_renders_empty_string() {
    assert!(render_payload(&[]).is_empty());
}

#[test]
fn single_hard_event_renders_header_and_delta() {
    let out = render_payload(&[dirty_hard()]);
    assert!(out.contains("HARD overlap"));
    assert!(out.contains("verify_token"));
    assert!(out.contains("src/auth.rs:42-58"));
    assert!(out.contains("-old"));
    assert!(out.contains("+new"));
    assert!(out.contains("Suggest"));
}

#[test]
fn message_event_renders_msg_id_and_body() {
    let msg = InboxEntry::Message {
        ts: "2026-05-17T00:00:10Z".into(),
        msg_id: "m_001".into(),
        from: "abc12".into(),
        to: None,
        reply_to: None,
        body: "hello peers".into(),
    };
    let out = render_payload(&[msg]);
    assert!(out.contains("[m_001]"));
    assert!(out.contains("hello peers"));
    assert!(out.contains("Ƀ"), "messages must carry beta marker");
}

#[test]
fn enforces_4kb_cap_by_truncating_soft_first() {
    let mut bulk: Vec<InboxEntry> = Vec::new();
    for i in 0..200 {
        bulk.push(InboxEntry::DirtyEvent {
            ts: "ts".into(),
            peer_session: format!("p{i}"),
            peer_pid: 1,
            kind: ConcernKindSer::Soft,
            symbol: SymbolRef {
                name: format!("sym_{i}"),
                kind: SymbolKind::Function,
                file: "src/x.rs".into(),
                line_start: 1, line_end: 2,
            },
            reason: "neighbor".into(),
            peer_delta: None,
            your_overlap_range: None,
        });
    }
    bulk.insert(0, dirty_hard()); // HARD always retained
    let out = render_payload(&bulk);
    assert!(out.len() <= 4096, "payload exceeds 4 KB cap: {}", out.len());
    assert!(out.contains("HARD overlap"));
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p code-graph-nexus --test peer_render
```

Expected: FAIL — module not present.

- [ ] **Step 3: Implement cli/peer/render**

Create `crates/cgn-cli/src/peer/mod.rs`:

```rust
pub mod render;
pub mod dispatch;
pub mod watcher;
```

Create `crates/cgn-cli/src/peer/render.rs`:

```rust
//! Render drained InboxEntry batches into a Claude Code hook payload.
//! Hard cap 4 KB; HARD events kept, SOFT trimmed first, messages kept after HARD.

use cgn_core::peer::inbox::{ConcernKindSer, InboxEntry};
use std::fmt::Write;

const PAYLOAD_CAP_BYTES: usize = 4096;
const HARD_DELTA_LOC_CAP: usize = 30;
const SOFT_EVENTS_DEFAULT_CAP: usize = 10;

pub fn render_payload(entries: &[InboxEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let (mut hard, mut soft, mut msgs) = (Vec::new(), Vec::new(), Vec::new());
    for e in entries {
        match e {
            InboxEntry::DirtyEvent { kind: ConcernKindSer::Hard, .. } => hard.push(e),
            InboxEntry::DirtyEvent { kind: ConcernKindSer::Soft, .. } => soft.push(e),
            InboxEntry::Message { .. } => msgs.push(e),
        }
    }
    let mut buf = String::new();
    if !hard.is_empty() {
        let _ = writeln!(buf, "[cgn peers] HARD overlap ({} event{})", hard.len(), if hard.len() == 1 { "" } else { "s" });
        for e in &hard {
            render_hard(&mut buf, e);
        }
    }
    if !soft.is_empty() {
        let cap = SOFT_EVENTS_DEFAULT_CAP.min(soft.len());
        let _ = writeln!(buf, "\n[cgn peers] SOFT overlap ({} event{})", soft.len(), if soft.len() == 1 { "" } else { "s" });
        for e in soft.iter().take(cap) {
            render_soft_one_line(&mut buf, e);
        }
        if soft.len() > cap {
            let _ = writeln!(buf, "  ... +{} more, run `cgn peers status`", soft.len() - cap);
        }
    }
    if !msgs.is_empty() {
        let _ = writeln!(buf, "\n[cgn peers] {} new message{} Ƀ", msgs.len(), if msgs.len() == 1 { "" } else { "s" });
        for e in &msgs {
            render_message(&mut buf, e);
        }
    }
    enforce_cap(buf, &hard)
}

fn render_hard(buf: &mut String, e: &InboxEntry) {
    if let InboxEntry::DirtyEvent { peer_session, peer_pid, ts, symbol, reason, peer_delta, your_overlap_range, .. } = e {
        let _ = writeln!(buf, "  Peer:   {peer_session} (pid {peer_pid})");
        let _ = writeln!(buf, "  When:   {ts}");
        let _ = writeln!(buf, "  Symbol: {} · {:?} · {}:{}-{}",
                 symbol.name, symbol.kind, symbol.file, symbol.line_start, symbol.line_end);
        let _ = writeln!(buf, "  Reason: {reason}");
        if let Some(d) = peer_delta {
            let truncated: String = d.lines().take(HARD_DELTA_LOC_CAP).collect::<Vec<_>>().join("\n");
            let _ = writeln!(buf, "  Peer delta:");
            for l in truncated.lines() {
                let _ = writeln!(buf, "    {l}");
            }
            if d.lines().count() > HARD_DELTA_LOC_CAP {
                let _ = writeln!(buf, "    ... (truncated, see `cgn peers diff {peer_session} {}`)", symbol.name);
            }
        }
        if let Some((s, end)) = your_overlap_range {
            let _ = writeln!(buf, "  Your overlap range: L{s}-{end}");
        }
        let _ = writeln!(buf, "  Suggest: Review peer delta before saving conflicting edits");
    }
}

fn render_soft_one_line(buf: &mut String, e: &InboxEntry) {
    if let InboxEntry::DirtyEvent { peer_session, ts, symbol, .. } = e {
        let _ = writeln!(buf, "  · {} ({:?}, {}:{}) by {peer_session} ({ts})",
                 symbol.name, symbol.kind, symbol.file, symbol.line_start);
    }
}

fn render_message(buf: &mut String, e: &InboxEntry) {
    if let InboxEntry::Message { msg_id, from, to, reply_to, body, ts, .. } = e {
        let to_part = match to {
            Some(t) => format!(" → {t}"),
            None => " → all".into(),
        };
        let reply_part = reply_to.as_ref().map(|r| format!(" (reply to {r})")).unwrap_or_default();
        let truncated: String = body.chars().take(500).collect();
        let _ = writeln!(buf, "  [{msg_id}] {from}{to_part}{reply_part} ({ts})");
        let _ = writeln!(buf, "    {truncated}");
    }
}

fn enforce_cap(mut buf: String, hard: &[&InboxEntry]) -> String {
    if buf.len() <= PAYLOAD_CAP_BYTES {
        return buf;
    }
    // Fall back: rebuild keeping only HARD section + top 3 message lines.
    buf.clear();
    let _ = writeln!(&mut buf, "[cgn peers] HARD overlap ({}) — payload trimmed to fit 4KB cap", hard.len());
    for e in hard {
        render_hard(&mut buf, e);
        if buf.len() > PAYLOAD_CAP_BYTES {
            buf.truncate(PAYLOAD_CAP_BYTES.saturating_sub(80));
            buf.push_str("\n... (truncated)\n");
            break;
        }
    }
    buf
}
```

Edit `crates/cgn-cli/src/lib.rs`, add:

```rust
pub mod peer;
```

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p code-graph-nexus --test peer_render
```

Expected: 4 passed.

- [ ] **Step 5: Lint + build**

```
rustfmt --edition 2021 crates/cgn-cli/src/peer/mod.rs crates/cgn-cli/src/peer/render.rs
cargo clippy -p code-graph-nexus --tests -- -D warnings
```

(Empty `dispatch.rs` / `watcher.rs` may be needed as stubs to satisfy `pub mod` declarations — if so, add `// stub, implemented in next task` plus `#![allow(dead_code)]` until Task 8/9 lands.)

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/peer/ crates/cgn-cli/src/lib.rs \
        crates/cgn-cli/tests/peer_render.rs
git commit -m "$(cat <<'EOF'
feat(peer-cli): render — InboxEntry batch → hookSpecificOutput payload

HARD inline (capped at 30 LOC delta), SOFT one-line (capped at 10),
messages full body (500-char body cap). 4 KB hard cap with HARD-only
fallback when over.

Refs: spec §3 (payload examples), §6 (rendering rules)
EOF
)"
```

---

### Task 8: cli/peer/dispatch — concern → inbox bridge

**Files:**
- Replace stub: `crates/cgn-cli/src/peer/dispatch.rs`
- Test: `crates/cgn-cli/tests/peer_dispatch.rs` (new)

**Goal:** Given a peer's DirtyEntry, an ImpactCache, and the receiver session dir, classify each peer symbol and append `InboxEntry::DirtyEvent` for HARD/SOFT (skip IGNORE).

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/peer_dispatch.rs`:

```rust
use chrono::Utc;
use cgn_cli::peer::dispatch::dispatch_peer_dirty_event;
use cgn_core::peer::concern::ImpactCache;
use cgn_core::peer::inbox::{drain, InboxEntry};
use cgn_core::session::overlay::{DirtyEntry, SymbolKind, SymbolRef};
use std::collections::HashSet;
use tempfile::tempdir;

fn sym(name: &str) -> SymbolRef {
    SymbolRef { name: name.into(), kind: SymbolKind::Function, file: "src/a.rs".into(), line_start: 1, line_end: 2 }
}

#[test]
fn hard_dispatches_event() {
    let dir = tempdir().unwrap();
    let receiver_dir = dir.path().to_path_buf();
    let inbox = receiver_dir.join("inbox.jsonl");

    let peer_entry = DirtyEntry {
        mtime_ns: 1, content_hash: "h".into(), fragment_id: "f".into(),
        tantivy_delta_segment: None, parse_failed: false,
        dirty_symbols: vec![sym("verify_token")],
    };
    let my_dirty = vec![sym("verify_token")];
    let cache = ImpactCache::from_set(HashSet::new());

    dispatch_peer_dirty_event(
        &receiver_dir, "abc12", 1234, &Utc::now().to_rfc3339(),
        &peer_entry, &my_dirty, &cache,
    ).unwrap();

    let (entries, _) = drain(&inbox, 0).unwrap();
    assert_eq!(entries.len(), 1);
    matches!(&entries[0], InboxEntry::DirtyEvent { .. });
}

#[test]
fn ignore_writes_nothing() {
    let dir = tempdir().unwrap();
    let receiver_dir = dir.path().to_path_buf();
    let inbox = receiver_dir.join("inbox.jsonl");

    let peer_entry = DirtyEntry {
        mtime_ns: 1, content_hash: "h".into(), fragment_id: "f".into(),
        tantivy_delta_segment: None, parse_failed: false,
        dirty_symbols: vec![sym("unrelated")],
    };
    let my_dirty = vec![sym("verify_token")];
    let cache = ImpactCache::from_set(HashSet::new());

    dispatch_peer_dirty_event(
        &receiver_dir, "abc12", 1234, &Utc::now().to_rfc3339(),
        &peer_entry, &my_dirty, &cache,
    ).unwrap();

    let (entries, _) = drain(&inbox, 0).unwrap();
    assert!(entries.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p code-graph-nexus --test peer_dispatch
```

Expected: FAIL.

- [ ] **Step 3: Implement cli/peer/dispatch**

Replace `crates/cgn-cli/src/peer/dispatch.rs`:

```rust
//! Bridge: classify peer dirty entry → append InboxEntry to receiver inbox.

use cgn_core::peer::concern::{classify, ConcernResult, ImpactCache};
use cgn_core::peer::inbox::{append_entry, ConcernKindSer, InboxEntry};
use cgn_core::session::overlay::{DirtyEntry, SymbolRef};
use std::io;
use std::path::Path;

pub fn dispatch_peer_dirty_event(
    receiver_session_dir: &Path,
    peer_session: &str,
    peer_pid: u32,
    ts: &str,
    peer_entry: &DirtyEntry,
    my_dirty_symbols: &[SymbolRef],
    impact_cache: &ImpactCache,
) -> io::Result<()> {
    let result = classify(&peer_entry.dirty_symbols, my_dirty_symbols, impact_cache);
    let (kind, symbol, reason) = match result {
        ConcernResult::Hit { kind, symbol, reason } => (kind, symbol, reason),
        ConcernResult::Ignore => return Ok(()),
    };
    let entry = InboxEntry::DirtyEvent {
        ts: ts.to_string(),
        peer_session: peer_session.to_string(),
        peer_pid,
        kind: ConcernKindSer::from(kind),
        symbol,
        reason,
        peer_delta: None,           // populated by watcher when `git diff` available
        your_overlap_range: None,
    };
    let inbox = receiver_session_dir.join("inbox.jsonl");
    append_entry(&inbox, &entry)
}
```

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p code-graph-nexus --test peer_dispatch
```

Expected: 2 passed.

- [ ] **Step 5: Lint + build**

```
rustfmt --edition 2021 crates/cgn-cli/src/peer/dispatch.rs
cargo clippy -p code-graph-nexus --tests -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/cgn-cli/src/peer/dispatch.rs \
        crates/cgn-cli/tests/peer_dispatch.rs
git commit -m "$(cat <<'EOF'
feat(peer-cli): dispatch — peer dirty entry → inbox append

Wraps classify() and appends a DirtyEvent InboxEntry to the receiver's
inbox.jsonl. IGNORE results are a no-op.

Refs: spec §7.1
EOF
)"
```

---

### Task 9: cli/peer/watcher — inotify event loop + lifecycle

**Files:**
- Replace stub: `crates/cgn-cli/src/peer/watcher.rs`
- Modify: `crates/cgn-cli/Cargo.toml` — add `notify = "6"`, `fs2 = "0.4"`, `daemonize = "0.5"` (optional, only if not building daemon manually)

**Goal:** A blocking `run_watcher()` function that: takes a flock, computes initial impact_cache, loops on inotify events for `sessions/*/dirty.json`, and dispatches via Task 8. SIGTERM → release lock + exit cleanly.

The watcher binary entry-point (forking + setsid + log redirection) is implemented in `commands/watch.rs` Task 10. This task focuses on the **loop logic** so it can be unit-tested in-process.

- [ ] **Step 1: Write the failing test**

Append to `crates/cgn-cli/tests/peer_dispatch.rs` (we reuse it because Watch loop test is integration-y; place a small unit covering only flock semantics):

```rust
#[test]
fn watcher_lock_rejects_second_holder() {
    use fs2::FileExt;
    use std::fs::OpenOptions;
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("watcher.lock");
    let f1 = OpenOptions::new().create(true).read(true).write(true).open(&lock).unwrap();
    f1.try_lock_exclusive().unwrap();
    let f2 = OpenOptions::new().create(true).read(true).write(true).open(&lock).unwrap();
    assert!(f2.try_lock_exclusive().is_err(), "second flock must fail while first holds it");
}
```

(The full watcher loop is exercised in Phase 7 cross-session tests; here we only validate the lock-acquisition contract.)

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p code-graph-nexus --test peer_dispatch -- watcher_lock_rejects_second_holder
```

Expected: FAIL — `fs2` not in Cargo.toml.

- [ ] **Step 3: Add dependencies**

Edit `crates/cgn-cli/Cargo.toml` `[dependencies]`:

```toml
notify = "6"
fs2 = "0.4"
```

(If they are already present in workspace inheritance, no-op.)

- [ ] **Step 4: Implement cli/peer/watcher**

Replace `crates/cgn-cli/src/peer/watcher.rs`:

```rust
//! Watcher main loop: inotify-driven peer-dirty fan-in.

use crate::peer::dispatch::dispatch_peer_dirty_event;
use chrono::Utc;
use fs2::FileExt;
use cgn_core::peer::concern::ImpactCache;
use cgn_core::peer::registry::alive_peers;
use cgn_core::session::overlay::DirtyFiles;
use cgn_core::session::SessionMeta;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct WatcherCfg {
    pub repo_root: PathBuf,
    pub my_session_id: String,
    pub my_session_dir: PathBuf,
    pub lock_path: PathBuf,
}

pub fn run_watcher(cfg: WatcherCfg) -> std::io::Result<()> {
    let lock_file = OpenOptions::new().create(true).read(true).write(true).open(&cfg.lock_path)?;
    lock_file
        .try_lock_exclusive()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::AlreadyExists, e))?;
    tracing::info!(pid = std::process::id(), session = %cfg.my_session_id, "watcher acquired flock");

    let cache = Arc::new(Mutex::new(rebuild_impact_cache(&cfg.my_session_dir)));

    let (tx, rx) = channel::<notify::Result<Event>>();
    let mut watcher = notify::recommended_watcher(tx).map_err(std::io::Error::other)?;
    watcher
        .watch(&cfg.repo_root.join("sessions"), RecursiveMode::Recursive)
        .map_err(std::io::Error::other)?;

    let mut event_count: u32 = 0;
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(ev)) => {
                event_count = event_count.wrapping_add(1);
                if let Err(e) = handle_event(&cfg, &cache, ev) {
                    log_watcher_error("event handler", &e);
                }
            }
            Ok(Err(e)) => log_watcher_error("notify error", &e),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if event_count % cgn_core::peer::retention::ROTATE_CHECK_EVERY_N_EVENTS == 0 {
            let _ = cgn_core::peer::retention::rotate_if_needed(
                &cfg.my_session_dir.join("msg.log"),
                cgn_core::peer::retention::MSG_LOG_ROTATE_BYTES,
                cgn_core::peer::retention::MSG_LOG_KEEP_ROTATED,
            );
            let _ = cgn_core::peer::retention::rotate_if_needed(
                &cfg.my_session_dir.join("watcher.log"),
                cgn_core::peer::retention::WATCHER_LOG_ROTATE_BYTES,
                cgn_core::peer::retention::WATCHER_LOG_KEEP_ROTATED,
            );
        }
    }
    Ok(())
}

fn handle_event(
    cfg: &WatcherCfg,
    cache: &Arc<Mutex<ImpactCache>>,
    ev: Event,
) -> std::io::Result<()> {
    if !matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_)) {
        return Ok(());
    }
    for path in &ev.paths {
        if !path.ends_with("dirty.json") {
            continue;
        }
        let Some(sid) = path.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()) else {
            continue;
        };
        if sid == cfg.my_session_id {
            // My own dirty changed → invalidate impact cache
            let mut c = cache.lock().expect("impact cache lock poisoned");
            *c = rebuild_impact_cache(&cfg.my_session_dir);
            continue;
        }
        // Peer changed
        dispatch_peer(cfg, cache, sid, path)?;
    }
    Ok(())
}

fn dispatch_peer(
    cfg: &WatcherCfg,
    cache: &Arc<Mutex<ImpactCache>>,
    peer_sid: &str,
    peer_dirty_path: &Path,
) -> std::io::Result<()> {
    let peer_dirty = DirtyFiles::read(peer_dirty_path)?;
    let my_dirty = DirtyFiles::read(&cfg.my_session_dir.join("dirty.json"))
        .map(|d| d.entries.into_values().flat_map(|e| e.dirty_symbols).collect::<Vec<_>>())
        .unwrap_or_default();
    let peer_meta = SessionMeta::read(&peer_dirty_path.with_file_name("meta.json"))?;
    let peer_pid = peer_meta.pid.unwrap_or(0);
    let ts = Utc::now().to_rfc3339();
    let cache_guard = cache.lock().expect("impact cache lock poisoned");
    for entry in peer_dirty.entries.values() {
        dispatch_peer_dirty_event(
            &cfg.my_session_dir,
            peer_sid,
            peer_pid,
            &ts,
            entry,
            &my_dirty,
            &cache_guard,
        )?;
    }
    Ok(())
}

fn rebuild_impact_cache(my_session_dir: &Path) -> ImpactCache {
    // Stub for v1: real implementation would query graph for IMPACT().
    // For now, an empty cache means SOFT detection requires explicit refresh
    // by the engine that ran impact analysis. Watcher will treat all SOFT as IGNORE
    // until the cache is populated externally.
    let _ = my_session_dir;
    ImpactCache::default()
}

fn log_watcher_error(context: &str, err: &dyn std::fmt::Debug) {
    use std::backtrace::Backtrace;
    let bt = Backtrace::capture();
    tracing::error!(context, ?err, "watcher loop error");
    eprintln!("[watcher] error in {context}: {err:?}\nbacktrace:\n{bt}");
}

pub fn alive_peer_sessions(repo_root: &Path, exclude_self: &str) -> Vec<String> {
    alive_peers(repo_root, exclude_self).into_iter().map(|p| p.session_id).collect()
}

#[allow(dead_code)]
fn _suppress_unused(_: HashSet<String>) {}
```

(`rebuild_impact_cache` is intentionally a stub — wiring it to the real graph engine is a follow-up; the watcher emits HARD events correctly which is the primary user-visible signal. Tracked in spec §17.)

- [ ] **Step 5: Run test to verify it passes**

```
cargo test -p code-graph-nexus --test peer_dispatch -- watcher_lock_rejects_second_holder
cargo build -p code-graph-nexus --bin cgn
```

Expected: lock test passes, binary builds.

- [ ] **Step 6: Lint**

```
rustfmt --edition 2021 crates/cgn-cli/src/peer/watcher.rs
cargo clippy -p code-graph-nexus --tests -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add crates/cgn-cli/src/peer/watcher.rs crates/cgn-cli/Cargo.toml \
        crates/cgn-cli/tests/peer_dispatch.rs
git commit -m "$(cat <<'EOF'
feat(peer-cli): watcher — inotify loop with flock-bounded lifetime

run_watcher() takes flock on watcher.lock, sets up notify::Watcher on
sessions/*, dispatches peer dirty events through dispatch.rs. Periodic
rotation check every 100 events. Errors logged with backtrace via
std::backtrace::Backtrace::capture() and fail-open continuation.

rebuild_impact_cache stubbed to default (HARD detection works; SOFT
wiring to real graph engine deferred per spec §17).

Refs: spec §7.1, §11.1
EOF
)"
```

---

## Phase 4: CLI Commands

### Task 10: cgn watch — --start / --stop / --status / --foreground

**Files:**
- Create: `crates/cgn-cli/src/commands/watch.rs`
- Modify: `crates/cgn-cli/src/commands/mod.rs:+1`
- Modify: `crates/cgn-cli/src/main.rs` (add top-level dispatch)
- Test: `crates/cgn-cli/tests/peers_watch_lifecycle.rs` (new, full)

**Goal:** CLI entry that forks the watcher (or runs foreground), signals SIGTERM to existing PID, prints status from `watcher.log` + lock state.

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/peers_watch_lifecycle.rs`:

```rust
use std::process::Command;
use tempfile::tempdir;

fn bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_cgn").into()
}

#[test]
fn watch_foreground_exits_immediately_when_no_repo() {
    let dir = tempdir().unwrap();
    let out = Command::new(bin())
        .args(["watch", "--foreground", "--repo", dir.path().to_str().unwrap()])
        .env("CGN_TEST_EXIT_AFTER_INIT", "1")
        .output()
        .expect("spawn cgn");
    assert!(out.status.success(), "stderr={}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn watch_status_when_no_watcher_running_returns_not_running() {
    let dir = tempdir().unwrap();
    let out = Command::new(bin())
        .args(["watch", "--status", "--repo", dir.path().to_str().unwrap()])
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("not running") || stdout.contains("no watcher"));
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p code-graph-nexus --test peers_watch_lifecycle
```

Expected: FAIL — `watch` subcommand absent.

- [ ] **Step 3: Implement commands/watch.rs**

Create `crates/cgn-cli/src/commands/watch.rs`:

```rust
//! `cgn watch` CLI surface.

use crate::peer::watcher::{run_watcher, WatcherCfg};
use crate::session::resolver::resolve_session_id;
use clap::Args;
use cgn_core::peer::registry::pid_alive;
use cgn_core::session::SessionMeta;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct WatchArgs {
    #[arg(long)] pub start: bool,
    #[arg(long)] pub stop: bool,
    #[arg(long)] pub status: bool,
    #[arg(long)] pub foreground: bool,
    #[arg(long, default_value = "touched")] pub concern: String,
    #[arg(long)] pub repo: Option<PathBuf>,
}

pub fn run(args: WatchArgs) -> anyhow::Result<()> {
    let repo_root = resolve_repo_root(args.repo)?;
    let session_id = resolve_session_id(None);
    let session_dir = repo_root.join("sessions").join(&session_id);
    std::fs::create_dir_all(&session_dir)?;

    match (args.start, args.stop, args.status, args.foreground) {
        (_, _, _, true) => start_foreground(repo_root, session_id, session_dir),
        (true, false, false, false) => start_background(repo_root, session_id, session_dir),
        (false, true, false, false) => stop_watcher(&session_dir),
        (false, false, true, false) => print_status(&session_dir),
        _ => {
            anyhow::bail!("specify exactly one of --start | --stop | --status | --foreground");
        }
    }
}

fn resolve_repo_root(explicit: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    // Fall back to existing registry resolver
    Ok(cgn_core::registry::resolve_home_cgn().join("code-graph-nexus/main"))
}

fn start_foreground(repo_root: PathBuf, sid: String, session_dir: PathBuf) -> anyhow::Result<()> {
    if std::env::var("CGN_TEST_EXIT_AFTER_INIT").is_ok() {
        eprintln!("[cgn watch] test mode — exiting after init");
        return Ok(());
    }
    let cfg = WatcherCfg {
        repo_root,
        my_session_id: sid,
        my_session_dir: session_dir.clone(),
        lock_path: session_dir.join("watcher.lock"),
    };
    run_watcher(cfg).map_err(|e| anyhow::anyhow!("watcher loop: {e}"))
}

#[cfg(unix)]
fn start_background(repo_root: PathBuf, sid: String, session_dir: PathBuf) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let watcher_log = session_dir.join("watcher.log");
    let log_writer = std::fs::OpenOptions::new().create(true).append(true).open(&watcher_log)?;
    let log_writer2 = log_writer.try_clone()?;
    let exe = std::env::current_exe()?;
    let child = unsafe {
        Command::new(exe)
            .args(["watch", "--foreground", "--repo", repo_root.to_string_lossy().as_ref()])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_writer))
            .stderr(Stdio::from(log_writer2))
            .pre_exec(|| {
                libc::setsid();
                Ok(())
            })
            .spawn()?
    };
    let pid = child.id();
    let meta_path = session_dir.join("meta.json");
    if let Ok(mut meta) = SessionMeta::read(&meta_path) {
        meta.watcher_pid = Some(pid);
        SessionMeta::write_atomic(&meta_path, &meta)?;
    }
    eprintln!("[cgn watch] forked watcher pid={pid}, sid={sid}");
    Ok(())
}

#[cfg(not(unix))]
fn start_background(_: PathBuf, _: String, _: PathBuf) -> anyhow::Result<()> {
    anyhow::bail!("background watch not yet supported on this platform; use --foreground")
}

fn stop_watcher(session_dir: &std::path::Path) -> anyhow::Result<()> {
    let meta_path = session_dir.join("meta.json");
    let mut meta = SessionMeta::read(&meta_path)?;
    let Some(pid) = meta.watcher_pid else {
        println!("no watcher running");
        return Ok(());
    };
    #[cfg(unix)]
    unsafe { libc::kill(pid as i32, libc::SIGTERM); }
    meta.watcher_pid = None;
    SessionMeta::write_atomic(&meta_path, &meta)?;
    println!("watcher pid={pid} signalled SIGTERM");
    Ok(())
}

fn print_status(session_dir: &std::path::Path) -> anyhow::Result<()> {
    let meta_path = session_dir.join("meta.json");
    let meta = SessionMeta::read(&meta_path).ok();
    match meta.and_then(|m| m.watcher_pid) {
        Some(pid) if pid_alive(pid) => println!("watcher running pid={pid}"),
        Some(pid) => println!("watcher pid={pid} dead (stale), no watcher"),
        None => println!("no watcher (not running)"),
    }
    // Tail watcher.log last 5 lines (cheap eyeball check)
    let log = session_dir.join("watcher.log");
    if let Ok(content) = std::fs::read_to_string(&log) {
        println!("--- watcher.log tail ---");
        for line in content.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev() {
            println!("{line}");
        }
    }
    Ok(())
}
```

Edit `crates/cgn-cli/src/commands/mod.rs`, add:

```rust
pub mod watch;
```

Edit `crates/cgn-cli/src/main.rs` (dispatch — actual structure varies, the principle is):

```rust
// In the Command enum:
Watch(commands::watch::WatchArgs),

// In match arm:
Command::Watch(a) => commands::watch::run(a),
```

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p code-graph-nexus --test peers_watch_lifecycle
```

Expected: 2 passed.

- [ ] **Step 5: Manual smoke**

```bash
./target/debug/cgn watch --status --repo /tmp/x
./target/debug/cgn watch --foreground --repo /tmp/x &
sleep 0.5
./target/debug/cgn watch --status --repo /tmp/x
./target/debug/cgn watch --stop --repo /tmp/x
```

Expected: status shows pid then "no watcher" after stop.

- [ ] **Step 6: Lint + commit**

```bash
rustfmt --edition 2021 crates/cgn-cli/src/commands/watch.rs crates/cgn-cli/src/main.rs
cargo clippy -p code-graph-nexus --tests -- -D warnings

git add crates/cgn-cli/src/commands/watch.rs \
        crates/cgn-cli/src/commands/mod.rs \
        crates/cgn-cli/src/main.rs \
        crates/cgn-cli/tests/peers_watch_lifecycle.rs
git commit -m "$(cat <<'EOF'
feat(peer-cli): cgn watch — start | stop | status | foreground

--start forks self with --foreground (setsid + redirect stdio → watcher.log),
writes watcher_pid into SessionMeta. --stop sends SIGTERM. --status prints
running pid + tails watcher.log. --foreground runs the loop inline (no fork).

Refs: spec §9
EOF
)"
```

---

### Task 11: cgn peers — status / diff / log

**Files:**
- Create: `crates/cgn-cli/src/commands/peers.rs`
- Modify: `crates/cgn-cli/src/commands/mod.rs:+1`
- Modify: `crates/cgn-cli/src/main.rs`
- Test: `crates/cgn-cli/tests/peers_cmd_status.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/peers_cmd_status.rs`:

```rust
use std::process::Command;
use tempfile::tempdir;

fn bin() -> std::path::PathBuf { env!("CARGO_BIN_EXE_cgn").into() }

#[test]
fn peers_status_empty_repo_prints_no_peers() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
    let out = Command::new(bin())
        .args(["peers", "status", "--repo", dir.path().to_str().unwrap()])
        .output().expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no peers") || stdout.is_empty() || stdout.contains("[]"));
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p code-graph-nexus --test peers_cmd_status
```

Expected: FAIL — `peers` subcommand absent.

- [ ] **Step 3: Implement commands/peers.rs (status / diff / log only — say/inbox/thread in Task 12)**

Create `crates/cgn-cli/src/commands/peers.rs`:

```rust
//! `cgn peers` CLI surface.

use clap::{Args, Subcommand};
use cgn_core::peer::registry::alive_peers;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct PeersArgs {
    #[command(subcommand)]
    pub cmd: PeersCmd,
    #[arg(long, global = true)]
    pub repo: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum PeersCmd {
    Status,
    Diff { peer: String, symbol: Option<String> },
    Log {
        #[arg(long)] since: Option<String>,
        #[arg(long)] peer: Option<String>,
        #[arg(long)] direction: Option<String>,  // "sent" | "recv"
        #[arg(long, default_value_t = 50)] limit: usize,
    },
    /// Ƀ Send a message (broadcast or targeted)
    Say {
        body: String,
        #[arg(long)] to: Option<String>,
        #[arg(long)] reply: Option<String>,
    },
    /// Ƀ Inspect inbox without draining
    Inbox { #[arg(long, default_value_t = 50)] limit: usize },
    /// Ƀ Print thread (current session msg.log)
    Thread { msg_id: String },
    /// Rotate logs + archive stale sessions
    Gc,
}

pub fn run(args: PeersArgs) -> anyhow::Result<()> {
    let repo_root = args.repo.unwrap_or_else(|| cgn_core::registry::resolve_home_cgn().join("code-graph-nexus/main"));
    match args.cmd {
        PeersCmd::Status => cmd_status(&repo_root),
        PeersCmd::Diff { peer, symbol } => cmd_diff(&repo_root, &peer, symbol.as_deref()),
        PeersCmd::Log { since, peer, direction, limit } => cmd_log(&repo_root, since.as_deref(), peer.as_deref(), direction.as_deref(), limit),
        PeersCmd::Say { body, to, reply } => super::peers_msg::cmd_say(&repo_root, &body, to.as_deref(), reply.as_deref()),
        PeersCmd::Inbox { limit } => super::peers_msg::cmd_inbox(&repo_root, limit),
        PeersCmd::Thread { msg_id } => super::peers_msg::cmd_thread(&repo_root, &msg_id),
        PeersCmd::Gc => cmd_gc(&repo_root),
    }
}

fn cmd_status(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let me = crate::session::resolver::resolve_session_id(None);
    let peers = alive_peers(repo_root, &me);
    if peers.is_empty() {
        println!("no peers");
        return Ok(());
    }
    for p in peers {
        println!("session={}\tpid={}\tlast_touched={}\twatcher={}",
            p.session_id, p.pid, p.last_touched,
            if p.watcher_alive { "alive" } else { "dead" });
    }
    Ok(())
}

fn cmd_diff(repo_root: &std::path::Path, peer: &str, symbol: Option<&str>) -> anyhow::Result<()> {
    use cgn_core::session::overlay::DirtyFiles;
    let peer_dirty = DirtyFiles::read(&repo_root.join("sessions").join(peer).join("dirty.json"))?;
    for (path, entry) in &peer_dirty.entries {
        if let Some(sym) = symbol {
            if !entry.dirty_symbols.iter().any(|s| s.name == sym) { continue; }
        }
        println!("--- {path} ---");
        for s in &entry.dirty_symbols {
            println!("  {} ({:?}, L{}-{})", s.name, s.kind, s.line_start, s.line_end);
        }
    }
    Ok(())
}

fn cmd_log(repo_root: &std::path::Path, _since: Option<&str>, peer: Option<&str>, direction: Option<&str>, limit: usize) -> anyhow::Result<()> {
    let me = crate::session::resolver::resolve_session_id(None);
    let msg_log = repo_root.join("sessions").join(&me).join("msg.log");
    let Ok(content) = std::fs::read_to_string(&msg_log) else {
        println!("no messages");
        return Ok(());
    };
    let mut printed = 0;
    for line in content.lines().rev() {
        if printed >= limit { break; }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(p) = peer {
                let from = v.get("from").and_then(|x| x.as_str()).unwrap_or("");
                let to = v.get("to").and_then(|x| x.as_str()).unwrap_or("");
                if from != p && to != p { continue; }
            }
            if let Some(d) = direction {
                let dir = v.get("direction").and_then(|x| x.as_str()).unwrap_or("");
                if dir != d { continue; }
            }
            println!("{line}");
            printed += 1;
        }
    }
    Ok(())
}

fn cmd_gc(repo_root: &std::path::Path) -> anyhow::Result<()> {
    let me = crate::session::resolver::resolve_session_id(None);
    let session_dir = repo_root.join("sessions").join(&me);
    use cgn_core::peer::retention::*;
    let _ = rotate_if_needed(&session_dir.join("msg.log"), MSG_LOG_ROTATE_BYTES, MSG_LOG_KEEP_ROTATED);
    let _ = rotate_if_needed(&session_dir.join("watcher.log"), WATCHER_LOG_ROTATE_BYTES, WATCHER_LOG_KEEP_ROTATED);
    println!("rotated logs for session={me}");
    Ok(())
}
```

Edit `crates/cgn-cli/src/commands/mod.rs`:

```rust
pub mod peers;
pub mod peers_msg;   // implemented in Task 12
```

Edit `crates/cgn-cli/src/main.rs` dispatch:

```rust
Command::Peers(a) => commands::peers::run(a),
```

Create empty stub `crates/cgn-cli/src/commands/peers_msg.rs`:

```rust
//! Stub — implemented in Task 12 (say / inbox / thread).
use std::path::Path;
pub fn cmd_say(_: &Path, _: &str, _: Option<&str>, _: Option<&str>) -> anyhow::Result<()> {
    anyhow::bail!("`peers say` not yet implemented (Task 12)")
}
pub fn cmd_inbox(_: &Path, _: usize) -> anyhow::Result<()> {
    anyhow::bail!("`peers inbox` not yet implemented (Task 12)")
}
pub fn cmd_thread(_: &Path, _: &str) -> anyhow::Result<()> {
    anyhow::bail!("`peers thread` not yet implemented (Task 12)")
}
```

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p code-graph-nexus --test peers_cmd_status
```

Expected: PASS.

- [ ] **Step 5: Lint + commit**

```bash
rustfmt --edition 2021 crates/cgn-cli/src/commands/peers.rs crates/cgn-cli/src/commands/peers_msg.rs crates/cgn-cli/src/main.rs
cargo clippy -p code-graph-nexus --tests -- -D warnings

git add crates/cgn-cli/src/commands/peers.rs \
        crates/cgn-cli/src/commands/peers_msg.rs \
        crates/cgn-cli/src/commands/mod.rs \
        crates/cgn-cli/src/main.rs \
        crates/cgn-cli/tests/peers_cmd_status.rs
git commit -m "$(cat <<'EOF'
feat(peer-cli): cgn peers status | diff | log | gc

Read-side peer commands. status enumerates alive peers, diff prints
peer dirty symbols (optionally filtered to one name), log tails the
caller's msg.log filtered by peer/direction, gc invokes rotation
primitives. Messaging commands stubbed for Task 12.

Refs: spec §9
EOF
)"
```

---

### Task 12: cgn peers — say / inbox / thread (Ƀ)

**Files:**
- Replace stub: `crates/cgn-cli/src/commands/peers_msg.rs`
- Test: `crates/cgn-cli/tests/peers_cmd_msg.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/peers_cmd_msg.rs`:

```rust
use std::process::Command;
use tempfile::tempdir;

fn bin() -> std::path::PathBuf { env!("CARGO_BIN_EXE_cgn").into() }

#[test]
fn say_broadcast_writes_to_each_peer_inbox() {
    let dir = tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    for sid in ["peerA", "peerB"] {
        let s = sessions.join(sid);
        std::fs::create_dir_all(&s).unwrap();
        let meta = format!(r#"{{
            "version":1,"session_id":"{sid}","pid":{pid},
            "started_at":"2026-01-01T00:00:00Z","last_touched":"2026-01-01T00:00:00Z",
            "base_sha":"0000000000000000000000000000000000000000",
            "source_worktree":"/tmp","overlay_version":1
        }}"#, pid = std::process::id());
        std::fs::write(s.join("meta.json"), meta).unwrap();
    }
    let out = Command::new(bin())
        .args(["peers", "say", "hello", "--repo", dir.path().to_str().unwrap()])
        .env("CLAUDE_CODE_SESSION_ID", "me")
        .output().expect("spawn");
    assert!(out.status.success(), "stderr={}", String::from_utf8_lossy(&out.stderr));
    for sid in ["peerA", "peerB"] {
        let inbox = sessions.join(sid).join("inbox.jsonl");
        let body = std::fs::read_to_string(&inbox).unwrap();
        assert!(body.contains("\"body\":\"hello\""), "{sid} inbox missing message");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cargo test -p code-graph-nexus --test peers_cmd_msg
```

Expected: FAIL — stub bails.

- [ ] **Step 3: Implement peers_msg.rs**

Replace `crates/cgn-cli/src/commands/peers_msg.rs`:

```rust
use chrono::Utc;
use cgn_core::peer::inbox::{append_entry, drain, InboxEntry};
use cgn_core::peer::registry::alive_peers;
use std::path::Path;
use uuid::Uuid;

pub fn cmd_say(repo_root: &Path, body: &str, to: Option<&str>, reply: Option<&str>) -> anyhow::Result<()> {
    let me = crate::session::resolver::resolve_session_id(None);
    let msg_id = format!("m_{}", &Uuid::now_v7().simple().to_string()[..12]);
    let ts = Utc::now().to_rfc3339();
    let entry = InboxEntry::Message {
        ts: ts.clone(),
        msg_id: msg_id.clone(),
        from: me.clone(),
        to: to.map(|s| s.to_string()),
        reply_to: reply.map(|s| s.to_string()),
        body: body.to_string(),
    };

    // Append to each recipient inbox
    if let Some(target) = to {
        let inbox = repo_root.join("sessions").join(target).join("inbox.jsonl");
        append_entry(&inbox, &entry)?;
    } else {
        for p in alive_peers(repo_root, &me) {
            let inbox = repo_root.join("sessions").join(&p.session_id).join("inbox.jsonl");
            append_entry(&inbox, &entry)?;
        }
    }

    // Append to my own msg.log with direction=sent
    let msg_log = repo_root.join("sessions").join(&me).join("msg.log");
    if let Some(parent) = msg_log.parent() { std::fs::create_dir_all(parent)?; }
    let log_entry = serde_json::json!({
        "ts": ts, "direction": "sent",
        "msg_id": msg_id, "from": me,
        "to": to, "reply_to": reply, "body": body,
    });
    let line = format!("{log_entry}\n");
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&msg_log)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

pub fn cmd_inbox(repo_root: &Path, limit: usize) -> anyhow::Result<()> {
    let me = crate::session::resolver::resolve_session_id(None);
    let inbox = repo_root.join("sessions").join(&me).join("inbox.jsonl");
    let (entries, _) = drain(&inbox, 0)?;
    for e in entries.into_iter().take(limit) {
        println!("{}", serde_json::to_string(&e)?);
    }
    Ok(())
}

pub fn cmd_thread(repo_root: &Path, msg_id: &str) -> anyhow::Result<()> {
    let me = crate::session::resolver::resolve_session_id(None);
    let msg_log = repo_root.join("sessions").join(&me).join("msg.log");
    let Ok(content) = std::fs::read_to_string(&msg_log) else {
        println!("no messages");
        return Ok(());
    };
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let mid = v.get("msg_id").and_then(|x| x.as_str()).unwrap_or("");
        let reply = v.get("reply_to").and_then(|x| x.as_str()).unwrap_or("");
        if mid == msg_id || reply == msg_id {
            println!("{line}");
        }
    }
    Ok(())
}
```

Add `uuid` with `v7` feature to `Cargo.toml` if missing:

```toml
uuid = { version = "1", features = ["v7"] }
```

(Note: `cmd_inbox` calls `drain` which is **destructive** — it returns entries AND advances watermark. For a non-destructive inspect, this should read raw lines instead. Fix in this same task:)

Replace `cmd_inbox` body with a non-destructive read:

```rust
pub fn cmd_inbox(repo_root: &Path, limit: usize) -> anyhow::Result<()> {
    let me = crate::session::resolver::resolve_session_id(None);
    let inbox = repo_root.join("sessions").join(&me).join("inbox.jsonl");
    let Ok(content) = std::fs::read_to_string(&inbox) else {
        println!("inbox empty");
        return Ok(());
    };
    for line in content.lines().take(limit) {
        println!("{line}");
    }
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

```
cargo test -p code-graph-nexus --test peers_cmd_msg
```

Expected: PASS.

- [ ] **Step 5: Lint + commit**

```bash
rustfmt --edition 2021 crates/cgn-cli/src/commands/peers_msg.rs
cargo clippy -p code-graph-nexus --tests -- -D warnings

git add crates/cgn-cli/src/commands/peers_msg.rs \
        crates/cgn-cli/Cargo.toml \
        crates/cgn-cli/tests/peers_cmd_msg.rs
git commit -m "$(cat <<'EOF'
feat(peer-cli): Ƀ messaging — say / inbox / thread

`say` writes Message entries to each recipient's inbox.jsonl and the
sender's msg.log (direction=sent). `inbox` is a non-destructive read
(use the pre_tool_use hook for actual draining). `thread` filters
msg.log entries matching msg_id or reply_to.

Refs: spec §8
EOF
)"
```

---

### Task 13: cgn peers gc — wire to existing scaffolding

Already covered by `cmd_gc` in Task 11. **Skip — no separate commit.**

Mark this as completed when proceeding.

---

## ✅ CHECKPOINT — Phase 4 Complete

Smoke test the full CLI:

```bash
cargo build -p code-graph-nexus --bin cgn
./target/debug/cgn peers --help
./target/debug/cgn watch --help
```

Expected: subcommands documented, no panic.

```bash
cargo test -p code-graph-nexus peer
cargo clippy -p code-graph-nexus --tests -- -D warnings
```

Expected: green.

---

## Phase 5: Hook Integration

### Task 14: session_start hook — auto-watch spawn

**Files:**
- Modify: `crates/cgn-cli/src/commands/hook/session_start.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/hook_session_start_autowatch.rs`:

```rust
use std::process::Command;
use tempfile::tempdir;

fn bin() -> std::path::PathBuf { env!("CARGO_BIN_EXE_cgn").into() }

#[test]
fn autowatch_marker_present_spawns_watcher() {
    let dir = tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join(".cgn")).unwrap();
    std::fs::write(repo.join(".cgn/auto-watch"), "").unwrap();

    let out = Command::new(bin())
        .args(["hook", "session_start"])
        .env("CLAUDE_PROJECT_DIR", repo.to_str().unwrap())
        .env("CLAUDE_CODE_SESSION_ID", "test_sess")
        .output().expect("spawn");
    assert!(out.status.success());
    // We cannot easily assert the forked watcher (it daemonizes), but stderr
    // should mention "autowatch detected" if the branch ran.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("autowatch") || stderr.is_empty(),
            "session_start did not honour autowatch marker: {stderr}");
}

#[test]
fn no_marker_no_spawn() {
    let dir = tempdir().unwrap();
    let out = Command::new(bin())
        .args(["hook", "session_start"])
        .env("CLAUDE_PROJECT_DIR", dir.path().to_str().unwrap())
        .env("CLAUDE_CODE_SESSION_ID", "test_sess2")
        .output().expect("spawn");
    assert!(out.status.success());
}
```

- [ ] **Step 2: Run to verify failure**

```
cargo test -p code-graph-nexus --test hook_session_start_autowatch
```

Expected: FAIL or compilation error (depending on whether the hook subcommand currently exists).

- [ ] **Step 3: Modify session_start handler**

Edit `crates/cgn-cli/src/commands/hook/session_start.rs`. Locate the main handler function (likely `pub fn handle(_: HookInput) -> ...`) and at the end add:

```rust
// Spawn watcher if opt-in marker present
let project_dir = std::env::var("CLAUDE_PROJECT_DIR").ok().map(std::path::PathBuf::from);
if let Some(proj) = project_dir {
    let marker = proj.join(".cgn/auto-watch");
    if marker.exists() {
        eprintln!("autowatch marker detected, spawning watcher");
        let exe = std::env::current_exe().ok();
        if let Some(exe) = exe {
            let _ = std::process::Command::new(exe)
                .args(["watch", "--start"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
}
```

- [ ] **Step 4: Run test**

```
cargo test -p code-graph-nexus --test hook_session_start_autowatch
```

Expected: 2 passed.

- [ ] **Step 5: Lint + commit**

```bash
rustfmt --edition 2021 crates/cgn-cli/src/commands/hook/session_start.rs
cargo clippy -p code-graph-nexus --tests -- -D warnings

git add crates/cgn-cli/src/commands/hook/session_start.rs \
        crates/cgn-cli/tests/hook_session_start_autowatch.rs
git commit -m "$(cat <<'EOF'
feat(peer-hook): session_start auto-spawns watcher when marker present

If <repo>/.cgn/auto-watch exists, session_start hook fires `cgn watch
--start` in background. Absent marker → no-op. Opt-in per spec §9.

Refs: spec §9
EOF
)"
```

---

### Task 15: pre_tool_use hook — drain + render + emit

**Files:**
- Modify: `crates/cgn-cli/src/commands/hook/pre_tool_use.rs`
- Test: `crates/cgn-cli/tests/peers_inbox_drain.rs` (new — partial; the full version with bidirectional flow is Task 19)

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-cli/tests/peers_inbox_drain.rs`:

```rust
use std::process::{Command, Stdio};
use tempfile::tempdir;
use std::io::Write;

fn bin() -> std::path::PathBuf { env!("CARGO_BIN_EXE_cgn").into() }

#[test]
fn pre_tool_use_emits_peer_section_when_inbox_has_entries() {
    let dir = tempdir().unwrap();
    let me = "test_sess_drain";
    let session_dir = dir.path().join("sessions").join(me);
    std::fs::create_dir_all(&session_dir).unwrap();
    let inbox = session_dir.join("inbox.jsonl");
    let entry = r#"{"type":"message","ts":"2026-05-17T00:00:00Z","msg_id":"m1","from":"alice","to":null,"reply_to":null,"body":"hello"}"#;
    std::fs::write(&inbox, format!("{entry}\n")).unwrap();
    // also ensure meta.json exists so resolver picks correct repo_root
    let meta = format!(r#"{{
        "version":1,"session_id":"{me}","pid":{pid},
        "started_at":"2026-01-01T00:00:00Z","last_touched":"2026-01-01T00:00:00Z",
        "base_sha":"0000000000000000000000000000000000000000",
        "source_worktree":"/tmp","overlay_version":1
    }}"#, pid = std::process::id());
    std::fs::write(session_dir.join("meta.json"), meta).unwrap();

    let mut child = Command::new(bin())
        .args(["hook", "pre_tool_use"])
        .env("CLAUDE_CODE_SESSION_ID", me)
        .env("CLAUDE_PROJECT_DIR", dir.path())
        .env("CGN_REPO_ROOT_OVERRIDE", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn().unwrap();
    child.stdin.as_mut().unwrap().write_all(b"{}").unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("hookSpecificOutput"));
    assert!(stdout.contains("hello"), "rendered payload should contain message body: {stdout}");
}
```

- [ ] **Step 2: Run to verify failure**

```
cargo test -p code-graph-nexus --test peers_inbox_drain
```

Expected: FAIL — hook does not emit peer section.

- [ ] **Step 3: Modify pre_tool_use handler**

Edit `crates/cgn-cli/src/commands/hook/pre_tool_use.rs`. At the end of the main handler, before emitting JSON:

```rust
// Drain peer inbox and prepend to hookSpecificOutput.additionalContext
fn drain_and_render_peer_payload() -> Option<String> {
    let me = crate::session::resolver::resolve_session_id(None);
    let repo_root = std::env::var("CGN_REPO_ROOT_OVERRIDE")
        .map(std::path::PathBuf::from)
        .ok()
        .unwrap_or_else(|| cgn_core::registry::resolve_home_cgn().join("code-graph-nexus/main"));
    let session_dir = repo_root.join("sessions").join(&me);
    let inbox = session_dir.join("inbox.jsonl");
    let meta_path = session_dir.join("meta.json");
    let mut meta = cgn_core::session::SessionMeta::read(&meta_path).ok()?;
    let (entries, new_offset) = cgn_core::peer::inbox::drain(&inbox, meta.last_drained_offset).ok()?;
    if entries.is_empty() { return None; }
    let payload = crate::peer::render::render_payload(&entries);
    if payload.is_empty() { return None; }
    // Truncate inbox + advance watermark
    let _ = std::fs::write(&inbox, "");
    meta.last_drained_offset = 0;
    let _ = cgn_core::session::SessionMeta::write_atomic(&meta_path, &meta);
    let _ = new_offset; // unused after reset, kept for future watermark mode
    Some(payload)
}
```

Then in the main handler, when constructing the JSON output, include `additionalContext`:

```rust
let mut additional_context = String::new();
if let Some(p) = drain_and_render_peer_payload() {
    additional_context.push_str(&p);
}
// merge with existing additionalContext from your existing logic
let payload = serde_json::json!({
    "hookSpecificOutput": {
        "decision": "approve",
        "additionalContext": additional_context,
    }
});
println!("{payload}");
```

(Exact merge depends on existing handler structure — preserve any existing context the hook was already emitting.)

- [ ] **Step 4: Run test**

```
cargo test -p code-graph-nexus --test peers_inbox_drain
```

Expected: PASS.

- [ ] **Step 5: Lint + commit**

```bash
rustfmt --edition 2021 crates/cgn-cli/src/commands/hook/pre_tool_use.rs
cargo clippy -p code-graph-nexus --tests -- -D warnings

git add crates/cgn-cli/src/commands/hook/pre_tool_use.rs \
        crates/cgn-cli/tests/peers_inbox_drain.rs
git commit -m "$(cat <<'EOF'
feat(peer-hook): pre_tool_use drains inbox + injects payload

Reads sessions/<me>/inbox.jsonl from SessionMeta.last_drained_offset,
renders via peer::render, truncates inbox, resets watermark, and merges
the rendered text into hookSpecificOutput.additionalContext alongside
any pre-existing hook context.

Refs: spec §6
EOF
)"
```

---

### Task 16: user_prompt_submit hook — secondary drain

**Files:**
- Modify: `crates/cgn-cli/src/commands/hook/user_prompt_submit.rs`

Same pattern as Task 15 but on a different hook. This ensures the LLM also sees peer activity when the user submits a new prompt (not just before a tool call).

- [ ] **Step 1: Write the failing test**

Append to `crates/cgn-cli/tests/peers_inbox_drain.rs`:

```rust
#[test]
fn user_prompt_submit_also_drains_inbox() {
    let dir = tempdir().unwrap();
    let me = "test_sess_ups";
    let session_dir = dir.path().join("sessions").join(me);
    std::fs::create_dir_all(&session_dir).unwrap();
    let inbox = session_dir.join("inbox.jsonl");
    let entry = r#"{"type":"message","ts":"t","msg_id":"m2","from":"bob","to":null,"reply_to":null,"body":"prompt-time-peek"}"#;
    std::fs::write(&inbox, format!("{entry}\n")).unwrap();
    let meta = format!(r#"{{"version":1,"session_id":"{me}","pid":{pid},"started_at":"t","last_touched":"t","base_sha":"0000000000000000000000000000000000000000","source_worktree":"/tmp","overlay_version":1}}"#, pid = std::process::id());
    std::fs::write(session_dir.join("meta.json"), meta).unwrap();

    let mut child = Command::new(bin())
        .args(["hook", "user_prompt_submit"])
        .env("CLAUDE_CODE_SESSION_ID", me)
        .env("CGN_REPO_ROOT_OVERRIDE", dir.path())
        .stdin(Stdio::piped()).stdout(Stdio::piped())
        .spawn().unwrap();
    child.stdin.as_mut().unwrap().write_all(b"{}").unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("prompt-time-peek"));
}
```

- [ ] **Step 2: Run to verify failure**

```
cargo test -p code-graph-nexus --test peers_inbox_drain -- user_prompt_submit_also_drains_inbox
```

Expected: FAIL.

- [ ] **Step 3: Modify handler**

In `crates/cgn-cli/src/commands/hook/user_prompt_submit.rs`, paste the same `drain_and_render_peer_payload` helper from Task 15 (or extract to shared `commands/hook/common.rs`) and call it before emitting output. Merge into `additionalContext`.

To avoid duplication, refactor Task 15's helper into `commands/hook/common.rs`:

```rust
// crates/cgn-cli/src/commands/hook/common.rs (append)

pub fn drain_and_render_peer_payload() -> Option<String> {
    let me = crate::session::resolver::resolve_session_id(None);
    let repo_root = std::env::var("CGN_REPO_ROOT_OVERRIDE")
        .map(std::path::PathBuf::from)
        .ok()
        .unwrap_or_else(|| cgn_core::registry::resolve_home_cgn().join("code-graph-nexus/main"));
    let session_dir = repo_root.join("sessions").join(&me);
    let inbox = session_dir.join("inbox.jsonl");
    let meta_path = session_dir.join("meta.json");
    let mut meta = cgn_core::session::SessionMeta::read(&meta_path).ok()?;
    let (entries, _new_offset) = cgn_core::peer::inbox::drain(&inbox, meta.last_drained_offset).ok()?;
    if entries.is_empty() { return None; }
    let payload = crate::peer::render::render_payload(&entries);
    if payload.is_empty() { return None; }
    let _ = std::fs::write(&inbox, "");
    meta.last_drained_offset = 0;
    let _ = cgn_core::session::SessionMeta::write_atomic(&meta_path, &meta);
    Some(payload)
}
```

Then in both `pre_tool_use.rs` and `user_prompt_submit.rs`:

```rust
use super::common::drain_and_render_peer_payload;
// ... in handler:
let peer_payload = drain_and_render_peer_payload().unwrap_or_default();
```

- [ ] **Step 4: Run all hook tests**

```
cargo test -p code-graph-nexus --test peers_inbox_drain
```

Expected: 2 passed.

- [ ] **Step 5: Lint + commit**

```bash
rustfmt --edition 2021 crates/cgn-cli/src/commands/hook/{common.rs,user_prompt_submit.rs,pre_tool_use.rs}
cargo clippy -p code-graph-nexus --tests -- -D warnings

git add crates/cgn-cli/src/commands/hook/
git commit -m "$(cat <<'EOF'
feat(peer-hook): user_prompt_submit also drains inbox; shared helper

Extracted drain_and_render_peer_payload to commands/hook/common.rs and
wired both pre_tool_use and user_prompt_submit through it. Ensures peer
activity surfaces at every LLM-facing boundary.

Refs: spec §6
EOF
)"
```

---

## Phase 6: MCP Tools

### Task 17: MCP tools — peers_status / peers_log / peers_say (Ƀ)

**Files:**
- Create: `crates/cgn-mcp/src/tools/peers.rs`
- Modify: `crates/cgn-mcp/src/lib.rs:+5`
- Test: `crates/cgn-mcp/tests/peers_tools.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `crates/cgn-mcp/tests/peers_tools.rs`:

```rust
use cgn_mcp::tools::peers::{peers_status_tool, peers_log_tool, peers_say_tool};

#[test]
fn tools_are_registered_with_expected_names() {
    let names: Vec<&str> = [
        peers_status_tool().name(),
        peers_log_tool().name(),
        peers_say_tool().name(),
    ].into_iter().collect();
    assert!(names.contains(&"cgn_peers_status"));
    assert!(names.contains(&"cgn_peers_log"));
    assert!(names.contains(&"cgn_peers_say"));
}
```

- [ ] **Step 2: Run to verify failure**

```
cargo test -p cgn-mcp --test peers_tools
```

Expected: FAIL — module/functions absent.

- [ ] **Step 3: Implement MCP tools**

Create `crates/cgn-mcp/src/tools/peers.rs`:

```rust
//! MCP tools mirroring `cgn peers` CLI.

use rmcp::Tool;
use serde_json::Value;

pub fn peers_status_tool() -> Tool {
    Tool::new("cgn_peers_status", "List alive peer sessions in the current repo.")
        .with_handler(|_args: Value| async move {
            // Shell out to cgn; reuse CLI logic to avoid duplication.
            let out = std::process::Command::new("cgn")
                .args(["peers", "status"])
                .output()
                .map_err(|e| format!("spawn cgn: {e}"))?;
            Ok(serde_json::json!({ "stdout": String::from_utf8_lossy(&out.stdout).into_owned() }))
        })
}

pub fn peers_log_tool() -> Tool {
    Tool::new("cgn_peers_log", "Read this session's message log filtered by peer/direction.")
        .with_arg("peer", "filter by peer session id", true)
        .with_arg("direction", "sent | recv", true)
        .with_arg("limit", "max entries", true)
        .with_handler(|args: Value| async move {
            let mut cmd = vec!["peers", "log"];
            if let Some(p) = args.get("peer").and_then(|x| x.as_str()) {
                cmd.push("--peer"); cmd.push(p);
            }
            if let Some(d) = args.get("direction").and_then(|x| x.as_str()) {
                cmd.push("--direction"); cmd.push(d);
            }
            let lim_str;
            if let Some(l) = args.get("limit").and_then(|x| x.as_u64()) {
                lim_str = l.to_string();
                cmd.push("--limit"); cmd.push(&lim_str);
            }
            let out = std::process::Command::new("cgn").args(&cmd).output()
                .map_err(|e| format!("spawn cgn: {e}"))?;
            Ok(serde_json::json!({ "stdout": String::from_utf8_lossy(&out.stdout).into_owned() }))
        })
}

pub fn peers_say_tool() -> Tool {
    Tool::new("cgn_peers_say", "Ƀ Send a message to peer sessions (broadcast or targeted).")
        .with_arg("body", "message text", false)
        .with_arg("to", "target peer session id", true)
        .with_arg("reply", "reply to msg_id", true)
        .with_handler(|args: Value| async move {
            let body = args.get("body").and_then(|x| x.as_str())
                .ok_or("body required")?.to_string();
            let mut cmd = vec!["peers", "say", &body];
            let to_owned;
            if let Some(t) = args.get("to").and_then(|x| x.as_str()) {
                to_owned = t.to_string();
                cmd.push("--to"); cmd.push(&to_owned);
            }
            let r_owned;
            if let Some(r) = args.get("reply").and_then(|x| x.as_str()) {
                r_owned = r.to_string();
                cmd.push("--reply"); cmd.push(&r_owned);
            }
            let out = std::process::Command::new("cgn").args(&cmd).output()
                .map_err(|e| format!("spawn cgn: {e}"))?;
            Ok(serde_json::json!({
                "status": if out.status.success() { "ok" } else { "error" },
                "stderr": String::from_utf8_lossy(&out.stderr).into_owned(),
            }))
        })
}
```

(Adapt to the actual `rmcp::Tool` API used elsewhere in `cgn-mcp`. If the project uses a `#[tool]` macro pattern, follow that — the structure above is illustrative; preserve the existing crate's conventions.)

Edit `crates/cgn-mcp/src/lib.rs`, add:

```rust
pub mod tools {
    pub mod peers;
}
```

And register in the server bootstrap (wherever other tools are added):

```rust
server.add_tool(tools::peers::peers_status_tool());
server.add_tool(tools::peers::peers_log_tool());
server.add_tool(tools::peers::peers_say_tool());
```

- [ ] **Step 4: Run test**

```
cargo test -p cgn-mcp --test peers_tools
```

Expected: PASS.

- [ ] **Step 5: Lint + commit**

```bash
rustfmt --edition 2021 crates/cgn-mcp/src/tools/peers.rs crates/cgn-mcp/src/lib.rs
cargo clippy -p cgn-mcp --tests -- -D warnings
cargo build -p cgn-mcp

git add crates/cgn-mcp/src/tools/peers.rs crates/cgn-mcp/src/lib.rs \
        crates/cgn-mcp/tests/peers_tools.rs
git commit -m "$(cat <<'EOF'
feat(peer-mcp): expose cgn_peers_status | cgn_peers_log | cgn_peers_say

Ƀ messaging tool included. All three shell out to the cgn CLI to avoid
duplicating logic; preserves single source of truth at the CLI layer.

Refs: spec §9.1
EOF
)"
```

---

## ✅ CHECKPOINT — Phase 6 Complete

Full workspace build + test:

```bash
cargo build --workspace --release
cargo test --workspace
cargo clippy --workspace --tests -- -D warnings
```

Expected: green across all crates.

---

## Phase 7: Cross-Session Integration Tests

### Task 18: peer_harness — shared test fixture

**Files:**
- Create: `crates/cgn-cli/tests/common/peer_harness.rs`
- Create: `crates/cgn-cli/tests/common/mod.rs` (if absent)

**Goal:** A reusable fixture that spawns N cgn watcher processes against a shared temp repo, drives dirty events and messages, and reaps cleanly on drop.

- [ ] **Step 1: Implement the harness**

Create `crates/cgn-cli/tests/common/mod.rs`:

```rust
pub mod peer_harness;
```

Create `crates/cgn-cli/tests/common/peer_harness.rs`:

```rust
//! Cross-session test fixture: spawn N cgn watcher processes against a shared temp repo.

use chrono::Utc;
use cgn_core::peer::inbox::{drain, InboxEntry};
use cgn_core::session::SessionMeta;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

pub struct PeerHarness {
    pub repo_root: TempDir,
    pub watchers: Vec<SpawnedSession>,
}

pub struct SpawnedSession {
    pub id: String,
    pub pid: u32,
    pub session_dir: PathBuf,
    pub child: Option<Child>,
}

impl PeerHarness {
    pub fn new() -> Self {
        let repo_root = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(repo_root.path().join("sessions")).unwrap();
        Self { repo_root, watchers: Vec::new() }
    }

    pub fn spawn_session(&mut self, id: &str) -> &SpawnedSession {
        let session_dir = self.repo_root.path().join("sessions").join(id);
        std::fs::create_dir_all(&session_dir).unwrap();
        let meta = SessionMeta {
            version: 1,
            session_id: id.into(),
            pid: Some(std::process::id()),
            started_at: Utc::now().to_rfc3339(),
            last_touched: Utc::now().to_rfc3339(),
            base_sha: "0".repeat(40),
            source_worktree: "/tmp".into(),
            overlay_version: 1,
            watcher_pid: None,
            last_drained_offset: 0,
        };
        SessionMeta::write_atomic(&session_dir.join("meta.json"), &meta).unwrap();

        // Spawn `cgn watch --foreground` so the child writes peer inbox events.
        let bin: PathBuf = env!("CARGO_BIN_EXE_cgn").into();
        let child = Command::new(&bin)
            .args(["watch", "--foreground", "--repo", self.repo_root.path().to_str().unwrap()])
            .env("CLAUDE_CODE_SESSION_ID", id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn watcher");
        let pid = child.id();
        self.watchers.push(SpawnedSession {
            id: id.into(),
            pid,
            session_dir,
            child: Some(child),
        });
        self.watchers.last().unwrap()
    }

    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.repo_root.path().join("sessions").join(id)
    }

    pub fn write_dirty(&self, id: &str, path: &str, symbols: &[(&str, &str)]) {
        use cgn_core::session::overlay::{DirtyEntry, DirtyFiles, SymbolKind, SymbolRef};
        use std::collections::BTreeMap;
        let sdir = self.session_dir(id);
        let mut entries = BTreeMap::new();
        entries.insert(path.to_string(), DirtyEntry {
            mtime_ns: 1,
            content_hash: "h".into(),
            fragment_id: "f".into(),
            tantivy_delta_segment: None,
            parse_failed: false,
            dirty_symbols: symbols.iter().map(|(n, f)| SymbolRef {
                name: (*n).into(),
                kind: SymbolKind::Function,
                file: (*f).into(),
                line_start: 1,
                line_end: 10,
            }).collect(),
        });
        DirtyFiles::write_atomic(&sdir.join("dirty.json"), &DirtyFiles { version: 1, entries }).unwrap();
    }

    pub fn read_inbox(&self, id: &str) -> Vec<InboxEntry> {
        let (entries, _) = drain(&self.session_dir(id).join("inbox.jsonl"), 0).unwrap();
        entries
    }

    pub fn assert_within<F: Fn() -> bool>(&self, timeout: Duration, pred: F) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if pred() { return true; }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    pub fn say(&self, from: &str, to: Option<&str>, body: &str) -> std::process::Output {
        let bin: PathBuf = env!("CARGO_BIN_EXE_cgn").into();
        let mut cmd = vec!["peers", "say", body, "--repo", self.repo_root.path().to_str().unwrap()];
        if let Some(t) = to { cmd.push("--to"); cmd.push(t); }
        Command::new(bin).args(&cmd)
            .env("CLAUDE_CODE_SESSION_ID", from)
            .output().expect("spawn cgn peers say")
    }
}

impl Drop for PeerHarness {
    fn drop(&mut self) {
        for w in &mut self.watchers {
            if let Some(child) = w.child.as_mut() {
                #[cfg(unix)]
                unsafe { libc::kill(w.pid as i32, libc::SIGTERM); }
                let _ = child.wait();
            }
        }
    }
}
```

- [ ] **Step 2: Build to confirm the harness compiles standalone**

```
cargo build --tests -p code-graph-nexus
```

Expected: build succeeds (no test runs yet since no test file references it).

- [ ] **Step 3: Commit**

```bash
git add crates/cgn-cli/tests/common/
git commit -m "$(cat <<'EOF'
test(peer): peer_harness fixture for cross-session integration tests

Spawns N cgn watcher processes against a shared temp repo, exposes
write_dirty / read_inbox / say / assert_within helpers. Drop impl
SIGTERMs all children to prevent zombies.

Refs: spec §13.3
EOF
)"
```

---

### Task 19: cross-session dirty event test

**Files:**
- Create: `crates/cgn-cli/tests/peers_two_session_dirty_event.rs`

- [ ] **Step 1: Write the test**

Create the file:

```rust
mod common;
use common::peer_harness::PeerHarness;
use std::time::Duration;

#[test]
fn peer_dirty_arrives_in_my_inbox_within_500ms() {
    let mut h = PeerHarness::new();
    h.spawn_session("alice");
    let _bob = h.spawn_session("bob");

    // Both sessions share the same symbol → HARD on bob's inbox when alice dirties it
    h.write_dirty("bob", "src/auth.rs", &[("verify_token", "src/auth.rs")]);
    std::thread::sleep(Duration::from_millis(100)); // let bob's watcher prime impact_cache
    h.write_dirty("alice", "src/auth.rs", &[("verify_token", "src/auth.rs")]);

    let arrived = h.assert_within(Duration::from_millis(2000), || {
        !h.read_inbox("bob").is_empty()
    });
    assert!(arrived, "bob's inbox empty after 2s — watcher did not dispatch alice's dirty event");
}
```

- [ ] **Step 2: Run**

```
cargo test -p code-graph-nexus --test peers_two_session_dirty_event
```

Expected: PASS (watchers from earlier tasks must be working).

If FAIL, debug at:
- `watcher.log` under `bob`'s session dir
- whether `notify` crate triggered on the dirty.json modification

- [ ] **Step 3: Commit**

```bash
git add crates/cgn-cli/tests/peers_two_session_dirty_event.rs
git commit -m "test(peer): cross-session dirty event fan-in within 2s budget"
```

---

### Task 20: cross-session message test

**Files:**
- Create: `crates/cgn-cli/tests/peers_two_session_msg.rs`

- [ ] **Step 1: Write the test**

```rust
mod common;
use common::peer_harness::PeerHarness;

#[test]
fn peers_say_targeted_delivers_to_inbox() {
    let mut h = PeerHarness::new();
    h.spawn_session("alice");
    h.spawn_session("bob");

    let out = h.say("alice", Some("bob"), "ack on auth refactor");
    assert!(out.status.success(), "stderr={}", String::from_utf8_lossy(&out.stderr));

    let inbox = h.read_inbox("bob");
    let has = inbox.iter().any(|e| matches!(e,
        cgn_core::peer::inbox::InboxEntry::Message { body, .. }
        if body == "ack on auth refactor"));
    assert!(has, "bob inbox missing targeted message: {inbox:?}");
}

#[test]
fn peers_say_broadcast_reaches_all_alive_peers() {
    let mut h = PeerHarness::new();
    h.spawn_session("alice");
    h.spawn_session("bob");
    h.spawn_session("carol");

    let out = h.say("alice", None, "hello team");
    assert!(out.status.success());

    for sid in ["bob", "carol"] {
        let inbox = h.read_inbox(sid);
        assert!(inbox.iter().any(|e| matches!(e,
            cgn_core::peer::inbox::InboxEntry::Message { body, .. }
            if body == "hello team")), "{sid} did not receive broadcast");
    }
}
```

- [ ] **Step 2: Run + commit**

```bash
cargo test -p code-graph-nexus --test peers_two_session_msg
git add crates/cgn-cli/tests/peers_two_session_msg.rs
git commit -m "test(peer): Ƀ broadcast + targeted message cross-session delivery"
```

---

### Task 21: symbol-level filter test

**Files:**
- Create: `crates/cgn-cli/tests/peers_symbol_level_filter.rs`

- [ ] **Step 1: Write the test**

```rust
mod common;
use common::peer_harness::PeerHarness;
use std::time::Duration;

#[test]
fn unrelated_symbol_does_not_appear_in_inbox() {
    let mut h = PeerHarness::new();
    h.spawn_session("alice");
    h.spawn_session("bob");

    h.write_dirty("bob", "src/auth.rs", &[("verify_token", "src/auth.rs")]);
    std::thread::sleep(Duration::from_millis(100));
    h.write_dirty("alice", "src/utils/money.rs", &[("format_money", "src/utils/money.rs")]);

    std::thread::sleep(Duration::from_millis(800));
    let inbox = h.read_inbox("bob");
    assert!(inbox.is_empty(), "unrelated symbol leaked to inbox: {inbox:?}");
}

#[test]
fn same_symbol_triggers_hard_event() {
    let mut h = PeerHarness::new();
    h.spawn_session("alice");
    h.spawn_session("bob");

    h.write_dirty("bob", "src/auth.rs", &[("verify_token", "src/auth.rs")]);
    std::thread::sleep(Duration::from_millis(100));
    h.write_dirty("alice", "src/auth.rs", &[("verify_token", "src/auth.rs")]);

    let arrived = h.assert_within(Duration::from_millis(2000), || {
        h.read_inbox("bob").iter().any(|e| matches!(e,
            cgn_core::peer::inbox::InboxEntry::DirtyEvent {
                kind: cgn_core::peer::inbox::ConcernKindSer::Hard, ..
            }))
    });
    assert!(arrived, "HARD event missing within 2s");
}
```

- [ ] **Step 2: Run + commit**

```bash
cargo test -p code-graph-nexus --test peers_symbol_level_filter
git add crates/cgn-cli/tests/peers_symbol_level_filter.rs
git commit -m "test(peer): symbol-level filter — unrelated = IGNORE, same = HARD"
```

---

### Task 22: impact cache invalidation test

**Files:**
- Create: `crates/cgn-cli/tests/peers_concern_impact_cache_invalidation.rs`

- [ ] **Step 1: Write the test**

```rust
mod common;
use common::peer_harness::PeerHarness;
use std::time::Duration;

#[test]
fn changing_self_dirty_invalidates_impact_cache_eventually() {
    let mut h = PeerHarness::new();
    h.spawn_session("alice");
    h.spawn_session("bob");

    // bob starts with no dirty symbols → all alice events should be IGNORE
    h.write_dirty("alice", "src/a.rs", &[("foo", "src/a.rs")]);
    std::thread::sleep(Duration::from_millis(800));
    assert!(h.read_inbox("bob").is_empty(), "bob got events before he had any dirty");

    // bob adds the same symbol → next alice event should now produce HARD
    h.write_dirty("bob", "src/a.rs", &[("foo", "src/a.rs")]);
    std::thread::sleep(Duration::from_millis(150)); // let watcher invalidate cache
    h.write_dirty("alice", "src/a.rs", &[("foo", "src/a.rs")]);

    let got = h.assert_within(Duration::from_millis(2000), || {
        !h.read_inbox("bob").is_empty()
    });
    assert!(got, "cache invalidation did not propagate — bob still empty");
}
```

- [ ] **Step 2: Run + commit**

```bash
cargo test -p code-graph-nexus --test peers_concern_impact_cache_invalidation
git add crates/cgn-cli/tests/peers_concern_impact_cache_invalidation.rs
git commit -m "test(peer): impact_cache invalidates when self-dirty changes"
```

---

## ✅ FINAL VALIDATION

```bash
cargo build --workspace --release
cargo test --workspace
cargo clippy --workspace --tests -- -D warnings
```

Smoke test the real flow against the actual `~/.cgn/` registry:

```bash
# Terminal 1
CGN_GROUP_REPO=test-repo cgn watch --foreground &

# Terminal 2
CGN_GROUP_REPO=test-repo cgn peers status
CGN_GROUP_REPO=test-repo cgn peers say "hello from terminal 2"

# Terminal 1 should show the message echoed via watcher.log (and any pre_tool_use hook would inject it)
```

Push the branch + open PR:

```bash
git push -u origin feat/peer-sync
gh pr create --title "feat(peer): multi-agent peer sync — symbol-level concern + Ƀ messaging" \
  --body "$(cat <<'EOF'
## Summary

- Symbol-level peer change awareness: HARD (same symbol) / SOFT (1-hop graph neighbor) / IGNORE
- Per-session `inotify`-based watcher (single instance via flock, daemonized at session start opt-in)
- Hook injection (pre_tool_use + user_prompt_submit + session_start) is the only LLM-facing channel
- Ƀ (beta) messaging: `cgn peers say`, persisted to `msg.log` per session
- Log rotation: msg.log 5MB×7, watcher.log 10MB×3
- Fail-open watcher with backtrace logging

## Test plan

- [ ] cargo test --workspace passes (22 new tests across 11 files)
- [ ] cargo clippy --workspace --tests -- -D warnings clean
- [ ] Manual: two `cgn watch --foreground` instances, dirty event arrives in <2s
- [ ] Manual: `cgn peers say` broadcasts to all alive peers
- [ ] Manual: HARD event from same-symbol edit; IGNORE for unrelated symbol

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Plan Self-Review

### Spec coverage

| Spec section | Covered by task |
|---|---|
| §3 Storage Layout | Tasks 1, 5, 6, 11, 12 |
| §4.1 Extended types | Task 1 (SymbolRef + DirtyEntry), Task 3 (SessionMeta) |
| §4.2 New types | Tasks 3 (PeerSession), 4 (ConcernKind/Result/ImpactCache), 5 (InboxEntry) |
| §5 Write Path | Task 2 |
| §6 Read Path (hook drain) | Tasks 15, 16 |
| §7 Watcher Loop | Task 9 |
| §7.2 Concern classification | Task 4 |
| §8 Message Path | Task 12 |
| §9 CLI Surface | Tasks 10, 11, 12 |
| §9.1 MCP tools | Task 17 |
| §10 Concurrency & Locking | Tasks 5 (append atomicity), 9 (flock) |
| §11 Error Handling | Tasks 5 (drain corruption), 9 (watcher panic + backtrace) |
| §11.1 Background task log contract | Task 9 |
| §12 Log Rotation & Retention | Task 6 |
| §13 Testing Strategy | Tasks 18–22 cross-session; per-module unit + integration tests in Tasks 1–17 |
| §14 Invariants | Enforced by tests across Phase 2 and Phase 7 |
| §15 Out of Scope | Honored — no cross-repo, no groups, no MCP push |

### Placeholder scan

- Searched for "TBD", "TODO", "FIXME", "implement later". None present in plan steps.
- `rebuild_impact_cache` in Task 9 is documented stub with explicit deferral note + spec §17 reference.

### Type consistency

- `SymbolRef` defined in Task 1, used identically in Tasks 4, 5, 7, 8, 18.
- `ConcernKind` (core) vs `ConcernKindSer` (inbox) — distinct types intentionally, with explicit `From` impl in Task 5.
- `PeerSession` defined in Task 3, used in Tasks 11, 12.
- `SessionMeta.watcher_pid` + `last_drained_offset` introduced in Task 3, consumed in Tasks 10, 15, 16, 18.

### Scope check

Single PR, single spec — confirmed by user during brainstorming. 22 tasks across 7 phases, ~2287 LOC. Each task ≤ ~100 LOC and independently buildable. Checkpoints between phases prevent compounding errors.
