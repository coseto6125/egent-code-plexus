use ecp_cli::auto_ensure::{
    ensure_index, head_sha_sidecar_path, write_head_sha_sidecar_with_sha, EnsureResult,
};
use ecp_core::contracts::dirty_event::{ClassifiedPath, DirtyEvent, DirtySet, PathClass, RepoPath};
use ecp_core::graph_fixture::GraphFixture;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

/// Init a tempdir as a git repo with one committed source file, returning
/// the HEAD SHA. Mirrors the helper in `engine_session_state_test.rs` —
/// duplicated here so each test file's helpers stay co-located.
fn git_init_with_commit(p: &Path) -> String {
    let g = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(args)
            .output()
            .unwrap()
    };
    g(&["init", "-q"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    fs::write(p.join("src.rs"), "fn foo() {}").unwrap();
    g(&["add", "."]);
    g(&["commit", "-qm", "init"]);
    let out = g(&["rev-parse", "HEAD"]);
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Materialise a minimal v3 graph.bin so `ensure_index`'s header pre-check
/// can confirm the file is well-formed and exit on mtime logic alone. The
/// graph carries no nodes / edges; only the magic + version fields matter
/// for these tests.
fn write_valid_empty_graph(path: &Path) {
    let bytes = GraphFixture::new().into_bytes();
    fs::write(path, &bytes).unwrap();
}

#[test]
fn ensure_returns_ready_when_graph_exists_and_no_newer_source() {
    let tmp = TempDir::new().unwrap();
    // Write a source file FIRST (older mtime)
    fs::write(tmp.path().join("src.rs"), "fn foo() {}").unwrap();
    // Wait a moment, then write the graph (newer mtime)
    std::thread::sleep(std::time::Duration::from_millis(20));
    let graph_path = tmp.path().join("graph.bin");
    write_valid_empty_graph(&graph_path);

    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(
        matches!(result, EnsureResult::Ready),
        "expected Ready, got {:?}",
        result
    );
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
    write_valid_empty_graph(&graph_path);
    // Wait, then touch a source file with newer mtime
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(tmp.path().join("src.rs"), "fn foo() {}").unwrap();

    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(
        matches!(result, EnsureResult::Stale { .. }),
        "expected Stale, got {:?}",
        result
    );
}

/// `.gitignore`d dirs must not trip the staleness check, even when they
/// contain newer-than-graph files. Pre-fix: `auto_ensure` walked with
/// `.git_ignore(false)` and relied on a hardcoded `SKIP_DIRS` list, so a
/// repo whose `.gitignore` listed `.claude/` (sibling git worktrees) or
/// any other build/cache dir not in `SKIP_DIRS` would false-positive Stale
/// on every agent command, churning reindex.
#[test]
fn ensure_returns_ready_when_only_gitignored_files_are_newer() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(".gitignore"), ".claude/\nbuild_out/\n").unwrap();
    fs::write(tmp.path().join("src.rs"), "fn foo() {}").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let graph_path = tmp.path().join("graph.bin");
    write_valid_empty_graph(&graph_path);

    // Newer files inside gitignored dirs — must NOT count as source changes.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::create_dir_all(tmp.path().join(".claude/worktrees/sibling")).unwrap();
    fs::write(
        tmp.path().join(".claude/worktrees/sibling/main.rs"),
        "fn other() {}",
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("build_out")).unwrap();
    fs::write(tmp.path().join("build_out/gen.rs"), "fn gen() {}").unwrap();

    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(
        matches!(result, EnsureResult::Ready),
        "expected Ready (gitignored dirs ignored), got {:?}",
        result
    );
}

#[test]
fn ensure_returns_ready_when_only_ecpignored_files_are_newer() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(".ecpignore"), "vendor/**/*.c\n").unwrap();
    fs::write(tmp.path().join("src.rs"), "fn foo() {}").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let graph_path = tmp.path().join("graph.bin");
    write_valid_empty_graph(&graph_path);

    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::create_dir_all(tmp.path().join("vendor/tree-sitter-nim/src")).unwrap();
    fs::write(
        tmp.path().join("vendor/tree-sitter-nim/src/parser.c"),
        "void generated(void) {}",
    )
    .unwrap();

    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(
        matches!(result, EnsureResult::Ready),
        "expected Ready (ecpignored files ignored), got {:?}",
        result
    );
}

