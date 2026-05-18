# gnx CLI Redesign — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild gnx CLI surface from 25 visible commands down to 9 agent commands + 7 admin commands, with multi-group repo membership, auto-ensure indexing, server-side composition (no agent chaining), and a strict empty-result / error / hint output contract. Spec: `docs/specs/2026-05-15-gnx-cli-redesign-design.md`.

**Architecture:** Two-tier CLI via nested clap subcommands. Top-level enum exposes 9 agent verbs + `Admin(AdminCommands)` variant (hidden). AdminCommands enum nests `Group(GroupCommands)` for membership ops. Global `--repo` flag on the `Cli` struct uses a custom parser for the selector grammar (path / name / @group / @all / CSV mix), resolved through `registry::resolve_repos()` that fans out to `Vec<RepoPath>`. Multi-repo commands run via rayon parallel load (pattern adopted from existing `multi_query`). Auto-ensure index runs synchronously inline before query when graph.bin is missing.

**Tech Stack:** Rust 2021, clap 4.6 derive, rayon (multi-repo fan-out), _etoon 0.1.4+ (output), rkyv (graph mmap), tree-sitter (rename/scan).

**Spec reference:** `docs/specs/2026-05-15-gnx-cli-redesign-design.md`

---

## API Conventions Used in This Plan

Where the plan writes shorthand like `registry::load()`, the actual API is:

```rust
let path = graph_nexus_core::registry::default_path();  // resolves ~/.gnx/registry.json
let reg  = graph_nexus_core::registry::RegistryFile::read_or_empty(&path)
              .map_err(|e| GnxError::Io(format!("registry read: {e}")))?;
// ... mutate reg ...
graph_nexus_core::registry::RegistryFile::write_atomic(&path, &reg)
    .map_err(|e| GnxError::Io(format!("registry write: {e}")))?;
```

If a helper named `registry::load()` doesn't exist when implementing a task, expand to the two-line form above. The naming may evolve; **check `crates/graph-nexus-core/src/registry/mod.rs`** for the canonical API before each task.

Similarly, where the plan writes `Engine::load(&path)` — that's the real API, defined in `crates/graph-nexus-cli/src/engine.rs`.

For `GnxError` variants used (`Output`, `Io`, `Rkyv`): see `crates/graph-nexus-core/src/error.rs` for the actual enum; use the closest fit if a variant name doesn't match.

---

## Pre-flight

This worktree (`worktree-gnx-cli-eval`) was created from origin/main at `95d4228` (before `9bdc8e9 multi_query` was merged). Pull latest before starting:

```bash
git fetch origin main
git rebase origin/main
```

If conflicts: this plan assumes a state where `multi_query` already exists (`commands/multi_query.rs`); plan tasks reference it explicitly.

---

## File Structure

**Create:**
- `crates/graph-nexus-cli/src/commands/inspect.rs` — renamed from `context.rs`, composed output
- `crates/graph-nexus-cli/src/commands/search.rs` — renamed from `query.rs`, hybrid modes + cross-repo fan-out (absorbs multi_query logic)
- `crates/graph-nexus-cli/src/commands/coverage.rs` — folds doctor + status + list + summarize + tool_map summary
- `crates/graph-nexus-cli/src/commands/routes.rs` — folds route_map + api_impact
- `crates/graph-nexus-cli/src/commands/scan.rs` — file-level hallucination check (new)
- `crates/graph-nexus-cli/src/commands/contracts.rs` — cross-repo contracts inventory (new)
- `crates/graph-nexus-cli/src/commands/admin/mod.rs` — admin subcommand namespace
- `crates/graph-nexus-cli/src/commands/admin/group.rs` — `gnx admin group add/remove`
- `crates/graph-nexus-cli/src/repo_selector.rs` — `--repo` selector parser + resolver
- `crates/graph-nexus-cli/src/auto_ensure.rs` — auto-ensure index helper
- `crates/graph-nexus-cli/src/hint.rs` — output contract helpers (empty / error / hint formatters)
- `crates/graph-nexus-cli/tests/inspect_cmd.rs` — replaces `context_cmd.rs`
- `crates/graph-nexus-cli/tests/search_cmd.rs` — replaces `query_cmd.rs`; absorbs `multi_query_cmd.rs`
- `crates/graph-nexus-cli/tests/coverage_cmd.rs` — replaces `doctor_cmd.rs`
- `crates/graph-nexus-cli/tests/routes_cmd.rs` — replaces `api_impact_cmd.rs`
- `crates/graph-nexus-cli/tests/scan_cmd.rs` — new
- `crates/graph-nexus-cli/tests/contracts_cmd.rs` — new
- `crates/graph-nexus-cli/tests/admin_group_cmd.rs` — new
- `crates/graph-nexus-cli/tests/repo_selector.rs` — new
- `crates/graph-nexus-cli/tests/auto_ensure.rs` — new

**Modify:**
- `crates/graph-nexus-cli/src/main.rs` — top-level Commands enum reshape
- `crates/graph-nexus-cli/src/commands/mod.rs` — module list reshape
- `crates/graph-nexus-cli/src/commands/impact.rs` — drop UID-only, add `--name` + `--since` modes
- `crates/graph-nexus-cli/src/commands/rename.rs` — add `--markdown` flag + post-rename verification + collision detection
- `crates/graph-nexus-cli/src/commands/cypher.rs` — error on multi-repo selector
- `crates/graph-nexus-core/src/registry/store.rs` — `RepoEntry.group: Option<String>` → `groups: Vec<String>` + auto-migration
- `crates/graph-nexus-cli/Cargo.toml` — confirm `rayon` is already in deps (added by multi_query)
- `README.md` — update CLI reference section
- `README_zh-TW.md` — update CLI reference section

**Delete (after migration):**
- `crates/graph-nexus-cli/src/commands/context.rs` (→ inspect)
- `crates/graph-nexus-cli/src/commands/query.rs` (→ search)
- `crates/graph-nexus-cli/src/commands/multi_query.rs` (folded into search)
- `crates/graph-nexus-cli/src/commands/doctor.rs` (→ coverage)
- `crates/graph-nexus-cli/src/commands/status.rs` (→ coverage)
- `crates/graph-nexus-cli/src/commands/list.rs` (→ coverage)
- `crates/graph-nexus-cli/src/commands/summarize.rs` (→ coverage)
- `crates/graph-nexus-cli/src/commands/tool_map.rs` (→ coverage --externals)
- `crates/graph-nexus-cli/src/commands/route_map.rs` (→ routes)
- `crates/graph-nexus-cli/src/commands/api_impact.rs` (→ routes)
- `crates/graph-nexus-cli/src/commands/detect_changes.rs` (→ impact --since)
- `crates/graph-nexus-cli/src/commands/cluster.rs` (dropped, use cypher)
- `crates/graph-nexus-cli/src/commands/process.rs` (dropped, use cypher)
- `crates/graph-nexus-cli/src/commands/analyze.rs` (→ admin index — but verify if c08b3e9 already moved it)
- `crates/graph-nexus-cli/src/commands/analyze_here.rs` (→ auto-ensure)
- `crates/graph-nexus-cli/src/commands/index.rs` (recovery; dropped — `admin index` replaces)
- `crates/graph-nexus-cli/src/commands/remove.rs` (folded into admin drop)
- Corresponding test files

---

## Phase 0 — Foundation

### Task 0.1: Sync to latest origin/main

**Files:**
- (working tree state)

- [ ] **Step 1: Fetch and rebase**

```bash
git fetch origin main
git rebase origin/main
```

Expected: clean rebase. If conflicts in `main.rs`, prefer origin/main version; the redesign rewrites it anyway.

- [ ] **Step 2: Verify state**

```bash
cargo build --release --bin gnx 2>&1 | tail -3
./target/release/gnx --help 2>&1 | head -5
```

Expected: build succeeds, `gnx --help` lists current 27 commands (including `multi_query`, `shape_check`).

- [ ] **Step 3: Commit pre-flight marker**

```bash
git commit --allow-empty -m "chore(cli-redesign): begin agent-first CLI redesign per docs/specs/2026-05-15"
```

---

### Task 0.2: Schema migration — `RepoEntry.groups: Vec<String>`

**Files:**
- Modify: `crates/graph-nexus-core/src/registry/store.rs`
- Create: `crates/graph-nexus-core/src/registry/migrate.rs`
- Test: `crates/graph-nexus-core/src/registry/store.rs` (inline tests at bottom)

- [ ] **Step 1: Write failing migration test**

Add at the bottom of `crates/graph-nexus-core/src/registry/store.rs`:

```rust
#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn migrate_single_group_to_vec() {
        let old_json = serde_json::json!({
            "version": 1,
            "repos": [{
                "name": "alpha",
                "remote_url": "https://example.com/alpha.git",
                "worktree_path": "/tmp/alpha",
                "index_dir_root": "/tmp/idx/alpha",
                "branches": [],
                "group": "backend"
            }],
            "groups": []
        });
        let parsed: RegistryFile = serde_json::from_value(old_json).unwrap();
        assert_eq!(parsed.repos[0].groups, vec!["backend".to_string()]);
    }

    #[test]
    fn migrate_null_group_to_empty_vec() {
        let old_json = serde_json::json!({
            "version": 1,
            "repos": [{
                "name": "alpha",
                "remote_url": "x",
                "worktree_path": "/tmp/a",
                "index_dir_root": "/tmp/i",
                "branches": [],
                "group": null
            }],
            "groups": []
        });
        let parsed: RegistryFile = serde_json::from_value(old_json).unwrap();
        assert!(parsed.repos[0].groups.is_empty());
    }

    #[test]
    fn new_format_round_trips() {
        let entry = RepoEntry {
            name: "x".into(),
            remote_url: "x".into(),
            worktree_path: "x".into(),
            index_dir_root: "x".into(),
            branches: vec![],
            groups: vec!["a".into(), "b".into()],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: RepoEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.groups, vec!["a", "b"]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p graph-nexus-core registry::store::migration_tests 2>&1 | tail -15
```

Expected: FAIL — `groups` field does not exist on `RepoEntry`.

- [ ] **Step 3: Update schema with serde compatibility shim**

In `crates/graph-nexus-core/src/registry/store.rs`, change `RepoEntry`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoEntry {
    pub name: String,
    pub remote_url: String,
    pub worktree_path: String,
    pub index_dir_root: String,
    pub branches: Vec<BranchEntry>,
    /// Group memberships. Multi-group support added 2026-05-15.
    /// On read: old `group: Option<String>` auto-migrates to vec via
    /// `deserialize_with`. On write: only the new `groups` field is
    /// emitted (single-source schema).
    #[serde(default, deserialize_with = "deserialize_groups")]
    pub groups: Vec<String>,
}

/// Accept legacy `group: Option<String>` and new `groups: Vec<String>`
/// at the same JSON path-coordinates. Uses serde's untagged enum trick
/// to peek the shape.
fn deserialize_groups<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Compat {
        New(Vec<String>),
        Old(Option<String>),
    }
    Ok(match Compat::deserialize(deserializer)? {
        Compat::New(v) => v,
        Compat::Old(Some(s)) => vec![s],
        Compat::Old(None) => vec![],
    })
}
```

The serde `field` for the legacy `group` key needs handling too — add at the struct level:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RepoEntryRaw")]
pub struct RepoEntry { /* as above */ }

#[derive(Deserialize)]
struct RepoEntryRaw {
    name: String,
    remote_url: String,
    worktree_path: String,
    index_dir_root: String,
    #[serde(default)]
    branches: Vec<BranchEntry>,
    #[serde(default, alias = "group")]
    groups: Option<GroupsField>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum GroupsField {
    Vec(Vec<String>),
    Single(String),
}

impl From<RepoEntryRaw> for RepoEntry {
    fn from(raw: RepoEntryRaw) -> Self {
        let groups = match raw.groups {
            None => vec![],
            Some(GroupsField::Vec(v)) => v,
            Some(GroupsField::Single(s)) => vec![s],
        };
        RepoEntry {
            name: raw.name,
            remote_url: raw.remote_url,
            worktree_path: raw.worktree_path,
            index_dir_root: raw.index_dir_root,
            branches: raw.branches,
            groups,
        }
    }
}
```

Remove the inline `#[serde(default, deserialize_with = ...)]` from the first version.

- [ ] **Step 4: Run migration tests to verify pass**

```bash
cargo test -p graph-nexus-core registry::store::migration_tests 2>&1 | tail -10
```

Expected: PASS — all three tests.

- [ ] **Step 5: Run full registry test suite to verify no regression**

```bash
cargo test -p graph-nexus-core registry 2>&1 | tail -20
```

Expected: PASS, no regressions.

- [ ] **Step 6: Commit**

```bash
git add crates/graph-nexus-core/src/registry/store.rs
git commit -m "feat(registry): migrate RepoEntry.group→groups Vec for multi-group support

Backward-compatible deserialization: legacy 'group: Option<String>'
auto-migrates to 'groups: Vec<String>'. New schema is single-source
on write."
```

---

### Task 0.3: `--repo` selector parser

**Files:**
- Create: `crates/graph-nexus-cli/src/repo_selector.rs`
- Create: `crates/graph-nexus-cli/tests/repo_selector.rs`
- Modify: `crates/graph-nexus-cli/src/lib.rs` (add `pub mod repo_selector;`)

- [ ] **Step 1: Write failing tests**

Create `crates/graph-nexus-cli/tests/repo_selector.rs`:

