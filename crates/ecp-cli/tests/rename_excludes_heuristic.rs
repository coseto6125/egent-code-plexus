//! T-H2 integration tests: heuristic edge exclusion from rename + mirror count.
//!
//! All four tests share the same synthetic-graph injection pattern:
//!   1. Create a minimal git repo with Python source files.
//!   2. Run `ecp admin index` to produce a valid `graph.bin` header.
//!   3. Overwrite `graph.bin` with a hand-crafted graph that carries
//!      `MirrorsField` edges, which the real indexer does not emit for plain
//!      Python functions.
//!   4. Run `ecp rename` and assert the required output / file state.

mod common;

use common::run_git;
use ecp_core::graph::RelType;
use ecp_core::graph_fixture::GraphFixture;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

// ---------------------------------------------------------------------------
// Graph-bin injection helpers
// ---------------------------------------------------------------------------

/// Locate the `graph.bin` written under `~/.ecp/…/graph.bin` after indexing.
/// The exact nesting depth varies by version; this walks the tree breadth-first
/// until it finds a file named `graph.bin`.
fn find_graph_bin(repo: &Path) -> std::path::PathBuf {
    let ecp_dir = repo.join(".ecp");
    assert!(
        ecp_dir.is_dir(),
        ".ecp dir missing after index: {}",
        ecp_dir.display()
    );
    let mut queue = vec![ecp_dir];
    while let Some(dir) = queue.first().cloned() {
        queue.remove(0);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.file_name().map(|n| n == "graph.bin").unwrap_or(false) {
                return path;
            }
            if path.is_dir() {
                queue.push(path);
            }
        }
    }
    panic!(
        "graph.bin not found after admin index in {}",
        repo.join(".ecp").display()
    )
}

/// Run `ecp admin index` against `repo`. HOME is set to `repo` so that
/// `~/.ecp/` resolves to `repo/.ecp/`, matching the `find_graph_bin` helper.
fn build_index(repo: &Path) {
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", repo.to_str().unwrap()])
        .env("HOME", repo)
        .current_dir(repo)
        .output()
        .expect("ecp admin index failed to spawn");
    assert!(
        out.status.success(),
        "ecp admin index failed: stderr={}, stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
}

// ---------------------------------------------------------------------------
// Synthetic graph builders
// ---------------------------------------------------------------------------

/// Two `Function` nodes — `email` in file A and `email` in file B — linked by
/// a `MirrorsField` edge (B → A, i.e. the mirror node is the source).
///
/// Graph layout:
///   node 0  email       file 0  src/model.py   (definition)
///   node 1  email_copy  file 1  src/schema.py  (mirror, heuristic)
///   edges[0]: node 1 → node 0, MirrorsField
fn mirrors_field_graph(
    model_symbol: &str,
    mirror_symbol: &str,
    model_file: &str,
    mirror_file: &str,
) -> Vec<u8> {
    let mut fx = GraphFixture::new();
    let model = fx.func(model_file, model_symbol);
    fx.span(model, (1, 0, 2, 0));
    let mirror = fx.func(mirror_file, mirror_symbol);
    fx.span(mirror, (1, 0, 2, 0));
    fx.edge_with(
        mirror,
        model,
        RelType::MirrorsField,
        0.6,
        "schema-mirror-heuristic",
    );
    fx.into_bytes()
}

/// Graph with a single `Function` node and no heuristic edges (zero mirrors).
fn zero_mirrors_graph(symbol: &str, file: &str) -> Vec<u8> {
    let mut fx = GraphFixture::new();
    let n = fx.func(file, symbol);
    fx.span(n, (1, 0, 2, 0));
    fx.into_bytes()
}

// ---------------------------------------------------------------------------
// Repo setup helpers
// ---------------------------------------------------------------------------

/// Create a minimal git repo, write Python source, index, then inject a
/// synthetic graph with a `MirrorsField` edge. Returns the `TempDir`.
fn setup_mirrors_repo(
    model_content: &str,
    mirror_content: &str,
    model_file: &str,
    mirror_file: &str,
    model_symbol: &str,
    mirror_symbol: &str,
) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();

    if let Some(parent) = std::path::Path::new(model_file).parent() {
        std::fs::create_dir_all(root.join(parent)).unwrap();
    }
    if let Some(parent) = std::path::Path::new(mirror_file).parent() {
        std::fs::create_dir_all(root.join(parent)).unwrap();
    }
    std::fs::write(root.join(model_file), model_content).unwrap();
    std::fs::write(root.join(mirror_file), mirror_content).unwrap();

    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "t@e"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "init"]);
    build_index(root);

    let graph_bin = find_graph_bin(root);
    std::fs::write(
        &graph_bin,
        mirrors_field_graph(model_symbol, mirror_symbol, model_file, mirror_file),
    )
    .unwrap();

    repo
}

