use ecp_cli::auto_ensure::{
    ensure_index, head_sha_sidecar_path, write_head_sha_sidecar_with_sha, EnsureResult,
};
use ecp_core::graph::{ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use std::fs;
use std::path::Path;
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

/// Wait long enough for the detached sidecar write to flush. The write is
/// spawned on a background thread (see `write_head_sha_sidecar_with_sha`),
/// so callers that read the sidecar immediately after creating it need a
/// short rendezvous. 50ms is comfortably above the observed ~100µs write
/// latency on a warm fs.
fn wait_for_sidecar(path: &Path) {
    for _ in 0..50 {
        if path.exists() {
            if let Ok(content) = fs::read_to_string(path) {
                if !content.trim().is_empty() {
                    return;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Materialise a minimal v3 graph.bin so `ensure_index`'s header pre-check
/// can confirm the file is well-formed and exit on mtime logic alone. The
/// graph carries no nodes / edges; only the magic + version fields matter
/// for these tests.
fn write_valid_empty_graph(path: &Path) {
    let graph = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: Vec::new(),
        files: Vec::new(),
        nodes: Vec::new(),
        edges: Vec::new(),
        out_offsets: vec![0],
        in_offsets: vec![0],
        in_edge_idx: Vec::new(),
        name_index: Vec::new(),
        process_start: 0,
        traces_offsets: Vec::new(),
        traces_data: Vec::new(),
        blind_spots: Vec::new(),
        route_shapes: Vec::new(),
        call_metas: vec![],
        function_metas: vec![],
        kind_offsets: vec![],
        kind_node_idx: vec![],
        node_flags: vec![],
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap();
    fs::write(path, &*bytes).unwrap();
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
    wait_for_sidecar(&head_sha_sidecar_path(&graph_path));

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
    wait_for_sidecar(&head_sha_sidecar_path(&graph_path));

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
    wait_for_sidecar(&head_sha_sidecar_path(&graph_path));

    // Walk sees no newer files (graph is newest) ⇒ Ready, but reached via
    // the walk path, not the shortcut.
    let result = ensure_index(&graph_path, tmp.path()).unwrap();
    assert!(
        matches!(result, EnsureResult::Ready),
        "expected Ready via walk fallback, got {:?}",
        result
    );
}