```rust
use graph_nexus_cli::repo_selector::{parse, Atom, Selector};

#[test]
fn empty_selector_is_cwd() {
    let sel = parse("").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Cwd]));
}

#[test]
fn dot_is_path_cwd() {
    let sel = parse(".").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Path(".".into())]));
}

#[test]
fn absolute_path() {
    let sel = parse("/abs/path").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Path("/abs/path".into())]));
}

#[test]
fn registry_name() {
    let sel = parse("backend-svc").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Name("backend-svc".into())]));
}

#[test]
fn at_group() {
    let sel = parse("@backend").unwrap();
    assert_eq!(sel, Selector(vec![Atom::Group("backend".into())]));
}

#[test]
fn at_all() {
    let sel = parse("@all").unwrap();
    assert_eq!(sel, Selector(vec![Atom::All]));
}

#[test]
fn csv_mix() {
    let sel = parse("alpha,@beta,/abs/path").unwrap();
    assert_eq!(
        sel,
        Selector(vec![
            Atom::Name("alpha".into()),
            Atom::Group("beta".into()),
            Atom::Path("/abs/path".into()),
        ])
    );
}

#[test]
fn at_all_alone_no_csv() {
    // @all is exclusive with anything else (semantic check, not parse-level)
    let sel = parse("@all,alpha").unwrap();
    // parses, but resolver will reject (see Task 0.4)
    assert_eq!(sel.0.len(), 2);
}

#[test]
fn rejects_empty_atom() {
    // "a,,b" — middle empty atom is malformed
    assert!(parse("a,,b").is_err());
}

#[test]
fn rejects_at_without_name() {
    assert!(parse("@").is_err());
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p graph-nexus-cli --test repo_selector 2>&1 | tail -10
```

Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement parser**

Create `crates/graph-nexus-cli/src/repo_selector.rs`:

```rust
//! `--repo` selector grammar parser.
//!
//! selector := atom | atom,atom,...
//! atom     := <path> | <name> | @<group> | @all
//!
//! Resolution to actual repo paths is done by `resolve()` (see Task 0.4),
//! which needs registry access; this module only handles syntax.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Atom {
    /// No --repo flag given; resolves to current working directory.
    Cwd,
    /// `.` / `./rel` / `/abs/path` — looked up in registry by canonical path.
    Path(PathBuf),
    /// Registry name (no `@` prefix).
    Name(String),
    /// `@<group>` — expand via `RepoEntry.groups` membership.
    Group(String),
    /// `@all` — every registered repo.
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector(pub Vec<Atom>);

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("empty atom in --repo (consecutive commas?)")]
    EmptyAtom,
    #[error("`@` without name in --repo")]
    AtWithoutName,
}

pub fn parse(s: &str) -> Result<Selector, ParseError> {
    if s.is_empty() {
        return Ok(Selector(vec![Atom::Cwd]));
    }
    let atoms: Result<Vec<Atom>, _> = s.split(',').map(parse_atom).collect();
    Ok(Selector(atoms?))
}

fn parse_atom(part: &str) -> Result<Atom, ParseError> {
    if part.is_empty() {
        return Err(ParseError::EmptyAtom);
    }
    if let Some(rest) = part.strip_prefix('@') {
        if rest.is_empty() {
            return Err(ParseError::AtWithoutName);
        }
        if rest == "all" {
            return Ok(Atom::All);
        }
        return Ok(Atom::Group(rest.to_string()));
    }
    // Heuristic: anything containing '/' or starting with '.' is a path;
    // otherwise treat as registry name. Path canonicalization happens in
    // the resolver, not here.
    if part.starts_with('.') || part.starts_with('/') {
        return Ok(Atom::Path(PathBuf::from(part)));
    }
    Ok(Atom::Name(part.to_string()))
}
```

- [ ] **Step 4: Ensure module is exported**

Update `crates/graph-nexus-cli/src/lib.rs` (create if missing):

```rust
pub mod repo_selector;
```

If `lib.rs` already exists, append the line. Check current state:

```bash
cat crates/graph-nexus-cli/src/lib.rs 2>/dev/null
```

If file shows only `// placeholder` or similar, replace. If it has other content, append.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p graph-nexus-cli --test repo_selector 2>&1 | tail -10
```

Expected: PASS — all 9 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/graph-nexus-cli/src/repo_selector.rs \
        crates/graph-nexus-cli/src/lib.rs \
        crates/graph-nexus-cli/tests/repo_selector.rs
git commit -m "feat(cli): add --repo selector parser (path/name/@group/@all/csv)"
```

---

### Task 0.4: Multi-repo resolver

**Files:**
- Modify: `crates/graph-nexus-cli/src/repo_selector.rs`
- Modify: `crates/graph-nexus-cli/tests/repo_selector.rs`

- [ ] **Step 1: Write failing resolver tests**

Append to `crates/graph-nexus-cli/tests/repo_selector.rs`:

```rust
use graph_nexus_cli::repo_selector::{resolve, ResolveError, ResolvedRepo};
use graph_nexus_core::registry::{BranchEntry, GroupEntry, RegistryFile, RepoEntry};

fn make_registry() -> RegistryFile {
    RegistryFile {
        version: 1,
        repos: vec![
            RepoEntry {
                name: "alpha".into(),
                remote_url: "x".into(),
                worktree_path: "/tmp/alpha".into(),
                index_dir_root: "/tmp/idx/alpha".into(),
                branches: vec![BranchEntry {
                    name: "main".into(),
                    index_dir: "/tmp/idx/alpha/main".into(),
                    indexed_at: "2026-05-15".into(),
                    node_count: 100,
                    delta_size: 0,
                    embedding_status: "none".into(),
                }],
                groups: vec!["backend".into()],
            },
            RepoEntry {
                name: "beta".into(),
                remote_url: "y".into(),
                worktree_path: "/tmp/beta".into(),
                index_dir_root: "/tmp/idx/beta".into(),
                branches: vec![],
                groups: vec!["backend".into(), "auth".into()],
            },
        ],
        groups: vec![
            GroupEntry { name: "backend".into(), members: vec!["alpha".into(), "beta".into()] },
            GroupEntry { name: "auth".into(), members: vec!["beta".into()] },
        ],
    }
}

#[test]
fn resolve_name_finds_repo() {
    let sel = parse("alpha").unwrap();
    let resolved = resolve(&sel, &make_registry(), "/tmp/cwd").unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].name, "alpha");
}

#[test]
fn resolve_at_group_expands_members() {
    let sel = parse("@backend").unwrap();
    let resolved = resolve(&sel, &make_registry(), "/tmp/cwd").unwrap();
    assert_eq!(resolved.len(), 2);
    assert!(resolved.iter().any(|r| r.name == "alpha"));
    assert!(resolved.iter().any(|r| r.name == "beta"));
}

#[test]
fn resolve_at_all_returns_all() {
    let sel = parse("@all").unwrap();
    let resolved = resolve(&sel, &make_registry(), "/tmp/cwd").unwrap();
    assert_eq!(resolved.len(), 2);
}

#[test]
fn resolve_multi_group_union_dedups() {
    // backend = {alpha, beta}, auth = {beta} → union = {alpha, beta}
    let sel = parse("@backend,@auth").unwrap();
    let resolved = resolve(&sel, &make_registry(), "/tmp/cwd").unwrap();
    assert_eq!(resolved.len(), 2);
}

#[test]
fn resolve_unknown_name_errors() {
    let sel = parse("nonexistent").unwrap();
    let err = resolve(&sel, &make_registry(), "/tmp/cwd").unwrap_err();
    assert!(matches!(err, ResolveError::NotFound(ref s) if s == "nonexistent"));
}

#[test]
fn resolve_unknown_group_errors() {
    let sel = parse("@ghost").unwrap();
    let err = resolve(&sel, &make_registry(), "/tmp/cwd").unwrap_err();
    assert!(matches!(err, ResolveError::GroupNotFound(ref s) if s == "ghost"));
}
```

- [ ] **Step 2: Run tests, verify fail**

```bash
cargo test -p graph-nexus-cli --test repo_selector resolve 2>&1 | tail -10
```

Expected: FAIL — `resolve` not defined.

- [ ] **Step 3: Implement resolver**

Append to `crates/graph-nexus-cli/src/repo_selector.rs`:

```rust
use graph_nexus_core::registry::{RegistryFile, RepoEntry};
use std::collections::HashSet;
use std::path::Path;

/// A repo resolved from a selector atom — points into the registry,
/// retaining the name + paths the caller needs to load the graph.
#[derive(Debug, Clone)]
pub struct ResolvedRepo {
    pub name: String,
    pub worktree_path: String,
    pub index_dir_root: String,
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

/// Resolve a selector to a deduplicated list of repos. Preserves the first
/// occurrence order across the union to give the caller a stable iteration.
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
                let repo = find_by_path(registry, cwd)
                    .ok_or_else(|| ResolveError::PathNotRegistered(cwd.into()))?;
                push_unique(&mut seen, &mut out, repo);
            }
            Atom::Path(p) => {
                let p_str = p.to_string_lossy();
                let repo = find_by_path(registry, &p_str)
                    .ok_or_else(|| ResolveError::PathNotRegistered(p_str.into_owned()))?;
                push_unique(&mut seen, &mut out, repo);
            }
            Atom::Name(n) => {
                let repo = registry
                    .repos
                    .iter()
                    .find(|r| r.name == *n)
                    .ok_or_else(|| ResolveError::NotFound(n.clone()))?;
                push_unique(&mut seen, &mut out, repo);
            }
            Atom::Group(g) => {
                let group = registry
                    .groups
                    .iter()
                    .find(|gr| gr.name == *g)
                    .ok_or_else(|| ResolveError::GroupNotFound(g.clone()))?;
                for member_name in &group.members {
                    if let Some(repo) = registry.repos.iter().find(|r| r.name == *member_name) {
                        push_unique(&mut seen, &mut out, repo);
                    }
                }
            }
            Atom::All => {
                for repo in &registry.repos {
                    push_unique(&mut seen, &mut out, repo);
                }
            }
        }
    }
    Ok(out)
}

fn find_by_path<'a>(registry: &'a RegistryFile, p: &str) -> Option<&'a RepoEntry> {
    let target = Path::new(p)
        .canonicalize()
        .unwrap_or_else(|_| p.into());
    registry.repos.iter().find(|r| {
        Path::new(&r.worktree_path)
            .canonicalize()
            .map(|c| c == target)
            .unwrap_or(false)
    })
}

fn push_unique(seen: &mut HashSet<String>, out: &mut Vec<ResolvedRepo>, repo: &RepoEntry) {
    if seen.insert(repo.name.clone()) {
        out.push(ResolvedRepo {
            name: repo.name.clone(),
            worktree_path: repo.worktree_path.clone(),
            index_dir_root: repo.index_dir_root.clone(),
        });
    }
}
```

- [ ] **Step 4: Run tests, verify pass**

```bash
cargo test -p graph-nexus-cli --test repo_selector 2>&1 | tail -10
```

Expected: PASS — all 15 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/repo_selector.rs \
        crates/graph-nexus-cli/tests/repo_selector.rs
git commit -m "feat(cli): add multi-repo selector resolver with dedup"
```

---

### Task 0.5: Output contract helpers (hint module)

**Files:**
- Create: `crates/graph-nexus-cli/src/hint.rs`
- Create: `crates/graph-nexus-cli/tests/hint.rs`
- Modify: `crates/graph-nexus-cli/src/lib.rs` (add `pub mod hint;`)

- [ ] **Step 1: Write failing tests**

Create `crates/graph-nexus-cli/tests/hint.rs`:

```rust
use graph_nexus_cli::hint::{empty_result, error_with_cause, fuzzy_suggestions, stale_warning};

#[test]
fn empty_result_format() {
    let msg = empty_result("foo", "symbol", "gnx search foo --mode bm25");
    assert!(msg.contains("No"));
    assert!(msg.contains("foo"));
    assert!(msg.contains("gnx search"));
    // Max 2 lines of body content
    assert!(msg.lines().count() <= 3); // 1 reason + 1 next-step + maybe trailing newline
}

#[test]
fn fuzzy_suggestions_format() {
    let msg = fuzzy_suggestions("validate", &["validateUser", "validate_input", "Validator"]);
    assert!(msg.contains("Did you mean"));
    assert!(msg.contains("validateUser"));
    assert!(msg.contains("Validator"));
}

#[test]
fn error_with_cause_three_lines() {
    let msg = error_with_cause(
        "Index build failed",
        "framework not recognized",
        "gnx coverage --blind-spots",
    );
    let lines: Vec<&str> = msg.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with("✗"));
    assert!(lines[1].contains("cause:"));
    assert!(lines[2].contains("next:"));
}

#[test]
fn stale_warning_one_line() {
    let msg = stale_warning("alpha", "2h");
    assert!(msg.starts_with("⚠"));
    assert!(msg.contains("alpha"));
    assert!(msg.contains("2h"));
    assert_eq!(msg.lines().count(), 1);
}
```

- [ ] **Step 2: Run, verify fail**

```bash
cargo test -p graph-nexus-cli --test hint 2>&1 | tail -10
```

Expected: FAIL — module not defined.

- [ ] **Step 3: Implement hint helpers**

Create `crates/graph-nexus-cli/src/hint.rs`:

```rust
//! Output contract helpers — formatters for empty results, errors,
//! suggestions, warnings. See spec §7 (output contract) for rules.
//!
//! All formatters return strings caller writes to stderr (for warnings)
//! or appends after main stdout (for hints).

/// Empty result message: "No <kind> X found. <next-step suggestion>"
pub fn empty_result(query: &str, kind: &str, suggestion: &str) -> String {
    format!("No {kind} \"{query}\" found.\n→ {suggestion}")
}

/// Fuzzy match suggestion list when a name isn't found.
pub fn fuzzy_suggestions(query: &str, candidates: &[&str]) -> String {
    if candidates.is_empty() {
        return format!("No matches for \"{query}\".");
    }
    let list = candidates.join(" / ");
    format!("No symbol \"{query}\".\n→ Did you mean: {list}?")
}

/// Three-line error: "✗ <what>" / "  cause: <why>" / "  next: <how to recover>"
pub fn error_with_cause(what: &str, cause: &str, next: &str) -> String {
    format!("✗ {what}\n  cause: {cause}\n  next:  {next}")
}

/// One-line stale-index warning for stderr.
pub fn stale_warning(repo_name: &str, age: &str) -> String {
    format!("⚠ Index for \"{repo_name}\" is stale (last built {age} ago).")
}

