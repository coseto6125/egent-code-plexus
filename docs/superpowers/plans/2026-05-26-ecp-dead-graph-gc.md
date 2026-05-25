# `~/.ecp` Dead-Graph GC Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop `~/.ecp` graph-cache leak (16G observed, 13G zombie) by converging stale same-SHA generation dirs (L2, main cause), sweeping retired repo dirs (L1), wiring the existing `admin gc` subcommand (L3), and recording retire-thread failures instead of swallowing them.

**Architecture:** Two new pure-fn sweeps in the already-tested `gc.rs` module (`sweep_stale_generations`, `sweep_retired_repos`), wired behind a new `ecp admin gc` subcommand that runs in `session_start`'s existing detached+flock background prune job (prune → gc, single lock, serial low-priority deletion). L2 retention reuses `registry::CommitDirName::parse` + `Generation` Ord instead of hand-rolled string/mtime logic.

**Tech Stack:** Rust, `registry::CommitDirName`/`Generation`, `rustc_hash::FxHashMap`, `walkdir`, clap subcommand, `cargo test -p egent-code-plexus`.

**Note on crate naming (verified):** the CLI package is `egent-code-plexus` (dir `crates/ecp-cli`, bin `ecp`, lib `ecp_cli`). Build with `cargo build -p egent-code-plexus`; test with `cargo test -p egent-code-plexus`. The index/rebuild command is `ecp admin index` (NOT `ecp index`). Do NOT run `cargo build --workspace` or any `ecp cypher --repo .sample_repo` / `ecp admin index` against shared corpora — those trigger heavy ingest and caused a host freeze; all verification here is unit-test only.

---

## File Structure

| File | Responsibility |
|------|----------------|
| `crates/ecp-cli/src/admin/gc.rs` | Add `sweep_stale_generations` + `sweep_retired_repos`; remove `#![allow(dead_code)]` once wired |
| `crates/ecp-cli/src/commands/admin/gc.rs` (NEW) | `GcArgs` + `run` — the `ecp admin gc` subcommand entry, calls the gc.rs sweeps |
| `crates/ecp-cli/src/commands/admin/mod.rs` | Register `Gc(gc::GcArgs)` variant + dispatch arm |
| `crates/ecp-core/src/registry/fs_safe.rs` | `retire_dir_async` detached thread: log failure instead of `let _ =` |
| `crates/ecp-cli/src/commands/hook/session_start.rs` | Background job runs `prune` then `gc` under one flock |
| `crates/ecp-cli/tests/gc.rs` | New tests for the two sweeps |

**Disambiguation:** there are TWO `gc.rs` files — the **logic** module `crates/ecp-cli/src/admin/gc.rs` (where `sweep_sessions`/`reachability`/`enforce_quota` live) and the NEW **subcommand** file `crates/ecp-cli/src/commands/admin/gc.rs` (clap entry). Throughout this plan, "gc.rs logic" = the former, "gc subcommand" = the latter.

---

## Task 1: `sweep_stale_generations` — converge same-SHA generations (L2, main cause)

**Files:**
- Modify: `crates/ecp-cli/src/admin/gc.rs`
- Test: `crates/ecp-cli/tests/gc.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/ecp-cli/tests/gc.rs`. First read the top of the file for existing imports (`use` lines) and the `make_commit_dir` helper signature; reuse them. Add a helper that creates a generation-suffixed commit dir, then the test:

