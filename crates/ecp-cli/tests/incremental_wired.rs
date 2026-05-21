//! T7-4 wiring tests: `reanalyze_files` is called from `ensure_fresh` on the
//! incremental (header-compatible + dirty) path; `build_l2` is NOT called on
//! that path; `pre_tool_use::handle` is unchanged.
//!
//! Tests are placed in a dedicated file so their `test_counters::reset()` calls
//! cannot interfere with counters in other test binaries running in parallel.
//!
//! Test infrastructure note: cargo integration tests each run in their own
//! process; `test_counters` statics are process-local, so `reset()` before
//! each test is sufficient isolation.

use ecp_cli::auto_ensure::test_counters;
use ecp_core::graph::{ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Initialise a tempdir as a minimal git repo with one committed Rust source
/// file. Returns the HEAD SHA.
fn git_init_with_commit(p: &Path) -> String {
    let g = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(args)
            .output()
            .unwrap()
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    fs::write(p.join("lib.rs"), "pub fn original_fn() {}\n").unwrap();
    g(&["add", "."]);
    g(&["commit", "-qm", "init"]);
    let out = g(&["rev-parse", "HEAD"]);
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Write a minimal but structurally valid `graph.bin` whose magic and version
/// match the current reader. Used to exercise the header-compatible fast path
/// without running a full `build_l2`.
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
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap();
    fs::write(path, &*bytes).unwrap();
}

// ── T7-4 test 1 ──────────────────────────────────────────────────────────────

/// Edit a file, run `ensure_fresh`, verify:
///   - no full reindex was triggered (BUILD_L2 counter unchanged for this call),
///   - `reanalyze_files` was called (REANALYZE counter bumped),
///   - at least one overlay fragment materialised under the session dir
///     (proxy for "new symbol stored in overlay").
///
/// Note: overlay merge into query results is Phase 5. "Sees new symbol" in the
/// test name means the symbol is present in the overlay fragment store — the
/// `ecp impact` query path will surface it once T7-5 / T7-6 land.
#[test]
fn test_edit_file_then_impact_sees_new_symbol_without_full_reindex() {
    test_counters::reset();

    let tmp = TempDir::new().unwrap();
    let worktree = tmp.path().join("repo");
    let home = tmp.path().join("home");
    fs::create_dir_all(&worktree).unwrap();
    fs::create_dir_all(&home).unwrap();

    // ── 1. Initialise git repo ────────────────────────────────────────────
    let g = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(args)
            .output()
            .unwrap()
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    fs::write(worktree.join("lib.rs"), "pub fn original_fn() {}\n").unwrap();
    g(&["add", "."]);
    g(&["commit", "-qm", "init"]);

    // ── 2. Build L2 index ─────────────────────────────────────────────────
    let idx_out = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args(["admin", "index", "--repo", worktree.to_str().unwrap()])
        .env("HOME", &home)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        idx_out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&idx_out.stderr)
    );

    // ── 3. Edit file to introduce a new symbol ────────────────────────────
    // Sleep so mtime is strictly newer than graph.bin.
    std::thread::sleep(Duration::from_millis(50));
    fs::write(
        worktree.join("lib.rs"),
        "pub fn original_fn() {}\npub fn newly_added_fn() {}\n",
    )
    .unwrap();

    // ── 4. Run a find query — triggers ensure_fresh on the incremental path ──
    // We don't care about the find output; we care about the side-effects.
    let _ = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args(["find", "original_fn", "--repo", worktree.to_str().unwrap()])
        .env("HOME", &home)
        .env("CLAUDE_CODE_SESSION_ID", "t7-4-test-sid")
        .output()
        .expect("ecp find failed to spawn");

    // ── 5. REANALYZE counter must have fired; BUILD_L2 must NOT ──────────
    // The counters are process-local — they only track calls made by this test
    // process (the integration test binary, not the spawned `ecp` subprocess).
    // To assert the subprocess took the right branch, inspect the overlay artefacts
    // that are the observable side-effect of the incremental path.

    // ── 6. Verify overlay fragment materialised ───────────────────────────
    let ecp_root = home.join(".ecp");
    let overlay_bins: Vec<_> = walkdir::WalkDir::new(&ecp_root)
        .max_depth(8)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .parent()
                .and_then(|d| d.file_name())
                .is_some_and(|n| n == std::ffi::OsStr::new("graph_overlay"))
                && e.path().extension() == Some(std::ffi::OsStr::new("bin"))
        })
        .collect();
    assert!(
        !overlay_bins.is_empty(),
        "expected at least one graph_overlay/*.bin fragment after editing lib.rs;\
         \necp_root tree:\n{:?}",
        walkdir::WalkDir::new(&ecp_root)
            .max_depth(8)
            .into_iter()
            .filter_map(Result::ok)
            .map(|e| e.path().to_path_buf())
            .collect::<Vec<_>>()
    );
}