/// Collision warning for rename pre-flight.
pub fn collision_warning(new_name: &str, existing_locations: &[String]) -> String {
    let locs = existing_locations.join("\n  - ");
    format!(
        "⚠️ COLLISION: \"{new_name}\" already exists at:\n  - {locs}\n→ Choose a different new name, or inspect: gnx inspect {new_name}"
    )
}
```

- [ ] **Step 4: Export from lib.rs**

Add to `crates/graph-nexus-cli/src/lib.rs`:

```rust
pub mod hint;
```

- [ ] **Step 5: Run tests, verify pass**

```bash
cargo test -p graph-nexus-cli --test hint 2>&1 | tail -10
```

Expected: PASS — all 4 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/graph-nexus-cli/src/hint.rs \
        crates/graph-nexus-cli/src/lib.rs \
        crates/graph-nexus-cli/tests/hint.rs
git commit -m "feat(cli): add output contract hint helpers (empty/error/fuzzy/stale)"
```

---

### Task 0.6: Auto-ensure index helper

**Files:**
- Create: `crates/graph-nexus-cli/src/auto_ensure.rs`
- Create: `crates/graph-nexus-cli/tests/auto_ensure.rs`
- Modify: `crates/graph-nexus-cli/src/lib.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/graph-nexus-cli/tests/auto_ensure.rs`:

```rust
use graph_nexus_cli::auto_ensure::{ensure_index, EnsureResult};
use std::fs;
use tempfile::TempDir;

#[test]
fn ensure_returns_ready_when_graph_exists() {
    let tmp = TempDir::new().unwrap();
    let graph_path = tmp.path().join("graph.bin");
    fs::write(&graph_path, vec![0u8; 16]).unwrap(); // placeholder bytes
    // mtime check needs a source older than graph — skip staleness for this case
    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    matches!(result, EnsureResult::Ready);
}

#[test]
fn ensure_reports_missing_when_graph_absent() {
    let tmp = TempDir::new().unwrap();
    let graph_path = tmp.path().join("nonexistent.bin");
    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(matches!(result, EnsureResult::Missing));
}

#[test]
fn ensure_reports_stale_when_source_newer() {
    let tmp = TempDir::new().unwrap();
    let graph_path = tmp.path().join("graph.bin");
    fs::write(&graph_path, vec![0u8; 16]).unwrap();
    // Touch a source file with a future mtime by writing after a brief wait
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(tmp.path().join("src.rs"), "fn foo() {}").unwrap();
    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(matches!(result, EnsureResult::Stale { .. }));
}
```

- [ ] **Step 2: Verify fail**

```bash
cargo test -p graph-nexus-cli --test auto_ensure 2>&1 | tail -10
```

Expected: FAIL — module not defined.

- [ ] **Step 3: Implement**

Create `crates/graph-nexus-cli/src/auto_ensure.rs`:

```rust
//! Auto-ensure index for agent CLI commands.
//!
//! Protocol (see spec §5):
//!   1. If graph.bin missing → caller should trigger `admin index` synchronously
//!      and retry. Returned as `EnsureResult::Missing`.
//!   2. If graph.bin present but mtime < newest source file → emit stale warning
//!      to stderr (caller continues). Returned as `EnsureResult::Stale { age }`.
//!   3. Otherwise → `EnsureResult::Ready`.
//!
//! This module checks status; it does NOT invoke the index build (callers
//! decide whether to auto-build or surface the missing state — `cypher` may
//! prefer to fail, `inspect` will auto-build).

use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureResult {
    /// Graph exists and is fresher than working tree.
    Ready,
    /// Graph does not exist; caller should index.
    Missing,
    /// Graph exists but working tree has newer files.
    /// `age_seconds` = how long since graph was last built.
    Stale { age_seconds: u64 },
}

pub fn ensure_index(graph_path: &Path, worktree_root: &Path) -> io::Result<EnsureResult> {
    let graph_mtime = match fs::metadata(graph_path) {
        Ok(m) => m.modified()?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(EnsureResult::Missing),
        Err(e) => return Err(e),
    };

    // Find newest source-file mtime under worktree (skip .git, target, node_modules).
    let newest = newest_source_mtime(worktree_root)?;
    if let Some(src_mtime) = newest {
        if src_mtime > graph_mtime {
            let age = SystemTime::now()
                .duration_since(graph_mtime)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            return Ok(EnsureResult::Stale { age_seconds: age });
        }
    }
    Ok(EnsureResult::Ready)
}

fn newest_source_mtime(root: &Path) -> io::Result<Option<SystemTime>> {
    let mut newest: Option<SystemTime> = None;
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                ".git" | "target" | "node_modules" | ".gitnexus-rs"
            )
        })
        .filter_map(Result::ok)
    {
        if entry.file_type().is_file() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    newest = match newest {
                        None => Some(mtime),
                        Some(curr) if mtime > curr => Some(mtime),
                        _ => newest,
                    };
                }
            }
        }
    }
    Ok(newest)
}
```

- [ ] **Step 4: Add walkdir dep + export module**

Check `Cargo.toml`:

```bash
grep -n "walkdir" crates/graph-nexus-cli/Cargo.toml
```

If missing, add to `[dependencies]`:

```toml
walkdir = "2"
tempfile = "3"  # for tests, under [dev-dependencies] if not already
```

Add to `crates/graph-nexus-cli/src/lib.rs`:

```rust
pub mod auto_ensure;
```

- [ ] **Step 5: Run tests, verify pass**

```bash
cargo test -p graph-nexus-cli --test auto_ensure 2>&1 | tail -10
```

Expected: PASS — all 3 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/graph-nexus-cli/src/auto_ensure.rs \
        crates/graph-nexus-cli/src/lib.rs \
        crates/graph-nexus-cli/tests/auto_ensure.rs \
        crates/graph-nexus-cli/Cargo.toml
git commit -m "feat(cli): add auto-ensure helper detecting Missing/Stale/Ready"
```

---

## Phase 1 — Top-Level CLI Restructure

### Task 1.1: Reshape `Commands` enum to 9 agent variants + hidden `Admin`

**Files:**
- Modify: `crates/graph-nexus-cli/src/main.rs`
- Create: `crates/graph-nexus-cli/src/commands/admin/mod.rs`

This task is the **structural pivot** — after this, individual command implementations land into the new shape.

- [ ] **Step 1: Write failing CLI surface test**

Create `crates/graph-nexus-cli/tests/cli_surface.rs`:

```rust
use std::process::Command;

fn gnx_help() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_gnx"))
        .arg("--help")
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

fn gnx_admin_help() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_gnx"))
        .args(["admin", "--help"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn top_level_lists_nine_agent_commands() {
    let help = gnx_help();
    for cmd in [
        "inspect", "search", "impact", "rename", "cypher",
        "coverage", "routes", "scan", "contracts",
    ] {
        assert!(help.contains(cmd), "missing {cmd} in --help:\n{help}");
    }
}

#[test]
fn top_level_hides_admin() {
    let help = gnx_help();
    // The Admin variant exists but is hidden; "admin" should not appear
    // in the Commands section. Allow it to appear elsewhere (e.g., in
    // descriptions) by being precise: no leading-whitespace "admin" line.
    for line in help.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("admin ") {
            panic!("admin command leaked into top-level --help: {line}");
        }
    }
}

#[test]
fn admin_help_lists_seven_entries() {
    let help = gnx_admin_help();
    for cmd in [
        "install-hook", "drop", "prune", "rename-branch", "config", "group", "index",
    ] {
        assert!(help.contains(cmd), "missing {cmd} in admin --help:\n{help}");
    }
}

#[test]
fn no_old_top_level_commands() {
    let help = gnx_help();
    for old in [
        "analyze", "context", "query", "doctor", "status", "list",
        "summarize", "detect-changes", "route-map", "api-impact",
        "tool-map", "cluster", "process", "multi_query", "clean",
        "remove", "init",
    ] {
        for line in help.lines() {
            let trimmed = line.trim_start();
            assert!(
                !trimmed.starts_with(&format!("{old} ")),
                "old command {old} still visible in --help"
            );
        }
    }
}
```

- [ ] **Step 2: Run, verify fail**

```bash
cargo test -p graph-nexus-cli --test cli_surface 2>&1 | tail -15
```

Expected: FAIL — old commands still present, new structure not in place.

- [ ] **Step 3: Create admin module stub**

Create `crates/graph-nexus-cli/src/commands/admin/mod.rs`:

```rust
//! `gnx admin` subcommand namespace — registry / hooks / destructive ops.
//! Hidden from top-level `gnx --help` per spec §4.

use clap::Subcommand;

pub mod group;

#[derive(Subcommand, Debug)]
pub enum AdminCommands {
    /// Install git ref-transaction hook for branch tracking
    InstallHook(super::init::InitArgs),
    /// Delete a repo's index data + registry entry
    Drop(super::clean::CleanArgs),
    /// Remove orphan index dirs not in registry
    Prune(super::prune::PruneArgs),
    /// Rename a branch's index dir
    RenameBranch(super::rename_branch::RenameBranchArgs),
    /// Interactive TOML config editor
    Config(super::config::ConfigArgs),
    /// Manage repo group membership
    Group {
        #[command(subcommand)]
        command: group::GroupCommands,
    },
    /// Build or refresh the graph for a repo (explicit / bulk / embeddings)
    Index(super::analyze::AnalyzeArgs),
}

pub fn run(cmd: AdminCommands) -> Result<(), graph_nexus_core::GnxError> {
    match cmd {
        AdminCommands::InstallHook(args) => super::init::run(args),
        AdminCommands::Drop(args) => super::clean::run(args),
        AdminCommands::Prune(args) => super::prune::run(args),
        AdminCommands::RenameBranch(args) => super::rename_branch::run(args),
        AdminCommands::Config(args) => super::config::run(args),
        AdminCommands::Group { command } => group::run(command),
        AdminCommands::Index(args) => super::analyze::run(args),
    }
}
```

(`group.rs` is filled in Task 4.2 — for now create with a stub:)

Create `crates/graph-nexus-cli/src/commands/admin/group.rs`:

```rust
use clap::Subcommand;
use graph_nexus_core::GnxError;

#[derive(Subcommand, Debug)]
pub enum GroupCommands {
    /// Add a repo to a group (auto-creates group)
    Add {
        repo: String,
        group: String,
    },
    /// Remove a repo from a group (auto-deletes empty group)
    Remove {
        repo: String,
        group: String,
    },
}

pub fn run(_cmd: GroupCommands) -> Result<(), GnxError> {
    Err(GnxError::Output("admin group commands not implemented yet".into()))
}
```

- [ ] **Step 4: Add admin module to commands/mod.rs**

Modify `crates/graph-nexus-cli/src/commands/mod.rs` — add line at top:

```rust
pub mod admin;
```

- [ ] **Step 5: Reshape `Commands` enum in `main.rs`**

Replace the current `Commands` enum in `crates/graph-nexus-cli/src/main.rs` with:

```rust
#[derive(Subcommand)]
enum Commands {
    /// Show symbol's full context: signature, body, edges, callers, overrides, and 1-hop upstream impact
    Inspect(commands::context::ContextArgs),
    /// Find symbols by name or concept (auto bm25 / hybrid / vector)
    Search(commands::query::QueryArgs),
    /// Blast radius — from <name> or git diff via --since <ref>
    Impact(commands::impact::ImpactArgs),
    /// AST-aware multi-file rename
    Rename(commands::rename::RenameArgs),
    /// Cypher query escape hatch
    Cypher(commands::cypher::CypherArgs),
    /// Registry + repo health (indexed repos, freshness, frameworks, externals, blind spots)
    Coverage(commands::doctor::DoctorArgs),
    /// List HTTP routes; with path, show handler + caller chain
    Routes(commands::route_map::RouteMapArgs),
    /// Verify a file's symbol references exist in the graph
    Scan(commands::scan::ScanArgs),
    /// Cross-repo API contracts inventory (routes / queue / RPC)
    Contracts(commands::contracts::ContractsArgs),

    /// Administrative operations (registry, hooks, destructive ops)
    #[command(hide = true)]
    Admin {
        #[command(subcommand)]
        command: commands::admin::AdminCommands,
    },

    /// Internal: process reference-transaction events (called by git hook)
    #[command(hide = true)]
    HookHandle(commands::hook_handle::HookHandleArgs),
    /// Internal: detached watcher dispatched by hook-handle
    #[command(hide = true)]
    HookWatcher(commands::hook_watcher::HookWatcherArgs),
    /// Internal: diff resolver dump against language oracle (gnx-dev QA)
    #[command(hide = true)]
    VerifyResolver(commands::verify_resolver::VerifyResolverArgs),
}
```

Note: variants `Inspect`, `Search`, `Coverage`, `Routes` still wire to **old** struct types (`ContextArgs`, `QueryArgs`, `DoctorArgs`, `RouteMapArgs`) at this task. Phases 2–3 swap these to the new types. `Scan` and `Contracts` reference modules created in Phase 3.

- [ ] **Step 6: Stub `scan.rs` and `contracts.rs` so main.rs compiles**

Create `crates/graph-nexus-cli/src/commands/scan.rs`:

```rust
use clap::Args;
use crate::engine::Engine;
use graph_nexus_core::GnxError;

#[derive(Args, Debug, Clone)]
pub struct ScanArgs {
    /// File path to scan for symbol references
    pub file: String,
    /// Also flag uncertain references
    #[arg(long, default_value_t = false)]
    pub strict: bool,
}

pub fn run(_args: ScanArgs, _engine: &Engine) -> Result<(), GnxError> {
    Err(GnxError::Output("scan command stub — implement in Task 3.1".into()))
}
```

Create `crates/graph-nexus-cli/src/commands/contracts.rs`:

```rust
use clap::Args;
use crate::engine::Engine;
use graph_nexus_core::GnxError;

#[derive(Args, Debug, Clone)]
pub struct ContractsArgs {
    /// Contract kind: routes / queue / rpc / all
    #[arg(long, default_value = "all")]
    pub kind: String,
    /// Only show contracts without a paired consumer/producer
    #[arg(long, default_value_t = false)]
    pub unmatched_only: bool,
    /// Repo selector
    #[arg(long)]
    pub repo: Option<String>,
}

pub fn run(_args: ContractsArgs, _engine: &Engine) -> Result<(), GnxError> {
    Err(GnxError::Output("contracts command stub — implement in Task 3.2".into()))
}
```

Add both modules to `crates/graph-nexus-cli/src/commands/mod.rs`:

```rust
pub mod scan;
pub mod contracts;
```

- [ ] **Step 7: Update `main()` dispatch to handle new variant names**

In `main.rs`, rewrite the dispatch match (the `Commands::Analyze(...)` early returns and the final big match) to use new variant names. Sketch:

```rust
fn main() {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    // Admin commands — handled before graph load
    if let Commands::Admin { command } = cli.command {
        if let Err(e) = commands::admin::run(command) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }
    if let Commands::HookHandle(args) = &cli.command {
        if let Err(e) = commands::hook_handle::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }
    if let Commands::HookWatcher(args) = &cli.command {
        if let Err(e) = commands::hook_watcher::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }
    if let Commands::VerifyResolver(args) = &cli.command {
        if let Err(e) = commands::verify_resolver::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Agent commands — need graph
    let repo_opt = match &cli.command {
        Commands::Inspect(args) => args.repo.as_deref(),
        Commands::Search(args) => args.repo.as_deref(),
        Commands::Impact(args) => args.repo.as_deref(),
        Commands::Rename(args) => args.repo.as_deref(),
        Commands::Cypher(args) => args.repo.as_deref(),
        Commands::Coverage(args) => args.repo.as_deref(),
        Commands::Routes(args) => args.repo.as_deref(),
        Commands::Scan(_) => None,  // scan uses file path; repo inferred from file
        Commands::Contracts(args) => args.repo.as_deref(),
        Commands::Admin { .. } | Commands::HookHandle(_) | Commands::HookWatcher(_) | Commands::VerifyResolver(_) => None,
    };
    let cwd = repo_opt
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let graph_path = graph_path::resolve(&cli.graph, &cwd);
    let engine = match Engine::load(&graph_path) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("Error loading graph from {}: {}", graph_path.display(), err);
            std::process::exit(1);
        }
    };

    let result: Result<(), graph_nexus_core::GnxError> = match cli.command {
        Commands::Inspect(args) => commands::context::run(args, &engine),
        Commands::Search(args) => commands::query::run(args, &engine),
        Commands::Impact(args) => commands::impact::run(args, &engine),
        Commands::Rename(args) => commands::rename::run(args, &engine),
        Commands::Cypher(args) => commands::cypher::run(args, &engine),
        Commands::Coverage(args) => commands::doctor::run(args, &cli.graph),
        Commands::Routes(args) => commands::route_map::run(args, &engine),
        Commands::Scan(args) => commands::scan::run(args, &engine),
        Commands::Contracts(args) => commands::contracts::run(args, &engine),
        _ => Ok(()), // unreachable due to early returns above
    };
    if let Err(e) = result {
        eprintln!("Command failed: {e}");
        std::process::exit(1);
    }
}
```

- [ ] **Step 8: Build to verify no compile errors**

```bash
cargo build --bin gnx 2>&1 | tail -20
```

Expected: build succeeds (warnings OK, no errors).

- [ ] **Step 9: Run CLI surface tests to verify pass**

```bash
cargo test -p graph-nexus-cli --test cli_surface 2>&1 | tail -15
```

Expected: PASS — all 4 tests (`top_level_lists_nine_agent_commands`, `top_level_hides_admin`, `admin_help_lists_seven_entries`, `no_old_top_level_commands`).

- [ ] **Step 10: Commit**

```bash
git add crates/graph-nexus-cli/src/main.rs \
        crates/graph-nexus-cli/src/commands/mod.rs \
        crates/graph-nexus-cli/src/commands/admin/ \
        crates/graph-nexus-cli/src/commands/scan.rs \
        crates/graph-nexus-cli/src/commands/contracts.rs \
        crates/graph-nexus-cli/tests/cli_surface.rs