```rust
/// Create a commit dir with an explicit on-disk name (supports `.gen.<...>` suffixes).
fn make_named_commit_dir(commits: &std::path::Path, dir_name: &str) {
    let dir = commits.join(dir_name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("graph.bin"), vec![0u8; 16]).unwrap();
}

#[test]
fn sweep_stale_generations_keeps_newest_per_sha() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha_a = "a".repeat(40);
    let sha_b = "b".repeat(40);
    // Same SHA, three generations — only the highest Generation must survive.
    make_named_commit_dir(&commits, &format!("branch_main__{sha_a}.gen.1000.10.0"));
    make_named_commit_dir(&commits, &format!("branch_main__{sha_a}.gen.2000.20.0"));
    make_named_commit_dir(&commits, &format!("branch_main__{sha_a}.gen.3000.30.0"));
    // Different SHA — must be untouched.
    make_named_commit_dir(&commits, &format!("branch_main__{sha_b}.gen.1500.15.0"));

    let stats = ecp_cli::admin::gc::sweep_stale_generations(tmp.path()).unwrap();

    assert_eq!(stats.removed, 2, "two older same-SHA generations removed");
    // Highest generation (3000) for sha_a survives.
    assert!(commits.join(format!("branch_main__{sha_a}.gen.3000.30.0")).exists());
    assert!(!commits.join(format!("branch_main__{sha_a}.gen.1000.10.0")).exists());
    assert!(!commits.join(format!("branch_main__{sha_a}.gen.2000.20.0")).exists());
    // Different SHA untouched.
    assert!(commits.join(format!("branch_main__{sha_b}.gen.1500.15.0")).exists());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p egent-code-plexus --test gc sweep_stale_generations_keeps_newest_per_sha 2>&1 | tail -10`
Expected: FAIL — `sweep_stale_generations` not found in `ecp_cli::admin::gc`.

- [ ] **Step 3: Implement `sweep_stale_generations`**

In `crates/ecp-cli/src/admin/gc.rs`, add the function. Check the existing imports at the top of the file first; add `use ecp_core::registry::CommitDirName;` and `use rustc_hash::FxHashMap;` if not already present (`FxHashSet` is already imported — add `FxHashMap` to the same line or a new `use`).

```rust
/// Converge same-SHA generation dirs under `<repo_root>/commits/`: for each SHA,
/// keep only the dir with the greatest `Generation` (a base dir with no `.gen`
/// suffix has `generation == None`, which orders below any `Some(_)`), remove
/// the rest. Same SHA → identical graph (ingest is idempotent), so older
/// generations are pure waste. Skips dirs whose mtime is < 10s old or that have
/// a sibling `.building/` marker for the same commit (another session may be
/// mid-ingest). Reuses `CommitDirName::parse` rather than hand-rolling the name
/// grammar.
pub fn sweep_stale_generations(repo_root: &Path) -> io::Result<SweepStats> {
    let commits = repo_root.join("commits");
    let mut removed = 0usize;
    let Ok(it) = fs::read_dir(&commits) else {
        return Ok(SweepStats { marked: 0, removed });
    };

    // Group dirs by SHA: sha -> Vec<(parsed, path)>.
    let mut by_sha: FxHashMap<[u8; 20], Vec<(CommitDirName, std::path::PathBuf)>> =
        FxHashMap::default();
    let now = std::time::SystemTime::now();
    for entry in it.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // A `.building/` dir or any dotfile is never a candidate.
        if name.starts_with('.') || name.contains(".building") {
            continue;
        }
        let Ok(parsed) = CommitDirName::parse(&name) else {
            continue;
        };
        // Skip freshly-written dirs (< 10s) — another session may be ingesting.
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                if now.duration_since(modified).map(|d| d.as_secs() < 10).unwrap_or(false) {
                    continue;
                }
            }
        }
        by_sha.entry(parsed.sha).or_default().push((parsed, path));
    }

    for (_sha, mut group) in by_sha {
        if group.len() < 2 {
            continue;
        }
        // Keep the dir with the greatest Generation; remove the rest.
        group.sort_by(|a, b| a.0.generation.cmp(&b.0.generation));
        let keep_idx = group.len() - 1;
        for (i, (_, path)) in group.iter().enumerate() {
            if i == keep_idx {
                continue;
            }
            // A sibling `.building/` for this exact commit name means an active
            // build — never delete under it.
            let building = path.with_extension("building");
            if building.exists() {
                continue;
            }
            match fs::remove_dir_all(path) {
                Ok(()) => removed += 1,
                Err(e) => eprintln!("gc: failed to remove stale generation {}: {e}", path.display()),
            }
        }
    }

    Ok(SweepStats { marked: 0, removed })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p egent-code-plexus --test gc sweep_stale_generations_keeps_newest_per_sha 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/src/admin/gc.rs crates/ecp-cli/tests/gc.rs
git commit -m "feat(gc): sweep_stale_generations converges same-SHA gen dirs (FU-2026-05-26-001)"
```