/// HEAD-SHA sidecar must short-circuit the mtime walk: a source file
/// re-touched after graph build (newer mtime, identical content) would
/// trip the old walk into Stale, but the fingerprint shortcut says
/// "HEAD unchanged + git status clean ⇒ Ready". This is both faster and
/// more correct — `git checkout HEAD path` re-touches mtime without
/// changing content; the index is genuinely fresh.
#[test]
fn shortcut_returns_ready_when_sidecar_matches_head_and_tree_clean() {
    let tmp = TempDir::new().unwrap();
    let head_sha = git_init_with_commit(tmp.path());

    let graph_path = tmp.path().join("graph.bin");
    write_valid_empty_graph(&graph_path);

    // Re-touch src.rs with identical content so its mtime > graph mtime
    // but the working tree is git-clean.
    std::thread::sleep(Duration::from_millis(20));
    fs::write(tmp.path().join("src.rs"), "fn foo() {}").unwrap();

    // Without sidecar: the legacy walk wins and reports Stale.
    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(
        matches!(result, EnsureResult::Stale { .. }),
        "expected Stale (no sidecar, walk path), got {:?}",
        result
    );

    // Drop the sidecar matching HEAD → shortcut path should win.
    write_head_sha_sidecar_with_sha(&graph_path, &head_sha);

    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(
        matches!(result, EnsureResult::Ready),
        "expected Ready (sidecar shortcut), got {:?}",
        result
    );
}

/// Uncommitted modifications must still report Stale even with a matching
/// HEAD sidecar — git status sees them; the shortcut must not lie about
/// freshness when there's real work for the L1 overlay to do.
#[test]
fn shortcut_returns_stale_when_tree_dirty_even_with_matching_sidecar() {
    let tmp = TempDir::new().unwrap();
    let head_sha = git_init_with_commit(tmp.path());

    let graph_path = tmp.path().join("graph.bin");
    write_valid_empty_graph(&graph_path);
    write_head_sha_sidecar_with_sha(&graph_path, &head_sha);

    // Introduce a real uncommitted change.
    std::thread::sleep(Duration::from_millis(20));
    fs::write(tmp.path().join("src.rs"), "fn foo() { let x = 1; }").unwrap();

    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(
        matches!(result, EnsureResult::Stale { .. }),
        "expected Stale (working tree dirty), got {:?}",
        result
    );
}

/// `admin index` used to write the `.head_sha` sidecar on a detached thread,
/// so a fast exit left it missing or empty. A graph whose commit dir names
/// HEAD must still report work made before indexing, which the mtime walk
/// cannot see.
#[test]
fn test_ensure_index_head_sha_sidecar_lost_graph_at_head_reports_prior_edits_stale() {
    for sidecar in [None, Some("")] {
        let wt = TempDir::new().unwrap();
        let head_sha = git_init_with_commit(wt.path());
        fs::write(wt.path().join("untracked.rs"), "fn untracked_fn() {}").unwrap();
        fs::write(wt.path().join("src.rs"), "fn foo() { edited(); }").unwrap();
        let cache = TempDir::new().unwrap();
        let dir = cache.path().join(format!("branch_main__{head_sha}"));
        fs::create_dir_all(&dir).unwrap();
        let graph_path = dir.join("graph.bin");
        write_valid_empty_graph(&graph_path);
        if let Some(content) = sidecar {
            fs::write(head_sha_sidecar_path(&graph_path), content).unwrap();
        }
        // Both edits predate the graph: the mtime walk reports nothing.
        let past = filetime::FileTime::from_unix_time(1_000_000_000, 0);
        for file in ["untracked.rs", "src.rs"] {
            filetime::set_file_mtime(wt.path().join(file), past).unwrap();
        }
        match ensure_index(&graph_path, wt.path()).unwrap() {
            EnsureResult::Stale {
                dirty_files: Some(set),
                ..
            } => {
                for file in ["untracked.rs", "src.rs"] {
                    assert!(names_path(&set, file), "{sidecar:?} {file}: {set:?}");
                }
            }
            other => panic!("{sidecar:?}: prior edits must reach the overlay, got {other:?}"),
        }
    }
}