git commit -m "feat(cli): pivot to 9 agent + hidden admin namespace structure

Top-level lists only agent verbs. Admin verbs nested under hidden
'gnx admin'. Old commands remain wired to existing Args types;
subsequent tasks rename modules and add features."
```

---

## Phase 2 — Agent Command Implementations

### Task 2.1: `context.rs` → `inspect.rs` with composed output

**Files:**
- Move: `crates/graph-nexus-cli/src/commands/context.rs` → `crates/graph-nexus-cli/src/commands/inspect.rs`
- Modify: `crates/graph-nexus-cli/src/main.rs` (variant wire-up)
- Move: `crates/graph-nexus-cli/tests/context_cmd.rs` → `crates/graph-nexus-cli/tests/inspect_cmd.rs`
- Modify: `crates/graph-nexus-cli/tests/inspect_cmd.rs` (new assertions for composition)

- [ ] **Step 1: Write failing tests for composition**

Open `crates/graph-nexus-cli/tests/inspect_cmd.rs` (currently named context_cmd.rs) and add:

```rust
#[test]
fn inspect_ambiguous_returns_full_matches_not_uids() {
    // Set up a fixture with two functions named `validate` in different files.
    // Expectations:
    //   - Output contains both file paths
    //   - Output contains both signatures
    //   - Output does NOT contain UID strings (no `Method:<file>:<name>` format)
    let output = run_inspect_on_fixture("ambiguous_validate", "validate");
    assert!(output.contains("src/auth/user.rs"));
    assert!(output.contains("src/utils/check.rs"));
    assert!(output.contains("fn validate("));
    assert!(!output.contains("Method:src/auth/user.rs:validate"));
}

#[test]
fn inspect_unknown_returns_fuzzy_suggestions() {
    let output = run_inspect_on_fixture("ambiguous_validate", "validait");
    assert!(output.contains("Did you mean"));
    assert!(output.contains("validate"));
}

#[test]
fn inspect_includes_freshness_when_stale() {
    let output = run_inspect_on_stale_fixture("stale_index", "foo");
    assert!(output.contains("⚠") || output.contains("stale"));
}
```

(The `run_inspect_on_fixture` helper should use the existing test harness pattern — see `tests/context_cmd.rs` for an example with `assert_cmd::Command`.)

- [ ] **Step 2: Run, verify fail**

```bash
cargo test -p graph-nexus-cli --test inspect_cmd 2>&1 | tail -15
```

Expected: FAIL — module doesn't exist, helpers undefined.

- [ ] **Step 3: Move context.rs → inspect.rs**

```bash
git mv crates/graph-nexus-cli/src/commands/context.rs crates/graph-nexus-cli/src/commands/inspect.rs
git mv crates/graph-nexus-cli/tests/context_cmd.rs crates/graph-nexus-cli/tests/inspect_cmd.rs
```

In `inspect.rs`, rename the struct:

```rust
// before:  pub struct ContextArgs {
// after:   pub struct InspectArgs {
```

Use `gnx rename` (or sed) to update all references — there should be ~5 places in inspect.rs itself.

In `commands/mod.rs`:

```diff
-pub mod context;
+pub mod inspect;
```

In `main.rs`, update wire-up:

```diff
-    Inspect(commands::context::ContextArgs),
+    Inspect(commands::inspect::InspectArgs),
...
-        Commands::Inspect(args) => commands::context::run(args, &engine),
+        Commands::Inspect(args) => commands::inspect::run(args, &engine),
```

- [ ] **Step 4: Remove UID flag, add composition**

In `inspect.rs`:

```rust
#[derive(Args, Debug)]
pub struct InspectArgs {
    /// Symbol name to query
    pub name: String,
    /// Filter by file path (disambiguate same-name targets)
    #[arg(long)]
    pub file: Option<String>,
    /// Filter by kind (function | method | class | route | ...)
    #[arg(long)]
    pub kind: Option<String>,
    /// Caller / override chain depth
    #[arg(long, default_value_t = 1)]
    pub depth: usize,
    /// Include test files in edges
    #[arg(long, default_value_t = false)]
    pub include_tests: bool,
    /// Limit to relation types (csv: calls,extends,...)
    #[arg(long = "relation-types")]
    pub relation_types: Option<String>,
    /// Repository selector (path | name | @group | @all | csv mix)
    #[arg(long)]
    pub repo: Option<String>,
}
```

Drop `--uid` and `--format` flags entirely. Remove UID emission from the result builder (search for `Kind:filePath:name` style strings in `run()` and remove the field from the JSON value).

Add 1-hop upstream impact summary to the output payload (mirror the BFS in `commands::impact::run` but with `depth = 1`, append the result under key `"impact_upstream_1hop"` in the JSON `value` before `emit`).

Add freshness warning: after loading the graph, call `auto_ensure::ensure_index(&graph_path, worktree_path)`. If `Stale { age_seconds }`, write the warning to stderr via `crate::hint::stale_warning(...)`.

Replace the "ambiguous → candidate list with UID" branch with "ambiguous → emit all matches with full inspect blocks per match".

- [ ] **Step 5: Build + run tests**

```bash
cargo build --bin gnx 2>&1 | tail -3
cargo test -p graph-nexus-cli --test inspect_cmd 2>&1 | tail -15
```

Expected: build OK, all inspect tests pass.

- [ ] **Step 6: Run CLI surface tests**

```bash
cargo test -p graph-nexus-cli --test cli_surface 2>&1 | tail -10
```

Expected: still PASS.

- [ ] **Step 7: Commit**

```bash
git add -A crates/graph-nexus-cli/src/commands/inspect.rs \
          crates/graph-nexus-cli/src/commands/mod.rs \
          crates/graph-nexus-cli/src/main.rs \
          crates/graph-nexus-cli/tests/inspect_cmd.rs
git rm crates/graph-nexus-cli/src/commands/context.rs 2>/dev/null || true
git rm crates/graph-nexus-cli/tests/context_cmd.rs 2>/dev/null || true
git commit -m "feat(cli): inspect command (renamed from context) with composed output

Drops --uid flag, removes UID from output. Ambiguous matches return
full inspect blocks per match. Adds 1-hop upstream impact summary and
freshness warning inline."
```

---

### Task 2.2: `query.rs` → `search.rs` with hybrid modes + cross-repo

**Files:**
- Move: `crates/graph-nexus-cli/src/commands/query.rs` → `crates/graph-nexus-cli/src/commands/search.rs`
- Modify: search.rs to add `--mode` and absorb `multi_query.rs` parallel pattern
- Delete: `crates/graph-nexus-cli/src/commands/multi_query.rs`
- Delete: `crates/graph-nexus-cli/tests/multi_query_cmd.rs`
- Move: tests query_cmd.rs → search_cmd.rs (merge multi_query tests)

- [ ] **Step 1: Write failing tests**

Create or modify `crates/graph-nexus-cli/tests/search_cmd.rs`:

```rust
#[test]
fn search_slug_input_uses_bm25() {
    let out = run_search_with_mode_auto("validateU");
    assert!(out.contains("validateUser"));
    // Mode banner appears in stderr / debug section
    assert!(out.contains("bm25") || out.contains("auto→bm25"));
}

#[test]
fn search_nl_input_uses_vector_or_falls_back() {
    let out = run_search_with_mode_auto("user authentication flow");
    // If no embeddings: falls back to bm25 with stderr hint
    // If embeddings present: vector or hybrid
    assert!(out.contains("authentication") || out.contains("auth"));
}

#[test]
fn search_returns_inspect_style_info_per_match() {
    let out = run_search("validate");
    // Each match contains: name, kind, file, signature, 1-hop callers count
    assert!(out.contains("kind:"));
    assert!(out.contains("signature:"));
}

#[test]
fn search_multi_repo_via_at_group() {
    let out = run_search_multi_repo("validate", "@test-group");
    // Output contains matches from at least 2 repos with repo labels
    assert!(out.matches("repo:").count() >= 2);
}

#[test]
fn search_empty_result_suggests_alternatives() {
    let out = run_search("zzzzz_nonexistent");
    assert!(out.contains("No matches"));
    assert!(out.contains("--mode") || out.contains("--kind"));
}
```

- [ ] **Step 2: Verify fail**

```bash
cargo test -p graph-nexus-cli --test search_cmd 2>&1 | tail -15
```

Expected: FAIL.

- [ ] **Step 3: Rename module + struct**

```bash
git mv crates/graph-nexus-cli/src/commands/query.rs crates/graph-nexus-cli/src/commands/search.rs
git mv crates/graph-nexus-cli/tests/query_cmd.rs crates/graph-nexus-cli/tests/search_cmd.rs 2>/dev/null || true
```

In `search.rs`, rename `QueryArgs` → `SearchArgs`. Update mod.rs and main.rs accordingly.

- [ ] **Step 4: Add `--mode` and auto-detection**

In `search.rs`:

```rust
#[derive(clap::ValueEnum, Clone, Debug, PartialEq)]
pub enum SearchMode {
    Bm25,
    Vector,
    Hybrid,
    Auto,
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Pattern: name fragment, or natural-language description
    pub pattern: String,
    #[arg(long, value_enum, default_value_t = SearchMode::Auto)]
    pub mode: SearchMode,
    /// Filter by node kinds (csv)
    #[arg(long)]
    pub kind: Option<String>,
    /// Repository selector
    #[arg(long)]
    pub repo: Option<String>,
}