---

## Task 2: `sweep_stale_generations` edge cases — skip building + fresh

**Files:**
- Test: `crates/ecp-cli/tests/gc.rs`

The implementation from Task 1 already handles these; this task pins the behavior with tests.

- [ ] **Step 1: Write the failing/characterization tests**

Append to `crates/ecp-cli/tests/gc.rs`:

```rust
#[test]
fn sweep_stale_generations_skips_building() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "c".repeat(40);
    make_named_commit_dir(&commits, &format!("branch_main__{sha}.gen.1000.10.0"));
    let older = format!("branch_main__{sha}.gen.500.5.0");
    make_named_commit_dir(&commits, &older);
    // A `.building` sibling for the OLDER dir blocks its removal.
    std::fs::create_dir_all(commits.join(format!("{older}.building")).parent().unwrap()).ok();
    let building = commits.join(&older).with_extension("building");
    std::fs::create_dir_all(&building).unwrap();

    let stats = ecp_cli::admin::gc::sweep_stale_generations(tmp.path()).unwrap();
    assert_eq!(stats.removed, 0, "older dir guarded by .building sibling must stay");
    assert!(commits.join(&older).exists());
}

#[test]
fn sweep_stale_generations_skips_fresh() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "d".repeat(40);
    // Two same-SHA gens, both just created (< 10s) → fresh guard skips them.
    make_named_commit_dir(&commits, &format!("branch_main__{sha}.gen.1000.10.0"));
    make_named_commit_dir(&commits, &format!("branch_main__{sha}.gen.2000.20.0"));

    let stats = ecp_cli::admin::gc::sweep_stale_generations(tmp.path()).unwrap();
    assert_eq!(stats.removed, 0, "freshly-written dirs (<10s) are skipped");
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p egent-code-plexus --test gc sweep_stale_generations_skips 2>&1 | tail -12`
Expected: both PASS (Task 1 impl already covers them). If `skips_building` fails because the `.building` path construction differs from the impl's `path.with_extension("building")`, align the test's `building` path to exactly `commits.join(&older).with_extension("building")` (it already does) — the impl and test must use the identical expression.

- [ ] **Step 3: Commit**

```bash
git add crates/ecp-cli/tests/gc.rs
git commit -m "test(gc): sweep_stale_generations skips .building + fresh (<10s) dirs"
```

---

## Task 3: `sweep_retired_repos` — sweep top-level `<repo>.dead.*` (L1)

**Files:**
- Modify: `crates/ecp-cli/src/admin/gc.rs`
- Test: `crates/ecp-cli/tests/gc.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/ecp-cli/tests/gc.rs`:

```rust
#[test]
fn sweep_retired_repos_removes_dead() {
    let tmp = tempfile::tempdir().unwrap();
    let home_ecp = tmp.path();
    // A live repo dir + two retired (.dead.*) sibling dirs.
    std::fs::create_dir_all(home_ecp.join("myrepo__abc123")).unwrap();
    std::fs::create_dir_all(home_ecp.join("myrepo__abc123.dead.111.0.1700000000000")).unwrap();
    std::fs::create_dir_all(home_ecp.join("other__def456.dead.222.1.1700000000001")).unwrap();

    let stats = ecp_cli::admin::gc::sweep_retired_repos(home_ecp).unwrap();

    assert_eq!(stats.removed, 2, "both .dead.* repo dirs removed");
    assert!(home_ecp.join("myrepo__abc123").exists(), "live repo untouched");
    assert!(!home_ecp.join("myrepo__abc123.dead.111.0.1700000000000").exists());
    assert!(!home_ecp.join("other__def456.dead.222.1.1700000000001").exists());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p egent-code-plexus --test gc sweep_retired_repos_removes_dead 2>&1 | tail -10`