/// The commit dir names an older commit and the sidecar is gone: the graph is
/// behind HEAD, so the clean `git status` must not report Ready. The walk sees
/// the file the newer commit touched.
#[test]
fn test_ensure_index_head_sha_sidecar_lost_graph_behind_head_reports_stale() {
    let wt = TempDir::new().unwrap();
    let older_sha = git_init_with_commit(wt.path());
    let cache = TempDir::new().unwrap();
    let dir = cache.path().join(format!("branch_main__{older_sha}"));
    fs::create_dir_all(&dir).unwrap();
    let graph_path = dir.join("graph.bin");
    write_valid_empty_graph(&graph_path);
    std::thread::sleep(Duration::from_millis(20));
    fs::write(wt.path().join("src.rs"), "fn foo() { next(); }").unwrap();
    let commit = Command::new("git")
        .arg("-C")
        .arg(wt.path())
        .args(["commit", "-qam", "next"])
        .output()
        .unwrap();
    assert!(commit.status.success(), "{commit:?}");
    let result = ensure_index(&graph_path, wt.path()).unwrap();
    assert!(
        matches!(result, EnsureResult::Stale { .. }),
        "a graph behind HEAD must stay Stale, got {result:?}"
    );
}

/// The sidecar names another commit (an overlay refresh at a branch since left)
/// while the commit dir names HEAD: the graph is HEAD's, so the shortcut lists
/// the untracked file the mtime walk cannot see.
#[test]
fn test_ensure_index_head_sha_sidecar_other_sha_graph_at_head_reports_untracked_stale() {
    let wt = TempDir::new().unwrap();
    let head_sha = git_init_with_commit(wt.path());
    fs::write(wt.path().join("untracked.rs"), "fn untracked_fn() {}").unwrap();
    let cache = TempDir::new().unwrap();
    let dir = cache.path().join(format!("branch_main__{head_sha}"));
    fs::create_dir_all(&dir).unwrap();
    let graph_path = dir.join("graph.bin");
    write_valid_empty_graph(&graph_path);
    write_head_sha_sidecar_with_sha(&graph_path, &"0".repeat(40));
    let past = filetime::FileTime::from_unix_time(1_000_000_000, 0);
    filetime::set_file_mtime(wt.path().join("untracked.rs"), past).unwrap();
    match ensure_index(&graph_path, wt.path()).unwrap() {
        EnsureResult::Stale {
            dirty_files: Some(set),
            ..
        } => assert!(names_path(&set, "untracked.rs"), "{set:?}"),
        other => panic!("untracked file must reach the overlay, got {other:?}"),
    }
}

/// Sidecar SHA differs from current HEAD (e.g. user switched branches /
/// pulled a fast-forward) ⇒ fingerprint check is inconclusive, fall back
/// to the walk. Walk on a fresh git repo with no newer files = Ready,
/// confirming the fingerprint path didn't latch a stale answer.
#[test]
fn shortcut_falls_through_when_sidecar_sha_mismatch() {
    let tmp = TempDir::new().unwrap();
    let _head_sha = git_init_with_commit(tmp.path());

    let graph_path = tmp.path().join("graph.bin");
    std::thread::sleep(Duration::from_millis(20));
    write_valid_empty_graph(&graph_path);
    // Write a sidecar with a bogus SHA (40 hex zeros).
    write_head_sha_sidecar_with_sha(&graph_path, "0".repeat(40).as_str());

    // Walk sees no newer files (graph is newest) ⇒ Ready, but reached via
    // the walk path, not the shortcut.
    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(
        matches!(result, EnsureResult::Ready),
        "expected Ready via walk fallback, got {:?}",
        result
    );
}