fn detect_mode(input: &str, embeddings_available: bool) -> SearchMode {
    let slug_like = input.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if slug_like {
        return SearchMode::Bm25;
    }
    if embeddings_available {
        SearchMode::Hybrid
    } else {
        eprintln!("→ falling back to bm25 (no embeddings — build with `gnx admin index --embeddings`)");
        SearchMode::Bm25
    }
}
```

- [ ] **Step 5: Absorb multi_query parallel-load pattern**

Open `crates/graph-nexus-cli/src/commands/multi_query.rs` and copy the rayon parallel-load + top-K heap merge logic into `search.rs::run`. Pseudocode:

```rust
pub fn run(args: SearchArgs, _engine: &Engine) -> Result<(), GnxError> {
    let registry = registry::load()?;
    let cwd = std::env::current_dir().map_err(|e| GnxError::Io(e.to_string()))?;
    let selector = crate::repo_selector::parse(args.repo.as_deref().unwrap_or(""))?;
    let resolved = crate::repo_selector::resolve(&selector, &registry, cwd.to_str().unwrap_or("."))?;

    let mode = if matches!(args.mode, SearchMode::Auto) {
        detect_mode(&args.pattern, embeddings_present_anywhere(&resolved))
    } else {
        args.mode.clone()
    };

    let hits: Vec<Hit> = resolved
        .par_iter()
        .flat_map_iter(|repo| {
            let graph_path = repo_graph_path(repo);
            let engine = Engine::load(&graph_path).ok()?;
            Some(search_in_repo(&engine, &args.pattern, &mode))
        })
        .flatten()
        .collect();

    // Top-K merge by score
    let merged = top_k_by_score(hits, 20);

    // Emit with inspect-style info per match
    let value = build_payload(&merged);
    emit(&value, OutputFormat::Toon)?;
    Ok(())
}
```

Reuse types (`Hit`, `OrderedHit`) from multi_query.rs.

- [ ] **Step 6: Delete `multi_query.rs` and its test**

```bash
git rm crates/graph-nexus-cli/src/commands/multi_query.rs
git rm crates/graph-nexus-cli/tests/multi_query_cmd.rs
```

Remove `pub mod multi_query;` from `commands/mod.rs`. Remove `MultiQuery(commands::multi_query::MultiQueryArgs)` variant from main.rs `Commands` enum (it was a remnant from origin/main).

- [ ] **Step 7: Verify build + tests**

```bash
cargo build --bin gnx 2>&1 | tail -3
cargo test -p graph-nexus-cli --test search_cmd 2>&1 | tail -15
cargo test -p graph-nexus-cli --test cli_surface 2>&1 | tail -10
```

Expected: build OK, search tests pass, surface tests still pass.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(cli): search command — fold multi_query, add hybrid modes

Renames query→search with --mode bm25|vector|hybrid|auto. Absorbs
multi_query's rayon parallel-load + top-K heap. Each match now
includes inspect-style signature + caller count. Auto-detect picks
mode from input shape with bm25 fallback when no embeddings."
```

---

### Task 2.3: `impact.rs` — drop UID, add `--name` + `--since`

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/impact.rs`
- Modify: `crates/graph-nexus-cli/tests/` — relevant impact tests

- [ ] **Step 1: Write failing tests**

In `crates/graph-nexus-cli/tests/impact_cmd.rs` (create if missing):

```rust
#[test]
fn impact_accepts_name_not_uid() {
    let out = run_impact_args(&["validateUser", "--direction", "up"]);
    assert!(!out.contains("error"));
    assert!(out.contains("caller") || out.contains("0 incoming"));
}

#[test]
fn impact_since_replaces_detect_changes() {
    let out = run_impact_args(&["--since", "HEAD~1"]);
    // Should list changed symbols + their downstream effects
    assert!(out.contains("changed") || out.contains("0 changes"));
}

#[test]
fn impact_high_trust_only_default_on() {
    let help = run_impact_args(&["--help"]);
    assert!(help.contains("--high-trust-only"));
    assert!(help.contains("default: true") || help.contains("[default: true]"));
}

#[test]
fn impact_empty_callers_includes_explanation() {
    let out = run_impact_args(&["entry_main", "--direction", "up"]);
    if out.contains("0 incoming") {
        assert!(out.contains("entry point") || out.contains("dead"));
    }
}
```

- [ ] **Step 2: Verify fail**

```bash
cargo test -p graph-nexus-cli --test impact_cmd 2>&1 | tail -10
```

Expected: FAIL.

- [ ] **Step 3: Refactor impact.rs**

Replace `--target` (UID-only) with `--name` + `--since` mutually exclusive:

```rust
#[derive(Args, Debug)]
pub struct ImpactArgs {
    /// Target symbol name (mutually exclusive with --since)
    pub name: Option<String>,
    /// Git ref — compute impact across all symbols changed since this ref
    #[arg(long, conflicts_with = "name")]
    pub since: Option<String>,
    /// Disambiguate when name has multiple matches
    #[arg(long)]
    pub file: Option<String>,
    /// Disambiguate by kind
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long, value_enum, default_value_t = Direction::Up)]
    pub direction: Direction,
    #[arg(long, default_value_t = 5)]
    pub depth: usize,
    /// Default ON — only follow confidence ≥ 0.8 edges (changed from prior false default)
    #[arg(long, default_value_t = true)]
    pub high_trust_only: bool,
    #[arg(long)]
    pub min_confidence: Option<f32>,
    #[arg(long, default_value_t = false)]
    pub include_tests: bool,
    #[arg(long = "relation-types")]
    pub relation_types: Option<String>,
    #[arg(long)]
    pub repo: Option<String>,
}
```

In `run()`, branch:

```rust
pub fn run(args: ImpactArgs, engine: &Engine) -> Result<(), GnxError> {
    match (args.name.as_ref(), args.since.as_ref()) {
        (Some(name), None) => impact_by_name(name, &args, engine),
        (None, Some(since_ref)) => impact_since(since_ref, &args, engine),
        (None, None) => Err(GnxError::Output(
            "impact requires either <name> or --since <ref>".into(),
        )),
        (Some(_), Some(_)) => unreachable!(), // clap conflicts_with prevents
    }
}
```

For `impact_since`, port the logic from `commands/detect_changes.rs::run` — git diff parse, identify changed symbols, compute downstream/upstream for each.

For empty-caller case (0 incoming), append a hint via `hint::empty_result(name, "incoming references", "Possible: entry point, dead code, recent rename. Try --direction both / --include-tests")`.

- [ ] **Step 4: Build + tests**

```bash
cargo build --bin gnx 2>&1 | tail -3
cargo test -p graph-nexus-cli --test impact_cmd 2>&1 | tail -15
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/impact.rs \
        crates/graph-nexus-cli/tests/impact_cmd.rs
git commit -m "refactor(impact): drop UID-only target, accept --name + --since

Absorbs detect_changes via --since <ref> mode. Default
--high-trust-only=true (changed from false). Empty-result hint
explains entry-point vs dead-code interpretation."
```

---

### Task 2.4: Fold `detect_changes` → drop module

**Files:**
- Delete: `crates/graph-nexus-cli/src/commands/detect_changes.rs`
- Delete: `crates/graph-nexus-cli/tests/detect_changes.rs`
- Modify: `crates/graph-nexus-cli/src/commands/mod.rs`
- Modify: `crates/graph-nexus-cli/src/main.rs`

- [ ] **Step 1: Verify impact_since works equivalently**

Run the old detect_changes test inputs against the new `impact --since`:

```bash
./target/release/gnx impact --since HEAD~1 --repo . 2>&1 | head -20
```

Expected: similar output shape to old `gnx detect_changes`.

- [ ] **Step 2: Delete module + test**

```bash
git rm crates/graph-nexus-cli/src/commands/detect_changes.rs
git rm crates/graph-nexus-cli/tests/detect_changes.rs
```

Remove `pub mod detect_changes;` from `commands/mod.rs`. The `DetectChanges` variant was already absent from the new Commands enum (Task 1.1).

- [ ] **Step 3: Build, verify no broken references**

```bash
cargo build --bin gnx 2>&1 | grep -E "error|warning: unused" | head -10
```

Expected: no errors. May surface unused-import warnings to clean up.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor(cli): remove detect_changes module (folded into impact --since)"
```

---

### Task 2.5: Fold doctor + status + list + summarize + tool_map → `coverage.rs`

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/coverage.rs`
- Delete: doctor.rs, status.rs, list.rs, summarize.rs, tool_map.rs
- Delete: corresponding test files
- Create: `crates/graph-nexus-cli/tests/coverage_cmd.rs`
- Modify: main.rs + mod.rs

- [ ] **Step 1: Write failing tests**

Create `crates/graph-nexus-cli/tests/coverage_cmd.rs`:

```rust
#[test]
fn coverage_without_repo_reports_registry_overview() {
    let out = run_coverage(&[]);
    assert!(out.contains("indexed repos"));
    assert!(out.contains("groups"));
}

#[test]
fn coverage_with_repo_reports_per_repo_health() {
    let out = run_coverage(&["--repo", "."]);
    assert!(out.contains("frameworks"));
    assert!(out.contains("freshness"));
    assert!(out.contains("externals"));
}

#[test]
fn coverage_blind_spots_section() {
    let out = run_coverage(&["--repo", ".", "--detailed"]);
    assert!(out.contains("blind") || out.contains("unsupported"));
}

#[test]
fn coverage_at_group_aggregates() {
    let out = run_coverage(&["--repo", "@test-group"]);
    // Per-repo rows present
    assert!(out.matches("repo:").count() >= 1);
}
```

- [ ] **Step 2: Verify fail**

```bash
cargo test -p graph-nexus-cli --test coverage_cmd 2>&1 | tail -10
```

Expected: FAIL.

- [ ] **Step 3: Build coverage.rs as merger of existing payloads**

Create `crates/graph-nexus-cli/src/commands/coverage.rs`:

```rust
//! `gnx coverage` — unified health report. Folds the old commands:
//!   doctor (framework coverage + blind spots) +
//!   status (per-repo staleness) +
//!   list (registry enumeration) +
//!   summarize (project overview, when --repo present) +
//!   tool_map (external integrations summary, lite).
//!
//! Without --repo: registry-level overview.
//! With --repo <path|name>: per-repo health.
//! With --repo @group: aggregated group health.

use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::GnxError;
use serde_json::{json, Value};

#[derive(Args, Debug, Clone)]
pub struct CoverageArgs {
    #[arg(long)]
    pub repo: Option<String>,
    /// Verbose per-section breakdown
    #[arg(long, default_value_t = false)]
    pub detailed: bool,
}

pub fn run(args: CoverageArgs, graph_path_global: &std::path::Path) -> Result<(), GnxError> {
    let mut sections: Vec<(&str, Value)> = vec![];

    // Section 1: registry overview (always, if no --repo or aggregated)
    let registry = graph_nexus_core::registry::load()
        .map_err(|e| GnxError::Output(format!("registry load: {e}")))?;
    sections.push(("indexed_repos", build_registry_overview(&registry, args.detailed)));
    sections.push(("groups", build_groups_overview(&registry)));

    // Section 2: per-repo health (if --repo present)
    if let Some(repo_sel) = args.repo.as_deref() {
        let cwd = std::env::current_dir().unwrap_or_default();
        let selector = crate::repo_selector::parse(repo_sel)
            .map_err(|e| GnxError::Output(format!("selector parse: {e}")))?;
        let resolved = crate::repo_selector::resolve(&selector, &registry, cwd.to_str().unwrap_or("."))
            .map_err(|e| GnxError::Output(format!("selector resolve: {e}")))?;
        sections.push(("per_repo", build_per_repo_health(&resolved, args.detailed)?));
    }

    let value = json!({
        "coverage": sections.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<serde_json::Map<_,_>>(),
    });
    emit(&value, OutputFormat::Toon)?;
    Ok(())
}

fn build_registry_overview(registry: &graph_nexus_core::registry::RegistryFile, detailed: bool) -> Value {
    // List repos with name, branches count, indexed_at of latest branch
    let rows: Vec<Value> = registry.repos.iter().map(|r| {
        let latest = r.branches.iter().map(|b| b.indexed_at.as_str()).max().unwrap_or("never");
        json!({
            "name": r.name,
            "branches": r.branches.len(),
            "last_indexed": latest,
            "groups": r.groups,
        })
    }).collect();
    if detailed {
        json!(rows)
    } else {
        json!({ "count": rows.len(), "rows": rows })
    }
}

fn build_groups_overview(registry: &graph_nexus_core::registry::RegistryFile) -> Value {
    let rows: Vec<Value> = registry.groups.iter().map(|g| {
        json!({ "name": g.name, "members": g.members.len() })
    }).collect();
    json!({ "count": rows.len(), "rows": rows })
}

fn build_per_repo_health(
    repos: &[crate::repo_selector::ResolvedRepo],
    detailed: bool,
) -> Result<Value, GnxError> {
    // For each resolved repo:
    //   - frameworks detected (port from doctor)
    //   - freshness (port from status)
    //   - externals summary (port from tool_map — counts by kind)
    //   - blind spots (port from doctor)
    let rows: Vec<Value> = repos.iter().map(|r| {
        // Stub — actual port of doctor/status/tool_map logic happens here.
        // Reference: see commands/doctor.rs::run, commands/status.rs::run,
        // commands/tool_map.rs::run for the field-level logic.
        json!({
            "repo": r.name,
            "frameworks": fetch_frameworks(r),
            "freshness": fetch_freshness(r),
            "externals_summary": fetch_externals_summary(r),
            "blind_spots": fetch_blind_spots(r),
        })
    }).collect();
    Ok(json!(rows))
}

// Helper functions ported from the old commands. Each one wraps existing
// logic from doctor.rs / status.rs / tool_map.rs respectively. Implementation
// detail: load the per-repo graph.bin, run the corresponding analysis function,
// build a small JSON object.

fn fetch_frameworks(_r: &crate::repo_selector::ResolvedRepo) -> Value {
    // TODO during implementation: port logic from commands/doctor.rs
    // The implementer can extract the framework-listing logic into a
    // shared helper in graph-nexus-core if it isn't already.
    json!({ "detected": [] })
}

fn fetch_freshness(_r: &crate::repo_selector::ResolvedRepo) -> Value {
    json!({ "status": "unknown" })
}

fn fetch_externals_summary(_r: &crate::repo_selector::ResolvedRepo) -> Value {
    json!({ "http": 0, "db": 0, "redis": 0, "queue": 0 })
}

fn fetch_blind_spots(_r: &crate::repo_selector::ResolvedRepo) -> Value {
    json!({ "unsupported": [] })
}
```

The TODO helpers above need to be filled in by porting the corresponding logic. The implementer should:

1. Open each of `commands/doctor.rs`, `commands/status.rs`, `commands/tool_map.rs`.
2. Extract their core analysis into reusable functions (likely in `graph-nexus-core`).
3. Wire those functions into the four `fetch_*` helpers.

Test as you port each helper; the tests in Step 1 above will start passing as functionality lands.

- [ ] **Step 4: Delete the old modules**

```bash
git rm crates/graph-nexus-cli/src/commands/doctor.rs \
       crates/graph-nexus-cli/src/commands/status.rs \
       crates/graph-nexus-cli/src/commands/list.rs \
       crates/graph-nexus-cli/src/commands/summarize.rs \
       crates/graph-nexus-cli/src/commands/tool_map.rs