Expected: FAIL — `sweep_retired_repos` not found.

- [ ] **Step 3: Implement `sweep_retired_repos`**

In `crates/ecp-cli/src/admin/gc.rs`, add:

```rust
/// Remove top-level retired repo dirs (`<repo>__<hash>.dead.<pid>.<n>.<ts>`)
/// left behind when `fs_safe::retire_dir_async`'s background delete thread died
/// with the process before finishing. These are already marked dead, so removal
/// is unconditional. The `.dead.` infix with a trailing all-digit timestamp
/// segment is the marker (mirrors `sweep_sessions`' dead-detection).
pub fn sweep_retired_repos(home_ecp: &Path) -> io::Result<SweepStats> {
    let mut removed = 0usize;
    let Ok(it) = fs::read_dir(home_ecp) else {
        return Ok(SweepStats { marked: 0, removed });
    };
    for entry in it.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dead = name.ends_with(".dead")
            || name
                .rsplit_once(".dead.")
                .map(|(_, rest)| {
                    // rest is `<pid>.<n>.<ts>` — require it to be dot-separated digits.
                    rest.split('.').all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()))
                })
                .unwrap_or(false);
        if !is_dead {
            continue;
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(e) => eprintln!("gc: failed to remove retired repo {}: {e}", path.display()),
        }
    }
    Ok(SweepStats { marked: 0, removed })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p egent-code-plexus --test gc sweep_retired_repos_removes_dead 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/src/admin/gc.rs crates/ecp-cli/tests/gc.rs
git commit -m "feat(gc): sweep_retired_repos removes top-level .dead.* repo dirs"
```

---

## Task 4: Record retire-thread failures (fix L1 leak source)

**Files:**
- Modify: `crates/ecp-core/src/registry/fs_safe.rs:103-105`

- [ ] **Step 1: Replace the silent `let _ =` with failure logging**

In `crates/ecp-core/src/registry/fs_safe.rs`, the `retire_dir_async` body currently has:

```rust
        std::thread::spawn(move || {
            let _ = fs::remove_dir_all(retired_path);
        });
```

Replace the closure body with:

```rust
        std::thread::spawn(move || {
            // WHY log, not swallow: a short-lived CLI process can exit before
            // this detached thread finishes, leaving a `.dead.*` dir behind.
            // `admin gc` sweeps such leftovers; recording the stderr here makes
            // the leak diagnosable instead of silent (FU-2026-05-26-001).
            if let Err(e) = fs::remove_dir_all(&retired_path) {
                eprintln!("retire_dir_async: background remove of {} failed: {e}", retired_path.display());
            }
        });
```

Note: `retired_path` is currently `move`d into the closure. After this change it is borrowed by `Display` then dropped at closure end — still fine since `remove_dir_all` takes `&Path`. If the borrow checker complains about `retired_path` move vs `&retired_path`, change `fs::remove_dir_all(&retired_path)` is already a borrow; the `.display()` borrow is after. No clone needed.

- [ ] **Step 2: Build to verify**

Run: `cargo build -p ecp-core 2>&1 | tail -5`
Expected: compiles clean.

- [ ] **Step 3: Commit**

```bash
git add crates/ecp-core/src/registry/fs_safe.rs
git commit -m "fix(registry): log retire_dir_async background-delete failures (FU-2026-05-26-001)"
```

---

## Task 5: Wire the `ecp admin gc` subcommand

**Files:**
- Create: `crates/ecp-cli/src/commands/admin/gc.rs`
- Modify: `crates/ecp-cli/src/commands/admin/mod.rs` (enum ~37, dispatch ~69)
- Modify: `crates/ecp-cli/src/admin/gc.rs` (remove `#![allow(dead_code)]` at line 6)

- [ ] **Step 1: Create the subcommand file**