// ── C3 behaviour slice: classified dirty set at the porcelain shortcut ──────

fn git(p: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(args)
        .output()
        .unwrap()
}

fn git_ok(p: &Path, args: &[&str]) {
    let out = git(p, args);
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// The repo of `git_init_with_commit` plus `files`, committed; then an
/// in-tree graph whose sidecar names HEAD, as
/// `shortcut_returns_stale_when_tree_dirty_even_with_matching_sidecar` builds
/// it. The graph and its sidecar show as untracked "other" paths.
fn repo_with_graph_at_head(p: &Path, files: &[&str]) -> PathBuf {
    git_init_with_commit(p);
    for file in files {
        fs::write(p.join(file), "x = 1\n").unwrap();
    }
    if !files.is_empty() {
        git_ok(p, &["add", "."]);
        git_ok(p, &["commit", "-qm", "files"]);
    }
    graph_at_head(p)
}

fn graph_at_head(p: &Path) -> PathBuf {
    let head = String::from_utf8(git(p, &["rev-parse", "HEAD"]).stdout).unwrap();
    let graph_path = p.join("graph.bin");
    write_valid_empty_graph(&graph_path);
    write_head_sha_sidecar_with_sha(&graph_path, head.trim());
    graph_path
}

fn stale_set(graph_path: &Path, worktree: &Path) -> DirtySet<ClassifiedPath> {
    match ensure_index(graph_path, worktree).unwrap() {
        EnsureResult::Stale {
            dirty_files: Some(set),
            ..
        } => set,
        other => panic!("expected Stale with a dirty set, got {other:?}"),
    }
}

fn names_path(set: &DirtySet<ClassifiedPath>, file: &str) -> bool {
    set.events()
        .iter()
        .flat_map(|event| event.halves())
        .any(|(_, half)| half.path.as_str() == file)
}

fn classified(path: &str, class: PathClass) -> ClassifiedPath {
    ClassifiedPath {
        path: RepoPath::new(path).unwrap(),
        class,
    }
}

#[test]
fn test_ensure_index_only_untracked_csv_returns_ready() {
    let tmp = TempDir::new().unwrap();
    let graph_path = repo_with_graph_at_head(tmp.path(), &[]);
    for i in 0..50 {
        fs::write(tmp.path().join(format!("data{i}.csv")), "a,b\n").unwrap();
    }
    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert_eq!(result, EnsureResult::Ready);
}

#[test]
fn test_ensure_index_untracked_csv_and_py_returns_stale_with_py_only() {
    let tmp = TempDir::new().unwrap();
    let graph_path = repo_with_graph_at_head(tmp.path(), &[]);
    fs::write(tmp.path().join("data.csv"), "a,b\n").unwrap();
    fs::write(tmp.path().join("new.py"), "x = 1\n").unwrap();
    assert_eq!(
        stale_set(&graph_path, tmp.path()).events(),
        &[DirtyEvent::Added(classified("new.py", PathClass::SOURCE))]
    );
}

/// A resolution config file stays in the set for areas 4 and 5, though
/// nothing parses it.
#[test]
fn test_ensure_index_modified_cargo_toml_returns_stale_with_config_path() {
    let tmp = TempDir::new().unwrap();
    let graph_path = repo_with_graph_at_head(tmp.path(), &["Cargo.toml"]);
    fs::write(tmp.path().join("Cargo.toml"), "[package]\n").unwrap();
    assert_eq!(
        stale_set(&graph_path, tmp.path()).events(),
        &[DirtyEvent::Modified(classified(
            "Cargo.toml",
            PathClass::RESOLUTION_CONFIG
        ))]
    );
}

#[test]
fn test_ensure_index_modified_tsconfig_returns_source_and_config() {
    let tmp = TempDir::new().unwrap();
    let graph_path = repo_with_graph_at_head(tmp.path(), &["tsconfig.json"]);
    fs::write(tmp.path().join("tsconfig.json"), "{}\n").unwrap();
    assert_eq!(
        stale_set(&graph_path, tmp.path()).events(),
        &[DirtyEvent::Modified(classified(
            "tsconfig.json",
            PathClass::new(true, true)
        ))]
    );
}

#[test]
fn test_ensure_index_git_mv_returns_renamed_both_halves() {
    let tmp = TempDir::new().unwrap();
    let graph_path = repo_with_graph_at_head(tmp.path(), &["a.py"]);
    git_ok(tmp.path(), &["mv", "a.py", "b.py"]);
    assert_eq!(
        stale_set(&graph_path, tmp.path()).events(),
        &[DirtyEvent::Renamed {
            from: classified("a.py", PathClass::SOURCE),
            to: classified("b.py", PathClass::SOURCE),
        }]
    );
}

/// The source segment `ab c.py` has a space at byte 2, the shape of a status
/// entry; a path with a newline needs `-z` to survive.
#[cfg(unix)]
#[test]
fn test_ensure_index_git_mv_source_with_space_and_newline_path_returns_both() {
    let tmp = TempDir::new().unwrap();
    let graph_path = repo_with_graph_at_head(tmp.path(), &["ab c.py"]);
    git_ok(tmp.path(), &["mv", "ab c.py", "d e.py"]);
    fs::write(tmp.path().join("x\ny.py"), "x = 1\n").unwrap();
    assert_eq!(
        stale_set(&graph_path, tmp.path()).events(),
        &[
            DirtyEvent::Renamed {
                from: classified("ab c.py", PathClass::SOURCE),
                to: classified("d e.py", PathClass::SOURCE),
            },
            DirtyEvent::Added(classified("x\ny.py", PathClass::SOURCE)),
        ]
    );
}

/// The event keeps the path that no longer exists. Only a lone worktree half
/// meets the artifact check, so the gate never stats the missing path.
#[test]
fn test_ensure_index_deleted_source_returns_deleted_event() {
    let tmp = TempDir::new().unwrap();
    let graph_path = repo_with_graph_at_head(tmp.path(), &[]);
    fs::remove_file(tmp.path().join("src.rs")).unwrap();
    assert_eq!(
        stale_set(&graph_path, tmp.path()).events(),
        &[DirtyEvent::Deleted(classified("src.rs", PathClass::SOURCE))]
    );
}

/// Git's `UU` describes the index stages, not the file: removed by hand, the
/// path is neither proven present nor proven absent.
#[test]
fn test_ensure_index_conflicted_file_deleted_by_hand_returns_unmerged() {
    let tmp = TempDir::new().unwrap();
    let wt = tmp.path();
    git_init_with_commit(wt);
    git_ok(wt, &["checkout", "-q", "-b", "side"]);
    fs::write(wt.join("src.rs"), "fn side() {}").unwrap();
    git_ok(wt, &["commit", "-qam", "side"]);
    git_ok(wt, &["checkout", "-q", "-"]);
    fs::write(wt.join("src.rs"), "fn main_line() {}").unwrap();
    git_ok(wt, &["commit", "-qam", "main"]);
    assert!(!git(wt, &["merge", "-q", "side"]).status.success());
    fs::remove_file(wt.join("src.rs")).unwrap();
    let graph_path = graph_at_head(wt);
    assert_eq!(
        stale_set(&graph_path, wt).events(),
        &[DirtyEvent::Unmerged(classified(
            "src.rs",
            PathClass::SOURCE
        ))]
    );
}

/// A path git prints that names no file ecp can read is a skipped entry,
/// counted on stderr; with no event left the graph is Ready.
#[cfg(target_os = "linux")]
#[test]
fn test_ensure_index_non_utf8_untracked_path_returns_ready() {
    use std::os::unix::ffi::OsStrExt;
    let tmp = TempDir::new().unwrap();
    let graph_path = repo_with_graph_at_head(tmp.path(), &[]);
    let name = std::ffi::OsStr::from_bytes(b"bad\xff.py");
    fs::write(tmp.path().join(name), "x = 1\n").unwrap();
    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert_eq!(result, EnsureResult::Ready);
}