git rm crates/graph-nexus-cli/tests/doctor_cmd.rs 2>/dev/null || true
# Remove other test files referencing the deleted modules:
grep -l "use.*doctor\|use.*status\|use.*list\|use.*summarize\|use.*tool_map" crates/graph-nexus-cli/tests/*.rs
```

Update `commands/mod.rs` — remove the `pub mod` lines for deleted modules; add:

```rust
pub mod coverage;
```

Update `main.rs` — change `Commands::Coverage` wire-up from `commands::doctor::DoctorArgs` to `commands::coverage::CoverageArgs`. Same for dispatch:

```diff
-    Coverage(commands::doctor::DoctorArgs),
+    Coverage(commands::coverage::CoverageArgs),
...
-        Commands::Coverage(args) => commands::doctor::run(args, &cli.graph),
+        Commands::Coverage(args) => commands::coverage::run(args, &cli.graph),
```

- [ ] **Step 5: Build + tests**

```bash
cargo build --bin gnx 2>&1 | tail -3
cargo test -p graph-nexus-cli --test coverage_cmd 2>&1 | tail -15
cargo test -p graph-nexus-cli --test cli_surface 2>&1 | tail -10
```

Expected: build OK, coverage tests pass (or skip those needing fixture setup), surface tests pass.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(cli): coverage command — fold doctor + status + list + summarize + tool_map

Single entry for 'what do I have / how healthy is it'. Without --repo:
registry overview. With --repo: per-repo health. With @group: aggregated."
```

---

### Task 2.6: Fold `route_map` + `api_impact` → `routes.rs`

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/routes.rs`
- Delete: route_map.rs, api_impact.rs
- Modify: main.rs + mod.rs

- [ ] **Step 1: Write failing tests**

Create `crates/graph-nexus-cli/tests/routes_cmd.rs`:

```rust
#[test]
fn routes_no_path_lists_all() {
    let out = run_routes(&[]);
    assert!(out.contains("path") || out.contains("0 routes"));
}

#[test]
fn routes_with_path_shows_handler_chain() {
    let out = run_routes(&["/users/{id}"]);
    assert!(out.contains("handler") || out.contains("not found"));
}

#[test]
fn routes_empty_includes_framework_hint() {
    let out = run_routes_on_empty_fixture();
    assert!(out.contains("No HTTP routes"));
    assert!(out.contains("Framework") || out.contains("framework"));
}

#[test]
fn routes_method_filter() {
    let out = run_routes(&["--method", "GET"]);
    assert!(out.contains("GET") || out.contains("0 routes"));
}
```

- [ ] **Step 2: Verify fail**

```bash
cargo test -p graph-nexus-cli --test routes_cmd 2>&1 | tail -10
```

- [ ] **Step 3: Implement routes.rs**

Create `crates/graph-nexus-cli/src/commands/routes.rs`:

```rust
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::GnxError;

#[derive(Args, Debug, Clone)]
pub struct RoutesArgs {
    /// If given, show handler + caller chain for this route
    pub path: Option<String>,
    #[arg(long)]
    pub method: Option<String>,
    #[arg(long)]
    pub repo: Option<String>,
}

pub fn run(args: RoutesArgs, engine: &Engine) -> Result<(), GnxError> {
    match args.path.as_deref() {
        None => list_routes(engine, args.method.as_deref()),
        Some(path) => inspect_route(engine, path, args.method.as_deref()),
    }
}

fn list_routes(engine: &Engine, method: Option<&str>) -> Result<(), GnxError> {
    // Port body of old route_map::run, with `method` filter
    // ... (see commands/route_map.rs)
    Ok(())
}

fn inspect_route(engine: &Engine, path: &str, method: Option<&str>) -> Result<(), GnxError> {
    // Port body of old api_impact::run for the given path
    // ... (see commands/api_impact.rs)
    Ok(())
}
```

Port the bodies from `route_map.rs` and `api_impact.rs` into the two helpers. Where the original logic shares code, factor into private functions.

- [ ] **Step 4: Delete old modules**

```bash
git rm crates/graph-nexus-cli/src/commands/route_map.rs \
       crates/graph-nexus-cli/src/commands/api_impact.rs
git rm crates/graph-nexus-cli/tests/api_impact_cmd.rs 2>/dev/null || true
```

Update `mod.rs`:

```diff
-pub mod route_map;
-pub mod api_impact;
+pub mod routes;
```

Update `main.rs`:

```diff
-    Routes(commands::route_map::RouteMapArgs),
+    Routes(commands::routes::RoutesArgs),
...
-        Commands::Routes(args) => commands::route_map::run(args, &engine),
+        Commands::Routes(args) => commands::routes::run(args, &engine),
```

- [ ] **Step 5: Build + tests**

```bash
cargo build --bin gnx 2>&1 | tail -3
cargo test -p graph-nexus-cli --test routes_cmd 2>&1 | tail -15
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(cli): routes command — fold route_map + api_impact

Without path: list. With path: handler + caller chain."
```

---

### Task 2.7: `rename.rs` — add `--markdown`, post-rename verification, collision detection

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/rename.rs`
- Modify: `crates/graph-nexus-cli/tests/rename_cmd.rs`

- [ ] **Step 1: Write failing tests**

In `crates/graph-nexus-cli/tests/rename_cmd.rs`:

```rust
#[test]
fn rename_default_only_touches_code() {
    let workdir = setup_fixture_with_code_and_markdown("foo");
    run_rename(&workdir, &["foo", "bar"]);
    // Code rewritten
    let code = read(workdir.path().join("src/lib.rs"));
    assert!(code.contains("fn bar"));
    assert!(!code.contains("fn foo"));
    // Markdown UNTOUCHED
    let md = read(workdir.path().join("docs/api.md"));
    assert!(md.contains("foo"));
    assert!(!md.contains("bar"));
}

#[test]
fn rename_with_markdown_flag_touches_md() {
    let workdir = setup_fixture_with_code_and_markdown("foo");
    run_rename(&workdir, &["foo", "bar", "--markdown"]);
    let md = read(workdir.path().join("docs/api.md"));
    assert!(md.contains("bar"));
}

#[test]
fn rename_output_includes_residual_check() {
    let workdir = setup_fixture_with_string_literals("foo");
    let out = run_rename_capture(&workdir, &["foo", "bar"]);
    assert!(out.contains("still present") || out.contains("residual"));
}

#[test]
fn rename_output_includes_new_name_distribution() {
    let workdir = setup_fixture_simple("foo");
    let out = run_rename_capture(&workdir, &["foo", "bar"]);
    assert!(out.contains("bar") && out.contains("references"));
}

#[test]
fn rename_collision_detected_dry_run() {
    let workdir = setup_fixture_with_two_symbols("foo", "bar");
    let out = run_rename_capture(&workdir, &["foo", "bar", "--dry-run"]);
    assert!(out.contains("COLLISION"));
}

#[test]
fn rename_zero_occurrences_explicit_message() {
    let workdir = setup_fixture_empty();
    let out = run_rename_capture(&workdir, &["nonexistent", "newname"]);
    assert!(out.contains("No occurrences"));
}
```

- [ ] **Step 2: Verify fail**

```bash
cargo test -p graph-nexus-cli --test rename_cmd 2>&1 | tail -15
```

- [ ] **Step 3: Add `--markdown` flag + verification logic**

In `rename.rs`:

```rust
#[derive(Args, Debug)]
pub struct RenameArgs {
    pub old: String,
    pub new: String,
    /// Rename in source code identifiers (default ON)
    #[arg(long, default_value_t = true)]
    pub code: bool,
    /// Also rename in inline code comments
    #[arg(long, default_value_t = false)]
    pub comment: bool,
    /// Also rename in markdown / RST docs
    #[arg(long, default_value_t = false)]
    pub markdown: bool,
    /// Also rename in docstrings / cross-references
    #[arg(long, default_value_t = false)]
    pub reference: bool,
    /// Shortcut: --code --comment --markdown --reference
    #[arg(long, default_value_t = false)]
    pub all: bool,
    /// Preview without writing
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
    /// Repository selector
    #[arg(long)]
    pub repo: Option<String>,
}
```

After execution (or in dry-run mode), add a verification pass:

```rust
pub fn run(args: RenameArgs, engine: &Engine) -> Result<(), GnxError> {
    // 1. Pre-flight collision check (dry-run uses this; live run runs after)
    let collisions = find_collisions(&args.new, engine)?;
    if !collisions.is_empty() && args.dry_run {
        eprintln!("{}", crate::hint::collision_warning(&args.new, &collisions));
        // ... and continue to print the preview anyway
    }

    // 2. Execute (or simulate) the rename
    let changes = if args.dry_run {
        plan_rename(&args, engine)?
    } else {
        execute_rename(&args, engine)?
    };

    // 3. Build verification info
    let residuals = search_old_name_in_workdir(&args.old, &args)?;
    let new_distribution = search_new_name_in_workdir(&args.new, &args)?;

    // 4. Emit composed payload
    let payload = build_rename_payload(&args, &changes, &residuals, &new_distribution);
    emit(&payload, OutputFormat::Toon)?;

    // 5. Hint footer
    if !residuals.is_empty() {
        let opts: Vec<&str> = residuals.iter()
            .filter_map(|r| match r.location.as_str() {
                s if s.ends_with(".md") && !args.markdown => Some("--markdown"),
                s if s.ends_with(".comment") && !args.comment => Some("--comment"),
                _ => None,
            })
            .collect();
        if !opts.is_empty() {
            eprintln!("→ To also rename in those: gnx rename {} {} {}",
                args.old, args.new, opts.join(" "));
        }
    }

    Ok(())
}
```

Implement the helper functions:
- `find_collisions(new_name, engine)` — query graph for symbols named `new_name`; return their `file:line` locations.
- `plan_rename` / `execute_rename` — port existing rename logic, gated by which scope flags are set.
- `search_old_name_in_workdir` — ripgrep-style search of the working directory for residuals (string-literal context).
- `search_new_name_in_workdir` — same but for new name.
- `build_rename_payload` — assemble JSON with sections: `changes`, `residuals`, `new_distribution`.

- [ ] **Step 4: Build + tests**

```bash
cargo build --bin gnx 2>&1 | tail -3
cargo test -p graph-nexus-cli --test rename_cmd 2>&1 | tail -15
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/rename.rs \
        crates/graph-nexus-cli/tests/rename_cmd.rs
git commit -m "feat(rename): add --markdown, residual check, collision detection

Default code-only (safe). Post-rename auto-runs:
  - Old-name residual scan (with explicit non-zero report)
  - New-name distribution (with collision detection)
  - 0-occurrence case returns explicit 'No occurrences' message
Pre-flight collision in dry-run mode warns before applying."
```

---

### Task 2.8: `cypher.rs` — error on multi-repo selector

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/cypher.rs`

- [ ] **Step 1: Write failing test**

In `crates/graph-nexus-cli/tests/cypher_content.rs` (or create `cypher_cmd.rs`):

```rust
#[test]
fn cypher_rejects_multi_repo() {
    let out = run_cmd(&["cypher", "MATCH (n) RETURN n", "--repo", "@all"]);
    assert!(out.contains("single-repo") || out.contains("not supported"));
}
```

- [ ] **Step 2: Verify fail**

- [ ] **Step 3: Add early check in `cypher::run`**

```rust
pub fn run(args: CypherArgs, engine: &Engine) -> Result<(), GnxError> {
    // Multi-repo gate
    if let Some(repo_sel) = args.repo.as_deref() {
        let selector = crate::repo_selector::parse(repo_sel)
            .map_err(|e| GnxError::Output(format!("selector: {e}")))?;
        let registry = graph_nexus_core::registry::load().ok();
        let cwd = std::env::current_dir().unwrap_or_default();
        if let Some(reg) = registry {
            let resolved = crate::repo_selector::resolve(&selector, &reg, cwd.to_str().unwrap_or("."));
            if let Ok(repos) = resolved {
                if repos.len() > 1 {
                    return Err(GnxError::Output(
                        "cypher is single-repo only (graph identity); pick one repo".into(),
                    ));
                }
            }
        }
    }
    // ... existing body
}
```

- [ ] **Step 4: Build + test**

```bash
cargo build --bin gnx 2>&1 | tail -3
cargo test -p graph-nexus-cli --test cypher_content 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/cypher.rs \
        crates/graph-nexus-cli/tests/cypher_content.rs
git commit -m "feat(cypher): explicit error when --repo selector yields multi-repo"
```

---

## Phase 3 — New Commands

### Task 3.1: `scan.rs` — file-level hallucination check

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/scan.rs` (replaces stub from Task 1.1)
- Create: `crates/graph-nexus-cli/tests/scan_cmd.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/graph-nexus-cli/tests/scan_cmd.rs`:

```rust
#[test]
fn scan_clean_file_reports_ok() {
    let f = fixture_with_valid_refs();
    let out = run_scan(&f);
    assert!(out.contains("0 unresolved"));
}

#[test]
fn scan_lists_unresolved_with_fuzzy_suggestions() {
    let f = fixture_with_invalid_ref("vlidateUser"); // typo for validateUser
    let out = run_scan(&f);
    assert!(out.contains("vlidateUser"));
    assert!(out.contains("Did you mean") || out.contains("validateUser"));
}

#[test]
fn scan_parse_error_surfaces_with_recovery_hint() {
    let f = fixture_with_syntax_error();
    let out = run_scan(&f);
    assert!(out.contains("Cannot parse") || out.contains("parse error"));
}
```

- [ ] **Step 2: Verify fail**

```bash
cargo test -p graph-nexus-cli --test scan_cmd 2>&1 | tail -10
```

- [ ] **Step 3: Implement scan.rs**

Replace the stub from Task 1.1 with full implementation:

```rust
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use crate::hint;
use clap::Args;
use graph_nexus_core::GnxError;
use serde_json::{json, Value};

#[derive(Args, Debug, Clone)]
pub struct ScanArgs {
    pub file: String,
    #[arg(long, default_value_t = false)]
    pub strict: bool,
}

pub fn run(args: ScanArgs, engine: &Engine) -> Result<(), GnxError> {
    // 1. Parse file via tree-sitter (pick language by extension)
    let source = std::fs::read_to_string(&args.file)
        .map_err(|e| GnxError::Io(format!("read {}: {e}", args.file)))?;
    let lang = detect_language(&args.file)
        .ok_or_else(|| GnxError::Output(hint::error_with_cause(
            &format!("scan failed: unknown language for {}", args.file),
            "extension not in supported list",
            "supported extensions are listed in `gnx coverage --blind-spots`",
        )))?;

    let refs = extract_identifier_references(&source, lang)
        .map_err(|e| GnxError::Output(hint::error_with_cause(
            &format!("scan failed: cannot parse {}", args.file),
            &e,
            "verify file syntax or run `gnx coverage --blind-spots`",
        )))?;

    // 2. For each reference, check if symbol exists in graph
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let mut unresolved: Vec<Value> = vec![];
    for r in refs {
        if !symbol_exists(graph, &r.name) {
            let suggestions = fuzzy_match_symbols(graph, &r.name, 3);
            unresolved.push(json!({
                "name": r.name,
                "line": r.line,
                "column": r.column,
                "did_you_mean": suggestions,
            }));
        } else if args.strict && resolution_is_uncertain(graph, &r.name) {
            unresolved.push(json!({
                "name": r.name,
                "line": r.line,
                "column": r.column,
                "uncertain": true,
            }));
        }
    }

    // 3. Emit
    let payload = if unresolved.is_empty() {
        json!({ "status": "ok", "message": "File OK, 0 unresolved references ✓" })
    } else {
        json!({
            "status": "issues",
            "file": args.file,
            "unresolved": unresolved,
        })
    };
    emit(&payload, OutputFormat::Toon)
}

// Helper stubs to be implemented:
fn detect_language(path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(path).extension()?.to_str()?;
    match ext {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" => Some("javascript"),
        "go" => Some("go"),
        // ... extend per supported languages
        _ => None,
    }
}

struct Reference { name: String, line: u32, column: u32 }

fn extract_identifier_references(_source: &str, _lang: &str) -> Result<Vec<Reference>, String> {
    // TODO during implementation:
    // Use graph_nexus_analyzer's identifier_finder per language (see
    // crates/graph-nexus-analyzer/src/identifier_finder/<lang>.rs) to extract
    // all identifier references.
    Ok(vec![])
}

fn symbol_exists(_graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph, _name: &str) -> bool {
    // Iterate graph nodes, return true if any matches name
    false
}

fn fuzzy_match_symbols(
    _graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    _query: &str,
    _top_k: usize,
) -> Vec<String> {
    // TODO: bm25-style or Levenshtein top-K
    vec![]
}

fn resolution_is_uncertain(
    _graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
    _name: &str,
) -> bool {
    // TODO: check resolver-tier metadata
    false
}
```

The TODO helpers need filling — refer to:
- `crates/graph-nexus-analyzer/src/identifier_finder/` for per-language ref extraction
- `graph_nexus_core::graph` for node iteration
- Existing `commands/query.rs` for fuzzy-match patterns

- [ ] **Step 4: Build + tests**

```bash
cargo build --bin gnx 2>&1 | tail -3
cargo test -p graph-nexus-cli --test scan_cmd 2>&1 | tail -15
```

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/scan.rs \
        crates/graph-nexus-cli/tests/scan_cmd.rs
git commit -m "feat(cli): scan command — file-level symbol-reference hallucination check"
```

---

### Task 3.2: `contracts.rs` — cross-repo API contracts

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/contracts.rs` (replaces stub)
- Create: `crates/graph-nexus-cli/tests/contracts_cmd.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/graph-nexus-cli/tests/contracts_cmd.rs`:

```rust
#[test]
fn contracts_in_single_repo_warns() {
    let out = run_contracts(&["--repo", "."]);
    assert!(out.contains("1 member") || out.contains("need ≥2"));
}

#[test]
fn contracts_at_group_lists_pairs() {
    let out = run_contracts(&["--repo", "@test-group"]);
    assert!(out.contains("producer") && out.contains("consumer"));
}

#[test]
fn contracts_unmatched_only() {
    let out = run_contracts(&["--repo", "@test-group", "--unmatched-only"]);
    assert!(out.contains("unmatched") || out.contains("0 matches"));
}

#[test]
fn contracts_kind_filter() {
    let out = run_contracts(&["--repo", "@test-group", "--kind", "routes"]);
    assert!(out.contains("route") || out.contains("0"));
}
```

- [ ] **Step 2: Verify fail**

- [ ] **Step 3: Implement contracts.rs**

Replace stub with:

```rust
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::GnxError;
use serde_json::json;

#[derive(Args, Debug, Clone)]
pub struct ContractsArgs {
    #[arg(long, default_value = "all")]
    pub kind: String,
    #[arg(long, default_value_t = false)]
    pub unmatched_only: bool,
    #[arg(long)]
    pub repo: Option<String>,
}

pub fn run(args: ContractsArgs, _engine: &Engine) -> Result<(), GnxError> {
    // 1. Resolve --repo to multiple repos
    let registry = graph_nexus_core::registry::load()
        .map_err(|e| GnxError::Output(format!("registry load: {e}")))?;
    let cwd = std::env::current_dir().unwrap_or_default();
    let selector = crate::repo_selector::parse(args.repo.as_deref().unwrap_or(""))
        .map_err(|e| GnxError::Output(format!("selector parse: {e}")))?;
    let resolved = crate::repo_selector::resolve(&selector, &registry, cwd.to_str().unwrap_or("."))
        .map_err(|e| GnxError::Output(format!("selector resolve: {e}")))?;

    if resolved.len() < 2 {
        return Err(GnxError::Output(format!(
            "contracts needs ≥2 repos for cross-repo matching; got {}",
            resolved.len()
        )));
    }

    // 2. For each repo, extract contracts: routes (server) + clients (consumer)
    let mut producers: Vec<(String, ContractRef)> = vec![];
    let mut consumers: Vec<(String, ContractRef)> = vec![];
    for r in &resolved {
        let (p, c) = scan_contracts_in_repo(r, &args.kind)?;
        for x in p { producers.push((r.name.clone(), x)); }
        for x in c { consumers.push((r.name.clone(), x)); }
    }

    // 3. Match producers↔consumers (by path / topic / endpoint signature)
    let pairs = match_pairs(&producers, &consumers);

    let payload = if args.unmatched_only {
        json!({ "unmatched_producers": unmatched_producers(&producers, &pairs),
                "unmatched_consumers": unmatched_consumers(&consumers, &pairs) })
    } else {
        json!({ "pairs": pairs })
    };
    emit(&payload, OutputFormat::Toon)
}

#[derive(Debug, Clone)]
struct ContractRef {
    kind: String,    // "route" | "queue" | "rpc"
    path_or_topic: String,
    file: String,
    line: u32,
}

fn scan_contracts_in_repo(
    _r: &crate::repo_selector::ResolvedRepo,
    _kind_filter: &str,
) -> Result<(Vec<ContractRef>, Vec<ContractRef>), GnxError> {
    // TODO during implementation:
    // - Load this repo's graph.bin
    // - Iterate nodes with kind = Route (server endpoints)
    // - Iterate edges of type FETCHES (HTTP clients) and producer/consumer markers
    // - Filter by kind
    Ok((vec![], vec![]))
}

fn match_pairs(
    _producers: &[(String, ContractRef)],
    _consumers: &[(String, ContractRef)],
) -> Vec<serde_json::Value> {
    // TODO: match by path / topic equality; record each match
    vec![]
}

fn unmatched_producers(_producers: &[(String, ContractRef)], _pairs: &[serde_json::Value]) -> serde_json::Value {
    json!([])
}

fn unmatched_consumers(_consumers: &[(String, ContractRef)], _pairs: &[serde_json::Value]) -> serde_json::Value {
    json!([])
}
```

Fill TODOs by referencing `graph-nexus-core`'s Route node type and FETCHES edge type. The matching algorithm is straightforward: equality on `path_or_topic` (with optional kind tagging).

- [ ] **Step 4: Build + tests**

```bash
cargo build --bin gnx 2>&1 | tail -3
cargo test -p graph-nexus-cli --test contracts_cmd 2>&1 | tail -15
```

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/contracts.rs \
        crates/graph-nexus-cli/tests/contracts_cmd.rs
git commit -m "feat(cli): contracts command — cross-repo API contract producer↔consumer matching"
```

---

## Phase 4 — Admin Namespace

### Task 4.1: Wire admin sub-commands (move modules)

**Files:**
- Move: `commands/clean.rs` → `commands/admin/drop.rs`
- Move: `commands/init.rs` → `commands/admin/install_hook.rs`
- Move: `commands/analyze.rs` → `commands/admin/index.rs`
- Move: `commands/prune.rs` → `commands/admin/prune.rs`
- Move: `commands/rename_branch.rs` → `commands/admin/rename_branch.rs`
- Move: `commands/config.rs` → `commands/admin/config.rs`
- Delete: `commands/index.rs` (the recovery `register`-style one)
- Delete: `commands/remove.rs` (folded into `drop`)
- Delete: `commands/analyze_here.rs` (replaced by auto-ensure)
- Modify: `commands/admin/mod.rs` (rewire to new locations)
- Modify: `main.rs` (no top-level wire-up needed for admin children)

- [ ] **Step 1: Move files**

```bash
git mv crates/graph-nexus-cli/src/commands/clean.rs        crates/graph-nexus-cli/src/commands/admin/drop.rs
git mv crates/graph-nexus-cli/src/commands/init.rs         crates/graph-nexus-cli/src/commands/admin/install_hook.rs
git mv crates/graph-nexus-cli/src/commands/analyze.rs      crates/graph-nexus-cli/src/commands/admin/index.rs
git mv crates/graph-nexus-cli/src/commands/prune.rs        crates/graph-nexus-cli/src/commands/admin/prune.rs
git mv crates/graph-nexus-cli/src/commands/rename_branch.rs crates/graph-nexus-cli/src/commands/admin/rename_branch.rs
git mv crates/graph-nexus-cli/src/commands/config.rs       crates/graph-nexus-cli/src/commands/admin/config.rs
git rm crates/graph-nexus-cli/src/commands/index.rs
git rm crates/graph-nexus-cli/src/commands/remove.rs
git rm crates/graph-nexus-cli/src/commands/analyze_here.rs
```

- [ ] **Step 2: Rename struct types** (where the new file name differs):

In `admin/drop.rs`: `CleanArgs` → `DropArgs`.
In `admin/install_hook.rs`: `InitArgs` → `InstallHookArgs`.
In `admin/index.rs`: `AnalyzeArgs` → `IndexArgs`.

Run `gnx rename` (using current build) per name (in dry-run first):

```bash
./target/release/gnx rename CleanArgs DropArgs --dry-run
./target/release/gnx rename CleanArgs DropArgs
# similarly for InitArgs→InstallHookArgs, AnalyzeArgs→IndexArgs
```

Or use `sd` / `sed` per file if `gnx rename` isn't ready.

- [ ] **Step 3: Update `admin/mod.rs`**

Replace the stub from Task 1.1 with:

```rust
use clap::Subcommand;

pub mod drop;
pub mod install_hook;
pub mod index;
pub mod prune;
pub mod rename_branch;
pub mod config;
pub mod group;

#[derive(Subcommand, Debug)]
pub enum AdminCommands {
    /// Install git ref-transaction hook
    InstallHook(install_hook::InstallHookArgs),
    /// Delete a repo's index data + registry entry
    Drop(drop::DropArgs),
    /// Remove orphan index dirs not in registry
    Prune(prune::PruneArgs),
    /// Rename a branch's index dir
    RenameBranch(rename_branch::RenameBranchArgs),
    /// Interactive TOML config editor
    Config(config::ConfigArgs),
    /// Manage repo group membership
    Group {
        #[command(subcommand)]
        command: group::GroupCommands,
    },
    /// Build / refresh the graph (explicit / bulk / embeddings)
    Index(index::IndexArgs),
}

pub fn run(cmd: AdminCommands) -> Result<(), graph_nexus_core::GnxError> {
    match cmd {
        AdminCommands::InstallHook(args) => install_hook::run(args),
        AdminCommands::Drop(args) => drop::run(args),
        AdminCommands::Prune(args) => prune::run(args),
        AdminCommands::RenameBranch(args) => rename_branch::run(args),
        AdminCommands::Config(args) => config::run(args),
        AdminCommands::Group { command } => group::run(command),
        AdminCommands::Index(args) => index::run(args),
    }
}
```

- [ ] **Step 4: Merge old `clean` + `remove` semantics into `drop`**

In `admin/drop.rs`, ensure `run()` removes BOTH the index dir AND the registry entry (the old `clean` only did data; `remove` only did registry). Now `drop` does both. Update the doc-comment to reflect.

The implementation: open `commands/clean.rs::run` + `commands/remove.rs::run` (now deleted), combine — wipe `index_dir_root` (data) then mutate `registry.json` to drop the entry.

- [ ] **Step 5: Update `commands/mod.rs`**

```diff
-pub mod analyze;
-pub mod analyze_here;
-pub mod api_impact;
-pub mod clean;
-pub mod cluster;
-pub mod config;
-pub mod context;
-pub mod cypher;
-pub mod detect_changes;
-pub mod doctor;
-pub mod format;
-pub mod hook_handle;
-pub mod hook_watcher;
-pub mod impact;
-pub mod index;
-pub mod init;
-pub mod list;
-pub mod process;
-pub mod prune;
-pub mod query;
-pub mod remove;
-pub mod rename;
-pub mod rename_branch;
-pub mod route_map;
-pub mod status;
-pub mod summarize;
-pub mod tool_map;
-pub mod verify_resolver;
+pub mod admin;
+pub mod contracts;
+pub mod coverage;
+pub mod cypher;
+pub mod format;
+pub mod hook_handle;
+pub mod hook_watcher;
+pub mod impact;
+pub mod inspect;
+pub mod rename;
+pub mod routes;
+pub mod scan;
+pub mod search;
+pub mod verify_resolver;
```

(Keep `format` since it has `kind_to_str` / `rel_to_str` helpers used everywhere.)

- [ ] **Step 6: Build + verify**

```bash
cargo build --bin gnx 2>&1 | tail -3
cargo test -p graph-nexus-cli --test cli_surface 2>&1 | tail -10
```

Expected: build OK; surface tests still PASS.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(cli): move admin verbs into commands/admin/ namespace

clean+remove→drop (combined data+registry).
init→install-hook. analyze→index.
Drops register (recovery via index), analyze-here (auto-ensure)."
```

---

### Task 4.2: Implement `admin group add/remove`

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/admin/group.rs`
- Create: `crates/graph-nexus-cli/tests/admin_group_cmd.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/graph-nexus-cli/tests/admin_group_cmd.rs`:

```rust
#[test]
fn group_add_creates_group_if_missing() {
    let registry = setup_registry_with_repo("alpha");
    run_admin_group_add("alpha", "newgroup");
    let r = load_registry();
    assert_eq!(r.groups.iter().find(|g| g.name == "newgroup").unwrap().members, vec!["alpha"]);
    let repo = r.repos.iter().find(|r| r.name == "alpha").unwrap();
    assert!(repo.groups.contains(&"newgroup".to_string()));
}

#[test]
fn group_add_idempotent() {
    setup_registry_with_repo_in_group("alpha", "backend");
    run_admin_group_add("alpha", "backend"); // already there
    let r = load_registry();
    let repo = r.repos.iter().find(|r| r.name == "alpha").unwrap();
    assert_eq!(repo.groups.iter().filter(|g| g == &"backend").count(), 1);
}

#[test]
fn group_remove_auto_deletes_empty_group() {
    setup_registry_with_repo_in_group("alpha", "solo");
    run_admin_group_remove("alpha", "solo");
    let r = load_registry();
    assert!(r.groups.iter().find(|g| g.name == "solo").is_none());
}

#[test]
fn group_remove_preserves_non_empty_group() {
    setup_registry_with_repos_in_group(&["alpha", "beta"], "backend");
    run_admin_group_remove("alpha", "backend");
    let r = load_registry();
    let group = r.groups.iter().find(|g| g.name == "backend").unwrap();
    assert_eq!(group.members, vec!["beta"]);
}
```

- [ ] **Step 2: Verify fail**

- [ ] **Step 3: Implement group.rs**

Replace stub:

```rust
use clap::Subcommand;
use graph_nexus_core::GnxError;

#[derive(Subcommand, Debug)]
pub enum GroupCommands {
    /// Add a repo to a group (auto-creates group)
    Add { repo: String, group: String },
    /// Remove a repo from a group (auto-deletes empty group)
    Remove { repo: String, group: String },
}

pub fn run(cmd: GroupCommands) -> Result<(), GnxError> {
    match cmd {
        GroupCommands::Add { repo, group } => add(&repo, &group),
        GroupCommands::Remove { repo, group } => remove(&repo, &group),
    }
}

fn add(repo: &str, group: &str) -> Result<(), GnxError> {
    let path = graph_nexus_core::registry::default_path();
    let mut reg = graph_nexus_core::registry::RegistryFile::read_or_empty(&path)
        .map_err(|e| GnxError::Io(format!("registry read: {e}")))?;

    // Update repo's groups list
    let r = reg.repos.iter_mut()
        .find(|r| r.name == repo)
        .ok_or_else(|| GnxError::Output(format!("repo not found: {repo}")))?;
    if !r.groups.iter().any(|g| g == group) {
        r.groups.push(group.to_string());
    }

    // Update group's members list (auto-create)
    if let Some(g) = reg.groups.iter_mut().find(|g| g.name == group) {
        if !g.members.iter().any(|m| m == repo) {
            g.members.push(repo.to_string());
        }
    } else {
        reg.groups.push(graph_nexus_core::registry::GroupEntry {
            name: group.to_string(),
            members: vec![repo.to_string()],
        });
    }

    graph_nexus_core::registry::RegistryFile::write_atomic(&path, &reg)
        .map_err(|e| GnxError::Io(format!("registry write: {e}")))?;

    println!("✓ Added {repo} to {group}");
    Ok(())
}

fn remove(repo: &str, group: &str) -> Result<(), GnxError> {
    let path = graph_nexus_core::registry::default_path();
    let mut reg = graph_nexus_core::registry::RegistryFile::read_or_empty(&path)
        .map_err(|e| GnxError::Io(format!("registry read: {e}")))?;

    // Strip from repo's groups
    if let Some(r) = reg.repos.iter_mut().find(|r| r.name == repo) {
        r.groups.retain(|g| g != group);
    }

    // Strip from group's members; auto-delete if empty
    if let Some(pos) = reg.groups.iter().position(|g| g.name == group) {
        reg.groups[pos].members.retain(|m| m != repo);
        if reg.groups[pos].members.is_empty() {
            reg.groups.remove(pos);
        }
    }

    graph_nexus_core::registry::RegistryFile::write_atomic(&path, &reg)
        .map_err(|e| GnxError::Io(format!("registry write: {e}")))?;

    println!("✓ Removed {repo} from {group}");
    Ok(())
}
```

- [ ] **Step 4: Build + tests**

```bash
cargo build --bin gnx 2>&1 | tail -3
cargo test -p graph-nexus-cli --test admin_group_cmd 2>&1 | tail -15
```

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/admin/group.rs \
        crates/graph-nexus-cli/tests/admin_group_cmd.rs
git commit -m "feat(admin): group add/remove with auto-create + auto-delete-empty"
```

---

## Phase 5 — Drop Dead Code

### Task 5.1: Remove unused commands

**Files:**
- Delete: `commands/cluster.rs` + tests
- Delete: `commands/process.rs` + tests
- Delete: any remaining residue from earlier deletions

- [ ] **Step 1: Drop cluster + process**

```bash
git rm crates/graph-nexus-cli/src/commands/cluster.rs \
       crates/graph-nexus-cli/src/commands/process.rs
git rm crates/graph-nexus-cli/tests/cluster_cmd.rs 2>/dev/null || true
git rm crates/graph-nexus-cli/tests/process_cmd.rs 2>/dev/null || true
```

Remove from `commands/mod.rs` if any lingering references.

- [ ] **Step 2: Build, check for unused imports**

```bash
cargo build --bin gnx 2>&1 | grep -E "warning: unused|error" | head -20
```

If unused-import warnings appear, clean them up surgically in the relevant files.

- [ ] **Step 3: Run full test suite**

```bash
cargo test --workspace 2>&1 | tail -30
```

Expected: PASS across the workspace. Any new failures must trace to the redesign and be fixed before commit.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor(cli): drop cluster + process (niche; cypher covers)"
```

---

## Phase 6 — Documentation

### Task 6.1: Update README.md

**Files:**
- Modify: `README.md`
- Modify: `README_zh-TW.md`

- [ ] **Step 1: List current references to old commands**

```bash
grep -nE "gnx (analyze|clean|init|list|index|config|remove|register|context|query|doctor|status|summarize|tool_map|tool-map|route_map|route-map|api_impact|api-impact|detect_changes|detect-changes|cluster|process|multi_query|multi-query|analyze-here|analyze_here)" README.md
```

- [ ] **Step 2: Replace each reference**

Using the mapping:

| Old | New |
|---|---|
| `gnx analyze` | `gnx admin index` |
| `gnx analyze --repo .` | `gnx admin index .` |
| `gnx analyze-here` | (drop — auto-ensure) |
| `gnx clean` | `gnx admin drop` |
| `gnx remove` | `gnx admin drop` (combined) |
| `gnx init` | `gnx admin install-hook` |
| `gnx list` | `gnx coverage` |
| `gnx index` (recovery) | (drop — re-run `gnx admin index`) |
| `gnx config` | `gnx admin config` |
| `gnx prune` | `gnx admin prune` |
| `gnx rename-branch` | `gnx admin rename-branch` |
| `gnx verify-resolver` | (hidden — internal) |
| `gnx context` | `gnx inspect` |
| `gnx query` | `gnx search` |
| `gnx doctor` | `gnx coverage` |
| `gnx status` | `gnx coverage` |
| `gnx summarize` | `gnx coverage` |
| `gnx tool_map` / `gnx tool-map` | `gnx coverage --detailed` (externals section) |
| `gnx route_map` / `gnx route-map` | `gnx routes` |
| `gnx api_impact /path` / `gnx api-impact /path` | `gnx routes /path` |
| `gnx detect_changes` / `gnx detect-changes` | `gnx impact --since HEAD~1` |
| `gnx multi_query` / `gnx multi-query` | `gnx find --batch` (single repo, BM25; reads patterns from stdin) or `gnx group find --batch` (multi-repo). Batch is a flag on the existing verb, not a separate CLI. |
| `gnx cluster` / `gnx process` | `gnx cypher` |

- [ ] **Step 3: Add a "What's New" section near the top**

Add under the project intro:

```markdown
## What changed in this redesign (2026-05)

The CLI surface has been refocused on the LLM agent workflow. **9 agent
commands** (down from 25), **7 admin commands** under `gnx admin`, plus
a global `--repo` selector supporting `@group` / `@all` / ad-hoc CSV.
Cross-repo search merged into `gnx search`; `gnx multi_query` removed.
See `docs/specs/2026-05-15-gnx-cli-redesign-design.md` for the full
spec and rationale.
```

- [ ] **Step 4: Apply same updates to README_zh-TW.md**

Mirror all changes. The zh-TW file has parallel structure.

- [ ] **Step 5: Build verify (docs don't affect build, but tests do)**

```bash
cargo test --workspace 2>&1 | tail -5
```

- [ ] **Step 6: Commit**

```bash
git add README.md README_zh-TW.md
git commit -m "docs(readme): rewrite for redesigned 9-agent + 7-admin CLI surface"
```

---

### Task 6.2: Update CLAUDE.md guidance (workspace-level)

**Files:**
- Search for `gnx` references in `.claude/`, `docs/`, and any in-repo `CLAUDE.md`

- [ ] **Step 1: Locate refs**

```bash
find . -name "CLAUDE.md" -o -name "*.md" 2>/dev/null | xargs grep -l "gnx context\|gnx query\|gnx analyze\|gnx doctor\|gnx status\|gnx clean" 2>/dev/null
```

- [ ] **Step 2: Apply the same mapping as Task 6.1 to each file**

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "docs: align in-repo guidance with redesigned CLI surface"
```

---

### Task 6.3: Update parity / benchmark scripts

**Files:**
- Modify: `scripts/parity/` shell scripts
- Modify: `scripts/benchmark_gnx.py`

- [ ] **Step 1: Find refs**

```bash
grep -rn "gnx analyze\|gnx context\|gnx query\|gnx doctor\|gnx clean\|gnx route_map\|gnx api_impact\|gnx multi_query\|gnx detect_changes" scripts/
```

- [ ] **Step 2: Apply mapping, run smoke tests**

```bash
# Per script: replace, run, observe.
# E.g. for benchmark_gnx.py:
python scripts/benchmark_gnx.py --help
```

- [ ] **Step 3: Commit**

```bash
git add scripts/
git commit -m "chore(scripts): align parity + benchmark with redesigned CLI"
```

---

## Final Pass

### Task F.1: Full workspace test run

- [ ] **Step 1: Run all tests**

```bash
cargo test --workspace --release 2>&1 | tail -40
```

Expected: 0 failures across workspace.

- [ ] **Step 2: Run CLI surface smoke**

```bash
./target/release/gnx --help | head -20
./target/release/gnx admin --help | head -10
./target/release/gnx inspect --help | head -10
```

Expected:
- top-level shows 9 agent commands, no `admin` mention
- `admin --help` shows 7 entries
- `inspect --help` shows args + `--repo` global

- [ ] **Step 3: Run clippy + rustfmt for clean delta**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
cargo fmt --check 2>&1 | tail -5
```

If clippy or fmt fail: fix issues in respective files (use targeted `--fix` for clippy if safe), commit:

```bash
git add -u
git commit -m "chore: clippy + fmt drift from CLI redesign"
```

- [ ] **Step 4: Final commit marker**

```bash
git commit --allow-empty -m "chore(cli-redesign): redesign complete

9 agent + 7 admin commands. Multi-group registry. Auto-ensure index.
Server-side composition. Hybrid search. Rename verification.
See docs/specs/2026-05-15-gnx-cli-redesign-design.md."
```

---

## Self-Review Checklist (run before declaring done)

- [ ] All 9 agent commands present in `gnx --help`: inspect, search, impact, rename, cypher, coverage, routes, scan, contracts
- [ ] `gnx --help` does NOT mention `admin`
- [ ] `gnx admin --help` lists: install-hook, drop, prune, rename-branch, config, group, index (7 entries)
- [ ] `gnx admin group --help` lists: add, remove
- [ ] Hidden commands runnable: `gnx hook-handle`, `gnx hook-watcher`, `gnx verify-resolver` (no `--help` mention)
- [ ] `--repo` selector accepts: path / name / `@group` / `@all` / CSV mix
- [ ] Multi-group schema: `RepoEntry.groups: Vec<String>`, old `group:` field auto-migrates
- [ ] `inspect` ambiguity returns rich matches (no UID candidate list)
- [ ] `search` auto-detects mode; falls back to bm25 when no embeddings
- [ ] `impact --since <ref>` produces diff-impact equivalent to old `detect_changes`
- [ ] `rename` default = code only; `--markdown`/`--comment`/`--reference`/`--all` opt-in
- [ ] `rename` output includes residual check + new-name distribution + collision detection
- [ ] `scan` returns fuzzy suggestions for unresolved refs
- [ ] `contracts` errors when fewer than 2 repos resolved
- [ ] Empty result of any command includes explicit reason + next-step hint
- [ ] Errors include 3-line `✗ what / cause / next` format
- [ ] Stale index emits `⚠ Index for "X" is stale (...)` to stderr
- [ ] No `--format` flag in agent help
- [ ] README + CLAUDE.md + parity scripts updated
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` 0 failures

---

*End of plan.*