Create `crates/ecp-cli/src/commands/admin/gc.rs`. Model it on `crates/ecp-cli/src/commands/admin/prune.rs` (read it for the `EcpError`/`resolve_home_ecp` conventions):

```rust
use clap::Args;

/// `ecp admin gc` — converge stale graph generations + sweep retired repo/session
/// dirs across all repos under `~/.ecp`. Idempotent; safe to run repeatedly.
#[derive(Args, Debug, Clone)]
pub struct GcArgs {
    /// Print what would be removed without deleting.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: GcArgs) -> Result<(), ecp_core::EcpError> {
    let home_ecp = ecp_core::registry::resolve_home_ecp();
    let mut total_removed = 0usize;

    // L1: top-level retired repo dirs.
    if !args.dry_run {
        match crate::admin::gc::sweep_retired_repos(&home_ecp) {
            Ok(s) => total_removed += s.removed,
            Err(e) => eprintln!("gc: sweep_retired_repos: {e}"),
        }
    }

    // L2 + L3: per-repo generation convergence + session sweep.
    if let Ok(it) = std::fs::read_dir(&home_ecp) {
        for entry in it.flatten() {
            let repo_root = entry.path();
            if !repo_root.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip retired/marker dirs and non-repo entries.
            if name.starts_with('.') || name.contains(".dead") || name == "telemetry" {
                continue;
            }
            if args.dry_run {
                continue;
            }
            if let Ok(s) = crate::admin::gc::sweep_stale_generations(&repo_root) {
                total_removed += s.removed;
            }
            if let Ok(s) = crate::admin::gc::sweep_sessions(&repo_root) {
                total_removed += s.removed;
            }
        }
    }

    println!("gc: removed {total_removed} stale/retired dirs");
    Ok(())
}
```

- [ ] **Step 2: Register the module + enum variant + dispatch in `mod.rs`**

In `crates/ecp-cli/src/commands/admin/mod.rs`:
- Add `pub mod gc;` near the other `pub mod` lines (after `pub mod prune;`).
- Add the enum variant (after the `Prune(...)` variant, ~line 29):
```rust
    /// Garbage-collect stale graph generations + retired repo/session dirs
    Gc(gc::GcArgs),
```
- Add the dispatch arm (after `AdminCommands::Prune(args) => prune::run(args),`, ~line 69):
```rust
        AdminCommands::Gc(args) => gc::run(args),
```

- [ ] **Step 3: Remove the `dead_code` allow on the logic module**