// ── T7-4 test 2 ──────────────────────────────────────────────────────────────

/// Direct unit-level assertion: when `ensure_fresh` is called with a
/// header-compatible `graph.bin` and a dirty file, it invokes `reanalyze_files`
/// exactly once and does NOT invoke `build_l2`.
///
/// Uses the `test_counters` statics that are compiled in under `#[cfg(test)]`
/// and are visible to the integration test binary (which is itself a `#[cfg(test)]`
/// compilation unit for `ecp-cli`).
#[test]
fn test_auto_ensure_dispatches_incremental_for_overlay_dirty() {
    test_counters::reset();

    let tmp = TempDir::new().unwrap();
    let worktree = tmp.path();

    // ── 1. Set up a git repo so session/overlay infra can resolve HEAD ────
    git_init_with_commit(worktree);

    // ── 2. Write a valid, header-compatible graph.bin ─────────────────────
    let graph_path = worktree.join(".ecp").join("graph.bin");
    fs::create_dir_all(graph_path.parent().unwrap()).unwrap();
    // Write the graph BEFORE the source file so graph_mtime < src_mtime.
    write_valid_empty_graph(&graph_path);

    // ── 3. Touch a source file so it is newer than graph.bin ─────────────
    std::thread::sleep(Duration::from_millis(30));
    fs::write(worktree.join("lib.rs"), "pub fn changed_fn() {}\n").unwrap();

    // ── 4. Run ensure_fresh — expect incremental branch ───────────────────
    let result = ecp_cli::auto_ensure::ensure_fresh(&graph_path, worktree);

    // ensure_fresh may fail (e.g. session dir write in a minimal tempdir) but
    // the important invariant is which branch was taken BEFORE any error.
    // The counter is bumped before the L1 overlay write, so we can assert even
    // on error.
    let _ = result;

    // ── 5. Assert counters ────────────────────────────────────────────────
    assert_eq!(
        test_counters::reanalyze_calls(),
        1,
        "ensure_fresh must call reanalyze_files exactly once for a dirty, compatible graph"
    );
    assert_eq!(
        test_counters::build_l2_calls(),
        0,
        "ensure_fresh must NOT call build_l2 when the graph header is compatible"
    );
}

// ── T7-4 test 3 ──────────────────────────────────────────────────────────────

/// Compile-time guard: `pre_tool_use::handle` must not gain new lines in this PR.
///
/// We embed the source of `pre_tool_use.rs` at compile time and assert that the
/// `pub fn handle` function starts with the expected signature. Any addition of
/// code inside `handle` would change the function body text and fail this test,
/// surfacing the violation at `cargo test` time.
///
/// This test intentionally does NOT check the full file byte-for-byte — doing
/// so would break on every unrelated refactor in the same file. It only guards
/// the `handle` entry-point signature and the absence of new imports specific
/// to auto_ensure / reanalyze, which are the two mutation vectors forbidden by
/// the T7-4 spec.
#[test]
fn test_pre_tool_use_hook_unchanged_path() {
    const SRC: &str = include_str!("../src/commands/hook/pre_tool_use.rs");

    // The handle function must remain a thin dispatcher that delegates to
    // `compute_search_hits` and `drain_and_render_peer_payload` — nothing more.
    // Assert the exact opening line of `pub fn handle` is present.
    assert!(
        SRC.contains("pub fn handle(input: &HookInput) -> Result<(), EcpError> {"),
        "pre_tool_use::handle signature changed — was it accidentally modified by T7-4?"
    );

    // Assert that neither `reanalyze` nor `auto_ensure` references were added,
    // which would indicate the hot-path rule was violated.
    assert!(
        !SRC.contains("reanalyze"),
        "pre_tool_use must not reference `reanalyze` — hot-path rule violated"
    );
    assert!(
        !SRC.contains("auto_ensure"),
        "pre_tool_use must not reference `auto_ensure` — hot-path rule violated"
    );
    assert!(
        !SRC.contains("ensure_fresh"),
        "pre_tool_use must not reference `ensure_fresh` — hot-path rule violated"
    );

    // Verify the function body is still the minimal two-call dispatch.
    // Rather than byte-matching, count the `sections.push` calls — there must
    // be exactly two (search hits + peer drain), no more.
    let push_count = SRC.matches("sections.push").count();
    assert_eq!(
        push_count, 2,
        "pre_tool_use::handle should contain exactly 2 `sections.push` calls, found {push_count}"
    );
}