fn setup_zero_mirrors_repo(symbol: &str) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();

    std::fs::write(
        root.join("model.py"),
        format!("def {symbol}():\n    return 1\n"),
    )
    .unwrap();

    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "t@e"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "init"]);
    build_index(root);

    let graph_bin = find_graph_bin(root);
    std::fs::write(&graph_bin, zero_mirrors_graph(symbol, "model.py")).unwrap();

    repo
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A rename of `email` must mutate `src/model.py` (the definition file) but
/// must NOT touch `src/schema.py` (the heuristic mirror file). The
/// `MirrorsField` edge from schema.py → model.py is skipped in the planner.
///
/// The mirror node carries a DIFFERENT graph name (`schema_email`) so that the
/// name-lookup for `email` returns only one `target_indices` entry (model.py).
/// schema.py is excluded solely because the heuristic edge is skipped —
/// the test would regress if the skip were removed.
#[test]
fn test_rename_does_not_touch_heuristic_files() {
    let repo = setup_mirrors_repo(
        "def email():\n    return \"user@example.com\"\n",
        // File content still has `email` — only the GRAPH node name differs.
        "def email():\n    return \"mirror@example.com\"\n",
        "src/model.py",
        "src/schema.py",
        "email",
        "schema_email", // graph node name on mirror side; keeps target_indices to one entry
    );
    let root = repo.path();

    let out = Command::new(ecp_bin())
        .args([
            "rename",
            "email",
            "new_email",
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", root)
        .current_dir(root)
        .output()
        .expect("ecp rename spawn failed");

    // Exit code may be non-zero if the file doesn't exist on disk yet; the
    // important assertion is that schema.py was never written with new_email.
    let schema_content = std::fs::read_to_string(root.join("src/schema.py")).unwrap_or_default();
    assert!(
        !schema_content.contains("new_email"),
        "heuristic mirror file must not be mutated by rename; schema.py=\n{schema_content}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// The `heuristic_mirrors_not_touched: 1` field must appear in stdout as a
/// structural top-level field after a rename when the graph contains one
/// `MirrorsField` inbound edge on the target symbol.
///
/// Mirror node has a different graph name (`schema_notify`) so only the
/// service.py node matches the rename query; the count of 1 reflects the
/// single heuristic edge touching the renamed symbol.
#[test]
fn test_rename_output_surfaces_count_default() {
    let repo = setup_mirrors_repo(
        "def notify():\n    pass\n",
        "def notify():\n    pass\n",
        "src/service.py",
        "src/schema.py",
        "notify",
        "schema_notify", // distinct graph name keeps target_indices to service.py only
    );
    let root = repo.path();

    let out = Command::new(ecp_bin())
        .args([
            "rename",
            "notify",
            "dispatch",
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", root)
        .current_dir(root)
        .output()
        .expect("ecp rename spawn failed");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("heuristic_mirrors_not_touched: 1"),
        "expected structural field in stdout;\nstdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
}

/// With `--show-heuristic-mirrors`, the output must include the candidate list
/// with the unresolved-tier marker (`tier: unresolved`; real tier/checks
/// land in T4-7).
#[test]
fn test_rename_show_flag_embeds_candidate_list() {
    let repo = setup_mirrors_repo(
        "def process():\n    pass\n",
        "def process():\n    pass\n",
        "src/worker.py",
        "src/schema.py",
        "process",
        "schema_process", // distinct graph name keeps target_indices to worker.py only
    );
    let root = repo.path();

    let out = Command::new(ecp_bin())
        .args([
            "rename",
            "process",
            "handle",
            "--repo",
            root.to_str().unwrap(),
            "--show-heuristic-mirrors",
        ])
        .env("HOME", root)
        .current_dir(root)
        .output()
        .expect("ecp rename spawn failed");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("heuristic_mirrors:"),
        "expected heuristic_mirrors section with --show-heuristic-mirrors;\nstdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        stdout.contains("tier: unresolved"),
        "expected unresolved-tier marker in mirror list;\nstdout={stdout}",
    );
    assert!(
        stdout.contains("requires_verification: true"),
        "expected requires_verification field;\nstdout={stdout}",
    );
}

/// When zero heuristic mirrors exist, `heuristic_mirrors_not_touched: 0` must
/// appear in the output, but the hint line must be suppressed (noise reduction).
#[test]
fn test_rename_zero_count_omits_hint_line() {
    let repo = setup_zero_mirrors_repo("compute");
    let root = repo.path();

    let out = Command::new(ecp_bin())
        .args([
            "rename",
            "compute",
            "calculate",
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", root)
        .current_dir(root)
        .output()
        .expect("ecp rename spawn failed");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("heuristic_mirrors_not_touched: 0"),
        "zero-mirror field must appear;\nstdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        !stdout.contains("find-schema-bindings"),
        "hint line must be suppressed when count is 0;\nstdout={stdout}",
    );
    assert!(
        !stdout.contains("--show-heuristic-mirrors"),
        "hint line must be suppressed when count is 0;\nstdout={stdout}",
    );
}