In `crates/ecp-cli/src/admin/gc.rs`, delete line 6 `#![allow(dead_code)]` — the module now has callers via the subcommand. (If clippy then flags a still-unused function like `reachability`/`enforce_quota` that the subcommand doesn't call yet, KEEP a narrowed `#[allow(dead_code)]` on those specific items only, with a `// wired in a later phase` comment — do not blanket-allow the module.)

- [ ] **Step 4: Build + run the gc subcommand smoke test**

Run: `cargo build -p egent-code-plexus 2>&1 | tail -8`
Expected: compiles clean.
Run: `target/debug/ecp admin gc --dry-run 2>&1 | tail -3`
Expected: prints `gc: removed 0 stale/retired dirs` (dry-run deletes nothing). This is safe — dry-run does no fs writes.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/src/commands/admin/gc.rs crates/ecp-cli/src/commands/admin/mod.rs crates/ecp-cli/src/admin/gc.rs
git commit -m "feat(cli): wire ecp admin gc subcommand (stale-gen + retired sweep)"
```

---

## Task 6: Run gc after prune in the session_start background job

**Files:**
- Modify: `crates/ecp-cli/src/commands/hook/session_start.rs` (`spawn_orphan_prune` ~188-211)

The existing job runs `ecp admin prune --orphans` under flock on `.prune.lock`. We need gc to run in the SAME job under the SAME lock (sequential), not a competing spawn.

- [ ] **Step 1: Read the current `spawn_orphan_prune` + `spawn_bg`/`BgJob` shape**

Read `crates/ecp-cli/src/commands/hook/session_start.rs:188-211` and `crates/ecp-cli/src/background.rs` `BgJob` struct + the marker-script branch (the `while [ $ATTEMPT -lt $MAX ]` loop runs `{ecp} {args}`). Determine whether `BgJob.args` can express two sequential commands. It runs a single `{ecp} {args}` invocation per attempt, so two subcommands need either (a) a new `BgJob` field for a follow-up command, or (b) a dedicated composite. Choose the MINIMAL change: prefer adding the gc call as a second statement in the generated script for THIS job only.

- [ ] **Step 2: Implement — run gc after a successful prune, same lock**

Modify `spawn_orphan_prune` so the background script runs prune then gc under the one flock. The simplest correct form: change the job's command to a small shell sequence. If `BgJob.args` is `&[&str]` joined into one `ecp …` call, add an optional `then_args` to `BgJob` (in `background.rs`) that, when present, is emitted as a second `{ecp} {then_args}` line after the primary command succeeds, before writing `.prune-complete`. Then in `spawn_orphan_prune`:

```rust
    let _ = spawn_bg(BgJob {
        args: &["admin", "prune", "--orphans"],
        then_args: Some(&["admin", "gc"]),   // runs under the SAME flock, after prune
        lock: &lock,
        cwd: &home_ecp,
        retry: (1, 0),
        markers: Some(BgMarkers { log: &log, complete: &complete, failed: &failed }),
    });
```

In `background.rs`, add `pub then_args: Option<&'a [&'a str]>` to `BgJob` (default `None` at all other call sites — grep `BgJob {` and add `then_args: None,` to each existing construction), and in the marker-script branch emit, after the primary `if {ecp} {args} …` succeeds and before `: > {complete}`:

```text
  {ecp} {then_args} >> {log} 2>&1 || true
```

(gc failure must NOT flip the prune job to `.prune-failed` — gc is best-effort cleanup; `|| true` keeps prune's success authoritative.)

- [ ] **Step 3: Build**

Run: `cargo build -p egent-code-plexus 2>&1 | tail -8`
Expected: compiles clean. Fix any other `BgJob {` construction that now misses `then_args: None`.

- [ ] **Step 4: Commit**

```bash
git add crates/ecp-cli/src/background.rs crates/ecp-cli/src/commands/hook/session_start.rs
git commit -m "feat(hook): run admin gc after prune under one flock in session_start (FU-2026-05-26-001)"
```

---

## Task 7: Full test + clippy + self-review

- [ ] **Step 1: Run the full gc test file + any registry tests**

Run: `cargo test -p egent-code-plexus --test gc 2>&1 | tail -15`
Expected: all `sweep_*` + existing `reachability`/`enforce_quota`/`sweep_sessions` tests PASS.
Run: `cargo test -p ecp-core registry 2>&1 | tail -10`
Expected: existing registry/fs_safe tests PASS (Task 4 changed only the logging line).

- [ ] **Step 2: Clippy (touched crates only — avoid workspace blast radius)**

Run: `cargo clippy -p egent-code-plexus --tests 2>&1 | tail -20`
Run: `cargo clippy -p ecp-core 2>&1 | tail -10`
Expected: no new warnings.

- [ ] **Step 3: Self-review the diff**

Run: `git diff origin/main --stat` then review each hunk: no dead code left, every `BgJob` construction has `then_args`, no blanket `#[allow(dead_code)]` re-added.

- [ ] **Step 4: Update FOLLOWUPS — file FU-2026-05-26-001 as resolved**

Per the followups protocol, this PR resolves FU-2026-05-26-001. Since the FU was surfaced AND fixed in the same effort, add it directly to `/home/enor/code-graph-nexus/.claude/FOLLOWUPS_DONE.md` with a `✅ done` heading (no Open stub needed — it never lived in Open). The Follow-ups update is a SEPARATE `chore/followups-…` commit/PR per protocol — do NOT bundle it into this feature PR. Note the FU ID in the PR body only.

---

## Task 8: /simplify, push, open PR, confirm CI green

This task fulfills the user goal: implement → /simplify → push to remote (via PR, never direct to main) → confirm CI passes.

- [ ] **Step 1: Run /simplify on the diff**

The diff is small (~5 files). Per CLAUDE.md, for <3 files or <100 LOC do a single-pass self-review instead of 3 fan-out agents. This diff touches ~6 files but is mechanically simple; run `/simplify` (the controller decides fan-out vs single-pass). Address any MUST-FIX findings as new commits (never `--amend` per [[feedback-no-amend-after-hook-fail]]).

- [ ] **Step 2: Push the branch**

```bash
git push -u origin fix/ecp-dead-graph-gc
```

- [ ] **Step 3: Open the PR**

```bash
gh pr create --base main --head fix/ecp-dead-graph-gc \
  --title "fix(gc): converge ~/.ecp stale graph generations + wire admin gc (FU-2026-05-26-001)" \
  --body "$(cat <<'EOF'
## Problem
`~/.ecp` graph cache leaked to 16G (13G zombie). Three cleanup layers all missed:
- **L2 (main cause):** same-SHA `.gen.<ts>` generation dirs never converged — one SHA accumulated 25× 63MB graphs.
- **L1:** `retire_dir_async` detached delete thread died with the process, leaving `<repo>.dead.*` dirs; failure was swallowed by `let _ =`.
- **L3:** `gc::sweep_sessions` existed but `admin gc` was never wired.

## Fix
- `sweep_stale_generations` — per-SHA, keep greatest `Generation` (reuses `CommitDirName::parse` + `Generation` Ord), skip `.building`/fresh(<10s).
- `sweep_retired_repos` — remove top-level `.dead.*` repo dirs.
- `retire_dir_async` — log background-delete failures instead of swallowing.
- Wire `ecp admin gc` subcommand; run it after `prune --orphans` under one flock in `session_start` (serial, gc failure is best-effort, doesn't flip prune marker).

## Verification
- Unit tests for all sweeps (newest-per-SHA, skip-building, skip-fresh, retired removal).
- `ecp admin gc --dry-run` smoke test.
- NO shared-corpus reindex (that triggered a host freeze during investigation).

Resolves FU-2026-05-26-001.
EOF
)"
```

- [ ] **Step 4: Confirm CI starts + watch to completion**

Run: `gh pr checks --watch 2>&1 | tail -30`
Expected: all checks pass. If any check FAILS, read its log (`gh run view <run-id> --log-failed`), fix as a NEW commit, push, re-watch. Do NOT merge; the repo's auto-merge flow handles merge once CI is green (per pr-finalize skill). Report final CI status to the user.

---

## Self-Review (plan vs spec)

- **Spec coverage:** L2 sweep (T1/T2) ✓; L1 sweep (T3) ✓; retire-thread fix (T4) ✓; admin gc wiring (T5) ✓; session_start background trigger, one flock, prune→gc (T6) ✓; tests (T1-T3) ✓; YAGNI boundaries (serial delete, no ingest-path change, prune semantics unchanged) — honored: gc deletion is serial in T5's loop, no ingest hot-path touched, prune untouched. ✓
- **Type consistency:** `SweepStats { marked, removed }` used consistently (both new fns set `marked: 0`); `sweep_stale_generations`/`sweep_retired_repos`/`sweep_sessions` names stable across T1/T3/T5; `GcArgs`/`gc::run` stable across T5/T6; `then_args` field stable across T6.
- **Concurrency requirement (user):** check (Phase 1) is the readdir+parse+stat loop; delete (Phase 2) is the serial `remove_dir_all` loop — separated. flock via existing `.prune.lock` (T6) serializes across sessions. Fresh(<10s)+`.building` guards prevent deleting in-flight ingests. Completion ensured by running in the detached background PROCESS (not in-process thread) under flock with markers.
- **Placeholder scan:** no TBD/TODO; every code step has full code; commands have expected output.
